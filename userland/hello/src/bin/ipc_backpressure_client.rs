//! FIFO receiver for the blocking-send/backpressure proof.

#![no_std]
#![no_main]

use core::arch::global_asm;
use hello_user::*;

global_asm!(r#".global _start
_start:
    call {main}
1:  jmp 1b"#, main = sym rust_main);

extern "C" fn rust_main() -> ! {
    let endpoint = ipc_accept(100);
    let mut first = IpcMessageV1::new(endpoint as u64, 0, 100);
    let mut second = IpcMessageV1::new(endpoint as u64, 0, 100);
    if endpoint <= 0
        || ipc_message_call(SYS_RECEIVE, &mut first) != 3
        || &first.payload[..3] != b"one"
        || ipc_message_call(SYS_RECEIVE, &mut second) != 3
        || &second.payload[..3] != b"two"
    {
        write(b"ipc-backpressure-client: FIFO receive FAIL\n");
        exit(1)
    }
    write(b"ipc-backpressure-client: PASS FIFO order and sender wake\n");
    exit(0)
}

fn exit(code: i32) -> ! {
    unsafe { syscall(SYS_EXIT, code as u64, 0, 0); }
    loop {}
}
