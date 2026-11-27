//! The corpus's fragment stage: the derivatives, the samples whose level of
//! detail is implicit and that level queried, and the two ways a fragment
//! ends its invocation. A draw covers a square of the image work's sampled
//! images' own side, so each pixel samples its own texel at the image's one
//! level; each derivative is of a function linear across the square, so it
//! is exact.

use alloc::vec::Vec;

use tairix_gpu::{
    Aspects, BindSetLayout, ClearValue, ColourAttachment, ColourMask, Context, ContextDesc,
    CullMode, Device, Draw, Error, Features, Format, FormatFeatures, FrontFace, ImageUsage, Load,
    Pass, PushConstants, QueueCapabilities, QueueFamily, Rasterization, Rect, RenderPipeline,
    RenderPipelineDesc, Resource, StageDesc, Topology, Viewport,
};
use tairix_spirv::enumerant::{BuiltIn, Capability, Decoration, ExecutionModel, StorageClass};
use tairix_spirv::{Admitted, BuildError, Builder, Function, Id, Op, Stage, Stages, Version};

use super::{
    declare_sampled, exact, layout, member, sampled_bindings, through_buffer, Family, Held, Io,
};
use crate::images::{placed, square, whole_view, Sampled, SIDE};
use crate::render::{built_in, target, ONE_SAMPLE};
use crate::{family, family_where, in_binding_order, near, states_uses, submit};

pub(super) const FAMILY: Family = Family::compute(
    "the corpus's fragment stage",
    &[Capability::DERIVATIVE_CONTROL, Capability::IMAGE_QUERY],
    fragment,
)
.fragment(SIDE * SIDE, &["SPV_KHR_terminate_invocation"]);

/// The format of the pass's target, which each fragment writes its pixel's
/// place to.
const TARGET: Format = Format::R32Uint;

/// What the family asks of each format it uses.
const USES: [(Format, FormatFeatures); 2] = [
    (
        TARGET,
        FormatFeatures::SAMPLED.with(FormatFeatures::COLOR_ATTACHMENT),
    ),
    (Format::Depth32Float, FormatFeatures::SAMPLED),
];

/// The derivatives, each of the same linear function.
const DERIVATIVES: [(&str, Op); 9] = [
    ("DPdx", Op::D_PDX),
    ("DPdy", Op::D_PDY),
    ("Fwidth", Op::FWIDTH),
    ("DPdxFine", Op::D_PDX_FINE),
    ("DPdyFine", Op::D_PDY_FINE),
    ("FwidthFine", Op::FWIDTH_FINE),
    ("DPdxCoarse", Op::D_PDX_COARSE),
    ("DPdyCoarse", Op::D_PDY_COARSE),
    ("FwidthCoarse", Op::FWIDTH_COARSE),
];

/// Each pixel's answers, one word of each per pixel: every instruction
/// needing its quad's derivatives first, while all four run; then the pixels
/// whose place modulo four is one or two end, by each instruction in turn,
/// before writing their last word.
fn fragment(f: &mut Function<'_>, io: &mut Io) -> Result<(), BuildError> {
    let kit = io.kit;
    let place = io.built_in(f, (BuiltIn::FRAG_COORD, kit.vec(4)))?;
    let (across, down) = (
        member(f, place, kit.float, 0)?,
        member(f, place, kit.float, 1)?,
    );
    let column = f.op(Op::CONVERT_F_TO_U, kit.uint, &[across])?;
    let row = f.op(Op::CONVERT_F_TO_U, kit.uint, &[down])?;
    let side = f.module().uint(SIDE)?;
    let start = f.op(Op::I_MUL, kit.uint, &[row, side])?;
    let lane = f.op(Op::I_ADD, kit.uint, &[start, column])?;
    let written = io.global(f, (kit.uint, StorageClass::OUTPUT))?;
    f.module().decorate(written, Decoration::LOCATION, &[0])?;
    f.emit(Op::STORE, &[written, lane])?;
    let (slope_across, slope_down) = (io.float(f, 3.0)?, io.float(f, 5.0)?);
    let along = f.op(Op::F_MUL, kit.float, &[across, slope_across])?;
    let over = f.op(Op::F_MUL, kit.float, &[down, slope_down])?;
    let linear = f.op(Op::F_ADD, kit.float, &[along, over])?;
    for (case, code) in DERIVATIVES {
        let derived = f.op(code, kit.float, &[linear])?;
        io.out_lanes(f, exact(case), (derived, kit.float), lane)?;
    }
    sampled(f, io, ((across, down), column), lane)?;
    ended(f, io, lane)
}

/// The sampled images at each pixel's own texel: samples at the level of
/// detail its quad's derivatives give, plain and projective, compared and
/// not, and that level queried.
fn sampled(
    f: &mut Function<'_>,
    io: &mut Io,
    ((across, down), column): ((Id, Id), Id),
    lane: Id,
) -> Result<(), BuildError> {
    let kit = io.kit;
    let (types, [combined_at, depth_at, comparing_at]) = declare_sampled(f, io)?;
    let (scale, divisor) = (io.float(f, 0.25)?, io.float(f, 2.0)?);
    let horizontal = f.op(Op::F_MUL, kit.float, &[across, scale])?;
    let vertical = f.op(Op::F_MUL, kit.float, &[down, scale])?;
    let coordinate = f.op(Op::COMPOSITE_CONSTRUCT, kit.vec(2), &[horizontal, vertical])?;
    let far = [
        f.op(Op::F_MUL, kit.float, &[horizontal, divisor])?,
        f.op(Op::F_MUL, kit.float, &[vertical, divisor])?,
        divisor,
    ];
    let projected = f.op(Op::COMPOSITE_CONSTRUCT, kit.vec(3), &far)?;
    let combined = f.op(Op::LOAD, types.combined, &[combined_at])?;
    for (case, code, at) in [
        (
            "ImageSampleImplicitLod",
            Op::IMAGE_SAMPLE_IMPLICIT_LOD,
            coordinate,
        ),
        (
            "ImageSampleProjImplicitLod",
            Op::IMAGE_SAMPLE_PROJ_IMPLICIT_LOD,
            projected,
        ),
    ] {
        let texel = f.op(code, kit.uvec(4), &[combined, at])?;
        let value = member(f, texel, kit.uint, 0)?;
        io.out_lanes(f, exact(case), (value, kit.uint), lane)?;
    }
    let depth = f.op(Op::LOAD, types.depth, &[depth_at])?;
    let comparing = f.op(Op::LOAD, types.comparing, &[comparing_at])?;
    let compared = f.op(Op::SAMPLED_IMAGE, types.depth_sampled, &[depth, comparing])?;
    // The column over the side, as the image work compares; a projective
    // comparison divides its reference by the divisor too.
    let column_real = f.op(Op::CONVERT_U_TO_F, kit.float, &[column])?;
    let reference = f.op(Op::F_MUL, kit.float, &[column_real, scale])?;
    let projected_reference = f.op(Op::F_MUL, kit.float, &[reference, divisor])?;
    for (case, code, (at, against)) in [
        (
            "ImageSampleDrefImplicitLod",
            Op::IMAGE_SAMPLE_DREF_IMPLICIT_LOD,
            (coordinate, reference),
        ),
        (
            "ImageSampleProjDrefImplicitLod",
            Op::IMAGE_SAMPLE_PROJ_DREF_IMPLICIT_LOD,
            (projected, projected_reference),
        ),
    ] {
        let passed = f.op(code, kit.float, &[compared, at, against])?;
        io.out_lanes(f, exact(case), (passed, kit.float), lane)?;
    }
    // Its level, clamped and not, are each nought where the computed level
    // of detail is; Vulkan lets that be approximated.
    let lod = f.op(Op::IMAGE_QUERY_LOD, kit.vec(2), &[combined, coordinate])?;
    for at in 0..2 {
        let part = member(f, lod, kit.float, at)?;
        io.out_lanes(
            f,
            ("ImageQueryLod", Held::Within(8)),
            (part, kit.float),
            lane,
        )?;
    }
    Ok(())
}

/// The pixel at `lane` ending where its place modulo four is one, by
/// `OpKill`, or two, by `OpTerminateInvocation`; every other writing its
/// place plus one.
fn ended(f: &mut Function<'_>, io: &mut Io, lane: Id) -> Result<(), BuildError> {
    let kit = io.kit;
    let (one, two, four) = (
        f.module().uint(1)?,
        f.module().uint(2)?,
        f.module().uint(4)?,
    );
    let quarter = f.op(Op::U_MOD, kit.uint, &[lane, four])?;
    for (which, ending) in [(one, Op::KILL), (two, Op::TERMINATE_INVOCATION)] {
        let ends = f.op(Op::I_EQUAL, kit.boolean, &[quarter, which])?;
        f.if_then(ends, |f| f.emit(ending, &[]))?;
    }
    let survived = f.op(Op::I_ADD, kit.uint, &[lane, one])?;
    io.out_lanes(
        f,
        exact("Kill and TerminateInvocation"),
        (survived, kit.uint),
        lane,
    )
}

/// The vertex stage: the triangle whose three corners from `VertexIndex`
/// cover the whole viewport.
fn covering() -> Result<Admitted, BuildError> {
    let mut b = Builder::new(Version::OLDEST, 0)?;
    b.capability(Capability::SHADER)?;
    let (float, int, boolean) = (b.float(32)?, b.int(32, true)?, b.boolean()?);
    let corner_type = b.vector(float, 4)?;
    let index = built_in(&mut b, (StorageClass::INPUT, int), BuiltIn::VERTEX_INDEX)?;
    let position = built_in(
        &mut b,
        (StorageClass::OUTPUT, corner_type),
        BuiltIn::POSITION,
    )?;
    let (void, main) = (b.void()?, b.id()?);
    b.define(main, void, &[], |f, _| {
        let at = f.op(Op::LOAD, int, &[index])?;
        let (low, high) = (f.module().real(-1.0)?, f.module().real(3.0)?);
        let (zero, one) = (f.module().real(0.0)?, f.module().real(1.0)?);
        // The second corner is far along, the third far down.
        let mut axes = [low; 2];
        for (axis, corner) in axes.iter_mut().zip([1, 2]) {
            let named = f.module().sint(corner)?;
            let reaches = f.op(Op::I_EQUAL, boolean, &[at, named])?;
            *axis = f.op(Op::SELECT, float, &[reaches, high, low])?;
        }
        let [along, down] = axes;
        let corner = f.op(
            Op::COMPOSITE_CONSTRUCT,
            corner_type,
            &[along, down, zero, one],
        )?;
        f.emit(Op::STORE, &[position, corner])?;
        f.emit(Op::RETURN, &[])
    })?;
    b.entry_point(ExecutionModel::VERTEX, main, "main", &[index, position])?;
    b.finish()
}

/// The pipeline drawing the family's fragment stage `module` over
/// [`covering`], into the pass's one target, and its set's layout.
fn pipeline(context: &Context, module: Admitted) -> Result<(BindSetLayout, RenderPipeline), Error> {
    let bindings = layout(
        &module,
        (Stage::Fragment, Stages::FRAGMENT),
        &sampled_bindings(),
    )?;
    let set_layout = context.create_bind_set_layout(&bindings)?;
    let pipeline_layout = context.create_pipeline_layout(&[&set_layout], PushConstants::NONE)?;
    let vertex = context.create_shader_module(covering().map_err(|_| Error::Malformed)?)?;
    let fragment = context.create_shader_module(module)?;
    let stage = |module| StageDesc {
        module,
        entry_point: "main",
        specialization: &[],
    };
    let pipeline = context.create_render_pipeline(&RenderPipelineDesc {
        layout: &pipeline_layout,
        vertex: stage(&vertex),
        fragment: Some(stage(&fragment)),
        vertex_buffers: &[],
        topology: Topology::Triangles,
        primitive_restart: false,
        rasterization: Rasterization {
            cull: CullMode::None,
            front_face: FrontFace::Clockwise,
            depth_bias: None,
            depth_clamp: false,
            line_width: 1.0,
        },
        depth_stencil: None,
        multisample: ONE_SAMPLE,
        targets: &[target(TARGET, None, ColourMask::ALL)],
        logic_op: None,
    })?;
    Ok((set_layout, pipeline))
}

/// The family's fragment stage `module` run over one draw on `device`, its
/// buffer starting as `start`: the words it left; `None` where the device
/// draws nothing, writes no storage from a fragment stage, or does not state
/// the uses the family makes of its formats.
pub(super) fn draw(
    device: &Device,
    module: Admitted,
    start: &[u8],
) -> Result<Option<Vec<u32>>, Error> {
    let info = device.info();
    let Ok(graphics) = family(device, QueueCapabilities::GRAPHICS) else {
        return Ok(None);
    };
    if !info.features.contains(Features::FRAGMENT_STORES) || !states_uses(device, &USES) {
        return Ok(None);
    }
    // A depth image is filled from memory.
    let Ok(transfer) = family_where(device, QueueFamily::transfers_depth_stencil) else {
        return Ok(None);
    };
    let near = near(device)?;
    let context = device.create_context(&ContextDesc::default())?;
    let sampled = Sampled::new(&context, (near, transfer))?;
    let (_target_memory, target) = placed(
        &context,
        &square(TARGET, (1, 1), ImageUsage::COLOR_ATTACHMENT),
    )?;
    let target_view = whole_view(&context, &target, (TARGET, Aspects::COLOR))?;
    let (set_layout, pipeline) = pipeline(&context, module)?;
    let area = Rect {
        x: 0,
        y: 0,
        width: SIDE,
        height: SIDE,
    };
    let side = f32::from(u16::try_from(SIDE).map_err(|_| Error::TooLarge)?);
    through_buffer(&context, near, start, |words, size| {
        let [combined, depth, comparing] = sampled.resources();
        let resources = [
            Resource::Buffer {
                buffer: words,
                offset: 0,
                size,
            },
            combined,
            depth,
            comparing,
        ];
        let entries = in_binding_order(&resources);
        let set = context.create_bind_set(&set_layout, &entries)?;
        let drawn = Draw {
            pipeline: &pipeline,
            sets: &[&set],
            push_constants: &[],
            vertex_buffers: &[],
            viewport: Viewport {
                x: 0.0,
                y: 0.0,
                width: side,
                height: side,
                min_depth: 0.0,
                max_depth: 1.0,
            },
            scissor: area,
            stencil_reference: 0,
            blend_constant: [0.0; 4],
        };
        let mut commands = context.command_buffer();
        commands.render_pass(
            &Pass {
                colour: &[ColourAttachment {
                    view: &target_view,
                    resolve: None,
                    load: Load::Clear(ClearValue::Uint([0; 4])),
                }],
                depth_stencil: None,
                area,
            },
            |drawer| drawer.draw(&drawn, 0..3, 0..1),
        )?;
        submit(&context, graphics, &commands)
    })
    .map(Some)
}
