//! Render pipelines and passes in a Venus context. A render pipeline is its
//! modules, folded and guarded, its layouts and one graphics pipeline rendering without a
//! render pass object. A pass is a rendering instance in the general layout
//! every image is kept in; where a device checks its indirect draws' records
//! and they outrun the slot holding them, it is several in turn, each loading
//! what the one before stored and only the last resolving.

use alloc::sync::Arc;
use alloc::vec::Vec;

use tairix_abi::DriverError;
use tairix_gpu::{
    Aspects, Blend, BlendFactor, BlendOp, BufferRange, CullMode, DrawCommand, DrawCount,
    DrawCounts, Draws, Error, Format, FrontFace, IndexFormat, Load, LogicOp, PassDepthStencil,
    Rasterization, Rect, RenderStages, StencilFaceState, StencilOp, Targets, Topology, VertexStep,
    Viewport, DRAW_RECORD_BYTES, INDEXED_DRAW_RECORD_BYTES, MAX_DISPATCH_SETS,
    MAX_DRAW_VERTEX_BUFFERS, MAX_PASS_ATTACHMENTS, MAX_PUSH_BYTES,
};
use tairix_inline::ArrayVec;
use tairix_shaders::indirect::records;
use tairix_util::mathf;

use super::super::encode::{
    AttachmentInfo, AttributeInfo, CopyRegion, DrawInfo, Encoder, GraphicsInfo, RasterInfo,
    StageInfo, StencilInfo, TargetInfo,
};
use super::super::info::Statement;
use super::super::{stage, BIND_POINT_GRAPHICS};
use super::images::{compare, held, host_clear};
use super::{
    bind_check, device_error, dispatch_check, make_in, prepared, PipelineIds, Shape, VenusContext,
    VenusMemory, VenusPipeline, FEEDBACK, RECORDS_AT, RECORDS_CHECK, RECORDS_SLOT_BYTES,
};

/// The words of push constants a draw binds at most.
const PUSH_WORDS: usize = MAX_PUSH_BYTES / 4;

/// The host's format of `format`, as `statement` holds it.
fn host_of(statement: &Statement, format: Format) -> Result<i32, Error> {
    held(statement, format)
        .map(|held| held.format)
        .map_err(device_error)
}

/// A pipeline's vertex buffers as the host binds them, each its stride and
/// whether instances step it, and the attributes they hold.
type VertexInput = (
    ArrayVec<(u32, u32), MAX_DRAW_VERTEX_BUFFERS>,
    Vec<AttributeInfo>,
);

/// A pipeline's targets' blends and channels written, as the host names
/// them, and their formats.
type ColourTargets = (
    ArrayVec<TargetInfo, MAX_PASS_ATTACHMENTS>,
    ArrayVec<i32, MAX_PASS_ATTACHMENTS>,
);

/// The vertex input of `stages`.
fn vertex_input(statement: &Statement, stages: RenderStages<'_>) -> Result<VertexInput, Error> {
    let mut bindings = ArrayVec::new();
    let mut attributes = Vec::new();
    for (binding, buffer) in (0u32..).zip(stages.vertex_buffers()) {
        let rate = match buffer.step {
            VertexStep::Vertex => 0,
            VertexStep::Instance => 1,
        };
        bindings
            .try_push((buffer.stride, rate))
            .map_err(|_| Error::TooMany)?;
        attributes
            .try_reserve(buffer.attributes.len())
            .map_err(|_| Error::OutOfMemory)?;
        for attribute in buffer.attributes {
            attributes.push(AttributeInfo {
                location: attribute.location,
                binding,
                format: host_of(statement, attribute.format)?,
                offset: attribute.offset,
            });
        }
    }
    Ok((bindings, attributes))
}

/// The colour targets of `stages`.
fn colour_targets(statement: &Statement, stages: RenderStages<'_>) -> Result<ColourTargets, Error> {
    let mut targets = ArrayVec::new();
    let mut formats = ArrayVec::new();
    for (at, target) in stages.targets().iter().enumerate() {
        targets
            .try_push(TargetInfo {
                blend: target.blend.map(blend),
                write_mask: u32::from(stages.writes(at).bits()),
            })
            .map_err(|_| Error::TooMany)?;
        formats
            .try_push(host_of(statement, target.format)?)
            .map_err(|_| Error::TooMany)?;
    }
    Ok((targets, formats))
}

/// How the host rasterises what `raster` says, its lines asked for as
/// rectangles where `rectangular_lines`.
fn raster_info(raster: Rasterization, rectangular_lines: bool) -> RasterInfo {
    RasterInfo {
        depth_clamp: raster.depth_clamp,
        cull: match raster.cull {
            CullMode::None => 0,
            CullMode::Front => 1,
            CullMode::Back => 2,
            CullMode::Both => 3,
        },
        front_face: match raster.front_face {
            FrontFace::CounterClockwise => 0,
            FrontFace::Clockwise => 1,
        },
        bias: raster
            .depth_bias
            .map(|bias| [bias.constant, bias.clamp, bias.slope].map(mathf::single)),
        line_width: raster.line_width,
        rectangular_lines,
    }
}

/// A pipeline's stages as the host names them, and each one's module: its
/// id and words.
type Programs<'a> = (ArrayVec<StageInfo<'a>, 2>, ArrayVec<(u64, &'a [u8]), 2>);

/// Add the stage `stage` of entry point `entry`, its module `words` made as
/// `id`, to `programs`.
fn add_stage<'a>(
    (programs, modules): &mut Programs<'a>,
    (stage, id, entry): (u32, u64, &'a str),
    words: &'a [u8],
) -> Result<(), Error> {
    programs
        .try_push(StageInfo {
            stage,
            module: id,
            entry,
        })
        .map_err(|_| Error::OutOfMemory)?;
    modules
        .try_push((id, words))
        .map_err(|_| Error::OutOfMemory)
}

/// A render pipeline in `context` of `stages`.
pub(super) fn make_render(
    context: &Arc<VenusContext>,
    stages: RenderStages<'_>,
) -> Result<VenusPipeline, Error> {
    let statement = &context.core.statement;
    let vertex = stages.vertex();
    let [vertex_id, fragment_id, layout, pipeline] = context.ids();
    let vertex_module = prepared(context, vertex)?;
    let fragment_module = stages
        .fragment()
        .map(|fragment| prepared(context, fragment))
        .transpose()?;
    let mut programs: Programs<'_> = (ArrayVec::new(), ArrayVec::new());
    add_stage(
        &mut programs,
        (stage::VERTEX, vertex_id, vertex.entry_point()),
        vertex_module.admitted().module().bytes(),
    )?;
    if let Some((fragment, module)) = stages.fragment().zip(fragment_module.as_ref()) {
        add_stage(
            &mut programs,
            (stage::FRAGMENT, fragment_id, fragment.entry_point()),
            module.admitted().module().bytes(),
        )?;
    }
    let (bindings, attributes) = vertex_input(statement, stages)?;
    let (targets, colour_formats) = colour_targets(statement, stages)?;
    let depth_stencil = stages.depth_stencil();
    let aspects = depth_stencil.map_or(Aspects::NONE, |state| state.format.aspects());
    let format_of = |aspect: Aspects| match depth_stencil {
        Some(state) if aspects.contains(aspect) => host_of(statement, state.format),
        _ => Ok(0),
    };
    let multisample = stages.multisample();
    let info = GraphicsInfo {
        stages: &programs.0,
        bindings: &bindings,
        attributes: &attributes,
        topology: topology(stages.topology()),
        restart: stages.primitive_restart(),
        raster: raster_info(stages.rasterization(), statement.rectangular_lines),
        samples: multisample.samples,
        sample_mask: multisample.mask,
        alpha_to_coverage: multisample.alpha_to_coverage,
        // A depth written without a test is tested always, as no host
        // writes a depth it does not test.
        depth: depth_stencil
            .filter(|_| aspects.contains(Aspects::DEPTH))
            .map(|state| (compare(state.depth_compare), state.depth_write)),
        stencil: depth_stencil
            .and_then(|state| state.stencil)
            .map(|faces| faces.map(stencil)),
        logic_op: stages.logic_op().map(logic),
        targets: &targets,
        colour_formats: &colour_formats,
        depth_format: format_of(Aspects::DEPTH)?,
        stencil_format: format_of(Aspects::STENCIL)?,
    };
    make_in(
        context,
        vertex,
        (PipelineIds { layout, pipeline }, &programs.1),
        Shape::Render(&info),
    )
}

const fn topology(topology: Topology) -> u32 {
    match topology {
        Topology::Points => 0,
        Topology::Lines => 1,
        Topology::LineStrip => 2,
        Topology::Triangles => 3,
        Topology::TriangleStrip => 4,
        Topology::TriangleFan => 5,
    }
}

/// The host's names of a blend's colour factors and operation, then its
/// alpha's.
const fn blend(blend: Blend) -> [u32; 6] {
    [
        factor(blend.source_color),
        factor(blend.destination_color),
        blend_op(blend.color),
        factor(blend.source_alpha),
        factor(blend.destination_alpha),
        blend_op(blend.alpha),
    ]
}

const fn factor(factor: BlendFactor) -> u32 {
    match factor {
        BlendFactor::Zero => 0,
        BlendFactor::One => 1,
        BlendFactor::SourceColor => 2,
        BlendFactor::OneMinusSourceColor => 3,
        BlendFactor::DestinationColor => 4,
        BlendFactor::OneMinusDestinationColor => 5,
        BlendFactor::SourceAlpha => 6,
        BlendFactor::OneMinusSourceAlpha => 7,
        BlendFactor::DestinationAlpha => 8,
        BlendFactor::OneMinusDestinationAlpha => 9,
        BlendFactor::ConstantColor => 10,
        BlendFactor::OneMinusConstantColor => 11,
        BlendFactor::ConstantAlpha => 12,
        BlendFactor::OneMinusConstantAlpha => 13,
        BlendFactor::SourceAlphaSaturate => 14,
        BlendFactor::Source1Color => 15,
        BlendFactor::OneMinusSource1Color => 16,
        BlendFactor::Source1Alpha => 17,
        BlendFactor::OneMinusSource1Alpha => 18,
    }
}

const fn blend_op(op: BlendOp) -> u32 {
    match op {
        BlendOp::Add => 0,
        BlendOp::Subtract => 1,
        BlendOp::ReverseSubtract => 2,
        BlendOp::Min => 3,
        BlendOp::Max => 4,
    }
}

const fn logic(op: LogicOp) -> u32 {
    match op {
        LogicOp::Clear => 0,
        LogicOp::And => 1,
        LogicOp::AndReverse => 2,
        LogicOp::Copy => 3,
        LogicOp::AndInverted => 4,
        LogicOp::NoOp => 5,
        LogicOp::Xor => 6,
        LogicOp::Or => 7,
        LogicOp::Nor => 8,
        LogicOp::Equivalent => 9,
        LogicOp::Invert => 10,
        LogicOp::OrReverse => 11,
        LogicOp::CopyInverted => 12,
        LogicOp::OrInverted => 13,
        LogicOp::Nand => 14,
        LogicOp::Set => 15,
    }
}

fn stencil(face: StencilFaceState) -> StencilInfo {
    StencilInfo {
        ops: [face.fail, face.pass, face.depth_fail].map(stencil_op),
        compare: compare(face.compare),
        compare_mask: u32::from(face.compare_mask),
        write_mask: u32::from(face.write_mask),
    }
}

const fn stencil_op(op: StencilOp) -> u32 {
    match op {
        StencilOp::Keep => 0,
        StencilOp::Zero => 1,
        StencilOp::Replace => 2,
        StencilOp::IncrementAndClamp => 3,
        StencilOp::DecrementAndClamp => 4,
        StencilOp::Invert => 5,
        StencilOp::IncrementAndWrap => 6,
        StencilOp::DecrementAndWrap => 7,
    }
}

/// A pass of a job, as its command names it.
#[derive(Copy, Clone)]
pub(super) struct Pass<'a> {
    pub(super) targets: Targets<'a, super::VenusDevice>,
    pub(super) depth_stencil: Option<PassDepthStencil<'a, super::VenusDevice>>,
    pub(super) area: [u32; 4],
    pub(super) draws: Draws<'a, super::VenusDevice>,
}

/// Encode `pass` into `one` for a job of `context` holding feedback slot
/// `slot`, handing each command finished to `flush`: where `checks`, each
/// indirect draw's records are copied into the slot's records and checked
/// there before the rendering instance drawing them begins.
pub(super) fn encode_pass(
    one: &mut Encoder,
    (commands, slot, checks): (u64, u32, bool),
    pass: Pass<'_>,
    flush: &mut dyn FnMut(&mut Encoder) -> Result<(), DriverError>,
) -> Result<(), DriverError> {
    let mut remaining = pass.draws.iter();
    let mut first = true;
    loop {
        let taken = if checks {
            // The instance before may still be reading the records about to
            // be copied over them.
            if !first {
                one.cmd_barrier(commands)?;
            }
            check_records(one, (commands, slot), remaining.clone(), flush)?
        } else {
            remaining.len()
        };
        // A device checking records states no draw of more than a slot holds.
        if taken == 0 && remaining.len() > 0 {
            return Err(DriverError::OutOfRange);
        }
        let last = taken == remaining.len();
        begin(one, commands, &pass, (first, last))?;
        flush(one)?;
        let mut bound = Bound::default();
        let mut checked = slot_records(slot);
        for draw in remaining.by_ref().take(taken) {
            bind(one, commands, &mut bound, &draw)?;
            let indexed = draw.index.is_some();
            match draw.count {
                DrawCount::Direct(counts) => one.cmd_draw(commands, direct(counts, indexed))?,
                DrawCount::Indirect { count, .. } if checks => {
                    let record = record_bytes(indexed);
                    one.cmd_draw_indirect(commands, indexed, (FEEDBACK, checked), (count, record))?;
                    checked += u64::from(count) * u64::from(record);
                }
                DrawCount::Indirect {
                    memory,
                    offset,
                    count,
                    stride,
                } => one.cmd_draw_indirect(
                    commands,
                    indexed,
                    (memory.buffer, offset),
                    (count, stride),
                )?,
            }
            flush(one)?;
        }
        one.cmd_end_rendering(commands)?;
        if last {
            return Ok(());
        }
        flush(one)?;
        first = false;
    }
}

/// Where slot `slot`'s records start in the feedback buffer.
fn slot_records(slot: u32) -> u64 {
    RECORDS_AT + u64::from(slot) * RECORDS_SLOT_BYTES
}

const fn record_bytes(indexed: bool) -> u32 {
    if indexed {
        INDEXED_DRAW_RECORD_BYTES
    } else {
        DRAW_RECORD_BYTES
    }
}

/// Copy the records of as many of `draws` as slot `slot` holds into it, one
/// after another, and check them there: how many draws that is. Where none
/// of them reads a record, nothing is copied or checked.
fn check_records<'a>(
    one: &mut Encoder,
    (commands, slot): (u64, u32),
    draws: impl Iterator<Item = DrawCommand<'a, super::VenusDevice>> + Clone,
    flush: &mut dyn FnMut(&mut Encoder) -> Result<(), DriverError>,
) -> Result<usize, DriverError> {
    let start = slot_records(slot);
    let end = start + RECORDS_SLOT_BYTES;
    let (mut taken, mut at) = (0, start);
    for draw in draws.clone() {
        if let DrawCount::Indirect {
            memory,
            offset,
            count,
            stride,
        } = draw.count
        {
            let record = record_bytes(draw.index.is_some());
            let bytes = u64::from(count) * u64::from(record);
            if at + bytes > end {
                break;
            }
            let into = (memory.buffer, FEEDBACK);
            if count > 1 && stride != record {
                let regions = (0..count).map(|index| CopyRegion {
                    src: offset + u64::from(index) * u64::from(stride),
                    dst: at + u64::from(index) * u64::from(record),
                    size: u64::from(record),
                });
                one.cmd_copy_buffer(commands, into, regions)?;
                flush(one)?;
            } else if count > 0 {
                let whole = CopyRegion {
                    src: offset,
                    dst: at,
                    size: bytes,
                };
                one.cmd_copy_buffer(commands, into, core::iter::once(whole))?;
                flush(one)?;
            }
            at += bytes;
        }
        taken += 1;
    }
    if at == start {
        return Ok(taken);
    }
    one.cmd_barrier(commands)?;
    bind_check(one, commands, &RECORDS_CHECK)?;
    let mut at = start;
    for draw in draws.take(taken) {
        let DrawCount::Indirect { count, .. } = draw.count else {
            continue;
        };
        if count == 0 {
            continue;
        }
        // Each check reads and writes the slot's fault word.
        if at != start {
            one.cmd_barrier(commands)?;
        }
        let indexed = draw.index.is_some();
        let first = u32::try_from((at - RECORDS_AT) / 4).map_err(|_| DriverError::OutOfRange)?;
        let push = records::Push {
            slot,
            first,
            count,
            indexed,
        };
        dispatch_check(one, commands, &RECORDS_CHECK, &push.words())?;
        flush(one)?;
        at += u64::from(count) * u64::from(record_bytes(indexed));
    }
    one.cmd_barrier(commands)?;
    Ok(taken)
}

/// Begin a rendering instance of `pass`: its targets loaded as the pass says
/// where `first`, else as the instance before stored them, and resolved where
/// `last`.
fn begin(
    one: &mut Encoder,
    commands: u64,
    pass: &Pass<'_>,
    (first, last): (bool, bool),
) -> Result<(), DriverError> {
    let clear = |load: Load, format: Format| match load {
        Load::Clear(value) if first => Some(host_clear(value, format)),
        _ => None,
    };
    let mut colour: ArrayVec<AttachmentInfo, MAX_PASS_ATTACHMENTS> = ArrayVec::new();
    for target in pass.targets.iter() {
        colour
            .try_push(AttachmentInfo {
                view: target.view.view,
                resolve: target.resolve.filter(|_| last).map(|resolve| resolve.view),
                clear: clear(target.load, target.format),
            })
            .map_err(|_| DriverError::OutOfRange)?;
    }
    let aspect = |aspect: Aspects, load: fn(&PassDepthStencil<'_, super::VenusDevice>) -> Load| {
        pass.depth_stencil
            .filter(|target| target.format.aspects().contains(aspect))
            .map(|target| AttachmentInfo {
                view: target.view.view,
                resolve: None,
                clear: clear(load(&target), target.format),
            })
    };
    let depth = aspect(Aspects::DEPTH, |target| target.depth);
    let stencil = aspect(Aspects::STENCIL, |target| target.stencil);
    one.cmd_begin_rendering(commands, pass.area, (&colour, depth, stencil))
}

/// What a rendering instance has bound, so that a draw binds again only what
/// changes: a pipeline bound afresh has its sets and push constants bound
/// again too.
#[derive(Default)]
struct Bound {
    pipeline: Option<u64>,
    sets: Option<ArrayVec<u64, MAX_DISPATCH_SETS>>,
    push: Option<ArrayVec<u32, PUSH_WORDS>>,
    vertex_buffers: Option<ArrayVec<[u64; 3], MAX_DRAW_VERTEX_BUFFERS>>,
    index: Option<([u64; 3], bool)>,
    viewport: Option<[u32; 6]>,
    scissor: Option<[u32; 4]>,
    reference: Option<u32>,
    constants: Option<[u32; 4]>,
}

/// `value` into `held`, answering whether it changed what was held.
fn changes<T: PartialEq>(held: &mut Option<T>, value: T) -> bool {
    if held.as_ref() == Some(&value) {
        false
    } else {
        *held = Some(value);
        true
    }
}

/// Bind what `draw` runs with that `bound` does not already hold.
fn bind(
    one: &mut Encoder,
    commands: u64,
    bound: &mut Bound,
    draw: &DrawCommand<'_, super::VenusDevice>,
) -> Result<(), DriverError> {
    let pipeline = draw.pipeline;
    if changes(&mut bound.pipeline, pipeline.pipeline) {
        one.cmd_bind_pipeline(commands, BIND_POINT_GRAPHICS, pipeline.pipeline)?;
        bound.sets = None;
        bound.push = None;
    }
    let mut sets = ArrayVec::new();
    for set in draw.sets.iter() {
        sets.try_push(set.set)
            .map_err(|_| DriverError::OutOfRange)?;
    }
    if !sets.is_empty() && changes(&mut bound.sets, sets.clone()) {
        one.cmd_bind_sets(commands, (BIND_POINT_GRAPHICS, pipeline.layout), &sets)?;
    }
    let push = ArrayVec::try_from(draw.push).map_err(|_| DriverError::OutOfRange)?;
    if !draw.push.is_empty() && changes(&mut bound.push, push) {
        one.cmd_push_constants(commands, (pipeline.layout, pipeline.push_stages), draw.push)?;
    }
    let mut vertex_buffers = ArrayVec::new();
    for range in draw.vertex_buffers.iter() {
        vertex_buffers
            .try_push(host_range(range, 1))
            .map_err(|_| DriverError::OutOfRange)?;
    }
    if !vertex_buffers.is_empty() && changes(&mut bound.vertex_buffers, vertex_buffers.clone()) {
        one.cmd_bind_vertex_buffers(commands, &vertex_buffers)?;
    }
    if let Some((range, format)) = draw.index {
        let wide = format == IndexFormat::U32;
        let index = (host_range(range, if wide { 4 } else { 2 }), wide);
        if changes(&mut bound.index, index) {
            one.cmd_bind_index_buffer(commands, index.0, index.1)?;
        }
    }
    let port = draw.viewport;
    let viewport = [
        port.x,
        port.y,
        port.width,
        port.height,
        port.min_depth,
        port.max_depth,
    ];
    if changes(&mut bound.viewport, viewport.map(f32::to_bits)) {
        one.cmd_set_viewport(commands, viewport)?;
    }
    let area = held_scissor(draw.scissor, port);
    if changes(&mut bound.scissor, area) {
        one.cmd_set_scissor(commands, area)?;
    }
    let reference = u32::from(draw.stencil_reference);
    if changes(&mut bound.reference, reference) {
        one.cmd_set_stencil_reference(commands, reference)?;
    }
    if changes(&mut bound.constants, draw.blend_constant.map(f32::to_bits)) {
        one.cmd_set_blend_constants(commands, draw.blend_constant)?;
    }
    Ok(())
}

/// `range` as the host binds it, whole `step`s of it: an empty range at its
/// buffer's start, so its offset lies within the buffer.
fn host_range(range: BufferRange<'_, VenusMemory>, step: u64) -> [u64; 3] {
    let size = range.size - range.size % step;
    if size == 0 {
        [range.memory.buffer, 0, 0]
    } else {
        [range.memory.buffer, range.offset, size]
    }
}

/// `scissor` held to the pixels `port` covers any part of, so the host
/// holds a point or line to its viewport as clipping holds a triangle there:
/// an empty one at the scissor's corner.
fn held_scissor(scissor: Rect, port: Viewport) -> [u32; 4] {
    let (top, tall) = if port.height < 0.0 {
        (
            f64::from(port.y) + f64::from(port.height),
            -f64::from(port.height),
        )
    } else {
        (f64::from(port.y), f64::from(port.height))
    };
    let edge = |at: f64, round: fn(f64) -> f64| {
        u32::try_from(mathf::round_i32(round(at)).max(0)).unwrap_or(0)
    };
    let span = |start: u32, len: u32, (low, high): (u32, u32)| {
        let from = start.max(low);
        (
            from,
            start.saturating_add(len).min(high).saturating_sub(from),
        )
    };
    let left = f64::from(port.x);
    let (x, width) = span(
        scissor.x,
        scissor.width,
        (
            edge(left, mathf::floor),
            edge(left + f64::from(port.width), mathf::ceil),
        ),
    );
    let (y, height) = span(
        scissor.y,
        scissor.height,
        (edge(top, mathf::floor), edge(top + tall, mathf::ceil)),
    );
    if width == 0 || height == 0 {
        [scissor.x, scissor.y, 0, 0]
    } else {
        [x, y, width, height]
    }
}

/// A direct draw's counts as the host names them.
const fn direct(counts: DrawCounts, indexed: bool) -> DrawInfo {
    if indexed {
        DrawInfo::Indexed(
            [counts.positions, counts.instances, counts.first],
            counts.base_vertex,
            counts.first_instance,
        )
    } else {
        DrawInfo::Vertices([
            counts.positions,
            counts.instances,
            counts.first,
            counts.first_instance,
        ])
    }
}
