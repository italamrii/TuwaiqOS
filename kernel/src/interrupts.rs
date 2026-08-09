//! IDT, legacy PIC routing, PIT timer tick, and CPU exception handlers.
//!
//! This is the subsystem that turns TuwaiqOS from a purely polled kernel
//! into one that reacts to hardware asynchronously: the timer interrupt
//! drives the tick counter (and, from Phase 3 on, preemption), and the
//! keyboard interrupt replaces the old busy-poll loop in `keyboard.rs`.

use core::sync::atomic::{AtomicU64, Ordering};

use lazy_static::lazy_static;
use pic8259::ChainedPics;
use spin::Mutex;
use x86_64::structures::idt::{InterruptDescriptorTable, InterruptStackFrame, PageFaultErrorCode};
use x86_64::{PrivilegeLevel, VirtAddr};

use crate::{framebuffer_console, gdt, keyboard, mouse, syscall};

/// The one vector Ring 3 code is allowed to invoke via `int n` -- the real
/// syscall ABI now (see `syscall.rs`), registered below with DPL=3; every
/// other vector stays at the default DPL=0 and would itself
/// General-Protection-Fault if a Ring 3 `int` instruction targeted it.
/// Chosen clear of both the CPU exception range (0-31) and the remapped
/// hardware IRQ range (`PIC_1_OFFSET..PIC_2_OFFSET+8`, 32-47); 0x80 is also
/// the traditional x86 "syscall gate" vector, which this deliberately
/// echoes.
const SYSCALL_VECTOR: u8 = 0x80;

/// Exit codes `exit_with_code` records for a process the kernel terminates
/// on its behalf after fault-isolation recovery (see
/// `general_protection_fault_handler` / `page_fault_handler` /
/// `invalid_opcode_handler` / `divide_error_handler` below) -- deliberately
/// echoing the traditional Unix "128 + signal number" convention
/// (`SIGSEGV`=11, `SIGILL`=4, `SIGFPE`=8) purely as a recognizable,
/// self-documenting value in `ps`/`taskinfo` output, not because this
/// kernel has real Unix signals. `#UD` (invalid opcode) shares `SIGILL`
/// with a Ring 3 `#GP` (privileged instruction) -- both are "the CPU
/// refused to execute this instruction," the same category a real kernel
/// would report identically.
const EXIT_CODE_SEGV: i32 = 139;
const EXIT_CODE_ILL: i32 = 132;
const EXIT_CODE_FPE: i32 = 136;

/// Legacy PICs are remapped so hardware IRQs 0-15 land at vectors 32-47,
/// clear of the CPU's own exception vectors 0-31.
pub const PIC_1_OFFSET: u8 = 32;
pub const PIC_2_OFFSET: u8 = PIC_1_OFFSET + 8;

/// Safety: 0x20/0xA0 (master) and 0x21/0xA1 (slave) are the fixed legacy
/// PIC I/O ports on the PC platform; this is the one and only PIC handle
/// for the kernel, so there is no risk of two owners racing the hardware.
pub static PICS: Mutex<ChainedPics> =
    unsafe { Mutex::new(ChainedPics::new(PIC_1_OFFSET, PIC_2_OFFSET)) };

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum InterruptIndex {
    Timer = PIC_1_OFFSET,
    Keyboard,
    /// IRQ12, routed through the slave PIC's cascade line (IRQ2 on the
    /// master) -- `PIC_2_OFFSET + 4`, vector 44. Only reachable at all once
    /// `enable_mouse` unmasks both the cascade line and this line (see that
    /// function's docs for why the two-line unmask is required).
    Mouse = PIC_2_OFFSET + 4,
}

impl InterruptIndex {
    fn as_u8(self) -> u8 {
        self as u8
    }

    fn as_usize(self) -> usize {
        usize::from(self.as_u8())
    }
}

static TICKS: AtomicU64 = AtomicU64::new(0);
static FIRST_TICK_TSC: AtomicU64 = AtomicU64::new(0);
static LAST_TICK_TSC: AtomicU64 = AtomicU64::new(0);

/// PIT channel 0 is programmed for this frequency in `init_pit`.
const TIMER_HZ: u64 = 100;

/// Raw tick count since the timer interrupt was enabled.
pub fn ticks() -> u64 {
    TICKS.load(Ordering::Relaxed)
}

/// Whole seconds of uptime, derived from the real timer tick count -- not
/// a placeholder string. Zero until `init()` has enabled the PIT.
pub fn uptime_seconds() -> u64 {
    ticks() / TIMER_HZ
}

/// Average invariant-TSC cycles observed per delivered PIT tick. Diagnostics
/// use this to express short RDTSC measurements in timer-tick units without
/// assuming a host or virtual CPU frequency.
pub fn average_tsc_cycles_per_tick() -> Option<u64> {
    let ticks = ticks();
    let first = FIRST_TICK_TSC.load(Ordering::Relaxed);
    let last = LAST_TICK_TSC.load(Ordering::Relaxed);
    if ticks < 2 || first == 0 || last <= first {
        return None;
    }
    Some((last - first) / (ticks - 1))
}

/// Halt the CPU until the next interrupt fires (timer or keyboard). Used by
/// the shell's input loop instead of a CPU-burning empty spin.
pub fn halt() {
    x86_64::instructions::hlt();
}

lazy_static! {
    static ref IDT: InterruptDescriptorTable = {
        let mut idt = InterruptDescriptorTable::new();

        idt.breakpoint.set_handler_fn(breakpoint_handler);
        idt.page_fault.set_handler_fn(page_fault_handler);
        idt.general_protection_fault
            .set_handler_fn(general_protection_fault_handler);
        idt.invalid_opcode.set_handler_fn(invalid_opcode_handler);
        idt.divide_error.set_handler_fn(divide_error_handler);

        // Safety: DOUBLE_FAULT_IST_INDEX names a stack gdt::init() already
        // installed into the TSS before this IDT is loaded.
        unsafe {
            idt.double_fault
                .set_handler_fn(double_fault_handler)
                .set_stack_index(gdt::DOUBLE_FAULT_IST_INDEX);
        }

        // Timer: deliberately no IST stack (see gdt::IRQ_IST_INDEX) -- it
        // must run on whichever task's stack was interrupted so a context
        // switch inside it can resume that task correctly later.
        idt[InterruptIndex::Timer.as_usize()].set_handler_fn(timer_interrupt_handler);

        // Safety: IRQ_IST_INDEX names a stack gdt::init() already installed
        // into the TSS before this IDT is loaded. Keyboard never redirects
        // control flow, so a fixed stack is safe (and simpler) for it.
        unsafe {
            idt[InterruptIndex::Keyboard.as_usize()]
                .set_handler_fn(keyboard_interrupt_handler)
                .set_stack_index(gdt::IRQ_IST_INDEX);
        }

        // Safety: same reasoning as the keyboard entry above -- mouse
        // packets never redirect control flow, so a fixed IST stack is
        // safe. Registered unconditionally at boot; the line itself stays
        // masked (see `init` below) until `enable_mouse` runs, so no
        // spurious vector-44 interrupt can arrive before `mouse::init` has
        // actually programmed the device.
        unsafe {
            idt[InterruptIndex::Mouse.as_usize()]
                .set_handler_fn(mouse_interrupt_handler)
                .set_stack_index(gdt::IRQ_IST_INDEX);
        }

        // The syscall gate (see `syscall.rs`). Not `set_handler_fn`: that
        // only accepts the `extern "x86-interrupt"` ABI, which exposes the
        // CPU-pushed frame but not general-purpose registers -- the ABI
        // `syscall.rs` documents needs to read/write `RAX`/`RDI`/`RSI`/`RDX`,
        // so `syscall_entry` is hand-written asm instead, registered here
        // by raw address. DPL=3 is required for the `int 0x80` instruction
        // Ring 3 code executes to be allowed at all -- without it, the CPU
        // rejects the attempt with a Ring-0-origin-looking GPF before this
        // handler ever runs, since a software `int` requires CPL <= the
        // gate's DPL.
        //
        // Safety: `syscall::syscall_entry` is a valid code address for the
        // lifetime of the kernel (a `global_asm!` symbol, not freed or
        // reused), and its calling convention (raw entry, all GPRs
        // saved/restored by hand, ending in `iretq`) is exactly what an
        // IDT gate handler must do -- see `syscall.rs`'s module docs.
        unsafe {
            idt[SYSCALL_VECTOR as usize]
                .set_handler_addr(VirtAddr::new(syscall::syscall_entry as *const () as u64))
                .set_privilege_level(PrivilegeLevel::Ring3);
        }

        idt
    };
}

/// Bring up GDT, IDT, PIC remap, and the PIT timer, then enable interrupts.
/// Must run after the heap is initialized (the keyboard queue allocates)
/// and before anything expects `poll_key()` or `uptime_seconds()` to be live.
pub fn init() {
    gdt::init();
    IDT.load();

    // Self-test: a software breakpoint exercises the full IDT/GDT/IRETQ
    // path (the same mechanism every exception and IRQ handler relies on)
    // before any hardware interrupt is ever allowed to fire. If this
    // doesn't return cleanly, nothing past this point can be trusted.
    x86_64::instructions::interrupts::int3();
    serial_println!("interrupts: breakpoint self-test OK");

    // Safety: PIC_1_OFFSET/PIC_2_OFFSET move IRQs 0-15 to vectors 32-47,
    // matching the vectors registered above, and this runs exactly once
    // before interrupts are enabled.
    //
    // `ChainedPics::initialize()` *preserves* whatever IRQ mask the BIOS
    // left rather than resetting it -- diagnosed during Phase 1 bring-up:
    // SeaBIOS leaves several lines unmasked (IRQ14, the primary ATA/IDE
    // controller, among them), and this kernel only registers IDT entries
    // for vectors 32 (timer) and 33 (keyboard). A hardware IRQ landing on
    // any other, not-present vector -- entirely plausible the moment
    // `ata::read_sector`'s polling loop below causes the disk controller
    // to raise IRQ14 -- produced an unrecoverable fault with no handler
    // to attribute it to. Only the two lines this kernel actually
    // services are left unmasked; everything else, including the PIC2
    // cascade line, is masked until a real driver for it exists.
    unsafe {
        let mut pics = PICS.lock();
        pics.initialize();
        pics.write_masks(0b1111_1100, 0b1111_1111);
    }
    serial_println!("interrupts: PIC remapped, only timer+keyboard IRQs unmasked");

    init_pit(TIMER_HZ);
    serial_println!("interrupts: PIT programmed");

    x86_64::instructions::interrupts::enable();
    serial_println!("interrupts: IDT/PIC/PIT online, timer at {} Hz", TIMER_HZ);
}

/// Unmask IRQ12 (mouse) after `mouse::init()` has actually programmed the
/// PS/2 auxiliary device -- called from `main.rs`, deliberately separate
/// from `init()` above and from `mouse::init()` itself, so the sequence is
/// always "program the device, *then* let the PIC start delivering its
/// interrupts," never the reverse (which could deliver IRQ12 to a device
/// still mid-configuration, or before `mouse.rs`'s packet-sync state is
/// ready to receive it).
///
/// Also unmasks IRQ2, the master PIC's cascade line: real IRQ12 physically
/// arrives *through* the slave PIC, whose own output is wired to the
/// master's IRQ2 input, so the master must also let that line through or no
/// slave-PIC interrupt (mouse included) can ever reach the CPU, regardless
/// of the slave's own per-line mask.
pub fn enable_mouse() {
    // Unlike `init`'s own PIC setup (which runs *before* interrupts are
    // enabled), this runs from ordinary boot context *after*
    // `x86_64::instructions::interrupts::enable()` -- the 100 Hz timer is
    // already live. Locking `PICS` here without disabling interrupts would
    // reproduce Bugs 1/3/4 (see "Locking invariant" in ARCHITECTURE.md)
    // exactly: a timer tick landing mid-lock would deadlock against
    // `timer_interrupt_handler`'s own `PICS.lock()` for EOI, since that ISR
    // cannot return (and so cannot release nothing -- it never acquired
    // anything, it just spins) until this lock is released, which cannot
    // happen until the ISR itself returns.
    //
    // Safety: PIC_1_OFFSET/PIC_2_OFFSET already match the vectors this
    // IDT registers (see `init` above); this only changes which of those
    // already-valid vectors are allowed to fire.
    x86_64::instructions::interrupts::without_interrupts(|| unsafe {
        PICS.lock().write_masks(0b1111_1000, 0b1110_1111);
    });
    serial_println!("interrupts: mouse IRQ (IRQ12 via cascade) unmasked");
}

/// Program PIT channel 0 (legacy 8253/8254) for a periodic square-wave
/// interrupt at `hz`. 1_193_182 Hz is the PIT's fixed input clock.
fn init_pit(hz: u64) {
    use x86_64::instructions::port::Port;

    const PIT_INPUT_HZ: u64 = 1_193_182;
    let divisor = (PIT_INPUT_HZ / hz) as u16;

    // Safety: 0x43 (mode/command) and 0x40 (channel 0 data) are the fixed
    // legacy PIT ports; this sequence (command byte, then low/high divisor
    // bytes) is the documented 8253/8254 programming protocol and runs
    // once, before interrupts are enabled.
    unsafe {
        let mut command: Port<u8> = Port::new(0x43);
        let mut channel0: Port<u8> = Port::new(0x40);
        command.write(0x36u8); // channel 0, lo/hi byte access, mode 3, binary
        channel0.write((divisor & 0xFF) as u8);
        channel0.write((divisor >> 8) as u8);
    }
}

/// Print a fault to serial (always -- the only channel a fault handler can
/// trust unconditionally, since it's port I/O, not memory-mapped) and to
/// the framebuffer console *only if it is confirmed initialized*.
///
/// Discovered during Phase 1 bring-up: this bootloader's page tables do
/// not map the legacy VGA text buffer (0xB8000) at all in this boot
/// configuration, framebuffer or not. v0.5's original panic handler wrote
/// to `vga_buffer` unconditionally, but that path was simply never
/// exercised (it never panicked); the first fault handler that actually
/// ran here turned a single fault into a recursive page-fault storm
/// against unmapped memory. Until Phase 2 gives us real page-table
/// introspection to check mappings before writing, VGA is treated as
/// untrustworthy from a fault context and is not used here at all.
fn report_fault(name: &str) {
    serial_println!(
        "fault context: last successful boot stage={}",
        crate::boot_diag::last_stage()
    );
    if framebuffer_console::is_active() {
        framebuffer_console::println("");
        framebuffer_console::println("KERNEL PANIC: ");
        framebuffer_console::println(name);
    }
}

extern "x86-interrupt" fn breakpoint_handler(stack_frame: InterruptStackFrame) {
    serial_println!("EXCEPTION: BREAKPOINT\n{:#?}", stack_frame);
}

extern "x86-interrupt" fn double_fault_handler(
    stack_frame: InterruptStackFrame,
    error_code: u64,
) -> ! {
    serial_println!(
        "EXCEPTION: DOUBLE FAULT (error_code={})\n{:#?}",
        error_code,
        stack_frame
    );
    report_fault("double fault");
    loop {
        x86_64::instructions::hlt();
    }
}

extern "x86-interrupt" fn page_fault_handler(
    stack_frame: InterruptStackFrame,
    error_code: PageFaultErrorCode,
) {
    let fault_addr = x86_64::registers::control::Cr2::read();
    serial_println!(
        "EXCEPTION: PAGE FAULT at {:?}\nerror_code={:?}\n{:#?}",
        fault_addr,
        error_code,
        stack_frame
    );

    // Same reasoning as the GPF handler below: a Ring-3-origin page fault
    // (unmapped address, or a permission the mapping genuinely doesn't
    // grant -- writing a read-only page, executing a NO_EXECUTE one,
    // touching kernel memory that was never USER_ACCESSIBLE in this
    // process's own address space) is a user program doing something its
    // own mappings forbid, not a kernel bug. Recover by ending only that
    // process; a Ring-0-origin page fault is a genuine kernel memory-safety
    // bug and keeps the unconditional halt below, unchanged from before
    // Phase 4.
    if stack_frame.code_segment & 0b11 == 3 {
        serial_println!(
            "usermode: page fault trapped safely from CPL=3 at {:?} (RIP={:?}) -- \
             terminating the offending process, kernel continues",
            fault_addr,
            stack_frame.instruction_pointer
        );
        crate::task::exit_with_code(EXIT_CODE_SEGV);
    }

    report_fault("page fault");
    loop {
        x86_64::instructions::hlt();
    }
}

extern "x86-interrupt" fn general_protection_fault_handler(
    stack_frame: InterruptStackFrame,
    error_code: u64,
) {
    serial_println!(
        "EXCEPTION: GENERAL PROTECTION FAULT (error_code={})\n{:#?}",
        error_code,
        stack_frame
    );

    // A GPF whose saved CS has RPL=3 can only mean the faulting instruction
    // itself executed at CPL=3 -- the CPU stamps the *current* CS onto the
    // frame it builds, it is not something the interrupted code could have
    // faked. A Ring-0-origin GPF still falls through to the unconditional
    // halt below exactly as it always has, because the kernel faulting is
    // a real, unrecoverable bug, not something to route around. A
    // Ring-3-origin GPF is different in kind, the same way a real OS
    // treats "kernel bug" and "userspace program did something CPL=3
    // forbids" differently: any user process executing a privileged
    // instruction (or otherwise tripping a protection check) takes exactly
    // this path, and recovering by ending just that process and letting
    // the scheduler carry on proves the trap didn't corrupt the kernel or
    // any other process, which a permanent halt here could never
    // demonstrate.
    if stack_frame.code_segment & 0b11 == 3 {
        serial_println!(
            "usermode: privileged instruction trapped safely from CPL=3 (RIP={:?}) -- \
             terminating the offending task, kernel continues",
            stack_frame.instruction_pointer
        );
        crate::task::exit_with_code(EXIT_CODE_ILL);
    }

    report_fault("general protection fault");
    loop {
        x86_64::instructions::hlt();
    }
}

extern "x86-interrupt" fn invalid_opcode_handler(stack_frame: InterruptStackFrame) {
    serial_println!("EXCEPTION: INVALID OPCODE\n{:#?}", stack_frame);

    // Same reasoning as the GPF/page-fault handlers: a Ring-3-origin #UD
    // (an undefined or unsupported instruction, e.g. `ud2`, executed by a
    // user program) is that program's own bug, not a kernel one -- kill
    // only the offending process and let the kernel and every other
    // process carry on. A Ring-0-origin #UD is a genuine kernel bug
    // (corrupted code, a real toolchain/codegen problem) and keeps the
    // unconditional halt below, unchanged.
    if stack_frame.code_segment & 0b11 == 3 {
        serial_println!(
            "usermode: invalid opcode trapped safely from CPL=3 (RIP={:?}) -- \
             terminating the offending process, kernel continues",
            stack_frame.instruction_pointer
        );
        crate::task::exit_with_code(EXIT_CODE_ILL);
    }

    report_fault("invalid opcode");
    loop {
        x86_64::instructions::hlt();
    }
}

extern "x86-interrupt" fn divide_error_handler(stack_frame: InterruptStackFrame) {
    serial_println!("EXCEPTION: DIVIDE ERROR\n{:#?}", stack_frame);

    // Same reasoning as the other Ring-3-origin recovery paths: a division
    // by zero (or a quotient overflow) in a user program's own code is
    // that program's bug, not the kernel's -- kill only the offending
    // process. A Ring-0-origin divide error is a genuine kernel bug and
    // keeps the unconditional halt below, unchanged.
    if stack_frame.code_segment & 0b11 == 3 {
        serial_println!(
            "usermode: divide error trapped safely from CPL=3 (RIP={:?}) -- \
             terminating the offending process, kernel continues",
            stack_frame.instruction_pointer
        );
        crate::task::exit_with_code(EXIT_CODE_FPE);
    }

    report_fault("divide error");
    loop {
        x86_64::instructions::hlt();
    }
}

extern "x86-interrupt" fn timer_interrupt_handler(_stack_frame: InterruptStackFrame) {
    let tsc = unsafe { core::arch::x86_64::_rdtsc() };
    let _ = FIRST_TICK_TSC.compare_exchange(0, tsc, Ordering::Relaxed, Ordering::Relaxed);
    LAST_TICK_TSC.store(tsc, Ordering::Relaxed);
    TICKS.fetch_add(1, Ordering::Relaxed);
    // Safety: EOI is only ever issued here, for the interrupt this ISR
    // itself is handling, matching the IRQ this vector is registered for.
    // Sent before the scheduler runs so the PIC can deliver the next IRQ
    // regardless of how long a context switch takes.
    unsafe {
        PICS.lock()
            .notify_end_of_interrupt(InterruptIndex::Timer.as_u8());
    }

    // May perform a real context switch (see task.rs's module docs) --
    // this is what makes preemption real rather than cosmetic. Correct
    // only because this handler does not use an IST stack (see gdt.rs):
    // it runs on whichever task was interrupted, so a switch here leaves
    // that task's own suspended state on its own stack.
    crate::task::on_timer_tick();
}

extern "x86-interrupt" fn keyboard_interrupt_handler(_stack_frame: InterruptStackFrame) {
    use x86_64::instructions::port::Port;

    let mut data_port: Port<u8> = Port::new(0x60);
    // Safety: the CPU only vectors here in response to IRQ1, at which
    // point the PS/2 controller guarantees a byte is waiting at 0x60.
    let scancode: u8 = unsafe { data_port.read() };
    keyboard::on_scancode(scancode);

    // Safety: same reasoning as the timer handler above.
    unsafe {
        PICS.lock()
            .notify_end_of_interrupt(InterruptIndex::Keyboard.as_u8());
    }
}

extern "x86-interrupt" fn mouse_interrupt_handler(_stack_frame: InterruptStackFrame) {
    use x86_64::instructions::port::Port;

    let mut data_port: Port<u8> = Port::new(0x60);
    // Safety: the CPU only vectors here in response to IRQ12, at which
    // point the PS/2 controller guarantees a byte is waiting at 0x60,
    // exactly as for the keyboard handler above.
    let byte: u8 = unsafe { data_port.read() };
    mouse::on_byte(byte);

    // Safety: same reasoning as the timer/keyboard handlers above -- EOI
    // must be sent for this exact vector.
    unsafe {
        PICS.lock()
            .notify_end_of_interrupt(InterruptIndex::Mouse.as_u8());
    }
}
