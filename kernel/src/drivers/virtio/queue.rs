//! Fixed-storage split VirtQueue for the legacy PCI transport.

use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{fence, Ordering};

use crate::hal::dma::{self, PageAligned};

use super::transport::LegacyTransport;

pub const QUEUE_MEMORY_BYTES: usize = 3 * dma::PAGE_SIZE;
const DESC_BYTES: usize = 16;
const AVAIL_OFFSET: usize = 4096;
const USED_OFFSET: usize = 8192;

pub const DESC_F_NEXT: u16 = 1;
pub const DESC_F_WRITE: u16 = 2;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct Descriptor {
    pub address: u64,
    pub length: u32,
    pub flags: u16,
    pub next: u16,
}

pub struct SplitQueue {
    memory: *mut u8,
    size: u16,
    last_used: u16,
    queue_index: u16,
}

impl SplitQueue {
    /// Bind a statically reserved queue region to one transport queue.
    ///
    /// # Safety
    /// `memory` must be uniquely owned by this queue until `reset`/drop and
    /// must not be accessed by the CPU while the device may still perform DMA.
    pub unsafe fn initialize(
        transport: &LegacyTransport,
        queue_index: u16,
        memory: *mut PageAligned<QUEUE_MEMORY_BYTES>,
    ) -> Result<Self, &'static str> {
        transport.select_queue(queue_index);
        let size = transport.queue_size();
        if size == 0 || size > 256 {
            return Err("unsupported VirtIO queue size");
        }
        if transport.queue_pfn() != 0 {
            return Err("VirtIO queue is already owned");
        }
        let bytes = unsafe { &mut (*memory).0 };
        bytes.fill(0);
        let physical = dma::contiguous_physical_start(bytes.as_ptr(), bytes.len())?;
        transport.set_queue_pfn(physical)?;
        let mut queue = Self {
            memory: bytes.as_mut_ptr(),
            size,
            last_used: 0,
            queue_index,
        };
        // Suppress device interrupts.  This driver's explicit IRQ ownership
        // is polling and all completion loops have finite budgets.
        queue.write_u16(AVAIL_OFFSET, 1);
        Ok(queue)
    }

    pub fn size(&self) -> u16 {
        self.size
    }

    pub fn set_descriptor(&mut self, index: u16, descriptor: Descriptor) {
        assert!(index < self.size);
        let pointer = unsafe { self.memory.add(index as usize * DESC_BYTES) as *mut Descriptor };
        // Safety: descriptor table is aligned and index was bounded.
        unsafe { write_volatile(pointer, descriptor) };
    }

    pub fn submit(&mut self, head: u16) {
        assert!(head < self.size);
        let avail_index = self.read_u16(AVAIL_OFFSET + 2);
        let slot = usize::from(avail_index % self.size);
        self.write_u16(AVAIL_OFFSET + 4 + slot * 2, head);
        fence(Ordering::Release);
        self.write_u16(AVAIL_OFFSET + 2, avail_index.wrapping_add(1));
    }

    pub fn notify(&self, transport: &LegacyTransport) {
        transport.notify_queue(self.queue_index);
    }

    pub fn pop_used(&mut self) -> Option<(u32, u32)> {
        let device_index = self.read_u16(USED_OFFSET + 2);
        fence(Ordering::Acquire);
        if self.last_used == device_index {
            return None;
        }
        let slot = usize::from(self.last_used % self.size);
        let element = USED_OFFSET + 4 + slot * 8;
        let id = self.read_u32(element);
        let len = self.read_u32(element + 4);
        self.last_used = self.last_used.wrapping_add(1);
        Some((id, len))
    }

    pub fn scrub(&mut self) {
        // The caller resets the device before scrubbing this region.
        unsafe { core::slice::from_raw_parts_mut(self.memory, QUEUE_MEMORY_BYTES) }.fill(0);
        self.last_used = 0;
    }

    fn read_u16(&self, offset: usize) -> u16 {
        // Safety: all offsets are inside the reserved queue layout.
        unsafe { read_volatile(self.memory.add(offset) as *const u16) }
    }

    fn write_u16(&mut self, offset: usize, value: u16) {
        // Safety: all offsets are inside the reserved queue layout.
        unsafe { write_volatile(self.memory.add(offset) as *mut u16, value) }
    }

    fn read_u32(&self, offset: usize) -> u32 {
        // Safety: all offsets are inside the reserved queue layout.
        unsafe { read_volatile(self.memory.add(offset) as *const u32) }
    }
}
