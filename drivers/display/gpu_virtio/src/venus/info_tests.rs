//! A host's statement narrowed to the device the seam takes.

use alloc::sync::Arc;
use alloc::vec::Vec;

use tairix_abi::DriverError;
use tairix_gpu::{
    Device, Features, FormatFeatures, MemoryProperties, QueueCapabilities, MAX_SUBGROUP_SIZE,
};
use tairix_inline::ArrayVec;

use super::narrow;
use crate::venus::reply::{
    HostLimits, HostMemory, HostMemoryHeap, HostMemoryType, HostProperties, HostQueueFamily,
};
use crate::venus::testing::{Idle, Still};
use crate::venus::{memory, queue, API_VERSION, HEAP_DEVICE_LOCAL, PHYSICAL_DEVICE_NAME_LEN};

fn host_limits() -> HostLimits {
    HostLimits {
        image_1d: 16384,
        image_2d: 16384,
        image_3d: 2048,
        array_layers: 2048,
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
        sampler_anisotropy: 16.0,
        viewport_dimensions: [16384; 2],
        viewport_bounds: [-32768.0, 32767.0],
        uniform_offset_alignment: 64,
        storage_offset_alignment: 16,
        colour_attachments: 16,
        point_sizes: [1.0, 255.0],
        line_widths: [1.0, 8.0],
    }
}

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

#[test]
fn the_host_is_narrowed_to_coherent_memory_and_one_family_running_transfers() {
    let statement = narrow(&properties(), &host_memory(), &families()).expect("stated");
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
        QueueCapabilities::TRANSFER
    );
    assert_eq!(info.queue_families[0].queues, 2);
    assert!(info.capabilities.is_empty());
    assert_eq!(info.features, Features::NONE);
    assert!(tairix_gpu::Format::ALL
        .iter()
        .all(|&format| info.formats.get(format) == FormatFeatures::NONE));
}

#[test]
fn the_limits_are_the_host_s_held_to_the_seam_s_bounds() {
    let limits = narrow(&properties(), &host_memory(), &families())
        .expect("stated")
        .info
        .limits;
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
    assert_eq!(limits.subgroup_size, 8);
    assert_eq!(limits.viewport_bounds_range, [-32768, 32767]);
    assert_eq!(limits.max_point_size, 255);
    assert_eq!(
        (
            limits.max_binding_number,
            limits.max_storage_image_bytes,
            limits.max_invocation_memory
        ),
        (0, 0, 0),
        "what the host states no counterpart of is none"
    );
    let mut small = host_memory();
    small.heaps[0].size = 1 << 20;
    let limits = narrow(&properties(), &small, &families())
        .expect("stated")
        .info
        .limits;
    assert_eq!(
        limits.max_allocation_size,
        1 << 20,
        "no larger than the largest heap"
    );
    let mut wide = properties();
    wide.subgroup_size = 128;
    let limits = narrow(&wide, &host_memory(), &families())
        .expect("stated")
        .info
        .limits;
    assert_eq!(limits.subgroup_size, MAX_SUBGROUP_SIZE);
}

#[test]
fn a_host_the_backend_cannot_serve_or_whose_statement_is_none_is_refused() {
    let mut old = properties();
    old.api_version = 1 << 22;
    assert_eq!(
        narrow(&old, &host_memory(), &families()).err(),
        Some(DriverError::Unsupported)
    );
    let idle = [HostQueueFamily {
        flags: queue::TRANSFER,
        queues: 0,
    }];
    assert_eq!(
        narrow(&properties(), &host_memory(), &idle).err(),
        Some(DriverError::Unsupported)
    );
    let mut incoherent = host_memory();
    incoherent.types.truncate(2);
    assert_eq!(
        narrow(&properties(), &incoherent, &families()).err(),
        Some(DriverError::Unsupported),
        "no memory type the seam names"
    );
    let mut odd = properties();
    odd.limits.storage_offset_alignment = 24;
    assert_eq!(
        narrow(&odd, &host_memory(), &families()).err(),
        Some(DriverError::DeviceFault)
    );
    let mut huge = properties();
    huge.limits.uniform_offset_alignment = 512;
    assert_eq!(
        narrow(&huge, &host_memory(), &families()).err(),
        Some(DriverError::DeviceFault)
    );
    let mut uneven = properties();
    uneven.subgroup_size = 48;
    assert_eq!(
        narrow(&uneven, &host_memory(), &families()).err(),
        Some(DriverError::DeviceFault)
    );
}

#[test]
fn the_statement_is_a_device_the_seam_takes() {
    let statement = narrow(&properties(), &host_memory(), &families()).expect("stated");
    assert!(Device::hardware(Idle, statement.info, &[1], Arc::new(Still)).is_ok());
}
