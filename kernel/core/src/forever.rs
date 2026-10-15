//! State kept for the rest of the kernel's life.
//!
//! The boot path builds the kernel's collaborators once and publishes them as
//! `&'static`, and nothing ever frees them. [`forever`] is that allocation.
//!
//! A host test builds the same state once per test, and its fixtures stand in
//! for it the same way. There each allocation is also recorded in a static, so
//! the UB oracle's leak check accounts for everything kept on purpose and an
//! allocation it still reports is one nothing roots.

use alloc::boxed::Box;

/// `boxed`'s contents, for the rest of the kernel's life.
#[must_use]
pub fn forever<T: ?Sized + 'static>(boxed: Box<T>) -> &'static mut T {
    let kept = Box::leak(boxed);
    #[cfg(any(test, feature = "test-arch", feature = "fs-conformance"))]
    host::root(core::ptr::NonNull::from(&mut *kept).cast());
    kept
}

#[cfg(any(test, feature = "test-arch", feature = "fs-conformance"))]
mod host {
    use alloc::vec::Vec;
    use core::ptr::NonNull;

    use tairix_sync::SpinLock;

    /// Where everything kept lives.
    static KEPT: SpinLock<Vec<Kept>> = SpinLock::new(Vec::new());

    /// The address of one kept allocation, which roots it and is never followed.
    struct Kept(
        #[expect(dead_code, reason = "held to root the allocation, never read")] NonNull<u8>,
    );

    // SAFETY: the list only roots the allocation; nothing reads or writes
    // through a kept pointer, so moving one between threads hands over no
    // access.
    unsafe impl Send for Kept {}

    pub(super) fn root(kept: NonNull<u8>) {
        KEPT.lock().push(Kept(kept));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kept_state_stays_where_it_was_put() {
        let first = forever(Box::new([7u8; 3]));
        let second = forever(alloc::vec![1u32, 2, 3].into_boxed_slice());
        first[1] = 9;
        second[2] = 4;
        assert_eq!(*first, [7, 9, 7]);
        assert_eq!(*second, [1, 2, 4]);
    }
}
