//! Preemptive task scheduler (Phase 3).
//!
//! Replaces the v0.5/Phase-1-2 decorative two-row task table with real
//! execution: each task has its own kernel stack and a saved CPU context,
//! `context_switch` (hand-written assembly, see below) actually transfers
//! control between them, and the timer interrupt drives preemption --
//! `ps`/`taskinfo`/`kill` now report and act on genuine scheduler state.
//!
//! ## How the context switch works
//!
//! `context_switch(old_rsp: *mut u64, new_rsp: u64)` looks like an ordinary
//! `extern "C"` function call from the Rust side. That's deliberate: the
//! System V calling convention already specifies which registers a normal
//! function call must preserve (`rbx`, `rbp`, `r12`-`r15`, and the stack
//! pointer itself) and which it's free to clobber (everything else,
//! including the floating-point/SSE registers -- SysV has no callee-saved
//! XMM registers at all). `context_switch` only needs to save/restore
//! exactly the callee-saved set to be a *correct* function call from the
//! compiler's point of view; it doesn't need to know or care what the
//! caller was doing with any other register, because the compiler already
//! assumes a normal call might clobber those.
//!
//! The trick is what happens between the push and the pop: it switches
//! `rsp` to a *different* task's stack in the middle, so the `ret` at the
//! end returns not to the caller, but to wherever *that* task last called
//! `context_switch` from (or, for a brand new task, to a small trampoline
//! that starts it running for the first time). Each suspended task's own
//! call chain -- including, critically, the interrupt frame the CPU pushed
//! when the timer fired -- sits dormant on that task's own stack until the
//! scheduler switches back to it, at which point it unwinds completely
//! normally: back up through `schedule`, back up through the timer ISR,
//! and out through a perfectly ordinary `iretq`.
//!
//! This is why the timer interrupt (unlike keyboard) can no longer use a
//! dedicated IST stack (see `gdt.rs`): IST would force every timer
//! interrupt onto the *same* physical stack regardless of which task was
//! running, destroying the very thing that makes resuming a task later
//! possible.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use spin::Mutex;
use x86_64::structures::paging::{Page, PageTableFlags, PhysFrame, Size4KiB};
use x86_64::VirtAddr;

use crate::{elf, gdt, paging};

/// Per-task kernel stack size. Generous relative to what `idle`/`heartbeat`
/// actually need, since it must also absorb however many Rust call frames
/// are active (scheduler + interrupt handler) at the moment a task is
/// preempted.
const STACK_SIZE: usize = 32 * 1024;

/// Bytes needed for the fake initial stack frame `spawn` builds: six
/// callee-saved registers plus a return address, matching exactly what
/// `context_switch`'s epilogue pops before its `ret`.
const INITIAL_FRAME_SIZE: usize = 7 * 8;

/// A task is preempted after this many timer ticks (100 Hz -- see
/// `interrupts.rs` -- so 5 ticks is a 50 ms time slice).
const TIME_SLICE_TICKS: u64 = 5;

/// How long a `Terminated` task's `Tcb` (and its 32 KiB kernel stack) stays
/// reachable via `ps`/`taskinfo`/`task::info` before automatic reaping
/// removes it -- 500 ticks (5 s at the PIT's 100 Hz) is generously longer
/// than the few-microsecond gap between `shell.rs`'s `wait_for_terminated`
/// returning and its immediate follow-up `task::info` read, so ordinary
/// shell diagnostics never race a reap, while still being short enough
/// that a kernel left running keeps reclaiming promptly rather than
/// accumulating terminated `Tcb`s forever. See `Scheduler::reap_terminated`.
const REAP_GRACE_TICKS: u64 = 500;

/// Lifecycle state of a kernel task.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TaskState {
    Ready,
    Running,
    Blocked,
    Terminated,
}

impl TaskState {
    fn label(self) -> &'static str {
        match self {
            TaskState::Ready => "Ready",
            TaskState::Running => "Running",
            TaskState::Blocked => "Blocked",
            TaskState::Terminated => "Terminated",
        }
    }
}

/// Whether a task runs kernel code at Ring 0 only, or is a genuine user
/// process with its own private address space and Ring 3 execution -- see
/// `Tcb::process`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Privilege {
    Kernel,
    User,
}

impl Privilege {
    fn label(self) -> &'static str {
        match self {
            Privilege::Kernel => "kernel",
            Privilege::User => "user",
        }
    }
}

/// Lightweight snapshot returned by `list`/`info` -- deliberately does not
/// expose a task's stack, saved context, or address space, only what
/// `ps`/`taskinfo` need.
#[derive(Clone)]
pub struct Task {
    pub id: u32,
    pub name: String,
    pub state: TaskState,
    pub privilege: Privilege,
    /// `Some` once a user process has exited (via the `exit` syscall or
    /// fault-isolation recovery -- see `syscall.rs` / `interrupts.rs`).
    /// Always `None` for kernel-only tasks.
    pub exit_code: Option<i32>,
}

/// The real task control block. Not exposed outside this module: callers
/// get `Task` snapshots instead (see `list`/`info`).
struct Tcb {
    id: u32,
    name: String,
    state: TaskState,
    /// `None` only for the boot task (id 1, "shell"): it runs on the stack
    /// the bootloader handed the kernel, which this module doesn't own and
    /// must not free. Never read directly -- its only job is to keep the
    /// allocation (and therefore `saved_rsp`, which points into it) alive
    /// for as long as this `Tcb` exists; dropping a `Tcb` frees it.
    #[allow(dead_code)]
    stack: Option<Box<[u8; STACK_SIZE]>>,
    /// Saved stack pointer. Meaningless while this task is `Running` (the
    /// real value lives in the CPU's `rsp` register); valid for every other
    /// state.
    saved_rsp: u64,
    /// Absolute tick count (see `interrupts::ticks`) at which a `Blocked`
    /// task should become `Ready` again. Zero means "not sleeping".
    wake_at_tick: u64,
    /// Top of this task's own kernel stack (16-byte aligned), or 0 for the
    /// boot task (id 1, "shell"), which has no stack this module allocated
    /// (see `stack` above). This is what `schedule()` now writes into the
    /// TSS's RSP0 on *every* switch (see `gdt::set_kernel_stack`) -- the
    /// mechanism that lets any number of user processes safely interleave
    /// under preemption, each trapping from Ring 3 onto its own kernel
    /// stack rather than a single shared one.
    kernel_stack_top: u64,
    /// Present only for genuine user processes -- Ring 3 privilege, a
    /// private address space, and (once set) an exit status. `None` for
    /// kernel-only tasks (shell, idle, heartbeat), which run entirely at
    /// Ring 0 under the shared kernel address space.
    process: Option<ProcessState>,
    /// Absolute tick count (see `interrupts::ticks`) at which this task
    /// became `Terminated`, or 0 if it never has. Read by
    /// `Scheduler::reap_terminated` to enforce `REAP_GRACE_TICKS` -- long
    /// enough that `ps`/`taskinfo`/the immediate post-`wait_for_terminated`
    /// read in `runelf`/`isolate` always still find the task, short enough
    /// that a kernel left running keeps reclaiming `Tcb`s (and their 32 KiB
    /// kernel stacks) promptly rather than accumulating them forever -- see
    /// `ARCHITECTURE.md`'s Phase 5 section for the full reasoning.
    terminated_at_tick: u64,
}

/// A user process's private state: its own address space and, once it has
/// run, an exit status. See `spawn_user_process`.
struct ProcessState {
    /// `None` only in the brief window between a process being marked
    /// `Terminated` (by itself, via `exit`, or by `kill`) and the point at
    /// which `Scheduler::prepare_switch`/`kill` actually reclaims it --
    /// see `schedule()`'s and `kill()`'s docs on why that reclamation is
    /// sequenced the way it is (CR3 must move off an address space before
    /// its frames can be safely freed).
    address_space: Option<paging::AddressSpace>,
    entry_point: u64,
    user_stack_top: u64,
    exit_code: Option<i32>,
    /// Bump pointer for this process's own anonymous-memory arena (`SYS_MMAP`
    /// -- see `mmap_in_current_process`), starting at `paging::USER_MMAP_BASE`
    /// and only ever moving up. No free-list/reuse in this minimal design --
    /// `SYS_MUNMAP` genuinely unmaps and reclaims the underlying physical
    /// frames, but the virtual address range it freed is not reused by a
    /// later `SYS_MMAP` call within the same process (documented limitation,
    /// see `ARCHITECTURE.md`).
    mmap_next: u64,
    /// Normalized absolute VFS path. Each Ring-3 process owns this value;
    /// changing one process's directory cannot affect the shell or a peer.
    cwd: String,
    /// Bounded, process-owned read-only file descriptions. File contents are
    /// snapshotted by VFS at OPEN time, so no backend lock or node reference
    /// survives across a syscall or process lifetime boundary.
    open_files: Vec<Option<OpenFile>>,
    open_file_bytes: usize,
}

struct OpenFile {
    data: Arc<[u8]>,
    offset: usize,
}

pub const MAX_OPEN_FILES: usize = 16;
pub const MAX_OPEN_FILE_BYTES: usize = 256 * 1024;
pub const MAX_TASKS: usize = 64;
const FIRST_FILE_HANDLE: u32 = 3;

struct UserProcessResources {
    name: String,
    cwd: String,
    stack: Box<[u8; STACK_SIZE]>,
    open_files: Vec<Option<OpenFile>>,
}

struct Scheduler {
    tasks: Vec<Box<Tcb>>,
    current: usize,
}

/// Everything `schedule()` needs to actually perform a switch, computed
/// while `SCHEDULER`'s lock is held (`Scheduler::prepare_switch`) so the
/// lock can be released before any of it happens -- `context_switch` may
/// not return to that call frame for an arbitrarily long time.
struct SwitchPlan {
    old_rsp_ptr: *mut u64,
    new_rsp: u64,
    /// CR3 to load for the incoming task: its own `AddressSpace` if it's a
    /// user process, otherwise the kernel's permanent root.
    new_cr3: PhysFrame,
    /// RSP0 to install into the TSS for the incoming task, or 0 for the
    /// one task that owns no module-allocated stack (id 1, "shell") --
    /// see `Tcb::kernel_stack_top`'s docs on why 0 there is safe to leave
    /// untouched rather than meaningful.
    new_kernel_stack_top: u64,
    /// An outgoing, just-terminated task's address space, taken out here
    /// so `schedule()` can free it -- but only *after* `new_cr3` above has
    /// been loaded (see `schedule()`).
    reclaim: Option<paging::AddressSpace>,
}

impl Scheduler {
    /// Remove every `Terminated` task that is *not* the currently running
    /// one and has been `Terminated` for at least `REAP_GRACE_TICKS` (or,
    /// if `force` is set, remove every such task regardless of how long
    /// ago it terminated -- see `task::reap_now`, used by the `reap` shell
    /// command and by tests that need deterministic, immediate reclamation
    /// rather than waiting out the grace period).
    ///
    /// Never touches `self.current`: freeing a task's `Tcb` frees its 32 KiB
    /// kernel stack too (an ordinary `Drop`, not special-cased), and that
    /// stack is exactly what the CPU is physically executing on top of for
    /// as long as that task remains current -- reaping it would be a
    /// genuine use-after-free the instant this function, or anything it
    /// calls, touched the stack again. Restricting reaping to non-current
    /// tasks makes that impossible by construction, the same way
    /// `schedule()` already restricts *address-space* reclamation to
    /// "provably not the active CR3."
    ///
    /// A reaped task's address space is expected to already be `None` here
    /// (freed synchronously by `kill()`, or by a prior `schedule()` call's
    /// own post-switch reclaim when this same task was the one being
    /// switched away from -- see both functions' docs); if one is somehow
    /// still present this frees it too, defensively, which is sound for the
    /// identical reason reaping the `Tcb` itself is: a task this function
    /// is willing to remove is never the current one, so its address space
    /// can never be the active CR3.
    fn reap_terminated(&mut self, force: bool) {
        let now = crate::interrupts::ticks();
        let current_id = self.tasks[self.current].id;

        let should_reap = |t: &Tcb| -> bool {
            t.id != current_id
                && t.state == TaskState::Terminated
                && (force || now.saturating_sub(t.terminated_at_tick) >= REAP_GRACE_TICKS)
        };

        for tcb in self.tasks.iter_mut() {
            if !should_reap(tcb) {
                continue;
            }
            if let Some(process) = tcb.process.as_mut() {
                if let Some(space) = process.address_space.take() {
                    // Safety: `should_reap` confirmed `tcb.id != current_id`,
                    // so this address space cannot be the active CR3.
                    unsafe { paging::free_address_space(space) };
                }
            }
        }

        self.tasks.retain(|t| !should_reap(t));
        self.current = self
            .tasks
            .iter()
            .position(|t| t.id == current_id)
            .expect("current task vanished during reap");
    }

    /// Decide whether a switch is needed and, if so, everything about it
    /// -- but do not perform any of it (see `SwitchPlan`'s docs).
    fn prepare_switch(&mut self) -> Option<SwitchPlan> {
        self.reap_terminated(false);

        let n = self.tasks.len();
        if n < 2 {
            return None;
        }

        if self.tasks[self.current].state == TaskState::Running {
            self.tasks[self.current].state = TaskState::Ready;
        }

        let requested = FOREGROUND_WAKE_ID.swap(0, Ordering::AcqRel);
        let mut next = if requested != 0 {
            self.tasks
                .iter()
                .position(|task| {
                    task.id == requested
                        && task.state == TaskState::Ready
                        && task.id != self.tasks[self.current].id
                })
                .unwrap_or(self.current)
        } else {
            self.current
        };
        if next == self.current {
            for offset in 1..=n {
                let idx = (self.current + offset) % n;
                if self.tasks[idx].state == TaskState::Ready {
                    next = idx;
                    break;
                }
            }
        }

        if next == self.current {
            // Nothing else runnable; keep going unless this task just
            // terminated itself (see `exit`), in which case there is
            // truly nothing left to do but let the caller's fallback halt.
            if self.tasks[self.current].state != TaskState::Terminated {
                self.tasks[self.current].state = TaskState::Running;
            }
            return None;
        }

        // Safe to take ownership of the outgoing task's address space here
        // (nothing else can reach it once we're past this point), but not
        // safe to actually free its frames until `new_cr3` below has
        // genuinely been loaded -- CR3 may still be pointing at it.
        let reclaim = if self.tasks[self.current].state == TaskState::Terminated {
            self.tasks[self.current]
                .process
                .as_mut()
                .and_then(|p| p.address_space.take())
        } else {
            None
        };

        let old_ptr: *mut u64 = &mut self.tasks[self.current].saved_rsp;
        let new_val = self.tasks[next].saved_rsp;
        let new_cr3 = self.tasks[next]
            .process
            .as_ref()
            .and_then(|p| p.address_space.as_ref())
            .map(paging::AddressSpace::pml4_frame)
            .unwrap_or_else(|| {
                paging::kernel_pml4_frame().expect("kernel pml4 frame not recorded")
            });
        let new_kernel_stack_top = self.tasks[next].kernel_stack_top;

        self.tasks[next].state = TaskState::Running;
        self.current = next;

        Some(SwitchPlan {
            old_rsp_ptr: old_ptr,
            new_rsp: new_val,
            new_cr3,
            new_kernel_stack_top,
            reclaim,
        })
    }
}

static SCHEDULER: Mutex<Option<Scheduler>> = Mutex::new(None);
static NEXT_ID: AtomicU32 = AtomicU32::new(3); // 1 = shell, 2 = idle
static TICKS_SINCE_SWITCH: AtomicU64 = AtomicU64::new(0);
static FOREGROUND_WAKE_ID: AtomicU32 = AtomicU32::new(0);
static VM_BATCH_COUNT: AtomicU64 = AtomicU64::new(0);
static VM_BATCH_TOTAL_CYCLES: AtomicU64 = AtomicU64::new(0);
static VM_BATCH_MAX_CYCLES: AtomicU64 = AtomicU64::new(0);
static VM_BATCH_MAX_KIND: AtomicU32 = AtomicU32::new(0);
static MMAP_FAIL_AFTER_PAGES: AtomicU64 = AtomicU64::new(u64::MAX);

/// The only sanctioned way to touch `SCHEDULER`. Every call site used to
/// take `SCHEDULER.lock()` directly; several (`init`, `spawn`, `list`,
/// `info`, `kill`) did so with interrupts still enabled. On a single-core
/// kernel that is a real interrupt-reentrancy deadlock, not a theoretical
/// one: `on_timer_tick` (called from the timer ISR -- see
/// `interrupts::timer_interrupt_handler`) also locks `SCHEDULER`, and
/// `spin::Mutex` is not reentrant. If the timer fires while, say, `kill`
/// holds the lock, the ISR spins forever waiting for a lock owned by the
/// exact context it just interrupted -- which can never run again to
/// release it, because the CPU is stuck spinning in the ISR instead.
/// Routing every access through this one function makes that mistake
/// structurally impossible to reintroduce at a new call site.
///
/// Safe to call from interrupt context too: `without_interrupts` only
/// disables/restores the flag it itself changed, so nesting (this being
/// called from `on_timer_tick`, which is already running with IF clear)
/// is a correct no-op rather than a bug.
fn with_scheduler<F, R>(f: F) -> R
where
    F: FnOnce(&mut Option<Scheduler>) -> R,
{
    x86_64::instructions::interrupts::without_interrupts(|| {
        let mut guard = SCHEDULER.lock();
        f(&mut guard)
    })
}

fn with_vm_batch<F, R>(kind: u32, f: F) -> R
where
    F: FnOnce(&mut Option<Scheduler>) -> R,
{
    let started = unsafe { core::arch::x86_64::_rdtsc() };
    let result = with_scheduler(f);
    let cycles = unsafe { core::arch::x86_64::_rdtsc() }.saturating_sub(started);
    record_vm_batch_cycles(cycles, kind);
    result
}

fn record_vm_batch_cycles(cycles: u64, kind: u32) {
    VM_BATCH_COUNT.fetch_add(1, Ordering::Relaxed);
    VM_BATCH_TOTAL_CYCLES.fetch_add(cycles, Ordering::Relaxed);
    let mut current = VM_BATCH_MAX_CYCLES.load(Ordering::Relaxed);
    while cycles > current {
        match VM_BATCH_MAX_CYCLES.compare_exchange_weak(
            current,
            cycles,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => {
                VM_BATCH_MAX_KIND.store(kind, Ordering::Relaxed);
                break;
            }
            Err(observed) => current = observed,
        }
    }
}

#[derive(Clone, Copy)]
pub struct VmBatchTelemetry {
    pub count: u64,
    pub total_cycles: u64,
    pub max_cycles: u64,
    pub max_kind: u32,
}

pub fn reset_vm_batch_telemetry() {
    VM_BATCH_COUNT.store(0, Ordering::Relaxed);
    VM_BATCH_TOTAL_CYCLES.store(0, Ordering::Relaxed);
    VM_BATCH_MAX_CYCLES.store(0, Ordering::Relaxed);
    VM_BATCH_MAX_KIND.store(0, Ordering::Relaxed);
}

pub fn vm_batch_telemetry() -> VmBatchTelemetry {
    VmBatchTelemetry {
        count: VM_BATCH_COUNT.load(Ordering::Relaxed),
        total_cycles: VM_BATCH_TOTAL_CYCLES.load(Ordering::Relaxed),
        max_cycles: VM_BATCH_MAX_CYCLES.load(Ordering::Relaxed),
        max_kind: VM_BATCH_MAX_KIND.load(Ordering::Relaxed),
    }
}

/// Arm one deterministic, kernel-internal MMAP rollback test. This is not
/// reachable from the syscall ABI; the shell acceptance command uses it once
/// before starting the dedicated hostile ELF.
pub fn inject_next_mmap_failure_after(mapped_pages: u64) {
    MMAP_FAIL_AFTER_PAGES.store(mapped_pages, Ordering::Release);
}

/// Disarm the kernel-internal MMAP rollback test hook. The shell calls this
/// on every completion path so a failed process spawn can never leave a
/// latent failure armed for an unrelated userspace process.
pub fn clear_mmap_failure_injection() {
    MMAP_FAIL_AFTER_PAGES.store(u64::MAX, Ordering::Release);
}

// Safety: `context_switch` only touches the callee-saved registers SysV
// requires a normal `extern "C"` call to preserve (see the module docs),
// plus `rsp` itself, which is the whole point. `task_trampoline` is the
// landing point the very first time a freshly spawned task runs (see
// `spawn`): it reads the entry-point function pointer `spawn` stashed in
// `r15`'s saved slot, calls it, and falls through to `task_exit_trampoline`
// if it ever returns.
core::arch::global_asm!(
    r#"
.global context_switch
context_switch:
    push rbp
    push rbx
    push r12
    push r13
    push r14
    push r15
    mov [rdi], rsp
    mov rsp, rsi
    pop r15
    pop r14
    pop r13
    pop r12
    pop rbx
    pop rbp
    ret

.global task_trampoline
task_trampoline:
    sti
    call r15
    call {task_exit}
2:
    hlt
    jmp 2b

.global user_task_trampoline
user_task_trampoline:
    sti
    call {user_entry}
    call {task_exit}
3:
    hlt
    jmp 3b
"#,
    task_exit = sym task_exit_trampoline,
    user_entry = sym rust_user_entry,
);

extern "C" fn task_exit_trampoline() {
    exit();
}

/// Landing point for `user_task_trampoline`: reads back this now-running
/// task's own entry point and user stack top (stashed in its `Tcb` by
/// `spawn_user_process`, since -- unlike the plain kernel `task_trampoline`
/// -- a user task needs more than one word of context smuggled through the
/// fake initial stack frame) and drops to Ring 3.
extern "C" fn rust_user_entry() {
    let (entry_point, user_stack_top) =
        current_process_entry().expect("user_task_trampoline entered without process info");
    // Safety: `entry_point` was validated by `elf::load` to lie within the
    // permitted user address range and mapped `USER_ACCESSIBLE`;
    // `user_stack_top` is the top of a dedicated `WRITABLE | USER_ACCESSIBLE`
    // stack range in this same process's address space (`spawn_user_process`).
    // `schedule()` already loaded this process's own CR3 and this task's own
    // RSP0 before resuming it, so both addresses resolve correctly and any
    // trap lands on the right kernel stack.
    unsafe {
        crate::usermode::enter_ring3(
            entry_point,
            user_stack_top,
            gdt::user_code_selector().0 as u64,
            gdt::user_data_selector().0 as u64,
        )
    }
}

extern "C" {
    fn context_switch(old_rsp: *mut u64, new_rsp: u64);
    fn task_trampoline();
    fn user_task_trampoline();
}

/// Create the built-in tasks (`shell`, `idle`) and the `heartbeat` demo
/// task before the shell starts, then bring the scheduler online.
pub fn init() {
    let shell = Tcb {
        id: 1,
        name: String::from("shell"),
        state: TaskState::Running,
        stack: None,
        saved_rsp: 0,
        wake_at_tick: 0,
        kernel_stack_top: 0,
        process: None,
        terminated_at_tick: 0,
    };
    let idle = new_tcb(2, "idle", idle_entry);

    let mut sched = Scheduler {
        // Reserve the complete bounded task table during trusted boot. A
        // Ring-3 SPAWN can never force this vector to grow in a syscall.
        tasks: Vec::with_capacity(MAX_TASKS),
        current: 0,
    };
    sched.tasks.push(Box::new(shell));
    sched.tasks.push(Box::new(idle));
    with_scheduler(|slot| *slot = Some(sched));

    // Concrete, observable proof that Phase 3 is real: this task sleeps
    // and logs a heartbeat over serial roughly once a second. Watching its
    // count climb in the serial log *while the shell stays interactively
    // responsive* is the demonstration the project's engineering rules
    // require before this phase counts as done -- not just "it compiles".
    spawn("heartbeat", heartbeat_entry);
}

fn new_tcb(id: u32, name: &str, entry: fn()) -> Tcb {
    let mut stack = Box::new([0u8; STACK_SIZE]);
    let stack_top = unsafe { stack.as_mut_ptr().add(STACK_SIZE) as usize };
    let aligned_top = stack_top & !0xF; // 16-byte align, matching the SysV stack ABI
    let mut sp = aligned_top;
    sp -= INITIAL_FRAME_SIZE;

    // Safety: `sp` was just computed from a freshly allocated, 16-byte
    // aligned, INITIAL_FRAME_SIZE-larger-than-needed buffer that nothing
    // else references yet, so writing these 7 words is in-bounds and
    // exclusive. The layout matches context_switch's pop order exactly:
    // whichever value ends up under `r15`'s slot is what `task_trampoline`
    // (see the asm above) will find in the real r15 register the moment
    // it starts running, which is how the entry point is smuggled through
    // a context switch that otherwise doesn't know anything about tasks.
    unsafe {
        let base = sp as *mut u64;
        base.add(0).write(entry as usize as u64); // -> r15 (entry fn ptr)
        base.add(1).write(0); // -> r14
        base.add(2).write(0); // -> r13
        base.add(3).write(0); // -> r12
        base.add(4).write(0); // -> rbx
        base.add(5).write(0); // -> rbp
        base.add(6).write(task_trampoline as *const () as u64); // return address for `ret`
    }

    Tcb {
        id,
        name: String::from(name),
        state: TaskState::Ready,
        stack: Some(stack),
        saved_rsp: sp as u64,
        wake_at_tick: 0,
        kernel_stack_top: aligned_top as u64,
        process: None,
        terminated_at_tick: 0,
    }
}

fn try_kernel_stack() -> Result<Box<[u8; STACK_SIZE]>, &'static str> {
    let layout = core::alloc::Layout::new::<[u8; STACK_SIZE]>();
    // `alloc_zeroed` is used directly so exhaustion is an ordinary error
    // rather than `Box::new` invoking the kernel's fatal allocation handler.
    let pointer = unsafe { alloc::alloc::alloc_zeroed(layout) };
    if pointer.is_null() {
        return Err("kernel stack allocation failed");
    }
    // Safety: `pointer` came from `alloc_zeroed` with exactly this array's
    // layout and is uniquely owned. `Box` will return it through the same
    // global allocator on every later success or rollback path.
    Ok(unsafe { Box::from_raw(pointer.cast::<[u8; STACK_SIZE]>()) })
}

fn try_owned_string(value: &str) -> Result<String, &'static str> {
    let mut owned = String::new();
    owned
        .try_reserve_exact(value.len())
        .map_err(|_| "process string allocation failed")?;
    owned.push_str(value);
    Ok(owned)
}

fn try_user_process_resources(name: &str, cwd: &str) -> Result<UserProcessResources, &'static str> {
    let stack = try_kernel_stack()?;
    let name = try_owned_string(name)?;
    let cwd = try_owned_string(cwd)?;
    let mut open_files = Vec::new();
    open_files
        .try_reserve_exact(MAX_OPEN_FILES)
        .map_err(|_| "file table allocation failed")?;
    Ok(UserProcessResources {
        name,
        cwd,
        stack,
        open_files,
    })
}

fn try_box_value<T>(value: T) -> Result<Box<T>, T> {
    let layout = core::alloc::Layout::new::<T>();
    // Safety: a non-null result is valid and suitably aligned for `T`.
    let pointer = unsafe { alloc::alloc::alloc(layout) }.cast::<T>();
    if pointer.is_null() {
        return Err(value);
    }
    // Safety: the allocation has exactly `T`'s layout, is uniquely owned,
    // and is initialized once before ownership transfers to `Box`.
    unsafe {
        pointer.write(value);
        Ok(Box::from_raw(pointer))
    }
}

/// Build the Tcb for a genuine user process: same kernel-stack setup as
/// `new_tcb`, except the fake initial frame lands in `user_task_trampoline`
/// (which drops to Ring 3 via `enter_ring3`) instead of `task_trampoline`
/// (which calls a plain kernel `fn()`), and `r15` goes unused -- the entry
/// point and user stack top live in `process.entry_point`/`user_stack_top`
/// instead, read back via `current_process_entry()` once this task is
/// actually running (see `rust_user_entry`).
fn new_user_tcb(
    id: u32,
    resources: UserProcessResources,
    address_space: paging::AddressSpace,
    entry_point: u64,
    user_stack_top: u64,
) -> Tcb {
    let UserProcessResources {
        name,
        cwd,
        mut stack,
        open_files,
    } = resources;
    let stack_top = unsafe { stack.as_mut_ptr().add(STACK_SIZE) as usize };
    let aligned_top = stack_top & !0xF;
    let mut sp = aligned_top;
    sp -= INITIAL_FRAME_SIZE;

    // Safety: see `new_tcb` -- identical reasoning, just landing in
    // `user_task_trampoline` and leaving r15 unused (0).
    unsafe {
        let base = sp as *mut u64;
        base.add(0).write(0); // -> r15 (unused)
        base.add(1).write(0); // -> r14
        base.add(2).write(0); // -> r13
        base.add(3).write(0); // -> r12
        base.add(4).write(0); // -> rbx
        base.add(5).write(0); // -> rbp
        base.add(6).write(user_task_trampoline as *const () as u64);
    }

    Tcb {
        id,
        name,
        state: TaskState::Ready,
        stack: Some(stack),
        saved_rsp: sp as u64,
        wake_at_tick: 0,
        kernel_stack_top: aligned_top as u64,
        process: Some(ProcessState {
            address_space: Some(address_space),
            entry_point,
            user_stack_top,
            exit_code: None,
            mmap_next: paging::USER_MMAP_BASE,
            cwd,
            open_files,
            open_file_bytes: 0,
        }),
        terminated_at_tick: 0,
    }
}

fn idle_entry() {
    loop {
        crate::interrupts::halt();
    }
}

fn heartbeat_entry() {
    let mut count: u64 = 0;
    loop {
        sleep_ticks(100); // ~1 second at the PIT's 100 Hz tick rate
        count += 1;
        crate::serial_println!("task heartbeat: beat #{}", count);
    }
}

/// Start a new task running `entry` from the beginning. Returns its id.
pub fn spawn(name: &str, entry: fn()) -> u32 {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let tcb = new_tcb(id, name, entry);
    with_scheduler(|slot| {
        let sched = slot.as_mut().expect("scheduler not initialized");
        sched.tasks.push(Box::new(tcb));
    });
    id
}

/// Request one latency-sensitive scheduling choice for a foreground process.
/// The request is consumed exactly once by `prepare_switch`; it does not alter
/// the timer quantum or permanently prioritize the task over round-robin peers.
pub fn request_foreground_wake(id: u32) {
    FOREGROUND_WAKE_ID.store(id, Ordering::Release);
    // Ask the next 100 Hz PIT interrupt to run the scheduler instead of
    // waiting out the current task's remaining 50 ms quantum. The request is
    // still consumed once in `prepare_switch`; if `id` is already current,
    // ordinary round-robin selection applies and continuous input cannot pin
    // the desktop on CPU.
    TICKS_SINCE_SWITCH.store(TIME_SLICE_TICKS, Ordering::Release);
}

/// Called from `interrupts::timer_interrupt_handler` on every PIT tick.
/// Wakes any `Blocked` task whose sleep has elapsed, then preempts into
/// the scheduler once a full time slice has passed.
pub fn on_timer_tick() {
    let now = crate::interrupts::ticks();
    with_scheduler(|slot| {
        if let Some(sched) = slot.as_mut() {
            for task in sched.tasks.iter_mut() {
                if task.state == TaskState::Blocked
                    && task.wake_at_tick != 0
                    && now >= task.wake_at_tick
                {
                    task.state = TaskState::Ready;
                    task.wake_at_tick = 0;
                }
            }
        }
    });

    if TICKS_SINCE_SWITCH.fetch_add(1, Ordering::Relaxed) + 1 >= TIME_SLICE_TICKS {
        TICKS_SINCE_SWITCH.store(0, Ordering::Relaxed);
        schedule();
    }
}

/// Perform a scheduling decision and, if warranted, a real context switch
/// -- now including the CR3 and TSS-RSP0 half of that switch, which is
/// what makes any number of user processes safe under real preemption
/// (Phase 4): every switch loads the incoming task's own address space
/// (its private one if it's a user process, the kernel's shared one
/// otherwise) and its own kernel stack top into RSP0 *before* the
/// register/stack-pointer switch happens, so a Ring 3 -> Ring 0 trap for whichever
/// task ends up running always lands in the right place. Safe to call from
/// both interrupt context (the timer ISR already runs with interrupts
/// disabled) and normal context (`yield_now`/`sleep_ticks`/a syscall
/// below, where `without_interrupts` prevents a reentrant tick from
/// corrupting the switch in progress).
pub fn schedule() {
    // Note: this wraps strictly more than the `SCHEDULER` lock itself --
    // `context_switch` below must also run with interrupts continuously
    // disabled (a timer tick landing mid-switch, while `rsp` points
    // somewhere between two tasks' stacks, would be a genuine hazard), so
    // it cannot go through `with_scheduler` alone. `without_interrupts`
    // nests safely with the one inside `with_scheduler` (see its doc
    // comment), so this stays correct either way.
    x86_64::instructions::interrupts::without_interrupts(|| {
        let plan = with_scheduler(|slot| slot.as_mut().and_then(Scheduler::prepare_switch));
        if let Some(plan) = plan {
            // Safety: `new_cr3` is either the kernel's own permanent root
            // or a process's `AddressSpace` that is about to become (or
            // remain) the current task -- both are valid, fully populated
            // PML4s. This runs while still executing as the *outgoing*
            // task, on its own stack, which is safe: kernel mappings are
            // identical across every address space (see
            // `paging::new_address_space`), so nothing this function does
            // between here and `context_switch` below is affected by which
            // one is loaded.
            unsafe { paging::switch_to(plan.new_cr3) };

            // A `kernel_stack_top` of 0 only ever belongs to the boot task
            // (id 1, "shell"), which never runs Ring 3 code -- RSP0's value
            // is simply never consulted while it's current, so it's left
            // alone rather than overwritten with a meaningless 0.
            if plan.new_kernel_stack_top != 0 {
                gdt::set_kernel_stack(VirtAddr::new(plan.new_kernel_stack_top));
            }

            if let Some(space) = plan.reclaim {
                // Safety: `new_cr3` was just loaded above, so this
                // reclaimed address space's PML4 is provably no longer the
                // active CR3 -- see `free_address_space`'s contract.
                unsafe { paging::free_address_space(space) };
            }

            // Safety: `old_rsp_ptr` points at the currently-running task's
            // own `saved_rsp` field (a stable heap address behind
            // `Box<Tcb>`, unaffected by the `Vec` it lives in
            // reallocating), and `new_rsp` was populated either by a
            // previous `context_switch` call or by a fake initial frame
            // (`new_tcb`/`new_user_tcb`) -- either way, a valid stack
            // pointer for `context_switch` to resume from.
            unsafe { context_switch(plan.old_rsp_ptr, plan.new_rsp) };
        }
    });
}

/// Voluntarily give up the remaining time slice.
pub fn yield_now() {
    TICKS_SINCE_SWITCH.store(0, Ordering::Relaxed);
    schedule();
}

/// Block the current task until at least `ticks` PIT ticks have passed.
pub fn sleep_ticks(ticks: u64) {
    let wake_at = crate::interrupts::ticks() + ticks;
    with_scheduler(|slot| {
        if let Some(sched) = slot.as_mut() {
            let idx = sched.current;
            sched.tasks[idx].state = TaskState::Blocked;
            sched.tasks[idx].wake_at_tick = wake_at;
        }
    });
    schedule();
}

/// Terminate the current task with exit code 0. Never returns.
pub fn exit() -> ! {
    exit_with_code(0)
}

/// Terminate the current task, recording `code` as its exit status if it's
/// a user process (see `Task::exit_code`; a no-op for kernel-only tasks,
/// which have nowhere to show it). Never returns: the next `schedule()`
/// call switches away from it permanently (a `Terminated` task is never
/// chosen by `prepare_switch` again), and if it owned a private address
/// space, that same `schedule()` call frees it once CR3 has moved off it
/// (see `Scheduler::prepare_switch` / `schedule`'s docs).
pub fn exit_with_code(code: i32) -> ! {
    with_scheduler(|slot| {
        if let Some(sched) = slot.as_mut() {
            let idx = sched.current;
            sched.tasks[idx].state = TaskState::Terminated;
            sched.tasks[idx].terminated_at_tick = crate::interrupts::ticks();
            if let Some(process) = sched.tasks[idx].process.as_mut() {
                process.exit_code = Some(code);
            }
        }
    });
    loop {
        schedule();
        // Only reachable if `schedule` found nothing else runnable, which
        // should not happen (`idle` never terminates) -- halt rather than
        // spin if it somehow does.
        x86_64::instructions::hlt();
    }
}

fn snapshot(t: &Tcb) -> Task {
    Task {
        id: t.id,
        name: t.name.clone(),
        state: t.state,
        privilege: if t.process.is_some() {
            Privilege::User
        } else {
            Privilege::Kernel
        },
        exit_code: t.process.as_ref().and_then(|p| p.exit_code),
    }
}

/// List all tasks for the `ps` command -- genuine scheduler state, not a
/// static table.
pub fn list() -> Result<Vec<Task>, &'static str> {
    with_scheduler(|slot| {
        let sched = slot.as_ref().ok_or("scheduler not initialized")?;
        Ok(sched.tasks.iter().map(|t| snapshot(t)).collect())
    })
}

/// Number of `Tcb`s currently in the scheduler's task list -- including
/// `Terminated` ones still inside their reap grace period. Diagnostic/test
/// use (`stress` shell command, reap tests): proves the count genuinely
/// shrinks back down after a batch of spawn/exit/reap cycles rather than
/// growing without bound.
pub fn task_count() -> usize {
    with_scheduler(|slot| slot.as_ref().map(|s| s.tasks.len()).unwrap_or(0))
}

/// Detailed information about one task.
pub fn info(id: u32) -> Result<Task, &'static str> {
    with_scheduler(|slot| {
        let sched = slot.as_ref().ok_or("scheduler not initialized")?;
        sched
            .tasks
            .iter()
            .find(|t| t.id == id)
            .map(|t| snapshot(t))
            .ok_or("task not found")
    })
}

/// Terminate a task by id. The killed task stops being scheduled starting
/// with the next `schedule()` call -- a real effect, not a cosmetic state
/// change.
///
/// If `id` owns a private address space and is *not* the currently running
/// task, its frames are freed immediately: CR3 can only ever equal the
/// current task's own address space, so a non-current task's is provably
/// already inactive. If `id` *is* the current task (killing yourself, or
/// -- more realistically -- fault-isolation recovery in `interrupts.rs`
/// calling `exit_with_code` on itself), the address space is left in place
/// for `Scheduler::prepare_switch` to reclaim right after the next context
/// switch genuinely moves CR3 off it (see `schedule()`).
pub fn kill(id: u32) -> Result<(), &'static str> {
    if id == 1 {
        return Err("cannot kill shell task");
    }
    let reclaim = with_scheduler(|slot| {
        let sched = slot.as_mut().ok_or("scheduler not initialized")?;
        let is_current = sched.tasks[sched.current].id == id;
        match sched.tasks.iter_mut().find(|t| t.id == id) {
            Some(task) => {
                if task.state == TaskState::Terminated {
                    return Err("task already terminated");
                }
                task.state = TaskState::Terminated;
                task.terminated_at_tick = crate::interrupts::ticks();
                if is_current {
                    Ok(None)
                } else {
                    Ok(task.process.as_mut().and_then(|p| p.address_space.take()))
                }
            }
            None => Err("task not found"),
        }
    })?;


    if let Some(space) = reclaim {
        // Safety: confirmed above that `id` was not the current task's id,
        // so its address space cannot be the active CR3.
        unsafe { paging::free_address_space(space) };
    }

    Ok(())
}

pub fn state_label(state: TaskState) -> &'static str {
    state.label()
}

/// Immediately reap every `Terminated` non-current task, ignoring
/// `REAP_GRACE_TICKS` -- the `reap` shell command's implementation, and the
/// deterministic hook stress tests use (`spawn`/wait/`kill` a batch of
/// processes, call this once, then compare `paging::frame_stats()`/task
/// count against a recorded baseline) instead of needing to either wait out
/// the real grace period or depend on wall-clock timing for a repeatable
/// result. Organic reaping (`Scheduler::prepare_switch`, every scheduler
/// decision) already does the same thing automatically once a task has
/// been `Terminated` long enough; this only changes *when* it happens, not
/// *what* happens or *how* it's made safe.
pub fn reap_now() {
    with_scheduler(|slot| {
        if let Some(sched) = slot.as_mut() {
            sched.reap_terminated(true);
        }
    });
}

pub fn privilege_label(privilege: Privilege) -> &'static str {
    privilege.label()
}

/// The currently running task's own id -- used by `syscall::sys_getpid`
/// (a process's task id *is* its pid in this minimal model) and by
/// `copy_from_current_user` to find its address space.
pub fn current_task_id() -> Option<u32> {
    with_scheduler(|slot| {
        let sched = slot.as_ref()?;
        Some(sched.tasks[sched.current].id)
    })
}

pub fn current_process_name() -> Option<String> {
    let (bytes, len) = with_scheduler(|slot| {
        let sched = slot.as_ref()?;
        let task = &sched.tasks[sched.current];
        task.process.as_ref()?;
        if task.name.len() > crate::vfs::NAME_MAX {
            return None;
        }
        let mut bytes = [0u8; crate::vfs::NAME_MAX];
        bytes[..task.name.len()].copy_from_slice(task.name.as_bytes());
        Some((bytes, task.name.len()))
    })?;
    try_owned_string(core::str::from_utf8(&bytes[..len]).ok()?).ok()
}

pub fn current_working_directory() -> Option<String> {
    let (bytes, len) = with_scheduler(|slot| {
        let sched = slot.as_ref()?;
        let cwd = &sched.tasks[sched.current].process.as_ref()?.cwd;
        if cwd.len() > crate::vfs::PATH_MAX {
            return None;
        }
        let mut bytes = [0u8; crate::vfs::PATH_MAX];
        bytes[..cwd.len()].copy_from_slice(cwd.as_bytes());
        Some((bytes, cwd.len()))
    })?;
    try_owned_string(core::str::from_utf8(&bytes[..len]).ok()?).ok()
}

pub fn set_current_working_directory(cwd: String) -> bool {
    with_scheduler(|slot| {
        let sched = slot.as_mut()?;
        sched.tasks[sched.current].process.as_mut()?.cwd = cwd;
        Some(())
    })
    .is_some()
}

pub fn open_file_for_current_process(data: Arc<[u8]>) -> Option<u32> {
    with_scheduler(|slot| {
        let sched = slot.as_mut()?;
        let process = sched.tasks[sched.current].process.as_mut()?;
        let total = process.open_file_bytes.checked_add(data.len())?;
        if total > MAX_OPEN_FILE_BYTES {
            return None;
        }
        if let Some(index) = process.open_files.iter().position(Option::is_none) {
            process.open_files[index] = Some(OpenFile { data, offset: 0 });
            process.open_file_bytes = total;
            return u32::try_from(index).ok()?.checked_add(FIRST_FILE_HANDLE);
        }
        if process.open_files.len() >= MAX_OPEN_FILES {
            return None;
        }
        let index = process.open_files.len();
        process.open_files.push(Some(OpenFile { data, offset: 0 }));
        process.open_file_bytes = total;
        u32::try_from(index).ok()?.checked_add(FIRST_FILE_HANDLE)
    })
}

pub fn peek_file_for_current_process(
    handle: u32,
    max_len: usize,
) -> Option<(Arc<[u8]>, usize, usize)> {
    let index = usize::try_from(handle.checked_sub(FIRST_FILE_HANDLE)?).ok()?;
    with_scheduler(|slot| {
        let sched = slot.as_mut()?;
        let file = sched.tasks[sched.current]
            .process
            .as_ref()?
            .open_files
            .get(index)?
            .as_ref()?;
        let end = file.offset.checked_add(max_len)?.min(file.data.len());
        Some((Arc::clone(&file.data), file.offset, end))
    })
}

pub fn advance_file_for_current_process(handle: u32, amount: usize) -> bool {
    let Some(index) = handle
        .checked_sub(FIRST_FILE_HANDLE)
        .and_then(|value| usize::try_from(value).ok())
    else {
        return false;
    };
    with_scheduler(|slot| {
        let sched = slot.as_mut()?;
        let file = sched.tasks[sched.current]
            .process
            .as_mut()?
            .open_files
            .get_mut(index)?
            .as_mut()?;
        let new_offset = file.offset.checked_add(amount)?;
        if new_offset > file.data.len() {
            return None;
        }
        file.offset = new_offset;
        Some(())
    })
    .is_some()
}

pub fn seek_file_for_current_process(handle: u32, offset: usize) -> bool {
    let Some(index) = handle
        .checked_sub(FIRST_FILE_HANDLE)
        .and_then(|value| usize::try_from(value).ok())
    else {
        return false;
    };
    with_scheduler(|slot| {
        let sched = slot.as_mut()?;
        let file = sched.tasks[sched.current]
            .process
            .as_mut()?
            .open_files
            .get_mut(index)?
            .as_mut()?;
        if offset > file.data.len() {
            return None;
        }
        file.offset = offset;
        Some(())
    })
    .is_some()
}

pub fn close_file_for_current_process(handle: u32) -> bool {
    let Some(index) = handle
        .checked_sub(FIRST_FILE_HANDLE)
        .and_then(|value| usize::try_from(value).ok())
    else {
        return false;
    };
    with_scheduler(|slot| {
        let sched = slot.as_mut()?;
        let process = sched.tasks[sched.current].process.as_mut()?;
        let slot = process.open_files.get_mut(index)?;
        if slot.is_none() {
            return None;
        }
        let file = slot.take()?;
        process.open_file_bytes = process.open_file_bytes.checked_sub(file.data.len())?;
        Some(())
    })
    .is_some()
}

/// This now-running user task's entry point and user stack top, stashed by
/// `spawn_user_process` -- see `rust_user_entry`.
fn current_process_entry() -> Option<(u64, u64)> {
    with_scheduler(|slot| {
        let sched = slot.as_ref()?;
        let process = sched.tasks[sched.current].process.as_ref()?;
        Some((process.entry_point, process.user_stack_top))
    })
}

/// Copy `len` bytes out of the *currently running* task's own user memory
/// starting at `addr`, or `None` if the current task isn't a user process
/// at all, or if any byte in range fails validation (unmapped, not
/// user-accessible, or `addr + len` overflows) -- see
/// `paging::read_bytes_from_address_space`, which this is a thin,
/// current-task-scoped wrapper around. This is the only way `syscall.rs`
/// ever reads memory a Ring 3 program pointed it at: never a raw pointer
/// dereference of a user-supplied address.
pub fn copy_from_current_user(addr: u64, len: usize) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    out.try_reserve_exact(len).ok()?;
    out.resize(len, 0);
    with_scheduler(|slot| {
        let user_addr = VirtAddr::try_new(addr).ok()?;
        if user_addr.as_u64() != addr {
            return None;
        }
        let sched = slot.as_ref()?;
        let space = sched.tasks[sched.current]
            .process
            .as_ref()?
            .address_space
            .as_ref()?;
        paging::read_bytes_from_address_space_into(space, user_addr, &mut out).ok()
    })?;
    Some(out)
}

/// Copy `data` into the *currently running* task's own user memory at
/// `addr` -- the write-direction counterpart to `copy_from_current_user`,
/// used by syscalls that hand kernel-computed data back to userspace
/// through a caller-supplied destination pointer (`DISPLAY_INFO`,
/// `INPUT_POLL`). Goes through `paging::write_bytes_in_address_space`,
/// which (as of Phase 5) requires both `WRITABLE` and `USER_ACCESSIBLE` on
/// every page touched -- a destination that resolves to kernel memory
/// (`WRITABLE` but never `USER_ACCESSIBLE`) is rejected, not silently
/// written through. Returns `false` if the current task isn't a user
/// process or if any byte in range fails validation.
pub fn copy_to_current_user(addr: u64, data: &[u8]) -> bool {
    with_scheduler(|slot| {
        let user_addr = VirtAddr::try_new(addr).ok()?;
        if user_addr.as_u64() != addr {
            return None;
        }
        let sched = slot.as_ref()?;
        let space = sched.tasks[sched.current]
            .process
            .as_ref()?
            .address_space
            .as_ref()?;
        paging::write_bytes_in_address_space(space, user_addr, data).ok()
    })
    .is_some()
}

/// Validate an entire caller-supplied range in the currently running user
/// process without reading from or writing to it. `INPUT_POLL` uses this
/// before removing an event from the queue, including when the queue is empty,
/// so an invalid destination always reports `-1` and can never consume data.
pub fn validate_current_user_range(addr: u64, len: usize, writable: bool) -> bool {
    with_scheduler(|slot| {
        let sched = slot.as_ref()?;
        let space = sched.tasks[sched.current]
            .process
            .as_ref()?
            .address_space
            .as_ref()?;
        paging::validate_user_range(space, addr, len, writable).ok()
    })
    .is_some()
}

/// Round `len` up to a whole number of 4 KiB pages -- shared by
/// `mmap_in_current_process` and anywhere else that needs to turn a byte
/// count into a page count. `None` on overflow (an absurd `len` close to
/// `u64::MAX`), never a silently wrapped/truncated result.
fn page_count_for(len: u64) -> Option<u64> {
    if len == 0 {
        return None;
    }
    len.checked_add(0xFFF)
        .map(|rounded| (rounded & !0xFFF) / 4096)
}

/// Upper bound on a single `SYS_MMAP` request -- generous for the
/// desktop's own needs (a 1280x720x4-byte back-buffer is ~3.5 MiB) while
/// still bounding how much any one syscall can make the kernel map on a
/// caller's behalf.
pub const MAX_MMAP_LEN: u64 = 64 * 1024 * 1024;

/// Bound VM work done while the scheduler lock and interrupts are held. Long
/// syscalls explicitly enable interrupts between these batches and yield, so
/// a hostile maximum-size request cannot suppress the 100 Hz clock or starve
/// round-robin peers.
pub const VM_BATCH_PAGES: u64 = 4;

fn current_space_matches_mmap_start(process: &ProcessState, start: u64) -> bool {
    process.mmap_next == start && process.address_space.is_some()
}

fn rollback_mmap_or_exit(
    start: u64,
    mapped_pages: u64,
    requested_pages: u64,
    owned_frames_before: usize,
) {
    let mut rolled_back = 0u64;
    while rolled_back < mapped_pages {
        let batch = VM_BATCH_PAGES.min(mapped_pages - rolled_back);
        let batch_start = start + rolled_back * 4096;
        let restored = with_vm_batch(14, |slot| {
            let Some(sched) = slot.as_mut() else {
                return false;
            };
            let Some(space) = sched.tasks[sched.current]
                .process
                .as_mut()
                .and_then(|process| process.address_space.as_mut())
            else {
                return false;
            };
            paging::unmap_range_in_address_space_atomic(space, batch_start, batch).is_ok()
        });
        if !restored {
            crate::serial_println!(
                "mmap: rollback invariant failed at {:#x}; terminating caller fail-closed",
                batch_start
            );
            exit_with_code(255);
        }
        rolled_back += batch;
    }

    let exact = with_vm_batch(15, |slot| {
        let Some(sched) = slot.as_mut() else {
            return None;
        };
        let Some(space) = sched.tasks[sched.current]
            .process
            .as_mut()
            .and_then(|process| process.address_space.as_mut())
        else {
            return None;
        };
        paging::clean_up_empty_tables_in_range(space, start, requested_pages).ok()?;
        Some(space.frame_count())
    });
    let Some(owned_frames_after) = exact else {
        crate::serial_println!("mmap: page-table rollback cleanup failed; terminating caller");
        exit_with_code(255);
    };
    crate::serial_println!(
        "mmap: rollback frames before={} after={}",
        owned_frames_before,
        owned_frames_after
    );
    if owned_frames_after != owned_frames_before {
        crate::serial_println!("mmap: rollback ownership mismatch; terminating caller fail-closed");
        exit_with_code(255);
    }
}

/// `SYS_MMAP`'s implementation: grow the *currently running* user
/// process's own anonymous-memory arena by `len` bytes (rounded up to
/// whole pages) and return the new region's starting address, or `None` on
/// any failure (not a user process, `len` is zero/absurd, the arena is
/// exhausted, or the underlying mapping failed -- e.g. out of physical
/// frames).
///
/// Every mapped page is `PRESENT | USER_ACCESSIBLE`, `NO_EXECUTE`
/// unconditionally (this ABI has no concept of executable anonymous
/// memory -- see `ARCHITECTURE.md`), and `WRITABLE` only if `writable` is
/// set; the freshly mapped range is zeroed before the address is handed
/// back, so a process can never observe another process's (or its own
/// prior mapping's) leftover physical-memory contents. Bounded to
/// `paging::USER_MMAP_BASE..USER_MMAP_LIMIT` -- independently re-checked by
/// `paging::map_in_address_space` itself (defense in depth, same pattern
/// as the ELF loader), so this can never reach kernel memory or another
/// process's address space no matter what this function does or doesn't
/// check.
///
/// Mapping, zero-fill, and final-permission installation are transactional at
/// the syscall boundary: if any step fails, every leaf page installed for the
/// request is unmapped and reclaimed, newly empty paging-structure frames are
/// removed and reclaimed, and `mmap_next` is left unchanged. The rollback
/// verifies the address space owns exactly its pre-call frame count before
/// returning failure.
pub fn mmap_in_current_process(len: u64, writable: bool) -> Option<u64> {
    if len == 0 || len > MAX_MMAP_LEN {
        return None;
    }
    let pages = page_count_for(len)?;
    let region_len = pages.checked_mul(4096)?;

    let (start, end, worst_case_frames, owned_frames_before) = with_vm_batch(1, |slot| {
        let sched = slot.as_ref()?;
        let process = sched.tasks[sched.current].process.as_ref()?;
        let start = process.mmap_next;
        let end = start.checked_add(region_len)?;
        if end > paging::USER_MMAP_LIMIT {
            return None;
        }
        let first_2m = start >> 21;
        let last_2m = end.checked_sub(1)? >> 21;
        let p1_tables = last_2m.checked_sub(first_2m)?.checked_add(1)?;
        let worst_case_frames = pages.checked_add(p1_tables)?.checked_add(2)?;
        let worst_case_frames = usize::try_from(worst_case_frames).ok()?;
        let owned_frames_before = process.address_space.as_ref()?.frame_count();
        Some((start, end, worst_case_frames, owned_frames_before))
    })?;

    // Admission control is conservative and mutation-free. A competing
    // process may consume frames after this check once interrupts are enabled;
    // any resulting mid-map failure is fully rolled back below.
    if !paging::can_allocate_frames(worst_case_frames) {
        return None;
    }

    // Safety: `MMAP` is entered through an interrupt gate with IF cleared.
    // From this point onward no scheduler/paging borrow or lock escapes its
    // bounded closure, so timer/IRQ preemption between batches is safe. IRET
    // restores the caller's original flags on return.
    x86_64::instructions::interrupts::enable();

    // Prevalidate the complete destination before the first mutation.
    let mut checked_pages = 0u64;
    while checked_pages < pages {
        let batch = VM_BATCH_PAGES.min(pages - checked_pages);
        let batch_start = start + checked_pages * 4096;
        let clear = with_vm_batch(2, |slot| {
            let Some(sched) = slot.as_ref() else {
                return false;
            };
            let Some(process) = sched.tasks[sched.current].process.as_ref() else {
                return false;
            };
            if !current_space_matches_mmap_start(process, start) {
                return false;
            }
            let space = process.address_space.as_ref().unwrap();
            (0..batch).all(|offset| {
                VirtAddr::try_new(batch_start + offset * 4096)
                    .ok()
                    .and_then(|addr| paging::translate_in_address_space(space, addr))
                    .is_none()
            })
        });
        if !clear {
            return None;
        }
        checked_pages += batch;
    }

    let staging_flags = PageTableFlags::PRESENT
        | PageTableFlags::USER_ACCESSIBLE
        | PageTableFlags::NO_EXECUTE
        | PageTableFlags::WRITABLE;
    let mut mapped_pages = 0u64;
    while mapped_pages < pages {
        let fail_after = MMAP_FAIL_AFTER_PAGES.load(Ordering::Acquire);
        if fail_after != u64::MAX && mapped_pages >= fail_after {
            MMAP_FAIL_AFTER_PAGES.store(u64::MAX, Ordering::Release);
            rollback_mmap_or_exit(start, mapped_pages, pages, owned_frames_before);
            return None;
        }
        let batch = VM_BATCH_PAGES.min(pages - mapped_pages);
        let batch_start = start + mapped_pages * 4096;
        let mapped_now = with_vm_batch(3, |slot| {
            let Some(sched) = slot.as_mut() else {
                return 0;
            };
            let Some(process) = sched.tasks[sched.current].process.as_mut() else {
                return 0;
            };
            if !current_space_matches_mmap_start(process, start) {
                return 0;
            }
            let space = process.address_space.as_mut().unwrap();
            let mut done = 0u64;
            for offset in 0..batch {
                let Ok(addr) = VirtAddr::try_new(batch_start + offset * 4096) else {
                    break;
                };
                let page: Page<Size4KiB> = Page::containing_address(addr);
                if paging::map_in_address_space(space, page, staging_flags).is_err() {
                    break;
                }
                done += 1;
            }
            done
        });
        mapped_pages += mapped_now;
        if mapped_now != batch {
            rollback_mmap_or_exit(start, mapped_pages, pages, owned_frames_before);
            return None;
        }
    }

    let mut zeroed_pages = 0u64;
    while zeroed_pages < pages {
        let batch = VM_BATCH_PAGES.min(pages - zeroed_pages);
        let batch_start = start + zeroed_pages * 4096;
        let zeroed = with_vm_batch(4, |slot| {
            let Some(sched) = slot.as_mut() else {
                return false;
            };
            let Some(process) = sched.tasks[sched.current].process.as_mut() else {
                return false;
            };
            let Some(space) = process.address_space.as_mut() else {
                return false;
            };
            let Ok(addr) = VirtAddr::try_new(batch_start) else {
                return false;
            };
            paging::zero_bytes_in_address_space(space, addr, batch * 4096).is_ok()
        });
        if !zeroed {
            rollback_mmap_or_exit(start, mapped_pages, pages, owned_frames_before);
            return None;
        }
        zeroed_pages += batch;
    }

    if !writable {
        let final_flags =
            PageTableFlags::PRESENT | PageTableFlags::USER_ACCESSIBLE | PageTableFlags::NO_EXECUTE;
        let mut protected_pages = 0u64;
        while protected_pages < pages {
            let batch = VM_BATCH_PAGES.min(pages - protected_pages);
            let batch_start = start + protected_pages * 4096;
            let protected = with_vm_batch(5, |slot| {
                let Some(sched) = slot.as_mut() else {
                    return false;
                };
                let Some(space) = sched.tasks[sched.current]
                    .process
                    .as_mut()
                    .and_then(|process| process.address_space.as_mut())
                else {
                    return false;
                };
                for offset in 0..batch {
                    let Ok(addr) = VirtAddr::try_new(batch_start + offset * 4096) else {
                        return false;
                    };
                    let page: Page<Size4KiB> = Page::containing_address(addr);
                    if paging::update_flags_in_address_space(space, page, final_flags).is_err() {
                        return false;
                    }
                }
                true
            });
            if !protected {
                rollback_mmap_or_exit(start, mapped_pages, pages, owned_frames_before);
                return None;
            }
            protected_pages += batch;
        }
    }

    let committed = with_vm_batch(6, |slot| {
        let Some(sched) = slot.as_mut() else {
            return false;
        };
        let Some(process) = sched.tasks[sched.current].process.as_mut() else {
            return false;
        };
        if process.mmap_next != start {
            return false;
        }
        process.mmap_next = end;
        true
    });
    if !committed {
        rollback_mmap_or_exit(start, mapped_pages, pages, owned_frames_before);
        return None;
    }
    Some(start)
}

/// `SYS_MUNMAP`'s implementation: unmap `[ptr, ptr+len)` from the
/// *currently running* user process's own address space and return the
/// underlying physical frames to the global allocator. Both `ptr` and
/// `len` must be exact multiples of 4 KiB (no partial-page unmaps -- every
/// `SYS_MMAP` region already starts and ends on a page boundary, so a
/// well-behaved caller never needs anything else), and the entire range
/// must fall within `[paging::USER_MMAP_BASE, mmap_next)` -- the process's
/// own mmap arena, and never past how far it has actually grown -- which
/// rules out ever unmapping the ELF's own segments or the user stack (both
/// live outside the mmap arena entirely) by construction, not by a
/// case-by-case check.
pub fn munmap_in_current_process(ptr: u64, len: u64) -> bool {
    if len == 0 || ptr % 4096 != 0 || len % 4096 != 0 {
        return false;
    }
    let Some(end) = ptr.checked_add(len) else {
        return false;
    };

    let in_bounds = with_vm_batch(7, |slot| {
        let Some(sched) = slot.as_ref() else {
            return false;
        };
        let Some(process) = sched.tasks[sched.current].process.as_ref() else {
            return false;
        };
        ptr >= paging::USER_MMAP_BASE && end <= process.mmap_next && process.address_space.is_some()
    });
    if !in_bounds {
        return false;
    }

    // Safety: identical bounded-lock argument to `mmap_in_current_process`.
    x86_64::instructions::interrupts::enable();
    let pages = len / 4096;
    let mut records = Vec::with_capacity(pages as usize);
    let mut collected = 0u64;
    while collected < pages {
        let batch = VM_BATCH_PAGES.min(pages - collected);
        let batch_start = ptr + collected * 4096;
        let valid = with_vm_batch(8, |slot| {
            let Some(sched) = slot.as_ref() else {
                return false;
            };
            let Some(space) = sched.tasks[sched.current]
                .process
                .as_ref()
                .and_then(|process| process.address_space.as_ref())
            else {
                return false;
            };
            paging::collect_unmap_records(space, batch_start, batch, &mut records).is_ok()
        });
        if !valid {
            return false;
        }
        collected += batch;
    }

    let Some(frames) = paging::unmap_record_frames(&records) else {
        return false;
    };
    let ownership_snapshot = with_vm_batch(9, |slot| {
        slot.as_ref()
            .and_then(|sched| sched.tasks[sched.current].process.as_ref())
            .and_then(|process| process.address_space.as_ref())
            .map(paging::owned_frame_snapshot)
    });
    let Some(ownership_snapshot) = ownership_snapshot else {
        return false;
    };
    let Some(removals) = paging::plan_unmap_ownership(&ownership_snapshot, &frames) else {
        return false;
    };

    let mut removed = 0usize;
    while removed < records.len() {
        let batch_end = (removed + VM_BATCH_PAGES as usize).min(records.len());
        let ok = with_vm_batch(10, |slot| {
            let Some(sched) = slot.as_mut() else {
                return false;
            };
            let Some(space) = sched.tasks[sched.current]
                .process
                .as_mut()
                .and_then(|process| process.address_space.as_mut())
            else {
                return false;
            };
            paging::unmap_records_atomic(space, &records[removed..batch_end]).is_ok()
        });
        if !ok {
            let restored = with_vm_batch(11, |slot| {
                let Some(sched) = slot.as_mut() else {
                    return false;
                };
                let Some(space) = sched.tasks[sched.current]
                    .process
                    .as_mut()
                    .and_then(|process| process.address_space.as_mut())
                else {
                    return false;
                };
                paging::restore_unmap_records(space, &records[..removed]).is_ok()
            });
            if !restored {
                crate::serial_println!(
                    "munmap: restoration invariant failed; terminating caller fail-closed"
                );
                exit_with_code(255);
            }
            return false;
        }
        removed = batch_end;
    }

    for removal_batch in removals.chunks(VM_BATCH_PAGES as usize) {
        let ownership_removed = with_vm_batch(12, |slot| {
            let Some(sched) = slot.as_mut() else {
                return false;
            };
            let Some(space) = sched.tasks[sched.current]
                .process
                .as_mut()
                .and_then(|process| process.address_space.as_mut())
            else {
                return false;
            };
            paging::remove_unmapped_ownership(space, removal_batch)
        });
        if !ownership_removed {
            crate::serial_println!(
                "munmap: ownership commit invariant failed; terminating caller fail-closed"
            );
            exit_with_code(255);
        }
        let release_frames: Vec<_> = removal_batch
            .iter()
            .map(paging::OwnershipRemoval::frame)
            .collect();
        let started = unsafe { core::arch::x86_64::_rdtsc() };
        let released = paging::release_frames(&release_frames).is_ok();
        let cycles = unsafe { core::arch::x86_64::_rdtsc() }.saturating_sub(started);
        record_vm_batch_cycles(cycles, 13);
        if !released {
            crate::serial_println!(
                "munmap: frame-release invariant failed; terminating caller fail-closed"
            );
            exit_with_code(255);
        }
    }
    let mut cleaned = 0u64;
    const TABLE_CLEANUP_BATCH_PAGES: u64 = 512;
    while cleaned < pages {
        let batch = TABLE_CLEANUP_BATCH_PAGES.min(pages - cleaned);
        let batch_start = ptr + cleaned * 4096;
        let cleanup_ok = with_vm_batch(15, |slot| {
            let Some(sched) = slot.as_mut() else {
                return false;
            };
            let Some(space) = sched.tasks[sched.current]
                .process
                .as_mut()
                .and_then(|process| process.address_space.as_mut())
            else {
                return false;
            };
            paging::clean_up_empty_tables_in_range(space, batch_start, batch).is_ok()
        });
        if !cleanup_ok {
            crate::serial_println!(
                "munmap: page-table cleanup invariant failed; terminating caller fail-closed"
            );
            exit_with_code(255);
        }
        cleaned += batch;
    }
    true
}

/// Number of physical frames a process's address space currently owns --
/// diagnostic/isolation-test evidence (`monitor`, the isolation-test shell
/// command), not load-bearing for correctness. `None` if `id` doesn't exist
/// or isn't a user process.
pub fn process_frame_count(id: u32) -> Option<usize> {
    with_scheduler(|slot| {
        let sched = slot.as_ref()?;
        sched
            .tasks
            .iter()
            .find(|t| t.id == id)?
            .process
            .as_ref()?
            .address_space
            .as_ref()
            .map(paging::AddressSpace::frame_count)
    })
}

/// The raw physical address of a process's own PML4 -- concrete,
/// hardware-level evidence that two processes have genuinely separate page
/// tables (see the isolation-test shell command), not just a claim. `None`
/// if `id` doesn't exist or isn't a user process.
pub fn process_pml4_phys(id: u32) -> Option<u64> {
    with_scheduler(|slot| {
        let sched = slot.as_ref()?;
        sched
            .tasks
            .iter()
            .find(|t| t.id == id)?
            .process
            .as_ref()?
            .address_space
            .as_ref()
            .map(|space| space.pml4_frame().start_address().as_u64())
    })
}

/// User stack, in pages, for every process this loader spawns -- 16 KiB,
/// generous for `hello_user`'s needs (a handful of stack frames and a
/// 20-byte decimal-conversion buffer) with headroom to spare.
const USER_STACK_PAGES: u64 = 4;

/// Load `elf_bytes` as a new user process and add it to the scheduler.
/// Returns its task id (== its pid, see `current_task_id`/`sys_getpid`) on
/// success.
///
/// Builds a fresh private address space (`paging::new_address_space`),
/// loads the ELF into it (`elf::load`, which maps and populates every
/// `PT_LOAD` segment with its own real permissions), maps a dedicated user
/// stack, and creates a `Ready` task whose first run drops straight to
/// Ring 3 at the ELF's entry point (`user_task_trampoline` /
/// `rust_user_entry`). None of the address-space setup requires this
/// process's CR3 to be the currently active one -- every `paging::`
/// function used here works against an arbitrary `AddressSpace` via the
/// physical-memory-offset mapping, so this is safe to call from whichever
/// task (ordinarily the shell) initiates the spawn.
pub fn spawn_user_process(name: &str, elf_bytes: &[u8]) -> Result<u32, &'static str> {
    spawn_user_process_with_cwd(name, elf_bytes, "/")
}

pub fn spawn_user_process_with_cwd(
    name: &str,
    elf_bytes: &[u8],
    cwd: &str,
) -> Result<u32, &'static str> {
    spawn_user_process_with_state(name, elf_bytes, cwd, TaskState::Ready)
}

/// Build a complete process but leave it blocked until `activate_task`.
/// Foreground launchers use this to perform ELF loading with interrupts
/// enabled, then bind input ownership and make the task runnable in one short
/// interrupt-disabled transition.
pub fn spawn_user_process_suspended(
    name: &str,
    elf_bytes: &[u8],
    cwd: &str,
) -> Result<u32, &'static str> {
    spawn_user_process_with_state(name, elf_bytes, cwd, TaskState::Blocked)
}

fn spawn_user_process_with_state(
    name: &str,
    elf_bytes: &[u8],
    cwd: &str,
    initial_state: TaskState,
) -> Result<u32, &'static str> {
    if task_count() >= MAX_TASKS {
        return Err("task limit reached");
    }
    // Allocate every bounded per-process heap object fallibly before page
    // tables are created. Malicious or simply concurrent Ring-3 SPAWN
    // requests must receive an ABI error under pressure, never enter the
    // kernel allocation panic path.
    let resources = try_user_process_resources(name, cwd)?;
    let address_space = paging::new_address_space()?;

    // `address_space` is never installed into a `Tcb`, never reachable from
    // the scheduler, and never scheduled until `build_user_tcb` returns a
    // complete, ready-to-run `Tcb` -- so at every failure point *inside*
    // `build_user_tcb`, it is provably not the active CR3, and freeing it
    // immediately is always sound. See `build_user_tcb`'s own docs for why
    // this matters: without this, every failed spawn (a truncated ELF, an
    // out-of-memory stack mapping, ...) would silently leak every frame
    // `new_address_space` and everything `build_user_tcb` had mapped so
    // far -- the PML4 at minimum, every ELF segment page and stack page
    // mapped before the failing step at worst.
    let mut tcb = build_user_tcb(elf_bytes, resources, address_space)?;
    tcb.state = initial_state;
    let mut tcb = match try_box_value(tcb) {
        Ok(tcb) => Some(tcb),
        Err(mut tcb) => {
            reclaim_unstarted_process(&mut tcb);
            return Err("task control block allocation failed");
        }
    };
    let id = tcb.as_ref().expect("just constructed").id;

    // The very last failure point: `with_scheduler` itself refusing (the
    // scheduler not being initialized -- unreachable in practice, since
    // `task::init()` always runs before any shell command could reach
    // this, but handled for the same reason every other step is). `tcb`
    // is an `Option` captured by the closure (by mutable reference, since
    // the closure only ever calls `.take()`/reads it, never moves it
    // outright) specifically so it's still available in this scope
    // afterward on the error path, to reclaim its address space -- a
    // closure that moved `tcb` in directly would make it unreachable here
    // regardless of which branch inside actually ran.
    let push_result: Result<(), &'static str> = with_scheduler(|slot| {
        let sched = slot.as_mut().ok_or("scheduler not initialized")?;
        if sched.tasks.len() >= MAX_TASKS {
            return Err("task limit reached");
        }
        sched.tasks.push(tcb.take().expect("tcb not yet taken"));
        Ok(())
    });

    match push_result {
        Ok(()) => Ok(id),
        Err(reason) => {
            // Safety: the push above never ran (this is the `Err` branch,
            // and `with_scheduler`'s closure only calls `tcb.take()` right
            // before the push it's guarding), so `tcb` is still `Some`
            // here, was never added to `sched.tasks`, and therefore never
            // had any chance of being scheduled or having its address
            // space loaded into CR3 -- freeing it is sound for the same
            // reason it's sound inside `build_user_tcb`.
            if let Some(mut tcb) = tcb {
                reclaim_unstarted_process(&mut tcb);
            }
            Err(reason)
        }
    }
}

pub fn activate_task(id: u32) -> bool {
    with_scheduler(|slot| {
        let sched = slot.as_mut()?;
        let task = sched.tasks.iter_mut().find(|task| task.id == id)?;
        if task.state != TaskState::Blocked || task.wake_at_tick != 0 {
            return None;
        }
        task.state = TaskState::Ready;
        Some(())
    })
    .is_some()
}

/// Build a complete, ready-to-run `Tcb` for a new user process: load the
/// ELF, map and zero its stack, and wrap it all up -- freeing
/// `address_space`'s frames before returning on *any* failure along the
/// way, since nothing outside this function call has referenced it yet
/// (see `spawn_user_process`'s docs on why that makes every early-return
/// here safe to reclaim immediately rather than leaking).
fn build_user_tcb(
    elf_bytes: &[u8],
    resources: UserProcessResources,
    mut address_space: paging::AddressSpace,
) -> Result<Tcb, &'static str> {
    macro_rules! try_or_free {
        ($expr:expr) => {
            match $expr {
                Ok(value) => value,
                Err(reason) => {
                    // Safety: see this function's own docs.
                    unsafe { paging::free_address_space(address_space) };
                    return Err(reason);
                }
            }
        };
    }

    let loaded = try_or_free!(elf::load(&mut address_space, elf_bytes));

    let stack_top = paging::USER_SPACE_BASE + paging::USER_SPACE_SIZE - 0x1000;
    let stack_bottom = stack_top - USER_STACK_PAGES * 4096;
    let stack_flags = PageTableFlags::PRESENT
        | PageTableFlags::WRITABLE
        | PageTableFlags::USER_ACCESSIBLE
        | PageTableFlags::NO_EXECUTE;
    let mut page_addr = stack_bottom;
    while page_addr < stack_top {
        let page: Page<Size4KiB> = Page::containing_address(VirtAddr::new(page_addr));
        try_or_free!(paging::map_in_address_space(
            &mut address_space,
            page,
            stack_flags
        ));
        page_addr += 4096;
    }
    // Zero the freshly mapped stack -- same reasoning as `elf.rs`'s segment
    // loading: a reused physical frame must never expose a previous
    // process's leftover contents to this one.
    try_or_free!(paging::zero_bytes_in_address_space(
        &address_space,
        VirtAddr::new(stack_bottom),
        USER_STACK_PAGES * 4096,
    ));

    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    Ok(new_user_tcb(
        id,
        resources,
        address_space,
        loaded.entry_point.as_u64(),
        stack_top,
    ))
}

fn reclaim_unstarted_process(tcb: &mut Tcb) {
    if let Some(process) = tcb.process.as_mut() {
        if let Some(space) = process.address_space.take() {
            // Safety: this TCB was never inserted into the scheduler, so its
            // address space cannot have become the active CR3.
            unsafe { paging::free_address_space(space) };
        }
    }
}
