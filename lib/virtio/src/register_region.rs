//! A device's register block for the transport tests: word-aligned storage
//! the region owns and frees once it is dropped, after every window built
//! over it.

use alloc::boxed::Box;
use core::ptr::NonNull;
use tairix_abi::RegisterWindow;

/// One register region. It is reached only through the volatile
/// [`RegisterWindow`] accessors, so the transport's window and the test's
/// own alias the same bytes exactly as they would a real device's.
pub(crate) struct Region {
    words: NonNull<[u64]>,
    len: usize,
}

impl Region {
    /// A zeroed region of `len` bytes.
    pub(crate) fn new(len: usize) -> Self {
        let words = len.div_ceil(8).max(1);
        let words = NonNull::from(Box::leak(alloc::vec![0u64; words].into_boxed_slice()));
        Self { words, len }
    }

    /// The region's length in bytes.
    pub(crate) fn len(&self) -> usize {
        self.len
    }

    /// A window over the region at the synthetic physical address `phys`.
    ///
    /// # Safety
    ///
    /// The region outlives the window and everything built over it: the
    /// window carries no borrow of it.
    pub(crate) unsafe fn window(&self, phys: u64) -> RegisterWindow {
        // SAFETY: the words cover `len` bytes the region owns until it is
        // dropped, which the caller places after the window; a window
        // performs only volatile accesses, so aliasing windows are sound for
        // the single-threaded test.
        unsafe { RegisterWindow::from_mapping(phys, self.words.cast::<u8>(), self.len) }
    }
}

impl Drop for Region {
    fn drop(&mut self) {
        // SAFETY: `words` is the slice `Box::leak` gave, freed once, here.
        drop(unsafe { Box::from_raw(self.words.as_ptr()) });
    }
}
