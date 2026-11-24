//! A text area painted as RISC OS 5.30's DrawFile paints one. Each
//! paragraph is built as Font Manager text and control codes in a buffer of
//! 255 bytes, broken into lines at its column's width by `Font_ScanString`,
//! and each line painted by `Font_Paint` at a point in millipoints through
//! the render's matrix, the matrix's own four multipliers turning the text.
//!
//! The paragraph's fonts, colours, underlining and moves travel as codes in
//! its text, so a margin, leading or alignment met after them takes effect
//! where RISC OS 5.30's does: margins from the line's first character,
//! leading from the next line, alignment as the line goes out. Each font a
//! `\F` defines is found there and let go at the end, an earlier font of the
//! same number not, as RISC OS 5.30 leaves it.

use tairix_drawing::riscos::format::FIXED_ONE;
use tairix_drawing::riscos::units::{DRAW_UNITS_PER_POINT, MILLIPOINTS_PER_POINT};
use tairix_geometry::saturate_i32;
use tairix_riscos_kernel::os::{Fail, Os, Regs};
use tairix_riscos_kernel::swi::font::code;
use tairix_riscos_kernel::swi::{colour_trans, font, X};

use super::parse::{Align, Command, Piece, Reader, FONTS};
use super::{Area, Columns, Refusal};
use crate::drawfile::workspace::write_name;
use crate::geometry::Matrix;

/// Room before a line's text for its underline code: the code, a position
/// and a thickness.
const PREFIX: usize = 3;
/// A paragraph's longest text.
const CAPACITY: usize = 255;
/// The buffer in guest memory: the underline code, the paragraph and its
/// terminator.
#[expect(clippy::cast_possible_truncation, reason = "a few hundred bytes")]
pub(in crate::drawfile) const BUFFER_BYTES: u32 = (PREFIX + CAPACITY + 1) as u32;
/// The coordinate block's eight words.
pub(in crate::drawfile) const BLOCK_BYTES: u32 = 32;

/// Draw units, millipoints and points in the sixteenths of a millipoint
/// the layout is reckoned in: a draw unit is 25/16 of a millipoint.
const PER_DRAW_UNIT: i64 =
    MILLIPOINTS_PER_POINT as i64 * PER_MILLIPOINT / DRAW_UNITS_PER_POINT as i64;
const PER_MILLIPOINT: i64 = 16;
/// Millipoints a point.
const MILLIPOINTS: i64 = MILLIPOINTS_PER_POINT as i64;
/// The least margin and leading, a point; the least paragraph leading.
const LEAST_MARGIN: i32 = 1;
const LEAST_PARAGRAPH: i32 = 0;
/// What a text area's settings start as, in points.
const MARGIN: i32 = 1;
const LEADING: i32 = 10;
const PARAGRAPH: i32 = 10;

/// How many bytes the Font Manager's code `byte` takes in a paragraph, or
/// none for text.
fn code_length(byte: u8) -> Option<usize> {
    match byte {
        code::FONT | code::COMMENT => Some(2),
        code::UNDERLINE => Some(PREFIX),
        code::MOVE_UP => Some(4),
        code::RGB => Some(8),
        _ => None,
    }
}

/// `Font_Paint`'s flags DrawFile paints a line with: R5 a coordinate block,
/// R6 a matrix and R7 a length.
const PAINT_FLAGS: u32 = font::flag::BLOCK | font::flag::MATRIX | font::flag::LENGTH;
/// No limit, to measure a line.
const NO_LIMIT: u32 = 0x7FFF_FFFF;

/// Where in guest memory a text area is painted from.
#[derive(Clone, Copy, Debug)]
pub(in crate::drawfile) struct Places {
    pub(in crate::drawfile) buffer: u32,
    pub(in crate::drawfile) block: u32,
    pub(in crate::drawfile) matrix: u32,
    pub(in crate::drawfile) name: u32,
    /// The longest name it holds, its terminator included.
    pub(in crate::drawfile) name_bytes: usize,
}

/// `area` painted through `render`.
///
/// # Errors
///
/// A refused text, a font not found, or the Font Manager's or
/// ColourTrans's error; every font found is let go first.
pub(in crate::drawfile) fn paint(
    os: &mut Os<'_>,
    area: &Area<'_>,
    render: &Matrix,
    places: Places,
) -> Result<(), Fail> {
    let mut colours = Regs::default();
    colours.r[1..4].copy_from_slice(&[
        area.background,
        area.foreground,
        font::MOST_OFFSET.cast_unsigned(),
    ]);
    os.perform(X | colour_trans::SET_FONT_COLOURS, &mut colours)?;
    os.write_words(
        places.matrix,
        &Matrix {
            e: 0,
            f: 0,
            ..*render
        }
        .words(),
    )?;
    let mut painter = Painter::new(area, render, places);
    let laid_out = painter.run(os, area);
    let lost = painter.lose_fonts(os);
    laid_out.and(lost)
}

/// A refusal raised.
fn refused(os: &mut Os<'_>, refusal: Refusal) -> Fail {
    super::super::raise_area(os, refusal)
}

/// How a line's text is set out within it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ending {
    /// Broken short of the paragraph's end: a justified line is justified.
    Broken,
    /// The paragraph's last.
    Last,
    /// All the buffer holds of a line too long for it.
    Piece,
}

/// Where a line breaks: its end, where the next starts, and the soft
/// hyphen it ends at, which shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Break {
    end: usize,
    next: usize,
    hyphen: Option<usize>,
}

/// A newline run waiting on what follows it.
#[derive(Clone, Copy, Debug, Default)]
struct Run {
    newlines: u32,
    /// Ended the paragraph already, at a command.
    resolved: bool,
}

/// The settings a text's commands change.
#[derive(Clone, Copy, Debug)]
struct Settings {
    align: Align,
    /// Left and right, in millipoints.
    margins: (i64, i64),
    leading: i64,
    paragraph: i64,
    underline: Option<(i8, u8)>,
    background: [u8; 3],
    foreground: [u8; 3],
}

/// A line's room across: its edges in the layout's units and its width in
/// whole millipoints, as `Font_ScanString` is given it.
#[derive(Clone, Copy, Debug)]
struct Room {
    left: i64,
    right: i64,
    width: i64,
}

struct Painter<'a> {
    columns: Columns<'a>,
    /// The column being filled, in draw units, or none once they are all.
    column: Option<[i32; 4]>,
    render: Matrix,
    places: Places,
    settings: Settings,
    fonts: [Option<u32>; FONTS],
    buffer: [u8; BUFFER_BYTES as usize],
    /// The text's length after the prefix.
    len: usize,
    /// Where the line being built starts.
    start: usize,
    /// The margins its first character took.
    margins: Option<(i64, i64)>,
    /// The underline it goes out with first.
    underline: Option<(i8, u8)>,
    /// What is left of a line part of which went out as a piece.
    room: Option<Room>,
    /// The next line's baseline, from the column's first text.
    y: Option<i64>,
    /// Lines gone out in this column.
    lines: u32,
    run: Run,
    /// A paragraph ended and its next printing character not yet met.
    after_break: bool,
}

impl<'a> Painter<'a> {
    fn new(area: &Area<'a>, render: &Matrix, places: Places) -> Self {
        let mut columns = area.columns();
        let column = columns.next();
        let rgb = |word: u32| {
            let [_, r, g, b] = word.to_le_bytes();
            [r, g, b]
        };
        let margin = i64::from(MARGIN) * MILLIPOINTS;
        Self {
            columns,
            column,
            render: *render,
            places,
            settings: Settings {
                align: Align::Left,
                margins: (margin, margin),
                leading: i64::from(LEADING) * MILLIPOINTS,
                paragraph: i64::from(PARAGRAPH) * MILLIPOINTS,
                underline: None,
                background: rgb(area.background),
                foreground: rgb(area.foreground),
            },
            fonts: [None; FONTS],
            buffer: [0; BUFFER_BYTES as usize],
            len: 0,
            start: 0,
            margins: None,
            underline: None,
            room: None,
            y: None,
            lines: 0,
            run: Run::default(),
            after_break: false,
        }
    }

    /// Every piece of the text laid out, until the text or the columns end.
    fn run(&mut self, os: &mut Os<'_>, area: &Area<'_>) -> Result<(), Fail> {
        let mut reader = Reader::new(area.text);
        if !reader.versioned() {
            return Err(refused(os, Refusal::NoVersion));
        }
        while self.column.is_some() {
            let Some(piece) = reader.next().map_err(|refusal| refused(os, refusal))? else {
                break;
            };
            match piece {
                Piece::Newline => {
                    if self.run.resolved {
                        self.run = Run::default();
                    }
                    self.run.newlines += 1;
                }
                Piece::Break => {
                    self.resolve_early(os)?;
                    self.end_paragraph(os)?;
                }
                Piece::Char(byte) => self.printing(os, &[byte], byte == b' ')?,
                Piece::SoftHyphen => self.printing(os, &[code::COMMENT, 0], false)?,
                Piece::Command(command) => self.command(os, command)?,
            }
        }
        self.end_paragraph(os)
    }

    fn command(&mut self, os: &mut Os<'_>, command: Command<'_>) -> Result<(), Fail> {
        self.resolve_early(os)?;
        let codes = match command {
            Command::Select(number) => {
                let Some(handle) = self.fonts[usize::from(number)] else {
                    return Err(refused(os, Refusal::UndefinedFont(u32::from(number))));
                };
                Codes::new(&[code::FONT, handle.to_le_bytes()[0]])
            }
            Command::Colour { background, rgb } => {
                if background {
                    self.settings.background = rgb;
                } else {
                    self.settings.foreground = rgb;
                }
                let [br, bg, bb] = self.settings.background;
                let [fr, fg, fb] = self.settings.foreground;
                let offset = font::MOST_OFFSET.to_le_bytes()[0];
                Codes::new(&[code::RGB, br, bg, bb, fr, fg, fb, offset])
            }
            Command::Underline(underline) => {
                self.settings.underline = underline;
                let (position, thickness) = underline.unwrap_or((0, 0));
                Codes::new(&[code::UNDERLINE, position.to_le_bytes()[0], thickness])
            }
            Command::Move(points) => {
                let moved = (i64::from(points) * MILLIPOINTS).to_le_bytes();
                Codes::new(&[code::MOVE_UP, moved[0], moved[1], moved[2]])
            }
            Command::Define {
                number,
                name,
                width,
                height,
            } => {
                let handle = self.find_font(os, name, width, height)?;
                self.fonts[usize::from(number)] = Some(handle);
                return Ok(());
            }
            Command::Columns => return Ok(()),
            Command::Align(_)
            | Command::Leading(_)
            | Command::Margins(..)
            | Command::Paragraph(_) => {
                if self.len > self.start {
                    self.flush(os, true)?;
                }
                let points = |given: i32, least: i32| i64::from(given.max(least)) * MILLIPOINTS;
                match command {
                    Command::Align(align) => self.settings.align = align,
                    Command::Leading(given) => self.settings.leading = points(given, LEAST_MARGIN),
                    Command::Margins(left, right) => {
                        self.settings.margins =
                            (points(left, LEAST_MARGIN), points(right, LEAST_MARGIN));
                    }
                    Command::Paragraph(given) => {
                        self.settings.paragraph = points(given, LEAST_PARAGRAPH);
                    }
                    _ => {}
                }
                return Ok(());
            }
        };
        self.started();
        self.append(os, codes.bytes())
    }

    /// A printing character's `bytes`, after what a newline run before it
    /// makes of it.
    fn printing(&mut self, os: &mut Os<'_>, bytes: &[u8], space: bool) -> Result<(), Fail> {
        if self.run.newlines > 0 && !self.run.resolved {
            let newlines = self.run.newlines;
            self.run = Run::default();
            if newlines == 1 && !space {
                self.started();
                self.append(os, b" ")?;
                self.take_margins();
            } else {
                self.end_paragraph(os)?;
                self.gap(newlines - 1);
                self.after_break = true;
            }
        }
        self.run = Run::default();
        if core::mem::take(&mut self.after_break) && space {
            if self.len > self.start {
                self.flush(os, false)?;
                self.reset_buffer();
            } else {
                self.gap(1);
            }
        }
        if self.column.is_none() {
            return Ok(());
        }
        self.started();
        self.append(os, bytes)?;
        self.take_margins();
        Ok(())
    }

    /// A run of two or more newlines ended at a command: the paragraph put
    /// out, the run's paragraph leading taken, what follows it waited on.
    fn resolve_early(&mut self, os: &mut Os<'_>) -> Result<(), Fail> {
        if self.run.newlines >= 2 && !self.run.resolved {
            self.end_paragraph(os)?;
            self.gap(self.run.newlines - 1);
            self.run.resolved = true;
            self.after_break = true;
        }
        Ok(())
    }

    /// `paragraphs` paragraph leadings taken before the next line, but at
    /// a column's top.
    fn gap(&mut self, paragraphs: u32) {
        if self.lines > 0 {
            if let Some(y) = &mut self.y {
                let gap = i64::from(paragraphs).saturating_mul(self.settings.paragraph);
                *y = y.saturating_sub(gap.saturating_mul(PER_MILLIPOINT));
            }
        }
    }

    /// The column's first baseline fixed by its first text.
    fn started(&mut self) {
        if self.y.is_none() {
            if let Some([_, _, _, top]) = self.column {
                self.y =
                    Some(i64::from(top) * PER_DRAW_UNIT - self.settings.leading * PER_MILLIPOINT);
            }
        }
    }

    /// The line's margins taken at its first character.
    fn take_margins(&mut self) {
        if self.margins.is_none() {
            self.margins = Some(self.settings.margins);
        }
    }

    /// The paragraph's lines all put out and its buffer emptied.
    fn end_paragraph(&mut self, os: &mut Os<'_>) -> Result<(), Fail> {
        self.flush(os, false)?;
        self.reset_buffer();
        Ok(())
    }

    fn reset_buffer(&mut self) {
        self.len = 0;
        self.start = 0;
        self.margins = None;
        self.room = None;
    }

    /// `bytes` added to the paragraph, room made first where the buffer is
    /// full: whole lines put out, the unfinished one moved to the start,
    /// or, filling it alone, put out as a piece of its line.
    fn append(&mut self, os: &mut Os<'_>, bytes: &[u8]) -> Result<(), Fail> {
        if self.len + bytes.len() > CAPACITY {
            let rest = self.flush(os, true)?;
            if self.start > 0 {
                self.buffer
                    .copy_within(PREFIX + self.start..PREFIX + self.len, PREFIX);
                self.len -= self.start;
                self.start = 0;
                self.write(os, 0, self.len)?;
            }
            if self.len + bytes.len() > CAPACITY {
                self.put_piece(os, rest)?;
            }
        }
        if self.column.is_none() {
            return Ok(());
        }
        let at = self.len;
        self.buffer[PREFIX + at..PREFIX + at + bytes.len()].copy_from_slice(bytes);
        self.len += bytes.len();
        self.write(os, at, self.len)
    }

    /// The buffer's text from `from` to `to` written to guest memory.
    fn write(&self, os: &mut Os<'_>, from: usize, to: usize) -> Result<(), Fail> {
        os.write_bytes(self.address(from), &self.buffer[PREFIX + from..PREFIX + to])
    }

    fn address(&self, at: usize) -> u32 {
        self.places.buffer + u32::try_from(PREFIX + at).unwrap_or(0)
    }

    /// The buffer's lines put out from the line being built: only those it
    /// breaks short of the text's end where `whole_only`, else all. Where
    /// whole lines alone are put out, the width the rest takes.
    fn flush(&mut self, os: &mut Os<'_>, whole_only: bool) -> Result<Option<i64>, Fail> {
        while self.start < self.len && self.column.is_some() {
            os.write_bytes(self.address(self.len), &[0])?;
            let room = self.room();
            let (split, width) = self.scan(os, self.start, Some(room.width), None)?;
            if split >= self.len {
                if whole_only {
                    return Ok(Some(width));
                }
                let end = self.trimmed(self.start, self.len);
                self.put_line(os, end, Ending::Last, None)?;
                self.start = self.len;
                return Ok(None);
            }
            let Break { end, next, hyphen } = self.break_at(split);
            if let Some(at) = hyphen {
                self.buffer[PREFIX + at] = b'-';
                self.write(os, at, at + 1)?;
            }
            self.put_line(os, end, Ending::Broken, None)?;
            self.start = next;
            self.margins = self
                .has_text(self.start, self.len)
                .then_some(self.settings.margins);
        }
        Ok(None)
    }

    /// The line from `start`, broken at or before `split`: at its last
    /// space, its trailing spaces dropped and the next line after the run
    /// of them; else at a soft hyphen, which shows; else at `split` itself.
    fn break_at(&self, split: usize) -> Break {
        let mut last = None;
        let mut i = self.start;
        while i <= split && i < self.len {
            let byte = self.buffer[PREFIX + i];
            if let Some(length) = code_length(byte) {
                if byte == code::COMMENT && i < split {
                    last = Some((i, true));
                }
                i += length;
            } else {
                if byte == b' ' {
                    last = Some((i, false));
                }
                i += 1;
            }
        }
        let plain = |end, next| Break {
            end,
            next,
            hyphen: None,
        };
        match last {
            Some((at, false)) => {
                let mut next = at + 1;
                while next < self.len && self.buffer[PREFIX + next] == b' ' {
                    next += 1;
                }
                plain(self.trimmed(self.start, at), next)
            }
            Some((at, true)) => Break {
                end: at + 1,
                next: at + 2,
                hyphen: Some(at),
            },
            None if split > self.start => plain(split, split),
            None => {
                let end = self.first_character_end(self.start);
                plain(end, end)
            }
        }
    }

    /// `end` brought back over the spaces before it, no further than `start`.
    fn trimmed(&self, start: usize, mut end: usize) -> usize {
        while end > start && self.buffer[PREFIX + end - 1] == b' ' {
            end -= 1;
        }
        end
    }

    /// Past the codes from `at` and the character after them: the least a
    /// line holds that nothing fits.
    fn first_character_end(&self, mut at: usize) -> usize {
        while at < self.len {
            match code_length(self.buffer[PREFIX + at]) {
                Some(length) => at += length,
                None => return (at + 1).min(self.len),
            }
        }
        self.len
    }

    /// Whether the buffer from `from` to `to` holds a printing character.
    fn has_text(&self, from: usize, to: usize) -> bool {
        let mut i = from;
        while i < to {
            match code_length(self.buffer[PREFIX + i]) {
                Some(length) => i += length,
                None => return true,
            }
        }
        false
    }

    /// The line being built's room: what a piece left, else its column's
    /// less its margins.
    fn room(&self) -> Room {
        if let Some(room) = self.room {
            return room;
        }
        let [x0, _, x1, _] = self.column.unwrap_or_default();
        let (left_margin, right_margin) = self.margins.unwrap_or(self.settings.margins);
        let across = (i64::from(x1) - i64::from(x0)) * PER_DRAW_UNIT / PER_MILLIPOINT;
        Room {
            left: i64::from(x0) * PER_DRAW_UNIT + left_margin * PER_MILLIPOINT,
            right: i64::from(x1) * PER_DRAW_UNIT - right_margin * PER_MILLIPOINT,
            width: across - left_margin - right_margin,
        }
    }

    /// `Font_ScanString` from `from` to the width `limit`, or measuring
    /// `length` bytes whole: where it stopped and the width to there.
    fn scan(
        &self,
        os: &mut Os<'_>,
        from: usize,
        limit: Option<i64>,
        length: Option<usize>,
    ) -> Result<(usize, i64), Fail> {
        let mut regs = Regs::default();
        regs.r[1] = self.address(from);
        match (limit, length) {
            (Some(width), _) => {
                regs.r[3] = saturate_i32(width).cast_unsigned();
                regs.r[4] = NO_LIMIT;
            }
            (None, length) => {
                regs.r[2] = font::flag::LENGTH;
                regs.r[3] = NO_LIMIT;
                regs.r[4] = NO_LIMIT;
                regs.r[7] = u32::try_from(length.unwrap_or(0)).unwrap_or(0);
            }
        }
        os.perform(X | font::SCAN_STRING, &mut regs)?;
        let stopped =
            usize::try_from(regs.r[1].wrapping_sub(self.address(0))).unwrap_or(usize::MAX);
        Ok((stopped.min(self.len), i64::from(regs.r[3].cast_signed())))
    }

    /// A line too long for the buffer: all it holds put out as a piece, the
    /// rest of the line laid out in the room right of it, on a new line
    /// where none is left.
    fn put_piece(&mut self, os: &mut Os<'_>, width: Option<i64>) -> Result<(), Fail> {
        os.write_bytes(self.address(self.len), &[0])?;
        self.put_line(os, self.len, Ending::Piece, width)?;
        self.len = 0;
        self.start = 0;
        Ok(())
    }

    /// The line from `self.start` to `end` painted where it goes, a new
    /// column taken where it falls below this one; `known` its width where
    /// a scan already gave it.
    fn put_line(
        &mut self,
        os: &mut Os<'_>,
        end: usize,
        ending: Ending,
        known: Option<i64>,
    ) -> Result<(), Fail> {
        self.started();
        let Some(mut y) = self.y else {
            return Ok(());
        };
        if self.room.is_none() {
            loop {
                let Some([_, bottom, _, _]) = self.column else {
                    return Ok(());
                };
                if y >= i64::from(bottom) * PER_DRAW_UNIT {
                    break;
                }
                self.column = self.columns.next();
                self.lines = 0;
                let Some([_, _, _, top]) = self.column else {
                    return Ok(());
                };
                y = i64::from(top) * PER_DRAW_UNIT - self.settings.leading * PER_MILLIPOINT;
            }
            self.y = Some(y);
        }
        let room = self.room();
        let length = end - self.start;
        let (x, width, spacing) = match self.settings.align {
            Align::Left => (room.left, known.unwrap_or(0), 0),
            align => {
                let (_, width) = self.scan(os, self.start, None, Some(length))?;
                let x = match align {
                    Align::Right => room.right - width * PER_MILLIPOINT,
                    Align::Centre => room.left + (room.width - width) / 2 * PER_MILLIPOINT,
                    _ => room.left,
                };
                let spaces = self.spaces(self.start, end);
                let spacing = if align == Align::Double && ending == Ending::Broken && spaces > 0 {
                    (room.width - width) / spaces
                } else {
                    0
                };
                (x, width, spacing)
            }
        };
        let mut from = self.address(self.start);
        let mut painted = length;
        if let Some((position, thickness)) = self.underline {
            let prefix: [u8; PREFIX] = [code::UNDERLINE, position.to_le_bytes()[0], thickness];
            from -= u32::try_from(PREFIX).unwrap_or(0);
            painted += PREFIX;
            os.write_bytes(from, &prefix)?;
        }
        let kept = self.buffer[PREFIX + end];
        os.write_bytes(self.address(end), &[0])?;
        let mut block = [0u32; BLOCK_BYTES as usize / 4];
        block[0] = saturate_i32(spacing).cast_unsigned();
        os.write_words(self.places.block, &block)?;
        let (px, py) = self.millipoints(x, y);
        let mut regs = Regs::default();
        regs.r[1..8].copy_from_slice(&[
            from,
            PAINT_FLAGS,
            px.cast_unsigned(),
            py.cast_unsigned(),
            self.places.block,
            self.places.matrix,
            u32::try_from(painted).unwrap_or(0),
        ]);
        os.perform(X | font::PAINT, &mut regs)?;
        os.write_bytes(self.address(end), &[kept])?;
        self.underline = self.underline_after(self.start, end);
        self.finish_line(ending, room, x, width);
        Ok(())
    }

    /// After a line or piece went out: the next line's baseline, or the
    /// room left of this line right of a piece.
    fn finish_line(&mut self, ending: Ending, room: Room, x: i64, width: i64) {
        if ending == Ending::Piece {
            let left = x + width * PER_MILLIPOINT;
            let left_over = (room.right - left) / PER_MILLIPOINT;
            if left_over > 0 {
                self.room = Some(Room {
                    left,
                    right: room.right,
                    width: left_over,
                });
                return;
            }
        }
        self.room = None;
        self.lines += 1;
        if let Some(y) = &mut self.y {
            *y -= self.settings.leading * PER_MILLIPOINT;
        }
    }

    /// The spaces in the line's text from `from` to `to`.
    fn spaces(&self, from: usize, to: usize) -> i64 {
        let mut count = 0;
        let mut i = from;
        while i < to {
            if let Some(length) = code_length(self.buffer[PREFIX + i]) {
                i += length;
            } else {
                count += i64::from(self.buffer[PREFIX + i] == b' ');
                i += 1;
            }
        }
        count
    }

    /// The underline the next line goes out with: the last code in this
    /// line's own text, or none.
    fn underline_after(&self, from: usize, to: usize) -> Option<(i8, u8)> {
        let mut found = None;
        let mut i = from;
        while i < to {
            let byte = self.buffer[PREFIX + i];
            match code_length(byte) {
                Some(length) => {
                    if byte == code::UNDERLINE {
                        let argument =
                            |k: usize| self.buffer.get(PREFIX + i + k).copied().unwrap_or(0);
                        found = Some((i8::from_le_bytes([argument(1)]), argument(2)));
                    }
                    i += length;
                }
                None => i += 1,
            }
        }
        found.filter(|&underline| underline != (0, 0))
    }

    /// A point in the layout's units through the render's matrix, in
    /// whole millipoints cut toward nought.
    fn millipoints(&self, x: i64, y: i64) -> (i32, i32) {
        let m = &self.render;
        let axis = |p: i32, q: i32, t: i32| {
            let sum = i128::from(p) * i128::from(x)
                + i128::from(q) * i128::from(y)
                + i128::from(t) * i128::from(PER_DRAW_UNIT) * i128::from(FIXED_ONE);
            let value = sum / (i128::from(PER_MILLIPOINT) * i128::from(FIXED_ONE));
            i32::try_from(value).unwrap_or(if value < 0 { i32::MIN } else { i32::MAX })
        };
        (axis(m.a, m.c, m.e), axis(m.b, m.d, m.f))
    }

    /// Font `name` found at `width` and `height` sixteenths, its name
    /// copied out to the workspace.
    fn find_font(
        &mut self,
        os: &mut Os<'_>,
        name: &[u8],
        width: u32,
        height: u32,
    ) -> Result<u32, Fail> {
        write_name(os, self.places.name, self.places.name_bytes, name)?;
        let mut regs = Regs::default();
        regs.r[1..4].copy_from_slice(&[self.places.name, width, height]);
        os.perform(X | font::FIND_FONT, &mut regs)?;
        Ok(regs.r[0])
    }

    /// Every font the text found let go, in number order.
    fn lose_fonts(&mut self, os: &mut Os<'_>) -> Result<(), Fail> {
        let mut first = Ok(());
        for handle in self.fonts.iter_mut().filter_map(Option::take) {
            let mut regs = Regs::default();
            regs.r[0] = handle;
            let lost = os.perform(X | font::LOSE_FONT, &mut regs);
            if first.is_ok() {
                first = lost;
            }
        }
        first
    }
}

/// A code's bytes, four at most short of a colour change's eight.
struct Codes {
    bytes: [u8; 8],
    len: usize,
}

impl Codes {
    fn new(bytes: &[u8]) -> Self {
        let mut held = [0; 8];
        held[..bytes.len()].copy_from_slice(bytes);
        Self {
            bytes: held,
            len: bytes.len(),
        }
    }

    fn bytes(&self) -> &[u8] {
        &self.bytes[..self.len]
    }
}

#[cfg(test)]
mod tests;
