//! VirtIO legacy network device with fixed, bounded DMA storage.

use core::ptr::{addr_of_mut, read_volatile};

use smoltcp::phy::{Device, DeviceCapabilities, Medium, RxToken, TxToken};
use smoltcp::time::Instant;

use crate::hal::dma::{self, PageAligned};
use crate::hal::pci::PciDevice;

use super::queue::{Descriptor, SplitQueue, DESC_F_WRITE, QUEUE_MEMORY_BYTES};
use super::transport::LegacyTransport;

const VIRTIO_NET_F_MAC: u32 = 1 << 5;
const VIRTIO_NET_F_STATUS: u32 = 1 << 16;
const VIRTIO_NET_S_LINK_UP: u16 = 1;
const HEADER_BYTES: usize = 10;
const RX_SLOTS: usize = 16;
const RX_STORAGE_BYTES: usize = RX_SLOTS * dma::PAGE_SIZE;
const TX_STORAGE_BYTES: usize = dma::PAGE_SIZE;
const MAX_ETHERNET_FRAME: usize = 1514;

static mut RX_QUEUE: PageAligned<QUEUE_MEMORY_BYTES> = PageAligned::zeroed();
static mut TX_QUEUE: PageAligned<QUEUE_MEMORY_BYTES> = PageAligned::zeroed();
static mut RX_STORAGE: PageAligned<RX_STORAGE_BYTES> = PageAligned::zeroed();
static mut TX_STORAGE: PageAligned<TX_STORAGE_BYTES> = PageAligned::zeroed();

pub struct VirtioNet {
    transport: LegacyTransport,
    receive: SplitQueue,
    transmit: SplitQueue,
    mac: [u8; 6],
    status_feature: bool,
    transmit_in_flight: bool,
}

impl VirtioNet {
    pub fn bind(device: &PciDevice) -> Result<Self, &'static str> {
        let transport = LegacyTransport::bind(device)?;
        let features = transport.host_features();
        if features & VIRTIO_NET_F_MAC == 0 {
            transport.fail();
            return Err("VirtIO network device does not expose a stable MAC address");
        }
        let status_feature = features & VIRTIO_NET_F_STATUS != 0;
        let accepted = VIRTIO_NET_F_MAC
            | if status_feature {
                VIRTIO_NET_F_STATUS
            } else {
                0
            };
        transport.set_guest_features(accepted);

        let mac = [
            transport.read_config_u8(0),
            transport.read_config_u8(1),
            transport.read_config_u8(2),
            transport.read_config_u8(3),
            transport.read_config_u8(4),
            transport.read_config_u8(5),
        ];
        if mac == [0; 6] || mac == [0xFF; 6] || mac[0] & 1 != 0 {
            transport.fail();
            return Err("VirtIO network device supplied an invalid unicast MAC address");
        }

        let mut receive = unsafe { SplitQueue::initialize(&transport, 0, addr_of_mut!(RX_QUEUE))? };
        let transmit = unsafe { SplitQueue::initialize(&transport, 1, addr_of_mut!(TX_QUEUE))? };
        if receive.size() < RX_SLOTS as u16 || transmit.size() == 0 {
            transport.fail();
            return Err("VirtIO network queues are smaller than the bounded driver layout");
        }

        let receive_storage = addr_of_mut!(RX_STORAGE).cast::<u8>();
        // Safety: RX_STORAGE is page-aligned, each slot is exactly one page,
        // and the device owns writes only until its used-ring entry appears.
        unsafe { core::slice::from_raw_parts_mut(receive_storage, RX_STORAGE_BYTES) }.fill(0);
        for slot in 0..RX_SLOTS {
            let buffer = unsafe { receive_storage.add(slot * dma::PAGE_SIZE) };
            let physical = dma::single_page_physical(buffer, dma::PAGE_SIZE)?;
            receive.set_descriptor(
                slot as u16,
                Descriptor {
                    address: physical,
                    length: dma::PAGE_SIZE as u32,
                    flags: DESC_F_WRITE,
                    next: 0,
                },
            );
            receive.submit(slot as u16);
        }
        unsafe { &mut (*addr_of_mut!(TX_STORAGE)).0 }.fill(0);

        transport.finish_init();
        receive.notify(&transport);
        let ownership = transport.ownership(
            "virtio-network",
            2 * QUEUE_MEMORY_BYTES + RX_STORAGE_BYTES + TX_STORAGE_BYTES,
        );
        crate::serial_println!(
            "virtio-net: bind {} MAC={:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x} polling DMA={} bytes",
            ownership.device,
            mac[0],
            mac[1],
            mac[2],
            mac[3],
            mac[4],
            mac[5],
            match ownership.dma {
                crate::hal::driver::DmaOwnership::StaticReserved { bytes } => bytes,
            }
        );
        Ok(Self {
            transport,
            receive,
            transmit,
            mac,
            status_feature,
            transmit_in_flight: false,
        })
    }

    pub fn mac(&self) -> [u8; 6] {
        self.mac
    }

    pub fn ownership(&self) -> crate::hal::driver::DriverOwnership {
        self.transport.ownership(
            "virtio-network",
            2 * QUEUE_MEMORY_BYTES + RX_STORAGE_BYTES + TX_STORAGE_BYTES,
        )
    }

    pub fn link_up(&self) -> bool {
        !self.status_feature || self.transport.read_config_u16(6) & VIRTIO_NET_S_LINK_UP != 0
    }

    fn receive_frame(&mut self, destination: &mut [u8]) -> Option<usize> {
        for _ in 0..RX_SLOTS {
            let (id, written) = self.receive.pop_used()?;
            let valid_id = usize::try_from(id).ok().filter(|id| *id < RX_SLOTS);
            let mut result = None;
            if let Some(slot) = valid_id {
                let written = written as usize;
                if written >= HEADER_BYTES && written <= dma::PAGE_SIZE {
                    let frame_len = written - HEADER_BYTES;
                    if frame_len <= destination.len() {
                        let source = unsafe {
                            addr_of_mut!(RX_STORAGE)
                                .cast::<u8>()
                                .add(slot * dma::PAGE_SIZE + HEADER_BYTES)
                        };
                        for (offset, byte) in destination[..frame_len].iter_mut().enumerate() {
                            *byte = unsafe { read_volatile(source.add(offset)) };
                        }
                        result = Some(frame_len);
                    }
                }
                self.receive.submit(slot as u16);
                self.receive.notify(&self.transport);
            } else {
                crate::serial_println!(
                    "virtio-net: discarded completion with invalid descriptor {}",
                    id
                );
            }
            if result.is_some() {
                return result;
            }
        }
        None
    }

    fn reclaim_transmit(&mut self) {
        if self.transmit_in_flight {
            if let Some((id, _)) = self.transmit.pop_used() {
                // A malformed device completion never grants permission to
                // reuse the DMA buffer while descriptor zero may remain live.
                if id == 0 {
                    self.transmit_in_flight = false;
                }
            }
        }
    }

    fn transmit_available(&mut self) -> bool {
        self.reclaim_transmit();
        !self.transmit_in_flight && self.link_up()
    }

    fn transmit_with<R>(&mut self, frame_len: usize, fill: impl FnOnce(&mut [u8]) -> R) -> R {
        let storage = unsafe { &mut (*addr_of_mut!(TX_STORAGE)).0 };
        storage[..HEADER_BYTES].fill(0);
        let bounded_len = frame_len.min(MAX_ETHERNET_FRAME);
        let result = fill(&mut storage[HEADER_BYTES..HEADER_BYTES + bounded_len]);
        if frame_len > MAX_ETHERNET_FRAME {
            crate::serial_println!("virtio-net: refused oversized {}-byte frame", frame_len);
            return result;
        }
        let physical = match dma::single_page_physical(storage.as_ptr(), HEADER_BYTES + frame_len) {
            Ok(address) => address,
            Err(reason) => {
                crate::serial_println!("virtio-net: transmit DMA validation failed: {}", reason);
                return result;
            }
        };
        self.transmit.set_descriptor(
            0,
            Descriptor {
                address: physical,
                length: (HEADER_BYTES + frame_len) as u32,
                flags: 0,
                next: 0,
            },
        );
        // Ensure the header and payload are visible before publishing the
        // descriptor through the available ring.
        core::sync::atomic::fence(core::sync::atomic::Ordering::Release);
        self.transmit.submit(0);
        self.transmit_in_flight = true;
        self.transmit.notify(&self.transport);
        result
    }

    pub fn shutdown(&mut self) {
        self.transport.reset();
        self.receive.scrub();
        self.transmit.scrub();
        unsafe { &mut (*addr_of_mut!(RX_STORAGE)).0 }.fill(0);
        unsafe { &mut (*addr_of_mut!(TX_STORAGE)).0 }.fill(0);
        self.transmit_in_flight = false;
    }
}

pub struct VirtioNetDevice {
    driver: VirtioNet,
    receive_frame: [u8; MAX_ETHERNET_FRAME],
}

impl VirtioNetDevice {
    pub fn new(driver: VirtioNet) -> Self {
        Self {
            driver,
            receive_frame: [0; MAX_ETHERNET_FRAME],
        }
    }

    pub fn mac(&self) -> [u8; 6] {
        self.driver.mac()
    }

    pub fn link_up(&self) -> bool {
        self.driver.link_up()
    }

    pub fn shutdown(&mut self) {
        self.driver.shutdown();
    }
}

pub struct VirtioRxToken<'a> {
    frame: &'a [u8],
}

impl RxToken for VirtioRxToken<'_> {
    fn consume<R, F>(self, fill: F) -> R
    where
        F: FnOnce(&[u8]) -> R,
    {
        fill(self.frame)
    }
}

pub struct VirtioTxToken<'a> {
    driver: &'a mut VirtioNet,
}

impl TxToken for VirtioTxToken<'_> {
    fn consume<R, F>(self, len: usize, fill: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        self.driver.transmit_with(len, fill)
    }
}

impl Device for VirtioNetDevice {
    type RxToken<'a> = VirtioRxToken<'a>;
    type TxToken<'a> = VirtioTxToken<'a>;

    fn receive(&mut self, _timestamp: Instant) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        let length = self.driver.receive_frame(&mut self.receive_frame)?;
        let frame = &self.receive_frame[..length];
        let transmit = &mut self.driver;
        Some((VirtioRxToken { frame }, VirtioTxToken { driver: transmit }))
    }

    fn transmit(&mut self, _timestamp: Instant) -> Option<Self::TxToken<'_>> {
        self.driver.transmit_available().then_some(VirtioTxToken {
            driver: &mut self.driver,
        })
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let mut capabilities = DeviceCapabilities::default();
        capabilities.medium = Medium::Ethernet;
        capabilities.max_transmission_unit = MAX_ETHERNET_FRAME;
        capabilities.max_burst_size = Some(1);
        capabilities
    }
}
