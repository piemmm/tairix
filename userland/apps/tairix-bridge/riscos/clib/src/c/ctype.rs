//! `ctype.h`: each class test the `ctype` table's bits for its character,
//! and the case conversions, as RISC OS 5.30 answers them — the table read
//! at the character's offset unchecked, as the headers' macros read it.

use tairix_riscos_kernel::os::{Fail, Os, Regs};

use super::statics::{at, CTYPE_BYTES};
use crate::kernel::context;
use crate::kernel::statics::Statics;

/// The `ctype` table's bits.
pub(crate) mod bit {
    pub(crate) const SPACE: u8 = 1 << 0;
    pub(crate) const PUNCT: u8 = 1 << 1;
    /// A printing space.
    pub(crate) const BLANK: u8 = 1 << 2;
    pub(crate) const LOWER: u8 = 1 << 3;
    pub(crate) const UPPER: u8 = 1 << 4;
    pub(crate) const DIGIT: u8 = 1 << 5;
    pub(crate) const CONTROL: u8 = 1 << 6;
    /// `A` to `F` and `a` to `f`.
    pub(crate) const HEX: u8 = 1 << 7;
}

use bit::{BLANK, CONTROL, DIGIT, HEX, LOWER, PUNCT, SPACE, UPPER};

/// The C locale's table.
pub(crate) const C_LOCALE: [u8; 256] = {
    let mut table = [0; 256];
    let mut c = 0;
    while c < 128 {
        #[expect(clippy::cast_possible_truncation, reason = "below 128")]
        let byte = c as u8;
        table[c] = match byte {
            b'\t'..=b'\r' => CONTROL | SPACE,
            0..=0x1F | 0x7F => CONTROL,
            b' ' => SPACE | BLANK,
            b'0'..=b'9' => DIGIT,
            b'A'..=b'F' => UPPER | HEX,
            b'G'..=b'Z' => UPPER,
            b'a'..=b'f' => LOWER | HEX,
            b'g'..=b'z' => LOWER,
            _ => PUNCT,
        };
        c += 1;
    }
    table
};

/// The distance between a letter's cases in the C locale.
const CASE_DISTANCE: u32 = 32;

/// `c`'s bits in the client's table, read where the macros read them.
pub(crate) fn class_of(os: &mut Os<'_>, statics: Statics, c: u32) -> Result<u8, Fail> {
    os.as_caller()
        .read_byte(statics.field(at::CTYPE).wrapping_add(c))
}

/// The client's whole table.
pub(crate) fn table(os: &mut Os<'_>, statics: Statics) -> Result<[u8; CTYPE_BYTES as usize], Fail> {
    let mut table = [0; CTYPE_BYTES as usize];
    os.as_caller()
        .read_bytes(statics.field(at::CTYPE), &mut table)?;
    Ok(table)
}

fn class(os: &mut Os<'_>, regs: &mut Regs, mask: u8) -> Result<(), Fail> {
    let statics = context::statics_at(os, regs.r[10])?;
    regs.r[0] = u32::from(class_of(os, statics, regs.r[0])? & mask);
    Ok(())
}

/// `isalnum(c)`.
pub(super) fn isalnum(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    class(os, regs, UPPER | LOWER | DIGIT)
}

/// `isalpha(c)`.
pub(super) fn isalpha(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    class(os, regs, UPPER | LOWER)
}

/// `iscntrl(c)`.
pub(super) fn iscntrl(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    class(os, regs, CONTROL)
}

/// `isdigit(c)`.
pub(super) fn isdigit(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    class(os, regs, DIGIT)
}

/// `isgraph(c)`.
pub(super) fn isgraph(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    class(os, regs, LOWER | UPPER | DIGIT | PUNCT)
}

/// `islower(c)`.
pub(super) fn islower(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    class(os, regs, LOWER)
}

/// `isprint(c)`.
pub(super) fn isprint(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    class(os, regs, LOWER | UPPER | DIGIT | PUNCT | BLANK)
}

/// `ispunct(c)`.
pub(super) fn ispunct(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    class(os, regs, PUNCT)
}

/// `isspace(c)`.
pub(super) fn isspace(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    class(os, regs, SPACE)
}

/// `isupper(c)`.
pub(super) fn isupper(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    class(os, regs, UPPER)
}

/// `isxdigit(c)`.
pub(super) fn isxdigit(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    class(os, regs, DIGIT | HEX)
}

/// `isblank(c)`: one for a space or a tab, as RISC OS 5.30 answers, not a
/// test of the table.
#[expect(clippy::unnecessary_wraps, reason = "an entry's signature")]
pub(crate) fn isblank(_os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    regs.r[0] = u32::from(regs.r[0] == u32::from(b' ') || regs.r[0] == u32::from(b'\t'));
    Ok(())
}

/// `c` in the other case: the territory's table at `c` where the locale's
/// `LC_CTYPE` has one, else `c` moved by the C locale's distance where its
/// class is `from`'s.
fn convert(
    os: &mut Os<'_>,
    regs: &mut Regs,
    table_at: u32,
    from: u8,
    up: bool,
) -> Result<(), Fail> {
    let statics = context::statics_at(os, regs.r[10])?;
    let c = regs.r[0];
    let table = statics.read(os, table_at)?;
    regs.r[0] = if table != 0 {
        u32::from(os.as_caller().read_byte(table.wrapping_add(c))?)
    } else if class_of(os, statics, c)? & from == 0 {
        c
    } else if up {
        c.wrapping_sub(CASE_DISTANCE)
    } else {
        c.wrapping_add(CASE_DISTANCE)
    };
    Ok(())
}

/// `tolower(c)`.
pub(super) fn tolower(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    convert(os, regs, at::LOWER_TABLE, UPPER, false)
}

/// `toupper(c)`.
pub(super) fn toupper(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    convert(os, regs, at::UPPER_TABLE, LOWER, true)
}

#[cfg(test)]
mod tests;
