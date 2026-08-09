//! Physical frame allocation and virtual memory mapping (Phase 2).
//!
//! Before this module, the kernel heap was a static array baked into the
//! binary's `.bss` section specifically *to avoid* touching physical
//! memory or page tables at all -- the v0.5 `memory.rs` doc comment says
//! so directly. That was a reasonable way to get an early kernel running,
//! but it means the kernel could never map anything else: no user-process
//! memory, no MMIO, no growing the heap past a size fixed at compile time.
//!
//! This module gives the kernel real access to physical memory (via the
//! bootloader's `physical_memory_offset` mapping, opted into in `main.rs`),
//! a frame allocator over the usable regions the bootloader reports, and a
//! thin, error-returning wrapper around `x86_64::structures::paging::Mapper`
//! so callers get a `Result` instead of a panic when a mapping can't be
//! made.

use alloc::vec;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

use bootloader_api::info::{MemoryRegionKind, MemoryRegions};
use spin::Mutex;
use x86_64::structures::paging::mapper::{CleanUp, Translate, TranslateResult};
use x86_64::structures::paging::{
    FrameAllocator, FrameDeallocator, Mapper, OffsetPageTable, Page, PageTable, PageTableFlags,
    PhysFrame, Size4KiB,
};
use x86_64::{PhysAddr, VirtAddr};

/// Enable the CPU's no-execute page-protection feature (`EFER.NXE`).
///
/// Must run before any page is ever mapped with `PageTableFlags::NO_EXECUTE`
/// set -- until this bit is on, the CPU treats that flag as a reserved,
/// must-be-zero bit in every page-table entry, rather than the deny-execute
/// permission it's meant to be. Marking a page `NO_EXECUTE` while `NXE=0`
/// would not fail loudly; it would just silently *not* deny execution (or,
/// on stricter hardware, fault on a "reserved bit set" violation that has
/// nothing to do with the intended protection) -- either way, a security
/// property the rest of the kernel believes is enforced would not actually
/// be. Called once, at the very start of `kernel_main`, before any paging
/// setup happens at all -- see `main.rs`.
pub fn enable_nx() {
    use x86_64::registers::model_specific::{Efer, EferFlags};
    // Safety: setting NXE only makes future NO_EXECUTE-flagged page-table
    // entries actually deny execution instead of being ignored/reserved. It
    // cannot break any existing mapping: nothing has marked a page
    // NO_EXECUTE yet at this point in boot -- this runs before any page
    // table this kernel controls is even created.
    unsafe {
        Efer::update(|flags| flags.insert(EferFlags::NO_EXECUTE_ENABLE));
    }
    // Verify rather than assume: a feature bit that silently failed to
    // stick would make every later NO_EXECUTE mapping a no-op instead of
    // the enforced boundary the rest of this phase's isolation claims
    // depend on.
    assert!(
        Efer::read().contains(EferFlags::NO_EXECUTE_ENABLE),
        "enable_nx: EFER.NXE did not take effect -- CPU does not support (or rejected) the no-execute feature"
    );
    crate::serial_println!("paging: EFER.NXE enabled and verified");
}

/// Build an `OffsetPageTable` over the CPU's currently active level-4 page
/// table.
///
/// # Safety
/// The complete physical memory must already be mapped starting at
/// `physical_memory_offset` (see `BOOTLOADER_CONFIG` in `main.rs`), and
/// this must be called at most once for the lifetime of the returned
/// value: it hands out a `&'static mut` to the live page table, and a
/// second live `&mut` to the same table would be aliasing, which is
/// undefined behavior.
pub unsafe fn init(physical_memory_offset: VirtAddr) -> OffsetPageTable<'static> {
    let level_4_table = active_level_4_table(physical_memory_offset);
    OffsetPageTable::new(level_4_table, physical_memory_offset)
}

/// # Safety
/// Same requirement as `init`: the physical-memory mapping must be live,
/// and this must not be called more than once (see `init`).
unsafe fn active_level_4_table(physical_memory_offset: VirtAddr) -> &'static mut PageTable {
    use x86_64::registers::control::Cr3;

    let (level_4_frame, _) = Cr3::read();
    let phys = level_4_frame.start_address();
    let virt = physical_memory_offset + phys.as_u64();
    let page_table_ptr: *mut PageTable = virt.as_mut_ptr();

    &mut *page_table_ptr
}

/// A physical frame allocator over the bootloader's usable memory regions.
///
/// This is a real, if simple, allocator: frames are bump-allocated from the
/// usable regions the bootloader reported, but freed frames go onto a
/// small pool and are reused before the bump cursor advances further --
/// deallocation genuinely returns memory to circulation rather than
/// leaking it forever.
pub struct BootInfoFrameAllocator {
    memory_regions: &'static MemoryRegions,
    next: usize,
    freed: Vec<PhysFrame<Size4KiB>>,
    allocated_count: usize,
}

impl BootInfoFrameAllocator {
    /// # Safety
    /// `memory_regions` must be the bootloader-provided map from `BootInfo`:
    /// every region marked `Usable` is handed out as free RAM, so the map
    /// must accurately reflect what's genuinely unused.
    pub unsafe fn init(memory_regions: &'static MemoryRegions) -> Self {
        Self {
            memory_regions,
            next: 0,
            freed: Vec::new(),
            allocated_count: 0,
        }
    }

    /// Resolve a bump-cursor index without replaying every preceding frame.
    ///
    /// The former iterator-and-`nth(self.next)` implementation made the
    /// Nth allocation walk O(N) prior frames. Mapping a 64 MiB region therefore
    /// became O(N^2), held the syscall/scheduler critical section for minutes,
    /// and delayed interrupts. The boot memory map has only a small number of
    /// regions, so selecting the containing region directly keeps each fresh
    /// allocation O(number of memory-map regions).
    fn usable_frame_at(&self, mut index: usize) -> Option<PhysFrame<Size4KiB>> {
        for region in self
            .memory_regions
            .iter()
            .filter(|region| region.kind == MemoryRegionKind::Usable)
        {
            let bytes = region.end.checked_sub(region.start)?;
            let frames = usize::try_from(bytes / 4096).ok()?;
            if index < frames {
                let offset = u64::try_from(index).ok()?.checked_mul(4096)?;
                let address = region.start.checked_add(offset)?;
                return PhysFrame::from_start_address(PhysAddr::new(address)).ok();
            }
            index -= frames;
        }
        None
    }

    fn usable_frame_count(&self) -> usize {
        self.memory_regions
            .iter()
            .filter(|region| region.kind == MemoryRegionKind::Usable)
            .filter_map(|region| region.end.checked_sub(region.start))
            .filter_map(|bytes| usize::try_from(bytes / 4096).ok())
            .fold(0usize, usize::saturating_add)
    }

    /// Total frames handed out over this allocator's lifetime (including
    /// ones later freed and re-handed-out) -- a monotonically increasing
    /// counter, distinct from currently-in-use frames. Because a reused
    /// frame increments this exactly like a fresh one, `frames_allocated()
    /// - frames_in_free_pool()` is *not* a valid "frames currently in use"
    /// metric whenever any reuse has happened -- see `frames_bumped()` for
    /// the one that actually is.
    pub fn frames_allocated(&self) -> usize {
        self.allocated_count
    }

    /// Frames that were freed and are waiting to be reused.
    pub fn frames_in_free_pool(&self) -> usize {
        self.freed.len()
    }

    /// How far the bump cursor over *fresh* memory has advanced --
    /// distinct from `frames_allocated()`, which also counts every reused
    /// frame. This only ever grows when `allocate_frame` finds the free
    /// list empty and has to hand out memory it has never given out
    /// before; a frame recycled through `deallocate_frame` and reused via
    /// the free list never touches it. This is the metric a leak test
    /// should compare before/after a batch of allocate+free cycles: if
    /// nothing leaked, every one of those frees left something in the free
    /// list for the next allocation to reuse, so this stays flat no matter
    /// how many cycles ran; if something leaked, the free list runs dry
    /// and this grows once per leaked frame (see `shell.rs`'s
    /// `spawnfail` command).
    pub fn frames_bumped(&self) -> usize {
        self.next
    }

    /// Frames that can still be allocated without changing any allocator
    /// state: recycled frames plus never-before-issued usable frames.
    pub fn frames_available(&self) -> usize {
        self.freed
            .len()
            .saturating_add(self.usable_frame_count().saturating_sub(self.next))
    }
}

// Safety: `allocate_frame` only ever returns frames from the bootloader's
// `Usable` regions (or previously-freed frames that came from the same
// source), and each physical frame is handed out at most once before
// being explicitly freed -- the bump cursor never repeats a frame, and
// the free-list only returns frames this same allocator previously gave
// out.
unsafe impl FrameAllocator<Size4KiB> for BootInfoFrameAllocator {
    fn allocate_frame(&mut self) -> Option<PhysFrame<Size4KiB>> {
        if let Some(frame) = self.freed.pop() {
            self.allocated_count += 1;
            return Some(frame);
        }

        let frame = self.usable_frame_at(self.next);
        if frame.is_some() {
            self.next += 1;
            self.allocated_count += 1;
        }
        frame
    }
}

impl FrameDeallocator<Size4KiB> for BootInfoFrameAllocator {
    /// # Safety
    /// `frame` must have been obtained from this same allocator's
    /// `allocate_frame` and must no longer be mapped or in use anywhere.
    unsafe fn deallocate_frame(&mut self, frame: PhysFrame<Size4KiB>) {
        self.freed.push(frame);
    }
}

/// Map one page to a freshly allocated frame with the given flags.
///
/// Returns a descriptive `Err` instead of panicking on failure (out of
/// physical memory, or the page is already mapped) -- callers decide how
/// to react rather than the mapping code deciding for them.
pub fn map_page(
    mapper: &mut OffsetPageTable<'static>,
    frame_allocator: &mut BootInfoFrameAllocator,
    page: Page<Size4KiB>,
    flags: PageTableFlags,
) -> Result<(), &'static str> {
    let frame = frame_allocator
        .allocate_frame()
        .ok_or("out of physical memory frames")?;

    // Safety: `frame` was just allocated by `frame_allocator` and is not
    // mapped anywhere else (frames are only ever handed out once between
    // allocation and a matching deallocation), and `page` is the caller's
    // to map -- satisfying map_to's aliasing requirement.
    unsafe {
        mapper
            .map_to(page, frame, flags, frame_allocator)
            .map_err(|_| "page mapping failed: already mapped or invalid page table state")?
            .flush();
    }
    Ok(())
}

/// Global mapper + frame allocator, installed once by `memory::init_heap`.
/// A `Mutex` rather than a bare `static mut`: Phase 3's scheduler will make
/// these genuinely reachable from more than one execution context, and
/// this is the point past which that needs to already be safe.
static MAPPER: Mutex<Option<OffsetPageTable<'static>>> = Mutex::new(None);
static FRAME_ALLOCATOR: Mutex<Option<BootInfoFrameAllocator>> = Mutex::new(None);

/// The single sanctioned way to touch `MAPPER` and/or `FRAME_ALLOCATOR`,
/// mirroring `task::with_scheduler` and `keyboard::with_queue`: both locks
/// are plain `spin::Mutex`, and the 100 Hz timer ISR can preempt any task
/// mid-critical-section on this single-core kernel. Without disabling
/// interrupts for the duration of the lock(s), a tick landing while a task
/// holds either lock would have the ISR's own path (or a re-scheduled task)
/// spin on a lock its own preemption victim can never resume to release --
/// the same deadlock shape already fixed once in `task.rs` and again in
/// `allocator.rs`/`keyboard.rs`. Both locks are always acquired together
/// here, in the same order, so there's no separate lock-ordering hazard to
/// introduce by routing everything through one function.
fn with_paging<F, R>(f: F) -> R
where
    F: FnOnce(&mut Option<OffsetPageTable<'static>>, &mut Option<BootInfoFrameAllocator>) -> R,
{
    x86_64::instructions::interrupts::without_interrupts(|| {
        let mut mapper = MAPPER.lock();
        let mut frame_allocator = FRAME_ALLOCATOR.lock();
        f(&mut mapper, &mut frame_allocator)
    })
}

/// Set once by `install` below, read-only for the rest of the kernel's
/// life afterward -- plain relaxed atomics rather than another `Mutex`
/// deliberately: adding more locks to the interrupt-safety surface for
/// state that's written exactly once, during single-threaded boot, before
/// interrupts are even enabled, would be pure overhead. `0` means "not set
/// yet"; both a real physical-memory offset and a real PML4 physical
/// address are always non-zero, so it doubles as the "unset" sentinel.
static PHYS_MEM_OFFSET: AtomicU64 = AtomicU64::new(0);
static KERNEL_PML4_FRAME: AtomicU64 = AtomicU64::new(0);

/// Install the mapper and frame allocator built during heap setup as the
/// kernel-wide instances used by diagnostics and (from later phases) new
/// mappings outside the heap. Also records the physical-memory offset and
/// the kernel's own PML4 frame (read from CR3, which at this point in boot
/// -- before `task::init` creates anything -- is still exactly the one
/// page table this whole kernel has ever used): both are needed by
/// `new_address_space` to build every later process's private page tables.
pub fn install(
    mapper: OffsetPageTable<'static>,
    frame_allocator: BootInfoFrameAllocator,
    physical_memory_offset: VirtAddr,
) {
    let (kernel_pml4_frame, _) = x86_64::registers::control::Cr3::read();
    PHYS_MEM_OFFSET.store(physical_memory_offset.as_u64(), Ordering::Relaxed);
    KERNEL_PML4_FRAME.store(
        kernel_pml4_frame.start_address().as_u64(),
        Ordering::Relaxed,
    );

    with_paging(|mapper_slot, frame_allocator_slot| {
        *mapper_slot = Some(mapper);
        *frame_allocator_slot = Some(frame_allocator);
    });
}

/// The physical-memory offset established at boot (see `paging::init`) --
/// needed anywhere the kernel must turn a physical address into a
/// dereferenceable one, such as building an `OffsetPageTable` over a
/// process's own PML4 frame rather than the currently active one.
pub fn physical_memory_offset() -> Option<VirtAddr> {
    match PHYS_MEM_OFFSET.load(Ordering::Relaxed) {
        0 => None,
        raw => Some(VirtAddr::new(raw)),
    }
}

/// Translate an address in the kernel's active address space for a DMA
/// driver.  The mapper lock and interrupt-disabled interval cover only the
/// page-table walk; callers must perform no allocation or device I/O inside
/// this function.
pub fn translate_kernel_address(addr: VirtAddr) -> Option<PhysAddr> {
    with_paging(|mapper_slot, _| mapper_slot.as_ref()?.translate_addr(addr))
}

/// The kernel's own PML4 frame -- the address space every kernel-only task
/// (shell, idle, heartbeat) runs under, and what the scheduler loads into
/// CR3 whenever the current task isn't a user process (see `task.rs`).
pub fn kernel_pml4_frame() -> Option<PhysFrame> {
    match KERNEL_PML4_FRAME.load(Ordering::Relaxed) {
        0 => None,
        raw => Some(PhysFrame::containing_address(PhysAddr::new(raw))),
    }
}

/// Whether `install` has run. Diagnostics use this to report "paging not
/// active" instead of silently showing zeroes if heap init fell back to
/// the static array (see `memory::init_heap`).
pub fn is_active() -> bool {
    with_paging(|mapper_slot, _| mapper_slot.is_some())
}

/// Frame allocator statistics for `sysinfo`/`monitor`.
pub struct FrameStats {
    pub allocated: usize,
    pub free_in_pool: usize,
    /// See `BootInfoFrameAllocator::frames_bumped` -- the correct metric
    /// for "did anything just leak," immune to `allocated`'s
    /// double-counting of reused frames.
    pub bumped: usize,
}

pub fn frame_stats() -> Option<FrameStats> {
    with_paging(|_, frame_allocator_slot| {
        frame_allocator_slot.as_ref().map(|allocator| FrameStats {
            allocated: allocator.frames_allocated(),
            free_in_pool: allocator.frames_in_free_pool(),
            bumped: allocator.frames_bumped(),
        })
    })
}

/// Read-only admission control for multi-page mapping transactions. A caller
/// can conservatively reserve enough capacity before installing the first
/// leaf, so physical exhaustion is rejected with zero page-table mutation.
pub fn can_allocate_frames(required: usize) -> bool {
    with_paging(|_, frame_allocator_slot| {
        frame_allocator_slot
            .as_ref()
            .map(|allocator| allocator.frames_available() >= required)
            .unwrap_or(false)
    })
}

/// A 9-bit PML4 index (bits 47:39) for `addr`.
const fn pml4_index(addr: u64) -> usize {
    ((addr >> 39) & 0x1FF) as usize
}

/// Base of the per-process private user address range. Every user
/// process's ELF segments and stack live somewhere in
/// `[USER_SPACE_BASE, USER_SPACE_BASE + USER_SPACE_SIZE)`. Chosen so the
/// whole range sits inside a single PML4 entry's 512 GiB span (comfortably
/// -- `USER_SPACE_SIZE` is 1 GiB), far from the bootloader's dynamic
/// kernel/physical-memory mappings and the heap (`0x_4444_4444_0000`).
pub const USER_SPACE_BASE: u64 = 0x_7000_0000_0000;
pub const USER_SPACE_SIZE: u64 = 0x_4000_0000;

/// Phase 5: base of the per-process anonymous-memory (`SYS_MMAP`) arena --
/// 256 MiB into the user region, comfortably clear of any ELF's own
/// `PT_LOAD` segments (which start at `USER_SPACE_BASE` and grow upward;
/// every ELF this project loads, including the desktop, is a few hundred
/// KiB at most) without needing to track each process's actual highest
/// loaded address.
pub const USER_MMAP_BASE: u64 = USER_SPACE_BASE + 0x_1000_0000;

/// Upper bound of the `SYS_MMAP` arena -- 1 MiB below the top of the user
/// region, which leaves a large guard gap below where
/// `task::spawn_user_process` maps the process's stack (the stack itself
/// occupies only the top ~20 KiB: `USER_SPACE_BASE + USER_SPACE_SIZE -
/// 0x1000` down to `- 0x5000`, see `task::USER_STACK_PAGES`). `SYS_MMAP`
/// rejects any request that would grow the arena's bump pointer past this.
pub const USER_MMAP_LIMIT: u64 = USER_SPACE_BASE + USER_SPACE_SIZE - 0x_0010_0000;

/// The one PML4 slot every process's private page-table subtree lives
/// under. `new_address_space` leaves exactly this index empty when it
/// clones the kernel's other 511 entries, and `map_in_address_space`
/// refuses to map anything outside it -- together, that's what makes two
/// processes' private memory genuinely private from each other even when
/// both use the identical virtual address (see `new_address_space`'s docs).
const USER_REGION_PML4_INDEX: usize = pml4_index(USER_SPACE_BASE);

/// A process's private page-table root plus every physical frame it owns
/// (its own page-table subtree and every mapped leaf page), so
/// `free_address_space` can give it all back without walking the tree.
pub struct AddressSpace {
    pml4_frame: PhysFrame,
    owned_frames: Vec<PhysFrame<Size4KiB>>,
}

impl AddressSpace {
    pub fn pml4_frame(&self) -> PhysFrame {
        self.pml4_frame
    }

    /// Number of physical frames this address space currently owns --
    /// diagnostic use (`sysinfo`/`monitor`/isolation-test evidence), not
    /// load-bearing for correctness.
    pub fn frame_count(&self) -> usize {
        self.owned_frames.len()
    }
}

/// Build an `OffsetPageTable` over an arbitrary (not necessarily currently
/// active) PML4 frame, via the same physical-memory-offset technique
/// `paging::init` uses for the CPU's *active* table.
///
/// # Safety
/// `frame` must be a valid, live level-4 page table for the duration the
/// returned mapper is used, and nothing else may hold a live `&mut`
/// reference to it concurrently (single-threaded kernel, so this reduces to
/// "the caller doesn't stash a second one").
unsafe fn mapper_for(frame: PhysFrame, phys_offset: VirtAddr) -> OffsetPageTable<'static> {
    let virt = phys_offset + frame.start_address().as_u64();
    let table_ptr: *mut PageTable = virt.as_mut_ptr();
    // Safety: forwarded from this function's own contract.
    let table: &'static mut PageTable = unsafe { &mut *table_ptr };
    // Safety: `table` is a valid level-4 table per this function's contract,
    // and `phys_offset` is the same offset mapping used to reach it.
    unsafe { OffsetPageTable::new(table, phys_offset) }
}

/// Build a fresh, private address space for a user process.
///
/// Every kernel mapping (heap, kernel image, the physical-memory identity
/// window) is copied in verbatim -- the *same* physical subtree pointers
/// and the *same* flags the kernel's own table already has, which is what
/// "kernel mappings remain available to Ring 0" and "kernel pages must not
/// become USER_ACCESSIBLE" both mean concretely: Ring 0 code (interrupt
/// handlers, syscalls) keeps working correctly no matter which process's
/// CR3 happens to be loaded, and copying an entry cannot change its
/// `USER_ACCESSIBLE` bit, so those pages stay exactly as supervisor-only as
/// they always were. The one exception is `USER_REGION_PML4_INDEX`, left
/// completely empty (not merely unmapped -- the slot itself is absent) so
/// `map_in_address_space` builds a subtree there that belongs to nothing
/// else: this is the actual mechanism behind "one process cannot read or
/// write another's private memory," not a policy this module has to
/// enforce after the fact -- there is no shared page-table entry through
/// which it could happen.
pub fn new_address_space() -> Result<AddressSpace, &'static str> {
    let phys_offset = physical_memory_offset().ok_or("physical memory offset not set")?;

    with_paging(|mapper_slot, frame_allocator_slot| {
        let mapper = mapper_slot.as_mut().ok_or("paging not active")?;
        let frame_allocator = frame_allocator_slot
            .as_mut()
            .ok_or("frame allocator not active")?;

        let pml4_frame = frame_allocator
            .allocate_frame()
            .ok_or("out of physical memory frames")?;

        // Safety: `pml4_frame` was just allocated and is not yet referenced
        // by any page table or CR3, so this is an exclusive access; the
        // physical-memory offset mapping covers all usable RAM.
        let new_table: &mut PageTable = unsafe {
            let virt = phys_offset + pml4_frame.start_address().as_u64();
            &mut *virt.as_mut_ptr()
        };
        new_table.zero();

        let kernel_table = mapper.level_4_table();
        for i in 0..512 {
            if i == USER_REGION_PML4_INDEX {
                continue;
            }
            new_table[i] = kernel_table[i].clone();
        }

        Ok(AddressSpace {
            pml4_frame,
            owned_frames: vec![pml4_frame],
        })
    })
}

/// Thin `FrameAllocator` wrapper that records every frame it hands out into
/// `owned`, so a single `map_to` call -- which may itself allocate any
/// number of new P3/P2/P1 tables internally, opaquely, before it ever
/// reaches the leaf mapping -- still leaves `AddressSpace::owned_frames`
/// with a complete, exact record of everything to free later.
struct TrackingFrameAllocator<'a> {
    inner: &'a mut BootInfoFrameAllocator,
    owned: &'a mut Vec<PhysFrame<Size4KiB>>,
}

/// Page-table cleanup counterpart to `TrackingFrameAllocator`. The mapper's
/// `CleanUp` implementation clears only empty P1-P3 entries; every frame it
/// reports must therefore be a private page-table frame owned by this address
/// space, which is removed from ownership and returned to the same allocator.
struct TrackingFrameDeallocator<'a> {
    inner: &'a mut BootInfoFrameAllocator,
    owned: &'a mut Vec<PhysFrame<Size4KiB>>,
    released: usize,
    ownership_mismatch: bool,
}

impl FrameDeallocator<Size4KiB> for TrackingFrameDeallocator<'_> {
    unsafe fn deallocate_frame(&mut self, frame: PhysFrame<Size4KiB>) {
        let Some(index) = self.owned.iter().position(|owned| *owned == frame) else {
            self.ownership_mismatch = true;
            return;
        };
        self.owned.remove(index);
        // Safety: `CleanUp` removed the only page-table entry referencing this
        // private, now-empty table before invoking the deallocator.
        unsafe { self.inner.deallocate_frame(frame) };
        self.released += 1;
    }
}

// Safety: delegates entirely to `inner`'s own already-`unsafe impl`
// guarantee (each frame handed out at most once until freed); recording the
// frame in `owned` afterward doesn't affect that.
unsafe impl FrameAllocator<Size4KiB> for TrackingFrameAllocator<'_> {
    fn allocate_frame(&mut self) -> Option<PhysFrame<Size4KiB>> {
        let frame = self.inner.allocate_frame()?;
        self.owned.push(frame);
        Some(frame)
    }
}

/// Map one page into `space`'s private address space. Safe to call whether
/// or not `space` is the currently active CR3 -- correctness for a newly
/// mapped page comes from the CR3 reload the scheduler performs on every
/// switch into a process (`task.rs`), not from the `invlpg` `MapperFlush`
/// issues here (which, for a page under a CR3 that isn't loaded, is
/// architecturally harmless: at worst it evicts an unrelated stale TLB
/// entry).
///
/// Refuses to map anything outside `USER_SPACE_BASE..+USER_SPACE_SIZE` --
/// see `new_address_space`'s docs on why that boundary is what actually
/// keeps processes' private memory private, not just a policy check.
pub fn map_in_address_space(
    space: &mut AddressSpace,
    page: Page<Size4KiB>,
    flags: PageTableFlags,
) -> Result<(), &'static str> {
    if pml4_index(page.start_address().as_u64()) != USER_REGION_PML4_INDEX {
        return Err("refusing to map outside the process's private address-space region");
    }
    let phys_offset = physical_memory_offset().ok_or("physical memory offset not set")?;

    with_paging(|_, frame_allocator_slot| {
        let frame_allocator = frame_allocator_slot
            .as_mut()
            .ok_or("frame allocator not active")?;

        // Safety: `space.pml4_frame` was allocated by `new_address_space`
        // and is exclusively owned by `space`.
        let mut mapper = unsafe { mapper_for(space.pml4_frame, phys_offset) };
        let frame = {
            let mut tracking = TrackingFrameAllocator {
                inner: frame_allocator,
                owned: &mut space.owned_frames,
            };
            tracking
                .allocate_frame()
                .ok_or("out of physical memory frames")?
        };

        // Safety: `frame` was just allocated exclusively for this mapping,
        // and `page` falls within `space`'s own private region (checked
        // above), never aliased by another address space's mappings.
        let map_result = {
            let mut tracking = TrackingFrameAllocator {
                inner: frame_allocator,
                owned: &mut space.owned_frames,
            };
            // Safety: documented immediately above.
            unsafe { mapper.map_to(page, frame, flags, &mut tracking) }
        };
        match map_result {
            Ok(flush) => flush.flush(),
            Err(_) => {
                // The leaf frame is passed to `map_to`; it is never installed
                // when `map_to` returns an error. Reclaim it immediately so a
                // rejected/failed mapping cannot leak one frame per attempt.
                // Any intermediate page-table frames `map_to` managed to
                // install before an allocator failure stay owned by `space`
                // and form valid empty tables reusable by a retry.
                if let Some(pos) = space.owned_frames.iter().position(|owned| *owned == frame) {
                    space.owned_frames.remove(pos);
                }
                // Safety: `map_to` failed before installing `frame` as a leaf,
                // and it was removed from ownership tracking above.
                unsafe {
                    frame_allocator.deallocate_frame(frame);
                }
                return Err("page mapping failed: already mapped or invalid page table state");
            }
        }
        Ok(())
    })
}

/// Reclaim empty private P1-P3 tables covering a page-aligned range.
///
/// Leaf mappings must already be absent. This is the final step of failed
/// MMAP rollback and successful MUNMAP: it ensures page-table infrastructure
/// created solely for the removed leaves does not remain as hidden process
/// memory state or consume frames until process exit.
pub fn clean_up_empty_tables_in_range(
    space: &mut AddressSpace,
    raw_start: u64,
    page_count: u64,
) -> Result<usize, &'static str> {
    if page_count == 0 || raw_start % 4096 != 0 {
        return Err("page-table cleanup range must contain aligned whole pages");
    }
    let byte_len = page_count
        .checked_mul(4096)
        .ok_or("page-table cleanup range overflow")?;
    let raw_end = raw_start
        .checked_add(byte_len.checked_sub(1).ok_or("empty cleanup range")?)
        .ok_or("page-table cleanup end overflow")?;
    let start = checked_user_virt_addr(raw_start)?;
    let end = checked_user_virt_addr(raw_end)?;
    let range = Page::range_inclusive(
        Page::<Size4KiB>::containing_address(start),
        Page::<Size4KiB>::containing_address(end),
    );
    let phys_offset = physical_memory_offset().ok_or("physical memory offset not set")?;

    with_paging(|_, frame_allocator_slot| {
        let frame_allocator = frame_allocator_slot
            .as_mut()
            .ok_or("frame allocator not active")?;
        // Safety: `space` exclusively owns its private page-table subtree.
        let mut mapper = unsafe { mapper_for(space.pml4_frame, phys_offset) };
        let mut deallocator = TrackingFrameDeallocator {
            inner: frame_allocator,
            owned: &mut space.owned_frames,
            released: 0,
            ownership_mismatch: false,
        };
        // Safety: every user-region page-table frame belongs only to `space`;
        // kernel mappings occupy other PML4 slots and the range is constrained
        // to this process's private user region above.
        unsafe { mapper.clean_up_addr_range(range, &mut deallocator) };
        if deallocator.ownership_mismatch {
            return Err("cleaned page-table frame was not owned by address space");
        }
        Ok(deallocator.released)
    })
}

/// Resolve `addr` within `space`'s own page tables -- regardless of whether
/// `space` is the currently active CR3 -- returning the mapped physical
/// address and the leaf entry's flags, or `None` if unmapped.
///
/// This is the primitive user-pointer validation (`syscall.rs`) is built
/// on: a Ring 3 pointer's numeric value is never trusted on its own, it
/// must first resolve to a real mapping in the *calling* process's own
/// tables, with the permissions the operation actually needs.
pub fn translate_in_address_space(
    space: &AddressSpace,
    addr: VirtAddr,
) -> Option<(PhysAddr, PageTableFlags)> {
    let phys_offset = physical_memory_offset()?;
    // Safety: `space.pml4_frame` is a valid level-4 table for as long as
    // `space` exists, which outlives this call.
    let mapper = unsafe { mapper_for(space.pml4_frame, phys_offset) };
    match mapper.translate(addr) {
        TranslateResult::Mapped {
            frame,
            offset,
            flags,
        } => Some((frame.start_address() + offset, flags)),
        TranslateResult::NotMapped | TranslateResult::InvalidFrameAddress(_) => None,
    }
}

/// Convert an untrusted raw Ring-3 address into a `VirtAddr` without ever
/// invoking `VirtAddr::new`'s panicking invalid-address path. In addition to
/// being canonical *as supplied* (no implicit sign-extension), the address
/// must be inside the one private user region this kernel maps for a process.
///
/// Keeping this check separate from the page-table walk is deliberate: a
/// kernel address might happen to translate through the kernel half cloned
/// into every process CR3, but it is never a valid userspace pointer even if
/// the eventual permission check would also reject it.
pub fn checked_user_virt_addr(raw: u64) -> Result<VirtAddr, &'static str> {
    let addr = VirtAddr::try_new(raw).map_err(|_| "non-canonical user address")?;
    if addr.as_u64() != raw {
        return Err("non-canonical user address");
    }
    let user_end = USER_SPACE_BASE
        .checked_add(USER_SPACE_SIZE)
        .ok_or("user address-space bound overflow")?;
    if raw < USER_SPACE_BASE || raw >= user_end {
        return Err("address is outside the process user region");
    }
    Ok(addr)
}

/// Prevalidate every page touched by an untrusted Ring-3 range.
///
/// This is the common security boundary for all pointer-bearing syscalls.
/// It rejects a non-canonical start, arithmetic overflow, a range that leaves
/// the private user region, an unmapped page, a supervisor-only page, or (for
/// copy-out operations) a read-only page. Callers that promise atomic failure
/// semantics invoke this for the *whole* range before copying or unmapping a
/// single byte/page.
pub fn validate_user_range(
    space: &AddressSpace,
    raw_start: u64,
    len: usize,
    require_writable: bool,
) -> Result<VirtAddr, &'static str> {
    let start = checked_user_virt_addr(raw_start)?;
    let len = u64::try_from(len).map_err(|_| "user range length does not fit in u64")?;
    let end = raw_start
        .checked_add(len)
        .ok_or("user range arithmetic overflow")?;
    let user_end = USER_SPACE_BASE
        .checked_add(USER_SPACE_SIZE)
        .ok_or("user address-space bound overflow")?;
    if end > user_end {
        return Err("user range leaves the process user region");
    }

    // A zero-byte operation dereferences nothing, but its pointer is still
    // required to be a canonical in-region user value. This gives all
    // pointer-bearing syscalls one deterministic contract and ensures a
    // malicious non-canonical value never becomes accepted merely because
    // its accompanying length happened to be zero.
    if len == 0 {
        return Ok(start);
    }

    let mut checked = 0u64;
    while checked < len {
        let raw = raw_start
            .checked_add(checked)
            .ok_or("user range arithmetic overflow")?;
        // `end <= user_end` above proves this exact conversion is canonical,
        // but retain the fallible constructor at the untrusted boundary.
        let addr = VirtAddr::try_new(raw).map_err(|_| "non-canonical user address")?;
        if addr.as_u64() != raw {
            return Err("non-canonical user address");
        }
        let (_, flags) = translate_in_address_space(space, addr)
            .ok_or("user range contains an unmapped page")?;
        if !flags.contains(PageTableFlags::PRESENT) {
            return Err("user range contains a non-present page");
        }
        if !flags.contains(PageTableFlags::USER_ACCESSIBLE) {
            return Err("user range contains a supervisor-only page");
        }
        if require_writable && !flags.contains(PageTableFlags::WRITABLE) {
            return Err("user range contains a read-only page");
        }

        let page_offset = raw % 4096;
        checked += (4096 - page_offset).min(len - checked);
    }
    Ok(start)
}

/// Change the flags of an already-mapped page in `space`. Used by the ELF
/// loader (`elf.rs`) to map a segment `WRITABLE` just long enough to copy
/// its bytes in, then drop `WRITABLE` (or add `NO_EXECUTE`) to reach its
/// real, final permissions -- "user executable code: readable/executable,
/// not writable *after loading*" is implemented as exactly this two-step
/// sequence, not a claim.
pub fn update_flags_in_address_space(
    space: &AddressSpace,
    page: Page<Size4KiB>,
    flags: PageTableFlags,
) -> Result<(), &'static str> {
    let phys_offset = physical_memory_offset().ok_or("physical memory offset not set")?;
    // Safety: `space.pml4_frame` is a valid level-4 table for as long as
    // `space` exists.
    let mut mapper = unsafe { mapper_for(space.pml4_frame, phys_offset) };
    // Safety: `page` was previously mapped by `map_in_address_space` within
    // this same `space`; narrowing or changing its flags here cannot make
    // any *other* mapping unsound, since this table's user-region subtree
    // is exclusively owned by `space`.
    unsafe {
        mapper
            .update_flags(page, flags)
            .map_err(|_| "flag update failed: page not mapped")?
            .flush();
    }
    Ok(())
}

/// Walk `[start, start+len)` in `space`'s own mapped memory one page-chunk
/// at a time, handing `f` a kernel-writable pointer (via the
/// physical-memory-offset mapping) and a length for each chunk. Every byte
/// touched must already be mapped `WRITABLE | USER_ACCESSIBLE` in `space`
/// -- this is the shared primitive behind `write_bytes_in_address_space`
/// and `zero_bytes_in_address_space`, used by the ELF loader and
/// `task::spawn_user_process` to populate freshly mapped segments/stacks
/// (always already `USER_ACCESSIBLE` by the time either calls this) *and*,
/// as of Phase 5, by syscalls that write kernel-computed data into a
/// caller-supplied destination pointer (`DISPLAY_INFO`, `INPUT_POLL`).
///
/// Requiring `USER_ACCESSIBLE` here, not just `WRITABLE`, is load-bearing
/// for that second use: most kernel memory (the heap, in particular) is
/// `WRITABLE` but never `USER_ACCESSIBLE`, so a `WRITABLE`-only check would
/// let a syscall write into arbitrary kernel memory the moment a Ring 3
/// caller supplied a kernel address as the destination -- exactly the
/// "never trust Ring 3 pointers" boundary this project's syscalls exist to
/// enforce. Every existing caller's destination is already
/// `USER_ACCESSIBLE` by construction (kernel-computed addresses inside a
/// range the caller itself just mapped that way), so this is a pure
/// hardening with no effect on any legitimate existing use.
fn for_each_mapped_chunk(
    space: &AddressSpace,
    start: VirtAddr,
    len: u64,
    mut f: impl FnMut(*mut u8, usize),
) -> Result<(), &'static str> {
    // Validate the complete destination before invoking `f` for the first
    // chunk. This makes all users of this primitive atomic on validation
    // failure: a bad later page can no longer leave an earlier page partly
    // modified.
    validate_user_range(space, start.as_u64(), len as usize, true)?;
    let phys_offset = physical_memory_offset().ok_or("physical memory offset not set")?;
    let mut written = 0u64;
    while written < len {
        let addr = start + written;
        let (phys, flags) = translate_in_address_space(space, addr)
            .ok_or("destination page not mapped in this address space")?;
        if !flags.contains(PageTableFlags::WRITABLE) {
            return Err("destination page not writable");
        }
        if !flags.contains(PageTableFlags::USER_ACCESSIBLE) {
            return Err("destination page not user-accessible");
        }
        let page_offset = addr.as_u64() % 4096;
        let chunk_len = (4096 - page_offset).min(len - written);
        let dst_ptr: *mut u8 = (phys_offset + phys.as_u64()).as_mut_ptr();
        // Safety: `phys` was just resolved from a `PRESENT | WRITABLE |
        // USER_ACCESSIBLE` mapping in `space`'s own tables (checked above),
        // and the physical-memory offset mapping covers all usable RAM --
        // `dst_ptr` is valid and writable for exactly `chunk_len` bytes
        // starting there, which is bounded to stay within this one 4 KiB
        // frame.
        f(dst_ptr, chunk_len as usize);
        written += chunk_len;
    }
    Ok(())
}

/// Copy `data` into `space`'s own mapped memory at `dest`. Every byte
/// touched must already be mapped `WRITABLE` in `space` (see
/// `map_in_address_space`).
pub fn write_bytes_in_address_space(
    space: &AddressSpace,
    dest: VirtAddr,
    data: &[u8],
) -> Result<(), &'static str> {
    let mut src_offset = 0usize;
    for_each_mapped_chunk(space, dest, data.len() as u64, |dst_ptr, chunk_len| {
        // Safety: see `for_each_mapped_chunk`'s docs; `data[src_offset..]`
        // has at least `chunk_len` bytes remaining by construction (the
        // chunk walk never exceeds `data.len()` total).
        unsafe {
            core::ptr::copy_nonoverlapping(
                data[src_offset..src_offset + chunk_len].as_ptr(),
                dst_ptr,
                chunk_len,
            );
        }
        src_offset += chunk_len;
    })
}

/// Zero `len` bytes of `space`'s own mapped memory starting at `dest` --
/// used for a segment's BSS tail (`p_memsz > p_filesz`) and, just as
/// importantly, to guarantee a freshly allocated physical frame never
/// exposes a previous owner's leftover contents to a new process (frames
/// handed back to the allocator by `free_address_space` are not zeroed on
/// free, only reused ones would otherwise leak data on the *next*
/// allocation).
pub fn zero_bytes_in_address_space(
    space: &AddressSpace,
    dest: VirtAddr,
    len: u64,
) -> Result<(), &'static str> {
    for_each_mapped_chunk(space, dest, len, |dst_ptr, chunk_len| {
        // Safety: see `for_each_mapped_chunk`'s docs.
        unsafe {
            core::ptr::write_bytes(dst_ptr, 0, chunk_len);
        }
    })
}

/// Copy into caller-owned kernel storage without allocating. Syscall paths
/// reserve their bounded destination before taking the scheduler lock, then
/// use this helper while the current address-space reference is protected.
pub fn read_bytes_from_address_space_into(
    space: &AddressSpace,
    src: VirtAddr,
    out: &mut [u8],
) -> Result<(), &'static str> {
    let phys_offset = physical_memory_offset().ok_or("physical memory offset not set")?;
    validate_user_range(space, src.as_u64(), out.len(), false)?;

    let mut read = 0u64;
    while read < out.len() as u64 {
        let addr = src + read;
        let (phys, flags) =
            translate_in_address_space(space, addr).ok_or("source page not mapped")?;
        if !flags.contains(PageTableFlags::USER_ACCESSIBLE) {
            return Err("source page not user-accessible");
        }
        let page_offset = addr.as_u64() % 4096;
        let chunk_len = (4096 - page_offset).min(out.len() as u64 - read) as usize;
        let src_ptr: *const u8 = (phys_offset + phys.as_u64()).as_ptr();
        // Safety: validation above covers the complete source. This chunk is
        // bounded to one present user-accessible frame and `out` is an
        // exclusive kernel slice of at least `chunk_len` remaining bytes.
        unsafe {
            core::ptr::copy_nonoverlapping(src_ptr, out.as_mut_ptr().add(read as usize), chunk_len);
        }
        read += chunk_len as u64;
    }
    Ok(())
}

/// Frame allocator used only while rolling back an interrupted batch
/// unmap. All parent page tables necessarily still exist, so restoring a
/// leaf mapping must not need a new frame; returning `None` makes that
/// invariant explicit instead of accidentally allocating during rollback.
struct NoFrameAllocator;

// Safety: this allocator never hands out a frame, so it cannot violate the
// uniqueness requirements of `FrameAllocator`.
unsafe impl FrameAllocator<Size4KiB> for NoFrameAllocator {
    fn allocate_frame(&mut self) -> Option<PhysFrame<Size4KiB>> {
        None
    }
}

/// A leaf mapping captured during `MUNMAP` prevalidation. Frames described by
/// these records remain owned and unavailable to the global allocator until
/// the complete transaction commits, which lets a long unmap yield between
/// bounded batches without sacrificing all-or-nothing failure semantics.
#[derive(Clone, Copy)]
pub struct UnmapRecord {
    page: Page<Size4KiB>,
    frame: PhysFrame<Size4KiB>,
    flags: PageTableFlags,
}

/// Append validated leaf mappings to an in-progress unmap transaction.
/// This changes no page table or ownership state.
pub fn collect_unmap_records(
    space: &AddressSpace,
    raw_start: u64,
    page_count: u64,
    out: &mut Vec<UnmapRecord>,
) -> Result<(), &'static str> {
    if page_count == 0 || raw_start % 4096 != 0 {
        return Err("unmap range must contain aligned whole pages");
    }
    let byte_len = page_count
        .checked_mul(4096)
        .ok_or("unmap range length overflow")?;
    let byte_len = usize::try_from(byte_len).map_err(|_| "unmap range too large")?;
    validate_user_range(space, raw_start, byte_len, false)?;

    let additional = usize::try_from(page_count).map_err(|_| "unmap page count too large")?;
    out.reserve(additional);
    let mut raw = raw_start;
    for _ in 0..page_count {
        let addr = VirtAddr::try_new(raw).map_err(|_| "non-canonical unmap address")?;
        let (phys, flags) = translate_in_address_space(space, addr)
            .ok_or("unmap range contains an unmapped page")?;
        let frame = PhysFrame::from_start_address(phys)
            .map_err(|_| "unmap translation was not frame-aligned")?;
        out.push(UnmapRecord {
            page: Page::<Size4KiB>::containing_address(addr),
            frame,
            flags,
        });
        raw = raw.checked_add(4096).ok_or("unmap address overflow")?;
    }
    Ok(())
}

/// Return a sorted, duplicate-free list of the transaction's leaf frames.
pub fn unmap_record_frames(records: &[UnmapRecord]) -> Option<Vec<PhysFrame<Size4KiB>>> {
    let mut frames: Vec<_> = records.iter().map(|record| record.frame).collect();
    frames.sort_unstable_by_key(|frame| frame.start_address().as_u64());
    frames.dedup_by_key(|frame| frame.start_address().as_u64());
    (frames.len() == records.len()).then_some(frames)
}

/// One descending ownership-vector removal planned from an immutable snapshot.
#[derive(Clone, Copy)]
pub struct OwnershipRemoval {
    index: usize,
    frame: PhysFrame<Size4KiB>,
}

impl OwnershipRemoval {
    pub fn frame(&self) -> PhysFrame<Size4KiB> {
        self.frame
    }
}

pub fn owned_frame_snapshot(space: &AddressSpace) -> Vec<PhysFrame<Size4KiB>> {
    space.owned_frames.clone()
}

/// Build descending vector indices for every transaction frame. This runs
/// outside scheduler/paging locks. Descending `Vec::remove` keeps all lower
/// precomputed indices stable as higher elements are removed in earlier
/// bounded batches.
pub fn plan_unmap_ownership(
    snapshot: &[PhysFrame<Size4KiB>],
    sorted_frames: &[PhysFrame<Size4KiB>],
) -> Option<Vec<OwnershipRemoval>> {
    let mut removals = Vec::with_capacity(sorted_frames.len());
    for (index, frame) in snapshot.iter().copied().enumerate() {
        if sorted_frames.binary_search(&frame).is_ok() {
            removals.push(OwnershipRemoval { index, frame });
        }
    }
    if removals.len() != sorted_frames.len() {
        return None;
    }
    removals.sort_unstable_by(|left, right| right.index.cmp(&left.index));
    Some(removals)
}

/// Remove a bounded record batch from page tables while retaining physical
/// ownership. On an unexpected failure the prefix removed by this call is
/// restored before returning, so the caller may also restore earlier batches.
pub fn unmap_records_atomic(
    space: &mut AddressSpace,
    records: &[UnmapRecord],
) -> Result<(), &'static str> {
    if records.is_empty() {
        return Ok(());
    }
    let phys_offset = physical_memory_offset().ok_or("physical memory offset not set")?;
    // Safety: the caller holds exclusive access to `space`; its root and all
    // parent tables remain owned throughout this deferred-reclamation phase.
    let mut mapper = unsafe { mapper_for(space.pml4_frame, phys_offset) };
    let mut removed = 0usize;
    for record in records {
        match mapper.unmap(record.page) {
            Ok((actual, flush)) if actual == record.frame => {
                flush.flush();
                removed += 1;
            }
            Ok((actual, flush)) => {
                flush.flush();
                let mut no_frames = NoFrameAllocator;
                // Safety: the leaf was just removed and its parent tables and
                // frame remain live and owned.
                let _ = unsafe { mapper.map_to(record.page, actual, record.flags, &mut no_frames) }
                    .map(|flush| flush.flush());
                break;
            }
            Err(_) => break,
        }
    }
    if removed == records.len() {
        return Ok(());
    }
    let mut no_frames = NoFrameAllocator;
    for record in records[..removed].iter().rev() {
        // Safety: each leaf was removed above and nothing has reclaimed its
        // frame or parent tables.
        unsafe {
            mapper
                .map_to(record.page, record.frame, record.flags, &mut no_frames)
                .map_err(|_| "failed to restore atomic unmap batch")?
                .flush();
        }
    }
    Err("atomic unmap batch failed; removed prefix was restored")
}

/// Restore previously removed transaction batches. No allocation is needed
/// because `Mapper::unmap` never frees parent page tables.
pub fn restore_unmap_records(
    space: &mut AddressSpace,
    records: &[UnmapRecord],
) -> Result<(), &'static str> {
    let phys_offset = physical_memory_offset().ok_or("physical memory offset not set")?;
    // Safety: exclusive `space` access and retained frame/table ownership.
    let mut mapper = unsafe { mapper_for(space.pml4_frame, phys_offset) };
    let mut no_frames = NoFrameAllocator;
    for record in records {
        // Safety: this record's leaf is absent, while its frame and parent
        // tables are still owned by `space`.
        unsafe {
            mapper
                .map_to(record.page, record.frame, record.flags, &mut no_frames)
                .map_err(|_| "failed to restore unmap transaction")?
                .flush();
        }
    }
    Ok(())
}

/// Commit ownership after every leaf in a transaction has been removed.
/// Physical frames are returned separately, after this succeeds.
pub fn remove_unmapped_ownership(space: &mut AddressSpace, removals: &[OwnershipRemoval]) -> bool {
    for removal in removals {
        if space.owned_frames.get(removal.index).copied() != Some(removal.frame) {
            return false;
        }
        space.owned_frames.remove(removal.index);
    }
    true
}

/// Return leaf frames from a committed unmap to the global reuse pool.
pub fn release_frames(frames: &[PhysFrame<Size4KiB>]) -> Result<(), &'static str> {
    with_paging(|_, frame_allocator_slot| {
        let allocator = frame_allocator_slot
            .as_mut()
            .ok_or("frame allocator not active")?;
        for frame in frames {
            // Safety: the transaction removed these leaves and their address
            // space relinquished ownership before this call.
            unsafe { allocator.deallocate_frame(*frame) };
        }
        Ok(())
    })
}

/// Atomically unmap a page-aligned range from one process address space.
///
/// The complete range is translated and permission-checked before the first
/// page-table entry is changed. Leaf frames are not returned to the physical
/// allocator until every unmap has succeeded. If an unexpected mapper error
/// occurs after an earlier leaf was removed, those earlier leaves are restored
/// to their original frames and flags before returning failure. Thus an
/// invalid or partially mapped `MUNMAP` request can never punch a partial hole
/// in the caller's address space.
pub fn unmap_range_in_address_space_atomic(
    space: &mut AddressSpace,
    raw_start: u64,
    page_count: u64,
) -> Result<(), &'static str> {
    let mut records = Vec::new();
    collect_unmap_records(space, raw_start, page_count, &mut records)?;
    let frames = unmap_record_frames(&records).ok_or("duplicate frame in unmap transaction")?;
    let snapshot = owned_frame_snapshot(space);
    let removals = plan_unmap_ownership(&snapshot, &frames)
        .ok_or("unmap range contains a frame not owned by this address space")?;
    unmap_records_atomic(space, &records)?;
    if !remove_unmapped_ownership(space, &removals) {
        restore_unmap_records(space, &records)?;
        return Err("unmapped leaf frame missing from address-space ownership");
    }
    release_frames(&frames)
}

/// Load `frame` as the active CR3. Called on *every* scheduler switch
/// (`task.rs`), not just when the address space actually changes: reloading
/// CR3 to its current value is just a slightly wasteful full TLB flush,
/// which is a better trade than trusting a separately maintained "currently
/// loaded" cache to never drift out of sync with reality.
///
/// # Safety
/// `frame` must be a valid, fully populated PML4 -- the kernel's own root
/// (`kernel_pml4_frame`) or one built by `new_address_space` -- that stays
/// valid for as long as it remains loaded. In particular the caller must
/// not have freed it (`free_address_space`'s own safety contract exists
/// precisely to sequence this correctly: switch away first, free second).
pub unsafe fn switch_to(frame: PhysFrame) {
    use x86_64::registers::control::{Cr3, Cr3Flags};
    // Safety: forwarded from this function's own contract.
    unsafe {
        Cr3::write(frame, Cr3Flags::empty());
    }
}

/// Return every frame `space` owns -- its private page-table subtree and
/// all leaf (code/data/stack) pages -- to the global frame allocator.
///
/// # Safety
/// The caller must guarantee `space`'s PML4 frame is **not** the currently
/// loaded CR3. On this single-core kernel that means: the scheduler must
/// have already switched to a different address space before this runs.
/// Freeing frames CR3 still references would let them be handed out again
/// while still live in the active page tables -- silent corruption the
/// instant the new owner and the stale mapping collide.
pub unsafe fn free_address_space(space: AddressSpace) {
    with_paging(|_, frame_allocator_slot| {
        if let Some(frame_allocator) = frame_allocator_slot.as_mut() {
            for frame in space.owned_frames {
                // Safety: every frame here was allocated by this same
                // global allocator specifically for `space` (see
                // `new_address_space` / `map_in_address_space`'s
                // `TrackingFrameAllocator`), and this function's own
                // contract guarantees it is no longer live in any loaded
                // CR3.
                unsafe {
                    frame_allocator.deallocate_frame(frame);
                }
            }
        }
    });
}
