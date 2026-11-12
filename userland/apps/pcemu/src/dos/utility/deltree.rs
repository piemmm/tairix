//! `DELTREE [/Y] [drive:]path [[drive:]path ...]`: files and directories
//! deleted, a directory with every file and directory below it, whatever
//! their attributes. Each match a path names is asked about first unless
//! `/Y` says not to.

use super::{refuse, Refusal};
use crate::dos::dirops::finding;
use crate::dos::name::directory_of;
use crate::dos::program::walk::{self, begin_walk, Walk};
use crate::dos::program::{answer_key, say, Flow, Frame, Id, Int, Key, Program, Routine};
use crate::dos::shell::{answered, asciz_at, parse, set_asciz};
use crate::frames::{locals, take, Outcome};
use alloc::vec::Vec;
use tairix_fat::dir::attr;

/// Where `DELTREE`'s state lies in its memory.
pub mod at {
    use crate::dos::program::at::STATE;
    use crate::dos::program::walk::walker;

    /// `/Y`, the path being worked through, and the walk's block.
    pub const YES: u16 = STATE;
    pub const PATH_AT: u16 = STATE + 0x01;
    pub const STACK: u16 = STATE + 0x02;
    pub const WALK: u16 = STATE + 0x04;
    /// The match being deleted as it is shown: the path's directory as typed
    /// and the name found.
    pub const MATCH: u16 = WALK + walker::LEN;
    pub const PATH_LEN: u16 = 0x80;
    /// The directory the walk is in, `\`-ended.
    pub const WALKED: u16 = MATCH + PATH_LEN;
    /// A path the calls that clear attributes and delete are given.
    pub const NAME: u16 = WALKED + PATH_LEN;
    /// The searches' DTAs: the path's matches, and a directory's files.
    pub const MATCHES: u16 = NAME + PATH_LEN;
    pub const FILES: u16 = MATCHES + DTA_LEN;
    pub const DTA_LEN: u16 = 0x30;
    /// Bytes of `DELTREE`'s state.
    pub const LEN: u16 = FILES + DTA_LEN - STATE;
}

/// What the searches match: every file, and for a path's matches its
/// directories too.
const FILES: u16 = 0x07;
const ANYTHING: u16 = 0x17;

/// What `DELTREE`'s command line asks for: whether to ask, and the paths.
///
/// # Errors
///
/// The [`Refusal`] DOS's parser gives a line it cannot take.
pub fn request(tail: &[u8]) -> Result<(bool, Vec<&[u8]>), Refusal<'_>> {
    let mut yes = false;
    let mut paths = Vec::new();
    for word in parse::arguments(tail) {
        match word.strip_prefix(b"/") {
            Some(switch) if switch.eq_ignore_ascii_case(b"Y") => yes = true,
            Some(_) => return Err(Refusal::Switch(word)),
            None => paths.push(word),
        }
    }
    if paths.is_empty() {
        return Err(Refusal::Missing);
    }
    Ok((yes, paths))
}

/// The name a DTA's search found.
fn found_name(p: &mut Program<'_, '_>, dta: u16) -> Vec<u8> {
    asciz_at(p, dta + finding::NAME, 13)
}

/// The line asking whether to delete `shown`, a directory or a file.
fn question(shown: &[u8], directory: bool) -> Vec<u8> {
    if directory {
        [
            &b"Delete directory \""[..],
            shown,
            b"\" and all its subdirectories? [yn] ",
        ]
        .concat()
    } else {
        [&b"Delete file \""[..], shown, b"\"? [yn] "].concat()
    }
}

locals! {
    /// Every file in the directory `dir` names, `\`-ended, deleted, its
    /// attributes cleared first.
    pub struct DeleteFiles {
        dir: u16,
    }
}

mod files {
    pub const DTA_SET: u8 = 1;
    pub const FOUND: u8 = 2;
    pub const CLEARED: u8 = 3;
    pub const DELETED: u8 = 4;
}

impl Routine for DeleteFiles {
    const ID: Id = Id::DeleteFiles;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        if answered(child) {
            return Flow::Return(child);
        }
        match *phase {
            0 => {
                *phase = files::DTA_SET;
                Flow::Call(Frame::of(&Int {
                    dx: at::FILES,
                    ..Int::dos(p.own, 0x1A00)
                }))
            }
            files::DTA_SET => {
                let pattern = [asciz_at(p, self.dir, at::PATH_LEN), b"*.*".to_vec()].concat();
                set_asciz(p, at::NAME, &pattern, at::PATH_LEN);
                *phase = files::FOUND;
                Flow::Call(Frame::of(&Int {
                    cx: FILES,
                    dx: at::NAME,
                    ..Int::dos(p.own, 0x4E00)
                }))
            }
            files::FOUND => {
                if child.is_err() {
                    return Flow::Return(Ok(0));
                }
                let file = [
                    asciz_at(p, self.dir, at::PATH_LEN),
                    found_name(p, at::FILES),
                ]
                .concat();
                set_asciz(p, at::NAME, &file, at::PATH_LEN);
                *phase = files::CLEARED;
                Flow::Call(Frame::of(&Int {
                    dx: at::NAME,
                    ..Int::dos(p.own, 0x4301)
                }))
            }
            files::CLEARED => {
                *phase = files::DELETED;
                Flow::Call(Frame::of(&Int {
                    dx: at::NAME,
                    ..Int::dos(p.own, 0x4100)
                }))
            }
            // A file that will not go is left, and the next looked for.
            _ => {
                *phase = files::FOUND;
                Flow::Call(Frame::of(&Int::dos(p.own, 0x4F00)))
            }
        }
    }
}

locals! {
    /// `DELTREE`'s main routine.
    pub struct DelTree {
        top: bool,
    }
}

mod phase {
    pub const REFUSED: u8 = 1;
    pub const SHRUNK: u8 = 2;
    pub const ALLOCATED: u8 = 3;
    pub const DTA_SET: u8 = 4;
    pub const FOUND: u8 = 5;
    pub const ASKED: u8 = 6;
    pub const KEY: u8 = 7;
    pub const CONFIRMED: u8 = 8;
    pub const SAID_DELETING: u8 = 9;
    pub const FILE_CLEARED: u8 = 10;
    pub const WALKING: u8 = 11;
    pub const WALKED: u8 = 12;
    pub const DIR_CLEARED: u8 = 13;
    pub const SKIPPED: u8 = 14;
    pub const FILE_DELETED: u8 = 15;
    pub const NEXT: u8 = 16;
}

impl DelTree {
    /// The path the line names at [`at::PATH_AT`], if it names that many.
    fn path(p: &mut Program<'_, '_>) -> Option<Vec<u8>> {
        let tail = p.tail();
        let (_, paths) = request(&tail).ok()?;
        paths
            .get(usize::from(p.get8(at::PATH_AT)))
            .map(|path| path.to_vec())
    }

    /// The search for the next path's matches, or the end with none left.
    fn next_path(p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        let Some(path) = Self::path(p) else {
            return Flow::Return(Ok(0));
        };
        set_asciz(p, at::NAME, &path, at::PATH_LEN);
        *phase = phase::DTA_SET;
        Flow::Call(Frame::of(&Int {
            dx: at::MATCHES,
            ..Int::dos(p.own, 0x1A00)
        }))
    }

    /// The match after this one, its search's DTA set again first: deleting
    /// a directory searched through DTAs of its own.
    fn next_match(p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        *phase = phase::NEXT;
        Flow::Call(Frame::of(&Int {
            dx: at::MATCHES,
            ..Int::dos(p.own, 0x1A00)
        }))
    }

    /// That the match is being deleted said.
    fn deleting(p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        let shown = asciz_at(p, at::MATCH, at::PATH_LEN);
        *phase = phase::SAID_DELETING;
        let said = [&b"Deleting "[..], &shown, b"...\r\n"].concat();
        Flow::Call(say(p, &said))
    }

    /// A match found: asked about unless `/Y`; none left, the next path.
    fn found(p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        if child.is_err() {
            let next = p.get8(at::PATH_AT).saturating_add(1);
            p.set8(at::PATH_AT, next);
            return Self::next_path(p, phase);
        }
        let name = found_name(p, at::MATCHES);
        if name == b"." || name == b".." {
            return Self::next_match(p, phase);
        }
        let path = Self::path(p).unwrap_or_default();
        let shown = [directory_of(&path), &name[..]].concat();
        set_asciz(p, at::MATCH, &shown, at::PATH_LEN);
        if p.get8(at::YES) != 0 {
            return Self::deleting(p, phase);
        }
        *phase = phase::ASKED;
        let asked = question(&shown, Self::is_directory(p));
        Flow::Call(say(p, &asked))
    }

    /// The match deleted: a file's attributes cleared first; a directory's
    /// files, then the walk down it.
    fn delete(p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        if !Self::is_directory(p) {
            *phase = phase::FILE_CLEARED;
            return Flow::Call(Frame::of(&Int {
                dx: at::MATCH,
                ..Int::dos(p.own, 0x4301)
            }));
        }
        let walked = [asciz_at(p, at::MATCH, at::PATH_LEN), b"\\".to_vec()].concat();
        set_asciz(p, at::WALKED, &walked, at::PATH_LEN);
        let stack = p.get16(at::STACK);
        begin_walk(p, at::WALK, stack, at::WALKED, at::PATH_LEN, 0);
        *phase = phase::WALKING;
        Flow::Call(Frame::of(&DeleteFiles { dir: at::WALKED }))
    }

    fn is_directory(p: &mut Program<'_, '_>) -> bool {
        p.get8(at::MATCHES + finding::ATTRIBUTE) & attr::DIRECTORY != 0
    }

    /// The walk's step taken: a directory gone down into has its files
    /// deleted; one left, or at the end the match itself, is removed.
    fn walked(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, step: u32) -> Flow {
        match step {
            walk::step::ENTERED => {
                *phase = phase::WALKING;
                Flow::Call(Frame::of(&DeleteFiles { dir: at::WALKED }))
            }
            walk::step::LEAVING => {
                let walked = asciz_at(p, at::WALKED, at::PATH_LEN);
                self.remove(
                    p,
                    phase,
                    walked.strip_suffix(b"\\").unwrap_or(&walked),
                    false,
                )
            }
            _ => {
                let shown = asciz_at(p, at::MATCH, at::PATH_LEN);
                self.remove(p, phase, &shown, true)
            }
        }
    }

    /// A directory the walk is done with removed: `name`, its attributes
    /// cleared first.
    fn remove(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, name: &[u8], top: bool) -> Flow {
        set_asciz(p, at::NAME, name, at::PATH_LEN);
        self.top = top;
        *phase = phase::DIR_CLEARED;
        Flow::Call(Frame::of(&Int {
            dx: at::NAME,
            ..Int::dos(p.own, 0x4301)
        }))
    }
}

impl Routine for DelTree {
    const ID: Id = Id::DelTree;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        if answered(child) {
            return Flow::Return(Ok(1));
        }
        match *phase {
            0 => {
                let tail = p.tail();
                let yes = match request(&tail) {
                    Ok((yes, _)) => yes,
                    Err(refusal) => {
                        *phase = phase::REFUSED;
                        return Flow::Call(refuse(p, b"", refusal));
                    }
                };
                p.set8(at::YES, u8::from(yes));
                p.set8(at::PATH_AT, 0);
                *phase = phase::SHRUNK;
                let shrink = p.shrink();
                Flow::Call(shrink)
            }
            phase::REFUSED => Flow::Return(Ok(1)),
            phase::SHRUNK => {
                take!(child);
                *phase = phase::ALLOCATED;
                Flow::Call(Frame::of(&Int {
                    bx: walk::PARAGRAPHS,
                    ..Int::dos(p.own, 0x4800)
                }))
            }
            phase::ALLOCATED => {
                let segment = take!(child);
                p.set16(at::STACK, u16::try_from(segment).unwrap_or(0));
                Self::next_path(p, phase)
            }
            phase::DTA_SET => {
                *phase = phase::FOUND;
                Flow::Call(Frame::of(&Int {
                    cx: ANYTHING,
                    dx: at::NAME,
                    ..Int::dos(p.own, 0x4E00)
                }))
            }
            phase::FOUND => Self::found(p, phase, child),
            phase::ASKED => {
                take!(child);
                *phase = phase::KEY;
                Flow::Call(Frame::of(&Key::of(0x0800)))
            }
            // An input with no answer left answers no to what is left.
            phase::KEY => match answer_key(take!(child)) {
                b'Y' => {
                    *phase = phase::CONFIRMED;
                    Flow::Call(say(p, b"y\r\n"))
                }
                b'N' => {
                    *phase = phase::SKIPPED;
                    Flow::Call(say(p, b"n\r\n"))
                }
                _ => Flow::Call(Frame::of(&Key::of(0x0800))),
            },
            phase::CONFIRMED => {
                take!(child);
                Self::deleting(p, phase)
            }
            phase::SAID_DELETING => {
                take!(child);
                Self::delete(p, phase)
            }
            phase::FILE_CLEARED => {
                *phase = phase::FILE_DELETED;
                Flow::Call(Frame::of(&Int {
                    dx: at::MATCH,
                    ..Int::dos(p.own, 0x4100)
                }))
            }
            // A match that will not go is left, and the next looked for.
            phase::SKIPPED | phase::FILE_DELETED => Self::next_match(p, phase),
            phase::WALKING => {
                *phase = phase::WALKED;
                Flow::Call(Frame::of(&Walk::of(at::WALK)))
            }
            phase::WALKED => {
                let walked = take!(child);
                self.walked(p, phase, walked)
            }
            phase::DIR_CLEARED => {
                *phase = if self.top {
                    phase::FILE_DELETED
                } else {
                    phase::WALKING
                };
                Flow::Call(Frame::of(&Int {
                    dx: at::NAME,
                    ..Int::dos(p.own, 0x3A00)
                }))
            }
            _ => {
                *phase = phase::FOUND;
                Flow::Call(Frame::of(&Int::dos(p.own, 0x4F00)))
            }
        }
    }
}
