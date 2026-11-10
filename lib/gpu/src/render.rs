//! Render pipelines: a vertex entry point and an optional fragment one, each
//! lowered once when made, and the fixed-function state a draw runs them
//! through — every part checked against what the device states, and the
//! tables a draw reads made here, once.

use alloc::sync::Arc;
use alloc::vec::Vec;

use tairix_spirv::{Access, EntryPoint, Interpolation, SpecValue, Stage, Varying};

use crate::binding::{PipelineLayout, ShaderModule};
use crate::device::{owned, Context};
use crate::format::{Aspects, Format, FormatFeatures};
use crate::info::{DeviceInfo, Features};
use crate::model::{shared_bytes, ContextKey, ContextState, Model, Pipelines, RenderPipelineKey};
use crate::pipeline::{lower_stage, PipelineState, Released};
use crate::software::dispatch::{self, Need};
use crate::software::shader::ir::{Carries, Fixed, Program, Term};
use crate::{CompareOp, Error, MAX_PASS_ATTACHMENTS};

pub use tairix_raster::{Blend, BlendFactor, BlendOp, DepthBias, LogicOp, StencilOp};

/// The most vertex attributes a device may state a render pipeline reads:
/// past every device's.
pub const MAX_VERTEX_ATTRIBUTES: u32 = 64;

/// One stage of a render pipeline: an entry point of a module, and the
/// values its specialization constants take.
#[derive(Copy, Clone, Debug)]
pub struct StageDesc<'a> {
    /// The module holding the entry point.
    pub module: &'a ShaderModule,
    /// The entry point's name: an entry point of the stage it is made for.
    pub entry_point: &'a str,
    /// The values its specialization constants take, by `SpecId`.
    pub specialization: &'a [SpecValue],
}

/// What steps a vertex buffer from one element to the next.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum VertexStep {
    /// Each vertex.
    Vertex,
    /// Each instance.
    Instance,
}

/// An attribute a vertex buffer's elements hold.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct VertexAttribute {
    /// The vertex input location it feeds.
    pub location: u32,
    /// Its format: one the device states as a vertex input.
    pub format: Format,
    /// The byte it starts at within each element.
    pub offset: u32,
}

/// A vertex buffer a draw binds: its stride, what steps it, and the
/// attributes its elements hold.
#[derive(Copy, Clone, Debug)]
pub struct VertexBuffer<'a> {
    /// The bytes from one element to the next.
    pub stride: u32,
    /// What steps it.
    pub step: VertexStep,
    /// The attributes each element holds.
    pub attributes: &'a [VertexAttribute],
}

/// What a draw's vertices make.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Topology {
    /// A point of each vertex.
    Points,
    /// A line of each two.
    Lines,
    /// A line from each vertex to the next.
    LineStrip,
    /// A triangle of each three.
    Triangles,
    /// A triangle of each vertex and the two before it.
    TriangleStrip,
    /// A triangle of the first vertex, and each vertex and the one before it.
    TriangleFan,
}

impl Topology {
    /// Whether an index of all ones starts it afresh where restart is set.
    const fn restarts(self) -> bool {
        matches!(
            self,
            Self::LineStrip | Self::TriangleStrip | Self::TriangleFan
        )
    }
}

/// Which faces a draw discards.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum CullMode {
    /// Neither.
    None,
    /// The front.
    Front,
    /// The back.
    Back,
    /// Both: only points and lines are drawn.
    Both,
}

/// Which winding on screen faces the front.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum FrontFace {
    /// Counter-clockwise, as the framebuffer's y axis points down.
    CounterClockwise,
    /// Clockwise.
    Clockwise,
}

/// How primitives are rasterised.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Rasterization {
    /// The faces discarded.
    pub cull: CullMode,
    /// The winding that faces the front.
    pub front_face: FrontFace,
    /// The offset each triangle's depth takes.
    pub depth_bias: Option<DepthBias>,
    /// Whether depth is clamped to the viewport's range rather than clipped:
    /// a feature.
    pub depth_clamp: bool,
    /// A line's width in pixels: one, or (a feature) up to the device's.
    pub line_width: f32,
}

/// What a face's stencil test and writes do; each draw names the reference.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct StencilFaceState {
    /// Where the stencil test fails.
    pub fail: StencilOp,
    /// Where the stencil and depth tests pass.
    pub pass: StencilOp,
    /// Where the stencil test passes and the depth test fails.
    pub depth_fail: StencilOp,
    /// How the reference compares with the value held.
    pub compare: CompareOp,
    /// The bits the comparison reads.
    pub compare_mask: u8,
    /// The bits an operation may change.
    pub write_mask: u8,
}

/// The depth-stencil attachment a pipeline renders with, and how its draws
/// test and write it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct DepthStencilState {
    /// The attachment's format.
    pub format: Format,
    /// How a fragment's depth compares with the depth held: `Always` tests
    /// nothing.
    pub depth_compare: CompareOp,
    /// Whether a passing fragment writes its depth.
    pub depth_write: bool,
    /// The stencil test of front and back faces, where there is one.
    pub stencil: Option<[StencilFaceState; 2]>,
}

/// How many samples a pipeline renders with and which its draws may cover.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Multisample {
    /// One or four.
    pub samples: u32,
    /// The samples a draw may cover, bit by bit.
    pub mask: u32,
    /// Whether the first target's alpha narrows a fragment's coverage.
    pub alpha_to_coverage: bool,
}

tairix_util::flags! {
    /// The channels a colour target is written in.
    pub struct ColourMask(u8) {
        /// Red.
        const RED = 1;
        /// Green.
        const GREEN = 1 << 1;
        /// Blue.
        const BLUE = 1 << 2;
        /// Alpha.
        const ALPHA = 1 << 3;
    }
}

impl ColourMask {
    /// Every channel.
    pub const ALL: Self = Self::RED
        .with(Self::GREEN)
        .with(Self::BLUE)
        .with(Self::ALPHA);
}

/// A colour target: its format, how a fragment blends into it, and the
/// channels written.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ColourTarget {
    /// The attachment's format.
    pub format: Format,
    /// How a fragment blends with what is held; `None` replaces it.
    pub blend: Option<Blend>,
    /// The channels written.
    pub write_mask: ColourMask,
}

/// How a render pipeline is made.
#[derive(Copy, Clone, Debug)]
pub struct RenderPipelineDesc<'a> {
    /// The layout its stages' bind sets and push constants follow.
    pub layout: &'a PipelineLayout,
    /// The vertex stage.
    pub vertex: StageDesc<'a>,
    /// The fragment stage; none renders depth and stencil alone.
    pub fragment: Option<StageDesc<'a>>,
    /// The vertex buffers a draw binds, in order.
    pub vertex_buffers: &'a [VertexBuffer<'a>],
    /// What its draws' vertices make.
    pub topology: Topology,
    /// Whether an index of all ones restarts a strip or fan.
    pub primitive_restart: bool,
    /// How its primitives are rasterised.
    pub rasterization: Rasterization,
    /// Its depth-stencil attachment and tests, where it has one.
    pub depth_stencil: Option<DepthStencilState>,
    /// Its samples.
    pub multisample: Multisample,
    /// Its colour targets, by location.
    pub targets: &'a [ColourTarget],
    /// A logic operation in place of blending, for the targets of integer
    /// and normalised formats: a feature.
    pub logic_op: Option<LogicOp>,
}

/// Where a vertex input port's words come from: an attribute of a vertex
/// buffer, read from its `channel`, narrowed to 16 bits where the port is.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Fetch {
    pub(crate) port: u32,
    pub(crate) buffer: u32,
    pub(crate) format: Format,
    pub(crate) offset: u32,
    pub(crate) channel: u32,
    pub(crate) narrow: bool,
}

/// Where a word of a fragment input comes from — a word of the shaded
/// vertex, or nothing the vertex stage wrote — and how it varies across its
/// primitive.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Link {
    pub(crate) word: Option<u32>,
    pub(crate) interpolation: Interpolation,
    pub(crate) centroid: bool,
    /// Whether it is 16 bits wide.
    pub(crate) narrow: bool,
}

tairix_util::flags! {
    /// What a fragment stage does that decides when its tests may run.
    pub(crate) struct Effects(u8) {
        /// Its fragments are tested before it runs, whatever it does.
        const EARLY = 1;
        /// It may discard.
        const DISCARDS = 1 << 1;
        /// It writes storage, images or atomics.
        const WRITES_MEMORY = 1 << 2;
        /// It writes its own depth.
        const WRITES_DEPTH = 1 << 3;
        /// It writes its own coverage.
        const WRITES_MASK = 1 << 4;
    }
}

/// A render pipeline's fragment stage: its program, where each word of its
/// located inputs comes from, the channels it writes of each target and of
/// the second colour a dual-source blend reads, and what decides when its
/// tests may run.
#[derive(Debug)]
pub(crate) struct Shading {
    pub(crate) program: Program,
    /// One per word of its located input ports, in their order, and the
    /// first of each input port's: none for a built-in's.
    pub(crate) links: Vec<Link>,
    pub(crate) starts: Vec<u32>,
    /// Which of its output ports, by place, are 16 bits wide.
    pub(crate) narrow_outputs: u64,
    /// The channels, red at bit 0, each target is written in; a target of
    /// none is left as it was.
    pub(crate) channels: [u8; MAX_PASS_ATTACHMENTS],
    pub(crate) second_channels: u8,
    pub(crate) effects: Effects,
}

/// A render pipeline as a draw runs it.
#[derive(Debug)]
pub(crate) struct Render {
    pub(crate) vertex: Program,
    pub(crate) fragment: Option<Shading>,
    /// Each vertex buffer's stride and step.
    pub(crate) buffers: Vec<(u32, VertexStep)>,
    pub(crate) fetches: Vec<Fetch>,
    /// The words a shaded vertex holds — each output port's in turn, from
    /// its start — and where its position and point size lie among them.
    pub(crate) record: u32,
    pub(crate) starts: Vec<u32>,
    pub(crate) position: Option<u32>,
    pub(crate) point_size: Option<u32>,
    pub(crate) topology: Topology,
    pub(crate) restart: bool,
    pub(crate) rasterization: Rasterization,
    pub(crate) depth_stencil: Option<DepthStencilState>,
    pub(crate) multisample: Multisample,
    pub(crate) targets: Vec<ColourTarget>,
    pub(crate) logic_op: Option<LogicOp>,
    /// The largest side a point takes.
    pub(crate) largest_point: f64,
}

/// A render pipeline.
pub(crate) type RenderPipelineState = PipelineState<Render>;

impl Render {
    /// The workspace a draw of it shades in: either stage's tile.
    fn need(&self) -> Need {
        let vertex = dispatch::need(&self.vertex);
        self.fragment.as_ref().map_or(vertex, |shading| {
            vertex.max(dispatch::need(&shading.program))
        })
    }

    /// The host memory it keeps beside its working memory.
    fn held_bytes(&self) -> u64 {
        use tairix_util::fallible::held;
        [
            shared_bytes::<RenderPipelineState>(),
            self.vertex.held_bytes(),
            self.fragment.as_ref().map_or(0, |shading| {
                shading
                    .program
                    .held_bytes()
                    .saturating_add(held(&shading.links))
                    .saturating_add(held(&shading.starts))
            }),
            held(&self.buffers),
            held(&self.fetches),
            held(&self.targets),
            held(&self.starts),
        ]
        .into_iter()
        .fold(0, u64::saturating_add)
    }
}

impl Context {
    /// A render pipeline: its stages agreed with its layout, specialized and
    /// lowered, each stage's interface matched with the next's, and its
    /// fixed-function state checked against what the device states.
    ///
    /// # Errors
    ///
    /// [`Error::Foreign`] for a module or layout of another context,
    /// [`Error::EntryPoint`] for a name no entry point of its stage has,
    /// [`Error::Disagreement`] for a layout that does not provide what a stage
    /// uses, [`Error::Specialization`] for values a module refuses,
    /// [`Error::Interface`] for a vertex input no attribute provides or
    /// provides as another component type, a fragment input no vertex output
    /// provides, or a fragment output with no target or of another component
    /// type, [`Error::Unsupported`] for a feature or format feature the device
    /// has not stated, [`Error::Samples`] for a sample count other than one or
    /// four, [`Error::Aspect`] for depth or stencil tests of a format without
    /// the aspect, [`Error::Shape`] for a value that is not finite, a write
    /// mask past its channels, or a line narrower than a pixel,
    /// [`Error::TooMany`] and [`Error::TooLarge`] past the device's limits or
    /// the lowering budget, and [`Error::OutOfMemory`] or [`Error::HeapFull`]
    /// for working memory, host memory or table room the device cannot
    /// reserve.
    ///
    /// And those of every request that makes memory ([`Context#memory`]).
    pub fn create_render_pipeline(
        &self,
        desc: &RenderPipelineDesc<'_>,
    ) -> Result<RenderPipeline, Error> {
        for stage in core::iter::once(&desc.vertex).chain(&desc.fragment) {
            owned(
                &stage.module.shared,
                stage.module.context,
                &self.shared,
                self.key,
            )?;
        }
        owned(
            &desc.layout.shared,
            desc.layout.context,
            &self.shared,
            self.key,
        )?;
        let info = &self.shared.info;
        check_fixed(info, desc)?;
        let (vertex_module, fragment_module, layout) = {
            let model = self.shared.model.lock();
            let context = model.live(self.key)?;
            let module = |stage: &StageDesc<'_>| {
                context
                    .shader_modules
                    .get(stage.module.key)
                    .cloned()
                    .ok_or(Error::Stale)
            };
            let fragment = desc.fragment.as_ref().map(module).transpose()?;
            let layout = context
                .pipeline_layouts
                .get(desc.layout.key)
                .cloned()
                .ok_or(Error::Stale)?;
            (module(&desc.vertex)?, fragment, layout)
        };
        let limits = &info.limits;
        let vertex = lower_stage(
            limits,
            &vertex_module.admitted,
            (
                Stage::Vertex,
                desc.vertex.entry_point,
                desc.vertex.specialization,
            ),
            &layout,
        )?;
        let fragment = match (&desc.fragment, &fragment_module) {
            (Some(stage), Some(module)) => Some(lower_stage(
                limits,
                &module.admitted,
                (Stage::Fragment, stage.entry_point, stage.specialization),
                &layout,
            )?),
            _ => None,
        };
        let render = Render::of(info, desc, vertex, fragment)?;
        let (need, held) = (render.need(), render.held_bytes());
        Ok(RenderPipeline {
            shared: Arc::clone(&self.shared),
            context: self.key,
            key: self.file_pipeline(renders, (render, layout), (need, held))?,
        })
    }
}

/// Refuse vertex buffers and attributes the device has not stated, or past
/// its limits, or two attributes feeding one location.
fn check_vertex_input(info: &DeviceInfo, desc: &RenderPipelineDesc<'_>) -> Result<(), Error> {
    let limits = &info.limits;
    let stated = |format: Format, wanted: FormatFeatures| info.formats.get(format).contains(wanted);
    if desc.vertex_buffers.len() > limits.max_vertex_input_bindings as usize {
        return Err(Error::TooMany);
    }
    let (mut attributes, mut fed) = (0u32, 0u64);
    for buffer in desc.vertex_buffers {
        if buffer.stride > limits.max_vertex_input_binding_stride {
            return Err(Error::TooLarge);
        }
        for attribute in buffer.attributes {
            attributes = attributes.saturating_add(1);
            if attributes > limits.max_vertex_input_attributes {
                return Err(Error::TooMany);
            }
            if attribute.offset > limits.max_vertex_input_attribute_offset {
                return Err(Error::TooLarge);
            }
            if !stated(attribute.format, FormatFeatures::VERTEX_INPUT) {
                return Err(Error::Unsupported);
            }
            if attribute.location >= limits.max_vertex_input_attributes {
                return Err(Error::OutOfRange);
            }
            let bit = 1u64
                .checked_shl(attribute.location)
                .ok_or(Error::OutOfRange)?;
            if fed & bit != 0 {
                return Err(Error::Interface);
            }
            fed |= bit;
        }
    }
    Ok(())
}

/// Refuse fixed-function state the device has not stated, malformed, or past
/// its limits.
fn check_fixed(info: &DeviceInfo, desc: &RenderPipelineDesc<'_>) -> Result<(), Error> {
    let limits = &info.limits;
    let stated = |format: Format, wanted: FormatFeatures| info.formats.get(format).contains(wanted);
    let feature = |wanted: Features| {
        if info.features.contains(wanted) {
            Ok(())
        } else {
            Err(Error::Unsupported)
        }
    };
    check_vertex_input(info, desc)?;
    if desc.primitive_restart && !desc.topology.restarts() {
        return Err(Error::Unsupported);
    }
    let raster = &desc.rasterization;
    if !raster.line_width.is_finite() || raster.line_width < 1.0 {
        return Err(Error::Shape);
    }
    // Held to one or more above, so any other width is wider.
    if raster.line_width > 1.0 {
        feature(Features::WIDE_LINES)?;
        if f64::from(raster.line_width) > f64::from(limits.max_line_width) {
            return Err(Error::TooLarge);
        }
    }
    if raster.depth_clamp {
        feature(Features::DEPTH_CLAMP)?;
    }
    if let Some(bias) = raster.depth_bias {
        if !(bias.constant.is_finite() && bias.slope.is_finite() && bias.clamp.is_finite()) {
            return Err(Error::Shape);
        }
    }
    let multisampled = match desc.multisample.samples {
        1 => false,
        4 => true,
        _ => return Err(Error::Samples),
    };
    let renders = |format: Format, wanted: FormatFeatures| {
        let samples = if multisampled {
            FormatFeatures::MULTISAMPLED
        } else {
            FormatFeatures::NONE
        };
        stated(format, wanted.with(samples))
    };
    if desc.targets.len() > limits.max_color_attachments as usize {
        return Err(Error::TooMany);
    }
    for target in desc.targets {
        if !renders(target.format, FormatFeatures::COLOR_ATTACHMENT) {
            return Err(Error::Unsupported);
        }
        if let Some(blend) = &target.blend {
            if !stated(target.format, FormatFeatures::BLENDED) {
                return Err(Error::Unsupported);
            }
            if blend.reads_second_source() {
                feature(Features::DUAL_SOURCE_BLEND)?;
            }
        }
        if !ColourMask::ALL.contains(target.write_mask) {
            return Err(Error::Shape);
        }
    }
    if desc.logic_op.is_some() {
        feature(Features::LOGIC_OP)?;
    }
    if let Some(depth_stencil) = &desc.depth_stencil {
        if !renders(depth_stencil.format, FormatFeatures::DEPTH_STENCIL) {
            return Err(Error::Unsupported);
        }
        let aspects = depth_stencil.format.aspects();
        let tests_depth =
            depth_stencil.depth_write || depth_stencil.depth_compare != CompareOp::Always;
        if (tests_depth && !aspects.contains(Aspects::DEPTH))
            || (depth_stencil.stencil.is_some() && !aspects.contains(Aspects::STENCIL))
        {
            return Err(Error::Aspect);
        }
    }
    Ok(())
}

/// Whether `entry` writes storage, images or atomics.
fn writes_memory(entry: &EntryPoint) -> bool {
    entry
        .bindings()
        .iter()
        .any(|binding| binding.access.contains(Access::WRITE))
}

/// The 32-bit components `varyings` hold together.
fn components(varyings: &[Varying]) -> u32 {
    varyings
        .iter()
        .fold(0u32, |total, v| total.saturating_add(v.components))
}

/// The varying of `varyings` holding component `component` of `location`.
fn holding(varyings: &[Varying], location: u32, component: u32) -> Option<&Varying> {
    varyings.iter().find(|v| {
        v.location == location
            && (v.component..v.component.saturating_add(v.components)).contains(&component)
    })
}

impl Render {
    /// The tables a draw reads, made from the lowered stages and `desc`, and
    /// each stage's interface matched with the next's.
    fn of(
        info: &DeviceInfo,
        desc: &RenderPipelineDesc<'_>,
        (vertex, vertex_entry): (Program, &EntryPoint),
        fragment: Option<(Program, &EntryPoint)>,
    ) -> Result<Self, Error> {
        let limits = &info.limits;
        if writes_memory(vertex_entry) && !info.features.contains(Features::VERTEX_STORES) {
            return Err(Error::Unsupported);
        }
        let fetches = fetches(desc, (&vertex, vertex_entry))?;
        if components(vertex_entry.outputs()) > limits.max_vertex_output_components {
            return Err(Error::TooLarge);
        }
        let mut record = 0u32;
        let (mut position, mut point_size) = (None, None);
        let mut starts = Vec::new();
        starts
            .try_reserve_exact(vertex.outputs.len())
            .map_err(|_| Error::OutOfMemory)?;
        for port in &vertex.outputs {
            match port.carries {
                Carries::Fixed(Fixed::Position) => position = Some(record),
                Carries::Fixed(Fixed::PointSize) => point_size = Some(record),
                _ => {}
            }
            starts.push(record);
            record = record.checked_add(port.words).ok_or(Error::TooLarge)?;
        }
        let fragment = fragment
            .map(|(program, entry)| {
                shading(
                    info,
                    desc,
                    (&vertex, vertex_entry, &starts),
                    (program, entry),
                )
            })
            .transpose()?;
        let mut buffers = Vec::new();
        buffers
            .try_reserve_exact(desc.vertex_buffers.len())
            .map_err(|_| Error::OutOfMemory)?;
        buffers.extend(desc.vertex_buffers.iter().map(|b| (b.stride, b.step)));
        let targets =
            tairix_util::fallible::collected(desc.targets.len(), desc.targets.iter().copied())
                .ok_or(Error::OutOfMemory)?;
        let largest_point = if info.features.contains(Features::LARGE_POINTS) {
            f64::from(limits.max_point_size)
        } else {
            1.0
        };
        Ok(Self {
            vertex,
            fragment,
            buffers,
            fetches,
            record,
            starts,
            position,
            point_size,
            topology: desc.topology,
            restart: desc.primitive_restart,
            rasterization: desc.rasterization,
            depth_stencil: desc.depth_stencil,
            multisample: desc.multisample,
            targets,
            logic_op: desc.logic_op,
            largest_point,
        })
    }
}

/// Where each of `vertex`'s located input ports is fed from: an attribute
/// of one of `desc`'s vertex buffers at its location, of its component type.
fn fetches(
    desc: &RenderPipelineDesc<'_>,
    (vertex, entry): (&Program, &EntryPoint),
) -> Result<Vec<Fetch>, Error> {
    let attribute_at = |location: u32| {
        desc.vertex_buffers
            .iter()
            .zip(0u32..)
            .find_map(|(buffer, index)| {
                buffer
                    .attributes
                    .iter()
                    .find(|attribute| attribute.location == location)
                    .map(|attribute| (index, attribute))
            })
    };
    for input in entry.inputs() {
        let (_, attribute) = attribute_at(input.location).ok_or(Error::Interface)?;
        if attribute.format.component() != input.scalar || input.width > 32 {
            return Err(Error::Interface);
        }
    }
    let mut fetches = Vec::new();
    fetches
        .try_reserve_exact(vertex.inputs.len())
        .map_err(|_| Error::OutOfMemory)?;
    for (port, index) in vertex.inputs.iter().zip(0u32..) {
        let Carries::Located {
            location,
            component,
            ..
        } = port.carries
        else {
            continue;
        };
        let (buffer, attribute) = attribute_at(location).ok_or(Error::Interface)?;
        let input = holding(entry.inputs(), location, component).ok_or(Error::Malformed)?;
        fetches.push(Fetch {
            port: index,
            buffer,
            format: attribute.format,
            offset: attribute.offset,
            channel: component,
            narrow: input.width == 16,
        });
    }
    Ok(fetches)
}

/// The channels, red at bit 0, `entry`'s outputs write of each of `desc`'s
/// targets and of the second colour: each output a target's of its
/// component type, the second colour's at location 0 and read only by the
/// first target's blend.
fn written_channels(
    info: &DeviceInfo,
    desc: &RenderPipelineDesc<'_>,
    entry: &EntryPoint,
) -> Result<([u8; MAX_PASS_ATTACHMENTS], u8), Error> {
    let mut channels = [0u8; MAX_PASS_ATTACHMENTS];
    let mut second_channels = 0u8;
    for output in entry.outputs() {
        if output.index != 0 {
            if output.location != 0 {
                return Err(Error::Interface);
            }
            if !info.features.contains(Features::DUAL_SOURCE_BLEND) {
                return Err(Error::Unsupported);
            }
        }
        let target = desc
            .targets
            .get(output.location as usize)
            .ok_or(Error::Interface)?;
        if target.format.component() != output.scalar || output.width > 32 {
            return Err(Error::Interface);
        }
        let span = (1u32 << output.components.min(4)) - 1;
        let bits = u8::try_from((span << output.component.min(3)) & 0xf).unwrap_or(0);
        let written = if output.index == 0 {
            channels
                .get_mut(output.location as usize)
                .ok_or(Error::TooMany)?
        } else {
            &mut second_channels
        };
        *written |= bits;
    }
    // Only the first target's blend may read the second colour, and only
    // where the stage writes one.
    for (index, target) in desc.targets.iter().enumerate() {
        let second = target
            .blend
            .is_some_and(|blend| blend.reads_second_source());
        if second && (index != 0 || second_channels == 0) {
            return Err(Error::Interface);
        }
    }
    Ok((channels, second_channels))
}

/// The fragment stage of a pipeline whose vertex stage is `vertex`: each of
/// its inputs provided by a vertex output, each of its outputs a target's.
fn shading(
    info: &DeviceInfo,
    desc: &RenderPipelineDesc<'_>,
    (vertex, vertex_entry, starts): (&Program, &EntryPoint, &[u32]),
    (program, entry): (Program, &EntryPoint),
) -> Result<Shading, Error> {
    if components(entry.inputs()) > info.limits.max_fragment_input_components {
        return Err(Error::TooLarge);
    }
    let stores = writes_memory(entry);
    if stores && !info.features.contains(Features::FRAGMENT_STORES) {
        return Err(Error::Unsupported);
    }
    for input in entry.inputs() {
        for component in input.component..input.component.saturating_add(input.components) {
            let provided = holding(vertex_entry.outputs(), input.location, component)
                .is_some_and(|output| output.scalar == input.scalar && output.width == input.width);
            if !provided {
                return Err(Error::Interface);
            }
        }
    }
    let (channels, second_channels) = written_channels(info, desc, entry)?;
    let source = |location: u32, component: u32| {
        vertex
            .outputs
            .iter()
            .zip(starts)
            .find_map(|(port, &start)| match port.carries {
                Carries::Located {
                    location: held,
                    component: first,
                    ..
                } if held == location
                    && (first..first.saturating_add(port.words)).contains(&component) =>
                {
                    Some(start + (component - first))
                }
                _ => None,
            })
    };
    let (links, firsts) = links_of(&program, entry, source)?;
    let narrow_outputs = narrow_outputs(&program, entry)?;
    let writes = |which: Fixed| {
        program
            .outputs
            .iter()
            .any(|port| port.carries == Carries::Fixed(which))
    };
    let effects = [
        (entry.early_fragment_tests(), Effects::EARLY),
        (
            program.blocks.iter().any(|block| block.term == Term::Kill),
            Effects::DISCARDS,
        ),
        (stores, Effects::WRITES_MEMORY),
        (writes(Fixed::FragDepth), Effects::WRITES_DEPTH),
        (writes(Fixed::SampleMask), Effects::WRITES_MASK),
    ]
    .into_iter()
    .filter(|&(holds, _)| holds)
    .fold(Effects::NONE, |all, (_, effect)| all.with(effect));
    Ok(Shading {
        effects,
        links,
        starts: firsts,
        narrow_outputs,
        channels,
        second_channels,
        program,
    })
}

/// Where each word of `program`'s located inputs comes from — `source`
/// finding the vertex output word at a location's component — and the first
/// link of each input port: none for a built-in's.
fn links_of(
    program: &Program,
    entry: &EntryPoint,
    source: impl Fn(u32, u32) -> Option<u32>,
) -> Result<(Vec<Link>, Vec<u32>), Error> {
    let words = program
        .inputs
        .iter()
        .fold(0usize, |total, port| match port.carries {
            Carries::Located { .. } => total.saturating_add(port.words as usize),
            Carries::Fixed(_) => total,
        });
    let mut links = Vec::new();
    links
        .try_reserve_exact(words)
        .map_err(|_| Error::OutOfMemory)?;
    let mut firsts = Vec::new();
    firsts
        .try_reserve_exact(program.inputs.len())
        .map_err(|_| Error::OutOfMemory)?;
    for port in &program.inputs {
        let Carries::Located {
            location,
            component,
            ..
        } = port.carries
        else {
            firsts.push(u32::MAX);
            continue;
        };
        firsts.push(u32::try_from(links.len()).map_err(|_| Error::TooLarge)?);
        for word in 0..port.words {
            let component = component + word;
            let input = holding(entry.inputs(), location, component).ok_or(Error::Malformed)?;
            links.push(Link {
                word: source(location, component),
                interpolation: input.interpolation,
                centroid: input.centroid,
                narrow: input.width == 16,
            });
        }
    }
    Ok((links, firsts))
}

/// Which of `program`'s output ports, by place, are 16 bits wide.
fn narrow_outputs(program: &Program, entry: &EntryPoint) -> Result<u64, Error> {
    let mut narrow_outputs = 0u64;
    for (port, place) in program.outputs.iter().zip(0u32..) {
        if let Carries::Located {
            location,
            component,
            index,
        } = port.carries
        {
            let narrow = entry.outputs().iter().any(|v| {
                v.location == location
                    && v.component == component
                    && v.index == index
                    && v.width == 16
            });
            if narrow {
                narrow_outputs |= 1u64.checked_shl(place).ok_or(Error::TooLarge)?;
            }
        }
    }
    Ok(narrow_outputs)
}

/// The context's render pipelines.
fn renders(
    context: &mut ContextState,
) -> &mut Pipelines<RenderPipelineKey, Arc<RenderPipelineState>> {
    &mut context.render_pipelines
}

impl Model {
    pub(crate) fn destroy_render_pipeline(
        &mut self,
        key: ContextKey,
        pipeline: RenderPipelineKey,
    ) -> Released<Render> {
        self.destroy_pipeline((key, renders), pipeline)
    }
}

handle!(
    /// A vertex stage, an optional fragment stage and the fixed-function
    /// state they run through, ready to draw.
    RenderPipeline,
    RenderPipelineKey,
    destroy_render_pipeline,
    releasing
);

#[cfg(test)]
#[path = "render_tests.rs"]
pub(crate) mod tests;
