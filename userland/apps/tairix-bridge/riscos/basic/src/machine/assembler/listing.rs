//! The listing `OPT` 1 prints: each statement's address, the bytes it laid
//! down, and its text, laid out as BASIC lays it out.
//!
//! The address and the bytes take fixed fields; a statement that began just
//! after a colon or `[` with a label shows the label's word in a field of ten
//! with at least a space after it, and the rest after it. Otherwise the text,
//! its leading spaces dropped, follows ten spaces, but for such a statement
//! that is empty, which shows nothing after the bytes. Only the label and the
//! text are characters `COUNT` counts and `WIDTH` wraps.

use alloc::vec::Vec;

use super::Statement;
use crate::machine::program::spelt;
use crate::machine::{len32, Machine, Step};
use crate::messages::Error;

/// The label field's width.
const LABEL_FIELD: usize = 10;

impl Machine<'_, '_> {
    /// `statement`'s line of the listing: at `address`, `bytes` laid down,
    /// its text to its terminator at `end`.
    pub(super) fn listing_line(
        &mut self,
        statement: Statement,
        address: u32,
        bytes: &[u8],
        end: u32,
    ) -> Step {
        self.write_raw(&head(address, bytes))?;
        let mut text = Vec::new();
        let length = end.saturating_sub(statement.start);
        text.try_reserve_exact(length as usize)
            .map_err(|_| Error::NoRoom)?;
        text.resize(length as usize, 0);
        self.os.read_bytes(statement.start, &mut text)?;
        if statement.field && text.first() == Some(&b'.') {
            let word = text.iter().position(|&b| b == b' ').unwrap_or(text.len());
            let label = spelt(&text[..word])?;
            self.output(&label)?;
            self.output_spaces(LABEL_FIELD.saturating_sub(label.len()).max(1))?;
            let rest = &text[word..];
            let spaces = rest.iter().take_while(|&&b| b == b' ').count();
            self.output(&spelt(&rest[spaces..])?)?;
        } else if !(statement.field && text.is_empty()) {
            self.write_raw(&[b' '; LABEL_FIELD])?;
            let spaces = text.iter().take_while(|&&b| b == b' ').count();
            self.output(&spelt(&text[spaces..])?)?;
        }
        self.write_raw(b"\n\r")
    }
}

/// The listing's fixed head: the address, the bytes as a little-endian
/// number where there are four at most, each field followed by a space.
fn head(address: u32, bytes: &[u8]) -> [u8; 18] {
    let mut head = [b' '; 18];
    hex(&mut head[..8], u64::from(address));
    if (1..=4).contains(&bytes.len()) {
        let value = bytes.iter().rev().fold(0u64, |v, &b| v << 8 | u64::from(b));
        hex(&mut head[9..9 + 2 * bytes.len()], value);
    }
    head
}

/// `value` in upper-case hexadecimal filling `out`.
fn hex(out: &mut [u8], value: u64) {
    let digits = len32(out.len());
    for (i, slot) in out.iter_mut().enumerate() {
        let shift = 4 * (digits - 1 - len32(i));
        let digit = (value >> shift & 0xF) as u8;
        *slot = if digit < 10 {
            b'0' + digit
        } else {
            b'A' + digit - 10
        };
    }
}
