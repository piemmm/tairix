//! The encoder held byte for byte to the renderer's grammar, each expected
//! stream written field by field from the renderer's decoder for its
//! command; the replies read as the renderer writes them, and refused where
//! it would not.

use alloc::vec::Vec;

use tairix_abi::DriverError;

use super::encode::{
    AttachmentInfo, AttributeInfo, BufferImageRegion, Chained, CopyRegion, DescriptorWrite,
    DrawInfo, Enabled, Encoder, FormatQuery, GraphicsInfo, HostClear, ImageInfo, ImageRegion,
    Layers, LayoutEntry, RasterInfo, SamplerInfo, StageInfo, StencilInfo, StreamSpan, Subresources,
    TargetInfo, ViewInfo, Written,
};
use super::reply::{
    HostExtensions, HostFeatures, HostFormatFeatures, HostImageLimits, HostMemoryHeap,
    HostMemoryType, HostQueueFamily, Reader, RobustAlignments,
};
use super::testing::{
    extensions_reply, features_reply, format_reply, image_format_reply, limits, properties_reply,
    Grammar,
};
use super::{
    descriptor, extension, stage, Command, Structure, VkResult, API_VERSION, BIND_POINT_COMPUTE,
    BIND_POINT_GRAPHICS, CORE_FEATURES, EXTENSION_NAME_LEN, MAX_MEMORY_HEAPS, MAX_MEMORY_TYPES,
};

/// The granularity the tests' host bounds robust accesses at.
const ROBUST: RobustAlignments = RobustAlignments {
    storage: 4,
    uniform: 16,
};

fn encoded(encode: impl FnOnce(&mut Encoder) -> Result<(), DriverError>) -> Vec<u8> {
    let mut encoder = Encoder::default();
    encode(&mut encoder).expect("encoded");
    encoder.as_bytes().to_vec()
}

#[test]
fn an_instance_is_made_at_the_api_version_with_no_name_layer_or_extension() {
    let expected = Grammar::default()
        .command(Command::CreateInstance, true)
        .u64(1)
        .structure(Structure::InstanceCreateInfo)
        .u32(0)
        .u64(1)
        .structure(Structure::ApplicationInfo)
        .u64(0)
        .u32(0)
        .u64(0)
        .u32(0)
        .u32(API_VERSION)
        .u32(0)
        .u64(0)
        .u32(0)
        .u64(0)
        .u64(0)
        .u64(1)
        .u64(7);
    assert_eq!(encoded(|e| e.create_instance(7)), expected.0);
    assert_eq!(API_VERSION, 0x0040_3000, "Vulkan 1.3");
}

#[test]
fn physical_devices_are_counted_then_named() {
    let counted = Grammar::default()
        .command(Command::EnumeratePhysicalDevices, true)
        .u64(7)
        .u64(1)
        .u32(0)
        .u64(0);
    assert_eq!(encoded(|e| e.enumerate_physical_devices(7, &[])), counted.0);
    let named = Grammar::default()
        .command(Command::EnumeratePhysicalDevices, true)
        .u64(7)
        .u64(1)
        .u32(2)
        .u64(2)
        .u64(8)
        .u64(9);
    assert_eq!(
        encoded(|e| e.enumerate_physical_devices(7, &[8, 9])),
        named.0
    );
}

#[test]
fn a_physical_device_is_asked_its_properties_families_and_memory_as_skeletons() {
    let asked = Grammar::default()
        .command(Command::GetPhysicalDeviceProperties2, true)
        .u64(8)
        .u64(1)
        .i32(Structure::PhysicalDeviceProperties2 as i32)
        .u64(1)
        .i32(Structure::PhysicalDeviceSubgroupProperties as i32)
        .u64(1);
    let unrobust = asked
        .clone()
        .structure(Structure::PhysicalDeviceMaintenance3Properties);
    assert_eq!(
        encoded(|e| e.get_physical_device_properties2(8, false)),
        unrobust.0
    );
    let robust = asked
        .i32(Structure::PhysicalDeviceMaintenance3Properties as i32)
        .u64(1)
        .structure(Structure::PhysicalDeviceRobustness2Properties);
    assert_eq!(
        encoded(|e| e.get_physical_device_properties2(8, true)),
        robust.0
    );
    let counted = Grammar::default()
        .command(Command::GetPhysicalDeviceQueueFamilyProperties2, true)
        .u64(8)
        .u64(1)
        .u32(0)
        .u64(0);
    assert_eq!(
        encoded(|e| e.get_physical_device_queue_family_properties2(8, 0)),
        counted.0
    );
    let families = Grammar::default()
        .command(Command::GetPhysicalDeviceQueueFamilyProperties2, true)
        .u64(8)
        .u64(1)
        .u32(2)
        .u64(2)
        .structure(Structure::QueueFamilyProperties2)
        .structure(Structure::QueueFamilyProperties2);
    assert_eq!(
        encoded(|e| e.get_physical_device_queue_family_properties2(8, 2)),
        families.0
    );
    let memory = Grammar::default()
        .command(Command::GetPhysicalDeviceMemoryProperties2, true)
        .u64(8)
        .u64(1)
        .structure(Structure::PhysicalDeviceMemoryProperties2)
        .u64(MAX_MEMORY_TYPES as u64)
        .u64(MAX_MEMORY_HEAPS as u64);
    assert_eq!(
        encoded(|e| e.get_physical_device_memory_properties2(8)),
        memory.0
    );
}

#[test]
fn a_physical_device_is_asked_its_features_and_extensions_as_skeletons() {
    let chain = [
        Structure::PhysicalDeviceRobustness2Features,
        Structure::PhysicalDeviceZeroInitializeWorkgroupMemoryFeatures,
    ];
    let features = Grammar::default()
        .command(Command::GetPhysicalDeviceFeatures2, true)
        .u64(8)
        .u64(1)
        .i32(Structure::PhysicalDeviceFeatures2 as i32)
        .u64(1)
        .i32(chain[0] as i32)
        .u64(1)
        .i32(chain[1] as i32)
        .u64(0);
    assert_eq!(
        encoded(|e| e.get_physical_device_features2(8, &chain)),
        features.0
    );
    let alone = Grammar::default()
        .command(Command::GetPhysicalDeviceFeatures2, true)
        .u64(8)
        .u64(1)
        .structure(Structure::PhysicalDeviceFeatures2);
    assert_eq!(
        encoded(|e| e.get_physical_device_features2(8, &[])),
        alone.0
    );
    let counted = Grammar::default()
        .command(Command::EnumerateDeviceExtensionProperties, true)
        .u64(8)
        .u64(0)
        .u64(1)
        .u32(0)
        .u64(0);
    assert_eq!(
        encoded(|e| e.enumerate_device_extension_properties(8, 0)),
        counted.0
    );
    let named = Grammar::default()
        .command(Command::EnumerateDeviceExtensionProperties, true)
        .u64(8)
        .u64(0)
        .u64(1)
        .u32(3)
        .u64(3);
    assert_eq!(
        encoded(|e| e.enumerate_device_extension_properties(8, 3)),
        named.0
    );
}

/// A device's queues as the encoder asks for them: two of family 3, then no
/// layers.
fn queues(grammar: Grammar) -> Grammar {
    grammar
        .u32(0)
        .u32(1)
        .u64(1)
        .structure(Structure::DeviceQueueCreateInfo)
        .u32(0)
        .u32(3)
        .u32(2)
        .u64(2)
        .f32(1.0)
        .f32(1.0)
        .u32(0)
        .u64(0)
}

/// The core features the device tests enable: robust buffer access and
/// 16-bit integers.
fn core_features() -> [bool; CORE_FEATURES] {
    let mut core = [false; CORE_FEATURES];
    core[0] = true;
    core[41] = true;
    core
}

#[test]
fn a_device_is_made_with_its_extensions_and_features_chained() {
    let core = core_features();
    let robust = [true, true, false];
    let zeroed = [true];
    let chain = [
        Chained {
            structure: Structure::PhysicalDeviceRobustness2Features,
            members: &robust,
        },
        Chained {
            structure: Structure::PhysicalDeviceZeroInitializeWorkgroupMemoryFeatures,
            members: &zeroed,
        },
    ];
    let names = [
        extension::ROBUSTNESS2_KHR,
        extension::ZERO_INITIALIZE_WORKGROUP_MEMORY,
    ];
    let enabled = Enabled {
        extensions: &names,
        core: &core,
        chain: &chain,
    };
    let device = queues(
        Grammar::default()
            .command(Command::CreateDevice, true)
            .u64(8)
            .u64(1)
            .i32(Structure::DeviceCreateInfo as i32)
            .u64(1)
            .i32(Structure::PhysicalDeviceFeatures2 as i32)
            .u64(1)
            .i32(chain[0].structure as i32)
            .u64(1)
            .i32(chain[1].structure as i32)
            .u64(0)
            .booleans(&zeroed)
            .booleans(&robust)
            .booleans(&core),
    )
    .u32(2)
    .u64(2)
    .string(names[0])
    .string(names[1])
    .u64(0)
    .u64(0)
    .u64(1)
    .u64(10);
    assert_eq!(
        encoded(|e| e.create_device(8, 10, (3, 2), &enabled)),
        device.0
    );
}

#[test]
fn a_device_of_no_extension_chains_its_core_features_alone_and_binds_a_queue_to_a_ring() {
    let core = core_features();
    let bare = Enabled {
        extensions: &[],
        core: &core,
        chain: &[],
    };
    let alone = queues(
        Grammar::default()
            .command(Command::CreateDevice, true)
            .u64(8)
            .u64(1)
            .i32(Structure::DeviceCreateInfo as i32)
            .u64(1)
            .structure(Structure::PhysicalDeviceFeatures2)
            .booleans(&core),
    )
    .u32(0)
    .u64(0)
    .u64(0)
    .u64(0)
    .u64(1)
    .u64(10);
    assert_eq!(encoded(|e| e.create_device(8, 10, (3, 2), &bare)), alone.0);
    let queue = Grammar::default()
        .command(Command::GetDeviceQueue2, false)
        .u64(10)
        .u64(1)
        .i32(Structure::DeviceQueueInfo2 as i32)
        .u64(1)
        .structure(Structure::DeviceQueueTimelineInfoMesa)
        .u32(5)
        .u32(0)
        .u32(3)
        .u32(1)
        .u64(1)
        .u64(11);
    assert_eq!(encoded(|e| e.get_device_queue2(10, (3, 1), 5, 11)), queue.0);
}

#[test]
fn memory_and_the_buffer_over_it_are_made_sized_and_bound() {
    let memory = Grammar::default()
        .command(Command::AllocateMemory, true)
        .u64(10)
        .u64(1)
        .structure(Structure::MemoryAllocateInfo)
        .u64(1 << 20)
        .u32(2)
        .u64(0)
        .u64(1)
        .u64(12);
    assert_eq!(
        encoded(|e| e.allocate_memory(10, 12, (1 << 20, 2))),
        memory.0
    );
    let buffer = Grammar::default()
        .command(Command::CreateBuffer, true)
        .u64(10)
        .u64(1)
        .structure(Structure::BufferCreateInfo)
        .u32(0)
        .u64(4096)
        .u32(0x1ff)
        .i32(0)
        .u32(0)
        .u64(0)
        .u64(0)
        .u64(1)
        .u64(13);
    assert_eq!(
        encoded(|e| e.create_buffer(10, 13, (4096, 0x1ff))),
        buffer.0
    );
    let required = Grammar::default()
        .command(Command::GetBufferMemoryRequirements2, true)
        .u64(10)
        .u64(1)
        .structure(Structure::BufferMemoryRequirementsInfo2)
        .u64(13)
        .u64(1)
        .structure(Structure::MemoryRequirements2);
    assert_eq!(
        encoded(|e| e.get_buffer_memory_requirements2(10, 13)),
        required.0
    );
    let bound = Grammar::default()
        .command(Command::BindBufferMemory2, true)
        .u64(10)
        .u32(1)
        .u64(1)
        .structure(Structure::BindBufferMemoryInfo)
        .u64(13)
        .u64(12)
        .u64(0);
    assert_eq!(encoded(|e| e.bind_buffer_memory2(10, 13, (12, 0))), bound.0);
    let freed = Grammar::default()
        .command(Command::FreeMemory, false)
        .u64(10)
        .u64(12)
        .u64(0);
    assert_eq!(encoded(|e| e.free_memory(10, 12)), freed.0);
    let destroyed = Grammar::default()
        .command(Command::DestroyBuffer, false)
        .u64(10)
        .u64(13)
        .u64(0);
    assert_eq!(encoded(|e| e.destroy_buffer(10, 13)), destroyed.0);
}

#[test]
fn a_pool_and_its_command_buffers_are_made_and_recorded() {
    let pool = Grammar::default()
        .command(Command::CreateCommandPool, true)
        .u64(10)
        .u64(1)
        .structure(Structure::CommandPoolCreateInfo)
        .u32(2)
        .u32(3)
        .u64(0)
        .u64(1)
        .u64(14);
    assert_eq!(encoded(|e| e.create_command_pool(10, 14, (3, 2))), pool.0);
    let buffers = Grammar::default()
        .command(Command::AllocateCommandBuffers, true)
        .u64(10)
        .u64(1)
        .structure(Structure::CommandBufferAllocateInfo)
        .u64(14)
        .i32(0)
        .u32(2)
        .u64(2)
        .u64(15)
        .u64(16);
    assert_eq!(
        encoded(|e| e.allocate_command_buffers(10, 14, &[15, 16])),
        buffers.0
    );
    let begun = Grammar::default()
        .command(Command::BeginCommandBuffer, false)
        .u64(15)
        .u64(1)
        .structure(Structure::CommandBufferBeginInfo)
        .u32(1)
        .u64(0);
    assert_eq!(encoded(|e| e.begin_command_buffer(15, 1)), begun.0);
    let ended = Grammar::default()
        .command(Command::EndCommandBuffer, false)
        .u64(15);
    assert_eq!(encoded(|e| e.end_command_buffer(15)), ended.0);
}

#[test]
fn transfers_and_the_barrier_between_them_are_recorded() {
    let copy = Grammar::default()
        .command(Command::CmdCopyBuffer, false)
        .u64(15)
        .u64(13)
        .u64(17)
        .u32(2)
        .u64(2)
        .u64(0)
        .u64(8)
        .u64(4)
        .u64(16)
        .u64(32)
        .u64(12);
    let regions = [
        CopyRegion {
            src: 0,
            dst: 8,
            size: 4,
        },
        CopyRegion {
            src: 16,
            dst: 32,
            size: 12,
        },
    ];
    assert_eq!(
        encoded(|e| e.cmd_copy_buffer(15, (13, 17), regions.iter().copied())),
        copy.0
    );
    let fill = Grammar::default()
        .command(Command::CmdFillBuffer, false)
        .u64(15)
        .u64(13)
        .u64(4)
        .u64(8)
        .u32(0xdead_beef);
    assert_eq!(
        encoded(|e| e.cmd_fill_buffer(15, 13, (4, 8), 0xdead_beef)),
        fill.0
    );
    let update = Grammar::default()
        .command(Command::CmdUpdateBuffer, false)
        .u64(15)
        .u64(13)
        .u64(8)
        .u64(8)
        .u64(8)
        .bytes(&[1, 2, 3, 4, 5, 6, 7, 8]);
    assert_eq!(
        encoded(|e| e.cmd_update_buffer(15, 13, 8, &[1, 2, 3, 4, 5, 6, 7, 8])),
        update.0
    );
    let mut encoder = Encoder::default();
    assert_eq!(
        encoder.cmd_update_buffer(15, 13, 8, &[1, 2, 3]),
        Err(DriverError::LengthOutOfRange),
        "an update is whole words"
    );
    assert!(encoder.as_bytes().is_empty(), "and encodes nothing");
    let barrier = Grammar::default()
        .command(Command::CmdPipelineBarrier, false)
        .u64(15)
        .u32(0x1_0000)
        .u32(0x1_0000)
        .u32(0)
        .u32(1)
        .u64(1)
        .structure(Structure::MemoryBarrier)
        .u32(0x1_0000)
        .u32(0x1_8000)
        .u32(0)
        .u64(0)
        .u32(0)
        .u64(0);
    assert_eq!(encoded(|e| e.cmd_barrier(15)), barrier.0);
    let host = Grammar::default()
        .command(Command::CmdPipelineBarrier, false)
        .u64(15)
        .u32(0x1_0000)
        .u32(0x4000)
        .u32(0)
        .u32(1)
        .u64(1)
        .structure(Structure::MemoryBarrier)
        .u32(0x1_0000)
        .u32(0x2000)
        .u32(0)
        .u64(0)
        .u32(0)
        .u64(0);
    assert_eq!(encoded(|e| e.cmd_host_barrier(15)), host.0);
    let mut joined = Encoder::default();
    joined.cmd_barrier(15).expect("encoded");
    let mut tail = Encoder::default();
    tail.cmd_host_barrier(15).expect("encoded");
    joined.append(&tail).expect("appended");
    assert_eq!(joined.as_bytes(), [barrier.0, host.0].concat().as_slice());
}

#[test]
fn a_submission_and_the_streams_it_travels_in_are_encoded() {
    let submitted = Grammar::default()
        .command(Command::QueueSubmit, false)
        .u64(11)
        .u32(1)
        .u64(1)
        .structure(Structure::SubmitInfo)
        .u32(0)
        .u64(0)
        .u64(0)
        .u32(1)
        .u64(1)
        .u64(15)
        .u32(0)
        .u64(0)
        .u64(0);
    assert_eq!(encoded(|e| e.queue_submit(11, &[15])), submitted.0);
    let span = StreamSpan {
        resource: 0x1_0002,
        offset: 4096,
        size: 640,
    };
    let reply = Grammar::default()
        .command(Command::SetReplyCommandStreamMesa, false)
        .u64(1)
        .u32(0x1_0002)
        .u64(4096)
        .u64(640);
    assert_eq!(encoded(|e| e.set_reply_command_stream(span)), reply.0);
    let seek = Grammar::default()
        .command(Command::SeekReplyCommandStreamMesa, false)
        .u64(64);
    assert_eq!(encoded(|e| e.seek_reply_command_stream(64)), seek.0);
    let executed = Grammar::default()
        .command(Command::ExecuteCommandStreamsMesa, false)
        .u32(2)
        .u64(2)
        .u32(0x1_0002)
        .u64(4096)
        .u64(640)
        .u32(0x1_0003)
        .u64(0)
        .u64(8)
        .u64(0)
        .u32(0)
        .u64(0)
        .u32(0);
    let second = StreamSpan {
        resource: 0x1_0003,
        offset: 0,
        size: 8,
    };
    assert_eq!(
        encoded(|e| e.execute_command_streams(&[span, second])),
        executed.0
    );
    let mut encoder = Encoder::default();
    assert_eq!(
        encoder.execute_command_streams(&[]),
        Err(DriverError::LengthOutOfRange),
        "the renderer runs no empty list of streams"
    );
}

#[test]
fn a_shader_module_is_made_of_whole_words() {
    let words = [3u8, 2, 35, 7, 0, 0, 1, 0];
    let module = Grammar::default()
        .command(Command::CreateShaderModule, true)
        .u64(10)
        .u64(1)
        .structure(Structure::ShaderModuleCreateInfo)
        .u32(0)
        .u64(8)
        .u64(2)
        .bytes(&words)
        .u64(0)
        .u64(1)
        .u64(20);
    assert_eq!(
        encoded(|e| e.create_shader_module(10, 20, &words)),
        module.0
    );
    let mut encoder = Encoder::default();
    assert_eq!(
        encoder.create_shader_module(10, 20, &words[..6]),
        Err(DriverError::LengthOutOfRange),
        "a module is whole words"
    );
    assert!(encoder.as_bytes().is_empty(), "and encodes nothing");
}

#[test]
fn a_set_layout_names_each_binding_and_none_with_a_sampler() {
    let entries = [
        LayoutEntry {
            binding: 0,
            kind: descriptor::STORAGE_BUFFER,
            count: 1,
            stages: stage::COMPUTE,
        },
        LayoutEntry {
            binding: 3,
            kind: descriptor::UNIFORM_BUFFER,
            count: 1,
            stages: stage::COMPUTE | stage::FRAGMENT,
        },
    ];
    let set_layout = Grammar::default()
        .command(Command::CreateDescriptorSetLayout, true)
        .u64(10)
        .u64(1)
        .structure(Structure::DescriptorSetLayoutCreateInfo)
        .u32(0)
        .u32(2)
        .u64(2)
        .u32(0)
        .u32(7)
        .u32(1)
        .u32(0x20)
        .u64(0)
        .u32(3)
        .u32(6)
        .u32(1)
        .u32(0x30)
        .u64(0)
        .u64(0)
        .u64(1)
        .u64(21);
    assert_eq!(
        encoded(|e| e.create_descriptor_set_layout(10, 21, &entries)),
        set_layout.0
    );
    let empty_set = Grammar::default()
        .command(Command::CreateDescriptorSetLayout, true)
        .u64(10)
        .u64(1)
        .structure(Structure::DescriptorSetLayoutCreateInfo)
        .u32(0)
        .u32(0)
        .u64(0)
        .u64(0)
        .u64(1)
        .u64(21);
    assert_eq!(
        encoded(|e| e.create_descriptor_set_layout(10, 21, &[])),
        empty_set.0
    );
}

#[test]
fn a_pipeline_layout_names_its_sets_and_its_push_constants() {
    let layout = Grammar::default()
        .command(Command::CreatePipelineLayout, true)
        .u64(10)
        .u64(1)
        .structure(Structure::PipelineLayoutCreateInfo)
        .u32(0)
        .u32(2)
        .u64(2)
        .u64(21)
        .u64(23)
        .u32(1)
        .u64(1)
        .u32(0x20)
        .u32(0)
        .u32(16)
        .u64(0)
        .u64(1)
        .u64(22);
    assert_eq!(
        encoded(|e| e.create_pipeline_layout(10, 22, &[21, 23], (stage::COMPUTE, 16))),
        layout.0
    );
    let bare = Grammar::default()
        .command(Command::CreatePipelineLayout, true)
        .u64(10)
        .u64(1)
        .structure(Structure::PipelineLayoutCreateInfo)
        .u32(0)
        .u32(0)
        .u64(0)
        .u32(0)
        .u64(0)
        .u64(0)
        .u64(1)
        .u64(22);
    assert_eq!(
        encoded(|e| e.create_pipeline_layout(10, 22, &[], (0, 0))),
        bare.0,
        "no sets and no push constants"
    );
}

#[test]
fn a_compute_pipeline_names_its_module_s_entry_point_and_its_layout() {
    let pipeline = Grammar::default()
        .command(Command::CreateComputePipelines, true)
        .u64(10)
        .u64(0)
        .u32(1)
        .u64(1)
        .structure(Structure::ComputePipelineCreateInfo)
        .u32(0)
        .structure(Structure::PipelineShaderStageCreateInfo)
        .u32(0)
        .u32(0x20)
        .u64(20)
        .string("main")
        .u64(0)
        .u64(22)
        .u64(0)
        .i32(-1)
        .u64(0)
        .u64(1)
        .u64(26);
    assert_eq!(
        encoded(|e| e.create_compute_pipelines(10, 26, (20, "main"), 22)),
        pipeline.0
    );
    for (command, id) in [
        (Command::DestroyShaderModule, 20),
        (Command::DestroyDescriptorSetLayout, 21),
        (Command::DestroyPipelineLayout, 22),
        (Command::DestroyPipeline, 26),
        (Command::DestroyDescriptorPool, 24),
    ] {
        let destroyed = Grammar::default()
            .command(command, false)
            .u64(10)
            .u64(id)
            .u64(0);
        assert_eq!(encoded(|e| e.destroy(command, 10, id)), destroyed.0);
    }
}

#[test]
fn a_set_is_made_from_a_pool_of_its_own_and_written() {
    let pool = Grammar::default()
        .command(Command::CreateDescriptorPool, true)
        .u64(10)
        .u64(1)
        .structure(Structure::DescriptorPoolCreateInfo)
        .u32(0)
        .u32(1)
        .u32(2)
        .u64(2)
        .u32(7)
        .u32(2)
        .u32(6)
        .u32(1)
        .u64(0)
        .u64(1)
        .u64(24);
    assert_eq!(
        encoded(|e| e.create_descriptor_pool(
            10,
            24,
            &[
                (descriptor::STORAGE_BUFFER, 2),
                (descriptor::UNIFORM_BUFFER, 1)
            ]
        )),
        pool.0
    );
    let set = Grammar::default()
        .command(Command::AllocateDescriptorSets, true)
        .u64(10)
        .u64(1)
        .structure(Structure::DescriptorSetAllocateInfo)
        .u64(24)
        .u32(1)
        .u64(1)
        .u64(21)
        .u64(1)
        .u64(25);
    assert_eq!(
        encoded(|e| e.allocate_descriptor_sets(10, (24, 21), 25)),
        set.0
    );
}

#[test]
fn a_set_s_descriptors_are_written_each_through_the_array_its_kind_reads() {
    let writes = [
        DescriptorWrite {
            binding: 0,
            kind: descriptor::STORAGE_BUFFER,
            written: Written::Buffer {
                buffer: 30,
                offset: 256,
                range: 64,
            },
        },
        DescriptorWrite {
            binding: 3,
            kind: descriptor::UNIFORM_BUFFER,
            written: Written::Buffer {
                buffer: 31,
                offset: 0,
                range: 16,
            },
        },
        DescriptorWrite {
            binding: 4,
            kind: descriptor::COMBINED_IMAGE_SAMPLER,
            written: Written::Image {
                sampler: 32,
                view: 33,
            },
        },
        DescriptorWrite {
            binding: 5,
            kind: descriptor::SAMPLER,
            written: Written::Image {
                sampler: 34,
                view: 0,
            },
        },
        DescriptorWrite {
            binding: 6,
            kind: descriptor::STORAGE_TEXEL_BUFFER,
            written: Written::Texels(35),
        },
    ];
    let written = writes
        .iter()
        .fold(
            Grammar::default()
                .command(Command::UpdateDescriptorSets, false)
                .u64(10)
                .u32(5)
                .u64(5),
            |grammar, write| {
                let grammar = grammar
                    .structure(Structure::WriteDescriptorSet)
                    .u64(25)
                    .u32(write.binding)
                    .u32(0)
                    .u32(1)
                    .u32(write.kind);
                match write.written {
                    Written::Image { sampler, view } => {
                        grammar.u64(1).u64(sampler).u64(view).i32(1).u64(0).u64(0)
                    }
                    Written::Buffer {
                        buffer,
                        offset,
                        range,
                    } => grammar
                        .u64(0)
                        .u64(1)
                        .u64(buffer)
                        .u64(offset)
                        .u64(range)
                        .u64(0),
                    Written::Texels(view) => grammar.u64(0).u64(0).u64(1).u64(view),
                }
            },
        )
        .u32(0)
        .u64(0);
    assert_eq!(
        encoded(|e| e.update_descriptor_sets(10, 25, &writes)),
        written.0
    );
}

#[test]
fn an_image_is_made_with_the_formats_its_views_read_it_as_asked_its_needs_and_bound() {
    let info = ImageInfo {
        flags: 8 | 16,
        kind: 1,
        format: 37,
        extent: [64, 64, 1],
        levels: 7,
        layers: 6,
        samples: 1,
        usage: 2 | 4,
        view_formats: Some([37, 43]),
    };
    let made = |grammar: Grammar, flags: u32| {
        grammar
            .u32(flags)
            .i32(1)
            .i32(37)
            .u32(64)
            .u32(64)
            .u32(1)
            .u32(7)
            .u32(6)
            .i32(1)
            .i32(0)
            .u32(2 | 4)
            .i32(0)
            .u32(0)
            .u64(0)
            .i32(0)
            .u64(0)
            .u64(1)
            .u64(40)
    };
    let listed = Grammar::default()
        .command(Command::CreateImage, true)
        .u64(10)
        .u64(1)
        .i32(Structure::ImageCreateInfo as i32)
        .u64(1)
        .i32(Structure::ImageFormatListCreateInfo as i32)
        .u64(0)
        .u32(2)
        .u64(2)
        .i32(37)
        .i32(43);
    assert_eq!(
        encoded(|e| e.create_image(10, 40, &info)),
        made(listed, 8 | 16).0
    );
    let alone = Grammar::default()
        .command(Command::CreateImage, true)
        .u64(10)
        .u64(1)
        .structure(Structure::ImageCreateInfo);
    let unlisted = ImageInfo {
        flags: 16,
        view_formats: None,
        ..info
    };
    assert_eq!(
        encoded(|e| e.create_image(10, 40, &unlisted)),
        made(alone, 16).0
    );
    let required = Grammar::default()
        .command(Command::GetImageMemoryRequirements2, true)
        .u64(10)
        .u64(1)
        .structure(Structure::ImageMemoryRequirementsInfo2)
        .u64(40)
        .u64(1)
        .structure(Structure::MemoryRequirements2);
    assert_eq!(
        encoded(|e| e.get_image_memory_requirements2(10, 40)),
        required.0
    );
    let bound = Grammar::default()
        .command(Command::BindImageMemory2, true)
        .u64(10)
        .u32(1)
        .u64(1)
        .structure(Structure::BindImageMemoryInfo)
        .u64(40)
        .u64(12)
        .u64(4096);
    assert_eq!(
        encoded(|e| e.bind_image_memory2(10, 40, (12, 4096))),
        bound.0
    );
}

#[test]
fn an_image_s_needs_are_asked_of_its_description_before_it_is_made() {
    let info = ImageInfo {
        flags: 16,
        kind: 1,
        format: 37,
        extent: [64, 64, 1],
        levels: 7,
        layers: 6,
        samples: 1,
        usage: 2 | 4,
        view_formats: Some([37, 43]),
    };
    let asked = Grammar::default()
        .command(Command::GetDeviceImageMemoryRequirements, true)
        .u64(10)
        .u64(1)
        .structure(Structure::DeviceImageMemoryRequirements)
        .u64(1)
        .i32(Structure::ImageCreateInfo as i32)
        .u64(1)
        .i32(Structure::ImageFormatListCreateInfo as i32)
        .u64(0)
        .u32(2)
        .u64(2)
        .i32(37)
        .i32(43)
        .u32(16)
        .i32(1)
        .i32(37)
        .u32(64)
        .u32(64)
        .u32(1)
        .u32(7)
        .u32(6)
        .i32(1)
        .i32(0)
        .u32(2 | 4)
        .i32(0)
        .u32(0)
        .u64(0)
        .i32(0)
        // No plane, then the requirements the reply fills.
        .i32(0)
        .u64(1)
        .structure(Structure::MemoryRequirements2);
    assert_eq!(
        encoded(|e| e.get_device_image_memory_requirements(10, &info)),
        asked.0
    );
}

#[test]
fn a_view_is_made_of_its_image_for_the_uses_it_names() {
    let view = ViewInfo {
        image: 40,
        kind: 3,
        format: 43,
        range: Subresources {
            aspects: 1,
            base_level: 1,
            levels: 2,
            base_layer: 0,
            layers: 6,
        },
        usage: 4,
    };
    let viewed = Grammar::default()
        .command(Command::CreateImageView, true)
        .u64(10)
        .u64(1)
        .i32(Structure::ImageViewCreateInfo as i32)
        .u64(1)
        .i32(Structure::ImageViewUsageCreateInfo as i32)
        .u64(0)
        .u32(4)
        .u32(0)
        .u64(40)
        .i32(3)
        .i32(43)
        .words(4)
        .u32(1)
        .u32(1)
        .u32(2)
        .u32(0)
        .u32(6)
        .u64(0)
        .u64(1)
        .u64(41);
    assert_eq!(encoded(|e| e.create_image_view(10, 41, &view)), viewed.0);
}

#[test]
fn a_sampler_is_made_anisotropic_and_comparing_only_where_it_is() {
    let sampler = SamplerInfo {
        filters: [1, 0, 1],
        address: [0, 2, 4],
        lod_bias: -1.5,
        anisotropy: Some(8.0),
        compare: Some(3),
        lod: [0.5, 12.0],
        border: 3,
        unnormalized: false,
    };
    let sampling = |grammar: Grammar, (anisotropy, compare): (Option<f32>, Option<i32>)| {
        grammar
            .f32(-1.5)
            .u32(u32::from(anisotropy.is_some()))
            .f32(anisotropy.unwrap_or(1.0))
            .u32(u32::from(compare.is_some()))
            .i32(compare.unwrap_or(0))
            .f32(0.5)
            .f32(12.0)
            .i32(3)
            .u32(0)
            .u64(0)
            .u64(1)
            .u64(42)
    };
    let head = Grammar::default()
        .command(Command::CreateSampler, true)
        .u64(10)
        .u64(1)
        .structure(Structure::SamplerCreateInfo)
        .u32(0)
        .i32(1)
        .i32(0)
        .i32(1)
        .i32(0)
        .i32(2)
        .i32(4);
    assert_eq!(
        encoded(|e| e.create_sampler(10, 42, &sampler)),
        sampling(head.clone(), (Some(8.0), Some(3))).0
    );
    let plain = SamplerInfo {
        anisotropy: None,
        compare: None,
        ..sampler
    };
    assert_eq!(
        encoded(|e| e.create_sampler(10, 42, &plain)),
        sampling(head, (None, None)).0,
        "no anisotropy past one texel and no comparison"
    );
}

#[test]
fn a_texel_buffer_is_made_and_each_image_object_let_go() {
    let texels = Grammar::default()
        .command(Command::CreateBufferView, true)
        .u64(10)
        .u64(1)
        .structure(Structure::BufferViewCreateInfo)
        .u32(0)
        .u64(13)
        .i32(100)
        .u64(256)
        .u64(1024)
        .u64(0)
        .u64(1)
        .u64(43);
    assert_eq!(
        encoded(|e| e.create_buffer_view(10, 43, (13, 100), (256, 1024))),
        texels.0
    );

    for (command, id) in [
        (Command::DestroyImage, 40),
        (Command::DestroyImageView, 41),
        (Command::DestroySampler, 42),
        (Command::DestroyBufferView, 43),
    ] {
        let destroyed = Grammar::default()
            .command(command, false)
            .u64(10)
            .u64(id)
            .u64(0);
        assert_eq!(encoded(|e| e.destroy(command, 10, id)), destroyed.0);
    }
}

/// Colour, level two, three layers from the first.
const LAYERS: Layers = Layers {
    aspect: 1,
    level: 2,
    base_layer: 1,
    count: 3,
};

#[test]
fn copies_between_buffers_and_images_are_recorded_row_by_row() {
    let layers = LAYERS;
    let region = BufferImageRegion {
        offset: 512,
        row_length: 64,
        image_height: 16,
        layers,
        at: [4, 8, 0],
        extent: [16, 8, 1],
    };
    let regions = |grammar: Grammar| {
        grammar
            .u32(1)
            .u64(1)
            .u64(512)
            .u32(64)
            .u32(16)
            .u32(1)
            .u32(2)
            .u32(1)
            .u32(3)
            .i32(4)
            .i32(8)
            .i32(0)
            .u32(16)
            .u32(8)
            .u32(1)
    };
    let into = Grammar::default()
        .command(Command::CmdCopyBufferToImage, false)
        .u64(20)
        .u64(13)
        .u64(40)
        .i32(1);
    assert_eq!(
        encoded(|e| e.cmd_copy_buffer_to_image(20, (13, 40), &[region])),
        regions(into).0
    );
    let out = Grammar::default()
        .command(Command::CmdCopyImageToBuffer, false)
        .u64(20)
        .u64(40)
        .i32(1)
        .u64(13);
    assert_eq!(
        encoded(|e| e.cmd_copy_image_to_buffer(20, (40, 13), &[region])),
        regions(out).0
    );
}

#[test]
fn a_copy_between_images_is_recorded() {
    let between = ImageRegion {
        src: (LAYERS, [1, 2, 3]),
        dst: (
            Layers {
                aspect: 2,
                level: 0,
                base_layer: 0,
                count: 1,
            },
            [0, 0, 5],
        ),
        extent: [8, 4, 2],
    };
    let copied = Grammar::default()
        .command(Command::CmdCopyImage, false)
        .u64(20)
        .u64(40)
        .i32(1)
        .u64(44)
        .i32(1)
        .u32(1)
        .u64(1)
        .u32(1)
        .u32(2)
        .u32(1)
        .u32(3)
        .i32(1)
        .i32(2)
        .i32(3)
        .u32(2)
        .u32(0)
        .u32(0)
        .u32(1)
        .i32(0)
        .i32(0)
        .i32(5)
        .u32(8)
        .u32(4)
        .u32(2);
    assert_eq!(
        encoded(|e| e.cmd_copy_image(20, (40, 44), &[between])),
        copied.0
    );
}

#[test]
fn clears_and_an_image_s_first_layout_are_recorded() {
    let whole = Subresources {
        aspects: 1,
        base_level: 0,
        levels: 7,
        base_layer: 0,
        layers: 6,
    };
    let range = |grammar: Grammar, aspects: u32| grammar.u32(aspects).u32(0).u32(7).u32(0).u32(6);
    let colour = Grammar::default()
        .command(Command::CmdClearColorImage, false)
        .u64(20)
        .u64(40)
        .i32(1)
        .u64(1)
        .u32(2)
        .u64(4)
        .u32(1)
        .u32(2)
        .u32(3)
        .u32(4)
        .u32(1)
        .u64(1);
    assert_eq!(
        encoded(|e| e.cmd_clear_color_image(20, 40, (2, [1, 2, 3, 4]), &[whole])),
        range(colour, 1).0
    );
    let depth = Grammar::default()
        .command(Command::CmdClearDepthStencilImage, false)
        .u64(20)
        .u64(45)
        .i32(1)
        .u64(1)
        .f32(0.25)
        .u32(7)
        .u32(1)
        .u64(1);
    let planes = Subresources {
        aspects: 2 | 4,
        ..whole
    };
    assert_eq!(
        encoded(|e| e.cmd_clear_depth_stencil_image(20, 45, (0.25, 7), &[planes])),
        range(depth, 2 | 4).0
    );

    let kept = Grammar::default()
        .command(Command::CmdPipelineBarrier, false)
        .u64(20)
        .u32(1)
        .u32(0x1_0000)
        .u32(0)
        .u32(0)
        .u64(0)
        .u32(0)
        .u64(0)
        .u32(1)
        .u64(1)
        .structure(Structure::ImageMemoryBarrier)
        .u32(0)
        .u32(0x1_8000)
        .i32(0)
        .i32(1)
        .u32(u32::MAX)
        .u32(u32::MAX)
        .u64(40);
    assert_eq!(
        encoded(|e| e.cmd_image_kept(20, 40, &whole)),
        range(kept, 1).0,
        "from undefined to general, before anything reads or writes it"
    );
}

#[test]
fn a_format_is_asked_what_it_offers_and_an_image_of_it_how_large_it_may_be() {
    let asked = Grammar::default()
        .command(Command::GetPhysicalDeviceFormatProperties2, true)
        .u64(2)
        .i32(130)
        .u64(1);
    assert_eq!(
        encoded(|e| e.get_physical_device_format_properties2(2, 130, false)),
        asked.clone().structure(Structure::FormatProperties2).0
    );
    assert_eq!(
        encoded(|e| e.get_physical_device_format_properties2(2, 130, true)),
        asked
            .i32(Structure::FormatProperties2 as i32)
            .u64(1)
            .structure(Structure::FormatProperties3)
            .0,
        "Vulkan 1.3's format features chained, for the renderer to fill"
    );
    let query = FormatQuery {
        format: 37,
        kind: 1,
        usage: 1 | 2 | 4 | 8,
        flags: 8 | 16,
        view_formats: None,
    };
    let asked = |grammar: Grammar| {
        grammar
            .i32(37)
            .i32(1)
            .i32(0)
            .u32(1 | 2 | 4 | 8)
            .u32(8 | 16)
            .u64(1)
            .structure(Structure::ImageFormatProperties2)
    };
    let most = asked(
        Grammar::default()
            .command(Command::GetPhysicalDeviceImageFormatProperties2, true)
            .u64(2)
            .u64(1)
            .structure(Structure::PhysicalDeviceImageFormatInfo2),
    );
    assert_eq!(
        encoded(|e| e.get_physical_device_image_format_properties2(2, &query)),
        most.0
    );
    let listed = FormatQuery {
        view_formats: Some([37, 43]),
        ..query
    };
    let most = asked(
        Grammar::default()
            .command(Command::GetPhysicalDeviceImageFormatProperties2, true)
            .u64(2)
            .u64(1)
            .i32(Structure::PhysicalDeviceImageFormatInfo2 as i32)
            .u64(1)
            .i32(Structure::ImageFormatListCreateInfo as i32)
            .u64(0)
            .u32(2)
            .u64(2)
            .i32(37)
            .i32(43),
    );
    assert_eq!(
        encoded(|e| e.get_physical_device_image_format_properties2(2, &listed)),
        most.0,
        "the formats the image's views read it as, listed as its making lists them"
    );
}

#[test]
fn a_dispatch_and_what_it_binds_are_recorded() {
    let pipeline = Grammar::default()
        .command(Command::CmdBindPipeline, false)
        .u64(15)
        .u32(BIND_POINT_COMPUTE)
        .u64(26);
    assert_eq!(
        encoded(|e| e.cmd_bind_pipeline(15, BIND_POINT_COMPUTE, 26)),
        pipeline.0
    );
    let sets = Grammar::default()
        .command(Command::CmdBindDescriptorSets, false)
        .u64(15)
        .u32(BIND_POINT_COMPUTE)
        .u64(22)
        .u32(0)
        .u32(2)
        .u64(2)
        .u64(25)
        .u64(27)
        .u32(0)
        .u64(0);
    assert_eq!(
        encoded(|e| e.cmd_bind_sets(15, (BIND_POINT_COMPUTE, 22), &[25, 27])),
        sets.0
    );
    let push = Grammar::default()
        .command(Command::CmdPushConstants, false)
        .u64(15)
        .u64(22)
        .u32(0x20)
        .u32(0)
        .u32(8)
        .u64(8)
        .u32(1)
        .u32(2);
    assert_eq!(
        encoded(|e| e.cmd_push_constants(15, (22, stage::COMPUTE), &[1, 2])),
        push.0
    );
    let direct = Grammar::default()
        .command(Command::CmdDispatch, false)
        .u64(15)
        .u32(4)
        .u32(5)
        .u32(6);
    assert_eq!(encoded(|e| e.cmd_dispatch(15, [4, 5, 6])), direct.0);
    let indirect = Grammar::default()
        .command(Command::CmdDispatchIndirect, false)
        .u64(15)
        .u64(30)
        .u64(128);
    assert_eq!(
        encoded(|e| e.cmd_dispatch_indirect(15, 30, 128)),
        indirect.0
    );
}

#[test]
fn every_stream_is_whole_words() {
    let mut encoder = Encoder::default();
    encoder.create_instance(1).expect("encoded");
    encoder.cmd_fill_buffer(2, 3, (0, 4), 9).expect("encoded");
    encoder
        .cmd_update_buffer(2, 3, 0, &[0; 12])
        .expect("encoded");
    assert!(encoder.as_bytes().len().is_multiple_of(4));
    encoder.clear();
    assert!(encoder.as_bytes().is_empty());
}

#[test]
fn a_physical_device_s_properties_are_read_whole() {
    let bytes = properties_reply(Some(ROBUST), API_VERSION, limits).0;
    assert_eq!(bytes.len(), 972, "the reply the renderer writes");
    let mut reader = Reader::new(&bytes);
    let properties = reader.properties(true).expect("read");
    assert_eq!(properties.robust_alignments, Some(ROBUST));
    assert_eq!(properties.subgroup_size, 32);
    assert_eq!(properties.max_allocation_size, 1 << 31);
    assert_eq!(properties.api_version, API_VERSION);
    assert_eq!(properties.vendor, 0x10005);
    assert_eq!(&properties.name[..8], b"llvmpipe");
    let limits = properties.limits;
    assert_eq!(limits.image_2d, 16384);
    assert_eq!(limits.buffer_image_granularity, 1);
    assert_eq!(limits.texel_buffer_elements, 1 << 27);
    assert_eq!(limits.storage_buffer_range, 1 << 30);
    assert_eq!(limits.push_constants, 256);
    assert_eq!(limits.bound_sets, 8);
    assert_eq!(limits.set_descriptors, [1, 2, 4, 6, 7]);
    assert_eq!(limits.vertex_attributes, 32);
    assert_eq!(limits.vertex_output_components, 128);
    assert_eq!(limits.fragment_input_components, 128);
    assert_eq!(limits.workgroup_memory, 32768);
    assert_eq!(limits.workgroup_count, [65535; 3]);
    assert_eq!(limits.workgroup_invocations, 1024);
    assert_eq!(limits.workgroup_size, [1024, 1024, 64]);
    assert_eq!(limits.draw_indirect_count, u32::MAX);
    assert_eq!(limits.viewport_dimensions, [16384, 16384]);
    let bits = |pair: [f32; 2]| pair.map(f32::to_bits);
    assert_eq!(bits(limits.viewport_bounds), bits([-32768.0, 32767.0]));
    assert_eq!(limits.uniform_offset_alignment, 64);
    assert_eq!(limits.storage_offset_alignment, 16);
    assert_eq!(limits.colour_attachments, 8);
    assert_eq!(bits(limits.point_sizes), bits([1.0, 255.0]));
    assert_eq!(bits(limits.line_widths), bits([1.0, 8.0]));
    let unrobust = properties_reply(None, API_VERSION, super::testing::limits).0;
    assert_eq!(unrobust.len(), 944);
    let read = Reader::new(&unrobust).properties(false).expect("read");
    assert_eq!(read.robust_alignments, None);
    assert_eq!(read.limits, properties.limits);
    assert!(
        Reader::new(&unrobust).properties(true).is_err(),
        "the robust accesses' properties asked and missing"
    );
    assert!(
        Reader::new(&bytes).properties(false).is_err(),
        "a chain past what was asked"
    );
    let mut empty = Reader::new(&[]);
    assert_eq!(
        empty.result_of(Command::QueueSubmit),
        Err(DriverError::DeviceFault)
    );
}

#[test]
fn a_reply_off_its_grammar_is_a_device_fault() {
    let good = properties_reply(Some(ROBUST), API_VERSION, limits).0;
    let fault = |bytes: &[u8]| Reader::new(bytes).properties(true).err();
    assert_eq!(
        fault(&good[..good.len() - 1]),
        Some(DriverError::DeviceFault),
        "cut short"
    );
    let mut other = good.clone();
    other[0] = Command::GetPhysicalDeviceMemoryProperties2 as u8;
    assert_eq!(
        fault(&other),
        Some(DriverError::DeviceFault),
        "another command's"
    );
    // The robust accesses' structure's chain, the innermost asked.
    let mut chained = good.clone();
    chained[52] = 1;
    assert_eq!(
        fault(&chained),
        Some(DriverError::DeviceFault),
        "a chain past what the driver asked"
    );
    let mut unchained = good.clone();
    unchained[16] = 0;
    assert_eq!(
        fault(&unchained),
        Some(DriverError::DeviceFault),
        "no chain where one was asked"
    );
    // The name's array size.
    let mut renamed = good;
    renamed[124] = 255;
    assert_eq!(
        fault(&renamed),
        Some(DriverError::DeviceFault),
        "a name of another length"
    );
}

#[test]
fn replies_are_read_one_after_another() {
    let bytes = Grammar::default()
        .i32(Command::CreateInstance as i32)
        .i32(0)
        .u64(1)
        .u64(7)
        .i32(Command::EnumeratePhysicalDevices as i32)
        .i32(0)
        .u64(1)
        .u32(1)
        .u64(1)
        .u64(8)
        .i32(Command::BindBufferMemory2 as i32)
        .i32(-2)
        .0;
    let mut reader = Reader::new(&bytes);
    assert_eq!(
        reader.made(Command::CreateInstance, 7),
        Ok(VkResult::Success)
    );
    let (result, count, ids) = reader.physical_devices(4).expect("read");
    assert_eq!(
        (result, count, ids.as_slice()),
        (VkResult::Success, 1, &[8u64][..])
    );
    assert_eq!(
        reader.result_of(Command::BindBufferMemory2),
        Ok(VkResult::Failed(-2))
    );
    let mut misnamed = Reader::new(&bytes);
    assert_eq!(
        misnamed.made(Command::CreateInstance, 9),
        Err(DriverError::DeviceFault),
        "an object the driver did not name"
    );
}

#[test]
fn more_physical_devices_named_than_asked_or_counted_is_a_device_fault() {
    let reply = |count: u32, named: &[u64]| {
        named
            .iter()
            .fold(
                Grammar::default()
                    .i32(Command::EnumeratePhysicalDevices as i32)
                    .i32(0)
                    .u64(1)
                    .u32(count)
                    .u64(named.len() as u64),
                |grammar, &id| grammar.u64(id),
            )
            .0
    };
    assert!(Reader::new(&reply(2, &[8, 9])).physical_devices(2).is_ok());
    assert!(Reader::new(&reply(2, &[8, 9])).physical_devices(1).is_err());
    assert!(Reader::new(&reply(1, &[8, 9])).physical_devices(2).is_err());
    let (_, counted, ids) = Reader::new(&reply(3, &[]))
        .physical_devices(0)
        .expect("counted");
    assert_eq!((counted, ids.len()), (3, 0));
}

#[test]
fn queue_families_are_read() {
    let families = Grammar::default()
        .i32(Command::GetPhysicalDeviceQueueFamilyProperties2 as i32)
        .u64(1)
        .u32(2)
        .u64(2)
        .structure(Structure::QueueFamilyProperties2)
        .u32(7)
        .u32(1)
        .words(4)
        .structure(Structure::QueueFamilyProperties2)
        .u32(4)
        .u32(2)
        .words(4)
        .0;
    let (count, read) = Reader::new(&families).queue_families(2).expect("read");
    assert_eq!(count, 2);
    assert_eq!(
        read.as_slice(),
        &[
            HostQueueFamily {
                flags: 7,
                queues: 1
            },
            HostQueueFamily {
                flags: 4,
                queues: 2
            }
        ]
    );
    assert!(
        Reader::new(&families).queue_families(1).is_err(),
        "more than asked"
    );
}

#[test]
fn memory_requirements_and_objects_made_as_an_array_are_read() {
    let memory = (0..MAX_MEMORY_HEAPS)
        .fold(
            (0..MAX_MEMORY_TYPES)
                .fold(
                    Grammar::default()
                        .i32(Command::GetPhysicalDeviceMemoryProperties2 as i32)
                        .u64(1)
                        .structure(Structure::PhysicalDeviceMemoryProperties2)
                        .u32(2)
                        .u64(MAX_MEMORY_TYPES as u64),
                    |grammar, index| match index {
                        0 => grammar.u32(1).u32(0),
                        1 => grammar.u32(6).u32(1),
                        _ => grammar.u32(0).u32(0),
                    },
                )
                .u32(2)
                .u64(MAX_MEMORY_HEAPS as u64),
            |grammar, index| match index {
                0 => grammar.u64(1 << 30).u32(1),
                1 => grammar.u64(1 << 28).u32(0),
                _ => grammar.u64(0).u32(0),
            },
        )
        .0;
    assert_eq!(memory.len(), 496, "the reply the renderer writes");
    let read = Reader::new(&memory).memory_properties().expect("read");
    assert_eq!(
        read.types.as_slice(),
        &[
            HostMemoryType { flags: 1, heap: 0 },
            HostMemoryType { flags: 6, heap: 1 }
        ]
    );
    assert_eq!(
        read.heaps.as_slice(),
        &[
            HostMemoryHeap {
                size: 1 << 30,
                flags: 1
            },
            HostMemoryHeap {
                size: 1 << 28,
                flags: 0
            }
        ]
    );
    let mut overcounted = memory.clone();
    overcounted[24] = 33;
    assert!(Reader::new(&overcounted).memory_properties().is_err());

    let requirements = Grammar::default()
        .i32(Command::GetBufferMemoryRequirements2 as i32)
        .u64(1)
        .structure(Structure::MemoryRequirements2)
        .u64(8192)
        .u64(256)
        .u32(3)
        .0;
    assert_eq!(requirements.len(), 44);
    let read = Reader::new(&requirements)
        .memory_requirements(Command::GetBufferMemoryRequirements2)
        .expect("read");
    assert_eq!((read.size, read.alignment, read.type_bits), (8192, 256, 3));

    let buffers = Grammar::default()
        .i32(Command::AllocateCommandBuffers as i32)
        .i32(0)
        .u64(2)
        .u64(15)
        .u64(16)
        .0;
    let allocated = |bytes: &[u8], ids: &[u64]| {
        Reader::new(bytes).allocated(Command::AllocateCommandBuffers, ids)
    };
    assert_eq!(allocated(&buffers, &[15, 16]), Ok(VkResult::Success));
    assert!(allocated(&buffers, &[15]).is_err(), "more than asked");
    assert!(allocated(&buffers, &[15, 17]).is_err(), "another's id");
    assert!(
        Reader::new(&buffers)
            .allocated(Command::AllocateDescriptorSets, &[15, 16])
            .is_err(),
        "another command's"
    );
    let failed = Grammar::default()
        .i32(Command::CreateComputePipelines as i32)
        .i32(-3)
        .u64(1)
        .u64(20)
        .0;
    assert_eq!(
        Reader::new(&failed).allocated(Command::CreateComputePipelines, &[20]),
        Ok(VkResult::Failed(-3)),
        "an answer the driver reads, whatever it says"
    );
}

#[test]
fn an_image_s_memory_requirements_are_read_as_its_command_s() {
    let image = Grammar::default()
        .i32(Command::GetImageMemoryRequirements2 as i32)
        .u64(1)
        .structure(Structure::MemoryRequirements2)
        .u64(8192)
        .u64(512)
        .u32(3)
        .0;
    let read = Reader::new(&image)
        .memory_requirements(Command::GetImageMemoryRequirements2)
        .expect("read");
    assert_eq!((read.size, read.alignment, read.type_bits), (8192, 512, 3));
    assert!(
        Reader::new(&image)
            .memory_requirements(Command::GetBufferMemoryRequirements2)
            .is_err(),
        "another command's"
    );
}

#[test]
fn a_format_s_offers_and_the_most_an_image_of_it_may_be_are_read() {
    let offered = HostFormatFeatures {
        optimal: 0xd203,
        buffer: 0x18,
    };
    let offers = Grammar::default()
        .i32(Command::GetPhysicalDeviceFormatProperties2 as i32)
        .u64(1)
        .structure(Structure::FormatProperties2)
        .u32(0x7)
        .u32(0xd203)
        .u32(0x18)
        .0;
    assert_eq!(offers.len(), 36, "the reply the renderer writes");
    assert_eq!(
        Reader::new(&offers).format_properties(false),
        Ok(offered),
        "an image laid out linearly is never made"
    );
    let mut fuzzed = format_reply(offered, false).0;
    fuzzed[24..28].copy_from_slice(&7u32.to_le_bytes());
    assert_eq!(fuzzed, offers, "the harness's reply the renderer's");
    assert!(Reader::new(&offers[..32]).format_properties(false).is_err());
    assert!(
        Reader::new(&offers).format_properties(true).is_err(),
        "the 1.3 features asked for and not given"
    );

    let wide = HostFormatFeatures {
        optimal: 0x3_8000_d203,
        buffer: 0x1_8000_0018,
    };
    let stated = Grammar::default()
        .i32(Command::GetPhysicalDeviceFormatProperties2 as i32)
        .u64(1)
        .i32(Structure::FormatProperties2 as i32)
        .u64(1)
        .structure(Structure::FormatProperties3)
        .u64(0x7)
        .u64(0x3_8000_d203)
        .u64(0x1_8000_0018)
        .u32(0x7)
        .u32(0xd203)
        .u32(0x18)
        .0;
    assert_eq!(stated.len(), 72, "the reply the renderer writes");
    assert_eq!(
        Reader::new(&stated).format_properties(true),
        Ok(wide),
        "the 1.3 features read, the older ones they include passed over"
    );
    let mut fuzzed = format_reply(wide, true).0;
    fuzzed[36..44].copy_from_slice(&7u64.to_le_bytes());
    fuzzed[60..64].copy_from_slice(&7u32.to_le_bytes());
    assert_eq!(fuzzed, stated, "the harness's reply the renderer's");
    assert!(Reader::new(&stated[..68]).format_properties(true).is_err());
    assert!(
        Reader::new(&stated).format_properties(false).is_err(),
        "a chain not asked for"
    );

    let most = HostImageLimits {
        extent: [16384, 16384, 1],
        levels: 15,
        layers: 2048,
        samples: 0x5,
    };
    let answered = |result: i32| {
        Grammar::default()
            .i32(Command::GetPhysicalDeviceImageFormatProperties2 as i32)
            .i32(result)
            .u64(1)
            .structure(Structure::ImageFormatProperties2)
            .u32(16384)
            .u32(16384)
            .u32(1)
            .u32(15)
            .u32(2048)
            .u32(0x5)
            .u64(1 << 40)
            .0
    };
    let read = |bytes: &[u8]| Reader::new(bytes).image_format_properties();
    assert_eq!(answered(0).len(), 60, "the reply the renderer writes");
    assert_eq!(read(&answered(0)), Ok(Some(most)));
    assert_eq!(
        read(&answered(-11)),
        Ok(None),
        "a format refused for the kind, usage or flags asked"
    );
    for exhausted in [-1, -2] {
        assert_eq!(
            read(&answered(exhausted)),
            Err(DriverError::OutOfMemory),
            "memory the host lacked now, not a format it refuses"
        );
    }
    assert!(read(&answered(5)).is_err(), "no part of an answer is done");
    assert!(read(&answered(0)[..56]).is_err());
    let mut fuzzed = image_format_reply(0, most).0;
    fuzzed[52..].copy_from_slice(&(1u64 << 40).to_le_bytes());
    assert_eq!(fuzzed, answered(0), "the harness's reply the renderer's");
}

#[test]
fn a_physical_device_s_extensions_are_counted_then_read_by_name() {
    let counted = Grammar::default()
        .i32(Command::EnumerateDeviceExtensionProperties as i32)
        .i32(0)
        .u64(1)
        .u32(40)
        .u64(0)
        .0;
    assert_eq!(
        Reader::new(&counted).extensions(0),
        Ok((VkResult::Success, 40, HostExtensions::NONE))
    );
    let names = [
        "VK_KHR_swapchain",
        extension::ROBUSTNESS2_EXT,
        extension::SHADER_FLOAT16_INT8,
    ];
    let reply = extensions_reply(&names).0;
    let offered = HostExtensions::ROBUSTNESS2_EXT.with(HostExtensions::SHADER_FLOAT16_INT8);
    assert_eq!(
        Reader::new(&reply).extensions(3),
        Ok((VkResult::Success, 3, offered))
    );
    assert!(
        Reader::new(&reply).extensions(2).is_err(),
        "more than asked"
    );
    let mut unterminated = reply.clone();
    let first = 4 + 4 + 8 + 4 + 8 + 8;
    unterminated[first..first + EXTENSION_NAME_LEN].fill(b'V');
    assert!(
        Reader::new(&unterminated).extensions(3).is_err(),
        "a name with no end"
    );
    let mut resized = reply;
    resized[first - 8] = 255;
    assert!(
        Reader::new(&resized).extensions(3).is_err(),
        "a name of another length"
    );
    let prefixed = extensions_reply(&["VK_KHR_robustness2_and_more"]).0;
    assert_eq!(
        Reader::new(&prefixed).extensions(1),
        Ok((VkResult::Success, 1, HostExtensions::NONE)),
        "a name is matched whole"
    );
}

#[test]
fn a_physical_device_s_features_are_read_in_the_chain_s_order() {
    let chain = [
        Structure::PhysicalDeviceRobustness2Features,
        Structure::PhysicalDevice16BitStorageFeatures,
    ];
    let mut core = [false; CORE_FEATURES];
    core[39] = true;
    let mut chained = tairix_inline::ArrayVec::new();
    for members in [&[true, false, true][..], &[false, true, true, false][..]] {
        let mut read = tairix_inline::ArrayVec::new();
        read.try_extend_from_slice(members).expect("room");
        chained.try_push(read).expect("room");
    }
    let features = HostFeatures { core, chained };
    let bytes = features_reply(&chain, &features).0;
    assert_eq!(Reader::new(&bytes).features(&chain), Ok(features.clone()));
    let swapped = [chain[1], chain[0]];
    assert!(
        Reader::new(&bytes).features(&swapped).is_err(),
        "another order than asked"
    );
    assert!(
        Reader::new(&bytes).features(&chain[..1]).is_err(),
        "a chain past what was asked"
    );
    let mut unboolean = bytes;
    let last = unboolean.len() - 4;
    unboolean[last] = 2;
    assert!(
        Reader::new(&unboolean).features(&chain).is_err(),
        "a member neither true nor false"
    );
}

#[test]
fn a_result_names_success_failure_or_a_part_done() {
    assert_eq!(VkResult::of(0), VkResult::Success);
    assert_eq!(VkResult::of(-4), VkResult::Failed(-4));
    assert_eq!(VkResult::of(5), VkResult::Partial(5));
}

/// The front face's stencil state [`every_state`] names.
const STENCIL_FRONT: StencilInfo = StencilInfo {
    ops: [1, 2, 3],
    compare: 4,
    compare_mask: 0xf0,
    write_mask: 0x0f,
};

/// A render pipeline naming every state: two stages, two vertex buffers, a
/// strip restarting, lines asked for as rectangles, a biased depth, four
/// samples covered by alpha, depth and stencil tests, a logic operation,
/// and two targets, the first blended.
fn every_state() -> GraphicsInfo<'static> {
    GraphicsInfo {
        stages: &[
            StageInfo {
                stage: 1,
                module: 20,
                entry: "main",
            },
            StageInfo {
                stage: 0x10,
                module: 21,
                entry: "paint",
            },
        ],
        bindings: &[(32, 0), (16, 1)],
        attributes: &[
            AttributeInfo {
                location: 0,
                binding: 0,
                format: 109,
                offset: 0,
            },
            AttributeInfo {
                location: 3,
                binding: 1,
                format: 98,
                offset: 4,
            },
        ],
        topology: 4,
        restart: true,
        raster: RasterInfo {
            depth_clamp: true,
            cull: 2,
            front_face: 1,
            bias: Some([1.0, 0.5, 2.0]),
            line_width: 3.0,
            rectangular_lines: true,
        },
        samples: 4,
        sample_mask: 0xd,
        alpha_to_coverage: true,
        depth: Some((1, true)),
        stencil: Some([STENCIL_FRONT, StencilInfo::default()]),
        logic_op: Some(3),
        targets: &[
            TargetInfo {
                blend: Some([6, 7, 0, 1, 0, 4]),
                write_mask: 0xf,
            },
            TargetInfo {
                blend: None,
                write_mask: 0x3,
            },
        ],
        colour_formats: &[37, 109],
        depth_format: 130,
        stencil_format: 130,
    }
}

/// A stage as the renderer reads it.
fn stage_grammar(grammar: Grammar, flag: u32, module: u64, entry: &str) -> Grammar {
    grammar
        .structure(Structure::PipelineShaderStageCreateInfo)
        .u32(0)
        .u32(flag)
        .u64(module)
        .string(entry)
        .u64(0)
}

/// A face's stencil state as the renderer reads it, its reference each
/// draw's.
fn face_grammar(grammar: Grammar, face: StencilInfo) -> Grammar {
    face.ops
        .iter()
        .fold(grammar, |grammar, &op| grammar.u32(op))
        .i32(face.compare)
        .u32(face.compare_mask)
        .u32(face.write_mask)
        .u32(0)
}

/// [`every_state`]'s command up to its stages, its stages, and its vertex
/// input, assembly and viewport states.
fn every_state_to_viewport() -> Grammar {
    let head = Grammar::default()
        .command(Command::CreateGraphicsPipelines, true)
        .u64(10)
        .u64(0)
        .u32(1)
        .u64(1)
        .i32(Structure::GraphicsPipelineCreateInfo as i32)
        .u64(1)
        .i32(Structure::PipelineRenderingCreateInfo as i32)
        .u64(0)
        .u32(0)
        .u32(2)
        .u64(2)
        .i32(37)
        .i32(109)
        .i32(130)
        .i32(130)
        .u32(0)
        .u32(2)
        .u64(2);
    stage_grammar(stage_grammar(head, 1, 20, "main"), 0x10, 21, "paint")
        .u64(1)
        .structure(Structure::PipelineVertexInputStateCreateInfo)
        .u32(0)
        .u32(2)
        .u64(2)
        .u32(0)
        .u32(32)
        .u32(0)
        .u32(1)
        .u32(16)
        .u32(1)
        .u32(2)
        .u64(2)
        .u32(0)
        .u32(0)
        .i32(109)
        .u32(0)
        .u32(3)
        .u32(1)
        .i32(98)
        .u32(4)
        .u64(1)
        .structure(Structure::PipelineInputAssemblyStateCreateInfo)
        .u32(0)
        .u32(4)
        .u32(1)
        .u64(0)
        .u64(1)
        .structure(Structure::PipelineViewportStateCreateInfo)
        .u32(0)
        .u32(1)
        .u64(0)
        .u32(1)
        .u64(0)
}

/// [`every_state`]'s rasterization, multisample and depth-stencil states,
/// after `grammar`.
fn every_state_tests(grammar: Grammar) -> Grammar {
    let tested = grammar
        .u64(1)
        .i32(Structure::PipelineRasterizationStateCreateInfo as i32)
        .u64(1)
        .i32(Structure::PipelineRasterizationLineStateCreateInfo as i32)
        .u64(0)
        .u32(1)
        .u32(0)
        .u32(0)
        .u32(0)
        .u32(0)
        .u32(1)
        .u32(0)
        .u32(0)
        .u32(2)
        .u32(1)
        .u32(1)
        .f32(1.0)
        .f32(0.5)
        .f32(2.0)
        .f32(3.0)
        .u64(1)
        .structure(Structure::PipelineMultisampleStateCreateInfo)
        .u32(0)
        .u32(4)
        .u32(0)
        .f32(0.0)
        .u64(1)
        .u32(0xd)
        .u32(1)
        .u32(0)
        .u64(1)
        .structure(Structure::PipelineDepthStencilStateCreateInfo)
        .u32(0)
        .u32(1)
        .u32(1)
        .i32(1)
        .u32(0)
        .u32(1);
    face_grammar(face_grammar(tested, STENCIL_FRONT), StencilInfo::default())
        .f32(0.0)
        .f32(1.0)
}

/// [`every_state`]'s colour blend and dynamic states and its layout, after
/// `grammar`, with neither render pass object nor a pipeline it derives
/// from.
fn every_state_to_end(grammar: Grammar) -> Grammar {
    grammar
        .u64(1)
        .structure(Structure::PipelineColorBlendStateCreateInfo)
        .u32(0)
        .u32(1)
        .u32(3)
        .u32(2)
        .u64(2)
        .u32(1)
        .u32(6)
        .u32(7)
        .u32(0)
        .u32(1)
        .u32(0)
        .u32(4)
        .u32(0xf)
        .u32(0)
        .words(6)
        .u32(0x3)
        .u64(4)
        .words(4)
        .u64(1)
        .structure(Structure::PipelineDynamicStateCreateInfo)
        .u32(0)
        .u32(4)
        .u64(4)
        .i32(0)
        .i32(1)
        .i32(4)
        .i32(8)
        .u64(22)
        .u64(0)
        .u32(0)
        .u64(0)
        .i32(-1)
        .u64(0)
        .u64(1)
        .u64(26)
}

/// A render pipeline as the renderer reads it: its rendering formats
/// chained, then its stages, each state in turn, its dynamic states and its
/// layout, with neither render pass object nor a pipeline it derives from.
#[test]
fn a_render_pipeline_names_its_stages_state_and_target_formats() {
    let info = every_state();
    let expected = every_state_to_end(every_state_tests(every_state_to_viewport()));
    assert_eq!(
        encoded(|e| e.create_graphics_pipelines(10, 26, (&info, 22))),
        expected.0
    );

    let strict = GraphicsInfo {
        raster: RasterInfo {
            rectangular_lines: false,
            bias: None,
            ..info.raster
        },
        depth: None,
        stencil: None,
        logic_op: None,
        ..info
    };
    let encoded_strict = encoded(|e| e.create_graphics_pipelines(10, 26, (&strict, 22)));
    let raster_at = encoded_strict
        .windows(4)
        .position(|window| {
            window == (Structure::PipelineRasterizationStateCreateInfo as i32).to_le_bytes()
        })
        .expect("the rasterization state");
    assert_eq!(
        &encoded_strict[raster_at + 4..raster_at + 12],
        &0u64.to_le_bytes(),
        "a host's strict lines need no line state"
    );
}

/// A rendering instance as the renderer reads it: each attachment its view
/// in the general layout, its resolve, its load and store and its clear,
/// tagged by kind; a depth and a stencil attachment each where there is one.
#[test]
fn a_rendering_instance_names_each_attachment_s_view_resolve_load_and_clear() {
    let attachment =
        |grammar: Grammar, view: u64, resolve: Option<u64>, clear: Option<HostClear>| {
            let grammar = grammar
                .structure(Structure::RenderingAttachmentInfo)
                .u64(view)
                .i32(1);
            let grammar = match resolve {
                Some(into) => grammar.u32(2).u64(into).i32(1),
                None => grammar.u32(0).u64(0).i32(0),
            };
            let grammar = grammar.i32(i32::from(clear.is_some())).i32(0);
            match clear.unwrap_or(HostClear::Colour(0, [0; 4])) {
                HostClear::Colour(kind, bits) => bits
                    .iter()
                    .fold(grammar.u32(0).u32(kind).u64(4), |grammar, &bit| {
                        grammar.u32(bit)
                    }),
                HostClear::DepthStencil(depth, stencil) => grammar.u32(1).f32(depth).u32(stencil),
            }
        };
    let cleared = HostClear::Colour(2, [1, 2, 3, 4]);
    let depth = HostClear::DepthStencil(0.5, 7);
    let mut expected = Grammar::default()
        .command(Command::CmdBeginRendering, false)
        .u64(15)
        .u64(1)
        .structure(Structure::RenderingInfo)
        .u32(0)
        .i32(2)
        .i32(3)
        .u32(4)
        .u32(5)
        .u32(1)
        .u32(0)
        .u32(2)
        .u64(2);
    expected = attachment(expected, 30, None, Some(cleared));
    expected = attachment(expected, 31, Some(32), None).u64(1);
    expected = attachment(expected, 33, None, Some(depth)).u64(0);
    let colour = [
        AttachmentInfo {
            view: 30,
            resolve: None,
            clear: Some(cleared),
        },
        AttachmentInfo {
            view: 31,
            resolve: Some(32),
            clear: None,
        },
    ];
    let depth_attachment = AttachmentInfo {
        view: 33,
        resolve: None,
        clear: Some(depth),
    };
    assert_eq!(
        encoded(|e| e.cmd_begin_rendering(
            15,
            [2, 3, 4, 5],
            (&colour, Some(depth_attachment), None)
        )),
        expected.0
    );
    assert_eq!(
        encoded(|e| e.cmd_end_rendering(15)),
        Grammar::default()
            .command(Command::CmdEndRendering, false)
            .u64(15)
            .0
    );
    assert_eq!(
        Encoder::default().cmd_begin_rendering(15, [1 << 31, 0, 1, 1], (&[], None, None)),
        Err(DriverError::OutOfRange),
        "a corner past what the renderer reads is refused"
    );
}

/// That each stream of `cases` is the grammar it is held to.
fn held_to_the_grammar(cases: impl IntoIterator<Item = (Vec<u8>, Grammar)>) {
    for (at, (encoded, expected)) in cases.into_iter().enumerate() {
        assert_eq!(encoded, expected.0, "case {at}");
    }
}

/// A draw's binds and its state, each as the renderer reads it.
#[test]
fn a_draw_s_binds_and_state_are_held_to_the_renderer_s_grammar() {
    held_to_the_grammar([
        (
            encoded(|e| e.cmd_bind_pipeline(15, BIND_POINT_GRAPHICS, 26)),
            Grammar::default()
                .command(Command::CmdBindPipeline, false)
                .u64(15)
                .u32(0)
                .u64(26),
        ),
        (
            encoded(|e| e.cmd_bind_vertex_buffers(15, &[[40, 8, 64], [41, 0, 16]])),
            Grammar::default()
                .command(Command::CmdBindVertexBuffers2, false)
                .u64(15)
                .u32(0)
                .u32(2)
                .u64(2)
                .u64(40)
                .u64(41)
                .u64(2)
                .u64(8)
                .u64(0)
                .u64(2)
                .u64(64)
                .u64(16)
                .u64(0),
        ),
        (
            encoded(|e| e.cmd_bind_index_buffer(15, [40, 4, 60], true)),
            Grammar::default()
                .command(Command::CmdBindIndexBuffer2, false)
                .u64(15)
                .u64(40)
                .u64(4)
                .u64(60)
                .i32(1),
        ),
        (
            encoded(|e| e.cmd_set_viewport(15, [0.0, 8.0, 16.0, -8.0, 0.0, 1.0])),
            Grammar::default()
                .command(Command::CmdSetViewport, false)
                .u64(15)
                .u32(0)
                .u32(1)
                .u64(1)
                .f32(0.0)
                .f32(8.0)
                .f32(16.0)
                .f32(-8.0)
                .f32(0.0)
                .f32(1.0),
        ),
        (
            encoded(|e| e.cmd_set_scissor(15, [1, 2, 3, 4])),
            Grammar::default()
                .command(Command::CmdSetScissor, false)
                .u64(15)
                .u32(0)
                .u32(1)
                .u64(1)
                .i32(1)
                .i32(2)
                .u32(3)
                .u32(4),
        ),
        (
            encoded(|e| e.cmd_set_stencil_reference(15, 9)),
            Grammar::default()
                .command(Command::CmdSetStencilReference, false)
                .u64(15)
                .u32(3)
                .u32(9),
        ),
        (
            encoded(|e| e.cmd_set_blend_constants(15, [0.25, 0.5, 0.75, 1.0])),
            Grammar::default()
                .command(Command::CmdSetBlendConstants, false)
                .u64(15)
                .u64(4)
                .f32(0.25)
                .f32(0.5)
                .f32(0.75)
                .f32(1.0),
        ),
    ]);
}

/// A draw itself, its counts given or in records, as the renderer reads it.
#[test]
fn a_draw_s_counts_are_held_to_the_renderer_s_grammar() {
    held_to_the_grammar([
        (
            encoded(|e| e.cmd_draw(15, DrawInfo::Vertices([3, 2, 1, 5]))),
            Grammar::default()
                .command(Command::CmdDraw, false)
                .u64(15)
                .u32(3)
                .u32(2)
                .u32(1)
                .u32(5),
        ),
        (
            encoded(|e| e.cmd_draw(15, DrawInfo::Indexed([6, 1, 3], -2, 4))),
            Grammar::default()
                .command(Command::CmdDrawIndexed, false)
                .u64(15)
                .u32(6)
                .u32(1)
                .u32(3)
                .i32(-2)
                .u32(4),
        ),
        (
            encoded(|e| e.cmd_draw_indirect(15, false, (40, 8), (2, 16))),
            Grammar::default()
                .command(Command::CmdDrawIndirect, false)
                .u64(15)
                .u64(40)
                .u64(8)
                .u32(2)
                .u32(16),
        ),
        (
            encoded(|e| e.cmd_draw_indirect(15, true, (40, 8), (2, 20))),
            Grammar::default()
                .command(Command::CmdDrawIndexedIndirect, false)
                .u64(15)
                .u64(40)
                .u64(8)
                .u32(2)
                .u32(20),
        ),
    ]);
}
