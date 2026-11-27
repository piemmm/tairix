//! What a Venus device states: the host's statement narrowed to what the
//! backend runs — memory, buffers, images, texel buffers, samplers, transfers
//! and dispatches — and held to the seam's bounds, and what the device is made
//! with to run it.
//!
//! A host-visible memory type is stated only where it is coherent, since the
//! renderer maps nothing and flushes nothing; a type of any property the seam
//! does not name is left out, as is a heap of no bytes. The family stated
//! first runs transfers and dispatches, so the model refuses a draw before
//! the backend is asked: one running graphics work too where the host has
//! one, as no other copies or clears depth and stencil. Beside it is the
//! host's copy engine where it has one: a family running transfers alone
//! (`plans/GPU.md` GD7). A shader
//! capability other than a subgroup one is stated where the host runs it, and
//! the feature it needs is enabled, as is each sampler feature stated. A limit the host states is its value, held
//! to the seam's bound; one it has no counterpart for gates only work the
//! stated device does not do, and is stated as none.
//!
//! The seam's one semantics for what a program reads past what it was given
//! or before it wrote it needs the host's second robustness level and its
//! zero-initialized workgroup memory: a host without either serves nothing. A
//! format's use by a shader through an image is stated only where the host's
//! robust image access holds an image's reads past its edges to the seam's
//! zeros, and any use of an image only where the host copies it both ways. A
//! depth copied into a float depth format keeps its bits on the seam, which
//! a host keeps outside `0.0..=1.0` only with its unrestricted depth range: a
//! float depth format is stated only there.
//!
//! A host states the format features Vulkan 1.3 added from that version or
//! with their extension: a depth sampled only where it is also compared, and
//! storage accessed through a binding naming no format only where the format
//! says so. An older host compares every depth it samples, and its device
//! features grant unformatted storage access to every format the seam stores.

use tairix_abi::{DriverError, Duration64};
use tairix_gpu::{
    Aspects, DeviceInfo, Features, Format, FormatFeatures, FormatTable, Granularity, Limits,
    MemoryHeap, MemoryProperties, MemoryRequirements, MemoryType, QueueCapabilities, QueueFamily,
    MAX_BINDING_NUMBER, MAX_CAPABILITIES, MAX_DRAW_VERTEX_BUFFERS, MAX_LOD_BIAS,
    MAX_PASS_ATTACHMENTS, MAX_PUSH_BYTES, MAX_QUEUES_PER_FAMILY, MAX_SUBGROUP_SIZE,
    MAX_VERTEX_ATTRIBUTES,
};
use tairix_inline::ArrayVec;
use tairix_spirv::enumerant::Capability;
use tairix_spirv::Version;
use tairix_util::mathf;

use super::encode::{Chained, Sharing};
use super::reply::{
    HostExtensions, HostFeatures, HostFormatFeatures, HostImageLimits, HostMemory, HostProperties,
    HostQueueFamily, HostRequirements, RobustAlignments, ASKED, MAX_CHAINED, MAX_CHAINED_MEMBERS,
};
use super::{
    extension, format_feature, memory, queue, Structure, API_VERSION, CORE_FEATURES,
    HEAP_DEVICE_LOCAL, MAX_MEMORY_HEAPS, MAX_MEMORY_TYPES, MAX_QUEUE_RINGS, MIN_API_VERSION,
};

/// The most records one indirect draw reads on a device stating no first
/// instance, whose records are checked in a slot of the feedback memory.
pub(crate) const CHECKED_RECORDS: u32 = 256;

/// The longest a job may run before its context is lost: the renderer cannot
/// stop a job once started, so this is how long one holds its queue.
const MAX_JOB_TIME: Duration64 = Duration64::from_secs(10);

/// The widest alignment Vulkan lets a device require of a buffer binding's
/// offset, or bound its robust accesses at.
const MAX_BINDING_ALIGNMENT: u64 = 256;

/// What a buffer's start keeps for the fills and copies the host runs at an
/// offset into it: a fill's word, and a whole number of every texel a copy
/// moves.
const COPY_ALIGNMENT: u32 = {
    let mut alignment = 4;
    let mut at = 0;
    while at < Format::ALL.len() {
        if let Some(texel) = Format::ALL[at].copy_bytes(Aspects::COLOR) {
            // So the widest is a whole number of every other.
            assert!(texel.is_power_of_two(), "a texel of a power of two bytes");
            if texel > alignment {
                alignment = texel;
            }
        }
        at += 1;
    }
    alignment
};

/// The places among the core features of those the driver enables.
mod core {
    pub(super) const ROBUST_BUFFER_ACCESS: usize = 0;
    pub(super) const FULL_DRAW_INDEX_UINT32: usize = 1;
    pub(super) const IMAGE_CUBE_ARRAY: usize = 2;
    pub(super) const INDEPENDENT_BLEND: usize = 3;
    pub(super) const SAMPLE_RATE_SHADING: usize = 6;
    pub(super) const DUAL_SRC_BLEND: usize = 7;
    pub(super) const LOGIC_OP: usize = 8;
    pub(super) const MULTI_DRAW_INDIRECT: usize = 9;
    pub(super) const DRAW_INDIRECT_FIRST_INSTANCE: usize = 10;
    pub(super) const DEPTH_CLAMP: usize = 11;
    pub(super) const DEPTH_BIAS_CLAMP: usize = 12;
    pub(super) const WIDE_LINES: usize = 15;
    pub(super) const LARGE_POINTS: usize = 16;
    pub(super) const SAMPLER_ANISOTROPY: usize = 19;
    pub(super) const VERTEX_PIPELINE_STORES_AND_ATOMICS: usize = 25;
    pub(super) const FRAGMENT_STORES_AND_ATOMICS: usize = 26;
    pub(super) const SHADER_IMAGE_GATHER_EXTENDED: usize = 28;
    pub(super) const SHADER_STORAGE_IMAGE_EXTENDED_FORMATS: usize = 29;
    pub(super) const SHADER_STORAGE_IMAGE_READ_WITHOUT_FORMAT: usize = 31;
    pub(super) const SHADER_STORAGE_IMAGE_WRITE_WITHOUT_FORMAT: usize = 32;
    pub(super) const SHADER_FLOAT64: usize = 39;
    pub(super) const SHADER_INT64: usize = 40;
    pub(super) const SHADER_INT16: usize = 41;
}

/// The host's format a seam format is read and written as. That of
/// [`Format::Depth24PlusStencil8`] is its depth of 24 bits; a host rendering to
/// no depth and stencil of it has [`Format::Depth32FloatStencil8`]'s stand in
/// (`host_formats`).
pub(crate) const fn host_format(format: Format) -> i32 {
    match format {
        Format::R8Unorm => 9,
        Format::R8Snorm => 10,
        Format::R8Uint => 13,
        Format::R8Sint => 14,
        Format::Rg8Unorm => 16,
        Format::Rg8Snorm => 17,
        Format::Rg8Uint => 20,
        Format::Rg8Sint => 21,
        Format::Rgba8Unorm => 37,
        Format::Rgba8Snorm => 38,
        Format::Rgba8Uint => 41,
        Format::Rgba8Sint => 42,
        Format::Rgba8UnormSrgb => 43,
        Format::Bgra8Unorm => 44,
        Format::Bgra8UnormSrgb => 50,
        Format::Rgb10A2Unorm => 64,
        Format::Rgb10A2Uint => 68,
        Format::R16Unorm => 70,
        Format::R16Snorm => 71,
        Format::R16Uint => 74,
        Format::R16Sint => 75,
        Format::R16Float => 76,
        Format::Rg16Unorm => 77,
        Format::Rg16Snorm => 78,
        Format::Rg16Uint => 81,
        Format::Rg16Sint => 82,
        Format::Rg16Float => 83,
        Format::Rgba16Unorm => 91,
        Format::Rgba16Snorm => 92,
        Format::Rgba16Uint => 95,
        Format::Rgba16Sint => 96,
        Format::Rgba16Float => 97,
        Format::R32Uint => 98,
        Format::R32Sint => 99,
        Format::R32Float => 100,
        Format::Rg32Uint => 101,
        Format::Rg32Sint => 102,
        Format::Rg32Float => 103,
        Format::Rgba32Uint => 107,
        Format::Rgba32Sint => 108,
        Format::Rgba32Float => 109,
        Format::Rg11B10Float => 122,
        Format::Rgb9E5Float => 123,
        Format::Depth16Unorm => 124,
        Format::Depth32Float => 126,
        Format::Stencil8 => 127,
        Format::Depth24PlusStencil8 => 129,
        Format::Depth32FloatStencil8 => 130,
    }
}

/// A seam format as the host holds it: its format there, what it offers an
/// image of it laid out as the device chooses and a buffer of it, in Vulkan
/// 1.3's format features whether the host states them or not, and what it
/// makes of the widest image of four samples the seam makes of it.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct HostFormat {
    pub(crate) format: i32,
    pub(crate) features: HostFormatFeatures,
    pub(crate) multisampled: Option<HostImageLimits>,
}

impl HostFormat {
    /// Whether an image of it is copied both ways, as every image is to be
    /// read as zeros once placed.
    pub(crate) const fn copied(self) -> bool {
        let both = format_feature::TRANSFER_SRC | format_feature::TRANSFER_DST;
        self.features.optimal & both == both
    }
}

/// Which unformatted storage accesses the device features of a host stating
/// none of Vulkan 1.3's format features grant.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct Unstated {
    pub(crate) read: bool,
    pub(crate) write: bool,
}

/// Whether a physical device of `api_version` offering `extensions` states
/// the format features Vulkan 1.3 added.
pub(crate) fn states_format_features(api_version: u32, extensions: HostExtensions) -> bool {
    minor_version(api_version) >= 3 || extensions.contains(HostExtensions::FORMAT_FEATURE_FLAGS2)
}

/// Each of the seam's formats as the host holds it, in [`Format::ALL`]'s
/// order, of each one's offers `offered` and what it makes `multisampled` in
/// that order: a depth or stencil format copied only where the queue family
/// runs `graphics` work too, the only family Vulkan copies and clears depth
/// and stencil on, and of a host stating none of Vulkan 1.3's format
/// features, what its `unstated` device features grant.
pub(crate) fn host_formats(
    (offered, multisampled): (
        &[HostFormatFeatures; Format::ALL.len()],
        &[Option<HostImageLimits>; Format::ALL.len()],
    ),
    graphics: bool,
    unstated: Option<Unstated>,
) -> [HostFormat; Format::ALL.len()] {
    let mut formats = [HostFormat::default(); Format::ALL.len()];
    for (((held, &format), &features), &multisampled) in formats
        .iter_mut()
        .zip(&Format::ALL)
        .zip(offered)
        .zip(multisampled)
    {
        let mut features = match unstated {
            Some(granted) => implied(format, features, granted),
            None => features,
        };
        if !graphics && format.aspects() != Aspects::COLOR {
            features.optimal &= !(format_feature::TRANSFER_SRC | format_feature::TRANSFER_DST);
        }
        *held = HostFormat {
            format: host_format(format),
            features,
            multisampled,
        };
    }
    let renders =
        |held: &HostFormat| held.features.optimal & format_feature::DEPTH_STENCIL_ATTACHMENT != 0;
    let standing_in = formats[Format::Depth32FloatStencil8.index()];
    let depth24 = &mut formats[Format::Depth24PlusStencil8.index()];
    if !renders(depth24) && renders(&standing_in) {
        *depth24 = standing_in;
    }
    formats
}

/// `offered`, of a host stating none of Vulkan 1.3's format features, with
/// those its older rules mean: a depth compared wherever it is sampled, and
/// storage read and written through a binding naming no format as `granted`
/// for the formats the device features cover.
fn implied(format: Format, offered: HostFormatFeatures, granted: Unstated) -> HostFormatFeatures {
    let mut features = offered;
    if format.aspects().contains(Aspects::DEPTH)
        && features.optimal & format_feature::SAMPLED_IMAGE != 0
    {
        features.optimal |= format_feature::SAMPLED_IMAGE_DEPTH_COMPARISON;
    }
    // The device features cover every format with a storage format but a
    // 64-bit one, of which the seam has none.
    if format.storage_format().is_some() {
        let mut unformatted = 0;
        if granted.read {
            unformatted |= format_feature::STORAGE_READ_WITHOUT_FORMAT;
        }
        if granted.write {
            unformatted |= format_feature::STORAGE_WRITE_WITHOUT_FORMAT;
        }
        if features.optimal & format_feature::STORAGE_IMAGE != 0 {
            features.optimal |= unformatted;
        }
        if features.buffer & format_feature::STORAGE_TEXEL_BUFFER != 0 {
            features.buffer |= unformatted;
        }
    }
    features
}

/// What a device states of `format`, held as `held` on a host whose image
/// reads past their edges read zero where `robust_images` and whose copied
/// depths keep their bits where `unrestricted`: of an image a shader reads
/// only with the first, of a float depth format only with the second, of any
/// image only where it is copied both ways, of a depth sampled only where it
/// is compared too, and nothing its encoding cannot mean. Of a device that
/// `draws`, besides, the uses a pass makes the host's tiling offers, four
/// samples where it makes them of every image of `largest` sides, and vertex
/// attributes.
fn stated_features(
    format: Format,
    held: HostFormat,
    (robust_images, unrestricted): (bool, bool),
    draws: Option<Largest>,
) -> FormatFeatures {
    let float_depth = matches!(format, Format::Depth32Float | Format::Depth32FloatStencil8);
    if float_depth && !unrestricted {
        return FormatFeatures::NONE;
    }
    let offers = |bits: u64, feature: u64| bits & feature == feature;
    let (optimal, buffer) = (held.features.optimal, held.features.buffer);
    let mut stated = FormatFeatures::NONE;
    let mut state = |feature| stated = stated.with(feature);
    let unformatted = |bits: u64, [read, write]: [FormatFeatures; 2]| {
        let mut accessed = FormatFeatures::NONE;
        if offers(bits, format_feature::STORAGE_READ_WITHOUT_FORMAT) {
            accessed = accessed.with(read);
        }
        if offers(bits, format_feature::STORAGE_WRITE_WITHOUT_FORMAT) {
            accessed = accessed.with(write);
        }
        accessed
    };
    if held.copied() && robust_images {
        let compared = !format.aspects().contains(Aspects::DEPTH)
            || offers(optimal, format_feature::SAMPLED_IMAGE_DEPTH_COMPARISON);
        if offers(optimal, format_feature::SAMPLED_IMAGE) && compared {
            state(FormatFeatures::SAMPLED);
            if offers(optimal, format_feature::SAMPLED_IMAGE_FILTER_LINEAR) {
                state(FormatFeatures::FILTERED);
            }
        }
        if offers(optimal, format_feature::STORAGE_IMAGE) {
            state(FormatFeatures::STORAGE);
            state(unformatted(
                optimal,
                [
                    FormatFeatures::STORAGE_READ_WITHOUT_FORMAT,
                    FormatFeatures::STORAGE_WRITE_WITHOUT_FORMAT,
                ],
            ));
        }
    }
    if let Some(largest) = draws {
        if held.copied() {
            let colour = offers(optimal, format_feature::COLOR_ATTACHMENT);
            if colour {
                state(FormatFeatures::COLOR_ATTACHMENT);
                // A host may average an sRGB target's samples encoded or
                // decoded, as it chooses; the seam averages them decoded.
                if !format.is_srgb() {
                    state(FormatFeatures::RESOLVED);
                }
                if offers(optimal, format_feature::COLOR_ATTACHMENT_BLEND) {
                    state(FormatFeatures::BLENDED);
                }
            }
            let depth_stencil = offers(optimal, format_feature::DEPTH_STENCIL_ATTACHMENT);
            if depth_stencil {
                state(FormatFeatures::DEPTH_STENCIL);
            }
            let four = held.multisampled.is_some_and(|most| {
                most.samples & 4 != 0 && most.extent[..2].iter().all(|&side| side >= largest.side)
            });
            if (colour || depth_stencil) && four && largest.standard_samples {
                state(FormatFeatures::MULTISAMPLED);
            }
        }
        if offers(buffer, format_feature::VERTEX_BUFFER) {
            state(FormatFeatures::VERTEX_INPUT);
        }
    }
    // A texel buffer is a view of its memory's one buffer, made for both
    // texel uses, and a view of such a buffer needs its format to offer both.
    if offers(
        buffer,
        format_feature::UNIFORM_TEXEL_BUFFER | format_feature::STORAGE_TEXEL_BUFFER,
    ) {
        state(FormatFeatures::UNIFORM_TEXEL_BUFFER);
        state(FormatFeatures::STORAGE_TEXEL_BUFFER);
        state(unformatted(
            buffer,
            [
                FormatFeatures::TEXEL_BUFFER_READ_WITHOUT_FORMAT,
                FormatFeatures::TEXEL_BUFFER_WRITE_WITHOUT_FORMAT,
            ],
        ));
    }
    FormatFeatures::from_bits(stated.bits() & format.features().bits())
}

/// The images a device that draws holds four samples of: of every side up to
/// `side`, at the standard positions where `standard_samples`.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct Largest {
    side: u32,
    standard_samples: bool,
}

/// What a host states of its physical device, as the driver read it.
pub(crate) struct HostDevice<'a> {
    pub(crate) properties: &'a HostProperties,
    pub(crate) extensions: HostExtensions,
    pub(crate) features: &'a HostFeatures,
    /// The extension structures `features` was read with, in its order.
    pub(crate) chain: &'a [Structure],
    pub(crate) memory: &'a HostMemory,
    pub(crate) families: &'a [HostQueueFamily],
    /// What it offers each of the seam's formats, in [`Format::ALL`]'s
    /// order, asked as [`host_format`] names it.
    pub(crate) formats: &'a [HostFormatFeatures; Format::ALL.len()],
    /// What it makes of each format's widest image of four samples, in the
    /// same order, where it was asked.
    pub(crate) multisampled: &'a [Option<HostImageLimits>; Format::ALL.len()],
}

/// What a device is made with besides its queues: the extensions it enables,
/// its core features, and each extension structure's features.
#[derive(Clone, Debug)]
pub(crate) struct Enables {
    pub(crate) extensions: ArrayVec<&'static str, { ASKED.len() }>,
    pub(crate) core: [bool; CORE_FEATURES],
    pub(crate) chain: ArrayVec<(Structure, ArrayVec<bool, MAX_CHAINED_MEMBERS>), MAX_CHAINED>,
}

impl Enables {
    /// Each extension structure as the encoder chains it.
    pub(crate) fn chained(&self) -> ArrayVec<Chained<'_>, MAX_CHAINED> {
        let mut chained = ArrayVec::new();
        for (structure, members) in &self.chain {
            // Within the chain's own bound.
            let _ = chained.try_push(Chained {
                structure: *structure,
                members: members.as_slice(),
            });
        }
        chained
    }
}

/// The most queue families a Venus device states: the one running
/// dispatches, and the host's copy engine beside it.
pub(crate) const STATED_FAMILIES: usize = 2;

/// What a Venus device states, which of the host's memory types and queue
/// families each stated one is, each format as the host holds it, and what
/// the device is made with.
#[derive(Clone, Debug)]
pub(crate) struct Statement {
    pub(crate) info: DeviceInfo,
    /// The host's index of each stated memory type, in order.
    pub(crate) memory_types: ArrayVec<u32, MAX_MEMORY_TYPES>,
    /// The host's index of each stated queue family, in order.
    pub(crate) families: ArrayVec<u32, STATED_FAMILIES>,
    /// How a buffer or an image is shared among the stated families.
    pub(crate) sharing: Sharing,
    /// Each of the seam's formats as the host holds it, in
    /// [`Format::ALL`]'s order.
    pub(crate) formats: [HostFormat; Format::ALL.len()],
    /// Whether an image is made with the list of formats its views read it
    /// as, which a host of Vulkan 1.2 takes.
    pub(crate) format_lists: bool,
    /// Whether the memory an image needs is asked of its description, which
    /// a host of Vulkan 1.3 answers, so the asking travels with its making.
    pub(crate) device_requirements: bool,
    /// The run of bytes an image's placement and size are whole numbers of,
    /// so no buffer shares a run with it.
    pub(crate) image_granularity: u64,
    /// Whether every render pipeline asks for its lines as rectangles: where
    /// the host's own lines are not strict.
    pub(crate) rectangular_lines: bool,
    pub(crate) enables: Enables,
}

impl Statement {
    /// What the host's `required` asks of an image's memory, as the seam
    /// names it: the stated memory types of the host's types it names, and
    /// whole runs of the host's buffer-image granularity, so no buffer placed
    /// beside the image shares a run with it.
    pub(crate) fn image_requirements(
        &self,
        required: HostRequirements,
    ) -> Result<MemoryRequirements, DriverError> {
        if !required.alignment.is_power_of_two() {
            return Err(DriverError::DeviceFault);
        }
        let granularity = self.image_granularity;
        let size = required
            .size
            .checked_next_multiple_of(granularity)
            .ok_or(DriverError::OutOfRange)?;
        let memory_types = (0u32..)
            .zip(&self.memory_types)
            .filter(|&(_, &host)| {
                1u32.checked_shl(host)
                    .is_some_and(|bit| required.type_bits & bit != 0)
            })
            .fold(0u32, |types, (stated, _)| types | (1 << stated));
        if memory_types == 0 {
            return Err(DriverError::Unsupported);
        }
        Ok(MemoryRequirements {
            size,
            alignment: required.alignment.max(granularity),
            memory_types,
        })
    }
}

/// The extension structures a physical device of `api_version` offering
/// `extensions` is asked its features with.
pub(crate) fn feature_chain(
    api_version: u32,
    extensions: HostExtensions,
) -> ArrayVec<Structure, MAX_CHAINED> {
    let minor = minor_version(api_version);
    let mut chain = ArrayVec::new();
    let mut ask = |structure| {
        let _ = chain.try_push(structure);
    };
    if extensions.intersects(HostExtensions::ROBUSTNESS2) {
        ask(Structure::PhysicalDeviceRobustness2Features);
    }
    if minor >= 3 || extensions.contains(HostExtensions::ZERO_INITIALIZE_WORKGROUP_MEMORY) {
        ask(Structure::PhysicalDeviceZeroInitializeWorkgroupMemoryFeatures);
    }
    ask(Structure::PhysicalDevice16BitStorageFeatures);
    if minor >= 2 || extensions.contains(HostExtensions::SHADER_FLOAT16_INT8) {
        ask(Structure::PhysicalDeviceShaderFloat16Int8Features);
    }
    if minor >= 3 {
        ask(Structure::PhysicalDeviceDynamicRenderingFeatures);
    }
    if extensions.contains(HostExtensions::MAINTENANCE5) {
        ask(Structure::PhysicalDeviceMaintenance5Features);
    }
    if extensions.intersects(HostExtensions::LINE_RASTERIZATION) {
        ask(Structure::PhysicalDeviceLineRasterizationFeatures);
    }
    chain
}

/// How a host draws with the software device's meaning, where it does: the
/// extension its pipelines ask for rectangular lines through, where its own
/// lines are not strict.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct Draws {
    rectangular_lines: Option<&'static str>,
}

/// Whether `host`, sharing Vulkan 1.`minor` with the instance, draws with
/// the software device's meaning: from Vulkan 1.3, rendering without render
/// pass objects, a point no stage sizes a pixel wide, every index of a full
/// word drawn, each target blended as its own, a depth bias clamped, and
/// lines as rectangles.
fn draws(host: &HostDevice<'_>, minor: u32) -> Option<Draws> {
    let chained = |structure: Structure, at: usize| {
        members(host, structure)
            .and_then(|members| members.get(at))
            .copied()
            .unwrap_or(false)
    };
    // From Vulkan 1.3 alone: before it, binding vertex buffers at their sizes
    // and rendering without render pass objects are extensions of their own.
    let dynamic = minor >= 3 && chained(Structure::PhysicalDeviceDynamicRenderingFeatures, 0);
    let points = host.extensions.contains(HostExtensions::MAINTENANCE5)
        && chained(Structure::PhysicalDeviceMaintenance5Features, 0);
    let core = [
        core::FULL_DRAW_INDEX_UINT32,
        core::INDEPENDENT_BLEND,
        core::DEPTH_BIAS_CLAMP,
    ]
    .iter()
    .all(|&at| offers_core(host, at));
    if !(dynamic && points && core) {
        return None;
    }
    if host.properties.limits.strict_lines {
        return Some(Draws {
            rectangular_lines: None,
        });
    }
    let rectangular = chained(Structure::PhysicalDeviceLineRasterizationFeatures, 0);
    let extension = if host
        .extensions
        .contains(HostExtensions::LINE_RASTERIZATION_KHR)
    {
        extension::LINE_RASTERIZATION_KHR
    } else {
        extension::LINE_RASTERIZATION_EXT
    };
    (rectangular
        && host
            .extensions
            .intersects(HostExtensions::LINE_RASTERIZATION))
    .then_some(Draws {
        rectangular_lines: Some(extension),
    })
}

/// The minor version of the Vulkan the instance and a host of
/// `api_version` share.
fn minor_version(api_version: u32) -> u32 {
    let of = |version: u32| (version >> 12) & 0x3ff;
    of(api_version).min(of(API_VERSION))
}

/// Whether `host` offers the core feature at `at`.
fn offers_core(host: &HostDevice<'_>, at: usize) -> bool {
    host.features.core.get(at).copied().unwrap_or(false)
}

/// The members `host` read of the chained structure `structure`.
fn members<'a>(host: &'a HostDevice<'_>, structure: Structure) -> Option<&'a [bool]> {
    host.chain
        .iter()
        .position(|&asked| asked == structure)
        .and_then(|at| host.features.chained.get(at))
        .map(ArrayVec::as_slice)
}

/// Whether `host` runs modules declaring `capability`, and where the feature
/// that lets it lies. No subgroup capability is stated: a Vulkan host decides
/// which invocations form a subgroup and, for a SPIR-V 1.6 module, how many,
/// so the seam's subgroups cannot be had on it.
fn capability_feature(host: &HostDevice<'_>, capability: Capability) -> Option<Feature> {
    let chained = |structure: Structure, at: usize| {
        members(host, structure)
            .and_then(|members| members.get(at))
            .copied()
            .unwrap_or(false)
    };
    let storage16 = |at: usize| {
        chained(Structure::PhysicalDevice16BitStorageFeatures, at).then_some(Feature::Chained(
            Structure::PhysicalDevice16BitStorageFeatures,
            at,
        ))
    };
    let core = |at: usize| offers_core(host, at).then_some(Feature::Core(at));
    // A host stating each format's unformatted storage accesses runs either
    // capability whatever its device features: those accesses decide which
    // formats a module reaches so.
    let unformatted = |at: usize| {
        if states_format_features(host.properties.api_version, host.extensions) {
            Some(Feature::None)
        } else {
            core(at)
        }
    };
    match capability {
        Capability::MATRIX
        | Capability::SHADER
        | Capability::SAMPLED1D
        | Capability::IMAGE1D
        | Capability::SAMPLED_BUFFER
        | Capability::IMAGE_BUFFER
        | Capability::IMAGE_QUERY
        | Capability::DERIVATIVE_CONTROL => Some(Feature::None),
        Capability::FLOAT16 => {
            chained(Structure::PhysicalDeviceShaderFloat16Int8Features, 0).then_some(
                Feature::Chained(Structure::PhysicalDeviceShaderFloat16Int8Features, 0),
            )
        }
        Capability::FLOAT64 => core(core::SHADER_FLOAT64),
        Capability::INT64 => core(core::SHADER_INT64),
        Capability::INT16 => core(core::SHADER_INT16),
        Capability::IMAGE_GATHER_EXTENDED => core(core::SHADER_IMAGE_GATHER_EXTENDED),
        Capability::IMAGE_CUBE_ARRAY | Capability::SAMPLED_CUBE_ARRAY => {
            core(core::IMAGE_CUBE_ARRAY)
        }
        Capability::SAMPLE_RATE_SHADING => core(core::SAMPLE_RATE_SHADING),
        Capability::STORAGE_IMAGE_EXTENDED_FORMATS => {
            core(core::SHADER_STORAGE_IMAGE_EXTENDED_FORMATS)
        }
        Capability::STORAGE_IMAGE_READ_WITHOUT_FORMAT => {
            unformatted(core::SHADER_STORAGE_IMAGE_READ_WITHOUT_FORMAT)
        }
        Capability::STORAGE_IMAGE_WRITE_WITHOUT_FORMAT => {
            unformatted(core::SHADER_STORAGE_IMAGE_WRITE_WITHOUT_FORMAT)
        }
        Capability::STORAGE_BUFFER16_BIT_ACCESS => storage16(0),
        Capability::UNIFORM_AND_STORAGE_BUFFER16_BIT_ACCESS => storage16(1),
        Capability::STORAGE_PUSH_CONSTANT16 => storage16(2),
        Capability::STORAGE_INPUT_OUTPUT16 => storage16(3),
        _ => None,
    }
}

/// Where the feature a capability needs lies, if it needs one.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Feature {
    None,
    Core(usize),
    Chained(Structure, usize),
}

/// The device the host's statement makes, and what it is made with.
///
/// # Errors
///
/// [`DriverError::Unsupported`] for a physical device of an older Vulkan,
/// without the second robustness level or zero-initialized workgroup memory,
/// or with no queue family running dispatches or no memory type the seam
/// names; [`DriverError::DeviceFault`] for a statement no device makes: an
/// alignment that is no power of two, or past what Vulkan allows.
pub(crate) fn narrow(host: &HostDevice<'_>) -> Result<Statement, DriverError> {
    let properties = host.properties;
    if properties.api_version < MIN_API_VERSION {
        return Err(DriverError::Unsupported);
    }
    let alignments = robust_alignments(host)?;
    // A family running graphics work too copies and clears depth and stencil.
    let (family, queues, graphics) = host
        .families
        .iter()
        .enumerate()
        .filter(|(_, family)| family.flags & queue::COMPUTE != 0 && family.queues > 0)
        .min_by_key(|(_, family)| family.flags & queue::GRAPHICS == 0)
        .map(|(index, family)| (index, family.queues, family.flags & queue::GRAPHICS != 0))
        .ok_or(DriverError::Unsupported)?;
    let memory = memory_of(host.memory)?;
    let largest = memory
        .heaps
        .iter()
        .map(|heap: &MemoryHeap| heap.size)
        .max()
        .unwrap_or(0);
    let minor = minor_version(properties.api_version);
    let drawing = if graphics { draws(host, minor) } else { None };
    // A family running graphics work reaches depth and stencil whether or not
    // the seam states its draws.
    let first = QueueFamily {
        capabilities: match (graphics, drawing.is_some()) {
            (_, true) => QueueCapabilities::ALL,
            (true, false) => QueueCapabilities::TRANSFER
                .with(QueueCapabilities::COMPUTE)
                .with(QueueCapabilities::DEPTH_STENCIL),
            (false, false) => QueueCapabilities::TRANSFER.with(QueueCapabilities::COMPUTE),
        },
        queues: queues.min(MAX_QUEUE_RINGS).min(MAX_QUEUES_PER_FAMILY),
        granularity: Granularity::TEXEL,
    };
    let (queue_families, families) = stated_families(host.families, (family, first))?;
    let (capabilities, features, enables) = runs_and_enables(host, minor, drawing)?;
    let unstated =
        (!states_format_features(properties.api_version, host.extensions)).then(|| Unstated {
            read: offers_core(host, core::SHADER_STORAGE_IMAGE_READ_WITHOUT_FORMAT),
            write: offers_core(host, core::SHADER_STORAGE_IMAGE_WRITE_WITHOUT_FORMAT),
        });
    let formats = host_formats((host.formats, host.multisampled), graphics, unstated);
    let kept = (
        robust_images(host),
        host.extensions
            .contains(HostExtensions::DEPTH_RANGE_UNRESTRICTED),
    );
    let mut limits = limits(
        properties,
        properties.max_allocation_size.min(largest),
        (
            binding_alignment(alignments.uniform)?,
            binding_alignment(alignments.storage)?,
        ),
    )?;
    if drawing.is_some() {
        let host_limits = &properties.limits;
        // An attachment is a two-dimensional image a rendering instance's
        // area may cover whole.
        limits.max_image_dimension_2d = limits
            .max_image_dimension_2d
            .min(host_limits.framebuffer[0])
            .min(host_limits.framebuffer[1]);
        if !offers_core(host, core::MULTI_DRAW_INDIRECT) {
            limits.max_draw_indirect_count = 1;
        }
        if !features.contains(Features::DRAW_INDIRECT_FIRST_INSTANCE) {
            limits.max_draw_indirect_count = limits.max_draw_indirect_count.min(CHECKED_RECORDS);
        }
    }
    let largest = drawing.map(|_| Largest {
        side: limits.max_image_dimension_2d,
        standard_samples: properties.limits.standard_sample_locations,
    });
    let table = FormatTable::from_fn(|format| {
        stated_features(format, formats[format.index()], kept, largest)
    });
    Ok(Statement {
        info: DeviceInfo {
            heaps: memory.heaps,
            memory_types: memory.types,
            queue_families,
            limits,
            capabilities,
            spirv_version: match minor {
                0..=1 => Version::OLDEST,
                2 => Version { major: 1, minor: 5 },
                _ => Version { major: 1, minor: 6 },
            },
            features,
            formats: table,
        },
        memory_types: memory.host_types,
        sharing: Sharing::of(&families)?,
        families,
        formats,
        format_lists: minor >= 2,
        device_requirements: minor >= 3,
        image_granularity: image_granularity(properties.limits.buffer_image_granularity)?,
        rectangular_lines: drawing.is_some_and(|drawing| drawing.rectangular_lines.is_some()),
        enables,
    })
}

/// The families a device states, the host's index of each beside it: `first`,
/// the host's family `index`, then the host's copy engine where it has one.
fn stated_families(
    host: &[HostQueueFamily],
    (index, first): (usize, QueueFamily),
) -> Result<
    (
        ArrayVec<QueueFamily, { tairix_gpu::MAX_QUEUE_FAMILIES }>,
        ArrayVec<u32, STATED_FAMILIES>,
    ),
    DriverError,
> {
    let mut stated = ArrayVec::new();
    let mut indices = ArrayVec::new();
    stated.try_push(first).map_err(full)?;
    indices
        .try_push(u32::try_from(index).map_err(|_| DriverError::DeviceFault)?)
        .map_err(full)?;
    if let Some((index, engine)) = copy_engine(host, MAX_QUEUE_RINGS.saturating_sub(first.queues)) {
        stated.try_push(engine).map_err(full)?;
        indices.try_push(index).map_err(full)?;
    }
    Ok((stated, indices))
}

/// The host's first copy engine — a family running transfers and nothing
/// else, with a queue — and how the seam states it: its queues within the
/// `rings` the family before it leaves and its granularity the host's; none
/// where there are no rings left or Vulkan's rules make no granularity of
/// the host's.
fn copy_engine(families: &[HostQueueFamily], rings: u32) -> Option<(u32, QueueFamily)> {
    let (index, family) = families.iter().enumerate().find(|(_, family)| {
        family.flags & queue::TRANSFER != 0
            && family.flags & !(queue::TRANSFER | queue::SPARSE_BINDING) == 0
            && family.queues > 0
    })?;
    let queues = family.queues.min(rings).min(MAX_QUEUES_PER_FAMILY);
    let stated = QueueFamily {
        capabilities: QueueCapabilities::TRANSFER,
        queues,
        granularity: granularity(family.granularity)?,
    };
    (queues > 0).then_some((u32::try_from(index).ok()?, stated))
}

/// A host's image transfer granularity as the seam states it: whole levels
/// for nought, blocks for powers of two along each axis.
fn granularity(sides: [u32; 3]) -> Option<Granularity> {
    if sides == [0; 3] {
        return Some(Granularity::Levels);
    }
    let log2 = |side: u32| {
        side.is_power_of_two()
            .then(|| u8::try_from(side.trailing_zeros()).ok())
            .flatten()
    };
    Some(Granularity::Blocks {
        log2: [log2(sides[0])?, log2(sides[1])?, log2(sides[2])?],
    })
}

/// Whether `host` holds an image's reads past its edges to zeros, its
/// second robustness level's image member.
fn robust_images(host: &HostDevice<'_>) -> bool {
    members(host, Structure::PhysicalDeviceRobustness2Features)
        .and_then(|members| members.get(1))
        .copied()
        .unwrap_or(false)
}

/// The granularity `host` bounds robust accesses at, where it offers both
/// robust buffer access levels and zero-initialized workgroup memory.
fn robust_alignments(host: &HostDevice<'_>) -> Result<RobustAlignments, DriverError> {
    let first = |members: Option<&[bool]>| {
        members
            .and_then(|members| members.first())
            .copied()
            .unwrap_or(false)
    };
    let robust_buffers = offers_core(host, core::ROBUST_BUFFER_ACCESS)
        && first(members(host, Structure::PhysicalDeviceRobustness2Features));
    let zeroed = first(members(
        host,
        Structure::PhysicalDeviceZeroInitializeWorkgroupMemoryFeatures,
    ));
    let alignments = host
        .properties
        .robust_alignments
        .ok_or(DriverError::Unsupported)?;
    if robust_buffers && zeroed {
        Ok(alignments)
    } else {
        Err(DriverError::Unsupported)
    }
}

impl Enables {
    /// Nothing enabled: no extension, no core feature, and each structure of
    /// `chain` chained with none of its members.
    fn none(chain: &[Structure]) -> Result<Self, DriverError> {
        let mut enables = Self {
            extensions: ArrayVec::new(),
            core: [false; CORE_FEATURES],
            chain: ArrayVec::new(),
        };
        for &structure in chain {
            let count = super::reply::chained_members(structure).ok_or(DriverError::DeviceFault)?;
            let mut members = ArrayVec::new();
            for _ in 0..count {
                members.try_push(false).map_err(full)?;
            }
            enables.chain.try_push((structure, members)).map_err(full)?;
        }
        Ok(enables)
    }

    fn enable(&mut self, feature: Feature) -> Result<(), DriverError> {
        let member = match feature {
            Feature::None => return Ok(()),
            Feature::Core(at) => self.core.get_mut(at),
            Feature::Chained(structure, at) => self
                .chain
                .iter_mut()
                .find(|(asked, _)| *asked == structure)
                .and_then(|(_, members)| members.get_mut(at)),
        };
        member
            .map(|member| *member = true)
            .ok_or(DriverError::DeviceFault)
    }

    fn extend(&mut self, extension: &'static str) -> Result<(), DriverError> {
        self.extensions.try_push(extension).map_err(full)
    }
}

/// The capabilities and features `host`, sharing Vulkan 1.`minor` with the
/// instance, runs, drawing as `drawing` says, and what its device is made
/// with to run them and the seam's semantics.
fn runs_and_enables(
    host: &HostDevice<'_>,
    minor: u32,
    drawing: Option<Draws>,
) -> Result<(ArrayVec<Capability, MAX_CAPABILITIES>, Features, Enables), DriverError> {
    let mut enables = Enables::none(host.chain)?;
    enables.enable(Feature::Core(core::ROBUST_BUFFER_ACCESS))?;
    let robust = Structure::PhysicalDeviceRobustness2Features;
    enables.enable(Feature::Chained(robust, 0))?;
    if robust_images(host) {
        enables.enable(Feature::Chained(robust, 1))?;
    }
    enables.enable(Feature::Chained(
        Structure::PhysicalDeviceZeroInitializeWorkgroupMemoryFeatures,
        0,
    ))?;
    enables.extend(
        if host.extensions.contains(HostExtensions::ROBUSTNESS2_KHR) {
            extension::ROBUSTNESS2_KHR
        } else {
            extension::ROBUSTNESS2_EXT
        },
    )?;
    let mut features = sampler_features(host, &mut enables)?;
    match drawing {
        Some(drawing) => {
            features = features.with(draw_features(host, drawing, &mut enables)?);
        }
        None => enables.chain.retain(|(structure, _)| {
            !matches!(
                structure,
                Structure::PhysicalDeviceDynamicRenderingFeatures
                    | Structure::PhysicalDeviceMaintenance5Features
                    | Structure::PhysicalDeviceLineRasterizationFeatures
            )
        }),
    }
    let mut capabilities = ArrayVec::new();
    let mut float16 = false;
    for &capability in &tairix_spirv::CAPABILITIES {
        let Some(feature) = capability_feature(host, capability) else {
            continue;
        };
        enables.enable(feature)?;
        float16 |= capability == Capability::FLOAT16;
        capabilities.try_push(capability).map_err(full)?;
    }
    if host
        .extensions
        .contains(HostExtensions::DEPTH_RANGE_UNRESTRICTED)
    {
        enables.extend(extension::DEPTH_RANGE_UNRESTRICTED)?;
    }
    if minor < 3 {
        enables.extend(extension::ZERO_INITIALIZE_WORKGROUP_MEMORY)?;
        // What lets a module reach storage unformatted on its formats' word.
        if host
            .extensions
            .contains(HostExtensions::FORMAT_FEATURE_FLAGS2)
        {
            enables.extend(extension::FORMAT_FEATURE_FLAGS2)?;
        }
    }
    if minor < 2 {
        if float16 {
            enables.extend(extension::SHADER_FLOAT16_INT8)?;
        } else {
            // An extension's structure is chained only with the extension.
            enables.chain.retain(|(structure, _)| {
                *structure != Structure::PhysicalDeviceShaderFloat16Int8Features
            });
        }
    }
    Ok((capabilities, features, enables))
}

/// What a host drawing as `drawing` says is made with to draw, and the draw
/// features it offers, each enabled.
fn draw_features(
    host: &HostDevice<'_>,
    drawing: Draws,
    enables: &mut Enables,
) -> Result<Features, DriverError> {
    for at in [
        core::FULL_DRAW_INDEX_UINT32,
        core::INDEPENDENT_BLEND,
        core::DEPTH_BIAS_CLAMP,
    ] {
        enables.enable(Feature::Core(at))?;
    }
    if offers_core(host, core::MULTI_DRAW_INDIRECT) {
        enables.enable(Feature::Core(core::MULTI_DRAW_INDIRECT))?;
    }
    enables.enable(Feature::Chained(
        Structure::PhysicalDeviceDynamicRenderingFeatures,
        0,
    ))?;
    enables.enable(Feature::Chained(
        Structure::PhysicalDeviceMaintenance5Features,
        0,
    ))?;
    enables.extend(extension::MAINTENANCE5)?;
    match drawing.rectangular_lines {
        Some(extension) => {
            enables.enable(Feature::Chained(
                Structure::PhysicalDeviceLineRasterizationFeatures,
                0,
            ))?;
            enables.extend(extension)?;
        }
        None => enables.chain.retain(|(structure, _)| {
            *structure != Structure::PhysicalDeviceLineRasterizationFeatures
        }),
    }
    let mut features = Features::NONE;
    for (at, feature) in [
        (core::DUAL_SRC_BLEND, Features::DUAL_SOURCE_BLEND),
        (core::LOGIC_OP, Features::LOGIC_OP),
        (core::DEPTH_CLAMP, Features::DEPTH_CLAMP),
        (core::WIDE_LINES, Features::WIDE_LINES),
        (core::LARGE_POINTS, Features::LARGE_POINTS),
        (
            core::VERTEX_PIPELINE_STORES_AND_ATOMICS,
            Features::VERTEX_STORES,
        ),
        (core::FRAGMENT_STORES_AND_ATOMICS, Features::FRAGMENT_STORES),
        (
            core::DRAW_INDIRECT_FIRST_INSTANCE,
            Features::DRAW_INDIRECT_FIRST_INSTANCE,
        ),
    ] {
        if offers_core(host, at) {
            enables.enable(Feature::Core(at))?;
            features = features.with(feature);
        }
    }
    Ok(features)
}

/// The sampler features `host` offers, each enabled.
fn sampler_features(host: &HostDevice<'_>, enables: &mut Enables) -> Result<Features, DriverError> {
    let mut features = Features::NONE;
    if offers_core(host, core::SAMPLER_ANISOTROPY) {
        enables.enable(Feature::Core(core::SAMPLER_ANISOTROPY))?;
        features = features.with(Features::SAMPLER_ANISOTROPY);
    }
    if host
        .extensions
        .contains(HostExtensions::SAMPLER_MIRROR_CLAMP_TO_EDGE)
    {
        enables.extend(extension::SAMPLER_MIRROR_CLAMP_TO_EDGE)?;
        features = features.with(Features::MIRROR_CLAMP_TO_EDGE);
    }
    Ok(features)
}

/// The memory a device states: its heaps, its types, and the host's index of
/// each type stated.
struct StatedMemory {
    heaps: ArrayVec<MemoryHeap, { tairix_gpu::MAX_MEMORY_HEAPS }>,
    types: ArrayVec<MemoryType, { tairix_gpu::MAX_MEMORY_TYPES }>,
    host_types: ArrayVec<u32, MAX_MEMORY_TYPES>,
}

/// The heaps of no bytes left out, and the memory types the seam names over
/// the rest.
fn memory_of(host_memory: &HostMemory) -> Result<StatedMemory, DriverError> {
    let mut heaps = ArrayVec::new();
    let mut heap_of = [None; MAX_MEMORY_HEAPS];
    for (host, heap) in host_memory.heaps.iter().enumerate() {
        if heap.size == 0 {
            continue;
        }
        heap_of[host] = u32::try_from(heaps.len()).ok();
        heaps
            .try_push(MemoryHeap {
                size: heap.size,
                device_local: heap.flags & HEAP_DEVICE_LOCAL != 0,
            })
            .map_err(full)?;
    }
    let mut types = ArrayVec::new();
    let mut host_types = ArrayVec::new();
    for (host, kind) in host_memory.types.iter().enumerate() {
        let Some(properties) = stated_properties(kind.flags) else {
            continue;
        };
        let Some(&Some(heap)) = heap_of.get(kind.heap as usize) else {
            continue;
        };
        types
            .try_push(MemoryType { heap, properties })
            .map_err(full)?;
        host_types
            .try_push(u32::try_from(host).map_err(|_| DriverError::DeviceFault)?)
            .map_err(full)?;
    }
    if types.is_empty() {
        return Err(DriverError::Unsupported);
    }
    Ok(StatedMemory {
        heaps,
        types,
        host_types,
    })
}

/// A list the host's own bounds kept within, overflowing: a statement no
/// device makes.
fn full<T>(_: T) -> DriverError {
    DriverError::DeviceFault
}

/// The seam's properties of a host memory type of `flags`, where the seam
/// names them all and the type is coherent wherever the CPU reaches it.
fn stated_properties(flags: u32) -> Option<MemoryProperties> {
    let named =
        memory::DEVICE_LOCAL | memory::HOST_VISIBLE | memory::HOST_COHERENT | memory::HOST_CACHED;
    let visible = flags & memory::HOST_VISIBLE != 0;
    if flags & !named != 0 || (visible && flags & memory::HOST_COHERENT == 0) {
        return None;
    }
    let mut properties = MemoryProperties::NONE;
    for (flag, property) in [
        (memory::DEVICE_LOCAL, MemoryProperties::DEVICE_LOCAL),
        (memory::HOST_VISIBLE, MemoryProperties::HOST_VISIBLE),
        (memory::HOST_COHERENT, MemoryProperties::HOST_COHERENT),
        (memory::HOST_CACHED, MemoryProperties::HOST_CACHED),
    ] {
        if flags & flag != 0 {
            properties = properties.with(property);
        }
    }
    Some(properties)
}

/// A binding alignment the host states, or the granularity it bounds robust
/// accesses at: a power of two Vulkan allows.
fn binding_alignment(alignment: u64) -> Result<u64, DriverError> {
    if alignment.is_power_of_two() && alignment <= MAX_BINDING_ALIGNMENT {
        Ok(alignment)
    } else {
        Err(DriverError::DeviceFault)
    }
}

/// The widest buffer-image granularity Vulkan lets a device state.
const MAX_IMAGE_GRANULARITY: u64 = 128 << 10;

/// `granularity`, refused where it is no power of two or past what Vulkan
/// allows.
fn image_granularity(granularity: u64) -> Result<u64, DriverError> {
    if granularity.is_power_of_two() && granularity <= MAX_IMAGE_GRANULARITY {
        Ok(granularity)
    } else {
        Err(DriverError::DeviceFault)
    }
}

/// A host's bound in whole units, rounded down: none for less than one, at
/// most an `i32`'s widest.
fn floored(value: f32) -> u32 {
    u32::try_from(mathf::round_i32(mathf::floor(f64::from(value)))).unwrap_or(0)
}

/// A host's limit in whole units: at least one.
fn whole(value: f32) -> u32 {
    floored(value).max(1)
}

/// One of the seam's bounds as a limit.
fn bound(bound: usize) -> u32 {
    u32::try_from(bound).unwrap_or(u32::MAX)
}

/// The host's limits held to the seam's bounds, its largest allocation
/// `max_allocation_size`, and the granularity it bounds a uniform and a
/// storage binding's reads at.
fn limits(
    properties: &HostProperties,
    max_allocation_size: u64,
    (uniform_size, storage_size): (u64, u64),
) -> Result<Limits, DriverError> {
    let host = &properties.limits;
    let uniform = binding_alignment(host.uniform_offset_alignment)?;
    // A texel buffer's offset keeps a storage binding's alignment.
    let storage = binding_alignment(host.storage_offset_alignment)?
        .max(binding_alignment(host.texel_offset_alignment)?);
    if !properties.subgroup_size.is_power_of_two() {
        return Err(DriverError::DeviceFault);
    }
    let [low, high] = host.viewport_bounds;
    // Rounded inward, so no viewport the model admits lies past the host's;
    // saturating, a bound past an `i32` is stated at its widest.
    let viewport_bounds_range = [
        mathf::round_i32(mathf::ceil(f64::from(low))),
        mathf::round_i32(mathf::floor(f64::from(high))),
    ];
    if viewport_bounds_range[0] > viewport_bounds_range[1] {
        return Err(DriverError::DeviceFault);
    }
    let [samplers, uniforms, storages, sampled, stored] = host.set_descriptors;
    let set_descriptors = samplers
        .saturating_add(uniforms)
        .saturating_add(storages)
        .saturating_add(sampled)
        .saturating_add(stored)
        .min(properties.max_set_descriptors);
    Ok(Limits {
        max_allocation_size,
        // A seam buffer is a range of the one buffer over its memory, so its
        // start keeps every binding's alignment and every copy's.
        buffer_alignment: uniform.max(storage).max(u64::from(COPY_ALIGNMENT)),
        max_job_time: MAX_JOB_TIME,
        max_image_dimension_1d: host.image_1d,
        max_image_dimension_2d: host.image_2d.min(host.image_cube),
        max_image_dimension_3d: host.image_3d,
        max_image_array_layers: host.array_layers,
        max_sampler_anisotropy: whole(host.sampler_anisotropy),
        max_sampler_lod_bias: floored(host.sampler_lod_bias).min(MAX_LOD_BIAS),
        max_bind_sets: host.bound_sets.max(1),
        max_binding_number: MAX_BINDING_NUMBER,
        max_bind_set_descriptors: set_descriptors,
        max_push_constant_bytes: host.push_constants.min(bound(MAX_PUSH_BYTES)),
        uniform_buffer_alignment: uniform,
        storage_buffer_alignment: storage,
        uniform_buffer_size_alignment: uniform_size,
        storage_buffer_size_alignment: storage_size,
        max_uniform_buffer_range: u64::from(host.uniform_buffer_range),
        max_storage_buffer_range: u64::from(host.storage_buffer_range),
        // An image lies in one allocation.
        max_storage_image_bytes: max_allocation_size,
        max_texel_buffer_elements: u64::from(host.texel_buffer_elements),
        max_workgroup_size: host.workgroup_size,
        max_workgroup_invocations: host.workgroup_invocations,
        max_workgroup_count: host.workgroup_count,
        max_workgroup_memory: host.workgroup_memory,
        max_invocation_memory: 0,
        subgroup_size: properties.subgroup_size.min(MAX_SUBGROUP_SIZE),
        max_vertex_input_attributes: host.vertex_attributes.min(MAX_VERTEX_ATTRIBUTES),
        max_vertex_input_bindings: host.vertex_bindings.min(bound(MAX_DRAW_VERTEX_BUFFERS)),
        max_vertex_input_attribute_offset: host.vertex_attribute_offset,
        max_vertex_input_binding_stride: host.vertex_binding_stride,
        max_vertex_output_components: host.vertex_output_components,
        max_fragment_input_components: host.fragment_input_components,
        max_color_attachments: host.colour_attachments.min(bound(MAX_PASS_ATTACHMENTS)),
        max_viewport_dimensions: host.viewport_dimensions,
        viewport_bounds_range,
        max_point_size: whole(host.point_sizes[1]),
        max_line_width: whole(host.line_widths[1]),
        max_draw_indirect_count: host.draw_indirect_count.max(1),
    })
}

#[cfg(test)]
#[path = "info_tests.rs"]
mod tests;
