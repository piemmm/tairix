//! Descriptions on the wire: the device's, and every object's a context
//! asks for.

use tairix_inline::ArrayVec;
use tairix_spirv::enumerant::{Capability, Dim, ImageFormat};
use tairix_spirv::{Access, BindingKind, Component, ImageShape, SpecValue, Stages};

use super::{wire_enum, wire_flags, Reader, Wire, Writer};
use crate::info::{FormatTable, MemoryHeap, MemoryType, QueueFamily};
use crate::{
    AddressMode, AllocationDesc, Aspects, Blend, BlendFactor, BlendOp, BorderColor, BufferDesc,
    BufferImageCopy, BufferUsage, BufferViewDesc, ClearValue, ColourMask, ColourTarget, CompareOp,
    CopyRegion, CullMode, DepthBias, DepthStencilState, DeviceInfo, Error, Extent3d, Features,
    Filter, Format, FormatFeatures, FrontFace, ImageCopy, ImageDesc, ImageDimension, ImageRange,
    ImageSubresource, ImageUsage, ImageViewDesc, IndexFormat, LayoutBinding, Limits, Load, LogicOp,
    MemoryProperties, MemoryRequirements, MemoryUse, Multisample, Origin3d, PushConstants,
    QueueCapabilities, Rasterization, Rect, SamplerDesc, StencilFaceState, StencilOp, Topology,
    VertexAttribute, VertexStep, ViewDimension, Viewport, MAX_CAPABILITIES, MAX_MEMORY_HEAPS,
    MAX_MEMORY_TYPES, MAX_QUEUE_FAMILIES,
};

wire_enum!(ImageDimension { D1 = 0, D2 = 1, D3 = 2 });
wire_enum!(ViewDimension {
    D1 = 0,
    D1Array = 1,
    D2 = 2,
    D2Array = 3,
    Cube = 4,
    CubeArray = 5,
    D3 = 6,
});
wire_enum!(Filter { Nearest = 0, Linear = 1 });
wire_enum!(AddressMode {
    Repeat = 0,
    MirroredRepeat = 1,
    ClampToEdge = 2,
    MirrorClampToEdge = 3,
    ClampToBorder = 4,
});
wire_enum!(BorderColor {
    TransparentBlack = 0,
    OpaqueBlack = 1,
    OpaqueWhite = 2,
});
wire_enum!(CompareOp {
    Never = 0,
    Less = 1,
    Equal = 2,
    LessOrEqual = 3,
    Greater = 4,
    NotEqual = 5,
    GreaterOrEqual = 6,
    Always = 7,
});
wire_enum!(StencilOp {
    Keep = 0,
    Zero = 1,
    Replace = 2,
    IncrementAndClamp = 3,
    DecrementAndClamp = 4,
    Invert = 5,
    IncrementAndWrap = 6,
    DecrementAndWrap = 7,
});
wire_enum!(BlendFactor {
    Zero = 0,
    One = 1,
    SourceColor = 2,
    OneMinusSourceColor = 3,
    DestinationColor = 4,
    OneMinusDestinationColor = 5,
    SourceAlpha = 6,
    OneMinusSourceAlpha = 7,
    DestinationAlpha = 8,
    OneMinusDestinationAlpha = 9,
    ConstantColor = 10,
    OneMinusConstantColor = 11,
    ConstantAlpha = 12,
    OneMinusConstantAlpha = 13,
    SourceAlphaSaturate = 14,
    Source1Color = 15,
    OneMinusSource1Color = 16,
    Source1Alpha = 17,
    OneMinusSource1Alpha = 18,
});
wire_enum!(BlendOp {
    Add = 0,
    Subtract = 1,
    ReverseSubtract = 2,
    Min = 3,
    Max = 4,
});
wire_enum!(LogicOp {
    Clear = 0,
    And = 1,
    AndReverse = 2,
    Copy = 3,
    AndInverted = 4,
    NoOp = 5,
    Xor = 6,
    Or = 7,
    Nor = 8,
    Equivalent = 9,
    Invert = 10,
    OrReverse = 11,
    CopyInverted = 12,
    OrInverted = 13,
    Nand = 14,
    Set = 15,
});
wire_enum!(VertexStep { Vertex = 0, Instance = 1 });
wire_enum!(Topology {
    Points = 0,
    Lines = 1,
    LineStrip = 2,
    Triangles = 3,
    TriangleStrip = 4,
    TriangleFan = 5,
});
wire_enum!(CullMode {
    None = 0,
    Front = 1,
    Back = 2,
    Both = 3,
});
wire_enum!(FrontFace {
    CounterClockwise = 0,
    Clockwise = 1,
});
wire_enum!(IndexFormat { U16 = 0, U32 = 1 });
wire_enum!(Component {
    Float = 0,
    Sint = 1,
    Uint = 2,
});

wire_flags!(MemoryProperties, u32);
wire_flags!(BufferUsage, u32);
wire_flags!(QueueCapabilities, u32);
wire_flags!(Features, u32);
wire_flags!(FormatFeatures, u32);
wire_flags!(ImageUsage, u32);
wire_flags!(Aspects, u32);
wire_flags!(Stages, u8);
wire_flags!(Access, u8);
wire_flags!(ColourMask, u8);

impl Wire for Format {
    fn put(&self, writer: &mut Writer<'_>) -> Result<(), Error> {
        u8::try_from(self.index())
            .map_err(|_| Error::Malformed)?
            .put(writer)
    }

    fn take(reader: &mut Reader<'_>) -> Result<Self, Error> {
        Self::ALL
            .get(usize::from(u8::take(reader)?))
            .copied()
            .ok_or(Error::Malformed)
    }
}

/// A struct, its fields in order.
macro_rules! wire_struct {
    ($ty:ident { $($field:ident),* $(,)? }) => {
        impl Wire for $ty {
            fn put(&self, writer: &mut Writer<'_>) -> Result<(), Error> {
                let Self { $($field),* } = self;
                $($field.put(writer)?;)*
                Ok(())
            }

            fn take(reader: &mut Reader<'_>) -> Result<Self, Error> {
                Ok(Self { $($field: Wire::take(reader)?),* })
            }
        }
    };
}

wire_struct!(Extent3d {
    width,
    height,
    depth
});
wire_struct!(Origin3d { x, y, z });
wire_struct!(ImageDesc {
    dimension,
    format,
    extent,
    mip_levels,
    array_layers,
    samples,
    usage
});
wire_struct!(ImageRange {
    aspects,
    base_level,
    levels,
    base_layer,
    layers
});
wire_struct!(ImageViewDesc {
    format,
    dimension,
    range
});
wire_struct!(BufferViewDesc {
    offset,
    size,
    format
});
wire_struct!(ImageSubresource {
    aspect,
    level,
    base_layer,
    layers
});
wire_struct!(BufferImageCopy {
    buffer_offset,
    bytes_per_row,
    rows_per_image,
    image,
    origin,
    extent
});
wire_struct!(ImageCopy {
    src,
    src_origin,
    dst,
    dst_origin,
    extent
});
wire_struct!(MemoryRequirements {
    size,
    alignment,
    memory_types
});
wire_struct!(MemoryUse {
    allocations,
    objects
});
wire_struct!(AllocationDesc { size, memory_type });
wire_struct!(BufferDesc {
    offset,
    size,
    usage
});
wire_struct!(CopyRegion {
    src_offset,
    dst_offset,
    size
});
wire_struct!(PushConstants { bytes, stages });
wire_struct!(SpecValue { id, bits });
wire_struct!(DepthBias {
    constant,
    slope,
    clamp
});
wire_struct!(Rasterization {
    cull,
    front_face,
    depth_bias,
    depth_clamp,
    line_width
});
wire_struct!(StencilFaceState {
    fail,
    pass,
    depth_fail,
    compare,
    compare_mask,
    write_mask
});
wire_struct!(Multisample {
    samples,
    mask,
    alpha_to_coverage
});
wire_struct!(Blend {
    source_color,
    destination_color,
    color,
    source_alpha,
    destination_alpha,
    alpha
});
wire_struct!(ColourTarget {
    format,
    blend,
    write_mask
});
wire_struct!(VertexAttribute {
    location,
    format,
    offset
});
wire_struct!(Rect {
    x,
    y,
    width,
    height
});
wire_struct!(Viewport {
    x,
    y,
    width,
    height,
    min_depth,
    max_depth
});
wire_struct!(ImageShape {
    dim,
    arrayed,
    multisampled,
    component,
    format
});
wire_struct!(LayoutBinding {
    binding,
    kind,
    count,
    access,
    stages
});

impl Wire for Dim {
    fn put(&self, writer: &mut Writer<'_>) -> Result<(), Error> {
        self.0.put(writer)
    }

    fn take(reader: &mut Reader<'_>) -> Result<Self, Error> {
        u32::take(reader).map(Self)
    }
}

impl Wire for ImageFormat {
    fn put(&self, writer: &mut Writer<'_>) -> Result<(), Error> {
        self.0.put(writer)
    }

    fn take(reader: &mut Reader<'_>) -> Result<Self, Error> {
        u32::take(reader).map(Self)
    }
}

impl Wire for Capability {
    fn put(&self, writer: &mut Writer<'_>) -> Result<(), Error> {
        self.0.put(writer)
    }

    fn take(reader: &mut Reader<'_>) -> Result<Self, Error> {
        u32::take(reader).map(Self)
    }
}

impl Wire for BindingKind {
    fn put(&self, writer: &mut Writer<'_>) -> Result<(), Error> {
        let (index, shape): (u8, Option<&ImageShape>) = match self {
            Self::Sampler => (0, None),
            Self::SampledImage(shape) => (1, Some(shape)),
            Self::StorageImage(shape) => (2, Some(shape)),
            Self::CombinedImageSampler(shape) => (3, Some(shape)),
            Self::UniformTexelBuffer(shape) => (4, Some(shape)),
            Self::StorageTexelBuffer(shape) => (5, Some(shape)),
            Self::UniformBuffer => (6, None),
            Self::StorageBuffer => (7, None),
        };
        index.put(writer)?;
        shape.map_or(Ok(()), |shape| shape.put(writer))
    }

    fn take(reader: &mut Reader<'_>) -> Result<Self, Error> {
        Ok(match u8::take(reader)? {
            0 => Self::Sampler,
            1 => Self::SampledImage(ImageShape::take(reader)?),
            2 => Self::StorageImage(ImageShape::take(reader)?),
            3 => Self::CombinedImageSampler(ImageShape::take(reader)?),
            4 => Self::UniformTexelBuffer(ImageShape::take(reader)?),
            5 => Self::StorageTexelBuffer(ImageShape::take(reader)?),
            6 => Self::UniformBuffer,
            7 => Self::StorageBuffer,
            _ => return Err(Error::Malformed),
        })
    }
}

impl Wire for ClearValue {
    fn put(&self, writer: &mut Writer<'_>) -> Result<(), Error> {
        match *self {
            Self::Float(value) => {
                0u8.put(writer)?;
                value.put(writer)
            }
            Self::Sint(value) => {
                1u8.put(writer)?;
                value.put(writer)
            }
            Self::Uint(value) => {
                2u8.put(writer)?;
                value.put(writer)
            }
            Self::DepthStencil { depth, stencil } => {
                3u8.put(writer)?;
                depth.put(writer)?;
                stencil.put(writer)
            }
        }
    }

    fn take(reader: &mut Reader<'_>) -> Result<Self, Error> {
        Ok(match u8::take(reader)? {
            0 => Self::Float(Wire::take(reader)?),
            1 => Self::Sint(Wire::take(reader)?),
            2 => Self::Uint(Wire::take(reader)?),
            3 => Self::DepthStencil {
                depth: Wire::take(reader)?,
                stencil: Wire::take(reader)?,
            },
            _ => return Err(Error::Malformed),
        })
    }
}

impl Wire for Load {
    fn put(&self, writer: &mut Writer<'_>) -> Result<(), Error> {
        match self {
            Self::Keep => None::<ClearValue>.put(writer),
            Self::Clear(value) => Some(*value).put(writer),
        }
    }

    fn take(reader: &mut Reader<'_>) -> Result<Self, Error> {
        Ok(<Option<ClearValue> as Wire>::take(reader)?.map_or(Self::Keep, Self::Clear))
    }
}

impl Wire for SamplerDesc {
    fn put(&self, writer: &mut Writer<'_>) -> Result<(), Error> {
        self.mag_filter.put(writer)?;
        self.min_filter.put(writer)?;
        self.mipmap_filter.put(writer)?;
        self.address.iter().try_for_each(|mode| mode.put(writer))?;
        self.lod_bias.put(writer)?;
        self.lod_min.put(writer)?;
        self.lod_max.put(writer)?;
        self.max_anisotropy.put(writer)?;
        self.compare.put(writer)?;
        self.border.put(writer)?;
        self.unnormalized.put(writer)
    }

    fn take(reader: &mut Reader<'_>) -> Result<Self, Error> {
        Ok(Self {
            mag_filter: Wire::take(reader)?,
            min_filter: Wire::take(reader)?,
            mipmap_filter: Wire::take(reader)?,
            address: [
                Wire::take(reader)?,
                Wire::take(reader)?,
                Wire::take(reader)?,
            ],
            lod_bias: Wire::take(reader)?,
            lod_min: Wire::take(reader)?,
            lod_max: Wire::take(reader)?,
            max_anisotropy: Wire::take(reader)?,
            compare: Wire::take(reader)?,
            border: Wire::take(reader)?,
            unnormalized: Wire::take(reader)?,
        })
    }
}

impl Wire for DepthStencilState {
    fn put(&self, writer: &mut Writer<'_>) -> Result<(), Error> {
        self.format.put(writer)?;
        self.depth_compare.put(writer)?;
        self.depth_write.put(writer)?;
        match &self.stencil {
            None => false.put(writer),
            Some([front, back]) => {
                true.put(writer)?;
                front.put(writer)?;
                back.put(writer)
            }
        }
    }

    fn take(reader: &mut Reader<'_>) -> Result<Self, Error> {
        Ok(Self {
            format: Wire::take(reader)?,
            depth_compare: Wire::take(reader)?,
            depth_write: Wire::take(reader)?,
            stencil: if bool::take(reader)? {
                Some([Wire::take(reader)?, Wire::take(reader)?])
            } else {
                None
            },
        })
    }
}

wire_struct!(MemoryHeap { size, device_local });
wire_struct!(MemoryType { heap, properties });
wire_struct!(QueueFamily {
    capabilities,
    queues
});
wire_struct!(Limits {
    max_allocation_size,
    buffer_alignment,
    max_job_time,
    max_image_dimension_1d,
    max_image_dimension_2d,
    max_image_dimension_3d,
    max_image_array_layers,
    max_sampler_anisotropy,
    max_bind_sets,
    max_binding_number,
    max_bind_set_descriptors,
    max_push_constant_bytes,
    uniform_buffer_alignment,
    storage_buffer_alignment,
    max_uniform_buffer_range,
    max_storage_buffer_range,
    max_storage_image_bytes,
    max_texel_buffer_elements,
    max_workgroup_size,
    max_workgroup_invocations,
    max_workgroup_count,
    max_workgroup_memory,
    max_invocation_memory,
    subgroup_size,
    max_vertex_input_attributes,
    max_vertex_input_bindings,
    max_vertex_input_attribute_offset,
    max_vertex_input_binding_stride,
    max_vertex_output_components,
    max_fragment_input_components,
    max_color_attachments,
    max_viewport_dimensions,
    viewport_bounds_range,
    max_point_size,
    max_line_width,
    max_draw_indirect_count
});

/// Write the device's description.
pub(crate) fn put_info(writer: &mut Writer<'_>, info: &DeviceInfo) -> Result<(), Error> {
    writer.list(&info.heaps, |writer, heap| heap.put(writer))?;
    writer.list(&info.memory_types, |writer, kind| kind.put(writer))?;
    writer.list(&info.queue_families, |writer, family| family.put(writer))?;
    info.limits.put(writer)?;
    writer.list(&info.capabilities, |writer, capability| {
        capability.put(writer)
    })?;
    info.features.put(writer)?;
    Format::ALL
        .iter()
        .try_for_each(|&format| info.formats.get(format).put(writer))
}

/// Read a device's description, held to what the seam admits of one.
pub(crate) fn take_info(reader: &mut Reader<'_>) -> Result<DeviceInfo, Error> {
    let heaps = array_vec::<MemoryHeap, MAX_MEMORY_HEAPS>(reader)?;
    let memory_types = array_vec::<MemoryType, MAX_MEMORY_TYPES>(reader)?;
    let queue_families = array_vec::<QueueFamily, MAX_QUEUE_FAMILIES>(reader)?;
    let limits = Limits::take(reader)?;
    let capabilities = array_vec::<Capability, MAX_CAPABILITIES>(reader)?;
    let features = Features::take(reader)?;
    let mut formats = [FormatFeatures::NONE; Format::ALL.len()];
    for features in &mut formats {
        *features = FormatFeatures::take(reader)?;
    }
    let info = DeviceInfo {
        heaps,
        memory_types,
        queue_families,
        limits,
        capabilities,
        features,
        formats: FormatTable::from_fn(|format| formats[format.index()]),
    };
    info.check()?;
    Ok(info)
}

fn array_vec<T: Wire, const N: usize>(reader: &mut Reader<'_>) -> Result<ArrayVec<T, N>, Error> {
    let count = reader.count(N)?;
    let mut items = ArrayVec::new();
    for _ in 0..count {
        items
            .try_push(T::take(reader)?)
            .map_err(|_| Error::Malformed)?;
    }
    Ok(items)
}
