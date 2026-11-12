//! The file named on `CHKDSK`'s command line: each file it matches, in the
//! directory its path names, said with the pieces its clusters lie in when
//! they are not one run — DOS 4.0's "non-contiguous blocks" — from the FAT
//! as `CHKDSK` left it.

use super::cells;
use super::{at, current_directory, held, path_with, spelt, volume};
use crate::dos::message::{self, decimal};
use crate::dos::program::{say, say_error, Flow, Frame, Id, Int, Program, Returned, Routine};
use crate::dos::shell::{asciz_at, set_asciz};
use crate::frames::{locals, take, Outcome};

/// Where a normal FCB search's entry lies in the DTA, after the drive.
const FOUND: u16 = at::DTA + 1;
/// Where the entry keeps its name and its first cluster.
const NAME: u16 = 0x00;
const CLUSTER: u16 = 0x1A;

locals! {
    /// The pieces of each file the named one matches.
    pub struct ChkdskExtents {
        /// The directory was changed for the search.
        changed: bool,
        /// A file matched, and one of them lies in pieces.
        matched: bool,
        broken: bool,
        /// Where the name lies in the path given.
        name_at: u16,
        /// The pieces of the file found.
        pieces: u32,
    }
}

mod phase {
    pub const CHANGED: u8 = 1;
    pub const PARSED: u8 = 2;
    pub const BLANK: u8 = 3;
    pub const DTA_SET: u8 = 4;
    pub const FOUND: u8 = 5;
    pub const AT: u8 = 6;
    pub const SAID: u8 = 7;
    pub const NO_PATH: u8 = 8;
    pub const ENDED: u8 = 9;
    pub const BACK: u8 = 10;
}

impl ChkdskExtents {
    /// The name parsed into the FCB, the drive's, as DOS's `CHKDSK` parses it.
    fn parse(&self, p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        let mut fcb = [0u8; 37];
        fcb[0] = p.get8(at::DRIVE) + 1;
        p.g.write_bytes(p.own, at::FCB, &fcb);
        *phase = phase::PARSED;
        Flow::Call(Frame::of(&Int {
            si: at::SPEC + self.name_at,
            di: at::FCB,
            ..Int::dos(p.own, 0x2902)
        }))
    }

    /// Done: the user's directory put back, if it was changed.
    fn end(self, p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        if !self.changed {
            return Flow::Return(Ok(0));
        }
        *phase = phase::BACK;
        Flow::Call(Frame::of(&Int {
            dx: at::USER_DIR,
            ..Int::dos(p.own, 0x3B00)
        }))
    }

    fn next(p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        *phase = phase::FOUND;
        Flow::Call(Frame::of(&Int {
            dx: at::FCB,
            ..Int::dos(p.own, 0x1200)
        }))
    }

    /// What a search for the next match answered: the end of the matches, or
    /// a file whose clusters are counted into the pieces they lie in.
    fn found(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        if take!(child).to_le_bytes()[0] != 0 {
            let words: &[u8] = match (self.matched, self.broken) {
                (false, _) => message::FILE_NOT_FOUND,
                (true, false) => b"All specified file(s) are contiguous\r\n",
                (true, true) => return self.end(p, phase),
            };
            *phase = phase::ENDED;
            return Flow::Call(if self.matched {
                say(p, words)
            } else {
                say_error(p, words)
            });
        }
        self.matched = true;
        let first = p.get16(FOUND + CLUSTER);
        let volume = volume(p);
        self.pieces = cells::breaks(&mut held(p), volume, first) + 1;
        if self.pieces == 1 {
            return Self::next(p, phase);
        }
        self.broken = true;
        *phase = phase::AT;
        Flow::Call(current_directory(p))
    }
}

impl Routine for ChkdskExtents {
    const ID: Id = Id::ChkdskExtents;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        match *phase {
            0 => {
                let spec = asciz_at(p, at::SPEC, at::SPEC_LEN);
                let Some(slash) = spec.iter().rposition(|&b| b == b'\\') else {
                    self.name_at = if spec.get(1) == Some(&b':') { 2 } else { 0 };
                    return self.parse(p, phase);
                };
                self.name_at = u16::try_from(slash + 1).unwrap_or(0);
                let root = slash == 0 || (slash == 2 && spec[1] == b':');
                let directory = &spec[..if root { slash + 1 } else { slash }];
                set_asciz(p, at::BUFFER, directory, at::BUFFER_LEN);
                p.set32(at::BUFFERED, u32::MAX);
                *phase = phase::CHANGED;
                Flow::Call(Frame::of(&Int {
                    dx: at::BUFFER,
                    ..Int::dos(p.own, 0x3B00)
                }))
            }
            phase::CHANGED => {
                if child.is_err() {
                    *phase = phase::NO_PATH;
                    return Flow::Call(say(p, super::text::NEW_LINE));
                }
                self.changed = true;
                self.parse(p, phase)
            }
            phase::PARSED => {
                take!(child);
                let past = p.result(Returned::Si);
                if p.get8(past) != 0 {
                    p.set8(at::FCB, 0xFF);
                }
                *phase = phase::BLANK;
                Flow::Call(say(p, super::text::NEW_LINE))
            }
            phase::BLANK => {
                take!(child);
                *phase = phase::DTA_SET;
                Flow::Call(Frame::of(&Int {
                    dx: at::DTA,
                    ..Int::dos(p.own, 0x1A00)
                }))
            }
            phase::DTA_SET => {
                take!(child);
                *phase = phase::FOUND;
                Flow::Call(Frame::of(&Int {
                    dx: at::FCB,
                    ..Int::dos(p.own, 0x1100)
                }))
            }
            phase::FOUND => self.found(p, phase, child),
            phase::AT => {
                let mut field = [0u8; 11];
                p.g.read_bytes(p.own, FOUND + NAME, &mut field);
                let path = path_with(p, &spelt(&field));
                let line = [
                    &path[..],
                    b" Contains ",
                    &decimal(self.pieces),
                    b" non-contiguous blocks\r\n",
                ]
                .concat();
                *phase = phase::SAID;
                Flow::Call(say(p, &line))
            }
            phase::SAID => {
                take!(child);
                Self::next(p, phase)
            }
            phase::NO_PATH => {
                take!(child);
                *phase = phase::ENDED;
                Flow::Call(say_error(p, message::PATH_NOT_FOUND))
            }
            phase::ENDED => self.end(p, phase),
            _ => Flow::Return(Ok(0)),
        }
    }
}
