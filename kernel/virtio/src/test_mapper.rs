//! The mapper the walks' tests provision through: each window lies over
//! storage of its own, which the mapper frees when it drops.

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::cell::RefCell;
use core::ptr::NonNull;

use tairix_abi::driver::mmio::MmioMapError;
use tairix_abi::{MmioMapper, RegisterWindow};

/// Hands out a window over fresh, aligned storage for each request, recording
/// the `(phys, len)` asked for; a test drops every window before the mapper.
pub(crate) struct RecordingMapper {
    grant: bool,
    pub(crate) requests: RefCell<Vec<(u64, usize)>>,
    backings: RefCell<Vec<NonNull<[u64]>>>,
}

impl RecordingMapper {
    pub(crate) fn new(grant: bool) -> Self {
        Self {
            grant,
            requests: RefCell::new(Vec::new()),
            backings: RefCell::new(Vec::new()),
        }
    }
}

impl MmioMapper for RecordingMapper {
    fn map_window(&self, phys_base: u64, len: usize) -> Result<RegisterWindow, MmioMapError> {
        if !self.grant {
            return Err(MmioMapError::CapabilityMissing);
        }
        self.requests.borrow_mut().push((phys_base, len));
        let words = len.div_ceil(8).max(1);
        let backing = NonNull::from(Box::leak(alloc::vec![0u64; words].into_boxed_slice()));
        self.backings.borrow_mut().push(backing);
        // SAFETY: the backing covers at least `len` bytes, is reached through
        // this window alone, and lives until the mapper drops, which the
        // test's windows precede.
        Ok(unsafe { RegisterWindow::from_mapping(phys_base, backing.cast::<u8>(), len) })
    }
}

impl Drop for RecordingMapper {
    fn drop(&mut self) {
        for backing in self.backings.get_mut().drain(..) {
            // SAFETY: each backing is a box `map_window` leaked, and no window
            // over it outlives the mapper.
            drop(unsafe { Box::from_raw(backing.as_ptr()) });
        }
    }
}
