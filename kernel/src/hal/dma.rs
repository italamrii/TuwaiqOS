//! DMA address validation for statically reserved, page-aligned storage.

use x86_64::VirtAddr;

pub const PAGE_SIZE: usize = 4096;

#[repr(C, align(4096))]
pub struct PageAligned<const N: usize>(pub [u8; N]);

impl<const N: usize> PageAligned<N> {
    pub const fn zeroed() -> Self {
        Self([0; N])
    }
}

/// Prove that a kernel virtual range is backed by consecutive physical
/// pages, as required by the legacy VirtIO queue-PFN interface.
pub fn contiguous_physical_start(base: *const u8, len: usize) -> Result<u64, &'static str> {
    if len == 0 || base.is_null() || base.addr() % PAGE_SIZE != 0 || len % PAGE_SIZE != 0 {
        return Err("DMA range must contain whole aligned pages");
    }
    let first = crate::paging::translate_kernel_address(
        VirtAddr::try_new(base.addr() as u64).map_err(|_| "invalid DMA virtual address")?,
    )
    .ok_or("DMA page is not mapped")?
    .as_u64();
    if first % PAGE_SIZE as u64 != 0 {
        return Err("DMA physical address is not page aligned");
    }
    for offset in (PAGE_SIZE..len).step_by(PAGE_SIZE) {
        let virtual_address = (base.addr() as u64)
            .checked_add(offset as u64)
            .ok_or("DMA virtual range overflow")?;
        let physical = crate::paging::translate_kernel_address(
            VirtAddr::try_new(virtual_address).map_err(|_| "invalid DMA virtual address")?,
        )
        .ok_or("DMA page is not mapped")?
        .as_u64();
        if physical != first + offset as u64 {
            return Err("DMA pages are not physically contiguous");
        }
    }
    Ok(first)
}

/// Resolve a buffer which fits wholly within one page.  This is sufficient
/// for the fixed Phase 7 network and block packet buffers and avoids exposing
/// scatter/gather assumptions to callers.
pub fn single_page_physical(base: *const u8, len: usize) -> Result<u64, &'static str> {
    if base.is_null() || len == 0 {
        return Err("empty DMA buffer");
    }
    let offset = base.addr() & (PAGE_SIZE - 1);
    if offset.checked_add(len).ok_or("DMA buffer overflow")? > PAGE_SIZE {
        return Err("DMA buffer crosses a page");
    }
    Ok(crate::paging::translate_kernel_address(
        VirtAddr::try_new(base.addr() as u64).map_err(|_| "invalid DMA virtual address")?,
    )
    .ok_or("DMA buffer is not mapped")?
    .as_u64())
}
