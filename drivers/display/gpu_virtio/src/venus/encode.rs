//! The commands the driver sends, encoded as the renderer decodes them: every
//! item a whole number of four-byte granules in little-endian order, a 64-bit
//! value with no further alignment; a handle the 64-bit id the driver chose
//! for it; a pointer or array a 64-bit marker or count ahead of what it
//! points at; a structure its type, then its chain of extensions, then its
//! members. An out-parameter is sent as its skeleton — its type, its chain
//! and the sizes of the arrays it holds — for the renderer to fill.

use alloc::vec::Vec;

use tairix_abi::DriverError;

use super::{
    image, Command, Structure, ACCESS_HOST_READ, ACCESS_MEMORY_READ, ACCESS_MEMORY_WRITE,
    API_VERSION, CORE_FEATURES, MAX_MEMORY_HEAPS, MAX_MEMORY_TYPES, STAGE_ALL_COMMANDS, STAGE_HOST,
    STAGE_TOP_OF_PIPE,
};

/// A queue family index no ownership transfer names.
const QUEUE_FAMILY_IGNORED: u32 = u32::MAX;

/// A command's flag asking the renderer to write its reply.
const GENERATE_REPLY: u32 = 1;

/// A span of a blob resource the renderer reads commands from or writes
/// replies into.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct StreamSpan {
    pub(crate) resource: u32,
    pub(crate) offset: u64,
    pub(crate) size: u64,
}

/// One region of a buffer copy.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct CopyRegion {
    pub(crate) src: u64,
    pub(crate) dst: u64,
    pub(crate) size: u64,
}

/// A structure an extension chains to another, and its Boolean members.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct Chained<'a> {
    pub(crate) structure: Structure,
    pub(crate) members: &'a [bool],
}

/// What a device is made with besides its queues: the extensions it enables,
/// its core features' Boolean members in their order, and each extension
/// structure chained after them.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Enabled<'a> {
    pub(crate) extensions: &'a [&'a str],
    pub(crate) core: &'a [bool; CORE_FEATURES],
    pub(crate) chain: &'a [Chained<'a>],
}

/// What one descriptor of a bind set binds.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Written {
    /// `range` bytes at `offset` into `buffer`.
    Buffer {
        buffer: u64,
        offset: u64,
        range: u64,
    },
    /// A sampler, a view, or both, the one not bound naming nothing.
    Image { sampler: u64, view: u64 },
    /// A texel buffer.
    Texels(u64),
}

/// One descriptor a bind set binds, of `kind` at `binding`.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct DescriptorWrite {
    pub(crate) binding: u32,
    pub(crate) kind: u32,
    pub(crate) written: Written,
}

/// How an image is made: laid out as the device chooses, used by one queue
/// family at a time and starting undefined, beside what this names.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct ImageInfo {
    pub(crate) flags: u32,
    pub(crate) kind: i32,
    pub(crate) format: i32,
    pub(crate) extent: [u32; 3],
    pub(crate) levels: u32,
    pub(crate) layers: u32,
    pub(crate) samples: i32,
    pub(crate) usage: u32,
    /// Its own format and the other its views may read it as, where the
    /// host takes such a list.
    pub(crate) view_formats: Option<[i32; 2]>,
}

/// Planes, levels and layers of an image.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct Subresources {
    pub(crate) aspects: u32,
    pub(crate) base_level: u32,
    pub(crate) levels: u32,
    pub(crate) base_layer: u32,
    pub(crate) layers: u32,
}

/// One plane, level and run of `count` layers of an image, as a copy names
/// it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct Layers {
    pub(crate) aspect: u32,
    pub(crate) level: u32,
    pub(crate) base_layer: u32,
    pub(crate) count: u32,
}

/// One region of a copy between a buffer and an image: in the buffer, from
/// `offset`, rows `row_length` texels apart and slices `image_height` rows.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct BufferImageRegion {
    pub(crate) offset: u64,
    pub(crate) row_length: u32,
    pub(crate) image_height: u32,
    pub(crate) layers: Layers,
    pub(crate) at: [i32; 3],
    pub(crate) extent: [u32; 3],
}

/// One region of a copy between images.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct ImageRegion {
    pub(crate) src: (Layers, [i32; 3]),
    pub(crate) dst: (Layers, [i32; 3]),
    pub(crate) extent: [u32; 3],
}

/// How a view sees its image, and the uses it is made for: those of its
/// image its format takes.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct ViewInfo {
    pub(crate) image: u64,
    pub(crate) kind: i32,
    pub(crate) format: i32,
    pub(crate) range: Subresources,
    pub(crate) usage: u32,
}

/// How a sampler samples: its filters, address modes, bias, anisotropy where
/// it filters past one texel, comparison where it compares, level clamps,
/// border colour and whether its coordinates are unnormalised.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) struct SamplerInfo {
    pub(crate) filters: [i32; 3],
    pub(crate) address: [i32; 3],
    pub(crate) lod_bias: f32,
    pub(crate) anisotropy: Option<f32>,
    pub(crate) compare: Option<i32>,
    pub(crate) lod: [f32; 2],
    pub(crate) border: i32,
    pub(crate) unnormalized: bool,
}

/// What an image of one format, kind, usage and creation flags is asked to
/// be made of.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct FormatQuery {
    pub(crate) format: i32,
    pub(crate) kind: i32,
    pub(crate) usage: u32,
    pub(crate) flags: u32,
    /// The formats the image's views read it as, listed as its making lists
    /// them, so the limits answered are the ones it is made within.
    pub(crate) view_formats: Option<[i32; 2]>,
}

/// One binding of a descriptor set layout.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct LayoutEntry {
    pub(crate) binding: u32,
    pub(crate) kind: u32,
    pub(crate) count: u32,
    pub(crate) stages: u32,
}

/// A clear as the host names it: a colour's kind and its four channels' bits,
/// or a depth and a stencil value.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) enum HostClear {
    Colour(u32, [u32; 4]),
    DepthStencil(f32, u32),
}

/// A stage of a render pipeline: its stage flag, its module and its entry
/// point.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct StageInfo<'a> {
    pub(crate) stage: u32,
    pub(crate) module: u64,
    pub(crate) entry: &'a str,
}

/// A vertex attribute: its location, the vertex buffer feeding it, its
/// format and the byte it starts at in each element.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct AttributeInfo {
    pub(crate) location: u32,
    pub(crate) binding: u32,
    pub(crate) format: i32,
    pub(crate) offset: u32,
}

/// How a render pipeline's primitives are rasterised, as the host names it.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) struct RasterInfo {
    pub(crate) depth_clamp: bool,
    pub(crate) cull: u32,
    pub(crate) front_face: u32,
    /// Its depth bias's constant, clamp and slope, where it has one.
    pub(crate) bias: Option<[f32; 3]>,
    pub(crate) line_width: f32,
    /// Whether its lines are asked for as rectangles: where the host's own
    /// are not.
    pub(crate) rectangular_lines: bool,
}

/// A face's stencil test and writes as the host names them: what it does
/// where the test fails, where both pass and where depth fails, its
/// comparison, and the bits compared and written.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct StencilInfo {
    pub(crate) ops: [u32; 3],
    pub(crate) compare: i32,
    pub(crate) compare_mask: u32,
    pub(crate) write_mask: u32,
}

/// A colour target's blend as the host names it, where it blends: the
/// colour's factors and operation, then the alpha's; and the channels a
/// draw writes.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct TargetInfo {
    pub(crate) blend: Option<[u32; 6]>,
    pub(crate) write_mask: u32,
}

/// A render pipeline as the host makes it, its viewport, scissor, stencil
/// reference and blend constants set by each draw.
#[derive(Copy, Clone, Debug)]
pub(crate) struct GraphicsInfo<'a> {
    pub(crate) stages: &'a [StageInfo<'a>],
    /// Each vertex buffer's stride and input rate, by binding.
    pub(crate) bindings: &'a [(u32, u32)],
    pub(crate) attributes: &'a [AttributeInfo],
    pub(crate) topology: u32,
    pub(crate) restart: bool,
    pub(crate) raster: RasterInfo,
    pub(crate) samples: u32,
    pub(crate) sample_mask: u32,
    pub(crate) alpha_to_coverage: bool,
    /// Its depth test's comparison and whether a passing fragment writes,
    /// where its target holds depth.
    pub(crate) depth: Option<(i32, bool)>,
    /// Its front and back faces' stencil tests, where it has them.
    pub(crate) stencil: Option<[StencilInfo; 2]>,
    pub(crate) logic_op: Option<u32>,
    pub(crate) targets: &'a [TargetInfo],
    pub(crate) colour_formats: &'a [i32],
    /// The host's formats of its depth and stencil targets: undefined, nought,
    /// for what it renders none of.
    pub(crate) depth_format: i32,
    pub(crate) stencil_format: i32,
}

/// An attachment of a rendering instance: its view, the one-sample view its
/// samples are averaged into as the instance ends, and what it is cleared
/// to as it begins, else loaded.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) struct AttachmentInfo {
    pub(crate) view: u64,
    pub(crate) resolve: Option<u64>,
    pub(crate) clear: Option<HostClear>,
}

/// A draw's vertices and instances, as the host's direct draws name them.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum DrawInfo {
    /// Its vertices' count, instances' count, first vertex and first
    /// instance.
    Vertices([u32; 4]),
    /// Its indices' count, instances' count, first index, the word added to
    /// each index and its first instance.
    Indexed([u32; 3], i32, u32),
}

/// Commands, encoded one after another.
#[derive(Default)]
pub(crate) struct Encoder {
    bytes: Vec<u8>,
}

impl Encoder {
    /// What has been encoded.
    pub(crate) fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Let go of what has been encoded, keeping its room.
    pub(crate) fn clear(&mut self) {
        self.bytes.clear();
    }

    /// The commands `other` encoded, after these.
    pub(crate) fn append(&mut self, other: &Self) -> Result<(), DriverError> {
        self.put(&other.bytes)
    }

    fn put(&mut self, bytes: &[u8]) -> Result<(), DriverError> {
        self.bytes
            .try_reserve(bytes.len())
            .map_err(|_| DriverError::OutOfMemory)?;
        self.bytes.extend_from_slice(bytes);
        Ok(())
    }

    fn u32(&mut self, value: u32) -> Result<(), DriverError> {
        self.put(&value.to_le_bytes())
    }

    fn i32(&mut self, value: i32) -> Result<(), DriverError> {
        self.put(&value.to_le_bytes())
    }

    fn u64(&mut self, value: u64) -> Result<(), DriverError> {
        self.put(&value.to_le_bytes())
    }

    fn f32(&mut self, value: f32) -> Result<(), DriverError> {
        self.u32(value.to_bits())
    }

    /// A null pointer, or an empty array or string.
    fn null(&mut self) -> Result<(), DriverError> {
        self.u64(0)
    }

    /// A pointer to what follows.
    fn present(&mut self) -> Result<(), DriverError> {
        self.u64(1)
    }

    /// The count of the array that follows.
    fn count(&mut self, count: usize) -> Result<(), DriverError> {
        self.u64(count as u64)
    }

    /// `count` as an array's 32-bit count member.
    fn count32(&mut self, count: usize) -> Result<(), DriverError> {
        self.u32(u32::try_from(count).map_err(|_| DriverError::LengthOutOfRange)?)
    }

    /// A string: its length with its terminator, then its bytes and the
    /// terminator, padded to whole granules.
    fn string(&mut self, text: &str) -> Result<(), DriverError> {
        let len = text.len() + 1;
        self.count(len)?;
        self.put(text.as_bytes())?;
        let pad = len.next_multiple_of(4) - text.len();
        self.put(&[0; 4][..pad])
    }

    /// A Boolean member.
    fn boolean(&mut self, value: bool) -> Result<(), DriverError> {
        self.u32(u32::from(value))
    }

    /// The extension structures `chain` names, chained in order after the
    /// structure just written: each one's type, then the chain's end, then
    /// their members, the innermost first, as the renderer reads a chain.
    fn chain(
        &mut self,
        chain: &[Structure],
        members: impl Fn(&mut Self, usize) -> Result<(), DriverError>,
    ) -> Result<(), DriverError> {
        for &structure in chain {
            self.present()?;
            self.i32(structure as i32)?;
        }
        self.null()?;
        (0..chain.len()).rev().try_for_each(|at| members(self, at))
    }

    fn command(&mut self, command: Command, reply: bool) -> Result<(), DriverError> {
        self.i32(command as i32)?;
        self.u32(if reply { GENERATE_REPLY } else { 0 })
    }

    /// A structure's type and an empty chain.
    fn structure(&mut self, structure: Structure) -> Result<(), DriverError> {
        self.i32(structure as i32)?;
        self.null()
    }

    /// No layers and no extensions: each a zero count and a null array.
    fn no_names(&mut self) -> Result<(), DriverError> {
        for _ in 0..2 {
            self.u32(0)?;
            self.null()?;
        }
        Ok(())
    }

    /// The instance `instance`, at [`API_VERSION`], with no layer or
    /// extension.
    pub(crate) fn create_instance(&mut self, instance: u64) -> Result<(), DriverError> {
        self.command(Command::CreateInstance, true)?;
        self.present()?;
        self.structure(Structure::InstanceCreateInfo)?;
        self.u32(0)?;
        self.present()?;
        self.structure(Structure::ApplicationInfo)?;
        // No application or engine name, nor their versions.
        self.null()?;
        self.u32(0)?;
        self.null()?;
        self.u32(0)?;
        self.u32(API_VERSION)?;
        self.no_names()?;
        self.null()?;
        self.present()?;
        self.u64(instance)
    }

    /// The instance's physical devices: how many there are where `ids` is
    /// empty, else the first of them named `ids`.
    pub(crate) fn enumerate_physical_devices(
        &mut self,
        instance: u64,
        ids: &[u64],
    ) -> Result<(), DriverError> {
        self.command(Command::EnumeratePhysicalDevices, true)?;
        self.u64(instance)?;
        self.present()?;
        self.u32(u32::try_from(ids.len()).map_err(|_| DriverError::LengthOutOfRange)?)?;
        if ids.is_empty() {
            return self.null();
        }
        self.count(ids.len())?;
        ids.iter().try_for_each(|&id| self.u64(id))
    }

    /// The physical device's properties, with its subgroup's, its largest
    /// allocation's, and where `robustness` its robust accesses' chained to
    /// them, as skeletons for the renderer to fill.
    pub(crate) fn get_physical_device_properties2(
        &mut self,
        physical: u64,
        robustness: bool,
    ) -> Result<(), DriverError> {
        self.command(Command::GetPhysicalDeviceProperties2, true)?;
        self.u64(physical)?;
        self.present()?;
        self.i32(Structure::PhysicalDeviceProperties2 as i32)?;
        let chain: &[Structure] = if robustness {
            &[
                Structure::PhysicalDeviceSubgroupProperties,
                Structure::PhysicalDeviceMaintenance3Properties,
                Structure::PhysicalDeviceRobustness2Properties,
            ]
        } else {
            &[
                Structure::PhysicalDeviceSubgroupProperties,
                Structure::PhysicalDeviceMaintenance3Properties,
            ]
        };
        self.chain(chain, |_, _| Ok(()))
    }

    /// The physical device's features, with `chain`'s extension structures
    /// chained to them, as skeletons for the renderer to fill.
    pub(crate) fn get_physical_device_features2(
        &mut self,
        physical: u64,
        chain: &[Structure],
    ) -> Result<(), DriverError> {
        self.command(Command::GetPhysicalDeviceFeatures2, true)?;
        self.u64(physical)?;
        self.present()?;
        self.i32(Structure::PhysicalDeviceFeatures2 as i32)?;
        self.chain(chain, |_, _| Ok(()))
    }

    /// The physical device's extensions: how many there are where `capacity`
    /// is zero, else as many as `capacity`.
    pub(crate) fn enumerate_device_extension_properties(
        &mut self,
        physical: u64,
        capacity: u32,
    ) -> Result<(), DriverError> {
        self.command(Command::EnumerateDeviceExtensionProperties, true)?;
        self.u64(physical)?;
        // No layer's name.
        self.null()?;
        self.present()?;
        self.u32(capacity)?;
        if capacity == 0 {
            return self.null();
        }
        // An extension's skeleton holds nothing past its array's place.
        self.count(capacity as usize)
    }

    /// The physical device's queue families: how many there are where
    /// `capacity` is zero, else as many as `capacity`.
    pub(crate) fn get_physical_device_queue_family_properties2(
        &mut self,
        physical: u64,
        capacity: u32,
    ) -> Result<(), DriverError> {
        self.command(Command::GetPhysicalDeviceQueueFamilyProperties2, true)?;
        self.u64(physical)?;
        self.present()?;
        self.u32(capacity)?;
        if capacity == 0 {
            return self.null();
        }
        self.count(capacity as usize)?;
        (0..capacity).try_for_each(|_| self.structure(Structure::QueueFamilyProperties2))
    }

    pub(crate) fn get_physical_device_memory_properties2(
        &mut self,
        physical: u64,
    ) -> Result<(), DriverError> {
        self.command(Command::GetPhysicalDeviceMemoryProperties2, true)?;
        self.u64(physical)?;
        self.present()?;
        self.structure(Structure::PhysicalDeviceMemoryProperties2)?;
        self.count(MAX_MEMORY_TYPES)?;
        self.count(MAX_MEMORY_HEAPS)
    }

    /// The device `device` of the physical device, with `queues` queues of
    /// family `family`, no layer, and what `enabled` enables.
    pub(crate) fn create_device(
        &mut self,
        physical: u64,
        device: u64,
        (family, queues): (u32, u32),
        enabled: &Enabled<'_>,
    ) -> Result<(), DriverError> {
        self.command(Command::CreateDevice, true)?;
        self.u64(physical)?;
        self.present()?;
        self.i32(Structure::DeviceCreateInfo as i32)?;
        let mut chain = [Structure::PhysicalDeviceFeatures2; 8];
        let chained = chain
            .get_mut(1..=enabled.chain.len())
            .ok_or(DriverError::LengthOutOfRange)?;
        for (slot, link) in chained.iter_mut().zip(enabled.chain) {
            *slot = link.structure;
        }
        let structures = chain
            .get(..=enabled.chain.len())
            .ok_or(DriverError::LengthOutOfRange)?;
        self.chain(structures, |encoder, at| match at.checked_sub(1) {
            None => enabled
                .core
                .iter()
                .try_for_each(|&member| encoder.boolean(member)),
            Some(link) => enabled
                .chain
                .get(link)
                .ok_or(DriverError::LengthOutOfRange)?
                .members
                .iter()
                .try_for_each(|&member| encoder.boolean(member)),
        })?;
        self.u32(0)?;
        self.u32(1)?;
        self.count(1)?;
        self.structure(Structure::DeviceQueueCreateInfo)?;
        self.u32(0)?;
        self.u32(family)?;
        self.u32(queues)?;
        self.count(queues as usize)?;
        (0..queues).try_for_each(|_| self.f32(1.0))?;
        // No layers.
        self.u32(0)?;
        self.null()?;
        self.count32(enabled.extensions.len())?;
        if enabled.extensions.is_empty() {
            self.null()?;
        } else {
            self.count(enabled.extensions.len())?;
            enabled
                .extensions
                .iter()
                .try_for_each(|name| self.string(name))?;
        }
        // The core features are chained, not pointed at; no allocator.
        self.null()?;
        self.null()?;
        self.present()?;
        self.u64(device)
    }

    /// Queue `index` of family `family`, named `queue`, its work fenced on
    /// the context's ring `ring`.
    pub(crate) fn get_device_queue2(
        &mut self,
        device: u64,
        (family, index): (u32, u32),
        ring: u32,
        queue: u64,
    ) -> Result<(), DriverError> {
        self.command(Command::GetDeviceQueue2, false)?;
        self.u64(device)?;
        self.present()?;
        self.i32(Structure::DeviceQueueInfo2 as i32)?;
        self.present()?;
        self.structure(Structure::DeviceQueueTimelineInfoMesa)?;
        self.u32(ring)?;
        self.u32(0)?;
        self.u32(family)?;
        self.u32(index)?;
        self.present()?;
        self.u64(queue)
    }

    pub(crate) fn allocate_memory(
        &mut self,
        device: u64,
        memory: u64,
        (size, memory_type): (u64, u32),
    ) -> Result<(), DriverError> {
        self.command(Command::AllocateMemory, true)?;
        self.u64(device)?;
        self.present()?;
        self.structure(Structure::MemoryAllocateInfo)?;
        self.u64(size)?;
        self.u32(memory_type)?;
        self.null()?;
        self.present()?;
        self.u64(memory)
    }

    pub(crate) fn free_memory(&mut self, device: u64, memory: u64) -> Result<(), DriverError> {
        self.command(Command::FreeMemory, false)?;
        self.u64(device)?;
        self.u64(memory)?;
        self.null()
    }

    /// The buffer `buffer` of `size` bytes, made for `usage`, used by one
    /// queue family at a time.
    pub(crate) fn create_buffer(
        &mut self,
        device: u64,
        buffer: u64,
        (size, usage): (u64, u32),
    ) -> Result<(), DriverError> {
        self.command(Command::CreateBuffer, true)?;
        self.u64(device)?;
        self.present()?;
        self.structure(Structure::BufferCreateInfo)?;
        self.u32(0)?;
        self.u64(size)?;
        self.u32(usage)?;
        self.i32(0)?;
        self.u32(0)?;
        self.null()?;
        self.null()?;
        self.present()?;
        self.u64(buffer)
    }

    pub(crate) fn destroy_buffer(&mut self, device: u64, buffer: u64) -> Result<(), DriverError> {
        self.command(Command::DestroyBuffer, false)?;
        self.u64(device)?;
        self.u64(buffer)?;
        self.null()
    }

    pub(crate) fn get_buffer_memory_requirements2(
        &mut self,
        device: u64,
        buffer: u64,
    ) -> Result<(), DriverError> {
        self.command(Command::GetBufferMemoryRequirements2, true)?;
        self.u64(device)?;
        self.present()?;
        self.structure(Structure::BufferMemoryRequirementsInfo2)?;
        self.u64(buffer)?;
        self.present()?;
        self.structure(Structure::MemoryRequirements2)
    }

    pub(crate) fn bind_buffer_memory2(
        &mut self,
        device: u64,
        buffer: u64,
        (memory, offset): (u64, u64),
    ) -> Result<(), DriverError> {
        self.command(Command::BindBufferMemory2, true)?;
        self.u64(device)?;
        self.u32(1)?;
        self.count(1)?;
        self.structure(Structure::BindBufferMemoryInfo)?;
        self.u64(buffer)?;
        self.u64(memory)?;
        self.u64(offset)
    }

    pub(crate) fn create_command_pool(
        &mut self,
        device: u64,
        pool: u64,
        (family, flags): (u32, u32),
    ) -> Result<(), DriverError> {
        self.command(Command::CreateCommandPool, true)?;
        self.u64(device)?;
        self.present()?;
        self.structure(Structure::CommandPoolCreateInfo)?;
        self.u32(flags)?;
        self.u32(family)?;
        self.null()?;
        self.present()?;
        self.u64(pool)
    }

    /// Primary command buffers of `pool`, named `ids`.
    pub(crate) fn allocate_command_buffers(
        &mut self,
        device: u64,
        pool: u64,
        ids: &[u64],
    ) -> Result<(), DriverError> {
        let count = u32::try_from(ids.len()).map_err(|_| DriverError::LengthOutOfRange)?;
        self.command(Command::AllocateCommandBuffers, true)?;
        self.u64(device)?;
        self.present()?;
        self.structure(Structure::CommandBufferAllocateInfo)?;
        self.u64(pool)?;
        self.i32(0)?;
        self.u32(count)?;
        self.count(ids.len())?;
        ids.iter().try_for_each(|&id| self.u64(id))
    }

    pub(crate) fn begin_command_buffer(
        &mut self,
        commands: u64,
        flags: u32,
    ) -> Result<(), DriverError> {
        self.command(Command::BeginCommandBuffer, false)?;
        self.u64(commands)?;
        self.present()?;
        self.structure(Structure::CommandBufferBeginInfo)?;
        self.u32(flags)?;
        self.null()
    }

    pub(crate) fn end_command_buffer(&mut self, commands: u64) -> Result<(), DriverError> {
        self.command(Command::EndCommandBuffer, false)?;
        self.u64(commands)
    }

    pub(crate) fn cmd_copy_buffer(
        &mut self,
        commands: u64,
        (src, dst): (u64, u64),
        regions: impl ExactSizeIterator<Item = CopyRegion>,
    ) -> Result<(), DriverError> {
        let len = regions.len();
        let count = u32::try_from(len).map_err(|_| DriverError::LengthOutOfRange)?;
        self.command(Command::CmdCopyBuffer, false)?;
        self.u64(commands)?;
        self.u64(src)?;
        self.u64(dst)?;
        self.u32(count)?;
        self.count(len)?;
        for region in regions {
            self.u64(region.src)?;
            self.u64(region.dst)?;
            self.u64(region.size)?;
        }
        Ok(())
    }

    pub(crate) fn cmd_fill_buffer(
        &mut self,
        commands: u64,
        dst: u64,
        (offset, size): (u64, u64),
        word: u32,
    ) -> Result<(), DriverError> {
        self.command(Command::CmdFillBuffer, false)?;
        self.u64(commands)?;
        self.u64(dst)?;
        self.u64(offset)?;
        self.u64(size)?;
        self.u32(word)
    }

    /// Write `data`, whole words, into `dst` at `offset`.
    pub(crate) fn cmd_update_buffer(
        &mut self,
        commands: u64,
        dst: u64,
        offset: u64,
        data: &[u8],
    ) -> Result<(), DriverError> {
        if !data.len().is_multiple_of(4) {
            return Err(DriverError::LengthOutOfRange);
        }
        self.command(Command::CmdUpdateBuffer, false)?;
        self.u64(commands)?;
        self.u64(dst)?;
        self.u64(offset)?;
        self.u64(data.len() as u64)?;
        self.count(data.len())?;
        self.put(data)
    }

    /// Everything recorded before made visible to everything after: every
    /// write available to every read and write.
    pub(crate) fn cmd_barrier(&mut self, commands: u64) -> Result<(), DriverError> {
        self.memory_barrier(
            commands,
            STAGE_ALL_COMMANDS,
            ACCESS_MEMORY_READ | ACCESS_MEMORY_WRITE,
        )
    }

    /// Everything recorded before made visible to the host's reads once the
    /// submission is done.
    pub(crate) fn cmd_host_barrier(&mut self, commands: u64) -> Result<(), DriverError> {
        self.memory_barrier(commands, STAGE_HOST, ACCESS_HOST_READ)
    }

    /// Every write before made available to `access` at `stage` after.
    fn memory_barrier(
        &mut self,
        commands: u64,
        stage: u32,
        access: u32,
    ) -> Result<(), DriverError> {
        self.command(Command::CmdPipelineBarrier, false)?;
        self.u64(commands)?;
        self.u32(STAGE_ALL_COMMANDS)?;
        self.u32(stage)?;
        self.u32(0)?;
        self.u32(1)?;
        self.count(1)?;
        self.structure(Structure::MemoryBarrier)?;
        self.u32(ACCESS_MEMORY_WRITE)?;
        self.u32(access)?;
        for _ in 0..2 {
            self.u32(0)?;
            self.null()?;
        }
        Ok(())
    }

    /// Submit `commands` to `queue`, with no semaphores and no fence.
    pub(crate) fn queue_submit(&mut self, queue: u64, commands: &[u64]) -> Result<(), DriverError> {
        let count = u32::try_from(commands.len()).map_err(|_| DriverError::LengthOutOfRange)?;
        self.command(Command::QueueSubmit, false)?;
        self.u64(queue)?;
        self.u32(1)?;
        self.count(1)?;
        self.structure(Structure::SubmitInfo)?;
        // No waits: their semaphores and stage masks.
        self.u32(0)?;
        self.null()?;
        self.null()?;
        self.u32(count)?;
        self.count(commands.len())?;
        commands.iter().try_for_each(|&id| self.u64(id))?;
        self.u32(0)?;
        self.null()?;
        self.null()
    }

    /// Where the renderer writes the replies of the commands after it.
    pub(crate) fn set_reply_command_stream(&mut self, span: StreamSpan) -> Result<(), DriverError> {
        self.command(Command::SetReplyCommandStreamMesa, false)?;
        self.present()?;
        self.stream_span(span)
    }

    /// Move the next reply to `position` of the reply stream.
    pub(crate) fn seek_reply_command_stream(&mut self, position: u64) -> Result<(), DriverError> {
        self.command(Command::SeekReplyCommandStreamMesa, false)?;
        self.u64(position)
    }

    /// Decode and run the commands in `streams`, in order, where they lie.
    pub(crate) fn execute_command_streams(
        &mut self,
        streams: &[StreamSpan],
    ) -> Result<(), DriverError> {
        let count = u32::try_from(streams.len()).map_err(|_| DriverError::LengthOutOfRange)?;
        if count == 0 {
            return Err(DriverError::LengthOutOfRange);
        }
        self.command(Command::ExecuteCommandStreamsMesa, false)?;
        self.u32(count)?;
        self.count(streams.len())?;
        streams
            .iter()
            .try_for_each(|&span| self.stream_span(span))?;
        // No reply positions, no dependencies, no flags.
        self.null()?;
        self.u32(0)?;
        self.null()?;
        self.u32(0)
    }

    fn stream_span(&mut self, span: StreamSpan) -> Result<(), DriverError> {
        self.u32(span.resource)?;
        self.u64(span.offset)?;
        self.u64(span.size)
    }

    /// The shader module `module` of a module's `words`.
    pub(crate) fn create_shader_module(
        &mut self,
        device: u64,
        module: u64,
        words: &[u8],
    ) -> Result<(), DriverError> {
        if !words.len().is_multiple_of(4) {
            return Err(DriverError::LengthOutOfRange);
        }
        self.command(Command::CreateShaderModule, true)?;
        self.u64(device)?;
        self.present()?;
        self.structure(Structure::ShaderModuleCreateInfo)?;
        self.u32(0)?;
        self.u64(words.len() as u64)?;
        self.count(words.len() / 4)?;
        self.put(words)?;
        self.null()?;
        self.present()?;
        self.u64(module)
    }

    /// The descriptor set layout `layout` of `entries`, none with an
    /// immutable sampler.
    pub(crate) fn create_descriptor_set_layout(
        &mut self,
        device: u64,
        layout: u64,
        entries: &[LayoutEntry],
    ) -> Result<(), DriverError> {
        self.command(Command::CreateDescriptorSetLayout, true)?;
        self.u64(device)?;
        self.present()?;
        self.structure(Structure::DescriptorSetLayoutCreateInfo)?;
        self.u32(0)?;
        self.count32(entries.len())?;
        if entries.is_empty() {
            self.null()?;
        } else {
            self.count(entries.len())?;
            for entry in entries {
                self.u32(entry.binding)?;
                self.u32(entry.kind)?;
                self.u32(entry.count)?;
                self.u32(entry.stages)?;
                self.null()?;
            }
        }
        self.null()?;
        self.present()?;
        self.u64(layout)
    }

    /// The pipeline layout `layout` of `sets`, with `push` bytes of push
    /// constants the stages `stages` see where there are any.
    pub(crate) fn create_pipeline_layout(
        &mut self,
        device: u64,
        layout: u64,
        sets: &[u64],
        (stages, push): (u32, u32),
    ) -> Result<(), DriverError> {
        self.command(Command::CreatePipelineLayout, true)?;
        self.u64(device)?;
        self.present()?;
        self.structure(Structure::PipelineLayoutCreateInfo)?;
        self.u32(0)?;
        self.count32(sets.len())?;
        if sets.is_empty() {
            self.null()?;
        } else {
            self.count(sets.len())?;
            sets.iter().try_for_each(|&set| self.u64(set))?;
        }
        if push == 0 {
            self.u32(0)?;
            self.null()?;
        } else {
            self.u32(1)?;
            self.count(1)?;
            self.u32(stages)?;
            self.u32(0)?;
            self.u32(push)?;
        }
        self.null()?;
        self.present()?;
        self.u64(layout)
    }

    /// The descriptor pool `pool` holding one set of `sizes`, each a
    /// descriptor type and how many of it.
    pub(crate) fn create_descriptor_pool(
        &mut self,
        device: u64,
        pool: u64,
        sizes: &[(u32, u32)],
    ) -> Result<(), DriverError> {
        self.command(Command::CreateDescriptorPool, true)?;
        self.u64(device)?;
        self.present()?;
        self.structure(Structure::DescriptorPoolCreateInfo)?;
        self.u32(0)?;
        self.u32(1)?;
        self.count32(sizes.len())?;
        if sizes.is_empty() {
            self.null()?;
        } else {
            self.count(sizes.len())?;
            for &(kind, count) in sizes {
                self.u32(kind)?;
                self.u32(count)?;
            }
        }
        self.null()?;
        self.present()?;
        self.u64(pool)
    }

    /// The descriptor set `set` of `layout`, from `pool`.
    pub(crate) fn allocate_descriptor_sets(
        &mut self,
        device: u64,
        (pool, layout): (u64, u64),
        set: u64,
    ) -> Result<(), DriverError> {
        self.command(Command::AllocateDescriptorSets, true)?;
        self.u64(device)?;
        self.present()?;
        self.structure(Structure::DescriptorSetAllocateInfo)?;
        self.u64(pool)?;
        self.u32(1)?;
        self.count(1)?;
        self.u64(layout)?;
        self.count(1)?;
        self.u64(set)
    }

    /// Write each of `writes`, one descriptor a binding, into `set`: every
    /// write's image, buffer and texel buffer arrays in that order, all but
    /// the one it binds empty.
    pub(crate) fn update_descriptor_sets(
        &mut self,
        device: u64,
        set: u64,
        writes: &[DescriptorWrite],
    ) -> Result<(), DriverError> {
        self.command(Command::UpdateDescriptorSets, false)?;
        self.u64(device)?;
        self.count32(writes.len())?;
        if writes.is_empty() {
            self.null()?;
        } else {
            self.count(writes.len())?;
            for write in writes {
                self.structure(Structure::WriteDescriptorSet)?;
                self.u64(set)?;
                self.u32(write.binding)?;
                self.u32(0)?;
                self.u32(1)?;
                self.u32(write.kind)?;
                match write.written {
                    Written::Image { sampler, view } => {
                        self.count(1)?;
                        self.u64(sampler)?;
                        self.u64(view)?;
                        self.i32(image::GENERAL)?;
                        self.null()?;
                        self.null()?;
                    }
                    Written::Buffer {
                        buffer,
                        offset,
                        range,
                    } => {
                        self.null()?;
                        self.count(1)?;
                        self.u64(buffer)?;
                        self.u64(offset)?;
                        self.u64(range)?;
                        self.null()?;
                    }
                    Written::Texels(view) => {
                        self.null()?;
                        self.null()?;
                        self.count(1)?;
                        self.u64(view)?;
                    }
                }
            }
        }
        // No copies.
        self.u32(0)?;
        self.null()
    }

    /// The compute pipeline `pipeline` of the entry point `entry` of
    /// `module`, laid out as `layout`, with no specialization and no cache.
    pub(crate) fn create_compute_pipelines(
        &mut self,
        device: u64,
        pipeline: u64,
        (module, entry): (u64, &str),
        layout: u64,
    ) -> Result<(), DriverError> {
        self.command(Command::CreateComputePipelines, true)?;
        self.u64(device)?;
        self.null()?;
        self.u32(1)?;
        self.count(1)?;
        self.structure(Structure::ComputePipelineCreateInfo)?;
        self.u32(0)?;
        self.structure(Structure::PipelineShaderStageCreateInfo)?;
        self.u32(0)?;
        self.u32(super::stage::COMPUTE)?;
        self.u64(module)?;
        self.string(entry)?;
        self.null()?;
        self.u64(layout)?;
        self.null()?;
        self.i32(-1)?;
        self.null()?;
        self.count(1)?;
        self.u64(pipeline)
    }

    /// The image `image` made as `info`.
    pub(crate) fn create_image(
        &mut self,
        device: u64,
        image: u64,
        info: &ImageInfo,
    ) -> Result<(), DriverError> {
        self.command(Command::CreateImage, true)?;
        self.u64(device)?;
        self.present()?;
        self.image_create_info(info)?;
        self.null()?;
        self.present()?;
        self.u64(image)
    }

    /// A structure's chain: the list of `formats` an image is read as, or
    /// none.
    fn format_list(&mut self, formats: Option<[i32; 2]>) -> Result<(), DriverError> {
        match formats {
            Some(formats) => self.chain(&[Structure::ImageFormatListCreateInfo], |this, _| {
                this.count32(formats.len())?;
                this.count(formats.len())?;
                formats.iter().try_for_each(|&format| this.i32(format))
            }),
            None => self.null(),
        }
    }

    /// The memory an image made as `info` needs, asked before it is made.
    pub(crate) fn get_device_image_memory_requirements(
        &mut self,
        device: u64,
        info: &ImageInfo,
    ) -> Result<(), DriverError> {
        self.command(Command::GetDeviceImageMemoryRequirements, true)?;
        self.u64(device)?;
        self.present()?;
        self.structure(Structure::DeviceImageMemoryRequirements)?;
        self.present()?;
        self.image_create_info(info)?;
        // No plane of its own: no image is made disjoint.
        self.i32(0)?;
        self.present()?;
        self.structure(Structure::MemoryRequirements2)
    }

    fn image_create_info(&mut self, info: &ImageInfo) -> Result<(), DriverError> {
        self.i32(Structure::ImageCreateInfo as i32)?;
        self.format_list(info.view_formats)?;
        self.u32(info.flags)?;
        self.i32(info.kind)?;
        self.i32(info.format)?;
        info.extent.iter().try_for_each(|&side| self.u32(side))?;
        self.u32(info.levels)?;
        self.u32(info.layers)?;
        self.i32(info.samples)?;
        self.i32(image::OPTIMAL)?;
        self.u32(info.usage)?;
        // Exclusive to a queue family at a time, so no family is named.
        self.i32(0)?;
        self.u32(0)?;
        self.null()?;
        self.i32(image::UNDEFINED)
    }

    pub(crate) fn get_image_memory_requirements2(
        &mut self,
        device: u64,
        image: u64,
    ) -> Result<(), DriverError> {
        self.command(Command::GetImageMemoryRequirements2, true)?;
        self.u64(device)?;
        self.present()?;
        self.structure(Structure::ImageMemoryRequirementsInfo2)?;
        self.u64(image)?;
        self.present()?;
        self.structure(Structure::MemoryRequirements2)
    }

    pub(crate) fn bind_image_memory2(
        &mut self,
        device: u64,
        image: u64,
        (memory, offset): (u64, u64),
    ) -> Result<(), DriverError> {
        self.command(Command::BindImageMemory2, true)?;
        self.u64(device)?;
        self.u32(1)?;
        self.count(1)?;
        self.structure(Structure::BindImageMemoryInfo)?;
        self.u64(image)?;
        self.u64(memory)?;
        self.u64(offset)
    }

    /// The view `view` made as `info`, its channels its image's own.
    pub(crate) fn create_image_view(
        &mut self,
        device: u64,
        view: u64,
        info: &ViewInfo,
    ) -> Result<(), DriverError> {
        self.command(Command::CreateImageView, true)?;
        self.u64(device)?;
        self.present()?;
        self.i32(Structure::ImageViewCreateInfo as i32)?;
        self.chain(&[Structure::ImageViewUsageCreateInfo], |this, _| {
            this.u32(info.usage)
        })?;
        self.u32(0)?;
        self.u64(info.image)?;
        self.i32(info.kind)?;
        self.i32(info.format)?;
        // Each channel its own.
        for _ in 0..4 {
            self.i32(0)?;
        }
        self.subresources(&info.range)?;
        self.null()?;
        self.present()?;
        self.u64(view)
    }

    pub(crate) fn create_sampler(
        &mut self,
        device: u64,
        sampler: u64,
        info: &SamplerInfo,
    ) -> Result<(), DriverError> {
        self.command(Command::CreateSampler, true)?;
        self.u64(device)?;
        self.present()?;
        self.structure(Structure::SamplerCreateInfo)?;
        self.u32(0)?;
        info.filters
            .iter()
            .try_for_each(|&filter| self.i32(filter))?;
        info.address.iter().try_for_each(|&mode| self.i32(mode))?;
        self.f32(info.lod_bias)?;
        self.boolean(info.anisotropy.is_some())?;
        self.f32(info.anisotropy.unwrap_or(1.0))?;
        self.boolean(info.compare.is_some())?;
        self.i32(info.compare.unwrap_or(0))?;
        self.f32(info.lod[0])?;
        self.f32(info.lod[1])?;
        self.i32(info.border)?;
        self.boolean(info.unnormalized)?;
        self.null()?;
        self.present()?;
        self.u64(sampler)
    }

    /// The texel buffer `view` of `range` bytes at `offset` into `buffer`,
    /// read as texels of `format`.
    pub(crate) fn create_buffer_view(
        &mut self,
        device: u64,
        view: u64,
        (buffer, format): (u64, i32),
        (offset, range): (u64, u64),
    ) -> Result<(), DriverError> {
        self.command(Command::CreateBufferView, true)?;
        self.u64(device)?;
        self.present()?;
        self.structure(Structure::BufferViewCreateInfo)?;
        self.u32(0)?;
        self.u64(buffer)?;
        self.i32(format)?;
        self.u64(offset)?;
        self.u64(range)?;
        self.null()?;
        self.present()?;
        self.u64(view)
    }

    /// Copy `regions` from `buffer` into `image`, kept in the general layout.
    pub(crate) fn cmd_copy_buffer_to_image(
        &mut self,
        commands: u64,
        (buffer, image_id): (u64, u64),
        regions: &[BufferImageRegion],
    ) -> Result<(), DriverError> {
        self.command(Command::CmdCopyBufferToImage, false)?;
        self.u64(commands)?;
        self.u64(buffer)?;
        self.u64(image_id)?;
        self.i32(image::GENERAL)?;
        self.buffer_image_regions(regions)
    }

    /// Copy `regions` from `image`, kept in the general layout, into
    /// `buffer`.
    pub(crate) fn cmd_copy_image_to_buffer(
        &mut self,
        commands: u64,
        (image_id, buffer): (u64, u64),
        regions: &[BufferImageRegion],
    ) -> Result<(), DriverError> {
        self.command(Command::CmdCopyImageToBuffer, false)?;
        self.u64(commands)?;
        self.u64(image_id)?;
        self.i32(image::GENERAL)?;
        self.u64(buffer)?;
        self.buffer_image_regions(regions)
    }

    fn buffer_image_regions(&mut self, regions: &[BufferImageRegion]) -> Result<(), DriverError> {
        self.count32(regions.len())?;
        self.count(regions.len())?;
        regions.iter().try_for_each(|region| {
            self.u64(region.offset)?;
            self.u32(region.row_length)?;
            self.u32(region.image_height)?;
            self.layers(&region.layers)?;
            region.at.iter().try_for_each(|&at| self.i32(at))?;
            region.extent.iter().try_for_each(|&side| self.u32(side))
        })
    }

    /// Copy `regions` between images, each kept in the general layout.
    pub(crate) fn cmd_copy_image(
        &mut self,
        commands: u64,
        (src, dst): (u64, u64),
        regions: &[ImageRegion],
    ) -> Result<(), DriverError> {
        self.command(Command::CmdCopyImage, false)?;
        self.u64(commands)?;
        self.u64(src)?;
        self.i32(image::GENERAL)?;
        self.u64(dst)?;
        self.i32(image::GENERAL)?;
        self.count32(regions.len())?;
        self.count(regions.len())?;
        regions.iter().try_for_each(|region| {
            for (layers, at) in [region.src, region.dst] {
                self.layers(&layers)?;
                at.iter().try_for_each(|&at| self.i32(at))?;
            }
            region.extent.iter().try_for_each(|&side| self.u32(side))
        })
    }

    /// Set `ranges` of `image` to the colour `bits`, read as floats, signed
    /// or unsigned integers as `kind` names: zero, one or two.
    pub(crate) fn cmd_clear_color_image(
        &mut self,
        commands: u64,
        image_id: u64,
        (kind, bits): (u32, [u32; 4]),
        ranges: &[Subresources],
    ) -> Result<(), DriverError> {
        self.command(Command::CmdClearColorImage, false)?;
        self.u64(commands)?;
        self.u64(image_id)?;
        self.i32(image::GENERAL)?;
        self.present()?;
        self.u32(kind)?;
        self.count(bits.len())?;
        bits.iter().try_for_each(|&word| self.u32(word))?;
        self.ranges(ranges)
    }

    /// Set the depth of `ranges` of `image` that name it to `depth`, and the
    /// stencil of those that name it to `stencil`.
    pub(crate) fn cmd_clear_depth_stencil_image(
        &mut self,
        commands: u64,
        image_id: u64,
        (depth, stencil): (f32, u32),
        ranges: &[Subresources],
    ) -> Result<(), DriverError> {
        self.command(Command::CmdClearDepthStencilImage, false)?;
        self.u64(commands)?;
        self.u64(image_id)?;
        self.i32(image::GENERAL)?;
        self.present()?;
        self.f32(depth)?;
        self.u32(stencil)?;
        self.ranges(ranges)
    }

    /// `range` of `image` taken from undefined to the general layout it is
    /// kept in, before anything after reads or writes it.
    pub(crate) fn cmd_image_kept(
        &mut self,
        commands: u64,
        image_id: u64,
        range: &Subresources,
    ) -> Result<(), DriverError> {
        self.command(Command::CmdPipelineBarrier, false)?;
        self.u64(commands)?;
        self.u32(STAGE_TOP_OF_PIPE)?;
        self.u32(STAGE_ALL_COMMANDS)?;
        self.u32(0)?;
        // No memory or buffer barrier.
        for _ in 0..2 {
            self.u32(0)?;
            self.null()?;
        }
        self.u32(1)?;
        self.count(1)?;
        self.structure(Structure::ImageMemoryBarrier)?;
        self.u32(0)?;
        self.u32(ACCESS_MEMORY_READ | ACCESS_MEMORY_WRITE)?;
        self.i32(image::UNDEFINED)?;
        self.i32(image::GENERAL)?;
        self.u32(QUEUE_FAMILY_IGNORED)?;
        self.u32(QUEUE_FAMILY_IGNORED)?;
        self.u64(image_id)?;
        self.subresources(range)
    }

    fn ranges(&mut self, ranges: &[Subresources]) -> Result<(), DriverError> {
        self.count32(ranges.len())?;
        self.count(ranges.len())?;
        ranges.iter().try_for_each(|range| self.subresources(range))
    }

    fn subresources(&mut self, range: &Subresources) -> Result<(), DriverError> {
        self.u32(range.aspects)?;
        self.u32(range.base_level)?;
        self.u32(range.levels)?;
        self.u32(range.base_layer)?;
        self.u32(range.layers)
    }

    fn layers(&mut self, layers: &Layers) -> Result<(), DriverError> {
        self.u32(layers.aspect)?;
        self.u32(layers.level)?;
        self.u32(layers.base_layer)?;
        self.u32(layers.count)
    }

    /// What `physical` offers `format`, asked with Vulkan 1.3's format
    /// features chained where it `states` them.
    pub(crate) fn get_physical_device_format_properties2(
        &mut self,
        physical: u64,
        format: i32,
        states: bool,
    ) -> Result<(), DriverError> {
        self.command(Command::GetPhysicalDeviceFormatProperties2, true)?;
        self.u64(physical)?;
        self.i32(format)?;
        self.present()?;
        if !states {
            return self.structure(Structure::FormatProperties2);
        }
        self.i32(Structure::FormatProperties2 as i32)?;
        self.chain(&[Structure::FormatProperties3], |_, _| Ok(()))
    }

    /// The most `physical` makes an image of `query`, laid out as the device
    /// chooses.
    pub(crate) fn get_physical_device_image_format_properties2(
        &mut self,
        physical: u64,
        query: &FormatQuery,
    ) -> Result<(), DriverError> {
        self.command(Command::GetPhysicalDeviceImageFormatProperties2, true)?;
        self.u64(physical)?;
        self.present()?;
        self.i32(Structure::PhysicalDeviceImageFormatInfo2 as i32)?;
        self.format_list(query.view_formats)?;
        self.i32(query.format)?;
        self.i32(query.kind)?;
        self.i32(image::OPTIMAL)?;
        self.u32(query.usage)?;
        self.u32(query.flags)?;
        self.present()?;
        self.structure(Structure::ImageFormatProperties2)
    }

    /// Let go of the object `id` the destroying command `command` names.
    pub(crate) fn destroy(
        &mut self,
        command: Command,
        device: u64,
        id: u64,
    ) -> Result<(), DriverError> {
        self.command(command, false)?;
        self.u64(device)?;
        self.u64(id)?;
        self.null()
    }

    /// Bind `pipeline` at `point`.
    pub(crate) fn cmd_bind_pipeline(
        &mut self,
        commands: u64,
        point: u32,
        pipeline: u64,
    ) -> Result<(), DriverError> {
        self.command(Command::CmdBindPipeline, false)?;
        self.u64(commands)?;
        self.u32(point)?;
        self.u64(pipeline)
    }

    /// Bind `sets` from the first set on at `point`, laid out as `layout`.
    pub(crate) fn cmd_bind_sets(
        &mut self,
        commands: u64,
        (point, layout): (u32, u64),
        sets: &[u64],
    ) -> Result<(), DriverError> {
        self.command(Command::CmdBindDescriptorSets, false)?;
        self.u64(commands)?;
        self.u32(point)?;
        self.u64(layout)?;
        self.u32(0)?;
        self.count32(sets.len())?;
        if sets.is_empty() {
            self.null()?;
        } else {
            self.count(sets.len())?;
            sets.iter().try_for_each(|&set| self.u64(set))?;
        }
        // No dynamic offsets.
        self.u32(0)?;
        self.null()
    }

    /// Push `words` from offset nought for the stages `stages`.
    pub(crate) fn cmd_push_constants(
        &mut self,
        commands: u64,
        (layout, stages): (u64, u32),
        words: &[u32],
    ) -> Result<(), DriverError> {
        let bytes = words.len() * 4;
        self.command(Command::CmdPushConstants, false)?;
        self.u64(commands)?;
        self.u64(layout)?;
        self.u32(stages)?;
        self.u32(0)?;
        self.count32(bytes)?;
        self.count(bytes)?;
        words.iter().try_for_each(|&word| self.u32(word))
    }

    pub(crate) fn cmd_dispatch(
        &mut self,
        commands: u64,
        groups: [u32; 3],
    ) -> Result<(), DriverError> {
        self.command(Command::CmdDispatch, false)?;
        self.u64(commands)?;
        groups.iter().try_for_each(|&count| self.u32(count))
    }

    /// Dispatch the counts three words at `offset` into `buffer` hold.
    pub(crate) fn cmd_dispatch_indirect(
        &mut self,
        commands: u64,
        buffer: u64,
        offset: u64,
    ) -> Result<(), DriverError> {
        self.command(Command::CmdDispatchIndirect, false)?;
        self.u64(commands)?;
        self.u64(buffer)?;
        self.u64(offset)
    }

    /// A rectangle: its corner, then its extent.
    fn rect(&mut self, [x, y, width, height]: [u32; 4]) -> Result<(), DriverError> {
        let corner = |at: u32| i32::try_from(at).map_err(|_| DriverError::OutOfRange);
        self.i32(corner(x)?)?;
        self.i32(corner(y)?)?;
        self.u32(width)?;
        self.u32(height)
    }

    /// The render pipeline `pipeline` made as `info`, laid out as `layout`,
    /// rendering without a render pass object.
    pub(crate) fn create_graphics_pipelines(
        &mut self,
        device: u64,
        pipeline: u64,
        (info, layout): (&GraphicsInfo<'_>, u64),
    ) -> Result<(), DriverError> {
        self.command(Command::CreateGraphicsPipelines, true)?;
        self.u64(device)?;
        self.null()?;
        self.u32(1)?;
        self.count(1)?;
        self.i32(Structure::GraphicsPipelineCreateInfo as i32)?;
        self.chain(&[Structure::PipelineRenderingCreateInfo], |this, _| {
            this.u32(0)?;
            this.count32(info.colour_formats.len())?;
            this.count(info.colour_formats.len())?;
            info.colour_formats
                .iter()
                .try_for_each(|&format| this.i32(format))?;
            this.i32(info.depth_format)?;
            this.i32(info.stencil_format)
        })?;
        self.u32(0)?;
        self.count32(info.stages.len())?;
        self.count(info.stages.len())?;
        for stage in info.stages {
            self.structure(Structure::PipelineShaderStageCreateInfo)?;
            self.u32(0)?;
            self.u32(stage.stage)?;
            self.u64(stage.module)?;
            self.string(stage.entry)?;
            self.null()?;
        }
        self.present()?;
        self.vertex_input(info)?;
        self.present()?;
        self.structure(Structure::PipelineInputAssemblyStateCreateInfo)?;
        self.u32(0)?;
        self.u32(info.topology)?;
        self.boolean(info.restart)?;
        // No tessellation.
        self.null()?;
        // One viewport and one scissor, each draw's.
        self.present()?;
        self.structure(Structure::PipelineViewportStateCreateInfo)?;
        self.u32(0)?;
        self.u32(1)?;
        self.null()?;
        self.u32(1)?;
        self.null()?;
        self.present()?;
        self.raster(&info.raster)?;
        self.present()?;
        self.multisample(info)?;
        self.present()?;
        self.depth_stencil(info.depth, info.stencil)?;
        self.present()?;
        self.colour_blend(info)?;
        self.present()?;
        self.structure(Structure::PipelineDynamicStateCreateInfo)?;
        self.u32(0)?;
        self.count32(DYNAMIC_STATES.len())?;
        self.count(DYNAMIC_STATES.len())?;
        DYNAMIC_STATES
            .iter()
            .try_for_each(|&state| self.i32(state))?;
        self.u64(layout)?;
        // No render pass object, nor a pipeline it derives from.
        self.null()?;
        self.u32(0)?;
        self.null()?;
        self.i32(-1)?;
        self.null()?;
        self.count(1)?;
        self.u64(pipeline)
    }

    /// A pipeline's vertex input state: its bindings, then its attributes.
    fn vertex_input(&mut self, info: &GraphicsInfo<'_>) -> Result<(), DriverError> {
        self.structure(Structure::PipelineVertexInputStateCreateInfo)?;
        self.u32(0)?;
        self.count32(info.bindings.len())?;
        self.count(info.bindings.len())?;
        for (binding, &(stride, rate)) in (0u32..).zip(info.bindings) {
            self.u32(binding)?;
            self.u32(stride)?;
            self.u32(rate)?;
        }
        self.count32(info.attributes.len())?;
        self.count(info.attributes.len())?;
        for attribute in info.attributes {
            self.u32(attribute.location)?;
            self.u32(attribute.binding)?;
            self.i32(attribute.format)?;
            self.u32(attribute.offset)?;
        }
        Ok(())
    }

    /// A pipeline's multisample state: no sample shading.
    fn multisample(&mut self, info: &GraphicsInfo<'_>) -> Result<(), DriverError> {
        self.structure(Structure::PipelineMultisampleStateCreateInfo)?;
        self.u32(0)?;
        self.u32(info.samples)?;
        self.boolean(false)?;
        self.f32(0.0)?;
        self.count(1)?;
        self.u32(info.sample_mask)?;
        self.boolean(info.alpha_to_coverage)?;
        self.boolean(false)
    }

    /// A pipeline's colour blend state, its blend constants each draw's.
    fn colour_blend(&mut self, info: &GraphicsInfo<'_>) -> Result<(), DriverError> {
        self.structure(Structure::PipelineColorBlendStateCreateInfo)?;
        self.u32(0)?;
        self.boolean(info.logic_op.is_some())?;
        self.u32(info.logic_op.unwrap_or(0))?;
        self.count32(info.targets.len())?;
        self.count(info.targets.len())?;
        for target in info.targets {
            self.boolean(target.blend.is_some())?;
            target
                .blend
                .unwrap_or_default()
                .iter()
                .try_for_each(|&word| self.u32(word))?;
            self.u32(target.write_mask)?;
        }
        self.count(4)?;
        (0..4).try_for_each(|_| self.f32(0.0))
    }

    fn raster(&mut self, raster: &RasterInfo) -> Result<(), DriverError> {
        self.i32(Structure::PipelineRasterizationStateCreateInfo as i32)?;
        if raster.rectangular_lines {
            self.chain(
                &[Structure::PipelineRasterizationLineStateCreateInfo],
                |this, _| {
                    this.u32(LINES_RECTANGULAR)?;
                    this.boolean(false)?;
                    this.u32(0)?;
                    this.u32(0)
                },
            )?;
        } else {
            self.null()?;
        }
        self.u32(0)?;
        self.boolean(raster.depth_clamp)?;
        self.boolean(false)?;
        // Filled polygons.
        self.u32(0)?;
        self.u32(raster.cull)?;
        self.u32(raster.front_face)?;
        self.boolean(raster.bias.is_some())?;
        raster
            .bias
            .unwrap_or_default()
            .iter()
            .try_for_each(|&factor| self.f32(factor))?;
        self.f32(raster.line_width)
    }

    fn depth_stencil(
        &mut self,
        depth: Option<(i32, bool)>,
        stencil: Option<[StencilInfo; 2]>,
    ) -> Result<(), DriverError> {
        self.structure(Structure::PipelineDepthStencilStateCreateInfo)?;
        self.u32(0)?;
        self.boolean(depth.is_some())?;
        self.boolean(depth.is_some_and(|(_, write)| write))?;
        // Read only where depth is tested.
        self.i32(depth.map_or(0, |(compare, _)| compare))?;
        self.boolean(false)?;
        self.boolean(stencil.is_some())?;
        for face in stencil.unwrap_or_default() {
            face.ops.iter().try_for_each(|&op| self.u32(op))?;
            self.i32(face.compare)?;
            self.u32(face.compare_mask)?;
            self.u32(face.write_mask)?;
            // Each draw's.
            self.u32(0)?;
        }
        self.f32(0.0)?;
        self.f32(1.0)
    }

    /// Begin rendering into `colour`, `depth` and `stencil` within `area`,
    /// every attachment in the general layout and stored.
    pub(crate) fn cmd_begin_rendering(
        &mut self,
        commands: u64,
        area: [u32; 4],
        (colour, depth, stencil): (
            &[AttachmentInfo],
            Option<AttachmentInfo>,
            Option<AttachmentInfo>,
        ),
    ) -> Result<(), DriverError> {
        self.command(Command::CmdBeginRendering, false)?;
        self.u64(commands)?;
        self.present()?;
        self.structure(Structure::RenderingInfo)?;
        self.u32(0)?;
        self.rect(area)?;
        self.u32(1)?;
        self.u32(0)?;
        self.count32(colour.len())?;
        self.count(colour.len())?;
        colour
            .iter()
            .try_for_each(|attachment| self.attachment(attachment))?;
        for attachment in [depth, stencil] {
            match attachment {
                Some(attachment) => {
                    self.present()?;
                    self.attachment(&attachment)?;
                }
                None => self.null()?,
            }
        }
        Ok(())
    }

    fn attachment(&mut self, attachment: &AttachmentInfo) -> Result<(), DriverError> {
        self.structure(Structure::RenderingAttachmentInfo)?;
        self.u64(attachment.view)?;
        self.i32(image::GENERAL)?;
        if let Some(view) = attachment.resolve {
            self.u32(RESOLVE_AVERAGE)?;
            self.u64(view)?;
            self.i32(image::GENERAL)?;
        } else {
            self.u32(0)?;
            self.null()?;
            self.i32(image::UNDEFINED)?;
        }
        self.i32(i32::from(attachment.clear.is_some()))?;
        // Stored.
        self.i32(0)?;
        match attachment.clear.unwrap_or(HostClear::Colour(0, [0; 4])) {
            HostClear::Colour(kind, bits) => {
                self.u32(0)?;
                self.u32(kind)?;
                self.count(bits.len())?;
                bits.iter().try_for_each(|&word| self.u32(word))
            }
            HostClear::DepthStencil(depth, stencil) => {
                self.u32(1)?;
                self.f32(depth)?;
                self.u32(stencil)
            }
        }
    }

    pub(crate) fn cmd_end_rendering(&mut self, commands: u64) -> Result<(), DriverError> {
        self.command(Command::CmdEndRendering, false)?;
        self.u64(commands)
    }

    /// Bind each of `buffers` — a buffer, where its range starts and its
    /// bytes — from the first vertex buffer on, a read past a range reading
    /// zero.
    pub(crate) fn cmd_bind_vertex_buffers(
        &mut self,
        commands: u64,
        buffers: &[[u64; 3]],
    ) -> Result<(), DriverError> {
        self.command(Command::CmdBindVertexBuffers2, false)?;
        self.u64(commands)?;
        self.u32(0)?;
        self.count32(buffers.len())?;
        for member in 0..3 {
            self.count(buffers.len())?;
            buffers
                .iter()
                .try_for_each(|buffer| self.u64(buffer[member]))?;
        }
        // Each buffer's stride its pipeline's.
        self.null()
    }

    /// Bind `size` bytes of `buffer` from `offset` as indices of 32 bits
    /// where `wide`, else 16.
    pub(crate) fn cmd_bind_index_buffer(
        &mut self,
        commands: u64,
        [buffer, offset, size]: [u64; 3],
        wide: bool,
    ) -> Result<(), DriverError> {
        self.command(Command::CmdBindIndexBuffer2, false)?;
        self.u64(commands)?;
        self.u64(buffer)?;
        self.u64(offset)?;
        self.u64(size)?;
        self.i32(i32::from(wide))
    }

    /// The viewport: its corner, its extent and its near and far depths.
    pub(crate) fn cmd_set_viewport(
        &mut self,
        commands: u64,
        viewport: [f32; 6],
    ) -> Result<(), DriverError> {
        self.command(Command::CmdSetViewport, false)?;
        self.u64(commands)?;
        self.u32(0)?;
        self.u32(1)?;
        self.count(1)?;
        viewport.iter().try_for_each(|&value| self.f32(value))
    }

    pub(crate) fn cmd_set_scissor(
        &mut self,
        commands: u64,
        area: [u32; 4],
    ) -> Result<(), DriverError> {
        self.command(Command::CmdSetScissor, false)?;
        self.u64(commands)?;
        self.u32(0)?;
        self.u32(1)?;
        self.count(1)?;
        self.rect(area)
    }

    /// Both faces' stencil reference.
    pub(crate) fn cmd_set_stencil_reference(
        &mut self,
        commands: u64,
        reference: u32,
    ) -> Result<(), DriverError> {
        self.command(Command::CmdSetStencilReference, false)?;
        self.u64(commands)?;
        self.u32(STENCIL_BOTH_FACES)?;
        self.u32(reference)
    }

    pub(crate) fn cmd_set_blend_constants(
        &mut self,
        commands: u64,
        constants: [f32; 4],
    ) -> Result<(), DriverError> {
        self.command(Command::CmdSetBlendConstants, false)?;
        self.u64(commands)?;
        self.count(constants.len())?;
        constants.iter().try_for_each(|&value| self.f32(value))
    }

    pub(crate) fn cmd_draw(&mut self, commands: u64, draw: DrawInfo) -> Result<(), DriverError> {
        match draw {
            DrawInfo::Vertices(words) => {
                self.command(Command::CmdDraw, false)?;
                self.u64(commands)?;
                words.iter().try_for_each(|&word| self.u32(word))
            }
            DrawInfo::Indexed([count, instances, first], vertex_offset, first_instance) => {
                self.command(Command::CmdDrawIndexed, false)?;
                self.u64(commands)?;
                self.u32(count)?;
                self.u32(instances)?;
                self.u32(first)?;
                self.i32(vertex_offset)?;
                self.u32(first_instance)
            }
        }
    }

    /// Draw the `count` records `stride` bytes apart from `offset` into
    /// `buffer`, of an indexed draw's where `indexed`.
    pub(crate) fn cmd_draw_indirect(
        &mut self,
        commands: u64,
        indexed: bool,
        (buffer, offset): (u64, u64),
        (count, stride): (u32, u32),
    ) -> Result<(), DriverError> {
        let command = if indexed {
            Command::CmdDrawIndexedIndirect
        } else {
            Command::CmdDrawIndirect
        };
        self.command(command, false)?;
        self.u64(commands)?;
        self.u64(buffer)?;
        self.u64(offset)?;
        self.u32(count)?;
        self.u32(stride)
    }
}

/// The state each draw sets: its viewport, its scissor, its blend constants
/// and its stencil reference.
const DYNAMIC_STATES: [i32; 4] = [0, 1, 4, 8];

/// Lines drawn as rectangles extruded from them.
const LINES_RECTANGULAR: u32 = 1;

/// Samples averaged into one.
const RESOLVE_AVERAGE: u32 = 2;

/// Both faces, to a stencil command.
const STENCIL_BOTH_FACES: u32 = 3;
