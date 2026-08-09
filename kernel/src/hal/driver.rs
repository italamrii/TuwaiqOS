//! Explicit resource ownership for kernel hardware drivers.

use super::pci::PciAddress;
use spin::Mutex;

const MAX_DRIVER_CLAIMS: usize = 16;
static CLAIMS: Mutex<[Option<DriverOwnership>; MAX_DRIVER_CLAIMS]> =
    Mutex::new([None; MAX_DRIVER_CLAIMS]);

/// How a driver receives completion notification.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IrqOwnership {
    /// The device interrupt source is disabled and the driver polls with a
    /// finite budget.  No PIC/APIC vector is owned.
    Polling,
}

/// Memory made visible to a DMA-capable device.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DmaOwnership {
    /// Page-aligned kernel storage reserved for the driver's entire lifetime.
    /// It is reset and scrubbed on teardown rather than returned to the heap.
    StaticReserved { bytes: usize },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RegisterOwnership {
    IoPort { base: u16, bytes: u16 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TeardownOwnership {
    /// Reset the device before releasing its logical owner.  DMA memory is
    /// scrubbed before the device may be rebound.
    ResetAndScrub,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DriverOwnership {
    pub name: &'static str,
    pub device: PciAddress,
    pub registers: RegisterOwnership,
    pub irq: IrqOwnership,
    pub dma: DmaOwnership,
    pub teardown: TeardownOwnership,
}

/// A short-lock registry claim. The registry lock is held only while adding
/// or removing this immutable descriptor; the claim's lifetime does not hold
/// the lock and therefore never encloses device I/O or DMA.
pub struct DriverClaim {
    ownership: DriverOwnership,
    active: bool,
}

impl DriverClaim {
    /// Release only after the driver has disabled interrupts, reset its
    /// device, and scrubbed or revoked DMA memory.
    pub fn release(&mut self) {
        if !self.active {
            return;
        }
        let mut claims = CLAIMS.lock();
        if let Some(slot) = claims
            .iter_mut()
            .find(|slot| slot.map(|value| value.device) == Some(self.ownership.device))
        {
            *slot = None;
        }
        self.active = false;
    }
}

impl Drop for DriverClaim {
    fn drop(&mut self) {
        // Defensive fallback for initialization errors. Fully initialized
        // drivers explicitly release after reset/scrub so teardown ordering
        // is visible at their call site.
        self.release();
    }
}

pub fn claim(ownership: DriverOwnership) -> Result<DriverClaim, &'static str> {
    let mut claims = CLAIMS.lock();
    if claims
        .iter()
        .flatten()
        .any(|claimed| claimed.device == ownership.device)
    {
        return Err("PCI function is already owned by another driver");
    }
    let slot = claims
        .iter_mut()
        .find(|slot| slot.is_none())
        .ok_or("driver ownership registry is full")?;
    *slot = Some(ownership);
    Ok(DriverClaim {
        ownership,
        active: true,
    })
}

pub fn active_claims() -> usize {
    CLAIMS.lock().iter().flatten().count()
}
