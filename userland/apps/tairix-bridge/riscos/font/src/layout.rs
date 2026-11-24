//! The walk the measuring calls make along a font string, as RISC OS 5.30
//! makes it: each character placed by its font's metrics, its kerning, the
//! justification offsets and the matrices, the control sequences obeyed,
//! until the string ends, its length is spent or a limit is passed.
//!
//! A character's advance and kern are carried through the font's matrix and
//! the matrix painting it; the justification offsets and the moves are not,
//! and right to left each step is taken backwards, a move's excepted. A limit
//! is checked on each axis a step changes. Where a limit is passed the walk
//! stops before the step, or with a split character at the last one met.

use tairix_riscos_kernel::os::{Fail, Os, Regs};

use crate::colours::Colours;
use crate::identifier::Matrix;
use crate::metrics;
use crate::module::{error, raise};
use crate::resolve::{self, Font};
use crate::transform::{scaled, scaled_box, Transform};
use crate::units;

/// How wide a string's units are.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Form {
    Bytes,
    Halfwords,
    Words,
}

impl Form {
    const fn width(self) -> u32 {
        match self {
            Self::Bytes => 1,
            Self::Halfwords => 2,
            Self::Words => 4,
        }
    }
}

/// Where a walk stops short of the string's end.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Limit {
    /// It goes to the end.
    None,
    /// `Font_ScanString`'s: past each, on its side of nought.
    Signed(i32, i32),
    /// `Font_StringWidth`'s: past either, upwards.
    Greater(i32, i32),
    /// It goes to the end, answering the place nearest the point.
    Caret(i32, i32),
}

/// What a walk is asked to do.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Request {
    /// The string's address.
    pub(crate) at: u32,
    pub(crate) form: Form,
    /// The bytes of it to walk at most.
    pub(crate) length: Option<u32>,
    pub(crate) kern: bool,
    pub(crate) right_to_left: bool,
    pub(crate) limit: Limit,
    /// The character a line may be split at.
    pub(crate) split: Option<u32>,
    /// What each space, and each character after the first, moves the pen
    /// besides, in millipoints.
    pub(crate) space: (i32, i32),
    pub(crate) letter: (i32, i32),
    /// The matrix painting the string, until the string sets another.
    pub(crate) matrix: Option<Matrix>,
}

/// A place in the walk: how far into the string, where the pen is, the
/// split characters or else the characters met before it, the box of what
/// was placed before it, the matrix painting there, and the font and
/// colours there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Place {
    pub(crate) offset: u32,
    pub(crate) pen: (i32, i32),
    pub(crate) count: u32,
    pub(crate) bbox: Option<metrics::Box>,
    pub(crate) matrix: Option<Matrix>,
    pub(crate) colours: Colours,
}

/// Where a walk stopped, and where the step that passed a limit would have
/// left it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Walked {
    pub(crate) stop: Place,
    pub(crate) past: Option<Place>,
}

/// The flag bits `Font_ScanString` and `Font_Paint` share in R2.
pub(crate) mod flag {
    /// R4 and R5 are in OS units: `Font_Paint`'s, and `Font_CharBBox`'s R2.
    pub(crate) const OS_UNITS: u32 = 1 << 4;
    /// R5 points at a block: justification offsets first.
    pub(crate) const BLOCK: u32 = 1 << 5;
    /// R6 points at a matrix.
    pub(crate) const MATRIX: u32 = 1 << 6;
    /// R7 holds the string's length in bytes.
    pub(crate) const LENGTH: u32 = 1 << 7;
    /// R0 holds the font, nought for the current one.
    pub(crate) const HANDLE: u32 = 1 << 8;
    pub(crate) const KERN: u32 = 1 << 9;
    pub(crate) const RIGHT_TO_LEFT: u32 = 1 << 10;
    /// The string's units are halfwords, or words.
    pub(crate) const HALFWORDS: u32 = 1 << 12;
    pub(crate) const WORDS: u32 = 1 << 13;
}

/// The font and the request R0 to R7 make of a string walk as
/// `Font_ScanString` and `Font_Paint` both take them, every limit and split
/// character left to the caller.
pub(crate) fn requested(os: &mut Os<'_>, regs: &Regs) -> Result<(Font, Request), Fail> {
    let flags = regs.r[2];
    let handle = match (flags & flag::HANDLE != 0, regs.r[0]) {
        (true, 0) | (false, _) => resolve::current(os)?,
        (true, handle) => handle,
    };
    let font = resolve::font(os, handle)?;
    let form = if flags & flag::WORDS != 0 {
        Form::Words
    } else if flags & flag::HALFWORDS != 0 {
        Form::Halfwords
    } else {
        Form::Bytes
    };
    let (space, letter) = if flags & flag::BLOCK != 0 {
        let block = regs.r[5];
        let word =
            |os: &mut Os<'_>, at: u32| os.read_word(block.wrapping_add(at)).map(u32::cast_signed);
        ((word(os, 0)?, word(os, 4)?), (word(os, 8)?, word(os, 12)?))
    } else {
        ((0, 0), (0, 0))
    };
    let matrix = if flags & flag::MATRIX != 0 {
        Some(read_matrix(os, regs.r[6])?)
    } else {
        None
    };
    let request = Request {
        at: regs.r[1],
        form,
        length: (flags & flag::LENGTH != 0).then_some(regs.r[7]),
        kern: flags & flag::KERN != 0,
        right_to_left: flags & flag::RIGHT_TO_LEFT != 0,
        limit: Limit::None,
        split: None,
        space,
        letter,
        matrix,
    };
    Ok((font, request))
}

/// The matrix of six words at `at`.
pub(crate) fn read_matrix(os: &mut Os<'_>, at: u32) -> Result<Matrix, Fail> {
    let mut words = [0i32; 6];
    for (i, word) in (0u32..).zip(words.iter_mut()) {
        *word = os.read_word(at.wrapping_add(4 * i))?.cast_signed();
    }
    Ok(Matrix(words))
}

/// A character a walk places: its glyph, where none is defined, the pen it
/// is drawn from, and the pen before and after the step it takes, in
/// millipoints.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Placed {
    pub(crate) glyph: Option<u32>,
    pub(crate) origin: (i32, i32),
    pub(crate) from: (i32, i32),
    pub(crate) to: (i32, i32),
}

/// What a walk hands a paint as it goes, and how a paint answers a change
/// of colour; the measuring calls, which draw nothing, take
/// [`Measuring`].
pub(crate) trait Visit {
    /// A character placed in `font`, drawn through `transform`, in
    /// `colours`.
    fn character(
        &mut self,
        os: &mut Os<'_>,
        font: &Font,
        transform: &Transform,
        placed: &Placed,
        colours: &Colours,
    ) -> Result<(), Fail>;

    /// The pen moved by a sequence from `from` to `to`.
    fn moved(&mut self, os: &mut Os<'_>, from: (i32, i32), to: (i32, i32)) -> Result<(), Fail>;

    /// An underline set at the pen `at`, its top and thickness in 256ths of
    /// `font`'s size.
    fn underline(
        &mut self,
        os: &mut Os<'_>,
        font: &Font,
        at: (i32, i32),
        position: i32,
        thickness: u32,
    ) -> Result<(), Fail>;

    /// The colours a sequence changed to: an RGB change, the background,
    /// the foreground and the most offset as `&BBGGRR00`, as `rgb` holds.
    fn colours(&mut self, os: &mut Os<'_>, colours: &mut Colours) -> Result<(), Fail>;
}

/// A measuring call's walk, which draws nothing and keeps the colours a
/// string sets as it sets them.
pub(crate) struct Measuring;

impl Visit for Measuring {
    fn character(
        &mut self,
        _: &mut Os<'_>,
        _: &Font,
        _: &Transform,
        _: &Placed,
        _: &Colours,
    ) -> Result<(), Fail> {
        Ok(())
    }

    fn moved(&mut self, _: &mut Os<'_>, _: (i32, i32), _: (i32, i32)) -> Result<(), Fail> {
        Ok(())
    }

    fn underline(
        &mut self,
        _: &mut Os<'_>,
        _: &Font,
        _: (i32, i32),
        _: i32,
        _: u32,
    ) -> Result<(), Fail> {
        Ok(())
    }

    fn colours(&mut self, _: &mut Os<'_>, _: &mut Colours) -> Result<(), Fail> {
        Ok(())
    }
}

/// The control codes a string holds.
mod code {
    pub(super) const END: u32 = 0;
    pub(super) const MOVE_ACROSS: u32 = 9;
    pub(super) const LINE_FEED: u32 = 10;
    pub(super) const MOVE_UP: u32 = 11;
    pub(super) const RETURN: u32 = 13;
    pub(super) const COLOUR: u32 = 17;
    pub(super) const COLOURS: u32 = 18;
    pub(super) const RGB: u32 = 19;
    pub(super) const COMMENT: u32 = 21;
    pub(super) const UNDERLINE: u32 = 25;
    pub(super) const FONT: u32 = 26;
    pub(super) const MATRIX: u32 = 27;
    pub(super) const MATRIX_MOVED: u32 = 28;
    pub(super) const SPACE: u32 = 32;
    pub(super) const BACKGROUND: u32 = 0x80;
}

/// What a step of the string is.
#[derive(Clone, Copy)]
enum Step {
    End,
    Character(u32),
    Move(i32, i32),
    Colour(u32),
    Colours(u32, u32, i32),
    Rgb(u32, u32, i32),
    Underline(i32, u32),
    Font(u32),
    Matrix(Matrix),
    Passed,
}

/// The character UTF-8 and UTF-16 read a sequence that is not one as, and
/// the least character a UTF-8 sequence of each length may hold.
const REPLACEMENT: u32 = 0xFFFD;
const SHORTEST: [u32; 6] = [0, 0x80, 0x800, 0x1_0000, 0x20_0000, 0x400_0000];
/// How many steps a walk takes between yields to the host.
const STEPS_BETWEEN_YIELDS: u32 = 256;

/// The string read a unit at a time.
struct Text {
    start: u32,
    form: Form,
    length: Option<u32>,
}

impl Text {
    /// Whether `units` units from `offset` lie within the length.
    fn holds(&self, offset: u32, units: u32) -> bool {
        let end = offset.saturating_add(units.saturating_mul(self.form.width()));
        self.length.is_none_or(|length| end <= length)
    }

    fn unit(&self, os: &mut Os<'_>, offset: u32) -> Result<u32, Fail> {
        let at = self.start.wrapping_add(offset);
        match self.form {
            Form::Bytes => Ok(u32::from(os.read_byte(at)?)),
            Form::Halfwords => {
                let low = os.read_byte(at)?;
                let high = os.read_byte(at.wrapping_add(1))?;
                Ok(u32::from(u16::from_le_bytes([low, high])))
            }
            Form::Words => os.read_word(at),
        }
    }

    /// A unit as a signed number of its width.
    fn signed(&self, unit: u32) -> i32 {
        match self.form {
            Form::Bytes => i32::from(unit.to_le_bytes()[0].cast_signed()),
            Form::Halfwords => i32::from(u16::try_from(unit).unwrap_or(0).cast_signed()),
            Form::Words => unit.cast_signed(),
        }
    }

    /// The step at `offset` and the offset after it; `None` for one the
    /// length does not hold.
    fn step(&self, os: &mut Os<'_>, offset: u32, utf8: bool) -> Result<Option<(Step, u32)>, Fail> {
        if !self.holds(offset, 1) {
            return Ok(None);
        }
        let width = self.form.width();
        let first = self.unit(os, offset)?;
        let after = offset + width;
        if first >= code::SPACE {
            return self.character(os, offset, first, utf8);
        }
        let mut params = Params {
            text: self,
            at: after,
        };
        let step = match first {
            code::END | code::LINE_FEED | code::RETURN => return Ok(Some((Step::End, offset))),
            code::MOVE_ACROSS | code::MOVE_UP => {
                let Some(by) = params.distance(os)? else {
                    return Ok(None);
                };
                if first == code::MOVE_ACROSS {
                    Step::Move(by, 0)
                } else {
                    Step::Move(0, by)
                }
            }
            code::COLOUR => match params.units(os, 1)? {
                Some([colour, ..]) => Step::Colour(colour),
                None => return Ok(None),
            },
            code::COLOURS => match params.units(os, 3)? {
                Some([background, foreground, offset, ..]) => {
                    Step::Colours(background, foreground, self.signed(offset))
                }
                None => return Ok(None),
            },
            code::RGB => match params.colours(os)? {
                Some(step) => step,
                None => return Ok(None),
            },
            code::COMMENT => loop {
                match params.units(os, 1)? {
                    Some([unit, ..]) if unit < code::SPACE => break Step::Passed,
                    Some(_) => {}
                    None => return Ok(None),
                }
            },
            code::UNDERLINE => match params.units(os, 2)? {
                Some([position, thickness, ..]) => {
                    Step::Underline(self.signed(position), thickness)
                }
                None => return Ok(None),
            },
            code::FONT => match params.units(os, 1)? {
                Some([handle, ..]) => Step::Font(handle),
                None => return Ok(None),
            },
            code::MATRIX | code::MATRIX_MOVED => {
                let words = if first == code::MATRIX { 4 } else { 6 };
                match params.matrix(os, words)? {
                    Some(matrix) => Step::Matrix(matrix),
                    None => return Ok(None),
                }
            }
            _ => return Err(raise(os, error::CTRL_CHAR, &[])),
        };
        Ok(Some((step, params.at)))
    }

    /// The character whose first unit `first` is at `offset`, decoded as a
    /// UTF-8 or UTF-16 font reads it.
    fn character(
        &self,
        os: &mut Os<'_>,
        offset: u32,
        first: u32,
        utf8: bool,
    ) -> Result<Option<(Step, u32)>, Fail> {
        let width = self.form.width();
        let after = offset + width;
        if !utf8 || self.form == Form::Words {
            return Ok(Some((Step::Character(first), after)));
        }
        if self.form == Form::Halfwords {
            if !(0xD800..=0xDBFF).contains(&first) {
                let character = if (0xDC00..=0xDFFF).contains(&first) {
                    REPLACEMENT
                } else {
                    first
                };
                return Ok(Some((Step::Character(character), after)));
            }
            if !self.holds(after, 1) {
                return Ok(None);
            }
            let low = self.unit(os, after)?;
            if !(0xDC00..=0xDFFF).contains(&low) {
                return Ok(Some((Step::Character(REPLACEMENT), after)));
            }
            let character = 0x1_0000 + ((first - 0xD800) << 10) + (low - 0xDC00);
            return Ok(Some((Step::Character(character), after + width)));
        }
        let (more, lead_bits) = match first {
            0x00..=0x7F => return Ok(Some((Step::Character(first), after))),
            0xC0..=0xDF => (1, first & 0x1F),
            0xE0..=0xEF => (2, first & 0x0F),
            0xF0..=0xF7 => (3, first & 0x07),
            0xF8..=0xFB => (4, first & 0x03),
            0xFC..=0xFD => (5, first & 0x01),
            _ => return Ok(Some((Step::Character(REPLACEMENT), after))),
        };
        let mut character = lead_bits;
        let mut at = after;
        for _ in 0..more {
            if !self.holds(at, 1) {
                return Ok(None);
            }
            let next = self.unit(os, at)?;
            if next & 0xC0 != 0x80 {
                return Ok(Some((Step::Character(REPLACEMENT), after)));
            }
            character = character << 6 | (next & 0x3F);
            at += 1;
        }
        if character < SHORTEST[more] {
            character = REPLACEMENT;
        }
        Ok(Some((Step::Character(character), at)))
    }
}

/// A control sequence's parameters read in order.
struct Params<'a> {
    text: &'a Text,
    at: u32,
}

impl Params<'_> {
    /// The next `n` units, at most four; none where the length runs out.
    fn units(&mut self, os: &mut Os<'_>, n: u32) -> Result<Option<[u32; 4]>, Fail> {
        if !self.text.holds(self.at, n) {
            return Ok(None);
        }
        let mut units = [0; 4];
        for unit in units.iter_mut().take(n as usize) {
            *unit = self.text.unit(os, self.at)?;
            self.at += self.text.form.width();
        }
        Ok(Some(units))
    }

    /// A move's distance: three bytes, two halfwords or a word, signed.
    fn distance(&mut self, os: &mut Os<'_>) -> Result<Option<i32>, Fail> {
        let distance = match self.text.form {
            Form::Bytes => self.units(os, 3)?.map(|[a, b, c, _]| {
                let packed = a | b << 8 | c << 16;
                ((packed << 8).cast_signed()) >> 8
            }),
            Form::Halfwords => self
                .units(os, 2)?
                .map(|[low, high, ..]| (low | high << 16).cast_signed()),
            Form::Words => self.units(os, 1)?.map(|[word, ..]| word.cast_signed()),
        };
        Ok(distance)
    }

    /// An RGB colour change: the background and the foreground as
    /// `&BBGGRR00`, then the most anti-aliasing offset.
    fn colours(&mut self, os: &mut Os<'_>) -> Result<Option<Step>, Fail> {
        let step = match self.text.form {
            Form::Bytes => {
                let Some([r, g, b, _]) = self.units(os, 3)? else {
                    return Ok(None);
                };
                let Some([fr, fg, fb, _]) = self.units(os, 3)? else {
                    return Ok(None);
                };
                let Some([most, ..]) = self.units(os, 1)? else {
                    return Ok(None);
                };
                let rgb = |r: u32, g: u32, b: u32| r << 8 | g << 16 | b << 24;
                Step::Rgb(rgb(r, g, b), rgb(fr, fg, fb), self.text.signed(most))
            }
            Form::Halfwords => {
                let Some([bl, bh, fl, fh]) = self.units(os, 4)? else {
                    return Ok(None);
                };
                let Some([most, ..]) = self.units(os, 1)? else {
                    return Ok(None);
                };
                Step::Rgb(bl | bh << 16, fl | fh << 16, self.text.signed(most))
            }
            Form::Words => {
                let Some([background, foreground, most, _]) = self.units(os, 3)? else {
                    return Ok(None);
                };
                Step::Rgb(background, foreground, most.cast_signed())
            }
        };
        Ok(Some(step))
    }

    /// A matrix of `words` words, from the next word boundary; a matrix of
    /// four has no translation.
    fn matrix(&mut self, os: &mut Os<'_>, words: u32) -> Result<Option<Matrix>, Fail> {
        let address = self.text.start.wrapping_add(self.at);
        let aligned = address.wrapping_add(3) & !3;
        let at = self.at.wrapping_add(aligned.wrapping_sub(address));
        let end = at.saturating_add(4 * words);
        if self.text.length.is_some_and(|length| end > length) {
            return Ok(None);
        }
        let mut matrix = [0i32; 6];
        for (i, word) in matrix.iter_mut().take(words as usize).enumerate() {
            let index = u32::try_from(i).unwrap_or(0);
            *word = os
                .read_word(self.text.start.wrapping_add(at + 4 * index))?
                .cast_signed();
        }
        self.at = end;
        Ok(Some(Matrix(matrix)))
    }
}

/// The walk's state.
struct Walker {
    request: Request,
    font: Font,
    /// The font's matrix and then the painting one.
    transform: Transform,
    place: Place,
    previous: Option<u32>,
    placed_any: bool,
}

impl Walker {
    fn painting(matrix: Option<Matrix>) -> Transform {
        matrix.map_or(Transform::IDENTITY, Transform::of)
    }

    fn sign(&self) -> i32 {
        if self.request.right_to_left {
            -1
        } else {
            1
        }
    }

    /// The step `step` taken from `self.place`, ending at `offset`, shown to
    /// `visit`.
    fn take(
        &mut self,
        os: &mut Os<'_>,
        step: Step,
        offset: u32,
        visit: &mut impl Visit,
    ) -> Result<(), Fail> {
        self.place.offset = offset;
        let colours = &mut self.place.colours;
        match step {
            Step::Character(character) => {
                let placed = self.character(character);
                visit.character(
                    os,
                    &self.font,
                    &self.transform,
                    &placed,
                    &self.place.colours,
                )?;
            }
            Step::Move(x, y) => {
                let from = self.place.pen;
                self.place.pen = (from.0.wrapping_add(x), from.1.wrapping_add(y));
                visit.moved(os, from, self.place.pen)?;
            }
            Step::Colour(colour) if colour & code::BACKGROUND != 0 => {
                colours.background = colour & !code::BACKGROUND;
                colours.rgb = None;
                visit.colours(os, colours)?;
            }
            Step::Colour(colour) => {
                colours.foreground = colour;
                colours.rgb = None;
                visit.colours(os, colours)?;
            }
            Step::Colours(background, foreground, offset) => {
                colours.background = background;
                colours.foreground = foreground;
                colours.offset = offset;
                colours.rgb = None;
                visit.colours(os, colours)?;
            }
            Step::Rgb(background, foreground, most) => {
                colours.rgb = Some((background, foreground, most));
                visit.colours(os, colours)?;
            }
            Step::Underline(position, thickness) => {
                visit.underline(os, &self.font, self.place.pen, position, thickness)?;
            }
            Step::Font(handle) => {
                let font = resolve::font(os, handle)?;
                self.place.colours.font = handle;
                self.transform = font.matrix.then(Self::painting(self.place.matrix));
                self.font = font;
                self.previous = None;
            }
            Step::Matrix(matrix) => {
                self.place.matrix = Some(matrix);
                self.transform = self.font.matrix.then(Self::painting(Some(matrix)));
                self.previous = None;
            }
            Step::Passed | Step::End => {}
        }
        Ok(())
    }

    /// Character `character` placed: what a paint draws of it.
    fn character(&mut self, character: u32) -> Placed {
        let sign = self.sign();
        let data = &self.font.data;
        let glyph = data.code(character);
        let measured = glyph.and_then(|g| data.glyph(g));
        let points = self.font.points;
        let advance = measured.map_or((0, 0), |m| {
            self.transform
                .vector((scaled(m.advance.0, points.0), scaled(m.advance.1, points.1)))
        });
        let kern = match (self.request.kern, self.previous, glyph) {
            (true, Some(left), Some(right)) => {
                let (x, y) = data.kern(left, right);
                self.transform
                    .vector((scaled(x, points.0), scaled(y, points.1)))
            }
            _ => (0, 0),
        };
        let from = self.place.pen;
        let mut pen = from;
        let step = |pen: &mut (i32, i32), (x, y): (i32, i32), sign: i32| {
            *pen = (
                pen.0.wrapping_add(x.wrapping_mul(sign)),
                pen.1.wrapping_add(y.wrapping_mul(sign)),
            );
        };
        if self.placed_any {
            step(&mut pen, self.request.letter, sign);
        }
        step(&mut pen, kern, sign);
        let origin = if sign < 0 {
            step(&mut pen, advance, sign);
            pen
        } else {
            let origin = pen;
            step(&mut pen, advance, sign);
            origin
        };
        if character == code::SPACE {
            step(&mut pen, self.request.space, sign);
        }
        let inked = measured.map_or(metrics::Box::default(), |m| {
            self.transform.bbox(scaled_box(m.bbox, points))
        });
        let placed = metrics::Box {
            x0: inked.x0.wrapping_add(origin.0),
            y0: inked.y0.wrapping_add(origin.1),
            x1: inked.x1.wrapping_add(origin.0),
            y1: inked.y1.wrapping_add(origin.1),
        };
        self.place.bbox = Some(self.place.bbox.map_or(placed, |b| metrics::Box {
            x0: b.x0.min(placed.x0),
            y0: b.y0.min(placed.y0),
            x1: b.x1.max(placed.x1),
            y1: b.y1.max(placed.y1),
        }));
        self.place.pen = pen;
        let counted = match self.request.split {
            Some(split) => split == character,
            None => true,
        };
        if counted {
            self.place.count = self.place.count.saturating_add(1);
        }
        self.previous = glyph;
        self.placed_any = true;
        Placed {
            glyph: glyph.filter(|_| measured.is_some()),
            origin,
            from,
            to: pen,
        }
    }

    /// Whether the pen, having moved from `before`, has passed a limit.
    fn passed(&self, before: (i32, i32)) -> bool {
        let pen = self.place.pen;
        let past = |value: i32, was: i32, limit: i32, signed: bool| {
            value != was
                && if signed && limit < 0 {
                    value < limit
                } else {
                    value > limit
                }
        };
        match self.request.limit {
            Limit::Signed(x, y) => past(pen.0, before.0, x, true) || past(pen.1, before.1, y, true),
            Limit::Greater(x, y) => {
                past(pen.0, before.0, x, false) || past(pen.1, before.1, y, false)
            }
            Limit::None | Limit::Caret(..) => false,
        }
    }
}

/// The 256ths of a font's height an underline's place and thickness are
/// given in.
const UNDERLINE_UNITS: i128 = 256;

/// The underline set at the pen `at` by `position` and `thickness`, in
/// 256ths of `font`'s height: its top and its bottom, in millipoints from the
/// pen's start, exact for any word a string gives.
pub(crate) fn underline_edges(
    font: &Font,
    at: (i32, i32),
    position: i32,
    thickness: u32,
) -> (i64, i64) {
    let height =
        i128::from(font.points.1) * i128::from(units::MILLIPOINTS) / i128::from(units::SIXTEENTHS);
    let scaled = |v: i128| (v * height).div_euclid(UNDERLINE_UNITS);
    let top = i128::from(at.1) + scaled(i128::from(position));
    let bottom = top - scaled(i128::from(thickness));
    let held = |v: i128| i64::try_from(v).unwrap_or(if v < 0 { i64::MIN } else { i64::MAX });
    (held(top), held(bottom))
}

/// The square of how far `place`'s pen is from `(x, y)`, exact for any two
/// points a word reaches.
pub(crate) fn distance((x, y): (i32, i32), place: &Place) -> i128 {
    let dx = i128::from(place.pen.0) - i128::from(x);
    let dy = i128::from(place.pen.1) - i128::from(y);
    dx * dx + dy * dy
}

/// The walk `request` asks for, starting with `font` and `colours`, shown
/// to `visit` as it goes.
pub(crate) fn walk(
    os: &mut Os<'_>,
    request: &Request,
    font: Font,
    colours: Colours,
    visit: &mut impl Visit,
) -> Result<Walked, Fail> {
    let text = Text {
        start: request.at,
        form: request.form,
        length: request.length,
    };
    let start = Place {
        offset: 0,
        pen: (0, 0),
        count: 0,
        bbox: None,
        matrix: request.matrix,
        colours: Colours {
            font: font.handle,
            ..colours
        },
    };
    let mut walker = Walker {
        request: *request,
        transform: font.matrix.then(Walker::painting(request.matrix)),
        font,
        place: start,
        previous: None,
        placed_any: false,
    };
    let mut split = None;
    let mut nearest = match request.limit {
        Limit::Caret(x, y) => Some((distance((x, y), &start), start)),
        _ => None,
    };
    let mut steps = 0u32;
    loop {
        steps = steps.wrapping_add(1);
        if steps.is_multiple_of(STEPS_BETWEEN_YIELDS) {
            os.yield_point()?;
        }
        let before = walker.place;
        let utf8 = walker.font.data.utf8;
        let Some((step, offset)) = text.step(os, before.offset, utf8)? else {
            break;
        };
        if let Step::End = step {
            walker.place.offset = offset;
            break;
        }
        if let (Step::Character(character), Some(wanted)) = (&step, request.split) {
            if *character == wanted {
                split = Some(before);
            }
        }
        walker.take(os, step, offset, visit)?;
        if walker.passed(before.pen) {
            let halted = match request.split {
                Some(_) => split.unwrap_or(start),
                None => before,
            };
            return Ok(Walked {
                stop: halted,
                past: Some(walker.place),
            });
        }
        if let (Limit::Caret(x, y), Some((best, _))) = (request.limit, nearest) {
            let d = distance((x, y), &walker.place);
            if d < best {
                nearest = Some((d, walker.place));
            }
        }
    }
    let stop = nearest.map_or(walker.place, |(_, place)| place);
    Ok(Walked { stop, past: None })
}

#[cfg(test)]
mod tests;
