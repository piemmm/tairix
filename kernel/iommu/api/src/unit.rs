//! The contract one hardware family implements for each translation unit it
//! drives.

use core::ops::Range;

use crate::fault::Fault;

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
}

/// A unit-local domain handle.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct DomainId(pub u32);

/// What a unit reports it can do.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct UnitProfile {
    /// Bits of IOVA its domains translate.
    pub input_bits: u32,
    /// Bits of physical address its tables can name.
    pub output_bits: u32,
    /// IOVA windows no domain may map, because the fabric claims them before
    /// translation (the x86 interrupt window).
    pub reserved: &'static [Range<u64>],
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
    /// unknown domain.
    fn attach(&self, stream: u32, domain: DomainId) -> Result<(), IommuError>;

    /// Block `stream`'s DMA and confirm no cached translation of it survives.
    /// Blocking a blocked stream is a no-op.
    ///
    /// # Errors
    ///
    /// [`IommuError::OutOfRange`] for a stream the unit does not cover, or
    /// [`IommuError::Unconfirmed`].
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
    /// [`IommuError::AlreadyMapped`], [`IommuError::OutOfRange`], or
    /// [`IommuError::Exhausted`]. A failed map leaves nothing of itself
    /// behind but what the next [`Self::sync`] removes from the caches.
    fn map(
        &self,
        domain: DomainId,
        iova: u64,
        phys: u64,
        len: u64,
        access: Access,
    ) -> Result<(), IommuError>;

    /// Remove `[iova, iova + len)` of `domain`, exactly as one earlier map
    /// installed it. The device may still hold a cached translation until the
    /// next [`Self::sync`].
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

    /// Hand the fault records the unit holds to `sink`, oldest first, clearing
    /// each, and answer whether any remain. A call may stop short, so a
    /// storming device cannot hold the caller; while it answers `true` the
    /// caller drains again rather than wait for the fault interrupt, which a
    /// unit need not raise for records it already holds.
    fn drain_faults(&self, sink: &mut dyn FnMut(Fault)) -> bool;
}

/// Writes table memory back for a unit whose walker does not snoop the CPU's
/// caches.
pub trait TableCoherence: Sync {
    /// Write the `len` bytes at physical `phys` back to memory.
    fn write_back(&self, phys: u64, len: usize);
}

/// A monotonic clock a family bounds its waits against.
pub trait Clock: Sync {
    /// Nanoseconds since an arbitrary fixed point.
    fn now_ns(&self) -> u64;
}
