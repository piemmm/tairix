//! Where a `printer:` stream writes: the file `PrinterType$n` names for the
//! printer type its special field gives, or the configured type without one,
//! opened when the stream is and closed with it; nowhere for a type no file
//! is set for, as the printer stream itself treats one.
//!
//! RISC OS 5 on RPCEmu does not route by its special field as its PRM
//! describes; the PRM's routing is the one followed here.

use tairix_obey::number::{read_unsigned, Reading};
use tairix_riscos_kernel::error::obey_error;
use tairix_riscos_kernel::os::{Fail, Os, Regs};
use tairix_riscos_kernel::swi::os::{args, find, gbpb, ARGS, BPUT, BYTE, FIND, GBPB};
use tairix_riscos_kernel::swi::X;

use super::{own, with, Printing, UNKNOWN_PRINTER};
use crate::fileswitch::error;

/// The printer types a special field may name, as the PRM names them.
const NAMED: [(&[u8], u8); 7] = [
    (b"sink", 0),
    (b"null", 0),
    (b"parallel", 1),
    (b"centronics", 1),
    (b"serial", 2),
    (b"rs423", 2),
    (b"user", 3),
];

/// How many `printer:` opens may be under way one within another: a
/// `PrinterType$n` naming `printer:` itself would otherwise open without end.
const NESTING_MOST: u8 = 8;

/// `OS_Byte` 245, which reads the configured printer type.
const PRINTER_TYPE: u32 = 245;

/// What opening a printer type's file came to.
enum Opened {
    /// No file is set for the type: its output goes nowhere.
    Nowhere,
    To(u32),
    /// The file could not be opened, or opens were nested too deep.
    Refused,
}

/// A stream on `handle` opened to the file the printer type `special`
/// names; false where that cannot be opened.
pub(super) fn open(os: &mut Os<'_>, handle: u32, special: Option<&[u8]>) -> Result<bool, Fail> {
    let target = match target(os, special)? {
        Opened::Refused => return Ok(false),
        Opened::Nowhere => None,
        Opened::To(file) => Some(file),
    };
    let kept = with(os, |state| {
        let at = match state.printers.binary_search_by_key(&handle, |p| p.handle) {
            Ok(at) | Err(at) => at,
        };
        let room = state.printers.try_reserve(1).is_ok();
        if room {
            state.printers.insert(at, Printing { handle, target });
        }
        room
    });
    let refusal = match kept {
        Ok(true) => return Ok(true),
        Ok(false) => os.no_memory(),
        Err(gone) => gone,
    };
    if let Some(file) = target {
        close_file(os, file)?;
    }
    Err(refusal)
}

/// The file the printer type `special` names, opened to be written.
fn target(os: &mut Os<'_>, special: Option<&[u8]>) -> Result<Opened, Fail> {
    let printer_type = printer_type(os, special)?;
    let nested = with(os, |state| {
        let room = state.opening_printers < NESTING_MOST;
        if room {
            state.opening_printers += 1;
        }
        room
    })?;
    if !nested {
        return Ok(Opened::Refused);
    }
    let opened = open_file(os, printer_type);
    // The open runs guest code, which may have started the module afresh,
    // its count with it.
    let unnested = with(os, |state| {
        state.opening_printers = state.opening_printers.saturating_sub(1);
    });
    if let Err(gone) = unnested {
        if let Ok(Opened::To(file)) = opened {
            close_file(os, file)?;
        }
        return Err(gone);
    }
    opened
}

fn open_file(os: &mut Os<'_>, printer_type: u8) -> Result<Opened, Fail> {
    let Some(file) = os.printer_file(printer_type)? else {
        return Ok(Opened::Nowhere);
    };
    os.with_text(&file, 0, |os, at| {
        let mut regs = Regs::default();
        regs.r[..2].copy_from_slice(&[find::OUTPUT, at]);
        os.perform(X | FIND, &mut regs)?;
        Ok(match regs.r[0] {
            0 => Opened::Refused,
            file => Opened::To(file),
        })
    })
}

/// The printer type a special field gives: one of the names, a number to
/// 255, or the configured type for none; "no such printer type" for
/// anything else that does not begin with a digit.
fn printer_type(os: &mut Os<'_>, special: Option<&[u8]>) -> Result<u8, Fail> {
    let field = special.unwrap_or_default();
    if field.is_empty() {
        return configured(os);
    }
    if let Some(&(_, named)) = NAMED
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(field))
    {
        return Ok(named);
    }
    if !field.first().is_some_and(u8::is_ascii_digit) {
        return Err(os.own_error(own, UNKNOWN_PRINTER, &[field]));
    }
    let reading = Reading {
        limit: u64::from(u8::MAX),
        terminated: true,
        ..Reading::plain(10)
    };
    let number = read_unsigned(field, reading).map_err(|e| os.error(obey_error(e)))?;
    u8::try_from(number.0).map_err(|_| os.error(obey_error(tairix_obey::Error::NumberTooBig)))
}

/// The configured printer type, which `OS_Byte` 245 reads.
fn configured(os: &mut Os<'_>) -> Result<u8, Fail> {
    let mut regs = Regs::default();
    regs.r[..3].copy_from_slice(&[PRINTER_TYPE, 0, 0xFF]);
    os.perform(X | BYTE, &mut regs)?;
    Ok(regs.byte(1))
}

/// The file the stream `handle` writes through: none where its output goes
/// nowhere; "bad operation" for no such stream.
fn file_of(os: &mut Os<'_>, handle: u32) -> Result<Option<u32>, Fail> {
    let found = with(os, |state| {
        let at = state
            .printers
            .binary_search_by_key(&handle, |p| p.handle)
            .ok()?;
        Some(state.printers[at].target)
    })?;
    found.ok_or_else(|| super::refused(os, super::Device::Printer))
}

/// `byte` written to the stream `handle`.
pub(super) fn put(os: &mut Os<'_>, handle: u32, byte: u8) -> Result<(), Fail> {
    let Some(file) = file_of(os, handle)? else {
        return Ok(());
    };
    let mut regs = Regs::default();
    regs.r[..2].copy_from_slice(&[u32::from(byte), file]);
    os.perform(X | BPUT, &mut regs)
}

/// `count` bytes from `at` written to the stream `handle` in one call, as
/// RISC OS 5 passes them on: how many were written.
pub(super) fn write(os: &mut Os<'_>, handle: u32, at: u32, count: u32) -> Result<u32, Fail> {
    match file_of(os, handle)? {
        Some(file) => write_file(os, file, at, count),
        None => Ok(count),
    }
}

fn write_file(os: &mut Os<'_>, file: u32, at: u32, count: u32) -> Result<u32, Fail> {
    let mut regs = Regs::default();
    regs.r[..4].copy_from_slice(&[gbpb::WRITE, file, at, count]);
    os.perform(X | GBPB, &mut regs)?;
    Ok(count - regs.r[3].min(count))
}

/// The stream `handle` closed, and the file it writes through, unless that
/// has been closed already: any program may close any file, and a stream
/// that could not then be closed could never be.
pub(super) fn close(os: &mut Os<'_>, handle: u32) -> Result<(), Fail> {
    let closed = with(os, |state| {
        let at = state
            .printers
            .binary_search_by_key(&handle, |p| p.handle)
            .ok()?;
        Some(state.printers.remove(at))
    })?;
    match closed {
        Some(Printing {
            target: Some(file), ..
        }) if still_open(os, file)? => close_file(os, file),
        _ => Ok(()),
    }
}

/// Whether a file is open on `file`.
fn still_open(os: &mut Os<'_>, file: u32) -> Result<bool, Fail> {
    let mut regs = Regs::default();
    regs.r[..2].copy_from_slice(&[args::STATUS, file]);
    os.perform(X | ARGS, &mut regs)?;
    Ok(regs.r[0] & args::status::UNALLOCATED == 0)
}

fn close_file(os: &mut Os<'_>, file: u32) -> Result<(), Fail> {
    let mut regs = Regs::default();
    regs.r[..2].copy_from_slice(&[find::CLOSE, file]);
    os.perform(X | FIND, &mut regs)
}

/// `count` bytes from `start` written to the file the printer type
/// `special` names, as a stream opened, written and closed would write
/// them; "unable to open" where that file cannot be.
pub(super) fn save(
    os: &mut Os<'_>,
    special: Option<&[u8]>,
    start: u32,
    count: u32,
) -> Result<(), Fail> {
    let file = match target(os, special)? {
        Opened::Nowhere => return Ok(()),
        Opened::To(file) => file,
        Opened::Refused => {
            return Err(error::fail(os, error::CANT_OPEN, &[b"printer:".as_slice()]))
        }
    };
    let written = write_file(os, file, start, count);
    let closed = close_file(os, file);
    written.and(closed)
}
