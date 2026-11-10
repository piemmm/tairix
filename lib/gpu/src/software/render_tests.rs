//! Passes drawn on the software device, their pixels pinned: coverage and
//! shared edges, each interpolation qualifier across a near-plane clip, depth,
//! stencil, blending and write masks, points, lines, strips, fans and
//! restarts, four samples and their resolve, early tests, discards, quads of
//! several primitives sharing a tile, a pass of many bins and flushes, a pass
//! of no draws, and the targets and channels a fragment stage leaves.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::ops::Range;

use crate::device_tests::{context, queue, submit};
use tairix_util::mathf::single;

use crate::render::tests::{
    bare, corners, floats, painting, passing, pipeline, plain, read, real, rendering, target,
    whole, Shape, Target, FILLED, ONE_SAMPLE, VEC4,
};
use crate::{
    Blend, BlendFactor, BlendOp, ClearValue, ColourAttachment, ColourMask, ColourTarget, CompareOp,
    Context, CullMode, DepthStencilAttachment, DepthStencilState, Draw, Drawer, Error, Format,
    IndexFormat, Indices, Load, Multisample, Pass, Rasterization, Rect, RenderPipeline,
    StencilFaceState, StencilOp, Topology, Viewport,
};

/// A corner of a quad over pixel rectangle `x0..x1` by `y0..y1` of a target
/// `extent` pixels across, at depth `z`, coloured `colour`: two triangles.
fn rectangle(
    extent: (u32, u32),
    (x0, y0, x1, y1): (f32, f32, f32, f32),
    (z, colour): (f32, [f32; 4]),
) -> Vec<([f32; 4], [f32; 4])> {
    let ndc = |x: f32, y: f32| {
        [
            x / real(extent.0) * 2.0 - 1.0,
            y / real(extent.1) * 2.0 - 1.0,
            z,
            1.0,
        ]
    };
    let at = |x, y| (ndc(x, y), colour);
    [
        at(x0, y0),
        at(x1, y0),
        at(x1, y1),
        at(x0, y0),
        at(x1, y1),
        at(x0, y1),
    ]
    .to_vec()
}

/// Render `count` vertices of `buffer` with each of `draws`' pipelines in
/// turn into `colour`, and `depth` where given, every attachment cleared.
fn render(
    context: &Context,
    (colour, depth): (&[&Target], Option<&Target>),
    draws: &[(&RenderPipeline, &crate::Buffer, Range<u32>)],
    (stencil_reference, blend_constant): (u8, [f32; 4]),
) {
    let extent = colour
        .first()
        .or(depth.as_ref())
        .map_or((1, 1), |t| t.extent);
    let (viewport, scissor) = whole(extent);
    let attachments: Vec<ColourAttachment<'_>> = colour
        .iter()
        .map(|target| ColourAttachment {
            view: &target.view,
            resolve: None,
            load: Load::Clear(clear_of(target.format)),
        })
        .collect();
    let depth_stencil = depth.map(|target| DepthStencilAttachment {
        view: &target.view,
        depth_load: Load::Clear(ClearValue::DepthStencil {
            depth: 1.0,
            stencil: 0,
        }),
        stencil_load: Load::Clear(ClearValue::DepthStencil {
            depth: 1.0,
            stencil: 0,
        }),
    });
    let mut commands = context.command_buffer();
    commands
        .render_pass(
            &Pass {
                colour: &attachments,
                depth_stencil,
                area: scissor,
            },
            |drawer: &mut Drawer<'_>| {
                for (pipeline, buffer, range) in draws {
                    drawer.draw(
                        &Draw {
                            pipeline,
                            sets: &[],
                            push_constants: &[],
                            vertex_buffers: &[(buffer, 0)],
                            viewport,
                            scissor,
                            stencil_reference,
                            blend_constant,
                        },
                        range.clone(),
                        0..1,
                    )?;
                }
                Ok(())
            },
        )
        .expect("recorded");
    submit(&queue(context), &commands).expect("drawn");
}

/// Black, transparent: what a colour target of `format` clears to.
fn clear_of(format: Format) -> ClearValue {
    match format.component() {
        tairix_spirv::Component::Float => ClearValue::Float([0.0; 4]),
        tairix_spirv::Component::Sint => ClearValue::Sint([0; 4]),
        tairix_spirv::Component::Uint => ClearValue::Uint([0; 4]),
    }
}

/// The pipeline of the plain shaders over targets `targets`, drawing
/// `topology`, as `shape` says beyond.
fn plain_pipeline(
    context: &Context,
    targets: &[ColourTarget],
    topology: Topology,
) -> RenderPipeline {
    let layout = bare(context);
    pipeline(
        context,
        &layout,
        (&passing(VEC4, ""), Some(&painting(""))),
        &Shape {
            topology,
            targets,
            depth_stencil: None,
            multisample: ONE_SAMPLE,
            rasterization: FILLED,
            restart: false,
        },
    )
    .expect("a pipeline")
}

/// The pixels of an `extent` target whose first byte is not nought.
fn painted(pixels: &[u8], extent: (u32, u32), bytes: usize) -> Vec<(u32, u32)> {
    let mut found = Vec::new();
    for y in 0..extent.1 {
        for x in 0..extent.0 {
            let at = (y * extent.0 + x) as usize * bytes;
            if pixels[at] != 0 {
                found.push((x, y));
            }
        }
    }
    found
}

#[test]
fn a_triangle_covers_the_pixels_whose_centres_it_holds() {
    let device = rendering();
    let context = context(&device);
    let extent = (16, 16);
    let targets = [plain(Format::Rgba8Unorm)];
    let pipeline = plain_pipeline(&context, &targets, Topology::Triangles);
    let ndc = |x: f32, y: f32| ([x / 8.0 - 1.0, y / 8.0 - 1.0, 0.5, 1.0], [1.0; 4]);
    let (_held, buffer) = floats(
        &context,
        &corners(&[ndc(1.0, 1.0), ndc(9.5, 1.0), ndc(1.0, 9.5)]),
    );
    let colour = target(&context, Format::Rgba8Unorm, extent, 1);
    render(
        &context,
        (&[&colour], None),
        &[(&pipeline, &buffer, 0..3)],
        (0, [0.0; 4]),
    );
    let found = painted(&read(&context, &colour), extent, 4);
    let mut expected = Vec::new();
    for y in 0..16u32 {
        for x in 0..16u32 {
            if x >= 1 && y >= 1 && x + y <= 9 {
                expected.push((x, y));
            }
        }
    }
    assert_eq!(found, expected);
}

/// A blend adding the source to what is held.
const ADD: Blend = Blend {
    source_color: BlendFactor::One,
    destination_color: BlendFactor::One,
    color: BlendOp::Add,
    source_alpha: BlendFactor::One,
    destination_alpha: BlendFactor::One,
    alpha: BlendOp::Add,
};

#[test]
fn triangles_sharing_an_edge_through_pixel_centres_cover_each_pixel_once() {
    let device = rendering();
    let context = context(&device);
    let extent = (16, 16);
    let targets = [ColourTarget {
        format: Format::Rgba8Unorm,
        blend: Some(ADD),
        write_mask: ColourMask::ALL,
    }];
    let pipeline = plain_pipeline(&context, &targets, Topology::Triangles);
    // Each half of the square adds a quarter: a pixel both covered would
    // hold a half.
    let quarter = [0.25, 0.0, 0.0, 0.0];
    let ndc = |x: f32, y: f32| ([x / 8.0 - 1.0, y / 8.0 - 1.0, 0.5, 1.0], quarter);
    // The diagonal from (0.5, 0.5) to (15.5, 15.5) runs through every centre
    // on it.
    let (_held, buffer) = floats(
        &context,
        &corners(&[
            ndc(0.5, 0.5),
            ndc(15.5, 0.5),
            ndc(15.5, 15.5),
            ndc(0.5, 0.5),
            ndc(15.5, 15.5),
            ndc(0.5, 15.5),
        ]),
    );
    let colour = target(&context, Format::Rgba8Unorm, extent, 1);
    render(
        &context,
        (&[&colour], None),
        &[(&pipeline, &buffer, 0..6)],
        (0, [0.0; 4]),
    );
    let pixels = read(&context, &colour);
    for y in 1..15u32 {
        for x in 1..15u32 {
            let red = pixels[((y * 16 + x) * 4) as usize];
            assert_eq!(red, 64, "pixel ({x}, {y}) covered once");
        }
    }
}

/// A fragment shader writing its location-0 float input to an `R32Float`
/// target, the input decorated `decorations`.
fn float_painting(decorations: &str) -> String {
    format!(
        r#"OpCapability Shader
OpMemoryModel Logical GLSL450
OpEntryPoint Fragment %main "main" %in_value %out_value
OpExecutionMode %main OriginUpperLeft
OpDecorate %in_value Location 0
OpDecorate %out_value Location 0
{decorations}
%void = OpTypeVoid
%fn = OpTypeFunction %void
%float = OpTypeFloat 32
%v4 = OpTypeVector %float 4
%in_v4 = OpTypePointer Input %v4
%out_float = OpTypePointer Output %float
%in_value = OpVariable %in_v4 Input
%out_value = OpVariable %out_float Output
%main = OpFunction %void None %fn
%entry = OpLabel
%v = OpLoad %v4 %in_value
%r = OpCompositeExtract %float %v 0
OpStore %out_value %r
OpReturn
OpFunctionEnd
"#
    )
}

/// The floats of an `R32Float` target.
fn read_floats(context: &Context, target: &Target) -> Vec<f32> {
    read(context, target)
        .chunks(4)
        .map(|bytes| f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
        .collect()
}

/// The value a triangle of clip-space `corners` holding `values` takes at
/// pixel `(x, y)` of a 16-by-16 target, interpolated as `how` says, and
/// whether the triangle covers its centre.
fn expected(
    corners: &[[f64; 4]; 3],
    values: [f64; 3],
    (x, y): (u32, u32),
    how: &str,
) -> Option<f64> {
    let screen = corners.map(|[cx, cy, _, w]| ((cx / w + 1.0) * 8.0, (cy / w + 1.0) * 8.0));
    let (px, py) = (f64::from(x) + 0.5, f64::from(y) + 0.5);
    let [(x0, y0), (x1, y1), (x2, y2)] = screen;
    let det = (x1 - x0) * (y2 - y0) - (x2 - x0) * (y1 - y0);
    let one = ((px - x0) * (y2 - y0) - (x2 - x0) * (py - y0)) / det;
    let two = ((x1 - x0) * (py - y0) - (px - x0) * (y1 - y0)) / det;
    let lambda = [1.0 - one - two, one, two];
    if lambda.iter().any(|&l| l < 1e-9) {
        return None;
    }
    Some(match how {
        "flat" => values[0],
        "linear" => (0..3).map(|i| lambda[i] * values[i]).sum(),
        _ => {
            let q: Vec<f64> = (0..3).map(|i| lambda[i] / corners[i][3]).collect();
            let total: f64 = q.iter().sum();
            (0..3).map(|i| q[i] / total * values[i]).sum()
        }
    })
}

#[test]
fn each_interpolation_qualifier_holds_across_a_near_plane_clip() {
    let device = rendering();
    let context = context(&device);
    let layout = bare(&context);
    let extent = (16, 16);
    // The third corner lies behind the near plane, so the clip cuts the
    // triangle; what is left must interpolate as the whole one does.
    let triangle: [[f64; 4]; 3] = [
        [-0.9, -0.9, 0.2, 1.0],
        [0.9 * 3.0, -0.6 * 3.0, 0.5 * 3.0, 3.0],
        [-0.2 * 2.0, 0.9 * 2.0, -0.5, 2.0],
    ];
    let values = [1.0, 5.0, 9.0];
    let points: Vec<([f32; 4], [f32; 4])> = triangle
        .iter()
        .zip(values)
        .map(|(corner, value)| (corner.map(single), [single(value), 0.0, 0.0, 0.0]))
        .collect();
    let (_held, buffer) = floats(&context, &corners(&points));
    for (how, decoration) in [
        ("perspective", ""),
        ("linear", "OpDecorate %in_value NoPerspective"),
        ("flat", "OpDecorate %in_value Flat"),
    ] {
        let vertex_decoration = decoration.replace("%in_value", "%out_value");
        let targets = [plain(Format::R32Float)];
        let pipeline = pipeline(
            &context,
            &layout,
            (
                &passing(VEC4, &vertex_decoration),
                Some(&float_painting(decoration)),
            ),
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
        let colour = target(&context, Format::R32Float, extent, 1);
        render(
            &context,
            (&[&colour], None),
            &[(&pipeline, &buffer, 0..3)],
            (0, [0.0; 4]),
        );
        let got = read_floats(&context, &colour);
        let mut checked = 0;
        for y in 0..16u32 {
            for x in 0..16u32 {
                let held = f64::from(got[(y * 16 + x) as usize]);
                // Only where the whole triangle's centre lies in front of the
                // near plane does the clipped one cover it.
                let Some(want) = expected(&triangle, values, (x, y), how) else {
                    continue;
                };
                if held == 0.0 {
                    continue;
                }
                // A corner lands on the converter's grid, a 256th of a pixel,
                // which over a triangle this small moves a value by about a
                // thousandth of its range; linear and perspective weights here
                // differ by far more.
                assert!(
                    (held - want).abs() <= 2e-3,
                    "{how} at ({x}, {y}): {held} against {want}"
                );
                checked += 1;
            }
        }
        assert!(
            checked > 20,
            "{how}: the clipped triangle drew {checked} pixels"
        );
    }
}

/// A depth test of `compare`, written, with no stencil.
const fn depth(compare: CompareOp, write: bool) -> DepthStencilState {
    DepthStencilState {
        format: Format::Depth32Float,
        depth_compare: compare,
        depth_write: write,
        stencil: None,
    }
}

fn depth_pipeline(
    context: &Context,
    targets: &[ColourTarget],
    (state, rasterization): (Option<DepthStencilState>, Rasterization),
) -> RenderPipeline {
    let layout = bare(context);
    pipeline(
        context,
        &layout,
        (&passing(VEC4, ""), Some(&painting(""))),
        &Shape {
            topology: Topology::Triangles,
            targets,
            depth_stencil: state,
            multisample: ONE_SAMPLE,
            rasterization,
            restart: false,
        },
    )
    .expect("a pipeline")
}

#[test]
fn the_nearer_of_two_quads_is_kept_whatever_order_they_are_drawn_in() {
    let device = rendering();
    let context = context(&device);
    let extent = (8, 8);
    let targets = [plain(Format::Rgba8Unorm)];
    let pipeline = depth_pipeline(
        &context,
        &targets,
        (Some(depth(CompareOp::Less, true)), FILLED),
    );
    let mut points = rectangle(extent, (0.0, 0.0, 8.0, 8.0), (0.25, [0.0, 1.0, 0.0, 1.0]));
    points.extend(rectangle(
        extent,
        (0.0, 0.0, 8.0, 8.0),
        (0.75, [1.0, 0.0, 0.0, 1.0]),
    ));
    let (_held, buffer) = floats(&context, &corners(&points));
    let colour = target(&context, Format::Rgba8Unorm, extent, 1);
    let depth_target = target(&context, Format::Depth32Float, extent, 1);
    render(
        &context,
        (&[&colour], Some(&depth_target)),
        &[(&pipeline, &buffer, 0..12)],
        (0, [0.0; 4]),
    );
    for texel in read(&context, &colour).chunks(4) {
        assert_eq!(texel, [0, 255, 0, 255], "the near green holds");
    }
    let depths: Vec<f32> = read(&context, &depth_target)
        .chunks(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect();
    assert!(
        depths.iter().all(|&d| d.to_bits() == 0.25f32.to_bits()),
        "{depths:?}"
    );
}

#[test]
fn a_bias_lets_a_coplanar_quad_pass_a_strict_test() {
    let device = rendering();
    let context = context(&device);
    let extent = (8, 8);
    let targets = [plain(Format::Rgba8Unorm)];
    let biased = Rasterization {
        depth_bias: Some(crate::DepthBias {
            constant: -4.0,
            slope: 0.0,
            clamp: 0.0,
        }),
        ..FILLED
    };
    for (bias, expected) in [(FILLED, [0, 0, 255, 255]), (biased, [255, 255, 0, 255])] {
        let first = depth_pipeline(
            &context,
            &targets,
            (Some(depth(CompareOp::Less, true)), FILLED),
        );
        let second = depth_pipeline(
            &context,
            &targets,
            (Some(depth(CompareOp::Less, true)), bias),
        );
        let blue = rectangle(extent, (0.0, 0.0, 8.0, 8.0), (0.5, [0.0, 0.0, 1.0, 1.0]));
        let yellow = rectangle(extent, (0.0, 0.0, 8.0, 8.0), (0.5, [1.0, 1.0, 0.0, 1.0]));
        let (_a, blue) = floats(&context, &corners(&blue));
        let (_b, yellow) = floats(&context, &corners(&yellow));
        let colour = target(&context, Format::Rgba8Unorm, extent, 1);
        let depth_target = target(&context, Format::Depth32Float, extent, 1);
        render(
            &context,
            (&[&colour], Some(&depth_target)),
            &[(&first, &blue, 0..6), (&second, &yellow, 0..6)],
            (0, [0.0; 4]),
        );
        for texel in read(&context, &colour).chunks(4) {
            assert_eq!(texel, expected);
        }
    }
}

#[test]
fn a_stencil_written_by_one_draw_masks_the_next() {
    let device = rendering();
    let context = context(&device);
    let extent = (8, 8);
    let targets = [plain(Format::Rgba8Unorm)];
    let write = StencilFaceState {
        fail: StencilOp::Keep,
        pass: StencilOp::Replace,
        depth_fail: StencilOp::Keep,
        compare: CompareOp::Always,
        compare_mask: 0xff,
        write_mask: 0xff,
    };
    let test = StencilFaceState {
        pass: StencilOp::Keep,
        compare: CompareOp::Equal,
        ..write
    };
    let stencil = |face| DepthStencilState {
        format: Format::Depth32FloatStencil8,
        depth_compare: CompareOp::Always,
        depth_write: false,
        stencil: Some([face, face]),
    };
    let marking = depth_pipeline(&context, &targets, (Some(stencil(write)), FILLED));
    let tested = depth_pipeline(&context, &targets, (Some(stencil(test)), FILLED));
    let (_a, left) = floats(
        &context,
        &corners(&rectangle(
            extent,
            (0.0, 0.0, 4.0, 8.0),
            (0.5, [0.0, 0.0, 1.0, 1.0]),
        )),
    );
    let (_b, all) = floats(
        &context,
        &corners(&rectangle(
            extent,
            (0.0, 0.0, 8.0, 8.0),
            (0.5, [1.0, 0.0, 0.0, 1.0]),
        )),
    );
    let colour = target(&context, Format::Rgba8Unorm, extent, 1);
    let depth_target = target(&context, Format::Depth32FloatStencil8, extent, 1);
    render(
        &context,
        (&[&colour], Some(&depth_target)),
        &[(&marking, &left, 0..6), (&tested, &all, 0..6)],
        (1, [0.0; 4]),
    );
    let pixels = read(&context, &colour);
    for y in 0..8u32 {
        for x in 0..8u32 {
            let texel = &pixels[((y * 8 + x) * 4) as usize..][..4];
            let want: &[u8] = if x < 4 {
                &[255, 0, 0, 255]
            } else {
                &[0, 0, 0, 0]
            };
            assert_eq!(texel, want, "pixel ({x}, {y})");
        }
    }
}

#[test]
fn blending_write_masks_and_the_blend_constant_combine_as_stated() {
    let device = rendering();
    let context = context(&device);
    let extent = (8, 8);
    let constant = Blend {
        source_color: BlendFactor::ConstantColor,
        destination_color: BlendFactor::Zero,
        color: BlendOp::Add,
        source_alpha: BlendFactor::One,
        destination_alpha: BlendFactor::Zero,
        alpha: BlendOp::Add,
    };
    for (target_state, expected) in [
        (
            ColourTarget {
                format: Format::Rgba8Unorm,
                blend: Some(Blend::OVER),
                write_mask: ColourMask::ALL,
            },
            [128, 128, 128, 128],
        ),
        (
            ColourTarget {
                format: Format::Rgba8Unorm,
                blend: None,
                write_mask: ColourMask::RED.with(ColourMask::ALPHA),
            },
            [128, 0, 0, 128],
        ),
        (
            ColourTarget {
                format: Format::Rgba8Unorm,
                blend: Some(constant),
                write_mask: ColourMask::ALL,
            },
            [64, 0, 0, 128],
        ),
    ] {
        let targets = [target_state];
        let pipeline = plain_pipeline(&context, &targets, Topology::Triangles);
        let (_held, buffer) = floats(
            &context,
            &corners(&rectangle(
                extent,
                (0.0, 0.0, 8.0, 8.0),
                (0.5, [0.5, 0.5, 0.5, 0.5]),
            )),
        );
        let colour = target(&context, Format::Rgba8Unorm, extent, 1);
        render(
            &context,
            (&[&colour], None),
            &[(&pipeline, &buffer, 0..6)],
            (0, [0.5, 0.0, 0.0, 0.0]),
        );
        for texel in read(&context, &colour).chunks(4) {
            // A premultiplied half over black, the masked green and blue
            // kept, or the source scaled by the constant's red.
            let close = texel
                .iter()
                .zip(expected)
                .all(|(&got, want)| got.abs_diff(want) <= 1);
            assert!(close, "{texel:?} against {expected:?}");
        }
    }
}

/// A vertex shader passing its position and writing a point size of four.
fn sized_points() -> String {
    passing(VEC4, "OpDecorate %size BuiltIn PointSize")
        .replace(
            "%out_value = OpVariable %out_value_t Output",
            "%out_value = OpVariable %out_value_t Output\n%out_float = OpTypePointer Output %float\n%size = OpVariable %out_float Output\n%four = OpConstant %float 4.0",
        )
        .replace("OpStore %out_value %v", "OpStore %out_value %v\nOpStore %size %four")
        .replace(
            "%in_position %in_value %out_position %out_value",
            "%in_position %in_value %out_position %out_value %size",
        )
}

#[test]
fn points_cover_their_square_and_lines_their_rectangle() {
    let device = rendering();
    let context = context(&device);
    let layout = bare(&context);
    let extent = (16, 16);
    let targets = [plain(Format::Rgba8Unorm)];
    let ndc = |x: f32, y: f32| ([x / 8.0 - 1.0, y / 8.0 - 1.0, 0.5, 1.0], [1.0; 4]);
    for (topology, vertex, points, expected) in [
        (
            Topology::Points,
            sized_points(),
            vec_of(&[ndc(4.0, 4.0)]),
            square_pixels((2, 2), (6, 6)),
        ),
        (
            Topology::Lines,
            passing(VEC4, ""),
            vec_of(&[ndc(1.0, 2.5), ndc(9.0, 2.5)]),
            square_pixels((1, 2), (9, 3)),
        ),
    ] {
        let pipeline = pipeline(
            &context,
            &layout,
            (&vertex, Some(&painting(""))),
            &Shape {
                topology,
                targets: &targets,
                depth_stencil: None,
                multisample: ONE_SAMPLE,
                rasterization: FILLED,
                restart: false,
            },
        )
        .expect("a pipeline");
        let (_held, buffer) = floats(&context, &corners(&points));
        let colour = target(&context, Format::Rgba8Unorm, extent, 1);
        let count = u32::try_from(points.len()).expect("small");
        render(
            &context,
            (&[&colour], None),
            &[(&pipeline, &buffer, 0..count)],
            (0, [0.0; 4]),
        );
        assert_eq!(
            painted(&read(&context, &colour), extent, 4),
            expected,
            "{topology:?}"
        );
    }
}

fn vec_of(points: &[([f32; 4], [f32; 4])]) -> Vec<([f32; 4], [f32; 4])> {
    points.to_vec()
}

/// The pixels of `x0..x1` by `y0..y1`, row by row.
fn square_pixels((x0, y0): (u32, u32), (x1, y1): (u32, u32)) -> Vec<(u32, u32)> {
    let mut found = Vec::new();
    for y in y0..y1 {
        for x in x0..x1 {
            found.push((x, y));
        }
    }
    found
}

#[test]
fn strips_fans_and_a_restarted_indexed_strip_cover_their_quads() {
    let device = rendering();
    let context = context(&device);
    let layout = bare(&context);
    let extent = (16, 16);
    let targets = [plain(Format::Rgba8Unorm)];
    let ndc = |x: f32, y: f32| ([x / 8.0 - 1.0, y / 8.0 - 1.0, 0.5, 1.0], [1.0; 4]);
    let strip = vec_of(&[ndc(0.0, 0.0), ndc(4.0, 0.0), ndc(0.0, 4.0), ndc(4.0, 4.0)]);
    let fan = vec_of(&[ndc(0.0, 0.0), ndc(4.0, 0.0), ndc(4.0, 4.0), ndc(0.0, 4.0)]);
    for (topology, points) in [
        (Topology::TriangleStrip, &strip),
        (Topology::TriangleFan, &fan),
    ] {
        let targets = [plain(Format::Rgba8Unorm)];
        let pipeline = plain_pipeline(&context, &targets, topology);
        let (_held, buffer) = floats(&context, &corners(points));
        let colour = target(&context, Format::Rgba8Unorm, extent, 1);
        render(
            &context,
            (&[&colour], None),
            &[(&pipeline, &buffer, 0..4)],
            (0, [0.0; 4]),
        );
        assert_eq!(
            painted(&read(&context, &colour), extent, 4),
            square_pixels((0, 0), (4, 4)),
            "{topology:?}"
        );
    }
    // Two quads of one strip, apart where an index of all ones restarts it,
    // their indices offset by a base vertex.
    let restarting = pipeline(
        &context,
        &layout,
        (&passing(VEC4, ""), Some(&painting(""))),
        &Shape {
            topology: Topology::TriangleStrip,
            targets: &targets,
            depth_stencil: None,
            multisample: ONE_SAMPLE,
            rasterization: FILLED,
            restart: true,
        },
    )
    .expect("a pipeline");
    let mut points = vec_of(&[ndc(0.0, 0.0); 2]);
    points.extend(strip.iter().copied());
    points.extend([
        ndc(8.0, 8.0),
        ndc(12.0, 8.0),
        ndc(8.0, 12.0),
        ndc(12.0, 12.0),
    ]);
    let (_vertices, buffer) = floats(&context, &corners(&points));
    let indices: [u16; 9] = [0, 1, 2, 3, u16::MAX, 4, 5, 6, 7];
    let bytes: Vec<u8> = indices.iter().flat_map(|i| i.to_le_bytes()).collect();
    let held = crate::device_tests::memory(&context, 32);
    let index_buffer = crate::device_tests::over(&context, &held, 0, 32);
    held.write(0, &bytes).expect("written");
    let colour = target(&context, Format::Rgba8Unorm, extent, 1);
    let (viewport, scissor) = whole(extent);
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
                drawer.draw_indexed(
                    &Draw {
                        pipeline: &restarting,
                        sets: &[],
                        push_constants: &[],
                        vertex_buffers: &[(&buffer, 0)],
                        viewport,
                        scissor,
                        stencil_reference: 0,
                        blend_constant: [0.0; 4],
                    },
                    &Indices {
                        buffer: &index_buffer,
                        offset: 0,
                        format: IndexFormat::U16,
                    },
                    (0..9, 2),
                    0..1,
                )
            },
        )
        .expect("recorded");
    submit(&queue(&context), &commands).expect("drawn");
    let mut expected = square_pixels((0, 0), (4, 4));
    expected.extend(square_pixels((8, 8), (12, 12)));
    expected.sort_unstable_by_key(|&(x, y)| (y, x));
    assert_eq!(painted(&read(&context, &colour), extent, 4), expected);
}

#[test]
fn four_samples_resolve_an_edge_to_its_coverage_and_a_mask_narrows_it() {
    let device = rendering();
    let context = context(&device);
    let layout = bare(&context);
    let extent = (8, 8);
    let targets = [plain(Format::Rgba8Unorm)];
    for (mask, full) in [(u32::MAX, 255u8), (0b0011, 128)] {
        let pipeline = pipeline(
            &context,
            &layout,
            (&passing(VEC4, ""), Some(&painting(""))),
            &Shape {
                topology: Topology::Triangles,
                targets: &targets,
                depth_stencil: None,
                multisample: Multisample {
                    samples: 4,
                    mask,
                    alpha_to_coverage: false,
                },
                rasterization: FILLED,
                restart: false,
            },
        )
        .expect("a pipeline");
        let ndc = |x: f32, y: f32| ([x / 4.0 - 1.0, y / 4.0 - 1.0, 0.5, 1.0], [1.0; 4]);
        let (_held, buffer) = floats(
            &context,
            &corners(&[ndc(0.0, 0.0), ndc(8.0, 0.0), ndc(0.0, 8.0)]),
        );
        let samples = target(&context, Format::Rgba8Unorm, extent, 4);
        let resolved = target(&context, Format::Rgba8Unorm, extent, 1);
        let (viewport, scissor) = whole(extent);
        let mut commands = context.command_buffer();
        commands
            .render_pass(
                &Pass {
                    colour: &[ColourAttachment {
                        view: &samples.view,
                        resolve: Some(&resolved.view),
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
        submit(&queue(&context), &commands).expect("drawn");
        let pixels = read(&context, &resolved);
        let red = |x: u32, y: u32| pixels[((y * 8 + x) * 4) as usize];
        assert_eq!(red(0, 0), full, "a pixel wholly inside");
        assert_eq!(red(7, 7), 0, "a pixel wholly outside");
        // On the diagonal edge, some samples are covered and some not.
        let edge = red(3, 4);
        assert!(
            edge > 0 && edge < full,
            "an edge pixel resolves between: {edge}"
        );
    }
}

#[test]
fn early_tests_shade_only_what_passes_and_late_ones_shade_everything() {
    let device = rendering();
    let context = context(&device);
    let extent = (8, 8);
    let (set_layout, layout) =
        crate::render::tests::counted_layout(&context, tairix_spirv::Stages::FRAGMENT);
    let targets = [plain(Format::Rgba8Unorm)];
    for (early, shaded) in [(true, 64u32), (false, 128)] {
        let pipeline = pipeline(
            &context,
            &layout,
            (
                &passing(VEC4, ""),
                Some(&crate::render::tests::counting(early)),
            ),
            &Shape {
                topology: Topology::Triangles,
                targets: &targets,
                depth_stencil: Some(depth(CompareOp::Less, true)),
                multisample: ONE_SAMPLE,
                rasterization: FILLED,
                restart: false,
            },
        )
        .expect("a pipeline");
        let held = crate::device_tests::memory(&context, 16);
        let counter = crate::device_tests::over(&context, &held, 0, 16);
        let set = context
            .create_bind_set(
                &set_layout,
                &[crate::BindSetEntry {
                    binding: 0,
                    resources: &[crate::Resource::Buffer {
                        buffer: &counter,
                        offset: 0,
                        size: 16,
                    }],
                }],
            )
            .expect("a set");
        // The near quad first, so the far one fails every test.
        let mut points = rectangle(extent, (0.0, 0.0, 8.0, 8.0), (0.25, [0.0, 1.0, 0.0, 1.0]));
        points.extend(rectangle(
            extent,
            (0.0, 0.0, 8.0, 8.0),
            (0.75, [1.0, 0.0, 0.0, 1.0]),
        ));
        let (_vertices, buffer) = floats(&context, &corners(&points));
        let colour = target(&context, Format::Rgba8Unorm, extent, 1);
        let depth_target = target(&context, Format::Depth32Float, extent, 1);
        let (viewport, scissor) = whole(extent);
        let mut commands = context.command_buffer();
        commands
            .render_pass(
                &Pass {
                    colour: &[ColourAttachment {
                        view: &colour.view,
                        resolve: None,
                        load: Load::Clear(ClearValue::Float([0.0; 4])),
                    }],
                    depth_stencil: Some(DepthStencilAttachment {
                        view: &depth_target.view,
                        depth_load: Load::Clear(ClearValue::DepthStencil {
                            depth: 1.0,
                            stencil: 0,
                        }),
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
                        0..12,
                        0..1,
                    )
                },
            )
            .expect("recorded");
        submit(&queue(&context), &commands).expect("drawn");
        let mut bytes = [0u8; 4];
        held.read(0, &mut bytes).expect("readable");
        assert_eq!(u32::from_le_bytes(bytes), shaded, "early {early}");
        for texel in read(&context, &colour).chunks(4) {
            assert_eq!(texel, [0, 255, 0, 255]);
        }
    }
}

#[test]
fn a_discarded_fragment_writes_nothing() {
    let device = rendering();
    let context = context(&device);
    let layout = bare(&context);
    let extent = (8, 8);
    let targets = [plain(Format::Rgba8Unorm)];
    let discarding = painting("").replace(
        "%c = OpLoad %v4 %in_colour\nOpStore %out_colour %c",
        "%c = OpLoad %v4 %in_colour\n%r = OpCompositeExtract %float %c 0\n%over = OpFOrdGreaterThan %bool %r %half\nOpSelectionMerge %go None\nOpBranchConditional %over %drop %go\n%drop = OpLabel\nOpKill\n%go = OpLabel\nOpStore %out_colour %c",
    )
    .replace(
        "%float = OpTypeFloat 32",
        "%float = OpTypeFloat 32\n%bool = OpTypeBool\n%half = OpConstant %float 0.5",
    );
    let pipeline = pipeline(
        &context,
        &layout,
        (&passing(VEC4, ""), Some(&discarding)),
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
    // The left half's red is past the threshold, the right half's below.
    let mut points = rectangle(extent, (0.0, 0.0, 4.0, 8.0), (0.5, [1.0, 0.0, 0.0, 1.0]));
    points.extend(rectangle(
        extent,
        (4.0, 0.0, 8.0, 8.0),
        (0.5, [0.25, 1.0, 0.0, 1.0]),
    ));
    let (_held, buffer) = floats(&context, &corners(&points));
    let colour = target(&context, Format::Rgba8Unorm, extent, 1);
    render(
        &context,
        (&[&colour], None),
        &[(&pipeline, &buffer, 0..12)],
        (0, [0.0; 4]),
    );
    let pixels = read(&context, &colour);
    for y in 0..8u32 {
        for x in 0..8u32 {
            let green = pixels[((y * 8 + x) * 4 + 1) as usize];
            assert_eq!(green, if x < 4 { 0 } else { 255 }, "pixel ({x}, {y})");
        }
    }
}

#[test]
fn quads_of_many_primitives_sharing_a_tile_blend_in_their_order() {
    let device = rendering();
    let context = context(&device);
    let extent = (8, 8);
    // Each halves what is held, by the blend constant, and adds its own: the
    // order of the adds is in the result.
    let halving = Blend {
        source_color: BlendFactor::One,
        destination_color: BlendFactor::ConstantColor,
        color: BlendOp::Add,
        source_alpha: BlendFactor::Zero,
        destination_alpha: BlendFactor::One,
        alpha: BlendOp::Add,
    };
    let targets = [ColourTarget {
        format: Format::R32Float,
        blend: Some(halving),
        write_mask: ColourMask::ALL,
    }];
    let layout = bare(&context);
    let pipeline = pipeline(
        &context,
        &layout,
        (&passing(VEC4, ""), Some(&float_painting(""))),
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
    // Twenty small triangles over the same pixel, each its own value.
    let mut points = Vec::new();
    for k in 0..20u32 {
        let value = real(k + 1);
        let ndc = |x: f32, y: f32| {
            (
                [x / 4.0 - 1.0, y / 4.0 - 1.0, 0.5, 1.0],
                [value, 0.0, 0.0, 0.5],
            )
        };
        points.extend([ndc(1.0, 1.0), ndc(3.0, 1.0), ndc(1.0, 3.0)]);
    }
    let (_held, buffer) = floats(&context, &corners(&points));
    let colour = target(&context, Format::R32Float, extent, 1);
    render(
        &context,
        (&[&colour], None),
        &[(&pipeline, &buffer, 0..60)],
        (0, [0.5; 4]),
    );
    let got = read_floats(&context, &colour);
    let want = (1..=20u32).fold(0.0f32, |held, k| held * 0.5 + real(k));
    assert_eq!(
        got[8 + 1].to_bits(),
        want.to_bits(),
        "pixel (1, 1) halved and added in order"
    );
}

#[test]
fn a_pass_of_many_bins_and_flushes_draws_whole() {
    let device = rendering();
    let context = context(&device);
    let extent = (200, 150);
    assert!(extent.0 > super::MAX_BIN_SIDE && extent.1 > super::MAX_BIN_SIDE / 2);
    let targets = [plain(Format::Rgba8Unorm), plain(Format::Rgba8Unorm)];
    let layout = bare(&context);
    let two_targets = painting("")
        .replace(
            "%out_colour = OpVariable %out_v4 Output",
            "%out_colour = OpVariable %out_v4 Output\n%second = OpVariable %out_v4 Output",
        )
        .replace(
            "OpStore %out_colour %c",
            "OpStore %out_colour %c\nOpStore %second %c",
        )
        .replace(
            "OpDecorate %out_colour Location 0",
            "OpDecorate %out_colour Location 0\nOpDecorate %second Location 1",
        )
        .replace("%in_colour %out_colour", "%in_colour %out_colour %second");
    let pipeline = pipeline(
        &context,
        &layout,
        (&passing(VEC4, ""), Some(&two_targets)),
        &Shape {
            topology: Topology::Triangles,
            targets: &targets,
            depth_stencil: Some(depth(CompareOp::LessOrEqual, true)),
            multisample: ONE_SAMPLE,
            rasterization: FILLED,
            restart: false,
        },
    )
    .expect("a pipeline");
    // Several flushes' primitives: thin quads tiling the whole target, three
    // bands of a column each.
    let mut points = Vec::new();
    for band in 0..3u32 {
        let (top, bottom) = (real(band) * 50.0, real(band + 1) * 50.0);
        for column in 0..200u32 {
            let x = real(column);
            points.extend(rectangle(
                extent,
                (x, top, x + 1.0, bottom),
                (0.5, [0.0, 1.0, 1.0, 1.0]),
            ));
        }
    }
    let flushes = (points.len() / 3)
        .div_ceil(super::front::FLUSH_BATCHES * super::front::BATCH_PRIMITIVES as usize);
    assert!(flushes > 1, "{flushes} flush");
    let (_held, buffer) = floats(&context, &corners(&points));
    let first = target(&context, Format::Rgba8Unorm, extent, 1);
    let second = target(&context, Format::Rgba8Unorm, extent, 1);
    let depth_target = target(&context, Format::Depth32Float, extent, 1);
    let count = u32::try_from(points.len()).expect("small");
    render(
        &context,
        (&[&first, &second], Some(&depth_target)),
        &[(&pipeline, &buffer, 0..count)],
        (0, [0.0; 4]),
    );
    for held in [&first, &second] {
        for (at, texel) in read(&context, held).chunks(4).enumerate() {
            assert_eq!(texel, [0, 255, 255, 255], "pixel {at}");
        }
    }
}

#[test]
fn a_culled_face_draws_nothing_and_a_flipped_viewport_turns_it() {
    let device = rendering();
    let context = context(&device);
    let extent = (8, 8);
    let targets = [plain(Format::Rgba8Unorm)];
    let culling = Rasterization {
        cull: CullMode::Back,
        ..FILLED
    };
    let pipeline = depth_pipeline(&context, &targets, (None, culling));
    // Clockwise on the screen, rows counted down: its back faces us under
    // the counter-clockwise front face.
    let (_held, buffer) = floats(
        &context,
        &corners(&rectangle(extent, (0.0, 0.0, 8.0, 8.0), (0.5, [1.0; 4]))),
    );
    for (flip, drawn) in [(false, false), (true, true)] {
        let colour = target(&context, Format::Rgba8Unorm, extent, 1);
        let (mut viewport, scissor) = whole(extent);
        if flip {
            viewport = Viewport {
                y: 8.0,
                height: -8.0,
                ..viewport
            };
        }
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
                            vertex_buffers: &[(&buffer, 0)],
                            viewport,
                            scissor: Rect {
                                width: 4,
                                ..scissor
                            },
                            stencil_reference: 0,
                            blend_constant: [0.0; 4],
                        },
                        0..6,
                        0..1,
                    )
                },
            )
            .expect("recorded");
        submit(&queue(&context), &commands).expect("drawn");
        let found = painted(&read(&context, &colour), extent, 4);
        let expected = if drawn {
            square_pixels((0, 0), (4, 8))
        } else {
            Vec::new()
        };
        assert_eq!(
            found, expected,
            "flipped {flip}: culled, and held to its scissor"
        );
    }
}

#[test]
fn a_draw_s_errors_name_it_among_its_pass_s_commands() {
    let device = rendering();
    let context = context(&device);
    let extent = (8, 8);
    let targets = [plain(Format::Rgba8Unorm)];
    let pipeline = plain_pipeline(&context, &targets, Topology::Triangles);
    let (_held, buffer) = floats(
        &context,
        &corners(&rectangle(extent, (0.0, 0.0, 8.0, 8.0), (0.5, [1.0; 4]))),
    );
    let wrong = target(&context, Format::R32Float, extent, 1);
    let (viewport, scissor) = whole(extent);
    let mut commands = context.command_buffer();
    commands.fill_buffer(&buffer, 0, 4, 0).expect("recorded");
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
    commands
        .render_pass(
            &Pass {
                colour: &[ColourAttachment {
                    view: &wrong.view,
                    resolve: None,
                    load: Load::Keep,
                }],
                depth_stencil: None,
                area: scissor,
            },
            |drawer| {
                drawer.draw(&draw, 0..6, 0..1)?;
                drawer.draw(&draw, 0..6, 0..1)
            },
        )
        .expect("recorded");
    assert_eq!(commands.len(), 4, "the fill, the pass and its two draws");
    let refused = submit(&queue(&context), &commands).expect_err("refused");
    assert_eq!(
        refused.command,
        Some(2),
        "the first draw, after the fill and the pass"
    );
    assert_eq!(refused.error, Error::Interface);
}

#[test]
fn a_pass_of_no_draws_clears_and_resolves() {
    let device = rendering();
    let context = context(&device);
    let extent = (8, 8);
    let samples = target(&context, Format::Rgba8Unorm, extent, 4);
    let resolved = target(&context, Format::Rgba8Unorm, extent, 1);
    let (_, scissor) = whole(extent);
    let mut commands = context.command_buffer();
    commands
        .render_pass(
            &Pass {
                colour: &[ColourAttachment {
                    view: &samples.view,
                    resolve: Some(&resolved.view),
                    load: Load::Clear(ClearValue::Float([0.0, 0.5, 1.0, 1.0])),
                }],
                depth_stencil: None,
                area: scissor,
            },
            |_| Ok(()),
        )
        .expect("recorded");
    submit(&queue(&context), &commands).expect("cleared");
    for (at, texel) in read(&context, &resolved).chunks(4).enumerate() {
        assert_eq!(texel, [0, 128, 255, 255], "pixel {at}");
    }
}

#[test]
fn a_target_no_output_writes_is_left_and_a_short_output_writes_zero_to_the_rest() {
    let device = rendering();
    let context = context(&device);
    let extent = (8, 8);
    let layout = bare(&context);
    let targets = [plain(Format::Rgba8Unorm), plain(Format::Rgba8Unorm)];
    let shape = Shape {
        topology: Topology::Triangles,
        targets: &targets,
        depth_stencil: None,
        multisample: ONE_SAMPLE,
        rasterization: FILLED,
        restart: false,
    };
    let whole_four = pipeline(
        &context,
        &layout,
        (&passing(VEC4, ""), Some(&painting(""))),
        &shape,
    )
    .expect("a pipeline writing four channels of the first target");
    let two_channels = painting("")
        .replace(
            "%v4 = OpTypeVector %float 4",
            "%v4 = OpTypeVector %float 4\n%v2 = OpTypeVector %float 2\n%out_v2 = OpTypePointer Output %v2",
        )
        .replace("%out_colour = OpVariable %out_v4 Output", "%out_colour = OpVariable %out_v2 Output")
        .replace("OpStore %out_colour %c", "%c2 = OpVectorShuffle %v2 %c %c 0 1\nOpStore %out_colour %c2");
    let short = pipeline(
        &context,
        &layout,
        (&passing(VEC4, ""), Some(&two_channels)),
        &shape,
    )
    .expect("a pipeline writing two channels of the first target");
    let (_white, white) = floats(
        &context,
        &corners(&rectangle(extent, (0.0, 0.0, 8.0, 8.0), (0.5, [1.0; 4]))),
    );
    let (_half, half) = floats(
        &context,
        &corners(&rectangle(
            extent,
            (0.0, 0.0, 8.0, 8.0),
            (0.5, [0.5, 0.25, 1.0, 1.0]),
        )),
    );
    let (first, second) = (
        target(&context, Format::Rgba8Unorm, extent, 1),
        target(&context, Format::Rgba8Unorm, extent, 1),
    );
    let (viewport, scissor) = whole(extent);
    let mut commands = context.command_buffer();
    commands
        .render_pass(
            &Pass {
                colour: &[
                    ColourAttachment {
                        view: &first.view,
                        resolve: None,
                        load: Load::Clear(ClearValue::Float([0.0; 4])),
                    },
                    ColourAttachment {
                        view: &second.view,
                        resolve: None,
                        load: Load::Clear(ClearValue::Float([0.25, 0.5, 0.75, 1.0])),
                    },
                ],
                depth_stencil: None,
                area: scissor,
            },
            |drawer| {
                for (pipeline, buffer) in [(&whole_four, &white), (&short, &half)] {
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
                        0..6,
                        0..1,
                    )?;
                }
                Ok(())
            },
        )
        .expect("recorded");
    submit(&queue(&context), &commands).expect("drawn");
    for (at, texel) in read(&context, &first).chunks(4).enumerate() {
        assert_eq!(texel, [128, 64, 0, 0], "the first target's pixel {at}");
    }
    for (at, texel) in read(&context, &second).chunks(4).enumerate() {
        assert_eq!(texel, [64, 128, 191, 255], "the second target's pixel {at}");
    }
}

/// A fragment shader writing the first of 4096 uniform buffers' colour.
const BINDLESS: &str = r#"OpCapability Shader
OpMemoryModel Logical GLSL450
OpEntryPoint Fragment %main "main" %in_colour %out_colour %ubos
OpExecutionMode %main OriginUpperLeft
OpDecorate %in_colour Location 0
OpDecorate %out_colour Location 0
OpDecorate %block Block
OpMemberDecorate %block 0 Offset 0
OpDecorate %ubos DescriptorSet 0
OpDecorate %ubos Binding 0
%void = OpTypeVoid
%fn = OpTypeFunction %void
%float = OpTypeFloat 32
%uint = OpTypeInt 32 0
%v4 = OpTypeVector %float 4
%in_v4 = OpTypePointer Input %v4
%out_v4 = OpTypePointer Output %v4
%block = OpTypeStruct %v4
%count = OpConstant %uint 4096
%blocks = OpTypeArray %block %count
%blocks_ptr = OpTypePointer Uniform %blocks
%v4_ptr = OpTypePointer Uniform %v4
%zero = OpConstant %uint 0
%in_colour = OpVariable %in_v4 Input
%out_colour = OpVariable %out_v4 Output
%ubos = OpVariable %blocks_ptr Uniform
%main = OpFunction %void None %fn
%entry = OpLabel
%at = OpAccessChain %v4_ptr %ubos %zero %zero
%c = OpLoad %v4 %at
OpStore %out_colour %c
OpReturn
OpFunctionEnd
"#;

/// A layout of `count` uniform buffers at binding 0 the fragment stage
/// reads, and a set of `buffer`'s first sixteen bytes in each.
fn uniforms(
    context: &Context,
    count: u32,
    buffer: &crate::Buffer,
) -> (crate::BindSet, crate::BindSetLayout, crate::PipelineLayout) {
    let set_layout = context
        .create_bind_set_layout(&[crate::LayoutBinding {
            binding: 0,
            kind: tairix_spirv::BindingKind::UniformBuffer,
            count,
            access: tairix_spirv::Access::READ,
            stages: tairix_spirv::Stages::FRAGMENT,
        }])
        .expect("a layout");
    let layout = context
        .create_pipeline_layout(&[&set_layout], crate::PushConstants::NONE)
        .expect("a layout");
    let resources: Vec<crate::Resource<'_>> = (0..count)
        .map(|_| crate::Resource::Buffer {
            buffer,
            offset: 0,
            size: 16,
        })
        .collect();
    let set = context
        .create_bind_set(
            &set_layout,
            &[crate::BindSetEntry {
                binding: 0,
                resources: &resources,
            }],
        )
        .expect("a set");
    (set, set_layout, layout)
}

#[test]
fn a_pass_of_many_draws_sharing_a_large_set_resolves_it_once() {
    let device = rendering();
    let context = context(&device);
    let extent = (8, 8);
    let (_colour_held, colour) = floats(&context, &[0.25, 0.5, 0.75, 1.0]);
    let (set, _set_layout, layout) = uniforms(&context, 4096, &colour);
    let targets = [plain(Format::Rgba8Unorm)];
    let pipeline = pipeline(
        &context,
        &layout,
        (&passing(VEC4, ""), Some(BINDLESS)),
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
    let (_held, buffer) = floats(
        &context,
        &corners(&rectangle(extent, (0.0, 0.0, 8.0, 8.0), (0.5, [0.0; 4]))),
    );
    let drawn = target(&context, Format::Rgba8Unorm, extent, 1);
    let (viewport, scissor) = whole(extent);
    let mut commands = context.command_buffer();
    commands
        .render_pass(
            &Pass {
                colour: &[ColourAttachment {
                    view: &drawn.view,
                    resolve: None,
                    load: Load::Clear(ClearValue::Float([0.0; 4])),
                }],
                depth_stencil: None,
                area: scissor,
            },
            |drawer| {
                for _ in 0..2000 {
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
                        0..6,
                        0..1,
                    )?;
                }
                Ok(())
            },
        )
        .expect("recorded");
    submit(&queue(&context), &commands).expect("charged and drawn");
    for (at, texel) in read(&context, &drawn).chunks(4).enumerate() {
        assert_eq!(texel, [64, 128, 191, 255], "pixel {at}");
    }
}

#[test]
fn a_pass_of_many_draws_binding_distinct_sets_gathers_their_memories_once() {
    use alloc::sync::Arc;
    use core::sync::atomic::Ordering;
    let clock = Arc::new(crate::device_tests::Clock::default());
    let device = crate::device_tests::software(32 << 20, Arc::clone(&clock) as Arc<_>);
    let context = context(&device);
    let extent = (8, 8);
    let targets = [plain(Format::Rgba8Unorm)];
    let (_held, buffer) = floats(
        &context,
        &corners(&rectangle(extent, (0.0, 0.0, 1.0, 1.0), (0.5, [0.0; 4]))),
    );
    let drawn = target(&context, Format::Rgba8Unorm, extent, 1);
    let (viewport, scissor) = whole(extent);
    // Each set over a memory of its own, so no two draws share a list.
    let held: Vec<_> = (0..3000).map(|_| floats(&context, &[1.0; 4])).collect();
    let sets: Vec<_> = held
        .iter()
        .map(|(_, ubo)| uniforms(&context, 1, ubo))
        .collect();
    let one = BINDLESS.replace("%uint 4096", "%uint 1");
    let pipeline = pipeline(
        &context,
        &sets[0].2,
        (&passing(VEC4, ""), Some(&one)),
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
    let readings = |distinct: bool| {
        let mut commands = context.command_buffer();
        commands
            .render_pass(
                &Pass {
                    colour: &[ColourAttachment {
                        view: &drawn.view,
                        resolve: None,
                        load: Load::Keep,
                    }],
                    depth_stencil: None,
                    area: scissor,
                },
                |drawer| {
                    for (at, _) in sets.iter().enumerate() {
                        let (set, _, _) = &sets[if distinct { at } else { 0 }];
                        drawer.draw(
                            &Draw {
                                pipeline: &pipeline,
                                sets: &[set],
                                push_constants: &[],
                                vertex_buffers: &[(&buffer, 0)],
                                viewport,
                                scissor,
                                stencil_reference: 0,
                                blend_constant: [0.0; 4],
                            },
                            0..6,
                            0..1,
                        )?;
                    }
                    Ok(())
                },
            )
            .expect("recorded");
        let before = clock.readings.load(Ordering::SeqCst);
        submit(&queue(&context), &commands).expect("drawn");
        clock.readings.load(Ordering::SeqCst) - before
    };
    let (shared, distinct) = (readings(false), readings(true));
    // Merging a list for each of 3000 sets would cost 3000 times 3000 words:
    // eight looks at the clock past the shared set's.
    assert!(distinct <= shared + 1, "the pass's memories gathered once");
}

#[test]
fn a_draw_of_no_primitive_in_any_instance_is_passed_over_whole() {
    use alloc::sync::Arc;
    use core::sync::atomic::Ordering;
    let clock = Arc::new(crate::device_tests::Clock::default());
    let device = crate::device_tests::software(32 << 20, Arc::clone(&clock) as Arc<_>);
    let context = context(&device);
    let layout = bare(&context);
    let targets = [plain(Format::Rgba8Unorm)];
    let (_held, buffer) = floats(&context, &corners(&[([0.0; 4], [0.0; 4]); 2]));
    let restarts = crate::device_tests::memory(&context, 16);
    restarts.write(0, &[0xff; 16]).expect("written");
    let indices = Indices {
        buffer: &crate::device_tests::over(&context, &restarts, 0, 16),
        offset: 0,
        format: IndexFormat::U16,
    };
    let drawn = target(&context, Format::Rgba8Unorm, (8, 8), 1);
    let (viewport, scissor) = whole((8, 8));
    // Two vertices of a list, and a restarting strip of nothing but restarts:
    // as empty in every instance as in the first.
    for (topology, restart) in [
        (Topology::Triangles, false),
        (Topology::TriangleStrip, true),
    ] {
        let pipeline = crate::render::tests::pipeline(
            &context,
            &layout,
            (&passing(VEC4, ""), Some(&painting(""))),
            &Shape {
                topology,
                targets: &targets,
                depth_stencil: None,
                multisample: ONE_SAMPLE,
                rasterization: FILLED,
                restart,
            },
        )
        .expect("a pipeline");
        let mut commands = context.command_buffer();
        commands
            .render_pass(
                &Pass {
                    colour: &[ColourAttachment {
                        view: &drawn.view,
                        resolve: None,
                        load: Load::Keep,
                    }],
                    depth_stencil: None,
                    area: scissor,
                },
                |drawer| {
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
                    if restart {
                        drawer.draw_indexed(&draw, &indices, (0..8, 0), 0..u32::MAX)
                    } else {
                        drawer.draw(&draw, 0..2, 0..u32::MAX)
                    }
                },
            )
            .expect("recorded");
        // Every look at the clock passes the deadline: instances counted one
        // by one would overrun it.
        clock.step.store(10 * 1_000_000_000, Ordering::SeqCst);
        submit(&queue(&context), &commands).expect("submitted");
        clock.step.store(0, Ordering::SeqCst);
        read(&context, &drawn);
    }
}

#[test]
fn a_pass_leaves_the_same_bytes_however_many_participants_draw_it() {
    use alloc::sync::Arc;
    // Interpreted, two bins of a few small triangles still run as pieces at
    // once.
    let (extent, triangles, reach) = if cfg!(miri) {
        ((132, 4), 8u32, (8, 4))
    } else {
        ((300, 140), 1100, (80, 40))
    };
    let halving = Blend {
        source_color: BlendFactor::One,
        destination_color: BlendFactor::ConstantColor,
        color: BlendOp::Add,
        source_alpha: BlendFactor::Zero,
        destination_alpha: BlendFactor::One,
        alpha: BlendOp::Add,
    };
    let targets = [ColourTarget {
        format: Format::R32Float,
        blend: Some(halving),
        write_mask: ColourMask::ALL,
    }];
    // Triangles of every size, at every depth, straddling the bins: the
    // order of the blends and the depth tests is in every pixel.
    let mut seed = 0x2545_f491_u32;
    let mut next = |most: u32| {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        real(seed % most)
    };
    let mut points = Vec::new();
    for k in 0..triangles {
        let (left, top, depth) = (next(extent.0), next(extent.1), next(100) / 100.0);
        let (wide, tall) = (next(reach.0) + 4.0, next(reach.1) + 4.0);
        let value = [real(k % 97 + 1), 0.0, 0.0, 1.0];
        let ndc = |px: f32, py: f32| {
            (
                [
                    px / real(extent.0) * 2.0 - 1.0,
                    py / real(extent.1) * 2.0 - 1.0,
                    depth,
                    1.0,
                ],
                value,
            )
        };
        points.extend([ndc(left, top), ndc(left + wide, top), ndc(left, top + tall)]);
    }
    let on = |runner: Arc<dyn tairix_parallel::JobRunner + Send>| {
        let clock: Arc<dyn tairix_abi::time::MonotonicClock + Send + Sync> =
            Arc::new(crate::device_tests::Clock::default());
        let device = crate::device_tests::software_on(32 << 20, clock, runner);
        let context = context(&device);
        let layout = bare(&context);
        let pipeline = pipeline(
            &context,
            &layout,
            (&passing(VEC4, ""), Some(&float_painting(""))),
            &Shape {
                topology: Topology::Triangles,
                targets: &targets,
                depth_stencil: Some(depth(CompareOp::LessOrEqual, true)),
                multisample: ONE_SAMPLE,
                rasterization: FILLED,
                restart: false,
            },
        )
        .expect("a pipeline");
        let (_held, buffer) = floats(&context, &corners(&points));
        let colour = target(&context, Format::R32Float, extent, 1);
        let depth_target = target(&context, Format::Depth32Float, extent, 1);
        render(
            &context,
            (&[&colour], Some(&depth_target)),
            &[(&pipeline, &buffer, 0..3 * triangles)],
            (0, [0.5; 4]),
        );
        read(&context, &colour)
    };
    let one = on(Arc::new(tairix_parallel::Serial));
    let reversed = Arc::new(tairix_parallel::Reversed::new(8));
    assert!(on(reversed.clone()) == one, "the pieces run last to first");
    assert!(reversed.widest() > 1, "the pass was split");
    assert!(
        on(Arc::new(tairix_parallel::Threaded::new(4))) == one,
        "the pieces run at once"
    );
}

#[test]
fn a_depth_fail_operation_is_written_though_a_fragment_stage_is_shaded() {
    let device = rendering();
    let context = context(&device);
    let extent = (8, 8);
    let targets = [plain(Format::Rgba8Unorm)];
    // Every sample fails its depth test and so takes its depth-fail operation.
    let failing = StencilFaceState {
        fail: StencilOp::Keep,
        pass: StencilOp::Keep,
        depth_fail: StencilOp::Replace,
        compare: CompareOp::Always,
        compare_mask: 0xff,
        write_mask: 0xff,
    };
    let test = StencilFaceState {
        depth_fail: StencilOp::Keep,
        compare: CompareOp::Equal,
        ..failing
    };
    let state = |face, depth_compare| DepthStencilState {
        format: Format::Depth32FloatStencil8,
        depth_compare,
        depth_write: false,
        stencil: Some([face, face]),
    };
    let marking = depth_pipeline(
        &context,
        &targets,
        (Some(state(failing, CompareOp::Never)), FILLED),
    );
    let tested = depth_pipeline(
        &context,
        &targets,
        (Some(state(test, CompareOp::Always)), FILLED),
    );
    let (_a, left) = floats(
        &context,
        &corners(&rectangle(extent, (0.0, 0.0, 4.0, 8.0), (0.5, [0.0; 4]))),
    );
    let (_b, all) = floats(
        &context,
        &corners(&rectangle(
            extent,
            (0.0, 0.0, 8.0, 8.0),
            (0.5, [1.0, 0.0, 0.0, 1.0]),
        )),
    );
    let colour = target(&context, Format::Rgba8Unorm, extent, 1);
    let depth_target = target(&context, Format::Depth32FloatStencil8, extent, 1);
    render(
        &context,
        (&[&colour], Some(&depth_target)),
        &[(&marking, &left, 0..6), (&tested, &all, 0..6)],
        (1, [0.0; 4]),
    );
    let pixels = read(&context, &colour);
    for (at, texel) in pixels.chunks(4).enumerate() {
        let want: &[u8] = if at % 8 < 4 {
            &[255, 0, 0, 255]
        } else {
            &[0, 0, 0, 0]
        };
        assert_eq!(texel, want, "pixel {at}");
    }
}

#[test]
fn a_draw_touches_no_pixel_outside_its_viewport() {
    let device = rendering();
    let context = context(&device);
    let extent = (8, 8);
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
    // The triangle every pass covers its viewport with, reaching well past it.
    let red = [1.0, 0.0, 0.0, 1.0];
    let (_held, buffer) = floats(
        &context,
        &corners(&[
            ([-1.0, -1.0, 0.5, 1.0], red),
            ([3.0, -1.0, 0.5, 1.0], red),
            ([-1.0, 3.0, 0.5, 1.0], red),
        ]),
    );
    let (_, scissor) = whole(extent);
    let quarter = Viewport {
        x: 0.0,
        y: 0.0,
        width: 4.0,
        height: 4.0,
        min_depth: 0.0,
        max_depth: 1.0,
    };
    let flipped = Viewport {
        y: 4.0,
        height: -4.0,
        ..quarter
    };
    for viewport in [quarter, flipped] {
        let drawn = target(&context, Format::Rgba8Unorm, extent, 1);
        let mut commands = context.command_buffer();
        commands
            .render_pass(
                &Pass {
                    colour: &[ColourAttachment {
                        view: &drawn.view,
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
        submit(&queue(&context), &commands).expect("drawn");
        for (at, texel) in read(&context, &drawn).chunks(4).enumerate() {
            let want: &[u8] = if at % 8 < 4 && at / 8 < 4 {
                &[255, 0, 0, 255]
            } else {
                &[0, 0, 0, 0]
            };
            assert_eq!(texel, want, "pixel {at} through {viewport:?}");
        }
    }
}

/// Four samples covered as `mask` says.
const fn four(mask: u32) -> Multisample {
    Multisample {
        samples: 4,
        mask,
        alpha_to_coverage: false,
    }
}

/// Draw each of `draws` — a pipeline, its vertices and how many — into the
/// four-sample `colour` and `depth`, cleared to nought and to one, and
/// resolve `colour` into `resolved`.
fn render_four(
    context: &Context,
    (colour, resolved, depth): (&Target, &Target, &Target),
    draws: &[(&RenderPipeline, &crate::Buffer, u32)],
) {
    let (viewport, scissor) = whole(colour.extent);
    let mut commands = context.command_buffer();
    commands
        .render_pass(
            &Pass {
                colour: &[ColourAttachment {
                    view: &colour.view,
                    resolve: Some(&resolved.view),
                    load: Load::Clear(ClearValue::Float([0.0; 4])),
                }],
                depth_stencil: Some(DepthStencilAttachment {
                    view: &depth.view,
                    depth_load: Load::Clear(ClearValue::DepthStencil {
                        depth: 1.0,
                        stencil: 0,
                    }),
                    stencil_load: Load::Keep,
                }),
                area: scissor,
            },
            |drawer| {
                for &(pipeline, buffer, count) in draws {
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
                    )?;
                }
                Ok(())
            },
        )
        .expect("recorded");
    submit(&queue(context), &commands).expect("drawn");
}

/// A pipeline of four samples covered as `mask` says into `targets`, its
/// fragment stage `fragment` where it has one, testing depth as `depth`.
fn four_sample_pipeline(
    context: &Context,
    (targets, mask): (&[ColourTarget], u32),
    fragment: Option<&str>,
    depth: DepthStencilState,
) -> RenderPipeline {
    let layout = bare(context);
    pipeline(
        context,
        &layout,
        (&passing(VEC4, ""), fragment),
        &Shape {
            topology: Topology::Triangles,
            targets,
            depth_stencil: Some(depth),
            multisample: four(mask),
            rasterization: FILLED,
            restart: false,
        },
    )
    .expect("a pipeline")
}

#[test]
fn a_sample_mask_holds_early_depth_writes_to_its_samples() {
    let device = rendering();
    let context = context(&device);
    let extent = (4, 4);
    let targets = [plain(Format::Rgba8Unorm)];
    // Depth written in the first sample alone, by a draw shading nothing.
    let marking = four_sample_pipeline(
        &context,
        (&targets, 0b0001),
        None,
        depth(CompareOp::Always, true),
    );
    let painted = four_sample_pipeline(
        &context,
        (&targets, u32::MAX),
        Some(&painting("")),
        depth(CompareOp::Less, false),
    );
    let quad = |z: f32| corners(&rectangle(extent, (0.0, 0.0, 4.0, 4.0), (z, [1.0; 4])));
    let (_near, near) = floats(&context, &quad(0.0));
    let (_far, far) = floats(&context, &quad(0.5));
    let colour = target(&context, Format::Rgba8Unorm, extent, 4);
    let resolved = target(&context, Format::Rgba8Unorm, extent, 1);
    let depth_target = target(&context, Format::Depth32Float, extent, 4);
    render_four(
        &context,
        (&colour, &resolved, &depth_target),
        &[(&marking, &near, 6), (&painted, &far, 6)],
    );
    for (at, texel) in read(&context, &resolved).chunks(4).enumerate() {
        assert_eq!(texel, [191, 191, 191, 191], "three samples of pixel {at}");
    }
}

/// A fragment shader whose fragments are tested early, writing the samples
/// its `SampleMask` input names, counted, as a float.
const SAMPLES_IN: &str = r#"OpCapability Shader
OpMemoryModel Logical GLSL450
OpEntryPoint Fragment %main "main" %mask_in %out_value
OpExecutionMode %main OriginUpperLeft
OpExecutionMode %main EarlyFragmentTests
OpDecorate %mask_in BuiltIn SampleMask
OpDecorate %out_value Location 0
%void = OpTypeVoid
%fn = OpTypeFunction %void
%float = OpTypeFloat 32
%uint = OpTypeInt 32 0
%one = OpConstant %uint 1
%zero = OpConstant %uint 0
%masks = OpTypeArray %uint %one
%in_masks = OpTypePointer Input %masks
%in_uint = OpTypePointer Input %uint
%out_float = OpTypePointer Output %float
%mask_in = OpVariable %in_masks Input
%out_value = OpVariable %out_float Output
%main = OpFunction %void None %fn
%entry = OpLabel
%at = OpAccessChain %in_uint %mask_in %zero
%m = OpLoad %uint %at
%n = OpBitCount %uint %m
%f = OpConvertUToF %float %n
OpStore %out_value %f
OpReturn
OpFunctionEnd
"#;

#[test]
fn a_fragments_sample_mask_and_centroid_are_its_coverage_before_its_tests() {
    let device = rendering();
    let context = context(&device);
    let extent = (2, 2);
    let targets = [plain(Format::R32Float)];
    let early = |decorations: &str| {
        float_painting(decorations).replace(
            "OpExecutionMode %main OriginUpperLeft",
            "OpExecutionMode %main OriginUpperLeft\nOpExecutionMode %main EarlyFragmentTests",
        )
    };
    let centroid = early("OpDecorate %in_value Centroid\nOpDecorate %in_value NoPerspective");
    let tested = |fragment: &str| {
        four_sample_pipeline(
            &context,
            (&targets, u32::MAX),
            Some(fragment),
            depth(CompareOp::Less, false),
        )
    };
    let (_near, near) = floats(
        &context,
        &corners(&rectangle(extent, (0.0, 0.0, 2.0, 2.0), (0.0, [0.0; 4]))),
    );
    // One triangle over every sample, so each fragment covers all four.
    let (_far, far) = floats(
        &context,
        &corners(&[
            ([-1.0, -1.0, 0.5, 1.0], [0.0; 4]),
            ([3.0, -1.0, 0.5, 1.0], [0.0; 4]),
            ([-1.0, 3.0, 0.5, 1.0], [0.0; 4]),
        ]),
    );
    let marking = |mask| {
        four_sample_pipeline(
            &context,
            (&targets, mask),
            None,
            depth(CompareOp::Always, true),
        )
    };
    let read_first = |marked: &RenderPipeline, drawn: (&RenderPipeline, &crate::Buffer, u32)| {
        let colour = target(&context, Format::R32Float, extent, 4);
        let resolved = target(&context, Format::R32Float, extent, 1);
        let depth_target = target(&context, Format::Depth32Float, extent, 4);
        render_four(
            &context,
            (&colour, &resolved, &depth_target),
            &[(marked, &near, 6), drawn],
        );
        read_floats(&context, &resolved)[0]
    };
    // The first two samples' depth near, so their early test fails: the
    // input still names all four, written in the last two.
    let counting = tested(SAMPLES_IN);
    let counted = read_first(&marking(0b0011), (&counting, &far, 3));
    assert_eq!(
        counted.to_bits(),
        2.0f32.to_bits(),
        "four samples counted in two"
    );
    // A primitive right of x = 0.6 covers samples 1 and 3 of pixel (0, 0) and
    // not its centre; sample 1's early test fails. Its centroid is still
    // sample 1, where its value, its x, is 0.875, written in sample 3 alone.
    let ndc = |x: f32, y: f32, value: f32| ([x - 1.0, y - 1.0, 0.5, 1.0], [value, 0.0, 0.0, 1.0]);
    let (_edge, edge) = floats(
        &context,
        &corners(&[
            ndc(0.6, -10.0, 0.6),
            ndc(100.0, -10.0, 100.0),
            ndc(0.6, 100.0, 0.6),
        ]),
    );
    let interpolating = tested(&centroid);
    let got = read_first(&marking(0b0010), (&interpolating, &edge, 3));
    assert!(
        (got - 0.875 / 4.0).abs() < 0.005,
        "interpolated at sample 1, not 3: {got}"
    );
}
