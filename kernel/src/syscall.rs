//! The real syscall ABI (Phase 4) -- what Milestone 1's `int 0x80` "the
//! demo is over, come back to Ring 0" gate has become now that user
//! processes are real and need to make more than one round trip.
//!
//! ## ABI
//!
//! `int 0x80`. `RAX` is the syscall number on entry and the return value on
//! exit. Arguments are `RDI`, `RSI`, `RDX` (up to three -- nothing here
//! needs more). Return values for syscalls 0-21 follow a simple POSIX-ish
//! convention: `>= 0` is success (a byte count, a pid, or plain `0`), and
//! `-1` is a generic failure. Syscalls 22-37 are the versioned IPC/capability
//! surface: they accept a structure pointer in `RDI` and exact length in
//! `RSI`, and return either a nonnegative result or one of the stable IPC
//! error codes documented in `docs/IPC_ABI.md` (for example `-2` version,
//! `-4` bad handle, `-6` rights). Unknown syscall numbers still return `-1`.
//!
//! | # | name           | args                          | returns                     |
//! |---|----------------|-------------------------------|------------------------------|
//! | 0 | EXIT           | `code: i32`                   | never returns                |
//! | 1 | WRITE          | `ptr: *u8, len: usize`        | bytes written, or `-1`       |
//! | 2 | YIELD          | --                             | `0`                          |
//! | 3 | GETPID         | --                             | this process's task id       |
//! | 4 | MMAP           | `len: usize, writable: bool`  | new region's address, or `-1`|
//! | 5 | MUNMAP         | `ptr: *u8, len: usize`        | `0`, or `-1`                 |
//! | 6 | DISPLAY_INFO   | `out_ptr: *mut u8, out_len`   | `0`, or `-1`                 |
//! | 7 | DISPLAY_PRESENT| `ptr: *u8, len: usize`        | `0`, or `-1`                 |
//! | 8 | INPUT_POLL     | `out_ptr: *mut u8, out_len`   | `1` (event written)/`0`/`-1` |
//! | 9 | UPTIME_TICKS   | --                             | PIT ticks since boot         |
//! |10 | CHDIR          | `path_ptr, path_len`           | `0`, or `-1`                 |
//! |11 | GETCWD         | `out_ptr, out_len`             | path bytes, or `-1`          |
//! |12 | OPEN           | `path_ptr, path_len`           | read handle, or `-1`         |
//! |13 | READ           | `handle, out_ptr, out_len`     | bytes read, or `-1`          |
//! |14 | CLOSE          | `handle`                       | `0`, or `-1`                 |
//! |15 | SPAWN          | `path_ptr, path_len`           | child pid, or `-1`           |
//! |16 | PUT_FILE       | `path_ptr, path_len, spec_ptr` | `0`, or `-1`                 |
//! |17 | REMOVE         | `path_ptr, path_len`           | `0`, or `-1`                 |
//! |18 | MKDIR          | `path_ptr, path_len`           | `0`, or `-1`                 |
//! |19 | READDIR        | `path_ptr, path_len, spec_ptr` | bytes listed, or `-1`        |
//! |20 | STAT           | `path_ptr, path_len, out_ptr`  | `0`, or `-1`                 |
//! |21 | SEEK           | `handle, absolute_offset`      | new offset, or `-1`          |
//! |22-37| IPC/CAPS/VFS | versioned structure pointer/len| result or stable IPC error  |
//!
//! (Phase 5 -- see `ARCHITECTURE.md`'s "Phase 5: userland runtime and the
//! first graphical desktop" section for the design behind 4-9.)
//!
//! Any other number is rejected with `-1` -- logged, not a fault, and the
//! calling process keeps running (see `dispatch`'s `_` arm).
//!
//! ## Entry mechanism
//!
//! `int 0x80` is a normal (non-exception) interrupt gate at DPL=3 (see
//! `interrupts.rs`), so no error code is pushed and the only privilege
//! change is Ring 3 -> Ring 0. Unlike every other handler in this kernel,
//! this one is **not** `extern "x86-interrupt"`: that calling convention
//! only exposes the CPU-pushed frame (`InterruptStackFrame`), not general
//! purpose registers, and the ABI above needs to read `RAX`/`RDI`/`RSI`/
//! `RDX` and write a return value back into `RAX`. Instead, `syscall_entry`
//! is a hand-written naked stub (same technique as `task.rs`'s
//! `context_switch`): it saves all 15 general-purpose registers it might
//! plausibly need to preserve, calls into Rust with a pointer to them,
//! restores them (with `RAX` now holding the dispatch result), and
//! `iretq`s back to Ring 3 -- except for `EXIT`, which ends the task
//! instead (see `sys_exit`), abandoning that saved-register frame exactly
//! the way `usermode.rs`'s retired demo abandoned its own call chain.
//!
//! ## Pointer validation
//!
//! `WRITE`, `MUNMAP`, `DISPLAY_INFO`, `DISPLAY_PRESENT`, `INPUT_POLL`, and
//! the Phase 6 path/file calls
//! accept Ring-3-supplied addresses. Every raw address is converted with the
//! fallible `VirtAddr::try_new` path, constrained to the private user region,
//! checked for range overflow, and walked through the calling process's own
//! page tables before any byte is read, written, presented, or unmapped.
//! Required permissions are checked for the whole range before mutation. A
//! malformed address or range is a clean `-1`, never a Ring-0 panic or fault.

use crate::{ipc, task};

const SYS_EXIT: u64 = 0;
const SYS_WRITE: u64 = 1;
const SYS_YIELD: u64 = 2;
const SYS_GETPID: u64 = 3;
const SYS_MMAP: u64 = 4;
const SYS_MUNMAP: u64 = 5;
const SYS_DISPLAY_INFO: u64 = 6;
const SYS_DISPLAY_PRESENT: u64 = 7;
const SYS_INPUT_POLL: u64 = 8;
const SYS_UPTIME_TICKS: u64 = 9;
const SYS_CHDIR: u64 = 10;
const SYS_GETCWD: u64 = 11;
const SYS_OPEN: u64 = 12;
const SYS_READ: u64 = 13;
const SYS_CLOSE: u64 = 14;
const SYS_SPAWN: u64 = 15;
const SYS_PUT_FILE: u64 = 16;
const SYS_REMOVE: u64 = 17;
const SYS_MKDIR: u64 = 18;
const SYS_READDIR: u64 = 19;
const SYS_STAT: u64 = 20;
const SYS_SEEK: u64 = 21;
const SYS_ENDPOINT_CREATE: u64 = 22;
const SYS_ENDPOINT_CLOSE: u64 = 23;
const SYS_SEND: u64 = 24;
const SYS_RECEIVE: u64 = 25;
const SYS_TRY_SEND: u64 = 26;
const SYS_TRY_RECEIVE: u64 = 27;
const SYS_CALL: u64 = 28;
const SYS_REPLY: u64 = 29;
const SYS_CAPABILITY_CLOSE: u64 = 30;
const SYS_CAPABILITY_DELEGATE: u64 = 31;
const SYS_CAPABILITY_ACCEPT: u64 = 32;
const SYS_FS_SCOPE_CREATE: u64 = 33;
const SYS_FS_READ: u64 = 34;
const SYS_FS_PUT: u64 = 35;
const SYS_FS_LIST: u64 = 36;
const SYS_CAPABILITY_QUERY: u64 = 37;

/// Upper bound on a single `WRITE`'s length -- generous for this ABI's
/// only real use (a handful of short diagnostic lines from `hello_user`),
/// and a firm cap on how much a single syscall can make the kernel copy on
/// a caller's behalf regardless of what `len` claims.
const MAX_WRITE_LEN: usize = 4096;
const MAX_FILE_IO_LEN: usize = 4096;
const BUFFER_SPEC_LEN: usize = 16;
const STAT_RECORD_LEN: usize = 16;

/// The 15 general-purpose registers `syscall_entry` saves, in the exact
/// order they land in memory (lowest address first) given the push order
/// in the asm below -- `r15` first (pushed last), `rax` last (pushed
/// first). `rdi`/`rsi`/`rdx` are read directly by `syscall_dispatch` from
/// the arguments the trap arrived with; `rax` is read for the syscall
/// number and overwritten with the return value before this frame is
/// popped back into real registers.
#[repr(C)]
struct SyscallFrame {
    r15: u64,
    r14: u64,
    r13: u64,
    r12: u64,
    r11: u64,
    r10: u64,
    r9: u64,
    r8: u64,
    rbp: u64,
    rdi: u64,
    rsi: u64,
    rdx: u64,
    rcx: u64,
    rbx: u64,
    rax: u64,
}

// Safety: this is the syscall gate's IDT entry point (registered in
// `interrupts.rs` at DPL=3), reached only via `int 0x80` from Ring 3. The
// prologue saves all 15 GPRs (120 bytes -- keeping RSP 16-byte aligned
// right before `call {dispatch}`, matching the SysV requirement, given
// RSP0 itself is 16-byte aligned -- see `task.rs`'s `aligned_top`) before
// touching anything, and the epilogue restores every one of them from
// exactly the same memory, in reverse, before `iretq`. `EXIT`'s path
// through `syscall_dispatch` never returns here at all -- see the module
// docs.
core::arch::global_asm!(
    r#"
.global syscall_entry
syscall_entry:
    push rax
    push rbx
    push rcx
    push rdx
    push rsi
    push rdi
    push rbp
    push r8
    push r9
    push r10
    push r11
    push r12
    push r13
    push r14
    push r15
    mov rdi, rsp
    call {dispatch}
    pop r15
    pop r14
    pop r13
    pop r12
    pop r11
    pop r10
    pop r9
    pop r8
    pop rbp
    pop rdi
    pop rsi
    pop rdx
    pop rcx
    pop rbx
    pop rax
    iretq
"#,
    dispatch = sym syscall_dispatch,
);

extern "C" {
    pub fn syscall_entry();
}

extern "C" fn syscall_dispatch(frame: *mut SyscallFrame) {
    // Safety: `frame` was constructed by `syscall_entry`'s own prologue,
    // immediately before this call, pointing at a live, exclusively-owned
    // region of the current task's own kernel stack -- valid for exactly
    // the duration of this call. Long VM syscalls may enable interrupts and
    // be preempted between bounded lock scopes, but another task runs on its
    // own kernel stack; nothing can alias this suspended task's frame.
    let frame = unsafe { &mut *frame };
    let result = dispatch(frame.rax, frame.rdi, frame.rsi, frame.rdx);
    frame.rax = result as u64;
}

/// `_a3` is unused by every syscall implemented so far -- kept in the
/// dispatch signature (matching the ABI's documented three-argument shape)
/// rather than dropped, so adding a syscall that needs it later doesn't
/// require threading a new parameter through here.
fn dispatch(num: u64, a1: u64, a2: u64, a3: u64) -> i64 {
    match num {
        SYS_EXIT => sys_exit(a1 as i32),
        SYS_WRITE => sys_write(a1, a2),
        SYS_YIELD => sys_yield(),
        SYS_GETPID => sys_getpid(),
        SYS_MMAP => sys_mmap(a1, a2),
        SYS_MUNMAP => sys_munmap(a1, a2),
        SYS_DISPLAY_INFO => sys_display_info(a1, a2),
        SYS_DISPLAY_PRESENT => sys_display_present(a1, a2),
        SYS_INPUT_POLL => sys_input_poll(a1, a2),
        SYS_UPTIME_TICKS => sys_uptime_ticks(),
        SYS_CHDIR => sys_chdir(a1, a2),
        SYS_GETCWD => sys_getcwd(a1, a2),
        SYS_OPEN => sys_open(a1, a2),
        SYS_READ => sys_read(a1, a2, a3),
        SYS_CLOSE => sys_close(a1),
        SYS_SPAWN => sys_spawn(a1, a2),
        SYS_PUT_FILE => sys_put_file(a1, a2, a3),
        SYS_REMOVE => sys_remove(a1, a2),
        SYS_MKDIR => sys_mkdir(a1, a2),
        SYS_READDIR => sys_readdir(a1, a2, a3),
        SYS_STAT => sys_stat(a1, a2, a3),
        SYS_SEEK => sys_seek(a1, a2),
        SYS_ENDPOINT_CREATE => ipc::sys_endpoint_create(a1, a2),
        SYS_ENDPOINT_CLOSE => ipc::sys_endpoint_close(a1, a2),
        SYS_SEND => ipc::sys_send(a1, a2),
        SYS_RECEIVE => ipc::sys_receive(a1, a2),
        SYS_TRY_SEND => ipc::sys_try_send(a1, a2),
        SYS_TRY_RECEIVE => ipc::sys_try_receive(a1, a2),
        SYS_CALL => ipc::sys_call(a1, a2),
        SYS_REPLY => ipc::sys_reply(a1, a2),
        SYS_CAPABILITY_CLOSE => ipc::sys_capability_close(a1, a2),
        SYS_CAPABILITY_DELEGATE => ipc::sys_capability_delegate(a1, a2),
        SYS_CAPABILITY_ACCEPT => ipc::sys_capability_accept(a1, a2),
        SYS_FS_SCOPE_CREATE => ipc::sys_fs_scope_create(a1, a2),
        SYS_FS_READ => ipc::sys_fs_read(a1, a2),
        SYS_FS_PUT => ipc::sys_fs_put(a1, a2),
        SYS_FS_LIST => ipc::sys_fs_list(a1, a2),
        SYS_CAPABILITY_QUERY => ipc::sys_capability_query(a1, a2),
        _ => {
            // Exactly the "unknown syscall numbers must fail safely"
            // requirement: logged for visibility, a plain error return,
            // the calling process is not touched otherwise and keeps
            // running normally afterward.
            crate::serial_println!("syscall: rejected unknown syscall number {}", num);
            -1
        }
    }
}

fn sys_exit(code: i32) -> ! {
    crate::serial_println!(
        "syscall: EXIT(code={}) from task {:?} -- terminating",
        code,
        task::current_task_id()
    );
    task::exit_with_code(code)
}

fn sys_write(ptr: u64, len: u64) -> i64 {
    let Ok(len_usize) = usize::try_from(len) else {
        return -1;
    };
    if len_usize > MAX_WRITE_LEN {
        crate::serial_println!(
            "syscall: WRITE rejected -- len {} exceeds max {}",
            len,
            MAX_WRITE_LEN
        );
        return -1;
    }

    match task::copy_from_current_user(ptr, len_usize) {
        Some(bytes) => {
            // This syscall's contract is "write these bytes," not "write
            // valid UTF-8" -- invalid sequences are replaced rather than
            // rejected, purely so the fallback still prints *something*
            // readable on serial rather than requiring a byte-for-byte
            // faithful (but harder to eyeball) hex dump.
            let text = core::str::from_utf8(&bytes).unwrap_or("<non-utf8 write payload>");
            crate::serial_print!("{}", text);
            bytes.len() as i64
        }
        None => {
            crate::serial_println!(
                "syscall: WRITE rejected -- invalid user pointer/range (ptr={:#x}, len={})",
                ptr,
                len
            );
            -1
        }
    }
}

fn sys_yield() -> i64 {
    task::yield_now();
    0
}

fn sys_getpid() -> i64 {
    task::current_task_id().map(i64::from).unwrap_or(-1)
}

/// `MMAP(len, writable)`. See `task::mmap_in_current_process` for the full
/// contract (arena bounds, permission handling, and transactional rollback
/// on mapping/zeroing/permission failure). `writable` is `a2 != 0`, matching this
/// ABI's usual "any nonzero value is true" convention for boolean-ish
/// arguments.
fn sys_mmap(len: u64, writable: u64) -> i64 {
    match task::mmap_in_current_process(len, writable != 0) {
        Some(addr) => addr as i64,
        None => {
            crate::serial_println!(
                "syscall: MMAP rejected (len={}, writable={})",
                len,
                writable != 0
            );
            -1
        }
    }
}

/// `MUNMAP(ptr, len)`. See `task::munmap_in_current_process` for the full
/// contract (page-alignment, arena-bounds requirements).
fn sys_munmap(ptr: u64, len: u64) -> i64 {
    if task::munmap_in_current_process(ptr, len) {
        0
    } else {
        crate::serial_println!("syscall: MUNMAP rejected (ptr={:#x}, len={})", ptr, len);
        -1
    }
}

/// `DISPLAY_INFO(out_ptr, out_len)`: writes a fixed 20-byte
/// `display::DisplayInfo` record (see that module for the exact layout)
/// into the caller's own buffer at `out_ptr`. Rejects a buffer smaller than
/// the record, a display that isn't active, or an invalid/non-writable
/// destination -- the actual write goes through
/// `task::copy_to_current_user`, which (via `paging::write_bytes_in_address_space`)
/// requires the destination to be both `WRITABLE` and `USER_ACCESSIBLE` in
/// the caller's own address space, so a kernel-address `out_ptr` is
/// rejected rather than silently written through.
fn sys_display_info(out_ptr: u64, out_len: u64) -> i64 {
    let Some(info) = crate::display::info() else {
        return -1;
    };
    let bytes = info.to_le_bytes();
    let Ok(out_len_usize) = usize::try_from(out_len) else {
        return -1;
    };
    if out_len_usize < bytes.len() {
        return -1;
    }
    if task::copy_to_current_user(out_ptr, &bytes) {
        0
    } else {
        crate::serial_println!(
            "syscall: DISPLAY_INFO rejected -- invalid destination (ptr={:#x}, len={})",
            out_ptr,
            out_len
        );
        -1
    }
}

/// `DISPLAY_PRESENT(ptr, len)`: copies the caller's own validated buffer at
/// `ptr` (exactly `len` bytes, which must exactly match the real
/// framebuffer's byte size) into the real, kernel-owned framebuffer -- see
/// `display::present` for the full validation this goes through (exact
/// size match, checked arithmetic, per-page `PRESENT | USER_ACCESSIBLE`
/// validation of the entire source range before a single byte is copied).
fn sys_display_present(ptr: u64, len: u64) -> i64 {
    let Some(caller_pid) = task::current_task_id() else {
        return -1;
    };
    if crate::keyboard::foreground_process_id() != Some(caller_pid) {
        crate::serial_println!(
            "syscall: DISPLAY_PRESENT rejected -- pid {} is not foreground owner",
            caller_pid
        );
        return -1;
    }
    let Ok(len) = usize::try_from(len) else {
        return -1;
    };
    match crate::display::present(ptr, len) {
        Ok(()) => 0,
        Err(reason) => {
            crate::serial_println!(
                "syscall: DISPLAY_PRESENT rejected -- {} (ptr={:#x}, len={})",
                reason,
                ptr,
                len
            );
            -1
        }
    }
}

/// `INPUT_POLL(out_ptr, out_len)`: writes the next queued input event (see
/// `input::InputEvent`'s fixed 8-byte encoding) into the caller's own
/// buffer, or nothing if the queue is empty. Returns `1` if an event was
/// written, `0` if the queue was empty (not an error -- callers poll in a
/// loop and are expected to see this constantly), `-1` for an invalid
/// destination buffer.
fn sys_input_poll(out_ptr: u64, out_len: u64) -> i64 {
    let Some(caller_pid) = task::current_task_id() else {
        return -1;
    };
    if crate::keyboard::foreground_process_id() != Some(caller_pid) {
        crate::serial_println!(
            "syscall: INPUT_POLL rejected -- pid {} is not foreground owner",
            caller_pid
        );
        return -1;
    }
    let bytes = crate::input::ENCODED_EVENT_LEN;
    let Ok(out_len) = usize::try_from(out_len) else {
        return -1;
    };
    if out_len < bytes {
        return -1;
    }
    // Validate before looking at queue state. Apart from making an invalid
    // pointer return `-1` even while the queue is empty, this ordering is what
    // guarantees a pending event cannot be consumed/lost by a failed copy.
    if !task::validate_current_user_range(out_ptr, bytes, true) {
        crate::serial_println!(
            "syscall: INPUT_POLL rejected -- invalid destination (ptr={:#x}, len={})",
            out_ptr,
            out_len
        );
        return -1;
    }
    match crate::input::poll() {
        Some(event) => {
            if task::copy_to_current_user(out_ptr, &event.to_le_bytes()) {
                1
            } else {
                crate::serial_println!(
                    "syscall: INPUT_POLL rejected -- invalid destination (ptr={:#x}, len={})",
                    out_ptr,
                    out_len
                );
                -1
            }
        }
        None => 0,
    }
}

fn sys_uptime_ticks() -> i64 {
    crate::interrupts::ticks() as i64
}

fn copy_user_path(ptr: u64, len: u64) -> Option<alloc::string::String> {
    let len = usize::try_from(len).ok()?;
    if len == 0 || len > crate::vfs::PATH_MAX {
        return None;
    }
    let bytes = task::copy_from_current_user(ptr, len)?;
    let path = core::str::from_utf8(&bytes).ok()?;
    Some(alloc::string::String::from(path))
}

fn sys_chdir(path_ptr: u64, path_len: u64) -> i64 {
    let Some(path) = copy_user_path(path_ptr, path_len) else {
        return -1;
    };
    let Some(cwd) = task::current_working_directory() else {
        return -1;
    };
    let Ok(target) = crate::vfs::normalize(&cwd, &path) else {
        return -1;
    };
    if crate::vfs::kind("/", &target) != Ok(crate::vfs::NodeKind::Directory) {
        return -1;
    }
    if task::set_current_working_directory(target) {
        0
    } else {
        -1
    }
}

fn sys_getcwd(out_ptr: u64, out_len: u64) -> i64 {
    let Some(cwd) = task::current_working_directory() else {
        return -1;
    };
    let Ok(out_len) = usize::try_from(out_len) else {
        return -1;
    };
    if out_len < cwd.len() || !task::copy_to_current_user(out_ptr, cwd.as_bytes()) {
        return -1;
    }
    cwd.len() as i64
}

fn sys_open(path_ptr: u64, path_len: u64) -> i64 {
    let Some(path) = copy_user_path(path_ptr, path_len) else {
        return -1;
    };
    let Some(cwd) = task::current_working_directory() else {
        return -1;
    };
    let Ok(bytes) = crate::vfs::read_file(&cwd, &path) else {
        return -1;
    };
    task::open_file_for_current_process(bytes)
        .map(i64::from)
        .unwrap_or(-1)
}

fn sys_read(handle: u64, out_ptr: u64, out_len: u64) -> i64 {
    let Ok(handle) = u32::try_from(handle) else {
        return -1;
    };
    let Ok(out_len) = usize::try_from(out_len) else {
        return -1;
    };
    if out_len > MAX_FILE_IO_LEN {
        return -1;
    }
    // Validate the complete caller-requested destination before observing or
    // advancing the file description. This also validates zero-length
    // pointers; a rejected copy consumes no file data.
    if !task::validate_current_user_range(out_ptr, out_len, true) {
        return -1;
    }
    let Some((data, start, end)) = task::peek_file_for_current_process(handle, out_len) else {
        return -1;
    };
    let bytes = &data[start..end];
    if !bytes.is_empty() && !task::copy_to_current_user(out_ptr, bytes) {
        return -1;
    }
    if !task::advance_file_for_current_process(handle, bytes.len()) {
        return -1;
    }
    bytes.len() as i64
}

fn sys_close(handle: u64) -> i64 {
    let Ok(handle) = u32::try_from(handle) else {
        return -1;
    };
    if task::close_file_for_current_process(handle) {
        0
    } else {
        -1
    }
}

fn sys_spawn(path_ptr: u64, path_len: u64) -> i64 {
    let Some(path) = copy_user_path(path_ptr, path_len) else {
        return -1;
    };
    let Some(cwd) = task::current_working_directory() else {
        return -1;
    };
    let Ok(absolute) = crate::vfs::normalize(&cwd, &path) else {
        return -1;
    };
    let Ok(bytes) = crate::vfs::read_file("/", &absolute) else {
        return -1;
    };
    if bytes.is_empty() || bytes.len() > crate::vfs::MAX_EXECUTABLE_SIZE {
        return -1;
    }

    let name = crate::vfs::basename(&absolute);
    task::spawn_user_process_with_cwd(name, &bytes, &cwd)
        .map(i64::from)
        .unwrap_or(-1)
}

/// Decode `{ pointer: u64, length: u64 }` from user memory. The descriptor
/// itself and the complete pointed-to range are both validated by each
/// caller before filesystem state is observed or changed.
fn copy_user_buffer_spec(spec_ptr: u64) -> Option<(u64, usize)> {
    let bytes = task::copy_from_current_user(spec_ptr, BUFFER_SPEC_LEN)?;
    let pointer = u64::from_le_bytes(bytes[0..8].try_into().ok()?);
    let length = usize::try_from(u64::from_le_bytes(bytes[8..16].try_into().ok()?)).ok()?;
    if length > MAX_FILE_IO_LEN {
        return None;
    }
    Some((pointer, length))
}

/// Resolve a mutation target and confine it to this executable's private
/// `/data/<process-name>` namespace. Trusted installation provisions that
/// root; Ring 3 cannot replace `/apps`, mutate a peer's data, or target the
/// filesystem root while the Phase 8 capability system is still future.
fn private_mutation_path(path: &str, allow_data_root: bool) -> Option<alloc::string::String> {
    let cwd = task::current_working_directory()?;
    let absolute = crate::vfs::normalize(&cwd, path).ok()?;
    let name = task::current_process_name()?;
    let mut root = alloc::string::String::new();
    root.try_reserve_exact(6usize.checked_add(name.len())?)
        .ok()?;
    root.push_str("/data/");
    root.push_str(&name);
    if crate::vfs::normalize("/", &root).ok().as_ref() != Some(&root) {
        return None;
    }
    let child = absolute
        .strip_prefix(&root)
        .is_some_and(|suffix| suffix.starts_with('/'));
    if child || allow_data_root && absolute == root {
        Some(absolute)
    } else {
        None
    }
}

/// Atomically create or replace one private data file. This is deliberately
/// a whole-file operation for the initial mutable ABI: pointer validation and
/// copying finish before persistence starts, and the VFS publishes either the
/// complete replacement or the previous tree.
fn sys_put_file(path_ptr: u64, path_len: u64, spec_ptr: u64) -> i64 {
    let Some(path) = copy_user_path(path_ptr, path_len) else {
        return -1;
    };
    let Some((data_ptr, data_len)) = copy_user_buffer_spec(spec_ptr) else {
        return -1;
    };
    let Some(bytes) = task::copy_from_current_user(data_ptr, data_len) else {
        return -1;
    };
    let Some(absolute) = private_mutation_path(&path, false) else {
        return -1;
    };
    crate::vfs::write_file("/", &absolute, &bytes)
        .map(|_| 0)
        .unwrap_or(-1)
}

fn sys_remove(path_ptr: u64, path_len: u64) -> i64 {
    let Some(path) = copy_user_path(path_ptr, path_len) else {
        return -1;
    };
    let Some(absolute) = private_mutation_path(&path, false) else {
        return -1;
    };
    crate::vfs::remove("/", &absolute).map(|_| 0).unwrap_or(-1)
}

fn sys_mkdir(path_ptr: u64, path_len: u64) -> i64 {
    let Some(path) = copy_user_path(path_ptr, path_len) else {
        return -1;
    };
    let Some(absolute) = private_mutation_path(&path, true) else {
        return -1;
    };
    crate::vfs::create_dir("/", &absolute)
        .map(|_| 0)
        .unwrap_or(-1)
}

fn sys_readdir(path_ptr: u64, path_len: u64, spec_ptr: u64) -> i64 {
    let Some(path) = copy_user_path(path_ptr, path_len) else {
        return -1;
    };
    let Some((out_ptr, out_len)) = copy_user_buffer_spec(spec_ptr) else {
        return -1;
    };
    if !task::validate_current_user_range(out_ptr, out_len, true) {
        return -1;
    }
    let Some(cwd) = task::current_working_directory() else {
        return -1;
    };
    let Ok(entries) = crate::vfs::list_dir(&cwd, &path) else {
        return -1;
    };
    let mut encoded = alloc::vec::Vec::new();
    if encoded.try_reserve_exact(out_len).is_err() {
        return -1;
    }
    for entry in entries {
        let Some(required) = encoded
            .len()
            .checked_add(entry.len())
            .and_then(|length| length.checked_add(1))
        else {
            return -1;
        };
        if required > out_len {
            return -1;
        }
        encoded.extend_from_slice(entry.as_bytes());
        encoded.push(b'\n');
    }
    if !encoded.is_empty() && !task::copy_to_current_user(out_ptr, &encoded) {
        return -1;
    }
    encoded.len() as i64
}

fn sys_stat(path_ptr: u64, path_len: u64, out_ptr: u64) -> i64 {
    let Some(path) = copy_user_path(path_ptr, path_len) else {
        return -1;
    };
    if !task::validate_current_user_range(out_ptr, STAT_RECORD_LEN, true) {
        return -1;
    }
    let Some(cwd) = task::current_working_directory() else {
        return -1;
    };
    let Ok(metadata) = crate::vfs::metadata(&cwd, &path) else {
        return -1;
    };
    let Ok(size) = u64::try_from(metadata.size) else {
        return -1;
    };
    let mut record = [0u8; STAT_RECORD_LEN];
    let kind = match metadata.kind {
        crate::vfs::NodeKind::File => 1u64,
        crate::vfs::NodeKind::Directory => 2u64,
    };
    record[0..8].copy_from_slice(&kind.to_le_bytes());
    record[8..16].copy_from_slice(&size.to_le_bytes());
    if task::copy_to_current_user(out_ptr, &record) {
        0
    } else {
        -1
    }
}

fn sys_seek(handle: u64, absolute_offset: u64) -> i64 {
    let Ok(handle) = u32::try_from(handle) else {
        return -1;
    };
    let Ok(offset) = usize::try_from(absolute_offset) else {
        return -1;
    };
    if task::seek_file_for_current_process(handle, offset) {
        absolute_offset as i64
    } else {
        -1
    }
}
