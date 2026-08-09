//! Hostile Ring 3 coverage for the Phase 7 bounded UDP ABI.

#![no_std]
#![no_main]

use core::arch::global_asm;

use hello_user::{
    syscall, write, SYS_EXIT, SYS_MMAP, SYS_MUNMAP, SYS_UDP_CLOSE, SYS_UDP_OPEN, SYS_UDP_RECV,
    SYS_UDP_SEND,
};

global_asm!(
    r#"
.global _start
_start:
    call {main}
1:
    jmp 1b
"#,
    main = sym rust_main,
);

const NON_CANONICAL: u64 = 0x0001_0000_0000_0000;
const RECEIVE_RECORD_BYTES: u64 = 1208;

#[repr(C)]
#[derive(Clone, Copy)]
struct SendSpec {
    data_ptr: u64,
    data_len: u32,
    destination: [u8; 4],
    destination_port: u16,
    reserved: [u8; 6],
}

extern "C" fn rust_main() -> ! {
    let mut ok = true;
    let payload = b"bounded-udp";
    let valid_spec = SendSpec {
        data_ptr: payload.as_ptr() as u64,
        data_len: payload.len() as u32,
        destination: [10, 0, 2, 2],
        destination_port: 9,
        reserved: [0; 6],
    };

    if unsafe { syscall(SYS_UDP_OPEN, 80, 0, 0) } < 0 {
        write(b"bad_net: reserved port rejected -- OK\n");
    } else {
        write(b"bad_net: reserved port was accepted\n");
        ok = false;
    }

    let handle = unsafe { syscall(SYS_UDP_OPEN, 41000, 0, 0) };
    if handle < 0 {
        write(b"bad_net: UDP_OPEN failed unexpectedly\n");
        ok = false;
    }
    let cleanup_handle = unsafe { syscall(SYS_UDP_OPEN, 42000, 0, 0) };
    if cleanup_handle >= 0 {
        write(b"bad_net: exit-owned socket acquired for cleanup proof -- OK\n");
    } else {
        write(b"bad_net: prior process socket was not reclaimed\n");
        ok = false;
    }
    if unsafe { syscall(SYS_UDP_OPEN, 41000, 0, 0) } < 0 {
        write(b"bad_net: duplicate bind rejected -- OK\n");
    } else {
        write(b"bad_net: duplicate bind was accepted\n");
        ok = false;
    }

    if unsafe { syscall(SYS_UDP_SEND, handle as u64, NON_CANONICAL, 0) } < 0 {
        write(b"bad_net: non-canonical send descriptor rejected -- OK\n");
    } else {
        ok = false;
    }
    let invalid_data = SendSpec {
        data_ptr: NON_CANONICAL,
        ..valid_spec
    };
    if unsafe {
        syscall(
            SYS_UDP_SEND,
            handle as u64,
            &invalid_data as *const SendSpec as u64,
            0,
        )
    } < 0
    {
        write(b"bad_net: non-canonical payload rejected -- OK\n");
    } else {
        ok = false;
    }

    let mapped = unsafe { syscall(SYS_MMAP, 4096, 1, 0) };
    if mapped < 0 {
        ok = false;
    } else {
        if unsafe { syscall(SYS_UDP_SEND, handle as u64, mapped as u64 + 4088, 0) } < 0 {
            write(b"bad_net: cross-page send descriptor rejected -- OK\n");
        } else {
            ok = false;
        }
        if unsafe {
            syscall(
                SYS_UDP_RECV,
                handle as u64,
                mapped as u64 + 4092,
                RECEIVE_RECORD_BYTES,
            )
        } < 0
        {
            write(b"bad_net: cross-page receive destination rejected -- OK\n");
        } else {
            ok = false;
        }
        let _ = unsafe { syscall(SYS_MUNMAP, mapped as u64, 4096, 0) };
    }

    if unsafe {
        syscall(
            SYS_UDP_RECV,
            handle as u64,
            NON_CANONICAL,
            RECEIVE_RECORD_BYTES,
        )
    } < 0
    {
        write(b"bad_net: empty-queue invalid destination rejected -- OK\n");
    } else {
        ok = false;
    }
    let mut receive = [0u8; RECEIVE_RECORD_BYTES as usize];
    if unsafe {
        syscall(
            SYS_UDP_RECV,
            handle as u64,
            receive.as_mut_ptr() as u64,
            RECEIVE_RECORD_BYTES - 1,
        )
    } < 0
    {
        write(b"bad_net: undersized receive record rejected -- OK\n");
    } else {
        ok = false;
    }
    if unsafe {
        syscall(
            SYS_UDP_RECV,
            handle as u64,
            receive.as_mut_ptr() as u64,
            RECEIVE_RECORD_BYTES,
        )
    } == 0
    {
        write(b"bad_net: valid empty receive is deterministic -- OK\n");
    } else {
        ok = false;
    }

    if unsafe { syscall(SYS_UDP_CLOSE, handle as u64, 0, 0) } == 0
        && unsafe { syscall(SYS_UDP_CLOSE, handle as u64, 0, 0) } < 0
        && unsafe {
            syscall(
                SYS_UDP_SEND,
                handle as u64,
                &valid_spec as *const SendSpec as u64,
                0,
            )
        } < 0
    {
        write(b"bad_net: stale handle and double close rejected -- OK\n");
    } else {
        ok = false;
    }

    // Deliberately leave `cleanup_handle` open. Kernel process-exit cleanup
    // must release it so the next independent run can bind port 42000.
    write(if ok { b"bad_net: PASS\n" } else { b"bad_net: FAIL\n" });
    unsafe { syscall(SYS_EXIT, if ok { 0 } else { 1 }, 0, 0) };
    loop {}
}
