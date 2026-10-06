//! Device registers the kernel reaches itself: through the direct map, over
//! which the firmware's MTRRs make every MMIO hole uncached.

use core::ptr::NonNull;

use tairix_abi::driver::{MmioMapError, MmioMapper, RegisterWindow};
use tairix_arch_x86_64::paging;

/// The kernel's mapping of the device registers at `[base, base + len)`, or
/// [`None`] for a window the live direct map does not wholly hold.
#[must_use]
pub fn device_registers(base: u64, len: u64) -> Option<NonNull<u8>> {
    let end = base.checked_add(len)?;
    if base == 0 || len == 0 || end > paging::physmap_bytes() {
        return None;
    }
    NonNull::new(usize::try_from(paging::physmap_virt(base)).ok()? as *mut u8)
}

/// Windows over device registers the kernel programs itself, on no
/// process's behalf: the MSI-X table entries it routes.
pub struct KernelRegisters;

impl MmioMapper for KernelRegisters {
    fn map_window(&self, phys_base: u64, len: usize) -> Result<RegisterWindow, MmioMapError> {
        let span = u64::try_from(len).map_err(|_| MmioMapError::InvalidRegion)?;
        let base = device_registers(phys_base, span).ok_or(MmioMapError::InvalidRegion)?;
        // SAFETY: `device_registers` proved `[phys_base, phys_base + len)`
        // lies wholly within the direct map, which is never torn down and
        // maps device memory uncached, so `base` is valid for `len` bytes for
        // the kernel's life; the window stays with the kernel.
        Ok(unsafe { RegisterWindow::from_mapping(phys_base, base, len) })
    }
}
