//! Legacy PCI VirtIO transport used for deterministic QEMU qualification.

pub mod block;
pub mod net;
pub mod queue;
pub mod transport;

pub const PCI_VENDOR: u16 = 0x1AF4;
pub const LEGACY_NET_DEVICE: u16 = 0x1000;
pub const LEGACY_BLOCK_DEVICE: u16 = 0x1001;
