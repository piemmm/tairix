//! `stdio.h` as RISC OS 5.30's library has it: the PRM's streams, their
//! characters, lines and blocks, positions and indicators, through the
//! `FILE` blocks programs' macros share with the library.

pub(crate) mod format;
pub(crate) mod open;
pub(crate) mod printf;
pub(crate) mod scan;
pub(crate) mod stream;

use tairix_riscos_arm::mmu::{to_page_end, PAGE_SIZE};
use tairix_riscos_kernel::os::{Fail, Os, Regs};

use crate::c::alloc;
use crate::c::errno::{self, EDOM, EOVERFLOW};
use crate::c::statics::at;
use crate::c::string;
use crate::kernel::context;
use crate::kernel::statics::Statics;
use stream::{flag, Stream, EOF};

/// The library's statics for the client calling, and the stream whose
/// `FILE` block is at `at`.
fn opened(os: &mut Os<'_>, regs: &Regs, at: u32) -> Result<(Statics, Stream), Fail> {
    let statics = context::statics_at(os, regs.r[10])?;
    Ok((statics, Stream::load(os, at)?))
}

/// `fgetc(stream)`, and `getc(stream)`.
pub(super) fn fgetc(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (statics, mut stream) = opened(os, regs, regs.r[0])?;
    regs.r[0] = stream.getc(os, statics)?;
    stream.store(os)
}

/// `getchar()`.
pub(super) fn getchar(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let statics = context::statics_at(os, regs.r[10])?;
    let mut stream = Stream::load(os, stream::stdin(statics))?;
    regs.r[0] = stream.getc(os, statics)?;
    stream.store(os)
}

/// `fputc(c, stream)`, and `putc(c, stream)`.
pub(super) fn fputc(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (statics, mut stream) = opened(os, regs, regs.r[1])?;
    regs.r[0] = stream.putc(os, statics, regs.byte(0))?;
    stream.store(os)
}

/// `putchar(c)`.
pub(super) fn putchar(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let statics = context::statics_at(os, regs.r[10])?;
    let mut stream = Stream::load(os, stream::stdout(statics))?;
    regs.r[0] = stream.putc(os, statics, regs.byte(0))?;
    stream.store(os)
}

/// `__filbuf(stream)`, which the `getc` macro calls once its count is
/// spent.
pub(super) fn filbuf(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (statics, mut stream) = opened(os, regs, regs.r[0])?;
    regs.r[0] = stream.get(os, statics)?;
    stream.store(os)
}

/// `__flsbuf(ch, stream)`, which the `putc` macro calls once its count is
/// spent.
pub(super) fn flsbuf(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (statics, mut stream) = opened(os, regs, regs.r[1])?;
    regs.r[0] = stream.put(os, statics, regs.byte(0))?;
    stream.store(os)
}

/// `ungetc(c, stream)`.
pub(super) fn ungetc(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let mut stream = Stream::load(os, regs.r[1])?;
    regs.r[0] = stream.unget(regs.r[0]);
    stream.store(os)
}

/// `fgets(s, n, stream)`: `s`, or null where the input ended or failed
/// before a character.
pub(super) fn fgets(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (into, room) = (regs.r[0], regs.r[1].cast_signed());
    let (statics, mut stream) = opened(os, regs, regs.r[2])?;
    let Some(most) = room
        .checked_sub(1)
        .and_then(|most| u32::try_from(most).ok())
    else {
        regs.r[0] = 0;
        return Ok(());
    };
    let (length, ended) = stream.read_line(os, statics, into, most, true)?;
    stream.store(os)?;
    if ended {
        regs.r[0] = 0;
        return Ok(());
    }
    os.as_caller().write_byte(into.wrapping_add(length), 0)?;
    regs.r[0] = into;
    Ok(())
}

/// `gets(s)`: a line of `stdin` without its newline at `s`, or null where
/// the input ended before a character.
pub(super) fn gets(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let into = regs.r[0];
    let statics = context::statics_at(os, regs.r[10])?;
    let mut stream = Stream::load(os, stream::stdin(statics))?;
    let (length, ended) = stream.read_line(os, statics, into, string::ANY_LENGTH, false)?;
    stream.store(os)?;
    if ended {
        regs.r[0] = 0;
        return Ok(());
    }
    os.as_caller().write_byte(into.wrapping_add(length), 0)?;
    regs.r[0] = into;
    Ok(())
}

/// The string at `text` written to `stream`, read a page at a time: false at
/// the first refusal.
fn write_string(
    os: &mut Os<'_>,
    statics: Statics,
    stream: &mut Stream,
    text: u32,
) -> Result<bool, Fail> {
    let mut page = [0; PAGE_SIZE as usize];
    let mut at = text;
    loop {
        let span = to_page_end(at);
        let part = &mut page[..span as usize];
        os.as_caller().read_bytes(at, part)?;
        let end = part.iter().position(|&b| b == 0);
        if !stream.write_all(os, statics, &part[..end.unwrap_or(part.len())])? {
            return Ok(false);
        }
        if end.is_some() {
            return Ok(true);
        }
        at = at.wrapping_add(span);
        os.yield_point()?;
    }
}

/// `fputs(s, stream)`: nought, or `EOF` where it was refused.
pub(super) fn fputs(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let text = regs.r[0];
    let (statics, mut stream) = opened(os, regs, regs.r[1])?;
    let written = write_string(os, statics, &mut stream, text)?;
    stream.store(os)?;
    regs.r[0] = if written { 0 } else { EOF };
    Ok(())
}

/// `puts(s)`: `s` and a newline to `stdout`; the newline, or `EOF` where it
/// was refused.
pub(super) fn puts(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let text = regs.r[0];
    let statics = context::statics_at(os, regs.r[10])?;
    let mut stream = Stream::load(os, stream::stdout(statics))?;
    let written =
        write_string(os, statics, &mut stream, text)? && stream.putc(os, statics, b'\n')? != EOF;
    stream.store(os)?;
    regs.r[0] = if written { u32::from(b'\n') } else { EOF };
    Ok(())
}

/// The bytes `size` items of `count` take, none past the address space.
fn total(size: u32, count: u32) -> Option<u32> {
    size.checked_mul(count)
}

/// `fread(ptr, size, nmemb, stream)`: the whole items read.
pub(super) fn fread(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (into, size) = (regs.r[0], regs.r[1]);
    let Some(bytes) = total(size, regs.r[2]).filter(|_| size != 0) else {
        regs.r[0] = 0;
        return Ok(());
    };
    let (statics, mut stream) = opened(os, regs, regs.r[3])?;
    let read = stream.read_into(os, statics, into, bytes)?;
    stream.store(os)?;
    regs.r[0] = read / size;
    Ok(())
}

/// `fwrite(ptr, size, nmemb, stream)`: the whole items written.
pub(super) fn fwrite(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (from, size) = (regs.r[0], regs.r[1]);
    let Some(bytes) = total(size, regs.r[2]).filter(|_| size != 0) else {
        regs.r[0] = 0;
        return Ok(());
    };
    let (statics, mut stream) = opened(os, regs, regs.r[3])?;
    let written = stream.write_from(os, statics, from, bytes)?;
    stream.store(os)?;
    regs.r[0] = written / size;
    Ok(())
}

/// What a positioning answers: made; refused by the file, whose extent
/// could not be read; or out of the reach of the call, or no position.
const POSITIONED: u32 = 0;
const REFUSED_BY_FILE: u32 = 1;
const OUT_OF_RANGE: u32 = 2;

/// `SEEK_SET`, `SEEK_CUR` and `SEEK_END`.
const FROM_START: u32 = 0;
const FROM_HERE: u32 = 1;
const FROM_END: u32 = 2;

/// `LONG_MAX`: the furthest position the 32-bit calls reach.
const LONG_MAX: u32 = i32::MAX.cast_unsigned();

/// What a position read answers where there is none to give.
const NO_POSITION: u32 = u32::MAX;

/// The stream sent `offset` from `whence`, no further than `most`: refused,
/// the stream in error, for a `whence` naming nothing, a target before the
/// start, and with `errno` `EOVERFLOW` one past `most`.
fn seek_to(
    os: &mut Os<'_>,
    statics: Statics,
    stream: &mut Stream,
    offset: i64,
    whence: u32,
    most: u32,
) -> Result<u32, Fail> {
    let from = match whence {
        FROM_START => 0,
        FROM_HERE => stream.tell(),
        FROM_END => {
            let Some(end) = stream.end(os, statics)? else {
                stream.refused();
                return Ok(REFUSED_BY_FILE);
            };
            end
        }
        _ => {
            stream.refused();
            return Ok(OUT_OF_RANGE);
        }
    };
    let target = i64::from(from).saturating_add(offset);
    if target < 0 {
        stream.refused();
        return Ok(OUT_OF_RANGE);
    }
    let Some(target) = u32::try_from(target).ok().filter(|&target| target <= most) else {
        errno::set(os, statics, EOVERFLOW)?;
        stream.refused();
        return Ok(OUT_OF_RANGE);
    };
    stream.seek(target);
    Ok(POSITIONED)
}

/// The stream sent to `position`, as `fsetpos` sends it: one before the
/// start or past `most` refused, the stream in error, with `errno` `EDOM`.
fn set_position(
    os: &mut Os<'_>,
    statics: Statics,
    stream: &mut Stream,
    position: i64,
    most: u32,
) -> Result<u32, Fail> {
    let Some(target) = u32::try_from(position)
        .ok()
        .filter(|&target| target <= most)
    else {
        errno::set(os, statics, EDOM)?;
        stream.refused();
        return Ok(OUT_OF_RANGE);
    };
    stream.seek(target);
    Ok(POSITIONED)
}

/// `position` as the 32-bit calls give it: minus one with `errno`
/// `EOVERFLOW` past `LONG_MAX`.
fn small_position(os: &mut Os<'_>, statics: Statics, position: u32) -> Result<u32, Fail> {
    if position > LONG_MAX {
        errno::set(os, statics, EOVERFLOW)?;
        return Ok(NO_POSITION);
    }
    Ok(position)
}

/// The 64-bit value in two words, the low first.
fn long_long([low, high]: [u32; 2]) -> i64 {
    (u64::from(high) << 32 | u64::from(low)).cast_signed()
}

/// `fseeko64(stream, offset, whence)`: the offset in R1 and R2, its low
/// word first.
pub(crate) fn fseeko64(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (statics, mut stream) = opened(os, regs, regs.r[0])?;
    regs.r[0] = seek_to(
        os,
        statics,
        &mut stream,
        long_long([regs.r[1], regs.r[2]]),
        regs.r[3],
        u32::MAX,
    )?;
    stream.store(os)
}

/// `ftello64(stream)`: the position in R0 and R1; minus one with `errno`
/// `EDOM` for no stream of the library's.
pub(crate) fn ftello64(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let statics = context::statics_at(os, regs.r[10])?;
    let at = regs.r[0];
    if !stream::is_stream(statics, at) {
        errno::set(os, statics, EDOM)?;
        regs.r[..2].copy_from_slice(&[NO_POSITION; 2]);
        return Ok(());
    }
    regs.r[..2].copy_from_slice(&[Stream::load(os, at)?.tell(), 0]);
    Ok(())
}

/// `fgetpos64(stream, pos)`: the position stored at `pos` as two words, its
/// low word first.
pub(crate) fn fgetpos64(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let position = Stream::load(os, regs.r[0])?.tell();
    os.as_caller().write_words(regs.r[1], &[position, 0])?;
    regs.r[0] = 0;
    Ok(())
}

/// `fsetpos64(stream, pos)`: the two-word position at `pos`.
pub(crate) fn fsetpos64(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let mut words = [0; 2];
    os.as_caller().read_words(regs.r[1], &mut words)?;
    let (statics, mut stream) = opened(os, regs, regs.r[0])?;
    regs.r[0] = set_position(os, statics, &mut stream, long_long(words), u32::MAX)?;
    stream.store(os)
}

/// `fseek(stream, offset, whence)`.
pub(super) fn fseek(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (statics, mut stream) = opened(os, regs, regs.r[0])?;
    let offset = i64::from(regs.r[1].cast_signed());
    regs.r[0] = seek_to(os, statics, &mut stream, offset, regs.r[2], LONG_MAX)?;
    stream.store(os)
}

/// `ftell(stream)`.
pub(super) fn ftell(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (statics, stream) = opened(os, regs, regs.r[0])?;
    regs.r[0] = small_position(os, statics, stream.tell())?;
    Ok(())
}

/// `fgetpos(stream, pos)`: the position stored at `pos`; nought, even where
/// that is the minus one of a position past `LONG_MAX`.
pub(super) fn fgetpos(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (statics, stream) = opened(os, regs, regs.r[0])?;
    let position = small_position(os, statics, stream.tell())?;
    os.as_caller().write_word(regs.r[1], position)?;
    regs.r[0] = 0;
    Ok(())
}

/// `fsetpos(stream, pos)`.
pub(super) fn fsetpos(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let position = os.as_caller().read_word(regs.r[1])?;
    let (statics, mut stream) = opened(os, regs, regs.r[0])?;
    regs.r[0] = set_position(
        os,
        statics,
        &mut stream,
        i64::from(position.cast_signed()),
        LONG_MAX,
    )?;
    stream.store(os)
}

/// `rewind(stream)`: at the start, both indicators cleared; R0 left at the
/// stream's flags, as RISC OS 5.30 leaves it.
pub(super) fn rewind(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let at = regs.r[0];
    let mut stream = Stream::load(os, at)?;
    stream.seek(0);
    stream.flag &= !(flag::EOF | flag::ERROR);
    stream.store(os)?;
    regs.r[0] = at.wrapping_add(stream::word::FLAG);
    Ok(())
}

/// `clearerr(stream)`: both indicators cleared; R0 left at the stream's
/// flags.
pub(super) fn clearerr(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let at = regs.r[0];
    let mut stream = Stream::load(os, at)?;
    stream.flag &= !(flag::EOF | flag::ERROR);
    stream.store(os)?;
    regs.r[0] = at.wrapping_add(stream::word::FLAG);
    Ok(())
}

/// `feof(stream)`: the end-of-file flag, as the macro reads it.
pub(super) fn feof(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    regs.r[0] = flags_of(os, regs.r[0])? & flag::EOF;
    Ok(())
}

/// `ferror(stream)`: the error flag, as the macro reads it.
pub(super) fn ferror(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    regs.r[0] = flags_of(os, regs.r[0])? & flag::ERROR;
    Ok(())
}

/// The flags word of the `FILE` block at `at`, read as a macro reads it,
/// wherever `at` points.
fn flags_of(os: &mut Os<'_>, at: u32) -> Result<u32, Fail> {
    os.as_caller()
        .read_word(at.wrapping_add(stream::word::FLAG))
}

/// `fflush(stream)`: every output stream's for null; nought, or `EOF` where
/// a file refused.
pub(super) fn fflush(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let statics = context::statics_at(os, regs.r[10])?;
    let flushed = if regs.r[0] == 0 {
        stream::flush_all(os, statics)?
    } else {
        let mut stream = Stream::load(os, regs.r[0])?;
        let flushed = stream.flush(os, statics)?;
        stream.store(os)?;
        flushed
    };
    regs.r[0] = if flushed { 0 } else { EOF };
    Ok(())
}

/// What `setvbuf` answers where it cannot honour a request.
const NOT_HONOURED: u32 = 1;

/// `setvbuf(stream, buf, mode, size)`: only before the stream's first
/// transfer; an unbuffered stream's buffer its extra block's first byte, and
/// a buffer the library gave the stream given back.
pub(super) fn setvbuf(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (buffer, mode, size) = (regs.r[1], regs.r[2], regs.r[3]);
    let (statics, mut stream) = opened(os, regs, regs.r[0])?;
    let honoured = stream.flag & flag::SET_UP == 0
        && matches!(mode, flag::FULLY | flag::LINE | flag::UNBUFFERED);
    if !honoured || (mode != flag::UNBUFFERED && size == 0) {
        regs.r[0] = NOT_HONOURED;
        return Ok(());
    }
    if stream.flag & flag::SYSTEM_BUFFER != 0 {
        alloc::freed(os, statics, stream.base)?;
    }
    stream.flag = (stream.flag & !(flag::BUFFERING | flag::SYSTEM_BUFFER)) | mode;
    if mode == flag::UNBUFFERED {
        stream.base = stream.extra;
        stream.ptr = stream.extra;
        stream.bufsiz = 1;
    } else {
        stream.base = buffer;
        stream.ptr = buffer;
        stream.bufsiz = size;
    }
    stream.store(os)?;
    regs.r[0] = 0;
    Ok(())
}

/// `setbuf(stream, buf)`: `setvbuf`'s full buffering of `BUFSIZ` bytes, or
/// no buffering for null, its answer left in R0.
pub(super) fn setbuf(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    regs.r[2] = if regs.r[1] == 0 {
        flag::UNBUFFERED
    } else {
        flag::FULLY
    };
    regs.r[3] = stream::BUFSIZ;
    setvbuf(os, regs)
}

/// `perror(s)`: `s: `, where `s` is a string, and `errno`'s text and a
/// newline written to `stderr`.
pub(super) fn perror(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let prefix = regs.r[0];
    let statics = context::statics_at(os, regs.r[10])?;
    let errno = statics.read(os, at::ERRNO)?;
    let text = string::error_text(os, statics, errno)?;
    let mut stream = Stream::load(os, stream::stderr(statics))?;
    let prefixed = prefix != 0 && os.as_caller().read_byte(prefix)? != 0;
    let written = !prefixed
        || write_string(os, statics, &mut stream, prefix)?
            && stream.write_all(os, statics, b": ")?;
    if written && write_string(os, statics, &mut stream, text)? {
        stream.putc(os, statics, b'\n')?;
    }
    stream.store(os)
}

#[cfg(test)]
mod tests;
