//! Immutable slices and strings shared between holders, and shared values,
//! every one allocated fallibly.
//!
//! `alloc` makes an `Arc<[T]>` or an `Arc<str>` only through an allocation
//! that aborts when the heap refuses it, and its fallible `Arc` constructor
//! is unstable. A [`SharedSlice`] is one block — a count of its holders, a
//! length, and the elements after them — behind a thin pointer, made only by
//! constructors that answer a refusal as a value and freed by the last holder
//! to let it go; the empty one allocates nothing. [`SharedStr`] is the same
//! for text, and [`try_arc`] shares any value so.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::alloc::Layout;
use core::borrow::Borrow;
use core::cmp::Ordering as Order;
use core::fmt;
use core::hash::{Hash, Hasher};
use core::marker::PhantomData;
use core::ops::Deref;
use core::ptr::{self, NonNull};

use crate::loom_compat::{fence, AtomicUsize, Ordering};
use crate::TryReserveError;

/// `value` shared behind an `Arc`, or a refusal where the heap cannot hold
/// it: the one place the unstable fallible constructor is reached, so the
/// crates sharing values need not reach for it themselves.
///
/// # Errors
///
/// [`TryReserveError::AllocFailed`] when the heap refuses the block.
pub fn try_arc<T>(value: T) -> Result<Arc<T>, TryReserveError> {
    Arc::try_new(value).map_err(|_| TryReserveError::AllocFailed)
}

/// `value` behind a `Box`, or a refusal where the heap cannot hold it, as
/// [`try_arc`] shares one.
///
/// # Errors
///
/// [`TryReserveError::AllocFailed`] when the heap refuses the block.
pub fn try_box<T>(value: T) -> Result<Box<T>, TryReserveError> {
    Box::try_new(value).map_err(|_| TryReserveError::AllocFailed)
}

/// What a block begins with: how many hold it, and how many elements follow.
#[repr(C)]
struct Header {
    holders: AtomicUsize,
    len: usize,
}

/// The count from which a block is never freed. Holders past it could only
/// come of handles leaked by the billion, and a count let wrap would free a
/// block still held, so the count saturates instead, as the kernel's
/// reference counts conventionally do.
const SATURATED: usize = usize::MAX >> 1;

/// Where a saturated count is put back to: far from both the ceiling and
/// zero, so no run of holders coming and going can leave the saturated range.
const PINNED: usize = SATURATED + (SATURATED >> 1);

/// An immutable slice that clones by sharing: one allocation, freed by the
/// last of its holders.
pub struct SharedSlice<T> {
    /// `None` for the empty slice, which holds no block.
    block: Option<NonNull<Header>>,
    marker: PhantomData<T>,
}

// SAFETY: a block is shared by its holders as `Arc` shares its value — read
// through `&T` from every thread, and dropped on whichever thread lets go
// last — so a slice moves and is shared between threads exactly when its
// elements may be both.
unsafe impl<T: Send + Sync> Send for SharedSlice<T> {}
// SAFETY: as for `Send`.
unsafe impl<T: Send + Sync> Sync for SharedSlice<T> {}

impl<T> SharedSlice<T> {
    /// Where a block's elements begin: its header, padded to theirs.
    const ELEMENTS: usize = size_of::<Header>().div_ceil(align_of::<T>()) * align_of::<T>();

    /// The empty slice, which allocates nothing.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            block: None,
            marker: PhantomData,
        }
    }

    /// `items` cloned into a slice of their own.
    ///
    /// # Errors
    ///
    /// [`TryReserveError`] when the heap refuses the block or its size has no
    /// layout.
    pub fn try_from_slice(items: &[T]) -> Result<Self, TryReserveError>
    where
        T: Clone,
    {
        if items.is_empty() {
            return Ok(Self::new());
        }
        let mut filling = Filling::<T> {
            block: Self::allocate(items.len())?,
            written: 0,
            marker: PhantomData,
        };
        for item in items {
            let copy = item.clone();
            // SAFETY: the block was laid out for `items.len()` elements, of
            // which the first `written` are written, so this slot is in the
            // block and holds nothing yet.
            unsafe {
                Self::elements(filling.block)
                    .add(filling.written)
                    .write(copy);
            }
            filling.written += 1;
        }
        let block = filling.block;
        core::mem::forget(filling);
        Ok(Self {
            block: Some(block),
            marker: PhantomData,
        })
    }

    /// `items` moved into a slice of their own, the vector's buffer given
    /// back.
    ///
    /// # Errors
    ///
    /// [`TryReserveError`] when the heap refuses the block or its size has no
    /// layout; the items are dropped with the vector.
    pub fn try_from_vec(mut items: Vec<T>) -> Result<Self, TryReserveError> {
        if items.is_empty() {
            return Ok(Self::new());
        }
        let len = items.len();
        let block = Self::allocate(len)?;
        // SAFETY: the block was laid out for `len` elements and holds none
        // yet, and the vector's first `len` are initialised and lie in its
        // own buffer, apart from a block just allocated. Setting the
        // vector's length to zero hands the elements to the block, so the
        // vector gives back its buffer without dropping them.
        unsafe {
            ptr::copy_nonoverlapping(items.as_ptr(), Self::elements(block), len);
            items.set_len(0);
        }
        Ok(Self {
            block: Some(block),
            marker: PhantomData,
        })
    }

    /// The elements.
    #[must_use]
    pub fn as_slice(&self) -> &[T] {
        match self.block {
            None => &[],
            // SAFETY: a block lives while a handle holds it; its header was
            // written as it was made and its length never changes; its
            // elements were all written before any handle reached it, and
            // nothing writes them while it is shared.
            Some(block) => unsafe {
                core::slice::from_raw_parts(Self::elements(block), block.as_ref().len)
            },
        }
    }

    /// How many elements it holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.as_slice().len()
    }

    /// Whether it holds none.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.block.is_none()
    }

    /// Whether `a` and `b` share one block, or are both empty: whether they
    /// are the very same slice, without comparing what they hold.
    #[must_use]
    pub fn ptr_eq(a: &Self, b: &Self) -> bool {
        a.block == b.block
    }

    /// The layout of a block of `len` elements.
    fn layout(len: usize) -> Result<Layout, TryReserveError> {
        let elements = Layout::array::<T>(len).map_err(|_| TryReserveError::CapacityOverflow)?;
        let (layout, _) = Layout::new::<Header>()
            .extend(elements)
            .map_err(|_| TryReserveError::CapacityOverflow)?;
        Ok(layout.pad_to_align())
    }

    /// A block for `len` elements, held once and its elements unwritten.
    fn allocate(len: usize) -> Result<NonNull<Header>, TryReserveError> {
        let layout = Self::layout(len)?;
        // SAFETY: the layout begins with the header, so it is never empty.
        let raw = unsafe { alloc::alloc::alloc(layout) };
        // The allocator aligns the block to its layout, which is the header's.
        let block = NonNull::new(raw)
            .ok_or(TryReserveError::AllocFailed)?
            .cast::<Header>();
        // SAFETY: the block is fresh, as large as its layout and aligned to
        // it, which begins with a header.
        unsafe {
            block.as_ptr().write(Header {
                holders: AtomicUsize::new(1),
                len,
            });
        }
        Ok(block)
    }

    /// Where `block`'s elements begin.
    fn elements(block: NonNull<Header>) -> *mut T {
        // SAFETY: every block is laid out with its elements `ELEMENTS` bytes
        // past its start, within the one allocation, so the offset stays in
        // bounds of it, or one past its end when the elements take no room.
        unsafe { block.as_ptr().cast::<u8>().add(Self::ELEMENTS).cast::<T>() }
    }

    /// `block`'s header dropped and its memory given back, its elements
    /// already dropped or never written.
    ///
    /// # Safety
    ///
    /// No handle may reach `block` again, and it must be one [`allocate`]
    /// made.
    ///
    /// [`allocate`]: Self::allocate
    unsafe fn release(block: NonNull<Header>) {
        // SAFETY: the caller hands over the block, whose header was written
        // as it was made.
        let len = unsafe { block.as_ref().len };
        // SAFETY: as above; nothing reads the header after this.
        unsafe { ptr::drop_in_place(block.as_ptr()) };
        // The layout was made once for this length, so it is made again.
        if let Ok(layout) = Self::layout(len) {
            // SAFETY: the block was allocated with this very layout.
            unsafe { alloc::alloc::dealloc(block.as_ptr().cast::<u8>(), layout) };
        }
    }
}

/// A block being filled: should a clone panic before it is full, the
/// elements written so far are dropped and the block given back.
struct Filling<T> {
    block: NonNull<Header>,
    written: usize,
    marker: PhantomData<T>,
}

impl<T> Drop for Filling<T> {
    fn drop(&mut self) {
        let written =
            ptr::slice_from_raw_parts_mut(SharedSlice::<T>::elements(self.block), self.written);
        // SAFETY: the first `written` slots were written and nothing else
        // reaches a block still being filled.
        unsafe {
            ptr::drop_in_place(written);
            SharedSlice::<T>::release(self.block);
        }
    }
}

impl<T> Clone for SharedSlice<T> {
    fn clone(&self) -> Self {
        if let Some(block) = self.block {
            // SAFETY: the block lives while this handle holds it.
            let holders = unsafe { &block.as_ref().holders };
            // A new holder is made from one that keeps the block alive, so
            // the count needs no ordering of its own.
            if holders.fetch_add(1, Ordering::Relaxed) >= SATURATED {
                holders.store(PINNED, Ordering::Relaxed);
            }
        }
        Self {
            block: self.block,
            marker: PhantomData,
        }
    }
}

impl<T> Drop for SharedSlice<T> {
    fn drop(&mut self) {
        let Some(block) = self.block else {
            return;
        };
        // SAFETY: the block lives while this handle holds it.
        let holders = unsafe { &block.as_ref().holders };
        // Release: what this holder did with the elements happens before the
        // last holder drops them.
        let before = holders.fetch_sub(1, Ordering::Release);
        if before != 1 {
            if before > SATURATED {
                holders.store(PINNED, Ordering::Relaxed);
            }
            return;
        }
        // Acquire: the last holder sees every other holder's use of the
        // elements before it drops them.
        fence(Ordering::Acquire);
        // SAFETY: this was the block's last holder, so nothing reaches it
        // again; its elements were all written as it was made.
        unsafe {
            let len = block.as_ref().len;
            ptr::drop_in_place(ptr::slice_from_raw_parts_mut(Self::elements(block), len));
            Self::release(block);
        }
    }
}

impl<T> Default for SharedSlice<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> Deref for SharedSlice<T> {
    type Target = [T];

    fn deref(&self) -> &[T] {
        self.as_slice()
    }
}

impl<T> AsRef<[T]> for SharedSlice<T> {
    fn as_ref(&self) -> &[T] {
        self.as_slice()
    }
}

impl<T> Borrow<[T]> for SharedSlice<T> {
    fn borrow(&self) -> &[T] {
        self.as_slice()
    }
}

impl<T: fmt::Debug> fmt::Debug for SharedSlice<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self.as_slice(), f)
    }
}

// Compared by what they hold, never by their blocks alone: an element that
// is not equal to itself, as a NaN is, keeps the slices holding it unequal.
impl<T: PartialEq> PartialEq for SharedSlice<T> {
    fn eq(&self, other: &Self) -> bool {
        self.as_slice() == other.as_slice()
    }
}

impl<T: Eq> Eq for SharedSlice<T> {}

impl<T: PartialEq> PartialEq<[T]> for SharedSlice<T> {
    fn eq(&self, other: &[T]) -> bool {
        self.as_slice() == other
    }
}

impl<T: PartialOrd> PartialOrd for SharedSlice<T> {
    fn partial_cmp(&self, other: &Self) -> Option<Order> {
        self.as_slice().partial_cmp(other.as_slice())
    }
}

impl<T: Ord> Ord for SharedSlice<T> {
    fn cmp(&self, other: &Self) -> Order {
        self.as_slice().cmp(other.as_slice())
    }
}

// Hashed as the slice it borrows as, so a map keyed on either finds the
// other.
impl<T: Hash> Hash for SharedSlice<T> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.as_slice().hash(state);
    }
}

/// Immutable text that clones by sharing: one allocation, freed by the last
/// of its holders.
#[derive(Clone, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct SharedStr(SharedSlice<u8>);

impl SharedStr {
    /// The empty text, which allocates nothing.
    #[must_use]
    pub const fn new() -> Self {
        Self(SharedSlice::new())
    }

    /// `text` copied into a block of its own.
    ///
    /// # Errors
    ///
    /// [`TryReserveError`] when the heap refuses the block.
    pub fn try_from_str(text: &str) -> Result<Self, TryReserveError> {
        SharedSlice::try_from_slice(text.as_bytes()).map(Self)
    }

    /// `text` moved into a block of its own, the string's buffer given back.
    ///
    /// # Errors
    ///
    /// [`TryReserveError`] when the heap refuses the block.
    pub fn try_from_string(text: String) -> Result<Self, TryReserveError> {
        SharedSlice::try_from_vec(text.into_bytes()).map(Self)
    }

    /// The text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        // SAFETY: a block of text is made only from a `str` or a `String`,
        // whose bytes are UTF-8, and is never written after.
        unsafe { core::str::from_utf8_unchecked(self.0.as_slice()) }
    }

    /// Its length in bytes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether it is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Whether `a` and `b` share one block, or are both empty.
    #[must_use]
    pub fn ptr_eq(a: &Self, b: &Self) -> bool {
        SharedSlice::ptr_eq(&a.0, &b.0)
    }
}

impl Deref for SharedStr {
    type Target = str;

    fn deref(&self) -> &str {
        self.as_str()
    }
}

impl AsRef<str> for SharedStr {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl Borrow<str> for SharedStr {
    fn borrow(&self) -> &str {
        self.as_str()
    }
}

impl PartialEq<str> for SharedStr {
    fn eq(&self, other: &str) -> bool {
        self.as_str() == other
    }
}

impl PartialEq<&str> for SharedStr {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

// Hashed as the `str` it borrows as.
impl Hash for SharedStr {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.as_str().hash(state);
    }
}

impl fmt::Debug for SharedStr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self.as_str(), f)
    }
}

impl fmt::Display for SharedStr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
#[path = "shared_tests.rs"]
mod tests;
