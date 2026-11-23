//! What a Venus device states: the host's statement narrowed to what the
//! backend runs — memory and transfers — and held to the seam's bounds.
//!
//! A host-visible memory type is stated only where it is coherent, since the
//! renderer maps nothing and flushes nothing; a type of any property the seam
//! does not name is left out, as is a heap of no bytes. One queue family is
//! stated, running transfers alone, and no format, shader capability or
//! feature, so the model refuses what the backend cannot run before it is
//! asked. A limit the host states is its value, held to the seam's bound; one
//! it has no counterpart for gates only work the stated device does not do,
//! and is stated as none.

use tairix_abi::{DriverError, Duration64};
use tairix_gpu::{
    DeviceInfo, Features, FormatFeatures, FormatTable, Limits, MemoryHeap, MemoryProperties,
    MemoryType, QueueCapabilities, QueueFamily, MAX_DRAW_VERTEX_BUFFERS, MAX_PASS_ATTACHMENTS,
    MAX_PUSH_BYTES, MAX_QUEUES_PER_FAMILY, MAX_SUBGROUP_SIZE, MAX_VERTEX_ATTRIBUTES,
};
use tairix_inline::ArrayVec;
use tairix_util::mathf;

use super::reply::{HostMemory, HostProperties, HostQueueFamily};
use super::{
    memory, queue, API_VERSION, HEAP_DEVICE_LOCAL, MAX_MEMORY_HEAPS, MAX_MEMORY_TYPES,
    MAX_QUEUE_RINGS,
};

/// The longest a job may run before its context is lost: the renderer cannot
/// stop a job once started, so this is how long one holds its queue.
const MAX_JOB_TIME: Duration64 = Duration64::from_secs(10);

/// The widest alignment Vulkan lets a device require of a buffer binding's
/// offset.
const MAX_BINDING_ALIGNMENT: u64 = 256;

/// What a Venus device states, and which of the host's memory types and
/// queue family each stated one is.
#[derive(Clone, Debug)]
pub(crate) struct Statement {
    pub(crate) info: DeviceInfo,
    /// The host's index of each stated memory type, in order.
    pub(crate) memory_types: ArrayVec<u32, MAX_MEMORY_TYPES>,
    /// The host's index of the stated queue family.
    pub(crate) family: u32,
}

/// The device the host's statement makes.
///
/// # Errors
///
/// [`DriverError::Unsupported`] for a physical device of an older Vulkan, or
/// with no queue family running transfers or no memory type the seam names;
/// [`DriverError::DeviceFault`] for a statement no device makes: an
/// alignment that is no power of two, or past what Vulkan allows.
pub(crate) fn narrow(
    properties: &HostProperties,
    host_memory: &HostMemory,
    families: &[HostQueueFamily],
) -> Result<Statement, DriverError> {
    if properties.api_version < API_VERSION {
        return Err(DriverError::Unsupported);
    }
    let transfers = queue::TRANSFER | queue::COMPUTE | queue::GRAPHICS;
    let (family, queues) = families
        .iter()
        .enumerate()
        .find(|(_, family)| family.flags & transfers != 0 && family.queues > 0)
        .map(|(index, family)| (index, family.queues))
        .ok_or(DriverError::Unsupported)?;
    let mut heaps = ArrayVec::new();
    let mut heap_of = [None; MAX_MEMORY_HEAPS];
    for (host, heap) in host_memory.heaps.iter().enumerate() {
        if heap.size == 0 {
            continue;
        }
        heap_of[host] = u32::try_from(heaps.len()).ok();
        heaps
            .try_push(MemoryHeap {
                size: heap.size,
                device_local: heap.flags & HEAP_DEVICE_LOCAL != 0,
            })
            .map_err(full)?;
    }
    let mut stated_types = ArrayVec::new();
    let mut memory_types = ArrayVec::new();
    for (host, kind) in host_memory.types.iter().enumerate() {
        let Some(properties) = stated_properties(kind.flags) else {
            continue;
        };
        let Some(&Some(heap)) = heap_of.get(kind.heap as usize) else {
            continue;
        };
        stated_types
            .try_push(MemoryType { heap, properties })
            .map_err(full)?;
        memory_types
            .try_push(u32::try_from(host).map_err(|_| DriverError::DeviceFault)?)
            .map_err(full)?;
    }
    if stated_types.is_empty() {
        return Err(DriverError::Unsupported);
    }
    let largest = heaps
        .iter()
        .map(|heap: &MemoryHeap| heap.size)
        .max()
        .unwrap_or(0);
    let mut queue_families = ArrayVec::new();
    queue_families
        .try_push(QueueFamily {
            capabilities: QueueCapabilities::TRANSFER,
            queues: queues.min(MAX_QUEUE_RINGS).min(MAX_QUEUES_PER_FAMILY),
        })
        .map_err(full)?;
    Ok(Statement {
        info: DeviceInfo {
            heaps,
            memory_types: stated_types,
            queue_families,
            limits: limits(properties, properties.max_allocation_size.min(largest))?,
            capabilities: ArrayVec::new(),
            features: Features::NONE,
            formats: FormatTable::from_fn(|_| FormatFeatures::NONE),
        },
        memory_types,
        family: u32::try_from(family).map_err(|_| DriverError::DeviceFault)?,
    })
}

/// A list the host's own bounds kept within, overflowing: a statement no
/// device makes.
fn full<T>(_: T) -> DriverError {
    DriverError::DeviceFault
}

/// The seam's properties of a host memory type of `flags`, where the seam
/// names them all and the type is coherent wherever the CPU reaches it.
fn stated_properties(flags: u32) -> Option<MemoryProperties> {
    let named =
        memory::DEVICE_LOCAL | memory::HOST_VISIBLE | memory::HOST_COHERENT | memory::HOST_CACHED;
    let visible = flags & memory::HOST_VISIBLE != 0;
    if flags & !named != 0 || (visible && flags & memory::HOST_COHERENT == 0) {
        return None;
    }
    let mut properties = MemoryProperties::NONE;
    for (flag, property) in [
        (memory::DEVICE_LOCAL, MemoryProperties::DEVICE_LOCAL),
        (memory::HOST_VISIBLE, MemoryProperties::HOST_VISIBLE),
        (memory::HOST_COHERENT, MemoryProperties::HOST_COHERENT),
        (memory::HOST_CACHED, MemoryProperties::HOST_CACHED),
    ] {
        if flags & flag != 0 {
            properties = properties.with(property);
        }
    }
    Some(properties)
}

/// A binding alignment the host states: a power of two Vulkan allows.
fn binding_alignment(alignment: u64) -> Result<u64, DriverError> {
    if alignment.is_power_of_two() && alignment <= MAX_BINDING_ALIGNMENT {
        Ok(alignment)
    } else {
        Err(DriverError::DeviceFault)
    }
}

/// A host's limit in whole units: at least one, at most an `i32`'s widest.
fn whole(value: f32) -> u32 {
    let floored = mathf::round_i32(mathf::floor(f64::from(value)));
    u32::try_from(floored.max(1)).unwrap_or(1)
}

/// One of the seam's bounds as a limit.
fn bound(bound: usize) -> u32 {
    u32::try_from(bound).unwrap_or(u32::MAX)
}

/// The host's limits held to the seam's bounds, its largest allocation
/// `max_allocation_size`.
fn limits(properties: &HostProperties, max_allocation_size: u64) -> Result<Limits, DriverError> {
    let host = &properties.limits;
    let uniform = binding_alignment(host.uniform_offset_alignment)?;
    let storage = binding_alignment(host.storage_offset_alignment)?;
    if !properties.subgroup_size.is_power_of_two() {
        return Err(DriverError::DeviceFault);
    }
    let [low, high] = host.viewport_bounds;
    // Saturating: a bound past an `i32` is stated at its widest.
    let viewport_bounds_range = [
        mathf::round_i32(mathf::floor(f64::from(low))),
        mathf::round_i32(mathf::ceil(f64::from(high))),
    ];
    if viewport_bounds_range[0] > viewport_bounds_range[1] {
        return Err(DriverError::DeviceFault);
    }
    let [samplers, uniforms, storages, sampled, stored] = host.set_descriptors;
    let set_descriptors = samplers
        .saturating_add(uniforms)
        .saturating_add(storages)
        .saturating_add(sampled)
        .saturating_add(stored)
        .min(properties.max_set_descriptors);
    Ok(Limits {
        max_allocation_size,
        // A seam buffer is a range of the one buffer over its memory, so its
        // start keeps every binding's alignment and a fill's word.
        buffer_alignment: uniform.max(storage).max(4),
        max_job_time: MAX_JOB_TIME,
        max_image_dimension_1d: host.image_1d,
        max_image_dimension_2d: host.image_2d,
        max_image_dimension_3d: host.image_3d,
        max_image_array_layers: host.array_layers,
        max_sampler_anisotropy: whole(host.sampler_anisotropy),
        max_bind_sets: host.bound_sets.max(1),
        max_binding_number: 0,
        max_bind_set_descriptors: set_descriptors,
        max_push_constant_bytes: host.push_constants.min(bound(MAX_PUSH_BYTES)),
        uniform_buffer_alignment: uniform,
        storage_buffer_alignment: storage,
        max_uniform_buffer_range: u64::from(host.uniform_buffer_range),
        max_storage_buffer_range: u64::from(host.storage_buffer_range),
        max_storage_image_bytes: 0,
        max_texel_buffer_elements: u64::from(host.texel_buffer_elements),
        max_workgroup_size: host.workgroup_size,
        max_workgroup_invocations: host.workgroup_invocations,
        max_workgroup_count: host.workgroup_count,
        max_workgroup_memory: host.workgroup_memory,
        max_invocation_memory: 0,
        subgroup_size: properties.subgroup_size.min(MAX_SUBGROUP_SIZE),
        max_vertex_input_attributes: host.vertex_attributes.min(MAX_VERTEX_ATTRIBUTES),
        max_vertex_input_bindings: host.vertex_bindings.min(bound(MAX_DRAW_VERTEX_BUFFERS)),
        max_vertex_input_attribute_offset: host.vertex_attribute_offset,
        max_vertex_input_binding_stride: host.vertex_binding_stride,
        max_vertex_output_components: host.vertex_output_components,
        max_fragment_input_components: host.fragment_input_components,
        max_color_attachments: host.colour_attachments.min(bound(MAX_PASS_ATTACHMENTS)),
        max_viewport_dimensions: host.viewport_dimensions,
        viewport_bounds_range,
        max_point_size: whole(host.point_sizes[1]),
        max_line_width: whole(host.line_widths[1]),
        max_draw_indirect_count: host.draw_indirect_count.max(1),
    })
}

#[cfg(test)]
#[path = "info_tests.rs"]
mod tests;
