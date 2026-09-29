//! What a line of text is in the grid: its bytes decoded into the units a
//! caret steps over, and where each unit's cells fall.
//!
//! A unit is a character, a tab, or a token standing for something a grid
//! must not draw as itself: a control byte `[x03]`, a byte that is not UTF-8
//! `[xC3]`, or a character that draws nothing on its own `[U+202E]`. Hiding
//! the last would let a document show one program and hold another.

use core::ops::ControlFlow;

use tairix_vt::char_width;

use crate::document::{rows_of, Document, LineBounds, MAX_ROW_BYTES};

/// What one caret stop of a line is.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Glyph {
    /// A character drawn as itself.
    Char(char),
    /// A horizontal tab, drawn as the space to the next stop.
    Tab,
    /// A control byte, drawn `[x03]`.
    Control(u8),
    /// A byte that is not UTF-8, drawn `[xC3]`.
    Invalid(u8),
    /// A character that draws nothing on its own — a C1 control, a
    /// bidirectional control, a zero-width or other format character — drawn
    /// `[U+202E]`.
    Hidden(char),
}

impl Glyph {
    /// The token text a glyph drawn as a token shows, written into `buf`;
    /// `None` for a character or a tab, which draw as themselves.
    #[must_use]
    pub fn token(self, buf: &mut [u8; 12]) -> Option<&str> {
        let mut len = 0;
        let mut put = |byte: u8| {
            buf[len] = byte;
            len += 1;
        };
        match self {
            Self::Control(byte) | Self::Invalid(byte) => {
                let [high, low] = crate::hex::hex_pair(byte);
                for b in [b'[', b'x', high, low, b']'] {
                    put(b);
                }
            }
            Self::Hidden(ch) => {
                let code = u32::from(ch);
                put(b'[');
                put(b'U');
                put(b'+');
                let digits = if code > 0xffff {
                    if code > 0xf_ffff {
                        6
                    } else {
                        5
                    }
                } else {
                    4
                };
                for shift in (0..digits).rev() {
                    put(crate::hex::HEX_DIGITS[((code >> (shift * 4)) & 0xf) as usize]);
                }
                put(b']');
            }
            Self::Char(_) | Self::Tab => return None,
        }
        core::str::from_utf8(&buf[..len]).ok()
    }
}

/// Whether `ch` draws nothing on its own, and so is shown as a token.
#[must_use]
pub fn is_hidden(ch: char) -> bool {
    matches!(
        u32::from(ch),
        0x0080..=0x009f
            | 0x00ad
            | 0x061c
            | 0x115f
            | 0x1160
            | 0x180e
            | 0x200b..=0x200f
            | 0x2028..=0x202e
            | 0x2060..=0x2064
            | 0x2066..=0x206f
            | 0x3164
            | 0xfeff
            | 0xffa0
            | 0xfff9..=0xfffb
            | 0x1_d173..=0x1_d17a
            | 0xe_0000..=0xe_007f
    )
}

/// One caret stop of a line.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Unit {
    /// Its first byte, as a document offset.
    pub offset: usize,
    /// How many bytes it spans.
    pub len: usize,
    /// What it is.
    pub glyph: Glyph,
    /// The grid column its first cell is in.
    pub column: usize,
    /// How many cells it takes.
    pub width: usize,
}

/// The glyph of the unit leading `bytes` and how many bytes it spans, or
/// `None` when `bytes` ends inside a multi-byte sequence that more bytes
/// could complete.
pub(crate) fn decode(bytes: &[u8]) -> Option<(Glyph, usize)> {
    let lead = *bytes.first()?;
    let len = match lead {
        0x00..=0x7f => {
            let glyph = match lead {
                b'\t' => Glyph::Tab,
                0x00..=0x1f | 0x7f => Glyph::Control(lead),
                _ => Glyph::Char(char::from(lead)),
            };
            return Some((glyph, 1));
        }
        0xc2..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf4 => 4,
        _ => return Some((Glyph::Invalid(lead), 1)),
    };
    let Some(sequence) = bytes.get(..len) else {
        // Short of a whole sequence: complete if every byte so far is a
        // continuation, invalid at once if one is not.
        return if bytes[1..].iter().all(|&b| b & 0xc0 == 0x80) {
            None
        } else {
            Some((Glyph::Invalid(lead), 1))
        };
    };
    match core::str::from_utf8(sequence)
        .ok()
        .and_then(|text| text.chars().next())
    {
        Some(ch) if is_hidden(ch) => Some((Glyph::Hidden(ch), len)),
        Some(ch) => Some((Glyph::Char(ch), len)),
        None => Some((Glyph::Invalid(lead), 1)),
    }
}

/// How many cells `glyph` takes starting at `column`, with tab stops every
/// `tab` columns.
fn width(glyph: Glyph, column: usize, tab: usize) -> usize {
    match glyph {
        Glyph::Char(ch) => usize::from(char_width(ch)),
        Glyph::Tab => tab - column % tab,
        token => token.token(&mut [0; 12]).map_or(1, str::len),
    }
}

/// Units decoded from bytes arriving a slice at a time: a sequence split
/// between two slices is carried to the next, so a character is one unit
/// wherever it is stored.
pub(crate) struct Decoder {
    carry: [u8; 4],
    carried: usize,
    /// The offset of the first carried byte, or of the next byte when none
    /// is carried.
    offset: usize,
}

impl Decoder {
    /// A decoder whose first byte is at `offset`.
    pub(crate) const fn new(offset: usize) -> Self {
        Self {
            carry: [0; 4],
            carried: 0,
            offset,
        }
    }

    /// Decode `slice`, visiting each whole unit — its offset, length and
    /// glyph — until `visit` breaks.
    pub(crate) fn feed(
        &mut self,
        mut slice: &[u8],
        visit: &mut impl FnMut(usize, usize, Glyph) -> ControlFlow<()>,
    ) -> ControlFlow<()> {
        while self.carried > 0 {
            let Some((&byte, rest)) = slice.split_first() else {
                return ControlFlow::Continue(());
            };
            slice = rest;
            self.carry[self.carried] = byte;
            self.carried += 1;
            while let Some((glyph, len)) = decode(&self.carry[..self.carried]) {
                if visit(self.offset, len, glyph).is_break() {
                    return ControlFlow::Break(());
                }
                self.offset += len;
                self.carry.copy_within(len..self.carried, 0);
                self.carried -= len;
                if self.carried == 0 {
                    break;
                }
            }
        }
        let mut at = 0;
        while at < slice.len() {
            let Some((glyph, len)) = decode(&slice[at..]) else {
                let rest = &slice[at..];
                self.carry[..rest.len()].copy_from_slice(rest);
                self.carried = rest.len();
                return ControlFlow::Continue(());
            };
            if visit(self.offset, len, glyph).is_break() {
                return ControlFlow::Break(());
            }
            self.offset += len;
            at += len;
        }
        ControlFlow::Continue(())
    }

    /// The input has ended: a sequence it ended inside is its invalid bytes.
    pub(crate) fn finish(
        &mut self,
        visit: &mut impl FnMut(usize, usize, Glyph) -> ControlFlow<()>,
    ) {
        for &byte in &self.carry[..self.carried] {
            if visit(self.offset, 1, Glyph::Invalid(byte)).is_break() {
                break;
            }
            self.offset += 1;
        }
        self.carried = 0;
    }
}

/// Visit the undecorated units of `range` — each unit's offset, length and
/// glyph — for as long as `visit` continues; a sequence the range ends
/// inside is its invalid bytes.
pub fn for_each_glyph(
    document: &Document,
    range: core::ops::Range<usize>,
    mut visit: impl FnMut(usize, usize, Glyph) -> ControlFlow<()>,
) {
    let mut decoder = Decoder::new(range.start);
    let mut remaining = range.end.saturating_sub(range.start);
    let mut stopped = false;
    document.walk(range.start, |slice| {
        let slice = &slice[..slice.len().min(remaining)];
        remaining -= slice.len();
        if decoder.feed(slice, &mut visit).is_break() {
            stopped = true;
            return ControlFlow::Break(());
        }
        if remaining == 0 {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    });
    if !stopped {
        decoder.finish(&mut visit);
    }
}

/// Visit the units of the line `bounds` from its start, for as long as
/// `visit` continues, laying them out with tab stops every `tab` columns.
pub fn for_each_unit(
    document: &Document,
    bounds: LineBounds,
    tab: usize,
    mut visit: impl FnMut(&Unit) -> ControlFlow<()>,
) {
    let tab = tab.max(1);
    let mut column = 0usize;
    for_each_glyph(document, bounds.start..bounds.end, |offset, len, glyph| {
        let unit = Unit {
            offset,
            len,
            glyph,
            column,
            width: width(glyph, column, tab),
        };
        column += unit.width;
        visit(&unit)
    });
}

/// The grid column the caret at `offset` stands in, on the line `bounds`.
#[must_use]
pub fn column_of(document: &Document, bounds: LineBounds, offset: usize, tab: usize) -> usize {
    let mut column = 0;
    for_each_unit(document, bounds, tab, |unit| {
        if unit.offset >= offset {
            return ControlFlow::Break(());
        }
        column = unit.column + unit.width;
        ControlFlow::Continue(())
    });
    column
}

/// The caret stop nearest the point `half` half-cells from the start of the
/// row `bounds`: before a unit whose left half holds it, after one whose
/// right half does, and the [`row_end`] past its last unit.
///
/// Half-cells are the pointer's own precision, so a narrow character and a
/// wide one are split at their middles by the one rule.
#[must_use]
pub fn offset_at(document: &Document, bounds: LineBounds, half: usize, tab: usize) -> usize {
    let last = row_end(document, bounds);
    let mut found = last;
    for_each_unit(document, bounds, tab, |unit| {
        let (left, width) = (unit.column * 2, unit.width * 2);
        if half < left + width {
            found = if half < left + width / 2 {
                unit.offset
            } else {
                (unit.offset + unit.len).min(last)
            };
            return ControlFlow::Break(());
        }
        ControlFlow::Continue(())
    });
    found
}

/// The start of the unit the point `half` half-cells into the row `bounds`
/// falls on — the one under it whichever half, or past the row's end its last
/// — which is what a word or line selection is made from, where
/// [`offset_at`] finds the nearest caret stop.
#[must_use]
pub fn unit_under(document: &Document, bounds: LineBounds, half: usize, tab: usize) -> usize {
    let mut under = bounds.start;
    for_each_unit(document, bounds, tab, |unit| {
        under = unit.offset;
        if half < (unit.column + unit.width) * 2 {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    });
    under
}

/// The last caret stop on the row `bounds`: its end, or on a row a long line
/// continues from — whose end is where the next row starts — the stop before
/// its last unit, so the caret stays on the row it was put on.
#[must_use]
pub fn row_end(document: &Document, bounds: LineBounds) -> usize {
    if bounds.next == bounds.end && bounds.end < document.len() {
        prev_stop(document, bounds.end).max(bounds.start)
    } else {
        bounds.end
    }
}

/// Up to `N` bytes from `at`.
fn peek<const N: usize>(document: &Document, at: usize) -> ([u8; N], usize) {
    let mut bytes = [0u8; N];
    let mut len = 0;
    document.walk(at, |slice| {
        let take = slice.len().min(N - len);
        bytes[len..len + take].copy_from_slice(&slice[..take]);
        len += take;
        if len == N {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    });
    (bytes, len)
}

/// The unit at `at`, as a caret steps over it: its glyph and its length. A
/// line terminator — LF, or CRLF as one — is a unit of its own.
#[must_use]
pub fn unit_at(document: &Document, at: usize) -> Option<(Option<Glyph>, usize)> {
    let (bytes, len) = peek::<4>(document, at);
    let bytes = &bytes[..len];
    match bytes {
        [] => None,
        [b'\n', ..] => Some((None, 1)),
        [b'\r', b'\n', ..] => Some((None, 2)),
        _ => Some(
            decode(bytes).map_or((Some(Glyph::Invalid(bytes[0])), 1), |(glyph, len)| {
                (Some(glyph), len)
            }),
        ),
    }
}

/// The caret stop after `at`.
#[must_use]
pub fn next_stop(document: &Document, at: usize) -> usize {
    unit_at(document, at).map_or(at, |(_, len)| at + len)
}

/// The unit `before` ends with — the longest sequence at its end that is
/// one character, else its last byte — and its length.
pub(crate) fn last_unit(before: &[u8]) -> Option<(Glyph, usize)> {
    let n = before.len();
    let last = *before.last()?;
    for back in (2..=n.min(4)).rev() {
        if let Some((glyph @ (Glyph::Char(_) | Glyph::Hidden(_)), len)) =
            decode(&before[n - back..])
        {
            if len == back {
                return Some((glyph, back));
            }
        }
    }
    decode(&[last]).or(Some((Glyph::Invalid(last), 1)))
}

/// The start of the unit holding byte `offset`: `offset` itself unless it
/// falls inside a character, or between the CR and LF of a CRLF; the
/// document's end for an offset past it.
#[must_use]
pub fn unit_start(document: &Document, offset: usize) -> usize {
    let offset = offset.min(document.len());
    // A sequence starting up to three bytes back may end three bytes on.
    let from = offset.saturating_sub(3);
    let (bytes, len) = peek::<7>(document, from);
    let within = offset - from;
    if within > 0 && bytes[within - 1] == b'\r' && within < len && bytes[within] == b'\n' {
        return offset - 1;
    }
    for back in 1..=within {
        if let Some((Glyph::Char(_) | Glyph::Hidden(_), n)) = decode(&bytes[within - back..len]) {
            if n > back {
                return offset - back;
            }
        }
    }
    offset
}

/// The caret stop before `at`.
#[must_use]
pub fn prev_stop(document: &Document, at: usize) -> usize {
    if at == 0 {
        return 0;
    }
    let from = at.saturating_sub(4);
    let (bytes, len) = peek::<4>(document, from);
    let before = &bytes[..len.min(at - from)];
    let n = before.len();
    if before[n - 1] == b'\n' {
        return if n >= 2 && before[n - 2] == b'\r' {
            at - 2
        } else {
            at - 1
        };
    }
    at - last_unit(before).map_or(1, |(_, len)| len)
}

/// One row of the grid: a line, or one part of a line longer than
/// [`MAX_ROW_BYTES`].
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Ord, PartialOrd)]
pub struct Row {
    /// The line.
    pub line: usize,
    /// Which part of the line, from 0.
    pub part: usize,
}

/// Where part `part` of `line` starts: [`MAX_ROW_BYTES`] in from the part
/// before, drawn back to the start of a character straddling that point, so
/// no character is split between rows and a line's row count follows from
/// its length alone.
fn part_start(document: &Document, line: LineBounds, part: usize) -> usize {
    let nominal = line
        .start
        .saturating_add(part.saturating_mul(MAX_ROW_BYTES));
    if part == 0 || nominal >= line.end {
        return nominal.min(line.end);
    }
    unit_start(document, nominal)
}

/// How many rows line `line` takes.
fn line_rows(document: &Document, line: usize) -> usize {
    let bounds = document.line_bounds(line);
    rows_of(bounds.end - bounds.start)
}

/// The bounds of `row`, shaped as a line's: `next` is where the row after it
/// starts, past the terminator on a line's last part.
#[must_use]
pub fn row_bounds(document: &Document, row: Row) -> LineBounds {
    let line = document.line_bounds(row.line);
    let start = part_start(document, line, row.part);
    let end = part_start(document, line, row.part.saturating_add(1));
    let next = if end < line.end { end } else { line.next };
    LineBounds { start, end, next }
}

/// The row the caret at `offset` stands in: on the boundary between two
/// parts of a line, the later one.
#[must_use]
pub fn row_of(document: &Document, offset: usize) -> Row {
    let line = document.line_of(offset);
    let bounds = document.line_bounds(line);
    let last = rows_of(bounds.end - bounds.start) - 1;
    let mut part = (offset.saturating_sub(bounds.start) / MAX_ROW_BYTES).min(last);
    if part < last && offset >= part_start(document, bounds, part + 1) {
        part += 1;
    }
    Row { line, part }
}

/// How many rows of the grid lie above `row`.
#[must_use]
pub fn row_index(document: &Document, row: Row) -> usize {
    document.first_row(row.line) + row.part
}

/// The row `index` rows down the grid; the last row for an index past it.
#[must_use]
pub fn row_at(document: &Document, index: usize) -> Row {
    let (line, first) = document.line_of_row(index);
    Row {
        line,
        part: index
            .saturating_sub(first)
            .min(line_rows(document, line) - 1),
    }
}

/// The row after `row`, if there is one.
#[must_use]
pub fn next_row(document: &Document, row: Row) -> Option<Row> {
    if row.part + 1 < line_rows(document, row.line) {
        Some(Row {
            part: row.part + 1,
            ..row
        })
    } else if row.line + 1 < document.line_count() {
        Some(Row {
            line: row.line + 1,
            part: 0,
        })
    } else {
        None
    }
}

/// The row before `row`, if there is one.
#[must_use]
pub fn prev_row(document: &Document, row: Row) -> Option<Row> {
    if row.part > 0 {
        return Some(Row {
            part: row.part - 1,
            ..row
        });
    }
    let line = row.line.checked_sub(1)?;
    Some(Row {
        line,
        part: line_rows(document, line) - 1,
    })
}

/// Whether `glyph` is part of a word.
pub(crate) fn is_word(glyph: Glyph) -> bool {
    matches!(glyph, Glyph::Char(ch) if ch.is_alphanumeric() || ch == '_')
}

/// Where a word motion right from `at` lands: past any gap and then the word
/// after it, within the caret's row; from a row's end, the next row's start.
#[must_use]
pub fn word_end(document: &Document, at: usize) -> usize {
    let bounds = row_bounds(document, row_of(document, at));
    if at >= bounds.end {
        return bounds.next.max(at);
    }
    let mut end = bounds.end;
    let mut in_word = false;
    for_each_glyph(document, at..bounds.end, |offset, _, glyph| {
        let word = is_word(glyph);
        if in_word && !word {
            end = offset;
            return ControlFlow::Break(());
        }
        in_word |= word;
        ControlFlow::Continue(())
    });
    end
}

/// Where a word motion left from `at` lands: back past any gap and then the
/// word before it, within the caret's row; from a line's start, the end of
/// the line before.
#[must_use]
pub fn word_start(document: &Document, at: usize) -> usize {
    let mut row = row_of(document, at);
    let mut bounds = row_bounds(document, row);
    if at <= bounds.start {
        if row.part == 0 {
            return prev_stop(document, at);
        }
        row.part -= 1;
        bounds = row_bounds(document, row);
    }
    let mut start = bounds.start;
    let mut run = bounds.start;
    let mut in_word = false;
    for_each_glyph(document, bounds.start..at, |offset, _, glyph| {
        let word = is_word(glyph);
        if word && !in_word {
            run = offset;
        }
        if word {
            start = run;
        }
        in_word = word;
        ControlFlow::Continue(())
    });
    start
}

/// The word under `at` on its row, or the one unit there when that is not
/// part of a word; empty at a row's end.
#[must_use]
pub fn word_around(document: &Document, at: usize) -> (usize, usize) {
    let bounds = row_bounds(document, row_of(document, at));
    let Some((Some(here), len)) = (at < bounds.end).then(|| unit_at(document, at)).flatten() else {
        return (at, at);
    };
    if !is_word(here) {
        return (at, at + len);
    }
    let mut start = bounds.start;
    for_each_glyph(document, bounds.start..at, |offset, len, glyph| {
        if !is_word(glyph) {
            start = offset + len;
        }
        ControlFlow::Continue(())
    });
    let mut end = bounds.end;
    for_each_glyph(document, at..bounds.end, |offset, _, glyph| {
        if is_word(glyph) {
            ControlFlow::Continue(())
        } else {
            end = offset;
            ControlFlow::Break(())
        }
    });
    (start, end)
}

#[cfg(test)]
#[path = "text_tests.rs"]
mod tests;
