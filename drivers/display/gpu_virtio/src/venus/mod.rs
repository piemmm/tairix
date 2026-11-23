//! The Venus protocol as the host's renderer reads it (`plans/GPU.md` GP5.3):
//! the commands the driver encodes, the replies the renderer writes back, and
//! the names both use. Any stream the renderer cannot decode is fatal to its
//! context on the host, so every command is held byte for byte to the
//! renderer's grammar.

pub(crate) mod backend;
pub(crate) mod desk;
pub(crate) mod display;
pub(crate) mod encode;
pub(crate) mod info;
pub(crate) mod reply;
#[cfg(any(test, feature = "test-support"))]
pub mod testing;

use alloc::boxed::Box;
use alloc::sync::Arc;

use tairix_abi::driver::MemoryMapper;
use tairix_abi::hwtree::HwResource;
use tairix_abi::DriverError;
use tairix_display::{BufferMemory, DeviceBuffer};
use tairix_drvrt::Recorder;
use tairix_gpu::{Device, Spawn};
use tairix_rt::sync::Mutex;
use tairix_virtio::{SharedMemory, Transport};

use crate::{ConsoleSource, Record, VirtioGpu};

/// The device's Venus host, served as a GPU: the desk the GPU's threads
/// order through, which the display loop serves.
pub struct Venus {
    desk: Arc<desk::Desk<backend::Then>>,
    shared: Arc<Mutex<SharedMemory>>,
}

impl Venus {
    /// The GPU the host states. Called from a thread of the GPU's own: it
    /// waits on the display loop, which serves the desk meanwhile; each answer
    /// is acted on by a thread `threads` starts.
    ///
    /// # Errors
    ///
    /// [`DriverError::Unsupported`] for a host the backend cannot serve or a
    /// thread that could not be started, else the device's refusal while it
    /// was read.
    pub fn device(&self, threads: &dyn Spawn) -> Result<Device, DriverError> {
        let clock = Arc::try_new(tairix_rt::ClockDelay).map_err(|_| DriverError::OutOfMemory)?;
        backend::device(
            Arc::clone(&self.desk),
            Arc::clone(&self.shared),
            threads,
            clock,
        )
    }
}

impl<'h, T, M, C, L> VirtioGpu<'h, T, M, C, L>
where
    T: Transport,
    M: BufferMemory,
    M::Region: DeviceBuffer,
    C: ConsoleSource,
    L: Recorder<Record>,
{
    /// Serve the device's Venus host as a GPU from here on: its blobs placed
    /// in `region`, the node's host-visible region, and mapped into the driver
    /// through `mapper`, the GPU's threads rousing the display loop on `port`,
    /// a port this process bound.
    ///
    /// # Errors
    ///
    /// As [`SharedMemory::new`] for the region, [`DriverError::Unsupported`]
    /// where the device takes no Venus work, [`DriverError::Busy`] where it is
    /// served already, and [`DriverError::OutOfMemory`] where the desk cannot
    /// be had.
    pub fn serve_venus(
        &mut self,
        region: HwResource,
        mapper: &'h dyn MemoryMapper,
        port: u64,
    ) -> Result<Venus, DriverError> {
        let out_of_memory = |_| DriverError::OutOfMemory;
        let shared = Arc::try_new(Mutex::new(SharedMemory::new(region)?)).map_err(out_of_memory)?;
        let rouse = Box::try_new(desk::PortRouse(port)).map_err(|_| DriverError::OutOfMemory)?;
        let desk = Arc::try_new(desk::Desk::new(rouse)).map_err(|_| DriverError::OutOfMemory)?;
        self.attach_desk(
            Arc::clone(&desk) as Arc<dyn display::Counter>,
            Arc::clone(&shared),
            mapper,
            Some(port),
        )?;
        Ok(Venus { desk, shared })
    }
}

/// A command, by the number the renderer dispatches on.
#[repr(i32)]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Command {
    CreateInstance = 0,
    EnumeratePhysicalDevices = 2,
    CreateDevice = 11,
    QueueSubmit = 18,
    AllocateMemory = 21,
    FreeMemory = 22,
    CreateBuffer = 50,
    DestroyBuffer = 51,
    CreateCommandPool = 85,
    AllocateCommandBuffers = 88,
    BeginCommandBuffer = 90,
    EndCommandBuffer = 91,
    CmdCopyBuffer = 112,
    CmdUpdateBuffer = 117,
    CmdFillBuffer = 118,
    CmdPipelineBarrier = 126,
    BindBufferMemory2 = 138,
    GetBufferMemoryRequirements2 = 145,
    GetPhysicalDeviceProperties2 = 148,
    GetPhysicalDeviceQueueFamilyProperties2 = 151,
    GetPhysicalDeviceMemoryProperties2 = 152,
    GetDeviceQueue2 = 155,
    SetReplyCommandStreamMesa = 178,
    SeekReplyCommandStreamMesa = 179,
    ExecuteCommandStreamsMesa = 180,
}

/// A structure, by the type its first member states.
#[repr(i32)]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Structure {
    ApplicationInfo = 0,
    InstanceCreateInfo = 1,
    DeviceQueueCreateInfo = 2,
    DeviceCreateInfo = 3,
    SubmitInfo = 4,
    MemoryAllocateInfo = 5,
    BufferCreateInfo = 12,
    CommandPoolCreateInfo = 39,
    CommandBufferAllocateInfo = 40,
    CommandBufferBeginInfo = 42,
    MemoryBarrier = 46,
    PhysicalDeviceProperties2 = 1_000_059_001,
    PhysicalDeviceSubgroupProperties = 1_000_094_000,
    PhysicalDeviceMaintenance3Properties = 1_000_168_000,
    QueueFamilyProperties2 = 1_000_059_005,
    PhysicalDeviceMemoryProperties2 = 1_000_059_006,
    DeviceQueueInfo2 = 1_000_145_003,
    BufferMemoryRequirementsInfo2 = 1_000_146_000,
    MemoryRequirements2 = 1_000_146_003,
    BindBufferMemoryInfo = 1_000_157_000,
    DeviceQueueTimelineInfoMesa = 1_000_384_005,
}

/// The Vulkan version the driver's instance asks for: the first with the
/// queries it makes.
pub(crate) const API_VERSION: u32 = (1 << 22) | (1 << 12);

/// The most memory types and heaps a physical device describes.
pub(crate) const MAX_MEMORY_TYPES: usize = 32;
pub(crate) const MAX_MEMORY_HEAPS: usize = 16;

/// The bytes of a physical device's name.
pub(crate) const PHYSICAL_DEVICE_NAME_LEN: usize = 256;

/// The bytes of a pipeline cache's identity.
pub(crate) const UUID_LEN: usize = 16;

/// The most rings a context's queues are bound to, each its own fence ring:
/// ring 0 is the context's own, answering once the renderer has decoded
/// everything sent before it.
pub(crate) const MAX_QUEUE_RINGS: u32 = 63;

/// Memory property flags.
pub(crate) mod memory {
    pub(crate) const DEVICE_LOCAL: u32 = 1;
    pub(crate) const HOST_VISIBLE: u32 = 1 << 1;
    pub(crate) const HOST_COHERENT: u32 = 1 << 2;
    pub(crate) const HOST_CACHED: u32 = 1 << 3;
}

/// A memory heap's flags.
pub(crate) const HEAP_DEVICE_LOCAL: u32 = 1;

/// A queue family's flags.
pub(crate) mod queue {
    pub(crate) const GRAPHICS: u32 = 1;
    pub(crate) const COMPUTE: u32 = 1 << 1;
    pub(crate) const TRANSFER: u32 = 1 << 2;
}

/// Buffer usage flags: every use the seam names a buffer for.
pub(crate) const BUFFER_USAGE_ALL: u32 = 0x1ff;

/// Pipeline stage flags.
pub(crate) const STAGE_HOST: u32 = 0x4000;
pub(crate) const STAGE_ALL_COMMANDS: u32 = 0x1_0000;

/// Access flags.
pub(crate) const ACCESS_HOST_READ: u32 = 0x2000;
pub(crate) const ACCESS_MEMORY_READ: u32 = 0x8000;
pub(crate) const ACCESS_MEMORY_WRITE: u32 = 0x1_0000;

/// A command buffer recorded once and submitted once.
pub(crate) const ONE_TIME_SUBMIT: u32 = 1;

/// A command pool whose buffers are reset one at a time.
pub(crate) const RESET_COMMAND_BUFFER: u32 = 1 << 1;

/// What a command answered: success, or the renderer's refusal.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum VkResult {
    Success,
    /// A result below success: the host could not do what was asked.
    Failed(i32),
    /// A result above success: done in part, which no command the driver
    /// sends may answer.
    Partial(i32),
}

impl VkResult {
    pub(crate) const fn of(raw: i32) -> Self {
        match raw {
            0 => Self::Success,
            failed if failed < 0 => Self::Failed(failed),
            partial => Self::Partial(partial),
        }
    }
}

#[cfg(test)]
#[path = "venus_tests.rs"]
mod tests;
