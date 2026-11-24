//! A text area: its columns, its colours and its text, painted through the
//! Font Manager as RISC OS 5.30's DrawFile paints one, or read through for
//! the fonts it defines.

mod paint;
mod parse;

use tairix_drawing::riscos::format::{
    Object, BARE_HEADER, OBJECT_BOUNDS, OBJECT_HEADER, OBJECT_SIZE, TEXT_COLUMN,
};
use tairix_util::endian::le_u32;

use super::render::{box_at, terminated};

pub(super) use paint::{paint, Places, BLOCK_BYTES, BUFFER_BYTES};

/// The words between a text area's columns and its colours: the columns'
/// end and two reserved.
const AFTER_COLUMNS: usize = 12;

/// Why a text area is refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Refusal {
    NoVersion,
    Version(u32),
    Command(u8),
    UndefinedFont(u32),
    /// The host refused memory.
    NoRoom,
}

/// A text area's parts.
#[derive(Clone, Copy, Debug)]
pub(super) struct Area<'a> {
    pub(super) foreground: u32,
    pub(super) background: u32,
    /// The object's own box, in draw units.
    bounds: [i32; 4],
    /// Its column objects.
    columns: &'a [u8],
    text: &'a [u8],
}

impl<'a> Area<'a> {
    /// The text area `object`.
    pub(super) fn of(object: Object<'a>) -> Self {
        let bytes = object.bytes;
        let mut at = OBJECT_HEADER;
        while le_u32(bytes, at) == Some(TEXT_COLUMN) {
            at = at.saturating_add(object_size(bytes, at));
        }
        let columns = bytes.get(OBJECT_HEADER..at.min(bytes.len())).unwrap_or(&[]);
        at = at.saturating_add(AFTER_COLUMNS);
        Self {
            foreground: le_u32(bytes, at).unwrap_or(0),
            background: le_u32(bytes, at.saturating_add(4)).unwrap_or(0),
            bounds: box_at(bytes, OBJECT_BOUNDS),
            columns,
            text: terminated(bytes, at.saturating_add(8)),
        }
    }

    /// Its columns' boxes in order, in draw units. One with none is painted
    /// in a column of no width at its own box's left, as RISC OS's is.
    fn columns(&self) -> Columns<'a> {
        let [x0, y0, _, y1] = self.bounds;
        Columns {
            bytes: self.columns,
            at: 0,
            lone: self.columns.is_empty().then_some([x0, y0, x0, y1]),
        }
    }

    /// The fonts its text defines, as names, read through to declare them:
    /// a version given must be 1, and no command may be foreign or
    /// malformed.
    pub(super) fn fonts(&self) -> Result<alloc::vec::Vec<&'a [u8]>, Refusal> {
        let mut names = alloc::vec::Vec::new();
        let mut reader = parse::Reader::new(self.text);
        while let Some(piece) = reader.next()? {
            if let parse::Piece::Command(parse::Command::Define { name, .. }) = piece {
                tairix_util::fallible::try_push(&mut names, name).map_err(|_| Refusal::NoRoom)?;
            }
        }
        Ok(names)
    }
}

/// A text area's columns' boxes.
struct Columns<'a> {
    bytes: &'a [u8],
    at: usize,
    /// The one column of an area with none.
    lone: Option<[i32; 4]>,
}

impl Iterator for Columns<'_> {
    type Item = [i32; 4];

    fn next(&mut self) -> Option<[i32; 4]> {
        if let Some(lone) = self.lone.take() {
            return Some(lone);
        }
        le_u32(self.bytes, self.at)?;
        let column = box_at(self.bytes, self.at.saturating_add(OBJECT_BOUNDS));
        self.at = self.at.saturating_add(object_size(self.bytes, self.at));
        Some(column)
    }
}

/// The size of the object at `at`, never less than its two header words.
fn object_size(bytes: &[u8], at: usize) -> usize {
    let size = le_u32(bytes, at.saturating_add(OBJECT_SIZE)).unwrap_or(0);
    usize::try_from(size).unwrap_or(usize::MAX).max(BARE_HEADER)
}

#[cfg(test)]
mod tests;
