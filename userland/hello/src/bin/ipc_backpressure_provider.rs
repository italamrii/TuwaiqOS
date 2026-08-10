//! A full bounded queue blocks without spinning and wakes when FIFO space opens.

#![no_std]
#![no_main]

use core::arch::global_asm;
use hello_user::*;

global_asm!(r#".global _start
_start:
    call {main}
1:  jmp 1b"#, main = sym rust_main);

extern "C" fn rust_main() -> ! {
    let endpoint = ipc_endpoint_create(1);
    let client = spawn(b"/apps/ipc-backpressure-client");
    if endpoint <= 0
        || client <= 0
        || ipc_delegate(endpoint as u64, client as u32, IPC_RIGHT_RECEIVE, b"") <= 0
    {
        fail(b"ipc-backpressure-provider: setup");
    }
    let mut first = IpcMessageV1::new(endpoint as u64, 1, 100);
    first.set_payload(b"one");
    let mut second = IpcMessageV1::new(endpoint as u64, 2, 100);
    second.set_payload(b"two");
    if ipc_message_call(SYS_SEND, &mut first) != 0
        || ipc_message_call(SYS_SEND, &mut second) != 0
    {
        fail(b"ipc-backpressure-provider: blocking send");
    }
    // The second send returning proves it was published, but endpoint-owner
    // exit deliberately cancels queued data. Let the woken receiver consume
    // that FIFO element before testing teardown.
    unsafe {
        syscall(SYS_YIELD, 0, 0, 0);
    }
    write(b"ipc-backpressure-provider: PASS full queue blocked and woke without loss\n");
    exit(0)
}

fn spawn(path: &[u8]) -> i64 {
    unsafe { syscall(SYS_SPAWN, path.as_ptr() as u64, path.len() as u64, 0) }
}

fn fail(message: &[u8]) -> ! {
    write(message);
    write(b" FAIL\n");
    exit(1)
}

fn exit(code: i32) -> ! {
    unsafe { syscall(SYS_EXIT, code as u64, 0, 0); }
    loop {}
}
