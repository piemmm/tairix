//! Input and output: `PRINT`, `INPUT`, `VDU`, the graphics and sound
//! statements as the VDU codes and SWIs they are, files, and the
//! pseudo-variables a statement sets.

use tairix_riscos_kernel::os::Regs;
use tairix_riscos_kernel::swi::os::set_colour::{ACTION, BACKGROUND, TEXT};
use tairix_riscos_kernel::swi::{os as swi, sound, X};

use super::super::functions::{string_of, Guest};
use super::super::lvalue::{Lvalue, Place};
use super::super::next::{Delivered, Next, Payload};
use super::super::{len32, Leave, Machine, Mode, Step};
use super::{cont, setting};
use crate::five::Five;
use crate::messages::Error;
use crate::six::Arithmetic;
use crate::stack::Kind;
use crate::token;
use crate::value::{Number, Text, Value};
use crate::workspace as ws;

/// `PRINT`'s state between items.
mod print {
    /// Numbers are right-justified in their field: the first item, and
    /// after a comma.
    pub(super) const PADDED: u32 = 1;
    /// The next number is written in hexadecimal.
    pub(super) const HEX: u32 = 1 << 1;
    /// The statement's last item was a separator that holds the line.
    pub(super) const HELD: u32 = 1 << 2;
}

/// The graphics and sound statements whose arguments are gathered on the
/// stack before they act.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u32)]
enum Gathered {
    Mode = 1,
    Colour,
    Gcol,
    Move,
    MoveBy,
    Draw,
    DrawBy,
    Plot,
    Sound,
    Envelope,
    Width,
    Circle,
    CircleFill,
    Ellipse,
    EllipseFill,
    Rectangle,
    RectangleFill,
    RectangleTo,
    RectangleFillTo,
    Fill,
    FillBy,
    Origin,
    Point,
    PointBy,
    Tint,
    Wait,
    Beats,
    Tempo,
    Voices,
    Stereo,
    Vdu,
    Line,
    ColourTint,
    GcolTint,
    ColourOf,
    ColourOn,
    GcolOf,
    GcolOn,
}

impl Gathered {
    const ALL: [Self; 38] = [
        Self::Mode,
        Self::Colour,
        Self::Gcol,
        Self::Move,
        Self::MoveBy,
        Self::Draw,
        Self::DrawBy,
        Self::Plot,
        Self::Sound,
        Self::Envelope,
        Self::Width,
        Self::Circle,
        Self::CircleFill,
        Self::Ellipse,
        Self::EllipseFill,
        Self::Rectangle,
        Self::RectangleFill,
        Self::RectangleTo,
        Self::RectangleFillTo,
        Self::Fill,
        Self::FillBy,
        Self::Origin,
        Self::Point,
        Self::PointBy,
        Self::Tint,
        Self::Wait,
        Self::Beats,
        Self::Tempo,
        Self::Voices,
        Self::Stereo,
        Self::Vdu,
        Self::Line,
        Self::ColourTint,
        Self::GcolTint,
        Self::ColourOf,
        Self::ColourOn,
        Self::GcolOf,
        Self::GcolOn,
    ];

    fn of(word: u32) -> Option<Self> {
        Self::ALL.into_iter().find(|&g| g as u32 == word)
    }

    /// The fewest and most arguments it takes.
    const fn arguments(self) -> (u32, u32) {
        match self {
            Self::Mode
            | Self::Colour
            | Self::Gcol
            | Self::ColourOf
            | Self::ColourOn
            | Self::GcolOf
            | Self::GcolOn => (1, 4),
            Self::Move
            | Self::MoveBy
            | Self::Draw
            | Self::DrawBy
            | Self::Fill
            | Self::FillBy
            | Self::Origin
            | Self::Point
            | Self::PointBy
            | Self::Tint
            | Self::Stereo
            | Self::ColourTint => (2, 2),
            Self::Plot | Self::GcolTint => (2, 3),
            Self::Sound | Self::Ellipse | Self::EllipseFill => (4, 5),
            Self::Envelope => (14, 14),
            Self::Width | Self::Beats | Self::Tempo | Self::Voices => (1, 1),
            Self::Circle | Self::CircleFill => (3, 3),
            Self::Rectangle | Self::RectangleFill => (3, 4),
            Self::RectangleTo | Self::RectangleFillTo => (5, 6),
            Self::Wait => (0, 1),
            Self::Vdu => (0, u32::MAX),
            Self::Line => (4, 4),
        }
    }

    /// Where `TINT` may follow `count` arguments: the tinted statement.
    const fn tinted(self, count: u32) -> Option<Self> {
        match (self, count) {
            (Self::Colour, 1) => Some(Self::ColourTint),
            (Self::Gcol, 1 | 2) => Some(Self::GcolTint),
            _ => None,
        }
    }
}

/// `OS_Byte` 19: the next vertical sync waited for.
const WAIT_FOR_SYNC: u32 = 19;
/// `OS_Word` 8: an envelope defined.
const ENVELOPE: u32 = 8;
/// The most VDU bytes one statement gathers: a containment bound, not a
/// capacity.
const VDU_MOST: usize = 1024;

impl Machine<'_, '_> {
    // PRINT.

    /// `PRINT`, or `PRINT#` to a file.
    pub(crate) fn print_statement(&mut self) -> Step<Next> {
        if self.take(b'#')? {
            return self.evaluate_factor(cont::CHANNEL, Payload::of(&[token::PRINT.into()]));
        }
        self.print_items(Payload::of(&[print::PADDED]))
    }

    /// `PRINT`'s items from the pointer: separators and the special items
    /// taken, an expression evaluated, the line ended as the last separator
    /// says.
    fn print_items(&mut self, payload: Payload) -> Step<Next> {
        let mut flags = payload.word(0);
        loop {
            let b = self.skip_spaces()?;
            match b {
                b':' | b'\r' | token::ELSE => {
                    if flags & print::HELD == 0 {
                        self.newline()?;
                    }
                    return Ok(Next::Done);
                }
                b';' => {
                    self.ptr += 1;
                    flags = (flags & !(print::PADDED | print::HEX)) | print::HELD;
                }
                b',' => {
                    self.ptr += 1;
                    self.print_comma()?;
                    flags = (flags | print::PADDED) & !(print::HEX | print::HELD);
                }
                b'\'' => {
                    self.ptr += 1;
                    self.newline()?;
                    flags &= !print::HELD;
                }
                b'~' => {
                    self.ptr += 1;
                    flags |= print::HEX;
                }
                token::TAB | token::SPC => {
                    self.ptr += 1;
                    let which = if b == token::TAB { 1 } else { 2 };
                    let payload = Payload::of(&[flags & !print::HELD, which]);
                    return self.evaluate(cont::PRINT, payload);
                }
                _ => {
                    let payload = Payload::of(&[flags & !print::HELD, 0]);
                    return self.evaluate(cont::PRINT, payload);
                }
            }
        }
    }

    /// `TAB(x)`: to column `x`, on a new line where the line is past it.
    fn tab_to(&mut self, x: i32) -> Step {
        if self.count()?.cast_signed() > x {
            self.newline()?;
        }
        let count = self.count()?.cast_signed();
        self.output_spaces(usize::try_from(x.saturating_sub(count)).unwrap_or(0))
    }

    /// A comma between items: to the next field of `@%`'s width.
    fn print_comma(&mut self) -> Step {
        let at = self.os.read_word(ws::AT_PERCENT.at())?;
        let width = at & 0xFF;
        if width == 0 {
            return Ok(());
        }
        let count = self.count()?;
        let pad = (width - count % width) % width;
        self.output_spaces(pad as usize)
    }

    /// A `PRINT` item's value written, then the items after it.
    pub(crate) fn print_item(&mut self, payload: Payload, value: &Value) -> Step<Next> {
        let flags = payload.word(0);
        match payload.word(1) {
            1 => {
                let x = self.int_value(value)?;
                if self.take(b',')? {
                    return self
                        .evaluate(cont::PRINT, payload.with(1, 3).with(2, x.cast_unsigned()));
                }
                self.expect(b')', Error::MissingBracket)?;
                self.tab_to(x)?;
            }
            2 => {
                let n = self.int_value(value)?;
                self.output_spaces(usize::try_from(n & 0xFF).unwrap_or(0))?;
            }
            3 => {
                let [x, ..] = payload.word(2).to_le_bytes();
                let y = self.int_value(value)?;
                self.expect(b')', Error::MissingBracket)?;
                self.vdu_codes(&[31, x, y.to_le_bytes()[0]])?;
            }
            _ => match value {
                Value::Str(text) => self.print_text(text)?,
                number => {
                    self.print_number(number, flags & print::PADDED != 0, flags & print::HEX != 0)?;
                }
            },
        }
        let flags = if payload.word(1) == 0 {
            flags & !print::HEX
        } else {
            flags
        };
        self.print_items(Payload::of(&[flags]))
    }

    // INPUT.

    /// `INPUT`: its prompts written and its variables read from the
    /// keyboard, or `INPUT#` from a file.
    pub(crate) fn input_statement(&mut self) -> Step<Next> {
        if self.take(b'#')? {
            return self.evaluate_factor(cont::CHANNEL, Payload::of(&[token::INPUT.into()]));
        }
        let line = self.take(token::LINE)?;
        self.input_items(Payload::of(&[u32::from(line), 1, 0, 0]))
    }

    /// `INPUT`'s items: a string prompt, a separator, a newline, `TAB` or
    /// `SPC`, or a variable read. The payload carries `INPUT LINE`, whether
    /// a `?` is due, and the line a comma left fields of: one past where its
    /// next field starts, nought for none, then its length.
    fn input_items(&mut self, payload: Payload) -> Step<Next> {
        let mut payload = payload;
        loop {
            let b = self.skip_spaces()?;
            match b {
                b':' | b'\r' | token::ELSE => return Ok(Next::Done),
                b'"' => {
                    self.ptr += 1;
                    let text = self.read_string_literal()?;
                    self.print_text(&text)?;
                    payload = payload.with(1, 0);
                }
                b',' | b';' => {
                    self.ptr += 1;
                    payload = payload.with(1, 1);
                }
                b'\'' => {
                    self.ptr += 1;
                    self.newline()?;
                }
                token::TAB | token::SPC => {
                    self.ptr += 1;
                    let which = u32::from(b == token::TAB);
                    let words = [
                        which,
                        payload.word(0),
                        payload.word(1),
                        payload.word(2),
                        payload.word(3),
                    ];
                    return self.evaluate(cont::INPUT_PLACE, Payload::of(&words));
                }
                _ => return self.lvalue(cont::INPUT, payload),
            }
        }
    }

    /// `INPUT`'s `TAB(x)` or `SPC(n)`, as `PRINT` writes them; then its
    /// next item.
    pub(crate) fn input_place(&mut self, payload: Payload, value: &Value) -> Step<Next> {
        let n = self.int_value(value)?;
        self.expect(b')', Error::MissingBracket)?;
        if payload.word(0) == 0 {
            self.output_spaces(usize::from(byte(n)))?;
        } else {
            self.tab_to(n)?;
        }
        self.input_items(Payload::of(&[
            payload.word(1),
            payload.word(2),
            payload.word(3),
            payload.word(4),
        ]))
    }

    /// The string literal after its opening quote.
    fn read_string_literal(&mut self) -> Step<Text> {
        let mut text = Text::EMPTY;
        loop {
            let b = self.next()?;
            match b {
                b'"' if self.peek()? == b'"' => self.ptr += 1,
                b'"' => return Ok(text),
                b'\r' => return Err(Error::MissingQuote.into()),
                _ => {}
            }
            text.push_bytes(&[b]).ok_or(Error::StringTooLong)?;
        }
    }

    /// `INPUT`'s variable read: the next field of the line a comma left, or
    /// a `?` where one is due and a line read from User mode for it.
    pub(crate) fn input_into(&mut self, payload: Payload, lvalue: Lvalue) -> Step<Next> {
        let held = payload.word(2);
        if payload.word(0) == 0 && held != 0 {
            return self.input_field(lvalue, held - 1, payload.word(3), false);
        }
        if payload.word(1) != 0 {
            self.output(b"?")?;
        }
        let place = lvalue.place.call_type(self.dialect());
        self.push(
            Kind::Guest,
            Guest::Input as u8,
            &[lvalue.address, place, payload.word(0)],
        )?;
        self.mode = Mode::Away;
        let buffer = ws::TYPED.at();
        let mut regs = [0; 10];
        regs[..5].copy_from_slice(&[buffer, ws::LINE_ROOM - 1, u32::from(b' '), 0xFF, 0]);
        Ok(Next::Leave(Leave::Swi {
            number: swi::READ_LINE32,
            regs,
            flags: 0,
        }))
    }

    /// A line `INPUT` read: its first field, or all of it for `INPUT LINE`,
    /// to its variable; then the statement's next item.
    pub(crate) fn input_read(&mut self, words: [u32; 3], length: u32, escape: bool) -> Step<Next> {
        if escape {
            return Err(Error::Escape.into());
        }
        // The read echoed the line's end, so the line printed on is new.
        self.os.write_word(ws::TALLY.at(), 0)?;
        let place = Place::of_call_type(words[1], self.dialect()).ok_or(Error::StackCorrupt)?;
        let lvalue = Lvalue {
            address: words[0],
            place,
        };
        self.mode = Mode::Statement;
        self.input_field(lvalue, 0, length, words[2] != 0)
    }

    /// The field from `at` of the `length` bytes `INPUT` read, or the whole
    /// line, into `lvalue`, a number read from it as `VAL` reads one; then
    /// the statement's next item, holding the line where a comma ended the
    /// field.
    fn input_field(
        &mut self,
        lvalue: Lvalue,
        at: u32,
        length: u32,
        whole_line: bool,
    ) -> Step<Next> {
        let mut line = [0; ws::LINE_ROOM as usize];
        let length = (length as usize).min(line.len());
        self.os.read_bytes(ws::TYPED.at(), &mut line[..length])?;
        let at = (at as usize).min(length);
        let (field, next) = if whole_line {
            (Text::of(&line[..length]).ok_or(Error::StringTooLong)?, None)
        } else {
            input_field_of(&line[at..length]).ok_or(Error::StringTooLong)?
        };
        let value = if lvalue.place.is_string() {
            Value::Str(field)
        } else {
            let digits = match self.dialect() {
                crate::value::Dialect::V => crate::five::EXPONENT_DIGITS,
                crate::value::Dialect::VI => crate::six::EXPONENT_DIGITS,
            };
            let written = crate::text::Written::val(field.as_bytes(), digits);
            Value::Real(self.read_real(&written)?)
        };
        self.write(lvalue, &value)?;
        let held = next.map_or(0, |n| len32(at + n) + 1);
        self.input_items(Payload::of(&[
            u32::from(whole_line),
            0,
            held,
            len32(length),
        ]))
    }

    // VDU.

    /// `CLS` or `CLG`, as VDU `code`: `COUNT` back to nought, as recorded.
    pub(crate) fn clear_screen(&mut self, code: u8) -> Step<Next> {
        self.vdu_codes(&[code])?;
        self.os.write_word(ws::TALLY.at(), 0)?;
        Ok(Next::Done)
    }

    /// `VDU`: its values as bytes, a semicolon making the one before it a
    /// word and `|` nine noughts.
    pub(crate) fn vdu_statement(&mut self) -> Step<Next> {
        if self.at_statement_end()? {
            return Ok(Next::Done);
        }
        self.evaluate(cont::GATHER, Payload::of(&[Gathered::Vdu as u32, 0]))
    }

    /// A statement whose arguments are gathered, from its keyword.
    pub(crate) fn gathered_statement(&mut self, b: u8, second: u8) -> Step<Next> {
        use token::statement as s;
        let fill = |m: &mut Self| m.take_two(token::STATEMENT, s::FILL);
        let kind = match (b, second) {
            (token::MODE, _) => Gathered::Mode,
            (token::COLOUR, _) if self.take(token::OF)? => Gathered::ColourOf,
            (token::COLOUR, _) if self.take(token::ON)? => Gathered::ColourOn,
            (token::COLOUR, _) => Gathered::Colour,
            (token::GCOL, _) if self.take(token::OF)? => Gathered::GcolOf,
            (token::GCOL, _) if self.take(token::ON)? => Gathered::GcolOn,
            (token::GCOL, _) => Gathered::Gcol,
            (token::MOVE, _) if self.take(b'B')? && self.take(b'Y')? => Gathered::MoveBy,
            (token::MOVE, _) => Gathered::Move,
            (token::DRAW, _) if self.take(b'B')? && self.take(b'Y')? => Gathered::DrawBy,
            (token::DRAW, _) => Gathered::Draw,
            (token::PLOT, _) => Gathered::Plot,
            (token::SOUND, _) => return self.sound_statement(),
            (token::ENVELOPE, _) => Gathered::Envelope,
            (token::WIDTH, _) => Gathered::Width,
            (token::LINE, _) => Gathered::Line,
            (token::STATEMENT, s::CIRCLE) if fill(self)? => Gathered::CircleFill,
            (token::STATEMENT, s::CIRCLE) => Gathered::Circle,
            (token::STATEMENT, s::ELLIPSE) if fill(self)? => Gathered::EllipseFill,
            (token::STATEMENT, s::ELLIPSE) => Gathered::Ellipse,
            (token::STATEMENT, s::RECTANGLE) if fill(self)? => Gathered::RectangleFill,
            (token::STATEMENT, s::RECTANGLE) => Gathered::Rectangle,
            (token::STATEMENT, s::FILL) if self.take(b'B')? && self.take(b'Y')? => Gathered::FillBy,
            (token::STATEMENT, s::FILL) => Gathered::Fill,
            (token::STATEMENT, s::ORIGIN) => Gathered::Origin,
            (token::STATEMENT, s::POINT) if self.take(b'B')? && self.take(b'Y')? => {
                Gathered::PointBy
            }
            (token::STATEMENT, s::POINT) => Gathered::Point,
            (token::STATEMENT, s::TINT) => Gathered::Tint,
            (token::STATEMENT, s::WAIT) => {
                if self.at_statement_end()? {
                    return self.wait(None).map(|()| Next::Done);
                }
                Gathered::Wait
            }
            (token::STATEMENT, s::BEATS) => Gathered::Beats,
            (token::STATEMENT, s::TEMPO) => Gathered::Tempo,
            (token::STATEMENT, s::VOICES) => Gathered::Voices,
            (token::STATEMENT, s::STEREO) => Gathered::Stereo,
            (token::STATEMENT, s::VOICE) => return self.voice_statement(),
            _ => return Err(Error::Mistake.into()),
        };
        if kind == Gathered::Mode && self.skip_spaces()? == b'"' {
            return self.evaluate(cont::GATHER, Payload::of(&[kind as u32, 0]));
        }
        self.evaluate(cont::GATHER, Payload::of(&[kind as u32, 0]))
    }

    /// A gathered statement's argument: kept, then the next or the
    /// statement done.
    pub(crate) fn gathered(&mut self, payload: Payload, value: &Value) -> Step<Next> {
        let kind = Gathered::of(payload.word(0)).ok_or(Error::StackCorrupt)?;
        let (_, most) = kind.arguments();
        let count = payload
            .word(1)
            .checked_add(1)
            .filter(|&count| count <= most)
            .ok_or(Error::StackCorrupt)?;
        if kind == Gathered::Vdu {
            return self.vdu_item(payload, value);
        }
        if kind == Gathered::Mode && count == 1 && matches!(value, Value::Str(_)) {
            return self.mode_string(string_of(value)?).map(|()| Next::Done);
        }
        self.push_value(value)?;
        let after = if matches!(kind, Gathered::Rectangle | Gathered::RectangleFill)
            && count >= 3
            && self.take(token::TO)?
        {
            let kind = if kind == Gathered::Rectangle {
                Gathered::RectangleTo
            } else {
                Gathered::RectangleFillTo
            };
            return self.evaluate(cont::GATHER, Payload::of(&[kind as u32, count]));
        } else {
            let tint = matches!(kind, Gathered::ColourTint | Gathered::GcolTint);
            count < most && !tint && self.take(b',')?
        };
        if after {
            return self.evaluate(cont::GATHER, payload.with(1, count));
        }
        let (least, _) = kind.arguments();
        if count < least {
            return Err(Error::MissingComma.into());
        }
        if let Some(tinted) = kind.tinted(count) {
            if self.take_two(token::STATEMENT, token::statement::TINT)? {
                return self.evaluate(cont::GATHER, Payload::of(&[tinted as u32, count]));
            }
        }
        self.act(kind, count)?;
        let on = match kind {
            Gathered::ColourOf => Some(Gathered::ColourOn),
            Gathered::GcolOf => Some(Gathered::GcolOn),
            _ => None,
        };
        if let Some(on) = on {
            if self.take(token::ON)? {
                return self.evaluate(cont::GATHER, Payload::of(&[on as u32, 0]));
            }
        }
        Ok(Next::Done)
    }

    /// A gathered statement done with its `count` arguments on the stack.
    #[expect(clippy::too_many_lines, reason = "one arm per statement")]
    fn act(&mut self, kind: Gathered, count: u32) -> Step {
        let mut args = [0_i32; 14];
        let n = count as usize;
        let angle = if matches!(kind, Gathered::Ellipse | Gathered::EllipseFill) && n == 5 {
            Some(self.pop_number()?)
        } else {
            None
        };
        let integers = n - usize::from(angle.is_some());
        for slot in args
            .get_mut(..integers)
            .ok_or(Error::StackCorrupt)?
            .iter_mut()
            .rev()
        {
            *slot = self.pop_int()?;
        }
        if let Some(angle) = angle {
            return self.ellipse(kind == Gathered::EllipseFill, &args[..4], Some(angle));
        }
        let a = args.get(..n).ok_or(Error::StackCorrupt)?;
        match kind {
            Gathered::Mode => match n {
                1 => {
                    let mode = a[0].to_le_bytes()[0];
                    self.vdu_codes(&[22, mode])?;
                    self.os.write_word(ws::TALLY.at(), 0)?;
                }
                _ => self.mode_numbers(a)?,
            },
            Gathered::Colour => match n {
                1 => self.vdu_codes(&[17, byte(a[0])])?,
                2 => self.vdu_codes(&[19, byte(a[0]), byte(a[1]), 0, 0, 0])?,
                3 => self.text_colour_rgb(false, a)?,
                _ => self.vdu_codes(&[19, byte(a[0]), 16, byte(a[1]), byte(a[2]), byte(a[3])])?,
            },
            Gathered::Gcol => match n {
                1 => self.vdu_codes(&[18, 0, byte(a[0])])?,
                2 => self.vdu_codes(&[18, byte(a[0]), byte(a[1])])?,
                _ => self.graphics_colour_rgb(false, a)?,
            },
            Gathered::ColourTint => {
                self.vdu_codes(&[17, byte(a[0])])?;
                self.tint(byte(a[0]) & 0x80 != 0, false, a[1])?;
            }
            Gathered::GcolTint => {
                let (action, colour) = if n == 3 { (a[0], a[1]) } else { (0, a[0]) };
                self.vdu_codes(&[18, byte(action), byte(colour)])?;
                self.tint(byte(colour) & 0x80 != 0, true, a[n - 1])?;
            }
            Gathered::ColourOf | Gathered::ColourOn => {
                let background = kind == Gathered::ColourOn;
                match n {
                    1 => self.set_colour(TEXT | if background { BACKGROUND } else { 0 }, a[0])?,
                    3 => self.text_colour_rgb(background, a)?,
                    _ => return Err(Error::Syntax.into()),
                }
            }
            Gathered::GcolOf | Gathered::GcolOn => {
                let background = if kind == Gathered::GcolOn {
                    BACKGROUND
                } else {
                    0
                };
                match n {
                    1 => self.set_colour(background, a[0])?,
                    2 => self.set_colour(background | (a[0].cast_unsigned() & ACTION), a[1])?,
                    _ => self.graphics_colour_rgb(background != 0, a)?,
                }
            }
            Gathered::Move => self.plot(plot::MOVE, a[0], a[1])?,
            Gathered::MoveBy => self.plot(plot::MOVE_BY, a[0], a[1])?,
            Gathered::Draw => self.plot(plot::DRAW, a[0], a[1])?,
            Gathered::DrawBy => self.plot(plot::DRAW_BY, a[0], a[1])?,
            Gathered::Plot => match n {
                2 => self.plot(plot::POINT, a[0], a[1])?,
                _ => self.plot(a[0], a[1], a[2])?,
            },
            Gathered::Point => self.plot(plot::POINT, a[0], a[1])?,
            Gathered::PointBy => self.plot(plot::POINT_BY, a[0], a[1])?,
            Gathered::Fill => self.plot(plot::FILL, a[0], a[1])?,
            Gathered::FillBy => self.plot(plot::FILL_BY, a[0], a[1])?,
            Gathered::Origin => {
                let (x, y) = (a[0].to_le_bytes(), a[1].to_le_bytes());
                self.vdu_codes(&[29, x[0], x[1], y[0], y[1]])?;
            }
            Gathered::Circle | Gathered::CircleFill => {
                self.plot(plot::MOVE, a[0], a[1])?;
                let code = if kind == Gathered::Circle {
                    plot::CIRCLE
                } else {
                    plot::CIRCLE_FILL
                };
                self.plot(code, a[0].wrapping_add(a[2]), a[1])?;
            }
            Gathered::Ellipse | Gathered::EllipseFill => {
                self.ellipse(kind == Gathered::EllipseFill, a, None)?;
            }
            Gathered::Rectangle | Gathered::RectangleFill => {
                let (x, y) = (a[0], a[1]);
                let (right, top) = (
                    x.wrapping_add(a[2]),
                    y.wrapping_add(if n == 4 { a[3] } else { a[2] }),
                );
                self.plot(plot::MOVE, x, y)?;
                if kind == Gathered::RectangleFill {
                    self.plot(plot::RECTANGLE_FILL, right, top)?;
                } else {
                    for (cx, cy) in [(right, y), (right, top), (x, top), (x, y)] {
                        self.plot(plot::LINE_SHORT, cx, cy)?;
                    }
                }
            }
            Gathered::RectangleTo | Gathered::RectangleFillTo => {
                let (x, y) = (a[0], a[1]);
                let (right, top) = (
                    x.wrapping_add(a[2]),
                    y.wrapping_add(if n == 6 { a[3] } else { a[2] }),
                );
                self.plot(plot::MOVE, x, y)?;
                self.plot(plot::MOVE, right, top)?;
                let code = if kind == Gathered::RectangleTo {
                    plot::BLOCK_COPY
                } else {
                    plot::BLOCK_MOVE
                };
                self.plot(code, a[n - 2], a[n - 1])?;
            }
            Gathered::Tint => {
                self.vdu_codes(&[23, 17, byte(a[0]), byte(a[1]), 0, 0, 0, 0, 0, 0])?;
            }
            Gathered::Wait => self.wait(a.first().copied())?,
            Gathered::Envelope => {
                let block = ws::STRACC.at();
                for (i, &v) in (0..).zip(a) {
                    self.os.write_byte(block + i, byte(v))?;
                }
                let mut regs = Regs::default();
                regs.r[..2].copy_from_slice(&[ENVELOPE, block]);
                self.os.perform(X | swi::WORD, &mut regs)?;
            }
            Gathered::Width => {
                let width = a[0].wrapping_sub(1).cast_unsigned();
                self.os.write_word(ws::WIDTHLOC.at(), width)?;
            }
            Gathered::Beats => self.sound_swi(sound::Q_BEAT, &[a[0]])?,
            Gathered::Tempo => self.sound_swi(sound::Q_TEMPO, &[a[0]])?,
            Gathered::Voices => self.sound_swi(sound::CONFIGURE, &[a[0], 0, 0, 0, 0])?,
            Gathered::Stereo => self.sound_swi(sound::STEREO, &[a[0], a[1]])?,
            Gathered::Sound => {
                let after = if n == 5 { a[4] } else { -1 };
                let _ = after;
                self.sound_swi(sound::CONTROL, &[a[0], a[1], a[2], a[3]])?;
            }
            Gathered::Line => {
                self.plot(plot::MOVE, a[0], a[1])?;
                self.plot(plot::DRAW, a[2], a[3])?;
            }
            Gathered::Vdu => return Err(Error::StackCorrupt.into()),
        }
        Ok(())
    }

    /// `OS_Plot` `code` at `x`, `y`; a code past a byte is passed whole, and
    /// plots nothing, as on RISC OS 5.
    fn plot(&mut self, code: i32, x: i32, y: i32) -> Step {
        let mut regs = Regs::default();
        regs.r[..3].copy_from_slice(&[code.cast_unsigned(), x.cast_unsigned(), y.cast_unsigned()]);
        self.os.perform(X | swi::PLOT, &mut regs)?;
        Ok(())
    }

    /// `ELLIPSE` at its centre with its two semi-axes and perhaps an angle:
    /// the centre, the point level with it on the rim, then the rim's
    /// highest point, each offset truncated as BASIC truncates them.
    fn ellipse(&mut self, fill: bool, a: &[i32], angle: Option<Number>) -> Step {
        let (cx, cy, major, minor) = (a[0], a[1], a[2], a[3]);
        let (across, top_x, top_y) = match angle {
            None => (major, 0, minor),
            Some(angle) => self.rotated_ellipse(major, minor, angle)?,
        };
        self.plot(plot::MOVE, cx, cy)?;
        self.plot(plot::MOVE, cx.wrapping_add(across), cy)?;
        let code = if fill {
            plot::ELLIPSE_FILL
        } else {
            plot::ELLIPSE
        };
        self.plot(code, cx.wrapping_add(top_x), cy.wrapping_add(top_y))
    }

    /// A rotated ellipse's offsets: the half-width across the centre
    /// `a·b/h`, and the highest point `((a² − b²)·sin·cos/h, h)`, where
    /// `h = √(a²·sin² + b²·cos²)`, each in the dialect's arithmetic.
    fn rotated_ellipse(&mut self, major: i32, minor: i32, angle: Number) -> Step<(i32, i32, i32)> {
        let sin = Number::Real(self.arith.function(angle, Five::sin, Arithmetic::sin)?);
        let cos = Number::Real(self.arith.function(angle, Five::cos, Arithmetic::cos)?);
        let (a, b) = (self.real_of(major)?, self.real_of(minor)?);
        let a_sin = self.arith.mul(a, sin)?;
        let b_cos = self.arith.mul(b, cos)?;
        let a_sin_squared = self.arith.mul(a_sin, a_sin)?;
        let b_cos_squared = self.arith.mul(b_cos, b_cos)?;
        let sum = self.arith.add(a_sin_squared, b_cos_squared)?;
        let height = Number::Real(self.arith.function(sum, Five::sqrt, Arithmetic::sqrt)?);
        let product = self.arith.mul(a, b)?;
        let across = self.arith.quotient(product, height)?;
        let a_squared = self.arith.mul(a, a)?;
        let b_squared = self.arith.mul(b, b)?;
        let spread = self.arith.sub(a_squared, b_squared)?;
        let spread_sin = self.arith.mul(spread, sin)?;
        let spread_sin_cos = self.arith.mul(spread_sin, cos)?;
        let top_x = self.arith.quotient(spread_sin_cos, height)?;
        Ok((
            self.arith.int(across)?,
            self.arith.int(top_x)?,
            self.arith.int(height)?,
        ))
    }

    /// `n` as the dialect's real.
    fn real_of(&mut self, n: i32) -> Step<Number> {
        Ok(Number::Real(self.arith.real(n)?))
    }

    /// `COLOUR`'s text colour from a red, green and blue, as RISC OS 5's
    /// `COLOUR r,g,b`, `COLOUR OF r,g,b` and `COLOUR ON r,g,b`: through
    /// `ColourTrans`, with no VDU codes.
    fn text_colour_rgb(&mut self, background: bool, rgb: &[i32]) -> Step {
        let mut regs = Regs::default();
        regs.r[0] = palette_entry(rgb);
        regs.r[3] = if background {
            COLOUR_TRANS_BACKGROUND
        } else {
            0
        };
        self.os
            .perform(X | COLOUR_TRANS_SET_TEXT_COLOUR, &mut regs)?;
        Ok(())
    }

    /// `GCOL`'s graphics colour from a red, green and blue, an action ahead
    /// of them perhaps: through `ColourTrans`, with no VDU codes.
    fn graphics_colour_rgb(&mut self, background: bool, a: &[i32]) -> Step {
        let (action, rgb) = if a.len() == 4 {
            (a[0], &a[1..])
        } else {
            (0, a)
        };
        let mut regs = Regs::default();
        regs.r[0] = palette_entry(rgb);
        regs.r[3] = if background {
            COLOUR_TRANS_BACKGROUND
        } else {
            0
        };
        regs.r[4] = action.cast_unsigned();
        self.os.perform(X | COLOUR_TRANS_SET_GCOL, &mut regs)?;
        Ok(())
    }

    /// `OS_SetColour` with `flags` and colour number `colour`: the `OF` and
    /// `ON` forms given a colour number.
    fn set_colour(&mut self, flags: u32, colour: i32) -> Step {
        let mut regs = Regs::default();
        regs.r[..2].copy_from_slice(&[flags, colour.cast_unsigned()]);
        self.os.perform(X | swi::SET_COLOUR, &mut regs)?;
        Ok(())
    }

    /// `TINT` after `COLOUR` or `GCOL`: `VDU 23,17` for the text or graphics
    /// colour, foreground or background, the colour's top bit choosing.
    fn tint(&mut self, background: bool, graphics: bool, tint: i32) -> Step {
        let which = u8::from(background) | (u8::from(graphics) << 1);
        self.vdu_codes(&[23, 17, which, byte(tint), 0, 0, 0, 0, 0, 0])
    }

    /// `MODE "string"`.
    fn mode_string(&mut self, text: &Text) -> Step {
        let at = ws::STRACC.at();
        self.os.write_bytes(at, text.as_bytes())?;
        self.os.write_byte(at + u32::from(text.len()), 0)?;
        let mut regs = Regs::default();
        regs.r[..2].copy_from_slice(&[15, at]);
        self.os.perform(X | swi::SCREEN_MODE, &mut regs)?;
        let mut select = Regs::default();
        select.r[..2].copy_from_slice(&[0, regs.r[1]]);
        self.os.perform(X | swi::SCREEN_MODE, &mut select)?;
        self.os.write_word(ws::TALLY.at(), 0)?;
        Ok(())
    }

    /// `MODE x, y, depth[, rate]`: a mode selector.
    fn mode_numbers(&mut self, a: &[i32]) -> Step {
        let block = ws::STRACC.at();
        let log2bpp = match a.get(2).copied().unwrap_or(8) {
            1 => 0,
            2 => 1,
            4 => 2,
            8 => 3,
            16 => 4,
            32 => 5,
            _ => return Err(Error::BadDim.into()),
        };
        let words = [
            1,
            a[0].cast_unsigned(),
            a[1].cast_unsigned(),
            log2bpp,
            a.get(3).copied().unwrap_or(-1).cast_unsigned(),
            u32::MAX,
        ];
        for (i, &w) in (0..).zip(&words) {
            self.os.write_word(block + 4 * i, w)?;
        }
        let mut regs = Regs::default();
        regs.r[..2].copy_from_slice(&[0, block]);
        self.os.perform(X | swi::SCREEN_MODE, &mut regs)?;
        self.os.write_word(ws::TALLY.at(), 0)?;
        Ok(())
    }

    /// `WAIT`: the next vertical sync, or `n` centiseconds.
    fn wait(&mut self, n: Option<i32>) -> Step {
        match n {
            None => {
                let mut regs = Regs::default();
                regs.r[0] = WAIT_FOR_SYNC;
                self.os.perform(X | swi::BYTE, &mut regs)?;
            }
            Some(n) => {
                let start = self.clock()?;
                while self.clock()?.wrapping_sub(start) < n {
                    let mut regs = Regs::default();
                    regs.r[0] = WAIT_FOR_SYNC;
                    self.os.perform(X | swi::BYTE, &mut regs)?;
                }
            }
        }
        Ok(())
    }

    /// A sound SWI with `args`.
    fn sound_swi(&mut self, number: u32, args: &[i32]) -> Step {
        let mut regs = Regs::default();
        for (slot, &a) in regs.r.iter_mut().zip(args) {
            *slot = a.cast_unsigned();
        }
        self.os.perform(X | number, &mut regs)?;
        Ok(())
    }

    /// `SOUND`, `SOUND ON` and `SOUND OFF`.
    fn sound_statement(&mut self) -> Step<Next> {
        if self.take(token::ON)? {
            self.sound_swi(sound::ENABLE, &[2])?;
            return Ok(Next::Done);
        }
        if self.take(token::OFF)? {
            self.sound_swi(sound::ENABLE, &[1])?;
            return Ok(Next::Done);
        }
        self.evaluate(cont::GATHER, Payload::of(&[Gathered::Sound as u32, 0]))
    }

    /// `VOICE n, name$`.
    fn voice_statement(&mut self) -> Step<Next> {
        self.evaluate(cont::SETTING, Payload::of(&[setting::VOICE]))
    }

    /// A `VDU` item: a byte, a word after a semicolon, or nine noughts after
    /// `|`, then the next.
    fn vdu_item(&mut self, payload: Payload, value: &Value) -> Step<Next> {
        let n = self.int_value(value)?;
        let bytes = n.to_le_bytes();
        let count = payload.word(1) as usize;
        if count >= VDU_MOST {
            return Err(Error::TooDeep.into());
        }
        if self.take(b';')? {
            self.vdu_codes(&bytes[..2])?;
        } else {
            self.vdu_codes(&bytes[..1])?;
        }
        if self.take(b'|')? {
            self.vdu_codes(&[0; 9])?;
        }
        if self.take(b',')? || !self.at_statement_end()? {
            return self.evaluate(cont::GATHER, payload.with(1, payload.word(1) + 1));
        }
        Ok(Next::Done)
    }

    /// A statement's or command's one number, which `SETTING`'s payload
    /// says the use of.
    pub(crate) fn setting(&mut self, payload: Payload, value: &Value) -> Step<Next> {
        match payload.word(0) {
            setting::VOICE => {
                let channel = self.int_value(value)?;
                self.expect(b',', Error::MissingComma)?;
                self.push(Kind::Int, 0, &[channel.cast_unsigned()])?;
                self.evaluate(cont::SETTING, Payload::of(&[setting::VOICE_NAME]))
            }
            setting::VOICE_NAME => {
                let name = string_of(value)?;
                let channel = self.pop_int()?;
                let at = ws::STRACC.at();
                self.os.write_bytes(at, name.as_bytes())?;
                self.os.write_byte(at + u32::from(name.len()), 0)?;
                self.sound_swi(sound::ATTACH_NAMED_VOICE, &[channel, at.cast_signed()])?;
                Ok(Next::Done)
            }
            which @ (setting::CRUNCH | setting::LISTO) => self.command_setting(which, value),
            _ => Err(Error::StackCorrupt.into()),
        }
    }

    // Files.

    /// A channel's number after `#`: the statement it belongs to goes on.
    pub(crate) fn channel(&mut self, payload: Payload, value: &Value) -> Step<Next> {
        let handle = self.int_value(value)?.cast_unsigned();
        match payload.byte(0) {
            token::PRINT => self.file_items(handle),
            token::INPUT => self.file_inputs(handle),
            token::BPUT => {
                self.expect(b',', Error::MissingComma)?;
                self.evaluate(
                    cont::TO_FILE,
                    Payload::of(&[handle, u32::from(token::BPUT)]),
                )
            }
            token::CLOSE => {
                let mut regs = Regs::default();
                regs.r[..2].copy_from_slice(&[swi::find::CLOSE, handle]);
                self.os.perform(X | swi::FIND, &mut regs)?;
                Ok(Next::Done)
            }
            token::PTR_LHS | token::EXT => {
                self.expect(b'=', Error::MissingEquals)?;
                self.evaluate(cont::TO_FILE, Payload::of(&[handle, payload.word(0)]))
            }
            _ => Err(Error::StackCorrupt.into()),
        }
    }

    /// `PRINT#`'s items, each written in BASIC's file format.
    fn file_items(&mut self, handle: u32) -> Step<Next> {
        if self.take(b',')? || self.take(b';')? {
            return self.evaluate(
                cont::TO_FILE,
                Payload::of(&[handle, u32::from(token::PRINT)]),
            );
        }
        Ok(Next::Done)
    }

    /// A value for a file: `PRINT#`'s encoded, `BPUT#`'s as bytes, or a
    /// file's pointer or extent set.
    pub(crate) fn file_value(&mut self, payload: Payload, value: &Value) -> Step<Next> {
        let handle = payload.word(0);
        match payload.byte(1) {
            token::PRINT => {
                self.print_to_file(handle, value)?;
                self.file_items(handle)
            }
            token::BPUT => {
                match value {
                    Value::Str(text) => {
                        for &b in text.as_bytes() {
                            self.bput(handle, b)?;
                        }
                        if !self.take(b';')? {
                            self.bput(handle, b'\n')?;
                        }
                    }
                    number => {
                        let n = self.int_value(number)?;
                        self.bput(handle, n.to_le_bytes()[0])?;
                    }
                }
                if self.take(b',')? {
                    return self.evaluate(cont::TO_FILE, payload);
                }
                Ok(Next::Done)
            }
            token::PTR_LHS | token::EXT => {
                let n = self.int_value(value)?.cast_unsigned();
                let reason = if payload.byte(1) == token::PTR_LHS {
                    swi::args::WRITE_POINTER
                } else {
                    swi::args::WRITE_EXTENT
                };
                let mut regs = Regs::default();
                regs.r[..3].copy_from_slice(&[reason, handle, n]);
                self.os.perform(X | swi::ARGS, &mut regs)?;
                Ok(Next::Done)
            }
            _ => Err(Error::StackCorrupt.into()),
        }
    }

    /// `byte` to the file `handle`.
    fn bput(&mut self, handle: u32, byte: u8) -> Step {
        let mut regs = Regs::default();
        regs.r[..2].copy_from_slice(&[u32::from(byte), handle]);
        self.os.perform(X | swi::BPUT, &mut regs)?;
        Ok(())
    }

    /// `value` in BASIC's file format, as recorded: an integer `&40` and its
    /// bytes from the top; a real `&80` and BASIC V's five bytes, or `&88`
    /// and BASIC VI's eight, each as memory holds it; a string `&00`, its
    /// length and its bytes last first.
    fn print_to_file(&mut self, handle: u32, value: &Value) -> Step {
        match value {
            Value::Int(n) => {
                self.bput(handle, FILE_INT)?;
                for b in n.to_be_bytes() {
                    self.bput(handle, b)?;
                }
            }
            Value::Real(r) => {
                let bytes = self.arith.encode(*r);
                let (marker, n) = match self.dialect() {
                    crate::value::Dialect::V => (FILE_FIVE, crate::five::BYTES),
                    crate::value::Dialect::VI => (FILE_SIX, crate::six::BYTES),
                };
                self.bput(handle, marker)?;
                for &b in &bytes[..n] {
                    self.bput(handle, b)?;
                }
            }
            Value::Str(text) => {
                self.bput(handle, FILE_STRING)?;
                self.bput(handle, text.len())?;
                for &b in text.as_bytes().iter().rev() {
                    self.bput(handle, b)?;
                }
            }
        }
        Ok(())
    }

    /// `INPUT#`'s variables, each read in BASIC's file format.
    fn file_inputs(&mut self, handle: u32) -> Step<Next> {
        if self.take(b',')? || self.take(b';')? {
            return self.lvalue(cont::FROM_FILE, Payload::of(&[handle]));
        }
        Ok(Next::Done)
    }

    /// A variable `INPUT#` reads, its marker taken as genuine BASIC takes
    /// one: `&00` a string; `&88` BASIC VI's real; any other with its top bit
    /// set BASIC V's; the rest an integer. A string for a number, or a number
    /// for a string, is a type mismatch.
    pub(crate) fn file_variable(&mut self, payload: Payload, lvalue: Lvalue) -> Step<Next> {
        let handle = payload.word(0);
        let marker = self.bget(handle)?;
        let string = lvalue.place.is_string();
        let value = match marker {
            FILE_STRING if string => {
                let length = usize::from(self.bget(handle)?);
                let mut bytes = [0; crate::value::STRING_MOST];
                for slot in bytes[..length].iter_mut().rev() {
                    *slot = self.bget(handle)?;
                }
                Value::Str(Text::of(&bytes[..length]).ok_or(Error::StringTooLong)?)
            }
            FILE_STRING => return Err(Error::NeedNumber.into()),
            _ if string => return Err(Error::NeedString.into()),
            FILE_SIX => {
                let mut bytes = [0; crate::six::BYTES];
                for slot in &mut bytes {
                    *slot = self.bget(handle)?;
                }
                let six = crate::six::Six::from_bytes(bytes);
                Value::Real(match self.dialect() {
                    crate::value::Dialect::V => {
                        crate::value::Arith::of_five(Five::from_double_bits(six.to_bits()))
                    }
                    crate::value::Dialect::VI => crate::value::Arith::of_six(six),
                })
            }
            0x80.. => {
                let mut bytes = [0; crate::five::BYTES];
                for slot in &mut bytes {
                    *slot = self.bget(handle)?;
                }
                let five = Five::from_bytes(bytes);
                Value::Real(match self.dialect() {
                    crate::value::Dialect::V => crate::value::Arith::of_five(five),
                    crate::value::Dialect::VI => crate::value::Arith::of_six(
                        crate::six::Six::from_bits(five.to_double_bits()),
                    ),
                })
            }
            _ => {
                let mut bytes = [0; 4];
                for slot in &mut bytes {
                    *slot = self.bget(handle)?;
                }
                Value::Int(i32::from_be_bytes(bytes))
            }
        };
        self.write(lvalue, &value)?;
        self.file_inputs(handle)
    }

    /// A byte from the file `handle`, "End of file" past its end as
    /// FileSwitch gives it.
    fn bget(&mut self, handle: u32) -> Step<u8> {
        let mut regs = Regs::default();
        regs.r[1] = handle;
        self.os.perform(swi::BGET | X, &mut regs)?;
        if regs.flag(tairix_riscos_arm::psr::C) {
            let mut eof = Regs::default();
            eof.r[..2].copy_from_slice(&[swi::args::AT_END, handle]);
            self.os.perform(X | swi::ARGS, &mut eof)?;
            return Err(Error::OutOfData.into());
        }
        Ok((regs.r[0] & 0xFF) as u8)
    }

    /// `BPUT#`.
    pub(crate) fn bput_statement(&mut self) -> Step<Next> {
        self.expect(b'#', Error::MissingHash)?;
        self.evaluate_factor(cont::CHANNEL, Payload::of(&[token::BPUT.into()]))
    }

    /// `CLOSE#`.
    pub(crate) fn close_statement(&mut self) -> Step<Next> {
        self.expect(b'#', Error::MissingHash)?;
        self.evaluate_factor(cont::CHANNEL, Payload::of(&[token::CLOSE.into()]))
    }

    // Pseudo-variables.

    /// A pseudo-variable set: `PTR#`, `PAGE`, `TIME`, `TIME$`, `LOMEM`,
    /// `HIMEM`.
    pub(crate) fn pseudo_statement(&mut self, b: u8) -> Step<Next> {
        if b == token::PTR_LHS {
            self.expect(b'#', Error::MissingHash)?;
            return self.evaluate_factor(cont::CHANNEL, Payload::of(&[token::PTR_LHS.into()]));
        }
        let string = b == token::TIME_LHS && self.take(b'$')?;
        self.expect(b'=', Error::MissingEquals)?;
        self.evaluate(
            cont::PSEUDO,
            Payload::of(&[u32::from(b), u32::from(string)]),
        )
    }

    /// A pseudo-variable's new value.
    pub(crate) fn pseudo_value(&mut self, payload: Payload, value: &Value) -> Step<Next> {
        match payload.byte(0) {
            token::TIME_LHS if payload.word(1) != 0 => {
                let text = string_of(value)?;
                let at = ws::STRACC.at();
                self.os.write_byte(at, text.len())?;
                self.os.write_bytes(at + 1, text.as_bytes())?;
                let mut regs = Regs::default();
                regs.r[..2].copy_from_slice(&[15, at]);
                self.os.perform(X | swi::WORD, &mut regs)?;
            }
            token::TIME_LHS => {
                let n = self.int_value(value)?;
                let at = ws::STRACC.at();
                self.os.write_word(at, n.cast_unsigned())?;
                self.os.write_byte(at + 4, if n < 0 { 0xFF } else { 0 })?;
                let mut regs = Regs::default();
                regs.r[..2].copy_from_slice(&[2, at]);
                self.os.perform(X | swi::WORD, &mut regs)?;
            }
            token::PAGE_LHS => {
                let n = self.int_value(value)?.cast_unsigned();
                self.set_page(n)?;
            }
            token::LOMEM_LHS => {
                let n = self.int_value(value)?.cast_unsigned();
                self.set_lomem(n)?;
            }
            token::HIMEM_LHS => {
                let n = self.int_value(value)?.cast_unsigned();
                self.set_himem(n)?;
            }
            token::END => {
                let n = self.int_value(value)?.cast_unsigned();
                self.set_end(n)?;
            }
            _ => return Err(Error::StackCorrupt.into()),
        }
        Ok(Next::Done)
    }

    /// `LEFT$(`, `MID$(` and `RIGHT$(` assigned: part of a string variable
    /// overwritten.
    pub(crate) fn slice_statement(&mut self, b: u8) -> Step<Next> {
        self.lvalue(cont::SLICE, Payload::of(&[u32::from(b), 0]))
    }

    /// A string slice's l-value, its numbers and its new text, in turn.
    pub(crate) fn slice_argument(&mut self, payload: Payload, delivered: &Delivered) -> Step<Next> {
        let which = u8::try_from(payload.word(0)).map_err(|_| Error::StackCorrupt)?;
        match (payload.word(1), delivered) {
            (0, Delivered::Lvalue(lvalue)) => {
                if !lvalue.place.is_string() {
                    return Err(Error::NeedStringVariable.into());
                }
                let place = lvalue.place.call_type(self.dialect());
                let payload = Payload::of(&[u32::from(which), 1, lvalue.address, place]);
                if self.take(b',')? {
                    return self.evaluate(cont::SLICE, payload);
                }
                self.expect(b')', Error::MissingBracket)?;
                self.expect(b'=', Error::MissingEquals)?;
                self.evaluate(
                    cont::SLICE,
                    payload.with(1, 4).with(4, u32::MAX).with(5, u32::MAX),
                )
            }
            (1, Delivered::Value(value)) => {
                let n = self.int_value(value)?.cast_unsigned();
                let payload = payload.with(4, n);
                if which == token::MID && self.take(b',')? {
                    return self.evaluate(cont::SLICE, payload.with(1, 2));
                }
                self.expect(b')', Error::MissingBracket)?;
                self.expect(b'=', Error::MissingEquals)?;
                self.evaluate(cont::SLICE, payload.with(1, 4).with(5, u32::MAX))
            }
            (2, Delivered::Value(value)) => {
                let n = self.int_value(value)?.cast_unsigned();
                self.expect(b')', Error::MissingBracket)?;
                self.expect(b'=', Error::MissingEquals)?;
                self.evaluate(cont::SLICE, payload.with(1, 4).with(5, n))
            }
            (4, Delivered::Value(value)) => {
                let new = string_of(value)?;
                let place = Place::of_call_type(payload.word(3), self.dialect())
                    .ok_or(Error::StackCorrupt)?;
                let lvalue = Lvalue {
                    address: payload.word(2),
                    place,
                };
                let current = self.read(lvalue)?;
                let result = splice(
                    which,
                    string_of(&current)?,
                    new,
                    payload.word(4),
                    payload.word(5),
                );
                self.write(lvalue, &Value::Str(result))?;
                Ok(Next::Done)
            }
            _ => Err(Error::StackCorrupt.into()),
        }
    }
}

/// `new` written over part of `old` as `LEFT$(`, `MID$(` or `RIGHT$(`
/// assigned does: never past `old`'s end, nor past `count` bytes of `new`.
fn splice(which: u8, old: &Text, new: &Text, n: u32, count: u32) -> Text {
    let old = old.as_bytes();
    let new = new.as_bytes();
    let mut out = *Text::of(old).as_ref().unwrap_or(&Text::EMPTY);
    let mut bytes = [0; crate::value::STRING_MOST];
    bytes[..old.len()].copy_from_slice(old);
    let length = old.len();
    let (start, most) = match which {
        token::LEFT => (0, if n == u32::MAX { new.len() } else { n as usize }),
        token::RIGHT => {
            let take = if n == u32::MAX { new.len() } else { n as usize };
            let take = take.min(new.len()).min(length);
            (length - take, take)
        }
        _ => {
            let start = (n.max(1) - 1) as usize;
            (
                start,
                if count == u32::MAX {
                    new.len()
                } else {
                    count as usize
                },
            )
        }
    };
    if start >= length {
        return out;
    }
    let take = most.min(new.len()).min(length - start);
    bytes[start..start + take].copy_from_slice(&new[..take]);
    out = Text::of(&bytes[..length]).unwrap_or(Text::EMPTY);
    out
}

/// The field an `INPUT` variable takes from `rest`, split as RISC OS 5's
/// BASIC splits a line: leading spaces skipped; a quoted field unquoted, a
/// doubled quote one quote and what follows the closing quote dropped; any
/// other to the next comma, trailing spaces kept. With it, where the field
/// after it starts, where a comma ended it. An unclosed quote runs to the
/// line's end, where genuine BASIC never returns.
fn input_field_of(rest: &[u8]) -> Option<(Text, Option<usize>)> {
    let mut at = rest.iter().take_while(|&&b| b == b' ').count();
    let mut field = Text::EMPTY;
    if rest.get(at) == Some(&b'"') {
        at += 1;
        while let Some(&b) = rest.get(at) {
            at += 1;
            if b == b'"' {
                if rest.get(at) != Some(&b'"') {
                    break;
                }
                at += 1;
            }
            field.push_bytes(&[b])?;
        }
        at += rest[at..].iter().take_while(|&&b| b != b',').count();
    } else {
        let end = at + rest[at..].iter().take_while(|&&b| b != b',').count();
        field.push_bytes(&rest[at..end])?;
        at = end;
    }
    Some((field, (at < rest.len()).then_some(at + 1)))
}

/// `n`'s low byte.
const fn byte(n: i32) -> u8 {
    n.to_le_bytes()[0]
}

/// `ColourTrans`'s palette entry, `&BBGGRR00`, for a red, green and blue.
fn palette_entry(rgb: &[i32]) -> u32 {
    u32::from_le_bytes([0, byte(rgb[0]), byte(rgb[1]), byte(rgb[2])])
}

/// The plot codes BASIC's graphics statements give `OS_Plot`, as recorded.
mod plot {
    use tairix_vdu::plot as vdu;

    const fn code(bits: u8) -> i32 {
        i32::from_le_bytes([bits, 0, 0, 0])
    }

    /// A move, relative and absolute.
    pub(super) const MOVE_BY: i32 = code(vdu::LINE);
    pub(super) const MOVE: i32 = code(vdu::LINE | vdu::ABSOLUTE);
    /// A line in the foreground colour, relative and absolute.
    pub(super) const DRAW_BY: i32 = code(vdu::LINE | vdu::FOREGROUND);
    pub(super) const DRAW: i32 = code(vdu::LINE | vdu::ABSOLUTE | vdu::FOREGROUND);
    /// An absolute line drawn without its last point: `RECTANGLE`'s sides.
    pub(super) const LINE_SHORT: i32 =
        code(vdu::LINE_WITHOUT_LAST | vdu::ABSOLUTE | vdu::FOREGROUND);
    /// A point, relative and absolute.
    pub(super) const POINT_BY: i32 = code(vdu::POINT | vdu::FOREGROUND);
    pub(super) const POINT: i32 = code(vdu::POINT | vdu::ABSOLUTE | vdu::FOREGROUND);
    /// A rectangle filled to the absolute point.
    pub(super) const RECTANGLE_FILL: i32 = code(vdu::RECTANGLE | vdu::ABSOLUTE | vdu::FOREGROUND);
    /// A flood fill, relative and absolute.
    pub(super) const FILL_BY: i32 = code(vdu::FLOOD | vdu::FOREGROUND);
    pub(super) const FILL: i32 = code(vdu::FLOOD | vdu::ABSOLUTE | vdu::FOREGROUND);
    /// A circle's outline and its fill, through the absolute point.
    pub(super) const CIRCLE: i32 = code(vdu::CIRCLE | vdu::ABSOLUTE | vdu::FOREGROUND);
    pub(super) const CIRCLE_FILL: i32 = code(vdu::CIRCLE_FILL | vdu::ABSOLUTE | vdu::FOREGROUND);
    /// The rectangle a move and a move bound moved, or copied, there.
    pub(super) const BLOCK_MOVE: i32 = code(vdu::BLOCK | vdu::ABSOLUTE | vdu::BLOCK_MOVE);
    pub(super) const BLOCK_COPY: i32 = code(vdu::BLOCK | vdu::ABSOLUTE | vdu::BLOCK_COPY);
    /// An ellipse's outline and its fill, through its highest point.
    pub(super) const ELLIPSE: i32 = code(vdu::ELLIPSE | vdu::ABSOLUTE | vdu::FOREGROUND);
    pub(super) const ELLIPSE_FILL: i32 = code(vdu::ELLIPSE_FILL | vdu::ABSOLUTE | vdu::FOREGROUND);
}

/// `ColourTrans_SetGCOL` and `ColourTrans_SetTextColour`, and their flag
/// for the background.
const COLOUR_TRANS_SET_GCOL: u32 = 0x4_0743;
const COLOUR_TRANS_SET_TEXT_COLOUR: u32 = 0x4_0761;
const COLOUR_TRANS_BACKGROUND: u32 = 1 << 7;
/// `PRINT#`'s markers: an integer, BASIC V's real, BASIC VI's real and a
/// string.
const FILE_INT: u8 = 0x40;
const FILE_FIVE: u8 = 0x80;
const FILE_SIX: u8 = 0x88;
const FILE_STRING: u8 = 0x00;

#[cfg(test)]
mod tests {
    use alloc::string::String;
    use alloc::vec::Vec;

    use super::{input_field_of, palette_entry, splice};
    use crate::token;
    use crate::value::Text;

    fn text(s: &str) -> Text {
        Text::of(s.as_bytes()).expect("short")
    }

    /// Each field `INPUT` takes from `line` in turn, as text.
    fn fields(line: &str) -> Vec<String> {
        let bytes = line.as_bytes();
        let mut at = 0;
        let mut out = Vec::new();
        loop {
            let (field, next) = input_field_of(&bytes[at..]).expect("room");
            out.push(String::from_utf8_lossy(field.as_bytes()).into_owned());
            match next {
                Some(n) => at += n,
                None => return out,
            }
        }
    }

    #[test]
    fn input_splits_a_line_as_recorded() {
        assert_eq!(fields("  lead,trail  "), ["lead", "trail  "]);
        assert_eq!(fields("1,,3"), ["1", "", "3"]);
        assert_eq!(fields("\"quoted, text\""), ["quoted, text"]);
        assert_eq!(fields("\"a,b\",c"), ["a,b", "c"]);
        assert_eq!(fields("\"x\"y,z"), ["x", "z"]);
        assert_eq!(fields("  \"q\"\"r\""), ["q\"r"]);
        assert_eq!(fields("x\"y"), ["x\"y"]);
        assert_eq!(fields("\"\""), [""]);
        assert_eq!(
            fields("1,"),
            ["1", ""],
            "a trailing comma leaves an empty field"
        );
        assert_eq!(fields(""), [""]);
        assert_eq!(
            fields("\"open"),
            ["open"],
            "an unclosed quote runs to the end"
        );
    }

    #[test]
    fn a_palette_entry_is_blue_green_red_and_a_nought() {
        assert_eq!(palette_entry(&[0x12, 0x34, 0x56]), 0x5634_1200);
        assert_eq!(
            palette_entry(&[256 + 1, -1, 0]),
            0x00FF_0100,
            "each its low byte"
        );
    }

    #[test]
    fn a_slice_overwrites_within_the_old_string() {
        assert_eq!(
            splice(token::LEFT, &text("hello"), &text("HE"), u32::MAX, 0).as_bytes(),
            b"HEllo"
        );
        assert_eq!(
            splice(token::LEFT, &text("hello"), &text("HEY"), 2, 0).as_bytes(),
            b"HEllo"
        );
        assert_eq!(
            splice(token::RIGHT, &text("hello"), &text("LO"), u32::MAX, 0).as_bytes(),
            b"helLO"
        );
        assert_eq!(
            splice(token::MID, &text("hello"), &text("XYZ"), 2, 2).as_bytes(),
            b"hXYlo"
        );
        assert_eq!(
            splice(token::MID, &text("hello"), &text("XYZW"), 4, u32::MAX).as_bytes(),
            b"helXY"
        );
        assert_eq!(
            splice(token::MID, &text("hi"), &text("X"), 5, u32::MAX).as_bytes(),
            b"hi"
        );
    }
}
