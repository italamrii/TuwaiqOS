//! System reboot helper.
//!
//! Flushes TuwaiqFS to disk, then triggers a CPU reset through the keyboard
//! controller so QEMU reloads the boot disk image.

use crate::vfs;

/// Sync the filesystem and reboot the machine.
pub fn system() -> ! {
    let _ = vfs::sync();
    if let Err(reason) = crate::storage::prepare_reboot() {
        crate::serial_println!("reboot: storage shutdown warning: {}", reason);
    }

    unsafe {
        // Pulse the 8042 keyboard controller reset line (works in QEMU/Bochs).
        core::arch::asm!(
            "out dx, al",
            in("dx") 0x64u16,
            in("al") 0xFEu8,
            options(nomem, nostack)
        );
    }

    loop {
        core::hint::spin_loop();
    }
}
