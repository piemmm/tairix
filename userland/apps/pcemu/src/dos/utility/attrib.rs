//! `ATTRIB [+R|-R] [+A|-A] [+S|-S] [+H|-H] [path] [/S]`: the attributes of
//! the files a pattern matches, shown or changed, in its directory and with
//! `/S` in every directory below. A hidden or system file keeps the other
//! attributes it has unless the line names its hidden or system one too.

use super::{refuse, Refusal};
use crate::dos::dirops::finding;
use crate::dos::message;
use crate::dos::program::walk::{self, begin_walk, Walk};
use crate::dos::program::{say, say_error, Flow, Frame, Id, Int, Program, Routine};
use crate::dos::shell::{answered, asciz_at, parse, set_asciz};
use crate::frames::{locals, take, Outcome};
use alloc::vec::Vec;
use tairix_fat::dir::attr;

/// Where `ATTRIB`'s state lies in its memory.
pub mod at {
    use crate::dos::program::at::STATE;
    use crate::dos::program::walk::walker;

    /// The attributes to set and to clear, `/S`, and whether any file
    /// matched.
    pub const SET: u16 = STATE;
    pub const CLEAR: u16 = STATE + 0x01;
    pub const SUBDIRECTORIES: u16 = STATE + 0x02;
    pub const MATCHED: u16 = STATE + 0x03;
    /// The walk's block, and the walk.
    pub const STACK: u16 = STATE + 0x04;
    pub const WALK: u16 = STATE + 0x06;
    /// The names a file in the directory being searched must match.
    pub const PATTERN: u16 = WALK + walker::LEN;
    pub const PATTERN_LEN: u16 = 0x10;
    /// The directory being searched, `\`-ended ASCIZ, and the canonical
    /// path the line named.
    pub const PATH: u16 = PATTERN + PATTERN_LEN;
    pub const PATH_LEN: u16 = 0x50;
    pub const NAMED: u16 = PATH + PATH_LEN;
    /// A file's path, ASCIZ, for the calls that read and set its
    /// attributes; then the DTA searches report into.
    pub const FILE: u16 = NAMED + PATH_LEN;
    pub const FILE_LEN: u16 = 0x60;
    pub const DTA: u16 = FILE + FILE_LEN;
    /// Bytes of `ATTRIB`'s state.
    pub const LEN: u16 = DTA + 0x30 - STATE;
}

/// What a search for the files matches: everything but directories and
/// labels.
const FILES: u16 = (attr::READ_ONLY | attr::HIDDEN | attr::SYSTEM) as u16;

/// What `ATTRIB`'s command line asks for.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Request {
    pub set: u8,
    pub clear: u8,
    pub subdirectories: bool,
    pub path: Vec<u8>,
}

/// `ATTRIB`'s command line read, or what is wrong with it.
pub fn request(tail: &[u8]) -> Result<Request, Refusal<'_>> {
    let mut out = Request::default();
    let mut named = false;
    for word in parse::arguments(tail) {
        if let Some(switch) = word.strip_prefix(b"/") {
            if !switch.eq_ignore_ascii_case(b"S") {
                return Err(Refusal::Switch(word));
            }
            out.subdirectories = true;
        } else if matches!(word.first(), Some(b'+' | b'-')) {
            for pair in word.chunks(2) {
                let bit = match pair.get(1).map(u8::to_ascii_uppercase) {
                    Some(b'R') => attr::READ_ONLY,
                    Some(b'A') => attr::ARCHIVE,
                    Some(b'S') => attr::SYSTEM,
                    Some(b'H') => attr::HIDDEN,
                    _ => return Err(Refusal::Format(word)),
                };
                match pair[0] {
                    b'+' => out.set |= bit,
                    b'-' => out.clear |= bit,
                    _ => return Err(Refusal::Format(word)),
                }
            }
        } else if named {
            return Err(Refusal::TooMany(word));
        } else {
            named = true;
            out.path = word.to_vec();
        }
    }
    if out.set & out.clear != 0 {
        return Err(Refusal::Format(b"+-"));
    }
    Ok(out)
}

/// The line `ATTRIB` shows a file with, as DOS's lists them.
#[must_use]
pub fn shown(attributes: u8, path: &[u8]) -> Vec<u8> {
    let mark = |bit: u8, letter: u8| if attributes & bit != 0 { letter } else { b' ' };
    let mut out = alloc::vec![b' '; 11];
    out[0] = mark(attr::ARCHIVE, b'A');
    out[5] = mark(attr::SYSTEM, b'S');
    out[6] = mark(attr::HIDDEN, b'H');
    out[7] = mark(attr::READ_ONLY, b'R');
    out.extend_from_slice(path);
    out.extend_from_slice(b"\r\n");
    out
}

/// Why a change is not made to a file with `attributes`: it is hidden or
/// system, and the line changes the rest without naming that one.
#[must_use]
pub fn kept(attributes: u8, set: u8, clear: u8) -> Option<&'static [u8]> {
    let named = set | clear;
    if attributes & attr::HIDDEN != 0 && named & attr::HIDDEN == 0 {
        return Some(b"Not resetting hidden file - ");
    }
    if attributes & attr::SYSTEM != 0 && named & attr::SYSTEM == 0 {
        return Some(b"Not resetting system file - ");
    }
    None
}

locals! {
    /// `ATTRIB`'s main routine.
    pub struct Attrib {}
}

mod phase {
    pub const REFUSED: u8 = 1;
    pub const NAMED: u8 = 2;
    pub const STACKED: u8 = 3;
    pub const DTA_SET: u8 = 4;
    pub const FOUND: u8 = 5;
    pub const DONE_FILE: u8 = 6;
    pub const WALKED: u8 = 7;
    pub const MISSED: u8 = 8;
    pub const REPORTED: u8 = 9;
    pub const SHRUNK: u8 = 10;
}

impl Attrib {
    /// The command line read into the request, and its path named in full.
    fn begin(phase: &mut u8, p: &mut Program<'_, '_>) -> Flow {
        let tail = p.tail();
        let request = match request(&tail) {
            Ok(request) => request,
            Err(refusal) => {
                *phase = phase::REFUSED;
                return Flow::Call(refuse(p, b"", refusal));
            }
        };
        p.set8(at::SET, request.set);
        p.set8(at::CLEAR, request.clear);
        p.set8(at::SUBDIRECTORIES, u8::from(request.subdirectories));
        p.set8(at::MATCHED, 0);
        let path = if request.path.is_empty() {
            b"*.*".to_vec()
        } else {
            request.path
        };
        set_asciz(p, at::FILE, &path, at::FILE_LEN);
        *phase = phase::NAMED;
        Flow::Call(Frame::of(&Int {
            si: at::FILE,
            di: at::NAMED,
            ..Int::dos(p.own, 0x6000)
        }))
    }

    /// Searches the directory the walk names, its DTA set first: the walk's
    /// own searches report elsewhere.
    fn search_directory(phase: &mut u8, p: &mut Program<'_, '_>) -> Flow {
        *phase = phase::DTA_SET;
        Flow::Call(Frame::of(&Int {
            dx: at::DTA,
            ..Int::dos(p.own, 0x1A00)
        }))
    }

    /// The next step of the walk, or the end without one.
    fn walk_on(phase: &mut u8, p: &mut Program<'_, '_>) -> Flow {
        if p.get8(at::SUBDIRECTORIES) == 0 {
            return Self::end(phase, p);
        }
        *phase = phase::WALKED;
        Flow::Call(Frame::of(&Walk::of(at::WALK)))
    }

    fn end(phase: &mut u8, p: &mut Program<'_, '_>) -> Flow {
        if p.get8(at::MATCHED) != 0 {
            return Flow::Return(Ok(0));
        }
        *phase = phase::MISSED;
        let named = asciz_at(p, at::NAMED, at::PATH_LEN);
        Flow::Call(say_error(
            p,
            &message::named(message::FILE_NOT_FOUND, &named),
        ))
    }

    /// The file the search found: its attributes shown, or changed.
    fn found(phase: &mut u8, p: &mut Program<'_, '_>) -> Flow {
        p.set8(at::MATCHED, 1);
        let attributes = p.get8(at::DTA + finding::ATTRIBUTE);
        let mut name = [0u8; 13];
        p.g.read_bytes(p.own, at::DTA + finding::NAME, &mut name);
        let name = &name[..name.iter().position(|&b| b == 0).unwrap_or(name.len())];
        let path = [asciz_at(p, at::PATH, at::PATH_LEN), name.to_vec()].concat();
        let (set, clear) = (p.get8(at::SET), p.get8(at::CLEAR));
        *phase = phase::DONE_FILE;
        if set | clear == 0 {
            return Flow::Call(say(p, &shown(attributes, &path)));
        }
        if let Some(why) = kept(attributes, set, clear) {
            return Flow::Call(say(p, &[why, &path, b"\r\n"].concat()));
        }
        set_asciz(p, at::FILE, &path, at::FILE_LEN);
        let new = (attributes & !clear | set)
            & (attr::READ_ONLY | attr::HIDDEN | attr::SYSTEM | attr::ARCHIVE);
        Flow::Call(Frame::of(&Int {
            cx: u16::from(new),
            dx: at::FILE,
            ..Int::dos(p.own, 0x4301)
        }))
    }
}

impl Routine for Attrib {
    const ID: Id = Id::Attrib;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        if answered(child) {
            return Flow::Return(Ok(1));
        }
        match *phase {
            0 => Self::begin(phase, p),
            phase::REFUSED | phase::MISSED => Flow::Return(Ok(1)),
            phase::NAMED => {
                if child.is_err() {
                    let named = asciz_at(p, at::FILE, at::FILE_LEN).to_ascii_uppercase();
                    *phase = phase::MISSED;
                    return Flow::Call(say_error(
                        p,
                        &[&b"Path not found - "[..], &named, b"\r\n"].concat(),
                    ));
                }
                let named = asciz_at(p, at::NAMED, at::PATH_LEN);
                let split = named
                    .iter()
                    .rposition(|&b| b == b'\\')
                    .map_or(0, |at| at + 1);
                set_asciz(p, at::PATH, &named[..split], at::PATH_LEN);
                set_asciz(p, at::PATTERN, &named[split..], at::PATTERN_LEN);
                if p.get8(at::SUBDIRECTORIES) == 0 {
                    return Self::search_directory(phase, p);
                }
                *phase = phase::SHRUNK;
                Flow::Call(p.shrink())
            }
            phase::SHRUNK => {
                take!(child);
                *phase = phase::STACKED;
                Flow::Call(Frame::of(&Int {
                    bx: walk::PARAGRAPHS,
                    ..Int::dos(p.own, 0x4800)
                }))
            }
            phase::STACKED => {
                let Ok(segment) = child else {
                    *phase = phase::MISSED;
                    return Flow::Call(say_error(p, message::MEMORY));
                };
                let segment = u16::try_from(segment).unwrap_or(0);
                p.set16(at::STACK, segment);
                begin_walk(p, at::WALK, segment, at::PATH, at::PATH_LEN, 0);
                Self::search_directory(phase, p)
            }
            phase::DTA_SET => {
                *phase = phase::FOUND;
                let pattern = [
                    asciz_at(p, at::PATH, at::PATH_LEN),
                    asciz_at(p, at::PATTERN, at::PATTERN_LEN),
                ]
                .concat();
                set_asciz(p, at::FILE, &pattern, at::FILE_LEN);
                Flow::Call(Frame::of(&Int {
                    cx: FILES,
                    dx: at::FILE,
                    ..Int::dos(p.own, 0x4E00)
                }))
            }
            phase::FOUND if child.is_ok() => Self::found(phase, p),
            phase::FOUND => Self::walk_on(phase, p),
            phase::DONE_FILE if child.is_err() => {
                *phase = phase::REPORTED;
                let file = asciz_at(p, at::FILE, at::FILE_LEN);
                Flow::Call(say_error(p, &message::named(message::ACCESS_DENIED, &file)))
            }
            phase::DONE_FILE | phase::REPORTED => {
                *phase = phase::FOUND;
                Flow::Call(Frame::of(&Int::dos(p.own, 0x4F00)))
            }
            phase::WALKED => match take!(child) {
                walk::step::ENTERED => Self::search_directory(phase, p),
                walk::step::LEAVING => Self::walk_on(phase, p),
                _ => Self::end(phase, p),
            },
            _ => Flow::Return(Ok(0)),
        }
    }
}
