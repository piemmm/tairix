//! The replies the renderer writes, read in the order their commands asked
//! for them. The host is trusted to run work, not to write well-formed
//! bytes: every reply is read whole, its command and structure types and its
//! array sizes checked, and anything else refused as a device fault.

use tairix_abi::DriverError;
use tairix_inline::ArrayVec;

use super::{
    Command, Structure, VkResult, MAX_MEMORY_HEAPS, MAX_MEMORY_TYPES, PHYSICAL_DEVICE_NAME_LEN,
    UUID_LEN,
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

    /// [`Command::GetPhysicalDeviceProperties2`], as
    /// [`Encoder::get_physical_device_properties2`](super::encode::Encoder::get_physical_device_properties2)
    /// asks: the innermost extension's members first.
    pub(crate) fn properties(&mut self) -> Result<HostProperties, DriverError> {
        self.header(Command::GetPhysicalDeviceProperties2)?;
        self.present()?;
        self.chained(Structure::PhysicalDeviceProperties2)?;
        self.chained(Structure::PhysicalDeviceSubgroupProperties)?;
        self.structure(Structure::PhysicalDeviceMaintenance3Properties)?;
        let max_set_descriptors = self.u32()?;
        let max_allocation_size = self.u64()?;
        let subgroup_size = self.u32()?;
        // The stages and operations a subgroup offers, and its quads'.
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
        })
    }

    /// A physical device's limits, every member in its order, those the
    /// seam names kept.
    fn limits(&mut self) -> Result<HostLimits, DriverError> {
        let image_1d = self.u32()?;
        let image_2d = self.u32()?;
        let image_3d = self.u32()?;
        let _cube = self.u32()?;
        let array_layers = self.u32()?;
        let texel_buffer_elements = self.u32()?;
        let uniform_buffer_range = self.u32()?;
        let storage_buffer_range = self.u32()?;
        let push_constants = self.u32()?;
        // Allocation and sampler counts, the buffer-image granularity and the
        // sparse address space.
        self.words(2)?;
        self.u64()?;
        self.u64()?;
        let bound_sets = self.u32()?;
        // Per-stage descriptor counts, seven.
        self.words(7)?;
        let set_samplers = self.u32()?;
        let set_uniform_buffers = self.u32()?;
        let _set_uniform_dynamic = self.u32()?;
        let set_storage_buffers = self.u32()?;
        let _set_storage_dynamic = self.u32()?;
        let set_sampled_images = self.u32()?;
        let set_storage_images = self.u32()?;
        let _set_input_attachments = self.u32()?;
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
        let _lod_bias = self.f32()?;
        let sampler_anisotropy = self.f32()?;
        let _viewports = self.u32()?;
        let viewport_dimensions = self.u32s::<2>()?;
        let viewport_bounds = self.f32s::<2>()?;
        let _viewport_sub_pixel_bits = self.u32()?;
        let _map_alignment = self.u64()?;
        let _texel_offset_alignment = self.u64()?;
        let uniform_offset_alignment = self.u64()?;
        let storage_offset_alignment = self.u64()?;
        // Texel and gather offsets, interpolation offsets and their bits, the
        // framebuffer's extent and its four sample counts.
        self.words(4 + 3 + 3 + 4)?;
        let colour_attachments = self.u32()?;
        // Sampled and storage image sample counts, the sample-mask words, the
        // timestamps, clip and cull distances, queue priorities.
        self.words(5 + 1 + 2 + 3 + 1)?;
        let point_sizes = self.f32s::<2>()?;
        let line_widths = self.f32s::<2>()?;
        // Their granularities, strict lines and standard sample locations.
        self.words(4)?;
        // The optimal copy alignments and the non-coherent atom.
        self.u64()?;
        self.u64()?;
        self.u64()?;
        Ok(HostLimits {
            image_1d,
            image_2d,
            image_3d,
            array_layers,
            texel_buffer_elements,
            uniform_buffer_range,
            storage_buffer_range,
            push_constants,
            bound_sets,
            set_descriptors: [
                set_samplers,
                set_uniform_buffers,
                set_storage_buffers,
                set_sampled_images,
                set_storage_images,
            ],
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
            sampler_anisotropy,
            viewport_dimensions,
            viewport_bounds,
            uniform_offset_alignment,
            storage_offset_alignment,
            colour_attachments,
            point_sizes,
            line_widths,
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
    pub(crate) fn memory_requirements(&mut self) -> Result<HostRequirements, DriverError> {
        self.header(Command::GetBufferMemoryRequirements2)?;
        self.present()?;
        self.structure(Structure::MemoryRequirements2)?;
        Ok(HostRequirements {
            size: self.u64()?,
            alignment: self.u64()?,
            type_bits: self.u32()?,
        })
    }

    /// [`Command::AllocateCommandBuffers`] of `ids`.
    pub(crate) fn command_buffers(&mut self, ids: &[u64]) -> Result<VkResult, DriverError> {
        self.header(Command::AllocateCommandBuffers)?;
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
}

/// The limits of a physical device the seam names.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) struct HostLimits {
    pub(crate) image_1d: u32,
    pub(crate) image_2d: u32,
    pub(crate) image_3d: u32,
    pub(crate) array_layers: u32,
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
    pub(crate) sampler_anisotropy: f32,
    pub(crate) viewport_dimensions: [u32; 2],
    pub(crate) viewport_bounds: [f32; 2],
    pub(crate) uniform_offset_alignment: u64,
    pub(crate) storage_offset_alignment: u64,
    pub(crate) colour_attachments: u32,
    pub(crate) point_sizes: [f32; 2],
    pub(crate) line_widths: [f32; 2],
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

/// What a buffer needs of the memory it is bound to.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct HostRequirements {
    pub(crate) size: u64,
    pub(crate) alignment: u64,
    pub(crate) type_bits: u32,
}
