//! Shared syscall ABI helper for this crate's test binaries
//! (`src/bin/*.rs`). See `kernel/src/syscall.rs` for the authoritative ABI
//! documentation -- this is just a thin, duplicated-nowhere wrapper around
//! it: `int 0x80`, `RAX` = syscall number in / return value out,
//! `RDI`/`RSI`/`RDX` = args 1-3.

#![no_std]

use core::arch::asm;
use core::panic::PanicInfo;

pub const SYS_EXIT: u64 = 0;
pub const SYS_WRITE: u64 = 1;
pub const SYS_YIELD: u64 = 2;
pub const SYS_GETPID: u64 = 3;
pub const SYS_MMAP: u64 = 4;
pub const SYS_MUNMAP: u64 = 5;
pub const SYS_DISPLAY_INFO: u64 = 6;
pub const SYS_DISPLAY_PRESENT: u64 = 7;
pub const SYS_INPUT_POLL: u64 = 8;
pub const SYS_UPTIME_TICKS: u64 = 9;
pub const SYS_CHDIR: u64 = 10;
pub const SYS_GETCWD: u64 = 11;
pub const SYS_OPEN: u64 = 12;
pub const SYS_READ: u64 = 13;
pub const SYS_CLOSE: u64 = 14;
pub const SYS_SPAWN: u64 = 15;
pub const SYS_PUT_FILE: u64 = 16;
pub const SYS_REMOVE: u64 = 17;
pub const SYS_MKDIR: u64 = 18;
pub const SYS_READDIR: u64 = 19;
pub const SYS_STAT: u64 = 20;
pub const SYS_SEEK: u64 = 21;
pub const SYS_ENDPOINT_CREATE: u64 = 22;
pub const SYS_ENDPOINT_CLOSE: u64 = 23;
pub const SYS_SEND: u64 = 24;
pub const SYS_RECEIVE: u64 = 25;
pub const SYS_TRY_SEND: u64 = 26;
pub const SYS_TRY_RECEIVE: u64 = 27;
pub const SYS_CALL: u64 = 28;
pub const SYS_REPLY: u64 = 29;
pub const SYS_CAPABILITY_CLOSE: u64 = 30;
pub const SYS_CAPABILITY_DELEGATE: u64 = 31;
pub const SYS_CAPABILITY_ACCEPT: u64 = 32;
pub const SYS_FS_SCOPE_CREATE: u64 = 33;
pub const SYS_FS_READ: u64 = 34;
pub const SYS_FS_PUT: u64 = 35;
pub const SYS_FS_LIST: u64 = 36;
pub const SYS_CAPABILITY_QUERY: u64 = 37;
/// Compatibility alias used by existing Phase 8 demos.
pub const SYS_FS_READ_CAP: u64 = SYS_FS_READ;
pub const SYS_FS_PUT_CAP: u64 = SYS_FS_PUT;
pub const SYS_FS_LIST_CAP: u64 = SYS_FS_LIST;

pub const IPC_ABI_VERSION: u16 = 1;
pub const IPC_MAX_MESSAGE_BYTES: usize = 256;
pub const IPC_RIGHT_SEND: u32 = 1 << 0;
pub const IPC_RIGHT_RECEIVE: u32 = 1 << 1;
pub const IPC_RIGHT_REPLY: u32 = 1 << 2;
pub const IPC_RIGHT_DELEGATE: u32 = 1 << 3;
pub const IPC_RIGHT_CLOSE: u32 = 1 << 4;
pub const IPC_RIGHT_FILE_READ: u32 = 1 << 5;
pub const IPC_RIGHT_FILE_WRITE: u32 = 1 << 6;
pub const IPC_RIGHT_FILE_LIST: u32 = 1 << 7;

pub const IPC_ERR_INVALID: i64 = -1;
pub const IPC_ERR_VERSION: i64 = -2;
pub const IPC_ERR_FLAGS: i64 = -3;
pub const IPC_ERR_BAD_HANDLE: i64 = -4;
pub const IPC_ERR_WRONG_TYPE: i64 = -5;
pub const IPC_ERR_RIGHTS: i64 = -6;
pub const IPC_ERR_WOULD_BLOCK: i64 = -7;
pub const IPC_ERR_CLOSED: i64 = -8;
pub const IPC_ERR_TIMEOUT: i64 = -9;
pub const IPC_ERR_PEER_EXITED: i64 = -10;
pub const IPC_ERR_EXHAUSTED: i64 = -11;
pub const IPC_ERR_BAD_MESSAGE: i64 = -12;
pub const IPC_ERR_REVOKED: i64 = -13;
pub const IPC_ERR_BAD_TOKEN: i64 = -14;
pub const IPC_ERR_DUPLICATE: i64 = -15;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct IpcMessageV1 {
    pub version: u16,
    pub size: u16,
    pub flags: u32,
    pub handle: u64,
    pub message_type: u32,
    pub payload_len: u32,
    pub timeout_ticks: u64,
    pub correlation: u64,
    pub sender_pid: u32,
    pub reserved: u32,
    pub payload: [u8; IPC_MAX_MESSAGE_BYTES],
}

impl IpcMessageV1 {
    pub const fn new(handle: u64, message_type: u32, timeout_ticks: u64) -> Self {
        Self {
            version: IPC_ABI_VERSION,
            size: core::mem::size_of::<Self>() as u16,
            flags: 0,
            handle,
            message_type,
            payload_len: 0,
            timeout_ticks,
            correlation: 0,
            sender_pid: 0,
            reserved: 0,
            payload: [0; IPC_MAX_MESSAGE_BYTES],
        }
    }

    pub fn set_payload(&mut self, bytes: &[u8]) -> bool {
        if bytes.len() > IPC_MAX_MESSAGE_BYTES {
            return false;
        }
        self.payload.fill(0);
        self.payload[..bytes.len()].copy_from_slice(bytes);
        self.payload_len = bytes.len() as u32;
        true
    }
}

#[repr(C)]
pub struct IpcEndpointCreateV1 {
    pub version: u16,
    pub size: u16,
    pub flags: u32,
    pub depth: u32,
    pub reserved: u32,
}

#[repr(C)]
pub struct IpcHandleV1 {
    pub version: u16,
    pub size: u16,
    pub flags: u32,
    pub handle: u64,
}

#[repr(C)]
pub struct IpcAcceptV1 {
    pub version: u16,
    pub size: u16,
    pub flags: u32,
    pub timeout_ticks: u64,
}

#[repr(C)]
pub struct IpcCapQueryV1 {
    pub version: u16,
    pub size: u16,
    pub flags: u32,
    pub handle: u64,
    pub rights: u32,
    pub object_kind: u32,
    pub reserved: u64,
}

#[repr(C)]
pub struct IpcDelegateV1 {
    pub version: u16,
    pub size: u16,
    pub flags: u32,
    pub source: u64,
    pub target_pid: u32,
    pub rights: u32,
    pub path_len: u16,
    pub reserved16: u16,
    pub reserved32: u32,
    pub path: [u8; 128],
}

#[repr(C)]
pub struct IpcFsScopeV1 {
    pub version: u16,
    pub size: u16,
    pub flags: u32,
    pub rights: u32,
    pub path_len: u16,
    pub reserved: u16,
    pub path: [u8; 120],
}

#[repr(C)]
pub struct IpcFsRequestV1 {
    pub version: u16,
    pub size: u16,
    pub flags: u32,
    pub handle: u64,
    pub path_len: u16,
    pub reserved16: u16,
    pub reserved32: u32,
    pub data_ptr: u64,
    pub data_len: u64,
    pub out_ptr: u64,
    pub out_len: u64,
    pub path: [u8; 120],
}

const _: [(); 304] = [(); core::mem::size_of::<IpcMessageV1>()];
const _: [(); 16] = [(); core::mem::size_of::<IpcEndpointCreateV1>()];
const _: [(); 16] = [(); core::mem::size_of::<IpcHandleV1>()];
const _: [(); 16] = [(); core::mem::size_of::<IpcAcceptV1>()];
const _: [(); 32] = [(); core::mem::size_of::<IpcCapQueryV1>()];
const _: [(); 160] = [(); core::mem::size_of::<IpcDelegateV1>()];
const _: [(); 136] = [(); core::mem::size_of::<IpcFsScopeV1>()];
const _: [(); 176] = [(); core::mem::size_of::<IpcFsRequestV1>()];

fn ipc_struct_call<T>(number: u64, value: &mut T) -> i64 {
    unsafe {
        syscall(
            number,
            value as *mut T as u64,
            core::mem::size_of::<T>() as u64,
            0,
        )
    }
}

pub fn ipc_endpoint_create(depth: u32) -> i64 {
    let mut value = IpcEndpointCreateV1 {
        version: IPC_ABI_VERSION,
        size: core::mem::size_of::<IpcEndpointCreateV1>() as u16,
        flags: 0,
        depth,
        reserved: 0,
    };
    ipc_struct_call(SYS_ENDPOINT_CREATE, &mut value)
}

pub fn ipc_handle_call(number: u64, handle: u64) -> i64 {
    let mut value = IpcHandleV1 {
        version: IPC_ABI_VERSION,
        size: core::mem::size_of::<IpcHandleV1>() as u16,
        flags: 0,
        handle,
    };
    ipc_struct_call(number, &mut value)
}

pub fn ipc_message_call(number: u64, message: &mut IpcMessageV1) -> i64 {
    ipc_struct_call(number, message)
}

pub fn ipc_accept(timeout_ticks: u64) -> i64 {
    let mut value = IpcAcceptV1 {
        version: IPC_ABI_VERSION,
        size: core::mem::size_of::<IpcAcceptV1>() as u16,
        flags: 0,
        timeout_ticks,
    };
    ipc_struct_call(SYS_CAPABILITY_ACCEPT, &mut value)
}

pub fn ipc_capability_query(handle: u64) -> Result<(u32, u32), i64> {
    let mut value = IpcCapQueryV1 {
        version: IPC_ABI_VERSION,
        size: core::mem::size_of::<IpcCapQueryV1>() as u16,
        flags: 0,
        handle,
        rights: 0,
        object_kind: 0,
        reserved: 0,
    };
    let result = ipc_struct_call(SYS_CAPABILITY_QUERY, &mut value);
    if result < 0 {
        Err(result)
    } else {
        Ok((value.rights, value.object_kind))
    }
}

pub fn ipc_delegate(source: u64, target_pid: u32, rights: u32, path: &[u8]) -> i64 {
    if path.len() > 120 {
        return IPC_ERR_INVALID;
    }
    let mut value = IpcDelegateV1 {
        version: IPC_ABI_VERSION,
        size: core::mem::size_of::<IpcDelegateV1>() as u16,
        flags: 0,
        source,
        target_pid,
        rights,
        path_len: path.len() as u16,
        reserved16: 0,
        reserved32: 0,
        path: [0; 128],
    };
    value.path[..path.len()].copy_from_slice(path);
    ipc_struct_call(SYS_CAPABILITY_DELEGATE, &mut value)
}

pub fn ipc_fs_scope(path: &[u8], rights: u32) -> i64 {
    if path.len() > 120 {
        return IPC_ERR_INVALID;
    }
    let mut value = IpcFsScopeV1 {
        version: IPC_ABI_VERSION,
        size: core::mem::size_of::<IpcFsScopeV1>() as u16,
        flags: 0,
        rights,
        path_len: path.len() as u16,
        reserved: 0,
        path: [0; 120],
    };
    value.path[..path.len()].copy_from_slice(path);
    ipc_struct_call(SYS_FS_SCOPE_CREATE, &mut value)
}

pub fn ipc_fs_request(
    number: u64,
    handle: u64,
    path: &[u8],
    data: Option<&[u8]>,
    output: Option<&mut [u8]>,
) -> i64 {
    if path.len() > 120 {
        return IPC_ERR_INVALID;
    }
    let (data_ptr, data_len) = data
        .map(|bytes| (bytes.as_ptr() as u64, bytes.len() as u64))
        .unwrap_or((0, 0));
    let (out_ptr, out_len) = output
        .map(|bytes| (bytes.as_mut_ptr() as u64, bytes.len() as u64))
        .unwrap_or((0, 0));
    let mut value = IpcFsRequestV1 {
        version: IPC_ABI_VERSION,
        size: core::mem::size_of::<IpcFsRequestV1>() as u16,
        flags: 0,
        handle,
        path_len: path.len() as u16,
        reserved16: 0,
        reserved32: 0,
        data_ptr,
        data_len,
        out_ptr,
        out_len,
        path: [0; 120],
    };
    value.path[..path.len()].copy_from_slice(path);
    ipc_struct_call(number, &mut value)
}

/// # Safety
/// Caller is responsible for `num`/`a1`/`a2`/`a3` meaning what the
/// kernel's syscall ABI expects for that syscall number.
#[inline(always)]
pub unsafe fn syscall(num: u64, a1: u64, a2: u64, a3: u64) -> i64 {
    let ret: i64;
    // Safety: `int 0x80` is the kernel's documented, DPL=3 syscall gate;
    // every register the asm reads or writes is declared, so the
    // compiler's view of clobbered state stays accurate.
    unsafe {
        asm!(
            "int 0x80",
            inlateout("rax") num => ret,
            in("rdi") a1,
            in("rsi") a2,
            in("rdx") a3,
            options(nostack),
        );
    }
    ret
}

pub fn write(msg: &[u8]) {
    // Safety: `msg` is a valid slice for its own lifetime; `write` only
    // reads `len` bytes starting at `ptr`.
    unsafe {
        syscall(SYS_WRITE, msg.as_ptr() as u64, msg.len() as u64, 0);
    }
}

pub fn u64_to_decimal(mut value: u64, buf: &mut [u8; 20]) -> &[u8] {
    if value == 0 {
        buf[0] = b'0';
        return &buf[..1];
    }
    let mut i = buf.len();
    while value > 0 {
        i -= 1;
        buf[i] = b'0' + (value % 10) as u8;
        value /= 10;
    }
    &buf[i..]
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    // Safety: exit code 1, no pointers involved.
    unsafe {
        syscall(SYS_EXIT, 1, 0, 0);
    }
    loop {}
}
