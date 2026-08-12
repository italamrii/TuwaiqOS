//! What Ring 3 can learn about the kernel without faulting.
//!
//! `sgdt`, `sidt`, `sldt` and `str` read the descriptor-table registers. On
//! x86-64 they are *unprivileged*: CPL=3 may execute all four, and the CPU
//! only refuses when `CR4.UMIP` is set.
//!
//! TuwaiqOS never writes `CR4`, so UMIP is clear and all four succeed. This
//! program runs them and reports the kernel addresses they hand back, then
//! exits 0 -- it proves an information leak rather than a fault, so a
//! non-zero exit would mean the leak had been closed.
//!
//! Nothing here is a privilege escalation on its own. Knowing where the GDT
//! and IDT live is the first step of one, and it is exactly the knowledge a
//! kernel is normally expected to withhold.

#![no_std]
#![no_main]

use core::arch::{asm, global_asm};

use hello_user::{syscall, write, SYS_EXIT};

global_asm!(
    r#"
.global _start
_start:
    call {main}
1:
    jmp 1b
"#,
    main = sym rust_main,
);

/// The 10-byte operand `sgdt` and `sidt` store: a 2-byte limit then an 8-byte
/// base, packed with no padding.
#[repr(C, packed)]
#[derive(Clone, Copy)]
struct DescriptorTablePointer {
    limit: u16,
    base: u64,
}

fn hex(value: u64, out: &mut [u8; 18]) {
    const DIGITS: &[u8; 16] = b"0123456789ABCDEF";
    out[0] = b'0';
    out[1] = b'x';
    for index in 0..16 {
        out[2 + index] = DIGITS[((value >> (60 - index * 4)) & 0xF) as usize];
    }
}

fn report(label: &[u8], pointer: DescriptorTablePointer) {
    let base = pointer.base;
    let limit = pointer.limit;
    let mut buffer = [0u8; 18];
    hex(base, &mut buffer);
    write(label);
    write(b" base=");
    write(&buffer);
    hex(limit as u64, &mut buffer);
    write(b" limit=");
    write(&buffer);
    write(b"\n");
}

extern "C" fn rust_main() -> ! {
    write(b"probe_descriptors: reading descriptor tables from CPL=3\n");

    let mut gdt = DescriptorTablePointer { limit: 0, base: 0 };
    let mut idt = DescriptorTablePointer { limit: 0, base: 0 };
    let mut ldt: u16 = 0;
    let mut task: u16 = 0;

    // Safety: all four are unprivileged reads of a register into a local. They
    // fault only when CR4.UMIP is set, in which case the kernel terminates
    // this process and the write below never runs -- which is the outcome this
    // program is checking for.
    unsafe {
        asm!("sgdt [{}]", in(reg) &mut gdt, options(nostack, preserves_flags));
        asm!("sidt [{}]", in(reg) &mut idt, options(nostack, preserves_flags));
        asm!("sldt {0:x}", out(reg) ldt, options(nomem, nostack, preserves_flags));
        asm!("str {0:x}", out(reg) task, options(nomem, nostack, preserves_flags));
    }

    report(b"probe_descriptors: GDT", gdt);
    report(b"probe_descriptors: IDT", idt);

    let mut buffer = [0u8; 18];
    hex(ldt as u64, &mut buffer);
    write(b"probe_descriptors: LDTR=");
    write(&buffer);
    hex(task as u64, &mut buffer);
    write(b" TR=");
    write(&buffer);
    write(b"\n");

    write(b"probe_descriptors: LEAKED -- CR4.UMIP is clear so CPL=3 read all four\n");

    // Safety: SYS_EXIT never returns.
    unsafe {
        syscall(SYS_EXIT, 0, 0, 0);
    }
    loop {}
}
