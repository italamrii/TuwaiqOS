//! Bounded VirtIO block probe and read-path qualification.

use core::hint::spin_loop;
use core::ptr::{addr_of_mut, read_volatile, write_volatile};

use crate::hal::dma::{self, PageAligned};
use crate::hal::pci::PciDevice;

use super::queue::{Descriptor, SplitQueue, DESC_F_NEXT, DESC_F_WRITE, QUEUE_MEMORY_BYTES};
use super::transport::LegacyTransport;

const REQUEST_BYTES: usize = 4096;
const HEADER_OFFSET: usize = 0;
const DATA_OFFSET: usize = 16;
const DATA_BYTES: usize = 512;
const STATUS_OFFSET: usize = DATA_OFFSET + DATA_BYTES;
const COMPLETION_BUDGET: usize = 2_000_000;

static mut BLOCK_QUEUE: PageAligned<QUEUE_MEMORY_BYTES> = PageAligned::zeroed();
static mut BLOCK_REQUEST: PageAligned<REQUEST_BYTES> = PageAligned::zeroed();

pub struct BlockProbe {
    pub capacity_sectors: u64,
    pub first_sector_checksum: u32,
}

pub fn probe_and_read(device: &PciDevice) -> Result<BlockProbe, &'static str> {
    let transport = LegacyTransport::bind(device)?;
    let _host_features = transport.host_features();
    transport.set_guest_features(0);
    let ownership = transport.ownership(
        "virtio-block",
        QUEUE_MEMORY_BYTES.saturating_add(REQUEST_BYTES),
    );
    crate::serial_println!(
        "virtio-blk: bind {} polling DMA={} bytes",
        ownership.device,
        match ownership.dma {
            crate::hal::driver::DmaOwnership::StaticReserved { bytes } => bytes,
        }
    );
    let mut queue = unsafe { SplitQueue::initialize(&transport, 0, addr_of_mut!(BLOCK_QUEUE))? };
    if queue.size() < 3 {
        transport.fail();
        return Err("VirtIO block queue has fewer than three descriptors");
    }
    let request = unsafe { &mut (*addr_of_mut!(BLOCK_REQUEST)).0 };
    request.fill(0);
    // VirtIO block request: type=IN, reserved=0, sector=0.
    unsafe {
        write_volatile(request.as_mut_ptr().add(HEADER_OFFSET) as *mut u32, 0);
        write_volatile(request.as_mut_ptr().add(HEADER_OFFSET + 4) as *mut u32, 0);
        write_volatile(request.as_mut_ptr().add(HEADER_OFFSET + 8) as *mut u64, 0);
        write_volatile(request.as_mut_ptr().add(STATUS_OFFSET), 0xFF);
    }
    let header_phys = dma::single_page_physical(request.as_ptr(), 16)?;
    let data_phys =
        dma::single_page_physical(unsafe { request.as_ptr().add(DATA_OFFSET) }, DATA_BYTES)?;
    let status_phys = dma::single_page_physical(unsafe { request.as_ptr().add(STATUS_OFFSET) }, 1)?;
    queue.set_descriptor(
        0,
        Descriptor {
            address: header_phys,
            length: 16,
            flags: DESC_F_NEXT,
            next: 1,
        },
    );
    queue.set_descriptor(
        1,
        Descriptor {
            address: data_phys,
            length: DATA_BYTES as u32,
            flags: DESC_F_NEXT | DESC_F_WRITE,
            next: 2,
        },
    );
    queue.set_descriptor(
        2,
        Descriptor {
            address: status_phys,
            length: 1,
            flags: DESC_F_WRITE,
            next: 0,
        },
    );
    transport.finish_init();
    queue.submit(0);
    queue.notify(&transport);
    let mut completion = None;
    for _ in 0..COMPLETION_BUDGET {
        if let Some(used) = queue.pop_used() {
            completion = Some(used);
            break;
        }
        spin_loop();
    }
    let result = (|| {
        let (id, written) = completion.ok_or("VirtIO block request timed out")?;
        if id != 0 || written < (DATA_BYTES + 1) as u32 {
            return Err("VirtIO block completion was malformed");
        }
        let status = unsafe { read_volatile(request.as_ptr().add(STATUS_OFFSET)) };
        if status != 0 {
            return Err("VirtIO block device rejected sector read");
        }
        let capacity_sectors = u64::from(transport.read_config_u32(0))
            | (u64::from(transport.read_config_u32(4)) << 32);
        let checksum = request[DATA_OFFSET..DATA_OFFSET + DATA_BYTES]
            .iter()
            .fold(0u32, |sum, byte| sum.wrapping_add(u32::from(*byte)));
        Ok(BlockProbe {
            capacity_sectors,
            first_sector_checksum: checksum,
        })
    })();
    transport.reset();
    queue.scrub();
    request.fill(0);
    result
}
