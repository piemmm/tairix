//! Commands as a command buffer records them, and the checked operations a
//! submission resolves them to.

use alloc::sync::Arc;
use alloc::vec::Vec;

use crate::info::BufferUsage;
use crate::memory::Memory;
use crate::model::{BufferKey, BufferState};
use crate::{Error, MAX_COPY_REGIONS, MAX_UPDATE_BYTES};

use tairix_collections::SlotMap;

/// One span a copy moves: `size` bytes from `src_offset` in its source buffer
/// to `dst_offset` in its destination.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct CopyRegion {
    /// Where the bytes start in the source buffer.
    pub src_offset: u64,
    /// Where they land in the destination buffer.
    pub dst_offset: u64,
    /// How many there are.
    pub size: u64,
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
        data: Arc<Vec<u8>>,
    },
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
        data: Arc<Vec<u8>>,
    },
}

impl Op {
    /// The bytes it writes.
    pub(crate) fn bytes(&self) -> u64 {
        match self {
            Self::Copy { spans, .. } => spans.iter().map(|span| span.len).sum(),
            Self::Fill { len, .. } => *len,
            Self::Update { data, .. } => data.len() as u64,
        }
    }
}

/// The buffer `key` names, made for `usage`.
fn buffer(
    buffers: &SlotMap<BufferKey, BufferState>,
    key: BufferKey,
    usage: BufferUsage,
) -> Result<&BufferState, Error> {
    let buffer = buffers.get(key).ok_or(Error::Stale)?;
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

/// Refuse a fill or an update whose offset or length is not whole words.
fn words(offset: u64, len: u64) -> Result<(), Error> {
    if offset.is_multiple_of(4) && len.is_multiple_of(4) {
        Ok(())
    } else {
        Err(Error::Misaligned)
    }
}

/// Check `command` against the buffers it names, resolving it to the
/// operation a device runs.
pub(crate) fn resolve(
    buffers: &SlotMap<BufferKey, BufferState>,
    command: &Command,
) -> Result<Op, Error> {
    match command {
        Command::Copy { src, dst, regions } => {
            let src = buffer(buffers, *src, BufferUsage::TRANSFER_SRC)?;
            let dst = buffer(buffers, *dst, BufferUsage::TRANSFER_DST)?;
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
                spans.push(Span {
                    src: within(src, region.src_offset, region.size)?,
                    dst: within(dst, region.dst_offset, region.size)?,
                    len: region.size,
                });
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
            let dst = buffer(buffers, *dst, BufferUsage::TRANSFER_DST)?;
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
            let dst = buffer(buffers, *dst, BufferUsage::TRANSFER_DST)?;
            if data.len() > MAX_UPDATE_BYTES {
                return Err(Error::TooMany);
            }
            let len = data.len() as u64;
            let at = within(dst, *offset, len)?;
            words(*offset, len)?;
            Ok(Op::Update {
                dst: Arc::clone(&dst.memory),
                offset: at,
                data: Arc::clone(data),
            })
        }
    }
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
