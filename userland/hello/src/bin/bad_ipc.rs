//! Hostile Phase 8 ABI/capability inputs. Every rejection must leave Ring 0 healthy.

#![no_std]
#![no_main]

use core::arch::global_asm;
use hello_user::*;

global_asm!(r#".global _start
_start:
    call {main}
1:  jmp 1b"#, main = sym rust_main);

const NONCANONICAL: u64 = 0x0000_8000_0000_0000;
const KERNEL: u64 = 0xffff_8000_0000_0000;
const UNMAPPED: u64 = 0x7000_2000_0000;

extern "C" fn rust_main() -> ! {
    let size = core::mem::size_of::<IpcEndpointCreateV1>() as u64;
    for pointer in [0, NONCANONICAL, KERNEL, UNMAPPED] {
        if raw(SYS_ENDPOINT_CREATE, pointer, size) != IPC_ERR_INVALID {
            fail(b"bad-ipc: invalid pointer");
        }
    }
    let page = call(SYS_MMAP, 4096, 1, 0);
    if page <= 0
        || raw(SYS_ENDPOINT_CREATE, page as u64 + 4096 - 8, size) != IPC_ERR_INVALID
        || raw(SYS_ENDPOINT_CREATE, u64::MAX - 7, size) != IPC_ERR_INVALID
        || raw(SYS_ENDPOINT_CREATE, page as u64, u64::MAX) != IPC_ERR_INVALID
    {
        fail(b"bad-ipc: cross-page overflow length");
    }
    for (number, structure_size) in [
        (SYS_ENDPOINT_CREATE, 16),
        (SYS_ENDPOINT_CLOSE, 16),
        (SYS_SEND, 304),
        (SYS_RECEIVE, 304),
        (SYS_TRY_SEND, 304),
        (SYS_TRY_RECEIVE, 304),
        (SYS_CALL, 304),
        (SYS_REPLY, 304),
        (SYS_CAPABILITY_CLOSE, 16),
        (SYS_CAPABILITY_DELEGATE, 160),
        (SYS_CAPABILITY_ACCEPT, 16),
        (SYS_CAPABILITY_QUERY, 32),
        (SYS_FS_SCOPE_CREATE, 136),
        (SYS_FS_READ, 176),
        (SYS_FS_PUT, 176),
        (SYS_FS_LIST, 176),
    ] {
        for pointer in [0, NONCANONICAL, KERNEL, UNMAPPED] {
            if raw(number, pointer, structure_size) != IPC_ERR_INVALID {
                fail(b"bad-ipc: operation-specific invalid pointer");
            }
        }
        if raw(
            number,
            page as u64 + 4096 - structure_size / 2,
            structure_size,
        ) != IPC_ERR_INVALID
        {
            fail(b"bad-ipc: operation-specific cross-page pointer");
        }
    }

    let mut create = IpcEndpointCreateV1 {
        version: IPC_ABI_VERSION + 1,
        size: size as u16,
        flags: 0,
        depth: 1,
        reserved: 0,
    };
    if structure(SYS_ENDPOINT_CREATE, &mut create) != IPC_ERR_VERSION {
        fail(b"bad-ipc: version");
    }
    create.version = IPC_ABI_VERSION;
    create.flags = 1;
    if structure(SYS_ENDPOINT_CREATE, &mut create) != IPC_ERR_FLAGS {
        fail(b"bad-ipc: flags");
    }
    create.flags = 0;
    let endpoint = structure(SYS_ENDPOINT_CREATE, &mut create);
    if endpoint <= 0 {
        fail(b"bad-ipc: endpoint create");
    }
    match ipc_capability_query(endpoint as u64) {
        Ok((rights, kind))
            if rights
                == (IPC_RIGHT_SEND
                    | IPC_RIGHT_RECEIVE
                    | IPC_RIGHT_REPLY
                    | IPC_RIGHT_DELEGATE
                    | IPC_RIGHT_CLOSE)
                && kind == 1 => {}
        _ => fail(b"bad-ipc: capability query"),
    }
    if ipc_capability_query(0x55aa_dead_beef).is_ok() {
        fail(b"bad-ipc: forged query handle");
    }

    let mut malformed = IpcMessageV1::new(endpoint as u64, 1, 0);
    malformed.payload_len = 257;
    if ipc_message_call(SYS_TRY_SEND, &mut malformed) != IPC_ERR_BAD_MESSAGE {
        fail(b"bad-ipc: oversized message");
    }
    malformed.payload_len = 0;
    malformed.message_type = 0;
    if ipc_message_call(SYS_TRY_SEND, &mut malformed) != IPC_ERR_BAD_MESSAGE {
        fail(b"bad-ipc: reserved message type");
    }

    let mut first = IpcMessageV1::new(endpoint as u64, 1, 0);
    first.set_payload(b"first");
    let mut second = IpcMessageV1::new(endpoint as u64, 2, 0);
    second.set_payload(b"second");
    if ipc_message_call(SYS_TRY_SEND, &mut first) != 0
        || ipc_message_call(SYS_TRY_SEND, &mut second) != IPC_ERR_WOULD_BLOCK
    {
        fail(b"bad-ipc: queue exhaustion");
    }
    let mut received = IpcMessageV1::new(endpoint as u64, 0, 0);
    if ipc_message_call(SYS_TRY_RECEIVE, &mut received) != 5 || &received.payload[..5] != b"first" {
        fail(b"bad-ipc: FIFO or partial publication");
    }
    let mut forged_reply = IpcMessageV1::new(endpoint as u64, 2, 0);
    forged_reply.correlation = 0x55aa_9911;
    if ipc_message_call(SYS_REPLY, &mut forged_reply) != IPC_ERR_BAD_TOKEN {
        fail(b"bad-ipc: forged correlation");
    }

    let old = endpoint as u64;
    expect(
        ipc_handle_call(SYS_ENDPOINT_CLOSE, old),
        0,
        b"bad-ipc: endpoint close",
    );
    expect(
        ipc_message_call(SYS_TRY_SEND, &mut second),
        IPC_ERR_CLOSED,
        b"bad-ipc: send after close",
    );
    received = IpcMessageV1::new(old, 0, 0);
    expect(
        ipc_message_call(SYS_TRY_RECEIVE, &mut received),
        IPC_ERR_CLOSED,
        b"bad-ipc: receive after close",
    );
    expect(
        ipc_handle_call(SYS_CAPABILITY_CLOSE, old),
        0,
        b"bad-ipc: capability close",
    );
    expect(
        ipc_handle_call(SYS_CAPABILITY_CLOSE, old),
        IPC_ERR_BAD_HANDLE,
        b"bad-ipc: double close",
    );

    // Fill and drain the complete process-local table; the failed create
    // must not leave an endpoint or grant behind.
    let mut handles = [0u64; 32];
    let mut count = 0usize;
    loop {
        let value = ipc_endpoint_create(1);
        if value == IPC_ERR_EXHAUSTED {
            break;
        }
        if value <= 0 || count == handles.len() {
            fail(b"bad-ipc: capability exhaustion bound");
        }
        handles[count] = value as u64;
        count += 1;
    }
    if count != 32 {
        fail(b"bad-ipc: capability table capacity");
    }
    for handle in &handles[..count] {
        if ipc_handle_call(SYS_CAPABILITY_CLOSE, *handle) != 0 {
            fail(b"bad-ipc: capability cleanup");
        }
    }
    let replacement = ipc_endpoint_create(1);
    if replacement <= 0 || ipc_handle_call(SYS_ENDPOINT_CLOSE, old) != IPC_ERR_BAD_HANDLE {
        fail(b"bad-ipc: stale generation reuse");
    }
    ipc_handle_call(SYS_CAPABILITY_CLOSE, replacement as u64);

    let waiter = spawn(b"/apps/ipc-waiter");
    let scope = ipc_fs_scope(
        b"/data/bad-ipc",
        IPC_RIGHT_FILE_READ | IPC_RIGHT_DELEGATE | IPC_RIGHT_CLOSE,
    );
    if waiter <= 0 || scope <= 0 {
        fail(b"bad-ipc: scope setup");
    }
    let mut bad_read = IpcFsRequestV1 {
        version: IPC_ABI_VERSION,
        size: core::mem::size_of::<IpcFsRequestV1>() as u16,
        flags: 0,
        handle: scope as u64,
        path_len: 0,
        reserved16: 0,
        reserved32: 0,
        data_ptr: 0,
        data_len: 0,
        out_ptr: NONCANONICAL,
        out_len: 8,
        path: [0; 120],
    };
    if structure(SYS_FS_READ, &mut bad_read) != IPC_ERR_INVALID {
        fail(b"bad-ipc: delegated read output pointer");
    }
    bad_read.data_ptr = NONCANONICAL;
    bad_read.data_len = 1;
    bad_read.out_ptr = 0;
    bad_read.out_len = 0;
    if structure(SYS_FS_PUT, &mut bad_read) != IPC_ERR_INVALID {
        fail(b"bad-ipc: delegated write input pointer");
    }
    bad_read.data_ptr = 0;
    bad_read.data_len = 0;
    bad_read.out_ptr = page as u64 + 4092;
    bad_read.out_len = 8;
    if structure(SYS_FS_LIST, &mut bad_read) != IPC_ERR_INVALID {
        fail(b"bad-ipc: delegated list cross-page output");
    }
    if ipc_delegate(
        scope as u64,
        waiter as u32,
        IPC_RIGHT_FILE_READ,
        b"../ipc-provider/shared.txt",
    ) != IPC_ERR_RIGHTS
        || ipc_delegate(
            scope as u64,
            waiter as u32,
            IPC_RIGHT_FILE_READ,
            b"../../boot/README.TXT",
        ) != IPC_ERR_RIGHTS
        || ipc_delegate(
            scope as u64,
            waiter as u32,
            IPC_RIGHT_FILE_READ | IPC_RIGHT_FILE_WRITE,
            b"",
        ) != IPC_ERR_RIGHTS
    {
        fail(b"bad-ipc: delegated scope escape/amplification");
    }
    if ipc_handle_call(SYS_CAPABILITY_CLOSE, scope as u64) != 0
        || ipc_delegate(scope as u64, waiter as u32, IPC_RIGHT_FILE_READ, b"") != IPC_ERR_BAD_HANDLE
    {
        fail(b"bad-ipc: delegation after close");
    }
    call(SYS_MUNMAP, page as u64, 4096, 0);
    write(b"bad-ipc: PASS pointers ABI queue tokens handles rights paths exhaustion cleanup\n");
    exit(0)
}

fn raw(number: u64, pointer: u64, len: u64) -> i64 {
    call(number, pointer, len, 0)
}

fn structure<T>(number: u64, value: &mut T) -> i64 {
    raw(
        number,
        value as *mut T as u64,
        core::mem::size_of::<T>() as u64,
    )
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

fn expect(actual: i64, expected: i64, message: &[u8]) {
    if actual != expected {
        write(message);
        write(b" actual=");
        let mut digits = [0u8; 20];
        if actual < 0 {
            write(b"-");
        }
        write(u64_to_decimal(actual.unsigned_abs(), &mut digits));
        write(b" expected=");
        if expected < 0 {
            write(b"-");
        }
        write(u64_to_decimal(expected.unsigned_abs(), &mut digits));
        write(b" FAIL\n");
        exit(1);
    }
}

fn exit(code: i32) -> ! {
    unsafe {
        syscall(SYS_EXIT, code as u64, 0, 0);
    }
    loop {}
}
