//! The files of one directory `XCOPY` copies: those its pattern matches and
//! its switches take, each to its place below the destination, the
//! directories there made as the first file needs them, or at once with
//! `/E`.

use super::{at, code, switch, text};
use crate::dos::dirops::finding;
use crate::dos::program::{answer_key, say, Flow, Frame, Id, Int, Key, Program, Returned, Routine};
use crate::dos::shell::copy::{error, mapped, CopyFile};
use crate::dos::shell::{asciz_at, set_asciz};
use crate::frames::{locals, take, Fault, Outcome};
use alloc::vec::Vec;
use tairix_fat::dir::attr;

/// What a search for the files matches: plain files alone, as DOS's `XCOPY`
/// copies.
const PLAIN: u16 = 0x00;

/// What the files' `\`-ended directory is below the source's starting one.
fn below(p: &mut Program<'_, '_>) -> Vec<u8> {
    let (walked, base) = (
        asciz_at(p, at::WALKED, at::PATH_LEN),
        asciz_at(p, at::BASE, at::PATH_LEN),
    );
    walked.get(base.len()..).unwrap_or_default().to_vec()
}

/// The directory the files go to: the destination's, and as far below it
/// as they are below the source's.
fn destination(p: &mut Program<'_, '_>) -> Vec<u8> {
    [asciz_at(p, at::TARGET, at::PATH_LEN), below(p)].concat()
}

locals! {
    /// One directory's files copied: gives `0` to go on, else the copy
    /// stops.
    pub struct XCopyFiles {
        making: u8,
        then: u8,
        key: u8,
    }
}

mod phase {
    pub const DTA_SET: u8 = 1;
    pub const FOUND: u8 = 2;
    pub const ASKED: u8 = 3;
    pub const ANSWERED: u8 = 4;
    pub const ECHOED: u8 = 5;
    pub const MAKING: u8 = 6;
    pub const SAID: u8 = 7;
    pub const COPIED: u8 = 8;
    pub const ARCHIVE_READ: u8 = 9;
    pub const NEXT: u8 = 10;
    pub const STOPPED: u8 = 11;
    pub const MADE_EMPTY: u8 = 12;
    pub const MADE_FOR_FILE: u8 = 13;
}

impl XCopyFiles {
    /// The next directory of the destination's path made, from the drive's
    /// root down; those already there are passed by. `then` follows once
    /// all are.
    fn make(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, then: u8) -> Flow {
        let path = destination(p);
        let ends: Vec<usize> = path
            .iter()
            .enumerate()
            .filter(|&(at, &b)| b == b'\\' && at > 2)
            .map(|(at, _)| at)
            .collect();
        let Some(&end) = ends.get(usize::from(self.making)) else {
            p.set8(at::MADE, 1);
            *phase = then;
            return self.step(p, phase, Ok(0));
        };
        self.making += 1;
        self.then = then;
        set_asciz(p, at::TO, &path[..end], at::PATH_LEN);
        *phase = phase::MAKING;
        Flow::Call(Frame::of(&Int {
            dx: at::TO,
            ..Int::dos(p.own, 0x3900)
        }))
    }

    /// The file found listed, its directory made first if it is not.
    fn take_file(&mut self, p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        if p.get8(at::MADE) == 0 {
            self.making = 0;
            return self.make(p, phase, phase::MADE_FOR_FILE);
        }
        let name = asciz_at(p, at::DTA + finding::NAME, 13);
        let from = [asciz_at(p, at::WALKED, at::PATH_LEN), name.clone()].concat();
        set_asciz(p, at::FROM, &from, at::PATH_LEN);
        let given = asciz_at(p, at::NAME, at::NAME_LEN);
        let named = match (p.get8(at::INTO_FILE) != 0, given.is_empty()) {
            (true, _) => given,
            (false, true) => name.clone(),
            (false, false) => mapped(&name, &given).unwrap_or(name.clone()),
        };
        let to = [destination(p), named].concat();
        set_asciz(p, at::TO, &to, at::PATH_LEN);
        *phase = phase::SAID;
        if p.get16(at::SWITCHES) & switch::PROMPT != 0 {
            return self.step(p, phase, Ok(0));
        }
        let shown = [
            asciz_at(p, at::SHOWN, at::PATH_LEN),
            below(p),
            name,
            b"\r\n".to_vec(),
        ]
        .concat();
        Flow::Call(say(p, &shown))
    }

    /// The copy stopped: `said` told and `code` kept for `XCOPY` to end
    /// with.
    fn stop(p: &mut Program<'_, '_>, phase: &mut u8, code: u8, said: &[u8]) -> Flow {
        p.set8(at::CODE, code);
        *phase = phase::STOPPED;
        Flow::Call(say(p, said))
    }

    fn next(p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        *phase = phase::FOUND;
        Flow::Call(Frame::of(&Int::dos(p.own, 0x4F00)))
    }
}

impl Routine for XCopyFiles {
    const ID: Id = Id::XCopyFiles;

    #[allow(
        clippy::too_many_lines,
        reason = "one arm a phase, in the order a file is copied"
    )]
    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        match *phase {
            0 => {
                if p.get16(at::SWITCHES) & switch::EMPTY_TOO != 0 && p.get8(at::MADE) == 0 {
                    self.making = 0;
                    return self.make(p, phase, phase::MADE_EMPTY);
                }
                *phase = phase::DTA_SET;
                Flow::Call(Frame::of(&Int {
                    dx: at::DTA,
                    ..Int::dos(p.own, 0x1A00)
                }))
            }
            phase::MADE_EMPTY => {
                *phase = phase::DTA_SET;
                Flow::Call(Frame::of(&Int {
                    dx: at::DTA,
                    ..Int::dos(p.own, 0x1A00)
                }))
            }
            phase::DTA_SET => {
                let pattern = [
                    asciz_at(p, at::WALKED, at::PATH_LEN),
                    asciz_at(p, at::PATTERN, at::NAME_LEN),
                ]
                .concat();
                set_asciz(p, at::FROM, &pattern, at::PATH_LEN);
                *phase = phase::FOUND;
                Flow::Call(Frame::of(&Int {
                    cx: PLAIN,
                    dx: at::FROM,
                    ..Int::dos(p.own, 0x4E00)
                }))
            }
            phase::FOUND => {
                if child.is_err() {
                    return Flow::Return(Ok(0));
                }
                p.set8(at::FOUND, 1);
                let switches = p.get16(at::SWITCHES);
                let attribute = u16::from(p.get8(at::DTA + finding::ATTRIBUTE));
                let archived = switches & (switch::ARCHIVED | switch::MOVE_ARCHIVE) == 0
                    || attribute & u16::from(attr::ARCHIVE) != 0;
                let recent = switches & switch::SINCE == 0
                    || p.get16(at::DTA + finding::DATE) >= p.get16(at::SINCE);
                if !(archived && recent) {
                    return Self::next(p, phase);
                }
                if switches & switch::PROMPT == 0 {
                    return self.take_file(p, phase);
                }
                let name = asciz_at(p, at::DTA + finding::NAME, 13);
                let asked = [
                    asciz_at(p, at::SHOWN, at::PATH_LEN),
                    below(p),
                    name,
                    b" (Y/N)?".to_vec(),
                ]
                .concat();
                *phase = phase::ASKED;
                Flow::Call(say(p, &asked))
            }
            phase::ASKED => {
                take!(child);
                *phase = phase::ANSWERED;
                Flow::Call(Frame::of(&Key::of(0x0C01)))
            }
            // An input with no answer left answers no to what is left.
            phase::ANSWERED => {
                self.key = answer_key(take!(child));
                *phase = phase::ECHOED;
                if !matches!(self.key, b'Y' | b'N') {
                    *phase = phase::FOUND;
                    return self.step(p, phase, Ok(0));
                }
                Flow::Call(say(p, b"\r\n"))
            }
            phase::ECHOED => {
                take!(child);
                if p.get16(at::SWITCHES) & switch::PROMPT != 0 && self.key != b'Y' {
                    return Self::next(p, phase);
                }
                self.take_file(p, phase)
            }
            // A directory already there is no failure: the copy into it
            // finds out whether it can be written.
            phase::MAKING => {
                let then = self.then;
                self.make(p, phase, then)
            }
            phase::MADE_FOR_FILE => self.take_file(p, phase),
            phase::SAID => {
                take!(child);
                *phase = phase::COPIED;
                Flow::Call(Frame::of(&CopyFile::between(
                    at::FROM,
                    at::TO,
                    at::BUFFER,
                    at::BUFFER_LEN,
                )))
            }
            phase::COPIED => match child {
                Ok(_) => {
                    let copied = p.get32(at::COPIED).saturating_add(1);
                    p.set32(at::COPIED, copied);
                    if p.get16(at::SWITCHES) & switch::MOVE_ARCHIVE == 0 {
                        return Self::next(p, phase);
                    }
                    *phase = phase::ARCHIVE_READ;
                    Flow::Call(Frame::of(&Int {
                        dx: at::FROM,
                        ..Int::dos(p.own, 0x4300)
                    }))
                }
                Err(Fault::Error(error::SELF)) => {
                    Self::stop(p, phase, code::REFUSED, text::ONTO_ITSELF)
                }
                Err(Fault::Error(error::FULL)) => {
                    Self::stop(p, phase, code::WRITE_FAILED, text::DISK_FULL)
                }
                Err(Fault::Error(_)) => Self::stop(p, phase, code::WRITE_FAILED, text::CREATION),
                Err(fault) => Flow::Return(Err(fault)),
            },
            phase::ARCHIVE_READ => {
                let attribute = if child.is_ok() {
                    p.result(Returned::Cx)
                } else {
                    0
                };
                *phase = phase::NEXT;
                Flow::Call(Frame::of(&Int {
                    cx: attribute & u16::from(attr::READ_ONLY | attr::HIDDEN | attr::SYSTEM),
                    dx: at::FROM,
                    ..Int::dos(p.own, 0x4301)
                }))
            }
            phase::NEXT => Self::next(p, phase),
            _ => {
                take!(child);
                Flow::Return(Ok(1))
            }
        }
    }
}
