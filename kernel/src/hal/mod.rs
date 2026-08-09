//! Hardware-abstraction boundary and driver ownership registry.
//!
//! Phase 7 keeps mechanism in the kernel and policy above it.  Bus discovery
//! produces immutable device descriptions; individual drivers then claim
//! only the resources they actually own.  The registry is descriptive and
//! is never held across port I/O, DMA, allocation, or interrupt-disabled work.

pub mod dma;
pub mod driver;
pub mod pci;

/// Discover buses before any device driver attempts to bind.
pub fn init() {
    let inventory = pci::discover();
    crate::serial_println!(
        "hal: PCI discovery complete ({} functions, truncated={})",
        inventory.len(),
        inventory.was_truncated()
    );
}
