//! The postmortem backtrace the C library writes to `stderr` as RISC OS
//! 5.30's does: from a trap's registers or a caller's frame, each frame's
//! saved arguments and then its function, by the name the APCS keeps before
//! it, the library's own frames named as the library's.

use alloc::vec::Vec;

use tairix_riscos_kernel::hex8;
use tairix_riscos_kernel::os::{Fail, Os, Regs};

use super::report;
use crate::kernel::context;
use crate::kernel::frame::{code_address, readable, SAVED_FP, SAVED_LR, SAVED_PC};
use crate::kernel::statics::Statics;
use crate::kernel::unwind::{block, storing_instruction};
use crate::module::{message_with, SHARED_C_LIBRARY};

/// What heads a backtrace: the request, or the address a checked access
/// failed at.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Heading {
    Postmortem,
    Address(u32),
}

/// The most frames a backtrace walks.
const FRAMES_MOST: u32 = 1024;
/// `MOV IP, SP`: the instruction an APCS function starts with.
const MOV_IP_SP: u32 = 0xE1A0_C00D;
/// A function name's word: the top byte all ones, the name's length below.
const NAME_MARK: u32 = 0xFF00_0000;
const NAME_LENGTH: u32 = 0x00FF_FFFF;
/// The longest function name read.
const NAME_MOST: u32 = 256;
/// The argument registers, R0 to R3, and the words a pointer argument shows.
const ARGUMENTS: u32 = 4;
const POINTED_WORDS: u32 = 4;

/// `value` as `%#.8x` writes it: eight digits, `0x` before all but nought.
fn hex_alternate(value: u32) -> Vec<u8> {
    let digits = hex8(value).to_ascii_lowercase();
    if value == 0 {
        digits
    } else {
        [&b"0x"[..], &digits].concat()
    }
}

/// `value` in lower-case hexadecimal without leading noughts, as `%x`.
fn hex_short(value: u32) -> Vec<u8> {
    let digits = hex8(value).to_ascii_lowercase();
    let first = digits
        .iter()
        .position(|&d| d != b'0')
        .unwrap_or(digits.len() - 1);
    digits[first..].to_vec()
}

fn line(os: &mut Os<'_>, out: &mut Vec<u8>, token: &str, arguments: &[&[u8]]) -> Result<(), Fail> {
    out.extend_from_slice(&message_with(os, token, arguments)?);
    out.push(b'\n');
    Ok(())
}

/// The backtrace headed `heading` written to `stderr`, walking frames from
/// the function at `pc` whose frame is at `fp`.
pub(crate) fn write(
    os: &mut Os<'_>,
    statics: Statics,
    heading: Heading,
    pc: u32,
    fp: u32,
) -> Result<(), Fail> {
    let out = text(os, heading, pc, fp)?;
    report::to_stderr(os, statics, &out)
}

/// The backtrace [`write()`] writes.
pub(crate) fn text(os: &mut Os<'_>, heading: Heading, pc: u32, fp: u32) -> Result<Vec<u8>, Fail> {
    let mut out = Vec::new();
    match heading {
        Heading::Postmortem => {
            out.push(b'\n');
            line(os, &mut out, "Postmortem", &[])?;
        }
        Heading::Address(address) => {
            line(
                os,
                &mut out,
                "BadAddress",
                &[&hex8(address).to_ascii_lowercase()],
            )?;
            out.push(b'\n');
        }
    }
    let bits = os.caller_bits();
    let (mut pc, mut fp) = (pc, fp);
    for _ in 0..FRAMES_MOST {
        if fp == 0 {
            break;
        }
        let record = (fp % 4 == 0)
            .then(|| {
                [SAVED_PC, SAVED_LR, SAVED_FP].map(|saved| readable(os, fp.wrapping_sub(saved)))
            })
            .and_then(|[a, b, c]| Some([a?, b?, c?]));
        let Some([saved_pc, lr, caller]) = record else {
            out.push(b'\n');
            line(os, &mut out, "StackOverwritten", &[])?;
            break;
        };
        frame(os, &mut out, pc, fp, code_address(bits, saved_pc))?;
        if caller != 0 && caller <= fp {
            out.push(b'\n');
            line(os, &mut out, "StackOverwritten", &[])?;
            break;
        }
        (pc, fp) = (code_address(bits, lr), caller);
    }
    Ok(out)
}

/// The frame at `fp` of the function at `pc`, whose frame-storing
/// instruction its saved PC follows: its arguments, then its line.
fn frame(os: &mut Os<'_>, out: &mut Vec<u8>, pc: u32, fp: u32, saved_pc: u32) -> Result<(), Fail> {
    let place = hex_short(pc);
    if os.holds_code_of(&SHARED_C_LIBRARY, pc) {
        return line(os, out, "LibraryFrame", &[&place]);
    }
    let Some((store, list)) = storing_instruction(os, saved_pc) else {
        return line(os, out, "UnnamedFrame", &[&place]);
    };
    for n in (0..ARGUMENTS).rev().filter(|n| list & 1 << n != 0) {
        let above = (n + 1..16).fold(0, |above, k| above + u32::from(list & 1 << k != 0));
        let Some(value) = readable(os, fp.wrapping_sub(4 * above)) else {
            continue;
        };
        argument(os, out, n + 1, value)?;
    }
    match function_name(os, store) {
        Some(name) => line(os, out, "NamedFrame", &[&place, &name]),
        None => line(os, out, "UnnamedFrame", &[&place]),
    }
}

/// Argument `number`'s line: its value, and where it is a word-aligned
/// address the caller may read, the words there.
fn argument(os: &mut Os<'_>, out: &mut Vec<u8>, number: u32, value: u32) -> Result<(), Fail> {
    let decimal = tairix_riscos_kernel::Decimal::of(value.cast_signed());
    out.extend_from_slice(&message_with(
        os,
        "Argument",
        &[
            tairix_riscos_kernel::Decimal::of(number.cast_signed()).as_bytes(),
            &hex_alternate(value),
            decimal.as_bytes(),
        ],
    )?);
    let pointed: Option<Vec<u32>> = value
        .is_multiple_of(4)
        .then(|| {
            (0..POINTED_WORDS)
                .map(|n| readable(os, value.wrapping_add(4 * n)))
                .collect()
        })
        .flatten();
    if let Some(words) = pointed {
        out.extend_from_slice(b" -> [");
        for (n, &word) in words.iter().enumerate() {
            if n > 0 {
                out.push(b' ');
            }
            out.extend_from_slice(&hex_alternate(word));
        }
        out.push(b']');
    }
    out.push(b'\n');
    Ok(())
}

/// The name the APCS keeps before the function whose frame-storing
/// instruction is at `store`: the word before its entry the name's length
/// with the top byte set, the name before that.
fn function_name(os: &mut Os<'_>, store: u32) -> Option<Vec<u8>> {
    let entry = match readable(os, store.wrapping_sub(4)) {
        Some(MOV_IP_SP) => store.wrapping_sub(4),
        _ => store,
    };
    let mark = readable(os, entry.wrapping_sub(4))?;
    let length = mark & NAME_LENGTH;
    if mark & !NAME_LENGTH != NAME_MARK
        || length == 0
        || length > NAME_MOST
        || !length.is_multiple_of(4)
    {
        return None;
    }
    let start = entry.wrapping_sub(4).wrapping_sub(length);
    let mut name = Vec::new();
    for n in 0..length / 4 {
        name.extend_from_slice(&readable(os, start.wrapping_add(4 * n))?.to_le_bytes());
    }
    let end = name.iter().position(|&b| b == 0).unwrap_or(name.len());
    name.truncate(end);
    Some(name)
}

/// `_backtrace(why, address, uwb)`: the backtrace from the frame the unwind
/// block describes, or the caller's where there is none, headed by the
/// address a read or write check failed at where `why` is that check's
/// error; then the program ended failing.
pub(super) fn backtrace(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let statics = context::statics_at(os, regs.r[10])?;
    let (why, address, uwb) = (regs.r[0], regs.r[1], regs.r[2]);
    let heading = match why {
        crate::kernel::trap::READ_FAIL | crate::kernel::trap::WRITE_FAIL => {
            Heading::Address(address)
        }
        _ => Heading::Postmortem,
    };
    let bits = os.caller_bits();
    let (pc, fp) = if uwb == 0 {
        let caller = os.registers();
        (caller.r[14], caller.r[11])
    } else {
        (
            os.as_caller().read_word(uwb.wrapping_add(block::PC))?,
            os.as_caller().read_word(uwb.wrapping_add(block::FP))?,
        )
    };
    write(os, statics, heading, code_address(bits, pc), fp)?;
    report::end_failing(os, statics)
}

#[cfg(test)]
mod tests;
