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
//!
//! The design and its staging are `plans/IOMMU.md`.

#![no_std]
#![deny(missing_docs)]
#![forbid(unsafe_op_in_unsafe_fn)]

extern crate alloc;

pub mod bindings;
pub mod domain;
pub mod fault;
pub mod ids;
pub mod interrupt;
pub mod iova;
mod memory;
pub mod pagetable;
pub mod queue;
mod registers;
mod unit;

#[cfg(any(test, feature = "host-tests"))]
pub mod conformance;
#[cfg(any(test, feature = "host-tests"))]
pub mod hostmem;
#[cfg(any(test, feature = "host-tests"))]
pub mod model;

pub use bindings::{Binding, Bindings, Room};
pub use domain::{Domain, IdentityWindow};
pub use fault::{
    drain_in_batches, Charge, Fault, FaultBatch, FaultBudget, FaultLimits, FaultReason,
    FaultVerdict, FAULT_BATCH, FAULT_QUEUE_RECORDS,
};
pub use ids::Ids;
pub use interrupt::{
    InterruptRemapping, InterruptSource, InterruptTarget, Remapped, MESSAGE_WINDOW,
};
pub use iova::{IovaError, IovaSpace};
pub use memory::{Block, Table, TableMemory};
pub use pagetable::{reach_bits, IoPageTable, Pte, PteFormat, MAX_LEVELS, MAX_ROOT_ORDER};
pub use queue::{
    wait_for, wait_within, Command, CommandQueue, Completion, QueueRegisters, COMMAND_BUDGET_NS,
};
pub use registers::Registers;
pub use unit::{
    Access, Clock, DomainId, FaultRoute, IommuError, IommuUnit, Reach, Stage, TableCoherence,
    UnitFunction, UnitProfile,
};

/// Bytes in one table frame of the HAL's entries.
pub const TABLE_BYTES: usize = tairix_arch_api::PAGE_TABLE_ENTRIES * core::mem::size_of::<u64>();

/// The translation granule every family maps at its finest: one table frame,
/// which is also what a level-0 leaf maps.
pub const IO_PAGE_SIZE: u64 = TABLE_BYTES as u64;

/// `log2` of [`IO_PAGE_SIZE`].
pub const IO_PAGE_SHIFT: u32 = IO_PAGE_SIZE.trailing_zeros();
