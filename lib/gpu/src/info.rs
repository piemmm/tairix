//! What a device is: its memory heaps and types, its queue families, and its
//! limits.

use tairix_abi::Duration64;
use tairix_inline::ArrayVec;

use crate::Error;

tairix_util::flags! {
    /// What a memory type's memory is.
    pub struct MemoryProperties(u32) {
        /// The device reaches it fastest.
        const DEVICE_LOCAL = 1;
        /// The CPU can read and write it.
        const HOST_VISIBLE = 1 << 1;
        /// What the CPU writes the device sees, and the other way, with no
        /// flush between.
        const HOST_COHERENT = 1 << 2;
        /// The CPU's reads of it are cached.
        const HOST_CACHED = 1 << 3;
    }
}

tairix_util::flags! {
    /// What a buffer may be used for.
    pub struct BufferUsage(u32) {
        /// The source of a copy.
        const TRANSFER_SRC = 1;
        /// The destination of a copy, a fill or an update.
        const TRANSFER_DST = 1 << 1;
    }
}

impl BufferUsage {
    /// Every usage a buffer may declare.
    pub const ALL: Self = Self::TRANSFER_SRC.with(Self::TRANSFER_DST);
}

tairix_util::flags! {
    /// The work a queue family runs.
    pub struct QueueCapabilities(u32) {
        /// Copies, fills and updates.
        const TRANSFER = 1;
    }
}

/// The most heaps a device describes.
pub const MAX_MEMORY_HEAPS: usize = 16;

/// The most memory types a device describes.
pub const MAX_MEMORY_TYPES: usize = 32;

/// The most queue families a device describes.
pub const MAX_QUEUE_FAMILIES: usize = 16;

/// The most queues of one family a context has.
pub const MAX_QUEUES_PER_FAMILY: u32 = 64;

/// A pool of memory every allocation of the types over it comes out of.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct MemoryHeap {
    /// What the heap holds, in bytes.
    pub size: u64,
    /// Whether the heap is the device's own memory rather than the CPU's.
    pub device_local: bool,
}

/// A kind of memory an allocation is made in.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct MemoryType {
    /// The heap it is taken from, an index into [`DeviceInfo::heaps`].
    pub heap: u32,
    /// What the memory is.
    pub properties: MemoryProperties,
}

/// A set of queues running the same kind of work on one engine of the
/// device.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct QueueFamily {
    /// The work its queues run.
    pub capabilities: QueueCapabilities,
    /// How many queues of the family each context has.
    pub queues: u32,
}

/// The bounds a device puts on what is asked of it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Limits {
    /// The largest single allocation.
    pub max_allocation_size: u64,
    /// What a buffer's offset into its allocation must be a multiple of: a
    /// power of two.
    pub buffer_alignment: u64,
    /// The longest a submission may run before its context is lost.
    pub max_job_time: Duration64,
}

/// What a device is.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeviceInfo {
    /// Its memory heaps.
    pub heaps: ArrayVec<MemoryHeap, MAX_MEMORY_HEAPS>,
    /// The kinds of memory it allocates, each over one of [`Self::heaps`].
    pub memory_types: ArrayVec<MemoryType, MAX_MEMORY_TYPES>,
    /// Its queue families.
    pub queue_families: ArrayVec<QueueFamily, MAX_QUEUE_FAMILIES>,
    /// The bounds it puts on what is asked of it.
    pub limits: Limits,
}

impl DeviceInfo {
    /// Refuse a description no context could be served by.
    pub(crate) fn check(&self) -> Result<(), Error> {
        let heaps = self.heaps.len();
        let typed = self
            .memory_types
            .iter()
            .all(|kind| usize::try_from(kind.heap).is_ok_and(|heap| heap < heaps));
        let sized = self.heaps.iter().all(|heap| heap.size > 0);
        let served = !self.queue_families.is_empty()
            && self.queue_families.iter().all(|family| {
                (1..=MAX_QUEUES_PER_FAMILY).contains(&family.queues)
                    && family.capabilities != QueueCapabilities::NONE
            });
        let limits = &self.limits;
        let bounded = limits.max_allocation_size > 0
            && limits.buffer_alignment.is_power_of_two()
            && limits.max_job_time.saturating_total_nanos() > 0;
        if typed && sized && served && bounded && !self.memory_types.is_empty() {
            Ok(())
        } else {
            Err(Error::Malformed)
        }
    }

    /// The memory type `index` names.
    pub(crate) fn memory_type(&self, index: u32) -> Result<MemoryType, Error> {
        usize::try_from(index)
            .ok()
            .and_then(|index| self.memory_types.get(index))
            .copied()
            .ok_or(Error::NoSuchMemoryType)
    }

    /// The flat index of queue `index` of family `family` among a context's
    /// queues, which are laid out family by family.
    pub(crate) fn queue_slot(&self, family: u32, index: u32) -> Result<usize, Error> {
        let family = usize::try_from(family).map_err(|_| Error::NoSuchQueue)?;
        let preceding = self
            .queue_families
            .get(..family)
            .ok_or(Error::NoSuchQueue)?;
        let kind = self.queue_families.get(family).ok_or(Error::NoSuchQueue)?;
        if index >= kind.queues {
            return Err(Error::NoSuchQueue);
        }
        let before = preceding.iter().fold(0usize, |total, family| {
            total.saturating_add(family.queues as usize)
        });
        Ok(before.saturating_add(index as usize))
    }
}
