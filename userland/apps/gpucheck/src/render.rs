//! The render work: a pass over a colour target, a target of integer tags
//! and a depth-stencil attachment, a pass keeping that colour, and a pass of
//! points and a line into a float target; then, on a device stating four
//! samples of the colour format, a pass whose samples resolve; and last, a
//! draw from a record naming its first instance, which a device stating no
//! first instances loses its context over. Its draws test and write depth and
//! stencil, cull a mirrored rectangle, draw instances, restart an indexed
//! strip and an indexed fan, draw a list, read records, add into the colour,
//! write two channels alone and cover a sample mask's samples, held to a
//! scissor, a viewport's offset and the pass's area. Every value it leaves is
//! exact on any conforming device: each triangle is half a rectangle on whole
//! pixels, or on half pixels between the standard sample positions, each
//! point and line centred on a row of pixels, its depths far apart and its
//! tags integers. An unsigned normalised target is given nought and one
//! alone, the only values every device converts to it alike, and the samples
//! averaged into a float, whose sums of these are exact.

use alloc::vec;
use alloc::vec::Vec;
use core::ops::Range;

use tairix_gpu::{
    Allocation, Aspects, Blend, BlendFactor, BlendOp, Buffer, BufferImageCopy, BufferUsage,
    ClearValue, ColourAttachment, ColourMask, ColourTarget, CommandBuffer, CompareOp, Context,
    ContextDesc, CullMode, DepthStencilAttachment, DepthStencilState, Device, Draw, Error,
    Extent3d, Features, Format, FormatFeatures, FrontFace, Image, ImageDesc, ImageDimension,
    ImageRange, ImageUsage, ImageView, IndexFormat, Indices, Indirect, Load, Multisample, Pass,
    PipelineLayout, PushConstants, QueueCapabilities, QueueFamily, Rasterization, Rect,
    RenderPipeline, RenderPipelineDesc, ShaderModule, StageDesc, StencilFaceState, StencilOp,
    Topology, VertexAttribute, VertexBuffer, VertexStep, Viewport, DRAW_RECORD_BYTES,
};
use tairix_spirv::enumerant::{
    BuiltIn, Capability, Decoration, ExecutionMode, ExecutionModel, StorageClass,
};
use tairix_spirv::{Admitted, Arg, BuildError, Builder, Function, Id, Op, Version};

use super::images::{packed, placed, read_back, whole_view};
use super::{buffer, bytes_of, family, family_where, states_uses, submit, Rendered};

/// The colour target's format.
const COLOUR: Format = Format::Rgba8Unorm;

/// The format of the four-sample pass's target and what it resolves into.
const RESOLVED_COLOUR: Format = Format::Rgba16Float;

/// The format of the target the tags are written to.
const TAGS: Format = Format::R32Uint;

/// The format of the target the points and the line are drawn into.
const FLOATS: Format = Format::R32Float;

/// The depth-stencil attachment's format.
const DEPTH_STENCIL: Format = Format::Depth24PlusStencil8;

/// What the work asks of each format it uses, beside copying it.
const USES: [(Format, FormatFeatures); 7] = [
    (
        COLOUR,
        FormatFeatures::COLOR_ATTACHMENT.with(FormatFeatures::BLENDED),
    ),
    (TAGS, FormatFeatures::COLOR_ATTACHMENT),
    (FLOATS, FormatFeatures::COLOR_ATTACHMENT),
    (DEPTH_STENCIL, FormatFeatures::DEPTH_STENCIL),
    (Format::Rg32Float, FormatFeatures::VERTEX_INPUT),
    (Format::Rgba32Float, FormatFeatures::VERTEX_INPUT),
    (Format::R32Float, FormatFeatures::VERTEX_INPUT),
];

/// What the four-sample pass asks of its format.
const RESOLVING: FormatFeatures = FormatFeatures::COLOR_ATTACHMENT
    .with(FormatFeatures::MULTISAMPLED)
    .with(FormatFeatures::RESOLVED);

/// The side of the square the first three passes render and every viewport
/// spans.
const SIDE: u16 = 16;

/// The side of the square the four-sample pass and the draw naming its first
/// instance render: their viewport's top-left corner.
const SMALL_SIDE: u16 = 8;

/// The samples of the four-sample pass a draw may cover: all but the last.
const SAMPLE_MASK: u32 = 0b0111;

/// The colour the first pass clears to.
const BACKGROUND: [f32; 4] = [0.0, 0.0, 0.0, 1.0];

/// The colour the four-sample pass and the draw naming its first instance
/// clear to.
const CLEARED: [f32; 4] = [0.0; 4];

/// The value the float target is cleared to.
const UNDRAWN: f32 = -1.0;

/// The tag the first pass clears to.
const UNTAGGED: u32 = 7;

/// The colour the colour target holds before the first pass, whose area
/// leaves its last row as it was.
const HELD_COLOUR: [f32; 4] = [1.0; 4];

/// The tag the tags hold before the first pass.
const HELD_TAG: u32 = 9;

/// The stencil value the stencil holds before the first pass.
const HELD_STENCIL: u8 = 2;

/// The stencil value the first draw writes.
const STAMPED: u8 = 5;

/// The rows the indexed strip is scissored to.
const STRIP_ROWS: u32 = 11;

/// The columns the first indirect draw's viewport is offset by.
const SHIFT: f32 = 1.0;

/// The corners the draws run through, each scaled by its instance's
/// rectangle: the unit square as a strip, its first two a line and all four
/// points; the indexed strip's rectangles in pixels; the unit square as a
/// list; and the indexed fan's rectangles in pixels.
const CORNERS: [[f32; 2]; 26] = [
    [0.0, 0.0],
    [1.0, 0.0],
    [0.0, 1.0],
    [1.0, 1.0],
    [0.0, 8.0],
    [4.0, 8.0],
    [0.0, 12.0],
    [4.0, 12.0],
    [10.0, 0.0],
    [16.0, 0.0],
    [10.0, 4.0],
    [16.0, 4.0],
    [0.0, 0.0],
    [1.0, 0.0],
    [0.0, 1.0],
    [0.0, 1.0],
    [1.0, 0.0],
    [1.0, 1.0],
    [0.0, 0.0],
    [2.0, 0.0],
    [2.0, 4.0],
    [0.0, 4.0],
    [14.0, 10.0],
    [16.0, 10.0],
    [16.0, 14.0],
    [14.0, 14.0],
];

/// The unit square's corners: a strip, or four points.
const SQUARE: u32 = 4;

/// A line's ends: the unit square's first two corners.
const LINE_ENDS: u32 = 2;

/// Where the unit square as a list starts among [`CORNERS`].
const LIST_FIRST: u32 = 12;

/// The corners of the unit square as a list: two triangles.
const LIST_CORNERS: u32 = 6;

/// Two rectangles' corners with a restart between them: the indexed strip's
/// and the indexed fan's, counted from where each one's corners start.
const RESTARTED: [u16; 9] = [0, 1, 2, 3, u16::MAX, 4, 5, 6, 7];

/// Where the indexed strip's corners start among [`CORNERS`].
const STRIP_BASE: i32 = 4;

/// Where the indexed fan's corners start among [`CORNERS`].
const FAN_BASE: i32 = 18;

/// What one instance draws: a rectangle's corner and extent in pixels, an
/// extent below zero mirroring it; its colour; and its depth.
struct Instance {
    rect: [f32; 4],
    colour: [f32; 4],
    depth: f32,
}

/// An instance as the corners scaled by `rect` draw it, `colour`ed, at a
/// depth nothing tests.
const fn untested(rect: [f32; 4], colour: [f32; 4]) -> Instance {
    Instance {
        rect,
        colour,
        depth: 0.5,
    }
}

/// The instances, by index: two overlapping rectangles and a mirrored one
/// the first draw makes; the indexed strip's; the first indirect draw's;
/// the second pass's three; the four-sample pass's; the points and the
/// line; and the draw naming its first instance's.
const INSTANCES: [Instance; 13] = [
    Instance {
        rect: [2.0, 2.0, 8.0, 8.0],
        colour: [1.0, 0.0, 0.0, 1.0],
        depth: 0.5,
    },
    Instance {
        rect: [6.0, 6.0, 8.0, 8.0],
        colour: [0.0, 0.0, 1.0, 1.0],
        depth: 0.75,
    },
    // Nearer than either, so a device that did not cull it would show it.
    Instance {
        rect: [12.0, 1.0, -4.0, 4.0],
        colour: [1.0; 4],
        depth: 0.125,
    },
    Instance {
        rect: [0.0, 0.0, 1.0, 1.0],
        colour: [0.0, 1.0, 0.0, 1.0],
        depth: 0.25,
    },
    untested([0.0, 12.0, 8.0, 4.0], [0.0, 1.0, 1.0, 0.0]),
    untested([12.0, 0.0, 4.0, 4.0], [1.0, 0.0, 0.0, 0.0]),
    // Its edges cross pixels between the standard sample positions.
    untested([0.0, 0.0, 2.5, 6.5], [1.0, 0.0, 0.5, 1.0]),
    untested([14.0, 5.0, 2.0, 4.0], [0.0, 1.0, 1.0, 1.0]),
    untested([0.0, 0.0, 1.0, 1.0], [1.0, 1.0, 0.0, 1.0]),
    untested([3.5, 11.5, 8.0, 2.0], [0.25, 0.0, 0.0, 1.0]),
    untested([0.5, 0.5, 15.0, 15.0], [-2.5, 0.0, 0.0, 1.0]),
    untested([0.0, 7.5, 16.0, 1.0], [0.75, 0.0, 0.0, 1.0]),
    untested([1.0, 1.0, 3.0, 2.0], [1.0, 0.0, 1.0, 1.0]),
];

/// The instance the indexed strip draws; the first draw draws those before.
const STRIP_INSTANCE: u32 = 3;

/// The instance the first indirect draw reads, its instances' buffer bound
/// from it.
const INDIRECT_INSTANCE: u64 = 4;

/// The instance the second pass's masked draw draws.
const MASKED_INSTANCE: u32 = 5;

/// The instance the four-sample pass draws.
const SAMPLED_INSTANCE: u32 = 6;

/// The instance the list draws.
const LISTED_INSTANCE: u32 = 7;

/// The instance the indexed fan draws.
const FANNED_INSTANCE: u32 = 8;

/// The first of the two instances of points.
const POINTS_INSTANCE: u32 = 9;

/// The instance the line draws.
const LINE_INSTANCE: u32 = 11;

/// The instance the record naming its first instance names.
const FIRST_INSTANCE: u32 = 12;

/// The words each instance's element takes: its rectangle, its colour and
/// its depth, padded to whole vectors.
const INSTANCE_WORDS: usize = 12;

const CORNER_ATTRIBUTES: [VertexAttribute; 1] = [VertexAttribute {
    location: 0,
    format: Format::Rg32Float,
    offset: 0,
}];

const INSTANCE_ATTRIBUTES: [VertexAttribute; 3] = [
    VertexAttribute {
        location: 1,
        format: Format::Rgba32Float,
        offset: 0,
    },
    VertexAttribute {
        location: 2,
        format: Format::Rgba32Float,
        offset: 16,
    },
    VertexAttribute {
        location: 3,
        format: Format::R32Float,
        offset: 32,
    },
];

/// The bytes from one instance's element to the next.
const INSTANCE_STRIDE: u32 = 48;

const VERTEX_BUFFERS: [VertexBuffer<'static>; 2] = [
    VertexBuffer {
        stride: 8,
        step: VertexStep::Vertex,
        attributes: &CORNER_ATTRIBUTES,
    },
    VertexBuffer {
        stride: INSTANCE_STRIDE,
        step: VertexStep::Instance,
        attributes: &INSTANCE_ATTRIBUTES,
    },
];

/// The records the indirect draws read, a record apart: the unit square
/// once from the first instance, and once from [`FIRST_INSTANCE`].
const RECORDS: [u32; 8] = [SQUARE, 1, 0, 0, SQUARE, 1, 0, FIRST_INSTANCE];

/// A face's stencil test and writes, comparing and writing every bit.
const fn face(
    compare: CompareOp,
    (fail, pass, depth_fail): (StencilOp, StencilOp, StencilOp),
) -> StencilFaceState {
    StencilFaceState {
        fail,
        pass,
        depth_fail,
        compare,
        compare_mask: u8::MAX,
        write_mask: u8::MAX,
    }
}

/// The nearer fragment wins and records its depth, its stencil as `stencil`
/// sets for both faces.
const fn nearer(stencil: StencilFaceState) -> DepthStencilState {
    DepthStencilState {
        format: DEPTH_STENCIL,
        depth_compare: CompareOp::Less,
        depth_write: true,
        stencil: Some([stencil; 2]),
    }
}

/// The first draw's: every fragment stamps the stencil, and one behind
/// what is held counts up instead.
const STAMPING: Option<DepthStencilState> = Some(nearer(face(
    CompareOp::Always,
    (
        StencilOp::Keep,
        StencilOp::Replace,
        StencilOp::IncrementAndClamp,
    ),
)));

/// The indexed strip's: drawn only where the stencil holds the reference,
/// counting it up, and inverting it where it does not.
const STENCILLED: Option<DepthStencilState> = Some(nearer(face(
    CompareOp::Equal,
    (
        StencilOp::Invert,
        StencilOp::IncrementAndWrap,
        StencilOp::Keep,
    ),
)));

/// The first indirect draw's: no test, and nothing written.
const UNTESTED: Option<DepthStencilState> = Some(DepthStencilState {
    format: DEPTH_STENCIL,
    depth_compare: CompareOp::Always,
    depth_write: false,
    stencil: None,
});

pub(crate) const fn target(
    format: Format,
    blend: Option<Blend>,
    write_mask: ColourMask,
) -> ColourTarget {
    ColourTarget {
        format,
        blend,
        write_mask,
    }
}

/// The first pass's targets, both replaced.
const TAGGED: [ColourTarget; 2] = [
    target(COLOUR, None, ColourMask::ALL),
    target(TAGS, None, ColourMask::ALL),
];

/// The colour added to what is held, the tags left.
const ADDED: [ColourTarget; 2] = [
    target(
        COLOUR,
        Some(Blend {
            source_color: BlendFactor::One,
            destination_color: BlendFactor::One,
            color: BlendOp::Add,
            source_alpha: BlendFactor::One,
            destination_alpha: BlendFactor::One,
            alpha: BlendOp::Add,
        }),
        ColourMask::ALL,
    ),
    target(TAGS, None, ColourMask::NONE),
];

/// Red and alpha written alone.
const MASKED: [ColourTarget; 1] = [target(
    COLOUR,
    None,
    ColourMask::RED.with(ColourMask::ALPHA),
)];

const REPLACED: [ColourTarget; 1] = [target(COLOUR, None, ColourMask::ALL)];

const RESOLVED_TARGET: [ColourTarget; 1] = [target(RESOLVED_COLOUR, None, ColourMask::ALL)];

const FLOATED: [ColourTarget; 1] = [target(FLOATS, None, ColourMask::RED)];

/// One sample, every one covered.
pub(crate) const ONE_SAMPLE: Multisample = Multisample {
    samples: 1,
    mask: u32::MAX,
    alpha_to_coverage: false,
};

/// An interface variable of `ty` in `class` at `location`.
fn located(
    b: &mut Builder,
    (class, ty): (StorageClass, Id),
    location: u32,
) -> Result<Id, BuildError> {
    let pointer = b.pointer(class, ty)?;
    let variable = b.variable(pointer, class)?;
    b.decorate(variable, Decoration::LOCATION, &[location])?;
    Ok(variable)
}

/// [`located`], unblended between vertices.
fn flat(b: &mut Builder, typed: (StorageClass, Id), location: u32) -> Result<Id, BuildError> {
    let variable = located(b, typed, location)?;
    b.decorate(variable, Decoration::FLAT, &[])?;
    Ok(variable)
}

/// The built-in variable `built_in`, of `ty` in `class`.
pub(crate) fn built_in(
    b: &mut Builder,
    (class, ty): (StorageClass, Id),
    built_in: BuiltIn,
) -> Result<Id, BuildError> {
    let pointer = b.pointer(class, ty)?;
    let variable = b.variable(pointer, class)?;
    b.decorate(variable, Decoration::BUILT_IN, &[built_in.0])?;
    Ok(variable)
}

/// The vertex stage: each corner scaled by its instance's rectangle, from
/// pixels of a viewport [`SIDE`] square into clip space at the instance's
/// depth, its instance's colour and index passed on; a point it draws no
/// size for is a pixel wide.
fn vertex() -> Result<Admitted, BuildError> {
    let mut b = Builder::new(Version::OLDEST, 0)?;
    b.capability(Capability::SHADER)?;
    let (float, uint, int) = (b.float(32)?, b.int(32, false)?, b.int(32, true)?);
    let (v2, v4) = (b.vector(float, 2)?, b.vector(float, 4)?);
    let corner = located(&mut b, (StorageClass::INPUT, v2), 0)?;
    let rect = located(&mut b, (StorageClass::INPUT, v4), 1)?;
    let colour = located(&mut b, (StorageClass::INPUT, v4), 2)?;
    let depth = located(&mut b, (StorageClass::INPUT, float), 3)?;
    let instance = built_in(&mut b, (StorageClass::INPUT, int), BuiltIn::INSTANCE_INDEX)?;
    let position = built_in(&mut b, (StorageClass::OUTPUT, v4), BuiltIn::POSITION)?;
    let passed_colour = flat(&mut b, (StorageClass::OUTPUT, v4), 0)?;
    let passed_instance = flat(&mut b, (StorageClass::OUTPUT, uint), 1)?;
    let (void, main) = (b.void()?, b.id()?);
    b.define(main, void, &[], |f, _| {
        let at = f.op(Op::LOAD, v2, &[corner])?;
        let rectangle = f.op(Op::LOAD, v4, &[rect])?;
        let half = |f: &mut Function<'_>, first: u32| {
            f.op_args(
                Op::VECTOR_SHUFFLE,
                v2,
                &[
                    Arg::Id(rectangle),
                    Arg::Id(rectangle),
                    Arg::Word(first),
                    Arg::Word(first + 1),
                ],
            )
        };
        let (origin, extent) = (half(f, 0)?, half(f, 2)?);
        let across = f.op(Op::F_MUL, v2, &[at, extent])?;
        let pixel = f.op(Op::F_ADD, v2, &[origin, across])?;
        let scale = f.module().real(2.0 / f32::from(SIDE))?;
        let one = f.module().real(1.0)?;
        let scaled = f.op(Op::VECTOR_TIMES_SCALAR, v2, &[pixel, scale])?;
        let centred = |f: &mut Function<'_>, word: u32| {
            let value = f.op_args(
                Op::COMPOSITE_EXTRACT,
                float,
                &[Arg::Id(scaled), Arg::Word(word)],
            )?;
            f.op(Op::F_SUB, float, &[value, one])
        };
        let (x, y) = (centred(f, 0)?, centred(f, 1)?);
        let z = f.op(Op::LOAD, float, &[depth])?;
        let placed = f.op(Op::COMPOSITE_CONSTRUCT, v4, &[x, y, z, one])?;
        f.emit(Op::STORE, &[position, placed])?;
        let shade = f.op(Op::LOAD, v4, &[colour])?;
        f.emit(Op::STORE, &[passed_colour, shade])?;
        let index = f.op(Op::LOAD, int, &[instance])?;
        let unsigned = f.op(Op::BITCAST, uint, &[index])?;
        f.emit(Op::STORE, &[passed_instance, unsigned])?;
        f.emit(Op::RETURN, &[])
    })?;
    let interface = [
        corner,
        rect,
        colour,
        depth,
        instance,
        position,
        passed_colour,
        passed_instance,
    ];
    b.entry_point(ExecutionModel::VERTEX, main, "main", &interface)?;
    b.finish()
}

/// The fragment stage: the colour passed on and, where `tagged`, a tag of
/// the instance's index and the pixel's place, the instance's
/// [`SIDE`]-squared tags past the last's.
fn fragment(tagged: bool) -> Result<Admitted, BuildError> {
    let mut b = Builder::new(Version::OLDEST, 0)?;
    b.capability(Capability::SHADER)?;
    let (float, uint) = (b.float(32)?, b.int(32, false)?);
    let v4 = b.vector(float, 4)?;
    let colour = flat(&mut b, (StorageClass::INPUT, v4), 0)?;
    let written = located(&mut b, (StorageClass::OUTPUT, v4), 0)?;
    let mut interface = vec![colour, written];
    let tagging = if tagged {
        let instance = flat(&mut b, (StorageClass::INPUT, uint), 1)?;
        let coordinate = built_in(&mut b, (StorageClass::INPUT, v4), BuiltIn::FRAG_COORD)?;
        let tag = located(&mut b, (StorageClass::OUTPUT, uint), 1)?;
        interface.extend([instance, coordinate, tag]);
        Some((instance, coordinate, tag))
    } else {
        None
    };
    let (void, main) = (b.void()?, b.id()?);
    b.define(main, void, &[], |f, _| {
        let shade = f.op(Op::LOAD, v4, &[colour])?;
        f.emit(Op::STORE, &[written, shade])?;
        if let Some((instance, coordinate, tag)) = tagging {
            let index = f.op(Op::LOAD, uint, &[instance])?;
            let centre = f.op(Op::LOAD, v4, &[coordinate])?;
            let whole = |f: &mut Function<'_>, word: u32| {
                let real = f.op_args(
                    Op::COMPOSITE_EXTRACT,
                    float,
                    &[Arg::Id(centre), Arg::Word(word)],
                )?;
                f.op(Op::CONVERT_F_TO_U, uint, &[real])
            };
            let (column, line) = (whole(f, 0)?, whole(f, 1)?);
            let side = f.module().uint(u32::from(SIDE))?;
            let tags = f.module().uint(u32::from(SIDE) * u32::from(SIDE))?;
            let row = f.op(Op::I_MUL, uint, &[line, side])?;
            let pixel = f.op(Op::I_ADD, uint, &[row, column])?;
            let first = f.op(Op::I_MUL, uint, &[index, tags])?;
            let value = f.op(Op::I_ADD, uint, &[first, pixel])?;
            f.emit(Op::STORE, &[tag, value])?;
        }
        f.emit(Op::RETURN, &[])
    })?;
    b.entry_point(ExecutionModel::FRAGMENT, main, "main", &interface)?;
    b.execution_mode(main, ExecutionMode::ORIGIN_UPPER_LEFT, &[])?;
    b.finish()
}

/// The work's pipeline layout, binding nothing, and its stages' modules.
struct Programs {
    layout: PipelineLayout,
    vertex: ShaderModule,
    tagged: ShaderModule,
    untagged: ShaderModule,
}

/// What a pipeline varies: its fragment stage's tags, what its vertices
/// make and whether a strip or fan restarts, its depth and stencil, its
/// targets, and its samples.
struct Fixed<'a> {
    tagged: bool,
    topology: Topology,
    restart: bool,
    depth_stencil: Option<DepthStencilState>,
    targets: &'a [ColourTarget],
    multisample: Multisample,
}

impl Fixed<'static> {
    /// A pipeline of one sample drawing `topology` without restarts into
    /// `targets` alone, untagged.
    const fn plain(topology: Topology, targets: &'static [ColourTarget]) -> Self {
        Self {
            tagged: false,
            topology,
            restart: false,
            depth_stencil: None,
            targets,
            multisample: ONE_SAMPLE,
        }
    }
}

impl Programs {
    fn new(context: &Context) -> Result<Self, Error> {
        let module = |admitted: Result<Admitted, BuildError>| {
            context.create_shader_module(admitted.map_err(|_| Error::Malformed)?)
        };
        Ok(Self {
            layout: context.create_pipeline_layout(&[], PushConstants::NONE)?,
            vertex: module(vertex())?,
            tagged: module(fragment(true))?,
            untagged: module(fragment(false))?,
        })
    }

    /// A pipeline culling back faces, the clockwise ones on screen facing
    /// the front.
    fn pipeline(&self, context: &Context, fixed: &Fixed<'_>) -> Result<RenderPipeline, Error> {
        let stage = |module| StageDesc {
            module,
            entry_point: "main",
            specialization: &[],
        };
        let fragment = if fixed.tagged {
            &self.tagged
        } else {
            &self.untagged
        };
        context.create_render_pipeline(&RenderPipelineDesc {
            layout: &self.layout,
            vertex: stage(&self.vertex),
            fragment: Some(stage(fragment)),
            vertex_buffers: &VERTEX_BUFFERS,
            topology: fixed.topology,
            primitive_restart: fixed.restart,
            rasterization: Rasterization {
                cull: CullMode::Back,
                front_face: FrontFace::Clockwise,
                depth_bias: None,
                depth_clamp: false,
                line_width: 1.0,
            },
            depth_stencil: fixed.depth_stencil,
            multisample: fixed.multisample,
            targets: fixed.targets,
            logic_op: None,
        })
    }
}

/// The buffers the draws read, in host-visible memory: the corners, the
/// instances, the restarted indices and the records.
struct Sources {
    corners: (Allocation, Buffer),
    instances: (Allocation, Buffer),
    restarted: (Allocation, Buffer),
    records: (Allocation, Buffer),
}

/// A buffer of `bytes` for `usage` in memory of type `near`, written.
fn filled(
    context: &Context,
    near: u32,
    (bytes, usage): (&[u8], BufferUsage),
) -> Result<(Allocation, Buffer), Error> {
    let size = u64::try_from(bytes.len()).map_err(|_| Error::TooLarge)?;
    let (memory, written) = buffer(context, (near, size.next_multiple_of(4)), usage)?;
    memory.write(0, bytes)?;
    Ok((memory, written))
}

impl Sources {
    fn new(context: &Context, near: u32) -> Result<Self, Error> {
        let corners: Vec<u32> = CORNERS
            .iter()
            .flatten()
            .map(|axis| axis.to_bits())
            .collect();
        let mut instances = Vec::new();
        for instance in &INSTANCES {
            let mut words = [0u32; INSTANCE_WORDS];
            let values = instance
                .rect
                .iter()
                .chain(&instance.colour)
                .chain([&instance.depth]);
            for (word, value) in words.iter_mut().zip(values) {
                *word = value.to_bits();
            }
            instances.extend(words);
        }
        let restarted: Vec<u8> = RESTARTED
            .iter()
            .flat_map(|index| index.to_le_bytes())
            .collect();
        Ok(Self {
            corners: filled(context, near, (&bytes_of(&corners), BufferUsage::VERTEX))?,
            instances: filled(context, near, (&bytes_of(&instances), BufferUsage::VERTEX))?,
            restarted: filled(context, near, (&restarted, BufferUsage::INDEX))?,
            records: filled(context, near, (&bytes_of(&RECORDS), BufferUsage::INDIRECT))?,
        })
    }

    /// The vertex buffers a draw binds: the corners, and the instances from
    /// the instance `first`.
    fn bound(&self, first: u64) -> [(&Buffer, u64); 2] {
        [
            (&self.corners.1, 0),
            (&self.instances.1, first * u64::from(INSTANCE_STRIDE)),
        ]
    }

    /// Where the restarted indices lie.
    const fn indices(&self) -> Indices<'_> {
        Indices {
            buffer: &self.restarted.1,
            offset: 0,
            format: IndexFormat::U16,
        }
    }

    /// The record at `at`, of the records.
    fn record(&self, at: u32) -> Indirect<'_> {
        Indirect {
            buffer: &self.records.1,
            offset: u64::from(at * DRAW_RECORD_BYTES),
            count: 1,
            stride: DRAW_RECORD_BYTES,
        }
    }
}

/// An image in memory of its own, and a view of all of it.
struct Target {
    _memory: Allocation,
    image: Image,
    view: ImageView,
}

/// An image of `format` `side` texels square with `samples` samples, for
/// `usage`.
fn square(
    context: &Context,
    (format, side, samples): (Format, u16, u32),
    usage: ImageUsage,
) -> Result<Target, Error> {
    let (memory, image) = placed(
        context,
        &ImageDesc {
            dimension: ImageDimension::D2,
            format,
            extent: Extent3d {
                width: u32::from(side),
                height: u32::from(side),
                depth: 1,
            },
            mip_levels: 1,
            array_layers: 1,
            samples,
            usage,
        },
    )?;
    let view = whole_view(context, &image, (format, format.aspects()))?;
    Ok(Target {
        _memory: memory,
        image,
        view,
    })
}

/// The viewport every draw renders through, `x` columns along.
fn viewport(x: f32) -> Viewport {
    Viewport {
        x,
        y: 0.0,
        width: f32::from(SIDE),
        height: f32::from(SIDE),
        min_depth: 0.0,
        max_depth: 1.0,
    }
}

/// A rectangle from the corner `width` wide, `height` tall.
const fn rect(width: u32, height: u32) -> Rect {
    Rect {
        x: 0,
        y: 0,
        width,
        height,
    }
}

/// A draw through `pipeline` binding `vertex_buffers`, through `viewport`
/// within `scissor`, stencilled against `stencil_reference`.
const fn draw<'a>(
    pipeline: &'a RenderPipeline,
    vertex_buffers: &'a [(&'a Buffer, u64)],
    (viewport, scissor): (Viewport, Rect),
    stencil_reference: u8,
) -> Draw<'a> {
    Draw {
        pipeline,
        sets: &[],
        push_constants: &[],
        vertex_buffers,
        viewport,
        scissor,
        stencil_reference,
        blend_constant: [0.0; 4],
    }
}

/// The bytes of a plane `side` texels square of `per_texel` bytes each.
fn plane_bytes(side: u16, per_texel: u32) -> u64 {
    u64::from(u32::from(side) * u32::from(side) * per_texel)
}

/// A copy of all of plane `aspect` of an image `side` texels square of
/// `per_texel` bytes each.
fn plane(aspect: Aspects, side: u16, per_texel: u32) -> [BufferImageCopy; 1] {
    [packed(0, (aspect, 0, 1), (u32::from(side), per_texel))]
}

/// What the render work leaves on `device`, through host-visible memory of
/// type `near`; `None` where the device draws nothing or states not every
/// use it makes of its formats.
pub(crate) fn work(device: &Device, near: u32) -> Result<Option<Rendered>, Error> {
    let info = device.info();
    let Ok(graphics) = family(device, QueueCapabilities::GRAPHICS) else {
        return Ok(None);
    };
    if !states_uses(device, &USES) {
        return Ok(None);
    }
    let Ok(transfer) = family_where(device, QueueFamily::transfers_depth_stencil) else {
        return Ok(None);
    };
    let queues = (near, transfer, graphics);
    let context = device.create_context(&ContextDesc::default())?;
    let programs = Programs::new(&context)?;
    let sources = Sources::new(&context, near)?;
    let passes = passes(&context, queues, &programs, &sources)?;
    let resolved = if info.formats.get(RESOLVED_COLOUR).contains(RESOLVING) {
        Some(resolve(&context, queues, &programs, &sources)?)
    } else {
        None
    };
    // Last, since a device stating no first instances loses the context.
    let first_instance = first_instance(&context, queues, &programs, &sources)?;
    Ok(Some(Rendered {
        passes,
        resolved,
        first_instance,
        states_first_instance: info
            .features
            .contains(Features::DRAW_INDIRECT_FIRST_INSTANCE),
    }))
}

/// The first three passes' images.
struct Attachments {
    colour: Target,
    tags: Target,
    floats: Target,
    depth_stencil: Target,
}

impl Attachments {
    fn new(context: &Context) -> Result<Self, Error> {
        let copied = ImageUsage::TRANSFER_SRC.with(ImageUsage::TRANSFER_DST);
        let colour = copied.with(ImageUsage::COLOR_ATTACHMENT);
        Ok(Self {
            colour: square(context, (COLOUR, SIDE, 1), colour)?,
            tags: square(context, (TAGS, SIDE, 1), colour)?,
            floats: square(
                context,
                (FLOATS, SIDE, 1),
                ImageUsage::TRANSFER_SRC.with(ImageUsage::COLOR_ATTACHMENT),
            )?,
            depth_stencil: square(
                context,
                (DEPTH_STENCIL, SIDE, 1),
                copied.with(ImageUsage::DEPTH_STENCIL_ATTACHMENT),
            )?,
        })
    }

    /// Fill what the first pass's area leaves as it was, on a queue of
    /// family `transfer`.
    fn hold(&self, context: &Context, transfer: u32) -> Result<(), Error> {
        let whole = |aspects| ImageRange {
            aspects,
            base_level: 0,
            levels: 1,
            base_layer: 0,
            layers: 1,
        };
        let colour = [whole(Aspects::COLOR)];
        let mut held = context.command_buffer();
        held.clear_image(&self.colour.image, ClearValue::Float(HELD_COLOUR), &colour)?;
        held.clear_image(
            &self.tags.image,
            ClearValue::Uint([HELD_TAG, 0, 0, 0]),
            &colour,
        )?;
        held.clear_image(
            &self.depth_stencil.image,
            ClearValue::DepthStencil {
                depth: 1.0,
                stencil: HELD_STENCIL,
            },
            &[whole(Aspects::DEPTH_STENCIL)],
        )?;
        submit(context, transfer, &held)
    }

    /// What the passes left in the colour target, the tags, the stencil and
    /// the float target, read back as `read_back` reads through `reach`.
    fn read(&self, context: &Context, reach: (u32, u32)) -> Result<Vec<u8>, Error> {
        let (colour, stencil) = (
            plane(Aspects::COLOR, SIDE, 4),
            plane(Aspects::STENCIL, SIDE, 1),
        );
        let (word, byte) = (plane_bytes(SIDE, 4), plane_bytes(SIDE, 1));
        read_back(
            context,
            reach,
            &[
                (&self.colour.image, &colour, word),
                (&self.tags.image, &colour, word),
                (&self.depth_stencil.image, &stencil, byte),
                (&self.floats.image, &colour, word),
            ],
        )
    }
}

/// The first three passes' pipelines.
struct Pipelines {
    stamping: RenderPipeline,
    stencilled: RenderPipeline,
    added: RenderPipeline,
    masked: RenderPipeline,
    listed: RenderPipeline,
    fanned: RenderPipeline,
    pointed: RenderPipeline,
    lined: RenderPipeline,
}

impl Pipelines {
    fn new(context: &Context, programs: &Programs) -> Result<Self, Error> {
        let make = |fixed: Fixed<'_>| programs.pipeline(context, &fixed);
        let tagged = |depth_stencil, targets: &'static [ColourTarget], restart| Fixed {
            tagged: true,
            topology: Topology::TriangleStrip,
            restart,
            depth_stencil,
            targets,
            multisample: ONE_SAMPLE,
        };
        Ok(Self {
            stamping: make(tagged(STAMPING, &TAGGED, false))?,
            stencilled: make(tagged(STENCILLED, &TAGGED, true))?,
            added: make(tagged(UNTESTED, &ADDED, false))?,
            masked: make(Fixed::plain(Topology::TriangleStrip, &MASKED))?,
            listed: make(Fixed::plain(Topology::Triangles, &REPLACED))?,
            fanned: make(Fixed {
                restart: true,
                ..Fixed::plain(Topology::TriangleFan, &REPLACED)
            })?,
            pointed: make(Fixed::plain(Topology::Points, &FLOATED))?,
            lined: make(Fixed::plain(Topology::Lines, &FLOATED))?,
        })
    }
}

/// The first three passes' images whole, through the viewport at their
/// corner.
fn everywhere() -> (Viewport, Rect) {
    (viewport(0.0), rect(u32::from(SIDE), u32::from(SIDE)))
}

/// The instance `instance` alone.
const fn one(instance: u32) -> Range<u32> {
    instance..instance + 1
}

/// Every one of the restarted indices.
fn restarted() -> Result<Range<u32>, Error> {
    Ok(0..u32::try_from(RESTARTED.len()).map_err(|_| Error::TooLarge)?)
}

/// The first pass, its area a row short of its images: instanced strips
/// stamping the stencil, an indexed strip restarting under a scissor where
/// the stencil holds nought, and an indirect strip added into the colour
/// through an offset viewport.
fn first_pass(
    commands: &mut CommandBuffer,
    (attachments, pipelines): (&Attachments, &Pipelines),
    sources: &Sources,
) -> Result<(), Error> {
    let whole = everywhere();
    let (from_start, from_indirect) = (sources.bound(0), sources.bound(INDIRECT_INSTANCE));
    let restarted = restarted()?;
    let cleared = Load::Clear(ClearValue::DepthStencil {
        depth: 1.0,
        stencil: 0,
    });
    commands.render_pass(
        &Pass {
            colour: &[
                ColourAttachment {
                    view: &attachments.colour.view,
                    resolve: None,
                    load: Load::Clear(ClearValue::Float(BACKGROUND)),
                },
                ColourAttachment {
                    view: &attachments.tags.view,
                    resolve: None,
                    load: Load::Clear(ClearValue::Uint([UNTAGGED, 0, 0, 0])),
                },
            ],
            depth_stencil: Some(DepthStencilAttachment {
                view: &attachments.depth_stencil.view,
                depth_load: cleared,
                stencil_load: cleared,
            }),
            area: rect(u32::from(SIDE), u32::from(SIDE) - 1),
        },
        |drawer| {
            drawer.draw(
                &draw(&pipelines.stamping, &from_start, whole, STAMPED),
                0..SQUARE,
                0..STRIP_INSTANCE,
            )?;
            drawer.draw_indexed(
                &draw(
                    &pipelines.stencilled,
                    &from_start,
                    (whole.0, rect(u32::from(SIDE), STRIP_ROWS)),
                    0,
                ),
                &sources.indices(),
                (restarted, STRIP_BASE),
                one(STRIP_INSTANCE),
            )?;
            drawer.draw_indirect(
                &draw(
                    &pipelines.added,
                    &from_indirect,
                    (viewport(SHIFT), whole.1),
                    0,
                ),
                &sources.record(0),
            )
        },
    )
}

/// The second pass, keeping the colour: a strip writing red and alpha
/// alone, a list, and an indexed fan restarting.
fn second_pass(
    commands: &mut CommandBuffer,
    (attachments, pipelines): (&Attachments, &Pipelines),
    sources: &Sources,
) -> Result<(), Error> {
    let whole = everywhere();
    let from_start = sources.bound(0);
    let restarted = restarted()?;
    commands.render_pass(
        &Pass {
            colour: &[ColourAttachment {
                view: &attachments.colour.view,
                resolve: None,
                load: Load::Keep,
            }],
            depth_stencil: None,
            area: whole.1,
        },
        |drawer| {
            drawer.draw(
                &draw(&pipelines.masked, &from_start, whole, 0),
                0..SQUARE,
                one(MASKED_INSTANCE),
            )?;
            drawer.draw(
                &draw(&pipelines.listed, &from_start, whole, 0),
                LIST_FIRST..LIST_FIRST + LIST_CORNERS,
                one(LISTED_INSTANCE),
            )?;
            drawer.draw_indexed(
                &draw(&pipelines.fanned, &from_start, whole, 0),
                &sources.indices(),
                (restarted, FAN_BASE),
                one(FANNED_INSTANCE),
            )
        },
    )
}

/// The third pass: two instances of four points, then a line, into the
/// float target.
fn third_pass(
    commands: &mut CommandBuffer,
    (attachments, pipelines): (&Attachments, &Pipelines),
    sources: &Sources,
) -> Result<(), Error> {
    let whole = everywhere();
    let from_start = sources.bound(0);
    commands.render_pass(
        &Pass {
            colour: &[ColourAttachment {
                view: &attachments.floats.view,
                resolve: None,
                load: Load::Clear(ClearValue::Float([UNDRAWN, 0.0, 0.0, 0.0])),
            }],
            depth_stencil: None,
            area: whole.1,
        },
        |drawer| {
            drawer.draw(
                &draw(&pipelines.pointed, &from_start, whole, 0),
                0..SQUARE,
                POINTS_INSTANCE..POINTS_INSTANCE + 2,
            )?;
            drawer.draw(
                &draw(&pipelines.lined, &from_start, whole, 0),
                0..LINE_ENDS,
                one(LINE_INSTANCE),
            )
        },
    )
}

/// The first three passes, on a queue of family `graphics`, their images
/// filled and read back on one of family `transfer`: what they leave in the
/// colour target, the tags, the stencil and the float target.
fn passes(
    context: &Context,
    (near, transfer, graphics): (u32, u32, u32),
    programs: &Programs,
    sources: &Sources,
) -> Result<Vec<u8>, Error> {
    let attachments = Attachments::new(context)?;
    attachments.hold(context, transfer)?;
    let pipelines = Pipelines::new(context, programs)?;
    let drawn = (&attachments, &pipelines);
    let mut commands = context.command_buffer();
    first_pass(&mut commands, drawn, sources)?;
    second_pass(&mut commands, drawn, sources)?;
    third_pass(&mut commands, drawn, sources)?;
    submit(context, graphics, &commands)?;
    attachments.read(context, (near, transfer))
}

/// The four-sample pass, on a queue of family `graphics`, read back on one
/// of family `transfer`: what it resolved to.
fn resolve(
    context: &Context,
    (near, transfer, graphics): (u32, u32, u32),
    programs: &Programs,
    sources: &Sources,
) -> Result<Vec<u8>, Error> {
    let four = square(
        context,
        (RESOLVED_COLOUR, SMALL_SIDE, 4),
        ImageUsage::COLOR_ATTACHMENT,
    )?;
    let resolved = square(
        context,
        (RESOLVED_COLOUR, SMALL_SIDE, 1),
        ImageUsage::COLOR_ATTACHMENT.with(ImageUsage::TRANSFER_SRC),
    )?;
    let sampled = programs.pipeline(
        context,
        &Fixed {
            multisample: Multisample {
                samples: 4,
                mask: SAMPLE_MASK,
                alpha_to_coverage: false,
            },
            ..Fixed::plain(Topology::TriangleStrip, &RESOLVED_TARGET)
        },
    )?;
    let area = rect(u32::from(SMALL_SIDE), u32::from(SMALL_SIDE));
    let mut commands = context.command_buffer();
    commands.render_pass(
        &Pass {
            colour: &[ColourAttachment {
                view: &four.view,
                resolve: Some(&resolved.view),
                load: Load::Clear(ClearValue::Float(CLEARED)),
            }],
            depth_stencil: None,
            area,
        },
        |drawer| {
            drawer.draw(
                &draw(&sampled, &sources.bound(0), (viewport(0.0), area), 0),
                0..SQUARE,
                one(SAMPLED_INSTANCE),
            )
        },
    )?;
    submit(context, graphics, &commands)?;
    read_back(
        context,
        (near, transfer),
        &[(
            &resolved.image,
            &plane(Aspects::COLOR, SMALL_SIDE, 8),
            plane_bytes(SMALL_SIDE, 8),
        )],
    )
}

/// A draw from the record naming [`FIRST_INSTANCE`], on a queue of family
/// `graphics`, read back on one of family `transfer`: what it left, or what
/// its submission was answered with.
fn first_instance(
    context: &Context,
    (near, transfer, graphics): (u32, u32, u32),
    programs: &Programs,
    sources: &Sources,
) -> Result<Result<Vec<u8>, Error>, Error> {
    let drawn_into = square(
        context,
        (COLOUR, SMALL_SIDE, 1),
        ImageUsage::COLOR_ATTACHMENT.with(ImageUsage::TRANSFER_SRC),
    )?;
    let drawn = programs.pipeline(context, &Fixed::plain(Topology::TriangleStrip, &REPLACED))?;
    let area = rect(u32::from(SMALL_SIDE), u32::from(SMALL_SIDE));
    let mut commands = context.command_buffer();
    commands.render_pass(
        &Pass {
            colour: &[ColourAttachment {
                view: &drawn_into.view,
                resolve: None,
                load: Load::Clear(ClearValue::Float(CLEARED)),
            }],
            depth_stencil: None,
            area,
        },
        |drawer| {
            drawer.draw_indirect(
                &draw(&drawn, &sources.bound(0), (viewport(0.0), area), 0),
                &sources.record(1),
            )
        },
    )?;
    if let Err(answered) = submit(context, graphics, &commands) {
        return Ok(Err(answered));
    }
    read_back(
        context,
        (near, transfer),
        &[(
            &drawn_into.image,
            &plane(Aspects::COLOR, SMALL_SIDE, 4),
            plane_bytes(SMALL_SIDE, 4),
        )],
    )
    .map(Ok)
}
