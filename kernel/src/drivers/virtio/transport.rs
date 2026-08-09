//! VirtIO 0.9-compatible PCI I/O transport.

use x86_64::instructions::port::Port;

use crate::hal::driver::{
    DmaOwnership, DriverOwnership, IrqOwnership, RegisterOwnership, TeardownOwnership,
};
use crate::hal::pci::PciDevice;

const HOST_FEATURES: u16 = 0;
const GUEST_FEATURES: u16 = 4;
const QUEUE_PFN: u16 = 8;
const QUEUE_SIZE: u16 = 12;
const QUEUE_SELECT: u16 = 14;
const QUEUE_NOTIFY: u16 = 16;
const DEVICE_STATUS: u16 = 18;
pub const DEVICE_CONFIG: u16 = 20;

pub const STATUS_ACKNOWLEDGE: u8 = 1;
pub const STATUS_DRIVER: u8 = 2;
pub const STATUS_DRIVER_OK: u8 = 4;
pub const STATUS_FAILED: u8 = 128;

pub struct LegacyTransport {
    pci: PciDevice,
    io_base: u16,
}

impl LegacyTransport {
    pub fn bind(device: &PciDevice) -> Result<Self, &'static str> {
        let io_base = device.io_bar(0).ok_or("VirtIO legacy I/O BAR is absent")?;
        device.enable_io_bus_mastering();
        let transport = Self {
            pci: *device,
            io_base,
        };
        transport.reset();
        transport.write_status(STATUS_ACKNOWLEDGE | STATUS_DRIVER);
        Ok(transport)
    }

    pub fn ownership(&self, name: &'static str, dma_bytes: usize) -> DriverOwnership {
        DriverOwnership {
            name,
            device: self.pci.address,
            registers: RegisterOwnership::IoPort {
                base: self.io_base,
                bytes: 32,
            },
            irq: IrqOwnership::Polling,
            dma: DmaOwnership::StaticReserved { bytes: dma_bytes },
            teardown: TeardownOwnership::ResetAndScrub,
        }
    }

    pub fn host_features(&self) -> u32 {
        self.read_u32(HOST_FEATURES)
    }

    pub fn set_guest_features(&self, features: u32) {
        self.write_u32(GUEST_FEATURES, features);
    }

    pub fn select_queue(&self, index: u16) {
        self.write_u16(QUEUE_SELECT, index);
    }

    pub fn queue_size(&self) -> u16 {
        self.read_u16(QUEUE_SIZE)
    }

    pub fn queue_pfn(&self) -> u32 {
        self.read_u32(QUEUE_PFN)
    }

    pub fn set_queue_pfn(&self, physical: u64) -> Result<(), &'static str> {
        if physical & 0xFFF != 0 || physical >> 44 != 0 {
            return Err("legacy VirtIO queue address is not a representable PFN");
        }
        self.write_u32(QUEUE_PFN, (physical >> 12) as u32);
        Ok(())
    }

    pub fn notify_queue(&self, index: u16) {
        self.write_u16(QUEUE_NOTIFY, index);
    }

    pub fn finish_init(&self) {
        self.write_status(self.read_status() | STATUS_DRIVER_OK);
    }

    pub fn fail(&self) {
        self.write_status(self.read_status() | STATUS_FAILED);
    }

    pub fn reset(&self) {
        self.write_status(0);
        // A read serializes the reset against following queue writes.
        let _ = self.read_status();
    }

    pub fn read_config_u8(&self, offset: u16) -> u8 {
        self.read_u8(DEVICE_CONFIG + offset)
    }

    pub fn read_config_u16(&self, offset: u16) -> u16 {
        self.read_u16(DEVICE_CONFIG + offset)
    }

    pub fn read_config_u32(&self, offset: u16) -> u32 {
        self.read_u32(DEVICE_CONFIG + offset)
    }

    pub fn read_status(&self) -> u8 {
        self.read_u8(DEVICE_STATUS)
    }

    fn write_status(&self, value: u8) {
        self.write_u8(DEVICE_STATUS, value);
    }

    fn port(&self, offset: u16) -> u16 {
        self.io_base.wrapping_add(offset)
    }

    fn read_u8(&self, offset: u16) -> u8 {
        let mut port = Port::<u8>::new(self.port(offset));
        // Safety: this transport exclusively owns its bounded PCI I/O BAR.
        unsafe { port.read() }
    }

    fn read_u16(&self, offset: u16) -> u16 {
        let mut port = Port::<u16>::new(self.port(offset));
        // Safety: this transport exclusively owns its bounded PCI I/O BAR.
        unsafe { port.read() }
    }

    fn read_u32(&self, offset: u16) -> u32 {
        let mut port = Port::<u32>::new(self.port(offset));
        // Safety: this transport exclusively owns its bounded PCI I/O BAR.
        unsafe { port.read() }
    }

    fn write_u8(&self, offset: u16, value: u8) {
        let mut port = Port::<u8>::new(self.port(offset));
        // Safety: this transport exclusively owns its bounded PCI I/O BAR.
        unsafe { port.write(value) }
    }

    fn write_u16(&self, offset: u16, value: u16) {
        let mut port = Port::<u16>::new(self.port(offset));
        // Safety: this transport exclusively owns its bounded PCI I/O BAR.
        unsafe { port.write(value) }
    }

    fn write_u32(&self, offset: u16, value: u32) {
        let mut port = Port::<u32>::new(self.port(offset));
        // Safety: this transport exclusively owns its bounded PCI I/O BAR.
        unsafe { port.write(value) }
    }
}

impl Drop for LegacyTransport {
    fn drop(&mut self) {
        self.reset();
    }
}
