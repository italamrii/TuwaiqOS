//! Phase 8 client: consumes delegated endpoint/file capabilities and proves revocation.

#![no_std]
#![no_main]

use core::arch::global_asm;
use hello_user::*;

global_asm!(r#".global _start
_start:
    call {main}
1:  jmp 1b"#, main = sym rust_main);

extern "C" fn rust_main() -> ! {
    let endpoint = ipc_accept(500);
    let file = ipc_accept(500);
    let directory = ipc_accept(500);
    if endpoint <= 0 || file <= 0 || directory <= 0 {
        fail(b"ipc-client: accept");
    }
    let mut file_bytes = [0u8; 64];
    if ipc_fs_request(
        SYS_FS_READ_CAP,
        file as u64,
        b"",
        None,
        Some(&mut file_bytes),
    ) != 22
        || &file_bytes[..22] != b"delegated-file-content"
    {
        fail(b"ipc-client: scoped file read");
    }
    if ipc_fs_request(
        SYS_FS_PUT_CAP,
        file as u64,
        b"",
        Some(b"delegated-updated"),
        None,
    ) != 0
    {
        fail(b"ipc-client: scoped file write");
    }
    file_bytes.fill(0);
    if ipc_fs_request(
        SYS_FS_READ_CAP,
        file as u64,
        b"",
        None,
        Some(&mut file_bytes),
    ) != 17
        || &file_bytes[..17] != b"delegated-updated"
    {
        fail(b"ipc-client: scoped file reopen");
    }
    let mut listing = [0u8; 64];
    let listed = ipc_fs_request(
        SYS_FS_LIST_CAP,
        directory as u64,
        b"",
        None,
        Some(&mut listing),
    );
    if listed <= 0 || !contains(&listing[..listed as usize], b"shared.txt\n") {
        fail(b"ipc-client: scoped directory list");
    }

    // Give a second process RECEIVE-only authority, then transmit this
    // process's raw root handle as data. The child must not be able to reuse it.
    let local = ipc_endpoint_create(2);
    let intruder = spawn(b"/apps/ipc-intruder");
    if local <= 0 || intruder <= 0 {
        fail(b"ipc-client: intruder setup");
    }
    let intruder_revoker = ipc_delegate(local as u64, intruder as u32, IPC_RIGHT_RECEIVE, b"");
    if intruder_revoker <= 0 {
        fail(b"ipc-client: intruder delegation");
    }
    let mut transfer = IpcMessageV1::new(local as u64, 9, 100);
    if !transfer.set_payload(&(local as u64).to_le_bytes())
        || ipc_message_call(SYS_SEND, &mut transfer) != 0
    {
        fail(b"ipc-client: cross-process test transfer");
    }

    let mut request = IpcMessageV1::new(endpoint as u64, 1, 500);
    request.set_payload(b"request");
    if ipc_message_call(SYS_CALL, &mut request) != 5 || &request.payload[..5] != b"reply" {
        fail(b"ipc-client: call reply");
    }
    if ipc_fs_request(
        SYS_FS_READ_CAP,
        file as u64,
        b"",
        None,
        Some(&mut file_bytes),
    ) != IPC_ERR_REVOKED
    {
        fail(b"ipc-client: file revocation");
    }
    if ipc_fs_request(
        SYS_FS_LIST_CAP,
        directory as u64,
        b"",
        None,
        Some(&mut listing),
    ) != IPC_ERR_REVOKED
    {
        fail(b"ipc-client: directory revocation");
    }
    let mut after_revoke = IpcMessageV1::new(endpoint as u64, 3, 0);
    if ipc_message_call(SYS_TRY_SEND, &mut after_revoke) != IPC_ERR_REVOKED {
        fail(b"ipc-client: endpoint revocation");
    }
    for _ in 0..8 {
        call(SYS_YIELD, 0, 0, 0);
    }
    write(b"ipc-client: PASS call reply scoped-read-write-list cross-process-isolation revocation\n");
    exit(0)
}

fn spawn(path: &[u8]) -> i64 {
    call(SYS_SPAWN, path.as_ptr() as u64, path.len() as u64, 0)
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|window| window == needle)
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
