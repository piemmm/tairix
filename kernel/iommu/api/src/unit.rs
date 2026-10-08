//! The contract one hardware family implements for each translation unit it
//! drives.

use core::ops::Range;

use crate::domain::FrameRun;
use crate::fault::Fault;
use crate::interrupt::{InterruptRemapping, MessageFiles};

/// How a device may use a mapping.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct Access(u8);

impl Access {
    /// The device may read.
    pub const READ: Self = Self(1 << 0);
    /// The device may write.
    pub const WRITE: Self = Self(1 << 1);
    /// The device may read and write: every DMA carve.
    pub const READ_WRITE: Self = Self(Self::READ.0 | Self::WRITE.0);

    /// Whether the device may read.
    #[must_use]
    pub const fn read(self) -> bool {
        self.0 & Self::READ.0 != 0
    }

    /// Whether the device may write.
    #[must_use]
    pub const fn write(self) -> bool {
        self.0 & Self::WRITE.0 != 0
    }
}

/// Why a unit refused or failed an operation.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum IommuError {
    /// No memory for a table, no free domain id, or no IOVA space left.
    Exhausted,
    /// An address, length, stream or domain outside what the unit accepts.
    OutOfRange,
    /// Something is already mapped where a map named.
    AlreadyMapped,
    /// Nothing is mapped where an unmap named.
    NotMapped,
    /// An unmap would split a leaf a map installed whole.
    Split,
    /// The stream is already translated through another domain.
    StreamBusy,
    /// The domain still has streams attached.
    DomainBusy,
    /// The unit did not confirm an invalidation within its budget. Memory it
    /// could once reach must never be reused.
    Unconfirmed,
    /// The unit reported an error of its own: a rejected command, a hardware
    /// fault, or a state it should not be in.
    Hardware,
    /// The unit has no endpoint by the stream named, so no DMA arrives as it:
    /// a requester id the fabric delivers under another.
    NoEndpoint,
}

/// A unit-local domain handle.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct DomainId(pub u32);

impl DomainId {
    /// The id as a sixteen-bit domain field holds it: a VT-d or AMD-Vi
    /// domain id, an Arm VMID or ASID.
    ///
    /// # Errors
    ///
    /// [`IommuError::OutOfRange`] for one wider.
    pub fn sixteen_bits(self) -> Result<u16, IommuError> {
        u16::try_from(self.0).map_err(|_| IommuError::OutOfRange)
    }
}

/// The translation stage a unit's domains are walked at.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Stage {
    /// The stage a process's address space would use, its translations
    /// tagged by address space.
    First,
    /// The stage a hypervisor would own, its translations tagged by virtual
    /// machine: what a unit translating at one stage alone usually offers.
    Second,
}

impl Stage {
    /// The stage as the audit trail spells it.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::First => "first",
            Self::Second => "second",
        }
    }
}

/// Where a unit keeps its domains' translations.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Tables {
    /// In I/O page tables of the kernel's, which the unit walks at this
    /// stage.
    Walked(Stage),
    /// In the unit itself, each installed by a request: a virtio-iommu's,
    /// which its hypervisor applies.
    Kept,
}

impl Tables {
    /// As the audit trail spells it: the stage walked, or `kept`.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Walked(stage) => stage.name(),
            Self::Kept => "kept",
        }
    }
}

/// What a unit's domains translate between.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Reach {
    /// Bits of IOVA its domains translate.
    pub input_bits: u32,
    /// Bits of physical address its tables can name.
    pub output_bits: u32,
}

impl Reach {
    /// The exclusive physical address its tables can name up to.
    #[must_use]
    pub const fn output_limit(self) -> u64 {
        if self.output_bits >= 64 {
            u64::MAX
        } else {
            1 << self.output_bits
        }
    }
}

/// What a unit reports it can do.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct UnitProfile {
    /// Where its domains' translations live.
    pub tables: Tables,
    /// The addresses its domains translate between.
    pub reach: Reach,
    /// IOVA windows no domain may map, because the fabric claims them before
    /// translation (the x86 interrupt window).
    pub reserved: &'static [Range<u64>],
    /// Whether its tables can let a device write what it may not read: where
    /// they cannot, [`IommuUnit::map`] refuses [`Access::WRITE`] alone.
    pub write_only: bool,
}

/// One translation unit, as its family drives it.
///
/// Every method is callable from any CPU: a family serialises its own queue
/// and tables. An operation that removes a translation is complete only once
/// [`Self::sync`] (or [`Self::block`]) has returned `Ok`.
pub trait IommuUnit: Sync {
    /// The unit's capabilities.
    fn profile(&self) -> UnitProfile;

    /// Start translating. A family brings its unit up blocked, and the
    /// kernel attaches firmware's reserved windows before calling this, so
    /// firmware's own DMA never sees translation without them.
    ///
    /// # Errors
    ///
    /// The unit's refusal or timeout.
    fn enable(&self) -> Result<(), IommuError>;

    /// Create an empty domain.
    ///
    /// # Errors
    ///
    /// [`IommuError::Exhausted`] when the unit has no domain id or table left.
    fn create_domain(&self) -> Result<DomainId, IommuError>;

    /// Release a domain no stream is attached to, freeing its tables once
    /// the unit confirms no cached translation of it survives.
    ///
    /// # Errors
    ///
    /// [`IommuError::DomainBusy`] while a stream is attached,
    /// [`IommuError::OutOfRange`] for an unknown domain, or
    /// [`IommuError::Unconfirmed`], after which the tables are kept, never
    /// freed.
    fn destroy_domain(&self, domain: DomainId) -> Result<(), IommuError>;

    /// Translate `stream`'s DMA through `domain` from now on.
    ///
    /// # Errors
    ///
    /// [`IommuError::StreamBusy`] when the stream is attached elsewhere,
    /// [`IommuError::OutOfRange`] for a stream the unit does not cover or an
    /// unknown domain, and [`IommuError::NoEndpoint`] for one it covers but
    /// has no endpoint by, which stays unattached.
    fn attach(&self, stream: u32, domain: DomainId) -> Result<(), IommuError>;

    /// Block `stream`'s DMA and confirm no cached translation of it survives.
    /// Blocking a blocked stream is a no-op.
    ///
    /// # Errors
    ///
    /// [`IommuError::OutOfRange`] for a stream the unit does not cover, or
    /// [`IommuError::Unconfirmed`], after which the stream still counts as
    /// its domain's until a later block confirms it gone.
    fn block(&self, stream: u32) -> Result<(), IommuError>;

    /// Block `stream` as [`Self::block`] does, and stop recording its faults:
    /// a stream that storms is silenced so its device cannot keep the fault
    /// path busy. A later [`Self::attach`] gives it an owner again.
    ///
    /// # Errors
    ///
    /// As [`Self::block`], and [`IommuError::Exhausted`] when the unit cannot
    /// build what it silences with.
    fn silence(&self, stream: u32) -> Result<(), IommuError>;

    /// Map `[iova, iova + len)` of `domain` onto `[phys, phys + len)`. All
    /// three are page-aligned. The device may use the mapping once this
    /// returns.
    ///
    /// # Errors
    ///
    /// [`IommuError::AlreadyMapped`], [`IommuError::OutOfRange`],
    /// [`IommuError::Exhausted`], or the unit's own refusal: a failed map
    /// leaves nothing of itself in the tables, only what the next
    /// [`Self::sync`] removes from the caches. [`IommuError::Unconfirmed`]
    /// when it could not take back what it installed, after which nothing
    /// it may have mapped may be reused.
    fn map(
        &self,
        domain: DomainId,
        iova: u64,
        phys: u64,
        len: u64,
        access: Access,
    ) -> Result<(), IommuError>;

    /// [`Self::map`] each of `runs`, back to back from `iova`, publishing
    /// them as one: a unit that must flush what it cached of absent entries
    /// does so once, not once a run. All or nothing: a refusal takes back
    /// every run installed before it, or answers
    /// [`IommuError::Unconfirmed`] where they could not all be taken back.
    ///
    /// # Errors
    ///
    /// As [`Self::map`].
    fn map_runs(
        &self,
        domain: DomainId,
        iova: u64,
        runs: &[FrameRun],
        access: Access,
    ) -> Result<(), IommuError> {
        let mut mapped = 0;
        for run in runs {
            match self.map(domain, iova + mapped, run.phys, run.bytes(), access) {
                Ok(()) => mapped += run.bytes(),
                Err(IommuError::Unconfirmed) => return Err(IommuError::Unconfirmed),
                Err(err) if mapped == 0 || self.unmap(domain, iova, mapped).is_ok() => {
                    return Err(err)
                }
                Err(_) => return Err(IommuError::Unconfirmed),
            }
        }
        Ok(())
    }

    /// Remove `[iova, iova + len)` of `domain`, exactly as earlier maps
    /// installed it: whole mappings, back to back. The device may still hold
    /// a cached translation until the next [`Self::sync`].
    ///
    /// # Errors
    ///
    /// [`IommuError::NotMapped`], [`IommuError::Split`], or
    /// [`IommuError::OutOfRange`].
    fn unmap(&self, domain: DomainId, iova: u64, len: u64) -> Result<(), IommuError>;

    /// Confirm that every translation `domain` lost since its last sync is
    /// gone from every cache, and free the tables those unmaps emptied.
    ///
    /// # Errors
    ///
    /// [`IommuError::Unconfirmed`], after which nothing it covered may be
    /// reused.
    fn sync(&self, domain: DomainId) -> Result<(), IommuError>;

    /// [`Self::sync`] for the unmaps within `[iova, iova + len)` alone,
    /// freeing the tables they emptied: what a unit can invalidate by
    /// address keeps the domain's other cached translations. The default
    /// confirms the whole domain.
    ///
    /// # Errors
    ///
    /// As [`Self::sync`].
    fn sync_range(&self, domain: DomainId, iova: u64, len: u64) -> Result<(), IommuError> {
        let _ = (iova, len);
        self.sync(domain)
    }

    /// Raise the unit's fault interrupt by `route`, and unmask it.
    ///
    /// # Errors
    ///
    /// [`IommuError::OutOfRange`] for a route the unit has no means to raise
    /// by, or the unit's refusal.
    fn route_faults(&self, route: FaultRoute) -> Result<(), IommuError>;

    /// Stop raising the fault interrupt: once this returns, the unit raises
    /// none until [`Self::route_faults`] routes it again. Its records stay
    /// queued.
    ///
    /// # Errors
    ///
    /// The unit's refusal, after which it may still raise the interrupt.
    fn unroute_faults(&self) -> Result<(), IommuError>;

    /// Hand the fault records the unit holds to `sink`, oldest first, clearing
    /// each, and answer whether any remain. A call may stop short, so a
    /// storming device cannot hold the caller; while it answers `true` the
    /// caller drains again rather than wait for the fault interrupt, which a
    /// unit need not raise for records it already holds.
    fn drain_faults(&self, sink: &mut dyn FnMut(Fault)) -> bool;

    /// Hand `sink` each page-aligned IOVA range no domain translating
    /// `stream` may map: what the unit claims for the stream itself (a
    /// virtio-iommu's probed reserved regions, the input range it
    /// translates). [`UnitProfile::reserved`] holds what every stream shares.
    ///
    /// # Errors
    ///
    /// [`IommuError::OutOfRange`] for a stream the unit does not cover, or
    /// the unit's refusal or timeout: a domain is then not made for it.
    fn reserved_iova(
        &self,
        stream: u32,
        sink: &mut dyn FnMut(Range<u64>),
    ) -> Result<(), IommuError> {
        let _ = (stream, sink);
        Ok(())
    }

    /// The unit's interrupt remapping, where it has it.
    fn interrupt_remapping(&self) -> Option<&dyn InterruptRemapping> {
        None
    }

    /// The unit's confinement of messages to interrupt files, where it can
    /// confine them.
    fn message_files(&self) -> Option<&dyn MessageFiles> {
        None
    }
}

/// How a unit signals its interrupts, for a family that must be told as the
/// unit is taken over: before its queues run, after which it may not change.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Signalling {
    /// On its wires.
    Wired,
    /// As messages.
    Message,
}

/// How a unit raises its fault interrupt.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum FaultRoute {
    /// As the message-signalled interrupt `address`/`data`.
    Message {
        /// The message's address.
        address: u64,
        /// The message's data.
        data: u32,
    },
    /// On the wired line its node names for its faults, the interrupt at
    /// `place` among the node's: a unit that chooses which of its lines
    /// raises a cause raises its faults there.
    Wired {
        /// The line's place among its node's interrupts.
        place: u32,
    },
}

/// Writes table memory back for a unit whose walker does not snoop the CPU's
/// caches.
pub trait TableCoherence: Sync {
    /// Write the `len` bytes at physical `phys` back to memory, complete
    /// before this returns.
    fn write_back(&self, phys: u64, len: usize);
}

/// The PCI function a unit is, which the kernel owns the configuration
/// space of: an AMD-Vi unit, whose fault interrupt is its function's MSI, or
/// a virtio-iommu, whose queues are its function's DMA. Every method names
/// the function by its node address, `(segment << 16) | requester id`.
pub trait UnitFunction: Sync {
    /// Raise the MSI of the function at `address` as
    /// `message_address`/`data`. A unit's own interrupt needs no bus
    /// mastering, so none is granted.
    ///
    /// # Errors
    ///
    /// [`IommuError::Hardware`] where the function cannot be programmed.
    fn route_msi(&self, address: u32, message_address: u64, data: u32) -> Result<(), IommuError>;

    /// Raise MSI-X table entry `entry` of the function at `address` as
    /// `message_address`/`data`, unmasked, with MSI-X on.
    ///
    /// # Errors
    ///
    /// [`IommuError::Hardware`] where the function cannot be programmed.
    fn route_msix(
        &self,
        address: u32,
        entry: u16,
        message_address: u64,
        data: u32,
    ) -> Result<(), IommuError>;

    /// How many MSI-X entries the function at `address` has: none for one
    /// that can raise no MSI-X message.
    ///
    /// # Errors
    ///
    /// [`IommuError::Hardware`] where its capabilities cannot be read.
    fn msix_entries(&self, address: u32) -> Result<u16, IommuError>;

    /// Set or clear the MSI-X function mask of the function at `address`:
    /// masked, it raises no MSI-X message until routed again.
    ///
    /// # Errors
    ///
    /// [`IommuError::Hardware`] where the function cannot be programmed or
    /// does not read back as asked.
    fn mask_msix(&self, address: u32, masked: bool) -> Result<(), IommuError>;

    /// The first dword of capability `id` of the function at `address`, or
    /// [`None`] where it lists none.
    ///
    /// # Errors
    ///
    /// [`IommuError::Hardware`] where its capabilities cannot be read.
    fn capability_header(&self, address: u32, id: u8) -> Result<Option<u32>, IommuError>;

    /// Turn the bus mastering of the function at `address` on or off: a
    /// unit's own, for one whose queues are its function's DMA.
    ///
    /// # Errors
    ///
    /// [`IommuError::Hardware`] where the function cannot be programmed or
    /// does not read back as asked.
    fn set_master(&self, address: u32, master: bool) -> Result<(), IommuError>;

    /// Let the function at `address` raise its INTx pin, or stop it.
    ///
    /// # Errors
    ///
    /// [`IommuError::Hardware`] where the function cannot be programmed.
    fn set_intx(&self, address: u32, raise: bool) -> Result<(), IommuError>;

    /// Where the register blocks of the virtio function at `address` lie,
    /// read from its virtio capabilities, with no MSI-X entry chosen.
    ///
    /// # Errors
    ///
    /// [`IommuError::OutOfRange`] for a function that is no modern virtio
    /// device, and [`IommuError::Hardware`] where it cannot be read.
    fn virtio_windows(&self, address: u32) -> Result<crate::VirtioPciWindows, IommuError>;
}

/// A monotonic clock a family bounds its waits against.
pub trait Clock: Sync {
    /// Nanoseconds since an arbitrary fixed point.
    fn now_ns(&self) -> u64;
}
