//! `FC [/A] [/B] [/C] [/L] [/LBn] [/N] [/T] [/W] [/nnnn] file1 file2`: two
//! files compared, as DOS 4's `FC` compares them. Text is compared a line at
//! a time, each run of differing lines shown with the matching line either
//! side of it once `/nnnn` lines (two unless given) match again; a program's
//! file, or any with `/B`, a byte at a time, each differing byte shown at
//! its offset.

mod bytes;
mod lines;

pub use bytes::FcBytes;
pub use lines::FcLines;

#[cfg(test)]
#[path = "fc_tests.rs"]
mod tests;

use crate::dos::program::{say, Flow, Frame, Id, Int, Program, Routine};
use crate::dos::shell::set_asciz;
use crate::frames::{locals, take, Fault, Outcome};
use alloc::vec::Vec;

/// Where `FC`'s state lies in its memory.
pub mod at {
    use crate::dos::program::at::STATE;

    /// The switches, the lines that must match to resynchronise, and the
    /// lines each buffer holds.
    pub const SWITCHES: u16 = STATE;
    pub const SYNC: u16 = STATE + 0x02;
    pub const LINES: u16 = STATE + 0x04;
    /// Each file's handle, and the file being opened.
    pub const HANDLES: u16 = STATE + 0x06;
    pub const OPENING: u16 = STATE + 0x0A;
    /// Whether no difference has been found.
    pub const SAME: u16 = STATE + 0x0B;
    /// The files' names as typed, ASCIZ.
    pub const NAMES: u16 = STATE + 0x10;
    pub const NAME_LEN: u16 = 0x50;
    /// The country's upper case for 80h–FFh, for `/C`.
    pub const UPPER: u16 = NAMES + 2 * NAME_LEN;
    pub const UPPER_LEN: u16 = 0x80;
    /// Each file's reading: its buffer, and where its unread bytes begin
    /// and end there.
    pub const FILES: u16 = UPPER + UPPER_LEN;
    /// Bytes of a file's reading, from its start: the two positions, its
    /// end met, then the buffer.
    pub const FILE: u16 = BUFFER + BUFFER_LEN;
    pub const AT: u16 = 0x00;
    pub const END: u16 = 0x02;
    pub const DONE: u16 = 0x04;
    pub const BUFFER: u16 = 0x08;
    pub const BUFFER_LEN: u16 = 0x200;
    /// What the line compare keeps past the files.
    pub const WORK: u16 = FILES + 2 * FILE;
    pub const WORK_LEN: u16 = 0x40;
    /// Bytes of `FC`'s state.
    pub const LEN: u16 = WORK + WORK_LEN - STATE;
}

/// `FC`'s switches, as [`at::SWITCHES`] keeps them.
pub mod switch {
    pub const ABBREVIATE: u16 = 0x01;
    pub const BINARY: u16 = 0x02;
    pub const IGNORE_CASE: u16 = 0x04;
    pub const LINES: u16 = 0x08;
    pub const NUMBERS: u16 = 0x10;
    pub const KEEP_TABS: u16 = 0x20;
    pub const IGNORE_SPACE: u16 = 0x40;
}

/// `FC`'s messages, as DOS 4's are worded.
pub mod text {
    pub const USAGE: &[u8] =
        b"usage: fc [/a] [/b] [/c] [/l] [/lbNN] [/w] [/t] [/n] [/NNNN] file1 file2\r\n";
    pub const INCOMPATIBLE: &[u8] = b"fc: Incompatible switches\r\n";
    pub const SAME: &[u8] = b"fc: no differences encountered\r\n";
    pub const NO_MEMORY: &[u8] = b"fc: out of memory\r\n";
    pub const RESYNC: &[u8] = b"Resync failed.  Files are too different\r\n";
}

/// The code `FC` ends with when the files differ or it could not compare
/// them; `0` when they are the same.
const DIFFERENT: u32 = 1;
/// The lines a buffer holds unless `/LB` says, and the most one 64 KiB
/// block of them can.
const LINES: u16 = 100;
const MOST_LINES: u16 = 504;
/// Extensions a file is compared byte by byte for.
const PROGRAMS: [&[u8]; 6] = [b"EXE", b"OBJ", b"LIB", b"COM", b"BIN", b"SYS"];

/// What `FC`'s command line asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Request<'a> {
    pub switches: u16,
    pub sync: u16,
    pub lines: u16,
    pub files: [&'a [u8]; 2],
}

/// Why a command line is not taken.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    Usage,
    Incompatible,
}

/// The extension of a path's last name, without its dot.
fn extension(path: &[u8]) -> &[u8] {
    let name = path
        .rsplit(|&b| matches!(b, b'\\' | b'/' | b':'))
        .next()
        .unwrap_or(path);
    name.iter()
        .rposition(|&b| b == b'.')
        .map_or(&[], |at| &name[at + 1..])
}

/// The leading decimal digits of `text`, `0` with none, as C's `ntoi` reads
/// them.
fn leading_number(text: &[u8]) -> u32 {
    text.iter()
        .take_while(|b| b.is_ascii_digit())
        .fold(0u32, |n, &b| {
            n.saturating_mul(10).saturating_add(u32::from(b - b'0'))
        })
}

/// `FC`'s command line read as a C program's arguments are, split at blanks
/// alone: each word not starting with `/` a file named as far as a `/`, and
/// every `/` anywhere a switch.
///
/// # Errors
///
/// [`Refused::Usage`] for a line it cannot read, [`Refused::Incompatible`]
/// for `/B` with `/L`, `/N` or a resynchronisation count.
pub fn request(tail: &[u8]) -> Result<Request<'_>, Refused> {
    let (mut switches, mut sync, mut lines) = (0, None, None);
    let mut files: Vec<&[u8]> = Vec::new();
    for word in tail
        .split(|&b| matches!(b, b' ' | b'\t'))
        .filter(|word| !word.is_empty())
    {
        if !word.starts_with(b"/") {
            let end = word.iter().position(|&b| b == b'/').unwrap_or(word.len());
            files.push(&word[..end]);
        }
        for (at, _) in word.iter().enumerate().filter(|(_, &b)| b == b'/') {
            let after = &word[at + 1..];
            match after.first().map(u8::to_ascii_uppercase) {
                Some(b'A') => switches |= switch::ABBREVIATE,
                Some(b'B') => switches |= switch::BINARY,
                Some(b'C') => switches |= switch::IGNORE_CASE,
                Some(b'W') => switches |= switch::IGNORE_SPACE,
                Some(b'N') => switches |= switch::NUMBERS,
                Some(b'T') => switches |= switch::KEEP_TABS,
                Some(b'L') if after.get(1).is_some_and(|b| b.eq_ignore_ascii_case(&b'B')) => {
                    lines = Some(leading_number(&after[2..]));
                }
                Some(b'L') => switches |= switch::LINES,
                _ if after.iter().all(u8::is_ascii_digit) => sync = Some(leading_number(after)),
                _ => return Err(Refused::Usage),
            }
        }
    }
    let [first, second] = files[..] else {
        return Err(Refused::Usage);
    };
    let lines = match lines {
        None => LINES,
        Some(0) => return Err(Refused::Usage),
        Some(n) => u16::try_from(n).unwrap_or(u16::MAX),
    };
    if sync.is_some() {
        switches |= switch::LINES;
    }
    let sync = u16::try_from(sync.unwrap_or(2)).unwrap_or(u16::MAX);
    if switches & (switch::BINARY | switch::LINES) == 0 {
        let extension = extension(first);
        let program = PROGRAMS
            .iter()
            .any(|known| extension.eq_ignore_ascii_case(known));
        switches |= if program {
            switch::BINARY
        } else {
            switch::LINES
        };
    }
    if switches & switch::BINARY != 0 && switches & (switch::LINES | switch::NUMBERS) != 0 {
        return Err(Refused::Incompatible);
    }
    Ok(Request {
        switches,
        sync,
        lines,
        files: [first, second],
    })
}

/// What a file that cannot be opened is said to have failed with, as DOS
/// 4's C library words the error.
fn open_error(error: u16) -> &'static [u8] {
    match error {
        2 | 3 | 15 | 18 => b"No such file or directory",
        4 => b"Too many open files",
        5 | 0x20 | 0x21 => b"Permission denied",
        _ => b"Invalid argument",
    }
}

/// A file's name, as `FC` was given it.
fn name(p: &mut Program<'_, '_>, file: u16) -> Vec<u8> {
    let at = at::NAMES + file * at::NAME_LEN;
    let mut bytes = alloc::vec![0u8; usize::from(at::NAME_LEN)];
    p.g.read_bytes(p.own, at, &mut bytes);
    let len = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    bytes.truncate(len);
    bytes
}

/// A file's handle.
fn handle(p: &mut Program<'_, '_>, file: u16) -> u16 {
    p.get16(at::HANDLES + file * 2)
}

/// Where file `file`'s reading lies.
fn file_at(file: u16) -> u16 {
    at::FILES + file * at::FILE
}

/// The call reading on into file `file`'s buffer, its unread bytes none.
fn read_into(p: &mut Program<'_, '_>, file: u16) -> Frame {
    let base = file_at(file);
    p.set16(base + at::AT, 0);
    p.set16(base + at::END, 0);
    Frame::of(&Int {
        bx: handle(p, file),
        cx: at::BUFFER_LEN,
        dx: base + at::BUFFER,
        ..Int::dos(p.own, 0x3F00)
    })
}

/// A read into file `file`'s buffer came back with `read` bytes: none is
/// its end.
fn have_read(p: &mut Program<'_, '_>, file: u16, read: u32) {
    let base = file_at(file);
    let read = u16::try_from(read).unwrap_or(0).min(at::BUFFER_LEN);
    p.set16(base + at::END, read);
    p.set8(base + at::DONE, u8::from(read == 0));
}

locals! {
    /// `FC`'s main routine: the line read, both files opened, then the
    /// compare.
    pub struct Fc {}
}

mod phase {
    pub const SAID: u8 = 1;
    pub const SHRUNK: u8 = 2;
    pub const UPPER: u8 = 3;
    pub const OPENED: u8 = 4;
    pub const COMPARED: u8 = 5;
}

impl Fc {
    /// Opens file `file`, its name kept at [`at::NAMES`].
    fn open(p: &mut Program<'_, '_>, file: u16) -> Flow {
        p.set8(at::OPENING, u8::try_from(file).unwrap_or(0));
        Flow::Call(Frame::of(&Int {
            dx: at::NAMES + file * at::NAME_LEN,
            ..Int::dos(p.own, 0x3D00)
        }))
    }
}

impl Routine for Fc {
    const ID: Id = Id::Fc;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        match *phase {
            0 => {
                let tail = p.tail();
                let request = match request(&tail) {
                    Ok(request) => request,
                    Err(refused) => {
                        *phase = phase::SAID;
                        let said = if refused == Refused::Incompatible {
                            text::INCOMPATIBLE
                        } else {
                            text::USAGE
                        };
                        return Flow::Call(say(p, said));
                    }
                };
                p.set16(at::SWITCHES, request.switches);
                p.set16(at::SYNC, request.sync);
                p.set16(at::LINES, request.lines);
                p.set8(at::SAME, 1);
                for (n, file) in (0u16..).zip(request.files) {
                    set_asciz(p, at::NAMES + n * at::NAME_LEN, file, at::NAME_LEN);
                }
                *phase = phase::SHRUNK;
                let shrink = p.shrink();
                Flow::Call(shrink)
            }
            phase::SAID => Flow::Return(Ok(DIFFERENT)),
            phase::SHRUNK => {
                take!(child);
                if p.get16(at::SWITCHES) & switch::IGNORE_CASE == 0 {
                    *phase = phase::OPENED;
                    return Self::open(p, 0);
                }
                *phase = phase::UPPER;
                Flow::Call(Frame::of(&Int {
                    bx: 0xFFFF,
                    cx: 5,
                    dx: 0xFFFF,
                    di: p.scratch(0),
                    ..Int::dos(p.own, 0x6502)
                }))
            }
            // A country with no table gives plain ASCII case.
            phase::UPPER => {
                let mut table = [0u8; at::UPPER_LEN as usize];
                if child.is_ok() {
                    let record = p.scratch(0);
                    let pointer = p.get_far(record + 1);
                    p.g.read_bytes(pointer.segment, pointer.offset.wrapping_add(2), &mut table);
                } else {
                    table
                        .iter_mut()
                        .zip(0x80u8..)
                        .for_each(|(slot, byte)| *slot = byte);
                }
                p.g.write_bytes(p.own, at::UPPER, &table);
                *phase = phase::OPENED;
                Self::open(p, 0)
            }
            phase::OPENED => {
                let file = u16::from(p.get8(at::OPENING));
                let opened = match child {
                    Ok(handle) => u16::try_from(handle).unwrap_or(0),
                    Err(Fault::Error(error)) => {
                        *phase = phase::SAID;
                        let said = [
                            &b"fc: cannot open "[..],
                            &name(p, file),
                            b" - ",
                            open_error(error),
                            b"\r\n",
                        ]
                        .concat();
                        return Flow::Call(say(p, &said));
                    }
                    Err(fault) => return Flow::Return(Err(fault)),
                };
                p.set16(at::HANDLES + file * 2, opened);
                if file == 0 {
                    return Self::open(p, 1);
                }
                for file in 0..2 {
                    p.set16(file_at(file) + at::AT, 0);
                    p.set16(file_at(file) + at::END, 0);
                    p.set8(file_at(file) + at::DONE, 0);
                }
                *phase = phase::COMPARED;
                if p.get16(at::SWITCHES) & switch::BINARY != 0 {
                    return Flow::Call(Frame::of(&FcBytes::default()));
                }
                Flow::Call(Frame::of(&FcLines::default()))
            }
            _ => Flow::Return(child),
        }
    }
}
