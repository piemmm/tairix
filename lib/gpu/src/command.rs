//! Commands as a command buffer records them, and the checked operations a
//! submission resolves them to.

use alloc::sync::Arc;
use alloc::vec::Vec;

use crate::binding::BindSet;
use crate::device::{owned, CommandBuffer};
use crate::image::{BufferImageCopy, ClearValue, ImageCopy, ImageRange};
use crate::info::{BufferUsage, DeviceInfo, QueueCapabilities};
use crate::memory::{Charge, Memory};
use crate::model::{
    exhausted, shared_bytes, BindSetKey, BufferKey, BufferState, ComputePipelineKey, ContextState,
    ImageKey,
};
use crate::pass::{PassRecord, PassWork};
use crate::resource::ImageState;
use crate::software::dispatch::{Dispatch, Groups};
use crate::transfer::{self, Direction};
use crate::{Error, MAX_COPY_REGIONS, MAX_UPDATE_BYTES};

/// One span a copy moves: `size` bytes from `src_offset` in its source buffer
/// to `dst_offset` in its destination, each a whole number of words.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct CopyRegion {
    /// Where the bytes start in the source buffer.
    pub src_offset: u64,
    /// Where they land in the destination buffer.
    pub dst_offset: u64,
    /// How many there are.
    pub size: u64,
}

/// An update's bytes, charged to their context from when they are recorded
/// until the command buffer and every submission of it have let them go, so
/// a buffer submitted again is charged for them once.
#[derive(Debug)]
pub(crate) struct Recorded {
    pub(crate) bytes: Vec<u8>,
    _charge: Charge,
}

impl Recorded {
    pub(crate) const fn new(bytes: Vec<u8>, charge: Charge) -> Self {
        Self {
            bytes,
            _charge: charge,
        }
    }

    /// What `bytes`, once recorded, keep on the device.
    pub(crate) fn held(bytes: &Vec<u8>) -> u64 {
        shared_bytes::<Self>().saturating_add(tairix_util::fallible::held(bytes))
    }
}

/// A command as recorded: buffers by key, checked only when submitted.
#[derive(Debug)]
pub(crate) enum Command {
    Copy {
        src: BufferKey,
        dst: BufferKey,
        regions: Vec<CopyRegion>,
    },
    Fill {
        dst: BufferKey,
        offset: u64,
        size: u64,
        word: u32,
    },
    Update {
        dst: BufferKey,
        offset: u64,
        data: Arc<Recorded>,
    },
    /// `overlapping` where two regions write one texel, decided as it was
    /// recorded.
    BufferToImage {
        src: BufferKey,
        dst: ImageKey,
        regions: Vec<BufferImageCopy>,
        overlapping: bool,
    },
    /// `overlapping` where two regions write one byte.
    ImageToBuffer {
        src: ImageKey,
        dst: BufferKey,
        regions: Vec<BufferImageCopy>,
        overlapping: bool,
    },
    /// `overlapping` where two regions write one texel, or one reads a
    /// texel of the same image another writes.
    CopyImage {
        src: ImageKey,
        dst: ImageKey,
        regions: Vec<ImageCopy>,
        overlapping: bool,
    },
    ClearImage {
        image: ImageKey,
        value: ClearValue,
        ranges: Vec<ImageRange>,
    },
    Dispatch(DispatchRecord),
    Pass(alloc::boxed::Box<PassRecord>),
}

/// A dispatch as it is recorded, its objects named by key.
#[derive(Debug)]
pub(crate) struct DispatchRecord {
    pub(crate) pipeline: ComputePipelineKey,
    pub(crate) sets: Vec<BindSetKey>,
    /// The push constants' whole words, shared by every submission of it;
    /// `None` for none.
    pub(crate) push: Option<Arc<Vec<u32>>>,
    pub(crate) push_bytes: usize,
    pub(crate) groups: Count,
}

/// How many workgroups a recorded dispatch makes: given, or read from a
/// buffer when it runs.
#[derive(Copy, Clone, Debug)]
pub(crate) enum Count {
    Direct([u32; 3]),
    Indirect { buffer: BufferKey, offset: u64 },
}

impl Command {
    /// The work a queue's family must run to take it.
    pub(crate) const fn needs(&self) -> QueueCapabilities {
        match self {
            Self::Copy { .. }
            | Self::Fill { .. }
            | Self::Update { .. }
            | Self::BufferToImage { .. }
            | Self::ImageToBuffer { .. }
            | Self::CopyImage { .. }
            | Self::ClearImage { .. } => QueueCapabilities::TRANSFER,
            Self::Dispatch(_) => QueueCapabilities::COMPUTE,
            Self::Pass(_) => QueueCapabilities::GRAPHICS,
        }
    }

    /// How many commands it counts as: a pass one, and each of its draws
    /// another.
    pub(crate) fn count(&self) -> usize {
        match self {
            Self::Pass(pass) => pass.draws.len().saturating_add(1),
            _ => 1,
        }
    }
}

/// Why a command was refused, and which of the commands it counts as: a
/// pass's draws follow it.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Refused {
    pub(crate) at: usize,
    pub(crate) error: Error,
}

impl From<Error> for Refused {
    fn from(error: Error) -> Self {
        Self { at: 0, error }
    }
}

/// The keys of `sets`, each of the buffer's context.
pub(crate) fn set_keys(
    buffer: &CommandBuffer,
    sets: &[&BindSet],
) -> Result<Vec<BindSetKey>, Error> {
    let mut keys = Vec::new();
    keys.try_reserve_exact(sets.len()).map_err(exhausted)?;
    for set in sets {
        owned(&set.shared, set.context, &buffer.shared, buffer.context)?;
        keys.push(set.key);
    }
    Ok(keys)
}

/// `bytes` of push constants as their whole words, decoded once: a length of
/// no whole words never matches a layout's, which holds whole words, so it is
/// refused at submission.
pub(crate) fn push_words(bytes: &[u8]) -> Result<Option<Arc<Vec<u32>>>, Error> {
    let (words, _) = bytes.as_chunks::<4>();
    if words.is_empty() {
        return Ok(None);
    }
    let words = tairix_util::fallible::collected(
        words.len(),
        words.iter().map(|bytes| u32::from_le_bytes(*bytes)),
    )
    .ok_or(Error::OutOfMemory)?;
    Ok(Some(crate::shared(words)?))
}

/// One span of a resolved copy, by offset into the memories themselves.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Span {
    pub(crate) src: u64,
    pub(crate) dst: u64,
    pub(crate) len: u64,
}

/// A command whose buffers resolved and whose every range was proved within
/// them: offsets are into the memories, not the buffers.
#[derive(Debug)]
pub(crate) enum Op {
    Copy {
        src: Arc<Memory>,
        dst: Arc<Memory>,
        spans: Vec<Span>,
    },
    Fill {
        dst: Arc<Memory>,
        offset: u64,
        len: u64,
        word: u32,
    },
    Update {
        dst: Arc<Memory>,
        offset: u64,
        data: Arc<Recorded>,
    },
    BufferImage {
        buffer: Arc<BufferState>,
        image: Arc<ImageState>,
        regions: Vec<BufferImageCopy>,
        direction: Direction,
    },
    CopyImage {
        src: Arc<ImageState>,
        dst: Arc<ImageState>,
        regions: Vec<ImageCopy>,
    },
    ClearImage {
        image: Arc<ImageState>,
        value: ClearValue,
        ranges: Vec<ImageRange>,
    },
    Dispatch(alloc::boxed::Box<Dispatch>),
    Pass(alloc::boxed::Box<PassWork>),
}

impl core::fmt::Debug for Dispatch {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Dispatch").finish_non_exhaustive()
    }
}

impl Op {
    /// The host bytes it holds past its own size while queued.
    pub(crate) fn held_bytes(&self) -> u64 {
        use tairix_util::fallible::held;
        match self {
            Self::Copy { spans, .. } => held(spans),
            // An update's bytes are charged with its recording.
            Self::Fill { .. } | Self::Update { .. } => 0,
            Self::BufferImage { regions, .. } => held(regions),
            Self::CopyImage { regions, .. } => held(regions),
            Self::ClearImage { ranges, .. } => held(ranges),
            Self::Dispatch(dispatch) => dispatch.held_bytes(),
            Self::Pass(pass) => pass.held_bytes(),
        }
    }

    /// The host bytes the tables of its run take on a device of
    /// `participants`, made as it starts and let go as it ends.
    pub(crate) fn run_bytes(&self, participants: usize) -> u64 {
        match self {
            Self::Dispatch(dispatch) => dispatch.run_bytes(participants),
            Self::Pass(pass) => crate::software::render::run_bytes(pass, participants),
            _ => 0,
        }
    }

    /// The host bytes its run's placements keep for its job on a device of
    /// `participants`.
    pub(crate) fn place_bytes(&self, participants: usize) -> u64 {
        match self {
            Self::Dispatch(dispatch) => dispatch.place_bytes(participants),
            _ => 0,
        }
    }
}

/// The buffer `key` names, made for `usage`.
pub(crate) fn buffer(
    context: &ContextState,
    key: BufferKey,
    usage: BufferUsage,
) -> Result<&Arc<BufferState>, Error> {
    let buffer = context.buffers.get(key).ok_or(Error::Stale)?;
    if buffer.usage.contains(usage) {
        Ok(buffer)
    } else {
        Err(Error::Usage)
    }
}

/// Where `len` bytes at `offset` into `buffer` lie in its memory, once proved
/// within it.
fn within(buffer: &BufferState, offset: u64, len: u64) -> Result<u64, Error> {
    if len == 0 {
        return Err(Error::Empty);
    }
    match offset.checked_add(len) {
        Some(end) if end <= buffer.size => Ok(buffer.offset + offset),
        _ => Err(Error::OutOfRange),
    }
}

/// Refuse a copy, fill or update whose offset or length is not whole words.
fn words(offset: u64, len: u64) -> Result<(), Error> {
    if offset.is_multiple_of(4) && len.is_multiple_of(4) {
        Ok(())
    } else {
        Err(Error::Misaligned)
    }
}

/// Check `command` against the objects of `context` it names and the
/// device `info` describes, resolving it to the operation a device runs.
pub(crate) fn resolve(
    info: &DeviceInfo,
    context: &ContextState,
    command: &Command,
) -> Result<Op, Refused> {
    match command {
        Command::Pass(pass) => crate::pass::resolve(info, context, pass),
        other => resolve_one(info, context, other).map_err(Refused::from),
    }
}

/// [`resolve`] for a command that counts as one.
fn resolve_one(info: &DeviceInfo, context: &ContextState, command: &Command) -> Result<Op, Error> {
    match command {
        Command::Copy { src, dst, regions } => {
            let src = buffer(context, *src, BufferUsage::TRANSFER_SRC)?;
            let dst = buffer(context, *dst, BufferUsage::TRANSFER_DST)?;
            if regions.is_empty() {
                return Err(Error::Empty);
            }
            if regions.len() > MAX_COPY_REGIONS {
                return Err(Error::TooMany);
            }
            let mut spans = Vec::new();
            spans
                .try_reserve_exact(regions.len())
                .map_err(|_| Error::OutOfMemory)?;
            for region in regions {
                let span = Span {
                    src: within(src, region.src_offset, region.size)?,
                    dst: within(dst, region.dst_offset, region.size)?,
                    len: region.size,
                };
                words(region.src_offset, region.size)?;
                words(region.dst_offset, region.size)?;
                spans.push(span);
            }
            refuse_overlap(&spans, Arc::ptr_eq(&src.memory, &dst.memory))?;
            Ok(Op::Copy {
                src: Arc::clone(&src.memory),
                dst: Arc::clone(&dst.memory),
                spans,
            })
        }
        Command::Fill {
            dst,
            offset,
            size,
            word,
        } => {
            let dst = buffer(context, *dst, BufferUsage::TRANSFER_DST)?;
            let at = within(dst, *offset, *size)?;
            words(*offset, *size)?;
            Ok(Op::Fill {
                dst: Arc::clone(&dst.memory),
                offset: at,
                len: *size,
                word: *word,
            })
        }
        Command::Update { dst, offset, data } => {
            let dst = buffer(context, *dst, BufferUsage::TRANSFER_DST)?;
            if data.bytes.len() > MAX_UPDATE_BYTES {
                return Err(Error::TooMany);
            }
            let len = data.bytes.len() as u64;
            let at = within(dst, *offset, len)?;
            words(*offset, len)?;
            Ok(Op::Update {
                dst: Arc::clone(&dst.memory),
                offset: at,
                data: Arc::clone(data),
            })
        }
        Command::BufferToImage {
            src,
            dst,
            regions,
            overlapping,
        } => transfer::buffer_image(
            context,
            (*src, *dst),
            (regions, *overlapping),
            Direction::ToImage,
        ),
        Command::ImageToBuffer {
            src,
            dst,
            regions,
            overlapping,
        } => transfer::buffer_image(
            context,
            (*dst, *src),
            (regions, *overlapping),
            Direction::ToBuffer,
        ),
        Command::CopyImage {
            src,
            dst,
            regions,
            overlapping,
        } => transfer::image_image(context, (*src, *dst), (regions, *overlapping)),
        Command::ClearImage {
            image,
            value,
            ranges,
        } => transfer::clear(context, *image, *value, ranges),
        Command::Dispatch(recorded) => dispatch(info, context, recorded),
        Command::Pass(_) => Err(Error::Malformed),
    }
}

/// A dispatch checked whole: its pipeline live, each bind set live and of
/// the layout's set's bindings, its push constants exactly the layout's, and
/// its workgroup counts within the device's or its indirect arguments within
/// a buffer made for them.
fn dispatch(
    info: &DeviceInfo,
    context: &ContextState,
    recorded: &DispatchRecord,
) -> Result<Op, Error> {
    let DispatchRecord {
        pipeline,
        sets,
        push,
        push_bytes,
        groups,
    } = recorded;
    let pipeline = Arc::clone(
        context
            .compute_pipelines
            .table
            .get(*pipeline)
            .ok_or(Error::Stale)?,
    );
    let layout = &pipeline.layout;
    if sets.len() != layout.sets.len() {
        return Err(Error::Bindings);
    }
    let mut held = Vec::new();
    held.try_reserve_exact(sets.len())
        .map_err(|_| Error::OutOfMemory)?;
    for (key, wanted) in sets.iter().zip(&layout.sets) {
        let set = context.bind_sets.get(*key).ok_or(Error::Stale)?;
        if !set.layout.serves(wanted) {
            return Err(Error::Bindings);
        }
        held.push(Arc::clone(set));
    }
    if *push_bytes != layout.push_constants.bytes as usize {
        return Err(Error::Shape);
    }
    let groups = match *groups {
        Count::Direct(counts) => {
            let within = counts
                .iter()
                .zip(&info.limits.max_workgroup_count)
                .all(|(count, most)| count <= most);
            if !within {
                return Err(Error::TooLarge);
            }
            Groups::Direct(counts)
        }
        Count::Indirect {
            buffer: key,
            offset,
        } => {
            let buffer = buffer(context, key, BufferUsage::INDIRECT)?;
            if !offset.is_multiple_of(4) {
                return Err(Error::Misaligned);
            }
            let at = within(buffer, offset, 12)?;
            Groups::Indirect {
                memory: Arc::clone(&buffer.memory),
                offset: at,
            }
        }
    };
    let work = alloc::boxed::Box::try_new(Dispatch {
        pipeline,
        sets: held,
        push: push.clone(),
        groups,
    })
    .map_err(|_| Error::OutOfMemory)?;
    Ok(Op::Dispatch(work))
}

/// Refuse a copy writing any byte twice, or, within one memory, writing a
/// byte it also reads: either would make what lands depend on the order the
/// spans run in.
fn refuse_overlap(spans: &[Span], one_memory: bool) -> Result<(), Error> {
    if let [only] = spans {
        let apart = only.dst + only.len <= only.src || only.src + only.len <= only.dst;
        return if !one_memory || apart {
            Ok(())
        } else {
            Err(Error::Overlap)
        };
    }
    let mut ranges = Vec::new();
    let count = if one_memory {
        spans.len() * 2
    } else {
        spans.len()
    };
    ranges
        .try_reserve_exact(count)
        .map_err(|_| Error::OutOfMemory)?;
    for span in spans {
        ranges.push((span.dst, span.dst + span.len, true));
        if one_memory {
            ranges.push((span.src, span.src + span.len, false));
        }
    }
    ranges.sort_unstable_by_key(|&(start, _, _)| start);
    let (mut read_end, mut write_end) = (0, 0);
    for (start, end, writes) in ranges {
        let clash = if writes {
            start < read_end || start < write_end
        } else {
            start < write_end
        };
        if clash {
            return Err(Error::Overlap);
        }
        if writes {
            write_end = write_end.max(end);
        } else {
            read_end = read_end.max(end);
        }
    }
    Ok(())
}
