//! The caller must wake exactly once with deterministic timeout.

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
    let mut request = IpcMessageV1::new(endpoint as u64, 1, 3);
    request.set_payload(b"timeout");
    if endpoint <= 0 || ipc_message_call(SYS_CALL, &mut request) != IPC_ERR_TIMEOUT {
        write(b"ipc-timeout-client: deterministic timeout FAIL\n");
        exit(1)
    }
    write(b"ipc-timeout-client: PASS deterministic timeout and single wake\n");
    exit(0)
}

fn exit(code: i32) -> ! {
    unsafe { syscall(SYS_EXIT, code as u64, 0, 0); }
    loop {}
}
