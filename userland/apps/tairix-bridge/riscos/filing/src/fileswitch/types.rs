//! File type names (`plans/RISCOS-EMULATOR.md` §6): `OS_FSControl` 18, 31
//! and 38, a type's name from its `File$Type_XXX` variable, as RISC OS 5
//! names one.

use alloc::vec::Vec;

use tairix_riscos_kernel::error::Error;
use tairix_riscos_kernel::os::{len32, Fail, Os, Regs};

use super::error;

/// "Unknown file type".
const UNKNOWN_TYPE: Error = Error {
    number: 0x41D,
    token: "UnknownFileType",
};

/// A type name's room: eight characters, padded with spaces.
pub(crate) const NAME: usize = 8;

/// The most variables a name is looked for among: a containment bound on a
/// variable store the guest fills.
const VARIABLES_MOST: u32 = 4096;

/// The room a variable's value is read into as `name` is looked up.
const NAME_ROOM: u32 = 256;

/// The variable naming type `kind`: `File$Type_` and three upper-case hex
/// digits.
fn variable_of(kind: u32) -> [u8; 13] {
    let mut name = *b"File$Type_000";
    for (i, slot) in name[10..].iter_mut().enumerate() {
        let digit = (kind >> (8 - 4 * i)) & 0xF;
        *slot = b"0123456789ABCDEF"[digit as usize];
    }
    name
}

/// `OS_FSControl` 18: R2's type, its low twelve bits, named in R2 and R3 as
/// [`name_of`] names it.
pub(crate) fn type_name(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let name = name_of(os, regs.r[2] & 0xFFF)?;
    regs.r[2] = u32::from_le_bytes([name[0], name[1], name[2], name[3]]);
    regs.r[3] = u32::from_le_bytes([name[4], name[5], name[6], name[7]]);
    Ok(())
}

/// Type `kind`'s name: its variable's value, else `Service_LookupFileType`'s
/// answer, else `&` and its three hex digits, padded with spaces to eight.
pub(crate) fn name_of(os: &mut Os<'_>, kind: u32) -> Result<[u8; NAME], Fail> {
    let mut name = [b' '; NAME];
    if let Some(value) = os.variable_text(&variable_of(kind))? {
        for (slot, &byte) in name.iter_mut().zip(&value) {
            *slot = byte;
        }
    } else {
        let mut service = Regs::default();
        service.r[1] = super::service::LOOKUP_FILE_TYPE;
        service.r[2] = kind;
        os.service(&mut service)?;
        if service.r[1] == 0 {
            name[..4].copy_from_slice(&service.r[2].to_le_bytes());
            name[4..].copy_from_slice(&service.r[3].to_le_bytes());
        } else {
            name[0] = b'&';
            name[1..4].copy_from_slice(&variable_of(kind)[10..]);
        }
    }
    Ok(name)
}

/// The type a `File$Type_` variable whose value is `name`, ignoring case,
/// gives, if one does.
fn named_type(os: &mut Os<'_>, name: &[u8]) -> Result<Option<u32>, Fail> {
    const PATTERN: &[u8] = b"File$Type_*\0";
    let room = (len32(PATTERN.len())).next_multiple_of(4);
    os.below_supervisor_stack(room + NAME_ROOM, |os, frame| {
        os.write_bytes(frame, PATTERN)?;
        let mut context = 0;
        for _ in 0..VARIABLES_MOST {
            let mut regs = Regs::default();
            regs.r[..5].copy_from_slice(&[frame, frame + room, NAME_ROOM, context, 3]);
            match os.perform(
                tairix_riscos_kernel::swi::X | tairix_riscos_kernel::swi::os::READ_VAR_VAL,
                &mut regs,
            ) {
                Ok(()) => {}
                Err(Fail::Error(_)) => return Ok(None),
                Err(unwind) => return Err(unwind),
            }
            context = regs.r[3];
            let mut value = Vec::new();
            if value.try_reserve_exact(regs.r[2] as usize).is_err() {
                return Err(os.no_memory());
            }
            value.resize(regs.r[2] as usize, 0);
            os.read_bytes(frame + room, &mut value)?;
            if !value.eq_ignore_ascii_case(name) {
                continue;
            }
            let (variable, _) = os.read_until(context, NAME_ROOM as usize, |b| b <= b' ')?;
            if let Some(kind) = variable.strip_prefix(b"File$Type_").and_then(hex) {
                return Ok(Some(kind));
            }
        }
        Ok(None)
    })
}

/// Up to three hex digits, with or without `&` before them, as a type.
fn hex(text: &[u8]) -> Option<u32> {
    let digits = text.strip_prefix(b"&").unwrap_or(text);
    if digits.is_empty() || digits.len() > 3 {
        return None;
    }
    digits.iter().try_fold(0, |kind, &b| {
        let digit = char::from(b).to_digit(16)?;
        Some(kind << 4 | digit)
    })
}

/// `OS_FSControl` 31: the type R1 names in R2: a type name, ignoring case,
/// else up to three hex digits; "Unknown file type", R2 zero, for neither.
pub(crate) fn type_number(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let name = super::name::read(os, regs.r[1])?;
    let Some(kind) = named_type(os, &name)?.or_else(|| hex(&name)) else {
        regs.r[2] = 0;
        return Err(error::fail(os, UNKNOWN_TYPE, &[&name]));
    };
    regs.r[2] = kind;
    Ok(())
}

/// `OS_FSControl` 38: the type of the object R2 to R6 describe, as `OS_File`
/// 20 gives it, R1 its leaf name where an application would show, or zero.
pub(crate) fn type_of_info(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let leaf = if regs.r[1] == 0 {
        Vec::new()
    } else {
        super::name::read(os, regs.r[1])?
    };
    regs.r[2] = super::file::object_type(regs.r[6], regs.r[2], &leaf);
    Ok(())
}

#[cfg(test)]
mod tests;
