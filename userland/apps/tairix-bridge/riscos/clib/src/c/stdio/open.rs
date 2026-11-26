//! Opening and closing streams, and the files they name, as RISC OS 5.30's
//! library does: a stream is the first free `FILE` block, a file opened
//! through FileSwitch, and a failed open answers a null pointer with `errno`
//! minus one; `remove` and `rename` answer one where FileSwitch would not;
//! `tmpnam` names `$.tmp.x` by the time and a count of the names made.

use tairix_riscos_kernel::os::{Fail, Os, Regs};
use tairix_riscos_kernel::swi::os::{self as kos, find};

use super::stream::{
    file_at, flag, is_stream, performed, registers, stderr, stdin, stdout, word, Stream, BUFSIZ,
    EOF,
};
use crate::c::alloc;
use crate::c::args::{Redirection, Standard};
use crate::c::errno::{self, OS_REFUSED};
use crate::c::statics::{at, L_TMPNAM, STREAMS};
use crate::c::time;
use crate::kernel::context;
use crate::kernel::statics::Statics;

/// `OS_Find`'s reasons, a missing object an error and no path searched; and
/// update without that error, which an append stream tries before it
/// creates.
const OPEN_IN: u32 = find::INPUT | find::MUST_OPEN | find::NO_PATH;
const OPEN_UP: u32 = find::UPDATE | find::MUST_OPEN | find::NO_PATH;
const CREATE: u32 = find::OUTPUT | find::MUST_OPEN | find::NO_PATH;
const OPEN_UP_IF_THERE: u32 = find::UPDATE | find::NO_PATH;
/// The buffer each of two streams sharing a file is given.
const SHARED_BUFFER: u32 = 0x100;
/// The mode `tmpfile` opens with.
const TMPFILE_MODE: &[u8] = b"w+b";
/// The most of a mode string read: what follows its letter, `+` and `b` is
/// passed over unread.
const MODE_MOST: usize = 3;

/// How a mode string opens a stream: its flags, the `OS_Find` reason, and
/// whether it appends.
#[derive(Clone, Copy)]
pub(crate) struct Mode {
    flags: u32,
    reason: u32,
    append: bool,
}

impl Mode {
    /// `r`, `w` and `a`.
    pub(crate) const READ: Self = Self {
        flags: flag::READ,
        reason: OPEN_IN,
        append: false,
    };
    pub(crate) const WRITE: Self = Self {
        flags: flag::WRITE,
        reason: CREATE,
        append: false,
    };
    pub(crate) const APPEND: Self = Self {
        flags: flag::WRITE | flag::REPOSITION | flag::APPENDING,
        reason: OPEN_UP_IF_THERE,
        append: true,
    };

    /// The mode a large-file open marks its stream with.
    const fn large_where(self, large: bool) -> Self {
        if large {
            Self {
                flags: self.flags | flag::LARGE,
                ..self
            }
        } else {
            self
        }
    }
}

/// The mode `text` names: `r`, `w` or `a`, then `+` and `b` either way
/// round, anything after them passed over; none for any other start.
fn mode(text: &[u8]) -> Option<Mode> {
    let (&first, rest) = text.split_first()?;
    let (mut update, mut binary) = (false, false);
    for &b in rest.iter().take(2) {
        match b {
            b'+' => update = true,
            b'b' => binary = true,
            _ => break,
        }
    }
    let base = match first {
        b'r' => Mode::READ,
        b'w' => Mode::WRITE,
        b'a' => Mode::APPEND,
        _ => return None,
    };
    let reason = if update && first == b'r' {
        OPEN_UP
    } else {
        base.reason
    };
    let update = if update { flag::READ | flag::WRITE } else { 0 };
    let binary = if binary { flag::BINARY } else { 0 };
    Some(Mode {
        flags: base.flags | update | binary,
        reason,
        ..base
    })
}

/// The file `name` opened as `mode` asks into the `FILE` block at `at`, an
/// append stream's first transfer sent to its end: false, `errno` minus one,
/// where it could not be.
fn open_into(
    os: &mut Os<'_>,
    statics: Statics,
    at: u32,
    name: u32,
    mode: Mode,
) -> Result<bool, Fail> {
    let mut opening = registers(&[mode.reason, name]);
    let mut opened = performed(os, statics, kos::FIND, &mut opening)? && opening.r[0] != 0;
    if mode.append && !opened {
        opening = registers(&[CREATE, name]);
        opened = performed(os, statics, kos::FIND, &mut opening)? && opening.r[0] != 0;
    }
    if !opened {
        errno::set(os, statics, OS_REFUSED)?;
        return Ok(false);
    }
    let handle = opening.r[0];
    let mut sought = 0;
    if mode.append {
        let mut args = registers(&[kos::args::READ_EXTENT, handle]);
        if performed(os, statics, kos::ARGS, &mut args)? {
            sought = args.r[2];
        }
    }
    let previous = Stream::load(os, at)?;
    let stream = Stream {
        at,
        flag: mode.flags,
        handle,
        bufsiz: BUFSIZ,
        extra: previous.extra,
        sought,
        ..Stream::default()
    };
    stream.store(os)?;
    Ok(true)
}

/// The file `name` opened as the mode string at `mode_text` asks into the
/// `FILE` block at `at`, marked a large file's where `large`: false where
/// the mode names none, or the file could not be opened.
fn open_as(
    os: &mut Os<'_>,
    statics: Statics,
    at: u32,
    name: u32,
    mode_text: u32,
    large: bool,
) -> Result<bool, Fail> {
    let text = os
        .as_caller()
        .read_until(mode_text, MODE_MOST, |b| b == 0)?
        .0;
    match mode(&text) {
        Some(mode) => open_into(os, statics, at, name, mode.large_where(large)),
        None => Ok(false),
    }
}

/// The first `FILE` block no stream is open in.
fn free_stream(os: &mut Os<'_>, statics: Statics) -> Result<Option<u32>, Fail> {
    for n in 0..STREAMS {
        let at = file_at(statics, n);
        if os.as_caller().read_word(at.wrapping_add(word::FLAG))? & (flag::READ | flag::WRITE) == 0
        {
            return Ok(Some(at));
        }
    }
    Ok(None)
}

/// `fopen(filename, mode)`, or `fopen64` where `large`.
fn opened_stream(os: &mut Os<'_>, regs: &mut Regs, large: bool) -> Result<(), Fail> {
    let statics = context::statics_at(os, regs.r[10])?;
    regs.r[0] = match free_stream(os, statics)? {
        Some(at) if open_as(os, statics, at, regs.r[0], regs.r[1], large)? => at,
        _ => 0,
    };
    Ok(())
}

/// `fopen(filename, mode)`.
pub(in crate::c) fn fopen(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    opened_stream(os, regs, false)
}

/// `fopen64(filename, mode)`.
pub(crate) fn fopen64(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    opened_stream(os, regs, true)
}

/// The stream at `at` closed where it is open, its failure ignored, as a
/// stream reopened is.
fn close_for_reopening(os: &mut Os<'_>, statics: Statics, at: u32) -> Result<(), Fail> {
    let mut stream = Stream::load(os, at)?;
    if stream.flag & (flag::READ | flag::WRITE) != 0 {
        stream.close(os, statics)?;
        stream.store(os)?;
    }
    Ok(())
}

/// `freopen(filename, mode, stream)`, or `freopen64` where `large`: the
/// stream closed, and the file opened in its place.
fn reopened_stream(os: &mut Os<'_>, regs: &mut Regs, large: bool) -> Result<(), Fail> {
    let statics = context::statics_at(os, regs.r[10])?;
    let at = regs.r[2];
    close_for_reopening(os, statics, at)?;
    regs.r[0] = if open_as(os, statics, at, regs.r[0], regs.r[1], large)? {
        at
    } else {
        0
    };
    Ok(())
}

/// `freopen(filename, mode, stream)`.
pub(in crate::c) fn freopen(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    reopened_stream(os, regs, false)
}

/// `freopen64(filename, mode, stream)`.
pub(crate) fn freopen64(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    reopened_stream(os, regs, true)
}

/// The stream at `at` reopened as `mode` asks onto the file `name`, which
/// the library holds rather than the client: false where it could not be.
fn reopen(
    os: &mut Os<'_>,
    statics: Statics,
    at: u32,
    name: &[u8],
    mode: Mode,
) -> Result<bool, Fail> {
    close_for_reopening(os, statics, at)?;
    let Some(bytes) = u32::try_from(name.len())
        .ok()
        .and_then(|length| length.checked_add(1))
    else {
        return Ok(false);
    };
    let copy = alloc::allocated(os, statics, bytes)?;
    if copy == 0 {
        return Ok(false);
    }
    os.as_caller().write_bytes(copy, name)?;
    os.as_caller().write_byte(copy.wrapping_add(bytes - 1), 0)?;
    let opened = open_into(os, statics, at, copy, mode)?;
    alloc::freed(os, statics, copy)?;
    Ok(opened)
}

/// `stderr` sent where `stdout` goes, the two sharing its handle: onto a
/// file, each line-buffered in a small buffer of its own so that their lines
/// reach it in the order they are delivered.
fn error_to_output(os: &mut Os<'_>, statics: Statics) -> Result<(), Fail> {
    let (output_at, error_at) = (stdout(statics), stderr(statics));
    close_for_reopening(os, statics, error_at)?;
    let mut output = Stream::load(os, output_at)?;
    let mut error = Stream::load(os, error_at)?;
    output.flag |= flag::SHARED;
    error.flag = output.flag & !flag::REPOSITION;
    error.handle = output.handle;
    error.bufsiz = 0;
    if !output.console() {
        for stream in [&mut output, &mut error] {
            let buffer = alloc::allocated(os, statics, SHARED_BUFFER)?;
            let taken = if buffer == 0 { 0 } else { flag::SYSTEM_BUFFER };
            stream.flag = (stream.flag & !flag::BUFFERING) | flag::LINE | taken;
            stream.base = buffer;
            stream.ptr = buffer;
            stream.bufsiz = SHARED_BUFFER;
        }
    }
    output.store(os)?;
    error.store(os)
}

/// One of the redirections a command line asks `_main` for made: false
/// where its file could not be opened.
pub(crate) fn redirect(
    os: &mut Os<'_>,
    statics: Statics,
    redirection: &Redirection,
) -> Result<bool, Fail> {
    let Redirection::File {
        stream: which,
        name,
        append,
    } = redirection
    else {
        error_to_output(os, statics)?;
        return Ok(true);
    };
    let (at, mode) = match (which, append) {
        (Standard::Input, _) => (stdin(statics), Mode::READ),
        (Standard::Error, false) => (stderr(statics), Mode::WRITE),
        (Standard::Error, true) => (stderr(statics), Mode::APPEND),
        (Standard::Output | Standard::OutputAndError, false) => (stdout(statics), Mode::WRITE),
        (Standard::Output | Standard::OutputAndError, true) => (stdout(statics), Mode::APPEND),
    };
    if !reopen(os, statics, at, name, mode)? {
        return Ok(false);
    }
    if matches!(which, Standard::OutputAndError) {
        error_to_output(os, statics)?;
    }
    Ok(true)
}

/// `fclose(stream)`: nought, or `EOF` where it is no open stream of the
/// library's or would not close cleanly.
pub(in crate::c) fn fclose(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let statics = context::statics_at(os, regs.r[10])?;
    if !is_stream(statics, regs.r[0]) {
        regs.r[0] = EOF;
        return Ok(());
    }
    let mut stream = Stream::load(os, regs.r[0])?;
    if stream.flag & (flag::READ | flag::WRITE) == 0 {
        regs.r[0] = EOF;
        return Ok(());
    }
    let closed = stream.close(os, statics)?;
    stream.store(os)?;
    regs.r[0] = if closed { 0 } else { EOF };
    Ok(())
}

/// `remove(filename)`: nought where a file was deleted, else one.
pub(in crate::c) fn remove(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let statics = context::statics_at(os, regs.r[10])?;
    let mut delete = registers(&[kos::file::DELETE, regs.r[0]]);
    let deleted = performed(os, statics, kos::FILE, &mut delete)? && delete.r[0] != 0;
    regs.r[0] = u32::from(!deleted);
    Ok(())
}

/// `rename(old, new)`: nought where FileSwitch renamed it, else one.
pub(in crate::c) fn rename(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let statics = context::statics_at(os, regs.r[10])?;
    let mut control = registers(&[kos::fs_control::RENAME, regs.r[0], regs.r[1]]);
    let renamed = performed(os, statics, kos::FS_CONTROL, &mut control)?;
    regs.r[0] = u32::from(!renamed);
    Ok(())
}

/// The next temporary name written at `into`: `$.tmp.x`, then in eight
/// hexadecimal digits the time's low 24 bits and the count of names made.
fn temporary_name(os: &mut Os<'_>, statics: Statics, into: u32) -> Result<(), Fail> {
    let made = statics.read(os, at::TMPNAM_COUNT)?;
    statics.write(os, at::TMPNAM_COUNT, made.wrapping_add(1))?;
    let stamp = time::now(os, statics)? << 8 | (made & 0xFF);
    let mut name = *b"$.tmp.x00000000\0";
    for (n, place) in name[7..15].iter_mut().enumerate() {
        *place = b"0123456789abcdef"[((stamp >> (28 - 4 * n)) & 0xF) as usize];
    }
    os.as_caller().write_bytes(into, &name)
}

/// `tmpnam(s)`: a name no file has, written at `s` or, where `s` is null,
/// in the library's buffer.
pub(in crate::c) fn tmpnam(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let statics = context::statics_at(os, regs.r[10])?;
    let into = if regs.r[0] == 0 {
        statics.field(at::TMPNAM_TEXT)
    } else {
        regs.r[0]
    };
    temporary_name(os, statics, into)?;
    regs.r[0] = into;
    Ok(())
}

/// `tmpfile()`, or `tmpfile64` where `large`: a temporary file opened for
/// update, removed when it is closed; null where it cannot be made.
fn temporary_stream(os: &mut Os<'_>, regs: &mut Regs, large: bool) -> Result<(), Fail> {
    let statics = context::statics_at(os, regs.r[10])?;
    regs.r[0] = 0;
    let (Some(at), Some(mode)) = (free_stream(os, statics)?, mode(TMPFILE_MODE)) else {
        return Ok(());
    };
    let mode = mode.large_where(large);
    let name = alloc::allocated(os, statics, L_TMPNAM)?;
    if name == 0 {
        return Ok(());
    }
    temporary_name(os, statics, name)?;
    if !open_into(os, statics, at, name, mode)? {
        return alloc::freed(os, statics, name);
    }
    let mut stream = Stream::load(os, at)?;
    stream.remove = name;
    stream.store(os)?;
    regs.r[0] = at;
    Ok(())
}

/// `tmpfile()`.
pub(in crate::c) fn tmpfile(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    temporary_stream(os, regs, false)
}

/// `tmpfile64()`.
pub(crate) fn tmpfile64(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    temporary_stream(os, regs, true)
}
