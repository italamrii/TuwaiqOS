//! Whether the `syscall` instruction works at all.
//!
//! Every Linux x86-64 binary enters the kernel with `syscall`, not with an
//! interrupt. The instruction is only decodable when `EFER.SCE` is set, and
//! `kernel/src/paging.rs` sets `EFER.NO_EXECUTE_ENABLE` and nothing else, so
//! it should raise #UD here.
//!
//! This is measured rather than read off the source because the answer decides
//! whether a Linux compatibility layer can live entirely in userspace: an
//! instruction that does not decode cannot be intercepted by anything running
//! at CPL=3.
//!
//! The arguments are Linux's `write(1, msg, len)`, so if the instruction ever
//! did decode on a kernel that had wired it up, the message would appear.

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

const LINUX_SYS_WRITE: u64 = 1;
static PAYLOAD: &[u8] = b"probe_syscall_insn: the syscall instruction decoded\n";

extern "C" fn rust_main() -> ! {
    write(b"probe_syscall_insn: about to execute `syscall` from CPL=3\n");

    // Safety: expected to raise #UD. Registers follow the Linux ABI so the
    // call would be meaningful on a kernel that had enabled SCE and installed
    // an entry point, rather than being an arbitrary trap.
    unsafe {
        asm!(
            "syscall",
            inlateout("rax") LINUX_SYS_WRITE => _,
            in("rdi") 1u64,
            in("rsi") PAYLOAD.as_ptr(),
            in("rdx") PAYLOAD.len(),
            out("rcx") _,
            out("r11") _,
            options(nostack),
        );
    }

    write(b"probe_syscall_insn: UNEXPECTED -- the instruction decoded\n");
    // Safety: SYS_EXIT never returns.
    unsafe {
        syscall(SYS_EXIT, 1, 0, 0);
    }
    loop {}
}
