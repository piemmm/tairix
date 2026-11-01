//! What a device is: its memory heaps and types, its queue families, its
//! limits, and what it states it supports past the portable core.

use tairix_abi::Duration64;
use tairix_inline::ArrayVec;
use tairix_spirv::enumerant::Capability;

use crate::format::{Format, FormatFeatures};
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
        /// Bound as a uniform buffer.
        const UNIFORM = 1 << 2;
        /// Bound as a storage buffer.
        const STORAGE = 1 << 3;
        /// Viewed as a uniform texel buffer.
        const UNIFORM_TEXEL = 1 << 4;
        /// Viewed as a storage texel buffer.
        const STORAGE_TEXEL = 1 << 5;
        /// Read as an indirect dispatch's arguments.
        const INDIRECT = 1 << 6;
    }
}

impl BufferUsage {
    /// Every usage a buffer may declare.
    pub const ALL: Self = Self::TRANSFER_SRC
        .with(Self::TRANSFER_DST)
        .with(Self::UNIFORM)
        .with(Self::STORAGE)
        .with(Self::UNIFORM_TEXEL)
        .with(Self::STORAGE_TEXEL)
        .with(Self::INDIRECT);
}

tairix_util::flags! {
    /// The work a queue family runs.
    pub struct QueueCapabilities(u32) {
        /// Copies, fills, updates and clears.
        const TRANSFER = 1;
        /// Dispatches.
        const COMPUTE = 1 << 1;
    }
}

impl QueueCapabilities {
    /// Every kind of work.
    pub const ALL: Self = Self::TRANSFER.with(Self::COMPUTE);
}

tairix_util::flags! {
    /// What a device offers past the portable core.
    pub struct Features(u32) {
        /// Anisotropic filtering.
        const SAMPLER_ANISOTROPY = 1;
        /// The mirror-clamp-to-edge address mode.
        const MIRROR_CLAMP_TO_EDGE = 1 << 1;
    }
}

impl Features {
    /// Every feature.
    pub const ALL: Self = Self::SAMPLER_ANISOTROPY.with(Self::MIRROR_CLAMP_TO_EDGE);
}

/// What a device states each format can be used for.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FormatTable([FormatFeatures; Format::ALL.len()]);

impl FormatTable {
    /// A table stating, of each format, what `features` says.
    #[must_use]
    pub fn from_fn(features: impl Fn(Format) -> FormatFeatures) -> Self {
        Self(Format::ALL.map(features))
    }

    /// What the device states `format` can be used for.
    #[must_use]
    pub fn get(&self, format: Format) -> FormatFeatures {
        self.0
            .get(format.index())
            .copied()
            .unwrap_or(FormatFeatures::NONE)
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

/// The widest subgroup a device states: a tile's lanes.
pub const MAX_SUBGROUP_SIZE: u32 = 64;

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
    /// The widest one-dimensional image.
    pub max_image_dimension_1d: u32,
    /// The widest and tallest two-dimensional image, cube or attachment.
    pub max_image_dimension_2d: u32,
    /// The widest, tallest and deepest three-dimensional image.
    pub max_image_dimension_3d: u32,
    /// The most layers an image has.
    pub max_image_array_layers: u32,
    /// The most anisotropy a sampler filters with.
    pub max_sampler_anisotropy: u32,
    /// The most bind sets a pipeline layout holds.
    pub max_bind_sets: u32,
    /// One past the highest binding number a bind set layout names.
    pub max_binding_number: u32,
    /// The most descriptors a bind set layout holds, its bindings' together.
    pub max_bind_set_descriptors: u32,
    /// The most push-constant bytes a pipeline layout holds.
    pub max_push_constant_bytes: u32,
    /// What a uniform buffer binding's offset must be a multiple of: a power
    /// of two.
    pub uniform_buffer_alignment: u64,
    /// What a storage buffer or texel buffer binding's offset must be a
    /// multiple of: a power of two.
    pub storage_buffer_alignment: u64,
    /// The most bytes one uniform buffer binding spans.
    pub max_uniform_buffer_range: u64,
    /// The most bytes one storage buffer binding spans.
    pub max_storage_buffer_range: u64,
    /// The most bytes an image made for storage takes, every level and
    /// layer together.
    pub max_storage_image_bytes: u64,
    /// The most texels one texel buffer binding spans.
    pub max_texel_buffer_elements: u64,
    /// The largest workgroup along each axis.
    pub max_workgroup_size: [u32; 3],
    /// The most invocations a workgroup holds.
    pub max_workgroup_invocations: u32,
    /// The most workgroups a dispatch makes along each axis.
    pub max_workgroup_count: [u32; 3],
    /// The most bytes of workgroup memory a workgroup holds.
    pub max_workgroup_memory: u32,
    /// The most bytes of registers and private memory one invocation holds.
    pub max_invocation_memory: u32,
    /// The invocations a subgroup holds.
    pub subgroup_size: u32,
}

/// The most shader capabilities a device states.
pub const MAX_CAPABILITIES: usize = 64;

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
    /// The shader capabilities it runs, by ascending value.
    pub capabilities: ArrayVec<Capability, MAX_CAPABILITIES>,
    /// What it offers past the portable core.
    pub features: Features,
    /// What it states each format can be used for.
    pub formats: FormatTable,
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
                    && QueueCapabilities::ALL.contains(family.capabilities)
            });
        let limits = &self.limits;
        let bounded = limits.max_allocation_size > 0
            && limits.buffer_alignment.is_power_of_two()
            && limits.uniform_buffer_alignment.is_power_of_two()
            && limits.storage_buffer_alignment.is_power_of_two()
            && limits.max_job_time.saturating_total_nanos() > 0
            && limits.subgroup_size.is_power_of_two()
            && limits.subgroup_size <= MAX_SUBGROUP_SIZE
            && limits.max_bind_sets > 0;
        let ordered = self.capabilities.windows(2).all(|w| w[0] < w[1]);
        if typed && sized && served && bounded && ordered && !self.memory_types.is_empty() {
            Ok(())
        } else {
            Err(Error::Malformed)
        }
    }

    /// Whether it runs modules declaring `capability`.
    #[must_use]
    pub fn runs(&self, capability: Capability) -> bool {
        self.capabilities.binary_search(&capability).is_ok()
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
