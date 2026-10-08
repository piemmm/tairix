//! Interrupt remapping: a unit that delivers every interrupt message a device
//! sends through a table the kernel writes, so a device raises only the
//! vectors its owner was given, at the CPUs the kernel chose, and only as
//! itself.
//!
//! An MSI is a DMA write the fabric turns into an interrupt; without
//! remapping, a device that can write memory can forge any vector to any CPU.

use core::ops::Range;

use crate::IommuError;

/// Where an x86 interrupt message is written: a write here is an interrupt
/// request, which a unit never translates, so no domain hands out an IOVA in
/// it.
pub const MESSAGE_WINDOW: Range<u64> = 0xFEE0_0000..0xFEF0_0000;

/// Which requester ids may raise a remapped interrupt.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum InterruptSource {
    /// Exactly this requester id.
    Requester(u16),
    /// Any requester id on buses `first..=last`: a bridge took ownership of
    /// the requests below it, so the unit can tell its devices apart by bus
    /// at best. Every device there shares the bridge's isolation group, and
    /// so one owner.
    Buses {
        /// The first bus.
        first: u8,
        /// The last bus, inclusive.
        last: u8,
    },
}

impl InterruptSource {
    /// Whether it names any requester: a range of buses ending before it
    /// starts names none, and is refused rather than remapped for no one.
    #[must_use]
    pub const fn names_any(self) -> bool {
        match self {
            Self::Requester(_) => true,
            Self::Buses { first, last } => first <= last,
        }
    }
}

/// Where a remapped interrupt is delivered: an x86 local APIC vector.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct InterruptTarget {
    /// The vector.
    pub vector: u8,
    /// The destination's APIC id: 8 bits in xAPIC mode, 32 in x2APIC.
    pub destination: u32,
    /// Level-triggered, as an IO-APIC pin may be, rather than an edge.
    pub level: bool,
}

/// One entry of a unit's remapping table, and what a source is programmed
/// with to raise it.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct Remapped {
    /// The entry's index in the table.
    pub entry: u32,
    /// The MSI address a message-signalling source writes.
    pub address: u64,
    /// The MSI data it writes.
    pub data: u32,
    /// The IO-APIC redirection entry that raises it, mask clear: the caller
    /// sets the mask as it sees fit.
    pub redirection: u64,
}

/// The remapping half of a unit (Intel VT-d's interrupt remapping, AMD-Vi's
/// interrupt remapping tables).
///
/// Remapping is brought up in three steps so a kernel can switch every source
/// over with nothing raised in between: [`Self::prepare_remapping`] builds
/// the table, entries are made while compatibility-format interrupts still
/// pass, then [`Self::enable_remapping`] refuses every interrupt the table
/// does not name.
pub trait InterruptRemapping: Sync {
    /// Whether the unit can remap to 32-bit x2APIC destinations.
    fn supports_extended(&self) -> bool;

    /// Build an empty remapping table of `entries` entries — as many as the
    /// machine has vectors to deliver, rounded to what the family's table
    /// can hold, and no more than its largest — and point the unit at it,
    /// for 32-bit x2APIC destinations where `extended` says so, leaving
    /// remapping off.
    ///
    /// # Errors
    ///
    /// [`IommuError::OutOfRange`] where the unit cannot remap the
    /// destinations asked for or was prepared already;
    /// [`IommuError::Exhausted`] when the table cannot be had; or the unit's
    /// refusal or timeout.
    fn prepare_remapping(&self, extended: bool, entries: u32) -> Result<(), IommuError>;

    /// An entry delivering `target`, raisable only by `source`, confirmed:
    /// no stale copy of the entry survives in any cache.
    ///
    /// # Errors
    ///
    /// [`IommuError::Exhausted`] when the table is full,
    /// [`IommuError::OutOfRange`] for a destination the table's mode cannot
    /// name or remapping not prepared, or [`IommuError::Unconfirmed`], after
    /// which the entry is never handed out again.
    fn remap_interrupt(
        &self,
        source: InterruptSource,
        target: InterruptTarget,
    ) -> Result<Remapped, IommuError>;

    /// Remove entry `entry`, confirmed: once this returns, nothing raises
    /// it.
    ///
    /// # Errors
    ///
    /// [`IommuError::NotMapped`] for an entry not handed out, or
    /// [`IommuError::Unconfirmed`], after which the entry is never reused.
    fn release_interrupt(&self, entry: u32) -> Result<(), IommuError>;

    /// Turn remapping on: from now on an interrupt the table does not name,
    /// one from a source its entry does not admit, and one in compatibility
    /// format are refused.
    ///
    /// # Errors
    ///
    /// The unit's refusal or timeout, or [`IommuError::OutOfRange`] before
    /// [`Self::prepare_remapping`].
    fn enable_remapping(&self) -> Result<(), IommuError>;

    /// Turn remapping back off, so compatibility-format interrupts pass
    /// again: the way back for a machine another of whose units could not
    /// turn it on. The table stays.
    ///
    /// # Errors
    ///
    /// The unit's refusal or timeout, after which it may still refuse
    /// compatibility interrupts, or [`IommuError::OutOfRange`] before
    /// [`Self::prepare_remapping`].
    fn disable_remapping(&self) -> Result<(), IommuError>;
}

/// Where a unit writes once it has recorded a confined device's message: an
/// interrupt file's page, as the hart it belongs to reaches it.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct Notice {
    /// The physical address of the interrupt file's page.
    pub address: u64,
    /// The identity written there.
    pub data: u32,
}

/// Bytes of a memory-resident interrupt file, and the alignment it needs
/// (RISC-V Advanced Interrupt Architecture, "Memory-resident interrupt
/// files").
pub const MESSAGE_FILE_BYTES: u64 = 512;

/// The half of a unit that intercepts its devices' message writes itself (the
/// RISC-V IOMMU's MSI page tables): each confined stream's messages set an
/// identity in a memory-resident interrupt file the kernel owns, and the unit
/// then writes a notice the kernel chose, so the stream reaches no interrupt
/// file but its own.
pub trait MessageFiles: Sync {
    /// Whether the unit sets a file's pending bit atomically. Where it does
    /// not, a pending bit the kernel clears as the unit sets another can come
    /// back: a repeated interrupt, never a lost one.
    fn atomic_files(&self) -> bool;

    /// Confine `stream`'s messages, at once where it translates and from its
    /// next attach otherwise: a write it makes to the page at IOVA
    /// `doorbell`, which its domains keep clear, sets the identity it writes
    /// in the file at physical address `file`, and the unit then writes
    /// `notice`. Every stream of a domain is confined alike, since the unit
    /// caches what a message translates to by domain, not by stream.
    ///
    /// # Errors
    ///
    /// [`IommuError::OutOfRange`] for a file not aligned to
    /// [`MESSAGE_FILE_BYTES`], a
    /// doorbell or notice address not page-aligned, a notice identity past
    /// 2047, or a stream the unit cannot name; [`IommuError::Exhausted`]
    /// when the table cannot be had.
    fn confine_messages(
        &self,
        stream: u32,
        doorbell: u64,
        file: u64,
        notice: Notice,
    ) -> Result<(), IommuError>;
}
