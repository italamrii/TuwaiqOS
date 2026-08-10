//! Phase 8 provider: delegates least privilege, serves one call, then revokes.

#![no_std]
#![no_main]

use core::arch::global_asm;
use hello_user::*;

global_asm!(r#".global _start
_start:
    call {main}
1:  jmp 1b"#, main = sym rust_main);

#[repr(C)]
struct BufferSpec {
    pointer: u64,
    length: u64,
}

extern "C" fn rust_main() -> ! {
    let data = b"delegated-file-content";
    let spec = BufferSpec {
        pointer: data.as_ptr() as u64,
        length: data.len() as u64,
    };
    if call(
        SYS_PUT_FILE,
        b"/data/ipc-provider/shared.txt".as_ptr() as u64,
        29,
        &spec as *const BufferSpec as u64,
    ) != 0
    {
        fail(b"ipc-provider: private file setup");
    }

    let endpoint = ipc_endpoint_create(4);
    let scope = ipc_fs_scope(
        b"/data/ipc-provider/shared.txt",
        IPC_RIGHT_FILE_READ | IPC_RIGHT_FILE_WRITE | IPC_RIGHT_DELEGATE | IPC_RIGHT_CLOSE,
    );
    let directory_scope = ipc_fs_scope(
        b"/data/ipc-provider",
        IPC_RIGHT_FILE_LIST | IPC_RIGHT_DELEGATE | IPC_RIGHT_CLOSE,
    );
    let client = spawn(b"/apps/ipc-client");
    if endpoint <= 0 || scope <= 0 || directory_scope <= 0 || client <= 0 {
        fail(b"ipc-provider: setup");
    }
    let endpoint_revoker = ipc_delegate(endpoint as u64, client as u32, IPC_RIGHT_SEND, b"");
    let file_revoker = ipc_delegate(
        scope as u64,
        client as u32,
        IPC_RIGHT_FILE_READ | IPC_RIGHT_FILE_WRITE,
        b"",
    );
    let directory_revoker = ipc_delegate(
        directory_scope as u64,
        client as u32,
        IPC_RIGHT_FILE_LIST,
        b"",
    );
    if endpoint_revoker <= 0 || file_revoker <= 0 || directory_revoker <= 0 {
        fail(b"ipc-provider: delegation");
    }

    let mut request = IpcMessageV1::new(endpoint as u64, 0, 500);
    if ipc_message_call(SYS_RECEIVE, &mut request) != 7
        || &request.payload[..7] != b"request"
        || request.correlation == 0
    {
        fail(b"ipc-provider: receive");
    }
    request.message_type = 2;
    request.timeout_ticks = 0;
    request.payload_len = 0;
    if !request.set_payload(b"reply") || ipc_message_call(SYS_REPLY, &mut request) != 0 {
        fail(b"ipc-provider: reply");
    }
    if ipc_message_call(SYS_REPLY, &mut request) != IPC_ERR_DUPLICATE {
        fail(b"ipc-provider: duplicate reply");
    }
    if ipc_handle_call(SYS_CAPABILITY_CLOSE, file_revoker as u64) != 0
        || ipc_handle_call(SYS_CAPABILITY_CLOSE, directory_revoker as u64) != 0
        || ipc_handle_call(SYS_CAPABILITY_CLOSE, endpoint_revoker as u64) != 0
    {
        fail(b"ipc-provider: revoke");
    }
    write(b"ipc-provider: PASS request reply duplicate-rejection least-rights file-scope delegation revocation\n");
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
    unsafe {
        syscall(SYS_EXIT, code as u64, 0, 0);
    }
    loop {}
}
