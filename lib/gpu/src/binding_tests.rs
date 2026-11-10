//! Bind set layouts, bind sets, pipeline layouts and shader modules on the
//! software device.

use alloc::sync::Arc;

use tairix_abi::time::MonotonicClock;
use tairix_parallel::Serial;
use tairix_spirv::assemble::admit;
use tairix_spirv::enumerant::{Capability, Dim, ImageFormat};
use tairix_spirv::{Access, BindingKind, Component, Disagreement, ImageShape, Stages};

use super::{BindSetEntry, LayoutBinding, PushConstants, Resource};
use crate::device_tests::{context, device, memory, over, Clock, RAM};
use crate::image_device_tests::image;
use crate::model::Model;
use crate::{
    Aspects, BufferUsage, BufferViewDesc, CompareOp, Context, Device, DeviceInfo, Error, Extent3d,
    Filter, Format, ImageDesc, ImageDimension, ImageRange, ImageUsage, ImageViewDesc, NoThreads,
    SamplerDesc, ViewDimension,
};

const fn shape(dim: Dim, arrayed: bool, component: Component, format: ImageFormat) -> ImageShape {
    ImageShape {
        dim,
        arrayed,
        multisampled: false,
        component,
        format,
    }
}

const SAMPLED_2D: ImageShape = shape(Dim::DIM_2D, false, Component::Float, ImageFormat::UNKNOWN);
const STORAGE_2D: ImageShape = shape(Dim::DIM_2D, false, Component::Float, ImageFormat::RGBA8);
const TEXELS: ImageShape = shape(Dim::BUFFER, false, Component::Float, ImageFormat::UNKNOWN);

const fn entry(binding: u32, kind: BindingKind, access: Access) -> LayoutBinding {
    LayoutBinding {
        binding,
        kind,
        count: 1,
        access,
        stages: Stages::ALL,
    }
}

const READ: Access = Access::READ;
const READ_WRITE: Access = Access::READ.with(Access::WRITE);

/// A binding of each kind, numbered in the order [`resources`] fills them.
const EVERY_KIND: [LayoutBinding; 8] = [
    entry(0, BindingKind::Sampler, READ),
    entry(1, BindingKind::SampledImage(SAMPLED_2D), READ),
    entry(2, BindingKind::StorageImage(STORAGE_2D), READ_WRITE),
    entry(3, BindingKind::CombinedImageSampler(SAMPLED_2D), READ),
    entry(4, BindingKind::UniformTexelBuffer(TEXELS), READ),
    entry(5, BindingKind::StorageTexelBuffer(TEXELS), READ_WRITE),
    entry(6, BindingKind::UniformBuffer, READ),
    entry(7, BindingKind::StorageBuffer, READ_WRITE),
];

fn image_desc(format: Format, usage: ImageUsage, levels: u32, layers: u32) -> ImageDesc {
    ImageDesc {
        dimension: ImageDimension::D2,
        format,
        extent: Extent3d {
            width: 8,
            height: 8,
            depth: 1,
        },
        mip_levels: levels,
        array_layers: layers,
        samples: 1,
        usage,
    }
}

fn view_desc(
    format: Format,
    dimension: ViewDimension,
    aspects: Aspects,
    levels: u32,
    layers: u32,
) -> ImageViewDesc {
    ImageViewDesc {
        format,
        dimension,
        range: ImageRange {
            aspects,
            base_level: 0,
            levels,
            base_layer: 0,
            layers,
        },
    }
}

/// What a bind set of [`EVERY_KIND`] binds, and what holds it.
struct Resources {
    sampler: crate::Sampler,
    view: crate::ImageView,
    texels: crate::BufferView,
    buffer: crate::Buffer,
    _memory: [crate::Allocation; 2],
    _image: crate::Image,
}

fn resources(context: &Context) -> Resources {
    let usage = ImageUsage::SAMPLED.with(ImageUsage::STORAGE);
    let (image_memory, picture) = image(context, &image_desc(Format::Rgba8Unorm, usage, 1, 1));
    let view = context
        .create_image_view(
            &picture,
            &view_desc(Format::Rgba8Unorm, ViewDimension::D2, Aspects::COLOR, 1, 1),
        )
        .expect("a view");
    let buffer_memory = memory(context, 4096);
    let buffer = over(context, &buffer_memory, 0, 4096);
    let texels = context
        .create_buffer_view(
            &buffer,
            &BufferViewDesc {
                offset: 0,
                size: 256,
                format: Format::Rgba8Unorm,
            },
        )
        .expect("a texel buffer");
    let sampler = context
        .create_sampler(&SamplerDesc::LINEAR)
        .expect("a sampler");
    Resources {
        sampler,
        view,
        texels,
        buffer,
        _memory: [image_memory, buffer_memory],
        _image: picture,
    }
}

impl Resources {
    fn each(&self) -> [Resource<'_>; 8] {
        let range = Resource::Buffer {
            buffer: &self.buffer,
            offset: 256,
            size: 512,
        };
        [
            Resource::Sampler(&self.sampler),
            Resource::Image(&self.view),
            Resource::Image(&self.view),
            Resource::ImageSampler(&self.view, &self.sampler),
            Resource::TexelBuffer(&self.texels),
            Resource::TexelBuffer(&self.texels),
            range,
            range,
        ]
    }
}

/// One entry per binding of [`EVERY_KIND`], each its one resource.
fn entries<'a>(each: &'a [Resource<'a>; 8]) -> [BindSetEntry<'a>; 8] {
    core::array::from_fn(|at| BindSetEntry {
        binding: EVERY_KIND[at].binding,
        resources: core::slice::from_ref(&each[at]),
    })
}

/// A device stating what the software device does, edited.
pub(crate) fn device_stating(edit: impl FnOnce(&mut DeviceInfo)) -> Device {
    let mut info = crate::software::info(RAM).expect("the software device");
    edit(&mut info);
    let clock: Arc<dyn MonotonicClock + Send + Sync> = Arc::new(Clock::default());
    Device::from_model(
        Model::new(info, &[1]).expect("a model"),
        clock,
        Arc::new(Serial),
        &NoThreads,
    )
    .expect("a device")
}

/// The one binding a layout is made of, refused for what is wrong with it.
fn refused(context: &Context, binding: LayoutBinding) -> Option<Error> {
    context.create_bind_set_layout(&[binding]).err()
}

#[test]
fn a_layout_takes_its_bindings_one_apiece_in_ascending_order() {
    let (device, _) = device();
    let context = context(&device);
    context
        .create_bind_set_layout(&EVERY_KIND)
        .expect("every kind");
    context
        .create_bind_set_layout(&[])
        .expect("no binding at all");
    let mut reversed = EVERY_KIND;
    reversed.swap(0, 1);
    assert_eq!(
        context.create_bind_set_layout(&reversed).err(),
        Some(Error::Bindings)
    );
    let repeated = [EVERY_KIND[0], EVERY_KIND[0]];
    assert_eq!(
        context.create_bind_set_layout(&repeated).err(),
        Some(Error::Bindings)
    );
}

#[test]
fn a_layout_binding_of_nothing_or_past_the_device_is_refused() {
    let (device, _) = device();
    let context = context(&device);
    let limit = device.info().limits.max_binding_number;
    let mut uniform = EVERY_KIND[6];
    uniform.count = 0;
    assert_eq!(refused(&context, uniform), Some(Error::Empty));
    uniform = EVERY_KIND[6];
    uniform.stages = Stages::NONE;
    assert_eq!(refused(&context, uniform), Some(Error::Empty));
    uniform = EVERY_KIND[6];
    uniform.binding = limit;
    assert_eq!(refused(&context, uniform), Some(Error::OutOfRange));
    uniform.binding = limit - 1;
    assert_eq!(
        refused(&context, uniform),
        None,
        "the last number is the device's"
    );
    uniform.access = READ_WRITE;
    assert_eq!(
        refused(&context, uniform),
        Some(Error::Shape),
        "a write to what is read"
    );
    uniform = EVERY_KIND[6];
    uniform.stages = Stages::from_bits(0x80);
    assert_eq!(
        refused(&context, uniform),
        Some(Error::Shape),
        "a stage there is not"
    );
}

#[test]
fn a_layout_binding_of_a_shape_no_module_declares_is_refused() {
    let (device, _) = device();
    let context = context(&device);
    let layered_box = shape(Dim::DIM_3D, true, Component::Float, ImageFormat::UNKNOWN);
    let mut multisampled = STORAGE_2D;
    multisampled.multisampled = true;
    let mut multisampled_cube = SAMPLED_2D;
    multisampled_cube.multisampled = true;
    multisampled_cube.dim = Dim::CUBE;
    let stored_texels = shape(Dim::BUFFER, false, Component::Float, ImageFormat::RGBA8);
    let float_r32ui = shape(Dim::BUFFER, false, Component::Float, ImageFormat::R32UI);
    let misshapen = [
        entry(0, BindingKind::SampledImage(layered_box), READ),
        entry(0, BindingKind::StorageImage(multisampled), READ_WRITE),
        entry(0, BindingKind::SampledImage(multisampled_cube), READ),
        entry(0, BindingKind::StorageImage(stored_texels), READ_WRITE),
        entry(0, BindingKind::UniformTexelBuffer(SAMPLED_2D), READ),
        entry(0, BindingKind::StorageTexelBuffer(float_r32ui), READ_WRITE),
    ];
    for (at, binding) in misshapen.into_iter().enumerate() {
        assert_eq!(refused(&context, binding), Some(Error::Shape), "case {at}");
    }
}

#[test]
fn a_bind_set_is_made_whole_from_resources_of_its_bindings_kinds() {
    let (device, _) = device();
    let context = context(&device);
    let layout = context
        .create_bind_set_layout(&EVERY_KIND)
        .expect("every kind");
    let held = resources(&context);
    let each = held.each();
    context
        .create_bind_set(&layout, &entries(&each))
        .expect("a whole set");
    let swapped = {
        let mut each = each;
        each.swap(0, 1);
        each
    };
    assert_eq!(
        context.create_bind_set(&layout, &entries(&swapped)).err(),
        Some(Error::Shape),
        "a resource of another kind"
    );
}

#[test]
fn a_bind_set_that_does_not_fill_its_layout_is_refused() {
    let (device, _) = device();
    let context = context(&device);
    let layout = context
        .create_bind_set_layout(&EVERY_KIND)
        .expect("every kind");
    let held = resources(&context);
    let each = held.each();
    let whole = entries(&each);
    assert_eq!(
        context.create_bind_set(&layout, &whole[..7]).err(),
        Some(Error::Bindings),
        "a binding left unwritten"
    );
    let mut renumbered = whole;
    renumbered[7].binding = 8;
    assert_eq!(
        context.create_bind_set(&layout, &renumbered).err(),
        Some(Error::Bindings),
        "a binding the layout does not have"
    );
    let mut short = whole;
    short[2].resources = &[];
    assert_eq!(
        context.create_bind_set(&layout, &short).err(),
        Some(Error::Bindings),
        "fewer resources than the binding's count"
    );
    let pair = [each[6], each[6]];
    let mut long = whole;
    long[6].resources = &pair;
    assert_eq!(
        context.create_bind_set(&layout, &long).err(),
        Some(Error::Bindings),
        "more resources than the binding's count"
    );
}

/// A bind set of the one binding `binding` holding `resource`, refused for
/// what is wrong with the pair.
fn binds(context: &Context, binding: LayoutBinding, resource: Resource<'_>) -> Option<Error> {
    let layout = context
        .create_bind_set_layout(&[binding])
        .expect("a layout");
    context
        .create_bind_set(
            &layout,
            &[BindSetEntry {
                binding: binding.binding,
                resources: &[resource],
            }],
        )
        .err()
}

/// A view of `image` as `format`, `dimension` and `aspects` over its first
/// `levels` and `layers`.
fn view(
    context: &Context,
    image: &crate::Image,
    format: Format,
    dimension: ViewDimension,
    (aspects, levels, layers): (Aspects, u32, u32),
) -> crate::ImageView {
    context
        .create_image_view(
            image,
            &view_desc(format, dimension, aspects, levels, layers),
        )
        .expect("a view")
}

const COLOUR: (Aspects, u32, u32) = (Aspects::COLOR, 1, 1);

const UINT_2D: ImageShape = shape(Dim::DIM_2D, false, Component::Uint, ImageFormat::UNKNOWN);

#[test]
fn a_sampled_view_of_another_shape_or_use_is_refused() {
    let (device, _) = device();
    let context = context(&device);
    let sampled_only = image_desc(Format::Rgba8Unorm, ImageUsage::SAMPLED, 2, 6);
    let (_memory, picture) = image(&context, &sampled_only);
    let one = view(
        &context,
        &picture,
        Format::Rgba8Unorm,
        ViewDimension::D2,
        COLOUR,
    );
    let sampled = entry(0, BindingKind::SampledImage(SAMPLED_2D), READ);
    assert_eq!(binds(&context, sampled, Resource::Image(&one)), None);
    let storage = entry(0, BindingKind::StorageImage(STORAGE_2D), READ_WRITE);
    assert_eq!(
        binds(&context, storage, Resource::Image(&one)),
        Some(Error::Usage),
        "an image not made for storage"
    );
    let faces = (Aspects::COLOR, 1, 6);
    let cube = view(
        &context,
        &picture,
        Format::Rgba8Unorm,
        ViewDimension::Cube,
        faces,
    );
    assert_eq!(
        binds(&context, sampled, Resource::Image(&cube)),
        Some(Error::Shape),
        "a cube for a rectangle"
    );
    let uint = entry(0, BindingKind::SampledImage(UINT_2D), READ);
    assert_eq!(
        binds(&context, uint, Resource::Image(&one)),
        Some(Error::Shape),
        "floats for integers"
    );
}

#[test]
fn a_storage_view_of_more_levels_or_another_format_is_refused() {
    let (device, _) = device();
    let context = context(&device);
    let both = ImageUsage::SAMPLED.with(ImageUsage::STORAGE);
    let (_memory, writable) = image(&context, &image_desc(Format::Rgba8Unorm, both, 2, 1));
    let storage = entry(0, BindingKind::StorageImage(STORAGE_2D), READ_WRITE);
    let two = (Aspects::COLOR, 2, 1);
    let levels = view(
        &context,
        &writable,
        Format::Rgba8Unorm,
        ViewDimension::D2,
        two,
    );
    assert_eq!(
        binds(&context, storage, Resource::Image(&levels)),
        Some(Error::Shape),
        "a storage view of more than one level"
    );
    let srgb = view(
        &context,
        &writable,
        Format::Rgba8UnormSrgb,
        ViewDimension::D2,
        COLOUR,
    );
    assert_eq!(
        binds(&context, storage, Resource::Image(&srgb)),
        Some(Error::Unsupported),
        "sRGB is never stored"
    );
    let plain = view(
        &context,
        &writable,
        Format::Rgba8Unorm,
        ViewDimension::D2,
        COLOUR,
    );
    assert_eq!(binds(&context, storage, Resource::Image(&plain)), None);
    let r32 = shape(Dim::DIM_2D, false, Component::Float, ImageFormat::R32F);
    let other = entry(0, BindingKind::StorageImage(r32), READ_WRITE);
    assert_eq!(
        binds(&context, other, Resource::Image(&plain)),
        Some(Error::Shape),
        "another storage format"
    );
    let mut unknown = STORAGE_2D;
    unknown.format = ImageFormat::UNKNOWN;
    let any = entry(0, BindingKind::StorageImage(unknown), READ_WRITE);
    assert_eq!(binds(&context, any, Resource::Image(&plain)), None);
}

#[test]
fn a_depth_stencil_view_is_sampled_one_plane_at_a_time() {
    let (device, _) = device();
    let context = context(&device);
    let usage = ImageUsage::SAMPLED.with(ImageUsage::DEPTH_STENCIL_ATTACHMENT);
    let format = Format::Depth32FloatStencil8;
    let (_memory, stencilled) = image(&context, &image_desc(format, usage, 1, 1));
    let plane = |aspects| {
        view(
            &context,
            &stencilled,
            format,
            ViewDimension::D2,
            (aspects, 1, 1),
        )
    };
    let (both, depth, stencil) = (
        plane(Aspects::DEPTH_STENCIL),
        plane(Aspects::DEPTH),
        plane(Aspects::STENCIL),
    );
    let floats = entry(0, BindingKind::SampledImage(SAMPLED_2D), READ);
    let uints = entry(0, BindingKind::SampledImage(UINT_2D), READ);
    assert_eq!(
        binds(&context, floats, Resource::Image(&both)),
        Some(Error::Aspect),
        "two planes at once"
    );
    assert_eq!(binds(&context, floats, Resource::Image(&depth)), None);
    assert_eq!(binds(&context, uints, Resource::Image(&stencil)), None);
    assert_eq!(
        binds(&context, floats, Resource::Image(&stencil)),
        Some(Error::Shape),
        "a stencil reads as integers"
    );
}

const NEAREST: SamplerDesc = SamplerDesc {
    mag_filter: Filter::Nearest,
    min_filter: Filter::Nearest,
    mipmap_filter: Filter::Nearest,
    ..SamplerDesc::LINEAR
};

/// An integer image of two layers, made for sampling, and a view of its
/// first.
fn integers(context: &Context) -> (crate::Allocation, crate::Image, crate::ImageView) {
    let desc = image_desc(Format::R32Uint, ImageUsage::SAMPLED, 1, 2);
    let (memory, picture) = image(context, &desc);
    let first = view(
        context,
        &picture,
        Format::R32Uint,
        ViewDimension::D2,
        COLOUR,
    );
    (memory, picture, first)
}

#[test]
fn a_combined_sampler_filters_and_compares_only_what_its_view_holds() {
    let (device, _) = device();
    let context = context(&device);
    let (_memory, _image, uint) = integers(&context);
    let combined = entry(0, BindingKind::CombinedImageSampler(UINT_2D), READ);
    let sampler = |desc: &SamplerDesc| context.create_sampler(desc).expect("a sampler");
    let (linear, point) = (sampler(&SamplerDesc::LINEAR), sampler(&NEAREST));
    assert_eq!(
        binds(&context, combined, Resource::ImageSampler(&uint, &linear)),
        Some(Error::Unsupported),
        "integers are never filtered"
    );
    assert_eq!(
        binds(&context, combined, Resource::ImageSampler(&uint, &point)),
        None
    );
    let comparing = sampler(&SamplerDesc {
        compare: Some(CompareOp::LessOrEqual),
        ..NEAREST
    });
    assert_eq!(
        binds(
            &context,
            combined,
            Resource::ImageSampler(&uint, &comparing)
        ),
        Some(Error::Aspect),
        "a comparison of what is not depth"
    );
}

#[test]
fn unnormalised_coordinates_take_one_level_and_layer_of_a_rectangle() {
    let (device, _) = device();
    let context = context(&device);
    let (_memory, picture, uint) = integers(&context);
    let unnormalized = context
        .create_sampler(&SamplerDesc {
            unnormalized: true,
            lod_max: 0.0,
            ..NEAREST
        })
        .expect("a sampler");
    let combined = entry(0, BindingKind::CombinedImageSampler(UINT_2D), READ);
    assert_eq!(
        binds(
            &context,
            combined,
            Resource::ImageSampler(&uint, &unnormalized)
        ),
        None
    );
    let layers = (Aspects::COLOR, 1, 2);
    let layered = view(
        &context,
        &picture,
        Format::R32Uint,
        ViewDimension::D2Array,
        layers,
    );
    let mut arrayed = UINT_2D;
    arrayed.arrayed = true;
    let over_layers = entry(0, BindingKind::CombinedImageSampler(arrayed), READ);
    assert_eq!(
        binds(
            &context,
            over_layers,
            Resource::ImageSampler(&layered, &unnormalized)
        ),
        Some(Error::Shape),
        "unnormalised coordinates over layers"
    );
}

#[test]
fn a_buffer_range_its_binding_cannot_take_is_refused() {
    let (device, _) = device();
    let context = context(&device);
    let limits = device.info().limits;
    let large = usize::try_from(limits.max_uniform_buffer_range).expect("small") * 2;
    let allocation = memory(&context, large as u64);
    let buffer = over(&context, &allocation, 0, large as u64);
    let uniform = EVERY_KIND[6];
    let range = |offset, size| Resource::Buffer {
        buffer: &buffer,
        offset,
        size,
    };
    assert_eq!(binds(&context, uniform, range(0, 64)), None);
    assert_eq!(binds(&context, uniform, range(0, 0)), Some(Error::Empty));
    assert_eq!(
        binds(
            &context,
            uniform,
            range(limits.uniform_buffer_alignment / 2, 64)
        ),
        Some(Error::Misaligned)
    );
    assert_eq!(
        binds(&context, uniform, range(large as u64 - 64, 128)),
        Some(Error::OutOfRange)
    );
    assert_eq!(
        binds(
            &context,
            uniform,
            range(0, limits.max_uniform_buffer_range + 16)
        ),
        Some(Error::TooLarge)
    );
    let storage_only = context
        .create_buffer(
            &allocation,
            &crate::BufferDesc {
                offset: 0,
                size: 256,
                usage: BufferUsage::STORAGE.with(BufferUsage::STORAGE_TEXEL),
            },
        )
        .expect("a storage buffer");
    assert_eq!(
        binds(
            &context,
            uniform,
            Resource::Buffer {
                buffer: &storage_only,
                offset: 0,
                size: 64,
            }
        ),
        Some(Error::Usage)
    );
    let texels = context
        .create_buffer_view(
            &storage_only,
            &BufferViewDesc {
                offset: 0,
                size: 64,
                format: Format::Rgba8Unorm,
            },
        )
        .expect("a storage texel buffer");
    assert_eq!(
        binds(&context, EVERY_KIND[4], Resource::TexelBuffer(&texels)),
        Some(Error::Usage),
        "a uniform texel buffer over a buffer made for storage texels"
    );
    assert_eq!(
        binds(&context, EVERY_KIND[5], Resource::TexelBuffer(&texels)),
        None
    );
}

#[test]
fn a_resource_of_another_context_is_refused() {
    let (device, _) = device();
    let (mine, theirs) = (context(&device), context(&device));
    let layout = mine
        .create_bind_set_layout(&EVERY_KIND[..1])
        .expect("a layout");
    let foreign = theirs
        .create_sampler(&SamplerDesc::LINEAR)
        .expect("a sampler");
    let entry = [BindSetEntry {
        binding: 0,
        resources: &[Resource::Sampler(&foreign)],
    }];
    assert_eq!(
        mine.create_bind_set(&layout, &entry).err(),
        Some(Error::Foreign)
    );
    assert_eq!(
        theirs.create_bind_set(&layout, &entry).err(),
        Some(Error::Foreign),
        "a layout of another context"
    );
    assert_eq!(
        theirs
            .create_pipeline_layout(&[&layout], PushConstants::NONE)
            .err(),
        Some(Error::Foreign)
    );
}

#[test]
fn a_bind_set_holds_what_it_binds_and_through_it_its_memory() {
    let (device, _) = device();
    let context = context(&device);
    let round = || {
        let layout = context
            .create_bind_set_layout(&EVERY_KIND)
            .expect("every kind");
        let held = resources(&context);
        let set = {
            let each = held.each();
            context
                .create_bind_set(&layout, &entries(&each))
                .expect("a whole set")
        };
        let used = context.memory_used().allocations;
        assert!(used > 0);
        drop(held);
        drop(layout);
        assert_eq!(
            context.memory_used().allocations,
            used,
            "the set holds the memory its resources are made over"
        );
        drop(set);
        assert_eq!(context.memory_used().allocations, 0);
        context.memory_used()
    };
    let kept = round();
    assert_eq!(
        round(),
        kept,
        "and lets it and every object it held go with itself, the room its tables grew to kept"
    );
}

/// A compute shader reading a uniform buffer at set 0, binding 1, and a
/// storage buffer at set 1, binding 0, with eight bytes of push constants.
const COMPUTE: &str = r#"
OpCapability Shader
OpMemoryModel Logical GLSL450
OpEntryPoint GLCompute %main "main" %ubo %ssbo %constants
OpExecutionMode %main LocalSize 1 1 1
OpDecorate %ubo DescriptorSet 0
OpDecorate %ubo Binding 1
OpDecorate %ssbo DescriptorSet 1
OpDecorate %ssbo Binding 0
OpDecorate %block Block
OpMemberDecorate %block 0 Offset 0
OpDecorate %push Block
OpMemberDecorate %push 0 Offset 0
OpMemberDecorate %push 1 Offset 4
%void = OpTypeVoid
%fn = OpTypeFunction %void
%uint = OpTypeInt 32 0
%zero = OpConstant %uint 0
%one = OpConstant %uint 1
%block = OpTypeStruct %uint
%push = OpTypeStruct %uint %uint
%ubo_ptr = OpTypePointer Uniform %block
%ssbo_ptr = OpTypePointer StorageBuffer %block
%push_ptr = OpTypePointer PushConstant %push
%uint_uniform = OpTypePointer Uniform %uint
%uint_storage = OpTypePointer StorageBuffer %uint
%uint_push = OpTypePointer PushConstant %uint
%ubo = OpVariable %ubo_ptr Uniform
%ssbo = OpVariable %ssbo_ptr StorageBuffer
%constants = OpVariable %push_ptr PushConstant
%main = OpFunction %void None %fn
%entry = OpLabel
%in = OpAccessChain %uint_uniform %ubo %zero
%value = OpLoad %uint %in
%extra = OpAccessChain %uint_push %constants %one
%more = OpLoad %uint %extra
%sum = OpIAdd %uint %value %more
%out = OpAccessChain %uint_storage %ssbo %zero
OpStore %out %sum
OpReturn
OpFunctionEnd
"#;

#[test]
fn a_pipeline_layout_is_what_an_entry_point_is_agreed_against() {
    let (device, _) = device();
    let context = context(&device);
    let module = context
        .create_shader_module(admit(COMPUTE).expect("admitted"))
        .expect("a module");
    let entry_point = &module.module().entry_points()[0];
    let uniform = LayoutBinding {
        binding: 1,
        ..EVERY_KIND[6]
    };
    let storage = LayoutBinding {
        binding: 0,
        ..EVERY_KIND[7]
    };
    let compute = |binding: LayoutBinding| LayoutBinding {
        stages: Stages::COMPUTE,
        ..binding
    };
    let push = PushConstants {
        bytes: 8,
        stages: Stages::COMPUTE,
    };
    let agreement = |sets: &[&crate::BindSetLayout], push| {
        let layout = context
            .create_pipeline_layout(sets, push)
            .expect("a pipeline layout");
        let model = context.shared.model.lock();
        let state = model
            .live(context.key)
            .expect("live")
            .pipeline_layouts
            .get(layout.key)
            .cloned()
            .expect("filed");
        drop(model);
        entry_point.agree(&state.agreement())
    };
    let set = |binding| context.create_bind_set_layout(&[binding]).expect("a set");
    let (first, second) = (set(compute(uniform)), set(compute(storage)));
    assert_eq!(agreement(&[&first, &second], push), Ok(()));
    let vertex = set(LayoutBinding {
        stages: Stages::VERTEX,
        ..storage
    });
    assert_eq!(
        agreement(&[&first, &vertex], push),
        Err(Disagreement::Stage(1, 0)),
        "a binding the compute stage does not see"
    );
    let fragment = PushConstants {
        stages: Stages::FRAGMENT,
        ..push
    };
    assert_eq!(
        agreement(&[&first, &second], fragment),
        Err(Disagreement::PushConstants)
    );
    assert_eq!(
        agreement(&[&second, &first], push),
        Err(Disagreement::Missing(0, 1)),
        "sets are numbered by their place"
    );
}

#[test]
fn a_pipeline_layout_past_the_devices_bounds_is_refused() {
    let (device, _) = device();
    let context = context(&device);
    let limits = device.info().limits;
    let empty = context.create_bind_set_layout(&[]).expect("an empty set");
    let too_many: alloc::vec::Vec<_> = (0..=limits.max_bind_sets).map(|_| &empty).collect();
    assert_eq!(
        context
            .create_pipeline_layout(&too_many, PushConstants::NONE)
            .err(),
        Some(Error::TooMany)
    );
    let push = |bytes, stages| PushConstants { bytes, stages };
    let cases = [
        (
            push(limits.max_push_constant_bytes + 4, Stages::ALL),
            Error::TooLarge,
        ),
        (push(6, Stages::ALL), Error::Misaligned),
        (push(8, Stages::NONE), Error::Empty),
        (push(0, Stages::VERTEX), Error::Empty),
        (push(8, Stages::from_bits(0x80)), Error::Shape),
    ];
    for (push, error) in cases {
        assert_eq!(
            context.create_pipeline_layout(&[&empty], push).err(),
            Some(error)
        );
    }
    context
        .create_pipeline_layout(
            &too_many[..too_many.len() - 1],
            push(limits.max_push_constant_bytes, Stages::ALL),
        )
        .expect("at the bounds");
}

#[test]
fn a_shader_module_needs_every_capability_it_declares_run() {
    const DOUBLES: &str = r#"
OpCapability Shader
OpCapability Float64
OpMemoryModel Logical GLSL450
OpEntryPoint GLCompute %main "main"
OpExecutionMode %main LocalSize 1 1 1
%void = OpTypeVoid
%fn = OpTypeFunction %void
%double = OpTypeFloat 64
%main = OpFunction %void None %fn
%entry = OpLabel
OpReturn
OpFunctionEnd
"#;
    let (device, _) = device();
    context(&device)
        .create_shader_module(admit(DOUBLES).expect("admitted"))
        .expect("the software device runs every capability");
    let singles = device_stating(|info| {
        info.capabilities.retain(|&c| c != Capability::FLOAT64);
    });
    assert_eq!(
        context(&singles)
            .create_shader_module(admit(DOUBLES).expect("admitted"))
            .err(),
        Some(Error::Unsupported)
    );
}

#[test]
fn a_cube_array_view_needs_a_device_that_samples_cube_arrays() {
    let cubes = ViewDimension::CubeArray;
    let desc = image_desc(Format::Rgba8Unorm, ImageUsage::SAMPLED, 1, 12);
    let view = view_desc(Format::Rgba8Unorm, cubes, Aspects::COLOR, 1, 12);
    let (device, _) = device();
    let able = context(&device);
    let (_m0, picture) = image(&able, &desc);
    able.create_image_view(&picture, &view)
        .expect("the software device samples cube arrays");
    let unable = device_stating(|info| {
        info.capabilities
            .retain(|&c| c != Capability::SAMPLED_CUBE_ARRAY && c != Capability::IMAGE_CUBE_ARRAY);
    });
    let context = context(&unable);
    let (_m1, picture) = image(&context, &desc);
    assert_eq!(
        context.create_image_view(&picture, &view).err(),
        Some(Error::Unsupported)
    );
}

#[test]
fn a_layout_is_checked_without_the_devices_lock_and_bounded_in_descriptors() {
    let (device, _) = device();
    let context = context(&device);
    let numbers = device.info().limits.max_binding_number;
    let long: alloc::vec::Vec<LayoutBinding> = (0..numbers + 64)
        .map(|number| entry(number, BindingKind::UniformBuffer, READ))
        .collect();
    let before = crate::fair::held::takings();
    assert_eq!(
        context.create_bind_set_layout(&long).err(),
        Some(Error::OutOfRange)
    );
    assert_eq!(crate::fair::held::takings(), before, "refused before it");
    let most = device.info().limits.max_bind_set_descriptors;
    let mut uniform = EVERY_KIND[6];
    uniform.count = most;
    assert!(context.create_bind_set_layout(&[uniform]).is_ok());
    uniform.count = most + 1;
    assert_eq!(
        context.create_bind_set_layout(&[uniform]).err(),
        Some(Error::TooMany)
    );
    let mut split = [EVERY_KIND[6], EVERY_KIND[7]];
    split[0].count = most / 2;
    split[1].count = most / 2 + 1;
    assert_eq!(
        context.create_bind_set_layout(&split).err(),
        Some(Error::TooMany),
        "the bound is the layout's, its bindings' together"
    );
}

#[test]
fn a_set_past_its_contexts_limit_is_refused_before_its_descriptors_are_looked_up() {
    let (device, _) = device();
    let tight = device
        .create_context(&crate::ContextDesc {
            memory_limit: Some(64 << 10),
        })
        .expect("a context");
    let mut uniform = EVERY_KIND[6];
    uniform.count = 4096;
    let layout = tight.create_bind_set_layout(&[uniform]).expect("a layout");
    let allocation = memory(&tight, 256);
    let stored = tight
        .create_buffer(
            &allocation,
            &crate::BufferDesc {
                offset: 0,
                size: 256,
                usage: BufferUsage::STORAGE,
            },
        )
        .expect("a buffer");
    let resources = alloc::vec![
        Resource::Buffer {
            buffer: &stored,
            offset: 0,
            size: 256,
        };
        4096
    ];
    let entries = [BindSetEntry {
        binding: 6,
        resources: &resources,
    }];
    assert_eq!(
        tight.create_bind_set(&layout, &entries).err(),
        Some(Error::OverLimit),
        "no buffer of the wrong use is reached"
    );
}

#[test]
fn a_sets_reach_merges_its_spans_and_meets_only_the_bytes_they_take() {
    use super::{merged, Reach};
    // Two memories: in the first, spans that overlap, touch, and stand apart,
    // one of them written; in the second, a span written.
    let spans = [
        (0u32, 0u64..16, false),
        (0, 8..24, true),
        (0, 24..32, false),
        (0, 64..80, false),
        (1, 100..200, true),
    ];
    let reach = Reach {
        any: merged(&spans, |_| true).expect("merged"),
        written: merged(&spans, |(_, _, writes)| *writes).expect("merged"),
    };
    assert_eq!(reach.any, [(0, 0..32), (0, 64..80), (1, 100..200)]);
    assert_eq!(reach.written, [(0, 8..24), (1, 100..200)]);
    for (place, span, written, meets) in [
        (0, 0..1, false, true),
        (0, 31..40, false, true),
        (0, 32..64, false, false),
        (0, 79..80, false, true),
        (0, 80..90, false, false),
        (1, 0..100, false, false),
        (1, 199..300, false, true),
        (2, 0..u64::MAX, false, false),
        (0, 0..8, true, false),
        (0, 23..24, true, true),
        (0, 24..80, true, false),
        (1, 150..151, true, true),
    ] {
        assert_eq!(
            reach.meets(place, &span, written),
            meets,
            "{span:?} of memory {place}, written {written}"
        );
    }
    assert!(!Reach::default().meets(0, &(0..u64::MAX), false));
}
