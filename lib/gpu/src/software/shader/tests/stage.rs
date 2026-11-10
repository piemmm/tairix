//! The vertex and fragment stages' own instructions and values, each run
//! through the one stage run a draw makes: a vertex batch's built-ins and
//! attributes; a fragment tile's derivatives across its quads' rows and
//! columns, its discards, the level of detail its quads choose, and its
//! helper lanes, which lend derivatives their values but write no memory
//! and take no part in a subgroup operation.

use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use tairix_abi::time::MonotonicClock;
use tairix_spirv::assemble::admit;
use tairix_spirv::Stage;

use super::f;
use crate::image::{Extent3d, ImageDesc, ImageDimension, ImageRange, ImageUsage, ImageViewDesc};
use crate::sampler::{AddressMode, BorderColor, Filter, SamplerDesc};
use crate::software::dispatch::{carve, Parts};
use crate::software::layout::ImageLayout;
use crate::software::shader::exec::{shade, Env, ImageAt, Lanes, Placement, Resolved};
use crate::software::shader::ir::{Carries, Fixed, Program, LANES};
use crate::software::shader::lower::lower;
use crate::software::shader::ops::low32;
use crate::software::texel::Srgb;
use crate::software::Meter;
use crate::{Aspects, CompareOp, Format, ViewDimension};

struct Never;

impl MonotonicClock for Never {
    fn now_ns(&self) -> u64 {
        0
    }
}

/// The subgroup width these tiles run at: four quads.
const WIDTH: u32 = 16;

/// The entry point `main` of `stage` in `text`, lowered.
fn lowered(text: &str, stage: Stage) -> Program {
    let admitted = admit(text).unwrap_or_else(|refusal| panic!("{refusal:?} in\n{text}"));
    let entry = admitted
        .entry_point("main", stage)
        .expect("the entry point");
    let specialized = admitted.specialize(&[]).expect("no constants to set");
    lower(&admitted, entry, &specialized, [1, 1, 1]).expect("lowered")
}

/// What a run left: each output port's words, lane by lane, and the lanes a
/// discard ended.
struct Shaded {
    outputs: Vec<(Carries, Vec<Lanes>)>,
    killed: u64,
}

impl Shaded {
    /// Word `word` of the output carrying `carries`.
    fn word(&self, carries: Carries, word: usize) -> &Lanes {
        let (_, words) = self
            .outputs
            .iter()
            .find(|(c, _)| *c == carries)
            .expect("the output");
        &words[word]
    }
}

/// `program` run over `place`, its inputs fed by `feed`, its bindings
/// `bindings`.
fn run(
    program: &Program,
    place: &Placement,
    bindings: &[&[Resolved<'_>]],
    feed: impl Fn(Carries, u32, &mut Lanes),
) -> Shaded {
    let parts = Parts::of(program);
    let mut words = vec![0u32; parts.words()];
    let mut masks = vec![0u64; parts.masks_len()];
    let space = carve(&mut words, &mut masks, parts, &mut []).expect("the tile's space");
    let srgb = Srgb::new();
    let env = Env {
        program,
        bindings,
        push: &[],
        srgb: &srgb,
        subgroup_size: WIDTH,
        groups: [1, 1, 1],
    };
    let stop = AtomicBool::new(false);
    let mut meter = Meter {
        stop: &stop,
        clock: &Never,
        deadline: u64::MAX,
        unchecked: 0,
        read: false,
    };
    let mut outputs: Vec<(Carries, Vec<Lanes>)> = program
        .outputs
        .iter()
        .map(|port| (port.carries, vec![[0; LANES]; port.words as usize]))
        .collect();
    let killed = shade(
        (&env, place),
        space,
        &mut meter,
        |port, word, values| feed(program.inputs[port].carries, word, values),
        |port, word, values| outputs[port].1[word as usize] = *values,
    )
    .expect("the run ends");
    Shaded { outputs, killed }
}

/// The pixel of a tile's lane: its quad's place in the tile's four by four,
/// then its own in the quad's two by two.
fn pixel(lane: usize) -> (u32, u32) {
    let quad = u32::try_from(lane / 4).expect("a tile's lane");
    let within = u32::try_from(lane % 4).expect("a quad's lane");
    (
        2 * (quad % 4) + (within & 1),
        2 * (quad / 4) + (within >> 1),
    )
}

/// `n`, a small count, as a single: exactly.
fn real(n: usize) -> f32 {
    f32::from(u16::try_from(n).expect("small"))
}

/// The single a lane's word holds, as its bits.
fn word(value: u64) -> u32 {
    low32(value)
}

/// Each lane's value of `f` at its pixel, as a single's bits.
fn per_pixel(f: impl Fn(f32, f32) -> f32, out: &mut Lanes) {
    for (lane, value) in out.iter_mut().enumerate() {
        let (x, y) = pixel(lane);
        *value = u64::from(f(real(x as usize), real(y as usize)).to_bits());
    }
}

/// A fragment shader `main` over `interface`, with `decorations`, the
/// shared types, `declarations`, and the entry block's `body`.
fn fragment(interface: &str, decorations: &str, declarations: &str, body: &str) -> String {
    format!(
        r#"OpCapability Shader
OpCapability ImageQuery
OpCapability GroupNonUniform
OpCapability GroupNonUniformArithmetic
%glsl = OpExtInstImport "GLSL.std.450"
OpMemoryModel Logical GLSL450
OpEntryPoint Fragment %main "main" {interface}
OpExecutionMode %main OriginUpperLeft
{decorations}
%void = OpTypeVoid
%fn = OpTypeFunction %void
%bool = OpTypeBool
%int = OpTypeInt 32 1
%uint = OpTypeInt 32 0
%float = OpTypeFloat 32
%v2float = OpTypeVector %float 2
%v3float = OpTypeVector %float 3
%v4float = OpTypeVector %float 4
%in_float = OpTypePointer Input %float
%in_v2 = OpTypePointer Input %v2float
%in_v3 = OpTypePointer Input %v3float
%in_uint = OpTypePointer Input %uint
%in_bool = OpTypePointer Input %bool
%out_float = OpTypePointer Output %float
%out_uint = OpTypePointer Output %uint
%out_v2 = OpTypePointer Output %v2float
%fzero = OpConstant %float 0
%uzero = OpConstant %uint 0
%uone = OpConstant %uint 1
%subgroup = OpConstant %uint 3
{declarations}
%main = OpFunction %void None %fn
%entry = OpLabel
{body}
OpReturn
OpFunctionEnd
"#
    )
}

/// A derivative of `v` written to `r`, both at location 0.
fn derivative(op: &str) -> String {
    fragment(
        "%v %r",
        "OpDecorate %v Location 0\nOpDecorate %r Location 0",
        "%v = OpVariable %in_float Input\n%r = OpVariable %out_float Output",
        &format!("%x = OpLoad %float %v\n%d = {op} %float %x\nOpStore %r %d"),
    )
}

const V: Carries = Carries::Located {
    location: 0,
    component: 0,
    index: 0,
};

/// The bodies the cases run, for the coverage test.
pub(crate) fn bodies() -> Vec<String> {
    let mut bodies: Vec<String> = ["OpDPdx", "OpDPdy", "OpFwidth"]
        .iter()
        .map(|op| derivative(op))
        .collect();
    bodies.push(discarding("OpKill"));
    bodies.push(discarding("OpTerminateInvocation"));
    for form in SAMPLES {
        bodies.push(sampling(form));
    }
    bodies
}

#[test]
fn a_derivative_is_taken_across_its_quads_row_or_column() {
    // A value of x y + x varies along x by y + 1 and along y by x, so each
    // quad row and column has a derivative of its own.
    let field = |x: f32, y: f32| x * y + x;
    let place = Placement::quads(u64::MAX, 0, WIDTH);
    let feed = |carries: Carries, _: u32, values: &mut Lanes| {
        if carries == V {
            per_pixel(field, values);
        }
    };
    let along_x: fn(u32, u32) -> f32 = |_, y| real(y as usize + 1);
    let along_y: fn(u32, u32) -> f32 = |x, _| real(x as usize);
    let width: fn(u32, u32) -> f32 = |x, y| real((x + y + 1) as usize);
    for (op, expected) in [
        ("OpDPdx", along_x),
        ("OpDPdy", along_y),
        ("OpFwidth", width),
    ] {
        let shaded = run(
            &lowered(&derivative(op), Stage::Fragment),
            &place,
            &[],
            feed,
        );
        for (lane, &written) in shaded.word(V, 0).iter().enumerate() {
            let (x, y) = pixel(lane);
            assert_eq!(
                word(written),
                f(expected(x, y)),
                "{op} at lane {lane}, pixel ({x}, {y})"
            );
        }
        assert_eq!(shaded.killed, 0);
    }
}

#[test]
fn a_helper_lends_its_value_to_its_quads_derivative() {
    // Lanes 1 and 2 of the first quad are helpers: their values still make
    // its derivatives, whose lanes alone the stage keeps.
    let field = |x: f32, y: f32| 3.0 * x + 5.0 * y;
    let place = Placement::quads(0b1001, 0b0110, WIDTH);
    let shaded = run(
        &lowered(&derivative("OpFwidth"), Stage::Fragment),
        &place,
        &[],
        |carries, _, values| {
            if carries == V {
                per_pixel(field, values);
            }
        },
    );
    for lane in [0, 3] {
        assert_eq!(word(shaded.word(V, 0)[lane]), f(8.0));
    }
}

#[test]
fn a_derivative_across_a_discarded_neighbour_reads_the_lanes_own_value() {
    // The second lane of the first quad discards before the others take the
    // derivative along x, so its row's lane reads its own value twice.
    let text = fragment(
        "%v %r",
        "OpDecorate %v Location 0\nOpDecorate %r Location 0",
        "%v = OpVariable %in_float Input\n%r = OpVariable %out_float Output\n%one = OpConstant %float 1.0",
        "%x = OpLoad %float %v\n%gone = OpFOrdEqual %bool %x %one\nOpSelectionMerge %go None\nOpBranchConditional %gone %kill %go\n%kill = OpLabel\nOpKill\n%go = OpLabel\n%d = OpDPdx %float %x\nOpStore %r %d",
    );
    let shaded = run(
        &lowered(&text, Stage::Fragment),
        &Placement::quads(0b1111, 0, WIDTH),
        &[],
        |carries, _, values| {
            if carries == V {
                for (value, x) in values.iter_mut().zip([10.0f32, 1.0, 20.0, 24.0]) {
                    *value = u64::from(f(x));
                }
            }
        },
    );
    assert_eq!(shaded.killed, 0b0010);
    let written = shaded.word(V, 0);
    assert_eq!(word(written[0]), f(0.0), "its neighbour is gone");
    assert_eq!(word(written[2]), f(4.0), "its row is whole");
    assert_eq!(word(written[3]), f(4.0), "its row is whole");
}

/// A shader that discards with `op` where `v` is negative, and writes it to
/// `r` otherwise.
fn discarding(op: &str) -> String {
    fragment(
        "%v %r",
        "OpDecorate %v Location 0\nOpDecorate %r Location 0",
        "%v = OpVariable %in_float Input\n%r = OpVariable %out_float Output",
        &format!(
            "%x = OpLoad %float %v\n%below = OpFOrdLessThan %bool %x %fzero\nOpSelectionMerge %go None\nOpBranchConditional %below %drop %go\n%drop = OpLabel\n{op}\n%go = OpLabel\nOpStore %r %x"
        ),
    )
}

#[test]
fn a_discarded_lane_is_reported_and_the_rest_write_their_outputs() {
    for op in ["OpKill", "OpTerminateInvocation"] {
        let shaded = run(
            &lowered(&discarding(op), Stage::Fragment),
            &Placement::quads(u64::MAX, 0, WIDTH),
            &[],
            |carries, _, values| {
                if carries == V {
                    for (lane, value) in values.iter_mut().enumerate() {
                        let x = if lane % 3 == 0 { -1.0f32 } else { real(lane) };
                        *value = u64::from(x.to_bits());
                    }
                }
            },
        );
        let expected = (0..LANES)
            .filter(|lane| lane % 3 == 0)
            .fold(0u64, |mask, lane| mask | 1 << lane);
        assert_eq!(shaded.killed, expected, "{op}");
        for lane in (0..LANES).filter(|lane| lane % 3 != 0) {
            assert_eq!(word(shaded.word(V, 0)[lane]), f(real(lane)));
        }
    }
}

/// An 8-by-8 image of four levels, each level's texels holding `value` of
/// the level.
struct Levels {
    desc: ImageDesc,
    layout: ImageLayout,
    words: Vec<AtomicU32>,
    view: ImageViewDesc,
}

impl Levels {
    fn new(format: Format, aspects: Aspects, value: impl Fn(u32) -> f32) -> Self {
        let desc = ImageDesc {
            dimension: ImageDimension::D2,
            format,
            extent: Extent3d {
                width: 8,
                height: 8,
                depth: 1,
            },
            mip_levels: 4,
            array_layers: 1,
            samples: 1,
            usage: ImageUsage::SAMPLED,
        };
        let layout = ImageLayout::of(&desc).expect("a layout");
        let words: Vec<AtomicU32> = (0..layout.size.div_ceil(4))
            .map(|_| AtomicU32::new(0))
            .collect();
        for level in 0..4 {
            let side = 8 >> level;
            for y in 0..side {
                for x in 0..side {
                    let byte = layout.texel_offset(level, 0, [x, y, 0], 0);
                    words[usize::try_from(byte / 4).expect("small")]
                        .store(value(level).to_bits(), Ordering::Relaxed);
                }
            }
        }
        let view = ImageViewDesc {
            format,
            dimension: ViewDimension::D2,
            range: ImageRange {
                aspects,
                base_level: 0,
                levels: 4,
                base_layer: 0,
                layers: 1,
            },
        };
        Self {
            desc,
            layout,
            words,
            view,
        }
    }

    fn at(&self) -> ImageAt<'_> {
        ImageAt {
            words: &self.words,
            base: 0,
            layout: &self.layout,
            desc: &self.desc,
            view: &self.view,
        }
    }
}

fn nearest(compare: Option<CompareOp>) -> SamplerDesc {
    SamplerDesc {
        mag_filter: Filter::Nearest,
        min_filter: Filter::Nearest,
        mipmap_filter: Filter::Nearest,
        address: [AddressMode::ClampToEdge; 3],
        lod_bias: 0.0,
        lod_min: 0.0,
        lod_max: 16.0,
        max_anisotropy: 1,
        compare,
        border: BorderColor::TransparentBlack,
        unnormalized: false,
    }
}

/// How each implicit-level form is spelt, whether it compares, and whether
/// its coordinate is projective.
const SAMPLES: [(&str, bool, bool); 5] = [
    ("OpImageSampleImplicitLod", false, false),
    ("OpImageSampleDrefImplicitLod", true, false),
    ("OpImageSampleProjImplicitLod", false, true),
    ("OpImageSampleProjDrefImplicitLod", true, true),
    ("OpImageQueryLod", false, false),
];

/// A shader sampling binding 0 at the coordinate `uv` in `form`, its
/// first component written to `r`; a level-of-detail query writes both.
fn sampling((op, dref, projective): (&str, bool, bool)) -> String {
    let depth = u8::from(dref);
    let (coordinate, load) = if projective {
        ("%in_v3", "%c = OpLoad %v3float %uv")
    } else {
        ("%in_v2", "%c = OpLoad %v2float %uv")
    };
    let call = match (op, dref) {
        ("OpImageQueryLod", _) => {
            String::from("%q = OpImageQueryLod %v2float %s %c\nOpStore %r %q")
        }
        (_, true) => format!("%t = {op} %float %s %c %reference\nOpStore %r %t"),
        _ => format!(
            "%t = {op} %v4float %s %c\n%t0 = OpCompositeExtract %float %t 0\nOpStore %r %t0"
        ),
    };
    let output = if op == "OpImageQueryLod" {
        "%out_v2"
    } else {
        "%out_float"
    };
    fragment(
        "%uv %tex %r",
        "OpDecorate %uv Location 0\nOpDecorate %r Location 0\nOpDecorate %tex DescriptorSet 0\nOpDecorate %tex Binding 0",
        &format!(
            "%uv = OpVariable {coordinate} Input\n%r = OpVariable {output} Output\n%image_t = OpTypeImage %float 2D {depth} 0 0 1 Unknown\n%si_t = OpTypeSampledImage %image_t\n%si_ptr = OpTypePointer UniformConstant %si_t\n%tex = OpVariable %si_ptr UniformConstant\n%reference = OpConstant %float 0.4"
        ),
        &format!("{load}\n%s = OpLoad %si_t %tex\n{call}"),
    )
}

#[test]
fn an_implicit_level_is_the_one_its_quads_derivatives_choose() {
    // Each pixel steps `scale` texels of the first level along each axis, so
    // the level of detail is the scale's logarithm. Each colour level holds
    // its own number; each depth level holds a quarter of it, which the
    // reference 0.4 is no further than from level two on.
    let colour = Levels::new(Format::R32Float, Aspects::COLOR, |level| {
        real(level as usize)
    });
    let depth = Levels::new(Format::Depth32Float, Aspects::DEPTH, |level| {
        real(level as usize) / 4.0
    });
    let plain = nearest(None);
    let compared = nearest(Some(CompareOp::LessOrEqual));
    for form @ (op, dref, projective) in SAMPLES {
        for (scale, level) in [(1.0f32, 0u32), (2.0, 1), (4.0, 2), (8.0, 3)] {
            let image = if dref { depth.at() } else { colour.at() };
            let sampler = if dref { &compared } else { &plain };
            let row = [Resolved::ImageSampler(image, sampler)];
            let shaded = run(
                &lowered(&sampling(form), Stage::Fragment),
                &Placement::quads(u64::MAX, 0, WIDTH),
                &[&row],
                |carries, word, values| {
                    if carries != V {
                        return;
                    }
                    let q = if projective { 2.0 } else { 1.0 };
                    match word {
                        0 => per_pixel(|x, _| (x + 0.5) * scale / 8.0 * q, values),
                        1 => per_pixel(|_, y| (y + 0.5) * scale / 8.0 * q, values),
                        _ => values.fill(u64::from(q.to_bits())),
                    }
                },
            );
            let written = shaded.word(V, 0);
            // A projective reference is divided by the coordinate's last
            // component as the point is.
            let deep_enough = if projective { level >= 1 } else { level >= 2 };
            let expected = if dref {
                f32::from(u8::from(deep_enough))
            } else {
                real(level as usize)
            };
            for (lane, &held) in written.iter().enumerate() {
                assert_eq!(
                    word(held),
                    f(expected),
                    "{op} at scale {scale}, lane {lane}"
                );
            }
            if op == "OpImageQueryLod" {
                for &held in shaded.word(V, 1) {
                    assert_eq!(word(held), f(real(level as usize)), "{op}'s level");
                }
            }
        }
    }
}

#[test]
fn a_helper_writes_no_buffer_makes_no_atomic_and_reads_zero_from_a_subgroup() {
    let text = fragment(
        "%lane %helper %buf %r %count",
        "OpDecorate %lane Location 1\nOpDecorate %lane Flat\nOpDecorate %helper BuiltIn HelperInvocation\nOpDecorate %r Location 0\nOpDecorate %count Location 1\nOpDecorate %block Block\nOpMemberDecorate %block 0 Offset 0\nOpMemberDecorate %block 1 Offset 4\nOpDecorate %words ArrayStride 4\nOpDecorate %buf DescriptorSet 0\nOpDecorate %buf Binding 0",
        "%lane = OpVariable %in_uint Input\n%helper = OpVariable %in_bool Input\n%r = OpVariable %out_uint Output\n%count = OpVariable %out_uint Output\n%c64 = OpConstant %uint 64\n%words = OpTypeArray %uint %c64\n%block = OpTypeStruct %uint %words\n%block_ptr = OpTypePointer StorageBuffer %block\n%uint_ptr = OpTypePointer StorageBuffer %uint\n%buf = OpVariable %block_ptr StorageBuffer\n%iuone = OpConstant %int 1\n%izero = OpConstant %int 0",
        "%l = OpLoad %uint %lane\n%h = OpLoad %bool %helper\n%slot = OpAccessChain %uint_ptr %buf %iuone %l\nOpStore %slot %uone\n%counter = OpAccessChain %uint_ptr %buf %izero\n%old = OpAtomicIAdd %uint %counter %uone %uzero %uone\n%sum = OpGroupNonUniformIAdd %uint %subgroup Reduce %uone\n%flag = OpSelect %uint %h %uone %uzero\nOpStore %r %flag\nOpStore %count %sum",
    );
    let program = lowered(&text, Stage::Fragment);
    // Lanes 1 and 2 of every quad are helpers.
    let helpers = (0..16).fold(0u64, |mask, quad| mask | 0b0110 << (4 * quad));
    let live = !helpers;
    let memory: Vec<AtomicU32> = (0..65).map(|_| AtomicU32::new(0)).collect();
    let row = [Resolved::Buffer {
        words: &memory,
        offset: 0,
        size: 65 * 4,
    }];
    let shaded = run(
        &program,
        &Placement::quads(live, helpers, WIDTH),
        &[&row],
        |carries, _, values| {
            if carries
                == (Carries::Located {
                    location: 1,
                    component: 0,
                    index: 0,
                })
            {
                for (lane, value) in values.iter_mut().enumerate() {
                    *value = lane as u64;
                }
            }
        },
    );
    let count = Carries::Located {
        location: 1,
        component: 0,
        index: 0,
    };
    assert_eq!(
        memory[0].load(Ordering::Relaxed),
        32,
        "one add per covered lane"
    );
    for lane in 0..LANES {
        let helper = helpers >> lane & 1 != 0;
        assert_eq!(
            memory[1 + lane].load(Ordering::Relaxed),
            u32::from(!helper),
            "lane {lane}'s store"
        );
        assert_eq!(
            shaded.word(V, 0)[lane],
            u64::from(helper),
            "lane {lane}'s flag"
        );
        // A subgroup of sixteen lanes holds four quads, eight lanes covered.
        let expected = if helper { 0 } else { 8 };
        assert_eq!(shaded.word(count, 0)[lane], expected, "lane {lane}'s sum");
    }
}

#[test]
fn a_vertex_batch_is_given_its_indices_and_attributes_and_gives_its_position() {
    let text = r#"OpCapability Shader
OpMemoryModel Logical GLSL450
OpEntryPoint Vertex %main "main" %vertex %instance %at %pv %tag
OpDecorate %vertex BuiltIn VertexIndex
OpDecorate %instance BuiltIn InstanceIndex
OpDecorate %at Location 0
OpMemberDecorate %per_vertex 0 BuiltIn Position
OpMemberDecorate %per_vertex 1 BuiltIn PointSize
OpDecorate %per_vertex Block
OpDecorate %tag Location 2
%void = OpTypeVoid
%fn = OpTypeFunction %void
%int = OpTypeInt 32 1
%float = OpTypeFloat 32
%v2float = OpTypeVector %float 2
%v4float = OpTypeVector %float 4
%per_vertex = OpTypeStruct %v4float %float
%in_int = OpTypePointer Input %int
%in_v2 = OpTypePointer Input %v2float
%out_pv = OpTypePointer Output %per_vertex
%out_v4 = OpTypePointer Output %v4float
%out_float = OpTypePointer Output %float
%out_int = OpTypePointer Output %int
%vertex = OpVariable %in_int Input
%instance = OpVariable %in_int Input
%at = OpVariable %in_v2 Input
%pv = OpVariable %out_pv Output
%tag = OpVariable %out_int Output
%izero = OpConstant %int 0
%ione = OpConstant %int 1
%hundred = OpConstant %int 100
%fzero = OpConstant %float 0
%fone = OpConstant %float 1.0
%main = OpFunction %void None %fn
%entry = OpLabel
%v = OpLoad %int %vertex
%i = OpLoad %int %instance
%xy = OpLoad %v2float %at
%x = OpCompositeExtract %float %xy 0
%y = OpCompositeExtract %float %xy 1
%p = OpCompositeConstruct %v4float %x %y %fzero %fone
%position = OpAccessChain %out_v4 %pv %izero
OpStore %position %p
%size = OpAccessChain %out_float %pv %ione
OpStore %size %fone
%scaled = OpIMul %int %i %hundred
%t = OpIAdd %int %scaled %v
OpStore %tag %t
OpReturn
OpFunctionEnd
"#;
    let program = lowered(text, Stage::Vertex);
    let shaded = run(
        &program,
        &Placement::batch(40, WIDTH),
        &[],
        |carries, word, values| match carries {
            Carries::Fixed(Fixed::VertexIndex) => {
                for (lane, value) in values.iter_mut().enumerate() {
                    *value = lane as u64 + 7;
                }
            }
            Carries::Fixed(Fixed::InstanceIndex) => values.fill(3),
            Carries::Located { location: 0, .. } => {
                for (lane, value) in values.iter_mut().enumerate() {
                    *value = u64::from(f(real(lane) * 2.0 + real(word as usize)));
                }
            }
            _ => {}
        },
    );
    let tag = Carries::Located {
        location: 2,
        component: 0,
        index: 0,
    };
    let position = Carries::Fixed(Fixed::Position);
    for lane in 0..40 {
        assert_eq!(shaded.word(tag, 0)[lane], 300 + lane as u64 + 7);
        assert_eq!(word(shaded.word(position, 0)[lane]), f(real(lane) * 2.0));
        assert_eq!(
            word(shaded.word(position, 1)[lane]),
            f(real(lane) * 2.0 + 1.0)
        );
        assert_eq!(word(shaded.word(position, 3)[lane]), f(1.0));
        assert_eq!(
            word(shaded.word(Carries::Fixed(Fixed::PointSize), 0)[lane]),
            f(1.0)
        );
    }
    for lane in 40..LANES {
        assert_eq!(shaded.word(tag, 0)[lane], 0, "lane {lane} holds no vertex");
    }
}
