//! Whether Ring 3 can read a model-specific register.
//!
//! `rdmsr` is privileged: at CPL=3 it raises #GP regardless of which MSR is
//! named. IA32_EFER is chosen because it holds NXE and SCE, so reading it
//! would tell a user program which protections are active.
//!
//! Success here would be a serious isolation failure, so this program is
//! written to fail. If the kernel is correct the `rdmsr` never returns and
//! the process is terminated with the illegal-instruction exit code, leaving
//! the trailing message unprinted.

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

const IA32_EFER: u32 = 0xC000_0080;

extern "C" fn rust_main() -> ! {
    write(b"probe_msr: about to read IA32_EFER with rdmsr from CPL=3\n");

    let low: u32;
    let high: u32;
    // Safety: this is expected to fault. Nothing after it runs on a correct
    // kernel, and the outputs are only read on the path that must not happen.
    unsafe {
        asm!(
            "rdmsr",
            in("ecx") IA32_EFER,
            out("eax") low,
            out("edx") high,
            options(nomem, nostack),
        );
    }

    let _ = (low, high);
    write(b"probe_msr: UNEXPECTED -- rdmsr succeeded at CPL=3\n");
    // Safety: SYS_EXIT never returns. Exit 1 marks the failure explicitly so
    // it cannot be mistaken for a clean run.
    unsafe {
        syscall(SYS_EXIT, 1, 0, 0);
    }
    loop {}
}
