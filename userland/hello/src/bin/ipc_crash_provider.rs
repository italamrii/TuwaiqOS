//! Provider-exit wakeup test.

#![no_std]
#![no_main]

use core::arch::global_asm;
use hello_user::*;

global_asm!(r#".global _start
_start:
    call {main}
1:  jmp 1b"#, main = sym rust_main);

extern "C" fn rust_main() -> ! {
    let endpoint = ipc_endpoint_create(2);
    let client = unsafe { syscall(SYS_SPAWN, b"/apps/ipc-crash-client".as_ptr() as u64, 22, 0) };
    if endpoint <= 0
        || client <= 0
        || ipc_delegate(endpoint as u64, client as u32, IPC_RIGHT_SEND, b"") <= 0
    {
        write(b"ipc-crash-provider: setup FAIL\n");
        exit(1)
    }
    // The first yield runs the client until its CALL blocks. Returning here
    // proves the scheduler did not busy-wait inside the syscall.
    unsafe {
        syscall(SYS_YIELD, 0, 0, 0);
    }
    write(b"ipc-crash-provider: exiting with blocked caller\n");
    exit(77)
}

fn exit(code: i32) -> ! {
    unsafe {
        syscall(SYS_EXIT, code as u64, 0, 0);
    }
    loop {}
}
