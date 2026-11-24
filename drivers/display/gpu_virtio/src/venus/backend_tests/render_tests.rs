//! Render pipelines and passes through Venus, held to a renderer of the
//! test's own: every draw of a pass run on the host leaves what the same work
//! leaves on the software device, the host refusing whatever Vulkan leaves
//! undefined; where the host draws no first instance, an indirect draw's
//! records are checked on the device, and a pass whose records outrun their
//! slot runs as several rendering instances that leave the same.

use alloc::vec::Vec;

use tairix_gpu::{
    AllocationDesc, Aspects, Blend, BlendFactor, BlendOp, BufferDesc, BufferImageCopy, BufferUsage,
    ClearValue, ColourAttachment, ColourMask, ColourTarget, CompareOp, DepthStencilAttachment,
    DepthStencilState, Device, Draw, Error, Extent3d, Features, Format, FrontFace, ImageDesc,
    ImageDimension, ImageRange, ImageSubresource, ImageUsage, ImageViewDesc, IndexFormat, Indices,
    Indirect, Load, Loss, MemoryProperties, Multisample, Origin3d, Pass, PushConstants,
    Rasterization, Rect, RenderPipeline, RenderPipelineDesc, StageDesc, Topology, VertexAttribute,
    VertexBuffer, VertexStep, ViewDimension, Viewport,
};
use tairix_spirv::assemble::admit;

use super::image_tests::software;
use super::{context, goes_on, let_go, submit, Rig, Unsent};
use crate::venus::Structure;

/// A vertex stage passing its position and a flat colour on.
const PASSING: &str = r#"OpCapability Shader
OpMemoryModel Logical GLSL450
OpEntryPoint Vertex %main "main" %in_position %in_colour %out_position %out_colour
OpDecorate %in_position Location 0
OpDecorate %in_colour Location 1
OpDecorate %out_position BuiltIn Position
OpDecorate %out_colour Location 0
OpDecorate %out_colour Flat
%void = OpTypeVoid
%fn = OpTypeFunction %void
%float = OpTypeFloat 32
%v4 = OpTypeVector %float 4
%in_v4 = OpTypePointer Input %v4
%out_v4 = OpTypePointer Output %v4
%in_position = OpVariable %in_v4 Input
%in_colour = OpVariable %in_v4 Input
%out_position = OpVariable %out_v4 Output
%out_colour = OpVariable %out_v4 Output
%main = OpFunction %void None %fn
%entry = OpLabel
%p = OpLoad %v4 %in_position
%c = OpLoad %v4 %in_colour
OpStore %out_position %p
OpStore %out_colour %c
OpReturn
OpFunctionEnd
"#;

/// A fragment stage painting its flat colour, every channel or `rg` alone.
fn painting(rg: bool) -> alloc::string::String {
    let (output, store) = if rg {
        (
            "%v2 = OpTypeVector %float 2\n%out_t = OpTypePointer Output %v2",
            "%rg = OpVectorShuffle %v2 %c %c 0 1\nOpStore %out_colour %rg",
        )
    } else {
        (
            "%out_t = OpTypePointer Output %v4",
            "OpStore %out_colour %c",
        )
    };
    alloc::format!(
        r#"OpCapability Shader
OpMemoryModel Logical GLSL450
OpEntryPoint Fragment %main "main" %in_colour %out_colour
OpExecutionMode %main OriginUpperLeft
OpDecorate %in_colour Location 0
OpDecorate %in_colour Flat
OpDecorate %out_colour Location 0
%void = OpTypeVoid
%fn = OpTypeFunction %void
%float = OpTypeFloat 32
%v4 = OpTypeVector %float 4
%in_v4 = OpTypePointer Input %v4
{output}
%in_colour = OpVariable %in_v4 Input
%out_colour = OpVariable %out_t Output
%main = OpFunction %void None %fn
%entry = OpLabel
%c = OpLoad %v4 %in_colour
{store}
OpReturn
OpFunctionEnd
"#
    )
}

/// The attributes of a vertex of eight floats: its position, its colour.
const ATTRIBUTES: [VertexAttribute; 2] = [
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

/// The side of every target.
const SIDE: u32 = 16;

/// A host-visible memory type of `device`.
fn visible(device: &Device) -> u32 {
    device
        .info()
        .memory_types
        .iter()
        .position(|kind| kind.properties.contains(MemoryProperties::HOST_VISIBLE))
        .and_then(|at| u32::try_from(at).ok())
        .expect("host-visible memory")
}

/// A buffer of every use holding `words`, in host-visible memory.
fn holding(
    device: &Device,
    context: &tairix_gpu::Context,
    words: &[u32],
) -> (tairix_gpu::Allocation, tairix_gpu::Buffer) {
    let bytes: Vec<u8> = words.iter().flat_map(|word| word.to_le_bytes()).collect();
    let size = (bytes.len() as u64).next_multiple_of(256);
    let memory = context
        .allocate(&AllocationDesc {
            size,
            memory_type: visible(device),
        })
        .expect("memory");
    memory.write(0, &bytes).expect("written");
    let buffer = context
        .create_buffer(
            &memory,
            &BufferDesc {
                offset: 0,
                size,
                usage: BufferUsage::ALL,
            },
        )
        .expect("a buffer");
    (memory, buffer)
}

/// Each corner's eight floats, as words.
fn corners(points: &[([f32; 4], [f32; 4])]) -> Vec<u32> {
    points
        .iter()
        .flat_map(|(position, colour)| position.iter().chain(colour).map(|value| value.to_bits()))
        .collect()
}

/// An image to render to, its view and its memory.
struct Target {
    image: tairix_gpu::Image,
    view: tairix_gpu::ImageView,
    format: Format,
    _memory: tairix_gpu::Allocation,
}

fn target(context: &tairix_gpu::Context, format: Format, samples: u32) -> Target {
    let aspects = format.aspects();
    let attachment = if aspects == Aspects::COLOR {
        ImageUsage::COLOR_ATTACHMENT
    } else {
        ImageUsage::DEPTH_STENCIL_ATTACHMENT
    };
    let usage = if samples == 1 {
        attachment.with(ImageUsage::TRANSFER_SRC)
    } else {
        attachment
    };
    let desc = ImageDesc {
        dimension: ImageDimension::D2,
        format,
        extent: Extent3d {
            width: SIDE,
            height: SIDE,
            depth: 1,
        },
        mip_levels: 1,
        array_layers: 1,
        samples,
        usage,
    };
    let needs = context.image_requirements(&desc).expect("requirements");
    let memory = context
        .allocate(&AllocationDesc {
            size: needs.size,
            memory_type: needs.memory_types.trailing_zeros(),
        })
        .expect("memory");
    let image = context.create_image(&memory, 0, &desc).expect("an image");
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
        format,
        _memory: memory,
    }
}

/// Each plane of a one-sample `target`, its texels' bytes in order.
fn read(device: &Device, context: &tairix_gpu::Context, target: &Target) -> Vec<u8> {
    let planes = [Aspects::COLOR, Aspects::DEPTH, Aspects::STENCIL];
    let mut seen = Vec::new();
    for plane in planes {
        if !target.format.aspects().contains(plane) {
            continue;
        }
        let texel = target.format.copy_bytes(plane).expect("a plane copied");
        let size = u64::from(SIDE * SIDE * texel);
        let (memory, buffer) = holding(
            device,
            context,
            &alloc::vec![0; usize::try_from(size / 4).expect("small").max(1)],
        );
        let mut commands = context.command_buffer();
        commands
            .copy_image_to_buffer(
                &target.image,
                &buffer,
                &[BufferImageCopy {
                    buffer_offset: 0,
                    bytes_per_row: SIDE * texel,
                    rows_per_image: SIDE,
                    image: ImageSubresource {
                        aspect: plane,
                        level: 0,
                        base_layer: 0,
                        layers: 1,
                    },
                    origin: Origin3d::default(),
                    extent: Extent3d {
                        width: SIDE,
                        height: SIDE,
                        depth: 1,
                    },
                }],
            )
            .expect("recorded");
        submit(context, &commands).expect("copied");
        let mut bytes = alloc::vec![0; usize::try_from(size).expect("small")];
        memory.read(0, &mut bytes).expect("read");
        seen.extend(bytes);
    }
    seen
}

/// What a pipeline is made of besides its stages.
#[derive(Copy, Clone)]
struct Shape<'a> {
    topology: Topology,
    restart: bool,
    targets: &'a [ColourTarget],
    depth_stencil: Option<DepthStencilState>,
    samples: u32,
}

const fn plain(format: Format) -> ColourTarget {
    ColourTarget {
        format,
        blend: None,
        write_mask: ColourMask::ALL,
    }
}

fn pipeline(context: &tairix_gpu::Context, fragment: &str, shape: Shape<'_>) -> RenderPipeline {
    let layout = context
        .create_pipeline_layout(&[], PushConstants::NONE)
        .expect("a layout");
    let vertex = context
        .create_shader_module(admit(PASSING).expect("admitted"))
        .expect("a module");
    let fragment = context
        .create_shader_module(admit(fragment).expect("admitted"))
        .expect("a module");
    context
        .create_render_pipeline(&RenderPipelineDesc {
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
            vertex_buffers: &[VertexBuffer {
                stride: 32,
                step: VertexStep::Vertex,
                attributes: &ATTRIBUTES,
            }],
            topology: shape.topology,
            primitive_restart: shape.restart,
            rasterization: Rasterization {
                cull: tairix_gpu::CullMode::None,
                front_face: FrontFace::CounterClockwise,
                depth_bias: None,
                depth_clamp: false,
                line_width: 1.0,
            },
            depth_stencil: shape.depth_stencil,
            multisample: Multisample {
                samples: shape.samples,
                mask: u32::MAX,
                alpha_to_coverage: false,
            },
            targets: shape.targets,
            logic_op: None,
        })
        .expect("a pipeline")
}

/// The viewport and scissor over every target.
fn whole() -> (Viewport, Rect) {
    (
        Viewport {
            x: 0.0,
            y: 0.0,
            width: 16.0,
            height: 16.0,
            min_depth: 0.0,
            max_depth: 1.0,
        },
        Rect {
            x: 0,
            y: 0,
            width: SIDE,
            height: SIDE,
        },
    )
}

const RED: [f32; 4] = [1.0, 0.0, 0.0, 1.0];
const GREEN: [f32; 4] = [0.0, 1.0, 0.0, 1.0];
const BLUE: [f32; 4] = [0.0, 0.0, 1.0, 0.5];

/// A triangle of `colour` at depth `z`, its corners those of a square's
/// half, the square's corners `(x0, y0)` and `(x1, y1)` in clip space.
fn triangle(
    ([x0, y0], [x1, y1]): ([f32; 2], [f32; 2]),
    z: f32,
    colour: [f32; 4],
) -> [([f32; 4], [f32; 4]); 3] {
    [
        ([x0, y0, z, 1.0], colour),
        ([x1, y0, z, 1.0], colour),
        ([x0, y1, z, 1.0], colour),
    ]
}

/// The pipelines of every kind of draw into a float target, depth-tested:
/// triangles; a strip restarting, added into the colour, writing no depth;
/// and points.
fn every_kind(context: &tairix_gpu::Context) -> [RenderPipeline; 3] {
    let depth_state = DepthStencilState {
        format: Format::Depth32FloatStencil8,
        depth_compare: CompareOp::Less,
        depth_write: true,
        stencil: None,
    };
    let targets = [plain(Format::Rgba32Float)];
    let added = [ColourTarget {
        blend: Some(Blend {
            source_color: BlendFactor::One,
            destination_color: BlendFactor::One,
            color: BlendOp::Add,
            source_alpha: BlendFactor::One,
            destination_alpha: BlendFactor::Zero,
            alpha: BlendOp::Add,
        }),
        ..plain(Format::Rgba32Float)
    }];
    let shape = Shape {
        topology: Topology::Triangles,
        restart: false,
        targets: &targets,
        depth_stencil: Some(depth_state),
        samples: 1,
    };
    let paint = painting(false);
    [
        pipeline(context, &paint, shape),
        pipeline(
            context,
            &paint,
            Shape {
                topology: Topology::TriangleStrip,
                restart: true,
                targets: &added,
                depth_stencil: Some(DepthStencilState {
                    depth_write: false,
                    ..depth_state
                }),
                ..shape
            },
        ),
        pipeline(
            context,
            &paint,
            Shape {
                topology: Topology::Points,
                ..shape
            },
        ),
    ]
}

/// The vertices every kind of draw reads: a triangle, the strip's corners,
/// a triangle an indirect record names, and two points.
fn every_kind_of_vertex() -> Vec<([f32; 4], [f32; 4])> {
    let mut points = Vec::new();
    points.extend(triangle(([-1.0, -1.0], [1.0, 1.0]), 0.5, RED));
    points.extend([
        ([-1.0, 0.0, 0.25, 1.0], GREEN),
        ([0.0, 0.0, 0.25, 1.0], GREEN),
        ([-1.0, 1.0, 0.25, 1.0], GREEN),
        ([0.0, 1.0, 0.25, 1.0], GREEN),
        ([0.25, -1.0, 0.75, 1.0], BLUE),
        ([1.0, -1.0, 0.75, 1.0], BLUE),
        ([0.25, -0.25, 0.75, 1.0], BLUE),
    ]);
    points.extend(triangle(([0.0, 0.0], [1.0, 1.0]), 0.1, BLUE));
    points.extend([
        ([-0.9375, -0.9375, 0.0, 1.0], GREEN),
        ([0.9375, 0.9375, 0.0, 1.0], RED),
    ]);
    points
}

/// What a pass of every kind of draw leaves on `device`: its colour target,
/// then its depth and stencil.
fn every_draw(device: &Device) -> Vec<u8> {
    let context = context(device);
    let colour = target(&context, Format::Rgba32Float, 1);
    let depth = target(&context, Format::Depth32FloatStencil8, 1);
    let [listed, stripped, pointed] = every_kind(&context);
    let points = every_kind_of_vertex();
    let (_vertex_memory, vertices) = holding(device, &context, &corners(&points));
    let (_index_memory, indices) = holding(
        device,
        &context,
        &[3 | 4 << 16, 5 | 6 << 16, 0xffff | 7 << 16, 8 | 9 << 16],
    );
    let (_record_memory, records) = holding(device, &context, &[9, 9, 0, 0, 3, 1, 10, 0]);
    let (viewport, scissor) = whole();
    let bound = [(&vertices, 0)];
    let draw = |pipeline| Draw {
        pipeline,
        sets: &[],
        push_constants: &[],
        vertex_buffers: &bound,
        viewport,
        scissor,
        stencil_reference: 0,
        blend_constant: [0.0; 4],
    };
    let mut commands = context.command_buffer();
    commands
        .render_pass(
            &Pass {
                colour: &[ColourAttachment {
                    view: &colour.view,
                    resolve: None,
                    load: Load::Clear(ClearValue::Float([0.25, 0.5, 0.75, 1.0])),
                }],
                depth_stencil: Some(DepthStencilAttachment {
                    view: &depth.view,
                    depth_load: Load::Clear(ClearValue::DepthStencil {
                        depth: 1.0,
                        stencil: 0,
                    }),
                    stencil_load: Load::Clear(ClearValue::DepthStencil {
                        depth: 1.0,
                        stencil: 5,
                    }),
                }),
                area: scissor,
            },
            |drawer| {
                drawer.draw(&draw(&listed), 0..3, 0..1)?;
                drawer.draw_indexed(
                    &draw(&stripped),
                    &Indices {
                        buffer: &indices,
                        offset: 0,
                        format: IndexFormat::U16,
                    },
                    (0..8, 0),
                    0..1,
                )?;
                drawer.draw_indirect(
                    &draw(&listed),
                    &Indirect {
                        buffer: &records,
                        offset: 16,
                        count: 1,
                        stride: 16,
                    },
                )?;
                drawer.draw(&draw(&pointed), 13..15, 0..1)
            },
        )
        .expect("recorded");
    submit(&context, &commands).expect("drawn");
    let mut seen = read(device, &context, &colour);
    seen.extend(read(device, &context, &depth));
    seen
}

#[test]
fn every_kind_of_draw_leaves_on_the_host_what_it_leaves_on_the_software_device() {
    let rig = Rig::new();
    let device = rig.device();
    assert!(device.info().queue_families[0]
        .capabilities
        .contains(tairix_gpu::QueueCapabilities::GRAPHICS));
    let seen = every_draw(&device);
    assert_eq!(seen, every_draw(&software()));
    assert_ne!(
        seen[..64],
        [0; 64],
        "the clear and the draws reached the target"
    );
    assert_eq!(rig.host.state.lock().expect("unpoisoned").rendered, 1);
    drop(device);
    let_go(&rig.finish());
}

/// What a pass leaves in a target cleared to `clear` and drawn by a
/// pipeline whose fragment stage writes red and green alone, scissored past
/// the pass's area.
fn red_and_green(device: &Device, clear: [f32; 4]) -> Vec<u8> {
    let context = context(device);
    let colour = target(&context, Format::Rgba32Float, 1);
    let targets = [plain(Format::Rgba32Float)];
    let pipeline = pipeline(
        &context,
        &painting(true),
        Shape {
            topology: Topology::Triangles,
            restart: false,
            targets: &targets,
            depth_stencil: None,
            samples: 1,
        },
    );
    let points = triangle(([-1.0, -1.0], [3.0, 3.0]), 0.5, BLUE);
    let (_memory, vertices) = holding(device, &context, &corners(&points));
    let (viewport, scissor) = whole();
    let area = Rect {
        x: 2,
        y: 3,
        width: 9,
        height: 7,
    };
    let mut commands = context.command_buffer();
    commands
        .render_pass(
            &Pass {
                colour: &[ColourAttachment {
                    view: &colour.view,
                    resolve: None,
                    load: Load::Clear(ClearValue::Float(clear)),
                }],
                depth_stencil: None,
                area,
            },
            |drawer| {
                drawer.draw(
                    &Draw {
                        pipeline: &pipeline,
                        sets: &[],
                        push_constants: &[],
                        vertex_buffers: &[(&vertices, 0)],
                        viewport,
                        scissor,
                        stencil_reference: 0,
                        blend_constant: [0.0; 4],
                    },
                    0..3,
                    0..1,
                )
            },
        )
        .expect("recorded");
    submit(&context, &commands).expect("drawn");
    read(device, &context, &colour)
}

#[test]
fn a_draw_writes_only_the_channels_its_fragment_stage_writes_and_only_within_its_pass() {
    let rig = Rig::new();
    let device = rig.device();
    let clear = [0.125, 0.25, 0.375, 0.5];
    let seen = red_and_green(&device, clear);
    assert_eq!(seen, red_and_green(&software(), clear));
    let texel = |x: usize, y: usize| {
        let at = (y * SIDE as usize + x) * 16;
        <[u8; 16]>::try_from(&seen[at..at + 16]).expect("a texel")
    };
    let words = |values: [f32; 4]| -> [u8; 16] {
        let mut bytes = [0u8; 16];
        for (chunk, value) in bytes.chunks_mut(4).zip(values) {
            chunk.copy_from_slice(&value.to_le_bytes());
        }
        bytes
    };
    assert_eq!(texel(0, 0), [0; 16], "outside the pass's area, as placed");
    assert_eq!(
        texel(4, 4),
        words([0.0, 0.0, 0.375, 0.5]),
        "red and green drawn, blue and alpha kept"
    );
    drop(device);
    let_go(&rig.finish());
}

/// What a pass leaves in the one-sample target four samples resolve into.
fn resolved(device: &Device) -> Vec<u8> {
    let context = context(device);
    let samples = target(&context, Format::Rgba32Float, 4);
    let into = target(&context, Format::Rgba32Float, 1);
    let targets = [plain(Format::Rgba32Float)];
    let pipeline = pipeline(
        &context,
        &painting(false),
        Shape {
            topology: Topology::Triangles,
            restart: false,
            targets: &targets,
            depth_stencil: None,
            samples: 4,
        },
    );
    let points = triangle(([-1.0, -1.0], [1.0, 1.0]), 0.5, GREEN);
    let (_memory, vertices) = holding(device, &context, &corners(&points));
    let (viewport, scissor) = whole();
    let mut commands = context.command_buffer();
    commands
        .render_pass(
            &Pass {
                colour: &[ColourAttachment {
                    view: &samples.view,
                    resolve: Some(&into.view),
                    load: Load::Clear(ClearValue::Float([0.0; 4])),
                }],
                depth_stencil: None,
                area: scissor,
            },
            |drawer| {
                drawer.draw(
                    &Draw {
                        pipeline: &pipeline,
                        sets: &[],
                        push_constants: &[],
                        vertex_buffers: &[(&vertices, 0)],
                        viewport,
                        scissor,
                        stencil_reference: 0,
                        blend_constant: [0.0; 4],
                    },
                    0..3,
                    0..1,
                )
            },
        )
        .expect("recorded");
    submit(&context, &commands).expect("drawn");
    read(device, &context, &into)
}

#[test]
fn four_samples_resolve_on_the_host_as_on_the_software_device() {
    let rig = Rig::new();
    let device = rig.device();
    assert!(device
        .info()
        .formats
        .get(Format::Rgba32Float)
        .contains(tairix_gpu::FormatFeatures::MULTISAMPLED));
    assert_eq!(resolved(&device), resolved(&software()));
    drop(device);
    let_go(&rig.finish());
}

/// What a pass leaves of a triangle, then `records` null indirect draws of
/// records `stride` bytes apart twice, then another triangle, the target
/// cleared first and loaded across every rendering instance the device
/// runs it as.
fn split(device: &Device, (records, stride): (u32, u32)) -> Vec<u8> {
    let context = context(device);
    let colour = target(&context, Format::Rgba32Float, 1);
    let targets = [plain(Format::Rgba32Float)];
    let pipeline = pipeline(
        &context,
        &painting(false),
        Shape {
            topology: Topology::Triangles,
            restart: false,
            targets: &targets,
            depth_stencil: None,
            samples: 1,
        },
    );
    let mut points = Vec::new();
    points.extend(triangle(([-1.0, -1.0], [0.0, 0.0]), 0.5, RED));
    points.extend(triangle(([0.0, 0.0], [1.0, 1.0]), 0.5, GREEN));
    let (_memory, vertices) = holding(device, &context, &corners(&points));
    let words = usize::try_from(records * stride / 4).expect("small");
    let (_record_memory, nulls) = holding(device, &context, &alloc::vec![0; words.max(1)]);
    let (viewport, scissor) = whole();
    let bound = [(&vertices, 0)];
    let draw = Draw {
        pipeline: &pipeline,
        sets: &[],
        push_constants: &[],
        vertex_buffers: &bound,
        viewport,
        scissor,
        stencil_reference: 0,
        blend_constant: [0.0; 4],
    };
    let none = Indirect {
        buffer: &nulls,
        offset: 0,
        count: records,
        stride,
    };
    let mut commands = context.command_buffer();
    commands
        .render_pass(
            &Pass {
                colour: &[ColourAttachment {
                    view: &colour.view,
                    resolve: None,
                    load: Load::Clear(ClearValue::Float([0.0, 0.0, 0.0, 1.0])),
                }],
                depth_stencil: None,
                area: scissor,
            },
            |drawer| {
                drawer.draw(&draw, 0..3, 0..1)?;
                drawer.draw_indirect(&draw, &none)?;
                drawer.draw_indirect(&draw, &none)?;
                drawer.draw(&draw, 3..6, 0..1)
            },
        )
        .expect("recorded");
    submit(&context, &commands).expect("drawn");
    read(device, &context, &colour)
}

/// The place of `drawIndirectFirstInstance` among the core features.
const DRAW_INDIRECT_FIRST_INSTANCE: usize = 10;

/// A pass whose checked records outrun the slot holding them renders as a
/// rendering instance a slot's worth, leaving what one instance would, its
/// records packed or strided.
#[test]
fn records_outrunning_their_slot_split_their_pass_into_rendering_instances() {
    let rig = Rig::new();
    rig.offer(|offered| offered.lacks.push(DRAW_INDIRECT_FIRST_INSTANCE));
    let device = rig.device();
    let records = crate::venus::info::CHECKED_RECORDS * 3 / 4;
    for stride in [16, 24] {
        let before = rig.host.state.lock().expect("unpoisoned").rendered;
        assert_eq!(
            split(&device, (records, stride)),
            split(&software(), (records, stride))
        );
        let after = rig.host.state.lock().expect("unpoisoned").rendered;
        assert_eq!(
            after - before,
            2,
            "one rendering instance a slot's worth of records"
        );
    }
    drop(device);
    let_go(&rig.finish());
}

/// A host drawing no first instance from a record has its device state none
/// and read at most a slot's worth of records a draw, and a record naming
/// one, checked on the device, loses its context alone, unread by the host.
#[test]
fn a_record_naming_a_first_instance_loses_its_context_on_a_host_drawing_none() {
    let rig = Rig::new();
    rig.offer(|offered| offered.lacks.push(DRAW_INDIRECT_FIRST_INSTANCE));
    let device = rig.device();
    let info = device.info();
    assert!(!info
        .features
        .contains(Features::DRAW_INDIRECT_FIRST_INSTANCE));
    assert_eq!(
        info.limits.max_draw_indirect_count,
        crate::venus::info::CHECKED_RECORDS
    );
    let kept = context(&device);
    let context = context(&device);
    let colour = target(&context, Format::Rgba32Float, 1);
    let targets = [plain(Format::Rgba32Float)];
    let pipeline = pipeline(
        &context,
        &painting(false),
        Shape {
            topology: Topology::Triangles,
            restart: false,
            targets: &targets,
            depth_stencil: None,
            samples: 1,
        },
    );
    let points = triangle(([-1.0, -1.0], [1.0, 1.0]), 0.5, RED);
    let (_memory, vertices) = holding(&device, &context, &corners(&points));
    let (_records_memory, named) = holding(&device, &context, &[3, 1, 0, 0, 3, 1, 0, 1]);
    let (viewport, scissor) = whole();
    let mut commands = context.command_buffer();
    commands
        .render_pass(
            &Pass {
                colour: &[ColourAttachment {
                    view: &colour.view,
                    resolve: None,
                    load: Load::Keep,
                }],
                depth_stencil: None,
                area: scissor,
            },
            |drawer| {
                drawer.draw_indirect(
                    &Draw {
                        pipeline: &pipeline,
                        sets: &[],
                        push_constants: &[],
                        vertex_buffers: &[(&vertices, 0)],
                        viewport,
                        scissor,
                        stencil_reference: 0,
                        blend_constant: [0.0; 4],
                    },
                    &Indirect {
                        buffer: &named,
                        offset: 0,
                        count: 2,
                        stride: 16,
                    },
                )
            },
        )
        .expect("recorded");
    assert!(matches!(
        submit(&context, &commands),
        Err(Error::Lost(Loss::Fault))
    ));
    assert!(matches!(context.status(), Err(Error::Lost(_))));
    assert!(
        !rig.host
            .state
            .lock()
            .expect("unpoisoned")
            .reached
            .contains(Unsent::FIRST_INSTANCED),
        "the host never read a record naming a first instance"
    );
    rig.wait_for(|state| state.contexts.iter().count() == 2);
    goes_on(&kept);
    drop((
        commands, pipeline, vertices, named, colour, context, kept, device,
    ));
    let_go(&rig.finish());
}

/// What draws at the edges of what they read and where they reach leave on
/// `device`: a triangle bound at its vertices' end, an indexed one whose
/// indices end part of an index short, one scissored far past its pass, and
/// one through a viewport narrower than its scissor.
fn edges(device: &Device) -> Vec<u8> {
    let context = context(device);
    let colour = target(&context, Format::Rgba32Float, 1);
    let targets = [plain(Format::Rgba32Float)];
    let triangles = pipeline(
        &context,
        &painting(false),
        Shape {
            topology: Topology::Triangles,
            restart: false,
            targets: &targets,
            depth_stencil: None,
            samples: 1,
        },
    );
    let (_vertex_memory, vertices) = holding(
        device,
        &context,
        &corners(&triangle(([-1.0, -1.0], [1.0, 1.0]), 0.5, RED)),
    );
    let index_memory = context
        .allocate(&AllocationDesc {
            size: 256,
            memory_type: visible(device),
        })
        .expect("memory");
    index_memory.write(0, &[0, 0, 0, 0, 1, 0]).expect("written");
    let indices = context
        .create_buffer(
            &index_memory,
            &BufferDesc {
                offset: 0,
                size: 6,
                usage: BufferUsage::INDEX,
            },
        )
        .expect("a buffer");
    let (viewport, scissor) = whole();
    let draw = |vertex_buffers, (viewport, scissor)| Draw {
        pipeline: &triangles,
        sets: &[],
        push_constants: &[],
        vertex_buffers,
        viewport,
        scissor,
        stencil_reference: 0,
        blend_constant: [0.0; 4],
    };
    let (from_start, at_end) = ([(&vertices, 0)], [(&vertices, 256)]);
    let far = Rect {
        x: 0x8000_0000,
        y: 0,
        width: 4,
        height: 4,
    };
    let half = Viewport {
        width: 8.0,
        ..viewport
    };
    let mut commands = context.command_buffer();
    commands
        .render_pass(
            &Pass {
                colour: &[ColourAttachment {
                    view: &colour.view,
                    resolve: None,
                    load: Load::Clear(ClearValue::Float([0.0, 0.0, 0.0, 1.0])),
                }],
                depth_stencil: None,
                area: scissor,
            },
            |drawer| {
                drawer.draw(&draw(&at_end, (viewport, scissor)), 0..3, 0..1)?;
                drawer.draw_indexed(
                    &draw(&from_start, (viewport, scissor)),
                    &Indices {
                        buffer: &indices,
                        offset: 0,
                        format: IndexFormat::U32,
                    },
                    (0..3, 0),
                    0..1,
                )?;
                drawer.draw(&draw(&from_start, (viewport, far)), 0..3, 0..1)?;
                drawer.draw(&draw(&from_start, (half, scissor)), 0..3, 0..1)
            },
        )
        .expect("recorded");
    submit(&context, &commands).expect("drawn");
    read(device, &context, &colour)
}

/// A range bound at its buffer's end binds nothing, an index range part of
/// an index short binds its whole indices, a scissor far past its pass draws
/// nothing, and a viewport narrower than its scissor holds the draw to it —
/// each bound as the host's valid usage has it, leaving what the software
/// device leaves.
#[test]
fn draws_at_the_edges_of_what_they_read_and_reach_draw_alike() {
    let rig = Rig::new();
    let device = rig.device();
    assert_eq!(edges(&device), edges(&software()));
    drop(device);
    let_go(&rig.finish());
}

/// Two indirect draws of one rendering instance have their records checked
/// in turn, a barrier ordering each check after the one before, since each
/// reads and writes the slot's fault word.
#[test]
fn two_record_checks_of_one_rendering_instance_are_ordered() {
    let rig = Rig::new();
    rig.offer(|offered| offered.lacks.push(DRAW_INDIRECT_FIRST_INSTANCE));
    let device = rig.device();
    let before = rig.host.state.lock().expect("unpoisoned").dispatched;
    assert_eq!(split(&device, (2, 16)), split(&software(), (2, 16)));
    let state = rig.host.state.lock().expect("unpoisoned");
    assert_eq!(state.dispatched - before, 2, "one check a draw");
    assert_eq!(state.rendered, 1, "one rendering instance");
    drop(state);
    drop(device);
    let_go(&rig.finish());
}

/// A pass whose indirect draws read no record copies and checks nothing on
/// a host checking records, and draws its direct draws alike.
#[test]
fn a_pass_reading_no_record_runs_no_check_on_a_host_checking_records() {
    let rig = Rig::new();
    rig.offer(|offered| offered.lacks.push(DRAW_INDIRECT_FIRST_INSTANCE));
    let device = rig.device();
    let before = rig.host.state.lock().expect("unpoisoned").dispatched;
    assert_eq!(split(&device, (0, 16)), split(&software(), (0, 16)));
    let state = rig.host.state.lock().expect("unpoisoned");
    assert_eq!(state.dispatched, before, "no record checked");
    assert_eq!(state.rendered, 1, "one rendering instance");
    drop(state);
    drop(device);
    let_go(&rig.finish());
}

/// What lines leave in a target on `device`.
fn lines(device: &Device) -> Vec<u8> {
    let context = context(device);
    let colour = target(&context, Format::Rgba32Float, 1);
    let targets = [plain(Format::Rgba32Float)];
    let pipeline = pipeline(
        &context,
        &painting(false),
        Shape {
            topology: Topology::LineStrip,
            restart: false,
            targets: &targets,
            depth_stencil: None,
            samples: 1,
        },
    );
    let points = [
        ([-0.9375, -0.9375, 0.5, 1.0], RED),
        ([0.9375, -0.5, 0.5, 1.0], RED),
        ([0.0, 0.9375, 0.5, 1.0], RED),
    ];
    let (_memory, vertices) = holding(device, &context, &corners(&points));
    let (viewport, scissor) = whole();
    let mut commands = context.command_buffer();
    commands
        .render_pass(
            &Pass {
                colour: &[ColourAttachment {
                    view: &colour.view,
                    resolve: None,
                    load: Load::Clear(ClearValue::Float([0.0; 4])),
                }],
                depth_stencil: None,
                area: scissor,
            },
            |drawer| {
                drawer.draw(
                    &Draw {
                        pipeline: &pipeline,
                        sets: &[],
                        push_constants: &[],
                        vertex_buffers: &[(&vertices, 0)],
                        viewport,
                        scissor,
                        stencil_reference: 0,
                        blend_constant: [0.0; 4],
                    },
                    0..3,
                    0..1,
                )
            },
        )
        .expect("recorded");
    submit(&context, &commands).expect("drawn");
    read(device, &context, &colour)
}

#[test]
fn a_host_whose_lines_are_its_own_draws_them_as_rectangles_asked_for() {
    let rig = Rig::new();
    rig.offer(|offered| offered.strict_lines = false);
    let device = rig.device();
    assert_eq!(lines(&device), lines(&software()));
    let enabled = rig
        .host
        .state
        .lock()
        .expect("unpoisoned")
        .enabled
        .as_ref()
        .map(|enabled| enabled.extensions.clone())
        .expect("made");
    assert!(enabled
        .iter()
        .any(|name| name == crate::venus::extension::LINE_RASTERIZATION_KHR));
    drop(device);
    let_go(&rig.finish());
}

#[test]
fn a_host_short_of_what_drawing_needs_states_no_queue_that_draws() {
    let rig = Rig::new();
    rig.offer(|offered| {
        offered
            .withheld
            .push((Structure::PhysicalDeviceMaintenance5Features, 0));
    });
    let device = rig.device();
    assert!(!device.info().queue_families[0]
        .capabilities
        .contains(tairix_gpu::QueueCapabilities::GRAPHICS));
    let context = context(&device);
    let refused = context.create_render_pipeline(&RenderPipelineDesc {
        layout: &context
            .create_pipeline_layout(&[], PushConstants::NONE)
            .expect("a layout"),
        vertex: StageDesc {
            module: &context
                .create_shader_module(admit(PASSING).expect("admitted"))
                .expect("a module"),
            entry_point: "main",
            specialization: &[],
        },
        fragment: None,
        vertex_buffers: &[VertexBuffer {
            stride: 32,
            step: VertexStep::Vertex,
            attributes: &ATTRIBUTES,
        }],
        topology: Topology::Triangles,
        primitive_restart: false,
        rasterization: Rasterization {
            cull: tairix_gpu::CullMode::None,
            front_face: FrontFace::CounterClockwise,
            depth_bias: None,
            depth_clamp: false,
            line_width: 1.0,
        },
        depth_stencil: None,
        multisample: Multisample {
            samples: 1,
            mask: u32::MAX,
            alpha_to_coverage: false,
        },
        targets: &[],
        logic_op: None,
    });
    assert_eq!(
        refused.err(),
        Some(Error::Unsupported),
        "no vertex attribute stated"
    );
    drop((context, device));
    let_go(&rig.finish());
}
