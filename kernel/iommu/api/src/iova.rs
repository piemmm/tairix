//! A domain's I/O virtual address space.
//!
//! A buddy allocator over naturally aligned power-of-two blocks of pages:
//! a buddy carve's frames are aligned to their own size, so an IOVA aligned
//! the same way lets the unit map the carve with the largest leaves it has.
//! Blocks are handed out top-down below the device's reach. Each order's free
//! blocks are a [`RadixTree`] keyed by block index, so a search or an update
//! costs the tree's height and nothing scans or shifts. Each order's highest
//! free block is kept beside its tree, since an allocation wants the highest
//! block, so one reaching the whole aperture walks no tree to choose it.

use alloc::vec::Vec;
use core::ops::Range;

use tairix_collections::RadixTree;

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
    /// No memory to record a free block.
    Exhausted,
}

/// A block an [`IovaSpace`] handed out. Only [`IovaSpace::alloc`] makes one
/// and [`IovaSpace::free`] consumes it, so a block goes back once, whole, at
/// the order it was handed out at:
///
/// ```compile_fail,E0382
/// # use tairix_kernel_iommu_api::IovaSpace;
/// let mut space = IovaSpace::new(0x1000..0x10_0000, &[]).unwrap();
/// let block = space.alloc(0, 0).unwrap();
/// space.free(block).unwrap();
/// space.free(block).unwrap();
/// ```
#[must_use = "a block dropped instead of freed stays out of its space"]
#[derive(Debug, Eq, PartialEq)]
pub struct IovaBlock {
    base: u64,
    order: usize,
}

impl IovaBlock {
    /// Its first IOVA, aligned to its size.
    #[must_use]
    pub const fn base(&self) -> u64 {
        self.base
    }

    /// Its size in bytes: `IO_PAGE_SIZE << order`.
    #[must_use]
    pub const fn bytes(&self) -> u64 {
        block_bytes(self.order)
    }
}

/// One domain's IOVA space.
pub struct IovaSpace {
    aperture: Range<u64>,
    /// `orders[o]` holds the free blocks of `IO_PAGE_SIZE << o` bytes.
    orders: Vec<Order>,
}

/// One order's free blocks, each by its index: its base over its size.
#[derive(Default)]
struct Order {
    /// The nodes a change needs are reserved before the space changes, so
    /// running out of memory leaves it as it was.
    blocks: RadixTree<()>,
    /// The greatest index in `blocks`.
    highest: Option<u64>,
}

impl Order {
    /// The greatest index at or below `at`.
    fn at_or_below(&self, at: u64) -> Option<u64> {
        match self.highest? {
            highest if highest <= at => Some(highest),
            _ => self.blocks.prev(at).map(|(index, ())| index),
        }
    }

    fn holds(&self, index: u64) -> bool {
        self.highest.is_some_and(|highest| index <= highest) && self.blocks.contains_key(index)
    }

    fn insert(&mut self, index: u64) -> Result<(), IovaError> {
        self.blocks
            .try_insert(index, ())
            .map_err(|_| IovaError::Exhausted)?;
        self.highest = self.highest.max(Some(index));
        Ok(())
    }

    /// Record `index` into room [`RadixTree::try_reserve_key`] made for it,
    /// which no exhaustion can take away.
    fn record(&mut self, index: u64) {
        let _reserved = self.insert(index);
    }

    fn remove(&mut self, index: u64) {
        if self.blocks.remove(index).is_some() && self.highest == Some(index) {
            self.highest = index
                .checked_sub(1)
                .and_then(|below| self.blocks.prev(below))
                .map(|(next, ())| next);
        }
    }
}

const fn block_bytes(order: usize) -> u64 {
    IO_PAGE_SIZE << order
}

/// The index of the block of `order` holding `at`.
const fn index(order: usize, at: u64) -> u64 {
    at >> (IO_PAGE_SHIFT as usize + order)
}

/// The block of `order` that is the buddy of the one holding `at`.
const fn buddy(order: usize, at: u64) -> u64 {
    (at & !(block_bytes(order) - 1)) ^ block_bytes(order)
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
        let mut orders = Vec::new();
        orders
            .try_reserve_exact(ORDERS)
            .map_err(|_| IovaError::Exhausted)?;
        orders.resize_with(ORDERS, Order::default);
        let mut space = Self {
            aperture: aperture.clone(),
            orders,
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
    /// where a switch could route them peer-to-peer.
    pub fn alloc(&mut self, order: u32, limit: u64) -> Option<IovaBlock> {
        let want = usize::try_from(order).ok().filter(|&o| o < ORDERS)?;
        let size = block_bytes(want);
        let limit = if limit == 0 {
            self.aperture.end
        } else {
            limit.min(self.aperture.end)
        };
        let highest_base = limit.checked_sub(size)?;
        // Free blocks are disjoint, so the highest one based low enough holds
        // the highest slot. The highest base an order offers falls as the
        // order grows, so once it is below the best found no larger order can
        // beat it.
        let mut best: Option<(usize, u64)> = None;
        for o in want..ORDERS {
            let ceiling = highest_base & !(block_bytes(o) - 1);
            if best.is_some_and(|(_, block)| block >= ceiling) {
                break;
            }
            if let Some(at) = self.orders[o].at_or_below(index(o, ceiling)) {
                let block = at << (IO_PAGE_SHIFT as usize + o);
                if best.is_none_or(|(_, highest)| block > highest) {
                    best = Some((o, block));
                }
            }
        }
        let (found, block) = best?;
        let target = ((block + block_bytes(found)).min(limit) - size) & !(size - 1);
        // Each order below the one split keeps the buddy of the half holding
        // the target, made room for before anything changes.
        for o in want..found {
            self.orders[o]
                .blocks
                .try_reserve_key(index(o, buddy(o, target)))
                .ok()?;
        }
        self.orders[found].remove(index(found, block));
        for o in want..found {
            self.orders[o].record(index(o, buddy(o, target)));
        }
        Some(IovaBlock {
            base: target,
            order: want,
        })
    }

    /// Take back `block`, which this space handed out, merging it with every
    /// free buddy.
    ///
    /// # Errors
    ///
    /// [`IovaError::Exhausted`] with no memory to record the merged block:
    /// the block then stays out of the space.
    // Taken by value so a block goes back once.
    #[allow(clippy::needless_pass_by_value)]
    pub fn free(&mut self, block: IovaBlock) -> Result<(), IovaError> {
        let IovaBlock { base, order } = block;
        let mut top = order;
        while top + 1 < ORDERS && self.orders[top].holds(index(top, buddy(top, base))) {
            top += 1;
        }
        let merged = index(top, base);
        self.orders[top]
            .blocks
            .try_reserve_key(merged)
            .map_err(|_| IovaError::Exhausted)?;
        for o in order..top {
            self.orders[o].remove(index(o, buddy(o, base)));
        }
        self.orders[top].record(merged);
        Ok(())
    }

    /// Free bytes in the space.
    #[must_use]
    pub fn free_bytes(&self) -> u64 {
        self.orders
            .iter()
            .enumerate()
            .map(|(o, order)| block_bytes(o).saturating_mul(order.blocks.len() as u64))
            .fold(0, u64::saturating_add)
    }

    /// Release `[start, end)` as the largest aligned blocks it holds.
    fn release_range(&mut self, mut start: u64, end: u64) -> Result<(), IovaError> {
        while start < end {
            let align = usize::try_from(start.trailing_zeros().saturating_sub(IO_PAGE_SHIFT))
                .unwrap_or(ORDERS - 1);
            let fit = usize::try_from((end - start).ilog2().saturating_sub(IO_PAGE_SHIFT))
                .unwrap_or(ORDERS - 1);
            let order = align.min(fit).min(ORDERS - 1);
            self.orders[order].insert(index(order, start))?;
            start += block_bytes(order);
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "iova_tests.rs"]
mod tests;
