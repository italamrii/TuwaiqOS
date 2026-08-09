//! Allocation-free early-boot stage tracking over COM1.

use core::sync::atomic::{AtomicU8, Ordering};
use x86_64::instructions::port::Port;

static LAST_STAGE: AtomicU8 = AtomicU8::new(0);

pub fn early_init() {
    unsafe {
        let mut interrupt_enable = Port::<u8>::new(0x3F9);
        let mut line_control = Port::<u8>::new(0x3FB);
        let mut divisor_low = Port::<u8>::new(0x3F8);
        let mut divisor_high = Port::<u8>::new(0x3F9);
        let mut fifo_control = Port::<u8>::new(0x3FA);
        let mut modem_control = Port::<u8>::new(0x3FC);
        interrupt_enable.write(0x00);
        line_control.write(0x80);
        divisor_low.write(0x03);
        divisor_high.write(0x00);
        line_control.write(0x03);
        fifo_control.write(0xC7);
        modem_control.write(0x0B);
    }
    raw_line("BOOT: stage=entry COM1-ready");
}

pub fn mark(stage: u8, label: &'static str) {
    LAST_STAGE.store(stage, Ordering::Release);
    raw_write(b"BOOT: stage=");
    raw_write(label.as_bytes());
    raw_write(b"\r\n");
}

pub fn last_stage() -> &'static str {
    match LAST_STAGE.load(Ordering::Acquire) {
        1 => "kernel-entry",
        2 => "boot-info-checked",
        3 => "nx-enabled",
        4 => "heap-online",
        5 => "interrupts-online",
        6 => "input-probed",
        7 => "pci-discovered",
        8 => "storage-selected",
        9 => "vfs-mounted",
        10 => "scheduler-online",
        11 => "network-initialized",
        12 => "console-selected",
        13 => "shell-entered",
        _ => "pre-stage",
    }
}

/// Record a non-fatal optional-device failure with an explicit fallback.
///
/// Format is fixed so COM1 logs remain greppable without allocation:
/// `BOOT-DEGRADE: component=... reason=... fallback=... alive=yes`.
pub fn degrade(component: &str, reason: &str, fallback: &str) {
    raw_write(b"BOOT-DEGRADE: component=");
    raw_write(component.as_bytes());
    raw_write(b" reason=");
    raw_write(reason.as_bytes());
    raw_write(b" fallback=");
    raw_write(fallback.as_bytes());
    raw_write(b" alive=yes stage=");
    raw_write(last_stage().as_bytes());
    raw_write(b"\r\n");
}

pub fn raw_line(text: &str) {
    raw_write(text.as_bytes());
    raw_write(b"\r\n");
}

fn raw_write(bytes: &[u8]) {
    for byte in bytes {
        for _ in 0..100_000 {
            let ready = unsafe { Port::<u8>::new(0x3FD).read() } & 0x20 != 0;
            if ready {
                unsafe { Port::<u8>::new(0x3F8).write(*byte) };
                break;
            }
            core::hint::spin_loop();
        }
    }
}
