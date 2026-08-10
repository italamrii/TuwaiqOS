//! Call timeout, late-reply rejection, and exact-once wake test provider.

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
    let client = spawn(b"/apps/ipc-timeout-client");
    if endpoint <= 0
        || client <= 0
        || ipc_delegate(endpoint as u64, client as u32, IPC_RIGHT_SEND, b"") <= 0
    {
        fail(b"ipc-timeout-provider: setup");
    }
    let mut request = IpcMessageV1::new(endpoint as u64, 0, 100);
    if ipc_message_call(SYS_RECEIVE, &mut request) != 7 || request.correlation == 0 {
        fail(b"ipc-timeout-provider: receive");
    }
    let start = call(SYS_UPTIME_TICKS, 0, 0, 0);
    while call(SYS_UPTIME_TICKS, 0, 0, 0) < start + 6 {
        call(SYS_YIELD, 0, 0, 0);
    }
    request.message_type = 2;
    request.timeout_ticks = 0;
    request.set_payload(b"late");
    if ipc_message_call(SYS_REPLY, &mut request) != IPC_ERR_BAD_TOKEN {
        fail(b"ipc-timeout-provider: late reply accepted");
    }
    write(b"ipc-timeout-provider: PASS late reply rejected after caller timeout\n");
    exit(0)
}

fn spawn(path: &[u8]) -> i64 {
    call(SYS_SPAWN, path.as_ptr() as u64, path.len() as u64, 0)
}

fn call(number: u64, a1: u64, a2: u64, a3: u64) -> i64 {
    unsafe { syscall(number, a1, a2, a3) }
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
