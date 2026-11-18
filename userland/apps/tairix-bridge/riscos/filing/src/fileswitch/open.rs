//! Open files (`plans/RISCOS-EMULATOR.md` §6): FileSwitch's handles, the
//! highest free from 255 down, each over a filing system's own handle.
//!
//! A filing system that asks for buffering gets one buffer per file, of the
//! natural block size it gave, in the RMA, holding one block at a time.
//! FileSwitch keeps a buffered file's pointer, extent and allocation itself
//! and moves whole blocks: a transfer of whole blocks that lie within the
//! extent goes straight between the filing system and the caller's memory,
//! and the rest through the buffer, a modified block written back before
//! another is read into it. An unbuffered file's pointer and extent are the
//! filing system's.

use alloc::vec::Vec;

use tairix_riscos_kernel::filing::fs;
use tairix_riscos_kernel::os::{len32, Fail, Os};

use super::call::{self, Info, Opened};
use super::file::{typed_stamp, DATA};
use super::name::Resolved;
use super::system::{Calls, Point};
use super::{error, with};

/// The handles there are: RISC OS's bound on open files.
pub const HANDLES: u32 = 255;

pub use tairix_riscos_kernel::filing::fs::file_info;

pub use tairix_riscos_kernel::swi::os::args::status;

/// A buffered file's buffer and FileSwitch's record of the file.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Buffer {
    /// Its size: the file's natural block size.
    pub size: u32,
    /// Its address in the RMA.
    pub at: u32,
    /// The offset of the block it holds.
    pub held: Option<u32>,
    /// Whether the block it holds has been written to.
    pub dirty: bool,
    /// The sequential pointer.
    pub pointer: u32,
    /// The extent.
    pub extent: u32,
    /// The space allocated to the file.
    pub allocated: u32,
}

impl Buffer {
    /// The offset of the block holding `offset`.
    #[must_use]
    pub const fn block(&self, offset: u32) -> u32 {
        offset & !(self.size - 1)
    }
}

/// An open file, as each operation on it reads and writes it back.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Stream {
    /// Its filing system.
    pub calls: Calls,
    /// The filing system's handle.
    pub handle: u32,
    /// The file information word the filing system opened it with.
    pub info: u32,
    /// Its buffer, if it is buffered.
    pub buffer: Option<Buffer>,
    /// Its [`status`] bits that change as it is used: opened to be written,
    /// written to since, and the next read at its end the error.
    pub status: u32,
    /// FileSwitch's own handle for it, which an upcall about it gives.
    pub own: u32,
    /// Made by `OS_Find`, which `*Opt 1` tells of when it is closed.
    pub created: bool,
}

impl Stream {
    /// Whether its [`status`] `bit` is set.
    #[must_use]
    pub const fn has(&self, bit: u32) -> bool {
        self.status & bit != 0
    }

    /// Its [`status`] `bit` set or cleared.
    pub fn mark(&mut self, bit: u32, set: bool) {
        if set {
            self.status |= bit;
        } else {
            self.status &= !bit;
        }
    }
}

/// An open file and the name it was opened by.
#[derive(Clone, PartialEq, Eq, Debug)]
struct Open {
    stream: Stream,
    /// The absolute path its filing system was given.
    path: Vec<u8>,
    /// Its special field.
    special: Option<Vec<u8>>,
}

/// A handle's place in the table.
fn index(handle: u32) -> Option<usize> {
    usize::try_from(handle.checked_sub(1)?).ok()
}

/// The open files, by handle.
#[derive(Debug, Default)]
pub struct Files {
    open: Vec<Option<Open>>,
}

impl Files {
    /// No file open.
    #[must_use]
    pub const fn new() -> Self {
        Self { open: Vec::new() }
    }

    fn slot(&self, handle: u32) -> Option<&Open> {
        self.open.get(index(handle)?)?.as_ref()
    }

    /// The file on `handle` taken out of the table.
    fn take(&mut self, handle: u32) -> Option<Open> {
        self.open.get_mut(index(handle)?)?.take()
    }

    /// The file open on `handle`.
    #[must_use]
    pub fn stream(&self, handle: u32) -> Option<Stream> {
        self.slot(handle).map(|open| open.stream)
    }

    /// `stream` written back to `handle`, where a file is still open on it.
    pub fn put(&mut self, handle: u32, stream: Stream) {
        if let Some(Some(open)) = index(handle).and_then(|i| self.open.get_mut(i)) {
            open.stream = stream;
        }
    }

    /// The highest handle no file is open on.
    fn free(&self) -> Option<u32> {
        (1..=HANDLES)
            .rev()
            .find(|&handle| self.slot(handle).is_none())
    }

    /// The name the file on `handle` was opened by: its filing system's
    /// number, path and special field.
    #[must_use]
    pub fn name(&self, handle: u32) -> Option<(u8, Vec<u8>, Option<Vec<u8>>)> {
        self.slot(handle).map(|open| {
            (
                open.stream.calls.number,
                open.path.clone(),
                open.special.clone(),
            )
        })
    }

    /// Every handle a file is open on, of the filing system `number` alone
    /// where one is given.
    #[must_use]
    pub fn handles(&self, number: Option<u8>) -> Vec<u32> {
        (1..=HANDLES)
            .filter(|&handle| {
                self.slot(handle).is_some_and(|open| {
                    number.is_none_or(|number| open.stream.calls.number == number)
                })
            })
            .collect()
    }
}

/// The file open on `handle`, or the error for a handle nothing is open on.
pub(crate) fn stream(os: &mut Os<'_>, handle: u32) -> Result<Stream, Fail> {
    match with(os, |state| state.files.stream(handle))? {
        Some(stream) => Ok(stream),
        None => Err(os.error(tairix_riscos_kernel::filing::NOT_OPEN)),
    }
}

/// `stream` written back to `handle`.
pub(crate) fn put(os: &mut Os<'_>, handle: u32, stream: Stream) -> Result<(), Fail> {
    with(os, |state| state.files.put(handle, stream))
}

/// Records on `handle` a file the filing system opened as `opened`, by the
/// name `resolved`: the handle. A buffer size the protocol does not allow,
/// or one there is no room for, is refused, the file closed again.
pub(crate) fn record(
    os: &mut Os<'_>,
    resolved: Resolved,
    opened: Opened,
    status: u32,
    handle: u32,
) -> Result<u32, Fail> {
    let Resolved {
        calls,
        path,
        special,
        ..
    } = resolved;
    let buffer = match opened.buffer {
        0 => None,
        size if size.is_power_of_two() && (64..=1024).contains(&size) => {
            let at = match os.rma_claim(size) {
                Ok(at) => at,
                Err(fail) => {
                    // The claim's failure is the one to report.
                    let _ = call::close(os, &calls, opened.handle, 0, 0);
                    return Err(fail);
                }
            };
            Some(Buffer {
                size,
                at,
                held: None,
                dirty: false,
                pointer: 0,
                extent: opened.extent,
                allocated: opened.allocated,
            })
        }
        _ => {
            call::close(os, &calls, opened.handle, 0, 0)?;
            return Err(error::fail(os, error::BAD_BUFFER, &[]));
        }
    };
    let open = Open {
        stream: Stream {
            calls,
            handle: opened.handle,
            info: opened.info,
            buffer,
            status,
            own: handle,
            created: false,
        },
        path,
        special,
    };
    let placed = with(os, |state| {
        let files = &mut state.files;
        if files.open.is_empty() {
            if files.open.try_reserve_exact(HANDLES as usize).is_err() {
                return false;
            }
            files.open.resize(HANDLES as usize, None);
        }
        match index(handle).and_then(|i| files.open.get_mut(i)) {
            Some(slot @ None) => {
                *slot = Some(open);
                true
            }
            _ => false,
        }
    })?;
    if !placed {
        if let Some(buffer) = buffer {
            os.rma_free(buffer.at)?;
        }
        call::close(os, &calls, opened.handle, 0, 0)?;
        return Err(os.no_memory());
    }
    Ok(handle)
}

/// The handle the next file opened takes, or "too many open files".
pub(crate) fn next_handle(os: &mut Os<'_>) -> Result<u32, Fail> {
    match with(os, |state| state.files.free())? {
        Some(handle) => Ok(handle),
        None => Err(error::fail(os, error::TOO_MANY_OPEN, &[])),
    }
}

/// The held block written back, if it was written to.
pub(crate) fn flush(os: &mut Os<'_>, stream: &Stream, buffer: &mut Buffer) -> Result<(), Fail> {
    if let (Some(block), true) = (buffer.held, buffer.dirty) {
        call::transfer(
            os,
            &stream.calls,
            Point::PutBytes,
            stream.handle,
            buffer.at,
            buffer.size,
            block,
        )?;
        buffer.dirty = false;
    }
    Ok(())
}

/// The block at `block` held, the one held before written back if it was
/// written to: read from the filing system where it lies within the extent,
/// zeros past it.
pub(crate) fn hold(
    os: &mut Os<'_>,
    stream: &Stream,
    buffer: &mut Buffer,
    block: u32,
) -> Result<(), Fail> {
    if buffer.held == Some(block) {
        return Ok(());
    }
    flush(os, stream, buffer)?;
    buffer.held = None;
    if block < buffer.extent {
        call::transfer(
            os,
            &stream.calls,
            Point::GetBytes,
            stream.handle,
            buffer.at,
            buffer.size,
            block,
        )?;
    } else {
        os.fill(buffer.at, buffer.size, 0)?;
    }
    buffer.held = Some(block);
    Ok(())
}

/// The space allocated made at least `size`, rounded up to whole blocks,
/// where it is not.
fn reserve(os: &mut Os<'_>, stream: &Stream, buffer: &mut Buffer, size: u32) -> Result<(), Fail> {
    let wanted = size.saturating_add(buffer.size - 1) & !(buffer.size - 1);
    ensure_space(os, stream, buffer, wanted)
}

/// The space allocated made at least `wanted`, by `FSEntry_Args` 7 where it
/// is not.
pub(crate) fn ensure_space(
    os: &mut Os<'_>,
    stream: &Stream,
    buffer: &mut Buffer,
    wanted: u32,
) -> Result<(), Fail> {
    if wanted > buffer.allocated {
        let got = call::ensure(os, &stream.calls, stream.own, stream.handle, wanted)?;
        buffer.allocated = got.max(wanted);
    }
    Ok(())
}

/// A buffered file's extent cut back to `extent`: the pointer brought within
/// it, and a block held beyond it let go of unwritten, as RISC OS 5 drops
/// one.
pub(crate) fn truncate(buffer: &mut Buffer, extent: u32) {
    buffer.extent = extent;
    buffer.pointer = buffer.pointer.min(extent);
    if buffer.held.is_some_and(|held| held >= extent) {
        buffer.held = None;
        buffer.dirty = false;
    }
}

/// A buffered file's extent made `extent`, past its old one: the block
/// holding the old end zeroed past it, the space ensured, and whole blocks
/// after it written as zeros, as RISC OS 5 extends one.
pub(crate) fn extend(
    os: &mut Os<'_>,
    stream: &Stream,
    buffer: &mut Buffer,
    extent: u32,
) -> Result<(), Fail> {
    let old = buffer.extent;
    let tail = buffer.block(old);
    if old != tail {
        hold(os, stream, buffer, tail)?;
        let from = old - tail;
        os.fill(buffer.at + from, buffer.size - from, 0)?;
        buffer.dirty = true;
    }
    reserve(os, stream, buffer, extent)?;
    let zeros_from = if old == tail {
        old
    } else {
        tail.saturating_add(buffer.size)
    };
    let zeros_to = extent.saturating_add(buffer.size - 1) & !(buffer.size - 1);
    if zeros_to > zeros_from {
        call::args(
            os,
            &stream.calls,
            8,
            stream.handle,
            zeros_from,
            zeros_to - zeros_from,
        )?;
        if buffer
            .held
            .is_some_and(|held| held >= zeros_from && held < zeros_to)
        {
            buffer.held = None;
            buffer.dirty = false;
        }
    }
    buffer.extent = extent;
    Ok(())
}

/// The byte at a buffered file's pointer, the pointer moved past it; none
/// at its extent.
pub(crate) fn get_buffered(
    os: &mut Os<'_>,
    stream: &Stream,
    buffer: &mut Buffer,
) -> Result<Option<u8>, Fail> {
    if buffer.pointer >= buffer.extent {
        return Ok(None);
    }
    let block = buffer.block(buffer.pointer);
    hold(os, stream, buffer, block)?;
    let byte = os.read_byte(buffer.at + (buffer.pointer - block))?;
    buffer.pointer += 1;
    Ok(Some(byte))
}

/// `byte` written at a buffered file's pointer, the pointer moved past it
/// and the file extended to hold it.
pub(crate) fn put_buffered(
    os: &mut Os<'_>,
    stream: &Stream,
    buffer: &mut Buffer,
    byte: u8,
) -> Result<(), Fail> {
    let at = buffer.pointer;
    let past = at
        .checked_add(1)
        .ok_or_else(|| error::fail(os, error::OUTSIDE, &[]))?;
    if at > buffer.extent {
        extend(os, stream, buffer, at)?;
    }
    reserve(os, stream, buffer, past)?;
    let block = buffer.block(at);
    hold(os, stream, buffer, block)?;
    os.write_byte(buffer.at + (at - block), byte)?;
    buffer.dirty = true;
    buffer.pointer = past;
    buffer.extent = buffer.extent.max(past);
    Ok(())
}

/// What a buffered transfer of `count` bytes from `start` does with whole
/// blocks: those from the first block boundary that the transfer covers, if
/// they all lie within the extent; none otherwise.
fn direct(buffer: &Buffer, start: u32, count: u32, within: u32) -> Option<(u32, u32)> {
    let first = start.checked_add(buffer.size - 1)? & !(buffer.size - 1);
    let end = start.checked_add(count)?;
    let blocks = end.checked_sub(first)? / buffer.size;
    let length = blocks * buffer.size;
    (blocks > 0 && first + length <= within).then_some((first, length))
}

/// Reads `count` bytes of a buffered file from `start` to guest memory at
/// `to`, up to its extent: how many were read.
pub(crate) fn read_buffered(
    os: &mut Os<'_>,
    stream: &Stream,
    buffer: &mut Buffer,
    to: u32,
    start: u32,
    count: u32,
) -> Result<u32, Fail> {
    let available = buffer.extent.saturating_sub(start).min(count);
    let whole = direct(buffer, start, count, buffer.extent);
    let mut done = 0;
    while done < available {
        let at = start + done;
        if let Some((first, length)) = whole.filter(|&(first, _)| first == at) {
            call::transfer(
                os,
                &stream.calls,
                Point::GetBytes,
                stream.handle,
                to.wrapping_add(done),
                length,
                first,
            )?;
            if let (Some(held), true) = (buffer.held, buffer.dirty) {
                if held >= first && held < first + length {
                    os.copy_bytes(
                        buffer.at,
                        to.wrapping_add(done + (held - first)),
                        buffer.size,
                    )?;
                }
            }
            done += length;
            continue;
        }
        let block = buffer.block(at);
        hold(os, stream, buffer, block)?;
        let within = at - block;
        let stop = whole.map_or(available, |(first, _)| {
            if first > at {
                (first - start).min(available)
            } else {
                available
            }
        });
        let length = (buffer.size - within).min(stop - done);
        os.copy_bytes(buffer.at + within, to.wrapping_add(done), length)?;
        done += length;
    }
    Ok(available)
}

/// Writes `count` bytes from guest memory at `from` to a buffered file at
/// `start`, the file extended to hold them.
pub(crate) fn write_buffered(
    os: &mut Os<'_>,
    stream: &Stream,
    buffer: &mut Buffer,
    from: u32,
    start: u32,
    count: u32,
) -> Result<(), Fail> {
    let end = start
        .checked_add(count)
        .ok_or_else(|| error::fail(os, error::OUTSIDE, &[]))?;
    if start > buffer.extent {
        extend(os, stream, buffer, start)?;
    }
    reserve(os, stream, buffer, end)?;
    let whole = direct(buffer, start, count, u32::MAX);
    let mut done = 0;
    while done < count {
        let at = start + done;
        if let Some((first, length)) = whole.filter(|&(first, _)| first == at) {
            call::transfer(
                os,
                &stream.calls,
                Point::PutBytes,
                stream.handle,
                from.wrapping_add(done),
                length,
                first,
            )?;
            if buffer
                .held
                .is_some_and(|held| held >= first && held < first + length)
            {
                buffer.held = None;
                buffer.dirty = false;
            }
            done += length;
            buffer.extent = buffer.extent.max(first + length);
            continue;
        }
        let block = buffer.block(at);
        hold(os, stream, buffer, block)?;
        let within = at - block;
        let stop = whole.map_or(
            count,
            |(first, _)| {
                if first > at {
                    first - start
                } else {
                    count
                }
            },
        );
        let length = (buffer.size - within).min(stop - done);
        os.copy_bytes(from.wrapping_add(done), buffer.at + within, length)?;
        buffer.dirty = true;
        done += length;
        buffer.extent = buffer.extent.max(at + length);
    }
    Ok(())
}

/// The file on `handle` made safe: a buffered one's held block written back,
/// and the filing system told of the flush where it asked to be; an
/// unbuffered one's filing system told of it always, as RISC OS 5 tells it.
pub(crate) fn ensure(os: &mut Os<'_>, handle: u32, mut stream: Stream) -> Result<(), Fail> {
    let Some(mut buffer) = stream.buffer else {
        return call::args(os, &stream.calls, fs::args::FLUSH, stream.handle, 0, 0).map(drop);
    };
    let flushed = flush(os, &stream, &mut buffer);
    stream.buffer = Some(buffer);
    put(os, handle, stream)?;
    flushed?;
    if stream.calls.has(super::system::info::FLUSH_NOTIFY) {
        call::args(os, &stream.calls, fs::args::FLUSH, stream.handle, 0, 0)?;
    }
    Ok(())
}

/// The stamp a file written to takes when it is closed, from what
/// `FSEntry_Args` 9 read of it: a typed file keeps its type, one with no
/// stamp at all becomes Data, both stamped `now`; any other keeps its
/// addresses, zero meaning no restamp.
fn new_stamp((load, exec): (u32, u32), now: u64) -> (u32, u32) {
    let kind = if let Some(kind) = tairix_riscos_kernel::filing::fs::stamp::type_of(load) {
        kind
    } else if load == 0 && exec == 0 {
        DATA
    } else {
        return (0, 0);
    };
    typed_stamp(kind, now)
}

/// Closes the file on `handle`: its held block written back, a writable
/// one's extent set, and, if it was written to, its stamp renewed and
/// `UpCall_ModifyingFile` told, before the filing system closes it; its
/// handle and buffer let go of.
pub(crate) fn close(os: &mut Os<'_>, handle: u32) -> Result<(), Fail> {
    close_stamped(os, handle, true)
}

/// As [`close`], its stamp left as it is where `restamp` is not asked: a
/// save has stamped it already.
pub(crate) fn close_stamped(os: &mut Os<'_>, handle: u32, restamp: bool) -> Result<(), Fail> {
    let mut stream = stream(os, handle)?;
    let mut stamp = (0, 0);
    match stream.buffer {
        Some(mut buffer) => {
            flush(os, &stream, &mut buffer)?;
            stream.buffer = Some(buffer);
            put(os, handle, stream)?;
            if stream.has(status::WRITE) {
                call::args(
                    os,
                    &stream.calls,
                    fs::args::WRITE_EXTENT,
                    stream.handle,
                    buffer.extent,
                    0,
                )?;
            }
        }
        None => {
            call::args(os, &stream.calls, fs::args::FLUSH, stream.handle, 0, 0)?;
        }
    }
    if stream.has(status::MODIFIED) && restamp {
        let read = call::args(os, &stream.calls, fs::args::READ_STAMP, stream.handle, 0, 0)?;
        stamp = new_stamp(read, os.real_time());
        call::upcall(
            os,
            &stream.calls,
            fs::modifying::CLOSE,
            handle,
            0,
            stamp.0,
            stamp.1,
        )?;
    }
    let closed = call::close(os, &stream.calls, stream.handle, stamp.0, stamp.1);
    let taken = with(os, |state| state.files.take(handle))?;
    let Some(taken) = taken else {
        return closed;
    };
    if let Some(buffer) = taken.stream.buffer {
        os.rma_free(buffer.at)?;
    }
    closed?;
    if stream.created {
        let made = Info {
            kind: call::FILE,
            load: stamp.0,
            exec: stamp.1,
            length: stream.buffer.map_or(0, |buffer| buffer.extent),
            attributes: 0,
        };
        super::listing::announce(
            os,
            stream.calls.number,
            super::name::leaf(&taken.path),
            &made,
        )?;
    }
    Ok(())
}

/// The files open on the object the canonical name at `at` names, or on
/// anything beneath it, closed — images after the files within them: how
/// many, as `Service_CloseFile` counts them.
pub(crate) fn close_beneath(os: &mut Os<'_>, at: u32) -> Result<u32, Fail> {
    let (target, _) = os.read_until(at, super::name::NAME_MOST, |b| b == 0)?;
    let mut held = Vec::new();
    for handle in with(os, |state| state.files.handles(None))? {
        let Some((number, path, special)) = with(os, |state| state.files.name(handle))? else {
            continue;
        };
        let named = call::Named::new(&path, special.as_deref());
        let canonical = super::args::canonical(os, number, named)?;
        let within = canonical
            .get(..target.len())
            .is_some_and(|head| head.eq_ignore_ascii_case(&target))
            && matches!(canonical.get(target.len()), None | Some(b'.'));
        if within {
            if held.try_reserve(1).is_err() {
                return Err(os.no_memory());
            }
            held.push((super::image::holds(os, handle)?, handle));
        }
    }
    held.sort_unstable();
    for &(image, handle) in &held {
        if image {
            super::image::close_handle(os, handle)?;
        } else {
            close(os, handle)?;
        }
    }
    Ok(len32(held.len()))
}

/// Closes every open file, or every one on the filing system `number`: the
/// images among them first, their image filing systems told.
pub(crate) fn close_all(os: &mut Os<'_>, number: Option<u8>) -> Result<(), Fail> {
    if os.extension::<super::State>().is_none() {
        return Ok(());
    }
    match number {
        Some(number) => super::image::close_on(os, number)?,
        None => super::image::close_where(os, |_| false)?,
    }
    let handles = match os.extension::<super::State>() {
        Some(state) => state.files.handles(number),
        None => return Ok(()),
    };
    for handle in handles {
        close(os, handle)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
