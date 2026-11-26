//! How the configured `DumpFormat` shows text: a file `*Type` and `*List`
//! show, and what SystemDevices' `vdu:` is given to write. A pure machine,
//! fed a byte at a time, so what it shows is tested apart from what reads or
//! writes the bytes.

use alloc::vec::Vec;

use crate::cmos;
use crate::convert::Decimal;
use crate::os::{Fail, Os};

/// `DumpFormat`'s bits, the low five of its CMOS byte: bit 7 taken off a
/// byte first, top-bit bytes shown as they are, an unshowable byte as `<n>`,
/// and that number in hexadecimal — or without `<n>`, as a `.`.
pub(crate) const SEVEN_BIT: u8 = 1 << 3;
pub(crate) const RAW_TOP: u8 = 1 << 2;
pub(crate) const ANGLED: u8 = 1 << 1;
pub(crate) const HEXADECIMAL: u8 = 1 << 0;
/// The bit that expands tabs as `-TabExpand` does.
pub(crate) const TAB_SPACES: u8 = 1 << 4;

/// How bytes are shown.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Listing {
    /// `DumpFormat`'s bits for a filtered listing; none for `*Print`, which
    /// writes each byte as it is.
    pub(crate) filter: Option<u8>,
    pub(crate) numbered: bool,
    /// A tab is eight spaces.
    pub(crate) tabs: bool,
}

/// How far a listing has got.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Progress {
    lines: u32,
    /// The character last shown, a line's end included, or zero after the
    /// second half of a CR LF or LF CR pair.
    last: u8,
    within_line: bool,
}

impl Listing {
    /// Each byte as it is, as `*Print` writes it.
    pub(crate) const RAW: Self = Self {
        filter: None,
        numbered: false,
        tabs: false,
    };

    /// Filtered by the `DumpFormat` bits of `format`, unnumbered and a tab
    /// shown as any control character is: as `vdu:` shows what it is given.
    #[must_use]
    pub const fn filtered(format: u8) -> Self {
        Self {
            filter: Some(format),
            numbered: false,
            tabs: false,
        }
    }

    /// What `byte` adds to the listing.
    pub fn feed(self, at: &mut Progress, byte: u8, out: &mut Vec<u8>) {
        if !at.within_line {
            at.within_line = true;
            if self.numbered {
                at.lines += 1;
                line_number(at.lines, out);
            }
        }
        let Some(format) = self.filter else {
            out.push(byte);
            return;
        };
        if byte == b'\r' || byte == b'\n' {
            if byte == at.last {
                newline(out);
                at.within_line = false;
            } else if at.last == b'\r' || at.last == b'\n' {
                // A pair's second half: what follows stays on the line that
                // its first half began, numbered already.
                at.last = 0;
            } else {
                at.last = byte;
                newline(out);
                at.within_line = false;
            }
            return;
        }
        if byte == b'\t' && self.tabs {
            out.extend_from_slice(b"        ");
            return;
        }
        at.last = byte;
        if byte == b'"' || byte == b'<' {
            out.push(byte);
        } else {
            shown(format, byte, out);
        }
    }

    /// What the end of the file adds: a filtered listing ends a line it
    /// is part way through.
    pub(crate) fn end(self, at: Progress, out: &mut Vec<u8>) {
        if self.filter.is_some() && at.within_line {
            newline(out);
        }
    }
}

/// `byte` as RISC OS's GSTrans would read it back, in `format`.
fn shown(format: u8, byte: u8, out: &mut Vec<u8>) {
    let byte = if format & SEVEN_BIT != 0 {
        byte & 0x7F
    } else {
        byte
    };
    let printable = (0x20..=0x7E).contains(&byte);
    let raw_top = format & RAW_TOP != 0 && byte >= 0x80;
    if (printable && !matches!(byte, b'|' | b'"' | b'<')) || raw_top {
        out.push(byte);
    } else if format & ANGLED != 0 {
        out.push(b'<');
        if format & HEXADECIMAL != 0 {
            out.push(b'&');
            out.extend_from_slice(&tairix_util::hex::upper_digits(byte));
        } else {
            out.extend_from_slice(Decimal::of(i32::from(byte)).as_bytes());
        }
        out.push(b'>');
    } else if format & HEXADECIMAL != 0 {
        out.push(if printable { byte } else { b'.' });
    } else {
        if byte >= 0x80 {
            out.extend_from_slice(b"|!");
        }
        match byte & 0x7F {
            0x7F => out.extend_from_slice(b"|?"),
            c @ (b'"' | b'|') => out.extend_from_slice(&[b'|', c]),
            c @ 0..=0x1F => out.extend_from_slice(&[b'|', c + b'@']),
            c => out.push(c),
        }
    }
}

pub(crate) fn newline(out: &mut Vec<u8>) {
    out.extend_from_slice(b"\n\r");
}

/// A line number as `*List` and `*Build` write it: right-aligned in four
/// columns, then a space.
pub(crate) fn line_number(line: u32, out: &mut Vec<u8>) {
    let digits = Decimal::of(line.cast_signed());
    let digits = digits.as_bytes();
    out.resize(out.len() + 4usize.saturating_sub(digits.len()), b' ');
    out.extend_from_slice(digits);
    out.push(b' ');
}

impl Os<'_> {
    /// The `DumpFormat` CMOS configures.
    ///
    /// # Errors
    ///
    /// What reading the CMOS gives.
    pub fn dump_format(&mut self) -> Result<u8, Fail> {
        self.read_cmos(cmos::PRINT_SOUND)
    }
}

#[cfg(test)]
mod tests;
