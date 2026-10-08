//! Erasing secret bytes so the compiler cannot delete the erasure.
//!
//! A password, key, or capability token that has been used is still in
//! memory afterwards, and that memory outlives the value: a stack buffer is
//! reused by the next call frame, a heap block is handed to the next
//! allocation. Overwriting it is the only thing that ends its lifetime as a
//! secret.
//!
//! A plain `buf.fill(0)` does not do that. Nothing reads the buffer
//! afterwards, so the write is dead by the language's own rules and an
//! optimiser is entitled to remove it — which is exactly what a release
//! build does. [`wipe`] writes through [`write_volatile`](core::ptr::write_volatile)
//! instead, which the compiler must emit, and fences afterwards so the
//! stores are not sunk past the point the caller believes the secret is
//! gone.
//!
//! [`Wiped`] applies the same erasure to a fixed-size buffer at the end of
//! its scope, including on an early return or an unwind, so a caller cannot
//! grow a new exit path that forgets to erase; [`WipedBuf`] does it for a
//! heap buffer sized at run time.

use alloc::vec::Vec;
use core::mem::MaybeUninit;
use core::ops::{Deref, DerefMut};
use core::ptr;
use core::sync::atomic::{compiler_fence, Ordering};

/// Overwrite every byte of `bytes` with zero, defeating dead-store
/// elimination.
///
/// Call this on any buffer that held a password, key, or capability token
/// before it goes out of scope or is reused. The write is volatile, so it
/// survives optimisation, and a fence after it stops the stores being
/// reordered past subsequent code.
///
/// This erases the bytes at the address given, and only those. A `String`
/// or `Vec` that reallocated while it held the secret left a copy in the
/// freed block that no later wipe can reach — size such a buffer once, up
/// front, so it never grows.
///
/// ```
/// let mut password = *b"correct horse";
/// tairix_util::secret::wipe(&mut password);
/// assert_eq!(password, [0u8; 13]);
/// ```
pub fn wipe(bytes: &mut [u8]) {
    // SAFETY: every bit pattern is a valid `usize`, and `align_to_mut` hands
    // only the aligned middle back as words, so viewing it as words and
    // writing zero words through it is sound. A buffer of a kernel stack's
    // size then costs a word store per word rather than one per byte.
    let (head, words, tail) = unsafe { bytes.align_to_mut::<usize>() };
    wipe_with(head, 0);
    wipe_with(words, 0);
    wipe_with(tail, 0);
}

/// Overwrite every element of `values` with `blank`, defeating dead-store
/// elimination: [`wipe`] for a buffer of something other than bytes, such as
/// a cache entry's pixels or measurements erased before its allocation is
/// freed.
///
/// Only the bytes `blank` itself initialises are erased, so it must cover a
/// whole `T`: an enum's widest variant, a type with no padding. A narrower
/// blank writes its uninitialised bytes from nothing, and what an element held
/// there may survive.
///
/// ```
/// let mut advances = [7u32, 9, 11];
/// tairix_util::secret::wipe_with(&mut advances, 0);
/// assert_eq!(advances, [0; 3]);
/// ```
pub fn wipe_with<T: Copy>(values: &mut [T], blank: T) {
    for value in values.iter_mut() {
        // SAFETY: `value` is a live, exclusively-borrowed, aligned `T` for
        // the duration of the write, and a `Copy` type has no drop glue, so
        // writing a `T` through it is in-bounds and forgets nothing it
        // overwrites. Volatility is what is wanted here rather than what
        // makes it sound: it forbids the compiler from eliding a store
        // nothing reads back.
        unsafe { ptr::write_volatile(value, blank) };
    }
    compiler_fence(Ordering::SeqCst);
}

/// A fixed-size byte buffer that erases itself when it goes out of scope.
///
/// Use it wherever a secret is marshalled through a buffer: encoding a
/// password into a request, reading one out of a reply. Every exit from the
/// scope erases the bytes — the value returned, the `?` that returned early,
/// the panic that unwound — so no future edit can add a path that leaks the
/// contents by forgetting to clean up.
///
/// The buffer derefs to `[u8; N]`, so it is used exactly like the array it
/// wraps.
///
/// ```
/// use tairix_util::secret::Wiped;
///
/// let mut buf = Wiped::<8>::new();
/// buf[..6].copy_from_slice(b"secret");
/// assert_eq!(&buf[..6], b"secret");
/// // Dropping `buf` here overwrites all eight bytes.
/// ```
#[derive(Debug)]
pub struct Wiped<const N: usize>([u8; N]);

impl<const N: usize> Wiped<N> {
    /// A zeroed buffer.
    #[must_use]
    pub const fn new() -> Self {
        Self([0; N])
    }

    /// Erase the buffer now, rather than waiting for the end of the scope.
    ///
    /// Dropping it erases it again; erasing twice costs one pass and is
    /// never wrong.
    pub fn wipe(&mut self) {
        wipe(&mut self.0);
    }
}

impl<const N: usize> Default for Wiped<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> Deref for Wiped<N> {
    type Target = [u8; N];

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<const N: usize> DerefMut for Wiped<N> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl<const N: usize> Drop for Wiped<N> {
    fn drop(&mut self) {
        self.wipe();
    }
}

/// A heap buffer erased with [`wipe`] at the end of its scope, including on
/// an early return or an unwind: [`Wiped`] for a buffer sized at run time.
///
/// It never grows past the block it was made with: a buffer that moved to a
/// larger block would leave what it held in the one it outgrew, past any
/// wipe's reach. The whole block is erased, spare capacity too, since a
/// vector handed in truncated still holds what it was truncated from.
#[derive(Debug)]
pub struct WipedBuf(Vec<u8>);

impl WipedBuf {
    /// `bytes`, to be erased when dropped.
    #[must_use]
    pub const fn new(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }

    /// An empty buffer with a block of `capacity` bytes, filled as it is used
    /// by [`Self::extend_zeroed`] rather than zeroed whole up front.
    ///
    /// # Errors
    ///
    /// [`TryReserveError`](alloc::collections::TryReserveError) when the
    /// block cannot be allocated.
    pub fn with_capacity(capacity: usize) -> Result<Self, alloc::collections::TryReserveError> {
        let mut bytes = Vec::new();
        bytes.try_reserve_exact(capacity)?;
        Ok(Self(bytes))
    }

    /// Grow by `additional` zeroed bytes and hand them out, or `None`, leaving
    /// the buffer as it was, when its block cannot hold them.
    pub fn extend_zeroed(&mut self, additional: usize) -> Option<&mut [u8]> {
        let held = self.0.len();
        let len = held
            .checked_add(additional)
            .filter(|&len| len <= self.0.capacity())?;
        self.0.resize(len, 0);
        self.0.get_mut(held..)
    }

    /// Erase the buffer now, rather than waiting for the end of the scope.
    pub fn wipe(&mut self) {
        wipe(&mut self.0);
        wipe_with(self.0.spare_capacity_mut(), MaybeUninit::new(0));
    }
}

impl Deref for WipedBuf {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        &self.0
    }
}

impl DerefMut for WipedBuf {
    fn deref_mut(&mut self) -> &mut [u8] {
        &mut self.0
    }
}

impl Drop for WipedBuf {
    fn drop(&mut self) {
        self.wipe();
    }
}

#[cfg(test)]
mod tests {
    use super::{wipe, wipe_with, Wiped, WipedBuf};

    #[test]
    fn a_wiped_buffer_reads_as_its_bytes_until_it_is_erased() {
        let mut buf = WipedBuf::new(alloc::vec![0x5Au8; 37]);
        buf[3..9].copy_from_slice(b"secret");
        assert_eq!(&buf[3..9], b"secret");
        assert_eq!(buf.len(), 37);
        buf.wipe();
        assert!(buf.iter().all(|&byte| byte == 0));
    }

    /// What a vector was truncated from is still in its block, so it is
    /// erased with the bytes the buffer shows.
    #[test]
    fn a_wiped_buffer_erases_what_its_vector_was_truncated_from() {
        let mut held = alloc::vec![0xA5u8; 32];
        held.truncate(4);
        let mut buf = WipedBuf::new(held);
        buf.wipe();
        let spare = buf.0.spare_capacity_mut();
        assert!(spare.len() >= 28);
        // SAFETY: the wipe just wrote every spare byte, so each is
        // initialised, and the borrow is the buffer's own.
        assert!(spare.iter().all(|byte| unsafe { byte.assume_init() } == 0));
    }

    /// Growth stays inside the block the buffer was made with, so it never
    /// moves and leaves a copy behind; past it, nothing changes.
    #[test]
    fn a_wiped_buffer_grows_only_within_its_block() {
        let mut buf = WipedBuf::with_capacity(10).expect("a small block");
        assert!(buf.is_empty());
        let block = buf.0.as_ptr();
        buf.extend_zeroed(4).expect("fits").copy_from_slice(b"abcd");
        assert_eq!(buf.extend_zeroed(6).expect("fills the block"), [0; 6]);
        assert_eq!(buf.extend_zeroed(1), None);
        assert_eq!(buf.extend_zeroed(usize::MAX), None);
        assert_eq!(&buf[..4], b"abcd");
        assert_eq!(buf.len(), 10);
        assert_eq!(buf.0.as_ptr(), block);
    }

    #[test]
    fn wipe_with_blanks_every_element_of_any_plain_type() {
        let mut advances = [u32::MAX; 9];
        wipe_with(&mut advances, 0);
        assert_eq!(advances, [0; 9]);
        let mut points = [(1.5f64, -2.5f64); 3];
        wipe_with(&mut points, (0.0, 0.0));
        assert_eq!(points, [(0.0, 0.0); 3]);
    }

    #[test]
    fn wipe_with_touches_only_the_slice_it_was_given() {
        let mut buf = [7u16; 6];
        wipe_with(&mut buf[1..3], 0);
        assert_eq!(buf, [7, 0, 0, 7, 7, 7]);
        let mut empty: [u64; 0] = [];
        wipe_with(&mut empty, 0);
    }

    #[test]
    fn wipe_zeroes_every_byte() {
        let mut buf = [0xAAu8; 64];
        wipe(&mut buf);
        assert_eq!(buf, [0u8; 64]);
    }

    /// The word-wide middle and the byte-wide ends between them cover every
    /// byte, whatever the slice's alignment and length, and nothing outside it.
    #[test]
    fn wipe_covers_a_misaligned_slice_exactly() {
        for start in 0..9 {
            for len in 0..41 {
                let mut buf = [0xAAu8; 64];
                wipe(&mut buf[start..start + len]);
                for (at, byte) in buf.iter().enumerate() {
                    let inside = (start..start + len).contains(&at);
                    assert_eq!(
                        *byte,
                        if inside { 0 } else { 0xAA },
                        "{start}+{len} at {at}"
                    );
                }
            }
        }
    }

    #[test]
    fn wipe_of_an_empty_slice_is_harmless() {
        let mut empty: [u8; 0] = [];
        wipe(&mut empty);
    }

    #[test]
    fn wipe_touches_only_the_slice_it_was_given() {
        let mut buf = [0xFFu8; 8];
        wipe(&mut buf[2..5]);
        assert_eq!(buf, [0xFF, 0xFF, 0, 0, 0, 0xFF, 0xFF, 0xFF]);
    }

    #[test]
    fn a_wiped_buffer_starts_zeroed_and_reads_back_what_was_written() {
        let mut buf = Wiped::<16>::new();
        assert_eq!(*buf, [0u8; 16]);
        buf[..6].copy_from_slice(b"secret");
        assert_eq!(&buf[..6], b"secret");
        assert_eq!(buf.len(), 16);
    }

    #[test]
    fn wiping_early_clears_the_buffer_in_place() {
        let mut buf = Wiped::<16>::new();
        buf[..6].copy_from_slice(b"secret");
        buf.wipe();
        assert_eq!(*buf, [0u8; 16]);
    }

    /// Going out of scope must erase the bytes, not merely release them.
    #[test]
    fn dropping_a_wiped_buffer_erases_it() {
        let mut buf = core::mem::ManuallyDrop::new(Wiped::<16>::new());
        buf[..6].copy_from_slice(b"secret");
        let slot = core::ptr::addr_of_mut!(buf);

        // SAFETY: `buf` is a `ManuallyDrop`, so its destructor has not run
        // and its storage belongs to this frame for the whole test. Running
        // that destructor by hand leaves the storage in place — the buffer
        // owns nothing but plain bytes — so reading it back afterwards
        // observes exactly what the destructor wrote into it. The drop and
        // the read are both reborrowed from `slot`, so the drop's exclusive
        // borrow cannot invalidate the pointer the read goes through.
        unsafe {
            core::mem::ManuallyDrop::drop(&mut *slot);
            let erased = &*slot;
            assert_eq!(erased.0, [0u8; 16], "the destructor erased the secret");
        }
    }
}
