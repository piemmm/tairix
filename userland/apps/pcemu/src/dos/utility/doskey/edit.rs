//! The line `DOSKEY` edits, as MS-DOS 5's edits it. ↑ and ↓, Page Up and Page Down
//! recall the commands kept; ←, →, Home, End and Ctrl with ← and → move
//! along the line; Ins puts what is typed in rather than over the line;
//! Del, Backspace, Esc and Ctrl with Home and End take text out. DOS's
//! template keys F1 to F6 work on the template, the last line typed; F7
//! lists the commands kept, F8 finds the last one beginning as the line
//! does, F9 one by its number, and Alt+F7 and Alt+F10 forget the commands
//! and the macros. The line is drawn through the BIOS where the cursor
//! stood, wrapping at the screen's edge and followed as the screen scrolls.

use super::{at, flag, keep, load};
use crate::dos::char::{scan, template_reach, BACKSPACE, BELL, CR, CTRL_C, CTRL_Z, DEL, ESC, LF};
use crate::dos::program::{Flow, Frame, Id, Int, Key, Program, Returned, Routine, ENDED};
use crate::frames::{locals, take, Far, Outcome};
use alloc::vec::Vec;

/// How an edit ends: with the line in the caller's buffer, or broken by
/// Ctrl+C, the line left as it stood.
pub mod ended {
    pub const LINE: u32 = 0;
    pub const BREAK: u32 = 1;
}

/// What F9 asks before its number, its length, and the most digits the
/// number takes.
const LINE_NUMBER: &[u8] = b"Line number: ";
#[allow(
    clippy::cast_possible_truncation,
    reason = "the question's thirteen bytes"
)]
const ASKED_LEN: u8 = LINE_NUMBER.len() as u8;
const MOST_DIGITS: u8 = 4;

/// What a key read after F2, F4 or F9 means.
mod pending {
    pub const NONE: u8 = 0;
    pub const COPY_UP_TO: u8 = 1;
    pub const DELETE_UP_TO: u8 = 2;
    pub const NUMBER: u8 = 3;
}

/// The call placing the cursor of `page` at `row`, `column`.
fn place(p: &Program<'_, '_>, page: u8, row: u8, column: u8) -> Frame {
    Frame::of(&Int {
        bx: u16::from(page) << 8,
        dx: u16::from_le_bytes([column, row]),
        ..Int::bios(p.own, 0x10, 0x0200)
    })
}

/// The call reading the cursor of `page`: its place in DX, its shape in
/// CX.
fn read_cursor(p: &Program<'_, '_>, page: u8) -> Frame {
    Frame::of(&Int {
        bx: u16::from(page) << 8,
        ..Int::bios(p.own, 0x10, 0x0300)
    })
}

/// `line` given to the caller in the function `0Ah` buffer at `buffer`, as
/// much of it as the buffer's room holds, its CR after it: the characters
/// given.
pub(super) fn give(p: &mut Program<'_, '_>, buffer: Far, line: &[u8]) -> u8 {
    let room = usize::from(p.g.read8(buffer.segment, buffer.offset).saturating_sub(1));
    let line = &line[..line.len().min(room)];
    let len = u8::try_from(line.len()).unwrap_or(0);
    let text = buffer.offset.wrapping_add(2);
    p.g.write_bytes(buffer.segment, text, line);
    p.g.write8(buffer.segment, text.wrapping_add(u16::from(len)), CR);
    p.g.write8(buffer.segment, buffer.offset.wrapping_add(1), len);
    len
}

/// The bytes at `at` in the program's memory, `len` of them.
fn bytes_at(p: &mut Program<'_, '_>, at: u16, len: u8) -> Vec<u8> {
    let mut out = alloc::vec![0u8; usize::from(len)];
    p.g.read_bytes(p.own, at, &mut out);
    out
}

locals! {
    /// Writes `count` bytes from `from` in the program's memory, then
    /// `blanks` spaces, through the teletype of `page`, which wraps and
    /// scrolls as it goes.
    pub struct DosKeyWrite {
        from: u16,
        count: u8,
        blanks: u8,
        page: u8,
        done: u8,
    }
}

impl DosKeyWrite {
    #[must_use]
    pub const fn of(from: u16, count: u8, blanks: u8, page: u8) -> Self {
        Self {
            from,
            count,
            blanks,
            page,
            done: 0,
        }
    }
}

impl Routine for DosKeyWrite {
    const ID: Id = Id::DosKeyWrite;

    fn step(&mut self, p: &mut Program<'_, '_>, _phase: &mut u8, child: Outcome) -> Flow {
        if self.done > 0 {
            take!(child);
        }
        if self.done >= self.count.saturating_add(self.blanks) {
            return Flow::Return(Ok(0));
        }
        let byte = if self.done < self.count {
            p.get8(self.from + u16::from(self.done))
        } else {
            b' '
        };
        self.done += 1;
        Flow::Call(Frame::of(&Int {
            bx: u16::from(self.page) << 8,
            ..Int::bios(p.own, 0x10, 0x0E00 | u16::from(byte))
        }))
    }
}

locals! {
    /// A line edited into the function `0Ah` buffer at `buffer`, its first
    /// byte the room in it. Answers how it [`ended`].
    pub struct DosKeyEdit {
        buffer: Far,
        cursor: u8,
        len: u8,
        /// The characters the screen shows of the line.
        shown: u8,
        /// Where the line begins on the screen, and the screen's width.
        row: u8,
        column: u8,
        columns: u8,
        page: u8,
        insert: bool,
        /// The command recalled, counted back from the newest; 0 for none.
        recalled: u16,
        /// The cursor's shape as it was found.
        shape: u16,
        /// The characters F8 searches by, held while F8 is pressed again.
        searched: u8,
        pending: u8,
        /// Where a redraw begins.
        from: u8,
        /// F9's number, or the command F7 lists next.
        number: u16,
    }
}

impl DosKeyEdit {
    #[must_use]
    pub fn of(buffer: Far) -> Self {
        Self {
            buffer,
            ..Self::default()
        }
    }
}

mod editing {
    pub const MODE: u8 = 1;
    pub const FOUND: u8 = 2;
    pub const KEYED: u8 = 3;
    pub const SCANNED: u8 = 4;
    pub const DRAW_PLACED: u8 = 5;
    pub const WRITTEN: u8 = 6;
    pub const DRAWN: u8 = 7;
    pub const PLACED: u8 = 8;
    pub const END_PLACED: u8 = 9;
    pub const ENDED_LINE: u8 = 10;
    pub const RESTORED: u8 = 11;
    pub const BROKEN: u8 = 12;
    pub const LIST_PLACED: u8 = 13;
    pub const LISTED: u8 = 14;
    pub const LIST_ENDED: u8 = 15;
    pub const ASK_PLACED: u8 = 16;
    pub const ASKED: u8 = 17;
    pub const ECHOED: u8 = 18;
    pub const SHAPED: u8 = 19;
    pub const BREAK_RESTORED: u8 = 20;
}

impl DosKeyEdit {
    /// The most characters the line takes: the room the caller's buffer
    /// gives less its CR, and no more than the line's own room.
    fn most(&self, p: &mut Program<'_, '_>) -> u8 {
        let room = p.g.read8(self.buffer.segment, self.buffer.offset);
        #[allow(clippy::cast_possible_truncation, reason = "the line's room is 128")]
        let own = (at::LINE_ROOM - 1) as u8;
        room.saturating_sub(1).min(own)
    }

    fn line(&self, p: &mut Program<'_, '_>) -> Vec<u8> {
        bytes_at(p, at::LINE, self.len)
    }

    fn set_line(&mut self, p: &mut Program<'_, '_>, line: &[u8]) {
        let most = usize::from(self.most(p));
        let line = &line[..line.len().min(most)];
        p.g.write_bytes(p.own, at::LINE, line);
        self.len = u8::try_from(line.len()).unwrap_or(0);
    }

    fn template(p: &mut Program<'_, '_>) -> Vec<u8> {
        let len = u8::try_from(p.get16(at::TEMPLATE_LEN)).unwrap_or(0);
        bytes_at(p, at::TEMPLATE, len)
    }

    /// Where character `n` of the line is on the screen.
    fn spot(&self, n: u8) -> (u8, u8) {
        let linear = u16::from(self.column) + u16::from(n);
        let columns = u16::from(self.columns.max(1));
        let row = u16::from(self.row) + linear / columns;
        (
            u8::try_from(row).unwrap_or(u8::MAX),
            u8::try_from(linear % columns).unwrap_or(0),
        )
    }

    fn read_key(phase: &mut u8, then: u8) -> Flow {
        *phase = then;
        Flow::Call(Frame::of(&Key::of(0x0700)))
    }

    /// The cursor put where typing goes on, then the next key read.
    fn settle(&mut self, p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        let (row, column) = self.spot(self.cursor);
        *phase = editing::PLACED;
        Flow::Call(place(p, self.page, row, column))
    }

    /// The line drawn again from character `from`, then the cursor put
    /// back.
    fn redraw(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, from: u8) -> Flow {
        self.from = from.min(self.len).min(self.shown);
        let (row, column) = self.spot(self.from);
        *phase = editing::DRAW_PLACED;
        Flow::Call(place(p, self.page, row, column))
    }

    fn beep(&mut self, p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        *phase = editing::ECHOED;
        Flow::Call(Frame::of(&Int {
            bx: u16::from(self.page) << 8,
            ..Int::bios(p.own, 0x10, 0x0E00 | u16::from(BELL))
        }))
    }

    /// `bytes` typed at the cursor: put in, or over the line, as far as the
    /// line has room.
    fn put(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, bytes: &[u8]) -> Flow {
        let mut line = self.line(p);
        let most = usize::from(self.most(p));
        let at = usize::from(self.cursor);
        let fits = if self.insert {
            let n = bytes.len().min(most - line.len().min(most));
            line.splice(at..at, bytes[..n].iter().copied());
            n
        } else {
            let n = bytes.len().min(most - at.min(most));
            let over = (line.len() - at).min(n);
            line.splice(at..at + over, bytes[..n].iter().copied());
            n
        };
        if fits == 0 {
            return self.beep(p, phase);
        }
        self.set_line(p, &line);
        self.cursor = self.cursor.saturating_add(u8::try_from(fits).unwrap_or(0));
        self.redraw(p, phase, u8::try_from(at).unwrap_or(0))
    }

    /// Characters `range` of the line taken out, the cursor left where it
    /// began.
    fn cut(
        &mut self,
        p: &mut Program<'_, '_>,
        phase: &mut u8,
        range: core::ops::Range<u8>,
    ) -> Flow {
        let mut line = self.line(p);
        let (start, end) = (
            usize::from(range.start),
            usize::from(range.end).min(line.len()),
        );
        if start >= end {
            return self.settle(p, phase);
        }
        line.drain(start..end);
        self.set_line(p, &line);
        self.cursor = range.start;
        self.redraw(p, phase, range.start)
    }

    /// The command `recalled` back from the newest made the line, at its
    /// end.
    fn recall(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, recalled: u16) -> Flow {
        let store = load(p, p.own);
        let count = u16::try_from(store.history().len()).unwrap_or(u16::MAX);
        if count == 0 {
            return Self::read_key(phase, editing::KEYED);
        }
        let recalled = recalled.clamp(1, count);
        let Some(command) = store.history().get(usize::from(count - recalled)) else {
            return Self::read_key(phase, editing::KEYED);
        };
        self.recalled = recalled;
        self.set_line(p, command);
        self.cursor = self.len;
        self.redraw(p, phase, 0)
    }

    /// F8: the next older command beginning as the line does up to the
    /// cursor, from the newest again once none is left.
    fn search(&mut self, p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        if self.searched == 0 {
            self.searched = self.cursor.max(1);
        }
        let prefix = bytes_at(p, at::LINE, self.searched.min(self.len));
        let store = load(p, p.own);
        let history = store.history();
        let count = history.len();
        let older = (usize::from(self.recalled) + 1..=count).chain(1..=count);
        let found = older.take(count).find(|&back| {
            history.get(count - back).is_some_and(|command| {
                command.len() >= prefix.len()
                    && command[..prefix.len()].eq_ignore_ascii_case(&prefix)
            })
        });
        let Some(back) = found else {
            return self.beep(p, phase);
        };
        self.recalled = u16::try_from(back).unwrap_or(0);
        let command = history[count - back].clone();
        self.set_line(p, &command);
        self.cursor = self.searched.min(self.len);
        self.redraw(p, phase, 0)
    }

    /// The word before the cursor's start, and the next word's.
    fn word_back(&self, line: &[u8]) -> u8 {
        let mut at = usize::from(self.cursor);
        while at > 0 && line[at - 1] == b' ' {
            at -= 1;
        }
        while at > 0 && line[at - 1] != b' ' {
            at -= 1;
        }
        u8::try_from(at).unwrap_or(0)
    }

    fn word_on(&self, line: &[u8]) -> u8 {
        let mut at = usize::from(self.cursor);
        while at < line.len() && line[at] != b' ' {
            at += 1;
        }
        while at < line.len() && line[at] == b' ' {
            at += 1;
        }
        u8::try_from(at).unwrap_or(0)
    }

    /// Insert or overstrike, the cursor shaped to say which.
    fn shape_for(&self, p: &Program<'_, '_>) -> Frame {
        let [end, start] = self.shape.to_le_bytes();
        let shape = if self.insert {
            u16::from_le_bytes([end, end / 2])
        } else {
            u16::from_le_bytes([end, start])
        };
        Frame::of(&Int {
            cx: shape,
            ..Int::bios(p.own, 0x10, 0x0100)
        })
    }

    /// Enter: the line given to the caller, kept as the newest command and
    /// the template, the cursor taken past it.
    fn enter(&mut self, p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        let line = self.line(p);
        give(p, self.buffer, &line);
        let mut store = load(p, p.own);
        store.remember(&line);
        keep(p, p.own, &store);
        p.g.write_bytes(p.own, at::TEMPLATE, &line);
        p.set16(at::TEMPLATE_LEN, u16::from(self.len));
        let (row, column) = self.spot(self.len);
        *phase = editing::END_PLACED;
        Flow::Call(place(p, self.page, row, column))
    }

    /// A key that is a character.
    fn typed(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, key: u8) -> Flow {
        match key {
            CR => self.enter(p, phase),
            CTRL_C => {
                let (row, column) = self.spot(self.len);
                *phase = editing::BROKEN;
                Flow::Call(place(p, self.page, row, column))
            }
            BACKSPACE | DEL if self.cursor > 0 => self.cut(p, phase, self.cursor - 1..self.cursor),
            ESC => {
                self.len = 0;
                self.cursor = 0;
                self.recalled = 0;
                self.redraw(p, phase, 0)
            }
            BACKSPACE | DEL | BELL | LF => Self::read_key(phase, editing::KEYED),
            key => self.put(p, phase, &[key]),
        }
    }

    /// A key read after F2, F4 or F9.
    fn answered(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, key: u8) -> Flow {
        let waiting = core::mem::replace(&mut self.pending, pending::NONE);
        match waiting {
            pending::COPY_UP_TO => {
                let template = Self::template(p);
                let Some(upto) = template_reach(&template, usize::from(self.cursor), key) else {
                    return Self::read_key(phase, editing::KEYED);
                };
                let copied = template[usize::from(self.cursor)..upto].to_vec();
                let insert = core::mem::replace(&mut self.insert, false);
                let flow = self.put(p, phase, &copied);
                self.insert = insert;
                flow
            }
            pending::DELETE_UP_TO => {
                let line = self.line(p);
                match template_reach(&line, usize::from(self.cursor), key) {
                    Some(upto) => self.cut(p, phase, self.cursor..u8::try_from(upto).unwrap_or(0)),
                    None => Self::read_key(phase, editing::KEYED),
                }
            }
            _ => match key {
                digit @ b'0'..=b'9' if self.number < 10u16.pow(u32::from(MOST_DIGITS - 1)) => {
                    self.number = self.number * 10 + u16::from(digit - b'0');
                    self.pending = pending::NUMBER;
                    *phase = editing::ECHOED;
                    Flow::Call(Frame::of(&Int {
                        bx: u16::from(self.page) << 8,
                        ..Int::bios(p.own, 0x10, 0x0E00 | u16::from(digit))
                    }))
                }
                CR if self.number > 0 => {
                    let store = load(p, p.own);
                    let count = u16::try_from(store.history().len()).unwrap_or(u16::MAX);
                    let back = count
                        .checked_sub(self.number.saturating_sub(1))
                        .filter(|&back| back >= 1 && self.number <= count);
                    match back {
                        Some(back) => self.recall(p, phase, back),
                        None => self.redraw(p, phase, 0),
                    }
                }
                _ => self.redraw(p, phase, 0),
            },
        }
    }

    /// A key with no character: its scan code.
    #[allow(
        clippy::too_many_lines,
        reason = "one arm a key, as DOSKEY's list of them runs"
    )]
    fn scanned(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, key: u8) -> Flow {
        let line = self.line(p);
        let template = Self::template(p);
        let at = usize::from(self.cursor);
        match key {
            scan::LEFT if self.cursor > 0 => {
                self.cursor -= 1;
                self.settle(p, phase)
            }
            scan::RIGHT if self.cursor < self.len => {
                self.cursor += 1;
                self.settle(p, phase)
            }
            scan::RIGHT | scan::F1 if at < template.len() => {
                let insert = core::mem::replace(&mut self.insert, false);
                let flow = self.put(p, phase, &template[at..=at]);
                self.insert = insert;
                flow
            }
            scan::HOME => {
                self.cursor = 0;
                self.settle(p, phase)
            }
            scan::END => {
                self.cursor = self.len;
                self.settle(p, phase)
            }
            scan::CTRL_LEFT => {
                self.cursor = self.word_back(&line);
                self.settle(p, phase)
            }
            scan::CTRL_RIGHT => {
                self.cursor = self.word_on(&line);
                self.settle(p, phase)
            }
            scan::UP => self.recall(p, phase, self.recalled.saturating_add(1)),
            scan::DOWN => self.recall(p, phase, self.recalled.saturating_sub(1)),
            scan::PAGE_UP => self.recall(p, phase, u16::MAX),
            scan::PAGE_DOWN => self.recall(p, phase, 1),
            scan::INSERT => {
                self.insert = !self.insert;
                *phase = editing::SHAPED;
                Flow::Call(self.shape_for(p))
            }
            scan::DELETE => self.cut(p, phase, self.cursor..self.cursor.saturating_add(1)),
            scan::CTRL_END => self.cut(p, phase, self.cursor..self.len),
            scan::CTRL_HOME => self.cut(p, phase, 0..self.cursor),
            scan::F2 => {
                self.pending = pending::COPY_UP_TO;
                Self::read_key(phase, editing::KEYED)
            }
            scan::F3 if at < template.len() => {
                let insert = core::mem::replace(&mut self.insert, false);
                let flow = self.put(p, phase, &template[at..]);
                self.insert = insert;
                flow
            }
            scan::F4 => {
                self.pending = pending::DELETE_UP_TO;
                Self::read_key(phase, editing::KEYED)
            }
            scan::F5 => {
                p.g.write_bytes(p.own, at::TEMPLATE, &line);
                p.set16(at::TEMPLATE_LEN, u16::from(self.len));
                self.len = 0;
                self.cursor = 0;
                self.redraw(p, phase, 0)
            }
            scan::F6 => self.put(p, phase, &[CTRL_Z]),
            scan::F7 => {
                let (row, column) = self.spot(self.len);
                self.number = 0;
                *phase = editing::LIST_PLACED;
                Flow::Call(place(p, self.page, row, column))
            }
            scan::F8 => {
                let held = self.searched;
                let flow = self.search(p, phase);
                if held != 0 {
                    self.searched = held;
                }
                flow
            }
            scan::F9 => {
                let (row, column) = self.spot(0);
                *phase = editing::ASK_PLACED;
                Flow::Call(place(p, self.page, row, column))
            }
            scan::ALT_F7 => {
                let mut store = load(p, p.own);
                store.clear_history();
                keep(p, p.own, &store);
                self.recalled = 0;
                Self::read_key(phase, editing::KEYED)
            }
            scan::ALT_F10 => {
                let mut store = load(p, p.own);
                store.clear_macros();
                keep(p, p.own, &store);
                Self::read_key(phase, editing::KEYED)
            }
            _ => Self::read_key(phase, editing::KEYED),
        }
    }

    /// F7's next command written below the line, numbered, or, with none
    /// left, the line drawn again under the list.
    fn list_next(&mut self, p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        let store = load(p, p.own);
        let Some(command) = store.history().get(usize::from(self.number)) else {
            *phase = editing::LIST_ENDED;
            return Flow::Call(read_cursor(p, self.page));
        };
        self.number += 1;
        let entry = [
            alloc::format!("{}: ", self.number).as_bytes(),
            command,
            b"\r\n",
        ]
        .concat();
        let len = entry.len().min(usize::from(at::TEXT_ROOM));
        p.g.write_bytes(p.own, at::TEXT, &entry[..len]);
        *phase = editing::LISTED;
        Flow::Call(Frame::of(&DosKeyWrite::of(
            at::TEXT,
            u8::try_from(len).unwrap_or(0),
            0,
            self.page,
        )))
    }
}

impl Routine for DosKeyEdit {
    const ID: Id = Id::DosKeyEdit;

    #[allow(clippy::too_many_lines, reason = "one arm a phase of the edit")]
    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        match *phase {
            editing::MODE => {
                let columns = take!(child).to_le_bytes()[1];
                self.columns = columns.max(1);
                self.page = p.result(Returned::Bx).to_le_bytes()[1];
                *phase = editing::FOUND;
                Flow::Call(read_cursor(p, self.page))
            }
            editing::FOUND => {
                take!(child);
                let [column, row] = p.result(Returned::Dx).to_le_bytes();
                (self.row, self.column) = (row, column);
                self.shape = p.result(Returned::Cx);
                self.insert = p.get8(at::FLAGS) & flag::INSERT != 0;
                if self.most(p) == 0 {
                    p.g.write8(self.buffer.segment, self.buffer.offset.wrapping_add(1), 0);
                    return Flow::Return(Ok(ended::LINE));
                }
                *phase = editing::SHAPED;
                Flow::Call(self.shape_for(p))
            }
            editing::SHAPED | editing::PLACED | editing::ECHOED => {
                take!(child);
                Self::read_key(phase, editing::KEYED)
            }
            editing::KEYED => {
                let read = take!(child);
                if read == ENDED {
                    return self.enter(p, phase);
                }
                let key = read.to_le_bytes()[0];
                if self.pending != pending::NONE {
                    return self.answered(p, phase, key);
                }
                if key == 0 {
                    return Self::read_key(phase, editing::SCANNED);
                }
                self.searched = 0;
                self.typed(p, phase, key)
            }
            editing::SCANNED => {
                let key = take!(child).to_le_bytes()[0];
                if key != scan::F8 {
                    self.searched = 0;
                }
                self.scanned(p, phase, key)
            }
            editing::DRAW_PLACED => {
                take!(child);
                let count = self.len - self.from;
                let blanks = self.shown.saturating_sub(self.len.max(self.from));
                *phase = editing::WRITTEN;
                Flow::Call(Frame::of(&DosKeyWrite::of(
                    at::LINE + u16::from(self.from),
                    count,
                    blanks,
                    self.page,
                )))
            }
            editing::WRITTEN => {
                take!(child);
                *phase = editing::DRAWN;
                Flow::Call(read_cursor(p, self.page))
            }
            editing::DRAWN => {
                take!(child);
                let row = p.result(Returned::Dx).to_le_bytes()[1];
                let end = u16::from(self.column) + u16::from(self.len.max(self.shown));
                let down = u8::try_from(end / u16::from(self.columns.max(1))).unwrap_or(u8::MAX);
                self.row = row.saturating_sub(down);
                self.shown = self.len;
                self.settle(p, phase)
            }
            editing::END_PLACED => {
                take!(child);
                *phase = editing::ENDED_LINE;
                Flow::Call(Frame::of(&Int {
                    bx: u16::from(self.page) << 8,
                    ..Int::bios(p.own, 0x10, 0x0E00 | u16::from(CR))
                }))
            }
            editing::ENDED_LINE => {
                take!(child);
                self.insert = false;
                *phase = editing::RESTORED;
                Flow::Call(self.shape_for(p))
            }
            editing::RESTORED => Flow::Return(Ok(ended::LINE)),
            editing::BROKEN => {
                take!(child);
                self.insert = false;
                *phase = editing::BREAK_RESTORED;
                Flow::Call(self.shape_for(p))
            }
            editing::BREAK_RESTORED => Flow::Return(Ok(ended::BREAK)),
            editing::LIST_PLACED => {
                take!(child);
                p.g.write_bytes(p.own, at::TEXT, b"\r\n");
                *phase = editing::LISTED;
                Flow::Call(Frame::of(&DosKeyWrite::of(at::TEXT, 2, 0, self.page)))
            }
            editing::LISTED => {
                take!(child);
                self.list_next(p, phase)
            }
            editing::LIST_ENDED => {
                take!(child);
                let [column, row] = p.result(Returned::Dx).to_le_bytes();
                (self.row, self.column, self.shown) = (row, column, 0);
                self.redraw(p, phase, 0)
            }
            editing::ASK_PLACED => {
                take!(child);
                p.g.write_bytes(p.own, at::TEXT, LINE_NUMBER);
                let blanks = self.shown.saturating_sub(ASKED_LEN);
                self.shown = self.shown.max(ASKED_LEN + MOST_DIGITS);
                self.number = 0;
                self.pending = pending::NUMBER;
                *phase = editing::ASKED;
                Flow::Call(Frame::of(&DosKeyWrite::of(
                    at::TEXT,
                    ASKED_LEN,
                    blanks,
                    self.page,
                )))
            }
            editing::ASKED => {
                take!(child);
                let (row, column) = self.spot(ASKED_LEN);
                *phase = editing::PLACED;
                Flow::Call(place(p, self.page, row, column))
            }
            _ => {
                *phase = editing::MODE;
                Flow::Call(Frame::of(&Int::bios(p.own, 0x10, 0x0F00)))
            }
        }
    }
}
