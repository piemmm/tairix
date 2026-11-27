//! What a device is: its memory heaps and types, its queue families, its
//! limits, and what it states it supports past the portable core.

use tairix_abi::Duration64;
use tairix_inline::ArrayVec;
use tairix_spirv::enumerant::Capability;
use tairix_spirv::{Admitted, Version};

use crate::format::{Format, FormatFeatures};
use crate::image::{Extent3d, Origin3d};
use crate::{Error, MAX_DRAW_VERTEX_BUFFERS, MAX_PASS_ATTACHMENTS, MAX_VERTEX_ATTRIBUTES};

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
        /// Read as an indirect dispatch's or draw's arguments.
        const INDIRECT = 1 << 6;
        /// Read as a draw's vertex attributes.
        const VERTEX = 1 << 7;
        /// Read as a draw's indices.
        const INDEX = 1 << 8;
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
        .with(Self::INDIRECT)
        .with(Self::VERTEX)
        .with(Self::INDEX);
}

tairix_util::flags! {
    /// The work a queue family runs.
    pub struct QueueCapabilities(u32) {
        /// Copies, fills and updates, and colour clears where the family is
        /// no copy engine ([`QueueFamily::clears_colour`]).
        const TRANSFER = 1;
        /// Dispatches.
        const COMPUTE = 1 << 1;
        /// Render passes and their draws.
        const GRAPHICS = 1 << 2;
        /// Depth and stencil planes reached by its transfers — cleared,
        /// copied to or from a buffer, or copied between multisampled
        /// images — as only an engine that draws reaches them, whether or
        /// not the device states its draws
        /// ([`QueueFamily::transfers_depth_stencil`]).
        const DEPTH_STENCIL = 1 << 3;
    }
}

impl QueueCapabilities {
    /// Every kind of work.
    pub const ALL: Self = Self::TRANSFER
        .with(Self::COMPUTE)
        .with(Self::GRAPHICS)
        .with(Self::DEPTH_STENCIL);
}

tairix_util::flags! {
    /// What a device offers past the portable core.
    pub struct Features(u32) {
        /// Anisotropic filtering.
        const SAMPLER_ANISOTROPY = 1;
        /// The mirror-clamp-to-edge address mode.
        const MIRROR_CLAMP_TO_EDGE = 1 << 1;
        /// Blend factors that read a fragment's second colour.
        const DUAL_SOURCE_BLEND = 1 << 2;
        /// A logic operation in place of blending.
        const LOGIC_OP = 1 << 3;
        /// Depth clamped to the viewport's range in place of clipped.
        const DEPTH_CLAMP = 1 << 4;
        /// Lines wider than a pixel.
        const WIDE_LINES = 1 << 5;
        /// Points larger than a pixel.
        const LARGE_POINTS = 1 << 6;
        /// Storage writes and atomics from a vertex stage.
        const VERTEX_STORES = 1 << 7;
        /// Storage writes and atomics from a fragment stage.
        const FRAGMENT_STORES = 1 << 8;
        /// An indirect draw's records naming a first instance other than
        /// nought.
        const DRAW_INDIRECT_FIRST_INSTANCE = 1 << 9;
        /// Bindings of more than one descriptor, each a shader names at a
        /// constant index.
        const BINDING_ARRAYS = 1 << 10;
        /// Memory one context exports and another of the device imports.
        const SHARED_MEMORY = 1 << 11;
    }
}

impl Features {
    /// Every feature.
    pub const ALL: Self = Self::SAMPLER_ANISOTROPY
        .with(Self::MIRROR_CLAMP_TO_EDGE)
        .with(Self::DUAL_SOURCE_BLEND)
        .with(Self::LOGIC_OP)
        .with(Self::DEPTH_CLAMP)
        .with(Self::WIDE_LINES)
        .with(Self::LARGE_POINTS)
        .with(Self::VERTEX_STORES)
        .with(Self::FRAGMENT_STORES)
        .with(Self::DRAW_INDIRECT_FIRST_INSTANCE)
        .with(Self::BINDING_ARRAYS)
        .with(Self::SHARED_MEMORY);
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
///
/// What a family runs is Vulkan's meaning of its capabilities, which the
/// model holds every device to: a family that only transfers — a copy engine
/// — copies, fills and updates, its image copies kept to its
/// [`Granularity`] and its buffers' side of them to whole words, and clears
/// nothing.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct QueueFamily {
    /// The work its queues run.
    pub capabilities: QueueCapabilities,
    /// How many queues of the family each context has.
    pub queues: u32,
    /// The least its image copies move: [`Granularity::TEXEL`] for any family
    /// but a copy engine.
    pub granularity: Granularity,
}

impl QueueFamily {
    /// Whether what it states of depth and stencil holds together: they are
    /// reached only by transfers, and a family that draws and transfers
    /// reaches them, its engine being one that draws.
    const fn reaches_depth_stencil_as_it_draws(&self) -> bool {
        let runs = self.capabilities;
        let transfers = runs.contains(QueueCapabilities::TRANSFER);
        let reaches = runs.contains(QueueCapabilities::DEPTH_STENCIL);
        (!reaches || transfers)
            && (!(transfers && runs.contains(QueueCapabilities::GRAPHICS)) || reaches)
    }

    /// Whether it is a copy engine: a family whose engine neither computes
    /// nor draws.
    #[must_use]
    pub const fn copies_alone(&self) -> bool {
        !self.capabilities.intersects(
            QueueCapabilities::COMPUTE
                .with(QueueCapabilities::GRAPHICS)
                .with(QueueCapabilities::DEPTH_STENCIL),
        )
    }

    /// Whether it clears colour images: a family that transfers and is no
    /// copy engine.
    #[must_use]
    pub const fn clears_colour(&self) -> bool {
        self.capabilities.contains(QueueCapabilities::TRANSFER) && !self.copies_alone()
    }

    /// Whether its transfers reach a depth or stencil plane past copies
    /// between single-sampled images: copying one to or from a buffer or
    /// between multisampled images, or clearing one.
    #[must_use]
    pub const fn transfers_depth_stencil(&self) -> bool {
        self.capabilities
            .contains(QueueCapabilities::TRANSFER.with(QueueCapabilities::DEPTH_STENCIL))
    }
}

/// The least a queue family's image copies move along each axis of a level.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Granularity {
    /// Blocks of `1 << log2[axis]` texels: each offset whole blocks, and each
    /// extent whole blocks too or reaching the level's edge.
    Blocks {
        /// Each axis's block, as a power of two, at most [`MAX_GRANULARITY_LOG2`].
        log2: [u8; 3],
    },
    /// Whole levels alone: each offset nought, each extent the level's.
    Levels,
}

impl Granularity {
    /// A texel at a time.
    pub const TEXEL: Self = Self::Blocks { log2: [0; 3] };

    /// Whether a family may state it: only a copy engine copies more than
    /// single texels.
    fn fits(self, copy_engine: bool) -> bool {
        match self {
            Self::Levels => copy_engine,
            Self::Blocks { log2 } if copy_engine => {
                log2.iter().all(|&log2| log2 <= MAX_GRANULARITY_LOG2)
            }
            Self::Blocks { .. } => self == Self::TEXEL,
        }
    }

    /// Whether a copy on its family moves the region `origin` and `extent`
    /// describe of a level `level` texels in size.
    pub(crate) fn admits(self, origin: Origin3d, extent: Extent3d, level: Extent3d) -> bool {
        match self {
            Self::Levels => origin == Origin3d::default() && extent == level,
            Self::Blocks { log2 } => {
                let whole = |start: u32, length: u32, edge: u32, log2: u8| {
                    1u32.checked_shl(u32::from(log2)).is_some_and(|block| {
                        start.is_multiple_of(block)
                            && (length.is_multiple_of(block)
                                || start.checked_add(length) == Some(edge))
                    })
                };
                whole(origin.x, extent.width, level.width, log2[0])
                    && whole(origin.y, extent.height, level.height, log2[1])
                    && whole(origin.z, extent.depth, level.depth, log2[2])
            }
        }
    }
}

/// The widest block a [`Granularity`] states, as a power of two: the widest
/// a texel offset's word holds.
pub const MAX_GRANULARITY_LOG2: u8 = 31;

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
    /// The furthest a sampler's level-of-detail bias moves the level sampled
    /// either way: at most [`MAX_LOD_BIAS`](crate::MAX_LOD_BIAS).
    pub max_sampler_lod_bias: u32,
    /// The most bind sets a pipeline layout holds.
    pub max_bind_sets: u32,
    /// One past the highest binding number a bind set layout names: at most
    /// [`MAX_BINDING_NUMBER`].
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
    /// What a uniform buffer binding's size must be a multiple of: a power of
    /// two, the granularity the device bounds a binding's reads at, so a read
    /// past the range reads zero rather than what lies beyond it.
    pub uniform_buffer_size_alignment: u64,
    /// What a storage buffer binding's size must be a multiple of, as
    /// [`Self::uniform_buffer_size_alignment`] is a uniform buffer's.
    pub storage_buffer_size_alignment: u64,
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
    /// The most vertex attributes a render pipeline reads, and one past the
    /// highest location one feeds: at most
    /// [`MAX_VERTEX_ATTRIBUTES`].
    pub max_vertex_input_attributes: u32,
    /// The most vertex buffers a render pipeline reads: at most
    /// [`MAX_DRAW_VERTEX_BUFFERS`].
    pub max_vertex_input_bindings: u32,
    /// The furthest an attribute starts into its vertex.
    pub max_vertex_input_attribute_offset: u32,
    /// The longest a vertex buffer's stride is.
    pub max_vertex_input_binding_stride: u32,
    /// The most 32-bit components a vertex stage's outputs hold together.
    pub max_vertex_output_components: u32,
    /// The most 32-bit components a fragment stage's inputs hold together.
    pub max_fragment_input_components: u32,
    /// The most colour targets a render pipeline writes and a pass holds: at
    /// most [`MAX_PASS_ATTACHMENTS`].
    pub max_color_attachments: u32,
    /// The widest and tallest viewport.
    pub max_viewport_dimensions: [u32; 2],
    /// The lowest and highest a viewport's edges reach, in order.
    pub viewport_bounds_range: [i32; 2],
    /// The largest point's side, in pixels, where points may be large.
    pub max_point_size: u32,
    /// The widest line, in pixels, where lines may be wide.
    pub max_line_width: u32,
    /// The most records one indirect draw reads: one where the device draws
    /// no more.
    pub max_draw_indirect_count: u32,
}

/// The most shader capabilities a device states.
pub const MAX_CAPABILITIES: usize = 64;

/// One past the highest binding number any device takes: a binding's number
/// sizes the tables a layout of it is looked up in.
pub const MAX_BINDING_NUMBER: u32 = 1024;

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
    /// The newest SPIR-V version a module it runs may declare.
    pub spirv_version: Version,
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
                    && family.reaches_depth_stencil_as_it_draws()
                    && family.granularity.fits(family.copies_alone())
            });
        let limits = &self.limits;
        let bounded = limits.max_allocation_size > 0
            && limits.buffer_alignment.is_power_of_two()
            && limits.uniform_buffer_alignment.is_power_of_two()
            && limits.storage_buffer_alignment.is_power_of_two()
            && limits.uniform_buffer_size_alignment.is_power_of_two()
            && limits.storage_buffer_size_alignment.is_power_of_two()
            && limits.max_job_time.saturating_total_nanos() > 0
            && limits.subgroup_size.is_power_of_two()
            && limits.subgroup_size <= MAX_SUBGROUP_SIZE
            && limits.max_bind_sets > 0
            && limits.max_binding_number <= MAX_BINDING_NUMBER
            && limits.max_sampler_lod_bias <= crate::MAX_LOD_BIAS
            && limits.max_draw_indirect_count > 0
            && limits.max_vertex_input_attributes <= MAX_VERTEX_ATTRIBUTES
            && limits.max_vertex_input_bindings as usize <= MAX_DRAW_VERTEX_BUFFERS
            && limits.max_color_attachments as usize <= MAX_PASS_ATTACHMENTS
            && limits.viewport_bounds_range[0] <= limits.viewport_bounds_range[1];
        let meant = Format::ALL
            .iter()
            .all(|&format| format.features().contains(self.formats.get(format)));
        let ordered = self.capabilities.windows(2).all(|w| w[0] < w[1]);
        let versioned = (Version::OLDEST..=Version::NEWEST).contains(&self.spirv_version);
        if typed
            && sized
            && served
            && bounded
            && meant
            && ordered
            && versioned
            && !self.memory_types.is_empty()
        {
            Ok(())
        } else {
            Err(Error::Malformed)
        }
    }

    /// Its families' engines, each holding `depth` jobs at once: the depths a
    /// backend running every family alike states.
    #[must_use]
    pub fn engines(&self, depth: usize) -> ArrayVec<usize, MAX_QUEUE_FAMILIES> {
        let mut engines = ArrayVec::new();
        for _ in &self.queue_families {
            // Within the bound the families are held to.
            let _ = engines.try_push(depth);
        }
        engines
    }

    /// Whether it runs modules declaring `capability`.
    #[must_use]
    pub fn runs(&self, capability: Capability) -> bool {
        self.capabilities.binary_search(&capability).is_ok()
    }

    /// Whether it runs `module`: every capability it declares, at a version
    /// it takes.
    #[must_use]
    pub fn runs_module(&self, module: &Admitted) -> bool {
        module.module().version() <= self.spirv_version
            && module
                .capabilities()
                .iter()
                .all(|&capability| self.runs(capability))
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
