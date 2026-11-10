//! A pass and its draws checked whole as a submission carries them: every
//! attachment of one extent and sample count, made for its use and named
//! once, its area within them, each resolve from four samples to one, each
//! draw's buffers, viewport and bindings as its pipeline needs, and nothing a
//! draw binds overlapping what the pass writes.

use tairix_spirv::{Access, BindingKind, Component, ImageShape, Stages};

use crate::device_tests::{context, queue, submit};
use crate::render::tests::{
    bare, corners, floats, painting, passing, pipeline, plain, rendering, target, whole, Shape,
    Target, FILLED, ONE_SAMPLE, VEC4,
};
use crate::{
    BindSetLayout, ClearValue, ColourAttachment, ColourTarget, CommandBuffer, CompareOp, Context,
    DepthStencilAttachment, DepthStencilState, Draw, Drawer, Error, Format, IndexFormat, Indices,
    Load, Pass, PipelineLayout, Rect, RenderPipeline, Topology, Viewport,
};

/// A pipeline of the plain stages under `layout`, its depth state `depth`.
fn plain_pipeline(
    context: &Context,
    layout: &PipelineLayout,
    targets: &[ColourTarget],
    depth: Option<DepthStencilState>,
) -> RenderPipeline {
    pipeline(
        context,
        layout,
        (&passing(VEC4, ""), Some(&painting(""))),
        &Shape {
            topology: Topology::Triangles,
            targets,
            depth_stencil: depth,
            multisample: ONE_SAMPLE,
            rasterization: FILLED,
            restart: false,
        },
    )
    .expect("a pipeline")
}

/// A colour attachment of `view`, cleared and kept.
fn attachment(view: &crate::ImageView) -> ColourAttachment<'_> {
    ColourAttachment {
        view,
        resolve: None,
        load: Load::Clear(ClearValue::Float([0.0; 4])),
    }
}

/// The command a submission of `commands` is refused at, and why.
fn refusal(context: &Context, commands: &CommandBuffer) -> Result<(), (Option<usize>, Error)> {
    submit(&queue(context), commands)
        .map(|_| ())
        .map_err(|refused| (refused.command, refused.error))
}

/// A buffer of three vertices of eight floats.
fn three(context: &Context) -> (crate::Allocation, crate::Buffer) {
    floats(context, &corners(&[([0.0; 4], [0.0; 4]); 3]))
}

#[test]
fn a_pass_whose_attachments_or_area_disagree_is_refused_at_the_pass() {
    let device = rendering();
    let context = context(&device);
    let layout = bare(&context);
    let targets = [plain(Format::Rgba8Unorm)];
    let pipeline = plain_pipeline(&context, &layout, &targets, None);
    let (_held, buffer) = three(&context);
    let small = target(&context, Format::Rgba8Unorm, (8, 8), 1);
    let large = target(&context, Format::Rgba8Unorm, (16, 8), 1);
    let (viewport, scissor) = whole((8, 8));
    let draw = Draw {
        pipeline: &pipeline,
        sets: &[],
        push_constants: &[],
        vertex_buffers: &[(&buffer, 0)],
        viewport,
        scissor,
        stencil_reference: 0,
        blend_constant: [0.0; 4],
    };
    let passes: [(&str, Error, &[ColourAttachment<'_>], Rect); 5] = [
        (
            "attachments of two extents",
            Error::Shape,
            &[attachment(&small.view), attachment(&large.view)],
            scissor,
        ),
        (
            "one attachment twice",
            Error::Overlap,
            &[attachment(&small.view), attachment(&small.view)],
            scissor,
        ),
        (
            "an area of nothing",
            Error::Empty,
            &[attachment(&small.view)],
            Rect {
                width: 0,
                ..scissor
            },
        ),
        (
            "an area past its attachments",
            Error::OutOfRange,
            &[attachment(&small.view)],
            Rect { x: 4, ..scissor },
        ),
        ("no attachment", Error::Empty, &[], scissor),
    ];
    for (what, error, colour, area) in passes {
        let mut commands = context.command_buffer();
        commands
            .render_pass(
                &Pass {
                    colour,
                    depth_stencil: None,
                    area,
                },
                |drawer| drawer.draw(&draw, 0..3, 0..1),
            )
            .expect("recorded");
        assert_eq!(
            refusal(&context, &commands),
            Err((Some(0), error)),
            "{what}"
        );
    }
    let four = target(&context, Format::Rgba8Unorm, (8, 8), 4);
    for (what, error, from, into) in [
        ("a resolve from one sample", Error::Samples, &small, &large),
        ("a resolve into another extent", Error::Shape, &four, &large),
    ] {
        let resolving = [ColourAttachment {
            resolve: Some(&into.view),
            ..attachment(&from.view)
        }];
        let mut commands = context.command_buffer();
        commands
            .render_pass(
                &Pass {
                    colour: &resolving,
                    depth_stencil: None,
                    area: scissor,
                },
                |_| Ok(()),
            )
            .expect("recorded");
        assert_eq!(
            refusal(&context, &commands),
            Err((Some(0), error)),
            "{what}"
        );
    }
}

#[test]
fn a_draw_its_pipeline_could_not_run_is_refused_at_the_draw() {
    let device = rendering();
    let context = context(&device);
    let layout = bare(&context);
    let targets = [plain(Format::Rgba8Unorm)];
    let pipeline = plain_pipeline(&context, &layout, &targets, None);
    let (_held, buffer) = three(&context);
    let small = target(&context, Format::Rgba8Unorm, (8, 8), 1);
    let (viewport, scissor) = whole((8, 8));
    let draw = Draw {
        pipeline: &pipeline,
        sets: &[],
        push_constants: &[],
        vertex_buffers: &[(&buffer, 0)],
        viewport,
        scissor,
        stencil_reference: 0,
        blend_constant: [0.0; 4],
    };
    let far = Viewport {
        x: 1.0e9,
        ..viewport
    };
    let record = |commands: &mut CommandBuffer,
                  body: &dyn Fn(&mut Drawer<'_>) -> Result<(), Error>| {
        commands
            .render_pass(
                &Pass {
                    colour: &[attachment(&small.view)],
                    depth_stencil: None,
                    area: scissor,
                },
                |drawer| body(drawer),
            )
            .expect("recorded");
    };
    let draws: [(&str, Error, Draw<'_>); 2] = [
        (
            "a viewport past the device's bounds",
            Error::OutOfRange,
            Draw {
                viewport: far,
                ..draw
            },
        ),
        (
            "vertex buffers its pipeline does not read",
            Error::Bindings,
            Draw {
                vertex_buffers: &[],
                ..draw
            },
        ),
    ];
    for (what, error, draw) in draws {
        let mut commands = context.command_buffer();
        record(&mut commands, &|drawer| drawer.draw(&draw, 0..3, 0..1));
        assert_eq!(
            refusal(&context, &commands),
            Err((Some(1), error)),
            "{what}"
        );
    }
    let mut commands = context.command_buffer();
    let indices = Indices {
        buffer: &buffer,
        offset: 1,
        format: IndexFormat::U16,
    };
    record(&mut commands, &|drawer| {
        drawer.draw_indexed(&draw, &indices, (0..3, 0), 0..1)
    });
    assert_eq!(
        refusal(&context, &commands),
        Err((Some(1), Error::Misaligned)),
        "indices off their size"
    );
}

/// A layout of one sampled image the fragment stage reads.
fn sampling(context: &Context) -> (BindSetLayout, PipelineLayout) {
    let set_layout = context
        .create_bind_set_layout(&[crate::LayoutBinding {
            binding: 0,
            kind: BindingKind::SampledImage(ImageShape {
                dim: tairix_spirv::enumerant::Dim::DIM_2D,
                arrayed: false,
                multisampled: false,
                component: Component::Float,
                format: tairix_spirv::enumerant::ImageFormat::UNKNOWN,
            }),
            count: 1,
            access: Access::READ,
            stages: Stages::FRAGMENT,
        }])
        .expect("a layout");
    let layout = context
        .create_pipeline_layout(&[&set_layout], crate::PushConstants::NONE)
        .expect("a layout");
    (set_layout, layout)
}

/// Submit a pass drawing with `pipeline`, `sampled` bound, into `colour`
/// and, where given, `depth`, each loaded as it was.
fn draw_sampling(
    context: &Context,
    (pipeline, set_layout): (&RenderPipeline, &BindSetLayout),
    (colour, depth, sampled): (&Target, Option<&Target>, &Target),
) -> Result<(), (Option<usize>, Error)> {
    let set = context
        .create_bind_set(
            set_layout,
            &[crate::BindSetEntry {
                binding: 0,
                resources: &[crate::Resource::Image(&sampled.view)],
            }],
        )
        .expect("a set");
    let (_held, buffer) = three(context);
    let (viewport, scissor) = whole((8, 8));
    let mut commands = context.command_buffer();
    commands
        .render_pass(
            &Pass {
                colour: &[ColourAttachment {
                    load: Load::Keep,
                    ..attachment(&colour.view)
                }],
                depth_stencil: depth.map(|depth| DepthStencilAttachment {
                    view: &depth.view,
                    depth_load: Load::Keep,
                    stencil_load: Load::Keep,
                }),
                area: scissor,
            },
            |drawer| {
                drawer.draw(
                    &Draw {
                        pipeline,
                        sets: &[&set],
                        push_constants: &[],
                        vertex_buffers: &[(&buffer, 0)],
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
    refusal(context, &commands)
}

#[test]
fn a_draw_binding_what_its_pass_writes_is_refused() {
    let device = rendering();
    let context = context(&device);
    let (set_layout, layout) = sampling(&context);
    let targets = [plain(Format::Rgba8Unorm)];
    let pipeline = plain_pipeline(&context, &layout, &targets, None);
    let colour = target(&context, Format::Rgba8Unorm, (8, 8), 1);
    let other = target(&context, Format::Rgba8Unorm, (8, 8), 1);
    assert_eq!(
        draw_sampling(&context, (&pipeline, &set_layout), (&colour, None, &colour)),
        Err((Some(1), Error::Overlap))
    );
    draw_sampling(&context, (&pipeline, &set_layout), (&colour, None, &other))
        .expect("another image may be sampled");
}

#[test]
fn a_depth_attachment_only_tested_may_be_sampled_and_one_written_may_not() {
    let device = rendering();
    let context = context(&device);
    let (set_layout, layout) = sampling(&context);
    let targets = [plain(Format::Rgba8Unorm)];
    let colour = target(&context, Format::Rgba8Unorm, (8, 8), 1);
    let depth = target(&context, Format::Depth32Float, (8, 8), 1);
    for (write, refused) in [(false, false), (true, true)] {
        let state = DepthStencilState {
            format: Format::Depth32Float,
            depth_compare: CompareOp::Less,
            depth_write: write,
            stencil: None,
        };
        let pipeline = plain_pipeline(&context, &layout, &targets, Some(state));
        let drawn = draw_sampling(
            &context,
            (&pipeline, &set_layout),
            (&colour, Some(&depth), &depth),
        );
        let expected = if refused {
            Err((Some(1), Error::Overlap))
        } else {
            Ok(())
        };
        assert_eq!(drawn, expected, "depth written: {write}");
    }
}

/// An image of `format` to render to, with room past it in its allocation,
/// its view, and a buffer over all of its allocation and one over the room
/// past it alone.
struct Aliased {
    memory: crate::Allocation,
    image: crate::Image,
    view: crate::ImageView,
    whole: crate::Buffer,
    past: crate::Buffer,
}

fn aliased(context: &Context, format: Format) -> Aliased {
    let usage = if format.aspects() == crate::Aspects::COLOR {
        crate::ImageUsage::COLOR_ATTACHMENT
    } else {
        crate::ImageUsage::DEPTH_STENCIL_ATTACHMENT
    };
    let desc = crate::ImageDesc {
        dimension: crate::ImageDimension::D2,
        format,
        extent: crate::Extent3d {
            width: 8,
            height: 8,
            depth: 1,
        },
        mip_levels: 1,
        array_layers: 1,
        samples: 1,
        usage,
    };
    let size = context
        .image_requirements(&desc)
        .expect("requirements")
        .size;
    let memory = crate::device_tests::memory(context, size + 256);
    let image = context.create_image(&memory, 0, &desc).expect("an image");
    let view = context
        .create_image_view(
            &image,
            &crate::ImageViewDesc {
                format,
                dimension: crate::ViewDimension::D2,
                range: crate::ImageRange {
                    aspects: format.aspects(),
                    base_level: 0,
                    levels: 1,
                    base_layer: 0,
                    layers: 1,
                },
            },
        )
        .expect("a view");
    let whole = crate::device_tests::over(context, &memory, 0, size + 256);
    let past = crate::device_tests::over(context, &memory, size, 256);
    Aliased {
        memory,
        image,
        view,
        whole,
        past,
    }
}

#[test]
fn a_draw_reading_as_vertices_or_indices_what_its_pass_renders_to_is_refused() {
    let device = rendering();
    let context = context(&device);
    let layout = bare(&context);
    let targets = [plain(Format::Rgba8Unorm)];
    let pipeline = plain_pipeline(&context, &layout, &targets, None);
    let rendered = aliased(&context, Format::Rgba8Unorm);
    let (viewport, scissor) = whole((8, 8));
    let pass = Pass {
        colour: &[attachment(&rendered.view)],
        depth_stencil: None,
        area: scissor,
    };
    let (_held, apart) = three(&context);
    for (what, vertices, indices, refused) in [
        ("vertices over the target", &rendered.whole, None, true),
        (
            "indices over the target",
            &apart,
            Some(&rendered.whole),
            true,
        ),
        (
            "vertices past it in its memory",
            &rendered.past,
            None,
            false,
        ),
        (
            "indices past it in its memory",
            &apart,
            Some(&rendered.past),
            false,
        ),
    ] {
        let vertex_buffers = [(vertices, 0)];
        let draw = Draw {
            pipeline: &pipeline,
            sets: &[],
            push_constants: &[],
            vertex_buffers: &vertex_buffers,
            viewport,
            scissor,
            stencil_reference: 0,
            blend_constant: [0.0; 4],
        };
        let mut commands = context.command_buffer();
        commands
            .render_pass(&pass, |drawer| match indices {
                Some(buffer) => drawer.draw_indexed(
                    &draw,
                    &Indices {
                        buffer,
                        offset: 0,
                        format: IndexFormat::U16,
                    },
                    (0..3, 0),
                    0..1,
                ),
                None => drawer.draw(&draw, 0..3, 0..1),
            })
            .expect("recorded");
        let expected = if refused {
            Err((Some(1), Error::Overlap))
        } else {
            Ok(())
        };
        assert_eq!(refusal(&context, &commands), expected, "{what}");
    }
}

/// A set of one storage buffer, `buffer`, its layout seen by `stages`, and a
/// pipeline layout of it.
fn storage_set(
    context: &Context,
    stages: Stages,
    buffer: &crate::Buffer,
) -> (crate::BindSet, BindSetLayout, PipelineLayout) {
    let (set_layout, layout) = crate::render::tests::counted_layout(context, stages);
    let set = context
        .create_bind_set(
            &set_layout,
            &[crate::BindSetEntry {
                binding: 0,
                resources: &[crate::Resource::Buffer {
                    buffer,
                    offset: 0,
                    size: 16,
                }],
            }],
        )
        .expect("a set");
    (set, set_layout, layout)
}

#[test]
fn a_binding_reaching_what_its_pass_renders_to_is_refused_only_where_its_stages_see_it() {
    let device = rendering();
    let context = context(&device);
    let rendered = aliased(&context, Format::Rgba8Unorm);
    let targets = [plain(Format::Rgba8Unorm)];
    let (_held, buffer) = three(&context);
    let (viewport, scissor) = whole((8, 8));
    let over_target = crate::device_tests::over(&context, &rendered.memory, 0, 16);
    for (what, stages, bound, refused) in [
        (
            "seen by its fragment stage",
            Stages::FRAGMENT,
            &over_target,
            true,
        ),
        (
            "seen by compute alone",
            Stages::COMPUTE,
            &over_target,
            false,
        ),
        (
            "past it in its memory",
            Stages::FRAGMENT,
            &rendered.past,
            false,
        ),
    ] {
        let (set, _set_layout, layout) = storage_set(&context, stages, bound);
        let pipeline = plain_pipeline(&context, &layout, &targets, None);
        let mut commands = context.command_buffer();
        commands
            .render_pass(
                &Pass {
                    colour: &[attachment(&rendered.view)],
                    depth_stencil: None,
                    area: scissor,
                },
                |drawer| {
                    drawer.draw(
                        &Draw {
                            pipeline: &pipeline,
                            sets: &[&set],
                            push_constants: &[],
                            vertex_buffers: &[(&buffer, 0)],
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
        let expected = if refused {
            Err((Some(1), Error::Overlap))
        } else {
            Ok(())
        };
        assert_eq!(refusal(&context, &commands), expected, "a binding {what}");
    }
}

#[test]
fn a_draw_writing_a_depth_attachment_it_only_tests_is_refused() {
    let device = rendering();
    let context = context(&device);
    let colour = target(&context, Format::Rgba8Unorm, (8, 8), 1);
    let depth = aliased(&context, Format::Depth32Float);
    let over_depth = crate::device_tests::over(&context, &depth.memory, 0, 16);
    let (set, _set_layout, layout) = storage_set(&context, Stages::FRAGMENT, &over_depth);
    let counting = crate::render::tests::counting(false);
    let targets = [plain(Format::Rgba8Unorm)];
    let pipeline = pipeline(
        &context,
        &layout,
        (&passing(VEC4, ""), Some(&counting)),
        &Shape {
            topology: Topology::Triangles,
            targets: &targets,
            depth_stencil: Some(DepthStencilState {
                format: Format::Depth32Float,
                depth_compare: CompareOp::Less,
                depth_write: false,
                stencil: None,
            }),
            multisample: ONE_SAMPLE,
            rasterization: FILLED,
            restart: false,
        },
    )
    .expect("a pipeline");
    let (_held, buffer) = three(&context);
    let (viewport, scissor) = whole((8, 8));
    let mut commands = context.command_buffer();
    commands
        .render_pass(
            &Pass {
                colour: &[attachment(&colour.view)],
                depth_stencil: Some(DepthStencilAttachment {
                    view: &depth.view,
                    depth_load: Load::Keep,
                    stencil_load: Load::Keep,
                }),
                area: scissor,
            },
            |drawer| {
                drawer.draw(
                    &Draw {
                        pipeline: &pipeline,
                        sets: &[&set],
                        push_constants: &[],
                        vertex_buffers: &[(&buffer, 0)],
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
    assert_eq!(refusal(&context, &commands), Err((Some(1), Error::Overlap)));
    drop(depth.image);
}

#[test]
fn an_attachment_of_a_format_the_device_does_not_render_to_is_refused() {
    let device = crate::binding::tests::device_stating(|info| {
        info.formats = crate::FormatTable::from_fn(|format| {
            let stated = format.software_features();
            if format == Format::Rgba8UnormSrgb {
                stated.without(crate::FormatFeatures::COLOR_ATTACHMENT)
            } else {
                stated
            }
        });
    });
    let context = context(&device);
    let rendered = target(&context, Format::Rgba8Unorm, (8, 8), 1);
    let twin = context
        .create_image_view(
            &rendered.image,
            &crate::ImageViewDesc {
                format: Format::Rgba8UnormSrgb,
                dimension: crate::ViewDimension::D2,
                range: crate::ImageRange {
                    aspects: crate::Aspects::COLOR,
                    base_level: 0,
                    levels: 1,
                    base_layer: 0,
                    layers: 1,
                },
            },
        )
        .expect("the sRGB twin's view");
    let (_, scissor) = whole((8, 8));
    for (view, expected) in [
        (&rendered.view, Ok(())),
        (&twin, Err((Some(0), Error::Unsupported))),
    ] {
        let mut commands = context.command_buffer();
        commands
            .render_pass(
                &Pass {
                    colour: &[attachment(view)],
                    depth_stencil: None,
                    area: scissor,
                },
                |_| Ok(()),
            )
            .expect("recorded");
        assert_eq!(refusal(&context, &commands), expected);
    }
}

#[test]
fn a_queued_pass_is_charged_for_its_own_bytes() {
    let device = rendering();
    let context = context(&device);
    let colour = target(&context, Format::Rgba8Unorm, (8, 8), 1);
    let (_, scissor) = whole((8, 8));
    let (_held, filled) = three(&context);
    let queue = queue(&context);
    let gate = context.create_timeline(0).expect("a gate");
    let opened = core::cell::Cell::new(0u64);
    // What a submission of an empty pass and one command more is charged
    // while it waits behind the gate, which then lets it run.
    let charged = |second_pass: bool| {
        let mut commands = context.command_buffer();
        let pass = Pass {
            colour: &[attachment(&colour.view)],
            depth_stencil: None,
            area: scissor,
        };
        commands.render_pass(&pass, |_| Ok(())).expect("recorded");
        if second_pass {
            commands.render_pass(&pass, |_| Ok(())).expect("recorded");
        } else {
            commands.fill_buffer(&filled, 0, 16, 0).expect("recorded");
        }
        let next = opened.get() + 1;
        let before = context.memory_used().objects;
        let fence = queue
            .submit(&crate::Submission {
                command_buffers: &[&commands],
                waits: &[(&gate, next)],
                signals: &[],
                deadline: crate::device_tests::SECOND,
            })
            .expect("admitted");
        let bytes = context.memory_used().objects - before;
        gate.signal(next).expect("opened");
        opened.set(next);
        fence.wait(crate::device_tests::SECOND).expect("ran");
        bytes
    };
    // Each table's room made first, so neither measure pays for room.
    charged(true);
    charged(false);
    let (with_fill, with_pass) = (charged(false), charged(true));
    let own = core::mem::size_of::<crate::pass::PassWork>() as u64;
    assert!(
        with_pass.saturating_sub(with_fill) >= own,
        "a queued pass is charged for its own {own} bytes: {with_pass} against {with_fill}"
    );
}
