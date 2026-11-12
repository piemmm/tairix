//! The page map translated code reaches memory through: for each page of
//! the address space, where its bytes lie in the host's memory and how the
//! bus treats them. A page the map gives no base is one the code may not
//! touch — device memory, a window nothing is mapped into, wait states that
//! vary within it — so an access there leaves the block for the
//! interpreter; one not writable — ROM, or a page holding translated code —
//! refuses a store the same way.

use alloc::vec;
use alloc::vec::Vec;

/// A page's size, as a power of two. A page holding translated code takes
/// no store from translated code, so a page is small, for the data beside
/// a program's code seldom to share one with it.
pub const PAGE_BITS: u32 = 8;
/// Bytes in a page.
pub const PAGE_BYTES: u32 = 1 << PAGE_BITS;
/// Pages: a megabyte's.
pub const PAGES: usize = 1 << (20 - PAGE_BITS);
/// Bytes an entry takes, as a power of two.
pub(crate) const SHIFT: u32 = 4;
/// Bytes an entry takes.
pub const ENTRY_BYTES: usize = 1 << SHIFT;

/// The host address of the page's first byte, a 64-bit word; zero where
/// the code may not touch it.
pub(crate) const BASE: i32 = 0;
/// Wait states each byte access takes.
pub(crate) const WAITS: i32 = 8;
/// What the code may do there: [`WRITABLE`]. The code reads it with the
/// wait states as one half-word.
pub(crate) const KIND: i32 = WAITS + 1;

/// A store may be made.
pub const WRITABLE: u8 = 1;

/// The offset of page `page`'s entry.
pub(crate) fn entry_offset(page: u32) -> i32 {
    let page = i32::try_from(page)
        .ok()
        .filter(|&page| page < 1 << (20 - PAGE_BITS));
    page.unwrap_or(0) << SHIFT
}

/// Words an entry takes: its base, then its waits and kind.
const WORDS: usize = ENTRY_BYTES / 8;

// The layout `PageMap::set` writes.
const _: () = assert!(BASE == 0 && WAITS == 8 && KIND == WAITS + 1 && WORDS == 2);

/// The map, a word at a time, so every field the code loads lies at its own
/// alignment wherever the map is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PageMap(Vec<u64>);

impl Default for PageMap {
    fn default() -> Self {
        Self(vec![0; PAGES * WORDS])
    }
}

impl PageMap {
    /// Page `page` at host address `base` (zero for none), its byte
    /// accesses taking `waits`, a store made there where `writable`.
    pub fn set(&mut self, page: usize, base: u64, waits: u8, writable: bool) {
        if let Some(entry) = self.0.get_mut(page * WORDS..(page + 1) * WORDS) {
            let kind = if writable { WRITABLE } else { 0 };
            entry[0] = base;
            entry[1] = u64::from(waits) | u64::from(kind) << 8;
        }
    }

    /// The map's words, as the code reads them: each the little-endian
    /// bytes its fields lie in.
    #[must_use]
    pub fn words(&self) -> &[u64] {
        &self.0
    }

    /// The `width` bytes at `offset`, little-endian, as the code loads them;
    /// none past the map.
    #[cfg(any(test, feature = "test-util"))]
    #[must_use]
    pub fn load(&self, offset: usize, width: usize) -> Option<u64> {
        let end = offset
            .checked_add(width)
            .filter(|&end| width <= 8 && end <= self.0.len() * 8)?;
        Some((offset..end).rev().fold(0, |value, at| {
            value << 8 | u64::from(self.0[at / 8].to_le_bytes()[at % 8])
        }))
    }
}
