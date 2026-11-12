//! `COPY`: files to a file, a directory or a name pattern; sources joined
//! by `+` into one; `/A` taking a file as text, as far as its Ctrl+Z, and
//! `/B` as bytes, each from where it appears; and a device as a source,
//! read as text. A copy keeps its source's date and time, and a file is
//! never copied onto itself.

use super::parse;
use super::{
    answered, asciz_at, at, say, set_asciz, text, text_at, Flow, Frame, Id, Int, Program, Returned,
    Routine,
};
use crate::dos::dirops::finding;
use crate::dos::fs::error::FAIL_ON_24;
use crate::dos::message::{self, decimal_in};
use crate::dos::name::{self, directory_of, wild};
use crate::frames::{locals, take, Fault, Outcome};
use alloc::vec::Vec;

/// A file `COPY` names, and how it is taken: as text or as bytes, or as its
/// place has it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Part {
    /// Its name.
    pub name: Vec<u8>,
    /// `Some(true)` for `/A`, `Some(false)` for `/B`.
    pub text: Option<bool>,
}

/// What a `COPY` line asks for.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Plan {
    /// The sources, `+` between them.
    pub sources: Vec<Part>,
    /// Where they go.
    pub destination: Option<Part>,
    /// Whether the sources were joined by `+`.
    pub joined: bool,
}

/// `COPY`'s tail read: its sources, `+` joining them, its destination, and
/// each one's mode; `None` for a tail that names no source or a switch DOS
/// does not know.
pub fn plan(tail: &[u8]) -> Option<Plan> {
    let mut plan = Plan::default();
    let mut mode: Option<bool> = None;
    let mut pending_plus = false;
    let mut chain_open = true;
    let mut last: Option<(bool, usize)> = None;
    for argument in parse::arguments(tail) {
        if let Some(switch) = argument.strip_prefix(b"/") {
            mode = match switch.to_ascii_uppercase().as_slice() {
                b"A" => Some(true),
                b"B" => Some(false),
                b"V" => mode,
                _ => return None,
            };
            if let Some((destination, n)) = last {
                let part = if destination {
                    plan.destination.as_mut()
                } else {
                    plan.sources.get_mut(n)
                };
                if let Some(part) = part {
                    part.text = mode;
                }
            }
            continue;
        }
        for (n, piece) in argument.split(|&b| b == b'+').enumerate() {
            if n > 0 {
                pending_plus = true;
                plan.joined = true;
            }
            if piece.is_empty() {
                continue;
            }
            let part = Part {
                name: piece.to_vec(),
                text: mode,
            };
            if plan.sources.is_empty() || (pending_plus && chain_open) {
                plan.sources.push(part);
                last = Some((false, plan.sources.len() - 1));
                pending_plus = false;
            } else if plan.destination.is_none() {
                plan.destination = Some(part);
                last = Some((true, 0));
                chain_open = false;
            } else {
                return None;
            }
        }
    }
    if plan.sources.is_empty() {
        return None;
    }
    Some(plan)
}

/// The name `pattern` gives a file named `name`, as `COPY` and `REN` map
/// it: each `?` keeps the byte the name has there.
pub(crate) fn mapped(name: &[u8], pattern: &[u8]) -> Option<Vec<u8>> {
    let pattern = name::field(pattern, true).ok()?;
    let old = name::field(name, false).ok()?;
    let mut field = pattern;
    for (slot, &byte) in field.iter_mut().zip(&old) {
        if *slot == b'?' {
            *slot = byte;
        }
    }
    let mut out = [0u8; 13];
    let len = name::spell(&field, &mut out);
    (len > 0).then(|| out[..len].to_vec())
}

locals! {
    /// `COPY`.
    pub struct Copy {
        source: u8,
        searched: bool,
        matched: bool,
        directory: bool,
        joined_open: bool,
        echoed: bool,
        handle: u16,
        copied: u16,
    }
}

mod phase {
    pub const KIND: u8 = 1;
    pub const SOURCE: u8 = 2;
    pub const DTA_SET: u8 = 3;
    pub const FOUND: u8 = 4;
    pub const COPIED: u8 = 5;
    pub const JOIN_OPENED: u8 = 6;
    pub const JOIN_SOUGHT: u8 = 7;
    pub const FINISH: u8 = 8;
    pub const MARKED: u8 = 9;
    pub const CLOSED: u8 = 10;
    pub const ECHOED: u8 = 11;
    pub const DONE: u8 = 12;
}

impl Routine for Copy {
    const ID: Id = Id::Copy;

    #[allow(
        clippy::too_many_lines,
        reason = "one arm a phase, in the order the routine runs"
    )]
    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        if child == Err(Fault::Break) {
            return Flow::Return(child);
        }
        if *phase == phase::DONE {
            return Flow::Return(Ok(0));
        }
        let args = text_at(p, at::ARGS);
        let Some(plan) = plan(&args) else {
            *phase = phase::DONE;
            let message = if parse::trim(&args).is_empty() {
                text::MISSING
            } else {
                text::BAD_PARAMETER
            };
            return Flow::Call(say(p, message));
        };
        let finishing = matches!(
            *phase,
            phase::FINISH | phase::MARKED | phase::CLOSED | phase::DONE
        );
        if answered(child) && !finishing {
            *phase = phase::FINISH;
            return self.step(p, phase, Ok(0));
        }
        match *phase {
            0 => {
                *phase = phase::KIND;
                match &plan.destination {
                    Some(destination) if !wild(&destination.name) => {
                        let name = destination.name.clone();
                        set_asciz(p, at::PATH2, &name, 128);
                        Flow::Call(Frame::of(&Int {
                            dx: at::PATH2,
                            ..Int::dos(p.own, 0x4300)
                        }))
                    }
                    _ => self.step(p, phase, Err(Fault::Error(0))),
                }
            }
            phase::KIND => {
                let named = plan.destination.as_ref().map_or(&b""[..], |d| &d.name[..]);
                self.directory = plan.destination.is_none()
                    || named.ends_with(b"\\")
                    || named.ends_with(b":")
                    || (child.is_ok() && p.result(Returned::Cx) & 0x10 != 0);
                *phase = phase::SOURCE;
                self.step(p, phase, Ok(0))
            }
            phase::SOURCE => {
                let Some(source) = plan.sources.get(usize::from(self.source)) else {
                    *phase = phase::FINISH;
                    return self.step(p, phase, Ok(0));
                };
                if !wild(&source.name) {
                    let name = source.name.clone();
                    set_asciz(p, at::PATH, &name, 128);
                    *phase = phase::FOUND;
                    return self.one(p, phase, &plan);
                }
                *phase = phase::DTA_SET;
                Flow::Call(Frame::of(&Int {
                    dx: at::DTA,
                    ..Int::dos(p.own, 0x1A00)
                }))
            }
            phase::DTA_SET => {
                let name = plan.sources[usize::from(self.source)].name.clone();
                set_asciz(p, at::NAME_IN, &name, 80);
                *phase = phase::FOUND;
                if self.searched {
                    return Flow::Call(Frame::of(&Int::dos(p.own, 0x4F00)));
                }
                self.searched = true;
                Flow::Call(Frame::of(&Int {
                    dx: at::NAME_IN,
                    ..Int::dos(p.own, 0x4E00)
                }))
            }
            phase::FOUND => {
                let source = &plan.sources[usize::from(self.source)];
                if child.is_err() {
                    let matched = self.matched;
                    (self.source, self.searched, self.matched) = (self.source + 1, false, false);
                    *phase = phase::SOURCE;
                    if !matched {
                        return Flow::Call(say(p, text::FILE_NOT_FOUND));
                    }
                    return self.step(p, phase, Ok(0));
                }
                self.matched = true;
                let found = asciz_at(p, at::DTA + finding::NAME, 13);
                let path = [directory_of(&source.name), &found].concat();
                set_asciz(p, at::PATH, &path, 128);
                self.one(p, phase, &plan)
            }
            phase::ECHOED => self.one(p, phase, &plan),
            phase::JOIN_OPENED => {
                let Ok(handle) = child else {
                    *phase = phase::DONE;
                    return Flow::Call(say(p, text::CREATION));
                };
                self.handle = u16::try_from(handle).unwrap_or(0);
                self.joined_open = true;
                *phase = phase::JOIN_SOUGHT;
                let appending = plan.destination.is_none();
                if !appending {
                    return self.step(p, phase, Ok(0));
                }
                Flow::Call(Frame::of(&Int {
                    bx: self.handle,
                    ..Int::dos(p.own, 0x4202)
                }))
            }
            phase::JOIN_SOUGHT => {
                *phase = phase::FOUND;
                // Appending to the first source: it is the destination, and
                // is not copied into itself.
                if plan.destination.is_none() && self.source == 0 && !self.searched {
                    self.copied = self.copied.saturating_add(1);
                    (self.source, self.matched) = (1, false);
                    *phase = phase::SOURCE;
                    return self.step(p, phase, Ok(0));
                }
                self.one(p, phase, &plan)
            }
            phase::COPIED => {
                match child {
                    Ok(_) => self.copied = self.copied.saturating_add(1),
                    Err(Fault::Error(error::SELF)) => {
                        *phase = phase::SOURCE;
                        self.advance(&plan);
                        return Flow::Call(say(p, message::ONTO_ITSELF));
                    }
                    Err(Fault::Error(error::FULL)) => {
                        *phase = phase::FINISH;
                        return Flow::Call(say(p, message::DISK_FULL));
                    }
                    Err(Fault::Error(error::CREATE)) => {
                        *phase = phase::FINISH;
                        return Flow::Call(say(p, text::CREATION));
                    }
                    Err(_) => {
                        *phase = phase::SOURCE;
                        self.advance(&plan);
                        return Flow::Call(say(p, text::FILE_NOT_FOUND));
                    }
                }
                self.advance(&plan);
                *phase = phase::SOURCE;
                self.step(p, phase, Ok(0))
            }
            phase::FINISH => {
                if !self.joined_open {
                    *phase = phase::CLOSED;
                    return self.step(p, phase, Ok(0));
                }
                self.joined_open = false;
                let marked = plan.sources.iter().all(|s| s.text != Some(false))
                    && plan
                        .destination
                        .as_ref()
                        .is_none_or(|d| d.text != Some(false));
                *phase = phase::MARKED;
                if !marked {
                    return self.step(p, phase, Ok(0));
                }
                p.set8(at::BUFFER, 0x1A);
                Flow::Call(Frame::of(&Int {
                    bx: self.handle,
                    cx: 1,
                    dx: at::BUFFER,
                    ..Int::dos(p.own, 0x4000)
                }))
            }
            phase::MARKED => {
                *phase = phase::CLOSED;
                Flow::Call(Frame::of(&Int {
                    bx: self.handle,
                    ..Int::dos(p.own, 0x3E00)
                }))
            }
            phase::CLOSED => {
                *phase = phase::DONE;
                let line = [
                    decimal_in(u32::from(self.copied), 9),
                    b" file(s) copied\r\n".to_vec(),
                ]
                .concat();
                Flow::Call(say(p, &line))
            }
            _ => Flow::Return(Ok(0)),
        }
    }
}

/// Why a [`CopyFile`] failed, beside DOS's own errors.
/// How a [`CopyFile`] failed past what DOS says: its own reasons.
pub(crate) mod error {
    /// The source and the destination are one file.
    pub const SELF: u16 = 0xF000;
    /// The destination's disk filled.
    pub const FULL: u16 = 0xF001;
    /// The destination could not be made.
    pub const CREATE: u16 = 0xF002;
}

impl Copy {
    /// Moves to the next source, or the next match of a wildcard's.
    fn advance(&mut self, plan: &Plan) {
        let wildcard = plan
            .sources
            .get(usize::from(self.source))
            .is_some_and(|s| wild(&s.name));
        if !wildcard {
            (self.source, self.searched, self.matched) = (self.source + 1, false, false);
        }
    }

    /// Copies the source [`at::PATH`] names: to its place in a directory, to
    /// a name a pattern maps it to, or into the one file sources are joined
    /// in, opened first.
    fn one(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, plan: &Plan) -> Flow {
        let shown = plan.joined || plan.sources.iter().any(|s| wild(&s.name));
        if shown && !self.echoed {
            self.echoed = true;
            let path = asciz_at(p, at::PATH, 128).to_ascii_uppercase();
            *phase = phase::ECHOED;
            return Flow::Call(say(p, &[&path[..], text::NEW_LINE].concat()));
        }
        self.echoed = false;
        let source = &plan.sources[usize::from(self.source)];
        let path = asciz_at(p, at::PATH, 128);
        let name = path[directory_of(&path).len()..].to_vec();
        let joining = plan.joined
            || (plan.destination.is_some()
                && !self.directory
                && plan.sources.iter().any(|s| wild(&s.name))
                && !plan.destination.as_ref().is_some_and(|d| wild(&d.name)));
        let text_in = source.text.unwrap_or(joining);
        if joining {
            if !self.joined_open {
                let destination = plan
                    .destination
                    .as_ref()
                    .map_or_else(|| plan.sources[0].name.clone(), |d| d.name.clone());
                set_asciz(p, at::PATH2, &destination, 128);
                *phase = phase::JOIN_OPENED;
                let ax = if plan.destination.is_none() {
                    0x3D01
                } else {
                    0x3C00
                };
                return Flow::Call(Frame::of(&Int {
                    dx: at::PATH2,
                    ..Int::dos(p.own, ax)
                }));
            }
            *phase = phase::COPIED;
            return Flow::Call(Frame::of(&CopyFile {
                into: self.handle,
                text_in,
                ..CopyFile::between(at::PATH, at::PATH2, at::BUFFER, at::BUFFER_LEN)
            }));
        }
        let destination = match &plan.destination {
            None => name,
            Some(d) if self.directory => {
                let mut target = d.name.clone();
                if !target.ends_with(b"\\") && !target.ends_with(b":") {
                    target.push(b'\\');
                }
                [target, name].concat()
            }
            Some(d) if wild(&d.name) => {
                let Some(target) = mapped(&name, &d.name[directory_of(&d.name).len()..]) else {
                    *phase = phase::DONE;
                    return Flow::Call(say(p, text::BAD_PARAMETER));
                };
                [directory_of(&d.name), &target].concat()
            }
            Some(d) => d.name.clone(),
        };
        set_asciz(p, at::PATH2, &destination, 128);
        *phase = phase::COPIED;
        Flow::Call(Frame::of(&CopyFile {
            text_in,
            text_out: plan
                .destination
                .as_ref()
                .and_then(|d| d.text)
                .unwrap_or(false),
            ..CopyFile::between(at::PATH, at::PATH2, at::BUFFER, at::BUFFER_LEN)
        }))
    }
}

locals! {
    /// Copies the file the ASCIZ path at `from` names to `to`'s, made for
    /// it and given the source's date and time; or, when `into` is a
    /// handle, onto the end of what it has. As text, as far as a Ctrl+Z,
    /// when `text_in` or the source is a device; a Ctrl+Z after it when
    /// `text_out`. It reads through the `room` bytes at `buffer`, at least
    /// 256 of them, where it first makes both names canonical to tell a
    /// file copied onto itself.
    pub struct CopyFile {
        from: u16,
        to: u16,
        buffer: u16,
        room: u16,
        into: u16,
        text_in: bool,
        text_out: bool,
        source: u16,
        target: u16,
        time: u16,
        date: u16,
        kept: u16,
        why: u16,
        ended: bool,
        device: bool,
    }
}

mod file {
    pub const OPENED: u8 = 1;
    pub const KNOWN: u8 = 2;
    pub const STAMPED: u8 = 3;
    pub const SOURCE_NAMED: u8 = 4;
    pub const TARGET_NAMED: u8 = 5;
    pub const CREATED: u8 = 6;
    pub const READ: u8 = 7;
    pub const WRITTEN: u8 = 8;
    pub const MARKED: u8 = 9;
    pub const SOURCE_CLOSED: u8 = 10;
    pub const RESTAMPED: u8 = 11;
    pub const DONE: u8 = 12;
    pub const FAIL_SOURCE: u8 = 13;
    pub const FAIL_TARGET: u8 = 14;
    pub const FAILED: u8 = 15;
}

/// Where in the buffer the two names are made canonical, before any of
/// the file passes through it.
const SOURCE_NAME: u16 = 0;
const TARGET_NAME: u16 = 0x80;

impl Routine for CopyFile {
    const ID: Id = Id::CopyFile;

    #[allow(
        clippy::too_many_lines,
        reason = "one arm a phase, in the order the routine runs"
    )]
    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        let call = |p: &mut Program<'_, '_>, ax: u16, bx: u16, cx: u16, dx: u16| {
            Frame::of(&Int {
                bx,
                cx,
                dx,
                ..Int::dos(p.own, ax)
            })
        };
        let joined = self.into != 0;
        match *phase {
            0 => {
                *phase = file::OPENED;
                Flow::Call(call(p, 0x3D00, 0, 0, self.from))
            }
            file::OPENED => {
                let handle = take!(child);
                self.source = u16::try_from(handle).unwrap_or(0);
                *phase = file::KNOWN;
                Flow::Call(call(p, 0x4400, self.source, 0, 0))
            }
            file::KNOWN => {
                self.device = child.is_ok() && p.result(Returned::Dx) & 0x80 != 0;
                self.text_in |= self.device;
                *phase = file::STAMPED;
                if self.device {
                    return self.step(p, phase, Err(Fault::Error(0)));
                }
                Flow::Call(call(p, 0x5700, self.source, 0, 0))
            }
            file::STAMPED => {
                if child.is_ok() {
                    (self.time, self.date) = (p.result(Returned::Cx), p.result(Returned::Dx));
                }
                if joined {
                    self.target = self.into;
                    return self.read(p, phase);
                }
                *phase = file::SOURCE_NAMED;
                Flow::Call(Frame::of(&Int {
                    si: self.from,
                    di: self.buffer + SOURCE_NAME,
                    ..Int::dos(p.own, 0x6000)
                }))
            }
            file::SOURCE_NAMED => {
                *phase = file::TARGET_NAMED;
                Flow::Call(Frame::of(&Int {
                    si: self.to,
                    di: self.buffer + TARGET_NAME,
                    ..Int::dos(p.own, 0x6000)
                }))
            }
            file::TARGET_NAMED => {
                let same = child.is_ok()
                    && !self.device
                    && asciz_at(p, self.buffer + SOURCE_NAME, 0x80)
                        == asciz_at(p, self.buffer + TARGET_NAME, 0x80);
                if same {
                    return self.fail(p, phase, error::SELF);
                }
                *phase = file::CREATED;
                Flow::Call(call(p, 0x3C00, 0, 0, self.to))
            }
            file::CREATED => {
                let Ok(handle) = child else {
                    let why = if answered(child) {
                        FAIL_ON_24
                    } else {
                        error::CREATE
                    };
                    return self.fail(p, phase, why);
                };
                self.target = u16::try_from(handle).unwrap_or(0);
                self.read(p, phase)
            }
            file::READ => {
                let read = match child {
                    Ok(read) => u16::try_from(read).unwrap_or(0).min(self.room),
                    Err(Fault::Error(why)) => return self.fail(p, phase, why),
                    Err(fault) => return Flow::Return(Err(fault)),
                };
                let mut bytes = alloc::vec![0u8; usize::from(read)];
                p.g.read_bytes(p.own, self.buffer, &mut bytes);
                let kept = if self.text_in {
                    bytes.iter().position(|&b| b == 0x1A).unwrap_or(bytes.len())
                } else {
                    bytes.len()
                };
                self.ended = read == 0 || kept < bytes.len() || (!self.device && read < self.room);
                self.kept = u16::try_from(kept).unwrap_or(0);
                *phase = file::WRITTEN;
                if kept == 0 {
                    return self.step(p, phase, Ok(0));
                }
                Flow::Call(call(p, 0x4000, self.target, self.kept, self.buffer))
            }
            file::WRITTEN => {
                let written = match child {
                    Ok(written) => written,
                    Err(Fault::Error(why)) => return self.fail(p, phase, why),
                    Err(fault) => return Flow::Return(Err(fault)),
                };
                if written < u32::from(self.kept) {
                    return self.fail(p, phase, error::FULL);
                }
                if !self.ended {
                    return self.read(p, phase);
                }
                *phase = file::MARKED;
                if !self.text_out || joined {
                    return self.step(p, phase, Ok(0));
                }
                p.set8(self.buffer, 0x1A);
                Flow::Call(call(p, 0x4000, self.target, 1, self.buffer))
            }
            file::MARKED => {
                *phase = file::SOURCE_CLOSED;
                Flow::Call(call(p, 0x3E00, self.source, 0, 0))
            }
            file::SOURCE_CLOSED => {
                if joined {
                    return Flow::Return(Ok(0));
                }
                *phase = file::RESTAMPED;
                if self.device {
                    return self.step(p, phase, Ok(0));
                }
                Flow::Call(call(p, 0x5701, self.target, self.time, self.date))
            }
            file::RESTAMPED => {
                *phase = file::DONE;
                Flow::Call(call(p, 0x3E00, self.target, 0, 0))
            }
            file::FAIL_SOURCE => {
                *phase = file::FAIL_TARGET;
                if self.target == 0 || joined {
                    return self.step(p, phase, Ok(0));
                }
                Flow::Call(call(p, 0x3E00, self.target, 0, 0))
            }
            file::FAIL_TARGET | file::FAILED => Flow::Return(Err(Fault::Error(self.why))),
            _ => Flow::Return(Ok(0)),
        }
    }
}

impl CopyFile {
    /// A copy of the file named at `from` to the one named at `to`, read
    /// through the `room` bytes at `buffer`.
    #[must_use]
    pub fn between(from: u16, to: u16, buffer: u16, room: u16) -> Self {
        Self {
            from,
            to,
            buffer,
            room,
            ..Self::default()
        }
    }

    fn read(&self, p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        *phase = file::READ;
        Flow::Call(Frame::of(&Int {
            bx: self.source,
            cx: self.room,
            dx: self.buffer,
            ..Int::dos(p.own, 0x3F00)
        }))
    }

    /// The copy fails for `why`: the source closed, and a target this
    /// routine made.
    fn fail(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, why: u16) -> Flow {
        self.why = why;
        *phase = file::FAIL_SOURCE;
        Flow::Call(Frame::of(&Int {
            bx: self.source,
            ..Int::dos(p.own, 0x3E00)
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn part(name: &[u8], text: Option<bool>) -> Part {
        Part {
            name: name.to_vec(),
            text,
        }
    }

    #[test]
    fn a_copy_line_reads_its_sources_destination_and_modes() {
        let simple = plan(b" a.txt b.txt").unwrap();
        assert_eq!(simple.sources, [part(b"a.txt", None)]);
        assert_eq!(simple.destination, Some(part(b"b.txt", None)));
        assert!(!simple.joined);
        let joined = plan(b" a+b /b + c d.txt /a").unwrap();
        assert_eq!(
            joined.sources,
            [
                part(b"a", None),
                part(b"b", Some(false)),
                part(b"c", Some(false))
            ]
        );
        assert_eq!(joined.destination, Some(part(b"d.txt", Some(true))));
        assert!(joined.joined);
        let appended = plan(b"log+new").unwrap();
        assert_eq!(appended.destination, None);
        assert_eq!(appended.sources.len(), 2);
        assert_eq!(plan(b""), None);
        assert_eq!(plan(b"a b c"), None, "one destination");
        assert_eq!(plan(b"a /x"), None);
    }

    #[test]
    fn a_pattern_maps_a_name_as_dos_does() {
        assert_eq!(
            mapped(b"REPORT.TXT", b"*.BAK").as_deref(),
            Some(&b"REPORT.BAK"[..])
        );
        assert_eq!(mapped(b"A.TXT", b"B?.*").as_deref(), Some(&b"B.TXT"[..]));
        assert_eq!(
            mapped(b"LETTER.DOC", b"???X.*").as_deref(),
            Some(&b"LETX.DOC"[..])
        );
    }
}
