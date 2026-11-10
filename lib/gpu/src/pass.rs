//! Render passes: the attachments a pass renders to, how each is loaded and
//! stored, and the draws recorded into it, each a self-describing call;
//! checked whole when a submission carries it.

use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::ops::Range;

use crate::binding::{BindSet, BindSetState};
use crate::command::{buffer, push_words, set_keys, Command, Op, Refused};
use crate::device::{owned, Buffer, CommandBuffer};
use crate::format::{Aspects, Format, FormatFeatures};
use crate::image::{ClearValue, ImageUsage, ViewDimension};
use crate::info::{BufferUsage, DeviceInfo};
use crate::memory::Memory;
use crate::model::{BindSetKey, BufferKey, ContextState, RenderPipelineKey, ViewKey};
use crate::objects::ImageView;
use crate::render::{Render, RenderPipeline, RenderPipelineState};
use crate::resource::ViewState;
use crate::{Error, MAX_DISPATCH_SETS, MAX_PUSH_BYTES, MAX_SUBMISSION_COMMANDS};

/// The most colour attachments one pass records: past every device's.
pub const MAX_PASS_ATTACHMENTS: usize = 8;

/// The most vertex buffers one draw binds: past every device's.
pub const MAX_DRAW_VERTEX_BUFFERS: usize = 32;

/// What an attachment holds as its pass begins; it keeps what the pass
/// renders.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum Load {
    /// What it held before.
    Keep,
    /// This value in the pass's area.
    Clear(ClearValue),
}

/// A colour attachment, the view its samples resolve into, and how it is
/// loaded.
#[derive(Copy, Clone, Debug)]
pub struct ColourAttachment<'a> {
    /// A two-dimensional view of one level and layer of an image made as a
    /// colour attachment.
    pub view: &'a ImageView,
    /// A one-sample view of the same format the four samples resolve into
    /// as the pass ends.
    pub resolve: Option<&'a ImageView>,
    /// What it holds as the pass begins.
    pub load: Load,
}

/// A depth-stencil attachment, and how each of its aspects is loaded.
#[derive(Copy, Clone, Debug)]
pub struct DepthStencilAttachment<'a> {
    /// A two-dimensional view of one level and layer of an image made as a
    /// depth-stencil attachment.
    pub view: &'a ImageView,
    /// What its depth holds as the pass begins; a clear names a depth.
    pub depth_load: Load,
    /// What its stencil holds as the pass begins; a clear names a stencil.
    pub stencil_load: Load,
}

/// A rectangle of pixels.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Rect {
    /// Its first column.
    pub x: u32,
    /// Its first row.
    pub y: u32,
    /// Its columns.
    pub width: u32,
    /// Its rows.
    pub height: u32,
}

/// A render pass: its attachments, of one extent and sample count, and the
/// area its draws, clears and resolves touch.
#[derive(Copy, Clone, Debug)]
pub struct Pass<'a> {
    /// Its colour attachments, by target.
    pub colour: &'a [ColourAttachment<'a>],
    /// Its depth-stencil attachment.
    pub depth_stencil: Option<DepthStencilAttachment<'a>>,
    /// The pixels it touches.
    pub area: Rect,
}

/// Where a draw's clip space lands in the framebuffer: its origin and size,
/// a negative height flipping it, and the depths its near and far planes
/// take.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Viewport {
    /// Its left edge.
    pub x: f32,
    /// Its top edge.
    pub y: f32,
    /// Its width: above nought.
    pub width: f32,
    /// Its height: negative to flip it, never nought.
    pub height: f32,
    /// The depth of its near plane, within `0.0..=1.0`.
    pub min_depth: f32,
    /// The depth of its far plane, within `0.0..=1.0`.
    pub max_depth: f32,
}

/// What a draw binds and sets: everything it runs with but its vertices.
#[derive(Copy, Clone, Debug)]
pub struct Draw<'a> {
    /// Its pipeline.
    pub pipeline: &'a RenderPipeline,
    /// Its bind sets, set by set.
    pub sets: &'a [&'a BindSet],
    /// Its push constants.
    pub push_constants: &'a [u8],
    /// Each vertex buffer its pipeline reads, and where in it the elements
    /// start.
    pub vertex_buffers: &'a [(&'a Buffer, u64)],
    /// Its viewport.
    pub viewport: Viewport,
    /// The pixels it may touch; it touches none outside its pass's area.
    pub scissor: Rect,
    /// The value its stencil test compares with and writes.
    pub stencil_reference: u8,
    /// The constant its blend factors read.
    pub blend_constant: [f32; 4],
}

/// How wide an index is.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum IndexFormat {
    /// Sixteen bits.
    U16,
    /// Thirty-two bits.
    U32,
}

impl IndexFormat {
    /// The bytes one index takes.
    pub(crate) const fn bytes(self) -> u64 {
        match self {
            Self::U16 => 2,
            Self::U32 => 4,
        }
    }
}

/// Where an indexed draw's indices lie.
#[derive(Copy, Clone, Debug)]
pub struct Indices<'a> {
    /// The buffer holding them.
    pub buffer: &'a Buffer,
    /// Where they start: a multiple of an index's size.
    pub offset: u64,
    /// How wide each is.
    pub format: IndexFormat,
}

/// A pass's attachment as recorded.
#[derive(Copy, Clone, Debug)]
pub(crate) struct AttachmentRecord {
    view: ViewKey,
    resolve: Option<ViewKey>,
    load: Load,
}

/// A pass's depth-stencil attachment as recorded.
#[derive(Copy, Clone, Debug)]
pub(crate) struct DepthStencilRecord {
    view: ViewKey,
    depth: Load,
    stencil: Load,
}

/// How many vertices and instances a draw makes, and where they come from.
#[derive(Clone, Debug)]
pub(crate) enum Count {
    Vertices {
        vertices: Range<u32>,
        instances: Range<u32>,
    },
    Indices {
        indices: Range<u32>,
        base_vertex: i32,
        instances: Range<u32>,
    },
}

/// A draw as recorded, its objects named by key.
#[derive(Debug)]
pub(crate) struct DrawRecord {
    pipeline: RenderPipelineKey,
    sets: Vec<BindSetKey>,
    push: Option<Arc<Vec<u32>>>,
    push_bytes: usize,
    vertex_buffers: Vec<(BufferKey, u64)>,
    index: Option<(BufferKey, u64, IndexFormat)>,
    viewport: Viewport,
    scissor: Rect,
    stencil_reference: u8,
    blend_constant: [f32; 4],
    count: Count,
}

/// A pass as recorded: its attachments, its area and its draws in order.
#[derive(Debug)]
pub(crate) struct PassRecord {
    colour: Vec<AttachmentRecord>,
    depth_stencil: Option<DepthStencilRecord>,
    area: Rect,
    pub(crate) draws: Vec<DrawRecord>,
}

/// What records a pass's draws, each counting as a command.
pub struct Drawer<'p> {
    buffer: &'p CommandBuffer,
    pass: &'p mut PassRecord,
    /// The commands the pass may still take.
    room: usize,
}

impl CommandBuffer {
    /// Render a pass: its attachments loaded, the draws `record` records run
    /// in order within its area, and its multisampled targets resolved.
    /// The pass counts as a command and each draw as another; one `record`
    /// refuses is recorded as nothing.
    ///
    /// # Errors
    ///
    /// [`Error::Foreign`] for an object of another context,
    /// [`Error::TooMany`] past [`MAX_PASS_ATTACHMENTS`] attachments or the
    /// commands a submission carries, [`Error::OutOfMemory`] for room the
    /// host refuses, and whatever `record` answers.
    pub fn render_pass<R>(
        &mut self,
        pass: &Pass<'_>,
        record: impl FnOnce(&mut Drawer<'_>) -> Result<R, Error>,
    ) -> Result<R, Error> {
        if pass.colour.len() > MAX_PASS_ATTACHMENTS {
            return Err(Error::TooMany);
        }
        let mut colour = Vec::new();
        colour
            .try_reserve_exact(pass.colour.len())
            .map_err(|_| Error::OutOfMemory)?;
        for attachment in pass.colour {
            colour.push(AttachmentRecord {
                view: self.named_view(attachment.view)?,
                resolve: attachment
                    .resolve
                    .map(|view| self.named_view(view))
                    .transpose()?,
                load: attachment.load,
            });
        }
        let depth_stencil = pass
            .depth_stencil
            .map(|attachment| {
                Ok::<_, Error>(DepthStencilRecord {
                    view: self.named_view(attachment.view)?,
                    depth: attachment.depth_load,
                    stencil: attachment.stencil_load,
                })
            })
            .transpose()?;
        let mut recorded = PassRecord {
            colour,
            depth_stencil,
            area: pass.area,
            draws: Vec::new(),
        };
        let room = MAX_SUBMISSION_COMMANDS
            .saturating_sub(self.len())
            .checked_sub(1)
            .ok_or(Error::TooMany)?;
        let answer = record(&mut Drawer {
            buffer: self,
            pass: &mut recorded,
            room,
        })?;
        self.record(Command::Pass(
            Box::try_new(recorded).map_err(|_| Error::OutOfMemory)?,
        ))?;
        Ok(answer)
    }

    fn named_view(&self, view: &ImageView) -> Result<ViewKey, Error> {
        owned(&view.shared, view.context, &self.shared, self.context)?;
        Ok(view.key)
    }
}

impl Drawer<'_> {
    /// Draw `instances` instances of `vertices`, consecutive vertices
    /// numbered from the range's start.
    ///
    /// # Errors
    ///
    /// [`Error::Foreign`] for an object of another context,
    /// [`Error::TooMany`] past [`MAX_DISPATCH_SETS`] sets,
    /// [`MAX_PUSH_BYTES`] bytes, the vertex buffers a pipeline reads or the
    /// commands a submission carries, and [`Error::OutOfMemory`] for room the
    /// host refuses.
    pub fn draw(
        &mut self,
        draw: &Draw<'_>,
        vertices: Range<u32>,
        instances: Range<u32>,
    ) -> Result<(), Error> {
        self.record(
            draw,
            None,
            Count::Vertices {
                vertices,
                instances,
            },
        )
    }

    /// Draw `instances` instances of the vertices the indices of `range`
    /// name, each index added to `base_vertex`.
    ///
    /// # Errors
    ///
    /// As [`Self::draw`].
    pub fn draw_indexed(
        &mut self,
        draw: &Draw<'_>,
        indices: &Indices<'_>,
        (range, base_vertex): (Range<u32>, i32),
        instances: Range<u32>,
    ) -> Result<(), Error> {
        let index = (
            self.buffer.named(indices.buffer)?,
            indices.offset,
            indices.format,
        );
        self.record(
            draw,
            Some(index),
            Count::Indices {
                indices: range,
                base_vertex,
                instances,
            },
        )
    }

    fn record(
        &mut self,
        draw: &Draw<'_>,
        index: Option<(BufferKey, u64, IndexFormat)>,
        count: Count,
    ) -> Result<(), Error> {
        let buffer = self.buffer;
        owned(
            &draw.pipeline.shared,
            draw.pipeline.context,
            &buffer.shared,
            buffer.context,
        )?;
        if self.room == 0
            || draw.sets.len() > MAX_DISPATCH_SETS
            || draw.push_constants.len() > MAX_PUSH_BYTES
            || draw.vertex_buffers.len() > MAX_DRAW_VERTEX_BUFFERS
        {
            return Err(Error::TooMany);
        }
        let mut vertex_buffers = Vec::new();
        vertex_buffers
            .try_reserve_exact(draw.vertex_buffers.len())
            .map_err(|_| Error::OutOfMemory)?;
        for &(vertex_buffer, offset) in draw.vertex_buffers {
            vertex_buffers.push((buffer.named(vertex_buffer)?, offset));
        }
        let recorded = DrawRecord {
            pipeline: draw.pipeline.key,
            sets: set_keys(buffer, draw.sets)?,
            push: push_words(draw.push_constants)?,
            push_bytes: draw.push_constants.len(),
            vertex_buffers,
            index,
            viewport: draw.viewport,
            scissor: draw.scissor,
            stencil_reference: draw.stencil_reference,
            blend_constant: draw.blend_constant,
            count,
        };
        self.pass
            .draws
            .try_reserve(1)
            .map_err(|_| Error::OutOfMemory)?;
        self.pass.draws.push(recorded);
        self.room -= 1;
        Ok(())
    }
}

/// A pass's colour attachment, checked, and its load.
pub(crate) struct Target {
    pub(crate) view: Arc<ViewState>,
    pub(crate) resolve: Option<Arc<ViewState>>,
    pub(crate) load: Load,
}

/// A pass's depth-stencil attachment, checked, and its aspects' loads.
pub(crate) struct DepthTarget {
    pub(crate) view: Arc<ViewState>,
    pub(crate) depth: Load,
    pub(crate) stencil: Load,
}

/// Where a draw's vertex attributes or indices are read from: `size` bytes
/// of a memory from byte `start`.
pub(crate) struct Source {
    pub(crate) memory: Arc<Memory>,
    pub(crate) start: u64,
    pub(crate) size: u64,
}

/// A draw, checked.
pub(crate) struct DrawWork {
    pub(crate) pipeline: Arc<RenderPipelineState>,
    pub(crate) sets: Vec<Arc<BindSetState>>,
    pub(crate) push: Option<Arc<Vec<u32>>>,
    pub(crate) vertex_buffers: Vec<Source>,
    pub(crate) index: Option<(Source, IndexFormat)>,
    pub(crate) viewport: Viewport,
    pub(crate) scissor: Rect,
    pub(crate) stencil_reference: u8,
    pub(crate) blend_constant: [f32; 4],
    pub(crate) count: Count,
}

/// A pass checked whole: its attachments of one extent and sample count, its
/// area within them, and each draw's pipeline rendering to them.
pub(crate) struct PassWork {
    pub(crate) colour: Vec<Target>,
    pub(crate) depth_stencil: Option<DepthTarget>,
    pub(crate) area: Rect,
    pub(crate) samples: u32,
    pub(crate) draws: Vec<DrawWork>,
}

impl core::fmt::Debug for PassWork {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("PassWork").finish_non_exhaustive()
    }
}

impl PassWork {
    /// The host bytes it holds while queued, its own included.
    pub(crate) fn held_bytes(&self) -> u64 {
        use tairix_util::fallible::held;
        let draws = self.draws.iter().fold(0u64, |total, draw| {
            [
                held(&draw.sets),
                held(&draw.vertex_buffers),
                draw.push.as_deref().map_or(0, |words| {
                    crate::model::shared_bytes::<Vec<u32>>().saturating_add(held(words))
                }),
            ]
            .into_iter()
            .fold(total, u64::saturating_add)
        });
        [
            core::mem::size_of::<Self>() as u64,
            held(&self.colour),
            held(&self.draws),
            draws,
        ]
        .into_iter()
        .fold(0, u64::saturating_add)
    }
}

/// An attachment's image and the one level and layer it renders to, of a
/// two-dimensional view made for `usage`, in a format the device states the
/// use and its sample count of.
fn attachment<'c>(
    (info, context): (&DeviceInfo, &'c ContextState),
    key: ViewKey,
    (usage, feature): (ImageUsage, FormatFeatures),
) -> Result<&'c Arc<ViewState>, Error> {
    let view = context.views.get(key).ok_or(Error::Stale)?;
    if !view.image.desc.usage.contains(usage) {
        return Err(Error::Usage);
    }
    let range = &view.desc.range;
    if view.desc.dimension != ViewDimension::D2 || range.levels != 1 || range.layers != 1 {
        return Err(Error::Shape);
    }
    let samples = if view.image.desc.samples == 1 {
        FormatFeatures::NONE
    } else {
        FormatFeatures::MULTISAMPLED
    };
    if !info
        .formats
        .get(view.desc.format)
        .contains(feature.with(samples))
    {
        return Err(Error::Unsupported);
    }
    Ok(view)
}

/// The extent and sample count of an attachment.
fn framed(view: &ViewState) -> (u32, u32, u32) {
    let extent = view.image.desc.level_extent(view.desc.range.base_level);
    (extent.width, extent.height, view.image.desc.samples)
}

/// Refuse a clear value of another kind than `format`'s.
fn check_load(load: Load, format: Format) -> Result<(), Error> {
    match load {
        Load::Clear(value) => value.check(format),
        Load::Keep => Ok(()),
    }
}

/// The bytes of its memory an attachment's one level and layer takes.
fn span_of(view: &ViewState) -> Range<u64> {
    let mut span = 0..0;
    view.spans(|taken| span = taken);
    span
}

/// A pass's attachments checked: its colour targets, its depth-stencil, and
/// their one extent and sample count.
type Attachments = (Vec<Target>, Option<DepthTarget>, (u32, u32, u32));

/// Byte spans of memories: what a pass's attachments' levels and layers
/// take.
type Spans = Vec<(Arc<Memory>, Range<u64>)>;

/// The colour and depth-stencil attachments `record` names, each made for
/// its use and of one extent and sample count with the rest, and that extent
/// and sample count.
fn attachments(
    info: &DeviceInfo,
    context: &ContextState,
    record: &PassRecord,
) -> Result<Attachments, Error> {
    let mut frame = None;
    let mut agree = |view: &ViewState| -> Result<(), Error> {
        let framed = framed(view);
        match frame.replace(framed) {
            Some(held) if held != framed => Err(if held.2 == framed.2 {
                Error::Shape
            } else {
                Error::Samples
            }),
            _ => Ok(()),
        }
    };
    let colour_use = (
        ImageUsage::COLOR_ATTACHMENT,
        FormatFeatures::COLOR_ATTACHMENT,
    );
    let mut colour = Vec::new();
    colour
        .try_reserve_exact(record.colour.len())
        .map_err(|_| Error::OutOfMemory)?;
    for recorded in &record.colour {
        let view = attachment((info, context), recorded.view, colour_use)?;
        agree(view)?;
        check_load(recorded.load, view.desc.format)?;
        let resolve = match recorded.resolve {
            Some(key) => {
                let destination = attachment((info, context), key, colour_use)?;
                resolvable(info, view, destination)?;
                Some(Arc::clone(destination))
            }
            None => None,
        };
        colour.push(Target {
            view: Arc::clone(view),
            resolve,
            load: recorded.load,
        });
    }
    let depth_stencil = match &record.depth_stencil {
        Some(recorded) => {
            let view = attachment(
                (info, context),
                recorded.view,
                (
                    ImageUsage::DEPTH_STENCIL_ATTACHMENT,
                    FormatFeatures::DEPTH_STENCIL,
                ),
            )?;
            agree(view)?;
            if view.desc.range.aspects != view.desc.format.aspects() {
                return Err(Error::Aspect);
            }
            check_load(recorded.depth, view.desc.format)?;
            check_load(recorded.stencil, view.desc.format)?;
            Some(DepthTarget {
                view: Arc::clone(view),
                depth: recorded.depth,
                stencil: recorded.stencil,
            })
        }
        None => None,
    };
    Ok((colour, depth_stencil, frame.ok_or(Error::Empty)?))
}

/// Refuse a resolve of `view` into `destination` but from four samples to
/// one, of one extent and format, in a format the device resolves.
fn resolvable(info: &DeviceInfo, view: &ViewState, destination: &ViewState) -> Result<(), Error> {
    let (width, height, samples) = framed(view);
    let (into_width, into_height, into_samples) = framed(destination);
    if samples != 4 || into_samples != 1 {
        return Err(Error::Samples);
    }
    if (width, height) != (into_width, into_height) || destination.desc.format != view.desc.format {
        return Err(Error::Shape);
    }
    if !info
        .formats
        .get(view.desc.format)
        .contains(FormatFeatures::RESOLVED)
    {
        return Err(Error::Unsupported);
    }
    Ok(())
}

/// The memory each attachment's level and layer lies in, colour targets'
/// and their resolves' first and the depth-stencil's last, refusing any that
/// overlaps another.
fn written_spans(colour: &[Target], depth_stencil: Option<&DepthTarget>) -> Result<Spans, Error> {
    let mut spans: Spans = Vec::new();
    spans
        .try_reserve_exact(colour.len() * 2 + 1)
        .map_err(|_| Error::OutOfMemory)?;
    for target in colour {
        spans.push((Arc::clone(&target.view.image.memory), span_of(&target.view)));
        if let Some(destination) = &target.resolve {
            spans.push((Arc::clone(&destination.image.memory), span_of(destination)));
        }
    }
    if let Some(depth) = depth_stencil {
        spans.push((Arc::clone(&depth.view.image.memory), span_of(&depth.view)));
    }
    for (at, (memory, span)) in spans.iter().enumerate() {
        let twice = spans
            .get(..at)
            .unwrap_or(&[])
            .iter()
            .any(|(other, held)| Arc::ptr_eq(other, memory) && overlaps(held, span));
        if twice {
            return Err(Error::Overlap);
        }
    }
    Ok(spans)
}

/// Whether two spans share a byte.
const fn overlaps(a: &Range<u64>, b: &Range<u64>) -> bool {
    a.start < b.end && b.start < a.end
}

/// A span of a memory a pass writes.
type Written = (Arc<Memory>, Range<u64>);

/// Refuse the first of `draws` reaching what its pass writes: a descriptor
/// its stages see, or a vertex or index buffer, reaching an attachment's
/// level and layer in `written` — and a descriptor its stages may write
/// reaching the depth-stencil attachment's, where only `tested`.
fn refuse_reaches(
    draws: &[DrawWork],
    (written, tested): (&[Written], Option<&Written>),
) -> Result<(), Refused> {
    let mut before: &[Arc<BindSetState>] = &[];
    for (at, draw) in draws.iter().enumerate() {
        let sets_reach = draw.sets.iter().enumerate().any(|(index, set)| {
            // Checked for the draw before, which bound it in the same place.
            if before.get(index).is_some_and(|held| Arc::ptr_eq(held, set)) {
                return false;
            }
            let meets = |(memory, span): &Written, writes: bool| {
                set.memories
                    .binary_search_by_key(&Arc::as_ptr(memory), Arc::as_ptr)
                    .ok()
                    .and_then(|place| u32::try_from(place).ok())
                    .is_some_and(|place| set.reach.meets(place, span, writes))
            };
            written.iter().any(|span| meets(span, false))
                || tested.is_some_and(|span| meets(span, true))
        });
        let buffers_reach = draw
            .vertex_buffers
            .iter()
            .chain(draw.index.as_ref().map(|(source, _)| source))
            .any(|source| {
                let read = source.start..source.start.saturating_add(source.size);
                written.iter().any(|(memory, span)| {
                    Arc::ptr_eq(memory, &source.memory) && overlaps(&read, span)
                })
            });
        if sets_reach || buffers_reach {
            return Err(Refused {
                at: at + 1,
                error: Error::Overlap,
            });
        }
        before = &draw.sets;
    }
    Ok(())
}

/// Check a recorded pass against the objects of `context` and the device
/// `info` describes: a refusal names the pass, at nought, or its draw, from
/// one.
pub(crate) fn resolve(
    info: &DeviceInfo,
    context: &ContextState,
    record: &PassRecord,
) -> Result<Op, Refused> {
    if record.colour.len() > info.limits.max_color_attachments as usize {
        return Err(Error::TooMany.into());
    }
    if record.colour.is_empty() && record.depth_stencil.is_none() {
        return Err(Error::Empty.into());
    }
    let (colour, depth_stencil, (width, height, samples)) = attachments(info, context, record)?;
    let area = record.area;
    let within = |start: u32, len: u32, end: u32| start.checked_add(len).is_some_and(|e| e <= end);
    if area.width == 0 || area.height == 0 {
        return Err(Error::Empty.into());
    }
    if !within(area.x, area.width, width) || !within(area.y, area.height, height) {
        return Err(Error::OutOfRange.into());
    }
    let spans = written_spans(&colour, depth_stencil.as_ref())?;
    let mut draws = Vec::new();
    draws
        .try_reserve_exact(record.draws.len())
        .map_err(|_| Error::OutOfMemory)?;
    for (at, recorded) in record.draws.iter().enumerate() {
        let draw = check_draw(
            info,
            context,
            (&colour, depth_stencil.as_ref(), samples),
            recorded,
        )
        .map_err(|error| Refused { at: at + 1, error })?;
        draws.push(draw);
    }
    // A depth-stencil attachment only tested may be read as it is tested:
    // the loads of its aspects keep what it held and no draw writes it.
    let tested_only = depth_stencil.as_ref().is_some_and(|depth| {
        let aspects = depth.view.desc.format.aspects();
        let cleared = |load: Load, aspect: Aspects| {
            aspects.contains(aspect) && matches!(load, Load::Clear(_))
        };
        !cleared(depth.depth, Aspects::DEPTH)
            && !cleared(depth.stencil, Aspects::STENCIL)
            && !draws
                .iter()
                .any(|draw| writes_depth_stencil(&draw.pipeline.stages))
    });
    let reached = match spans.split_last() {
        Some((depth, colour)) if tested_only => (colour, Some(depth)),
        _ => (&spans[..], None),
    };
    refuse_reaches(&draws, reached)?;
    let work = Box::try_new(PassWork {
        colour,
        depth_stencil,
        area,
        samples,
        draws,
    })
    .map_err(|_| Error::OutOfMemory)?;
    Ok(Op::Pass(work))
}

/// Whether a draw of `render` writes depth or stencil.
fn writes_depth_stencil(render: &Render) -> bool {
    render.depth_stencil.is_some_and(|state| {
        state.depth_write
            || state.stencil.is_some_and(|faces| {
                faces.iter().any(|face| {
                    face.write_mask != 0
                        && [face.fail, face.pass, face.depth_fail]
                            .iter()
                            .any(|&op| op != crate::render::StencilOp::Keep)
                })
            })
    })
}

/// A draw checked against its pass's attachments: its pipeline renders to
/// them, its bind sets and push constants are its layout's, its buffers are
/// made for their use, and its viewport and scissor are within the device's.
fn check_draw(
    info: &DeviceInfo,
    context: &ContextState,
    (colour, depth_stencil, samples): (&[Target], Option<&DepthTarget>, u32),
    recorded: &DrawRecord,
) -> Result<DrawWork, Error> {
    let pipeline = Arc::clone(
        context
            .render_pipelines
            .table
            .get(recorded.pipeline)
            .ok_or(Error::Stale)?,
    );
    let render = &pipeline.stages;
    let formats_agree = render.targets.len() == colour.len()
        && render
            .targets
            .iter()
            .zip(colour)
            .all(|(target, attachment)| target.format == attachment.view.desc.format)
        && render.depth_stencil.map(|state| state.format)
            == depth_stencil.map(|depth| depth.view.desc.format);
    if !formats_agree {
        return Err(Error::Interface);
    }
    if render.multisample.samples != samples {
        return Err(Error::Samples);
    }
    let layout = &pipeline.layout;
    if recorded.sets.len() != layout.sets.len()
        || recorded.vertex_buffers.len() != render.buffers.len()
    {
        return Err(Error::Bindings);
    }
    let mut sets = Vec::new();
    sets.try_reserve_exact(recorded.sets.len())
        .map_err(|_| Error::OutOfMemory)?;
    for (key, wanted) in recorded.sets.iter().zip(&layout.sets) {
        let set = context.bind_sets.get(*key).ok_or(Error::Stale)?;
        if !set.layout.serves(wanted) {
            return Err(Error::Bindings);
        }
        sets.push(Arc::clone(set));
    }
    if recorded.push_bytes != layout.push_constants.bytes as usize {
        return Err(Error::Shape);
    }
    let source = |key: BufferKey, offset: u64, usage: BufferUsage| -> Result<Source, Error> {
        let held = buffer(context, key, usage)?;
        if offset > held.size {
            return Err(Error::OutOfRange);
        }
        Ok(Source {
            memory: Arc::clone(&held.memory),
            start: held.offset + offset,
            size: held.size - offset,
        })
    };
    let mut vertex_buffers = Vec::new();
    vertex_buffers
        .try_reserve_exact(recorded.vertex_buffers.len())
        .map_err(|_| Error::OutOfMemory)?;
    for &(key, offset) in &recorded.vertex_buffers {
        vertex_buffers.push(source(key, offset, BufferUsage::VERTEX)?);
    }
    let index = match recorded.index {
        Some((key, offset, format)) => {
            if !offset.is_multiple_of(format.bytes()) {
                return Err(Error::Misaligned);
            }
            Some((source(key, offset, BufferUsage::INDEX)?, format))
        }
        None => None,
    };
    check_viewport(info, &recorded.viewport)?;
    let scissor = recorded.scissor;
    if scissor.x.checked_add(scissor.width).is_none()
        || scissor.y.checked_add(scissor.height).is_none()
    {
        return Err(Error::OutOfRange);
    }
    let ranges_hold = match &recorded.count {
        Count::Vertices {
            vertices,
            instances,
        } => vertices.start <= vertices.end && instances.start <= instances.end,
        Count::Indices {
            indices, instances, ..
        } => indices.start <= indices.end && instances.start <= instances.end,
    };
    if !ranges_hold || !recorded.blend_constant.iter().all(|c| c.is_finite()) {
        return Err(Error::OutOfRange);
    }
    Ok(DrawWork {
        pipeline,
        sets,
        push: recorded.push.clone(),
        vertex_buffers,
        index,
        viewport: recorded.viewport,
        scissor,
        stencil_reference: recorded.stencil_reference,
        blend_constant: recorded.blend_constant,
        count: recorded.count.clone(),
    })
}

/// Refuse a viewport that is not finite, of no width or height, past the
/// device's largest or bounds, or of depths outside `0.0..=1.0`.
fn check_viewport(info: &DeviceInfo, viewport: &Viewport) -> Result<(), Error> {
    let limits = &info.limits;
    let Viewport {
        x,
        y,
        width,
        height,
        min_depth,
        max_depth,
    } = *viewport;
    let finite = [x, y, width, height, min_depth, max_depth]
        .iter()
        .all(|value| value.is_finite());
    let [low, high] = limits.viewport_bounds_range.map(f64::from);
    let [widest, tallest] = limits.max_viewport_dimensions.map(f64::from);
    let bounded = |start: f32, len: f32| {
        let (start, end) = (f64::from(start), f64::from(start) + f64::from(len));
        start >= low && start <= high && end >= low && end <= high
    };
    let depths = (0.0..=1.0).contains(&min_depth) && (0.0..=1.0).contains(&max_depth);
    if finite
        && width > 0.0
        && height.abs() > 0.0
        && f64::from(width) <= widest
        && f64::from(height.abs()) <= tallest
        && bounded(x, width)
        && bounded(y, height)
        && depths
    {
        Ok(())
    } else {
        Err(Error::OutOfRange)
    }
}

#[cfg(test)]
#[path = "pass_tests.rs"]
mod tests;
