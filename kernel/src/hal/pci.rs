//! PCI configuration-mechanism-one discovery for x86_64.
//!
//! Enumeration is bounded, allocation-free, and performed once during boot.
//! The resulting inventory is immutable.  Config writes remain explicit and
//! are used only by the driver which owns the selected function.

use core::fmt;

use spin::Once;
use x86_64::instructions::port::Port;

const CONFIG_ADDRESS: u16 = 0xCF8;
const CONFIG_DATA: u16 = 0xCFC;
const MAX_FUNCTIONS: usize = 128;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PciAddress {
    pub bus: u8,
    pub device: u8,
    pub function: u8,
}

impl fmt::Display for PciAddress {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            out,
            "{:02x}:{:02x}.{}",
            self.bus, self.device, self.function
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PciDevice {
    pub address: PciAddress,
    pub vendor_id: u16,
    pub device_id: u16,
    pub class: u8,
    pub subclass: u8,
    pub programming_interface: u8,
    pub revision: u8,
    pub header_type: u8,
    pub interrupt_line: u8,
    pub bars: [u32; 6],
}

impl PciDevice {
    pub fn io_bar(&self, index: usize) -> Option<u16> {
        let raw = *self.bars.get(index)?;
        if raw & 1 == 0 {
            return None;
        }
        let base = raw & !0x3;
        if base == 0 || base > u16::MAX as u32 {
            return None;
        }
        Some(base as u16)
    }

    pub fn enable_io_bus_mastering(&self) {
        let command = read_u16(self.address, 0x04);
        // I/O space + bus mastering.  Memory-space decoding is deliberately
        // untouched because the initial VirtIO transport owns an I/O BAR.
        write_u16(self.address, 0x04, command | (1 << 0) | (1 << 2));
    }

    /// Return a checked memory BAR base and size. The device's memory and I/O
    /// decoders are disabled only for the bounded BAR-size probe, then its
    /// exact command register and BAR contents are restored.
    pub fn probe_memory_bar(&self, index: usize) -> Result<MemoryBar, &'static str> {
        if index >= self.bars.len() {
            return Err("PCI BAR index is outside the header");
        }
        let low = self.bars[index];
        if low & 1 != 0 {
            return Err("PCI BAR is an I/O BAR");
        }
        let bar_type = (low >> 1) & 0x3;
        if bar_type != 0 && bar_type != 2 {
            return Err("unsupported PCI memory BAR type");
        }
        let is_64 = bar_type == 2;
        if is_64 && index + 1 >= self.bars.len() {
            return Err("truncated 64-bit PCI BAR");
        }
        let high = if is_64 { self.bars[index + 1] } else { 0 };
        let base = (u64::from(high) << 32) | u64::from(low & !0xF);
        if base == 0 {
            return Err("PCI memory BAR has no assigned address");
        }

        let command = read_u16(self.address, 0x04);
        write_u16(self.address, 0x04, command & !0x3);
        let offset = 0x10 + (index as u8 * 4);
        write_u32(self.address, offset, u32::MAX);
        if is_64 {
            write_u32(self.address, offset + 4, u32::MAX);
        }
        let mask_low = read_u32(self.address, offset);
        let mask_high = if is_64 {
            read_u32(self.address, offset + 4)
        } else {
            0
        };
        write_u32(self.address, offset, low);
        if is_64 {
            write_u32(self.address, offset + 4, high);
        }
        write_u16(self.address, 0x04, command);

        let mask = (u64::from(mask_high) << 32) | u64::from(mask_low & !0xF);
        let width_mask = if is_64 { u64::MAX } else { u32::MAX as u64 };
        let size = (!mask & width_mask).wrapping_add(1);
        if size == 0 || !size.is_power_of_two() || base & (size - 1) != 0 {
            return Err("PCI memory BAR has invalid size or alignment");
        }
        base.checked_add(size - 1)
            .ok_or("PCI memory BAR address overflows")?;
        Ok(MemoryBar { base, size })
    }

    pub fn enable_memory_bus_mastering(&self) {
        let command = read_u16(self.address, 0x04);
        write_u16(self.address, 0x04, command | (1 << 1) | (1 << 2));
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MemoryBar {
    pub base: u64,
    pub size: u64,
}

pub struct PciInventory {
    devices: [Option<PciDevice>; MAX_FUNCTIONS],
    len: usize,
    truncated: bool,
}

impl PciInventory {
    const fn empty() -> Self {
        Self {
            devices: [None; MAX_FUNCTIONS],
            len: 0,
            truncated: false,
        }
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn was_truncated(&self) -> bool {
        self.truncated
    }

    pub fn iter(&self) -> impl Iterator<Item = &PciDevice> {
        self.devices[..self.len].iter().filter_map(Option::as_ref)
    }

    pub fn find(&self, vendor_id: u16, device_id: u16) -> Option<&PciDevice> {
        self.iter()
            .find(|device| device.vendor_id == vendor_id && device.device_id == device_id)
    }

    fn push(&mut self, device: PciDevice) {
        if self.len == self.devices.len() {
            self.truncated = true;
            return;
        }
        self.devices[self.len] = Some(device);
        self.len += 1;
    }
}

static INVENTORY: Once<PciInventory> = Once::new();

pub fn discover() -> &'static PciInventory {
    INVENTORY.call_once(scan)
}

fn scan() -> PciInventory {
    let mut inventory = PciInventory::empty();
    for bus in 0u16..=255 {
        for device in 0u8..32 {
            let first = PciAddress {
                bus: bus as u8,
                device,
                function: 0,
            };
            if read_u16(first, 0x00) == 0xFFFF {
                continue;
            }
            let multifunction = read_u8(first, 0x0E) & 0x80 != 0;
            let functions = if multifunction { 8 } else { 1 };
            for function in 0..functions {
                let address = PciAddress {
                    bus: bus as u8,
                    device,
                    function,
                };
                if read_u16(address, 0x00) == 0xFFFF {
                    continue;
                }
                let found = describe(address);
                crate::serial_println!(
                    "pci: {} {:04x}:{:04x} class {:02x}:{:02x} irq {}",
                    found.address,
                    found.vendor_id,
                    found.device_id,
                    found.class,
                    found.subclass,
                    found.interrupt_line
                );
                inventory.push(found);
            }
        }
    }
    if inventory.truncated {
        crate::serial_println!("pci: inventory truncated at {} functions", MAX_FUNCTIONS);
    }
    inventory
}

fn describe(address: PciAddress) -> PciDevice {
    let identity = read_u32(address, 0x00);
    let class = read_u32(address, 0x08);
    let header_type = read_u8(address, 0x0E);
    let bar_count = if header_type & 0x7F == 0 { 6 } else { 2 };
    let mut bars = [0u32; 6];
    for (index, bar) in bars.iter_mut().enumerate().take(bar_count) {
        *bar = read_u32(address, 0x10 + (index as u8 * 4));
    }
    PciDevice {
        address,
        vendor_id: identity as u16,
        device_id: (identity >> 16) as u16,
        revision: class as u8,
        programming_interface: (class >> 8) as u8,
        subclass: (class >> 16) as u8,
        class: (class >> 24) as u8,
        header_type,
        interrupt_line: read_u8(address, 0x3C),
        bars,
    }
}

fn config_address(address: PciAddress, offset: u8) -> u32 {
    0x8000_0000
        | ((address.bus as u32) << 16)
        | ((address.device as u32) << 11)
        | ((address.function as u32) << 8)
        | (u32::from(offset) & 0xFC)
}

pub fn read_u32(address: PciAddress, offset: u8) -> u32 {
    let mut address_port = Port::<u32>::new(CONFIG_ADDRESS);
    let mut data_port = Port::<u32>::new(CONFIG_DATA);
    // Safety: ports CF8/CFC are the architecture-defined PCI configuration
    // mechanism.  Calls are serialized by boot-time discovery or by the one
    // driver which already owns `address`.
    unsafe {
        address_port.write(config_address(address, offset));
        data_port.read()
    }
}

pub fn read_u16(address: PciAddress, offset: u8) -> u16 {
    let shift = u32::from(offset & 2) * 8;
    (read_u32(address, offset) >> shift) as u16
}

pub fn read_u8(address: PciAddress, offset: u8) -> u8 {
    let shift = u32::from(offset & 3) * 8;
    (read_u32(address, offset) >> shift) as u8
}

pub fn write_u16(address: PciAddress, offset: u8, value: u16) {
    let aligned = offset & 0xFC;
    let shift = u32::from(offset & 2) * 8;
    let mask = !(0xFFFFu32 << shift);
    let updated = (read_u32(address, aligned) & mask) | (u32::from(value) << shift);
    let mut address_port = Port::<u32>::new(CONFIG_ADDRESS);
    let mut data_port = Port::<u32>::new(CONFIG_DATA);
    // Safety: see `read_u32`; this changes only the selected function's
    // requested 16-bit configuration field.
    unsafe {
        address_port.write(config_address(address, aligned));
        data_port.write(updated);
    }
}

pub fn write_u32(address: PciAddress, offset: u8, value: u32) {
    let mut address_port = Port::<u32>::new(CONFIG_ADDRESS);
    let mut data_port = Port::<u32>::new(CONFIG_DATA);
    unsafe {
        address_port.write(config_address(address, offset));
        data_port.write(value);
    }
}
