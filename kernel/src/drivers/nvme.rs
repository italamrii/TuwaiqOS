//! Bounded polling NVMe driver for a single 512-byte namespace.
//!
//! The driver owns one PCI function, an uncached MMIO window, and fixed
//! page-aligned DMA storage. It never allocates or holds a global lock while
//! touching the controller. All controller and command waits have finite
//! tick and iteration budgets.

use core::mem::size_of;
use core::ptr::{addr_of_mut, read_volatile, write_volatile};
use core::sync::atomic::{fence, Ordering};

use crate::hal::dma::{self, PageAligned};
use crate::hal::driver::{
    self, DmaOwnership, DriverClaim, DriverOwnership, IrqOwnership, RegisterOwnership,
    TeardownOwnership,
};
use crate::hal::pci::PciDevice;

const PCI_CLASS_STORAGE: u8 = 0x01;
const PCI_SUBCLASS_NVME: u8 = 0x08;
const PCI_PROGIF_NVME: u8 = 0x02;
const MMIO_VIRTUAL_BASE: u64 = 0x5555_0000_0000;
const MMIO_BYTES: u64 = 0x2000;
const QUEUE_ENTRIES: u16 = 16;
const ADMIN_QUEUE_ID: u16 = 0;
const IO_QUEUE_ID: u16 = 1;
const MAX_NAMESPACE_SCAN: u32 = 32;
const MAX_WAIT_ITERATIONS: usize = 20_000_000;

const REG_CAP: usize = 0x00;
const REG_VS: usize = 0x08;
const REG_INTMS: usize = 0x0C;
const REG_CC: usize = 0x14;
const REG_CSTS: usize = 0x1C;
const REG_AQA: usize = 0x24;
const REG_ASQ: usize = 0x28;
const REG_ACQ: usize = 0x30;
const REG_DOORBELL: usize = 0x1000;

const CC_ENABLE: u32 = 1;
const CSTS_READY: u32 = 1;
const CSTS_FATAL: u32 = 1 << 1;

const ADMIN_CREATE_IO_SQ: u8 = 0x01;
const ADMIN_CREATE_IO_CQ: u8 = 0x05;
const ADMIN_IDENTIFY: u8 = 0x06;
const NVM_FLUSH: u8 = 0x00;
const NVM_WRITE: u8 = 0x01;
const NVM_READ: u8 = 0x02;

fn namespace_sector_count(bytes: &[u8]) -> Result<Option<u64>, &'static str> {
    if bytes.len() < 192 {
        return Err("NVMe namespace identify data is truncated");
    }
    let size = u64::from_le_bytes(
        bytes[0..8]
            .try_into()
            .map_err(|_| "NVMe namespace size is malformed")?,
    );
    let capacity = u64::from_le_bytes(
        bytes[8..16]
            .try_into()
            .map_err(|_| "NVMe namespace capacity is malformed")?,
    );
    if size == 0 || capacity == 0 {
        return Ok(None);
    }
    if capacity > size {
        return Err("NVMe namespace capacity exceeds its size");
    }
    let formats = usize::from(bytes[25]).saturating_add(1);
    let selected = usize::from(bytes[26] & 0x0F);
    if formats > 16 || selected >= formats {
        return Err("NVMe namespace format index is malformed");
    }
    let offset = 128usize
        .checked_add(selected.checked_mul(4).ok_or("NVMe LBA format overflow")?)
        .ok_or("NVMe LBA format offset overflow")?;
    let metadata = u16::from_le_bytes(
        bytes[offset..offset + 2]
            .try_into()
            .map_err(|_| "NVMe LBA metadata is malformed")?,
    );
    let sector_shift = bytes[offset + 2];
    if metadata != 0 || sector_shift != 9 {
        return Ok(None);
    }
    Ok(Some(size))
}

pub fn malformed_namespace_data_is_rejected() -> bool {
    let mut bytes = [0u8; 192];
    bytes[0..8].copy_from_slice(&1u64.to_le_bytes());
    bytes[8..16].copy_from_slice(&2u64.to_le_bytes());
    bytes[25] = 0;
    bytes[26] = 0;
    bytes[130] = 9;
    if namespace_sector_count(&bytes).is_ok() {
        return false;
    }
    bytes[8..16].copy_from_slice(&1u64.to_le_bytes());
    bytes[26] = 1;
    namespace_sector_count(&bytes).is_err()
}

static mut ADMIN_SQ: PageAligned<4096> = PageAligned::zeroed();
static mut ADMIN_CQ: PageAligned<4096> = PageAligned::zeroed();
static mut IO_SQ: PageAligned<4096> = PageAligned::zeroed();
static mut IO_CQ: PageAligned<4096> = PageAligned::zeroed();
static mut IDENTIFY: PageAligned<4096> = PageAligned::zeroed();
static mut DATA: PageAligned<4096> = PageAligned::zeroed();

#[repr(C)]
#[derive(Clone, Copy)]
struct Command {
    cdw0: u32,
    nsid: u32,
    reserved: u64,
    mptr: u64,
    prp1: u64,
    prp2: u64,
    cdw10: u32,
    cdw11: u32,
    cdw12: u32,
    cdw13: u32,
    cdw14: u32,
    cdw15: u32,
}

impl Command {
    const fn zeroed() -> Self {
        Self {
            cdw0: 0,
            nsid: 0,
            reserved: 0,
            mptr: 0,
            prp1: 0,
            prp2: 0,
            cdw10: 0,
            cdw11: 0,
            cdw12: 0,
            cdw13: 0,
            cdw14: 0,
            cdw15: 0,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Completion {
    result: u32,
    reserved: u32,
    sq_head: u16,
    sq_id: u16,
    command_id: u16,
    status: u16,
}

struct QueuePair {
    submission: *mut Command,
    completion: *mut Completion,
    submission_tail: u16,
    completion_head: u16,
    completion_phase: u16,
    next_command_id: u16,
    queue_id: u16,
    entries: u16,
}

impl QueuePair {
    unsafe fn new(submission: *mut u8, completion: *mut u8, queue_id: u16, entries: u16) -> Self {
        Self {
            submission: submission.cast(),
            completion: completion.cast(),
            submission_tail: 0,
            completion_head: 0,
            completion_phase: 1,
            next_command_id: 1,
            queue_id,
            entries,
        }
    }

    fn prepare(&mut self, mut command: Command) -> (u16, Command) {
        let command_id = self.next_command_id;
        self.next_command_id = self.next_command_id.wrapping_add(1).max(1);
        command.cdw0 = (command.cdw0 & 0xFFFF) | (u32::from(command_id) << 16);
        (command_id, command)
    }
}

pub struct NvmeController {
    mmio: *mut u8,
    doorbell_stride: usize,
    timeout_ticks: u64,
    admin: QueuePair,
    io: QueuePair,
    namespace_id: u32,
    namespace_count: u32,
    sector_count: u64,
    healthy: bool,
    claim: DriverClaim,
}

// Safety: access is serialized by the storage facade's atomic lease. The
// contained raw pointer targets one fixed supervisor-only MMIO mapping.
unsafe impl Send for NvmeController {}

#[derive(Clone, Copy)]
pub struct NvmeInfo {
    pub namespace_id: u32,
    pub sector_count: u64,
    pub version: u32,
}

pub fn is_nvme(device: &PciDevice) -> bool {
    device.class == PCI_CLASS_STORAGE
        && device.subclass == PCI_SUBCLASS_NVME
        && device.programming_interface == PCI_PROGIF_NVME
}

impl NvmeController {
    pub fn bind(device: &PciDevice) -> Result<(Self, NvmeInfo), &'static str> {
        if !is_nvme(device) {
            return Err("PCI function is not an NVMe controller");
        }
        let raw_low = device.bars[0];
        if raw_low & 1 != 0 || ((raw_low >> 1) & 0x3) != 2 {
            return Err("NVMe BAR0 must be a 64-bit memory BAR");
        }
        let physical_base = (u64::from(device.bars[1]) << 32) | u64::from(raw_low & !0xF);
        if physical_base == 0 {
            return Err("NVMe BAR0 is unassigned");
        }
        let ownership = DriverOwnership {
            name: "nvme-storage",
            device: device.address,
            registers: RegisterOwnership::Mmio {
                physical_base,
                bytes: MMIO_BYTES,
            },
            irq: IrqOwnership::Polling,
            dma: DmaOwnership::StaticReserved { bytes: 6 * 4096 },
            teardown: TeardownOwnership::ResetAndScrub,
        };
        let claim = driver::claim(ownership)?;
        let bar = match device.probe_memory_bar(0) {
            Ok(bar) => bar,
            Err(reason) => {
                drop(claim);
                return Err(reason);
            }
        };
        if bar.base != physical_base || bar.size < MMIO_BYTES || bar.size > 16 * 1024 * 1024 {
            drop(claim);
            return Err("NVMe BAR geometry is unsupported or changed during probe");
        }
        let virtual_base =
            crate::paging::map_mmio_range(physical_base, MMIO_BYTES, MMIO_VIRTUAL_BASE)?;
        let original_pci_command = crate::hal::pci::read_u16(device.address, 0x04);
        device.enable_memory_bus_mastering();

        let admin_sq = addr_of_mut!(ADMIN_SQ).cast::<u8>();
        let admin_cq = addr_of_mut!(ADMIN_CQ).cast::<u8>();
        let io_sq = addr_of_mut!(IO_SQ).cast::<u8>();
        let io_cq = addr_of_mut!(IO_CQ).cast::<u8>();
        let mut controller = Self {
            mmio: virtual_base.as_mut_ptr(),
            doorbell_stride: 0,
            timeout_ticks: 100,
            admin: unsafe { QueuePair::new(admin_sq, admin_cq, ADMIN_QUEUE_ID, QUEUE_ENTRIES) },
            io: unsafe { QueuePair::new(io_sq, io_cq, IO_QUEUE_ID, QUEUE_ENTRIES) },
            namespace_id: 0,
            namespace_count: 0,
            sector_count: 0,
            healthy: false,
            claim,
        };
        if let Err(reason) = controller.initialize() {
            if let Err(disable_reason) = controller.disable() {
                crate::serial_println!(
                    "nvme: controller quarantine after initialization failure: {}",
                    disable_reason
                );
                controller.claim.quarantine();
                return Err("NVMe initialization failed and controller could not be disabled");
            }
            scrub_dma();
            controller.claim.release();
            let _ = crate::paging::unmap_mmio_range(MMIO_VIRTUAL_BASE, MMIO_BYTES);
            crate::hal::pci::write_u16(device.address, 0x04, original_pci_command);
            return Err(reason);
        }
        let info = NvmeInfo {
            namespace_id: controller.namespace_id,
            sector_count: controller.sector_count,
            version: controller.read32(REG_VS),
        };
        Ok((controller, info))
    }

    fn initialize(&mut self) -> Result<(), &'static str> {
        if size_of::<Command>() != 64 || size_of::<Completion>() != 16 {
            return Err("NVMe queue entry layout mismatch");
        }
        let cap = self.read64(REG_CAP);
        if ((cap >> 37) & 1) == 0 {
            return Err("NVMe controller lacks the NVM command set");
        }
        if ((cap >> 48) & 0xF) != 0 {
            return Err("NVMe controller does not support 4096-byte host pages");
        }
        let max_entries = ((cap & 0xFFFF) + 1) as u16;
        if max_entries < QUEUE_ENTRIES {
            return Err("NVMe controller queue capacity is below the bounded layout");
        }
        let dstrd = ((cap >> 32) & 0xF) as u32;
        if dstrd > 7 {
            return Err("NVMe doorbell stride exceeds mapped bounds");
        }
        self.doorbell_stride = 4usize
            .checked_shl(dstrd)
            .ok_or("NVMe doorbell stride overflow")?;
        let cap_timeout = ((cap >> 24) & 0xFF).max(1);
        self.timeout_ticks = cap_timeout.saturating_mul(50).min(1_000);

        self.disable()?;
        scrub_dma();
        self.write32(REG_INTMS, u32::MAX);
        let admin_sq_phys = dma::single_page_physical(addr_of_mut!(ADMIN_SQ).cast(), 4096)?;
        let admin_cq_phys = dma::single_page_physical(addr_of_mut!(ADMIN_CQ).cast(), 4096)?;
        self.write32(
            REG_AQA,
            u32::from(QUEUE_ENTRIES - 1) | (u32::from(QUEUE_ENTRIES - 1) << 16),
        );
        self.write64(REG_ASQ, admin_sq_phys);
        self.write64(REG_ACQ, admin_cq_phys);
        let cc = CC_ENABLE | (6 << 16) | (4 << 20);
        self.write32(REG_CC, cc);
        self.wait_ready(true)?;
        self.healthy = true;

        self.identify_controller()?;
        self.identify_namespace()?;
        self.create_io_queues()?;
        Ok(())
    }

    fn identify_controller(&mut self) -> Result<(), &'static str> {
        unsafe { (*addr_of_mut!(IDENTIFY)).0.fill(0) };
        let mut command = Command::zeroed();
        command.cdw0 = u32::from(ADMIN_IDENTIFY);
        command.prp1 = dma::single_page_physical(addr_of_mut!(IDENTIFY).cast(), 4096)?;
        command.cdw10 = 1;
        self.submit_admin(command)?;
        fence(Ordering::Acquire);
        let bytes = unsafe { &(*addr_of_mut!(IDENTIFY)).0 };
        let namespaces = u32::from_le_bytes(bytes[516..520].try_into().unwrap());
        if namespaces == 0 || namespaces > 1024 {
            return Err("NVMe controller reported invalid namespace count");
        }
        self.namespace_count = namespaces;
        Ok(())
    }

    fn identify_namespace(&mut self) -> Result<(), &'static str> {
        for namespace_id in 1..=self.namespace_count.min(MAX_NAMESPACE_SCAN) {
            unsafe { (*addr_of_mut!(IDENTIFY)).0.fill(0) };
            let mut command = Command::zeroed();
            command.cdw0 = u32::from(ADMIN_IDENTIFY);
            command.nsid = namespace_id;
            command.prp1 = dma::single_page_physical(addr_of_mut!(IDENTIFY).cast(), 4096)?;
            command.cdw10 = 0;
            if self.submit_admin(command).is_err() {
                continue;
            }
            fence(Ordering::Acquire);
            let bytes = unsafe { &(*addr_of_mut!(IDENTIFY)).0 };
            let Some(size) = namespace_sector_count(bytes)? else {
                continue;
            };
            self.namespace_id = namespace_id;
            self.sector_count = size;
            return Ok(());
        }
        Err("NVMe has no active 512-byte namespace in the bounded scan")
    }

    fn create_io_queues(&mut self) -> Result<(), &'static str> {
        let io_cq_phys = dma::single_page_physical(addr_of_mut!(IO_CQ).cast(), 4096)?;
        let mut cq = Command::zeroed();
        cq.cdw0 = u32::from(ADMIN_CREATE_IO_CQ);
        cq.prp1 = io_cq_phys;
        cq.cdw10 = u32::from(IO_QUEUE_ID) | (u32::from(QUEUE_ENTRIES - 1) << 16);
        cq.cdw11 = 1;
        self.submit_admin(cq)?;

        let io_sq_phys = dma::single_page_physical(addr_of_mut!(IO_SQ).cast(), 4096)?;
        let mut sq = Command::zeroed();
        sq.cdw0 = u32::from(ADMIN_CREATE_IO_SQ);
        sq.prp1 = io_sq_phys;
        sq.cdw10 = u32::from(IO_QUEUE_ID) | (u32::from(QUEUE_ENTRIES - 1) << 16);
        sq.cdw11 = 1 | (u32::from(IO_QUEUE_ID) << 16);
        self.submit_admin(sq)
    }

    pub fn read_sector(&mut self, lba: u64, out: &mut [u8; 512]) -> Result<(), &'static str> {
        self.require_healthy()?;
        self.validate_lba(lba)?;
        unsafe { (&mut (*addr_of_mut!(DATA)).0)[..512].fill(0) };
        self.submit_io(NVM_READ, lba)?;
        fence(Ordering::Acquire);
        out.copy_from_slice(unsafe { &(&(*addr_of_mut!(DATA)).0)[..512] });
        Ok(())
    }

    pub fn write_sector(&mut self, lba: u64, input: &[u8; 512]) -> Result<(), &'static str> {
        self.require_healthy()?;
        self.validate_lba(lba)?;
        unsafe { (&mut (*addr_of_mut!(DATA)).0)[..512].copy_from_slice(input) };
        fence(Ordering::Release);
        self.submit_io(NVM_WRITE, lba)
    }

    pub fn flush(&mut self) -> Result<(), &'static str> {
        self.require_healthy()?;
        let mut command = Command::zeroed();
        command.cdw0 = u32::from(NVM_FLUSH);
        command.nsid = self.namespace_id;
        let queue = addr_of_mut!(self.io);
        self.submit(queue, command)
    }

    pub fn validate_lba(&self, lba: u64) -> Result<(), &'static str> {
        if lba >= self.sector_count {
            Err("NVMe LBA is outside the namespace")
        } else {
            Ok(())
        }
    }

    pub fn sector_count(&self) -> u64 {
        self.sector_count
    }

    fn submit_io(&mut self, opcode: u8, lba: u64) -> Result<(), &'static str> {
        let mut command = Command::zeroed();
        command.cdw0 = u32::from(opcode);
        command.nsid = self.namespace_id;
        command.prp1 = dma::single_page_physical(addr_of_mut!(DATA).cast(), 512)?;
        command.cdw10 = lba as u32;
        command.cdw11 = (lba >> 32) as u32;
        command.cdw12 = 0;
        let queue = addr_of_mut!(self.io);
        self.submit(queue, command)
    }

    fn submit_admin(&mut self, command: Command) -> Result<(), &'static str> {
        let queue = addr_of_mut!(self.admin);
        self.submit(queue, command)
    }

    fn submit(&mut self, queue: *mut QueuePair, command: Command) -> Result<(), &'static str> {
        self.submit_with_iteration_budget(queue, command, MAX_WAIT_ITERATIONS)
    }

    fn submit_with_iteration_budget(
        &mut self,
        queue: *mut QueuePair,
        command: Command,
        iteration_budget: usize,
    ) -> Result<(), &'static str> {
        // Safety: queue points to one of self's queue pairs. The storage lease
        // grants unique controller access for the complete operation.
        let queue = unsafe { &mut *queue };
        let (command_id, command) = queue.prepare(command);
        unsafe {
            write_volatile(
                queue.submission.add(usize::from(queue.submission_tail)),
                command,
            );
        }
        fence(Ordering::Release);
        queue.submission_tail = (queue.submission_tail + 1) % queue.entries;
        self.write_doorbell(queue.queue_id, false, queue.submission_tail)?;

        let start_tick = crate::interrupts::ticks();
        for _ in 0..iteration_budget {
            let completion =
                unsafe { read_volatile(queue.completion.add(usize::from(queue.completion_head))) };
            if completion.status & 1 == queue.completion_phase {
                fence(Ordering::Acquire);
                if completion.command_id != command_id || completion.sq_id != queue.queue_id {
                    self.healthy = false;
                    return Err("NVMe completion did not match the submitted command");
                }
                queue.completion_head = (queue.completion_head + 1) % queue.entries;
                if queue.completion_head == 0 {
                    queue.completion_phase ^= 1;
                }
                self.write_doorbell(queue.queue_id, true, queue.completion_head)?;
                if completion.status & !1 != 0 {
                    crate::serial_println!(
                        "nvme: command opcode={:#x} status={:#06x}",
                        command.cdw0 as u8,
                        completion.status
                    );
                    return Err("NVMe command completed with an error");
                }
                return Ok(());
            }
            if self.read32(REG_CSTS) & CSTS_FATAL != 0 {
                self.healthy = false;
                return Err("NVMe controller entered fatal status");
            }
            if crate::interrupts::ticks().saturating_sub(start_tick) >= self.timeout_ticks {
                self.healthy = false;
                return Err("NVMe command timeout");
            }
            core::hint::spin_loop();
        }
        self.healthy = false;
        Err("NVMe command iteration budget exhausted")
    }

    fn write_doorbell(
        &self,
        queue_id: u16,
        completion: bool,
        value: u16,
    ) -> Result<(), &'static str> {
        let index = usize::from(queue_id)
            .checked_mul(2)
            .and_then(|value| value.checked_add(usize::from(completion)))
            .ok_or("NVMe doorbell index overflow")?;
        let offset = index
            .checked_mul(self.doorbell_stride)
            .and_then(|value| value.checked_add(REG_DOORBELL))
            .ok_or("NVMe doorbell offset overflow")?;
        if offset.checked_add(4).ok_or("NVMe doorbell end overflow")? > MMIO_BYTES as usize {
            return Err("NVMe doorbell is outside the mapped BAR");
        }
        self.write32(offset, u32::from(value));
        Ok(())
    }

    fn wait_ready(&self, ready: bool) -> Result<(), &'static str> {
        let start_tick = crate::interrupts::ticks();
        for _ in 0..MAX_WAIT_ITERATIONS {
            let status = self.read32(REG_CSTS);
            if status & CSTS_FATAL != 0 {
                return Err("NVMe controller fatal status while changing state");
            }
            if (status & CSTS_READY != 0) == ready {
                return Ok(());
            }
            if crate::interrupts::ticks().saturating_sub(start_tick) >= self.timeout_ticks {
                return Err("NVMe controller state timeout");
            }
            core::hint::spin_loop();
        }
        Err("NVMe controller state iteration budget exhausted")
    }

    fn disable(&mut self) -> Result<(), &'static str> {
        if self.read32(REG_CC) & CC_ENABLE != 0 {
            self.write32(REG_CC, self.read32(REG_CC) & !CC_ENABLE);
            self.wait_ready(false)?;
        }
        Ok(())
    }

    pub fn reset_and_recover(&mut self) -> Result<(), &'static str> {
        if self.healthy {
            self.flush()?;
        }
        self.healthy = false;
        self.disable()?;
        scrub_dma();
        self.admin = unsafe {
            QueuePair::new(
                addr_of_mut!(ADMIN_SQ).cast(),
                addr_of_mut!(ADMIN_CQ).cast(),
                ADMIN_QUEUE_ID,
                QUEUE_ENTRIES,
            )
        };
        self.io = unsafe {
            QueuePair::new(
                addr_of_mut!(IO_SQ).cast(),
                addr_of_mut!(IO_CQ).cast(),
                IO_QUEUE_ID,
                QUEUE_ENTRIES,
            )
        };
        self.namespace_id = 0;
        self.namespace_count = 0;
        self.sector_count = 0;
        self.initialize()
    }

    pub fn prepare_reboot(&mut self) -> Result<(), &'static str> {
        if self.healthy {
            self.flush()?;
        }
        self.healthy = false;
        self.disable()
    }

    /// Ring one real read, deliberately give completion polling a zero-sized
    /// budget, and then prove reset discards the outstanding queue/DMA state.
    /// This is exposed only through the privileged Phase 7B diagnostic.
    pub fn exercise_timeout_and_recover(&mut self) -> Result<(), &'static str> {
        self.require_healthy()?;
        self.validate_lba(0)?;
        let mut command = Command::zeroed();
        command.cdw0 = u32::from(NVM_READ);
        command.nsid = self.namespace_id;
        command.prp1 = dma::single_page_physical(addr_of_mut!(DATA).cast(), 512)?;
        let queue = addr_of_mut!(self.io);
        if self.submit_with_iteration_budget(queue, command, 0).is_ok() {
            return Err("NVMe timeout injection unexpectedly completed");
        }
        if self.healthy {
            return Err("NVMe timeout did not poison the controller");
        }
        self.reset_and_recover()?;
        let mut sector = [0u8; 512];
        self.read_sector(0, &mut sector)
    }

    fn require_healthy(&self) -> Result<(), &'static str> {
        if self.healthy {
            Ok(())
        } else {
            Err("NVMe controller requires a successful reset before further I/O")
        }
    }

    fn read32(&self, offset: usize) -> u32 {
        unsafe { read_volatile(self.mmio.add(offset).cast::<u32>()) }
    }

    fn read64(&self, offset: usize) -> u64 {
        unsafe { read_volatile(self.mmio.add(offset).cast::<u64>()) }
    }

    fn write32(&self, offset: usize, value: u32) {
        unsafe { write_volatile(self.mmio.add(offset).cast::<u32>(), value) }
    }

    fn write64(&self, offset: usize, value: u64) {
        unsafe { write_volatile(self.mmio.add(offset).cast::<u64>(), value) }
    }
}

fn scrub_dma() {
    unsafe {
        (*addr_of_mut!(ADMIN_SQ)).0.fill(0);
        (*addr_of_mut!(ADMIN_CQ)).0.fill(0);
        (*addr_of_mut!(IO_SQ)).0.fill(0);
        (*addr_of_mut!(IO_CQ)).0.fill(0);
        (*addr_of_mut!(IDENTIFY)).0.fill(0);
        (*addr_of_mut!(DATA)).0.fill(0);
    }
    fence(Ordering::SeqCst);
}
