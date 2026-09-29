//! The hex view: sixteen bytes a row, each as two hex digits and as its
//! ASCII, with the caret on a nibble of the hex pane or a byte of the ASCII
//! one.
//!
//! A row is `offset  hh hh hh hh hh hh hh hh  hh hh hh hh hh hh hh hh  |ascii|`.

pub use tairix_util::hexdump::BYTES_PER_ROW;

/// Which pane the caret is in.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub enum Pane {
    /// The hex digits.
    #[default]
    Hex,
    /// The ASCII column.
    Ascii,
}

/// Which half of a byte the caret is on in the hex pane.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub enum Nibble {
    /// The first digit.
    #[default]
    High,
    /// The second digit.
    Low,
}

/// Where the caret is in the hex view.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct HexCaret {
    /// The byte it is on; the document's length when past the last byte.
    pub offset: usize,
    /// The pane it is in.
    pub pane: Pane,
    /// The nibble it is on, in the hex pane.
    pub nibble: Nibble,
}

/// How many rows a document of `len` bytes takes: one more than it fills
/// when its length is a whole number of rows, so the caret past the last
/// byte has a row to stand on.
#[must_use]
pub const fn rows(len: usize) -> usize {
    len / BYTES_PER_ROW + 1
}

/// The columns of a document's hex view.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct HexLayout {
    digits: usize,
}

impl HexLayout {
    /// The layout of a document of `len` bytes: its last row, the one the
    /// caret past the last byte stands on, starts at an offset up to `len`.
    #[must_use]
    pub const fn for_len(len: usize) -> Self {
        Self {
            digits: tairix_util::hexdump::offset_digits(len as u64),
        }
    }

    /// How many digits the offset column shows.
    #[cfg(test)]
    pub(crate) const fn offset_digits(self) -> usize {
        self.digits
    }

    /// The column byte `index` of a row starts at in the hex pane.
    #[must_use]
    pub const fn hex_column(self, index: usize) -> usize {
        let half = if index >= BYTES_PER_ROW / 2 { 1 } else { 0 };
        self.digits + 2 + index * 3 + half
    }

    /// The column byte `index` of a row is at in the ASCII pane.
    #[must_use]
    pub const fn ascii_column(self, index: usize) -> usize {
        self.hex_column(BYTES_PER_ROW) + 2 + index
    }

    /// Columns a whole row takes, the closing bar included.
    #[must_use]
    pub const fn width(self) -> usize {
        self.ascii_column(BYTES_PER_ROW) + 1
    }

    /// The caret stop a click at grid column `column` of row `row` lands on,
    /// clamped to a document of `len` bytes; `None` over the offset column.
    #[must_use]
    pub fn hit(self, row: usize, column: usize, len: usize) -> Option<HexCaret> {
        let base = row.saturating_mul(BYTES_PER_ROW);
        let at = |index: usize| base.saturating_add(index).min(len);
        let ascii = self.ascii_column(0);
        if column >= ascii - 1 {
            let index = column.saturating_sub(ascii).min(BYTES_PER_ROW - 1);
            return Some(HexCaret {
                offset: at(index),
                pane: Pane::Ascii,
                nibble: Nibble::High,
            });
        }
        if column < self.hex_column(0) {
            return None;
        }
        let index = (0..BYTES_PER_ROW)
            .rev()
            .find(|&index| self.hex_column(index) <= column)
            .unwrap_or(0);
        let nibble = if column > self.hex_column(index) {
            Nibble::Low
        } else {
            Nibble::High
        };
        let offset = at(index);
        Some(HexCaret {
            offset,
            pane: Pane::Hex,
            nibble: if offset == len { Nibble::High } else { nibble },
        })
    }

    /// The grid column the caret stands in.
    #[must_use]
    pub const fn caret_column(self, caret: HexCaret) -> usize {
        let index = caret.offset % BYTES_PER_ROW;
        match (caret.pane, caret.nibble) {
            (Pane::Hex, Nibble::High) => self.hex_column(index),
            (Pane::Hex, Nibble::Low) => self.hex_column(index) + 1,
            (Pane::Ascii, _) => self.ascii_column(index),
        }
    }

    /// `offset` as this layout's offset column shows it, written into `buf`.
    #[must_use]
    pub fn offset_text(self, offset: usize, buf: &mut [u8; 16]) -> &str {
        let digits = self.digits.min(buf.len());
        for (at, slot) in buf[..digits].iter_mut().enumerate() {
            let shift = (digits - 1 - at) * 4;
            let digit = u32::try_from(shift)
                .ok()
                .and_then(|shift| offset.checked_shr(shift))
                .unwrap_or(0)
                & 0xf;
            *slot = HEX_DIGITS[digit];
        }
        core::str::from_utf8(&buf[..digits]).unwrap_or("")
    }
}

/// The upper-case hex digits, by value.
pub const HEX_DIGITS: &[u8; 16] = b"0123456789ABCDEF";

/// A byte's two hex digits.
#[must_use]
pub fn hex_pair(byte: u8) -> [u8; 2] {
    [
        HEX_DIGITS[usize::from(byte >> 4)],
        HEX_DIGITS[usize::from(byte & 0xf)],
    ]
}

/// The value of a typed hex digit, in the width a nibble is kept in.
#[must_use]
pub fn digit_value(ch: char) -> Option<u8> {
    ch.to_digit(16).and_then(|digit| u8::try_from(digit).ok())
}

/// One edit typing makes in the hex view: replace `range` with `byte`, and
/// put the caret at `caret`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HexEdit {
    /// What the edit replaces: one byte when overwriting, empty when
    /// inserting.
    pub range: core::ops::Range<usize>,
    /// The byte put in its place.
    pub byte: u8,
    /// Where the caret goes.
    pub caret: HexCaret,
}

/// The edit typing hex digit `digit` makes with the caret at `caret`, where
/// `current` is the byte under the caret (`None` past the last byte).
///
/// Overwriting sets the caret's nibble of the byte under it; inserting puts
/// a new byte in on the high nibble and fills it in on the low one. Past the
/// last byte both append.
#[must_use]
pub fn type_digit(caret: HexCaret, current: Option<u8>, digit: u8, insert: bool) -> HexEdit {
    let at = caret.offset;
    let digit = digit & 0xf;
    let low = HexCaret {
        nibble: Nibble::Low,
        ..caret
    };
    let next = HexCaret {
        offset: at + 1,
        pane: Pane::Hex,
        nibble: Nibble::High,
    };
    match (caret.nibble, current) {
        (Nibble::High, Some(byte)) if !insert => HexEdit {
            range: at..at + 1,
            byte: (digit << 4) | (byte & 0xf),
            caret: low,
        },
        (Nibble::High, _) => HexEdit {
            range: at..at,
            byte: digit << 4,
            caret: low,
        },
        (Nibble::Low, Some(byte)) => HexEdit {
            range: at..at + 1,
            byte: (byte & 0xf0) | digit,
            caret: next,
        },
        (Nibble::Low, None) => HexEdit {
            range: at..at,
            byte: digit,
            caret: next,
        },
    }
}

/// The edit typing `byte` in the ASCII pane makes with the caret at
/// `caret`: overwrite the byte under it, or insert before it, and step on.
#[must_use]
pub fn type_byte(caret: HexCaret, current: Option<u8>, byte: u8, insert: bool) -> HexEdit {
    let at = caret.offset;
    let range = if insert || current.is_none() {
        at..at
    } else {
        at..at + 1
    };
    HexEdit {
        range,
        byte,
        caret: HexCaret {
            offset: at + 1,
            pane: Pane::Ascii,
            nibble: Nibble::High,
        },
    }
}

/// The caret one stop right: the low nibble after the high one in the hex
/// pane, the next byte otherwise; never past the end of a `len`-byte
/// document.
#[must_use]
pub fn step_right(caret: HexCaret, len: usize) -> HexCaret {
    match (caret.pane, caret.nibble) {
        (Pane::Hex, Nibble::High) if caret.offset < len => HexCaret {
            nibble: Nibble::Low,
            ..caret
        },
        _ => HexCaret {
            offset: (caret.offset + 1).min(len),
            nibble: Nibble::High,
            ..caret
        },
    }
}

/// The caret one stop left: the high nibble before the low one in the hex
/// pane, the previous byte's last stop otherwise.
#[must_use]
pub fn step_left(caret: HexCaret) -> HexCaret {
    match (caret.pane, caret.nibble) {
        (Pane::Hex, Nibble::Low) => HexCaret {
            nibble: Nibble::High,
            ..caret
        },
        (pane, _) if caret.offset > 0 => HexCaret {
            offset: caret.offset - 1,
            pane,
            nibble: if pane == Pane::Hex {
                Nibble::Low
            } else {
                Nibble::High
            },
        },
        _ => caret,
    }
}

#[cfg(test)]
#[path = "hex_tests.rs"]
mod tests;
