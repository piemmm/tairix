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
    EnumerateDeviceExtensionProperties = 14,
    QueueSubmit = 18,
    AllocateMemory = 21,
    FreeMemory = 22,
    CreateBuffer = 50,
    DestroyBuffer = 51,
    CreateBufferView = 52,
    DestroyBufferView = 53,
    CreateImage = 54,
    DestroyImage = 55,
    CreateImageView = 57,
    DestroyImageView = 58,
    CreateShaderModule = 59,
    DestroyShaderModule = 60,
    CreateGraphicsPipelines = 65,
    CreateComputePipelines = 66,
    DestroyPipeline = 67,
    CreatePipelineLayout = 68,
    DestroyPipelineLayout = 69,
    CreateSampler = 70,
    DestroySampler = 71,
    CreateDescriptorSetLayout = 72,
    DestroyDescriptorSetLayout = 73,
    CreateDescriptorPool = 74,
    DestroyDescriptorPool = 75,
    AllocateDescriptorSets = 77,
    UpdateDescriptorSets = 79,
    CreateCommandPool = 85,
    AllocateCommandBuffers = 88,
    BeginCommandBuffer = 90,
    EndCommandBuffer = 91,
    CmdBindPipeline = 93,
    CmdSetViewport = 94,
    CmdSetScissor = 95,
    CmdSetBlendConstants = 98,
    CmdSetStencilReference = 102,
    CmdBindDescriptorSets = 103,
    CmdDraw = 106,
    CmdDrawIndexed = 107,
    CmdDrawIndirect = 108,
    CmdDrawIndexedIndirect = 109,
    CmdDispatch = 110,
    CmdDispatchIndirect = 111,
    CmdCopyBuffer = 112,
    CmdCopyImage = 113,
    CmdCopyBufferToImage = 115,
    CmdCopyImageToBuffer = 116,
    CmdUpdateBuffer = 117,
    CmdFillBuffer = 118,
    CmdClearColorImage = 119,
    CmdClearDepthStencilImage = 120,
    CmdPipelineBarrier = 126,
    CmdPushConstants = 132,
    BindBufferMemory2 = 138,
    BindImageMemory2 = 139,
    GetImageMemoryRequirements2 = 144,
    GetBufferMemoryRequirements2 = 145,
    GetPhysicalDeviceFeatures2 = 147,
    GetPhysicalDeviceProperties2 = 148,
    GetPhysicalDeviceFormatProperties2 = 149,
    GetPhysicalDeviceImageFormatProperties2 = 150,
    GetPhysicalDeviceQueueFamilyProperties2 = 151,
    GetPhysicalDeviceMemoryProperties2 = 152,
    GetDeviceQueue2 = 155,
    SetReplyCommandStreamMesa = 178,
    SeekReplyCommandStreamMesa = 179,
    ExecuteCommandStreamsMesa = 180,
    CmdBeginRendering = 213,
    CmdEndRendering = 214,
    CmdBindVertexBuffers2 = 220,
    GetDeviceImageMemoryRequirements = 231,
    CmdBindIndexBuffer2 = 279,
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
    BufferViewCreateInfo = 13,
    ImageCreateInfo = 14,
    ImageViewCreateInfo = 15,
    ShaderModuleCreateInfo = 16,
    PipelineShaderStageCreateInfo = 18,
    PipelineVertexInputStateCreateInfo = 19,
    PipelineInputAssemblyStateCreateInfo = 20,
    PipelineViewportStateCreateInfo = 22,
    PipelineRasterizationStateCreateInfo = 23,
    PipelineMultisampleStateCreateInfo = 24,
    PipelineDepthStencilStateCreateInfo = 25,
    PipelineColorBlendStateCreateInfo = 26,
    PipelineDynamicStateCreateInfo = 27,
    GraphicsPipelineCreateInfo = 28,
    ComputePipelineCreateInfo = 29,
    PipelineLayoutCreateInfo = 30,
    SamplerCreateInfo = 31,
    DescriptorSetLayoutCreateInfo = 32,
    DescriptorPoolCreateInfo = 33,
    DescriptorSetAllocateInfo = 34,
    WriteDescriptorSet = 35,
    CommandPoolCreateInfo = 39,
    CommandBufferAllocateInfo = 40,
    CommandBufferBeginInfo = 42,
    ImageMemoryBarrier = 45,
    MemoryBarrier = 46,
    RenderingInfo = 1_000_044_000,
    RenderingAttachmentInfo = 1_000_044_001,
    PipelineRenderingCreateInfo = 1_000_044_002,
    PhysicalDeviceDynamicRenderingFeatures = 1_000_044_003,
    PhysicalDeviceFeatures2 = 1_000_059_000,
    PhysicalDeviceProperties2 = 1_000_059_001,
    FormatProperties2 = 1_000_059_002,
    ImageFormatProperties2 = 1_000_059_003,
    PhysicalDeviceImageFormatInfo2 = 1_000_059_004,
    PhysicalDeviceShaderFloat16Int8Features = 1_000_082_000,
    PhysicalDevice16BitStorageFeatures = 1_000_083_000,
    PhysicalDeviceSubgroupProperties = 1_000_094_000,
    ImageViewUsageCreateInfo = 1_000_117_002,
    PhysicalDeviceMaintenance3Properties = 1_000_168_000,
    QueueFamilyProperties2 = 1_000_059_005,
    PhysicalDeviceMemoryProperties2 = 1_000_059_006,
    DeviceQueueInfo2 = 1_000_145_003,
    BufferMemoryRequirementsInfo2 = 1_000_146_000,
    ImageMemoryRequirementsInfo2 = 1_000_146_001,
    MemoryRequirements2 = 1_000_146_003,
    ImageFormatListCreateInfo = 1_000_147_000,
    BindBufferMemoryInfo = 1_000_157_000,
    BindImageMemoryInfo = 1_000_157_001,
    PhysicalDeviceLineRasterizationFeatures = 1_000_259_000,
    PipelineRasterizationLineStateCreateInfo = 1_000_259_001,
    PhysicalDeviceRobustness2Features = 1_000_286_000,
    PhysicalDeviceRobustness2Properties = 1_000_286_001,
    PhysicalDeviceZeroInitializeWorkgroupMemoryFeatures = 1_000_325_000,
    FormatProperties3 = 1_000_360_000,
    DeviceQueueTimelineInfoMesa = 1_000_384_005,
    DeviceImageMemoryRequirements = 1_000_413_003,
    PhysicalDeviceMaintenance5Features = 1_000_470_000,
}

/// A Vulkan version, as its API word states it.
pub(crate) const fn api_version(minor: u32) -> u32 {
    (1 << 22) | (minor << 12)
}

/// The Vulkan version the driver requires of a host: the first with the
/// queries it makes.
pub(crate) const MIN_API_VERSION: u32 = api_version(1);

/// The Vulkan version the driver's instance asks for: the first whose
/// devices take every SPIR-V version the seam admits.
pub(crate) const API_VERSION: u32 = api_version(3);

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

/// A queue family's flags: the driver reads the one, and its tests state
/// the others of the families they describe.
pub(crate) mod queue {
    pub(crate) const GRAPHICS: u32 = 1;
    pub(crate) const COMPUTE: u32 = 1 << 1;
    #[cfg(test)]
    pub(crate) const TRANSFER: u32 = 1 << 2;
}

/// Buffer usage flags: every use the seam names a buffer for.
pub(crate) const BUFFER_USAGE_ALL: u32 = 0x1ff;

/// The descriptor types a bind set holds.
pub(crate) mod descriptor {
    pub(crate) const SAMPLER: u32 = 0;
    pub(crate) const COMBINED_IMAGE_SAMPLER: u32 = 1;
    pub(crate) const SAMPLED_IMAGE: u32 = 2;
    pub(crate) const STORAGE_IMAGE: u32 = 3;
    pub(crate) const UNIFORM_TEXEL_BUFFER: u32 = 4;
    pub(crate) const STORAGE_TEXEL_BUFFER: u32 = 5;
    pub(crate) const UNIFORM_BUFFER: u32 = 6;
    pub(crate) const STORAGE_BUFFER: u32 = 7;
}

/// What an image is made as, laid out in and seen through.
pub(crate) mod image {
    /// Usage flags, a seam usage's bit its own.
    pub(crate) const TRANSFER_SRC: u32 = 1;
    pub(crate) const TRANSFER_DST: u32 = 1 << 1;
    pub(crate) const SAMPLED: u32 = 1 << 2;
    pub(crate) const STORAGE: u32 = 1 << 3;
    pub(crate) const COLOR_ATTACHMENT: u32 = 1 << 4;
    pub(crate) const DEPTH_STENCIL_ATTACHMENT: u32 = 1 << 5;
    /// Creation flags.
    pub(crate) const MUTABLE_FORMAT: u32 = 1 << 3;
    pub(crate) const CUBE_COMPATIBLE: u32 = 1 << 4;
    /// The layout an image starts in, and the one it is kept in for every
    /// use.
    pub(crate) const UNDEFINED: i32 = 0;
    pub(crate) const GENERAL: i32 = 1;
    /// Texels laid out as the device chooses.
    pub(crate) const OPTIMAL: i32 = 0;
}

/// The features a format states, of an image laid out as the device chooses
/// or of a buffer: those past the first 31 bits only by a host stating
/// Vulkan 1.3's format features.
pub(crate) mod format_feature {
    pub(crate) const SAMPLED_IMAGE: u64 = 1;
    pub(crate) const STORAGE_IMAGE: u64 = 1 << 1;
    pub(crate) const UNIFORM_TEXEL_BUFFER: u64 = 1 << 3;
    pub(crate) const STORAGE_TEXEL_BUFFER: u64 = 1 << 4;
    pub(crate) const VERTEX_BUFFER: u64 = 1 << 6;
    pub(crate) const COLOR_ATTACHMENT: u64 = 1 << 7;
    pub(crate) const COLOR_ATTACHMENT_BLEND: u64 = 1 << 8;
    pub(crate) const DEPTH_STENCIL_ATTACHMENT: u64 = 1 << 9;
    pub(crate) const SAMPLED_IMAGE_FILTER_LINEAR: u64 = 1 << 12;
    pub(crate) const TRANSFER_SRC: u64 = 1 << 14;
    pub(crate) const TRANSFER_DST: u64 = 1 << 15;
    pub(crate) const STORAGE_READ_WITHOUT_FORMAT: u64 = 1 << 31;
    pub(crate) const STORAGE_WRITE_WITHOUT_FORMAT: u64 = 1 << 32;
    pub(crate) const SAMPLED_IMAGE_DEPTH_COMPARISON: u64 = 1 << 33;
}

/// The planes of an image a view or command names.
pub(crate) mod aspect {
    pub(crate) const COLOR: u32 = 1;
    pub(crate) const DEPTH: u32 = 1 << 1;
    pub(crate) const STENCIL: u32 = 1 << 2;
}

/// Shader stage flags.
pub(crate) mod stage {
    pub(crate) const VERTEX: u32 = 1;
    pub(crate) const FRAGMENT: u32 = 1 << 4;
    pub(crate) const COMPUTE: u32 = 1 << 5;
}

/// The points a render and a compute pipeline are bound at.
pub(crate) const BIND_POINT_GRAPHICS: u32 = 0;
pub(crate) const BIND_POINT_COMPUTE: u32 = 1;

/// The device extensions the driver asks for, by name.
pub(crate) mod extension {
    pub(crate) const ROBUSTNESS2_KHR: &str = "VK_KHR_robustness2";
    pub(crate) const ROBUSTNESS2_EXT: &str = "VK_EXT_robustness2";
    pub(crate) const ZERO_INITIALIZE_WORKGROUP_MEMORY: &str =
        "VK_KHR_zero_initialize_workgroup_memory";
    pub(crate) const SHADER_FLOAT16_INT8: &str = "VK_KHR_shader_float16_int8";
    pub(crate) const SAMPLER_MIRROR_CLAMP_TO_EDGE: &str = "VK_KHR_sampler_mirror_clamp_to_edge";
    pub(crate) const DEPTH_RANGE_UNRESTRICTED: &str = "VK_EXT_depth_range_unrestricted";
    pub(crate) const FORMAT_FEATURE_FLAGS2: &str = "VK_KHR_format_feature_flags2";
    pub(crate) const MAINTENANCE5: &str = "VK_KHR_maintenance5";
    pub(crate) const LINE_RASTERIZATION_KHR: &str = "VK_KHR_line_rasterization";
    pub(crate) const LINE_RASTERIZATION_EXT: &str = "VK_EXT_line_rasterization";
}

/// The bytes of an extension's name.
pub(crate) const EXTENSION_NAME_LEN: usize = 256;

/// The Boolean members of a physical device's features.
pub(crate) const CORE_FEATURES: usize = 55;

/// Pipeline stage flags.
pub(crate) const STAGE_TOP_OF_PIPE: u32 = 1;
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
