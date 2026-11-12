//! `TREE [drive:][path] [/F] [/A]`: a directory's subdirectories drawn as a
//! tree, each below its parent and a line on from it, with `/F` the files
//! of each directory listed first, and with `/A` drawn in plain characters
//! for a code page without the line ones. A directory is drawn as its
//! parent's last once the walk, which looks one subdirectory ahead, says no
//! other follows it.

use super::{refuse, Refusal};
use crate::dos::dir::{HIDDEN_FILES, HIDDEN_KINDS};
use crate::dos::dirops::finding;
use crate::dos::message;
use crate::dos::program::walk::{self, begin_walk, walker, Walk};
use crate::dos::program::{say, Flow, Frame, Id, Int, Program, Returned, Routine};
use crate::dos::shell::{answered, asciz_at, found_label, parse, set_asciz};
use crate::frames::{locals, take, Outcome};
use alloc::vec::Vec;
use tairix_fat::dir::attr;

/// Where `TREE`'s state lies in its memory.
pub mod at {
    use crate::dos::program::at::STATE;
    use crate::dos::program::walk::walker;

    /// The [`super::switch`]es, whether a subdirectory has been drawn, and
    /// whether the directory being listed has any.
    pub const SWITCHES: u16 = STATE;
    pub const ANY: u16 = STATE + 0x01;
    pub const BELOW: u16 = STATE + 0x02;
    /// Whether a file of it has been listed.
    pub const LISTED: u16 = STATE + 0x03;
    /// A bit a level: whether the directory drawn there has others after it.
    pub const MORE: u16 = STATE + 0x04;
    /// The walk's block, and the walk.
    pub const STACK: u16 = STATE + 0x08;
    pub const WALK: u16 = STATE + 0x0A;
    /// The directory being listed, `\`-ended ASCIZ.
    pub const PATH: u16 = WALK + walker::LEN;
    pub const PATH_LEN: u16 = 0x50;
    /// The path the line named, made canonical, then a search's pattern.
    pub const NAMED: u16 = PATH + PATH_LEN;
    pub const PATTERN: u16 = NAMED + PATH_LEN;
    pub const PATTERN_LEN: u16 = 0x60;
    /// The DTA searches report into, and the serial number's record.
    pub const DTA: u16 = PATTERN + PATTERN_LEN;
    pub const SERIAL: u16 = DTA + 0x30;
    /// Bytes of `TREE`'s state.
    pub const LEN: u16 = SERIAL + 0x20 - STATE;
}

/// What [`at::SWITCHES`] holds.
pub mod switch {
    pub const FILES: u8 = 0x01;
    pub const PLAIN: u8 = 0x02;
}

/// The characters a tree is drawn with: the corner, the line along, the
/// branch, the line down.
const LINES: [u8; 4] = [0xC0, 0xC4, 0xC3, 0xB3];
const PLAIN: [u8; 4] = *b"\\-+|";

/// What `TREE`'s command line asks for: its switches and its path.
pub fn request(tail: &[u8]) -> Result<(u8, Vec<u8>), Refusal<'_>> {
    let (mut switches, mut path) = (0, None);
    for word in parse::arguments(tail) {
        if let Some(switch) = word.strip_prefix(b"/") {
            switches |= match switch.to_ascii_uppercase().as_slice() {
                b"F" => switch::FILES,
                b"A" => switch::PLAIN,
                _ => return Err(Refusal::Switch(word)),
            };
        } else if path.is_some() {
            return Err(Refusal::TooMany(word));
        } else {
            path = Some(word.to_vec());
        }
    }
    Ok((switches, path.unwrap_or_else(|| b".".to_vec())))
}

/// The start of a line at `depth`: for each level above it, the line down
/// where the directory drawn there has others after it.
#[must_use]
pub fn indent(more: u32, depth: u8, chars: [u8; 4]) -> Vec<u8> {
    let mut out = Vec::new();
    for level in 0..u32::from(depth) {
        out.push(if more & (1 << level) != 0 {
            chars[3]
        } else {
            b' '
        });
        out.extend_from_slice(b"   ");
    }
    out
}

/// `line` without the spaces that trail it, and ended.
fn ended(mut line: Vec<u8>) -> Vec<u8> {
    while line.last() == Some(&b' ') {
        line.pop();
    }
    line.extend_from_slice(b"\r\n");
    line
}

locals! {
    /// `TREE`'s main routine.
    pub struct Tree {}
}

mod phase {
    pub const REFUSED: u8 = 1;
    pub const NAMED: u8 = 2;
    pub const CHECKED: u8 = 3;
    pub const LABEL_DTA: u8 = 4;
    pub const LABELLED: u8 = 5;
    pub const SERIAL: u8 = 6;
    pub const HEADED: u8 = 7;
    pub const SHRINK: u8 = 8;
    pub const ALLOCATE: u8 = 9;
    pub const STACKED: u8 = 10;
    pub const WALKED: u8 = 11;
    pub const DRAWN: u8 = 12;
    pub const DONE: u8 = 13;
    pub const FINISHED: u8 = 14;
}

impl Tree {
    fn chars(p: &mut Program<'_, '_>) -> [u8; 4] {
        if p.get8(at::SWITCHES) & switch::PLAIN != 0 {
            PLAIN
        } else {
            LINES
        }
    }

    fn depth(p: &mut Program<'_, '_>) -> u8 {
        p.get8(at::WALK + walker::DEPTH)
    }

    /// The directory the walk is at is listed, its files with `/F`, then
    /// the walk goes on.
    fn list(p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        if p.get8(at::SWITCHES) & switch::FILES == 0 {
            return Self::walk_on(p, phase);
        }
        *phase = phase::DRAWN;
        Flow::Call(Frame::of(&Files {}))
    }

    fn walk_on(_p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        *phase = phase::WALKED;
        Flow::Call(Frame::of(&Walk::of(at::WALK)))
    }

    /// The directory the walk entered, drawn below its parent.
    fn draw(p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        p.set8(at::ANY, 1);
        let depth = Self::depth(p);
        let last = p.get8(at::WALK + walker::LAST) != 0;
        let bit = 1u32 << (depth - 1).min(31);
        let more = if last {
            p.get32(at::MORE) & !bit
        } else {
            p.get32(at::MORE) | bit
        };
        p.set32(at::MORE, more);
        let chars = Self::chars(p);
        let path = asciz_at(p, at::PATH, at::PATH_LEN);
        let name = path[..path.len().saturating_sub(1)]
            .rsplit(|&b| b == b'\\')
            .next()
            .unwrap_or_default();
        let mut line = indent(more, depth - 1, chars);
        line.push(if last { chars[0] } else { chars[2] });
        line.extend_from_slice(&[chars[1]; 3]);
        line.extend_from_slice(name);
        *phase = phase::DONE;
        let line = ended(line);
        Flow::Call(say(p, &line))
    }
}

impl Tree {
    /// The line's path made canonical: a root, or a directory to be
    /// checked as one.
    fn named(p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        let named = asciz_at(p, at::NAMED, at::PATH_LEN);
        if child.is_err() || crate::dos::name::wild(&named) {
            *phase = phase::REFUSED;
            return Flow::Call(say(p, b"Invalid path\r\n"));
        }
        *phase = phase::CHECKED;
        if named.len() <= 3 {
            return Self::checked(p, phase, Ok(0));
        }
        Flow::Call(Frame::of(&Int {
            dx: at::NAMED,
            ..Int::dos(p.own, 0x4300)
        }))
    }

    fn checked(p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        let directory = child.is_ok() && p.result(Returned::Cx) & u16::from(attr::DIRECTORY) != 0;
        let named = asciz_at(p, at::NAMED, at::PATH_LEN);
        if !directory && named.len() > 3 {
            *phase = phase::REFUSED;
            return Flow::Call(say(p, &[&b"Invalid path - "[..], &named, b"\r\n"].concat()));
        }
        *phase = phase::LABEL_DTA;
        Flow::Call(Frame::of(&Int {
            dx: at::DTA,
            ..Int::dos(p.own, 0x1A00)
        }))
    }

    /// The heading's first line, naming the volume's label when it has one.
    fn labelled(p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        let mut line = b"Directory PATH listing".to_vec();
        if child.is_ok() {
            line.extend_from_slice(b" for Volume ");
            line.extend_from_slice(&found_label(p, at::DTA));
        }
        line.extend_from_slice(b"\r\n");
        *phase = phase::SERIAL;
        Flow::Call(say(p, &line))
    }

    /// The serial number, where the volume has one, and the tree's top.
    fn headed(p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        let mut text = Vec::new();
        if child.is_ok() {
            let serial = p.get32(at::SERIAL + 2);
            text = message::serial_line(serial);
        }
        let named = asciz_at(p, at::NAMED, at::PATH_LEN);
        text.extend_from_slice(&named);
        text.extend_from_slice(b"\r\n");
        let mut path = named;
        if !path.ends_with(b"\\") {
            path.push(b'\\');
        }
        set_asciz(p, at::PATH, &path, at::PATH_LEN);
        *phase = phase::SHRINK;
        Flow::Call(say(p, &text))
    }
}

impl Routine for Tree {
    const ID: Id = Id::Tree;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        if answered(child) {
            return Flow::Return(Ok(1));
        }
        match *phase {
            0 => {
                let tail = p.tail();
                let (switches, path) = match request(&tail) {
                    Ok(request) => request,
                    Err(refusal) => {
                        *phase = phase::REFUSED;
                        return Flow::Call(refuse(p, b"", refusal));
                    }
                };
                p.set8(at::SWITCHES, switches);
                p.set8(at::ANY, 0);
                p.set32(at::MORE, 0);
                set_asciz(p, at::PATTERN, &path, at::PATTERN_LEN);
                *phase = phase::NAMED;
                Flow::Call(Frame::of(&Int {
                    si: at::PATTERN,
                    di: at::NAMED,
                    ..Int::dos(p.own, 0x6000)
                }))
            }
            phase::REFUSED => Flow::Return(Ok(1)),
            phase::NAMED => Self::named(p, phase, child),
            phase::CHECKED => Self::checked(p, phase, child),
            phase::LABEL_DTA => {
                let named = asciz_at(p, at::NAMED, at::PATH_LEN);
                set_asciz(
                    p,
                    at::PATTERN,
                    &[&named[..2], b"\\*.*"].concat(),
                    at::PATTERN_LEN,
                );
                *phase = phase::LABELLED;
                Flow::Call(Frame::of(&Int {
                    cx: 0x08,
                    dx: at::PATTERN,
                    ..Int::dos(p.own, 0x4E00)
                }))
            }
            phase::LABELLED => Self::labelled(p, phase, child),
            phase::SERIAL => {
                let drive = asciz_at(p, at::NAMED, at::PATH_LEN)[0].wrapping_sub(b'A');
                *phase = phase::HEADED;
                Flow::Call(Frame::of(&Int {
                    bx: u16::from(drive) + 1,
                    dx: at::SERIAL,
                    ..Int::dos(p.own, 0x6900)
                }))
            }
            phase::HEADED => Self::headed(p, phase, child),
            phase::SHRINK => {
                take!(child);
                *phase = phase::ALLOCATE;
                Flow::Call(p.shrink())
            }
            phase::ALLOCATE => {
                take!(child);
                *phase = phase::STACKED;
                Flow::Call(Frame::of(&Int {
                    bx: walk::PARAGRAPHS,
                    ..Int::dos(p.own, 0x4800)
                }))
            }
            phase::STACKED => {
                let Ok(segment) = child else {
                    *phase = phase::REFUSED;
                    return Flow::Call(say(p, message::MEMORY));
                };
                let segment = u16::try_from(segment).unwrap_or(0);
                p.set16(at::STACK, segment);
                begin_walk(p, at::WALK, segment, at::PATH, at::PATH_LEN, 0);
                Self::list(p, phase)
            }
            phase::WALKED => match take!(child) {
                walk::step::ENTERED => Self::draw(p, phase),
                walk::step::LEAVING => Self::walk_on(p, phase),
                _ => {
                    if p.get8(at::ANY) != 0 {
                        return Flow::Return(Ok(0));
                    }
                    *phase = phase::FINISHED;
                    Flow::Call(say(p, b"No sub-directories exist\r\n\n"))
                }
            },
            phase::DONE => {
                take!(child);
                Self::list(p, phase)
            }
            phase::DRAWN => {
                take!(child);
                Self::walk_on(p, phase)
            }
            _ => Flow::Return(Ok(0)),
        }
    }
}

locals! {
    /// `/F`'s listing of the directory the walk is at: whether it has a
    /// subdirectory found first, for the line down beside its files, then
    /// each file, and a line to close them.
    pub struct Files {}
}

mod files {
    pub const DTA_SET: u8 = 1;
    pub const SUBDIRECTORY: u8 = 2;
    pub const FILE: u8 = 3;
    pub const WRITTEN: u8 = 4;
    pub const CLOSED: u8 = 5;
}

impl Files {
    fn search(p: &mut Program<'_, '_>, next: bool, attributes: u8) -> Frame {
        if next {
            return Frame::of(&Int::dos(p.own, 0x4F00));
        }
        let path = [asciz_at(p, at::PATH, at::PATH_LEN), b"*.*".to_vec()].concat();
        set_asciz(p, at::PATTERN, &path, at::PATTERN_LEN);
        Frame::of(&Int {
            cx: u16::from(attributes),
            dx: at::PATTERN,
            ..Int::dos(p.own, 0x4E00)
        })
    }

    fn found(p: &mut Program<'_, '_>) -> (u8, Vec<u8>) {
        let attribute = p.get8(at::DTA + finding::ATTRIBUTE);
        (attribute, asciz_at(p, at::DTA + finding::NAME, 13))
    }

    /// The start of each of this directory's file lines.
    fn margin(p: &mut Program<'_, '_>) -> Vec<u8> {
        let chars = Tree::chars(p);
        let mut line = indent(p.get32(at::MORE), Tree::depth(p), chars);
        line.push(if p.get8(at::BELOW) != 0 {
            chars[3]
        } else {
            b' '
        });
        line.extend_from_slice(b"   ");
        line
    }
}

impl Routine for Files {
    const ID: Id = Id::TreeFiles;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        if answered(child) {
            return Flow::Return(child);
        }
        match *phase {
            0 => {
                p.set8(at::BELOW, 0);
                p.set8(at::LISTED, 0);
                *phase = files::DTA_SET;
                Flow::Call(Frame::of(&Int {
                    dx: at::DTA,
                    ..Int::dos(p.own, 0x1A00)
                }))
            }
            files::DTA_SET => {
                *phase = files::SUBDIRECTORY;
                Flow::Call(Self::search(p, false, HIDDEN_KINDS))
            }
            files::SUBDIRECTORY => {
                if child.is_ok() {
                    let (attribute, name) = Self::found(p);
                    if attribute & attr::DIRECTORY == 0 || name == b"." || name == b".." {
                        return Flow::Call(Self::search(p, true, 0));
                    }
                    p.set8(at::BELOW, 1);
                }
                *phase = files::FILE;
                Flow::Call(Self::search(p, false, HIDDEN_FILES))
            }
            files::FILE => {
                if child.is_err() {
                    *phase = files::CLOSED;
                    if p.get8(at::LISTED) == 0 {
                        return Flow::Return(Ok(0));
                    }
                    let margin = Self::margin(p);
                    let close = ended(margin);
                    return Flow::Call(say(p, &close));
                }
                let (attribute, name) = Self::found(p);
                if attribute & attr::DIRECTORY != 0 {
                    return Flow::Call(Self::search(p, true, 0));
                }
                p.set8(at::LISTED, 1);
                let line = ended([Self::margin(p), name].concat());
                *phase = files::WRITTEN;
                Flow::Call(say(p, &line))
            }
            files::WRITTEN => {
                take!(child);
                *phase = files::FILE;
                Flow::Call(Self::search(p, true, 0))
            }
            _ => Flow::Return(Ok(0)),
        }
    }
}
