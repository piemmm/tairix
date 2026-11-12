//! Batch files: each running one a block of memory linked to the one that
//! `CALL`ed it, the file reopened for each line at the offset its last line
//! ended, as DOS reads it, so a batch file may be changed while it runs. A
//! line's `%` expansions come from the block's arguments, after its
//! `SHIFT`s, and the environment.

use super::parse::{self, Condition};
use super::run::{Command, External};
use super::{
    asciz_at, at, flag, flags, say, set_asciz, set_flag, set_text, text, text_at, Flow, Frame, Id,
    Int, Program, Routine,
};
use crate::dos::dirops::finding;
use crate::dos::message;
use crate::frames::{locals, take, Fault, Outcome};
use alloc::vec::Vec;

const MISSING_FILE: &[u8] = b"Batch file missing\r\n";

/// A batch block's fields.
mod block {
    /// The block of the batch file that `CALL`ed this one, zero for none.
    pub const LINK: u16 = 0x00;
    /// Whether lines were echoed when it started, for its end to restore.
    pub const ECHO: u16 = 0x02;
    /// `SHIFT`s made.
    pub const SHIFT: u16 = 0x03;
    /// Where the next line begins in the file.
    pub const POSITION: u16 = 0x04;
    /// The file's full name, ASCIZ.
    pub const NAME: u16 = 0x08;
    /// Its name as typed and its arguments, length-prefixed.
    pub const ARGS: u16 = 0x58;
    /// Paragraphs a block takes.
    pub const PARAGRAPHS: u16 = (ARGS + 0x81).div_ceil(16);
}

/// The batch file running's block.
fn current(p: &mut Program<'_, '_>) -> u16 {
    p.get16(at::BATCH)
}

fn position(p: &mut Program<'_, '_>, segment: u16) -> u32 {
    let low = p.g.read16(segment, block::POSITION);
    u32::from(low) | u32::from(p.g.read16(segment, block::POSITION + 2)) << 16
}

fn set_position(p: &mut Program<'_, '_>, segment: u16, position: u32) {
    let (low, high) = crate::frames::words(position);
    p.g.write16(segment, block::POSITION, low);
    p.g.write16(segment, block::POSITION + 2, high);
}

/// What a batch line's expansions read.
struct Expansions {
    arguments: Vec<Vec<u8>>,
    environment: Vec<Vec<u8>>,
}

impl parse::Source for Expansions {
    fn argument(&self, n: usize) -> Option<&[u8]> {
        self.arguments.get(n).map(Vec::as_slice)
    }

    fn variable(&self, name: &[u8]) -> Option<Vec<u8>> {
        self.environment.iter().find_map(|entry| {
            let equals = entry.iter().position(|&b| b == b'=')?;
            entry[..equals]
                .eq_ignore_ascii_case(name)
                .then(|| entry[equals + 1..].to_vec())
        })
    }
}

/// The running batch file's arguments, `%0` first, after its shifts.
fn arguments(p: &mut Program<'_, '_>, segment: u16) -> Vec<Vec<u8>> {
    let len = usize::from(p.g.read8(segment, block::ARGS).min(0x80));
    let mut text = alloc::vec![0u8; len];
    p.g.read_bytes(segment, block::ARGS + 1, &mut text);
    let (word, tail) = parse::command_word(&text);
    let shift = usize::from(p.g.read8(segment, block::SHIFT));
    core::iter::once(word)
        .chain(parse::arguments(tail))
        .skip(shift)
        .map(<[u8]>::to_vec)
        .collect()
}

locals! {
    /// Starts the batch file [`at::PATH`] names, [`at::WORD`] and
    /// [`at::ARGS`] its arguments: on top of the one running when `call`,
    /// else in its place.
    pub struct BatchStart {
        call: bool,
    }
}

impl Routine for BatchStart {
    const ID: Id = Id::BatchStart;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        match *phase {
            0 => {
                *phase = 1;
                Flow::Call(Frame::of(&Int {
                    si: at::PATH,
                    di: at::PATH2,
                    ..Int::dos(p.own, 0x6000)
                }))
            }
            1 => {
                let name = if child.is_ok() {
                    asciz_at(p, at::PATH2, 128)
                } else {
                    asciz_at(p, at::PATH, 128)
                };
                set_asciz(p, at::PATH, &name, 128);
                *phase = 2;
                Flow::Call(Frame::of(&Int {
                    bx: block::PARAGRAPHS,
                    ..Int::dos(p.own, 0x4800)
                }))
            }
            2 => {
                let Ok(segment) = child else {
                    *phase = 4;
                    return Flow::Call(say(p, message::MEMORY));
                };
                let segment = u16::try_from(segment).unwrap_or(0);
                let running = current(p);
                let link = if self.call || running == 0 {
                    running
                } else {
                    p.g.read16(running, block::LINK)
                };
                p.g.write16(segment, block::LINK, link);
                let echo = u8::from(flags(p) & flag::ECHO != 0);
                p.g.write8(segment, block::ECHO, echo);
                p.g.write8(segment, block::SHIFT, 0);
                set_position(p, segment, 0);
                let name = asciz_at(p, at::PATH, 80);
                p.g.write_bytes(segment, block::NAME, &name);
                p.g.write8(
                    segment,
                    block::NAME + u16::try_from(name.len()).unwrap_or(0),
                    0,
                );
                let mut args = text_at(p, at::WORD);
                args.extend_from_slice(&text_at(p, at::ARGS));
                args.truncate(0x7F);
                p.g.write8(segment, block::ARGS, u8::try_from(args.len()).unwrap_or(0));
                p.g.write_bytes(segment, block::ARGS + 1, &args);
                p.set16(at::BATCH, segment);
                *phase = 3;
                if self.call || running == 0 {
                    return self.step(p, phase, Ok(0));
                }
                Flow::Call(Frame::of(&Int {
                    es: running,
                    ..Int::dos(p.own, 0x4900)
                }))
            }
            _ => Flow::Return(Ok(0)),
        }
    }
}

locals! {
    /// Ends the batch file running: echoing as it was when it started, and
    /// the batch file that `CALL`ed it running again; every one of them
    /// when `all`.
    pub struct BatchEnd {
        all: bool,
    }
}

impl Routine for BatchEnd {
    const ID: Id = Id::BatchEnd;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, _child: Outcome) -> Flow {
        let segment = current(p);
        if segment == 0 || (*phase != 0 && !self.all) {
            return Flow::Return(Ok(0));
        }
        let echo = p.g.read8(segment, block::ECHO) != 0;
        set_flag(p, flag::ECHO, echo);
        let link = p.g.read16(segment, block::LINK);
        p.set16(at::BATCH, link);
        *phase = 1;
        Flow::Call(Frame::of(&Int {
            es: segment,
            ..Int::dos(p.own, 0x4900)
        }))
    }
}

locals! {
    /// The running batch file's next line into [`at::WORK`], expanded:
    /// `1`, or `0` once the file has ended, or gone, and its batch with it.
    pub struct BatchLine {
        handle: u16,
        read: u16,
    }
}

mod line {
    pub const OPENED: u8 = 1;
    pub const SOUGHT: u8 = 2;
    pub const READ: u8 = 3;
    pub const CLOSED: u8 = 4;
    pub const ENDED: u8 = 5;
}

/// Bytes a batch line is read in.
const LINE_READ: u16 = 0x100;

impl Routine for BatchLine {
    const ID: Id = Id::BatchLine;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        let segment = current(p);
        match *phase {
            _ if segment == 0 => Flow::Return(Ok(0)),
            0 if flags(p) & flag::BATCH_BROKEN != 0 => {
                set_flag(p, flag::BATCH_BROKEN, false);
                *phase = line::ENDED;
                Flow::Call(Frame::of(&BatchEnd { all: true }))
            }
            0 => {
                let mut name = Vec::new();
                for n in 0..80 {
                    let byte = p.g.read8(segment, block::NAME + n);
                    if byte == 0 {
                        break;
                    }
                    name.push(byte);
                }
                set_asciz(p, at::PATH, &name, 128);
                *phase = line::OPENED;
                Flow::Call(Frame::of(&Int {
                    dx: at::PATH,
                    ..Int::dos(p.own, 0x3D00)
                }))
            }
            line::OPENED => {
                let Ok(handle) = child else {
                    *phase = line::CLOSED;
                    self.read = 0;
                    return Flow::Call(say(p, MISSING_FILE));
                };
                self.handle = u16::try_from(handle).unwrap_or(0);
                let (low, high) = crate::frames::words(position(p, segment));
                *phase = line::SOUGHT;
                Flow::Call(Frame::of(&Int {
                    bx: self.handle,
                    cx: high,
                    dx: low,
                    ..Int::dos(p.own, 0x4200)
                }))
            }
            line::SOUGHT => {
                *phase = line::READ;
                Flow::Call(Frame::of(&Int {
                    bx: self.handle,
                    cx: LINE_READ,
                    dx: at::BUFFER,
                    ..Int::dos(p.own, 0x3F00)
                }))
            }
            line::READ => {
                self.read = u16::try_from(child.unwrap_or(0))
                    .unwrap_or(0)
                    .min(LINE_READ);
                *phase = line::CLOSED;
                Flow::Call(Frame::of(&Int {
                    bx: self.handle,
                    ..Int::dos(p.own, 0x3E00)
                }))
            }
            line::CLOSED => {
                let mut bytes = alloc::vec![0u8; usize::from(self.read)];
                p.g.read_bytes(p.own, at::BUFFER, &mut bytes);
                let Some((text, consumed)) = next_line(&bytes) else {
                    *phase = line::ENDED;
                    return Flow::Call(Frame::of(&BatchEnd { all: false }));
                };
                let at = position(p, segment);
                set_position(p, segment, at.saturating_add(consumed));
                let expansions = Expansions {
                    arguments: arguments(p, segment),
                    environment: super::environment(p),
                };
                let line = parse::expand(text, &expansions);
                set_text(p, at::WORK, &line);
                Flow::Return(Ok(1))
            }
            _ => Flow::Return(Ok(0)),
        }
    }
}

/// The line `bytes` begin with, and the bytes it takes with its end: `None`
/// at the file's end or its Ctrl+Z.
fn next_line(bytes: &[u8]) -> Option<(&[u8], u32)> {
    if bytes.is_empty() || bytes[0] == 0x1A {
        return None;
    }
    let end = bytes
        .iter()
        .position(|&b| matches!(b, b'\r' | b'\n' | 0x1A))
        .unwrap_or(bytes.len());
    let mut consumed = end;
    if bytes.get(consumed) == Some(&b'\r') {
        consumed += 1;
    }
    if bytes.get(consumed) == Some(&b'\n') {
        consumed += 1;
    }
    Some((&bytes[..end], u32::try_from(consumed).unwrap_or(0)))
}

locals! {
    /// `GOTO label`: the running batch file goes on from the line after the
    /// label, found by reading the file from its start; with no such label
    /// it says so and ends.
    pub struct Goto {
        handle: u16,
        scanned: u32,
        read: u16,
    }
}

mod goto {
    pub const OPENED: u8 = 1;
    pub const SOUGHT: u8 = 2;
    pub const READ: u8 = 3;
    pub const CLOSED: u8 = 4;
    pub const SAID: u8 = 5;
    pub const DONE: u8 = 6;
}

impl Routine for Goto {
    const ID: Id = Id::Goto;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        let segment = current(p);
        match *phase {
            _ if segment == 0 => Flow::Return(Ok(0)),
            0 => {
                let mut name = Vec::new();
                for n in 0..80 {
                    let byte = p.g.read8(segment, block::NAME + n);
                    if byte == 0 {
                        break;
                    }
                    name.push(byte);
                }
                set_asciz(p, at::PATH, &name, 128);
                self.scanned = 0;
                *phase = goto::OPENED;
                Flow::Call(Frame::of(&Int {
                    dx: at::PATH,
                    ..Int::dos(p.own, 0x3D00)
                }))
            }
            goto::OPENED => {
                let Ok(handle) = child else {
                    *phase = goto::DONE;
                    return Flow::Call(say(p, MISSING_FILE));
                };
                self.handle = u16::try_from(handle).unwrap_or(0);
                *phase = goto::SOUGHT;
                self.seek(p)
            }
            goto::SOUGHT => {
                *phase = goto::READ;
                Flow::Call(Frame::of(&Int {
                    bx: self.handle,
                    cx: at::BUFFER_LEN,
                    dx: at::BUFFER,
                    ..Int::dos(p.own, 0x3F00)
                }))
            }
            goto::READ => {
                self.read = u16::try_from(child.unwrap_or(0))
                    .unwrap_or(0)
                    .min(at::BUFFER_LEN);
                let mut bytes = alloc::vec![0u8; usize::from(self.read)];
                p.g.read_bytes(p.own, at::BUFFER, &mut bytes);
                let args = text_at(p, at::ARGS);
                let name = parse::trim(&args);
                let wanted =
                    parse::label(&[b":", name.strip_prefix(b":").unwrap_or(name)].concat())
                        .unwrap_or_default()
                        .to_vec();
                let at_end = self.read < at::BUFFER_LEN;
                let mut at = 0usize;
                while let Some((line, consumed)) = next_line(&bytes[at..]) {
                    let consumed = consumed as usize;
                    if consumed == line.len() && !at_end {
                        break;
                    }
                    at += consumed;
                    if parse::label(line).is_some_and(|found| parse::same_label(found, &wanted)) {
                        let target = self.scanned.saturating_add(u32::try_from(at).unwrap_or(0));
                        set_position(p, segment, target);
                        *phase = goto::DONE;
                        return Flow::Call(Frame::of(&Int {
                            bx: self.handle,
                            ..Int::dos(p.own, 0x3E00)
                        }));
                    }
                }
                if at_end || bytes.get(at) == Some(&0x1A) {
                    *phase = goto::CLOSED;
                    return Flow::Call(Frame::of(&Int {
                        bx: self.handle,
                        ..Int::dos(p.own, 0x3E00)
                    }));
                }
                // The next read starts with the line this one cut short, or
                // past a line longer than the buffer.
                let next = if at == 0 { bytes.len() } else { at };
                self.scanned = self
                    .scanned
                    .saturating_add(u32::try_from(next).unwrap_or(0));
                *phase = goto::SOUGHT;
                self.seek(p)
            }
            goto::CLOSED => {
                *phase = goto::SAID;
                Flow::Call(say(p, b"Label not found\r\n"))
            }
            goto::SAID => {
                *phase = goto::DONE;
                Flow::Call(Frame::of(&BatchEnd { all: false }))
            }
            _ => Flow::Return(Ok(0)),
        }
    }
}

impl Goto {
    fn seek(self, p: &mut Program<'_, '_>) -> Flow {
        let (low, high) = crate::frames::words(self.scanned);
        Flow::Call(Frame::of(&Int {
            bx: self.handle,
            cx: high,
            dx: low,
            ..Int::dos(p.own, 0x4200)
        }))
    }
}

locals! {
    /// `SHIFT`: the batch file's arguments move down one, `%0` dropped.
    pub struct Shift {}
}

impl Routine for Shift {
    const ID: Id = Id::Shift;

    fn step(&mut self, p: &mut Program<'_, '_>, _phase: &mut u8, _child: Outcome) -> Flow {
        let segment = current(p);
        if segment != 0 {
            let shift = p.g.read8(segment, block::SHIFT);
            p.g.write8(segment, block::SHIFT, shift.saturating_add(1));
        }
        Flow::Return(Ok(0))
    }
}

locals! {
    /// `CALL`: a batch file run on top of the one running, which goes on
    /// once it ends; a program runs as it would without `CALL`.
    pub struct Call {}
}

impl Routine for Call {
    const ID: Id = Id::Call;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        if *phase != 0 {
            return Flow::Return(child);
        }
        let args = text_at(p, at::ARGS);
        let (word, tail) = parse::command_word(&args);
        let (word, tail) = (word.to_vec(), tail.to_vec());
        *phase = 1;
        if word.is_empty() {
            return Flow::Return(Ok(0));
        }
        set_text(p, at::WORD, &word);
        set_text(p, at::ARGS, &tail);
        Flow::Call(Frame::of(&External {
            call: true,
            ..External::default()
        }))
    }
}

locals! {
    /// `ECHO`: whether lines are echoed, set or said; or the text after its
    /// first byte, `ECHO.` an empty line.
    pub struct Echo {}
}

impl Routine for Echo {
    const ID: Id = Id::Echo;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, _child: Outcome) -> Flow {
        if *phase != 0 {
            return Flow::Return(Ok(0));
        }
        *phase = 1;
        let args = text_at(p, at::ARGS);
        let word = parse::trim(&args);
        if word.is_empty() {
            let on = flags(p) & flag::ECHO != 0;
            return Flow::Call(say(
                p,
                if on {
                    b"ECHO is on\r\n"
                } else {
                    b"ECHO is off\r\n"
                },
            ));
        }
        if word.eq_ignore_ascii_case(b"ON") || word.eq_ignore_ascii_case(b"OFF") {
            set_flag(p, flag::ECHO, word.eq_ignore_ascii_case(b"ON"));
            return Flow::Return(Ok(0));
        }
        let mut shown = args.get(1..).unwrap_or_default().to_vec();
        shown.extend_from_slice(text::NEW_LINE);
        Flow::Call(say(p, &shown))
    }
}

locals! {
    /// `PAUSE`: waits for a key.
    pub struct Pause {}
}

impl Routine for Pause {
    const ID: Id = Id::Pause;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        match *phase {
            0 => {
                *phase = 1;
                Flow::Call(say(p, message::PRESS_ANY_KEY))
            }
            1 => {
                *phase = 2;
                Flow::Call(Frame::of(&Int::dos(p.own, 0x0C08)))
            }
            2 => {
                take!(child);
                *phase = 3;
                Flow::Call(say(p, text::NEW_LINE))
            }
            _ => Flow::Return(Ok(0)),
        }
    }
}

locals! {
    /// `IF [NOT] ERRORLEVEL n | EXIST name | a==b command`.
    pub struct IfCommand {
        negated: bool,
    }
}

impl Routine for IfCommand {
    const ID: Id = Id::IfCommand;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        match *phase {
            0 => {
                let args = text_at(p, at::ARGS);
                let Ok((negated, condition, rest)) = parse::condition(&args) else {
                    *phase = 3;
                    return Flow::Call(say(p, text::SYNTAX));
                };
                let rest = rest.to_vec();
                match condition {
                    Condition::ErrorLevel(level) => {
                        let holds = p.get8(at::ERRORLEVEL) >= level;
                        Self::then(p, phase, holds != negated, &rest)
                    }
                    Condition::Equal(a, b) => {
                        let holds = a == b;
                        Self::then(p, phase, holds != negated, &rest)
                    }
                    Condition::Exist(name) => {
                        let name = name.to_vec();
                        set_asciz(p, at::PATH, &name, 128);
                        set_text(p, at::COMMAND, &rest);
                        self.negated = negated;
                        *phase = 1;
                        Flow::Call(Frame::of(&Int {
                            dx: at::DTA,
                            ..Int::dos(p.own, 0x1A00)
                        }))
                    }
                }
            }
            1 => {
                *phase = 2;
                Flow::Call(Frame::of(&Int {
                    dx: at::PATH,
                    ..Int::dos(p.own, 0x4E00)
                }))
            }
            2 => {
                let holds = child.is_ok();
                let rest = text_at(p, at::COMMAND);
                Self::then(p, phase, holds != self.negated, &rest)
            }
            _ => Flow::Return(if child == Err(Fault::Break) {
                child
            } else {
                Ok(0)
            }),
        }
    }
}

impl IfCommand {
    /// Runs `command` when the condition `holds`.
    fn then(p: &mut Program<'_, '_>, phase: &mut u8, holds: bool, command: &[u8]) -> Flow {
        *phase = 3;
        if !holds {
            return Flow::Return(Ok(0));
        }
        set_text(p, at::COMMAND, command);
        Flow::Call(Frame::of(&Command {}))
    }
}

locals! {
    /// `FOR %v IN (set) DO command`: the command run for each name in the
    /// set, a wildcard standing for each file it matches.
    pub struct ForLoop {}
}

mod looping {
    pub const NEXT: u8 = 1;
    pub const DTA_SET: u8 = 2;
    pub const FOUND: u8 = 3;
    pub const RAN: u8 = 4;
    pub const DONE: u8 = 5;
}

impl Routine for ForLoop {
    const ID: Id = Id::ForLoop;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        if child == Err(Fault::Break) {
            p.set8(at::FOR_RUNNING, 0);
            return Flow::Return(child);
        }
        match *phase {
            0 => {
                if p.get8(at::FOR_RUNNING) != 0 {
                    *phase = looping::DONE;
                    return Flow::Call(say(p, b"FOR cannot be nested\r\n"));
                }
                let args = text_at(p, at::ARGS);
                let Ok(parsed) = parse::for_loop(&args) else {
                    *phase = looping::DONE;
                    return Flow::Call(say(p, text::SYNTAX));
                };
                let (set, command, variable) = (
                    parsed.set.to_vec(),
                    parsed.command.to_vec(),
                    parsed.variable,
                );
                set_text(p, at::FOR_SET, &set);
                set_text(p, at::FOR_COMMAND, &command);
                p.set8(at::FOR_VARIABLE, variable);
                p.set16(at::FOR_AT, 0);
                p.set8(at::FOR_FINDING, 0);
                p.set8(at::FOR_RUNNING, 1);
                *phase = looping::NEXT;
                self.step(p, phase, Ok(0))
            }
            looping::NEXT => {
                *phase = looping::DTA_SET;
                Flow::Call(Frame::of(&Int {
                    dx: at::FOR_DTA,
                    ..Int::dos(p.own, 0x1A00)
                }))
            }
            looping::DTA_SET => {
                if p.get8(at::FOR_FINDING) != 0 {
                    *phase = looping::FOUND;
                    return Flow::Call(Frame::of(&Int::dos(p.own, 0x4F00)));
                }
                let set = text_at(p, at::FOR_SET);
                let from = usize::from(p.get16(at::FOR_AT)).min(set.len());
                let rest = &set[from..];
                let start = rest
                    .iter()
                    .position(|&b| !parse::is_separator(b))
                    .unwrap_or(rest.len());
                let end = rest[start..]
                    .iter()
                    .position(|&b| parse::is_separator(b))
                    .map_or(rest.len(), |n| start + n);
                if start == end {
                    p.set8(at::FOR_RUNNING, 0);
                    return Flow::Return(Ok(0));
                }
                let element = rest[start..end].to_vec();
                p.set16(at::FOR_AT, u16::try_from(from + end).unwrap_or(u16::MAX));
                if crate::dos::name::wild(&element) {
                    set_asciz(p, at::PATH, &element, 128);
                    p.set8(at::FOR_FINDING, 1);
                    *phase = looping::FOUND;
                    return Flow::Call(Frame::of(&Int {
                        dx: at::PATH,
                        ..Int::dos(p.own, 0x4E00)
                    }));
                }
                Self::run(p, phase, &element)
            }
            looping::FOUND => {
                if child.is_err() {
                    p.set8(at::FOR_FINDING, 0);
                    *phase = looping::DTA_SET;
                    return self.step(p, phase, Ok(0));
                }
                let name = asciz_at(p, at::FOR_DTA + finding::NAME, 13);
                Self::run(p, phase, &name)
            }
            looping::RAN => {
                *phase = looping::NEXT;
                self.step(p, phase, Ok(0))
            }
            _ => {
                p.set8(at::FOR_RUNNING, 0);
                Flow::Return(Ok(0))
            }
        }
    }
}

impl ForLoop {
    /// Runs the command for `element`.
    fn run(p: &mut Program<'_, '_>, phase: &mut u8, element: &[u8]) -> Flow {
        let command = text_at(p, at::FOR_COMMAND);
        let variable = p.get8(at::FOR_VARIABLE);
        let line = parse::substitute(&command, variable, element);
        set_text(p, at::COMMAND, &line);
        *phase = looping::RAN;
        Flow::Call(Frame::of(&Command {}))
    }
}
