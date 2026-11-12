//! What `FDISK` reads as it is answered, as DOS 4.0's reads it: each key
//! taken through function `0Ch`'s flush and function `08h`, put into the
//! country's upper case, and echoed into the answer's field, where the
//! cursor stands. Esc, or an input with no key left to give, puts the
//! question aside.

use super::at;
use super::screen::{Screen, NO, YES};
use super::table::LABEL_LEN;
use super::text;
use super::views::{number, share};
use crate::dos::char::{scan, BACKSPACE, CR, ESC};
use crate::dos::country::TERMINATORS;
use crate::dos::program::{Flow, Frame, Id, Int, Key, Program, Returned, Routine, ENDED};
use crate::frames::{locals, take, Outcome};

/// What an answer comes back as besides itself.
pub mod answer {
    /// Esc pressed, or no key left: the question put aside.
    pub const ESCAPED: u32 = 0x2_0000;
    /// A size given as a share, not in megabytes.
    pub const SHARE: u32 = 0x1_0000;
    /// The size itself.
    pub const VALUE: u32 = 0xFFFF;
}

/// The call placing the cursor `offset` cells into the answer's field.
fn place_field(p: &mut Program<'_, '_>, offset: u8) -> Frame {
    let (row, column) = (p.get8(at::INPUT), p.get8(at::INPUT + 1));
    place(p, row, column.saturating_add(offset))
}

/// The call placing the cursor at `row`, `column`.
pub fn place(p: &Program<'_, '_>, row: u8, column: u8) -> Frame {
    Frame::of(&Int {
        dx: u16::from_le_bytes([column, row]),
        ..Int::bios(p.own, 0x10, 0x0200)
    })
}

/// The call writing `byte` where the cursor is, the cell's attribute kept,
/// as the C runtime's `putch` leaves it.
fn echo(p: &Program<'_, '_>, byte: u8) -> Frame {
    Frame::of(&Int {
        cx: 1,
        ..Int::bios(p.own, 0x10, 0x0A00 | u16::from(byte))
    })
}

/// The call sounding the bell.
fn beep(p: &Program<'_, '_>) -> Frame {
    Frame::of(&Int::bios(p.own, 0x10, 0x0E07))
}

/// The call upper-casing `byte` as the country does.
fn upper(p: &Program<'_, '_>, byte: u8) -> Frame {
    Frame::of(&Int {
        dx: u16::from(byte),
        ..Int::dos(p.own, 0x6520)
    })
}

/// The key the read `read` gave, or none for Esc or an input with none
/// left.
fn key_of(read: u32) -> Option<u8> {
    match read {
        ENDED => None,
        read => Some(read.to_le_bytes()[0]).filter(|&key| key != ESC),
    }
}

/// The call drawing `message` with `inserts`, below the question.
fn say(p: &mut Program<'_, '_>, message: &str, inserts: &[u8]) -> Frame {
    let mut screen = Screen::new();
    screen.draw(message, inserts);
    super::show(p, &screen)
}

/// What a one-key answer must be.
mod ask {
    pub const CHOICE: u8 = 0;
    pub const YES_NO: u8 = 1;
    pub const LETTER: u8 = 2;
}

mod answering {
    pub const DEFAULT_PLACED: u8 = 1;
    pub const PLACED: u8 = 2;
    pub const READ: u8 = 3;
    pub const UPPERED: u8 = 4;
    pub const SHOWN: u8 = 5;
    pub const BEEPED: u8 = 6;
    pub const CHECKED: u8 = 7;
}

locals! {
    /// A one-key answer typed into the field, as Enter takes it: the key
    /// typed last, or `default` before any is. It is asked again, saying
    /// what it must be, until it is an answer, or [`answer::ESCAPED`].
    pub struct FdiskAnswer {
        ask: u8,
        least: u8,
        most: u8,
        said_least: u8,
        said_most: u8,
        default: u8,
        typed: u8,
        typed_any: bool,
    }
}

impl FdiskAnswer {
    /// A digit from `1` to `most`, `default` the one Enter takes before a
    /// key is typed, none where it is zero: the digit.
    #[must_use]
    pub const fn choice(default: u8, most: u8) -> Self {
        Self::of(
            ask::CHOICE,
            b'1',
            b'0' + most,
            if default == 0 { 0 } else { b'0' + default },
        )
    }

    /// A yes or a no, `default` the key Enter takes before one is typed:
    /// `1` for yes and `0` for no.
    #[must_use]
    pub const fn yes_no(default: u8) -> Self {
        Self::of(ask::YES_NO, 0, 0, default)
    }

    /// A letter from `least` to `most`, the error naming `said_least` to
    /// `said_most`, the letters still there: the letter.
    #[must_use]
    pub const fn letter(least: u8, most: u8, said_least: u8, said_most: u8) -> Self {
        Self {
            said_least,
            said_most,
            ..Self::of(ask::LETTER, least, most, 0)
        }
    }

    const fn of(ask: u8, least: u8, most: u8, default: u8) -> Self {
        Self {
            ask,
            least,
            most,
            said_least: least,
            said_most: most,
            default,
            typed: 0,
            typed_any: false,
        }
    }

    /// Enter pressed: the answer, or what it must be said.
    fn entered(self, p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        let value = match (self.typed_any, self.default) {
            (true, _) => self.typed,
            (false, 0) if self.ask == ask::CHOICE => b' ',
            (false, default) => default,
        };
        if self.ask == ask::YES_NO {
            *phase = answering::CHECKED;
            return Flow::Call(Frame::of(&Int {
                dx: u16::from(value),
                ..Int::dos(p.own, 0x6523)
            }));
        }
        let fits = if self.ask == ask::CHOICE {
            value.is_ascii_digit()
        } else {
            value.is_ascii_alphabetic()
        };
        if fits && (self.least..=self.most).contains(&value) {
            return Flow::Return(Ok(u32::from(value)));
        }
        *phase = answering::SHOWN;
        Flow::Call(if fits {
            say(
                p,
                text::NOT_A_CHOICE,
                &[value, self.said_least, b'-', self.said_most],
            )
        } else {
            say(
                p,
                text::INVALID_ENTRY,
                &[self.said_least, b'-', self.said_most],
            )
        })
    }
}

impl Routine for FdiskAnswer {
    const ID: Id = Id::FdiskAnswer;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        match *phase {
            answering::DEFAULT_PLACED => {
                take!(child);
                *phase = answering::SHOWN;
                Flow::Call(echo(p, self.default))
            }
            answering::PLACED => {
                take!(child);
                *phase = answering::READ;
                Flow::Call(Frame::of(&Key::of(0x0C08)))
            }
            answering::READ => match key_of(take!(child)) {
                None => Flow::Return(Ok(answer::ESCAPED)),
                Some(0) => {
                    *phase = answering::BEEPED;
                    Flow::Call(beep(p))
                }
                Some(key) => {
                    *phase = answering::UPPERED;
                    Flow::Call(upper(p, key))
                }
            },
            answering::UPPERED | answering::BEEPED => {
                let beeped = *phase == answering::BEEPED;
                take!(child);
                let key = if beeped {
                    0
                } else {
                    p.result(Returned::Dx).to_le_bytes()[0]
                };
                if key == CR {
                    return self.entered(p, phase);
                }
                self.typed = key;
                self.typed_any = true;
                *phase = answering::SHOWN;
                Flow::Call(echo(p, key))
            }
            answering::CHECKED => {
                if let Ok(given @ (0 | 1)) = child {
                    return Flow::Return(Ok(given));
                }
                *phase = answering::SHOWN;
                Flow::Call(say(p, text::INVALID_ENTRY, &[YES, b'-', NO]))
            }
            answering::SHOWN => {
                take!(child);
                *phase = answering::PLACED;
                Flow::Call(place_field(p, 0))
            }
            _ => {
                *phase = if self.default == 0 {
                    answering::PLACED
                } else {
                    answering::DEFAULT_PLACED
                };
                Flow::Call(place_field(p, 0))
            }
        }
    }
}

/// The questions a size is asked by.
pub mod prompt {
    pub const PRIMARY: u8 = 0;
    pub const EXTENDED: u8 = 1;
    pub const LOGICAL: u8 = 2;
}

/// The question `prompt` names, and the error a size past its most is.
fn prompt_of(prompt: u8) -> (&'static str, &'static str) {
    match prompt {
        prompt::PRIMARY => (text::PRIMARY_SIZE, text::PARTITION_TOO_LARGE),
        prompt::EXTENDED => (text::EXTENDED_SIZE, text::PARTITION_TOO_LARGE),
        _ => (text::LOGICAL_SIZE, text::LOGICAL_TOO_LARGE),
    }
}

/// The question `prompt` drawn into `screen`, the size shown `value`, as a
/// share with `percent`.
pub fn size_question(screen: &mut Screen, prompt: u8, value: u16, percent: bool) {
    let shown = if percent { share(value) } else { number(value) };
    screen.draw(prompt_of(prompt).0, &shown);
}

locals! {
    /// A size typed into the question `prompt` asks, in megabytes up to
    /// `most_mbytes` or, after `%`, a share up to `most_share`; `default`
    /// the one Enter takes before a key is typed, a share with `percent`.
    /// Digits build the size, four at most, `%` makes it a share and
    /// Backspace takes a digit or the `%` off. Returns the size with
    /// [`answer::SHARE`] set for a share, or [`answer::ESCAPED`].
    pub struct FdiskSize {
        default: u16,
        most_mbytes: u16,
        most_share: u16,
        value: u16,
        prompt: u8,
        percent: bool,
        typed_any: bool,
    }
}

impl FdiskSize {
    #[must_use]
    pub const fn of(
        prompt: u8,
        default: u16,
        percent: bool,
        most_mbytes: u16,
        most_share: u16,
    ) -> Self {
        Self {
            default,
            most_mbytes,
            most_share,
            value: 0,
            prompt,
            percent,
            typed_any: false,
        }
    }

    /// The size Enter gives, or the error to say.
    fn entered(&mut self) -> Result<u32, &'static str> {
        if !self.typed_any {
            self.value = self.default;
        }
        let most = if self.percent {
            self.most_share
        } else {
            self.most_mbytes
        };
        match self.value {
            0 => Err(text::ZERO_SIZE),
            value if value > most => Err(prompt_of(self.prompt).1),
            value => Ok(u32::from(value) | if self.percent { answer::SHARE } else { 0 }),
        }
    }

    fn asked_again(&self, p: &mut Program<'_, '_>) -> Frame {
        let mut screen = Screen::new();
        size_question(&mut screen, self.prompt, self.value, self.percent);
        super::show(p, &screen)
    }
}

mod sizing {
    pub const PLACED: u8 = 1;
    pub const READ: u8 = 2;
    pub const SHOWN: u8 = 3;
}

impl Routine for FdiskSize {
    const ID: Id = Id::FdiskSize;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        match *phase {
            sizing::PLACED => {
                take!(child);
                *phase = sizing::READ;
                Flow::Call(Frame::of(&Key::of(0x0C08)))
            }
            sizing::READ => {
                let Some(key) = key_of(take!(child)) else {
                    return Flow::Return(Ok(answer::ESCAPED));
                };
                *phase = sizing::SHOWN;
                match key {
                    CR => match self.entered() {
                        Ok(size) => Flow::Return(Ok(size)),
                        Err(error) => Flow::Call(say(p, error, &[])),
                    },
                    BACKSPACE => {
                        if self.percent {
                            self.percent = false;
                        } else {
                            self.value /= 10;
                        }
                        self.typed_any = true;
                        Flow::Call(self.asked_again(p))
                    }
                    b'%' if !self.percent => {
                        self.percent = true;
                        self.value %= 1000;
                        self.typed_any = true;
                        Flow::Call(self.asked_again(p))
                    }
                    digit @ b'0'..=b'9' if !self.percent || !self.typed_any => {
                        self.value = (self.value % 1000) * 10 + u16::from(digit - b'0');
                        self.typed_any = true;
                        self.percent = false;
                        Flow::Call(self.asked_again(p))
                    }
                    _ if self.percent => Flow::Call(say(p, text::PRESS_ENTER, &[])),
                    _ => Flow::Call(say(p, text::INVALID_ENTRY, b"0-9")),
                }
            }
            _ => {
                if *phase == sizing::SHOWN {
                    take!(child);
                }
                *phase = sizing::PLACED;
                Flow::Call(place_field(p, 0))
            }
        }
    }
}

/// The longest label typed, as the cursor counts it.
#[allow(clippy::cast_possible_truncation, reason = "a label's eleven bytes")]
const LONGEST: u8 = LABEL_LEN as u8;

locals! {
    /// A label typed at [`at::TYPED`], NUL-ended, eleven characters at
    /// most, as DOS 4.0's field takes one: typing at the cursor ends the
    /// label there, Backspace takes the character before it out, and Home,
    /// End, ← and → move it. What the field shows is always the label.
    /// Returns `0`, or [`answer::ESCAPED`].
    pub struct FdiskLabel {
        cursor: u8,
        drawing: u8,
    }
}

mod labelling {
    pub const KEYED: u8 = 1;
    pub const UPPERED: u8 = 2;
    pub const SCANNED: u8 = 3;
    pub const DRAW_PLACED: u8 = 4;
    pub const DRAWN: u8 = 5;
    pub const READY: u8 = 6;
    pub const FIRST_READY: u8 = 7;
}

impl FdiskLabel {
    fn label(p: &mut Program<'_, '_>) -> [u8; LABEL_LEN + 1] {
        let mut label = [0u8; LABEL_LEN + 1];
        p.g.read_bytes(p.own, at::TYPED, &mut label);
        label[LABEL_LEN] = 0;
        label
    }

    fn len(p: &mut Program<'_, '_>) -> u8 {
        let label = Self::label(p);
        u8::try_from(label.iter().position(|&byte| byte == 0).unwrap_or(0)).unwrap_or(0)
    }

    /// The field redrawn from cell `from` to its end, then the next key
    /// read.
    fn redraw(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, from: u8) -> Flow {
        self.drawing = from;
        self.draw(p, phase)
    }

    fn draw(&mut self, p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        if self.drawing >= LONGEST {
            return self.ready(p, phase);
        }
        *phase = labelling::DRAW_PLACED;
        Flow::Call(place_field(p, self.drawing))
    }

    /// The cursor put where typing goes on, and the next key read.
    fn ready(&mut self, p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        *phase = labelling::READY;
        Flow::Call(place_field(p, self.cursor))
    }

    fn refused(p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        *phase = labelling::READY;
        Flow::Call(beep(p))
    }

    fn escaped(p: &mut Program<'_, '_>) -> Flow {
        p.g.write_bytes(p.own, at::TYPED, &[ESC, 0]);
        Flow::Return(Ok(answer::ESCAPED))
    }
}

impl Routine for FdiskLabel {
    const ID: Id = Id::FdiskLabel;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        match *phase {
            labelling::KEYED => {
                let Some(key) = key_of(take!(child)) else {
                    return Self::escaped(p);
                };
                match key {
                    CR => Flow::Return(Ok(0)),
                    BACKSPACE if self.cursor > 0 => {
                        let mut label = Self::label(p);
                        let at = usize::from(self.cursor);
                        label.copy_within(at.., at - 1);
                        label[LABEL_LEN] = 0;
                        p.g.write_bytes(p.own, at::TYPED, &label);
                        self.cursor -= 1;
                        self.redraw(p, phase, self.cursor)
                    }
                    0 => {
                        *phase = labelling::SCANNED;
                        Flow::Call(Frame::of(&Key::of(0x0800)))
                    }
                    key if key < 0x20 => Self::refused(p, phase),
                    key => {
                        *phase = labelling::UPPERED;
                        Flow::Call(upper(p, key))
                    }
                }
            }
            labelling::SCANNED => {
                let Some(scan) = key_of(take!(child)) else {
                    return Self::escaped(p);
                };
                let len = Self::len(p);
                match scan {
                    scan::HOME => self.cursor = 0,
                    scan::END => self.cursor = len,
                    scan::LEFT if self.cursor > 0 => self.cursor -= 1,
                    scan::RIGHT if self.cursor < len => self.cursor += 1,
                    _ => return Self::refused(p, phase),
                }
                self.ready(p, phase)
            }
            labelling::UPPERED => {
                take!(child);
                let key = p.result(Returned::Dx).to_le_bytes()[0];
                if TERMINATORS.contains(&key) || self.cursor >= LONGEST {
                    return Self::refused(p, phase);
                }
                let mut label = Self::label(p);
                let at = usize::from(self.cursor);
                label[at] = key;
                label[at + 1..].fill(0);
                p.g.write_bytes(p.own, at::TYPED, &label);
                let from = self.cursor;
                if self.cursor < LONGEST - 1 {
                    self.cursor += 1;
                }
                self.redraw(p, phase, from)
            }
            labelling::DRAW_PLACED => {
                take!(child);
                let byte = Self::label(p)[usize::from(self.drawing)];
                *phase = labelling::DRAWN;
                Flow::Call(echo(p, if byte == 0 { b' ' } else { byte }))
            }
            labelling::DRAWN => {
                take!(child);
                self.drawing += 1;
                self.draw(p, phase)
            }
            labelling::READY | labelling::FIRST_READY => {
                let first = *phase == labelling::FIRST_READY;
                take!(child);
                *phase = labelling::KEYED;
                Flow::Call(Frame::of(&Key::of(if first { 0x0C08 } else { 0x0800 })))
            }
            _ => {
                p.g.write_bytes(p.own, at::TYPED, &[0; LABEL_LEN + 1]);
                *phase = labelling::FIRST_READY;
                Flow::Call(place_field(p, 0))
            }
        }
    }
}

/// Where the cursor waits for Esc.
const WAITING: (u8, u8) = (24, 39);

locals! {
    /// Keys read until Esc, or until the input has none left.
    pub struct FdiskWait {}
}

mod waiting {
    pub const PLACED: u8 = 1;
    pub const READ: u8 = 2;
}

impl Routine for FdiskWait {
    const ID: Id = Id::FdiskWait;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        match *phase {
            waiting::PLACED => {
                take!(child);
                *phase = waiting::READ;
                Flow::Call(Frame::of(&Key::of(0x0C08)))
            }
            waiting::READ => {
                if key_of(take!(child)).is_none() {
                    return Flow::Return(Ok(answer::ESCAPED));
                }
                *phase = waiting::PLACED;
                Flow::Call(place(p, WAITING.0, WAITING.1))
            }
            _ => {
                *phase = waiting::PLACED;
                Flow::Call(place(p, WAITING.0, WAITING.1))
            }
        }
    }
}
