//! Whether the address `probe_descriptors` leaks can actually be used.
//!
//! `probe_descriptors` shows that CPL=3 can read the IDT base, because
//! `CR4.UMIP` is clear. That is only worth something if the address it hands
//! back can then be dereferenced.
//!
//! This program closes that loop in one process: it runs `sidt`, takes the
//! base it gets, and reads from it. Unlike `bad_kernel`, which dereferences a
//! constant chosen when the program was written, the address here is learned
//! at runtime from the machine itself -- so a kernel that moved its tables
//! around would not change the outcome.
//!
//! Written to fail. The read must raise a fault and the kernel must terminate
//! this process, which leaves the leak an information disclosure rather than a
//! way in.

#![no_std]
#![no_main]

use core::arch::{asm, global_asm};
use core::ptr;

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

#[repr(C, packed)]
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

extern "C" fn rust_main() -> ! {
    let mut idt = DescriptorTablePointer { limit: 0, base: 0 };
    // Safety: `sidt` is unprivileged while CR4.UMIP is clear. If UMIP is ever
    // set this faults here instead, which is also a pass.
    unsafe {
        asm!("sidt [{}]", in(reg) &mut idt, options(nostack, preserves_flags));
    }

    let base = idt.base;
    let mut buffer = [0u8; 18];
    hex(base, &mut buffer);
    write(b"probe_leaked: sidt gave IDT base=");
    write(&buffer);
    write(b"\nprobe_leaked: about to dereference it from CPL=3\n");

    // Safety: expected to fault. `read_volatile` stops the compiler eliding a
    // read whose value is unused.
    let first = unsafe { ptr::read_volatile(base as *const u64) };

    hex(first, &mut buffer);
    write(b"probe_leaked: UNEXPECTED -- read the IDT from CPL=3, first quadword=");
    write(&buffer);
    write(b"\n");
    // Safety: SYS_EXIT never returns.
    unsafe {
        syscall(SYS_EXIT, 1, 0, 0);
    }
    loop {}
}
