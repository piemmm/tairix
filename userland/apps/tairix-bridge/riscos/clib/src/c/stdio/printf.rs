//! The `printf` family's entries: arguments from the caller's registers and
//! stack, or from a `va_list` — a one-word array holding the argument
//! pointer, which is left past what the format took — written to a stream,
//! where an output error answers minus one, or to memory, terminated.

use tairix_riscos_kernel::os::{len32, Fail, Os, Regs};

use super::format::{format, Arguments, Floats, Sink, VaList, Variadic};
use super::stream::{self, Stream, EOF};
use crate::kernel::context;
use crate::kernel::statics::Statics;

/// Text written to memory from `at`.
struct Memory {
    at: u32,
    written: u32,
}

impl Sink for Memory {
    fn put(&mut self, os: &mut Os<'_>, bytes: &[u8]) -> Result<(), Fail> {
        os.as_caller()
            .write_bytes(self.at.wrapping_add(self.written), bytes)?;
        self.written = self
            .written
            .wrapping_add(u32::try_from(bytes.len()).unwrap_or(u32::MAX));
        Ok(())
    }
}

/// Text written to a stream, as `putc` writes it; whether any of it was
/// refused.
struct ToStream {
    stream: Stream,
    statics: Statics,
    refused: bool,
}

impl Sink for ToStream {
    fn put(&mut self, os: &mut Os<'_>, bytes: &[u8]) -> Result<(), Fail> {
        if !self.refused && !self.stream.write_all(os, self.statics, bytes)? {
            self.refused = true;
        }
        Ok(())
    }

    fn keeps(&self) -> usize {
        if self.refused {
            0
        } else {
            usize::MAX
        }
    }
}

/// The format at `format_at` written to the stream at `at`: the bytes, or
/// `EOF` where the stream refused any.
fn to_stream(
    os: &mut Os<'_>,
    statics: Statics,
    at: u32,
    format_at: u32,
    args: &mut impl Arguments,
    floats: Floats,
) -> Result<u32, Fail> {
    let mut sink = ToStream {
        stream: Stream::load(os, at)?,
        statics,
        refused: false,
    };
    let written = format(os, format_at, args, &mut sink, floats)?;
    sink.stream.store(os)?;
    Ok(if sink.refused { EOF } else { written })
}

/// The format at `format_at` written to memory at `into`, terminated: the
/// bytes before the terminator.
fn to_memory(
    os: &mut Os<'_>,
    into: u32,
    format_at: u32,
    args: &mut impl Arguments,
    floats: Floats,
) -> Result<u32, Fail> {
    let mut sink = Memory {
        at: into,
        written: 0,
    };
    let written = format(os, format_at, args, &mut sink, floats)?;
    os.as_caller()
        .write_byte(into.wrapping_add(sink.written), 0)?;
    Ok(written)
}

/// Text written to memory from `at`, at most `room` bytes of it; the rest
/// only counted.
struct Bounded {
    at: u32,
    room: u32,
    written: u32,
}

impl Sink for Bounded {
    fn put(&mut self, os: &mut Os<'_>, bytes: &[u8]) -> Result<(), Fail> {
        let kept = &bytes[..bytes.len().min(self.keeps())];
        if !kept.is_empty() {
            os.as_caller()
                .write_bytes(self.at.wrapping_add(self.written), kept)?;
        }
        self.count(bytes.len());
        Ok(())
    }

    fn keeps(&self) -> usize {
        self.room.saturating_sub(self.written) as usize
    }

    fn count(&mut self, bytes: usize) {
        self.written = self.written.saturating_add(len32(bytes));
    }
}

/// The format at `format_at` written to the `size` bytes at `into`: as much
/// as fits before a terminator, none at all for a size of nought. The bytes
/// the whole would take.
fn to_bounded(
    os: &mut Os<'_>,
    [into, size, format_at]: [u32; 3],
    args: &mut impl Arguments,
    floats: Floats,
) -> Result<u32, Fail> {
    let room = size.saturating_sub(1);
    let mut sink = Bounded {
        at: into,
        room,
        written: 0,
    };
    let written = format(os, format_at, args, &mut sink, floats)?;
    if size != 0 {
        os.as_caller()
            .write_byte(into.wrapping_add(written.min(room)), 0)?;
    }
    Ok(written)
}

/// `snprintf(s, n, format, ...)`, and `_snprintf` without floating point.
fn snprintf_with(os: &mut Os<'_>, regs: &mut Regs, floats: Floats) -> Result<(), Fail> {
    let mut args = Variadic::of(os, regs, 3);
    regs.r[0] = to_bounded(os, [regs.r[0], regs.r[1], regs.r[2]], &mut args, floats)?;
    Ok(())
}

/// `snprintf(s, n, format, ...)`.
pub(crate) fn snprintf(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    snprintf_with(os, regs, Floats::Converted)
}

/// `_snprintf(s, n, format, ...)`.
pub(crate) fn snprintf_without_floats(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    snprintf_with(os, regs, Floats::Absent)
}

/// `vsnprintf(s, n, format, arg)`.
pub(crate) fn vsnprintf(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let mut args = VaList::of(os, regs.r[3])?;
    regs.r[0] = to_bounded(
        os,
        [regs.r[0], regs.r[1], regs.r[2]],
        &mut args,
        Floats::Converted,
    )?;
    args.advanced(os)
}

/// `printf(format, ...)`, and `_printf` without floating point.
fn printf_with(os: &mut Os<'_>, regs: &mut Regs, floats: Floats) -> Result<(), Fail> {
    let statics = context::statics_at(os, regs.r[10])?;
    let mut args = Variadic::of(os, regs, 1);
    regs.r[0] = to_stream(
        os,
        statics,
        stream::stdout(statics),
        regs.r[0],
        &mut args,
        floats,
    )?;
    Ok(())
}

/// `fprintf(stream, format, ...)`, and `_fprintf`.
fn fprintf_with(os: &mut Os<'_>, regs: &mut Regs, floats: Floats) -> Result<(), Fail> {
    let statics = context::statics_at(os, regs.r[10])?;
    let mut args = Variadic::of(os, regs, 2);
    regs.r[0] = to_stream(os, statics, regs.r[0], regs.r[1], &mut args, floats)?;
    Ok(())
}

/// `sprintf(s, format, ...)`, and `_sprintf`.
fn sprintf_with(os: &mut Os<'_>, regs: &mut Regs, floats: Floats) -> Result<(), Fail> {
    let mut args = Variadic::of(os, regs, 2);
    regs.r[0] = to_memory(os, regs.r[0], regs.r[1], &mut args, floats)?;
    Ok(())
}

/// `vfprintf(stream, format, arg)`, and `_vfprintf`.
fn vfprintf_with(os: &mut Os<'_>, regs: &mut Regs, floats: Floats) -> Result<(), Fail> {
    let statics = context::statics_at(os, regs.r[10])?;
    let mut args = VaList::of(os, regs.r[2])?;
    regs.r[0] = to_stream(os, statics, regs.r[0], regs.r[1], &mut args, floats)?;
    args.advanced(os)
}

/// `printf(format, ...)`.
pub(in crate::c) fn printf(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    printf_with(os, regs, Floats::Converted)
}

/// `_printf(format, ...)`.
pub(in crate::c) fn printf_without_floats(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    printf_with(os, regs, Floats::Absent)
}

/// `fprintf(stream, format, ...)`.
pub(in crate::c) fn fprintf(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    fprintf_with(os, regs, Floats::Converted)
}

/// `_fprintf(stream, format, ...)`.
pub(in crate::c) fn fprintf_without_floats(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    fprintf_with(os, regs, Floats::Absent)
}

/// `sprintf(s, format, ...)`.
pub(in crate::c) fn sprintf(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    sprintf_with(os, regs, Floats::Converted)
}

/// `_sprintf(s, format, ...)`.
pub(in crate::c) fn sprintf_without_floats(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    sprintf_with(os, regs, Floats::Absent)
}

/// `vfprintf(stream, format, arg)`.
pub(in crate::c) fn vfprintf(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    vfprintf_with(os, regs, Floats::Converted)
}

/// `_vfprintf(stream, format, arg)`.
pub(in crate::c) fn vfprintf_without_floats(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    vfprintf_with(os, regs, Floats::Absent)
}

/// `vprintf(format, arg)`.
pub(in crate::c) fn vprintf(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let statics = context::statics_at(os, regs.r[10])?;
    let mut args = VaList::of(os, regs.r[1])?;
    regs.r[0] = to_stream(
        os,
        statics,
        stream::stdout(statics),
        regs.r[0],
        &mut args,
        Floats::Converted,
    )?;
    args.advanced(os)
}

/// `vsprintf(s, format, arg)`.
pub(in crate::c) fn vsprintf(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let mut args = VaList::of(os, regs.r[2])?;
    regs.r[0] = to_memory(os, regs.r[0], regs.r[1], &mut args, Floats::Converted)?;
    args.advanced(os)
}
