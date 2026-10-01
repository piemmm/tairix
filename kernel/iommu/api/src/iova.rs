//! A domain's I/O virtual address space.
//!
//! A buddy allocator over naturally aligned power-of-two blocks of pages:
//! a buddy carve's frames are aligned to their own size, so an IOVA aligned
//! the same way lets the unit map the carve with the largest leaves it has.
//! Blocks are handed out top-down below the device's reach, and every
//! operation costs `O(orders × log blocks)` — nothing scans.

use alloc::vec::Vec;
use core::ops::Range;

use crate::{IO_PAGE_SHIFT, IO_PAGE_SIZE};

/// Block orders an IOVA space can hold: one page up to half the 64-bit span,
/// the largest block an aperture starting above IOVA 0 can contain.
const ORDERS: usize = (u64::BITS - 1 - IO_PAGE_SHIFT) as usize + 1;

/// Why an IOVA space could not be built or could not take a block back.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum IovaError {
    /// The aperture is empty, not page-aligned, or starts at IOVA 0.
    BadAperture,
    /// A reserved range is not page-aligned or is empty.
    BadReservation,
    /// The block handed back is misaligned for its order, past the orders a
    /// space holds, or already free.
    NotAllocated,
    /// No memory to record a free block.
    Exhausted,
}

/// One domain's IOVA space.
pub struct IovaSpace {
    aperture: Range<u64>,
    /// `free[o]` holds the base of every free block of `IO_PAGE_SIZE << o`
    /// bytes, ascending. Room for a change is reserved before the space
    /// changes, so running out of memory leaves it as it was.
    free: Vec<Vec<u64>>,
}

const fn block_bytes(order: usize) -> u64 {
    IO_PAGE_SIZE << order
}

impl IovaSpace {
    /// The space `aperture` spans, less every range in `reserved` (which may
    /// overlap one another or reach outside the aperture).
    ///
    /// # Errors
    ///
    /// [`IovaError::BadAperture`] for an empty or misaligned aperture, or one
    /// that starts at IOVA 0 — the page a null device address names is never
    /// handed out — [`IovaError::BadReservation`] for an empty or misaligned
    /// reservation, and [`IovaError::Exhausted`] with no memory to record the
    /// space.
    pub fn new(aperture: Range<u64>, reserved: &[Range<u64>]) -> Result<Self, IovaError> {
        let aligned = |at: u64| at.is_multiple_of(IO_PAGE_SIZE);
        if aperture.start == 0
            || aperture.start >= aperture.end
            || !aligned(aperture.start)
            || !aligned(aperture.end)
        {
            return Err(IovaError::BadAperture);
        }
        if reserved
            .iter()
            .any(|r| r.start >= r.end || !aligned(r.start) || !aligned(r.end))
        {
            return Err(IovaError::BadReservation);
        }
        let mut holes = Vec::new();
        holes
            .try_reserve_exact(reserved.len())
            .map_err(|_| IovaError::Exhausted)?;
        holes.extend_from_slice(reserved);
        holes.sort_unstable_by_key(|r| r.start);
        let mut free = Vec::new();
        free.try_reserve_exact(ORDERS)
            .map_err(|_| IovaError::Exhausted)?;
        free.resize_with(ORDERS, Vec::new);
        let mut space = Self {
            aperture: aperture.clone(),
            free,
        };
        let mut cursor = aperture.start;
        for hole in holes {
            if hole.start > cursor {
                space.release_range(cursor, hole.start.min(aperture.end))?;
            }
            cursor = cursor.max(hole.end);
            if cursor >= aperture.end {
                break;
            }
        }
        if cursor < aperture.end {
            space.release_range(cursor, aperture.end)?;
        }
        Ok(space)
    }

    /// The aperture the space was built over.
    #[must_use]
    pub fn aperture(&self) -> Range<u64> {
        self.aperture.clone()
    }

    /// Hand out a naturally aligned block of `IO_PAGE_SIZE << order` bytes
    /// whose end is at most `limit` (`0` for the aperture's own end), at the
    /// highest address it fits, or [`None`] when no free block holds it or
    /// the split cannot be recorded.
    ///
    /// Highest-first keeps device addresses clear of the low MMIO windows
    /// where a switch could route them peer-to-peer; among equally high
    /// slots the smallest free block is split.
    pub fn alloc(&mut self, order: u32, limit: u64) -> Option<u64> {
        let want = usize::try_from(order).ok().filter(|&o| o < ORDERS)?;
        let size = block_bytes(want);
        let limit = if limit == 0 {
            self.aperture.end
        } else {
            limit.min(self.aperture.end)
        };
        let highest_base = limit.checked_sub(size)?;
        let mut best: Option<(u64, usize, usize)> = None;
        for o in want..ORDERS {
            // Blocks of one order are disjoint, so the highest base that
            // fits also holds that order's highest fitting slot.
            let blocks = &self.free[o];
            let Some(index) = blocks
                .partition_point(|&b| b <= highest_base)
                .checked_sub(1)
            else {
                continue;
            };
            let block = blocks[index];
            let target = ((block + block_bytes(o)).min(limit) - size) & !(size - 1);
            if best.is_none_or(|(highest, _, _)| target > highest) {
                best = Some((target, o, index));
            }
        }
        let (target, found_order, index) = best?;
        // Each order below the one split takes exactly one buddy.
        for o in want..found_order {
            self.free[o].try_reserve(1).ok()?;
        }
        let mut base = self.free[found_order].remove(index);
        for o in (want..found_order).rev() {
            let upper = base + block_bytes(o);
            if target >= upper {
                self.insert(o, base);
                base = upper;
            } else {
                self.insert(o, upper);
            }
        }
        Some(base)
    }

    /// Take back the block of order `order` at `base`, merging it with every
    /// free buddy.
    ///
    /// # Errors
    ///
    /// [`IovaError::NotAllocated`] for a base misaligned for its order, an
    /// order past the space's, or a block any part of which is already free,
    /// and [`IovaError::Exhausted`] with no memory to record the merged
    /// block: the block then stays out of the space.
    pub fn free(&mut self, base: u64, order: u32) -> Result<(), IovaError> {
        let order = usize::try_from(order)
            .ok()
            .filter(|&o| o < ORDERS)
            .ok_or(IovaError::NotAllocated)?;
        let end = base.checked_add(block_bytes(order));
        let inside = base >= self.aperture.start && end.is_some_and(|end| end <= self.aperture.end);
        if !base.is_multiple_of(block_bytes(order)) || !inside || self.overlaps_free(base, order) {
            return Err(IovaError::NotAllocated);
        }
        let (mut merged, mut top) = (base, order);
        while top + 1 < ORDERS && self.position(top, merged ^ block_bytes(top)).is_ok() {
            merged = merged.min(merged ^ block_bytes(top));
            top += 1;
        }
        self.free[top]
            .try_reserve(1)
            .map_err(|_| IovaError::Exhausted)?;
        let mut base = base;
        for o in order..top {
            let buddy = base ^ block_bytes(o);
            if let Ok(index) = self.position(o, buddy) {
                self.free[o].remove(index);
            }
            base = base.min(buddy);
        }
        self.insert(top, base);
        Ok(())
    }

    /// Free bytes in the space.
    #[must_use]
    pub fn free_bytes(&self) -> u64 {
        self.free
            .iter()
            .enumerate()
            .map(|(order, blocks)| block_bytes(order).saturating_mul(blocks.len() as u64))
            .fold(0, u64::saturating_add)
    }

    /// Where `base` is, or would go, among the free blocks of `order`.
    fn position(&self, order: usize, base: u64) -> Result<usize, usize> {
        self.free[order].binary_search(&base)
    }

    /// Record the free block of `order` at `base`, into room reserved for it.
    fn insert(&mut self, order: usize, base: u64) {
        if let Err(index) = self.position(order, base) {
            self.free[order].insert(index, base);
        }
    }

    /// Whether any byte of the aligned block at `base` is free: a free block
    /// of its order or above containing it, or a smaller one inside it.
    fn overlaps_free(&self, base: u64, order: usize) -> bool {
        let end = base + block_bytes(order);
        let contained =
            (order..ORDERS).any(|o| self.position(o, base & !(block_bytes(o) - 1)).is_ok());
        contained
            || (0..order).any(|o| {
                let blocks = &self.free[o];
                blocks
                    .get(blocks.partition_point(|&b| b < base))
                    .is_some_and(|&b| b < end)
            })
    }

    /// Release `[start, end)` as the largest aligned blocks it holds.
    fn release_range(&mut self, mut start: u64, end: u64) -> Result<(), IovaError> {
        while start < end {
            let align = usize::try_from(start.trailing_zeros().saturating_sub(IO_PAGE_SHIFT))
                .unwrap_or(ORDERS - 1);
            let fit = usize::try_from((end - start).ilog2().saturating_sub(IO_PAGE_SHIFT))
                .unwrap_or(ORDERS - 1);
            let order = align.min(fit).min(ORDERS - 1);
            self.free[order]
                .try_reserve(1)
                .map_err(|_| IovaError::Exhausted)?;
            self.insert(order, start);
            start += block_bytes(order);
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "iova_tests.rs"]
mod tests;
