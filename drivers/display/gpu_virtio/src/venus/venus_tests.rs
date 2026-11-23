//! The encoder held byte for byte to the renderer's grammar, each expected
//! stream written field by field from the renderer's decoder for its
//! command; the replies read as the renderer writes them, and refused where
//! it would not.

use alloc::vec::Vec;

use tairix_abi::DriverError;

use super::encode::{CopyRegion, Encoder, StreamSpan};
use super::reply::{HostMemoryHeap, HostMemoryType, HostQueueFamily, Reader};
use super::testing::{limits, properties_reply, Grammar};
use super::{Command, Structure, VkResult, API_VERSION, MAX_MEMORY_HEAPS, MAX_MEMORY_TYPES};

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
    assert_eq!(API_VERSION, 0x0040_1000, "Vulkan 1.1");
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
    let properties = Grammar::default()
        .command(Command::GetPhysicalDeviceProperties2, true)
        .u64(8)
        .u64(1)
        .i32(Structure::PhysicalDeviceProperties2 as i32)
        .u64(1)
        .i32(Structure::PhysicalDeviceSubgroupProperties as i32)
        .u64(1)
        .structure(Structure::PhysicalDeviceMaintenance3Properties);
    assert_eq!(
        encoded(|e| e.get_physical_device_properties2(8)),
        properties.0
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
fn a_device_and_its_queue_bound_to_a_ring_are_made() {
    let device = Grammar::default()
        .command(Command::CreateDevice, true)
        .u64(8)
        .u64(1)
        .structure(Structure::DeviceCreateInfo)
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
        .u32(0)
        .u64(0)
        .u64(0)
        .u64(0)
        .u64(1)
        .u64(10);
    assert_eq!(encoded(|e| e.create_device(8, 10, 3, 2)), device.0);
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
        encoded(|e| e.cmd_copy_buffer(15, (13, 17), &regions)),
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
    let bytes = properties_reply(limits).0;
    assert_eq!(bytes.len(), 944, "the reply the renderer writes");
    let mut reader = Reader::new(&bytes);
    let properties = reader.properties().expect("read");
    assert_eq!(properties.subgroup_size, 32);
    assert_eq!(properties.max_allocation_size, 1 << 31);
    assert_eq!(properties.api_version, API_VERSION);
    assert_eq!(properties.vendor, 0x10005);
    assert_eq!(&properties.name[..8], b"llvmpipe");
    let limits = properties.limits;
    assert_eq!(limits.image_2d, 16384);
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
    let mut empty = Reader::new(&[]);
    assert_eq!(
        empty.result_of(Command::QueueSubmit),
        Err(DriverError::DeviceFault)
    );
}

#[test]
fn a_reply_off_its_grammar_is_a_device_fault() {
    let good = properties_reply(limits).0;
    let fault = |bytes: &[u8]| Reader::new(bytes).properties().err();
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
    let mut chained = good.clone();
    chained[40] = 1;
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
    let mut renamed = good;
    renamed[96] = 255;
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
fn memory_requirements_and_command_buffers_are_read() {
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
        .memory_requirements()
        .expect("read");
    assert_eq!((read.size, read.alignment, read.type_bits), (8192, 256, 3));

    let buffers = Grammar::default()
        .i32(Command::AllocateCommandBuffers as i32)
        .i32(0)
        .u64(2)
        .u64(15)
        .u64(16)
        .0;
    assert_eq!(
        Reader::new(&buffers).command_buffers(&[15, 16]),
        Ok(VkResult::Success)
    );
    assert!(Reader::new(&buffers).command_buffers(&[15]).is_err());
}

#[test]
fn a_result_names_success_failure_or_a_part_done() {
    assert_eq!(VkResult::of(0), VkResult::Success);
    assert_eq!(VkResult::of(-4), VkResult::Failed(-4));
    assert_eq!(VkResult::of(5), VkResult::Partial(5));
}
