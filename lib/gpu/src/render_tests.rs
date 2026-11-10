//! Render pipelines and passes on the software device: made, refused, and
//! drawn, their pixels pinned.

use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use tairix_spirv::assemble::admit;

use crate::device_tests::{context, memory, over, queue, software, submit};
use crate::image_device_tests::image;
use crate::{
    Aspects, BufferImageCopy, ClearValue, ColourAttachment, ColourMask, ColourTarget, Context,
    CullMode, Draw, Extent3d, Format, FrontFace, ImageDesc, ImageDimension, ImageRange,
    ImageSubresource, ImageUsage, ImageViewDesc, Load, Multisample, Origin3d, Pass, PipelineLayout,
    PushConstants, Rasterization, Rect, RenderPipeline, RenderPipelineDesc, StageDesc, Topology,
    VertexAttribute, VertexBuffer, VertexStep, ViewDimension, Viewport,
};

/// A device of room enough for a few small targets and their passes.
pub(crate) fn rendering() -> crate::Device {
    software(
        32 << 20,
        alloc::sync::Arc::new(crate::device_tests::Clock::default()),
    )
}

/// An image to render to, its memory and the view a pass names.
pub(crate) struct Target {
    pub(crate) image: crate::Image,
    pub(crate) view: crate::ImageView,
    _memory: crate::Allocation,
    pub(crate) format: Format,
    pub(crate) extent: (u32, u32),
}

pub(crate) fn target(
    context: &Context,
    format: Format,
    extent: (u32, u32),
    samples: u32,
) -> Target {
    let aspects = format.aspects();
    let usage = if aspects == Aspects::COLOR {
        ImageUsage::COLOR_ATTACHMENT
    } else {
        ImageUsage::DEPTH_STENCIL_ATTACHMENT
    };
    let (memory, image) = image(
        context,
        &ImageDesc {
            dimension: ImageDimension::D2,
            format,
            extent: Extent3d {
                width: extent.0,
                height: extent.1,
                depth: 1,
            },
            mip_levels: 1,
            array_layers: 1,
            samples,
            usage: usage
                .with(ImageUsage::TRANSFER_SRC)
                .with(ImageUsage::SAMPLED),
        },
    );
    let view = context
        .create_image_view(
            &image,
            &ImageViewDesc {
                format,
                dimension: ViewDimension::D2,
                range: ImageRange {
                    aspects,
                    base_level: 0,
                    levels: 1,
                    base_layer: 0,
                    layers: 1,
                },
            },
        )
        .expect("a view");
    Target {
        image,
        view,
        _memory: memory,
        format,
        extent,
    }
}

/// Each texel of a one-sample colour `target`, its bytes in order.
pub(crate) fn read(context: &Context, target: &Target) -> Vec<u8> {
    let bytes = u64::from(target.format.texel_bytes());
    let (width, height) = target.extent;
    let size = u64::from(width) * u64::from(height) * bytes;
    let held = memory(context, size.next_multiple_of(4));
    let buffer = over(context, &held, 0, size.next_multiple_of(4));
    let mut commands = context.command_buffer();
    commands
        .copy_image_to_buffer(
            &target.image,
            &buffer,
            &[BufferImageCopy {
                buffer_offset: 0,
                bytes_per_row: u32::try_from(u64::from(width) * bytes).expect("small"),
                rows_per_image: height,
                image: ImageSubresource {
                    aspect: target.format.aspects(),
                    level: 0,
                    base_layer: 0,
                    layers: 1,
                },
                origin: Origin3d::default(),
                extent: Extent3d {
                    width,
                    height,
                    depth: 1,
                },
            }],
        )
        .expect("recorded");
    submit(&queue(context), &commands).expect("copied");
    let mut out = vec![0u8; usize::try_from(size).expect("small")];
    held.read(0, &mut out).expect("readable");
    out
}

/// A buffer holding `floats`, made for every use.
pub(crate) fn floats(context: &Context, floats: &[f32]) -> (crate::Allocation, crate::Buffer) {
    let bytes: Vec<u8> = floats.iter().flat_map(|f| f.to_le_bytes()).collect();
    let size = (bytes.len() as u64).next_multiple_of(16).max(16);
    let held = memory(context, size);
    let buffer = over(context, &held, 0, size);
    held.write(0, &bytes).expect("written");
    (held, buffer)
}

/// A vertex shader passing its position, at location 0, and a value of the
/// type `value` — `%v4`, or one `declared` — at location 1 through to
/// location 0, under `decorations`.
pub(crate) fn passing((value, declared): (&str, &str), decorations: &str) -> String {
    format!(
        r#"OpCapability Shader
OpMemoryModel Logical GLSL450
OpEntryPoint Vertex %main "main" %in_position %in_value %out_position %out_value
OpDecorate %in_position Location 0
OpDecorate %in_value Location 1
OpDecorate %out_position BuiltIn Position
OpDecorate %out_value Location 0
{decorations}
%void = OpTypeVoid
%fn = OpTypeFunction %void
%float = OpTypeFloat 32
%v4 = OpTypeVector %float 4
{declared}
%in_v4 = OpTypePointer Input %v4
%out_v4 = OpTypePointer Output %v4
%in_value_t = OpTypePointer Input {value}
%out_value_t = OpTypePointer Output {value}
%in_position = OpVariable %in_v4 Input
%in_value = OpVariable %in_value_t Input
%out_position = OpVariable %out_v4 Output
%out_value = OpVariable %out_value_t Output
%main = OpFunction %void None %fn
%entry = OpLabel
%p = OpLoad %v4 %in_position
%v = OpLoad {value} %in_value
OpStore %out_position %p
OpStore %out_value %v
OpReturn
OpFunctionEnd
"#
    )
}

/// A fragment shader writing its location-0 input of `vec4` to its target
/// at location 0, the input decorated `decorations`.
pub(crate) fn painting(decorations: &str) -> String {
    format!(
        r#"OpCapability Shader
OpMemoryModel Logical GLSL450
OpEntryPoint Fragment %main "main" %in_colour %out_colour
OpExecutionMode %main OriginUpperLeft
OpDecorate %in_colour Location 0
OpDecorate %out_colour Location 0
{decorations}
%void = OpTypeVoid
%fn = OpTypeFunction %void
%float = OpTypeFloat 32
%v4 = OpTypeVector %float 4
%in_v4 = OpTypePointer Input %v4
%out_v4 = OpTypePointer Output %v4
%in_colour = OpVariable %in_v4 Input
%out_colour = OpVariable %out_v4 Output
%main = OpFunction %void None %fn
%entry = OpLabel
%c = OpLoad %v4 %in_colour
OpStore %out_colour %c
OpReturn
OpFunctionEnd
"#
    )
}

/// The attributes of a vertex of eight floats: its position, then its value.
pub(crate) const ATTRIBUTES: [VertexAttribute; 2] = [
    VertexAttribute {
        location: 0,
        format: Format::Rgba32Float,
        offset: 0,
    },
    VertexAttribute {
        location: 1,
        format: Format::Rgba32Float,
        offset: 16,
    },
];

/// A rasterisation culling nothing, of one-pixel lines.
pub(crate) const FILLED: Rasterization = Rasterization {
    cull: CullMode::None,
    front_face: FrontFace::CounterClockwise,
    depth_bias: None,
    depth_clamp: false,
    line_width: 1.0,
};

pub(crate) const ONE_SAMPLE: Multisample = Multisample {
    samples: 1,
    mask: u32::MAX,
    alpha_to_coverage: false,
};

/// What a pipeline is made of, apart from its stages.
pub(crate) struct Shape<'a> {
    pub(crate) topology: Topology,
    pub(crate) targets: &'a [ColourTarget],
    pub(crate) depth_stencil: Option<crate::DepthStencilState>,
    pub(crate) multisample: Multisample,
    pub(crate) rasterization: Rasterization,
    pub(crate) restart: bool,
}

/// A plain colour target of `format`: replaced, every channel written.
pub(crate) const fn plain(format: Format) -> ColourTarget {
    ColourTarget {
        format,
        blend: None,
        write_mask: ColourMask::ALL,
    }
}

/// The pipeline of `vertex` and `fragment` under `layout`, its vertices of
/// eight floats.
pub(crate) fn pipeline(
    context: &Context,
    layout: &PipelineLayout,
    (vertex, fragment): (&str, Option<&str>),
    shape: &Shape<'_>,
) -> Result<RenderPipeline, crate::Error> {
    let vertex = context.create_shader_module(admit(vertex).expect("admitted"))?;
    let fragment = fragment
        .map(|text| context.create_shader_module(admit(text).expect("admitted")))
        .transpose()?;
    context.create_render_pipeline(&RenderPipelineDesc {
        layout,
        vertex: StageDesc {
            module: &vertex,
            entry_point: "main",
            specialization: &[],
        },
        fragment: fragment.as_ref().map(|module| StageDesc {
            module,
            entry_point: "main",
            specialization: &[],
        }),
        vertex_buffers: &[VertexBuffer {
            stride: 32,
            step: VertexStep::Vertex,
            attributes: &ATTRIBUTES,
        }],
        topology: shape.topology,
        primitive_restart: shape.restart,
        rasterization: shape.rasterization,
        depth_stencil: shape.depth_stencil,
        multisample: shape.multisample,
        targets: shape.targets,
        logic_op: None,
    })
}

/// A layout of nothing.
pub(crate) fn bare(context: &Context) -> PipelineLayout {
    context
        .create_pipeline_layout(&[], PushConstants::NONE)
        .expect("a layout")
}

/// `n`, a small count, as a single: exactly.
pub(crate) fn real(n: u32) -> f32 {
    f32::from(u16::try_from(n).expect("small"))
}

/// The viewport over all of an `extent`.
pub(crate) fn whole(extent: (u32, u32)) -> (Viewport, Rect) {
    (
        Viewport {
            x: 0.0,
            y: 0.0,
            width: real(extent.0),
            height: real(extent.1),
            min_depth: 0.0,
            max_depth: 1.0,
        },
        Rect {
            x: 0,
            y: 0,
            width: extent.0,
            height: extent.1,
        },
    )
}

/// The eight floats of each corner: its clip position and its colour.
pub(crate) fn corners(points: &[([f32; 4], [f32; 4])]) -> Vec<f32> {
    points
        .iter()
        .flat_map(|(position, colour)| position.iter().chain(colour).copied())
        .collect()
}

/// Draw `count` vertices of `buffer` with `pipeline` into `target`, cleared
/// to `clear` first.
pub(crate) fn draw_into(
    context: &Context,
    (pipeline, buffer): (&RenderPipeline, &crate::Buffer),
    target: &Target,
    (clear, count): ([f32; 4], u32),
) {
    let (viewport, scissor) = whole(target.extent);
    let mut commands = context.command_buffer();
    commands
        .render_pass(
            &Pass {
                colour: &[ColourAttachment {
                    view: &target.view,
                    resolve: None,
                    load: Load::Clear(ClearValue::Float(clear)),
                }],
                depth_stencil: None,
                area: scissor,
            },
            |drawer| {
                drawer.draw(
                    &Draw {
                        pipeline,
                        sets: &[],
                        push_constants: &[],
                        vertex_buffers: &[(buffer, 0)],
                        viewport,
                        scissor,
                        stencil_reference: 0,
                        blend_constant: [0.0; 4],
                    },
                    0..count,
                    0..1,
                )
            },
        )
        .expect("recorded");
    submit(&queue(context), &commands).expect("drawn");
}

const RED: [f32; 4] = [1.0, 0.0, 0.0, 1.0];

/// A value of four floats, the type every shader here declares.
pub(crate) const VEC4: (&str, &str) = ("%v4", "");

#[test]
fn a_quad_over_the_whole_target_paints_every_pixel() {
    let device = rendering();
    let context = context(&device);
    let layout = bare(&context);
    let targets = [plain(Format::Rgba8Unorm)];
    let pipeline = pipeline(
        &context,
        &layout,
        (&passing(VEC4, ""), Some(&painting(""))),
        &Shape {
            topology: Topology::Triangles,
            targets: &targets,
            depth_stencil: None,
            multisample: ONE_SAMPLE,
            rasterization: FILLED,
            restart: false,
        },
    )
    .expect("a pipeline");
    let at = |x: f32, y: f32| ([x, y, 0.5, 1.0], RED);
    let (_held, buffer) = floats(
        &context,
        &corners(&[
            at(-1.0, -1.0),
            at(1.0, -1.0),
            at(1.0, 1.0),
            at(-1.0, -1.0),
            at(1.0, 1.0),
            at(-1.0, 1.0),
        ]),
    );
    let target = target(&context, Format::Rgba8Unorm, (20, 12), 1);
    draw_into(&context, (&pipeline, &buffer), &target, ([0.0; 4], 6));
    let pixels = read(&context, &target);
    for (at, texel) in pixels.chunks(4).enumerate() {
        assert_eq!(texel, [255, 0, 0, 255], "pixel {at}");
    }
}

/// A pipeline of the plain stages and `shape`, but for what `change` alters
/// in its description.
fn made(
    context: &Context,
    shape: &Shape<'_>,
    ((vertex, fragment), attributes): ((&str, &str), &[VertexAttribute]),
    change: impl FnOnce(&mut RenderPipelineDesc<'_>),
) -> Result<RenderPipeline, crate::Error> {
    let layout = bare(context);
    let vertex = context.create_shader_module(admit(vertex).expect("admitted"))?;
    let fragment = context.create_shader_module(admit(fragment).expect("admitted"))?;
    let buffers = [VertexBuffer {
        stride: 32,
        step: VertexStep::Vertex,
        attributes,
    }];
    let mut desc = RenderPipelineDesc {
        layout: &layout,
        vertex: StageDesc {
            module: &vertex,
            entry_point: "main",
            specialization: &[],
        },
        fragment: Some(StageDesc {
            module: &fragment,
            entry_point: "main",
            specialization: &[],
        }),
        vertex_buffers: &buffers,
        topology: shape.topology,
        primitive_restart: shape.restart,
        rasterization: shape.rasterization,
        depth_stencil: shape.depth_stencil,
        multisample: shape.multisample,
        targets: shape.targets,
        logic_op: None,
    };
    change(&mut desc);
    context.create_render_pipeline(&desc)
}

/// A change to a pipeline's description.
type Change<'a> = &'a dyn Fn(&mut RenderPipelineDesc<'_>);

/// A list of filled triangles of one sample into `targets`.
const fn triangles(targets: &[ColourTarget]) -> Shape<'_> {
    Shape {
        topology: Topology::Triangles,
        targets,
        depth_stencil: None,
        multisample: ONE_SAMPLE,
        rasterization: FILLED,
        restart: false,
    }
}

#[test]
fn a_pipeline_whose_fixed_state_disagrees_is_refused() {
    use crate::Error;
    let device = rendering();
    let context = context(&device);
    let colour = [plain(Format::Rgba8Unorm)];
    let stages = (passing(VEC4, ""), painting(""));
    let plain_stages = ((stages.0.as_str(), stages.1.as_str()), &ATTRIBUTES[..]);
    made(&context, &triangles(&colour), plain_stages, |_| {}).expect("the plain pipeline");
    let stencil_only = crate::DepthStencilState {
        format: Format::Stencil8,
        depth_compare: crate::CompareOp::Less,
        depth_write: true,
        stencil: None,
    };
    let refusals: [(&str, Error, Change<'_>); 5] = [
        (
            "an entry point of another stage",
            Error::EntryPoint,
            &|desc| {
                desc.vertex.entry_point = "absent";
            },
        ),
        ("restart of a list", Error::Unsupported, &|desc| {
            desc.primitive_restart = true;
        }),
        ("two samples", Error::Samples, &|desc| {
            desc.multisample.samples = 2;
        }),
        ("a line narrower than a pixel", Error::Shape, &|desc| {
            desc.rasterization.line_width = 0.5;
        }),
        ("a depth test of a stencil format", Error::Aspect, &|desc| {
            desc.depth_stencil = Some(stencil_only);
        }),
    ];
    for (what, error, change) in refusals {
        let refused = made(&context, &triangles(&colour), plain_stages, change);
        assert_eq!(refused.err(), Some(error), "{what}");
    }
    let blended = [ColourTarget {
        format: Format::R32Uint,
        blend: Some(crate::Blend::OVER),
        write_mask: ColourMask::ALL,
    }];
    let refused = made(&context, &triangles(&blended), plain_stages, |_| {});
    assert_eq!(
        refused.err(),
        Some(Error::Unsupported),
        "a blend on a format that does not blend"
    );
}

#[test]
fn a_pipeline_whose_stages_and_targets_disagree_is_refused() {
    use crate::Error;
    let device = rendering();
    let context = context(&device);
    let colour = [plain(Format::Rgba8Unorm)];
    let stages = (passing(VEC4, ""), painting(""));
    let (vertex, fragment) = (stages.0.as_str(), stages.1.as_str());
    let plain_stages = ((vertex, fragment), &ATTRIBUTES[..]);
    let ints = [VertexAttribute {
        format: Format::Rgba32Uint,
        ..ATTRIBUTES[1]
    }];
    let missing = [ATTRIBUTES[0]];
    let wrong_type = [ATTRIBUTES[0], ints[0]];
    for (what, attributes) in [
        ("an input no attribute feeds", &missing[..]),
        ("an input fed another type", &wrong_type[..]),
    ] {
        let refused = made(
            &context,
            &triangles(&colour),
            ((vertex, fragment), attributes),
            |_| {},
        );
        assert_eq!(refused.err(), Some(Error::Interface), "{what}");
    }
    let integer = [plain(Format::R32Uint)];
    for (what, targets) in [
        ("a float output to an integer target", &integer[..]),
        ("an output with no target", &[][..]),
    ] {
        let refused = made(&context, &triangles(targets), plain_stages, |_| {});
        assert_eq!(refused.err(), Some(Error::Interface), "{what}");
    }
    let lonely = painting("")
        .replace("%in_colour %out_colour", "%in_colour %out_colour %other")
        .replace(
            "OpDecorate %out_colour Location 0",
            "OpDecorate %out_colour Location 0\nOpDecorate %other Location 3",
        )
        .replace(
            "%out_colour = OpVariable %out_v4 Output",
            "%out_colour = OpVariable %out_v4 Output\n%other = OpVariable %in_v4 Input",
        );
    let refused = made(
        &context,
        &triangles(&colour),
        ((vertex, &lonely), &ATTRIBUTES[..]),
        |_| {},
    );
    assert_eq!(
        refused.err(),
        Some(Error::Interface),
        "an input no vertex output provides"
    );
}

/// What a stage adds to count its invocations into the storage buffer at
/// set 0, binding 0: its decorations, its types and variable, and the add.
const COUNTER: (&str, &str, &str) = (
    "OpDecorate %block Block\nOpMemberDecorate %block 0 Offset 0\nOpDecorate %counter DescriptorSet 0\nOpDecorate %counter Binding 0",
    "%uint = OpTypeInt 32 0\n%block = OpTypeStruct %uint\n%block_ptr = OpTypePointer StorageBuffer %block\n%uint_ptr = OpTypePointer StorageBuffer %uint\n%zero = OpConstant %uint 0\n%one = OpConstant %uint 1\n%counter = OpVariable %block_ptr StorageBuffer",
    "%at = OpAccessChain %uint_ptr %counter %zero\n%old = OpAtomicIAdd %uint %at %one %zero %one",
);

/// A vertex shader passing as [`passing`] does, counting its invocations.
fn counting_vertex() -> String {
    let (decorations, types, add) = COUNTER;
    passing(("%v4", types), decorations)
        .replace(
            "%out_position %out_value\n",
            "%out_position %out_value %counter\n",
        )
        .replace(
            "OpStore %out_position %p",
            &format!("{add}\nOpStore %out_position %p"),
        )
}

/// A fragment shader painting as [`painting`] does, counting its
/// invocations, its fragments tested early where `early`.
pub(crate) fn counting(early: bool) -> String {
    let (decorations, types, add) = COUNTER;
    let mode = if early {
        "OpExecutionMode %main EarlyFragmentTests\n"
    } else {
        ""
    };
    painting(decorations)
        .replace(
            "%in_colour %out_colour\n",
            "%in_colour %out_colour %counter\n",
        )
        .replace(
            "OpExecutionMode %main OriginUpperLeft\n",
            &format!("OpExecutionMode %main OriginUpperLeft\n{mode}"),
        )
        .replace(
            "%in_colour = OpVariable",
            &format!("{types}\n%in_colour = OpVariable"),
        )
        .replace(
            "OpStore %out_colour %c",
            &format!("{add}\nOpStore %out_colour %c"),
        )
}

/// A layout of the one storage buffer a counting stage of `stages` writes.
pub(crate) fn counted_layout(
    context: &Context,
    stages: tairix_spirv::Stages,
) -> (crate::BindSetLayout, PipelineLayout) {
    let set_layout = context
        .create_bind_set_layout(&[crate::LayoutBinding {
            stages,
            ..crate::pipeline::tests::storage(0)
        }])
        .expect("a layout");
    let layout = context
        .create_pipeline_layout(&[&set_layout], PushConstants::NONE)
        .expect("a layout");
    (set_layout, layout)
}

/// A fragment shader writing `colour` at location 0, index 0, and the second
/// colour a dual-source blend reads at index 1.
fn painting_two() -> String {
    painting("OpDecorate %second Location 0\nOpDecorate %second Index 1")
        .replace(
            "%in_colour %out_colour\n",
            "%in_colour %out_colour %second\n",
        )
        .replace(
            "%out_colour = OpVariable %out_v4 Output",
            "%out_colour = OpVariable %out_v4 Output\n%second = OpVariable %out_v4 Output",
        )
        .replace(
            "OpStore %out_colour %c",
            "OpStore %out_colour %c\nOpStore %second %c",
        )
}

#[test]
fn a_pipeline_reading_past_its_device_or_its_fragment_stage_is_refused() {
    use crate::{BlendFactor, Error, Features};
    let device = rendering();
    let context = context(&device);
    let colour = [plain(Format::Rgba8Unorm)];
    let shape = triangles;
    let stages = (passing(VEC4, ""), painting(""));
    let (vertex, fragment) = (stages.0.as_str(), stages.1.as_str());
    let past = [
        ATTRIBUTES[0],
        ATTRIBUTES[1],
        VertexAttribute {
            location: device.info().limits.max_vertex_input_attributes,
            ..ATTRIBUTES[1]
        },
    ];
    let refused = made(
        &context,
        &shape(&colour),
        ((vertex, fragment), &past),
        |_| {},
    );
    assert_eq!(
        refused.err(),
        Some(Error::OutOfRange),
        "an attribute past the device's locations"
    );
    let second = crate::Blend {
        source_color: BlendFactor::Source1Color,
        ..crate::Blend::OVER
    };
    let reading = ColourTarget {
        blend: Some(second),
        ..plain(Format::Rgba8Unorm)
    };
    let (first, second_target) = ([reading], [plain(Format::Rgba8Unorm), reading]);
    let two = painting_two();
    let stages_of = |fragment| ((vertex, fragment), &ATTRIBUTES[..]);
    made(&context, &shape(&first), stages_of(two.as_str()), |_| {})
        .expect("the first target reads the second colour its stage writes");
    let refused = made(&context, &shape(&first), stages_of(fragment), |_| {});
    assert_eq!(
        refused.err(),
        Some(Error::Interface),
        "a second colour the stage does not write"
    );
    let refused = made(
        &context,
        &shape(&second_target),
        stages_of(two.as_str()),
        |_| {},
    );
    assert_eq!(
        refused.err(),
        Some(Error::Interface),
        "a second colour read past the first target"
    );

    let (counting_vertex, counting_fragment) = (counting_vertex(), counting(false));
    let stores = [
        (
            tairix_spirv::Stages::VERTEX,
            Features::VERTEX_STORES,
            counting_vertex.as_str(),
            fragment,
        ),
        (
            tairix_spirv::Stages::FRAGMENT,
            Features::FRAGMENT_STORES,
            vertex,
            counting_fragment.as_str(),
        ),
    ];
    for (stage, feature, vertex, fragment) in stores {
        for stated in [true, false] {
            let device = crate::binding::tests::device_stating(|info| {
                if !stated {
                    info.features = info.features.without(feature);
                }
            });
            let context = crate::device_tests::context(&device);
            let (_set_layout, layout) = counted_layout(&context, stage);
            let made = pipeline(&context, &layout, (vertex, Some(fragment)), &shape(&colour));
            let expected = if stated {
                None
            } else {
                Some(Error::Unsupported)
            };
            assert_eq!(
                made.err(),
                expected,
                "{stage:?} writing storage, the feature stated: {stated}"
            );
        }
    }
}
