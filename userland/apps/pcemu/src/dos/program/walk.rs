//! A walk down a directory tree through DOS's searches, for the programs
//! that list, change or remove a tree: each level's search keeps its own DTA
//! in a block the program allocates, so a walk goes as deep as a path can
//! name and holds nothing in Rust between its steps.

use super::{Flow, Frame, Id, Int, Program, Routine};
use crate::dos::dir::HIDDEN_KINDS;
use crate::dos::dirops::finding;
use crate::frames::{locals, Outcome};
use alloc::vec::Vec;
use tairix_fat::dir::attr;

/// Where a walk keeps its state, from the base its program gives it.
pub mod walker {
    /// The block the levels lie in, and how many levels down the walk is.
    pub const STACK: u16 = 0;
    pub const DEPTH: u16 = 2;
    /// Attributes of directories the walk passes by.
    pub const SKIP: u16 = 3;
    /// Whether the level the walk is at is done, to be left at its next step.
    pub const LEAVING: u16 = 4;
    /// Whether the directory last entered is its parent's last.
    pub const LAST: u16 = 5;
    /// Where the walk's path lies in the program's memory, `\`-ended ASCIZ,
    /// and the room it has there.
    pub const PATH: u16 = 6;
    pub const PATH_ROOM: u16 = 8;
    /// Bytes a walk's state takes.
    pub const LEN: u16 = 10;
}

/// A level in the walk's block: the length of the path above it, whether
/// its search has begun and whether it has ended, the subdirectory it found
/// ahead of the one it went down into, then the DTA its search keeps.
mod level {
    pub const PATH_LEN: u16 = 0;
    pub const SEARCHED: u16 = 1;
    pub const ENDED: u16 = 2;
    pub const HELD: u16 = 3;
    pub const HELD_NAME: u16 = 4;
    pub const DTA: u16 = 0x11;
    pub const LEN: u16 = 0x40;
}

/// Levels a walk goes down, as deep as a path can name.
pub const DEPTH: u8 = 32;
/// Where the pattern a level's search is begun with lies in the block, and
/// the subdirectory about to be gone down into while the search looks on.
const PATTERN: u16 = level::LEN * DEPTH as u16;
const PATTERN_LEN: u16 = 0x80;
const CHILD: u16 = PATTERN + PATTERN_LEN;
/// Paragraphs a walk's block takes.
pub const PARAGRAPHS: u16 = (CHILD + 13).div_ceil(16);

const NAME_LEN: usize = 13;

/// What a step of the walk did.
pub mod step {
    /// The walk is over: no directory is left to go down.
    pub const DONE: u32 = 0;
    /// It went down into a subdirectory, which its path now names; whether
    /// that is its parent's last is [`super::walker::LAST`].
    pub const ENTERED: u32 = 1;
    /// The directory its path names is done: its next step goes back up.
    pub const LEAVING: u32 = 2;
}

/// Readies the walk at `base`: its levels in the block at segment `stack`,
/// its path, at `path` with `room` bytes, naming the directory it starts
/// in; directories with any of the `skip` attributes are passed by.
pub fn begin_walk(p: &mut Program<'_, '_>, base: u16, stack: u16, path: u16, room: u16, skip: u8) {
    p.set16(base + walker::STACK, stack);
    p.set8(base + walker::DEPTH, 0);
    p.set8(base + walker::SKIP, skip);
    p.set8(base + walker::LEAVING, 0);
    p.set8(base + walker::LAST, 0);
    p.set16(base + walker::PATH, path);
    p.set16(base + walker::PATH_ROOM, room);
    p.g.write_bytes(stack, 0, &[0; level::DTA as usize]);
}

locals! {
    /// One step of the walk at `base`, which answers with a [`step`]. It
    /// finds each subdirectory one ahead of the one it goes down into, so
    /// it can say which is the last.
    pub struct Walk {
        base: u16,
        ahead: bool,
    }
}

mod phase {
    pub const DTA_SET: u8 = 1;
    pub const FOUND: u8 = 2;
}

impl Walk {
    #[must_use]
    pub const fn of(base: u16) -> Self {
        Self { base, ahead: false }
    }

    fn path(self, p: &mut Program<'_, '_>) -> Vec<u8> {
        let (at, room) = (
            p.get16(self.base + walker::PATH),
            p.get16(self.base + walker::PATH_ROOM),
        );
        crate::dos::shell::asciz_at(p, at, room)
    }

    fn set_path(self, p: &mut Program<'_, '_>, path: &[u8]) {
        let (at, room) = (
            p.get16(self.base + walker::PATH),
            p.get16(self.base + walker::PATH_ROOM),
        );
        crate::dos::shell::set_asciz(p, at, path, room);
    }

    /// The block, and where the walk's level lies in it.
    fn level(self, p: &mut Program<'_, '_>) -> (u16, u16) {
        let depth = p.get8(self.base + walker::DEPTH).min(DEPTH - 1);
        (
            p.get16(self.base + walker::STACK),
            u16::from(depth) * level::LEN,
        )
    }

    /// The name of a subdirectory the level's search found, if it is one the
    /// walk goes down into.
    fn subdirectory(self, p: &mut Program<'_, '_>) -> Option<Vec<u8>> {
        let (stack, at) = self.level(p);
        let attribute = p.g.read8(stack, at + level::DTA + finding::ATTRIBUTE);
        let mut name = [0u8; NAME_LEN];
        p.g.read_bytes(stack, at + level::DTA + finding::NAME, &mut name);
        let name = &name[..name.iter().position(|&b| b == 0).unwrap_or(NAME_LEN)];
        let depth = p.get8(self.base + walker::DEPTH);
        let room = usize::from(p.get16(self.base + walker::PATH_ROOM));
        let fits = depth + 1 < DEPTH && self.path(p).len() + name.len() + 2 < room;
        let taken = attribute & attr::DIRECTORY != 0
            && attribute & p.get8(self.base + walker::SKIP) == 0
            && name != b"."
            && name != b".."
            && fits;
        taken.then(|| name.to_vec())
    }

    /// Goes down into the subdirectory held in [`CHILD`].
    fn enter(self, p: &mut Program<'_, '_>, last: bool) -> Flow {
        let (stack, at) = self.level(p);
        let mut name = [0u8; NAME_LEN];
        p.g.read_bytes(stack, CHILD, &mut name);
        let name = &name[..name.iter().position(|&b| b == 0).unwrap_or(NAME_LEN)];
        let path = self.path(p);
        p.g.write8(
            stack,
            at + level::PATH_LEN,
            u8::try_from(path.len()).unwrap_or(0),
        );
        p.g.write_bytes(stack, at + level::LEN, &[0; level::DTA as usize]);
        self.set_path(p, &[&path[..], name, b"\\"].concat());
        let depth = p.get8(self.base + walker::DEPTH);
        p.set8(self.base + walker::DEPTH, depth + 1);
        p.set8(self.base + walker::LAST, u8::from(last));
        Flow::Return(Ok(step::ENTERED))
    }

    /// The level has no subdirectory left: the walk goes back up, or ends.
    fn finished(self, p: &mut Program<'_, '_>) -> Flow {
        if p.get8(self.base + walker::DEPTH) == 0 {
            return Flow::Return(Ok(step::DONE));
        }
        p.set8(self.base + walker::LEAVING, 1);
        Flow::Return(Ok(step::LEAVING))
    }

    /// Begins or goes on with the level's search, its DTA set.
    fn search(self, p: &mut Program<'_, '_>) -> Flow {
        let (stack, at) = self.level(p);
        if p.g.read8(stack, at + level::SEARCHED) != 0 {
            return Flow::Call(Frame::of(&Int::dos(p.own, 0x4F00)));
        }
        p.g.write8(stack, at + level::SEARCHED, 1);
        let pattern = [self.path(p), b"*.*".to_vec()].concat();
        let len = pattern.len().min(usize::from(PATTERN_LEN) - 1);
        p.g.write_bytes(stack, PATTERN, &pattern[..len]);
        p.g.write8(stack, PATTERN + u16::try_from(len).unwrap_or(0), 0);
        Flow::Call(Frame::of(&Int {
            cx: u16::from(HIDDEN_KINDS),
            dx: PATTERN,
            ds: stack,
            ..Int::dos(p.own, 0x4E00)
        }))
    }
}

impl Routine for Walk {
    const ID: Id = Id::Walk;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        match *phase {
            0 => {
                if p.get8(self.base + walker::LEAVING) != 0 {
                    p.set8(self.base + walker::LEAVING, 0);
                    let depth = p.get8(self.base + walker::DEPTH).saturating_sub(1);
                    p.set8(self.base + walker::DEPTH, depth);
                    let (stack, at) = self.level(p);
                    let len = usize::from(p.g.read8(stack, at + level::PATH_LEN));
                    let path = self.path(p);
                    self.set_path(p, &path[..len.min(path.len())]);
                }
                let (stack, at) = self.level(p);
                let ended = p.g.read8(stack, at + level::ENDED) != 0;
                if p.g.read8(stack, at + level::HELD) != 0 {
                    let mut name = [0u8; NAME_LEN];
                    p.g.read_bytes(stack, at + level::HELD_NAME, &mut name);
                    p.g.write_bytes(stack, CHILD, &name);
                    p.g.write8(stack, at + level::HELD, 0);
                    if ended {
                        return self.enter(p, true);
                    }
                    self.ahead = true;
                } else if ended {
                    return self.finished(p);
                }
                *phase = phase::DTA_SET;
                Flow::Call(Frame::of(&Int {
                    dx: at + level::DTA,
                    ds: stack,
                    ..Int::dos(p.own, 0x1A00)
                }))
            }
            phase::DTA_SET => {
                *phase = phase::FOUND;
                self.search(p)
            }
            _ => {
                if crate::dos::shell::answered(child) {
                    return Flow::Return(child);
                }
                let (stack, at) = self.level(p);
                if child.is_err() {
                    p.g.write8(stack, at + level::ENDED, 1);
                    return if self.ahead {
                        self.enter(p, true)
                    } else {
                        self.finished(p)
                    };
                }
                let Some(name) = self.subdirectory(p) else {
                    return self.search(p);
                };
                let mut field = [0u8; NAME_LEN];
                field[..name.len()].copy_from_slice(&name);
                if self.ahead {
                    p.g.write_bytes(stack, at + level::HELD_NAME, &field);
                    p.g.write8(stack, at + level::HELD, 1);
                    return self.enter(p, false);
                }
                p.g.write_bytes(stack, CHILD, &field);
                self.ahead = true;
                self.search(p)
            }
        }
    }
}
