//! Boot-storage selection and serialization.
//!
//! NVMe is preferred when a supported 512-byte namespace is present; ATA PIO
//! remains the Phase 0-7 fallback. A non-spinning atomic lease grants unique
//! access without holding a global lock across disk I/O.

use core::cell::UnsafeCell;
use core::mem::MaybeUninit;
use core::sync::atomic::{AtomicU8, Ordering};

use crate::drivers::nvme::{self, NvmeController};

const UNAVAILABLE: u8 = 0;
const READY: u8 = 1;
const BUSY: u8 = 2;

enum Backend {
    Nvme(NvmeController),
    Ata,
}

struct BackendSlot(UnsafeCell<MaybeUninit<Backend>>);

// Safety: STATE grants at most one mutable accessor. Initialization occurs
// once during single-threaded boot before READY is published.
unsafe impl Sync for BackendSlot {}

static SLOT: BackendSlot = BackendSlot(UnsafeCell::new(MaybeUninit::uninit()));
static STATE: AtomicU8 = AtomicU8::new(UNAVAILABLE);

pub fn init() {
    for device in crate::hal::pci::discover()
        .iter()
        .filter(|device| nvme::is_nvme(device))
    {
        crate::serial_println!(
            "storage: probing NVMe {} {:04x}:{:04x}",
            device.address,
            device.vendor_id,
            device.device_id
        );
        match NvmeController::bind(device) {
            Ok((controller, info)) => {
                unsafe { (*SLOT.0.get()).write(Backend::Nvme(controller)) };
                STATE.store(READY, Ordering::Release);
                crate::serial_println!(
                    "storage: NVMe active nsid={} sectors={} version={}.{}.{}",
                    info.namespace_id,
                    info.sector_count,
                    info.version >> 16,
                    (info.version >> 8) & 0xFF,
                    info.version & 0xFF
                );
                return;
            }
            Err(reason) => {
                crate::serial_println!("storage: NVMe rejected safely: {}", reason);
                crate::serial_println!(
                    "storage: NVMe failure cleanup active_claims={}",
                    crate::hal::driver::active_claims()
                );
                crate::boot_diag::degrade("nvme", reason, "try-ata-or-recovery");
            }
        }
        // A single controller is the bounded Phase 7B boot-storage limit.
        break;
    }

    if crate::ata::init() {
        unsafe { (*SLOT.0.get()).write(Backend::Ata) };
        STATE.store(READY, Ordering::Release);
        crate::serial_println!("storage: ATA PIO fallback active");
    } else {
        crate::serial_println!("storage: no supported NVMe or ATA boot disk");
        crate::boot_diag::degrade(
            "boot-storage",
            "no-supported-backend",
            "recovery-console-read-only",
        );
    }
}

fn with_backend<T>(
    operation: impl FnOnce(&mut Backend) -> Result<T, &'static str>,
) -> Result<T, &'static str> {
    STATE
        .compare_exchange(READY, BUSY, Ordering::Acquire, Ordering::Relaxed)
        .map_err(|_| "boot storage busy or unavailable")?;
    let backend = unsafe { (&mut *SLOT.0.get()).assume_init_mut() };
    let result = operation(backend);
    STATE.store(READY, Ordering::Release);
    result
}

pub fn read_sector(lba: u32, out: &mut [u8; 512]) -> Result<(), &'static str> {
    with_backend(|backend| match backend {
        Backend::Nvme(controller) => controller.read_sector(u64::from(lba), out),
        Backend::Ata => crate::ata::read_sector(lba, out),
    })
}

pub fn write_sector(lba: u32, input: &[u8; 512]) -> Result<(), &'static str> {
    with_backend(|backend| match backend {
        Backend::Nvme(controller) => controller.write_sector(u64::from(lba), input),
        Backend::Ata => crate::ata::write_sector(lba, input),
    })
}

pub fn flush() -> Result<(), &'static str> {
    with_backend(|backend| match backend {
        Backend::Nvme(controller) => controller.flush(),
        Backend::Ata => crate::ata::flush(),
    })
}

pub fn prepare_reboot() -> Result<(), &'static str> {
    with_backend(|backend| match backend {
        Backend::Nvme(controller) => controller.prepare_reboot(),
        Backend::Ata => crate::ata::flush(),
    })
}

pub fn backend_name() -> &'static str {
    if STATE
        .compare_exchange(READY, BUSY, Ordering::Acquire, Ordering::Relaxed)
        .is_err()
    {
        return "unavailable";
    }
    let backend = unsafe { (&mut *SLOT.0.get()).assume_init_mut() };
    let name = match backend {
        Backend::Nvme(_) => "nvme",
        Backend::Ata => "ata-pio",
    };
    STATE.store(READY, Ordering::Release);
    name
}

pub struct NvmeSelfTest {
    pub invalid_lba_rejected: bool,
    pub malformed_namespace_rejected: bool,
    pub timeout_bounded: bool,
    pub reset_recovered: bool,
    pub sector_count: u64,
    pub claims_stable: bool,
    pub frames_stable: bool,
    pub heap_stable: bool,
}

pub fn nvme_self_test() -> Result<NvmeSelfTest, &'static str> {
    with_backend(|backend| {
        let Backend::Nvme(controller) = backend else {
            return Err("active boot storage is not NVMe");
        };
        let sector_count = controller.sector_count();
        let claims_before = crate::hal::driver::active_claims();
        let frames_before = crate::paging::frame_stats().map(|stats| stats.bumped);
        let heap_before = crate::allocator::used();
        let invalid_lba_rejected = controller.validate_lba(sector_count).is_err();
        let malformed_namespace_rejected = nvme::malformed_namespace_data_is_rejected();
        controller.exercise_timeout_and_recover()?;
        let mut sector = [0u8; 512];
        controller.read_sector(0, &mut sector)?;
        let claims_after = crate::hal::driver::active_claims();
        let frames_after = crate::paging::frame_stats().map(|stats| stats.bumped);
        let heap_after = crate::allocator::used();
        Ok(NvmeSelfTest {
            invalid_lba_rejected,
            malformed_namespace_rejected,
            timeout_bounded: true,
            reset_recovered: true,
            sector_count,
            claims_stable: claims_before == claims_after,
            frames_stable: frames_before == frames_after,
            heap_stable: heap_before == heap_after,
        })
    })
}
