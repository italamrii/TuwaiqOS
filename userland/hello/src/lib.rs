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
pub const SYS_UDP_OPEN: u64 = 22;
pub const SYS_UDP_SEND: u64 = 23;
pub const SYS_UDP_RECV: u64 = 24;
pub const SYS_UDP_CLOSE: u64 = 25;

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
