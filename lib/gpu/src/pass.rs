//! Render passes: the attachments a pass renders to, how each is loaded and
//! stored, and the draws recorded into it, each a self-describing call;
//! checked whole when a submission carries it.

use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::ops::Range;

use tairix_collections::{Key, SlotKey};

use crate::backend::Backend;
use crate::binding::{identity, BindSet, BindSetState, Touched};
use crate::command::{buffer, push_words, put_push, set_keys, take_push, Command, Op, Refused};
use crate::device::{Buffer, CommandBuffer};
use crate::format::{Aspects, Format, FormatFeatures};
use crate::host::owned;
use crate::image::{ClearValue, ImageUsage, ViewDimension};
use crate::info::{BufferUsage, DeviceInfo};
use crate::model::{BindSetKey, BufferKey, ContextState, RenderPipelineKey, ViewKey};
use crate::objects::ImageView;
use crate::render::{FixedFunction, RenderPipeline, RenderPipelineState};
use crate::resource::ViewState;
use crate::wire::{put_key, take_key, Reader, Wire, Writer};
use crate::{Error, MAX_DISPATCH_SETS, MAX_PUSH_BYTES, MAX_SUBMISSION_COMMANDS};

/// The most colour attachments one pass records: past every device's.
pub const MAX_PASS_ATTACHMENTS: usize = 8;

/// The most vertex buffers one draw binds: past every device's.
pub const MAX_DRAW_VERTEX_BUFFERS: usize = 32;

/// The bytes a record of [`Drawer::draw_indirect`] takes: its vertices'
/// count, its instances' count, its first vertex and its first instance, each
/// a little-endian word.
pub const DRAW_RECORD_BYTES: u32 = 16;

/// The bytes a record of [`Drawer::draw_indexed_indirect`] takes: its
/// indices' count, its instances' count, its first index, the signed word
/// added to each index, and its first instance, each a little-endian word.
pub const INDEXED_DRAW_RECORD_BYTES: u32 = 20;

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

/// Where an indirect draw's records lie: `count` of them in `buffer`, the
/// first at `offset` and each `stride` bytes past the one before.
#[derive(Copy, Clone, Debug)]
pub struct Indirect<'a> {
    /// The buffer holding them, made for indirect use.
    pub buffer: &'a Buffer,
    /// Where the first starts: whole words.
    pub offset: u64,
    /// How many there are: at most the device's
    /// [`max_draw_indirect_count`](crate::Limits::max_draw_indirect_count).
    pub count: u32,
    /// The bytes from one record to the next: where there are several, whole
    /// words and at least a record.
    pub stride: u32,
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

/// How many vertices and instances a draw makes, as recorded: given, or read
/// from records when it runs.
#[derive(Debug)]
pub(crate) enum Count {
    /// Its positions — vertices, or for an indexed draw indices — each index
    /// added to `base_vertex`, and its instances.
    Direct {
        positions: Range<u32>,
        base_vertex: i32,
        instances: Range<u32>,
    },
    Indirect {
        buffer: BufferKey,
        offset: u64,
        count: u32,
        stride: u32,
    },
}

/// One draw's positions and instances: `positions` vertices numbered from
/// `first`, or for an indexed draw the vertices `positions` indices from index
/// `first` name, each added to `base_vertex`; and `instances` instances
/// numbered from `first_instance`. Vertex and instance numbers wrap.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct Counts {
    pub(crate) first: u32,
    pub(crate) positions: u32,
    pub(crate) base_vertex: i32,
    pub(crate) first_instance: u32,
    pub(crate) instances: u32,
}

impl Counts {
    /// What a record of `words` names, the first four of them a draw of
    /// vertices' and all five an indexed draw's.
    pub(crate) const fn of_record(words: [u32; 5], indexed: bool) -> Self {
        let [positions, instances, first, fourth, fifth] = words;
        if indexed {
            Self {
                first,
                positions,
                base_vertex: fourth.cast_signed(),
                first_instance: fifth,
                instances,
            }
        } else {
            Self {
                first,
                positions,
                base_vertex: 0,
                first_instance: fourth,
                instances,
            }
        }
    }
}

/// Where an indirect draw's records lie in their memory, proved within it:
/// `count` from byte `start`, `stride` apart, the `span` bytes from `start`
/// they reach.
pub(crate) struct Records<B: Backend> {
    pub(crate) memory: Arc<B::Memory>,
    pub(crate) start: u64,
    pub(crate) span: u64,
    pub(crate) count: u32,
    pub(crate) stride: u64,
}

/// How many vertices and instances a checked draw makes: once, as recorded,
/// or once for each of its records, read as the draw runs.
pub(crate) enum Counted<B: Backend> {
    Direct(Counts),
    Indirect(Records<B>),
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

impl PassRecord {
    /// Its draws, in order.
    pub(crate) fn draws(&self) -> &[DrawRecord] {
        &self.draws
    }

    /// Append `draws`, uploaded after the pass they continue.
    pub(crate) fn extend(&mut self, draws: &mut Vec<DrawRecord>) -> Result<(), Error> {
        self.draws
            .try_reserve(draws.len())
            .map_err(|_| Error::OutOfMemory)?;
        self.draws.append(draws);
        Ok(())
    }

    /// Write it with only its first `draws` draws.
    pub(crate) fn put_first(&self, writer: &mut Writer<'_>, draws: usize) -> Result<(), Error> {
        self.put_head(writer)?;
        writer.list(
            self.draws.get(..draws).ok_or(Error::Malformed)?,
            |writer, draw| draw.put(writer),
        )
    }

    pub(crate) fn put(&self, writer: &mut Writer<'_>) -> Result<(), Error> {
        self.put_first(writer, self.draws.len())
    }

    fn put_head(&self, writer: &mut Writer<'_>) -> Result<(), Error> {
        writer.list(&self.colour, |writer, attachment| {
            put_key(writer, attachment.view)?;
            attachment
                .resolve
                .map(|view| view.slot().to_bits())
                .put(writer)?;
            attachment.load.put(writer)
        })?;
        match &self.depth_stencil {
            None => false.put(writer)?,
            Some(attachment) => {
                true.put(writer)?;
                put_key(writer, attachment.view)?;
                attachment.depth.put(writer)?;
                attachment.stencil.put(writer)?;
            }
        }
        self.area.put(writer)
    }

    /// A pass whose draws, with the pass itself, count as at most `room`
    /// commands.
    pub(crate) fn take(reader: &mut Reader<'_>, room: usize) -> Result<Self, Error> {
        let colour = reader.list(MAX_PASS_ATTACHMENTS, |reader| {
            Ok(AttachmentRecord {
                view: take_key(reader)?,
                resolve: <Option<u64> as Wire>::take(reader)?
                    .map(|bits| {
                        SlotKey::from_bits(bits)
                            .map(ViewKey::from_slot)
                            .ok_or(Error::Malformed)
                    })
                    .transpose()?,
                load: Wire::take(reader)?,
            })
        })?;
        let depth_stencil = if bool::take(reader)? {
            Some(DepthStencilRecord {
                view: take_key(reader)?,
                depth: Wire::take(reader)?,
                stencil: Wire::take(reader)?,
            })
        } else {
            None
        };
        let area = Wire::take(reader)?;
        let draws = take_draws(reader, room.saturating_sub(1))?;
        Ok(Self {
            colour,
            depth_stencil,
            area,
            draws,
        })
    }
}

/// Draws, at most `room` of them.
pub(crate) fn take_draws(reader: &mut Reader<'_>, room: usize) -> Result<Vec<DrawRecord>, Error> {
    reader.list(room, DrawRecord::take)
}

impl DrawRecord {
    pub(crate) fn put(&self, writer: &mut Writer<'_>) -> Result<(), Error> {
        put_key(writer, self.pipeline)?;
        writer.list(&self.sets, |writer, &set| put_key(writer, set))?;
        put_push(writer, self.push.as_deref(), self.push_bytes)?;
        writer.list(&self.vertex_buffers, |writer, &(buffer, offset)| {
            put_key(writer, buffer)?;
            offset.put(writer)
        })?;
        match self.index {
            None => false.put(writer)?,
            Some((buffer, offset, format)) => {
                true.put(writer)?;
                put_key(writer, buffer)?;
                offset.put(writer)?;
                format.put(writer)?;
            }
        }
        self.viewport.put(writer)?;
        self.scissor.put(writer)?;
        self.stencil_reference.put(writer)?;
        self.blend_constant.put(writer)?;
        match &self.count {
            Count::Direct {
                positions,
                base_vertex,
                instances,
            } => {
                false.put(writer)?;
                positions.start.put(writer)?;
                positions.end.put(writer)?;
                base_vertex.put(writer)?;
                instances.start.put(writer)?;
                instances.end.put(writer)
            }
            &Count::Indirect {
                buffer,
                offset,
                count,
                stride,
            } => {
                true.put(writer)?;
                put_key(writer, buffer)?;
                offset.put(writer)?;
                count.put(writer)?;
                stride.put(writer)
            }
        }
    }

    fn take(reader: &mut Reader<'_>) -> Result<Self, Error> {
        let pipeline = take_key(reader)?;
        let sets = reader.list(MAX_DISPATCH_SETS, take_key)?;
        let (push, push_bytes) = take_push(reader)?;
        let vertex_buffers = reader.list(MAX_DRAW_VERTEX_BUFFERS, |reader| {
            Ok((take_key(reader)?, u64::take(reader)?))
        })?;
        let index = if bool::take(reader)? {
            Some((take_key(reader)?, u64::take(reader)?, Wire::take(reader)?))
        } else {
            None
        };
        let viewport = Wire::take(reader)?;
        let scissor = Wire::take(reader)?;
        let stencil_reference = Wire::take(reader)?;
        let blend_constant = Wire::take(reader)?;
        let count = if bool::take(reader)? {
            Count::Indirect {
                buffer: take_key(reader)?,
                offset: Wire::take(reader)?,
                count: Wire::take(reader)?,
                stride: Wire::take(reader)?,
            }
        } else {
            Count::Direct {
                positions: u32::take(reader)?..u32::take(reader)?,
                base_vertex: Wire::take(reader)?,
                instances: u32::take(reader)?..u32::take(reader)?,
            }
        };
        Ok(Self {
            pipeline,
            sets,
            push,
            push_bytes,
            vertex_buffers,
            index,
            viewport,
            scissor,
            stencil_reference,
            blend_constant,
            count,
        })
    }
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
        owned(&view.host, view.context, &self.host, self.context)?;
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
            Count::Direct {
                positions: vertices,
                base_vertex: 0,
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
        let index = self.named_indices(indices)?;
        self.record(
            draw,
            Some(index),
            Count::Direct {
                positions: range,
                base_vertex,
                instances,
            },
        )
    }

    /// [`Self::draw`] once for each of `records`' records, read from its
    /// buffer as the draw runs: four words, [`DRAW_RECORD_BYTES`]. A record
    /// naming a first instance on a device that does not state
    /// [`Features::DRAW_INDIRECT_FIRST_INSTANCE`](crate::Features::DRAW_INDIRECT_FIRST_INSTANCE)
    /// loses the context.
    ///
    /// # Errors
    ///
    /// As [`Self::draw`].
    pub fn draw_indirect(&mut self, draw: &Draw<'_>, records: &Indirect<'_>) -> Result<(), Error> {
        let count = self.indirect(records)?;
        self.record(draw, None, count)
    }

    /// [`Self::draw_indexed`] once for each of `records`' records, read as
    /// [`Self::draw_indirect`]'s are: five words,
    /// [`INDEXED_DRAW_RECORD_BYTES`].
    ///
    /// # Errors
    ///
    /// As [`Self::draw`].
    pub fn draw_indexed_indirect(
        &mut self,
        draw: &Draw<'_>,
        indices: &Indices<'_>,
        records: &Indirect<'_>,
    ) -> Result<(), Error> {
        let index = self.named_indices(indices)?;
        let count = self.indirect(records)?;
        self.record(draw, Some(index), count)
    }

    fn named_indices(&self, indices: &Indices<'_>) -> Result<(BufferKey, u64, IndexFormat), Error> {
        Ok((
            self.buffer.named(indices.buffer)?,
            indices.offset,
            indices.format,
        ))
    }

    fn indirect(&self, records: &Indirect<'_>) -> Result<Count, Error> {
        Ok(Count::Indirect {
            buffer: self.buffer.named(records.buffer)?,
            offset: records.offset,
            count: records.count,
            stride: records.stride,
        })
    }

    fn record(
        &mut self,
        draw: &Draw<'_>,
        index: Option<(BufferKey, u64, IndexFormat)>,
        count: Count,
    ) -> Result<(), Error> {
        let buffer = self.buffer;
        owned(
            &draw.pipeline.host,
            draw.pipeline.context,
            &buffer.host,
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
pub(crate) struct Target<B: Backend> {
    pub(crate) view: Arc<ViewState<B>>,
    pub(crate) resolve: Option<Arc<ViewState<B>>>,
    pub(crate) load: Load,
}

/// A pass's depth-stencil attachment, checked, and its aspects' loads.
pub(crate) struct DepthTarget<B: Backend> {
    pub(crate) view: Arc<ViewState<B>>,
    pub(crate) depth: Load,
    pub(crate) stencil: Load,
}

/// Where a draw's vertex attributes or indices are read from: `size` bytes
/// of a memory from byte `start`.
pub(crate) struct Source<B: Backend> {
    pub(crate) memory: Arc<B::Memory>,
    pub(crate) start: u64,
    pub(crate) size: u64,
}

/// A draw, checked.
pub(crate) struct DrawWork<B: Backend> {
    pub(crate) pipeline: Arc<RenderPipelineState<B>>,
    pub(crate) sets: Vec<Arc<BindSetState<B>>>,
    pub(crate) push: Option<Arc<Vec<u32>>>,
    pub(crate) vertex_buffers: Vec<Source<B>>,
    pub(crate) index: Option<(Source<B>, IndexFormat)>,
    pub(crate) viewport: Viewport,
    pub(crate) scissor: Rect,
    pub(crate) stencil_reference: u8,
    pub(crate) blend_constant: [f32; 4],
    pub(crate) count: Counted<B>,
}

impl<B: Backend> DrawWork<B> {
    /// Each span of memory it reads as vertices, indices or records.
    fn sources(&self) -> impl Iterator<Item = (&Arc<B::Memory>, Range<u64>)> {
        fn read<B: Backend>(source: &Source<B>) -> (&Arc<B::Memory>, Range<u64>) {
            (
                &source.memory,
                source.start..source.start.saturating_add(source.size),
            )
        }
        let records = match &self.count {
            Counted::Indirect(records) => Some((
                &records.memory,
                records.start..records.start.saturating_add(records.span),
            )),
            Counted::Direct(_) => None,
        };
        self.vertex_buffers
            .iter()
            .chain(self.index.as_ref().map(|(source, _)| source))
            .map(read)
            .chain(records)
    }
}

/// A pass checked whole: its attachments of one extent and sample count, its
/// area within them, and each draw's pipeline rendering to them.
pub(crate) struct PassWork<B: Backend> {
    pub(crate) colour: Vec<Target<B>>,
    pub(crate) depth_stencil: Option<DepthTarget<B>>,
    pub(crate) area: Rect,
    pub(crate) samples: u32,
    pub(crate) draws: Vec<DrawWork<B>>,
}

impl<B: Backend> core::fmt::Debug for PassWork<B> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("PassWork").finish_non_exhaustive()
    }
}

impl<B: Backend> PassWork<B> {
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
fn attachment<'c, B: Backend>(
    (info, context): (&DeviceInfo, &'c ContextState<B>),
    key: ViewKey,
    (usage, feature): (ImageUsage, FormatFeatures),
) -> Result<&'c Arc<ViewState<B>>, Error> {
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
fn framed<B: Backend>(view: &ViewState<B>) -> (u32, u32, u32) {
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

/// What an attachment's one level and layer holds of its image.
pub(crate) fn touched<B: Backend>(view: &ViewState<B>) -> Touched {
    let image = &view.image;
    let range = &view.desc.range;
    Touched {
        memory: identity(&image.memory),
        placement: image.offset..image.offset.saturating_add(image.size),
        image: identity(image),
        aspects: range.aspects,
        level: range.base_level,
        layer: range.base_layer,
    }
}

/// A pass's attachments checked: its colour targets, its depth-stencil, and
/// their one extent and sample count.
type Attachments<B> = (Vec<Target<B>>, Option<DepthTarget<B>>, (u32, u32, u32));

/// The colour and depth-stencil attachments `record` names, each made for
/// its use and of one extent and sample count with the rest, and that extent
/// and sample count.
fn attachments<B: Backend>(
    info: &DeviceInfo,
    context: &ContextState<B>,
    record: &PassRecord,
) -> Result<Attachments<B>, Error> {
    let mut frame = None;
    let mut agree = |view: &ViewState<B>| -> Result<(), Error> {
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
fn resolvable<B: Backend>(
    info: &DeviceInfo,
    view: &ViewState<B>,
    destination: &ViewState<B>,
) -> Result<(), Error> {
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

/// What each attachment holds of its image, colour targets' and their
/// resolves' first and the depth-stencil's last, refusing any that meets
/// another.
fn written<B: Backend>(
    colour: &[Target<B>],
    depth_stencil: Option<&DepthTarget<B>>,
) -> Result<Vec<Touched>, Error> {
    let mut written: Vec<Touched> = Vec::new();
    written
        .try_reserve_exact(colour.len() * 2 + 1)
        .map_err(|_| Error::OutOfMemory)?;
    for target in colour {
        written.push(touched(&target.view));
        if let Some(destination) = &target.resolve {
            written.push(touched(destination));
        }
    }
    if let Some(depth) = depth_stencil {
        written.push(touched(&depth.view));
    }
    for (at, attachment) in written.iter().enumerate() {
        let twice = written
            .get(..at)
            .unwrap_or(&[])
            .iter()
            .any(|other| other.meets(attachment));
        if twice {
            return Err(Error::Overlap);
        }
    }
    Ok(written)
}

/// Refuse the first of `draws` reaching what its pass writes: a descriptor
/// its stages see, or the vertices, indices or records it reads, reaching an
/// attachment in `written` — and a descriptor its stages may write reaching
/// the depth-stencil attachment, where only `tested`.
fn refuse_reaches<B: Backend>(
    draws: &[DrawWork<B>],
    (written, tested): (&[Touched], Option<&Touched>),
) -> Result<(), Refused> {
    let mut before: &[Arc<BindSetState<B>>] = &[];
    for (at, draw) in draws.iter().enumerate() {
        let sets_reach = draw.sets.iter().enumerate().any(|(index, set)| {
            // Checked for the draw before, which bound it in the same place.
            if before.get(index).is_some_and(|held| Arc::ptr_eq(held, set)) {
                return false;
            }
            written
                .iter()
                .any(|attachment| set.reach.meets(attachment, false))
                || tested.is_some_and(|attachment| set.reach.meets(attachment, true))
        });
        let buffers_reach = draw.sources().any(|(source, read)| {
            let source = identity(source);
            written
                .iter()
                .any(|attachment| attachment.meets_bytes(source, &read))
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
pub(crate) fn resolve<B: Backend>(
    info: &DeviceInfo,
    context: &ContextState<B>,
    record: &PassRecord,
) -> Result<Op<B>, Refused> {
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
    let spans = written(&colour, depth_stencil.as_ref())?;
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
                .any(|draw| writes_depth_stencil(&draw.pipeline.stages.fixed))
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
fn writes_depth_stencil(render: &FixedFunction) -> bool {
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
fn check_draw<B: Backend>(
    info: &DeviceInfo,
    context: &ContextState<B>,
    (colour, depth_stencil, samples): (&[Target<B>], Option<&DepthTarget<B>>, u32),
    recorded: &DrawRecord,
) -> Result<DrawWork<B>, Error> {
    let pipeline = Arc::clone(
        context
            .render_pipelines
            .table
            .get(recorded.pipeline)
            .ok_or(Error::Stale)?,
    );
    let render = &pipeline.stages.fixed;
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
    let source = |key: BufferKey, offset: u64, usage: BufferUsage| -> Result<Source<B>, Error> {
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
    if !recorded.blend_constant.iter().all(|c| c.is_finite()) {
        return Err(Error::OutOfRange);
    }
    let count = counted(info, context, &recorded.count, index.is_some())?;
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
        count,
    })
}

/// A draw's count checked: its ranges each in order; or its records whole
/// words into a buffer made for them, whole words apart, a record or more
/// apart where there are several, within the buffer, and no more of them than
/// the device reads.
fn counted<B: Backend>(
    info: &DeviceInfo,
    context: &ContextState<B>,
    count: &Count,
    indexed: bool,
) -> Result<Counted<B>, Error> {
    let (key, offset, count, stride) = match count {
        Count::Direct {
            positions,
            base_vertex,
            instances,
        } => {
            if positions.start > positions.end || instances.start > instances.end {
                return Err(Error::OutOfRange);
            }
            return Ok(Counted::Direct(Counts {
                first: positions.start,
                positions: positions.end - positions.start,
                base_vertex: *base_vertex,
                first_instance: instances.start,
                instances: instances.end - instances.start,
            }));
        }
        &Count::Indirect {
            buffer,
            offset,
            count,
            stride,
        } => (buffer, offset, count, stride),
    };
    let held = buffer(context, key, BufferUsage::INDIRECT)?;
    let several = count > 1;
    if !offset.is_multiple_of(4) || (several && !stride.is_multiple_of(4)) {
        return Err(Error::Misaligned);
    }
    if count > info.limits.max_draw_indirect_count {
        return Err(Error::TooLarge);
    }
    let record = if indexed {
        INDEXED_DRAW_RECORD_BYTES
    } else {
        DRAW_RECORD_BYTES
    };
    if several && stride < record {
        return Err(Error::OutOfRange);
    }
    // At most 2^32 - 2 strides of under 2^32 bytes and a record: within 64 bits.
    let span = count.checked_sub(1).map_or(0, |before| {
        u64::from(before) * u64::from(stride) + u64::from(record)
    });
    if offset.checked_add(span).is_none_or(|end| end > held.size) {
        return Err(Error::OutOfRange);
    }
    Ok(Counted::Indirect(Records {
        memory: Arc::clone(&held.memory),
        start: held.offset + offset,
        span,
        count,
        stride: u64::from(stride),
    }))
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
