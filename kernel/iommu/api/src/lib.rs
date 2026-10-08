//! DMA translation for TAIRiX: the contract every IOMMU family implements,
//! and the family-independent machinery built on it.
//!
//! A translation unit confines each bus-mastering device to the memory the
//! kernel mapped for it. This crate holds what every family shares:
//!
//! * [`IommuUnit`] — the per-unit contract a family (`kernel/iommu/vtd`, …)
//!   implements: domains, attach and block, map, unmap and a confirmed sync,
//!   and fault draining.
//! * [`IoPageTable`] — the one radix walker the table-walking families use,
//!   parameterised by a [`PteFormat`], over [`TableMemory`], which every
//!   family's own tables live in too.
//! * [`IovaSpace`] — a domain's buddy-allocated IOVA space.
//! * [`Domain`] — the kernel's handle on a domain, which never lets an IOVA
//!   or a frame be reused before the unit confirms its translation gone.
//! * [`Fault`] and [`FaultBudget`] — refused accesses, and the per-stream
//!   accounting that contains a device raising them in a storm.
//! * [`InterruptRemapping`] — the remapping half of a unit that delivers
//!   interrupt messages through a table the kernel writes.
//! * [`FrameRings`] — ring memory for a unit that is itself a virtio device.
//!
//! The design and its staging are `plans/IOMMU.md`.

#![no_std]
#![deny(missing_docs)]
#![forbid(unsafe_op_in_unsafe_fn)]

extern crate alloc;

pub mod bindings;
pub mod domain;
pub mod domains;
pub mod fault;
pub mod ids;
pub mod interrupt;
pub mod iova;
mod memory;
pub mod pagetable;
pub mod queue;
mod registers;
mod rings;
mod unit;

#[cfg(any(test, feature = "host-tests"))]
pub mod conformance;
#[cfg(any(test, feature = "host-tests"))]
pub mod hostmem;
#[cfg(any(test, feature = "host-tests"))]
pub mod model;
#[cfg(test)]
mod testunit;

pub use bindings::{Binding, Bindings, Room};
pub use domain::{Domain, FrameRun, IdentityWindow};
pub use domains::DomainMap;
pub use fault::{
    drain_in_batches, Charge, Fault, FaultBatch, FaultBudget, FaultLimits, FaultReason,
    FaultVerdict, FAULT_BATCH, FAULT_QUEUE_RECORDS,
};
pub use ids::Ids;
pub use interrupt::{
    InterruptRemapping, InterruptSource, InterruptTarget, MessageFiles, Notice, Remapped,
    MESSAGE_FILE_BYTES, MESSAGE_WINDOW,
};
pub use iova::{IovaBlock, IovaError, IovaSpace};
pub use memory::{Block, Table, TableMemory};
pub use pagetable::{reach_bits, IoPageTable, Leaf, Pte, PteFormat, MAX_LEVELS, MAX_ROOT_ORDER};
pub use queue::{
    wait_for, wait_within, Command, CommandQueue, Completion, Invalidator, QueueRegisters, Ticket,
    COMMAND_BUDGET_NS, PAGE_INVALIDATIONS,
};
pub use registers::Registers;
pub use rings::FrameRings;
pub use tairix_abi::driver::virtio_pci::VirtioPciWindows;
pub use unit::{
    Access, Clock, DomainId, FaultRoute, IommuError, IommuUnit, Reach, Signalling, Stage,
    TableCoherence, Tables, UnitFunction, UnitProfile,
};

/// Bytes in one table frame of the HAL's entries.
pub const TABLE_BYTES: usize = tairix_arch_api::PAGE_TABLE_ENTRIES * core::mem::size_of::<u64>();

/// The translation granule every family maps at its finest: one table frame,
/// which is also what a level-0 leaf maps.
pub const IO_PAGE_SIZE: u64 = TABLE_BYTES as u64;

/// `log2` of [`IO_PAGE_SIZE`].
pub const IO_PAGE_SHIFT: u32 = IO_PAGE_SIZE.trailing_zeros();

/// The IO pages a range of IOVAs touches, at least one.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct PageSpan {
    first: u64,
    pages: u64,
}

impl PageSpan {
    /// The pages `[iova, iova + len)` touches, an unaligned end reaching
    /// into the page it falls in.
    ///
    /// # Errors
    ///
    /// [`IommuError::OutOfRange`] for an empty range or one running past the
    /// top of the space.
    pub const fn of(iova: u64, len: u64) -> Result<Self, IommuError> {
        let Some(span) = len.checked_sub(1) else {
            return Err(IommuError::OutOfRange);
        };
        let Some(last) = iova.checked_add(span) else {
            return Err(IommuError::OutOfRange);
        };
        let first = iova & !(IO_PAGE_SIZE - 1);
        Ok(Self {
            first,
            pages: ((last - first) >> IO_PAGE_SHIFT) + 1,
        })
    }

    /// The first page's address.
    #[must_use]
    pub const fn first(self) -> u64 {
        self.first
    }

    /// How many pages.
    #[must_use]
    pub const fn pages(self) -> u64 {
        self.pages
    }

    /// Each page's address, ascending.
    pub fn addresses(self) -> impl Iterator<Item = u64> {
        (0..self.pages).map(move |page| self.first + page * IO_PAGE_SIZE)
    }

    /// The smallest naturally aligned block of pages holding the span: its
    /// base and its order. What an address-selective invalidation names.
    #[must_use]
    pub const fn covering_block(self) -> (u64, u32) {
        let last = self.first + (self.pages - 1) * IO_PAGE_SIZE;
        let differ = (self.first ^ last) >> IO_PAGE_SHIFT;
        let order = if differ == 0 { 0 } else { differ.ilog2() + 1 };
        let bits = order + IO_PAGE_SHIFT;
        let mask = if bits >= u64::BITS {
            u64::MAX
        } else {
            (1 << bits) - 1
        };
        (self.first & !mask, order)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn covering_block(iova: u64, len: u64) -> (u64, u32) {
        PageSpan::of(iova, len).unwrap().covering_block()
    }

    #[test]
    fn a_covering_block_is_the_smallest_aligned_one_holding_the_range() {
        assert_eq!(covering_block(0x5000, IO_PAGE_SIZE), (0x5000, 0));
        assert_eq!(covering_block(0x5000, 1), (0x5000, 0));
        assert_eq!(
            covering_block(0x1000, 0x2000),
            (0, 2),
            "pages 1 and 2 share block 0..4"
        );
        assert_eq!(covering_block(0x4000, 0x4000), (0x4000, 2));
        assert_eq!(
            covering_block(0x7000, 0x2000),
            (0, 4),
            "pages 7 and 8 meet only at 16"
        );
        let top = !(IO_PAGE_SIZE - 1);
        assert_eq!(covering_block(top, IO_PAGE_SIZE), (top, 0));
        assert_eq!(covering_block(0, u64::MAX), (0, 52), "the whole space");
    }

    /// An unaligned range reaches into the page its end falls in, and an
    /// empty or wrapping one names no page at all.
    #[test]
    fn a_span_counts_every_page_its_range_touches() {
        let span = PageSpan::of(0x1800, 0x1000).unwrap();
        assert_eq!((span.first(), span.pages()), (0x1000, 2));
        assert_eq!(
            span.addresses().collect::<alloc::vec::Vec<_>>(),
            [0x1000, 0x2000]
        );
        let top = !(IO_PAGE_SIZE - 1);
        assert_eq!(PageSpan::of(top, IO_PAGE_SIZE).map(PageSpan::pages), Ok(1));
        assert_eq!(PageSpan::of(0x1000, 0), Err(IommuError::OutOfRange));
        assert_eq!(
            PageSpan::of(top, IO_PAGE_SIZE + 1),
            Err(IommuError::OutOfRange)
        );
    }
}
