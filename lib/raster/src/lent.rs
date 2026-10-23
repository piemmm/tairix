//! Pixel memory a surface draws into without owning it.
//!
//! A [`Surface`](crate::Surface) over lent memory is how a picture is drawn
//! straight into a frame another process reads, with no copy between: the
//! lender keeps the memory alive by handing it to the surface, and gets it
//! back when the surface goes. The one view of bytes as pixels lives here.

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::mem::size_of;
use core::ptr::NonNull;

use crate::color::Pixel;

/// Memory a surface draws into without owning it: a frame mapped from a
/// region another process reads, kept alive by the surface built over it.
pub trait PixelMemory: Send + Sync {
    /// The memory, whose leading bytes are the pixels in `r, g, b, a` order:
    /// asked for once, as the surface is built, and drawn into from then
    /// until the surface lets it go.
    fn bytes_mut(&mut self) -> &mut [u8];
}

/// Where a surface's pixels live.
pub(crate) enum Store {
    /// Its own allocation.
    Owned(Vec<Pixel>),
    /// The first pixels of memory it was lent.
    Lent(Lent),
}

/// The first `count` pixels of memory a surface was lent, found once: the
/// lender is reached by nothing from then until the store drops, so the
/// pixels it answered stay the surface's alone and every borrow of them is a
/// slice, with no call through the lender.
pub(crate) struct Lent {
    pixels: NonNull<Pixel>,
    count: usize,
    /// Held as a pointer, not a box: moving a box asserts it is unique, which
    /// would retire `pixels` wherever a lender keeps its bytes inline.
    lender: NonNull<dyn PixelMemory>,
}

#[allow(unsafe_code)] // The lender is reclaimed only here, as proven below.
impl Drop for Lent {
    fn drop(&mut self) {
        // SAFETY: `lender` came from `Box::leak` in `Store::lent`, is reclaimed
        // nowhere else, and nothing borrows the pixels once the store drops.
        drop(unsafe { Box::from_raw(self.lender.as_ptr()) });
    }
}

// SAFETY: the lender is `Send + Sync` and owned by this store alone, and its
// pixels are reached only through borrows of the store, as a `Vec`'s are.
#[allow(unsafe_code)] // Ownership is the store's alone, as stated above.
unsafe impl Send for Lent {}
// SAFETY: as for `Send`: a shared store hands out only shared pixel slices.
#[allow(unsafe_code)] // As for `Send`.
unsafe impl Sync for Lent {}

/// The most pixels a store's `Debug` lists before it names only their count.
const DEBUG_PIXELS_MAX: usize = 256;

impl Store {
    /// The first `count` pixels of `memory`, or `None` when it holds fewer.
    #[allow(unsafe_code)] // The lender is leaked and reclaimed as proven below.
    pub(crate) fn lent(memory: Box<dyn PixelMemory>, count: usize) -> Option<Self> {
        let len = count.checked_mul(size_of::<Pixel>())?;
        let lender = NonNull::from(Box::leak(memory));
        // SAFETY: `lender` is the box just leaked, live and reached by nothing
        // else; this is the one borrow of it until the store drops.
        let bytes = unsafe { &mut *lender.as_ptr() }.bytes_mut();
        let Some(bytes) = bytes.get_mut(..len) else {
            // SAFETY: as above; the bytes it answered are not used again.
            drop(unsafe { Box::from_raw(lender.as_ptr()) });
            return None;
        };
        let pixels = NonNull::from(pixels_of_mut(bytes)).cast::<Pixel>();
        Some(Self::Lent(Lent {
            pixels,
            count,
            lender,
        }))
    }

    /// Bytes of the process heap this store holds.
    pub(crate) fn heap_bytes(&self) -> usize {
        match self {
            Self::Owned(pixels) => pixels.len() * size_of::<Pixel>(),
            Self::Lent(_) => 0,
        }
    }

    /// Clear the pixels a reclaimed surface leaves in freed heap memory. Lent
    /// memory is not freed with the surface, and is the lender's to clear.
    pub(crate) fn wipe(&mut self) {
        if let Self::Owned(pixels) = self {
            tairix_util::secret::wipe_with(pixels, Pixel::TRANSPARENT);
        }
    }
}

impl core::ops::Deref for Store {
    type Target = [Pixel];

    #[allow(unsafe_code)] // The pixels the lender answered, as proven below.
    fn deref(&self) -> &[Pixel] {
        match self {
            Self::Owned(pixels) => pixels,
            // SAFETY: `pixels` starts the `count` pixels the lender answered
            // once and nothing has reached since; `&self` keeps them unwritten
            // for the borrow.
            Self::Lent(lent) => unsafe {
                core::slice::from_raw_parts(lent.pixels.as_ptr(), lent.count)
            },
        }
    }
}

impl core::ops::DerefMut for Store {
    #[allow(unsafe_code)] // The pixels the lender answered, as proven below.
    fn deref_mut(&mut self) -> &mut [Pixel] {
        match self {
            Self::Owned(pixels) => pixels,
            // SAFETY: as for `deref`, and `&mut self` makes this borrow the
            // only one.
            Self::Lent(lent) => unsafe {
                core::slice::from_raw_parts_mut(lent.pixels.as_ptr(), lent.count)
            },
        }
    }
}

/// A copy owns its pixels: lent memory is its lender's alone.
impl Clone for Store {
    fn clone(&self) -> Self {
        Self::Owned(self.to_vec())
    }
}

impl core::fmt::Debug for Store {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        if self.len() <= DEBUG_PIXELS_MAX {
            core::fmt::Debug::fmt(&**self, f)
        } else {
            write!(f, "[{} pixels]", self.len())
        }
    }
}

/// `bytes` read as pixels to write through, four bytes each in `r, g, b, a`
/// order; a trailing part of a pixel is left out.
#[allow(unsafe_code)] // The crate's one view of bytes as pixels, proven below.
fn pixels_of_mut(bytes: &mut [u8]) -> &mut [Pixel] {
    let (quads, _) = bytes.as_chunks_mut::<4>();
    let len = quads.len();
    // SAFETY: `Pixel` is `repr(C)` over four `u8` fields with size 4 and
    // alignment 1 (asserted beside its definition), so `[u8; 4]` and `Pixel`
    // share one layout and every bit pattern is valid for both. The exclusive
    // borrow of `bytes` passes to the returned slice, which covers exactly the
    // whole quads.
    unsafe { core::slice::from_raw_parts_mut(quads.as_mut_ptr().cast::<Pixel>(), len) }
}

#[cfg(test)]
#[path = "lent_tests.rs"]
mod tests;
