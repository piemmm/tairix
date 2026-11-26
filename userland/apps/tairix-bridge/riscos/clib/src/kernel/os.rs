//! The library kernel's interface to the OS: the generic SWI calls, a
//! function for each of the commonest SWIs, and the last OS error, each
//! answering as RISC OS 5.30's — minus two for an OS error, which
//! `_kernel_last_oserror` then gives.

use alloc::vec::Vec;

use tairix_riscos_arm::psr;
use tairix_riscos_kernel::os::{Fail, Os, Regs};
use tairix_riscos_kernel::swi::{os as kos, X};

use super::context;
use super::statics::{at, Statics};

/// What a function answers for an OS error.
pub(crate) const OS_ERROR: u32 = (-2_i32).cast_unsigned();
/// What `_kernel_osbget` answers at the end of the file, and what
/// `_kernel_osgbpb` answers where the SWI set C.
const END_OF_FILE: u32 = u32::MAX;
const CARRY_SET: u32 = u32::MAX;
/// `_kernel_osgbpb`'s block, the SWI's R2 to R6, and `_kernel_osfile`'s,
/// R2 to R5.
const GBPB_BLOCK: usize = 5;
const FILE_BLOCK: usize = 4;
/// `_kernel_swi`'s registers block: R0 to R9.
const SWI_REGS: u32 = 10;
/// Bit 31 of a SWI's number asks `_kernel_swi` to leave its X bit clear.
const NO_X: u32 = 1 << 31;
/// An error block: its number, then its text.
pub(crate) const ERROR_BLOCK: u32 = 256;
const ERROR_TEXT_MOST: usize = ERROR_BLOCK as usize - 4;
/// The longest value `_kernel_setenv` sets.
const VALUE_MOST: u32 = 256 * 1024;
/// `OS_ReadVarVal`'s type asking a value expanded to a string, and
/// `OS_SetVarVal`'s for a string and its length deleting a variable.
pub(crate) const EXPANDED: u32 = 3;
const STRING: u32 = 0;
const DELETE: u32 = (-1_i32).cast_unsigned();

/// The OS error `block` noted as the last, for `_kernel_last_oserror`.
pub(crate) fn note(os: &mut Os<'_>, statics: Statics, block: u32) -> Result<(), Fail> {
    let number = os.as_caller().read_word(block)?;
    let text = os
        .read_until(block.wrapping_add(4), ERROR_TEXT_MOST - 1, |b| b == 0)?
        .0;
    note_error(os, statics, number, &text)
}

/// The error `number` with `text` noted as the last, cut to the buffer.
pub(crate) fn note_error(
    os: &mut Os<'_>,
    statics: Statics,
    number: u32,
    text: &[u8],
) -> Result<(), Fail> {
    let text = &text[..text.len().min(ERROR_TEXT_MOST - 1)];
    statics.write(os, at::LAST_ERROR, number)?;
    os.as_caller()
        .write_terminated(statics.field(at::LAST_ERROR + 4), text, 0, ERROR_TEXT_MOST)?;
    statics.write(os, at::ERROR_PENDING, 1)
}

/// The last error's text.
pub(crate) fn last_error_text(os: &mut Os<'_>, statics: Statics) -> Result<Vec<u8>, Fail> {
    os.as_caller()
        .read_zstring(statics.field(at::LAST_ERROR + 4), ERROR_TEXT_MOST)
}

/// `swi` performed with `regs`, an OS error noted and answered as minus
/// two, `answer` given the registers it left otherwise.
fn performed(
    os: &mut Os<'_>,
    sl: u32,
    swi: u32,
    regs: &mut Regs,
    answer: impl FnOnce(&Regs) -> u32,
) -> Result<u32, Fail> {
    match os.perform(X | swi, regs) {
        Ok(()) => Ok(answer(regs)),
        Err(Fail::Error(block)) => {
            let statics = context::statics_at(os, sl)?;
            note(os, statics, block)?;
            Ok(OS_ERROR)
        }
        Err(unwind) => Err(unwind),
    }
}

fn registers(low: &[u32]) -> Regs {
    let mut regs = Regs::default();
    regs.r[..low.len()].copy_from_slice(low);
    regs
}

/// `_kernel_hostos()`: R1 of `OS_Byte` 0 with R1 one.
pub(super) fn hostos(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let mut byte = registers(&[0, 1]);
    regs.r[0] = performed(os, regs.r[10], kos::BYTE, &mut byte, |regs| regs.r[1])?;
    Ok(())
}

/// `_kernel_command_string()`: the copy `_kernel_init` made.
pub(super) fn command_string(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let statics = context::statics_at(os, regs.r[10])?;
    regs.r[0] = statics.read(os, at::COMMAND)?;
    Ok(())
}

/// `_kernel_swi(number, in, out)`: the SWI performed with R0 to R9 from
/// `in` into `out` — which it fills on an error too, R0 the error — with
/// its X bit unless the number's bit 31 asks none; its error, which is also
/// noted, or nought.
pub(super) fn swi(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    regs.r[0] = called(os, regs, None)?;
    Ok(())
}

/// `_kernel_swi_c(number, in, out, carry)`: as `_kernel_swi`, the C flag the
/// SWI left given at `carry`.
pub(super) fn swi_c(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let carry = regs.r[3];
    regs.r[0] = called(os, regs, Some(carry))?;
    Ok(())
}

fn called(os: &mut Os<'_>, regs: &Regs, carry: Option<u32>) -> Result<u32, Fail> {
    let (number, input, output, sl) = (regs.r[0], regs.r[1], regs.r[2], regs.r[10]);
    let mut call = Regs::default();
    for (n, slot) in (0..SWI_REGS).zip(call.r.iter_mut()) {
        *slot = os.as_caller().read_word(input.wrapping_add(4 * n))?;
    }
    let swi = if number & NO_X != 0 {
        number & !(NO_X | X)
    } else {
        number | X
    };
    let error = match os.perform(swi, &mut call) {
        Ok(()) => 0,
        Err(Fail::Error(block)) => {
            let statics = context::statics_at(os, sl)?;
            note(os, statics, block)?;
            block
        }
        Err(unwind) => return Err(unwind),
    };
    if error != 0 {
        call.r[0] = error;
    }
    if output != 0 {
        for (n, &value) in (0..SWI_REGS).zip(&call.r) {
            os.as_caller()
                .write_word(output.wrapping_add(4 * n), value)?;
        }
    }
    if let Some(carry) = carry.filter(|&at| at != 0) {
        os.as_caller()
            .write_word(carry, u32::from(call.flags & psr::C != 0))?;
    }
    Ok(error)
}

/// `_kernel_osbyte(op, x, y)`: R1's low byte, R2's above it, C above that.
pub(super) fn osbyte(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let mut byte = registers(&regs.r[..3]);
    regs.r[0] = performed(os, regs.r[10], kos::BYTE, &mut byte, |done| {
        (done.r[1] & 0xFF) | (done.r[2] & 0xFF) << 8 | u32::from(done.flags & psr::C != 0) << 16
    })?;
    Ok(())
}

/// `_kernel_osrdch()`: the character read; minus it where an escape ended
/// the read.
pub(super) fn osrdch(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let mut read = Regs::default();
    regs.r[0] = performed(os, regs.r[10], kos::READ_C, &mut read, |done| {
        if done.flags & psr::C != 0 {
            done.r[0].wrapping_neg()
        } else {
            done.r[0]
        }
    })?;
    Ok(())
}

/// `_kernel_oswrch(ch)`: the character back.
pub(super) fn oswrch(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let mut write = registers(&regs.r[..1]);
    regs.r[0] = performed(os, regs.r[10], kos::WRITE_C, &mut write, |done| done.r[0])?;
    Ok(())
}

/// `_kernel_osbget(handle)`: the byte, or minus one at the file's end.
pub(super) fn osbget(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let mut get = registers(&[0, regs.r[0]]);
    regs.r[0] = performed(os, regs.r[10], kos::BGET, &mut get, |done| {
        if done.flags & psr::C != 0 {
            END_OF_FILE
        } else {
            done.r[0]
        }
    })?;
    Ok(())
}

/// `_kernel_osbput(ch, handle)`: the character back.
pub(super) fn osbput(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let mut put = registers(&regs.r[..2]);
    regs.r[0] = performed(os, regs.r[10], kos::BPUT, &mut put, |done| done.r[0])?;
    Ok(())
}

/// `_kernel_osgbpb(op, handle, block)`: R2 to R6 from the block and back
/// into it; R0 as the SWI left it, minus one where it set C.
pub(super) fn osgbpb(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let block = regs.r[2];
    let mut gbpb = registers(&regs.r[..2]);
    for (n, slot) in (0..).zip(&mut gbpb.r[2..2 + GBPB_BLOCK]) {
        *slot = os.as_caller().read_word(block.wrapping_add(4 * n))?;
    }
    let answer = performed(os, regs.r[10], kos::GBPB, &mut gbpb, |done| {
        if done.flags & psr::C != 0 {
            CARRY_SET
        } else {
            done.r[0]
        }
    })?;
    if answer != OS_ERROR {
        for (n, &value) in (0..).zip(&gbpb.r[2..2 + GBPB_BLOCK]) {
            os.as_caller()
                .write_word(block.wrapping_add(4 * n), value)?;
        }
    }
    regs.r[0] = answer;
    Ok(())
}

/// `_kernel_osword(op, data)`: the block's address back.
pub(super) fn osword(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let mut word = registers(&regs.r[..2]);
    regs.r[0] = performed(os, regs.r[10], kos::WORD, &mut word, |done| done.r[1])?;
    Ok(())
}

/// `_kernel_osfind(op, name)`: the handle, nought for none, or for a close
/// what R0 was left.
pub(super) fn osfind(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let mut find = registers(&regs.r[..2]);
    regs.r[0] = performed(os, regs.r[10], kos::FIND, &mut find, |done| done.r[0])?;
    Ok(())
}

/// `_kernel_osfile(op, name, block)`: R2 to R5 from the block and back into
/// it; the object's type.
pub(super) fn osfile(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let block = regs.r[2];
    let mut file = registers(&regs.r[..2]);
    for (n, slot) in (0..).zip(&mut file.r[2..2 + FILE_BLOCK]) {
        *slot = os.as_caller().read_word(block.wrapping_add(4 * n))?;
    }
    let answer = performed(os, regs.r[10], kos::FILE, &mut file, |done| done.r[0])?;
    if answer != OS_ERROR {
        for (n, &value) in (0..).zip(&file.r[2..2 + FILE_BLOCK]) {
            os.as_caller()
                .write_word(block.wrapping_add(4 * n), value)?;
        }
    }
    regs.r[0] = answer;
    Ok(())
}

/// `_kernel_osargs(op, handle, arg)`: the current filing system's number
/// for op nought of no file, which `OS_Args` answers in R0; R2 otherwise,
/// a file's pointer for op nought of one.
pub(super) fn osargs(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let filing_system = regs.r[0] == 0 && regs.r[1] == 0;
    let mut args = registers(&regs.r[..3]);
    regs.r[0] = performed(os, regs.r[10], kos::ARGS, &mut args, |done| {
        if filing_system {
            done.r[0]
        } else {
            done.r[2]
        }
    })?;
    Ok(())
}

/// `_kernel_oscli(command)`: one where it ran.
pub(super) fn oscli(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let mut cli = registers(&regs.r[..1]);
    regs.r[0] = performed(os, regs.r[10], kos::CLI, &mut cli, |_| 1)?;
    Ok(())
}

/// `_kernel_last_oserror()`: the error noted since it last answered, none
/// where none has been.
pub(super) fn last_oserror(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let statics = context::statics_at(os, regs.r[10])?;
    let pending = statics.read(os, at::ERROR_PENDING)? != 0;
    statics.write(os, at::ERROR_PENDING, 0)?;
    regs.r[0] = if pending {
        statics.field(at::LAST_ERROR)
    } else {
        0
    };
    Ok(())
}

/// `_kernel_getenv(name, buffer, size)`: the variable's value, expanded,
/// terminated in the buffer; the OS's error where it cannot be read, which
/// is also noted.
pub(super) fn getenv(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (name, buffer, size, sl) = (regs.r[0], regs.r[1], regs.r[2], regs.r[10]);
    let mut read = registers(&[name, buffer, size.saturating_sub(1), 0, EXPANDED]);
    regs.r[0] = match os.perform(X | kos::READ_VAR_VAL, &mut read) {
        Ok(()) => {
            os.as_caller()
                .write_byte(buffer.wrapping_add(read.r[2]), 0)?;
            0
        }
        Err(Fail::Error(block)) => {
            let statics = context::statics_at(os, sl)?;
            note(os, statics, block)?;
            block
        }
        Err(unwind) => return Err(unwind),
    };
    Ok(())
}

/// `_kernel_setenv(name, value)`: the variable set to the string `value`,
/// or deleted where `value` is nought; the OS's error, or nought.
pub(super) fn setenv(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (name, value, sl) = (regs.r[0], regs.r[1], regs.r[10]);
    let length = if value == 0 {
        DELETE
    } else {
        os.as_caller()
            .position(value, VALUE_MOST, |b| b == 0)?
            .unwrap_or(VALUE_MOST)
    };
    let mut set = registers(&[name, value, length, 0, STRING]);
    regs.r[0] = match os.perform(X | kos::SET_VAR_VAL, &mut set) {
        Ok(()) => 0,
        Err(Fail::Error(block)) => {
            let statics = context::statics_at(os, sl)?;
            note(os, statics, block)?;
            block
        }
        Err(unwind) => return Err(unwind),
    };
    Ok(())
}

/// `_kernel_escape_seen()`: whether an Escape has come since it last
/// answered.
pub(super) fn escape_seen(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let statics = context::statics_at(os, regs.r[10])?;
    regs.r[0] = statics.read(os, at::ESCAPE_SEEN)?;
    statics.write(os, at::ESCAPE_SEEN, 0)
}

/// `_kernel_fpavailable()`: whether `_kernel_init` found a floating-point
/// unit.
pub(super) fn fpavailable(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let statics = context::statics_at(os, regs.r[10])?;
    regs.r[0] = statics.read(os, at::FP_AVAILABLE)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{getenv, setenv};
    use crate::c::testing::{Client, FREE};

    #[test]
    fn setenv_sets_the_string_to_its_terminator_and_deletes_for_nought() {
        let (name, value, buffer) = (FREE, FREE + 0x40, FREE + 0x100);
        let mut client = Client::new();
        client.put(name, b"Tairix$Probe\0");
        client.put(value, b"one two\0trailing");
        assert_eq!(client.answer(setenv, &[name, value]), 0);
        assert_eq!(client.answer(getenv, &[name, buffer, 64]), 0);
        assert_eq!(client.string(buffer), b"one two");
        assert_eq!(client.answer(setenv, &[name, 0]), 0, "deleted");
        assert_ne!(client.answer(getenv, &[name, buffer, 64]), 0, "gone");
    }
}
