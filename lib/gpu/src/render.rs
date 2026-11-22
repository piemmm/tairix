//! Render pipelines: a vertex entry point and an optional fragment one, each
//! made once, and the fixed-function state a draw runs them through — every
//! part checked against what the device states, and each stage's interface
//! matched with the next's.

use alloc::sync::Arc;
use alloc::vec::Vec;

use tairix_spirv::{Access, EntryPoint, SpecValue, Stage, Varying};

use crate::backend::Backend;
use crate::binding::{PipelineLayout, ShaderModule};
use crate::device::{owned, Context, Shared};
use crate::format::{Aspects, Format, FormatFeatures};
use crate::info::{DeviceInfo, Features};
use crate::model::{
    shared_bytes, ContextKey, ContextState, Model, PipelineLayoutKey, Pipelines, RenderPipelineKey,
    ShaderModuleKey,
};
use crate::pipeline::{plan_stage, PipelineState, Released, StagePlan};
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

/// A render pipeline's fixed-function state, as its description gave it:
/// what a draw is checked against and runs through.
#[derive(Debug)]
pub(crate) struct FixedFunction {
    /// Each vertex buffer's stride and step.
    pub(crate) buffers: Vec<(u32, VertexStep)>,
    pub(crate) topology: Topology,
    pub(crate) restart: bool,
    pub(crate) rasterization: Rasterization,
    pub(crate) depth_stencil: Option<DepthStencilState>,
    pub(crate) multisample: Multisample,
    pub(crate) targets: Vec<ColourTarget>,
    pub(crate) logic_op: Option<LogicOp>,
}

impl FixedFunction {
    fn of(state: &FixedState<'_>) -> Result<Self, Error> {
        let buffers = tairix_util::fallible::collected(
            state.vertex_buffers.len(),
            state.vertex_buffers.iter().map(|b| (b.stride, b.step)),
        )
        .ok_or(Error::OutOfMemory)?;
        let targets =
            tairix_util::fallible::collected(state.targets.len(), state.targets.iter().copied())
                .ok_or(Error::OutOfMemory)?;
        Ok(Self {
            buffers,
            topology: state.topology,
            restart: state.primitive_restart,
            rasterization: state.rasterization,
            depth_stencil: state.depth_stencil,
            multisample: state.multisample,
            targets,
            logic_op: state.logic_op,
        })
    }

    /// The host bytes it keeps beside itself.
    fn held_bytes(&self) -> u64 {
        use tairix_util::fallible::held;
        held(&self.buffers).saturating_add(held(&self.targets))
    }
}

/// A render pipeline's stages: its fixed-function state, and its programs
/// as its backend made them.
#[derive(Debug)]
pub(crate) struct Rendered<B: Backend> {
    pub(crate) fixed: FixedFunction,
    pub(crate) form: B::Render,
}

/// A render pipeline.
pub(crate) type RenderPipelineState<B> = PipelineState<B, Rendered<B>>;

/// A render pipeline's state as a request describes it.
#[derive(Copy, Clone, Debug)]
pub(crate) struct FixedState<'a> {
    pub(crate) vertex_buffers: &'a [VertexBuffer<'a>],
    pub(crate) topology: Topology,
    pub(crate) primitive_restart: bool,
    pub(crate) rasterization: Rasterization,
    pub(crate) depth_stencil: Option<DepthStencilState>,
    pub(crate) multisample: Multisample,
    pub(crate) targets: &'a [ColourTarget],
    pub(crate) logic_op: Option<LogicOp>,
}

/// One stage of a render pipeline as a request names it.
#[derive(Copy, Clone, Debug)]
pub(crate) struct StageRequest<'a> {
    pub(crate) module: ShaderModuleKey,
    pub(crate) entry_point: &'a str,
    pub(crate) specialization: &'a [SpecValue],
}

/// A render pipeline as a request names it: its modules and layout by key,
/// and its state as described.
#[derive(Copy, Clone, Debug)]
pub(crate) struct RenderRequest<'a> {
    pub(crate) layout: PipelineLayoutKey,
    pub(crate) vertex: StageRequest<'a>,
    pub(crate) fragment: Option<StageRequest<'a>>,
    pub(crate) state: FixedState<'a>,
}

impl<'a> RenderRequest<'a> {
    fn of(desc: &RenderPipelineDesc<'a>) -> Self {
        let stage = |stage: &StageDesc<'a>| StageRequest {
            module: stage.module.key,
            entry_point: stage.entry_point,
            specialization: stage.specialization,
        };
        Self {
            layout: desc.layout.key,
            vertex: stage(&desc.vertex),
            fragment: desc.fragment.as_ref().map(stage),
            state: FixedState {
                vertex_buffers: desc.vertex_buffers,
                topology: desc.topology,
                primitive_restart: desc.primitive_restart,
                rasterization: desc.rasterization,
                depth_stencil: desc.depth_stencil,
                multisample: desc.multisample,
                targets: desc.targets,
                logic_op: desc.logic_op,
            },
        }
    }
}

/// A render pipeline's checked stages and what the model found of them:
/// what a backend makes its programs from.
pub(crate) struct RenderPlan<'m> {
    pub(crate) vertex_buffers: &'m [VertexBuffer<'m>],
    pub(crate) vertex: StagePlan<'m>,
    pub(crate) fragment: Option<StagePlan<'m>>,
    /// The channels, red at bit 0, the fragment stage writes of each target
    /// and of the second colour a dual-source blend reads.
    pub(crate) channels: [u8; MAX_PASS_ATTACHMENTS],
    pub(crate) second_channels: u8,
}

impl Context {
    /// A render pipeline: its stages agreed with its layout, specialized and
    /// made, each stage's interface matched with the next's, and its
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
        let key = self
            .shared
            .create_render_pipeline(self.key, &RenderRequest::of(desc))?;
        Ok(RenderPipeline {
            shared: Arc::clone(&self.shared),
            context: self.key,
            key,
        })
    }
}

impl<B: Backend> Shared<B> {
    /// A render pipeline of the context as `request` names it.
    pub(crate) fn create_render_pipeline(
        &self,
        context: ContextKey,
        request: &RenderRequest<'_>,
    ) -> Result<RenderPipelineKey, Error> {
        let info = &self.info;
        check_fixed(info, &request.state)?;
        let (vertex_module, fragment_module, layout) = self.modules_and_layout(
            context,
            (
                request.vertex.module,
                request.fragment.as_ref().map(|fragment| fragment.module),
            ),
            request.layout,
        )?;
        let plan_of = |module, stage: Stage, request: &StageRequest<'_>| {
            plan_stage(
                &info.limits,
                module,
                (stage, request.entry_point, request.specialization),
                &layout,
            )
        };
        let vertex = plan_of(&vertex_module.admitted, Stage::Vertex, &request.vertex)?;
        let fragment = request
            .fragment
            .as_ref()
            .zip(fragment_module.as_ref())
            .map(|(wanted, module)| plan_of(&module.admitted, Stage::Fragment, wanted))
            .transpose()?;
        let (channels, second_channels) = check_interface(
            info,
            &request.state,
            vertex.entry,
            fragment.as_ref().map(|stage| stage.entry),
        )?;
        let plan = RenderPlan {
            vertex_buffers: request.state.vertex_buffers,
            vertex,
            fragment,
            channels,
            second_channels,
        };
        #[cfg(test)]
        crate::fair::held::note_making();
        let prepared = self.backend.render(info, &plan)?;
        drop(plan);
        let fixed = FixedFunction::of(&request.state)?;
        let held = [
            shared_bytes::<RenderPipelineState<B>>(),
            prepared.held,
            fixed.held_bytes(),
        ]
        .into_iter()
        .fold(0, u64::saturating_add);
        let stages = Rendered {
            fixed,
            form: prepared.form,
        };
        self.file_pipeline(context, renders, (stages, layout), (prepared.need, held))
    }
}

/// Refuse vertex buffers and attributes the device has not stated, or past
/// its limits, or two attributes feeding one location.
fn check_vertex_input(info: &DeviceInfo, state: &FixedState<'_>) -> Result<(), Error> {
    let limits = &info.limits;
    let stated = |format: Format, wanted: FormatFeatures| info.formats.get(format).contains(wanted);
    if state.vertex_buffers.len() > limits.max_vertex_input_bindings as usize {
        return Err(Error::TooMany);
    }
    let (mut attributes, mut fed) = (0u32, 0u64);
    for buffer in state.vertex_buffers {
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
fn check_fixed(info: &DeviceInfo, state: &FixedState<'_>) -> Result<(), Error> {
    let limits = &info.limits;
    let stated = |format: Format, wanted: FormatFeatures| info.formats.get(format).contains(wanted);
    let feature = |wanted: Features| {
        if info.features.contains(wanted) {
            Ok(())
        } else {
            Err(Error::Unsupported)
        }
    };
    check_vertex_input(info, state)?;
    if state.primitive_restart && !state.topology.restarts() {
        return Err(Error::Unsupported);
    }
    let raster = &state.rasterization;
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
    let multisampled = match state.multisample.samples {
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
    if state.targets.len() > limits.max_color_attachments as usize {
        return Err(Error::TooMany);
    }
    for target in state.targets {
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
    if state.logic_op.is_some() {
        feature(Features::LOGIC_OP)?;
    }
    if let Some(depth_stencil) = &state.depth_stencil {
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
pub(crate) fn writes_memory(entry: &EntryPoint) -> bool {
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
pub(crate) fn holding(varyings: &[Varying], location: u32, component: u32) -> Option<&Varying> {
    varyings.iter().find(|v| {
        v.location == location
            && (v.component..v.component.saturating_add(v.components)).contains(&component)
    })
}

/// The channels, red at bit 0, `entry`'s outputs write of each of `state`'s
/// targets and of the second colour: each output a target's of its
/// component type, the second colour's at location 0 and read only by the
/// first target's blend.
fn written_channels(
    info: &DeviceInfo,
    state: &FixedState<'_>,
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
        let target = state
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
    for (index, target) in state.targets.iter().enumerate() {
        let second = target
            .blend
            .is_some_and(|blend| blend.reads_second_source());
        if second && (index != 0 || second_channels == 0) {
            return Err(Error::Interface);
        }
    }
    Ok((channels, second_channels))
}

/// Each stage's interface matched with its neighbours': every vertex input
/// fed by an attribute of its component type, every fragment input by a
/// vertex output of its type, every fragment output a target's, within the
/// device's limits and features; and the channels the fragment stage writes
/// of each target and of the second colour.
fn check_interface(
    info: &DeviceInfo,
    state: &FixedState<'_>,
    vertex: &EntryPoint,
    fragment: Option<&EntryPoint>,
) -> Result<([u8; MAX_PASS_ATTACHMENTS], u8), Error> {
    let limits = &info.limits;
    if writes_memory(vertex) && !info.features.contains(Features::VERTEX_STORES) {
        return Err(Error::Unsupported);
    }
    let fed = |location: u32| {
        state.vertex_buffers.iter().find_map(|buffer| {
            buffer
                .attributes
                .iter()
                .find(|attribute| attribute.location == location)
        })
    };
    for input in vertex.inputs() {
        let attribute = fed(input.location).ok_or(Error::Interface)?;
        if attribute.format.component() != input.scalar || input.width > 32 {
            return Err(Error::Interface);
        }
    }
    if components(vertex.outputs()) > limits.max_vertex_output_components {
        return Err(Error::TooLarge);
    }
    let Some(entry) = fragment else {
        return Ok(([0; MAX_PASS_ATTACHMENTS], 0));
    };
    if components(entry.inputs()) > limits.max_fragment_input_components {
        return Err(Error::TooLarge);
    }
    if writes_memory(entry) && !info.features.contains(Features::FRAGMENT_STORES) {
        return Err(Error::Unsupported);
    }
    for input in entry.inputs() {
        for component in input.component..input.component.saturating_add(input.components) {
            let provided = holding(vertex.outputs(), input.location, component)
                .is_some_and(|output| output.scalar == input.scalar && output.width == input.width);
            if !provided {
                return Err(Error::Interface);
            }
        }
    }
    written_channels(info, state, entry)
}

/// The context's render pipelines.
fn renders<B: Backend>(
    context: &mut ContextState<B>,
) -> &mut Pipelines<RenderPipelineKey, Arc<RenderPipelineState<B>>> {
    &mut context.render_pipelines
}

impl<B: Backend> Model<B> {
    pub(crate) fn destroy_render_pipeline(
        &mut self,
        key: ContextKey,
        pipeline: RenderPipelineKey,
    ) -> Released<B, Rendered<B>> {
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
