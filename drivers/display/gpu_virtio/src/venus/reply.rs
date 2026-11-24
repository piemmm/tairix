//! The replies the renderer writes, read in the order their commands asked
//! for them. The host is trusted to run work, not to write well-formed
//! bytes: every reply is read whole, its command and structure types and its
//! array sizes checked, and anything else refused as a device fault.

use tairix_abi::DriverError;
use tairix_inline::ArrayVec;

use super::{
    extension, Command, Structure, VkResult, CORE_FEATURES, EXTENSION_NAME_LEN, MAX_MEMORY_HEAPS,
    MAX_MEMORY_TYPES, PHYSICAL_DEVICE_NAME_LEN, UUID_LEN,
};

/// The most queue families the driver reads of a physical device.
pub(crate) const MAX_QUEUE_FAMILIES: usize = 16;

/// The most physical devices the driver names at once.
pub(crate) const MAX_PHYSICAL_DEVICES: usize = 8;

const FAULT: DriverError = DriverError::DeviceFault;

/// Replies one after another, read from the start.
pub(crate) struct Reader<'b> {
    bytes: &'b [u8],
}

impl<'b> Reader<'b> {
    pub(crate) const fn new(bytes: &'b [u8]) -> Self {
        Self { bytes }
    }

    fn take(&mut self, len: usize) -> Result<&'b [u8], DriverError> {
        let (taken, rest) = self.bytes.split_at_checked(len).ok_or(FAULT)?;
        self.bytes = rest;
        Ok(taken)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], DriverError> {
        self.take(N)?.try_into().map_err(|_| FAULT)
    }

    fn u32(&mut self) -> Result<u32, DriverError> {
        self.array().map(u32::from_le_bytes)
    }

    fn i32(&mut self) -> Result<i32, DriverError> {
        self.array().map(i32::from_le_bytes)
    }

    fn u64(&mut self) -> Result<u64, DriverError> {
        self.array().map(u64::from_le_bytes)
    }

    fn f32(&mut self) -> Result<f32, DriverError> {
        self.u32().map(f32::from_bits)
    }

    fn u32s<const N: usize>(&mut self) -> Result<[u32; N], DriverError> {
        self.sized(N)?;
        let mut out = [0; N];
        for value in &mut out {
            *value = self.u32()?;
        }
        Ok(out)
    }

    fn f32s<const N: usize>(&mut self) -> Result<[f32; N], DriverError> {
        self.sized(N)?;
        let mut out = [0.0; N];
        for value in &mut out {
            *value = self.f32()?;
        }
        Ok(out)
    }

    /// An array's size, which must be `len`.
    fn sized(&mut self, len: usize) -> Result<(), DriverError> {
        (self.u64()? == len as u64).then_some(()).ok_or(FAULT)
    }

    /// Skip `count` words.
    fn words(&mut self, count: usize) -> Result<(), DriverError> {
        self.take(count * 4).map(drop)
    }

    /// A reply's command.
    fn header(&mut self, command: Command) -> Result<(), DriverError> {
        (self.i32()? == command as i32).then_some(()).ok_or(FAULT)
    }

    fn result(&mut self) -> Result<VkResult, DriverError> {
        self.i32().map(VkResult::of)
    }

    /// A pointer the reply fills, which must be there.
    fn present(&mut self) -> Result<(), DriverError> {
        (self.u64()? == 1).then_some(()).ok_or(FAULT)
    }

    /// A structure's type, which must be `structure`, and its empty chain.
    fn structure(&mut self, structure: Structure) -> Result<(), DriverError> {
        let typed = self.i32()? == structure as i32;
        let unchained = self.u64()? == 0;
        (typed && unchained).then_some(()).ok_or(FAULT)
    }

    /// A reply of a command answering only its result.
    pub(crate) fn result_of(&mut self, command: Command) -> Result<VkResult, DriverError> {
        self.header(command)?;
        self.result()
    }

    /// A reply of a command that made the object it names, answering its
    /// result: the object exists only where it succeeded.
    pub(crate) fn made(&mut self, command: Command, id: u64) -> Result<VkResult, DriverError> {
        self.header(command)?;
        let result = self.result()?;
        self.present()?;
        (self.u64()? == id).then_some(result).ok_or(FAULT)
    }

    /// [`Command::EnumeratePhysicalDevices`]: its result, how many the
    /// instance has, and those named, at most `asked`.
    pub(crate) fn physical_devices(
        &mut self,
        asked: usize,
    ) -> Result<(VkResult, u32, ArrayVec<u64, MAX_PHYSICAL_DEVICES>), DriverError> {
        self.header(Command::EnumeratePhysicalDevices)?;
        let result = self.result()?;
        self.present()?;
        let count = self.u32()?;
        let named = usize::try_from(self.u64()?).map_err(|_| FAULT)?;
        if named > asked || named > count as usize {
            return Err(FAULT);
        }
        let mut ids = ArrayVec::new();
        for _ in 0..named {
            ids.try_push(self.u64()?).map_err(|_| FAULT)?;
        }
        Ok((result, count, ids))
    }

    /// A structure's type, which must be `structure`, with an extension
    /// chained to it.
    fn chained(&mut self, structure: Structure) -> Result<(), DriverError> {
        (self.i32()? == structure as i32)
            .then_some(())
            .ok_or(FAULT)?;
        self.present()
    }

    /// A Boolean member, which must be one or nought.
    fn boolean(&mut self) -> Result<bool, DriverError> {
        match self.u32()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(FAULT),
        }
    }

    /// The structures of a chain whose first is `first`, each with an
    /// extension chained to it but the last.
    fn chain_of(&mut self, first: Structure, rest: &[Structure]) -> Result<(), DriverError> {
        match rest.split_last() {
            None => self.structure(first),
            Some((&last, middle)) => {
                self.chained(first)?;
                for &structure in middle {
                    self.chained(structure)?;
                }
                self.structure(last)
            }
        }
    }

    /// [`Command::GetPhysicalDeviceProperties2`], as
    /// [`Encoder::get_physical_device_properties2`](super::encode::Encoder::get_physical_device_properties2)
    /// asks with `robustness` or without: the innermost extension's members
    /// first.
    pub(crate) fn properties(&mut self, robustness: bool) -> Result<HostProperties, DriverError> {
        self.header(Command::GetPhysicalDeviceProperties2)?;
        self.present()?;
        let robust_alignments = if robustness {
            self.chain_of(
                Structure::PhysicalDeviceProperties2,
                &[
                    Structure::PhysicalDeviceSubgroupProperties,
                    Structure::PhysicalDeviceMaintenance3Properties,
                    Structure::PhysicalDeviceRobustness2Properties,
                ],
            )?;
            Some(RobustAlignments {
                storage: self.u64()?,
                uniform: self.u64()?,
            })
        } else {
            self.chain_of(
                Structure::PhysicalDeviceProperties2,
                &[
                    Structure::PhysicalDeviceSubgroupProperties,
                    Structure::PhysicalDeviceMaintenance3Properties,
                ],
            )?;
            None
        };
        let max_set_descriptors = self.u32()?;
        let max_allocation_size = self.u64()?;
        let subgroup_size = self.u32()?;
        // The stages subgroup operations run in, the operations, and whether
        // quad operations run in every stage: no subgroup capability is
        // stated.
        self.words(3)?;
        let api_version = self.u32()?;
        let _driver_version = self.u32()?;
        let vendor = self.u32()?;
        let device = self.u32()?;
        let _device_type = self.i32()?;
        self.sized(PHYSICAL_DEVICE_NAME_LEN)?;
        let name: [u8; PHYSICAL_DEVICE_NAME_LEN] = self.array()?;
        self.sized(UUID_LEN)?;
        let _uuid: [u8; UUID_LEN] = self.array()?;
        let limits = self.limits()?;
        // The sparse-resource properties: five booleans the seam names none of.
        self.words(5)?;
        Ok(HostProperties {
            api_version,
            vendor,
            device,
            name,
            limits,
            subgroup_size,
            max_allocation_size,
            max_set_descriptors,
            robust_alignments,
        })
    }

    /// [`Command::EnumerateDeviceExtensionProperties`]: its result, how many
    /// extensions the physical device has, and which of those read, at most
    /// `asked`, the driver asks for.
    pub(crate) fn extensions(
        &mut self,
        asked: usize,
    ) -> Result<(VkResult, u32, HostExtensions), DriverError> {
        self.header(Command::EnumerateDeviceExtensionProperties)?;
        let result = self.result()?;
        self.present()?;
        let count = self.u32()?;
        let read = usize::try_from(self.u64()?).map_err(|_| FAULT)?;
        if read > asked || read > count as usize {
            return Err(FAULT);
        }
        let mut found = HostExtensions::NONE;
        for _ in 0..read {
            self.sized(EXTENSION_NAME_LEN)?;
            let name: [u8; EXTENSION_NAME_LEN] = self.array()?;
            let _spec_version = self.u32()?;
            let len = name.iter().position(|&byte| byte == 0).ok_or(FAULT)?;
            let named = name.get(..len).ok_or(FAULT)?;
            for (wanted, flag) in ASKED {
                if named == wanted.as_bytes() {
                    found = found.with(flag);
                }
            }
        }
        Ok((result, count, found))
    }

    /// [`Command::GetPhysicalDeviceFeatures2`], as
    /// [`Encoder::get_physical_device_features2`](super::encode::Encoder::get_physical_device_features2)
    /// asks with `chain`: each extension structure's Boolean members, in the
    /// chain's order, and the core features'.
    pub(crate) fn features(&mut self, chain: &[Structure]) -> Result<HostFeatures, DriverError> {
        self.header(Command::GetPhysicalDeviceFeatures2)?;
        self.present()?;
        self.chain_of(Structure::PhysicalDeviceFeatures2, chain)?;
        let mut chained: ArrayVec<ArrayVec<bool, MAX_CHAINED_MEMBERS>, MAX_CHAINED> =
            ArrayVec::new();
        for _ in chain {
            chained.try_push(ArrayVec::new()).map_err(|_| FAULT)?;
        }
        for (&structure, members) in chain.iter().zip(chained.as_mut_slice()).rev() {
            for _ in 0..chained_members(structure).ok_or(FAULT)? {
                members.try_push(self.boolean()?).map_err(|_| FAULT)?;
            }
        }
        let mut core = [false; CORE_FEATURES];
        for member in &mut core {
            *member = self.boolean()?;
        }
        Ok(HostFeatures { core, chained })
    }

    /// A physical device's per-set descriptor counts, the dynamic buffers'
    /// and input attachments' read past.
    fn set_descriptors(&mut self) -> Result<[u32; 5], DriverError> {
        let samplers = self.u32()?;
        let uniform_buffers = self.u32()?;
        let _uniform_dynamic = self.u32()?;
        let storage_buffers = self.u32()?;
        let _storage_dynamic = self.u32()?;
        let sampled_images = self.u32()?;
        let storage_images = self.u32()?;
        let _input_attachments = self.u32()?;
        Ok([
            samplers,
            uniform_buffers,
            storage_buffers,
            sampled_images,
            storage_images,
        ])
    }

    /// A physical device's limits, every member in its order, those the
    /// seam names kept.
    fn limits(&mut self) -> Result<HostLimits, DriverError> {
        let image_1d = self.u32()?;
        let image_2d = self.u32()?;
        let image_3d = self.u32()?;
        let image_cube = self.u32()?;
        let array_layers = self.u32()?;
        let texel_buffer_elements = self.u32()?;
        let uniform_buffer_range = self.u32()?;
        let storage_buffer_range = self.u32()?;
        let push_constants = self.u32()?;
        // Allocation and sampler counts.
        self.words(2)?;
        let buffer_image_granularity = self.u64()?;
        // The sparse address space.
        self.u64()?;
        let bound_sets = self.u32()?;
        // Per-stage descriptor counts, seven.
        self.words(7)?;
        let set_descriptors = self.set_descriptors()?;
        let vertex_attributes = self.u32()?;
        let vertex_bindings = self.u32()?;
        let vertex_attribute_offset = self.u32()?;
        let vertex_binding_stride = self.u32()?;
        let vertex_output_components = self.u32()?;
        // Tessellation, eight; geometry, five.
        self.words(13)?;
        let fragment_input_components = self.u32()?;
        // Fragment outputs, dual-source attachments, combined resources.
        self.words(3)?;
        let workgroup_memory = self.u32()?;
        let workgroup_count = self.u32s::<3>()?;
        let workgroup_invocations = self.u32()?;
        let workgroup_size = self.u32s::<3>()?;
        // Sub-pixel, sub-texel and mipmap precision, the largest index.
        self.words(4)?;
        let draw_indirect_count = self.u32()?;
        let sampler_lod_bias = self.f32()?;
        let sampler_anisotropy = self.f32()?;
        let _viewports = self.u32()?;
        let viewport_dimensions = self.u32s::<2>()?;
        let viewport_bounds = self.f32s::<2>()?;
        let _viewport_sub_pixel_bits = self.u32()?;
        let _map_alignment = self.u64()?;
        let texel_offset_alignment = self.u64()?;
        let uniform_offset_alignment = self.u64()?;
        let storage_offset_alignment = self.u64()?;
        // Texel and gather offsets, interpolation offsets and their bits.
        self.words(4 + 3)?;
        // Two members, not an array.
        let framebuffer = [self.u32()?, self.u32()?];
        // Its layers and its four sample counts.
        self.words(1 + 4)?;
        let colour_attachments = self.u32()?;
        // Sampled and storage image sample counts, the sample-mask words, the
        // timestamps, clip and cull distances, queue priorities.
        self.words(5 + 1 + 2 + 3 + 1)?;
        let point_sizes = self.f32s::<2>()?;
        let line_widths = self.f32s::<2>()?;
        // Their granularities.
        self.words(2)?;
        let strict_lines = self.boolean()?;
        let standard_sample_locations = self.boolean()?;
        // The optimal copy alignments and the non-coherent atom.
        self.u64()?;
        self.u64()?;
        self.u64()?;
        Ok(HostLimits {
            image_1d,
            image_2d,
            image_3d,
            image_cube,
            array_layers,
            buffer_image_granularity,
            texel_buffer_elements,
            uniform_buffer_range,
            storage_buffer_range,
            push_constants,
            bound_sets,
            set_descriptors,
            vertex_attributes,
            vertex_bindings,
            vertex_attribute_offset,
            vertex_binding_stride,
            vertex_output_components,
            fragment_input_components,
            workgroup_memory,
            workgroup_count,
            workgroup_invocations,
            workgroup_size,
            draw_indirect_count,
            sampler_lod_bias,
            sampler_anisotropy,
            viewport_dimensions,
            viewport_bounds,
            texel_offset_alignment,
            uniform_offset_alignment,
            storage_offset_alignment,
            colour_attachments,
            point_sizes,
            line_widths,
            framebuffer,
            strict_lines,
            standard_sample_locations,
        })
    }

    /// [`Command::GetPhysicalDeviceQueueFamilyProperties2`]: how many the
    /// physical device has, and those read, at most `asked`.
    pub(crate) fn queue_families(
        &mut self,
        asked: usize,
    ) -> Result<(u32, ArrayVec<HostQueueFamily, MAX_QUEUE_FAMILIES>), DriverError> {
        self.header(Command::GetPhysicalDeviceQueueFamilyProperties2)?;
        self.present()?;
        let count = self.u32()?;
        let read = usize::try_from(self.u64()?).map_err(|_| FAULT)?;
        if read > asked || read > count as usize {
            return Err(FAULT);
        }
        let mut families = ArrayVec::new();
        for _ in 0..read {
            self.structure(Structure::QueueFamilyProperties2)?;
            let flags = self.u32()?;
            let queues = self.u32()?;
            // Timestamp bits and the image transfer granularity.
            self.words(4)?;
            families
                .try_push(HostQueueFamily { flags, queues })
                .map_err(|_| FAULT)?;
        }
        Ok((count, families))
    }

    /// [`Command::GetPhysicalDeviceMemoryProperties2`].
    pub(crate) fn memory_properties(&mut self) -> Result<HostMemory, DriverError> {
        self.header(Command::GetPhysicalDeviceMemoryProperties2)?;
        self.present()?;
        self.structure(Structure::PhysicalDeviceMemoryProperties2)?;
        let type_count = self.u32()? as usize;
        self.sized(MAX_MEMORY_TYPES)?;
        let mut types = ArrayVec::new();
        for index in 0..MAX_MEMORY_TYPES {
            let kind = HostMemoryType {
                flags: self.u32()?,
                heap: self.u32()?,
            };
            if index < type_count {
                types.try_push(kind).map_err(|_| FAULT)?;
            }
        }
        let heap_count = self.u32()? as usize;
        self.sized(MAX_MEMORY_HEAPS)?;
        let mut heaps = ArrayVec::new();
        for index in 0..MAX_MEMORY_HEAPS {
            let heap = HostMemoryHeap {
                size: self.u64()?,
                flags: self.u32()?,
            };
            if index < heap_count {
                heaps.try_push(heap).map_err(|_| FAULT)?;
            }
        }
        if type_count > MAX_MEMORY_TYPES || heap_count > MAX_MEMORY_HEAPS {
            return Err(FAULT);
        }
        Ok(HostMemory { types, heaps })
    }

    /// [`Command::GetBufferMemoryRequirements2`].
    pub(crate) fn memory_requirements(
        &mut self,
        command: Command,
    ) -> Result<HostRequirements, DriverError> {
        self.header(command)?;
        self.present()?;
        self.structure(Structure::MemoryRequirements2)?;
        Ok(HostRequirements {
            size: self.u64()?,
            alignment: self.u64()?,
            type_bits: self.u32()?,
        })
    }

    /// [`Command::GetPhysicalDeviceFormatProperties2`], asked with Vulkan
    /// 1.3's format features chained where `stated`, whose 64-bit features it
    /// then answers.
    pub(crate) fn format_properties(
        &mut self,
        stated: bool,
    ) -> Result<HostFormatFeatures, DriverError> {
        self.header(Command::GetPhysicalDeviceFormatProperties2)?;
        self.present()?;
        if !stated {
            self.structure(Structure::FormatProperties2)?;
            let _linear = self.u32()?;
            return Ok(HostFormatFeatures {
                optimal: u64::from(self.u32()?),
                buffer: u64::from(self.u32()?),
            });
        }
        self.chain_of(
            Structure::FormatProperties2,
            &[Structure::FormatProperties3],
        )?;
        let _linear = self.u64()?;
        let features = HostFormatFeatures {
            optimal: self.u64()?,
            buffer: self.u64()?,
        };
        // The first 31 bits again, which the 64-bit features include.
        self.words(3)?;
        Ok(features)
    }

    /// [`Command::GetPhysicalDeviceImageFormatProperties2`], asked with no
    /// extension structure: the most an image of what was asked may be, or
    /// `None` where none of it is made, whose properties then mean nothing.
    pub(crate) fn image_format_properties(
        &mut self,
    ) -> Result<Option<HostImageLimits>, DriverError> {
        self.header(Command::GetPhysicalDeviceImageFormatProperties2)?;
        let result = self.result()?;
        self.present()?;
        self.structure(Structure::ImageFormatProperties2)?;
        let limits = HostImageLimits {
            // An extent is a structure of three words, not an array.
            extent: [self.u32()?, self.u32()?, self.u32()?],
            levels: self.u32()?,
            layers: self.u32()?,
            samples: self.u32()?,
        };
        let _resource_size = self.u64()?;
        match result {
            VkResult::Success => Ok(Some(limits)),
            // Host and device memory exhausted: the image may be made later.
            VkResult::Failed(-1 | -2) => Err(DriverError::OutOfMemory),
            VkResult::Failed(_) => Ok(None),
            VkResult::Partial(_) => Err(FAULT),
        }
    }

    /// A reply of `command`, which made the objects `ids` as an array
    /// rather than through a pointer — command buffers, descriptor sets and
    /// pipelines — answering its result: they exist only where it succeeded.
    pub(crate) fn allocated(
        &mut self,
        command: Command,
        ids: &[u64],
    ) -> Result<VkResult, DriverError> {
        self.header(command)?;
        let result = self.result()?;
        self.sized(ids.len())?;
        for &id in ids {
            if self.u64()? != id {
                return Err(FAULT);
            }
        }
        Ok(result)
    }
}

/// Every extension structure the driver chains to the core features.
pub(crate) const CHAINED: [Structure; 7] = [
    Structure::PhysicalDeviceRobustness2Features,
    Structure::PhysicalDeviceZeroInitializeWorkgroupMemoryFeatures,
    Structure::PhysicalDevice16BitStorageFeatures,
    Structure::PhysicalDeviceShaderFloat16Int8Features,
    Structure::PhysicalDeviceDynamicRenderingFeatures,
    Structure::PhysicalDeviceMaintenance5Features,
    Structure::PhysicalDeviceLineRasterizationFeatures,
];

/// The most extension structures the driver chains to the core features.
pub(crate) const MAX_CHAINED: usize = CHAINED.len();

/// The most Boolean members a chained feature structure holds.
pub(crate) const MAX_CHAINED_MEMBERS: usize = {
    let mut most = 0;
    let mut at = 0;
    while at < CHAINED.len() {
        match chained_members(CHAINED[at]) {
            Some(members) if members > most => most = members,
            Some(_) => {}
            None => panic!("a chained structure of no known members"),
        }
        at += 1;
    }
    most
};

/// How many Boolean members a feature structure the driver chains holds.
pub(crate) const fn chained_members(structure: Structure) -> Option<usize> {
    match structure {
        Structure::PhysicalDeviceRobustness2Features => Some(3),
        Structure::PhysicalDevice16BitStorageFeatures => Some(4),
        Structure::PhysicalDeviceShaderFloat16Int8Features => Some(2),
        Structure::PhysicalDeviceZeroInitializeWorkgroupMemoryFeatures
        | Structure::PhysicalDeviceDynamicRenderingFeatures
        | Structure::PhysicalDeviceMaintenance5Features => Some(1),
        Structure::PhysicalDeviceLineRasterizationFeatures => Some(6),
        _ => None,
    }
}

tairix_util::flags! {
    /// Which of the extensions the driver asks for a physical device offers.
    pub(crate) struct HostExtensions(u16) {
        const ROBUSTNESS2_KHR = 1;
        const ROBUSTNESS2_EXT = 1 << 1;
        const ZERO_INITIALIZE_WORKGROUP_MEMORY = 1 << 2;
        const SHADER_FLOAT16_INT8 = 1 << 3;
        const SAMPLER_MIRROR_CLAMP_TO_EDGE = 1 << 4;
        const DEPTH_RANGE_UNRESTRICTED = 1 << 5;
        const FORMAT_FEATURE_FLAGS2 = 1 << 6;
        const MAINTENANCE5 = 1 << 7;
        const LINE_RASTERIZATION_KHR = 1 << 8;
        const LINE_RASTERIZATION_EXT = 1 << 9;
    }
}

impl HostExtensions {
    /// Either name of the second robustness level.
    pub(crate) const ROBUSTNESS2: Self = Self::ROBUSTNESS2_KHR.with(Self::ROBUSTNESS2_EXT);
    /// Either name of line rasterization.
    pub(crate) const LINE_RASTERIZATION: Self =
        Self::LINE_RASTERIZATION_KHR.with(Self::LINE_RASTERIZATION_EXT);
}

/// The extensions the driver asks for, each by name.
pub(crate) const ASKED: [(&str, HostExtensions); 10] = [
    (extension::ROBUSTNESS2_KHR, HostExtensions::ROBUSTNESS2_KHR),
    (extension::ROBUSTNESS2_EXT, HostExtensions::ROBUSTNESS2_EXT),
    (
        extension::ZERO_INITIALIZE_WORKGROUP_MEMORY,
        HostExtensions::ZERO_INITIALIZE_WORKGROUP_MEMORY,
    ),
    (
        extension::SHADER_FLOAT16_INT8,
        HostExtensions::SHADER_FLOAT16_INT8,
    ),
    (
        extension::SAMPLER_MIRROR_CLAMP_TO_EDGE,
        HostExtensions::SAMPLER_MIRROR_CLAMP_TO_EDGE,
    ),
    (
        extension::DEPTH_RANGE_UNRESTRICTED,
        HostExtensions::DEPTH_RANGE_UNRESTRICTED,
    ),
    (
        extension::FORMAT_FEATURE_FLAGS2,
        HostExtensions::FORMAT_FEATURE_FLAGS2,
    ),
    (extension::MAINTENANCE5, HostExtensions::MAINTENANCE5),
    (
        extension::LINE_RASTERIZATION_KHR,
        HostExtensions::LINE_RASTERIZATION_KHR,
    ),
    (
        extension::LINE_RASTERIZATION_EXT,
        HostExtensions::LINE_RASTERIZATION_EXT,
    ),
];

/// A physical device's features: its core ones' Boolean members in their
/// order, and each extension structure's asked for, in the chain's order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct HostFeatures {
    pub(crate) core: [bool; CORE_FEATURES],
    pub(crate) chained: ArrayVec<ArrayVec<bool, MAX_CHAINED_MEMBERS>, MAX_CHAINED>,
}

/// The granularity a physical device bounds robust accesses at.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct RobustAlignments {
    pub(crate) storage: u64,
    pub(crate) uniform: u64,
}

/// What a physical device states of itself that the driver reads.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct HostProperties {
    pub(crate) api_version: u32,
    pub(crate) vendor: u32,
    pub(crate) device: u32,
    pub(crate) name: [u8; PHYSICAL_DEVICE_NAME_LEN],
    pub(crate) limits: HostLimits,
    /// The invocations its subgroups hold.
    pub(crate) subgroup_size: u32,
    /// The largest one allocation it makes.
    pub(crate) max_allocation_size: u64,
    /// The most descriptors one set layout holds.
    pub(crate) max_set_descriptors: u32,
    /// The granularity it bounds robust accesses at, where it was asked.
    pub(crate) robust_alignments: Option<RobustAlignments>,
}

/// The limits of a physical device the seam names.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) struct HostLimits {
    pub(crate) image_1d: u32,
    pub(crate) image_2d: u32,
    pub(crate) image_3d: u32,
    pub(crate) image_cube: u32,
    pub(crate) array_layers: u32,
    /// The run of bytes past which a buffer and an image share no memory.
    pub(crate) buffer_image_granularity: u64,
    pub(crate) texel_buffer_elements: u32,
    pub(crate) uniform_buffer_range: u32,
    pub(crate) storage_buffer_range: u32,
    pub(crate) push_constants: u32,
    pub(crate) bound_sets: u32,
    /// A set's samplers, uniform buffers, storage buffers, sampled images and
    /// storage images.
    pub(crate) set_descriptors: [u32; 5],
    pub(crate) vertex_attributes: u32,
    pub(crate) vertex_bindings: u32,
    pub(crate) vertex_attribute_offset: u32,
    pub(crate) vertex_binding_stride: u32,
    pub(crate) vertex_output_components: u32,
    pub(crate) fragment_input_components: u32,
    pub(crate) workgroup_memory: u32,
    pub(crate) workgroup_count: [u32; 3],
    pub(crate) workgroup_invocations: u32,
    pub(crate) workgroup_size: [u32; 3],
    pub(crate) draw_indirect_count: u32,
    pub(crate) sampler_lod_bias: f32,
    pub(crate) sampler_anisotropy: f32,
    pub(crate) viewport_dimensions: [u32; 2],
    pub(crate) viewport_bounds: [f32; 2],
    pub(crate) texel_offset_alignment: u64,
    pub(crate) uniform_offset_alignment: u64,
    pub(crate) storage_offset_alignment: u64,
    pub(crate) colour_attachments: u32,
    pub(crate) point_sizes: [f32; 2],
    pub(crate) line_widths: [f32; 2],
    /// The widest and tallest a rendering instance's area reaches.
    pub(crate) framebuffer: [u32; 2],
    /// Whether its lines are rectangles extruded from them.
    pub(crate) strict_lines: bool,
    /// Whether four samples lie at Vulkan's standard positions.
    pub(crate) standard_sample_locations: bool,
}

/// A queue family's flags and its queues.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct HostQueueFamily {
    pub(crate) flags: u32,
    pub(crate) queues: u32,
}

/// A memory type's flags and its heap.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct HostMemoryType {
    pub(crate) flags: u32,
    pub(crate) heap: u32,
}

/// A memory heap's bytes and flags.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct HostMemoryHeap {
    pub(crate) size: u64,
    pub(crate) flags: u32,
}

/// A physical device's memory types and heaps.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct HostMemory {
    pub(crate) types: ArrayVec<HostMemoryType, MAX_MEMORY_TYPES>,
    pub(crate) heaps: ArrayVec<HostMemoryHeap, MAX_MEMORY_HEAPS>,
}

/// What a format offers an image laid out as the device chooses, and a
/// buffer.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct HostFormatFeatures {
    pub(crate) optimal: u64,
    pub(crate) buffer: u64,
}

/// The most an image of one format, kind, usage and creation flags may be.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct HostImageLimits {
    pub(crate) extent: [u32; 3],
    pub(crate) levels: u32,
    pub(crate) layers: u32,
    pub(crate) samples: u32,
}

/// What a buffer or an image needs of the memory it is bound to.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct HostRequirements {
    pub(crate) size: u64,
    pub(crate) alignment: u64,
    pub(crate) type_bits: u32,
}
