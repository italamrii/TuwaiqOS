//! ATA PIO disk driver (primary master).
//!
//! Reads and writes 512-byte sectors on the boot disk image attached as
//! QEMU's first IDE drive. TuwaiqFS stores its metadata and file data in
//! a reserved region starting at LBA 8192.

const ATA_DATA: u16 = 0x1F0;
const ATA_SECCOUNT: u16 = 0x1F2;
const ATA_LBA_LO: u16 = 0x1F3;
const ATA_LBA_MID: u16 = 0x1F4;
const ATA_LBA_HI: u16 = 0x1F5;
const ATA_DRIVE: u16 = 0x1F6;
const ATA_STATUS: u16 = 0x1F7;
const ATA_CMD: u16 = 0x1F7;

const CMD_READ: u8 = 0x20;
const CMD_WRITE: u8 = 0x30;
const CMD_CACHE_FLUSH: u8 = 0xE7;

const STATUS_BSY: u8 = 0x80;
const STATUS_DRQ: u8 = 0x08;
const STATUS_ERR: u8 = 0x01;

/// Wait for the primary master drive to respond (call once at boot).
pub fn init() -> bool {
    for _ in 0..100_000 {
        unsafe {
            let status = inb(ATA_STATUS);
            // 0xFF means no device on the bus — keep polling briefly in QEMU.
            if status != 0 && status != 0xFF && status & STATUS_BSY == 0 {
                return true;
            }
        }
    }
    false
}

/// Read one 512-byte sector from the boot disk.
pub fn read_sector(lba: u32, buffer: &mut [u8; 512]) -> Result<(), &'static str> {
    select_drive(lba)?;
    wait_not_busy()?;

    unsafe {
        outb(ATA_SECCOUNT, 1);
        outb(ATA_LBA_LO, lba as u8);
        outb(ATA_LBA_MID, (lba >> 8) as u8);
        outb(ATA_LBA_HI, (lba >> 16) as u8);
        outb(ATA_CMD, CMD_READ);
    }

    wait_drq()?;
    read_words(buffer);
    wait_not_busy()?;
    Ok(())
}

/// Write one 512-byte sector to the boot disk.
pub fn write_sector(lba: u32, buffer: &[u8; 512]) -> Result<(), &'static str> {
    select_drive(lba)?;
    wait_not_busy()?;

    unsafe {
        outb(ATA_SECCOUNT, 1);
        outb(ATA_LBA_LO, lba as u8);
        outb(ATA_LBA_MID, (lba >> 8) as u8);
        outb(ATA_LBA_HI, (lba >> 16) as u8);
        outb(ATA_CMD, CMD_WRITE);
    }

    wait_drq()?;
    write_words(buffer);
    wait_not_busy()?;
    Ok(())
}

pub fn flush() -> Result<(), &'static str> {
    wait_not_busy()?;
    unsafe { outb(ATA_CMD, CMD_CACHE_FLUSH) };
    wait_not_busy()
}

fn select_drive(lba: u32) -> Result<(), &'static str> {
    if lba >= 1 << 28 {
        return Err("ATA LBA exceeds the 28-bit command range");
    }
    unsafe {
        outb(ATA_DRIVE, 0xE0 | ((lba >> 24) as u8 & 0x0F));
    }
    Ok(())
}

fn wait_not_busy() -> Result<(), &'static str> {
    for _ in 0..500_000 {
        unsafe {
            let status = inb(ATA_STATUS);
            if status & STATUS_ERR != 0 {
                return Err("ata error");
            }
            if status & STATUS_BSY == 0 {
                return Ok(());
            }
        }
    }
    Err("ata busy timeout")
}

fn wait_drq() -> Result<(), &'static str> {
    for _ in 0..500_000 {
        unsafe {
            let status = inb(ATA_STATUS);
            if status & STATUS_ERR != 0 {
                return Err("ata error");
            }
            if status & STATUS_DRQ != 0 {
                return Ok(());
            }
        }
    }
    Err("ata drq timeout")
}

fn read_words(buffer: &mut [u8; 512]) {
    unsafe {
        for chunk in buffer.chunks_mut(2) {
            let word = inw(ATA_DATA);
            chunk[0] = word as u8;
            if chunk.len() > 1 {
                chunk[1] = (word >> 8) as u8;
            }
        }
    }
}

fn write_words(buffer: &[u8; 512]) {
    unsafe {
        for chunk in buffer.chunks(2) {
            let low = chunk[0] as u16;
            let high = if chunk.len() > 1 { chunk[1] as u16 } else { 0 };
            outw(ATA_DATA, low | (high << 8));
        }
    }
}

unsafe fn inb(port: u16) -> u8 {
    let value: u8;
    core::arch::asm!("in al, dx", out("al") value, in("dx") port, options(nomem, nostack, preserves_flags));
    value
}

unsafe fn outb(port: u16, value: u8) {
    core::arch::asm!("out dx, al", in("dx") port, in("al") value, options(nomem, nostack, preserves_flags));
}

unsafe fn inw(port: u16) -> u16 {
    let value: u16;
    core::arch::asm!("in ax, dx", out("ax") value, in("dx") port, options(nomem, nostack, preserves_flags));
    value
}

unsafe fn outw(port: u16, value: u16) {
    core::arch::asm!("out dx, ax", in("dx") port, in("ax") value, options(nomem, nostack, preserves_flags));
}
