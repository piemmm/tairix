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
    Command, Structure, ACCESS_HOST_READ, ACCESS_MEMORY_READ, ACCESS_MEMORY_WRITE, API_VERSION,
    MAX_MEMORY_HEAPS, MAX_MEMORY_TYPES, STAGE_ALL_COMMANDS, STAGE_HOST,
};

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

    /// The physical device's properties, with its subgroup's and its largest
    /// allocation's chained to them.
    pub(crate) fn get_physical_device_properties2(
        &mut self,
        physical: u64,
    ) -> Result<(), DriverError> {
        self.command(Command::GetPhysicalDeviceProperties2, true)?;
        self.u64(physical)?;
        self.present()?;
        self.i32(Structure::PhysicalDeviceProperties2 as i32)?;
        self.present()?;
        self.i32(Structure::PhysicalDeviceSubgroupProperties as i32)?;
        self.present()?;
        self.structure(Structure::PhysicalDeviceMaintenance3Properties)
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
    /// family `family` and no layer, extension or feature past the core.
    pub(crate) fn create_device(
        &mut self,
        physical: u64,
        device: u64,
        family: u32,
        queues: u32,
    ) -> Result<(), DriverError> {
        self.command(Command::CreateDevice, true)?;
        self.u64(physical)?;
        self.present()?;
        self.structure(Structure::DeviceCreateInfo)?;
        self.u32(0)?;
        self.u32(1)?;
        self.count(1)?;
        self.structure(Structure::DeviceQueueCreateInfo)?;
        self.u32(0)?;
        self.u32(family)?;
        self.u32(queues)?;
        self.count(queues as usize)?;
        (0..queues).try_for_each(|_| self.f32(1.0))?;
        self.no_names()?;
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
        regions: &[CopyRegion],
    ) -> Result<(), DriverError> {
        let count = u32::try_from(regions.len()).map_err(|_| DriverError::LengthOutOfRange)?;
        self.command(Command::CmdCopyBuffer, false)?;
        self.u64(commands)?;
        self.u64(src)?;
        self.u64(dst)?;
        self.u32(count)?;
        self.count(regions.len())?;
        regions.iter().try_for_each(|region| {
            self.u64(region.src)?;
            self.u64(region.dst)?;
            self.u64(region.size)
        })
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
}
