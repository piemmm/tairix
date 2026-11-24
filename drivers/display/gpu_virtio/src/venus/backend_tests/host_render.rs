//! The renderer's render pipelines and rendering instances: each pipeline
//! decoded as the renderer reads it and made again on the reference, and
//! each instance's draws run there through what they render to and read, what
//! they leave written back. Whatever Vulkan's valid usage refuses or leaves
//! undefined — a channel written that no output writes, a draw outside its
//! instance's area, state a draw reads unset — is fatal to its context.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

use tairix_gpu::{
    Blend, BlendFactor, BlendOp, BufferDesc, BufferUsage, ClearValue, ColourAttachment, ColourMask,
    ColourTarget, CompareOp, ContextDesc, CullMode, DepthBias, DepthStencilAttachment,
    DepthStencilState, Direction, Draw, Format, FrontFace, IndexFormat, Indices, Indirect, Load,
    LogicOp, Multisample, Pass, PushConstants, Rasterization, Rect, RenderPipelineDesc, StageDesc,
    StencilFaceState, StencilOp, Topology, VertexAttribute, VertexBuffer, VertexStep, Viewport,
};
use tairix_spirv::{Admitted, Module, Stage};

use super::host_images::{self, seam_format};
use super::{bind, held, stages_of, Host, HostState, Object, Range, Unsent, Uploaded, Words};
use crate::three_d::Context;
use crate::venus::encode::LayoutEntry;
use crate::venus::{image, stage, Structure};

/// A render pipeline as the renderer keeps it: its stages, its layout's sets
/// and push constants, the state its draws run through, and the host's
/// formats of its targets, its depth and its stencil.
#[derive(Clone)]
pub(super) struct Graphics {
    pub(super) stages: Vec<(Arc<Admitted>, String, Stage)>,
    pub(super) sets: Vec<Vec<LayoutEntry>>,
    pub(super) push: (u32, u32),
    buffers: Vec<(u32, VertexStep, Vec<VertexAttribute>)>,
    topology: Topology,
    restart: bool,
    rasterization: Rasterization,
    depth_stencil: Option<DepthStencilState>,
    multisample: Multisample,
    targets: Vec<ColourTarget>,
    logic_op: Option<LogicOp>,
    formats: (Vec<i32>, i32, i32),
}

/// The place of `drawIndirectFirstInstance` among the core features.
const DRAW_INDIRECT_FIRST_INSTANCE: usize = 10;

/// A rendering instance as recorded: its area, and each attachment.
#[derive(Clone)]
pub(super) struct Rendering {
    area: Rect,
    colour: Vec<Attachment>,
    depth: Option<Attachment>,
    stencil: Option<Attachment>,
}

/// An attachment of a rendering instance: its view, the view its samples
/// are averaged into, and what it is cleared to, else loaded.
#[derive(Copy, Clone)]
struct Attachment {
    view: u64,
    resolve: Option<u64>,
    clear: Option<Clear>,
}

/// A clear as an attachment names it: a colour's kind and its bits, or a
/// depth and a stencil value.
#[derive(Copy, Clone)]
enum Clear {
    Colour(u32, [u32; 4]),
    DepthStencil(f32, u32),
}

/// What a draw runs with, as a command buffer has set it.
#[derive(Clone, Default)]
pub(super) struct DrawState {
    pub(super) pipeline: Option<u64>,
    pub(super) sets: Vec<u64>,
    pub(super) push: Vec<u8>,
    pub(super) vertex_buffers: Vec<[u64; 3]>,
    pub(super) index: Option<([u64; 3], bool)>,
    pub(super) viewport: Option<[f32; 6]>,
    pub(super) scissor: Option<[i64; 4]>,
    pub(super) reference: Option<u32>,
    pub(super) constants: Option<[f32; 4]>,
}

/// A draw as recorded: its counts, given or in records.
#[derive(Copy, Clone)]
pub(super) enum DrawRecord {
    /// Vertices' count, instances' count, first vertex and first instance.
    Vertices([u32; 4]),
    /// Indices' count, instances' count and first index; the word added to
    /// each index; the first instance.
    Indexed([u32; 3], i32, u32),
    /// Whether indexed, the buffer and where in it, the records' count and
    /// stride.
    Indirect(bool, u64, u64, u32, u32),
}

/// A rendering instance begun, and each draw recorded into it with what it
/// ran with.
pub(super) struct PassRun {
    rendering: Rendering,
    draws: Vec<(DrawState, DrawRecord)>,
}

impl PassRun {
    pub(super) const fn new(rendering: Rendering) -> Self {
        Self {
            rendering,
            draws: Vec::new(),
        }
    }
}

/// A count of `T` read with `read`: its count, then its array's size, then
/// each.
fn counted<'w, T>(
    words: &mut Words<'w>,
    mut read: impl FnMut(&mut Words<'w>) -> Option<T>,
) -> Option<Vec<T>> {
    let count = words.u32()?;
    words.is(u64::from(count))?;
    (0..count).map(|_| read(words)).collect()
}

/// The render pipeline the stream names after its device: its id and the
/// pipeline, its lines asked for as rectangles exactly where `rectangular`.
pub(super) fn decode_graphics(
    state: &HostState,
    context: Context,
    words: &mut Words<'_>,
    rectangular: bool,
) -> Option<(u64, Graphics)> {
    let objects = &state.contexts.get(context)?.objects;
    words.is(0)?;
    words.word(1)?;
    words.is(1)?;
    (words.i32()? == Structure::GraphicsPipelineCreateInfo as i32).then_some(())?;
    (words.chain()? == [Structure::PipelineRenderingCreateInfo as i32]).then_some(())?;
    words.word(0)?;
    let colour_formats = counted(words, Words::i32)?;
    let (depth_format, stencil_format) = (words.i32()?, words.i32()?);
    words.word(0)?;
    let stages = decode_stages(words, objects)?;
    words.is(1)?;
    let buffers = decode_vertex_input(words)?;
    words.is(1)?;
    words.structure(Structure::PipelineInputAssemblyStateCreateInfo)?;
    words.word(0)?;
    let topology = topology(words.u32()?)?;
    let restart = words.boolean()?;
    // No tessellation, and one viewport and one scissor, each set by a draw.
    words.is(0)?;
    words.is(1)?;
    words.structure(Structure::PipelineViewportStateCreateInfo)?;
    words.word(0)?;
    words.word(1)?;
    words.is(0)?;
    words.word(1)?;
    words.is(0)?;
    words.is(1)?;
    let rasterization = rasterization(words, rectangular)?;
    words.is(1)?;
    let multisample = decode_multisample(words)?;
    words.is(1)?;
    let tests = decode_tests(words)?;
    words.is(1)?;
    let (logic_op, blends) = decode_blend(words)?;
    words.is(1)?;
    decode_dynamic(words)?;
    let layout = words.u64()?;
    words.is(0)?;
    words.word(0)?;
    words.is(0)?;
    (words.i32()? == -1).then_some(())?;
    words.is(0)?;
    words.is(1)?;
    let id = words.u64()?;
    let Some(Object::PipelineLayout { sets, push }) = objects.get(&layout) else {
        return None;
    };
    let graphics = Graphics {
        stages,
        sets: sets.clone(),
        push: *push,
        buffers,
        topology,
        restart,
        rasterization,
        depth_stencil: match (depth_format, stencil_format) {
            (0, 0) => None,
            formats => Some(tests.state(formats)?),
        },
        multisample,
        targets: colour_targets(&colour_formats, &blends)?,
        logic_op,
        formats: (colour_formats, depth_format, stencil_format),
    };
    graphics
        .writes_only_what_is_written()
        .then_some((id, graphics))
}

/// A pipeline's stages: its vertex stage, then its fragment stage where it
/// has one, each an entry point of a module made before it.
fn decode_stages(
    words: &mut Words<'_>,
    objects: &BTreeMap<u64, Object>,
) -> Option<Vec<(Arc<Admitted>, String, Stage)>> {
    let count = words.u32()?;
    words.is(u64::from(count))?;
    let mut stages = Vec::new();
    for at in 0..count {
        words.structure(Structure::PipelineShaderStageCreateInfo)?;
        words.word(0)?;
        let (wanted, kind) = match at {
            0 => (stage::VERTEX, Stage::Vertex),
            1 => (stage::FRAGMENT, Stage::Fragment),
            _ => return None,
        };
        words.word(wanted)?;
        let module = words.u64()?;
        let entry = words.string()?;
        words.is(0)?;
        let Some(Object::Module(admitted)) = objects.get(&module) else {
            return None;
        };
        admitted.entry_point(&entry, kind)?;
        stages.push((Arc::clone(admitted), entry, kind));
    }
    (!stages.is_empty()).then_some(stages)
}

/// A pipeline's vertex buffers, each its stride, what steps it and the
/// attributes it holds, every attribute held by one of them.
fn decode_vertex_input(
    words: &mut Words<'_>,
) -> Option<Vec<(u32, VertexStep, Vec<VertexAttribute>)>> {
    words.structure(Structure::PipelineVertexInputStateCreateInfo)?;
    words.word(0)?;
    let bindings = counted(words, |words| {
        Some([words.u32()?, words.u32()?, words.u32()?])
    })?;
    let attributes = counted(words, |words| {
        Some((words.u32()?, words.u32()?, words.i32()?, words.u32()?))
    })?;
    let mut buffers = Vec::new();
    for (index, [binding, stride, rate]) in (0u32..).zip(bindings) {
        (binding == index).then_some(())?;
        let step = match rate {
            0 => VertexStep::Vertex,
            1 => VertexStep::Instance,
            _ => return None,
        };
        let fed = attributes
            .iter()
            .filter(|&&(_, from, _, _)| from == index)
            .map(|&(location, _, format, offset)| {
                Some(VertexAttribute {
                    location,
                    format: seam_format(format)?,
                    offset,
                })
            })
            .collect::<Option<Vec<_>>>()?;
        buffers.push((stride, step, fed));
    }
    attributes
        .iter()
        .all(|&(_, from, _, _)| (from as usize) < buffers.len())
        .then_some(buffers)
}

/// A pipeline's samples: one or four, none shaded alone.
fn decode_multisample(words: &mut Words<'_>) -> Option<Multisample> {
    words.structure(Structure::PipelineMultisampleStateCreateInfo)?;
    words.word(0)?;
    let samples = words.u32()?;
    (samples == 1 || samples == 4).then_some(())?;
    words.word(0)?;
    words.f32()?;
    words.is(1)?;
    let mask = words.u32()?;
    let alpha_to_coverage = words.boolean()?;
    words.word(0)?;
    Some(Multisample {
        samples,
        mask,
        alpha_to_coverage,
    })
}

/// A pipeline's depth and stencil tests as its depth-stencil state names
/// them.
#[derive(Copy, Clone)]
struct Tests {
    tested: bool,
    written: bool,
    compare: CompareOp,
    faces: Option<[StencilFaceState; 2]>,
}

fn decode_tests(words: &mut Words<'_>) -> Option<Tests> {
    words.structure(Structure::PipelineDepthStencilStateCreateInfo)?;
    words.word(0)?;
    let (tested, written) = (words.boolean()?, words.boolean()?);
    let compare = compare(words.u32()?)?;
    words.word(0)?;
    let stencilled = words.boolean()?;
    let front = face(words)?;
    let back = face(words)?;
    words.f32()?;
    words.f32()?;
    Some(Tests {
        tested,
        written,
        compare,
        faces: stencilled.then_some([front, back]),
    })
}

impl Tests {
    /// The depth-stencil state of an attachment of host formats `depth` and
    /// `stencil`, one at least, tested as these say: refused where a depth
    /// is written untested.
    fn state(self, (depth, stencil): (i32, i32)) -> Option<DepthStencilState> {
        let format = seam_format(if depth != 0 { depth } else { stencil })?;
        (depth == 0 || stencil == 0 || depth == stencil).then_some(())?;
        (self.tested || !self.written).then_some(())?;
        Some(DepthStencilState {
            format,
            depth_compare: if self.tested {
                self.compare
            } else {
                CompareOp::Always
            },
            depth_write: self.written,
            stencil: self.faces,
        })
    }
}

/// A target's blend as the host names it: whether it blends, its colour and
/// alpha factors and operations, and the channels it writes.
type HostBlend = (bool, [u32; 6], u32);

/// A pipeline's logic operation, where it has one, and its targets' blends;
/// its blend constants are each draw's.
fn decode_blend(words: &mut Words<'_>) -> Option<(Option<LogicOp>, Vec<HostBlend>)> {
    words.structure(Structure::PipelineColorBlendStateCreateInfo)?;
    words.word(0)?;
    let logic = words.boolean()?;
    let logic_op = logic_op(words.u32()?)?;
    let blends = counted(words, |words| {
        let enabled = words.boolean()?;
        let factors = [
            words.u32()?,
            words.u32()?,
            words.u32()?,
            words.u32()?,
            words.u32()?,
            words.u32()?,
        ];
        Some((enabled, factors, words.u32()?))
    })?;
    words.is(4)?;
    for _ in 0..4 {
        words.f32()?;
    }
    Some((logic.then_some(logic_op), blends))
}

/// A pipeline's dynamic state: its viewport, scissor, blend constants and
/// stencil reference, each set by a draw.
fn decode_dynamic(words: &mut Words<'_>) -> Option<()> {
    words.structure(Structure::PipelineDynamicStateCreateInfo)?;
    words.word(0)?;
    let mut dynamic = counted(words, Words::i32)?;
    dynamic.sort_unstable();
    (dynamic == [0, 1, 4, 8]).then_some(())
}

/// The targets of host formats `formats`, each blended as its one of
/// `blends` says.
fn colour_targets(formats: &[i32], blends: &[HostBlend]) -> Option<Vec<ColourTarget>> {
    (blends.len() == formats.len()).then_some(())?;
    formats
        .iter()
        .zip(blends)
        .map(|(&format, &(enabled, factors, write_mask))| {
            let blend = if enabled {
                Some(host_blend(factors)?)
            } else {
                None
            };
            (write_mask <= 0xf).then_some(())?;
            Some(ColourTarget {
                format: seam_format(format)?,
                blend,
                write_mask: ColourMask::from_bits(u8::try_from(write_mask).ok()?),
            })
        })
        .collect()
}

/// The blend of a target's colour and alpha factors and operations, as the
/// host names them.
fn host_blend(
    [source_color, destination_color, color, source_alpha, destination_alpha, alpha]: [u32; 6],
) -> Option<Blend> {
    Some(Blend {
        source_color: factor(source_color)?,
        destination_color: factor(destination_color)?,
        color: blend_op(color)?,
        source_alpha: factor(source_alpha)?,
        destination_alpha: factor(destination_alpha)?,
        alpha: blend_op(alpha)?,
    })
}

impl Graphics {
    /// Whether each target is written in no channel its fragment stage
    /// writes nothing to, which Vulkan leaves undefined.
    fn writes_only_what_is_written(&self) -> bool {
        let fragment = self
            .stages
            .iter()
            .find(|(_, _, stage)| *stage == Stage::Fragment)
            .and_then(|(module, entry, stage)| module.entry_point(entry, *stage));
        self.targets.iter().zip(0u32..).all(|(target, location)| {
            let written = fragment.map_or(0, |entry| {
                entry
                    .outputs()
                    .iter()
                    .filter(|output| output.location == location && output.index == 0)
                    .fold(0u8, |channels, output| {
                        let span = (1u32 << output.components.min(4)) - 1;
                        channels
                            | u8::try_from((span << output.component.min(3)) & 0xf).unwrap_or(0)
                    })
            });
            target.write_mask.bits() & !written == 0
        })
    }
}

fn rasterization(words: &mut Words<'_>, rectangular: bool) -> Option<Rasterization> {
    (words.i32()? == Structure::PipelineRasterizationStateCreateInfo as i32).then_some(())?;
    let chain = words.chain()?;
    if rectangular {
        (chain == [Structure::PipelineRasterizationLineStateCreateInfo as i32]).then_some(())?;
        words.word(1)?;
        words.word(0)?;
        words.u32()?;
        words.u32()?;
    } else {
        chain.is_empty().then_some(())?;
    }
    words.word(0)?;
    let depth_clamp = words.boolean()?;
    words.word(0)?;
    words.word(0)?;
    let cull = match words.u32()? {
        0 => CullMode::None,
        1 => CullMode::Front,
        2 => CullMode::Back,
        3 => CullMode::Both,
        _ => return None,
    };
    let front_face = match words.u32()? {
        0 => FrontFace::CounterClockwise,
        1 => FrontFace::Clockwise,
        _ => return None,
    };
    let biased = words.boolean()?;
    let (constant, clamp, slope) = (words.f32()?, words.f32()?, words.f32()?);
    Some(Rasterization {
        cull,
        front_face,
        depth_bias: biased.then_some(DepthBias {
            constant: f64::from(constant),
            slope: f64::from(slope),
            clamp: f64::from(clamp),
        }),
        depth_clamp,
        line_width: words.f32()?,
    })
}

fn face(words: &mut Words<'_>) -> Option<StencilFaceState> {
    let (fail, pass, depth_fail) = (
        stencil_op(words.u32()?)?,
        stencil_op(words.u32()?)?,
        stencil_op(words.u32()?)?,
    );
    let compare = compare(words.u32()?)?;
    let (compare_mask, write_mask) = (words.u32()?, words.u32()?);
    // Each draw's, set as it draws.
    words.u32()?;
    Some(StencilFaceState {
        fail,
        pass,
        depth_fail,
        compare,
        compare_mask: u8::try_from(compare_mask).ok()?,
        write_mask: u8::try_from(write_mask).ok()?,
    })
}

const fn topology(host: u32) -> Option<Topology> {
    Some(match host {
        0 => Topology::Points,
        1 => Topology::Lines,
        2 => Topology::LineStrip,
        3 => Topology::Triangles,
        4 => Topology::TriangleStrip,
        5 => Topology::TriangleFan,
        _ => return None,
    })
}

const fn compare(host: u32) -> Option<CompareOp> {
    Some(match host {
        0 => CompareOp::Never,
        1 => CompareOp::Less,
        2 => CompareOp::Equal,
        3 => CompareOp::LessOrEqual,
        4 => CompareOp::Greater,
        5 => CompareOp::NotEqual,
        6 => CompareOp::GreaterOrEqual,
        7 => CompareOp::Always,
        _ => return None,
    })
}

const fn stencil_op(host: u32) -> Option<StencilOp> {
    Some(match host {
        0 => StencilOp::Keep,
        1 => StencilOp::Zero,
        2 => StencilOp::Replace,
        3 => StencilOp::IncrementAndClamp,
        4 => StencilOp::DecrementAndClamp,
        5 => StencilOp::Invert,
        6 => StencilOp::IncrementAndWrap,
        7 => StencilOp::DecrementAndWrap,
        _ => return None,
    })
}

const fn factor(host: u32) -> Option<BlendFactor> {
    Some(match host {
        0 => BlendFactor::Zero,
        1 => BlendFactor::One,
        2 => BlendFactor::SourceColor,
        3 => BlendFactor::OneMinusSourceColor,
        4 => BlendFactor::DestinationColor,
        5 => BlendFactor::OneMinusDestinationColor,
        6 => BlendFactor::SourceAlpha,
        7 => BlendFactor::OneMinusSourceAlpha,
        8 => BlendFactor::DestinationAlpha,
        9 => BlendFactor::OneMinusDestinationAlpha,
        10 => BlendFactor::ConstantColor,
        11 => BlendFactor::OneMinusConstantColor,
        12 => BlendFactor::ConstantAlpha,
        13 => BlendFactor::OneMinusConstantAlpha,
        14 => BlendFactor::SourceAlphaSaturate,
        15 => BlendFactor::Source1Color,
        16 => BlendFactor::OneMinusSource1Color,
        17 => BlendFactor::Source1Alpha,
        18 => BlendFactor::OneMinusSource1Alpha,
        _ => return None,
    })
}

const fn blend_op(host: u32) -> Option<BlendOp> {
    Some(match host {
        0 => BlendOp::Add,
        1 => BlendOp::Subtract,
        2 => BlendOp::ReverseSubtract,
        3 => BlendOp::Min,
        4 => BlendOp::Max,
        _ => return None,
    })
}

const fn logic_op(host: u32) -> Option<LogicOp> {
    Some(match host {
        0 => LogicOp::Clear,
        1 => LogicOp::And,
        2 => LogicOp::AndReverse,
        3 => LogicOp::Copy,
        4 => LogicOp::AndInverted,
        5 => LogicOp::NoOp,
        6 => LogicOp::Xor,
        7 => LogicOp::Or,
        8 => LogicOp::Nor,
        9 => LogicOp::Equivalent,
        10 => LogicOp::Invert,
        11 => LogicOp::OrReverse,
        12 => LogicOp::CopyInverted,
        13 => LogicOp::OrInverted,
        14 => LogicOp::Nand,
        15 => LogicOp::Set,
        _ => return None,
    })
}

/// The rendering instance the stream names after its command buffer.
pub(super) fn decode_rendering(words: &mut Words<'_>) -> Option<Rendering> {
    words.is(1)?;
    words.structure(Structure::RenderingInfo)?;
    words.word(0)?;
    let (x, y) = (words.i32()?, words.i32()?);
    let (width, height) = (words.u32()?, words.u32()?);
    words.word(1)?;
    words.word(0)?;
    let colour = counted(words, attachment)?;
    let mut one = || match words.u64()? {
        0 => Some(None),
        1 => attachment(words).map(Some),
        _ => None,
    };
    let depth = one()?;
    let stencil = one()?;
    Some(Rendering {
        area: Rect {
            x: u32::try_from(x).ok()?,
            y: u32::try_from(y).ok()?,
            width,
            height,
        },
        colour,
        depth,
        stencil,
    })
}

fn attachment(words: &mut Words<'_>) -> Option<Attachment> {
    words.structure(Structure::RenderingAttachmentInfo)?;
    let view = words.u64()?;
    (words.i32()? == image::GENERAL).then_some(())?;
    let mode = words.u32()?;
    let (into, into_layout) = (words.u64()?, words.i32()?);
    let resolve = match mode {
        0 => (into == 0).then_some(None)?,
        2 => (into_layout == image::GENERAL).then_some(Some(into))?,
        _ => return None,
    };
    let load = words.i32()?;
    words.word(0)?;
    let value = match words.u32()? {
        0 => {
            let kind = words.u32()?;
            words.is(4)?;
            Clear::Colour(
                kind,
                [words.u32()?, words.u32()?, words.u32()?, words.u32()?],
            )
        }
        1 => Clear::DepthStencil(words.f32()?, words.u32()?),
        _ => return None,
    };
    let clear = match load {
        0 => None,
        1 => Some(value),
        _ => return None,
    };
    Some(Attachment {
        view,
        resolve,
        clear,
    })
}

/// The render pipeline `id` names among `objects`.
pub(super) fn graphics_of(objects: &BTreeMap<u64, Object>, id: u64) -> Option<&Graphics> {
    match objects.get(&id)? {
        Object::Graphics(graphics) => Some(graphics),
        _ => None,
    }
}

/// The host format view `id` reads its image as.
fn view_format(objects: &BTreeMap<u64, Object>, id: u64) -> Option<i32> {
    match objects.get(&id)? {
        Object::View(view) => Some(view.format),
        _ => None,
    }
}

impl PassRun {
    /// Note a draw of `record` run with `draw`, refused where it reads state
    /// unset, renders outside the instance's area or past the pixels its
    /// viewport covers — where a host's points and lines would reach and the
    /// software device's do not — or names a pipeline rendering to other
    /// formats than the instance's attachments are.
    pub(super) fn draw(
        &mut self,
        state: &HostState,
        context: Context,
        (draw, record): (&DrawState, DrawRecord),
    ) -> Option<()> {
        let objects = &state.contexts.get(context)?.objects;
        let graphics = graphics_of(objects, draw.pipeline?)?;
        let rendering = &self.rendering;
        let colour = rendering
            .colour
            .iter()
            .map(|attachment| view_format(objects, attachment.view))
            .collect::<Option<Vec<_>>>()?;
        let format_of = |attachment: Option<Attachment>| {
            attachment.map_or(Some(0), |attachment| view_format(objects, attachment.view))
        };
        let formats_agree = graphics.formats.0 == colour
            && graphics.formats.1 == format_of(rendering.depth)?
            && graphics.formats.2 == format_of(rendering.stencil)?;
        let area = rendering.area;
        let [x, y, width, height] = draw.scissor?;
        let within = x >= i64::from(area.x)
            && y >= i64::from(area.y)
            && x + width <= i64::from(area.x) + i64::from(area.width)
            && y + height <= i64::from(area.y) + i64::from(area.height);
        let held_to_viewport = width == 0
            || height == 0
            || draw.viewport.is_some_and(|[px, py, pw, ph, _, _]| {
                let (top, tall) = if ph < 0.0 { (py + ph, -ph) } else { (py, ph) };
                let pixel = |at: i64| i32::try_from(at).map_or(f64::INFINITY, f64::from);
                pixel(x) >= f64::from(px).floor()
                    && pixel(x + width) <= (f64::from(px) + f64::from(pw)).ceil()
                    && pixel(y) >= f64::from(top).floor()
                    && pixel(y + height) <= (f64::from(top) + f64::from(tall)).ceil()
            });
        let indexed = match record {
            DrawRecord::Vertices(_) | DrawRecord::Indirect(false, ..) => false,
            DrawRecord::Indexed(..) | DrawRecord::Indirect(true, ..) => true,
        };
        let set = draw.viewport.is_some()
            && draw.reference.is_some()
            && draw.constants.is_some()
            && draw.vertex_buffers.len() >= graphics.buffers.len()
            && draw.sets.len() == graphics.sets.len()
            && (!indexed || draw.index.is_some());
        (formats_agree && within && held_to_viewport && set).then_some(())?;
        self.draws.push((draw.clone(), record));
        Some(())
    }
}

/// Copy each image `uploaded` mirrors that holds texels of the host's
/// between its staging buffer and the image, `direction` the way.
fn copy_mirrored(
    commands: &mut tairix_gpu::CommandBuffer,
    uploaded: &Uploaded,
    direction: Direction,
) -> Option<()> {
    for (_, mirrored) in &uploaded.images {
        if mirrored.regions.is_empty() {
            continue;
        }
        let (_, staging) = &mirrored.staging;
        match direction {
            Direction::ToImage => {
                commands.copy_buffer_to_image(staging, &mirrored.image, &mirrored.regions)
            }
            Direction::ToBuffer => {
                commands.copy_image_to_buffer(&mirrored.image, staging, &mirrored.regions)
            }
        }
        .ok()?;
    }
    Some(())
}

/// The reference's attachments of `rendering`, each the view `uploaded`
/// mirrors, loaded as recorded.
fn pass_attachments<'u>(
    objects: &BTreeMap<u64, Object>,
    uploaded: &'u Uploaded,
    rendering: &Rendering,
) -> Option<(
    Vec<ColourAttachment<'u>>,
    Option<DepthStencilAttachment<'u>>,
)> {
    let view = |id: u64| held(&uploaded.views, id);
    let mut colour = Vec::new();
    for attachment in &rendering.colour {
        let format = seam_format(view_format(objects, attachment.view)?)?;
        colour.push(ColourAttachment {
            view: view(attachment.view)?,
            resolve: match attachment.resolve {
                Some(resolved) => Some(view(resolved)?),
                None => None,
            },
            load: load(attachment.clear, format)?,
        });
    }
    let depth_stencil = match rendering.depth.or(rendering.stencil) {
        Some(either) => {
            let format = seam_format(view_format(objects, either.view)?)?;
            let loaded = |attachment: Option<Attachment>| {
                load(attachment.and_then(|attachment| attachment.clear), format)
            };
            Some(DepthStencilAttachment {
                view: view(either.view)?,
                depth_load: loaded(rendering.depth)?,
                stencil_load: loaded(rendering.stencil)?,
            })
        }
        None => None,
    };
    Some((colour, depth_stencil))
}

/// The reference's pipeline of `graphics`, laid out as `layouts` and its
/// push constants, its modules its own.
fn reference_pipeline(
    gpu: &tairix_gpu::Context,
    graphics: &Graphics,
    layouts: &[tairix_gpu::BindSetLayout],
) -> Option<tairix_gpu::RenderPipeline> {
    let layout_refs: Vec<&tairix_gpu::BindSetLayout> = layouts.iter().collect();
    let (push_stages, push_bytes) = graphics.push;
    let layout = gpu
        .create_pipeline_layout(
            &layout_refs,
            PushConstants {
                bytes: push_bytes,
                stages: stages_of(push_stages)?,
            },
        )
        .ok()?;
    let shaders = graphics
        .stages
        .iter()
        .map(|(module, _, _)| {
            let own = Admitted::new(Module::decode(module.module().bytes()).ok()?).ok()?;
            gpu.create_shader_module(own).ok()
        })
        .collect::<Option<Vec<_>>>()?;
    let described = |at: usize| {
        Some(StageDesc {
            module: shaders.get(at)?,
            entry_point: graphics.stages.get(at)?.1.as_str(),
            specialization: &[],
        })
    };
    let buffers: Vec<VertexBuffer<'_>> = graphics
        .buffers
        .iter()
        .map(|(stride, step, attributes)| VertexBuffer {
            stride: *stride,
            step: *step,
            attributes,
        })
        .collect();
    gpu.create_render_pipeline(&RenderPipelineDesc {
        layout: &layout,
        vertex: described(0)?,
        fragment: described(1),
        vertex_buffers: &buffers,
        topology: graphics.topology,
        primitive_restart: graphics.restart,
        rasterization: graphics.rasterization,
        depth_stencil: graphics.depth_stencil,
        multisample: graphics.multisample,
        targets: &graphics.targets,
        logic_op: graphics.logic_op,
    })
    .ok()
}

/// What the reference draws each of a run's draws with: each pipeline made
/// once, and each draw's sets, vertex buffers and indices.
struct Prepared {
    pipelines: Vec<(u64, tairix_gpu::RenderPipeline)>,
    sets: Vec<Vec<tairix_gpu::BindSet>>,
    vertex_buffers: Vec<Vec<(tairix_gpu::Buffer, u64)>>,
    indices: Vec<Option<((tairix_gpu::Buffer, u64), IndexFormat)>>,
}

impl Prepared {
    fn new(
        state: &HostState,
        context: Context,
        (gpu, info): (&tairix_gpu::Context, &tairix_gpu::DeviceInfo),
        (uploaded, run, ranges): (&Uploaded, &PassRun, &[Vec<Range>]),
    ) -> Option<Self> {
        let objects = &state.contexts.get(context)?.objects;
        let alignment = info.limits.buffer_alignment;
        let bound_range = |[buffer, offset, size]: [u64; 3]| {
            let (memory, _) = Host::bound(state, context, buffer)?;
            let allocation = uploaded
                .memories
                .iter()
                .find(|(held, _, _)| *held == memory)
                .map(|(_, allocation, _)| allocation)?;
            range_buffer(gpu, allocation, [offset, size], alignment)
        };
        let mut prepared = Self {
            pipelines: Vec::new(),
            sets: Vec::new(),
            vertex_buffers: Vec::new(),
            indices: Vec::new(),
        };
        for ((draw, _), reached) in run.draws.iter().zip(ranges) {
            let id = draw.pipeline?;
            let graphics = graphics_of(objects, id)?;
            let entries: Vec<(&Admitted, &str, Stage)> = graphics
                .stages
                .iter()
                .map(|(module, entry, stage)| (&**module, entry.as_str(), *stage))
                .collect();
            let (layouts, bound) = bind(
                state,
                context,
                (gpu, info, (&graphics.sets, &entries)),
                reached,
                uploaded,
            )?;
            if held(&prepared.pipelines, id).is_none() {
                let made = reference_pipeline(gpu, graphics, &layouts)?;
                prepared.pipelines.push((id, made));
            }
            prepared.sets.push(bound);
            let vertices = draw
                .vertex_buffers
                .iter()
                .map(|&bound| bound_range(bound))
                .collect::<Option<Vec<_>>>()?;
            prepared.vertex_buffers.push(vertices);
            let index = match draw.index {
                Some((bound, wide)) => {
                    let format = if wide {
                        IndexFormat::U32
                    } else {
                        IndexFormat::U16
                    };
                    Some((bound_range(bound)?, format))
                }
                None => None,
            };
            prepared.indices.push(index);
        }
        Some(prepared)
    }

    /// Record each of `run`'s draws into `drawer` as recorded, its records
    /// read from what `uploaded` holds.
    fn draw(
        &self,
        drawer: &mut tairix_gpu::Drawer<'_>,
        run: &PassRun,
        uploaded: &Uploaded,
    ) -> Result<(), tairix_gpu::Error> {
        let past = |_| tairix_gpu::Error::OutOfRange;
        for (at, (draw, record)) in run.draws.iter().enumerate() {
            let pipeline = draw
                .pipeline
                .and_then(|id| held(&self.pipelines, id))
                .ok_or(tairix_gpu::Error::Stale)?;
            let [x, y, width, height] = draw.scissor.ok_or(tairix_gpu::Error::Stale)?;
            let [px, py, pw, ph, near, far] = draw.viewport.ok_or(tairix_gpu::Error::Stale)?;
            let sets: Vec<&tairix_gpu::BindSet> = self
                .sets
                .get(at)
                .ok_or(tairix_gpu::Error::Stale)?
                .iter()
                .collect();
            let vertex_buffers: Vec<(&tairix_gpu::Buffer, u64)> = self
                .vertex_buffers
                .get(at)
                .ok_or(tairix_gpu::Error::Stale)?
                .iter()
                .map(|(buffer, offset)| (buffer, *offset))
                .collect();
            let call = Draw {
                pipeline,
                sets: &sets,
                push_constants: &draw.push,
                vertex_buffers: &vertex_buffers,
                viewport: Viewport {
                    x: px,
                    y: py,
                    width: pw,
                    height: ph,
                    min_depth: near,
                    max_depth: far,
                },
                scissor: Rect {
                    x: u32::try_from(x).map_err(past)?,
                    y: u32::try_from(y).map_err(past)?,
                    width: u32::try_from(width).map_err(past)?,
                    height: u32::try_from(height).map_err(past)?,
                },
                stencil_reference: u8::try_from(draw.reference.unwrap_or(0)).map_err(past)?,
                blend_constant: draw.constants.unwrap_or_default(),
            };
            let indices =
                self.indices
                    .get(at)
                    .and_then(Option::as_ref)
                    .map(|((buffer, offset), format)| Indices {
                        buffer,
                        offset: *offset,
                        format: *format,
                    });
            record_draw(drawer, &call, (*record, indices), |buffer| {
                held(&uploaded.buffers, buffer)
            })?;
        }
        Ok(())
    }
}

/// Record `call` into `drawer` as `record` counts it, its indices
/// `indices`, its records' buffer found by `records`.
fn record_draw<'b>(
    drawer: &mut tairix_gpu::Drawer<'_>,
    call: &Draw<'_>,
    (record, indices): (DrawRecord, Option<Indices<'_>>),
    records: impl Fn(u64) -> Option<&'b tairix_gpu::Buffer>,
) -> Result<(), tairix_gpu::Error> {
    let span = |first: u32, count: u32| {
        first
            .checked_add(count)
            .map(|end| first..end)
            .ok_or(tairix_gpu::Error::OutOfRange)
    };
    let indexed = || indices.ok_or(tairix_gpu::Error::Stale);
    match record {
        DrawRecord::Vertices([count, instances, first, first_instance]) => {
            drawer.draw(call, span(first, count)?, span(first_instance, instances)?)
        }
        DrawRecord::Indexed([count, instances, first], base, first_instance) => drawer
            .draw_indexed(
                call,
                &indexed()?,
                (span(first, count)?, base),
                span(first_instance, instances)?,
            ),
        DrawRecord::Indirect(is_indexed, buffer, offset, count, stride) => {
            let read = Indirect {
                buffer: records(buffer).ok_or(tairix_gpu::Error::Stale)?,
                offset,
                count,
                stride,
            };
            if is_indexed {
                drawer.draw_indexed_indirect(call, &indexed()?, &read)
            } else {
                drawer.draw_indirect(call, &read)
            }
        }
    }
}

/// A clear `clear` of a view of host format `format` as the seam loads it.
fn load(clear: Option<Clear>, format: Format) -> Option<Load> {
    Some(match clear {
        None => Load::Keep,
        Some(Clear::Colour(kind, bits)) => Load::Clear(match (kind, format.component()) {
            (0, tairix_spirv::Component::Float) => ClearValue::Float(bits.map(f32::from_bits)),
            (1, tairix_spirv::Component::Sint) => ClearValue::Sint(bits.map(u32::cast_signed)),
            (2, tairix_spirv::Component::Uint) => ClearValue::Uint(bits),
            _ => return None,
        }),
        Some(Clear::DepthStencil(depth, stencil)) => Load::Clear(ClearValue::DepthStencil {
            depth,
            stencil: u8::try_from(stencil).ok()?,
        }),
    })
}

/// A buffer of the reference over bytes `offset..offset + size` of the
/// memory `allocation` holds, and where those bytes start in it: its start
/// is the reference's alignment's, so it may start before them.
fn range_buffer(
    gpu: &tairix_gpu::Context,
    allocation: &tairix_gpu::Allocation,
    [offset, size]: [u64; 2],
    alignment: u64,
) -> Option<(tairix_gpu::Buffer, u64)> {
    if size == 0 {
        // An empty range binds nothing: a granule bound at its end.
        let buffer = gpu
            .create_buffer(
                allocation,
                &BufferDesc {
                    offset: 0,
                    size: alignment,
                    usage: BufferUsage::ALL,
                },
            )
            .ok()?;
        return Some((buffer, alignment));
    }
    let start = offset - offset % alignment;
    let buffer = gpu
        .create_buffer(
            allocation,
            &BufferDesc {
                offset: start,
                size: offset.checked_add(size)? - start,
                usage: BufferUsage::ALL,
            },
        )
        .ok()?;
    Some((buffer, offset - start))
}

impl Host {
    /// Run `run`'s draws on the reference into its attachments, through what
    /// their sets, vertices, indices and records reach, and write what they
    /// leave back into this host's memory.
    pub(super) fn render(
        &self,
        state: &mut HostState,
        context: Context,
        run: &PassRun,
    ) -> Option<()> {
        let gpu = self
            .reference
            .create_context(&ContextDesc::default())
            .ok()?;
        let mut uploaded = Uploaded::default();
        self.mirror_attachments(state, context, (&gpu, &mut uploaded), &run.rendering)?;
        if self.names_first_instance(state, context, run)? {
            state.reached = state.reached.with(Unsent::FIRST_INSTANCED);
            return Some(());
        }
        let ranges = self.upload_draws(state, context, (&gpu, &mut uploaded), run)?;
        let info = self.reference.info();
        let prepared = Prepared::new(state, context, (&gpu, info), (&uploaded, run, &ranges))?;
        let objects = &state.contexts.get(context)?.objects;
        let (colour, depth_stencil) = pass_attachments(objects, &uploaded, &run.rendering)?;
        let mut commands = gpu.command_buffer();
        copy_mirrored(&mut commands, &uploaded, Direction::ToImage)?;
        commands
            .render_pass(
                &Pass {
                    colour: &colour,
                    depth_stencil,
                    area: run.rendering.area,
                },
                |drawer| prepared.draw(drawer, run, &uploaded),
            )
            .ok()?;
        copy_mirrored(&mut commands, &uploaded, Direction::ToBuffer)?;
        host_images::run(&gpu, &commands)?;
        self.write_back(state, context, &uploaded)?;
        state.rendered += 1;
        Some(())
    }

    /// Mirror each of `rendering`'s attachments and resolves on the
    /// reference, refused where a multisampled one is loaded: no sample of
    /// one outlives the instance drawing it.
    fn mirror_attachments(
        &self,
        state: &HostState,
        context: Context,
        (gpu, uploaded): (&tairix_gpu::Context, &mut Uploaded),
        rendering: &Rendering,
    ) -> Option<()> {
        for attachment in rendering
            .colour
            .iter()
            .chain(&rendering.depth)
            .chain(&rendering.stencil)
        {
            for view in core::iter::once(attachment.view).chain(attachment.resolve) {
                self.mirror_view(state, context, gpu, uploaded, view)?;
            }
            let objects = &state.contexts.get(context)?.objects;
            let Some(Object::View(view)) = objects.get(&attachment.view) else {
                return None;
            };
            let Some(Object::Image(image)) = objects.get(&view.image) else {
                return None;
            };
            (image.samples == 1 || attachment.clear.is_some()).then_some(())?;
        }
        Some(())
    }

    /// Whether, on a host lacking first instances, a record of `run`'s
    /// indirect draws names one, which Vulkan leaves undefined there.
    fn names_first_instance(
        &self,
        state: &HostState,
        context: Context,
        run: &PassRun,
    ) -> Option<bool> {
        if !state.offered.lacks.contains(&DRAW_INDIRECT_FIRST_INSTANCE) {
            return Some(false);
        }
        for (_, record) in &run.draws {
            let DrawRecord::Indirect(indexed, buffer, offset, count, stride) = *record else {
                continue;
            };
            let (memory, _) = Self::bound(state, context, buffer)?;
            let last = if indexed { 16 } else { 12 };
            for at in 0..u64::from(count) {
                let mut word = [0u8; 4];
                let read = offset + at * u64::from(stride) + last;
                self.read_memory(state, context, memory, read, &mut word);
                if u32::from_le_bytes(word) != 0 {
                    return Some(true);
                }
            }
        }
        Some(false)
    }

    /// Mirror on the reference everything `run`'s draws read: what their
    /// sets reach, their vertex and index buffers and their records; the
    /// ranges each draw's sets reach, in order.
    fn upload_draws(
        &self,
        state: &HostState,
        context: Context,
        (gpu, uploaded): (&tairix_gpu::Context, &mut Uploaded),
        run: &PassRun,
    ) -> Option<Vec<Vec<Range>>> {
        let mut ranges = Vec::new();
        for (draw, record) in &run.draws {
            let objects = &state.contexts.get(context)?.objects;
            let graphics = graphics_of(objects, draw.pipeline?)?;
            let reached = Self::ranges(state, context, &graphics.sets, &draw.sets)?;
            self.upload(state, context, gpu, (&reached, uploaded))?;
            let indirect = match record {
                DrawRecord::Indirect(_, buffer, ..) => Some(*buffer),
                _ => None,
            };
            for buffer in draw
                .vertex_buffers
                .iter()
                .map(|bound| bound[0])
                .chain(draw.index.map(|(bound, _)| bound[0]))
                .chain(indirect)
            {
                self.upload_buffer(state, context, gpu, uploaded, buffer)?;
            }
            ranges.push(reached);
        }
        Some(ranges)
    }

    /// Write what `uploaded`'s memories and mirrored images hold back into
    /// this host's memory, the images after the memories as an image may lie
    /// in one a buffer spans.
    fn write_back(
        &self,
        state: &mut HostState,
        context: Context,
        uploaded: &Uploaded,
    ) -> Option<()> {
        for (memory, allocation, len) in &uploaded.memories {
            let mut bytes = alloc::vec![0; *len];
            allocation.read(0, &mut bytes).ok()?;
            self.write_memory(state, context, *memory, 0, &bytes);
        }
        for (_, mirrored) in &uploaded.images {
            let (staging, _) = &mirrored.staging;
            let mut texels = alloc::vec![0; mirrored.len];
            staging.read(0, &mut texels).ok()?;
            let (memory, offset) = mirrored.place;
            self.write_memory(state, context, memory, offset, &texels);
        }
        Some(())
    }

    /// The reference's view of host view `id` in `uploaded`, and its image.
    fn mirror_view(
        &self,
        state: &HostState,
        context: Context,
        gpu: &tairix_gpu::Context,
        uploaded: &mut Uploaded,
        id: u64,
    ) -> Option<()> {
        if held(&uploaded.views, id).is_some() {
            return Some(());
        }
        let Some(&Object::View(view)) = state.contexts.get(context)?.objects.get(&id) else {
            return None;
        };
        if held(&uploaded.images, view.image).is_none() {
            let mirrored = self.mirror(state, context, gpu, view.image)?;
            uploaded.images.push((view.image, mirrored));
        }
        let image = &held(&uploaded.images, view.image)?.image;
        let made = Self::mirrored_view(gpu, &view, image)?;
        uploaded.views.push((id, made));
        Some(())
    }
}
