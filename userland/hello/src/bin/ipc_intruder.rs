//! Unauthorized peer checks process-local handles, object types, rights, and close reuse.

#![no_std]
#![no_main]

use core::arch::global_asm;
use hello_user::*;

global_asm!(r#".global _start
_start:
    call {main}
1:  jmp 1b"#, main = sym rust_main);

extern "C" fn rust_main() -> ! {
    let receive_cap = ipc_accept(500);
    if receive_cap <= 0 {
        fail(b"ipc-intruder: accept");
    }
    let mut incoming = IpcMessageV1::new(receive_cap as u64, 0, 500);
    if ipc_message_call(SYS_RECEIVE, &mut incoming) != 8 {
        fail(b"ipc-intruder: receive test token");
    }
    let foreign = u64::from_le_bytes(incoming.payload[..8].try_into().unwrap());
    let mut message = IpcMessageV1::new(foreign, 1, 0);
    if ipc_message_call(SYS_TRY_SEND, &mut message) != IPC_ERR_BAD_HANDLE {
        fail(b"ipc-intruder: cross-process handle reuse");
    }
    message.handle = 0x1122_3344_5566_7788;
    if ipc_message_call(SYS_TRY_SEND, &mut message) != IPC_ERR_BAD_HANDLE {
        fail(b"ipc-intruder: forged handle");
    }
    message.handle = receive_cap as u64;
    if ipc_message_call(SYS_TRY_SEND, &mut message) != IPC_ERR_RIGHTS {
        fail(b"ipc-intruder: missing send right");
    }
    let mut out = [0u8; 8];
    if ipc_fs_request(
        SYS_FS_READ_CAP,
        receive_cap as u64,
        b"",
        None,
        Some(&mut out),
    ) != IPC_ERR_WRONG_TYPE
    {
        fail(b"ipc-intruder: wrong object type");
    }
    if ipc_handle_call(SYS_CAPABILITY_CLOSE, receive_cap as u64) != 0
        || ipc_handle_call(SYS_CAPABILITY_CLOSE, receive_cap as u64) != IPC_ERR_BAD_HANDLE
    {
        fail(b"ipc-intruder: stale or double close");
    }
    write(b"ipc-intruder: PASS forged cross-process wrong-type missing-right stale double-close\n");
    exit(0)
}

fn fail(message: &[u8]) -> ! {
    write(message);
    write(b" FAIL\n");
    exit(1)
}

fn exit(code: i32) -> ! {
    unsafe {
        syscall(SYS_EXIT, code as u64, 0, 0);
    }
    loop {}
}
