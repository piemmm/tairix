//! The listing `OPT` 1 prints: each statement's address, the bytes it laid
//! down, and its text, laid out as BASIC lays it out.
//!
//! The address and the bytes take fixed fields; a statement that began just
//! after a colon or `[` with a label shows the label's word in a field of ten
//! with at least a space after it, and the rest after it. Otherwise the text,
//! its leading spaces dropped, follows ten spaces, but for such a statement
//! that is empty, which shows nothing after the bytes. Only the label and the
//! text are characters `COUNT` counts and `WIDTH` wraps.

use super::Statement;
use crate::machine::program::spelt;
use crate::machine::{Machine, Step};

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
        let length = end.saturating_sub(statement.start);
        let text = self.copied(statement.start, length as usize)?;
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
    tairix_util::hex::fill_upper(&mut head[..8], u64::from(address));
    if (1..=4).contains(&bytes.len()) {
        let value = bytes.iter().rev().fold(0u64, |v, &b| v << 8 | u64::from(b));
        tairix_util::hex::fill_upper(&mut head[9..9 + 2 * bytes.len()], value);
    }
    head
}
