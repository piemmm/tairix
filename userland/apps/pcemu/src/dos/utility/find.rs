//! `FIND [/V] [/C] [/N] [/I] "string" [file ...]`: the lines of each file,
//! or of the standard input, that hold the string — or with `/V` do not —
//! counted with `/C` and numbered with `/N`, its case ignored with `/I`.
//! Each file is headed by its name. It ends with `0` once a line was found,
//! `1` when none was, and `2` when a file could not be searched.

use super::{begin_reading, line, refuse, write_line, NextLine, Refusal, FAILED};
use crate::dos::fs::error;
use crate::dos::message::{self, decimal};
use crate::dos::program::{say, say_error, Flow, Frame, Id, Int, Program, Routine};
use crate::dos::shell::{answered, asciz_at, parse, set_asciz};
use crate::frames::{locals, take, Fault, Outcome};
use alloc::vec::Vec;

/// Where `FIND`'s state lies in its memory.
pub mod at {
    use crate::dos::program::at::STATE;

    /// The [`super::switch`]es given.
    pub const SWITCHES: u16 = STATE;
    /// Whether a file could not be searched.
    pub const FAILED: u16 = STATE + 0x01;
    /// Lines counted in every input, and in the one being searched; that
    /// one's lines read.
    pub const FOUND: u16 = STATE + 0x02;
    pub const COUNT: u16 = STATE + 0x06;
    pub const NUMBER: u16 = STATE + 0x0A;
    /// The string, its length first.
    pub const STRING: u16 = STATE + 0x10;
    pub const STRING_MAX: u16 = 127;
    /// The country's upper case of the characters from `80h`.
    pub const UPPER: u16 = STRING + 1 + STRING_MAX;
    /// The file being searched, ASCIZ.
    pub const NAME: u16 = UPPER + 0x80;
    pub const NAME_LEN: u16 = 0x50;
    pub const READER: u16 = NAME + NAME_LEN;
    /// Bytes of `FIND`'s state.
    pub const LEN: u16 = READER - STATE + super::super::reader::LEN;
}

/// What [`at::SWITCHES`] holds.
pub mod switch {
    pub const NOT: u8 = 0x01;
    pub const COUNT: u8 = 0x02;
    pub const NUMBER: u8 = 0x04;
    pub const IGNORE_CASE: u8 = 0x08;
}

const NAME: &[u8] = b"FIND: ";

/// What `FIND`'s command line asks for.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Request {
    pub switches: u8,
    pub string: Vec<u8>,
    pub files: Vec<Vec<u8>>,
}

/// `FIND`'s command line read, or what is wrong with it.
pub fn request(tail: &[u8]) -> Result<Request, Refusal<'_>> {
    let mut out = Request::default();
    let mut string = None;
    for word in parse::arguments(tail) {
        if let Some(letter) = word.strip_prefix(b"/") {
            out.switches |= match letter.to_ascii_uppercase().as_slice() {
                b"V" => switch::NOT,
                b"C" => switch::COUNT,
                b"N" => switch::NUMBER,
                b"I" => switch::IGNORE_CASE,
                _ => return Err(Refusal::Switch(word)),
            };
        } else if string.is_none() {
            string = Some(quoted(word).ok_or(Refusal::Format(word))?);
        } else {
            out.files.push(word.to_ascii_uppercase());
        }
    }
    out.string = string.ok_or(Refusal::Missing)?;
    Ok(out)
}

/// The string a quoted word spells, `""` in it a quote.
fn quoted(word: &[u8]) -> Option<Vec<u8>> {
    let mut rest = word.strip_prefix(b"\"")?.iter();
    let mut out = Vec::new();
    loop {
        match rest.next()? {
            b'"' if rest.as_slice().first() == Some(&b'"') => {
                rest.next();
                out.push(b'"');
            }
            b'"' => return rest.as_slice().is_empty().then_some(out),
            &byte => out.push(byte),
        }
    }
}

/// `byte` in upper case, through the country's table from `80h`.
fn upper(byte: u8, table: &[u8; 128]) -> u8 {
    match byte {
        0x80.. => table[usize::from(byte - 0x80)],
        _ => byte.to_ascii_uppercase(),
    }
}

/// Whether `line` holds `string`; an empty string is in every line.
fn holds(line: &[u8], string: &[u8]) -> bool {
    string.is_empty() || line.windows(string.len()).any(|window| window == string)
}

locals! {
    /// `FIND`'s main routine: the command line read, then each file, or
    /// the standard input, searched.
    pub struct Find {
        file: u8,
        handle: u16,
    }
}

mod phase {
    pub const REFUSED: u8 = 1;
    pub const TABLE: u8 = 2;
    pub const NEXT: u8 = 3;
    pub const OPENED: u8 = 4;
    pub const HEADED: u8 = 5;
    pub const SEARCHED: u8 = 6;
    pub const COUNTED: u8 = 7;
    pub const CLOSED: u8 = 8;
}

impl Find {
    /// The files the command line names.
    fn files(p: &mut Program<'_, '_>) -> Vec<Vec<u8>> {
        let tail = p.tail();
        request(&tail)
            .map(|request| request.files)
            .unwrap_or_default()
    }

    /// The count of the input just searched, after its heading for a file.
    fn count_line(p: &mut Program<'_, '_>, file: bool) -> Vec<u8> {
        let count = p.get32(at::COUNT);
        let mut out = if file { b": ".to_vec() } else { Vec::new() };
        out.extend_from_slice(&decimal(count));
        out.extend_from_slice(b"\r\n");
        out
    }

    /// The command line read and kept, and the country's upper case asked
    /// for when case is ignored.
    fn start(&mut self, p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        let tail = p.tail();
        let request = match request(&tail) {
            Ok(request) => request,
            Err(refusal) => {
                *phase = phase::REFUSED;
                return Flow::Call(refuse(p, NAME, refusal));
            }
        };
        p.set8(at::SWITCHES, request.switches);
        p.set8(at::FAILED, 0);
        p.set32(at::FOUND, 0);
        let len = request.string.len().min(usize::from(at::STRING_MAX));
        p.set8(at::STRING, u8::try_from(len).unwrap_or(0));
        p.g.write_bytes(p.own, at::STRING + 1, &request.string[..len]);
        *phase = phase::TABLE;
        if request.switches & switch::IGNORE_CASE == 0 {
            return self.step(p, phase, Ok(0));
        }
        Flow::Call(Frame::of(&Int {
            bx: 0xFFFF,
            cx: 5,
            dx: 0xFFFF,
            di: p.scratch(0),
            ..Int::dos(p.own, 0x6502)
        }))
    }

    /// The country's upper case, the string put in it: the table found, or
    /// none put in its place when the country gives none.
    fn fold(p: &mut Program<'_, '_>, given: bool) {
        let mut table = [0u8; 128];
        if given {
            let record = p.scratch(0);
            let pointer = p.get_far(record + 1);
            p.g.read_bytes(pointer.segment, pointer.offset.wrapping_add(2), &mut table);
        } else {
            for (slot, byte) in table.iter_mut().zip(0x80u8..) {
                *slot = byte;
            }
        }
        p.g.write_bytes(p.own, at::UPPER, &table);
        let len = p.get8(at::STRING);
        let mut string = alloc::vec![0u8; usize::from(len)];
        p.g.read_bytes(p.own, at::STRING + 1, &mut string);
        let folded: Vec<u8> = string.iter().map(|&b| upper(b, &table)).collect();
        p.g.write_bytes(p.own, at::STRING + 1, &folded);
    }

    /// A file that would not open: said so, unless the critical-error
    /// handler already has, and the next one searched.
    fn not_opened(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        p.set8(at::FAILED, 1);
        *phase = phase::NEXT;
        if answered(child) {
            return self.step(p, phase, Ok(0));
        }
        let what = if child == Err(Fault::Error(error::ACCESS_DENIED)) {
            message::ACCESS_DENIED
        } else {
            message::FILE_NOT_FOUND
        };
        let name = asciz_at(p, at::NAME, at::NAME_LEN);
        let text = [NAME, &message::named(what, &name)].concat();
        Flow::Call(say_error(p, &text))
    }

    fn code(p: &mut Program<'_, '_>) -> u32 {
        if p.get8(at::FAILED) != 0 {
            FAILED
        } else {
            u32::from(p.get32(at::FOUND) == 0)
        }
    }
}

impl Routine for Find {
    const ID: Id = Id::Find;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        match *phase {
            0 => self.start(p, phase),
            phase::REFUSED => Flow::Return(Ok(FAILED)),
            phase::TABLE => {
                if p.get8(at::SWITCHES) & switch::IGNORE_CASE != 0 {
                    Self::fold(p, child.is_ok());
                }
                self.file = 0;
                if Self::files(p).is_empty() {
                    *phase = phase::SEARCHED;
                    return Flow::Call(Frame::of(&Search::of(0)));
                }
                *phase = phase::NEXT;
                self.step(p, phase, Ok(0))
            }
            phase::NEXT => {
                let files = Self::files(p);
                let Some(name) = files.get(usize::from(self.file)) else {
                    return Flow::Return(Ok(Self::code(p)));
                };
                let name = name.clone();
                self.file += 1;
                set_asciz(p, at::NAME, &name, at::NAME_LEN);
                *phase = phase::OPENED;
                Flow::Call(Frame::of(&Int {
                    dx: at::NAME,
                    ..Int::dos(p.own, 0x3D00)
                }))
            }
            phase::OPENED => {
                let Ok(handle) = child else {
                    return self.not_opened(p, phase, child);
                };
                self.handle = u16::try_from(handle).unwrap_or(0);
                let name = asciz_at(p, at::NAME, at::NAME_LEN);
                let mut heading = [&b"\r\n---------- "[..], &name].concat();
                if p.get8(at::SWITCHES) & switch::COUNT == 0 {
                    heading.extend_from_slice(b"\r\n");
                }
                *phase = phase::HEADED;
                Flow::Call(say(p, &heading))
            }
            phase::HEADED => {
                *phase = phase::SEARCHED;
                Flow::Call(Frame::of(&Search::of(self.handle)))
            }
            phase::SEARCHED => {
                if child.is_err() && !answered(child) {
                    p.set8(at::FAILED, 1);
                }
                let file = self.handle != 0;
                *phase = phase::COUNTED;
                if p.get8(at::SWITCHES) & switch::COUNT == 0 || answered(child) {
                    return self.step(p, phase, Ok(0));
                }
                let text = Self::count_line(p, file);
                Flow::Call(say(p, &text))
            }
            phase::COUNTED => {
                if self.handle == 0 {
                    return Flow::Return(Ok(Self::code(p)));
                }
                *phase = phase::CLOSED;
                Flow::Call(Frame::of(&Int {
                    bx: self.handle,
                    ..Int::dos(p.own, 0x3E00)
                }))
            }
            phase::CLOSED => {
                take!(child);
                *phase = phase::NEXT;
                self.step(p, phase, Ok(0))
            }
            _ => Flow::Return(Ok(Self::code(p))),
        }
    }
}

locals! {
    /// One input searched, line by line: each line the switches take
    /// counted, and written unless only a count is asked for.
    pub struct Search {
        handle: u16,
    }
}

mod search {
    pub const READ: u8 = 1;
    pub const NUMBERED: u8 = 2;
    pub const WRITTEN: u8 = 3;
    pub const ENDED: u8 = 4;
}

impl Search {
    const fn of(handle: u16) -> Self {
        Self { handle }
    }

    fn next(self, phase: &mut u8) -> Flow {
        *phase = search::READ;
        Flow::Call(Frame::of(&NextLine::of(self.handle, at::READER)))
    }

    /// Whether the line the reader holds is one the switches take.
    fn takes(p: &mut Program<'_, '_>) -> bool {
        let switches = p.get8(at::SWITCHES);
        let mut text = line(p, at::READER);
        if switches & switch::IGNORE_CASE != 0 {
            let mut table = [0u8; 128];
            p.g.read_bytes(p.own, at::UPPER, &mut table);
            for byte in &mut text {
                *byte = upper(*byte, &table);
            }
        }
        let len = p.get8(at::STRING);
        let mut string = alloc::vec![0u8; usize::from(len)];
        p.g.read_bytes(p.own, at::STRING + 1, &mut string);
        holds(&text, &string) != (switches & switch::NOT != 0)
    }
}

impl Routine for Search {
    const ID: Id = Id::FindSearch;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        match *phase {
            0 => {
                begin_reading(p, at::READER);
                p.set32(at::COUNT, 0);
                p.set32(at::NUMBER, 0);
                self.next(phase)
            }
            search::READ => {
                if take!(child) == 0 {
                    return Flow::Return(Ok(0));
                }
                let number = p.get32(at::NUMBER).wrapping_add(1);
                p.set32(at::NUMBER, number);
                if !Self::takes(p) {
                    return self.next(phase);
                }
                for counter in [at::COUNT, at::FOUND] {
                    let n = p.get32(counter).wrapping_add(1);
                    p.set32(counter, n);
                }
                let switches = p.get8(at::SWITCHES);
                if switches & switch::COUNT != 0 {
                    return self.next(phase);
                }
                *phase = search::NUMBERED;
                if switches & switch::NUMBER == 0 {
                    return self.step(p, phase, Ok(0));
                }
                let text = [&b"["[..], &decimal(number), b"]"].concat();
                Flow::Call(say(p, &text))
            }
            search::NUMBERED => {
                take!(child);
                *phase = search::WRITTEN;
                Flow::Call(write_line(p, at::READER))
            }
            search::WRITTEN => {
                take!(child);
                *phase = search::ENDED;
                Flow::Call(say(p, b"\r\n"))
            }
            search::ENDED => {
                take!(child);
                self.next(phase)
            }
            _ => Flow::Return(Ok(0)),
        }
    }
}
