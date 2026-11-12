//! `MOVE [/Y | /-Y] source[,source...] destination`: files moved into a
//! directory or onto a name — renamed where both are on one drive, copied
//! and the source deleted where they are not — and a directory renamed.
//! Each move is told as `source => destination [ok]`, both canonical. A
//! file the destination already has is overwritten only once the user says
//! so, unless `/Y`, or `COPYCMD` holding `/Y`, says to.

use super::{refuse, Refusal};
use crate::dos::dirops::finding;
use crate::dos::message;
use crate::dos::name::directory_of;
use crate::dos::program::{answer_key, say, Flow, Frame, Id, Int, Key, Program, Returned, Routine};
use crate::dos::shell::copy::{error, CopyFile};
use crate::dos::shell::{answered, asciz_at, parse, set_asciz};
use crate::frames::{locals, take, Fault, Outcome};
use alloc::vec::Vec;
use tairix_fat::dir::attr;

/// Where `MOVE`'s state lies in its memory.
pub mod at {
    use crate::dos::program::at::STATE;

    /// Whether to overwrite without asking, the source reached, and whether
    /// the destination is a directory.
    pub const YES: u16 = STATE;
    pub const SOURCE: u16 = STATE + 0x01;
    pub const INTO: u16 = STATE + 0x02;
    /// Whether anything failed.
    pub const FAILED: u16 = STATE + 0x03;
    /// The destination as typed, then canonical.
    pub const DESTINATION: u16 = STATE + 0x10;
    pub const PATH_LEN: u16 = 0x80;
    pub const CANONICAL: u16 = DESTINATION + PATH_LEN;
    /// The source pattern searched, the file found, the file it moves to.
    pub const PATTERN: u16 = CANONICAL + PATH_LEN;
    pub const FROM: u16 = PATTERN + PATH_LEN;
    pub const TO: u16 = FROM + PATH_LEN;
    /// The two canonical, as they are told.
    pub const FROM_SHOWN: u16 = TO + PATH_LEN;
    pub const TO_SHOWN: u16 = FROM_SHOWN + PATH_LEN;
    /// The search's DTA, and the buffer a copy reads through.
    pub const DTA: u16 = TO_SHOWN + PATH_LEN;
    pub const DTA_LEN: u16 = 0x30;
    pub const BUFFER: u16 = DTA + DTA_LEN;
    pub const BUFFER_LEN: u16 = 0x800;
    /// Bytes of `MOVE`'s state.
    pub const LEN: u16 = BUFFER + BUFFER_LEN - STATE;
}

/// What a search for the sources matches: plain files, and a directory to
/// rename.
const SEARCHED: u16 = 0x10;

/// `MOVE`'s messages.
pub mod text {
    pub const MULTIPLE: &[u8] = b"Cannot move multiple files to a single file\r\n";
    pub const OK: &[u8] = b" [ok]\r\n";
    pub const SELF: &[u8] = b" [Unable to move a file onto itself]\r\n";
    pub const DENIED: &[u8] = b" [Permission denied]\r\n";
    pub const NO_SOURCE: &[u8] = b" [Unable to open source]\r\n";
    pub const NO_ROOM: &[u8] = b" [Insufficient disk space]\r\n";
    pub const NO_TARGET: &[u8] = b" [Unable to create destination]\r\n";
}

/// What `MOVE`'s command line asks for: the sources, the destination, and
/// whether `/Y` (`Some(true)`) or `/-Y` (`Some(false)`) was given.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request<'a> {
    pub sources: Vec<&'a [u8]>,
    pub destination: &'a [u8],
    pub overwrite: Option<bool>,
}

/// `MOVE`'s command line read: every word but the last a source, or a list
/// of them by `,`; the last the destination.
///
/// # Errors
///
/// The [`Refusal`] DOS's parser gives a line it cannot take.
pub fn request(tail: &[u8]) -> Result<Request<'_>, Refusal<'_>> {
    let mut words = Vec::new();
    let mut overwrite = None;
    for word in parse::arguments(tail) {
        match word.strip_prefix(b"/") {
            Some(switch) if switch.eq_ignore_ascii_case(b"Y") => overwrite = Some(true),
            Some(switch) if switch.eq_ignore_ascii_case(b"-Y") => overwrite = Some(false),
            Some(_) => return Err(Refusal::Switch(word)),
            None => words.push(word),
        }
    }
    let Some((&destination, sources)) = words.split_last() else {
        return Err(Refusal::Missing);
    };
    let sources: Vec<&[u8]> = sources
        .iter()
        .flat_map(|word| word.split(|&b| b == b','))
        .filter(|s| !s.is_empty())
        .collect();
    if sources.is_empty() {
        return Err(Refusal::Missing);
    }
    Ok(Request {
        sources,
        destination,
        overwrite,
    })
}

/// Whether `COPYCMD`'s value asks to overwrite without asking: its last
/// `/Y` or `/-Y` decides.
#[must_use]
pub fn copycmd_yes(value: &[u8]) -> bool {
    parse::arguments(value).iter().rev().find_map(|word| {
        match word.to_ascii_uppercase().as_slice() {
            b"/Y" => Some(true),
            b"/-Y" => Some(false),
            _ => None,
        }
    }) == Some(true)
}

/// Whether `path` names a directory by its spelling alone: a root, a
/// drive, `.` or `..`, or one ending in `\`.
fn spelt_as_directory(path: &[u8]) -> bool {
    let last = path
        .rsplit(|&b| b == b'\\' || b == b':')
        .next()
        .unwrap_or(path);
    path.ends_with(b"\\") || path.ends_with(b":") || last == b"." || last == b".."
}

locals! {
    /// `MOVE`'s main routine.
    pub struct Move {
        all: bool,
        count: u8,
        rename_directory: bool,
    }
}

mod phase {
    pub const REFUSED: u8 = 1;
    pub const CANONICAL: u8 = 2;
    pub const KNOWN: u8 = 3;
    pub const COUNTED: u8 = 4;
    pub const DTA_SET: u8 = 5;
    pub const FOUND: u8 = 6;
    pub const FROM_SHOWN: u8 = 7;
    pub const TO_SHOWN: u8 = 8;
    pub const TARGET_KNOWN: u8 = 9;
    pub const ASKED: u8 = 10;
    pub const ANSWERED: u8 = 11;
    pub const CLEARED_TARGET: u8 = 12;
    pub const MOVED: u8 = 13;
    pub const COPIED: u8 = 14;
    pub const ATTRIBUTED: u8 = 15;
    pub const SOURCE_CLEARED: u8 = 16;
    pub const SAID: u8 = 17;
    pub const NEXT: u8 = 18;
    pub const ENDED: u8 = 19;
    pub const NOT_FOUND: u8 = 20;
    pub const ECHOED: u8 = 21;
    pub const COUNT_SET: u8 = 22;
}

impl Move {
    fn requested(p: &mut Program<'_, '_>) -> Option<(Vec<Vec<u8>>, Vec<u8>)> {
        let tail = p.tail();
        let request = request(&tail).ok()?;
        Some((
            request.sources.iter().map(|s| s.to_vec()).collect(),
            request.destination.to_vec(),
        ))
    }

    /// The canonical name of the path at `from`, in `into`.
    fn canonical(p: &mut Program<'_, '_>, from: u16, into: u16) -> Frame {
        Frame::of(&Int {
            si: from,
            di: into,
            ..Int::dos(p.own, 0x6000)
        })
    }

    fn attributes_of(p: &mut Program<'_, '_>, path: u16) -> Frame {
        Frame::of(&Int {
            dx: path,
            ..Int::dos(p.own, 0x4300)
        })
    }

    /// The search of source pattern `n`, if the line has that many.
    fn search(p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        let Some((sources, _)) = Self::requested(p) else {
            return Flow::Return(Ok(1));
        };
        let Some(pattern) = sources.get(usize::from(p.get8(at::SOURCE))) else {
            *phase = phase::ENDED;
            let code = u32::from(p.get8(at::FAILED));
            return Flow::Return(Ok(code.min(1)));
        };
        set_asciz(p, at::PATTERN, pattern, at::PATH_LEN);
        *phase = phase::DTA_SET;
        Flow::Call(Frame::of(&Int {
            dx: at::DTA,
            ..Int::dos(p.own, 0x1A00)
        }))
    }

    /// The match after this one.
    fn next(p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        *phase = phase::NEXT;
        Flow::Call(Frame::of(&Int {
            dx: at::DTA,
            ..Int::dos(p.own, 0x1A00)
        }))
    }

    /// The move told, `outcome` after the two names; a failure marked.
    fn tell(p: &mut Program<'_, '_>, phase: &mut u8, outcome: &[u8]) -> Flow {
        if outcome != text::OK {
            p.set8(at::FAILED, 1);
        }
        let from = asciz_at(p, at::FROM_SHOWN, at::PATH_LEN).to_ascii_lowercase();
        let to = asciz_at(p, at::TO_SHOWN, at::PATH_LEN).to_ascii_lowercase();
        *phase = phase::SAID;
        let said = [&from[..], b" => ", &to, outcome].concat();
        Flow::Call(say(p, &said))
    }

    /// A match found: the file it moves to named, both made canonical.
    fn found(&mut self, p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        let name = asciz_at(p, at::DTA + finding::NAME, 13);
        let directory = p.get8(at::DTA + finding::ATTRIBUTE) & attr::DIRECTORY != 0;
        if name == b"." || name == b".." || (directory && !self.rename_directory) {
            return Self::next(p, phase);
        }
        let pattern = asciz_at(p, at::PATTERN, at::PATH_LEN);
        let from = [directory_of(&pattern), &name[..]].concat();
        set_asciz(p, at::FROM, &from, at::PATH_LEN);
        let destination = asciz_at(p, at::DESTINATION, at::PATH_LEN);
        let to = if p.get8(at::INTO) == 0 {
            destination
        } else {
            let mut into = destination;
            if !into.ends_with(b"\\") && !into.ends_with(b":") {
                into.push(b'\\');
            }
            [into, name].concat()
        };
        set_asciz(p, at::TO, &to, at::PATH_LEN);
        *phase = phase::FROM_SHOWN;
        Flow::Call(Self::canonical(p, at::FROM, at::FROM_SHOWN))
    }

    /// The source and the destination known: moved by a rename on one
    /// drive, else by a copy.
    fn move_now(p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        let (from, to) = (asciz_at(p, at::FROM_SHOWN, 3), asciz_at(p, at::TO_SHOWN, 3));
        let one_drive =
            from.first().map(u8::to_ascii_uppercase) == to.first().map(u8::to_ascii_uppercase);
        if one_drive {
            *phase = phase::MOVED;
            return Flow::Call(Frame::of(&Int {
                dx: at::FROM,
                di: at::TO,
                ..Int::dos(p.own, 0x5600)
            }));
        }
        *phase = phase::COPIED;
        Flow::Call(Frame::of(&CopyFile::between(
            at::FROM,
            at::TO,
            at::BUFFER,
            at::BUFFER_LEN,
        )))
    }
}

impl Routine for Move {
    const ID: Id = Id::Move;

    #[allow(
        clippy::too_many_lines,
        reason = "one arm a phase, in the order the move runs"
    )]
    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        if answered(child) {
            return Flow::Return(Ok(1));
        }
        match *phase {
            0 => {
                let tail = p.tail();
                let request = match request(&tail) {
                    Ok(request) => request,
                    Err(refusal) => {
                        *phase = phase::REFUSED;
                        return Flow::Call(refuse(p, b"", refusal));
                    }
                };
                let copycmd =
                    crate::dos::shell::program_variable(p, b"COPYCMD").unwrap_or_default();
                let yes = request.overwrite.unwrap_or_else(|| copycmd_yes(&copycmd));
                p.set8(at::YES, u8::from(yes));
                p.set8(at::SOURCE, 0);
                p.set8(at::FAILED, 0);
                set_asciz(p, at::DESTINATION, request.destination, at::PATH_LEN);
                let into = spelt_as_directory(request.destination);
                p.set8(at::INTO, u8::from(into));
                *phase = phase::CANONICAL;
                Flow::Call(Self::canonical(p, at::DESTINATION, at::CANONICAL))
            }
            phase::CANONICAL => {
                if p.get8(at::INTO) != 0 {
                    self.count = 0;
                    return Self::search(p, phase);
                }
                *phase = phase::KNOWN;
                Flow::Call(Self::attributes_of(p, at::DESTINATION))
            }
            // A destination that is no directory takes one file, or the
            // directory the one source names under its new name.
            phase::KNOWN => {
                let into =
                    child.is_ok() && p.result(Returned::Cx) & u16::from(attr::DIRECTORY) != 0;
                p.set8(at::INTO, u8::from(into));
                let exists = child.is_ok();
                if into {
                    return Self::search(p, phase);
                }
                let Some((sources, _)) = Self::requested(p) else {
                    return Flow::Return(Ok(1));
                };
                if sources.len() > 1 {
                    p.set8(at::FAILED, 1);
                    *phase = phase::ENDED;
                    return Flow::Call(say(p, text::MULTIPLE));
                }
                self.rename_directory = !exists;
                self.count = 0;
                set_asciz(p, at::PATTERN, &sources[0], at::PATH_LEN);
                *phase = phase::COUNT_SET;
                Flow::Call(Frame::of(&Int {
                    dx: at::DTA,
                    ..Int::dos(p.own, 0x1A00)
                }))
            }
            phase::COUNT_SET => {
                *phase = phase::COUNTED;
                Flow::Call(Frame::of(&Int {
                    cx: SEARCHED,
                    dx: at::PATTERN,
                    ..Int::dos(p.own, 0x4E00)
                }))
            }
            // One file onto one file: more are refused before any moves.
            phase::COUNTED => {
                if child.is_err() {
                    self.count = 0;
                    return Self::search(p, phase);
                }
                let name = asciz_at(p, at::DTA + finding::NAME, 13);
                if name != b"." && name != b".." {
                    self.count = self.count.saturating_add(1);
                }
                if self.count > 1 {
                    p.set8(at::FAILED, 1);
                    *phase = phase::ENDED;
                    return Flow::Call(say(p, text::MULTIPLE));
                }
                Flow::Call(Frame::of(&Int::dos(p.own, 0x4F00)))
            }
            phase::DTA_SET => {
                *phase = phase::FOUND;
                Flow::Call(Frame::of(&Int {
                    cx: SEARCHED,
                    dx: at::PATTERN,
                    ..Int::dos(p.own, 0x4E00)
                }))
            }
            phase::FOUND => {
                if child.is_err() {
                    let missing = self.count == 0;
                    let next = p.get8(at::SOURCE).saturating_add(1);
                    p.set8(at::SOURCE, next);
                    self.count = 0;
                    if !missing {
                        return Self::search(p, phase);
                    }
                    p.set8(at::FAILED, 1);
                    let pattern = asciz_at(p, at::PATTERN, at::PATH_LEN);
                    let said =
                        message::named(message::FILE_NOT_FOUND, &pattern.to_ascii_uppercase());
                    *phase = phase::NOT_FOUND;
                    return Flow::Call(say(p, &said));
                }
                self.count = self.count.saturating_add(1);
                self.found(p, phase)
            }
            phase::FROM_SHOWN => {
                *phase = phase::TO_SHOWN;
                Flow::Call(Self::canonical(p, at::TO, at::TO_SHOWN))
            }
            phase::TO_SHOWN => {
                if asciz_at(p, at::FROM_SHOWN, at::PATH_LEN)
                    == asciz_at(p, at::TO_SHOWN, at::PATH_LEN)
                {
                    return Self::tell(p, phase, text::SELF);
                }
                *phase = phase::TARGET_KNOWN;
                Flow::Call(Self::attributes_of(p, at::TO))
            }
            phase::TARGET_KNOWN => {
                let Ok(_) = child else {
                    return Self::move_now(p, phase);
                };
                if p.result(Returned::Cx) & u16::from(attr::READ_ONLY) != 0
                    || p.result(Returned::Cx) & u16::from(attr::DIRECTORY) != 0
                {
                    return Self::tell(p, phase, text::DENIED);
                }
                if p.get8(at::YES) != 0 || self.all {
                    return Self::overwrite(p, phase);
                }
                let shown = asciz_at(p, at::TO_SHOWN, at::PATH_LEN).to_ascii_lowercase();
                *phase = phase::ASKED;
                let asked = [&b"Overwrite "[..], &shown, b" (Yes/No/All)?"].concat();
                Flow::Call(say(p, &asked))
            }
            phase::ASKED => {
                take!(child);
                *phase = phase::ANSWERED;
                Flow::Call(Frame::of(&Key::of(0x0800)))
            }
            // An input with no answer left answers no to what is left.
            phase::ANSWERED => {
                let key = answer_key(take!(child));
                match key {
                    b'Y' | b'A' => {
                        self.all |= key == b'A';
                        *phase = phase::ECHOED;
                        let echo = [key.to_ascii_lowercase(), b'\r', b'\n'];
                        Flow::Call(say(p, &echo))
                    }
                    b'N' => {
                        *phase = phase::SAID;
                        Flow::Call(say(p, b"n\r\n"))
                    }
                    _ => Flow::Call(Frame::of(&Key::of(0x0800))),
                }
            }
            phase::CLEARED_TARGET => Self::move_now(p, phase),
            phase::ECHOED => Self::overwrite(p, phase),
            phase::NOT_FOUND => Self::search(p, phase),
            phase::MOVED => match child {
                Ok(_) => Self::tell(p, phase, text::OK),
                Err(_) => Self::tell(p, phase, text::DENIED),
            },
            phase::COPIED => match child {
                Ok(_) => {
                    let attributes = u16::from(p.get8(at::DTA + finding::ATTRIBUTE) & 0x27);
                    *phase = phase::ATTRIBUTED;
                    Flow::Call(Frame::of(&Int {
                        cx: attributes,
                        dx: at::TO,
                        ..Int::dos(p.own, 0x4301)
                    }))
                }
                Err(Fault::Error(error::SELF)) => Self::tell(p, phase, text::SELF),
                Err(Fault::Error(error::FULL)) => Self::tell(p, phase, text::NO_ROOM),
                Err(Fault::Error(error::CREATE)) => Self::tell(p, phase, text::NO_TARGET),
                Err(_) => Self::tell(p, phase, text::NO_SOURCE),
            },
            phase::ATTRIBUTED => {
                *phase = phase::SOURCE_CLEARED;
                Flow::Call(Frame::of(&Int {
                    dx: at::FROM,
                    ..Int::dos(p.own, 0x4301)
                }))
            }
            phase::SOURCE_CLEARED => {
                *phase = phase::MOVED;
                Flow::Call(Frame::of(&Int {
                    dx: at::FROM,
                    ..Int::dos(p.own, 0x4100)
                }))
            }
            phase::SAID => Self::next(p, phase),
            phase::NEXT => {
                *phase = phase::FOUND;
                Flow::Call(Frame::of(&Int::dos(p.own, 0x4F00)))
            }
            // Refused, or more files than one file can take: nothing moved.
            _ => Flow::Return(Ok(1)),
        }
    }
}

impl Move {
    /// The file the destination already has deleted for the source to
    /// take its place.
    fn overwrite(p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        *phase = phase::CLEARED_TARGET;
        Flow::Call(Frame::of(&Int {
            dx: at::TO,
            ..Int::dos(p.own, 0x4100)
        }))
    }
}
