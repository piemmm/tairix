//! A host's statement narrowed to the device the seam takes, and what the
//! device is made with to run it.

use alloc::sync::Arc;
use alloc::vec::Vec;

use tairix_abi::DriverError;
use tairix_gpu::{
    Aspects, Device, Features, Format, FormatFeatures, MemoryProperties, QueueCapabilities,
    MAX_BINDING_NUMBER, MAX_LOD_BIAS, MAX_SUBGROUP_SIZE,
};
use tairix_inline::ArrayVec;
use tairix_spirv::enumerant::Capability;
use tairix_spirv::Version;

use super::{core, feature_chain, host_format, narrow, HostDevice, Statement};
use crate::venus::reply::{
    chained_members, HostExtensions, HostFeatures, HostFormatFeatures, HostImageLimits, HostLimits,
    HostMemory, HostMemoryHeap, HostMemoryType, HostProperties, HostQueueFamily, HostRequirements,
    RobustAlignments, MAX_CHAINED,
};
use crate::venus::testing::{
    drawn, every_extension, every_use, Idle, Still, DRAWN, DRAW_FEATURES, OLDER_FORMAT_FEATURES,
};
use crate::venus::{
    api_version, extension, format_feature, memory, queue, Structure, API_VERSION, CORE_FEATURES,
    HEAP_DEVICE_LOCAL, PHYSICAL_DEVICE_NAME_LEN,
};

fn host_limits() -> HostLimits {
    HostLimits {
        image_1d: 16384,
        image_2d: 16384,
        image_3d: 2048,
        image_cube: 8192,
        array_layers: 2048,
        buffer_image_granularity: 64,
        texel_buffer_elements: 1 << 27,
        uniform_buffer_range: 65536,
        storage_buffer_range: 1 << 30,
        push_constants: 256,
        bound_sets: 8,
        set_descriptors: [1, 2, 3, 4, 5],
        vertex_attributes: 32,
        vertex_bindings: 64,
        vertex_attribute_offset: 2047,
        vertex_binding_stride: 2048,
        vertex_output_components: 128,
        fragment_input_components: 128,
        workgroup_memory: 32768,
        workgroup_count: [65535; 3],
        workgroup_invocations: 1024,
        workgroup_size: [1024, 1024, 64],
        draw_indirect_count: u32::MAX,
        sampler_lod_bias: 15.5,
        sampler_anisotropy: 16.0,
        viewport_dimensions: [16384; 2],
        viewport_bounds: [-32768.0, 32767.0],
        texel_offset_alignment: 16,
        uniform_offset_alignment: 64,
        storage_offset_alignment: 16,
        colour_attachments: 16,
        point_sizes: [1.0, 255.0],
        line_widths: [1.0, 8.0],
        framebuffer: [16384; 2],
        strict_lines: true,
        standard_sample_locations: true,
    }
}

/// What a host makes of an image of four samples of every format.
const FOUR_SAMPLES: HostImageLimits = HostImageLimits {
    extent: [16384, 16384, 1],
    levels: 1,
    layers: 2048,
    samples: 0x7f,
};

fn properties() -> HostProperties {
    HostProperties {
        api_version: API_VERSION,
        vendor: 0x10005,
        device: 0,
        name: [0; PHYSICAL_DEVICE_NAME_LEN],
        limits: host_limits(),
        subgroup_size: 8,
        max_allocation_size: 1 << 31,
        max_set_descriptors: 12,
        robust_alignments: Some(RobustAlignments {
            storage: 4,
            uniform: 16,
        }),
    }
}

/// A host with a device-local heap and a host heap of no bytes, and four
/// memory types: one of every property the seam names, one host-visible but
/// incoherent, one lazily allocated, and one over the empty heap.
fn host_memory() -> HostMemory {
    let mut heaps = ArrayVec::new();
    heaps
        .try_push(HostMemoryHeap {
            size: 4 << 30,
            flags: HEAP_DEVICE_LOCAL,
        })
        .expect("room");
    heaps
        .try_push(HostMemoryHeap { size: 0, flags: 0 })
        .expect("room");
    let all =
        memory::DEVICE_LOCAL | memory::HOST_VISIBLE | memory::HOST_COHERENT | memory::HOST_CACHED;
    let mut types = ArrayVec::new();
    for (flags, heap) in [
        (memory::HOST_VISIBLE | memory::HOST_CACHED, 0),
        (memory::DEVICE_LOCAL | 0x10, 0),
        (all, 0),
        (memory::DEVICE_LOCAL, 1),
        (memory::DEVICE_LOCAL, 0),
    ] {
        types
            .try_push(HostMemoryType { flags, heap })
            .expect("room");
    }
    HostMemory { types, heaps }
}

fn families() -> [HostQueueFamily; 2] {
    [
        HostQueueFamily {
            flags: 0,
            queues: 4,
        },
        HostQueueFamily {
            flags: queue::GRAPHICS | queue::COMPUTE | queue::TRANSFER,
            queues: 2,
        },
    ]
}

/// Every feature a format may offer an image and a buffer.
const EVERY_FEATURE: HostFormatFeatures = HostFormatFeatures {
    optimal: u64::MAX,
    buffer: u64::MAX,
};

/// Whether `capability` is one of the subgroup operations'.
fn subgroup(capability: Capability) -> bool {
    matches!(
        capability,
        Capability::GROUP_NON_UNIFORM
            | Capability::GROUP_NON_UNIFORM_VOTE
            | Capability::GROUP_NON_UNIFORM_ARITHMETIC
            | Capability::GROUP_NON_UNIFORM_BALLOT
    )
}

/// Everything a host states, each part changeable before it is narrowed.
struct Host {
    properties: HostProperties,
    extensions: HostExtensions,
    chained: Vec<(Structure, Vec<bool>)>,
    core: [bool; CORE_FEATURES],
    memory: HostMemory,
    families: Vec<HostQueueFamily>,
    formats: [HostFormatFeatures; Format::ALL.len()],
    multisampled: [Option<HostImageLimits>; Format::ALL.len()],
}

impl Host {
    /// A host offering every extension the driver asks for and every
    /// feature, of every format.
    fn every() -> Self {
        let extensions = every_extension();
        let chained = feature_chain(API_VERSION, extensions)
            .iter()
            .map(|&structure| {
                let members = chained_members(structure).expect("chained");
                (structure, alloc::vec![true; members])
            })
            .collect();
        Self {
            properties: properties(),
            extensions,
            chained,
            core: [true; CORE_FEATURES],
            memory: host_memory(),
            families: families().to_vec(),
            formats: [EVERY_FEATURE; Format::ALL.len()],
            multisampled: [Some(FOUR_SAMPLES); Format::ALL.len()],
        }
    }

    /// The structures asked of its version and extensions chained afresh,
    /// every member offered.
    fn rechain(&mut self) {
        self.chained = feature_chain(self.properties.api_version, self.extensions)
            .iter()
            .map(|&structure| {
                let members = chained_members(structure).expect("chained");
                (structure, alloc::vec![true; members])
            })
            .collect();
    }

    /// Set member `at` of the chained structure `structure`.
    fn set(&mut self, structure: Structure, at: usize, value: bool) {
        let (_, members) = self
            .chained
            .iter_mut()
            .find(|(asked, _)| *asked == structure)
            .expect("chained");
        members[at] = value;
    }

    fn narrowed(&self) -> Result<Statement, DriverError> {
        let mut chain: ArrayVec<Structure, MAX_CHAINED> = ArrayVec::new();
        let mut features = HostFeatures {
            core: self.core,
            chained: ArrayVec::new(),
        };
        for (structure, members) in &self.chained {
            chain.try_push(*structure).expect("room");
            let mut read = ArrayVec::new();
            read.try_extend_from_slice(members).expect("room");
            features.chained.try_push(read).expect("room");
        }
        narrow(&HostDevice {
            properties: &self.properties,
            extensions: self.extensions,
            features: &features,
            chain: &chain,
            memory: &self.memory,
            families: &self.families,
            formats: &self.formats,
            multisampled: &self.multisampled,
        })
    }
}

#[test]
fn the_host_is_narrowed_to_coherent_memory_and_one_family_running_dispatches() {
    let statement = Host::every().narrowed().expect("stated");
    let info = &statement.info;
    assert_eq!(info.heaps.len(), 1, "the empty heap is left out");
    assert_eq!(info.heaps[0].size, 4 << 30);
    assert!(info.heaps[0].device_local);
    let all = MemoryProperties::DEVICE_LOCAL
        .with(MemoryProperties::HOST_VISIBLE)
        .with(MemoryProperties::HOST_COHERENT)
        .with(MemoryProperties::HOST_CACHED);
    let stated: Vec<MemoryProperties> = info
        .memory_types
        .iter()
        .map(|kind| kind.properties)
        .collect();
    assert_eq!(stated.as_slice(), &[all, MemoryProperties::DEVICE_LOCAL]);
    assert_eq!(
        statement.memory_types.as_slice(),
        &[2, 4],
        "the host's own indices"
    );
    assert_eq!(statement.family, 1);
    assert_eq!(info.queue_families.len(), 1);
    assert_eq!(
        info.queue_families[0].capabilities,
        QueueCapabilities::ALL,
        "a family that draws too, the host drawing as the software device does"
    );
    assert_eq!(info.queue_families[0].queues, 2);
    let expected: Vec<Capability> = tairix_spirv::CAPABILITIES
        .into_iter()
        .filter(|&capability| !subgroup(capability))
        .collect();
    assert_eq!(
        info.capabilities.as_slice(),
        expected.as_slice(),
        "every capability the seam admits but the subgroup ones, the host running them all"
    );
    assert_eq!(info.spirv_version, Version { major: 1, minor: 6 });
    assert_eq!(
        info.features,
        Features::SAMPLER_ANISOTROPY
            .with(Features::MIRROR_CLAMP_TO_EDGE)
            .with(DRAW_FEATURES),
        "no arrays of descriptors, nor memory shared between contexts"
    );
    for format in Format::ALL {
        assert_eq!(
            info.formats.get(format),
            every_use(format),
            "{format:?}: every use its encoding means"
        );
    }
}

/// A storage image's reads and writes through a binding naming no format.
const UNFORMATTED_STORAGE: FormatFeatures =
    FormatFeatures::STORAGE_READ_WITHOUT_FORMAT.with(FormatFeatures::STORAGE_WRITE_WITHOUT_FORMAT);

/// A storage texel buffer's reads and writes through a binding naming no
/// format.
const UNFORMATTED_TEXELS: FormatFeatures = FormatFeatures::TEXEL_BUFFER_READ_WITHOUT_FORMAT
    .with(FormatFeatures::TEXEL_BUFFER_WRITE_WITHOUT_FORMAT);

#[test]
fn the_limits_are_the_host_s_held_to_the_seam_s_bounds() {
    let limits = Host::every().narrowed().expect("stated").info.limits;
    assert_eq!(
        limits.max_allocation_size,
        1 << 31,
        "the host's, under its largest heap"
    );
    assert_eq!(
        limits.buffer_alignment, 64,
        "every binding's alignment kept"
    );
    assert_eq!(limits.uniform_buffer_alignment, 64);
    assert_eq!(limits.storage_buffer_alignment, 16);
    assert_eq!(
        (
            limits.uniform_buffer_size_alignment,
            limits.storage_buffer_size_alignment
        ),
        (16, 4),
        "the granularity the host bounds robust accesses at"
    );
    let bound = |bound: usize| u32::try_from(bound).expect("a small bound");
    assert_eq!(
        limits.max_color_attachments,
        bound(tairix_gpu::MAX_PASS_ATTACHMENTS)
    );
    assert_eq!(
        limits.max_vertex_input_bindings,
        bound(tairix_gpu::MAX_DRAW_VERTEX_BUFFERS)
    );
    assert_eq!(
        limits.max_bind_set_descriptors, 12,
        "the host's per-set bound"
    );
    assert_eq!(limits.max_binding_number, MAX_BINDING_NUMBER);
    assert_eq!(limits.subgroup_size, 8);
    assert_eq!(limits.viewport_bounds_range, [-32768, 32767]);
    let mut fractional = Host::every();
    fractional.properties.limits.viewport_bounds = [-32768.5, 32767.5];
    assert_eq!(
        fractional
            .narrowed()
            .expect("stated")
            .info
            .limits
            .viewport_bounds_range,
        [-32768, 32767],
        "rounded inward, so no viewport admitted lies past the host's"
    );
    assert_eq!(limits.max_point_size, 255);
    assert_eq!(
        limits.max_image_dimension_2d, 8192,
        "a cube's face, a 2D image's layer, kept within the host's cube"
    );
    assert_eq!(limits.max_sampler_lod_bias, 15, "rounded down");
    assert_eq!(
        limits.max_storage_image_bytes, limits.max_allocation_size,
        "an image lies in one allocation"
    );
    assert_eq!(
        limits.max_invocation_memory, 0,
        "what the host states no counterpart of is none"
    );
    let mut small = Host::every();
    small.memory.heaps[0].size = 1 << 20;
    assert_eq!(
        small
            .narrowed()
            .expect("stated")
            .info
            .limits
            .max_allocation_size,
        1 << 20,
        "no larger than the largest heap"
    );
}

#[test]
fn sample_shading_is_stated_and_enabled_where_the_host_shades_each_sample() {
    for offered in [true, false] {
        let mut host = Host::every();
        host.core[6] = offered;
        let statement = host.narrowed().expect("stated");
        assert_eq!(
            statement
                .info
                .capabilities
                .contains(&Capability::SAMPLE_RATE_SHADING),
            offered
        );
        assert_eq!(statement.enables.core[6], offered, "sampleRateShading");
    }
}

#[test]
fn a_capability_every_vulkan_device_runs_is_stated_whatever_the_host_offers() {
    let mut host = Host::every();
    host.core.fill(false);
    host.core[0] = true;
    let statement = host.narrowed().expect("stated");
    for capability in [
        Capability::MATRIX,
        Capability::SHADER,
        Capability::SAMPLED1D,
        Capability::IMAGE1D,
        Capability::SAMPLED_BUFFER,
        Capability::IMAGE_BUFFER,
        Capability::IMAGE_QUERY,
        Capability::DERIVATIVE_CONTROL,
    ] {
        assert!(
            statement.info.capabilities.contains(&capability),
            "{capability:?} needs no device feature"
        );
    }
}

#[test]
fn a_capability_is_stated_where_the_host_runs_it_and_its_feature_enabled() {
    let mut host = Host::every();
    host.core[40] = false;
    host.set(Structure::PhysicalDevice16BitStorageFeatures, 2, false);
    let statement = host.narrowed().expect("stated");
    let runs = |capability| statement.info.capabilities.contains(&capability);
    assert!(!runs(Capability::INT64), "no 64-bit integers");
    assert!(!runs(Capability::STORAGE_PUSH_CONSTANT16));
    assert!(runs(Capability::FLOAT64) && runs(Capability::INT16));
    assert!(
        !tairix_spirv::CAPABILITIES
            .into_iter()
            .any(|capability| subgroup(capability) && runs(capability)),
        "no subgroup capability, however the host's subgroups run"
    );
    let enables = &statement.enables;
    assert!(enables.core[0], "robust buffer access");
    assert!(enables.core[39] && enables.core[41] && !enables.core[40]);
    let members = |structure| {
        enables
            .chain
            .iter()
            .find(|(asked, _)| *asked == structure)
            .map(|(_, members)| members.to_vec())
            .expect("enabled")
    };
    assert_eq!(
        members(Structure::PhysicalDeviceRobustness2Features),
        [true, true, false],
        "both robust accesses, no null descriptor"
    );
    assert_eq!(
        members(Structure::PhysicalDeviceZeroInitializeWorkgroupMemoryFeatures),
        [true]
    );
    assert_eq!(
        members(Structure::PhysicalDevice16BitStorageFeatures),
        [true, true, false, true]
    );
    assert_eq!(
        enables.extensions.as_slice(),
        &[
            extension::ROBUSTNESS2_KHR,
            extension::SAMPLER_MIRROR_CLAMP_TO_EDGE,
            extension::MAINTENANCE5,
            extension::DEPTH_RANGE_UNRESTRICTED
        ],
        "on a Vulkan 1.3 host the rest is core, its lines strict, and the mirror clamp, a point's \
         size and the depth range offered"
    );
    let mut wide = Host::every();
    wide.properties.subgroup_size = 128;
    let statement = wide.narrowed().expect("stated");
    assert_eq!(statement.info.limits.subgroup_size, MAX_SUBGROUP_SIZE);
}

#[test]
fn an_older_host_states_the_spirv_it_takes_and_enables_the_extensions_it_needs() {
    for (minor, spirv) in [(1, 3), (2, 5), (3, 6), (4, 6)] {
        let mut host = Host::every();
        host.properties.api_version = api_version(minor);
        host.extensions = host.extensions.without(HostExtensions::ROBUSTNESS2_KHR);
        let chain = feature_chain(host.properties.api_version, host.extensions);
        host.chained = chain
            .iter()
            .map(|&structure| {
                (
                    structure,
                    alloc::vec![true; chained_members(structure).expect("chained")],
                )
            })
            .collect();
        let statement = host.narrowed().expect("stated");
        assert_eq!(
            statement.info.spirv_version,
            Version {
                major: 1,
                minor: spirv
            },
            "Vulkan 1.{minor}"
        );
        let extensions = statement.enables.extensions.as_slice();
        assert_eq!(extensions[0], extension::ROBUSTNESS2_EXT);
        assert_eq!(
            extensions.contains(&extension::ZERO_INITIALIZE_WORKGROUP_MEMORY),
            minor < 3
        );
        assert_eq!(
            extensions.contains(&extension::SHADER_FLOAT16_INT8),
            minor < 2
        );
        assert_eq!(
            (statement.format_lists, statement.device_requirements),
            (minor >= 2, minor >= 3)
        );
    }
}

#[test]
fn an_extension_s_structure_is_chained_only_where_the_extension_is_enabled() {
    let mut host = Host::every();
    host.properties.api_version = api_version(1);
    host.extensions = host.extensions.without(HostExtensions::ROBUSTNESS2_KHR);
    host.chained = feature_chain(host.properties.api_version, host.extensions)
        .iter()
        .map(|&structure| {
            let members = chained_members(structure).expect("chained");
            (structure, alloc::vec![true; members])
        })
        .collect();
    host.set(Structure::PhysicalDeviceShaderFloat16Int8Features, 0, false);
    let statement = host.narrowed().expect("stated");
    assert!(!statement.info.capabilities.contains(&Capability::FLOAT16));
    let enables = &statement.enables;
    assert!(!enables.extensions.contains(&extension::SHADER_FLOAT16_INT8));
    assert!(
        enables
            .chain
            .iter()
            .all(|(structure, _)| *structure != Structure::PhysicalDeviceShaderFloat16Int8Features),
        "its features unchained with its extension not enabled"
    );
    assert!(enables.chain.iter().any(|(structure, _)| *structure
        == Structure::PhysicalDeviceZeroInitializeWorkgroupMemoryFeatures));
}

#[test]
fn a_host_the_backend_cannot_serve_or_whose_statement_is_none_is_refused() {
    let refused = |host: &Host| host.narrowed().err();
    let mut old = Host::every();
    old.properties.api_version = 1 << 22;
    assert_eq!(refused(&old), Some(DriverError::Unsupported));
    let mut transfers = Host::every();
    transfers.families = alloc::vec![HostQueueFamily {
        flags: queue::TRANSFER,
        queues: 2,
    }];
    assert_eq!(
        refused(&transfers),
        Some(DriverError::Unsupported),
        "no dispatches"
    );
    let mut incoherent = Host::every();
    incoherent.memory.types.truncate(2);
    assert_eq!(
        refused(&incoherent),
        Some(DriverError::Unsupported),
        "no memory type the seam names"
    );
    let mut unbounded = Host::every();
    unbounded.set(Structure::PhysicalDeviceRobustness2Features, 0, false);
    assert_eq!(refused(&unbounded), Some(DriverError::Unsupported));
    let mut unrobust = Host::every();
    unrobust.core[0] = false;
    assert_eq!(refused(&unrobust), Some(DriverError::Unsupported));
    let mut no_robustness = Host::every();
    no_robustness.extensions = no_robustness
        .extensions
        .without(HostExtensions::ROBUSTNESS2);
    no_robustness.properties.robust_alignments = None;
    no_robustness
        .chained
        .retain(|(structure, _)| *structure != Structure::PhysicalDeviceRobustness2Features);
    assert_eq!(refused(&no_robustness), Some(DriverError::Unsupported));
    let mut unzeroed = Host::every();
    unzeroed.set(
        Structure::PhysicalDeviceZeroInitializeWorkgroupMemoryFeatures,
        0,
        false,
    );
    assert_eq!(refused(&unzeroed), Some(DriverError::Unsupported));
    let mut odd = Host::every();
    odd.properties.limits.storage_offset_alignment = 24;
    assert_eq!(refused(&odd), Some(DriverError::DeviceFault));
    let mut huge = Host::every();
    huge.properties.limits.uniform_offset_alignment = 512;
    assert_eq!(refused(&huge), Some(DriverError::DeviceFault));
    let mut coarse = Host::every();
    coarse.properties.robust_alignments = Some(RobustAlignments {
        storage: 3,
        uniform: 16,
    });
    assert_eq!(refused(&coarse), Some(DriverError::DeviceFault));
    let mut uneven = Host::every();
    uneven.properties.subgroup_size = 48;
    assert_eq!(refused(&uneven), Some(DriverError::DeviceFault));
}

#[test]
fn a_bias_and_a_texel_buffer_s_offset_are_the_host_s_held_to_the_seam_s() {
    let stated = |change: &dyn Fn(&mut HostLimits)| {
        let mut host = Host::every();
        change(&mut host.properties.limits);
        host.narrowed().map(|statement| statement.info.limits)
    };
    for (bias, expected) in [
        (40.0, MAX_LOD_BIAS),
        (2.0, 2),
        (0.5, 0),
        (-3.0, 0),
        (f32::NAN, 0),
    ] {
        let limits = stated(&|limits| limits.sampler_lod_bias = bias).expect("stated");
        assert_eq!(
            limits.max_sampler_lod_bias, expected,
            "a host's bias of {bias}"
        );
    }
    let limits = stated(&|limits| limits.texel_offset_alignment = 128).expect("stated");
    assert_eq!(
        (limits.storage_buffer_alignment, limits.buffer_alignment),
        (128, 128),
        "a texel buffer's offset keeps a storage binding's alignment"
    );
    assert_eq!(limits.uniform_buffer_alignment, 64);
    for alignment in [3, 512] {
        assert_eq!(
            stated(&|limits| limits.texel_offset_alignment = alignment).err(),
            Some(DriverError::DeviceFault),
            "a texel buffer's offset alignment of {alignment}"
        );
    }
}

#[test]
fn a_buffer_s_start_keeps_every_copy_s_texel_where_the_bindings_ask_less() {
    let mut host = Host::every();
    let limits = &mut host.properties.limits;
    limits.uniform_offset_alignment = 4;
    limits.storage_offset_alignment = 4;
    limits.texel_offset_alignment = 4;
    let limits = host.narrowed().expect("stated").info.limits;
    assert_eq!(
        (
            limits.uniform_buffer_alignment,
            limits.storage_buffer_alignment
        ),
        (4, 4)
    );
    assert_eq!(limits.buffer_alignment, 16, "a four-channel float texel");
}

#[test]
fn depth_and_stencil_are_stated_only_of_a_family_running_graphics_work_chosen_where_one_is() {
    let mut host = Host::every();
    host.families = alloc::vec![
        HostQueueFamily {
            flags: queue::COMPUTE | queue::TRANSFER,
            queues: 4,
        },
        HostQueueFamily {
            flags: queue::GRAPHICS | queue::COMPUTE | queue::TRANSFER,
            queues: 2,
        },
    ];
    let with_graphics = host.narrowed().expect("stated");
    assert_eq!(with_graphics.family, 1, "the one running graphics work too");
    host.families.truncate(1);
    let compute_only = host.narrowed().expect("stated");
    assert_eq!(compute_only.family, 0);
    assert_eq!(
        compute_only.info.queue_families[0].capabilities,
        QueueCapabilities::TRANSFER.with(QueueCapabilities::COMPUTE),
        "no draw, as only a family running graphics work renders"
    );
    for format in Format::ALL {
        let (with, without) = (
            with_graphics.info.formats.get(format),
            compute_only.info.formats.get(format),
        );
        if format.aspects() == Aspects::COLOR {
            assert_eq!(without, with.without(DRAWN), "{format:?}");
        } else {
            assert_ne!(with, FormatFeatures::NONE, "{format:?}");
            assert_eq!(without, FormatFeatures::NONE, "{format:?}");
            assert!(!compute_only.formats[format.index()].copied(), "{format:?}");
        }
    }
}

#[test]
fn an_image_takes_whole_runs_of_the_host_s_buffer_image_granularity() {
    let mut host = Host::every();
    host.properties.limits.buffer_image_granularity = 1024;
    let statement = host.narrowed().expect("stated");
    assert_eq!(statement.image_granularity, 1024);
    let required = HostRequirements {
        size: 1500,
        alignment: 256,
        type_bits: u32::MAX,
    };
    let stated = statement.image_requirements(required).expect("stated");
    assert_eq!(
        (stated.size, stated.alignment),
        (2048, 1024),
        "no buffer placed beside it shares a run with it"
    );
    let finer = HostRequirements {
        alignment: 4096,
        ..required
    };
    let stated = statement.image_requirements(finer).expect("stated");
    assert_eq!((stated.size, stated.alignment), (2048, 4096));
    for refused in [3, 256 << 10] {
        host.properties.limits.buffer_image_granularity = refused;
        assert_eq!(
            host.narrowed().err(),
            Some(DriverError::DeviceFault),
            "a granularity of {refused}"
        );
    }
}

/// The robust-access members the device is made with.
fn robustness2(statement: &Statement) -> Vec<bool> {
    statement
        .enables
        .chain
        .iter()
        .find(|(structure, _)| *structure == Structure::PhysicalDeviceRobustness2Features)
        .map(|(_, members)| members.to_vec())
        .expect("chained")
}

#[test]
fn a_format_is_stated_of_an_image_copied_both_ways_whose_reads_are_held_to_its_edges() {
    let texels = FormatFeatures::UNIFORM_TEXEL_BUFFER
        .with(FormatFeatures::STORAGE_TEXEL_BUFFER)
        .with(UNFORMATTED_TEXELS);
    let every = Host::every().narrowed().expect("stated");
    assert_eq!(robustness2(&every), [true, true, false]);

    let mut unrobust = Host::every();
    unrobust.set(Structure::PhysicalDeviceRobustness2Features, 1, false);
    let statement = unrobust.narrowed().expect("stated");
    assert_eq!(robustness2(&statement), [true, false, false]);
    for format in Format::ALL {
        assert_eq!(
            statement.info.formats.get(format),
            FormatFeatures::from_bits(format.features().bits() & texels.with(drawn(format)).bits()),
            "{format:?}: a texel buffer's reads held to its edges and a pass's uses, an image's \
             reads not"
        );
    }

    let rgba = Format::Rgba8Unorm;
    let stated = |unoffered: u64, buffer_unoffered: u64| {
        let mut host = Host::every();
        host.formats[rgba.index()] = HostFormatFeatures {
            optimal: !unoffered,
            buffer: !buffer_unoffered,
        };
        host.narrowed().expect("stated").info.formats.get(rgba)
    };
    let sampled = FormatFeatures::SAMPLED.with(FormatFeatures::FILTERED);
    for transfer in [format_feature::TRANSFER_SRC, format_feature::TRANSFER_DST] {
        assert_eq!(
            stated(transfer, 0),
            texels.with(FormatFeatures::VERTEX_INPUT),
            "an image copied one way, and a buffer's uses"
        );
    }
    assert_eq!(
        stated(format_feature::SAMPLED_IMAGE_FILTER_LINEAR, 0),
        rgba.features().without(FormatFeatures::FILTERED)
    );
    assert_eq!(
        stated(format_feature::SAMPLED_IMAGE, 0),
        rgba.features().without(sampled),
        "filtering is of what is sampled"
    );
    assert_eq!(
        stated(format_feature::STORAGE_IMAGE, 0),
        rgba.features()
            .without(FormatFeatures::STORAGE.with(UNFORMATTED_STORAGE)),
        "reached unformatted is a storage image's"
    );
    for use_alone in [
        format_feature::UNIFORM_TEXEL_BUFFER,
        format_feature::STORAGE_TEXEL_BUFFER,
    ] {
        assert_eq!(
            stated(0, use_alone),
            rgba.features().without(texels),
            "a view of a buffer made for both texel uses needs both"
        );
    }
}

/// The formats Vulkan's device features for unformatted storage access
/// cover, of the seam's: every one Vulkan lists but the 8-bit alpha format
/// the seam has none of.
const COVERED: [Format; 39] = [
    Format::Rgba8Unorm,
    Format::Rgba8Snorm,
    Format::Rgba8Uint,
    Format::Rgba8Sint,
    Format::R32Uint,
    Format::R32Sint,
    Format::R32Float,
    Format::Rg32Uint,
    Format::Rg32Sint,
    Format::Rg32Float,
    Format::Rgba32Uint,
    Format::Rgba32Sint,
    Format::Rgba32Float,
    Format::Rgba16Uint,
    Format::Rgba16Sint,
    Format::Rgba16Float,
    Format::Rgba16Unorm,
    Format::Rgba16Snorm,
    Format::Rg16Float,
    Format::Rg16Unorm,
    Format::Rg16Snorm,
    Format::Rg16Sint,
    Format::Rg16Uint,
    Format::R16Float,
    Format::R16Unorm,
    Format::R16Snorm,
    Format::R16Sint,
    Format::R16Uint,
    Format::Rg8Unorm,
    Format::Rg8Snorm,
    Format::Rg8Sint,
    Format::Rg8Uint,
    Format::R8Unorm,
    Format::R8Snorm,
    Format::R8Sint,
    Format::R8Uint,
    Format::Rg11B10Float,
    Format::Rgb10A2Unorm,
    Format::Rgb10A2Uint,
];

/// A host of Vulkan 1.`minor`, offering every extension but the format
/// features' unless `extended`, and of every format only the features older
/// Vulkan names, and its device features for unformatted storage access
/// only where `granted`.
fn older(minor: u32, extended: bool, granted: bool) -> Host {
    let mut host = Host::every();
    host.properties.api_version = api_version(minor);
    if !extended {
        host.extensions = host
            .extensions
            .without(HostExtensions::FORMAT_FEATURE_FLAGS2);
    }
    host.formats = [HostFormatFeatures {
        optimal: OLDER_FORMAT_FEATURES,
        buffer: OLDER_FORMAT_FEATURES,
    }; Format::ALL.len()];
    host.core[core::SHADER_STORAGE_IMAGE_READ_WITHOUT_FORMAT] = granted;
    host.core[core::SHADER_STORAGE_IMAGE_WRITE_WITHOUT_FORMAT] = granted;
    host
}

const UNFORMATTED: [Capability; 2] = [
    Capability::STORAGE_IMAGE_READ_WITHOUT_FORMAT,
    Capability::STORAGE_IMAGE_WRITE_WITHOUT_FORMAT,
];

#[test]
fn a_host_stating_1_3_s_format_features_has_a_depth_compared_and_storage_unformatted_as_said() {
    let stated = |format: Format, (optimal, buffer): (u64, u64)| {
        let mut host = Host::every();
        host.formats[format.index()] = HostFormatFeatures {
            optimal: !optimal,
            buffer: !buffer,
        };
        host.narrowed().expect("stated").info.formats.get(format)
    };
    let comparison = (format_feature::SAMPLED_IMAGE_DEPTH_COMPARISON, 0);
    let sampled = FormatFeatures::SAMPLED.with(FormatFeatures::FILTERED);
    for depth in [Format::Depth16Unorm, Format::Depth24PlusStencil8] {
        assert_eq!(
            stated(depth, comparison),
            depth.features().without(sampled),
            "{depth:?}: a depth sampled is compared too"
        );
    }
    assert_eq!(
        stated(Format::Stencil8, comparison),
        Format::Stencil8.features(),
        "no stencil is compared"
    );
    let rgba = Format::Rgba8Unorm;
    let (read, write) = (
        format_feature::STORAGE_READ_WITHOUT_FORMAT,
        format_feature::STORAGE_WRITE_WITHOUT_FORMAT,
    );
    for (unoffered, unstated) in [
        ((read, 0), FormatFeatures::STORAGE_READ_WITHOUT_FORMAT),
        ((write, 0), FormatFeatures::STORAGE_WRITE_WITHOUT_FORMAT),
        ((0, read), FormatFeatures::TEXEL_BUFFER_READ_WITHOUT_FORMAT),
        (
            (0, write),
            FormatFeatures::TEXEL_BUFFER_WRITE_WITHOUT_FORMAT,
        ),
    ] {
        assert_eq!(
            stated(rgba, unoffered),
            rgba.features().without(unstated),
            "{unstated:?}"
        );
    }

    let mut featureless = Host::every();
    featureless.core[core::SHADER_STORAGE_IMAGE_READ_WITHOUT_FORMAT] = false;
    featureless.core[core::SHADER_STORAGE_IMAGE_WRITE_WITHOUT_FORMAT] = false;
    let statement = featureless.narrowed().expect("stated");
    for capability in UNFORMATTED {
        assert!(
            statement.info.runs(capability),
            "{capability:?}: each format's word decides, not a device feature"
        );
    }
    assert!(!statement.enables.core[core::SHADER_STORAGE_IMAGE_READ_WITHOUT_FORMAT]);
    assert!(!statement.enables.core[core::SHADER_STORAGE_IMAGE_WRITE_WITHOUT_FORMAT]);
    assert_eq!(statement.info.formats.get(rgba), rgba.features());
    assert!(!statement
        .enables
        .extensions
        .contains(&extension::FORMAT_FEATURE_FLAGS2));
}

#[test]
fn an_older_host_compares_each_depth_it_samples_and_its_features_grant_unformatted_storage() {
    let unformatted = UNFORMATTED_STORAGE.with(UNFORMATTED_TEXELS);
    for granted in [true, false] {
        let statement = older(2, false, granted).narrowed().expect("stated");
        for format in Format::ALL {
            // A host older than Vulkan 1.3 draws nothing.
            let undrawn = format.features().without(DRAWN);
            let expected = if granted {
                undrawn
            } else {
                undrawn.without(unformatted)
            };
            assert_eq!(
                statement.info.formats.get(format),
                expected,
                "{format:?}, the device features granted: {granted}"
            );
        }
        for capability in UNFORMATTED {
            assert_eq!(statement.info.runs(capability), granted, "{capability:?}");
        }
        assert_eq!(
            statement.enables.core[core::SHADER_STORAGE_IMAGE_READ_WITHOUT_FORMAT],
            granted
        );
    }

    let extended = older(2, true, true).narrowed().expect("stated");
    assert!(
        extended
            .enables
            .extensions
            .contains(&extension::FORMAT_FEATURE_FLAGS2),
        "what lets a module reach storage on its formats' word"
    );
    for format in Format::ALL {
        let mut expected = format.features().without(unformatted).without(DRAWN);
        if format.aspects().contains(Aspects::DEPTH) {
            expected = expected.without(FormatFeatures::SAMPLED.with(FormatFeatures::FILTERED));
        }
        assert_eq!(
            extended.info.formats.get(format),
            expected,
            "{format:?}: the extension's word, which states none of it"
        );
    }
}

#[test]
fn the_formats_an_older_host_s_features_cover_are_those_vulkan_lists() {
    let statement = older(2, false, true).narrowed().expect("stated");
    for format in Format::ALL {
        let held = statement.formats[format.index()].features;
        for (features, use_bit) in [
            (held.optimal, format_feature::STORAGE_IMAGE),
            (held.buffer, format_feature::STORAGE_TEXEL_BUFFER),
        ] {
            assert_eq!(features & use_bit, use_bit, "every use offered");
            assert_eq!(
                features & format_feature::STORAGE_READ_WITHOUT_FORMAT != 0,
                COVERED.contains(&format),
                "{format:?}"
            );
        }
    }
}

#[test]
fn a_depth_of_24_bits_is_stood_in_for_where_the_host_renders_none() {
    let (depth24, depth32) = (
        Format::Depth24PlusStencil8.index(),
        Format::Depth32FloatStencil8.index(),
    );
    let held = |host: &Host| host.narrowed().expect("stated").formats[depth24];
    let own = held(&Host::every());
    assert_eq!(own.format, host_format(Format::Depth24PlusStencil8));
    assert_eq!(own.features, EVERY_FEATURE);

    let mut unrendered = Host::every();
    unrendered.formats[depth24].optimal &= !format_feature::DEPTH_STENCIL_ATTACHMENT;
    let stood_in = held(&unrendered);
    assert_eq!(stood_in.format, host_format(Format::Depth32FloatStencil8));
    assert_eq!(stood_in.features, unrendered.formats[depth32]);

    unrendered.formats[depth32].optimal &= !format_feature::DEPTH_STENCIL_ATTACHMENT;
    let neither = held(&unrendered);
    assert_eq!(
        (neither.format, neither.features),
        (
            host_format(Format::Depth24PlusStencil8),
            unrendered.formats[depth24]
        ),
        "no stand-in that renders no better"
    );
}

#[test]
fn a_float_depth_format_is_stated_only_where_the_host_keeps_any_depth_copied_in() {
    let every = Host::every().narrowed().expect("stated");
    assert!(every
        .enables
        .extensions
        .contains(&extension::DEPTH_RANGE_UNRESTRICTED));
    let mut restricted = Host::every();
    restricted.extensions = restricted
        .extensions
        .without(HostExtensions::DEPTH_RANGE_UNRESTRICTED);
    let statement = restricted.narrowed().expect("stated");
    assert!(!statement
        .enables
        .extensions
        .contains(&extension::DEPTH_RANGE_UNRESTRICTED));
    for format in Format::ALL {
        let float_depth = matches!(format, Format::Depth32Float | Format::Depth32FloatStencil8);
        let expected = if float_depth {
            FormatFeatures::NONE
        } else {
            every_use(format)
        };
        assert_eq!(
            statement.info.formats.get(format),
            expected,
            "{format:?}: a depth of 24 bits copies no depth in, whatever stands in for it"
        );
    }
}

#[test]
fn a_sampler_feature_is_stated_where_the_host_offers_it_and_enabled() {
    let anisotropy = super::core::SAMPLER_ANISOTROPY;
    let every = Host::every().narrowed().expect("stated");
    assert!(every.enables.core[anisotropy]);
    assert!(
        !every.info.features.contains(Features::SHARED_MEMORY),
        "each context is a host device of its own"
    );
    assert!(every
        .enables
        .extensions
        .contains(&extension::SAMPLER_MIRROR_CLAMP_TO_EDGE));

    let mut plain = Host::every();
    plain.core[anisotropy] = false;
    plain.extensions = plain
        .extensions
        .without(HostExtensions::SAMPLER_MIRROR_CLAMP_TO_EDGE);
    let statement = plain.narrowed().expect("stated");
    assert_eq!(statement.info.features, DRAW_FEATURES, "no sampler feature");
    assert!(!statement.enables.core[anisotropy]);
    assert!(!statement
        .enables
        .extensions
        .contains(&extension::SAMPLER_MIRROR_CLAMP_TO_EDGE));
}

#[test]
fn the_statement_is_a_device_the_seam_takes() {
    let statement = Host::every().narrowed().expect("stated");
    assert!(Device::hardware(Idle, statement.info, &[1], Arc::new(Still)).is_ok());
}

/// Whether `statement` draws: a family that draws, the uses a pass makes,
/// and the structures drawing chains, each where the others are.
fn draws(statement: &Statement) -> bool {
    let family = statement.info.queue_families[0]
        .capabilities
        .contains(QueueCapabilities::GRAPHICS);
    let uses = Format::ALL
        .iter()
        .any(|&format| statement.info.formats.get(format).intersects(DRAWN));
    let chained = statement
        .enables
        .chain
        .iter()
        .any(|(structure, _)| *structure == Structure::PhysicalDeviceMaintenance5Features);
    assert_eq!(family, uses, "a family that draws with the uses it makes");
    assert_eq!(
        family, chained,
        "a family that draws with what it is made with"
    );
    assert_eq!(
        family,
        statement.info.features.intersects(DRAW_FEATURES),
        "a family that draws with its features"
    );
    family
}

#[test]
fn every_structure_the_driver_may_ask_is_chained_whole() {
    let chain = feature_chain(API_VERSION, every_extension());
    assert_eq!(chain.as_slice(), crate::venus::reply::CHAINED.as_slice());
}

/// The members `statement` enables of the chained structure `structure`.
fn chained_member(statement: &Statement, structure: Structure) -> Option<Vec<bool>> {
    statement
        .enables
        .chain
        .iter()
        .find(|(asked, _)| *asked == structure)
        .map(|(_, members)| members.to_vec())
}

#[test]
fn a_host_draws_only_with_the_software_device_s_meaning_and_is_made_to() {
    let every = Host::every().narrowed().expect("stated");
    assert!(draws(&every));
    let enables = &every.enables;
    for at in [
        core::FULL_DRAW_INDEX_UINT32,
        core::INDEPENDENT_BLEND,
        core::DEPTH_BIAS_CLAMP,
        core::MULTI_DRAW_INDIRECT,
        core::DRAW_INDIRECT_FIRST_INSTANCE,
    ] {
        assert!(enables.core[at], "core feature {at}");
    }
    for structure in [
        Structure::PhysicalDeviceDynamicRenderingFeatures,
        Structure::PhysicalDeviceMaintenance5Features,
    ] {
        assert_eq!(
            chained_member(&every, structure),
            Some(alloc::vec![true]),
            "{structure:?}"
        );
    }
    assert_eq!(
        chained_member(&every, Structure::PhysicalDeviceLineRasterizationFeatures),
        None,
        "strict lines need no line state"
    );
    assert!(!every.rectangular_lines);
}

/// A way a host may fall short of drawing with the software device's
/// meaning, and what makes it so.
type Short<'a> = (&'a str, &'a dyn Fn(&mut Host));

#[test]
fn a_host_short_of_the_software_device_s_meaning_draws_nothing_and_enables_none_of_it() {
    let shorts: [Short<'_>; 8] = [
        ("no point sized", &|host| {
            host.extensions = host.extensions.without(HostExtensions::MAINTENANCE5);
            host.rechain();
        }),
        ("no point sized by its feature", &|host| {
            host.set(Structure::PhysicalDeviceMaintenance5Features, 0, false);
        }),
        ("no rendering without a render pass object", &|host| {
            host.set(Structure::PhysicalDeviceDynamicRenderingFeatures, 0, false);
        }),
        ("a host older than Vulkan 1.3", &|host| {
            host.properties.api_version = api_version(2);
            host.rechain();
        }),
        ("indices of 24 bits", &|host| {
            host.core[core::FULL_DRAW_INDEX_UINT32] = false;
        }),
        ("one blend for every target", &|host| {
            host.core[core::INDEPENDENT_BLEND] = false;
        }),
        ("an unclamped bias", &|host| {
            host.core[core::DEPTH_BIAS_CLAMP] = false;
        }),
        ("lines of its own", &|host| {
            host.properties.limits.strict_lines = false;
            host.set(Structure::PhysicalDeviceLineRasterizationFeatures, 0, false);
        }),
    ];
    for (what, short) in shorts {
        let mut host = Host::every();
        short(&mut host);
        let statement = host.narrowed().expect("stated");
        assert!(!draws(&statement), "{what}");
        assert!(
            !statement
                .enables
                .extensions
                .contains(&extension::MAINTENANCE5),
            "{what}"
        );
        assert!(statement
            .enables
            .chain
            .iter()
            .all(|(structure, _)| !matches!(
                structure,
                Structure::PhysicalDeviceDynamicRenderingFeatures
                    | Structure::PhysicalDeviceLineRasterizationFeatures
                    | Structure::PhysicalDeviceMaintenance5Features
            )));
    }
}

#[test]
fn a_host_whose_lines_are_its_own_is_asked_for_rectangles_alone() {
    for (offered, enabled) in [
        (
            HostExtensions::LINE_RASTERIZATION,
            extension::LINE_RASTERIZATION_KHR,
        ),
        (
            HostExtensions::LINE_RASTERIZATION_EXT,
            extension::LINE_RASTERIZATION_EXT,
        ),
    ] {
        let mut host = Host::every();
        host.properties.limits.strict_lines = false;
        host.extensions = host
            .extensions
            .without(HostExtensions::LINE_RASTERIZATION)
            .with(offered);
        host.rechain();
        let statement = host.narrowed().expect("stated");
        assert!(draws(&statement));
        assert!(statement.rectangular_lines, "{enabled}");
        assert!(statement.enables.extensions.contains(&enabled));
        assert_eq!(
            chained_member(
                &statement,
                Structure::PhysicalDeviceLineRasterizationFeatures
            ),
            Some(alloc::vec![true, false, false, false, false, false]),
            "rectangular lines alone"
        );
    }
}

#[test]
fn a_draw_feature_is_stated_where_the_host_offers_it_and_enabled() {
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
        let mut host = Host::every();
        host.core[at] = false;
        let statement = host.narrowed().expect("stated");
        assert!(draws(&statement), "{feature:?}");
        assert_eq!(
            statement.info.features,
            Features::SAMPLER_ANISOTROPY
                .with(Features::MIRROR_CLAMP_TO_EDGE)
                .with(DRAW_FEATURES)
                .without(feature)
        );
        assert!(!statement.enables.core[at], "{feature:?} not enabled");
    }
}

#[test]
fn a_draw_s_limits_are_the_host_s_held_to_what_the_device_runs() {
    let every = Host::every().narrowed().expect("stated");
    assert_eq!(every.info.limits.max_draw_indirect_count, u32::MAX);
    assert_eq!(
        every.info.limits.max_image_dimension_2d, 8192,
        "the cube's side"
    );

    let mut single = Host::every();
    single.core[core::MULTI_DRAW_INDIRECT] = false;
    let statement = single.narrowed().expect("stated");
    assert_eq!(statement.info.limits.max_draw_indirect_count, 1);

    let mut checked = Host::every();
    checked.core[core::DRAW_INDIRECT_FIRST_INSTANCE] = false;
    let statement = checked.narrowed().expect("stated");
    assert_eq!(
        statement.info.limits.max_draw_indirect_count,
        super::CHECKED_RECORDS,
        "no more records than a slot holds checked"
    );

    let mut framed = Host::every();
    framed.properties.limits.framebuffer = [4096, 2048];
    let statement = framed.narrowed().expect("stated");
    assert_eq!(
        statement.info.limits.max_image_dimension_2d, 2048,
        "an attachment covered whole by a rendering instance's area"
    );
}

#[test]
fn four_samples_are_stated_where_the_host_makes_every_target_of_them_at_the_standard_positions() {
    let rgba = Format::Rgba8Unorm;
    let stated = |change: &dyn Fn(&mut Host)| {
        let mut host = Host::every();
        change(&mut host);
        host.narrowed().expect("stated").info.formats.get(rgba)
    };
    assert!(stated(&|_| {}).contains(FormatFeatures::MULTISAMPLED));
    let without = rgba.features().without(FormatFeatures::MULTISAMPLED);
    for (what, change) in [
        (
            "not asked, as no target",
            &(|host: &mut Host| host.multisampled[Format::Rgba8Unorm.index()] = None)
                as &dyn Fn(&mut Host),
        ),
        ("one sample alone", &|host: &mut Host| {
            host.multisampled[Format::Rgba8Unorm.index()] = Some(HostImageLimits {
                samples: 1,
                ..FOUR_SAMPLES
            });
        }),
        (
            "smaller than the largest two-dimensional image",
            &|host: &mut Host| {
                host.multisampled[Format::Rgba8Unorm.index()] = Some(HostImageLimits {
                    extent: [4096, 4096, 1],
                    ..FOUR_SAMPLES
                });
            },
        ),
        (
            "samples elsewhere than the standard positions",
            &|host: &mut Host| {
                host.properties.limits.standard_sample_locations = false;
            },
        ),
    ] {
        assert_eq!(stated(change), without, "{what}");
    }
}
