//! `MORE`: the standard input written to the standard output a screen at a
//! time, "-- More --" on the standard error between screens and a key from
//! the console to go on. The standard input is moved aside and the
//! standard error put in its place, as DOS's `MORE` does, so the keys come
//! from the console however the text arrives. A Ctrl+Z ends the text.

use crate::dos::char::CTRL_Z;
use crate::dos::device::generic::{self, display};
use crate::dos::program::{say, say_error, Flow, Frame, Id, Int, Program, Routine};
use crate::frames::{locals, take, Outcome};

/// Where `MORE`'s state lies in its memory.
pub mod at {
    use crate::dos::program::at::STATE;

    /// The screen's rows and columns, and where the cursor stands in them,
    /// from `1`.
    pub const ROWS: u16 = STATE;
    pub const COLUMNS: u16 = STATE + 0x01;
    pub const ROW: u16 = STATE + 0x02;
    pub const COLUMN: u16 = STATE + 0x03;
    /// The text's handle, once moved aside.
    pub const INPUT: u16 = STATE + 0x04;
    /// The bytes read and not yet written: where they begin and end in
    /// [`BUFFER`].
    pub const AT: u16 = STATE + 0x06;
    pub const END: u16 = STATE + 0x08;
    pub const BUFFER: u16 = STATE + 0x10;
    pub const BUFFER_LEN: u16 = 0x1000;
    /// Bytes of `MORE`'s state.
    pub const LEN: u16 = BUFFER + BUFFER_LEN - STATE;
}

const ROWS: u8 = 25;

/// Where the cursor stands once `byte` is written at `(row, column)` on a
/// screen `columns` wide.
#[must_use]
pub fn advance(byte: u8, (row, column): (u8, u8), columns: u8) -> (u8, u8) {
    match byte {
        b'\r' => (row, 1),
        b'\n' => (row.saturating_add(1), column),
        0x08 => (row, if column > 1 { column - 1 } else { column }),
        b'\t' => (row, (column.saturating_add(7) & 0xF8).saturating_add(1)),
        0x07 => (row, column),
        _ if column >= columns => (row.saturating_add(1), 1),
        _ => (row, column + 1),
    }
}

locals! {
    /// `MORE`'s main routine.
    pub struct More {}
}

mod phase {
    pub const IOCTL: u8 = 1;
    pub const MODE: u8 = 2;
    pub const MOVED: u8 = 3;
    pub const CLOSED: u8 = 4;
    pub const DUPLICATED: u8 = 5;
    pub const READ: u8 = 6;
    pub const WRITTEN: u8 = 7;
    pub const FULL: u8 = 8;
    pub const ASKED: u8 = 9;
    pub const KEYED: u8 = 10;
    pub const EXTENDED: u8 = 11;
    pub const DONE: u8 = 12;
}

impl More {
    /// The next run of bytes to write, from [`at::AT`]: as far as the one
    /// that fills the screen, a Ctrl+Z, or the bytes read. Whether the
    /// screen is full after it, and whether the text has ended.
    fn run(p: &mut Program<'_, '_>) -> (u16, bool, bool) {
        let (at, end) = (p.get16(at::AT), p.get16(at::END));
        let mut bytes = alloc::vec![0u8; usize::from(end.saturating_sub(at))];
        p.g.read_bytes(p.own, at::BUFFER + at, &mut bytes);
        let (rows, columns) = (p.get8(at::ROWS), p.get8(at::COLUMNS));
        let mut cursor = (p.get8(at::ROW), p.get8(at::COLUMN));
        for (n, &byte) in (0u16..).zip(&bytes) {
            if byte == CTRL_Z {
                return (n, false, true);
            }
            cursor = advance(byte, cursor, columns);
            if cursor.0 >= rows {
                p.set8(at::ROW, cursor.0);
                p.set8(at::COLUMN, cursor.1);
                return (n + 1, true, false);
            }
        }
        p.set8(at::ROW, cursor.0);
        p.set8(at::COLUMN, cursor.1);
        (u16::try_from(bytes.len()).unwrap_or(0), false, false)
    }

    /// Writes the next run, or reads more when none is left.
    fn write_on(p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        let (at, end) = (p.get16(at::AT), p.get16(at::END));
        if at >= end {
            *phase = phase::READ;
            let handle = p.get16(at::INPUT);
            return Flow::Call(Frame::of(&Int {
                bx: handle,
                cx: at::BUFFER_LEN,
                dx: at::BUFFER,
                ..Int::dos(p.own, 0x3F00)
            }));
        }
        let (len, full, ended) = Self::run(p);
        p.set16(at::AT, if ended { end } else { at + len });
        if len == 0 {
            return Flow::Return(Ok(0));
        }
        *phase = if ended {
            phase::DONE
        } else if full {
            phase::FULL
        } else {
            phase::WRITTEN
        };
        Flow::Call(Frame::of(&Int {
            bx: 1,
            cx: len,
            dx: at::BUFFER + at,
            ..Int::dos(p.own, 0x4000)
        }))
    }
}

impl Routine for More {
    const ID: Id = Id::More;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        match *phase {
            0 => {
                let block = p.scratch(0x80);
                p.g.write_bytes(p.own, block, &[0; display::LEN as usize]);
                p.set16(block + display::LENGTH, display::DATA);
                p.set8(at::ROWS, ROWS);
                *phase = phase::IOCTL;
                Flow::Call(Frame::of(&Int {
                    bx: 2,
                    cx: u16::from_le_bytes([generic::GET_DISPLAY, generic::DISPLAY]),
                    dx: block,
                    ..Int::dos(p.own, 0x440C)
                }))
            }
            phase::IOCTL => {
                let block = p.scratch(0x80);
                if child.is_ok() && p.get8(block + display::MODE) == display::TEXT {
                    let rows = p.get16(block + display::ROWS);
                    p.set8(at::ROWS, u8::try_from(rows).unwrap_or(u8::MAX));
                }
                *phase = phase::MODE;
                Flow::Call(Frame::of(&Int::bios(p.own, 0x10, 0x0F00)))
            }
            phase::MODE => {
                let columns = take!(child).to_le_bytes()[1];
                p.set8(at::COLUMNS, columns);
                *phase = phase::MOVED;
                Flow::Call(Frame::of(&Int::dos(p.own, 0x4500)))
            }
            phase::MOVED => {
                let handle = take!(child);
                p.set16(at::INPUT, u16::try_from(handle).unwrap_or(0));
                *phase = phase::CLOSED;
                Flow::Call(Frame::of(&Int::dos(p.own, 0x3E00)))
            }
            phase::CLOSED => {
                take!(child);
                *phase = phase::DUPLICATED;
                Flow::Call(Frame::of(&Int {
                    bx: 2,
                    ..Int::dos(p.own, 0x4500)
                }))
            }
            phase::DUPLICATED => {
                take!(child);
                p.set8(at::ROW, 1);
                p.set8(at::COLUMN, 1);
                p.set16(at::AT, 0);
                p.set16(at::END, 0);
                *phase = phase::WRITTEN;
                Flow::Call(say(p, b"\r\n"))
            }
            phase::READ => {
                let read = u16::try_from(take!(child)).unwrap_or(0).min(at::BUFFER_LEN);
                if read == 0 {
                    return Flow::Return(Ok(0));
                }
                p.set16(at::AT, 0);
                p.set16(at::END, read);
                Self::write_on(p, phase)
            }
            phase::WRITTEN => {
                take!(child);
                Self::write_on(p, phase)
            }
            phase::FULL => {
                take!(child);
                *phase = phase::ASKED;
                Flow::Call(say_error(p, b"-- More --"))
            }
            phase::ASKED => {
                take!(child);
                *phase = phase::KEYED;
                Flow::Call(Frame::of(&Int::dos(p.own, 0x0C08)))
            }
            phase::KEYED => {
                let key = take!(child).to_le_bytes()[0];
                *phase = phase::EXTENDED;
                if key == 0 {
                    return Flow::Call(Frame::of(&Int::dos(p.own, 0x0800)));
                }
                self.step(p, phase, Ok(0))
            }
            phase::EXTENDED => {
                p.set8(at::ROW, 1);
                p.set8(at::COLUMN, 1);
                *phase = phase::WRITTEN;
                Flow::Call(say_error(p, b"\r\n\r\n"))
            }
            phase::DONE => {
                take!(child);
                Flow::Return(Ok(0))
            }
            _ => Flow::Return(Ok(0)),
        }
    }
}
