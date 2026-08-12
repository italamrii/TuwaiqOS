//! Whether Ring 3 can talk to hardware directly.
//!
//! Port I/O at CPL=3 is allowed only when RFLAGS.IOPL is at least the current
//! privilege level, or when the port is permitted by the TSS I/O permission
//! bitmap. TuwaiqOS enters Ring 3 with RFLAGS = 0x202, which leaves IOPL at 0,
//! and installs no bitmap.
//!
//! Port 0x1F7 is the ATA status register. A user program that could reach it
//! would be able to drive the disk controller directly and read or write any
//! sector, with the filesystem and every check above it bypassed entirely.
//!
//! Like `probe_msr` this program is written to fail. The trailing message
//! printing at all is the failure.

#![no_std]
#![no_main]

use core::arch::{asm, global_asm};

use hello_user::{syscall, write, SYS_EXIT};

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

const ATA_STATUS_PORT: u16 = 0x1F7;

extern "C" fn rust_main() -> ! {
    write(b"probe_portio: about to read the ATA status port from CPL=3\n");

    let status: u8;
    // Safety: expected to fault. A correct kernel terminates this process
    // here, so the read result is never used.
    unsafe {
        asm!(
            "in al, dx",
            in("dx") ATA_STATUS_PORT,
            out("al") status,
            options(nomem, nostack),
        );
    }

    let _ = status;
    write(b"probe_portio: UNEXPECTED -- CPL=3 reached the disk controller\n");
    // Safety: SYS_EXIT never returns.
    unsafe {
        syscall(SYS_EXIT, 1, 0, 0);
    }
    loop {}
}
