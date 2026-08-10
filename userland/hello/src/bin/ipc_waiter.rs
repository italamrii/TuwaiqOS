//! Short-lived delegation target used by the hostile scope tests.

#![no_std]
#![no_main]

use core::arch::global_asm;
use hello_user::*;

global_asm!(r#".global _start
_start:
    call {main}
1:  jmp 1b"#, main = sym rust_main);

extern "C" fn rust_main() -> ! {
    let result = ipc_accept(100);
    let code = if result == IPC_ERR_TIMEOUT { 0 } else { 1 };
    unsafe {
        syscall(SYS_EXIT, code, 0, 0);
    }
    loop {}
}
