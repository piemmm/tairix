//! Images from a shader: a storage image written, read back, read past its
//! edge and added to through texel pointers; a float image fetched, sampled
//! nearest and linear, projectively, gathered and queried through separate
//! and combined samplers; a depth image compared against; and a
//! multisampled image's samples counted.

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write as _;

use tairix_spirv::assemble::admit;
use tairix_spirv::enumerant::{Dim, ImageFormat};
use tairix_spirv::{Access, BindingKind, Component, ImageShape, Stages};

use super::f;
use crate::device_tests::{context, device, memory, over, queue, submit};
use crate::image_device_tests::image;
use crate::{
    Aspects, BindSetEntry, BufferImageCopy, CompareOp, ComputePipelineDesc, Context, Extent3d,
    Filter, Format, ImageDesc, ImageDimension, ImageRange, ImageSubresource, ImageUsage,
    ImageViewDesc, LayoutBinding, Origin3d, PushConstants, Resource, SamplerDesc, ViewDimension,
};

/// What every shader here shares: its capabilities, its output buffer at
/// binding 0, the types and constants they use, and each output word's
/// index `%k0` to `%k15`.
pub(in crate::software::shader::tests) fn module(
    interface: &str,
    annotations: &str,
    decls: &str,
    body: &str,
    lanes: u32,
) -> String {
    let mut indexes = String::new();
    for k in 0..16 {
        let _ = writeln!(indexes, "%k{k} = OpConstant %uint {k}");
    }
    alloc::format!(
        r#"OpCapability Shader
OpCapability ImageQuery
OpCapability StorageImageExtendedFormats
OpMemoryModel Logical GLSL450
OpEntryPoint GLCompute %main "main" %gid %out {interface}
OpExecutionMode %main LocalSize {lanes} 1 1
OpDecorate %gid BuiltIn GlobalInvocationId
OpDecorate %out DescriptorSet 0
OpDecorate %out Binding 0
OpDecorate %block Block
OpMemberDecorate %block 0 Offset 0
OpDecorate %words ArrayStride 4
{annotations}
%void = OpTypeVoid
%fn = OpTypeFunction %void
%uint = OpTypeInt 32 0
%int = OpTypeInt 32 1
%float = OpTypeFloat 32
%v2uint = OpTypeVector %uint 2
%v3uint = OpTypeVector %uint 3
%v4uint = OpTypeVector %uint 4
%v2int = OpTypeVector %int 2
%v2float = OpTypeVector %float 2
%v3float = OpTypeVector %float 3
%v4float = OpTypeVector %float 4
%zero = OpConstant %uint 0
%one = OpConstant %uint 1
%four = OpConstant %uint 4
%izero = OpConstant %int 0
%ione = OpConstant %int 1
%fzero = OpConstant %float 0.0
{indexes}%words = OpTypeRuntimeArray %uint
%block = OpTypeStruct %words
%block_ptr = OpTypePointer StorageBuffer %block
%word_ptr = OpTypePointer StorageBuffer %uint
%gid_ptr = OpTypePointer Input %v3uint
%gid = OpVariable %gid_ptr Input
%out = OpVariable %block_ptr StorageBuffer
{decls}
%main = OpFunction %void None %fn
%entry = OpLabel
%gidv = OpLoad %v3uint %gid
%lane = OpCompositeExtract %uint %gidv 0
%base = OpIMul %uint %lane %four
{body}
OpReturn
OpFunctionEnd
"#
    )
}

/// Instructions writing each of `values`, words, from output word
/// `base + k` on: `%base` for the lane's own, or zero.
pub(in crate::software::shader::tests) fn stores(values: &[&str], lane_base: bool) -> String {
    let mut text = String::new();
    for (k, value) in values.iter().enumerate() {
        if lane_base {
            let _ = writeln!(text, "%at{k} = OpIAdd %uint %base %k{k}\n%p{k} = OpAccessChain %word_ptr %out %zero %at{k}");
        } else {
            let _ = writeln!(text, "%p{k} = OpAccessChain %word_ptr %out %zero %k{k}");
        }
        let _ = writeln!(text, "OpStore %p{k} {value}");
    }
    text
}

/// A binding a test makes: its kind and its resources, as many as its
/// count.
pub(in crate::software::shader::tests) struct Bound<'a>(
    pub(super) BindingKind,
    pub(super) Vec<Resource<'a>>,
);

/// Run `text` over one workgroup with an output buffer of `words` words at
/// binding 0, `bound` from binding 1 on and `push` as its push constants,
/// answering what it wrote.
pub(in crate::software::shader::tests) fn dispatch(
    context: &Context,
    text: &str,
    bound: &[Bound<'_>],
    push: &[u8],
    words: usize,
) -> Vec<u32> {
    let module = context
        .create_shader_module(admit(text).unwrap_or_else(|r| panic!("{r:?} in\n{text}")))
        .expect("a module");
    let mut bindings = alloc::vec![LayoutBinding {
        binding: 0,
        kind: BindingKind::StorageBuffer,
        count: 1,
        access: Access::READ.with(Access::WRITE),
        stages: Stages::COMPUTE,
    }];
    for (at, Bound(kind, resources)) in (1u32..).zip(bound) {
        let writable = matches!(
            kind,
            BindingKind::StorageImage(_)
                | BindingKind::StorageBuffer
                | BindingKind::StorageTexelBuffer(_)
        );
        bindings.push(LayoutBinding {
            binding: at,
            kind: *kind,
            count: u32::try_from(resources.len()).expect("a few"),
            access: if writable {
                Access::READ.with(Access::WRITE)
            } else {
                Access::READ
            },
            stages: Stages::COMPUTE,
        });
    }
    let set_layout = context
        .create_bind_set_layout(&bindings)
        .expect("a set layout");
    let push_constants = PushConstants {
        bytes: u32::try_from(push.len()).expect("a few"),
        stages: if push.is_empty() {
            Stages::NONE
        } else {
            Stages::COMPUTE
        },
    };
    let layout = context
        .create_pipeline_layout(&[&set_layout], push_constants)
        .expect("a layout");
    let pipeline = context
        .create_compute_pipeline(&ComputePipelineDesc {
            module: &module,
            entry_point: "main",
            layout: &layout,
            specialization: &[],
        })
        .expect("a pipeline");
    let bytes = (words * 4) as u64;
    let held = memory(context, bytes);
    let out = over(context, &held, 0, bytes);
    let output = [Resource::Buffer {
        buffer: &out,
        offset: 0,
        size: bytes,
    }];
    let mut entries = alloc::vec![BindSetEntry {
        binding: 0,
        resources: &output
    }];
    for (binding, Bound(_, resources)) in (1u32..).zip(bound) {
        entries.push(BindSetEntry { binding, resources });
    }
    let set = context
        .create_bind_set(&set_layout, &entries)
        .expect("a set");
    let mut commands = context.command_buffer();
    commands
        .dispatch(&pipeline, &[&set], push, [1, 1, 1])
        .expect("recorded");
    submit(&queue(context), &commands).expect("dispatched");
    let mut raw = alloc::vec![0u8; words * 4];
    held.read(0, &mut raw).expect("readable");
    raw.as_chunks::<4>()
        .0
        .iter()
        .map(|w| u32::from_le_bytes(*w))
        .collect()
}

/// An image of `desc` whose every texel of plane `aspect` holds
/// `texel(level, layer, x, y)` in each of its words, or in its low bytes
/// where it is narrower, made to be copied to and from as well.
pub(in crate::software::shader::tests) fn filled(
    context: &Context,
    desc: &ImageDesc,
    aspect: Aspects,
    texel: impl Fn(u32, u32, u32, u32) -> u32,
) -> (crate::Allocation, crate::Image) {
    let desc = ImageDesc {
        usage: desc
            .usage
            .with(ImageUsage::TRANSFER_DST)
            .with(ImageUsage::TRANSFER_SRC),
        ..*desc
    };
    let (held, picture) = image(context, &desc);
    let bytes = desc.format.copy_bytes(aspect).expect("a plane") as usize;
    let mut commands = context.command_buffer();
    let mut staged = Vec::new();
    for (level, layer, extent) in planes(&desc) {
        let mut data = Vec::new();
        for y in 0..extent.height {
            for x in 0..extent.width {
                let word = texel(level, layer, x, y).to_le_bytes();
                data.extend((0..bytes).map(|byte| word[byte % 4]));
            }
        }
        let size = data.len() as u64;
        let staging_memory = memory(context, size);
        staging_memory.write(0, &data).expect("writable");
        let staging = over(context, &staging_memory, 0, size);
        commands
            .copy_buffer_to_image(
                &staging,
                &picture,
                &[copy(aspect, level, layer, extent, bytes)],
            )
            .expect("recorded");
        staged.push((staging_memory, staging));
    }
    submit(&queue(context), &commands).expect("uploaded");
    (held, picture)
}

/// Every texel of plane `aspect` of `picture`, made as `desc` says, level by
/// level and layer by layer, each row by row, a word apiece or each of a
/// wider one's words.
pub(in crate::software::shader::tests) fn texels(
    context: &Context,
    picture: &crate::Image,
    desc: &ImageDesc,
    aspect: Aspects,
) -> Vec<u32> {
    let bytes = desc.format.copy_bytes(aspect).expect("a plane") as usize;
    let mut out = Vec::new();
    for (level, layer, extent) in planes(desc) {
        let size = u64::from(extent.width * extent.height) * bytes as u64;
        let target_memory = memory(context, size.next_multiple_of(4));
        let target = over(context, &target_memory, 0, size.next_multiple_of(4));
        let mut commands = context.command_buffer();
        commands
            .copy_image_to_buffer(
                picture,
                &target,
                &[copy(aspect, level, layer, extent, bytes)],
            )
            .expect("recorded");
        submit(&queue(context), &commands).expect("read back");
        let mut raw = alloc::vec![0u8; usize::try_from(size).expect("a small image")];
        target_memory.read(0, &mut raw).expect("readable");
        for part in raw.chunks(4) {
            let mut word = [0u8; 4];
            word[..part.len()].copy_from_slice(part);
            out.push(u32::from_le_bytes(word));
        }
    }
    out
}

/// Each level and layer of a 2D image of `desc`, and its extent.
fn planes(desc: &ImageDesc) -> impl Iterator<Item = (u32, u32, Extent3d)> + '_ {
    (0..desc.mip_levels).flat_map(move |level| {
        (0..desc.array_layers).map(move |layer| (level, layer, desc.level_extent(level)))
    })
}

fn copy(
    aspect: Aspects,
    level: u32,
    layer: u32,
    extent: Extent3d,
    bytes: usize,
) -> BufferImageCopy {
    BufferImageCopy {
        buffer_offset: 0,
        bytes_per_row: extent.width * u32::try_from(bytes).expect("a texel's bytes"),
        rows_per_image: extent.height,
        image: ImageSubresource {
            aspect,
            level,
            base_layer: layer,
            layers: 1,
        },
        origin: Origin3d::default(),
        extent,
    }
}

/// A 4-by-4 2D image of `format` with `levels` levels, made for `usage`.
pub(in crate::software::shader::tests) const fn square(
    format: Format,
    usage: ImageUsage,
    levels: u32,
) -> ImageDesc {
    ImageDesc {
        dimension: ImageDimension::D2,
        format,
        extent: Extent3d {
            width: 4,
            height: 4,
            depth: 1,
        },
        mip_levels: levels,
        array_layers: 1,
        samples: 1,
        usage,
    }
}

pub(in crate::software::shader::tests) fn view(
    context: &Context,
    picture: &crate::Image,
    desc: &ImageViewDesc,
) -> crate::ImageView {
    context.create_image_view(picture, desc).expect("a view")
}

/// A 2D view of `levels` levels from the first of a 2D image's `aspects`.
pub(in crate::software::shader::tests) const fn plain(
    format: Format,
    aspects: Aspects,
    levels: u32,
) -> ImageViewDesc {
    ImageViewDesc {
        format,
        dimension: ViewDimension::D2,
        range: ImageRange {
            aspects,
            base_level: 0,
            levels,
            base_layer: 0,
            layers: 1,
        },
    }
}

/// A small texel value as a float.
pub(in crate::software::shader::tests) fn small(value: u32) -> f32 {
    f32::from(u16::try_from(value).expect("a small value"))
}

pub(in crate::software::shader::tests) const fn shape(
    component: Component,
    format: ImageFormat,
    multisampled: bool,
) -> ImageShape {
    ImageShape {
        dim: Dim::DIM_2D,
        arrayed: false,
        multisampled,
        component,
        format,
    }
}

pub(in crate::software::shader::tests) fn nearest() -> SamplerDesc {
    SamplerDesc {
        mag_filter: Filter::Nearest,
        min_filter: Filter::Nearest,
        mipmap_filter: Filter::Nearest,
        ..SamplerDesc::LINEAR
    }
}

const STORAGE_DECLS: &str = r"%image_t = OpTypeImage %uint 2D 0 0 0 2 R32ui
%image_ptr = OpTypePointer UniformConstant %image_t
%storage = OpVariable %image_ptr UniformConstant
%texel_ptr = OpTypePointer Image %uint
%ten = OpConstant %uint 10
%far = OpConstant %int 9
%device = OpConstant %uint 1
%sem = OpConstant %uint 2120
%origin = OpConstantComposite %v2int %izero %izero
%beyond = OpConstantComposite %v2int %far %far";

/// Each lane writes its texel, reads it back, reads past the image, and adds
/// one to the first texel through a texel pointer; then the image's width
/// and a missing alpha read past it are summed.
fn storage_body() -> String {
    let mut body = String::from(
        "%img = OpLoad %image_t %storage\n%xu = OpUMod %uint %lane %four\n%yu = OpUDiv %uint %lane %four\n%x = OpBitcast %int %xu\n%y = OpBitcast %int %yu\n%at = OpCompositeConstruct %v2int %x %y\n%tens = OpIMul %uint %yu %ten\n%value = OpIAdd %uint %xu %tens\n%texel = OpCompositeConstruct %v4uint %value %zero %zero %one\nOpImageWrite %img %at %texel\n%back = OpImageRead %v4uint %img %at\n%back0 = OpCompositeExtract %uint %back 0\n%past = OpImageRead %v4uint %img %beyond\n%past0 = OpCompositeExtract %uint %past 0\n%past3 = OpCompositeExtract %uint %past 3\n%tp = OpImageTexelPointer %texel_ptr %storage %origin %zero\n%old = OpAtomicIAdd %uint %tp %device %sem %one\n%size = OpImageQuerySize %v2int %img\n%w = OpCompositeExtract %int %size 0\n%wu = OpBitcast %uint %w\n%pw = OpIAdd %uint %past3 %wu\n",
    );
    body.push_str(&stores(&["%back0", "%past0", "%old", "%pw"], true));
    body
}

#[test]
fn a_storage_image_is_written_read_and_added_to_texel_by_texel() {
    let (device, _) = device();
    let context = context(&device);
    let usage = ImageUsage::STORAGE.with(ImageUsage::TRANSFER_SRC);
    let (_memory, picture) = filled(
        &context,
        &square(Format::R32Uint, usage, 1),
        Aspects::COLOR,
        |_, _, _, _| 0,
    );
    let texels = view(
        &context,
        &picture,
        &plain(Format::R32Uint, Aspects::COLOR, 1),
    );
    let text = module(
        "%storage",
        "OpDecorate %storage DescriptorSet 0\nOpDecorate %storage Binding 1",
        STORAGE_DECLS,
        &storage_body(),
        16,
    );
    let kind = BindingKind::StorageImage(shape(Component::Uint, ImageFormat::R32UI, false));
    let out = dispatch(
        &context,
        &text,
        &[Bound(kind, alloc::vec![Resource::Image(&texels)])],
        &[],
        64,
    );
    for lane in 0..16u32 {
        let at = lane as usize * 4;
        assert_eq!(
            out[at],
            lane % 4 + 10 * (lane / 4),
            "lane {lane} reads back its own texel"
        );
        assert_eq!(out[at + 1], 0, "past the image reads zero");
        assert_eq!(out[at + 2], lane, "the first texel's adds, in lane order");
        assert_eq!(
            out[at + 3],
            1 + 4,
            "a missing alpha reads one, and the image is 4 wide"
        );
    }
}

const WIDE_DECLS: &str = r"%wide_t = OpTypeImage %uint 2D 0 0 0 2 Unknown
%wide_ptr = OpTypePointer UniformConstant %wide_t
%wide = OpVariable %wide_ptr UniformConstant
%hundred = OpConstant %uint 100";

/// Each lane writes a scalar texel through a view of unstated format, among
/// values live in the registers about it, then reads all four channels back.
fn wide_body() -> String {
    let mut body = String::from("%img = OpLoad %wide_t %wide\n%x = OpBitcast %int %lane\n%at = OpCompositeConstruct %v2int %x %izero\n");
    let mut live = Vec::new();
    for k in 0..8 {
        let _ = writeln!(
            body,
            "%m{k} = OpIMul %uint %lane %k{k}\n%l{k} = OpIAdd %uint %m{k} %hundred"
        );
        live.push(alloc::format!("%l{k}"));
    }
    body.push_str("%texel = OpIAdd %uint %lane %one\nOpImageWrite %img %at %texel\n%back = OpImageRead %v4uint %img %at\n");
    let mut values = Vec::new();
    for k in 0..4 {
        let _ = writeln!(body, "%b{k} = OpCompositeExtract %uint %back {k}");
        values.push(alloc::format!("%b{k}"));
    }
    let mut sum = String::from("%zero");
    for value in &live {
        let next = alloc::format!("%s{}", &value[1..]);
        let _ = writeln!(body, "{next} = OpIAdd %uint {sum} {value}");
        sum = next;
    }
    values.push(sum);
    let names: Vec<&str> = values.iter().map(String::as_str).collect();
    let mut stores_text = stores(&names, true);
    stores_text = stores_text.replace("%base %k", "%five_base %k");
    body.push_str("%five_base = OpIMul %uint %lane %five\n");
    body.push_str(&stores_text);
    body
}

#[test]
fn a_texel_of_fewer_components_writes_zero_to_the_rest() {
    let (device, _) = device();
    let context = context(&device);
    let desc = ImageDesc {
        extent: Extent3d {
            width: 4,
            height: 1,
            depth: 1,
        },
        ..square(Format::Rgba32Uint, ImageUsage::STORAGE, 1)
    };
    let (_memory, picture) = filled(&context, &desc, Aspects::COLOR, |_, _, _, _| 0xdead);
    let texels = view(
        &context,
        &picture,
        &plain(Format::Rgba32Uint, Aspects::COLOR, 1),
    );
    let decls = alloc::format!("{WIDE_DECLS}\n%five = OpConstant %uint 5");
    let text = module("%wide", "OpDecorate %wide DescriptorSet 0\nOpDecorate %wide Binding 1", &decls, &wide_body(), 4).replacen(
        "OpCapability ImageQuery",
        "OpCapability ImageQuery\nOpCapability StorageImageReadWithoutFormat\nOpCapability StorageImageWriteWithoutFormat",
        1,
    );
    let kind = BindingKind::StorageImage(shape(Component::Uint, ImageFormat::UNKNOWN, false));
    let out = dispatch(
        &context,
        &text,
        &[Bound(kind, alloc::vec![Resource::Image(&texels)])],
        &[],
        20,
    );
    for lane in 0..4u32 {
        let at = lane as usize * 5;
        assert_eq!(out[at..at + 4], [lane + 1, 0, 0, 0], "lane {lane}");
        let live: u32 = (0..8).map(|k| lane * k + 100).sum();
        assert_eq!(out[at + 4], live, "lane {lane}'s live values");
    }
}

const SAMPLED_DECLS: &str = r"%image_t = OpTypeImage %float 2D 0 0 0 1 Unknown
%image_ptr = OpTypePointer UniformConstant %image_t
%sampled = OpVariable %image_ptr UniformConstant
%sampler_t = OpTypeSampler
%sampler_ptr = OpTypePointer UniformConstant %sampler_t
%nearest = OpVariable %sampler_ptr UniformConstant
%linear = OpVariable %sampler_ptr UniformConstant
%si_t = OpTypeSampledImage %image_t
%si_ptr = OpTypePointer UniformConstant %si_t
%combined = OpVariable %si_ptr UniformConstant
%f2 = OpConstant %float 2.0
%quarter = OpConstant %float 0.25
%f0375 = OpConstant %float 0.375
%f0625 = OpConstant %float 0.625
%f075 = OpConstant %float 0.75
%f125 = OpConstant %float 1.25
%center = OpConstantComposite %v2float %f0375 %f0625
%between = OpConstantComposite %v2float %quarter %f0625
%corner = OpConstantComposite %v2float %quarter %quarter
%projected = OpConstantComposite %v3float %f075 %f125 %f2
%fetch_at = OpConstantComposite %v2int %ione %ione";

/// The float image fetched, sampled nearest, linear and projectively,
/// gathered through its combined sampler, and queried.
fn sampled_body() -> String {
    let mut body = String::from(
        "%img = OpLoad %image_t %sampled\n%s_near = OpLoad %sampler_t %nearest\n%s_lin = OpLoad %sampler_t %linear\n%near = OpSampledImage %si_t %img %s_near\n%lin = OpSampledImage %si_t %img %s_lin\n%comb = OpLoad %si_t %combined\n%fetched = OpImageFetch %v4float %img %fetch_at Lod %izero\n%nearest_v = OpImageSampleExplicitLod %v4float %near %center Lod %fzero\n%linear_v = OpImageSampleExplicitLod %v4float %lin %between Lod %fzero\n%proj_v = OpImageSampleProjExplicitLod %v4float %near %projected Lod %fzero\n%gathered = OpImageGather %v4float %comb %corner %izero\n%plain = OpImage %image_t %comb\n%size = OpImageQuerySizeLod %v2int %plain %ione\n%levels = OpImageQueryLevels %int %img\n%width = OpCompositeExtract %int %size 0\n%width_u = OpBitcast %uint %width\n%levels_u = OpBitcast %uint %levels\n",
    );
    let mut values = Vec::new();
    for (k, (value, component)) in [
        ("%fetched", 0),
        ("%nearest_v", 0),
        ("%linear_v", 0),
        ("%proj_v", 0),
        ("%gathered", 0),
        ("%gathered", 1),
        ("%gathered", 2),
        ("%gathered", 3),
    ]
    .iter()
    .enumerate()
    {
        let _ = writeln!(
            body,
            "%e{k} = OpCompositeExtract %float {value} {component}\n%u{k} = OpBitcast %uint %e{k}"
        );
        values.push(alloc::format!("%u{k}"));
    }
    values.push("%width_u".into());
    values.push("%levels_u".into());
    let names: Vec<&str> = values.iter().map(String::as_str).collect();
    body.push_str(&stores(&names, false));
    body
}

#[test]
fn a_float_image_is_fetched_sampled_gathered_and_queried() {
    let (device, _) = device();
    let context = context(&device);
    let (_memory, picture) = filled(
        &context,
        &square(Format::R32Float, ImageUsage::SAMPLED, 3),
        Aspects::COLOR,
        |_, _, x, y| f(small(x + 4 * y)),
    );
    let texels = view(
        &context,
        &picture,
        &plain(Format::R32Float, Aspects::COLOR, 3),
    );
    let point = context.create_sampler(&nearest()).expect("a sampler");
    let linear = context
        .create_sampler(&SamplerDesc::LINEAR)
        .expect("a sampler");
    let decorations = "OpDecorate %sampled DescriptorSet 0\nOpDecorate %sampled Binding 1\nOpDecorate %nearest DescriptorSet 0\nOpDecorate %nearest Binding 2\nOpDecorate %linear DescriptorSet 0\nOpDecorate %linear Binding 3\nOpDecorate %combined DescriptorSet 0\nOpDecorate %combined Binding 4";
    let text = module(
        "%sampled %nearest %linear %combined",
        decorations,
        SAMPLED_DECLS,
        &sampled_body(),
        1,
    );
    let float_image = shape(Component::Float, ImageFormat::UNKNOWN, false);
    let out = dispatch(
        &context,
        &text,
        &[
            Bound(
                BindingKind::SampledImage(float_image),
                alloc::vec![Resource::Image(&texels)],
            ),
            Bound(BindingKind::Sampler, alloc::vec![Resource::Sampler(&point)]),
            Bound(
                BindingKind::Sampler,
                alloc::vec![Resource::Sampler(&linear)],
            ),
            Bound(
                BindingKind::CombinedImageSampler(float_image),
                alloc::vec![Resource::ImageSampler(&texels, &point)],
            ),
        ],
        &[],
        16,
    );
    assert_eq!(out[0], f(5.0), "the texel at (1, 1)");
    assert_eq!(out[1], f(9.0), "the nearest texel to (1.5, 2.5)");
    assert_eq!(out[2], f(8.5), "halfway between (0, 2) and (1, 2)");
    assert_eq!(out[3], f(9.0), "projected by its last coordinate");
    assert_eq!(
        out[4..8],
        [f(4.0), f(5.0), f(1.0), f(0.0)],
        "the footprint at (1, 1)"
    );
    assert_eq!(out[8], 2, "level 1 is 2 wide");
    assert_eq!(out[9], 3, "three levels");
}

const DEPTH_DECLS: &str = r"%image_t = OpTypeImage %float 2D 1 0 0 1 Unknown
%si_t = OpTypeSampledImage %image_t
%si_ptr = OpTypePointer UniformConstant %si_t
%depth = OpVariable %si_ptr UniformConstant
%f0375 = OpConstant %float 0.375
%f0625 = OpConstant %float 0.625
%quarter = OpConstant %float 0.25
%f2 = OpConstant %float 2.0
%f075 = OpConstant %float 0.75
%f125 = OpConstant %float 1.25
%above = OpConstant %float 0.6
%below = OpConstant %float 0.5
%twice_below = OpConstant %float 1.0
%low = OpConstant %float 0.1
%center = OpConstantComposite %v2float %f0375 %f0625
%corner = OpConstantComposite %v2float %quarter %quarter
%projected = OpConstantComposite %v3float %f075 %f125 %f2";

/// References compared against the depth image: sampled, projected and
/// gathered.
fn depth_body() -> String {
    let mut body = String::from(
        "%d = OpLoad %si_t %depth\n%fail = OpImageSampleDrefExplicitLod %float %d %center %above Lod %fzero\n%pass = OpImageSampleDrefExplicitLod %float %d %center %below Lod %fzero\n%proj = OpImageSampleProjDrefExplicitLod %float %d %projected %twice_below Lod %fzero\n%gathered = OpImageDrefGather %v4float %d %corner %low\n%g0 = OpCompositeExtract %float %gathered 0\n%g3 = OpCompositeExtract %float %gathered 3\n",
    );
    let mut values = Vec::new();
    for (k, value) in ["%fail", "%pass", "%proj", "%g0", "%g3"].iter().enumerate() {
        let _ = writeln!(body, "%u{k} = OpBitcast %uint {value}");
        values.push(alloc::format!("%u{k}"));
    }
    let names: Vec<&str> = values.iter().map(String::as_str).collect();
    body.push_str(&stores(&names, false));
    body
}

#[test]
fn a_depth_image_is_compared_against_sampled_projected_and_gathered() {
    let (device, _) = device();
    let context = context(&device);
    let (_memory, picture) = filled(
        &context,
        &square(Format::Depth32Float, ImageUsage::SAMPLED, 1),
        Aspects::DEPTH,
        |_, _, x, y| f(small(x + 4 * y) / 16.0),
    );
    let depth = view(
        &context,
        &picture,
        &plain(Format::Depth32Float, Aspects::DEPTH, 1),
    );
    let compare = context
        .create_sampler(&SamplerDesc {
            compare: Some(CompareOp::LessOrEqual),
            ..nearest()
        })
        .expect("a sampler");
    let text = module(
        "%depth",
        "OpDecorate %depth DescriptorSet 0\nOpDecorate %depth Binding 1",
        DEPTH_DECLS,
        &depth_body(),
        1,
    );
    let kind =
        BindingKind::CombinedImageSampler(shape(Component::Float, ImageFormat::UNKNOWN, false));
    let out = dispatch(
        &context,
        &text,
        &[Bound(
            kind,
            alloc::vec![Resource::ImageSampler(&depth, &compare)],
        )],
        &[],
        8,
    );
    assert_eq!(out[0], f(0.0), "0.6 is not at most the texel's 0.5625");
    assert_eq!(out[1], f(1.0), "0.5 is");
    assert_eq!(out[2], f(1.0), "the reference is projected too: 1.0 / 2");
    assert_eq!(out[3], f(1.0), "0.1 is at most (0, 1)'s 0.25");
    assert_eq!(out[4], f(0.0), "and not at most (0, 0)'s 0.0");
}

const CUBE_DECLS: &str = r"%cube_t = OpTypeImage %float Cube 0 0 0 1 Unknown
%cube_si = OpTypeSampledImage %cube_t
%cube_ptr = OpTypePointer UniformConstant %cube_si
%cube = OpVariable %cube_ptr UniformConstant
%f1 = OpConstant %float 1.0
%near_edge = OpConstant %float 0.9
%centre = OpConstantComposite %v3float %f1 %fzero %fzero
%edge = OpConstantComposite %v3float %f1 %fzero %near_edge";

/// The footprint of the +X face's centre, and of a point near its edge with
/// +Z, gathered.
fn cube_body() -> String {
    let mut body = String::from(
        "%c = OpLoad %cube_si %cube\n%inside = OpImageGather %v4float %c %centre %izero\n%across = OpImageGather %v4float %c %edge %izero\n",
    );
    let mut values = Vec::new();
    for (k, (value, component)) in [
        ("%inside", 0),
        ("%inside", 1),
        ("%inside", 2),
        ("%inside", 3),
        ("%across", 0),
        ("%across", 1),
        ("%across", 2),
        ("%across", 3),
    ]
    .iter()
    .enumerate()
    {
        let _ = writeln!(
            body,
            "%e{k} = OpCompositeExtract %float {value} {component}\n%u{k} = OpBitcast %uint %e{k}"
        );
        values.push(alloc::format!("%u{k}"));
    }
    let names: Vec<&str> = values.iter().map(String::as_str).collect();
    body.push_str(&stores(&names, false));
    body
}

#[test]
fn a_cube_is_gathered_from_its_face_and_the_face_beside_it() {
    let (device, _) = device();
    let context = context(&device);
    let desc = ImageDesc {
        extent: Extent3d {
            width: 2,
            height: 2,
            depth: 1,
        },
        array_layers: 6,
        ..square(Format::R32Float, ImageUsage::SAMPLED, 1)
    };
    let (_memory, picture) = filled(&context, &desc, Aspects::COLOR, |_, face, x, y| {
        f(small(face * 10 + y * 2 + x))
    });
    let faces = view(
        &context,
        &picture,
        &ImageViewDesc {
            format: Format::R32Float,
            dimension: ViewDimension::Cube,
            range: ImageRange {
                aspects: Aspects::COLOR,
                base_level: 0,
                levels: 1,
                base_layer: 0,
                layers: 6,
            },
        },
    );
    let point = context.create_sampler(&nearest()).expect("a sampler");
    let text = module(
        "%cube",
        "OpDecorate %cube DescriptorSet 0\nOpDecorate %cube Binding 1",
        CUBE_DECLS,
        &cube_body(),
        1,
    );
    let kind = BindingKind::CombinedImageSampler(ImageShape {
        dim: Dim::CUBE,
        ..shape(Component::Float, ImageFormat::UNKNOWN, false)
    });
    let out = dispatch(
        &context,
        &text,
        &[Bound(
            kind,
            alloc::vec![Resource::ImageSampler(&faces, &point)],
        )],
        &[],
        8,
    );
    assert_eq!(
        out[..4],
        [2.0, 3.0, 1.0, 0.0].map(f),
        "the centre's four texels, not one of them four times"
    );
    assert_eq!(
        out[4..],
        [43.0, 2.0, 0.0, 41.0].map(f),
        "the column past the edge from +Z"
    );
}

const SAMPLES_DECLS: &str = r"%image_t = OpTypeImage %float 2D 0 0 1 1 Unknown
%image_ptr = OpTypePointer UniformConstant %image_t
%ms = OpVariable %image_ptr UniformConstant";

fn samples_body() -> String {
    alloc::format!(
        "%img = OpLoad %image_t %ms\n%count = OpImageQuerySamples %int %img\n%cu = OpBitcast %uint %count\n{}",
        stores(&["%cu"], false)
    )
}

#[test]
fn a_multisampled_image_counts_its_samples() {
    let (device, _) = device();
    let context = context(&device);
    let desc = ImageDesc {
        samples: 4,
        ..square(
            Format::Rgba8Unorm,
            ImageUsage::SAMPLED.with(ImageUsage::COLOR_ATTACHMENT),
            1,
        )
    };
    let (_memory, picture) = image(&context, &desc);
    let samples = view(
        &context,
        &picture,
        &plain(Format::Rgba8Unorm, Aspects::COLOR, 1),
    );
    let text = module(
        "%ms",
        "OpDecorate %ms DescriptorSet 0\nOpDecorate %ms Binding 1",
        SAMPLES_DECLS,
        &samples_body(),
        1,
    );
    let kind = BindingKind::SampledImage(shape(Component::Float, ImageFormat::UNKNOWN, true));
    let out = dispatch(
        &context,
        &text,
        &[Bound(kind, alloc::vec![Resource::Image(&samples)])],
        &[],
        4,
    );
    assert_eq!(out[0], 4);
}

/// The shaders this family runs.
pub(super) fn bodies() -> Vec<String> {
    alloc::vec![
        storage_body(),
        wide_body(),
        sampled_body(),
        depth_body(),
        cube_body(),
        samples_body()
    ]
}
