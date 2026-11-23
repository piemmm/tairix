//! [`MockRegion`], a device's shared-memory region in memory of its own, for
//! tests of drivers that map parts of one. Built only for tests, behind the
//! crate's `mock` feature.

use alloc::boxed::Box;
use alloc::vec;
use core::ptr::NonNull;
use core::sync::atomic::{AtomicUsize, Ordering};

use tairix_abi::driver::{MemoryMapper, MemoryWindow, RegionCaching};
use tairix_abi::hwtree::HwResource;
use tairix_abi::DriverError;

/// A shared-memory region standing in for a device's: every part it maps is
/// a window onto its own memory, so what a driver writes through one window
/// the device side reads, and another window onto the same bytes sees.
///
/// Its bytes are reached only through raw copies — the windows' and its own
/// [`read`](Self::read) and [`write`](Self::write) — so no reference to them
/// outlives a copy. A region dropped with a window still mapped keeps its
/// memory rather than leave the window dangling.
pub struct MockRegion {
    base: NonNull<u8>,
    len: usize,
    mapped: AtomicUsize,
}

// SAFETY: the region owns its allocation and reaches it only through raw
// copies; a test orders the device side's copies and the driver's window
// copies of one byte with its own locks, as a device and a driver order
// theirs with the queue.
unsafe impl Send for MockRegion {}
// SAFETY: as for `Send`; every method takes `&self` and copies raw.
unsafe impl Sync for MockRegion {}

impl MockRegion {
    /// A region of `len` zeroed bytes.
    #[must_use]
    pub fn new(len: usize) -> Self {
        let memory: Box<[u8]> = vec![0; len].into_boxed_slice();
        let base = NonNull::new(Box::into_raw(memory).cast::<u8>()).unwrap_or(NonNull::dangling());
        Self {
            base,
            len,
            mapped: AtomicUsize::new(0),
        }
    }

    /// Its bytes.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Whether it holds none.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// How many windows onto it are mapped.
    #[must_use]
    pub fn mapped(&self) -> usize {
        self.mapped.load(Ordering::SeqCst)
    }

    fn span(&self, at: usize, len: usize) -> Result<NonNull<u8>, DriverError> {
        let end = at.checked_add(len).ok_or(DriverError::OutOfRange)?;
        if end > self.len {
            return Err(DriverError::OutOfRange);
        }
        // SAFETY: `at + len` was proved within the allocation, so the offset
        // pointer stays inside it.
        Ok(unsafe { self.base.add(at) })
    }

    /// Copy `out.len()` bytes at `at` out of it, as the device reads them.
    ///
    /// # Errors
    ///
    /// [`DriverError::OutOfRange`] for bytes past it.
    pub fn read(&self, at: usize, out: &mut [u8]) -> Result<(), DriverError> {
        let from = self.span(at, out.len())?;
        // SAFETY: `span` proved the source within the allocation, `out` is a
        // distinct exclusive borrow, and the region is reached only by raw
        // copies, so no reference aliases the source.
        unsafe { core::ptr::copy_nonoverlapping(from.as_ptr(), out.as_mut_ptr(), out.len()) };
        Ok(())
    }

    /// Copy `data` into it at `at`, as the device writes it.
    ///
    /// # Errors
    ///
    /// [`DriverError::OutOfRange`] for bytes past it.
    pub fn write(&self, at: usize, data: &[u8]) -> Result<(), DriverError> {
        let into = self.span(at, data.len())?;
        // SAFETY: as in `read`, with the roles exchanged.
        unsafe { core::ptr::copy_nonoverlapping(data.as_ptr(), into.as_ptr(), data.len()) };
        Ok(())
    }
}

impl MemoryMapper for MockRegion {
    fn map_memory(
        &self,
        _region: &HwResource,
        offset: u64,
        len: usize,
        _caching: RegionCaching,
    ) -> Result<MemoryWindow, DriverError> {
        if len == 0 {
            return Err(DriverError::LengthOutOfRange);
        }
        let at = usize::try_from(offset).map_err(|_| DriverError::OutOfRange)?;
        let base = self.span(at, len)?;
        self.mapped.fetch_add(1, Ordering::SeqCst);
        // SAFETY: `span` proved `[offset, offset + len)` within the
        // allocation, which outlives every window: a region dropped with a
        // window mapped keeps its memory.
        Ok(unsafe { MemoryWindow::from_mapping(offset, base, len) })
    }

    fn unmap_memory(&self, _window: MemoryWindow) -> Result<(), (MemoryWindow, DriverError)> {
        self.mapped.fetch_sub(1, Ordering::SeqCst);
        Ok(())
    }
}

impl Drop for MockRegion {
    fn drop(&mut self) {
        if *self.mapped.get_mut() != 0 {
            return;
        }
        let slice = core::ptr::slice_from_raw_parts_mut(self.base.as_ptr(), self.len);
        // SAFETY: `base` and `len` are the allocation `new` leaked into the
        // region, no window onto it is mapped, and it is freed once.
        drop(unsafe { Box::from_raw(slice) });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn whole() -> HwResource {
        HwResource::memory_region(0x1_0000_0000, 8192, 1)
    }

    #[test]
    fn a_window_and_the_device_side_reach_the_same_bytes() {
        let region = MockRegion::new(8192);
        let window = region
            .map_memory(&whole(), 4096, 4096, RegionCaching::WriteCombine)
            .expect("mapped");
        assert_eq!(region.mapped(), 1);
        window.write(8, &[1, 2, 3]).expect("written");
        let mut seen = [0u8; 3];
        region.read(4096 + 8, &mut seen).expect("read");
        assert_eq!(seen, [1, 2, 3]);
        region.write(4096, &[9]).expect("written");
        let mut through = [0u8; 1];
        window.read(0, &mut through).expect("read");
        assert_eq!(through, [9]);
        assert!(region.unmap_memory(window).is_ok());
        assert_eq!(region.mapped(), 0);
    }

    #[test]
    fn a_part_past_the_region_or_of_no_bytes_is_refused() {
        let region = MockRegion::new(4096);
        assert_eq!(
            region
                .map_memory(&whole(), 4096, 1, RegionCaching::WriteCombine)
                .err(),
            Some(DriverError::OutOfRange)
        );
        assert_eq!(
            region
                .map_memory(&whole(), 0, 0, RegionCaching::WriteCombine)
                .err(),
            Some(DriverError::LengthOutOfRange)
        );
        assert_eq!(region.read(4095, &mut [0; 2]), Err(DriverError::OutOfRange));
        assert_eq!(region.mapped(), 0);
    }
}
