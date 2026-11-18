//! `OS_Args` (`plans/RISCOS-EMULATOR.md` §6): an open file's pointer,
//! extent, allocation, end, name, stream status and flushing.
//!
//! A buffered file's pointer, extent and allocation are FileSwitch's own
//! record, answered without asking the filing system; an unbuffered file's
//! are the filing system's.

use tairix_riscos_arm::psr;
use tairix_riscos_kernel::filing::fs;
use tairix_riscos_kernel::filing::BAD_ARGS;
use tairix_riscos_kernel::os::{len32, Fail, Os, Regs};
use tairix_riscos_kernel::swi::os::args::{
    AT_END, ENSURE, FLUSH, IMAGE_STAMP, IOCTL, NAME, READ_ALLOCATED, READ_EXTENT, READ_POINTER,
    STATUS, WRITE_EXTENT, WRITE_POINTER,
};

use super::call::{self, Named};
use super::open::{self, status, Stream};
use super::{error, with};

/// The extra information word's bit that says a filing system takes I/O
/// controls.
const EXTRA_IOCTL: u32 = 1 << 3;

/// `ArgsV`'s default.
pub(crate) fn args(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (reason, handle) = (regs.r[0], regs.r[1]);
    match (reason, handle) {
        (READ_POINTER, 0) => {
            regs.r[0] = with(os, |state| state.temporary().map_or(0, u32::from))?;
            Ok(())
        }
        (STATUS, _) => stream_status(os, regs),
        (FLUSH, 0) => {
            let handles = with(os, |state| state.files.handles(None))?;
            handles.into_iter().try_for_each(|h| flush(os, h))
        }
        (FLUSH, _) => flush(os, handle),
        (READ_POINTER..=IOCTL, _) => on_file(os, reason, handle, regs),
        _ => Err(os.error(BAD_ARGS)),
    }
}

/// The reasons that act on one open file.
fn on_file(os: &mut Os<'_>, reason: u32, handle: u32, regs: &mut Regs) -> Result<(), Fail> {
    let mut stream = open::stream(os, handle)?;
    let done = match reason {
        NAME => name(os, handle, regs),
        IMAGE_STAMP => image_stamp(os, &stream, regs.r[2]),
        IOCTL => ioctl(os, &stream, regs),
        _ => match stream.buffer {
            Some(mut buffer) => {
                let done = buffered(os, &mut stream, &mut buffer, reason, regs);
                stream.buffer = Some(buffer);
                done
            }
            None => unbuffered(os, &mut stream, reason, regs),
        },
    };
    open::put(os, handle, stream)?;
    done
}

/// A reason on a buffered file.
fn buffered(
    os: &mut Os<'_>,
    stream: &mut Stream,
    buffer: &mut open::Buffer,
    reason: u32,
    regs: &mut Regs,
) -> Result<(), Fail> {
    match reason {
        READ_POINTER => regs.r[2] = buffer.pointer,
        WRITE_POINTER => {
            let pointer = regs.r[2];
            if pointer > buffer.extent {
                if !stream.has(status::WRITE) {
                    return Err(error::fail(os, error::OUTSIDE, &[]));
                }
                open::extend(os, stream, buffer, pointer)?;
                stream.mark(status::MODIFIED, true);
            }
            buffer.pointer = pointer;
            stream.mark(status::AT_END, false);
        }
        READ_EXTENT => regs.r[2] = buffer.extent,
        WRITE_EXTENT => {
            if !stream.has(status::WRITE) {
                return Err(error::fail(os, error::NOT_FOR_UPDATE, &[]));
            }
            let extent = regs.r[2];
            if extent > buffer.extent {
                open::extend(os, stream, buffer, extent)?;
            } else {
                open::truncate(buffer, extent);
            }
            stream.mark(status::MODIFIED, true);
            stream.mark(status::AT_END, false);
        }
        READ_ALLOCATED => regs.r[2] = buffer.allocated,
        AT_END => at_end(regs, buffer.pointer >= buffer.extent),
        ENSURE => {
            open::ensure_space(os, stream, buffer, regs.r[2])?;
            regs.r[2] = buffer.allocated;
        }
        _ => return Err(os.error(BAD_ARGS)),
    }
    Ok(())
}

/// A reason on an unbuffered file: the filing system's own.
fn unbuffered(
    os: &mut Os<'_>,
    stream: &mut Stream,
    reason: u32,
    regs: &mut Regs,
) -> Result<(), Fail> {
    if reason == WRITE_EXTENT && !stream.has(status::WRITE) {
        return Err(error::fail(os, error::NOT_FOR_UPDATE, &[]));
    }
    if reason == ENSURE {
        regs.r[2] = call::ensure(os, &stream.calls, stream.own, stream.handle, regs.r[2])?;
        return Ok(());
    }
    let entry = if reason == AT_END { 5 } else { reason };
    let (r2, _) = call::args(os, &stream.calls, entry, stream.handle, regs.r[2], 0)?;
    match reason {
        AT_END => at_end(regs, r2 != 0),
        WRITE_POINTER | WRITE_EXTENT => {
            stream.mark(status::AT_END, false);
            if reason == WRITE_EXTENT {
                stream.mark(status::MODIFIED, true);
            }
        }
        _ => regs.r[2] = r2,
    }
    Ok(())
}

/// `OS_Args` 5's answer: R2 all ones at the end, and C set either way, as
/// RISC OS 5 leaves it.
fn at_end(regs: &mut Regs, end: bool) {
    regs.r[2] = if end { u32::MAX } else { 0 };
    regs.set_flag(psr::C, true);
}

/// `OS_Args` 7: the file's name, its filing system's prefix before the path
/// it was opened by, into R2's R5 bytes; R5 what is left, below zero where
/// the name did not fit.
fn name(os: &mut Os<'_>, handle: u32, regs: &mut Regs) -> Result<(), Fail> {
    let Some((number, path, special)) = with(os, |state| state.files.name(handle))? else {
        return Err(os.error(tairix_riscos_kernel::filing::NOT_OPEN));
    };
    let text = canonical(os, number, Named::new(&path, special.as_deref()))?;
    give(os, &text, regs.r[2], regs, Spare::WithTerminator).map(drop)
}

/// The canonical name of `path` on the filing system `number`:
/// `fs#special:path` or `fs:path`.
pub(crate) fn canonical(
    os: &mut Os<'_>,
    number: u8,
    named: Named<'_>,
) -> Result<alloc::vec::Vec<u8>, Fail> {
    let Named { path, special, .. } = named;
    let system = with(os, |state| state.system(number).map(|s| s.name.clone()))?;
    let Some(system) = system else {
        return Err(os.error(tairix_riscos_kernel::filing::NOT_PRESENT));
    };
    let length = system.len() + special.map_or(0, |s| s.len() + 1) + 1 + path.len();
    let mut text = alloc::vec::Vec::new();
    if text.try_reserve_exact(length).is_err() {
        return Err(os.no_memory());
    }
    text.extend_from_slice(&system);
    if let Some(special) = special {
        text.push(b'#');
        text.extend_from_slice(special);
    }
    text.push(b':');
    text.extend_from_slice(path);
    Ok(text)
}

/// What a buffer's spare bytes are counted after, as the call answering in
/// it counts them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Spare {
    /// After the text, the terminator's byte among them: `OS_FSControl` 37
    /// and `OS_Args` 7.
    WithTerminator,
    /// After the text and its terminator: `OS_FSControl` 54.
    AfterTerminator,
}

/// `text` and its terminator into the R5 bytes at `to`, R5 left what is
/// spare as `spare` counts it: below zero, and nothing written, where they
/// do not fit. Whether they fit.
pub(crate) fn give(
    os: &mut Os<'_>,
    text: &[u8],
    to: u32,
    regs: &mut Regs,
    spare: Spare,
) -> Result<bool, Fail> {
    let length = len32(text.len());
    let room = regs.r[5];
    let fits = room.cast_signed() > length.cast_signed();
    if fits {
        os.write_terminated(to, text, 0, text.len() + 1)?;
    }
    regs.r[5] = match spare {
        Spare::WithTerminator => room.wrapping_sub(length),
        Spare::AfterTerminator => room.wrapping_sub(length + 1),
    };
    Ok(fits)
}

/// `OS_Args` 8: an image's stamp, which only an image filing system's host
/// is told of; for any other filing system it is nothing, as RISC OS 5
/// leaves it.
fn image_stamp(os: &mut Os<'_>, stream: &Stream, stamp: u32) -> Result<(), Fail> {
    if !stream.calls.has(super::system::info::IMAGES) {
        return Ok(());
    }
    call::args(
        os,
        &stream.calls,
        fs::args::IMAGE_STAMP,
        stream.handle,
        stamp,
        0,
    )
    .map(drop)
}

/// `OS_Args` 9: an I/O control, passed to a filing system that takes them
/// and, as RISC OS 5 leaves it, nothing for one that does not.
fn ioctl(os: &mut Os<'_>, stream: &Stream, regs: &mut Regs) -> Result<(), Fail> {
    let extra = with(os, |state| {
        state.system(stream.calls.number).map_or(0, |s| s.extra)
    })?;
    if extra & EXTRA_IOCTL == 0 {
        return Ok(());
    }
    call::args(
        os,
        &stream.calls,
        fs::args::IOCTL,
        stream.handle,
        regs.r[2],
        0,
    )
    .map(drop)
}

/// `OS_Args` 254: the stream status word and the filing system's
/// information word; for a handle nothing is open on, "unallocated".
fn stream_status(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let Some(stream) = with(os, |state| state.files.stream(regs.r[1]))? else {
        regs.r[0] = status::UNALLOCATED;
        return Ok(());
    };
    regs.r[0] = status_of(&stream);
    regs.r[2] = stream.calls.info;
    Ok(())
}

/// An open file's stream status word.
pub(crate) fn status_of(stream: &Stream) -> u32 {
    let unbuffered = if stream.buffer.is_none() {
        status::UNBUFFERED
    } else {
        0
    };
    status::READ | stream.status | unbuffered
}

/// `OS_Args` 255 on one file: its held block written back.
fn flush(os: &mut Os<'_>, handle: u32) -> Result<(), Fail> {
    let stream = open::stream(os, handle)?;
    open::ensure(os, handle, stream)
}
