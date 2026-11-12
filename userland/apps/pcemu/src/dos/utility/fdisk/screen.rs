//! The screen `FDISK` draws, as DOS 4.0's draws it. A message is text and
//! codes: `^rrcc^` moves down `rr` rows from where the message began and to
//! column `cc`; between `<` and `>`, `H` and `R` choose the highlighted or
//! the regular attribute, `B` and `O` turn blinking on and off, `I` draws
//! the next of the message's inserts, `Y` and `N` the keys that answer yes
//! and no, `S` marks where the answer is typed, `C` clears the line from
//! there, and `W` sounds the bell. Anything else is drawn where the last
//! character went. A screen is the BIOS calls its messages make, which
//! [`FdiskShow`] takes through INT 10h — the cursor placed and characters
//! written with their attribute, as a program on the XT's BIOS must, a run
//! of one character a call.

use crate::dos::program::{Flow, Frame, Id, Int, Program, Routine};
use crate::frames::{locals, take, Outcome};
use alloc::vec::Vec;

/// The attributes text is drawn in.
pub mod attr {
    pub const REGULAR: u8 = 0x07;
    pub const HIGH: u8 = 0x0F;
    pub const BLINK: u8 = 0x80;
}

/// The keys that answer yes and no.
pub const YES: u8 = b'Y';
pub const NO: u8 = b'N';
/// The screen's last row and column.
pub const LAST_ROW: u8 = 24;
pub const LAST_COLUMN: u8 = 79;

/// The calls a screen is drawn with, as their bytes lie in memory.
mod op {
    pub const END: u8 = 0;
    /// Its row, column, attribute and length, then its characters.
    pub const TEXT: u8 = 1;
    pub const TEXT_HEADER: usize = 5;
    /// Its top, left, bottom and right.
    pub const CLEAR: u8 = 2;
    pub const CLEAR_LEN: usize = 5;
    pub const BEEP: u8 = 3;
}

/// A screen being drawn: the calls that draw it, and where an answer is
/// typed, if a message marked one.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Screen {
    ops: Vec<u8>,
    /// The start of the last call, while it is text that may run on.
    last_text: Option<usize>,
    pub input: Option<(u8, u8)>,
}

impl Screen {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Rows `top` to `bottom` cleared.
    pub fn clear(&mut self, top: u8, bottom: u8) {
        self.window(top, 0, bottom, LAST_COLUMN);
    }

    /// The rectangle `top`, `left` to `bottom`, `right` cleared.
    pub fn window(&mut self, top: u8, left: u8, bottom: u8, right: u8) {
        self.ops
            .extend_from_slice(&[op::CLEAR, top, left, bottom, right]);
        self.last_text = None;
    }

    fn beep(&mut self) {
        self.ops.push(op::BEEP);
        self.last_text = None;
    }

    /// `byte` drawn at `row`, `column` in `attribute`: added to the text
    /// call before it where it runs on from it.
    fn put(&mut self, row: u8, column: u8, attribute: u8, byte: u8) {
        if column > LAST_COLUMN {
            return;
        }
        if let Some(at) = self.last_text {
            let [_, r, c, a, len] = [
                self.ops[at],
                self.ops[at + 1],
                self.ops[at + 2],
                self.ops[at + 3],
                self.ops[at + 4],
            ];
            if r == row && a == attribute && c.checked_add(len) == Some(column) && len < u8::MAX {
                self.ops[at + 4] = len + 1;
                self.ops.push(byte);
                return;
            }
        }
        self.last_text = Some(self.ops.len());
        self.ops
            .extend_from_slice(&[op::TEXT, row, column, attribute, 1, byte]);
    }

    /// `text` drawn in `attribute` from `row`, `column`, none of it past the
    /// last column.
    pub fn write(&mut self, row: u8, column: u8, attribute: u8, text: &[u8]) {
        for (at, &byte) in (column..=LAST_COLUMN).zip(text) {
            self.put(row, at, attribute, byte);
        }
    }

    /// `text` drawn regular on row `row`, centred.
    pub fn centred(&mut self, row: u8, text: &[u8]) {
        let len = u8::try_from(text.len())
            .unwrap_or(u8::MAX)
            .min(LAST_COLUMN + 1);
        self.write(row, (LAST_COLUMN + 1 - len) / 2, attr::REGULAR, text);
    }

    /// `message` drawn, its `<I>`s taking `inserts` in turn, a blank for
    /// any past them.
    pub fn draw(&mut self, message: &str, inserts: &[u8]) {
        let text = message.as_bytes();
        let mut pen = Pen::default();
        let mut inserts = inserts.iter().copied();
        let mut i = 0;
        while i < text.len() {
            match text[i] {
                b'<' => {
                    i += 1;
                    while i < text.len() && text[i] != b'>' {
                        match text[i] {
                            b'H' => pen.attribute = (pen.attribute & attr::BLINK) | attr::HIGH,
                            b'R' => pen.attribute = (pen.attribute & attr::BLINK) | attr::REGULAR,
                            b'B' => pen.attribute |= attr::BLINK,
                            b'O' => pen.attribute &= !attr::BLINK,
                            b'W' => self.beep(),
                            b'I' => self.show(&mut pen, inserts.next().unwrap_or(b' ')),
                            b'Y' => self.show(&mut pen, YES),
                            b'N' => self.show(&mut pen, NO),
                            b'S' => self.input = Some((pen.row, pen.column)),
                            b'C' => self.window(pen.row, pen.column, pen.row, LAST_COLUMN),
                            _ => {}
                        }
                        i += 1;
                    }
                    i += 1;
                }
                b'^' => {
                    if let Some((down, to)) = position(&text[i..]) {
                        pen.row = pen.row.saturating_add(down);
                        pen.column = to;
                        i += POSITION_LEN;
                    } else {
                        self.show(&mut pen, b'^');
                        i += 1;
                    }
                }
                byte => {
                    self.show(&mut pen, byte);
                    i += 1;
                }
            }
        }
    }

    /// `byte` drawn where `pen` is, which moves on; a message's text before
    /// any attribute is chosen is regular.
    fn show(&mut self, pen: &mut Pen, byte: u8) {
        if pen.attribute == 0 {
            pen.attribute = attr::REGULAR;
        }
        self.put(pen.row, pen.column, pen.attribute, byte);
        pen.column = pen.column.saturating_add(1);
    }

    /// The calls as they lie in memory, ended, those that would run past
    /// `room` bytes left out whole.
    #[must_use]
    pub fn encoded(&self, room: usize) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.ops.len().min(room) + 1);
        let mut at = 0;
        while at < self.ops.len() {
            let len = match self.ops[at] {
                op::TEXT => op::TEXT_HEADER + usize::from(self.ops[at + 4]),
                op::CLEAR => op::CLEAR_LEN,
                _ => 1,
            };
            if out.len() + len >= room {
                break;
            }
            out.extend_from_slice(&self.ops[at..at + len]);
            at += len;
        }
        out.push(op::END);
        out
    }
}

/// Where a message is being drawn, and in what.
#[derive(Clone, Copy, Debug, Default)]
struct Pen {
    row: u8,
    column: u8,
    attribute: u8,
}

/// The bytes of a `^rrcc^` position.
const POSITION_LEN: usize = 6;

/// The rows down and the column a `^rrcc^` position at `text`'s start
/// names.
fn position(text: &[u8]) -> Option<(u8, u8)> {
    let code = text.get(..POSITION_LEN)?;
    if code[0] != b'^' || code[5] != b'^' || !code[1..5].iter().all(u8::is_ascii_digit) {
        return None;
    }
    let number = |at: usize| (code[at] - b'0') * 10 + (code[at + 1] - b'0');
    Some((number(1), number(3)))
}

locals! {
    /// The screen whose calls lie at `list` in the program's memory drawn
    /// through INT 10h.
    pub struct FdiskShow {
        list: u16,
        at: u16,
        done: u8,
        run: u8,
    }
}

impl FdiskShow {
    #[must_use]
    pub const fn of(list: u16) -> Self {
        Self {
            list,
            at: 0,
            done: 0,
            run: 0,
        }
    }
}

mod showing {
    pub const PLACED: u8 = 1;
    pub const WRITTEN: u8 = 2;
    pub const DONE: u8 = 3;
}

impl Routine for FdiskShow {
    const ID: Id = Id::FdiskShow;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        let call = self.list.wrapping_add(self.at);
        match *phase {
            showing::PLACED => {
                take!(child);
                let byte = p.get8(call + 5 + u16::from(self.done));
                *phase = showing::WRITTEN;
                Flow::Call(Frame::of(&Int {
                    bx: u16::from(p.get8(call + 3)),
                    cx: u16::from(self.run),
                    ..Int::bios(p.own, 0x10, 0x0900 | u16::from(byte))
                }))
            }
            showing::WRITTEN => {
                take!(child);
                self.done += self.run;
                self.next(p, phase)
            }
            showing::DONE => {
                take!(child);
                self.next(p, phase)
            }
            _ => self.next(p, phase),
        }
    }
}

impl FdiskShow {
    /// The next call made, or, with none left, the screen drawn.
    fn next(&mut self, p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        loop {
            let call = self.list.wrapping_add(self.at);
            return match p.get8(call) {
                op::TEXT => {
                    let len = p.get8(call + 4);
                    if self.done >= len {
                        self.at += u16::try_from(op::TEXT_HEADER).unwrap_or(0) + u16::from(len);
                        self.done = 0;
                        continue;
                    }
                    let first = p.get8(call + 5 + u16::from(self.done));
                    let run = (self.done..len)
                        .take_while(|&i| p.get8(call + 5 + u16::from(i)) == first)
                        .count();
                    self.run = u8::try_from(run).unwrap_or(1);
                    let (row, column) =
                        (p.get8(call + 1), p.get8(call + 2).saturating_add(self.done));
                    *phase = showing::PLACED;
                    Flow::Call(Frame::of(&Int {
                        dx: u16::from_le_bytes([column, row]),
                        ..Int::bios(p.own, 0x10, 0x0200)
                    }))
                }
                op::CLEAR => {
                    let [top, left, bottom, right] = [
                        p.get8(call + 1),
                        p.get8(call + 2),
                        p.get8(call + 3),
                        p.get8(call + 4),
                    ];
                    self.at += u16::try_from(op::CLEAR_LEN).unwrap_or(0);
                    *phase = showing::DONE;
                    Flow::Call(Frame::of(&Int {
                        bx: u16::from(attr::REGULAR) << 8,
                        cx: u16::from_le_bytes([left, top]),
                        dx: u16::from_le_bytes([right, bottom]),
                        ..Int::bios(p.own, 0x10, 0x0600)
                    }))
                }
                op::BEEP => {
                    self.at += 1;
                    *phase = showing::DONE;
                    Flow::Call(Frame::of(&Int::bios(p.own, 0x10, 0x0E07)))
                }
                _ => Flow::Return(Ok(0)),
            };
        }
    }
}

#[cfg(test)]
#[path = "screen_tests.rs"]
mod tests;
