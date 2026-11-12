//! The internal commands but `DIR`, `COPY` and the batch language's: each
//! takes its arguments from [`at::ARGS`] and says what goes wrong as
//! `COMMAND.COM` says it.

use super::dates::{two_digits, Writing};
use super::parse;
use super::{
    answered, asciz_at, at, environment, flag, found_label, letter, set_asciz, set_flag, set_text,
    set_variable, text, text_at, variable, Flow, Frame, Id, Int, Program, Returned, Routine,
};
use crate::dos::dirops::finding;
use crate::dos::message;
use crate::dos::name::{self, directory_of};
use crate::dos::program::say;
use crate::frames::{locals, take, Fault, Outcome};
use alloc::vec::Vec;

/// The arguments' text, its separators trimmed.
fn trimmed(p: &mut Program<'_, '_>) -> Vec<u8> {
    let args = text_at(p, at::ARGS);
    parse::trim(&args).to_vec()
}

/// A DOS call with BX, CX and DX set.
fn call(p: &mut Program<'_, '_>, ax: u16, bx: u16, cx: u16, dx: u16) -> Frame {
    Frame::of(&Int {
        bx,
        cx,
        dx,
        ..Int::dos(p.own, ax)
    })
}

/// The drive `X:` alone names, if it is that.
fn drive_alone(text: &[u8]) -> Option<u8> {
    match text {
        [letter, b':'] if letter.is_ascii_alphabetic() => Some(letter.to_ascii_uppercase() - b'A'),
        _ => None,
    }
}

const WEEKDAYS: [&[u8; 3]; 7] = [b"Sun", b"Mon", b"Tue", b"Wed", b"Thu", b"Fri", b"Sat"];
const DUPLICATE: &[u8] = b"Duplicate file name or file not found\r\n";

/// The date function `2Ah` just returned, as the country writes it, its
/// day of the week first.
pub(crate) fn date_text(p: &mut Program<'_, '_>) -> Vec<u8> {
    let year = p.result(Returned::Cx);
    let [day, month] = p.result(Returned::Dx).to_le_bytes();
    let weekday = usize::from(p.result(Returned::Ax).to_le_bytes()[0]) % 7;
    let writing = Writing::read(p, at::COUNTRY);
    [
        &WEEKDAYS[weekday][..],
        b" ",
        &writing.date(year, month, day, false),
    ]
    .concat()
}

/// The time function `2Ch` just returned, as the country writes it: twelve
/// hours with `a` or `p`, or twenty-four.
pub(crate) fn time_text(p: &mut Program<'_, '_>) -> Vec<u8> {
    let [minute, hour] = p.result(Returned::Cx).to_le_bytes();
    let [hundredths, second] = p.result(Returned::Dx).to_le_bytes();
    let writing = Writing::read(p, at::COUNTRY);
    let (shown, meridian) = writing.hour(hour);
    let mut out = two_digits(u16::from(shown)).to_vec();
    if meridian.is_some() && shown < 10 {
        out[0] = b' ';
    }
    for (mark, value) in [
        (writing.time_separator, minute),
        (writing.time_separator, second),
        (writing.decimal, hundredths),
    ] {
        out.push(mark);
        out.extend_from_slice(&two_digits(u16::from(value)));
    }
    out.extend(meridian);
    out
}

locals! {
    /// `CD`: the current directory said, of the current drive or the one
    /// named; or changed.
    pub struct Cd {}
}

impl Routine for Cd {
    const ID: Id = Id::Cd;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        if answered(child) {
            return Flow::Return(Ok(0));
        }
        match *phase {
            0 => {
                let args = trimmed(p);
                if args.is_empty() {
                    *phase = 1;
                    return Flow::Call(Frame::of(&Int::dos(p.own, 0x1900)));
                }
                if let Some(drive) = drive_alone(&args) {
                    p.set8(at::PATH, letter(drive));
                    *phase = 2;
                    return Flow::Call(Frame::of(&Int {
                        dx: u16::from(drive) + 1,
                        si: at::PATH + 3,
                        ..Int::dos(p.own, 0x4700)
                    }));
                }
                set_asciz(p, at::PATH, &args, 128);
                *phase = 4;
                Flow::Call(call(p, 0x3B00, 0, 0, at::PATH))
            }
            1 => {
                let drive = take!(child).to_le_bytes()[0];
                p.set8(at::PATH, letter(drive));
                *phase = 2;
                Flow::Call(Frame::of(&Int {
                    si: at::PATH + 3,
                    ..Int::dos(p.own, 0x4700)
                }))
            }
            2 => {
                *phase = 5;
                if child.is_err() {
                    return Flow::Call(say(p, text::BAD_DRIVE));
                }
                p.g.write_bytes(p.own, at::PATH + 1, b":\\");
                let mut shown = asciz_at(p, at::PATH, 80);
                shown.extend_from_slice(text::NEW_LINE);
                Flow::Call(say(p, &shown))
            }
            4 => {
                *phase = 5;
                if child.is_err() {
                    return Flow::Call(say(p, text::BAD_DIRECTORY));
                }
                Flow::Return(Ok(0))
            }
            _ => Flow::Return(Ok(0)),
        }
    }
}

locals! {
    /// `MD`: a directory made.
    pub struct MakeDir {}
}

impl Routine for MakeDir {
    const ID: Id = Id::MakeDir;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        directory_call(p, phase, child, 0x3900, message::NO_DIRECTORY)
    }
}

locals! {
    /// `RD`: a directory removed.
    pub struct RemoveDir {}
}

impl Routine for RemoveDir {
    const ID: Id = Id::RemoveDir;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        directory_call(
            p,
            phase,
            child,
            0x3A00,
            b"Invalid path, not directory,\r\nor directory not empty\r\n",
        )
    }
}

/// `MD` and `RD`: function `ax` on the path the argument names, `failed`
/// said when it fails.
fn directory_call(
    p: &mut Program<'_, '_>,
    phase: &mut u8,
    child: Outcome,
    ax: u16,
    failed: &[u8],
) -> Flow {
    if answered(child) {
        return Flow::Return(Ok(0));
    }
    match *phase {
        0 => {
            let args = text_at(p, at::ARGS);
            let Some(path) = parse::arguments(&args).first().map(|a| a.to_vec()) else {
                *phase = 2;
                return Flow::Call(say(p, text::MISSING));
            };
            set_asciz(p, at::PATH, &path, 128);
            *phase = 1;
            Flow::Call(call(p, ax, 0, 0, at::PATH))
        }
        1 => {
            *phase = 2;
            if child.is_err() {
                return Flow::Call(say(p, failed));
            }
            Flow::Return(Ok(0))
        }
        _ => Flow::Return(Ok(0)),
    }
}

locals! {
    /// `SET`: the environment listed, a variable set, or one removed.
    pub struct SetCommand {
        entry: u16,
    }
}

impl Routine for SetCommand {
    const ID: Id = Id::SetCommand;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, _child: Outcome) -> Flow {
        if *phase == 2 {
            return Flow::Return(Ok(0));
        }
        let args = text_at(p, at::ARGS);
        let start = args
            .iter()
            .position(|&b| !matches!(b, b' ' | b'\t'))
            .unwrap_or(args.len());
        let assignment = &args[start..];
        if assignment.is_empty() {
            let Some(entry) = environment(p).into_iter().nth(usize::from(self.entry)) else {
                return Flow::Return(Ok(0));
            };
            self.entry += 1;
            *phase = 1;
            let mut line = entry;
            line.extend_from_slice(text::NEW_LINE);
            return Flow::Call(say(p, &line));
        }
        *phase = 2;
        let Some(equals) = assignment.iter().position(|&b| b == b'=') else {
            return Flow::Call(say(p, text::SYNTAX));
        };
        let (name, value) = (&assignment[..equals], &assignment[equals + 1..]);
        if name.is_empty() {
            return Flow::Call(say(p, text::SYNTAX));
        }
        let (name, value) = (name.to_vec(), value.to_vec());
        if set_variable(p, &name, &value) {
            return Flow::Return(Ok(0));
        }
        Flow::Call(say(p, text::NO_ROOM))
    }
}

locals! {
    /// `PATH`: the search path said, cleared by `;`, or set.
    pub struct PathCommand {}
}

impl Routine for PathCommand {
    const ID: Id = Id::PathCommand;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, _child: Outcome) -> Flow {
        if *phase != 0 {
            return Flow::Return(Ok(0));
        }
        *phase = 1;
        let args = trimmed(p);
        if args.is_empty() {
            let shown = match variable(p, b"PATH") {
                Some(path) => [&b"PATH="[..], &path, text::NEW_LINE].concat(),
                None => b"No Path\r\n".to_vec(),
            };
            return Flow::Call(say(p, &shown));
        }
        let value = if args == b";" {
            Vec::new()
        } else {
            args.to_ascii_uppercase()
        };
        if set_variable(p, b"PATH", &value) {
            return Flow::Return(Ok(0));
        }
        Flow::Call(say(p, text::NO_ROOM))
    }
}

locals! {
    /// `PROMPT`: the prompt set, or put back to `$N$G`.
    pub struct PromptCommand {}
}

impl Routine for PromptCommand {
    const ID: Id = Id::PromptCommand;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, _child: Outcome) -> Flow {
        if *phase != 0 {
            return Flow::Return(Ok(0));
        }
        *phase = 1;
        let args = text_at(p, at::ARGS);
        let value = args.get(1..).unwrap_or_default().to_vec();
        if set_variable(p, b"PROMPT", &value) {
            return Flow::Return(Ok(0));
        }
        Flow::Call(say(p, text::NO_ROOM))
    }
}

locals! {
    /// `VER`.
    pub struct Version {}
}

impl Routine for Version {
    const ID: Id = Id::Version;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, _child: Outcome) -> Flow {
        if *phase != 0 {
            return Flow::Return(Ok(0));
        }
        *phase = 1;
        Flow::Call(say(
            p,
            &[text::NEW_LINE, text::VERSION, text::NEW_LINE].concat(),
        ))
    }
}

locals! {
    /// `VOL`: the label and serial number of the drive named, or the
    /// current one.
    pub struct Volume {}
}

impl Routine for Volume {
    const ID: Id = Id::Volume;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        match *phase {
            0 => {
                let args = trimmed(p);
                *phase = 2;
                if args.is_empty() {
                    *phase = 1;
                    return Flow::Call(Frame::of(&Int::dos(p.own, 0x1900)));
                }
                let Some(drive) = drive_alone(&args) else {
                    return Flow::Call(say(p, text::BAD_DRIVE));
                };
                Flow::Call(Frame::of(&VolumeHeader::shell(drive)))
            }
            1 => {
                let drive = take!(child).to_le_bytes()[0];
                *phase = 2;
                Flow::Call(Frame::of(&VolumeHeader::shell(drive)))
            }
            _ => Flow::Return(child.map(|_| 0)),
        }
    }
}

locals! {
    /// The lines `VOL`, `DIR` and `LABEL` begin with: drive `drive`'s label
    /// and its serial number, the search done through the DTA at `dta` and
    /// the serial read into `scratch`, both in the program's memory.
    /// Answers `1` when the drive has a label; fails for a drive that is
    /// not there.
    pub struct VolumeHeader {
        drive: u8,
        dta: u16,
        scratch: u16,
        labelled: bool,
    }
}

impl VolumeHeader {
    #[must_use]
    pub const fn of(drive: u8, dta: u16, scratch: u16) -> Self {
        Self {
            drive,
            dta,
            scratch,
            labelled: false,
        }
    }

    /// The shell's own, through its DTA and second path buffer.
    #[must_use]
    pub const fn shell(drive: u8) -> Self {
        Self::of(drive, at::DTA, at::PATH2)
    }
}

impl Routine for VolumeHeader {
    const ID: Id = Id::VolumeHeader;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        let letter = letter(self.drive);
        match *phase {
            0 => {
                *phase = 1;
                Flow::Call(call(p, 0x1A00, 0, 0, self.dta))
            }
            1 => {
                set_asciz(p, self.scratch, &[letter, b':', b'\\', b'*', b'.', b'*'], 8);
                *phase = 2;
                Flow::Call(call(p, 0x4E00, 0, 0x08, self.scratch))
            }
            2 => {
                if answered(child) {
                    return Flow::Return(child);
                }
                if child == Err(Fault::Error(crate::dos::fs::error::INVALID_DRIVE)) {
                    *phase = 6;
                    return Flow::Call(say(p, text::BAD_DRIVE));
                }
                self.labelled = child.is_ok();
                let named: Vec<u8> = if self.labelled {
                    [&b" is "[..], &found_label(p, self.dta)].concat()
                } else {
                    b" has no label".to_vec()
                };
                let line = [
                    &b"\r\n Volume in drive "[..],
                    &[letter],
                    &named,
                    text::NEW_LINE,
                ]
                .concat();
                *phase = 3;
                Flow::Call(say(p, &line))
            }
            3 => {
                *phase = 4;
                Flow::Call(call(p, 0x6900, u16::from(self.drive) + 1, 0, self.scratch))
            }
            4 => {
                *phase = 5;
                if child.is_err() {
                    return Flow::Return(Ok(u32::from(self.labelled)));
                }
                let serial = p.get32(self.scratch + 2);
                let line = [&b" "[..], &message::serial_line(serial)].concat();
                Flow::Call(say(p, &line))
            }
            6 => Flow::Return(Err(Fault::Error(crate::dos::fs::error::INVALID_DRIVE))),
            _ => Flow::Return(Ok(u32::from(self.labelled))),
        }
    }
}

/// The date `text` spells in the country's `format`: year, month, day.
pub(crate) fn parse_date(text: &[u8], format: u16, separator: u8) -> Option<(u16, u8, u8)> {
    let numbers: Vec<u16> = text
        .split(|&b| matches!(b, b'-' | b'/' | b'.') || b == separator)
        .map(|part| {
            part.iter().try_fold(0u16, |n, &b| {
                b.is_ascii_digit()
                    .then(|| n.saturating_mul(10).saturating_add(u16::from(b - b'0')))
            })
        })
        .collect::<Option<_>>()?;
    let [a, b, c] = numbers.as_slice() else {
        return None;
    };
    let (year, month, day) = match format {
        1 => (*c, *b, *a),
        2 => (*a, *b, *c),
        _ => (*c, *a, *b),
    };
    let year = match year {
        0..=79 => 2000 + year,
        80..=99 => 1900 + year,
        _ => year,
    };
    Some((year, u8::try_from(month).ok()?, u8::try_from(day).ok()?))
}

/// The time `text` spells: hours, minutes, seconds, hundredths, an `a` or
/// `p` making twelve hours twenty-four.
fn parse_time(text: &[u8], separator: u8, decimal: u8) -> Option<(u8, u8, u8, u8)> {
    let lower = text.to_ascii_lowercase();
    let (digits, meridian) = match lower.iter().position(|&b| b == b'a' || b == b'p') {
        Some(at) if lower[at + 1..].iter().all(|&b| b == b'm') => (&lower[..at], Some(lower[at])),
        Some(_) => return None,
        None => (&lower[..], None),
    };
    let mut parts = [0u8; 4];
    let fields = digits.split(|&b| b == b':' || b == b'.' || b == separator || b == decimal);
    for (n, field) in fields.enumerate() {
        let slot = parts.get_mut(n)?;
        if field.is_empty() || field.len() > 2 || !field.iter().all(u8::is_ascii_digit) {
            return None;
        }
        *slot = field.iter().fold(0, |v, &b| v * 10 + (b - b'0'));
    }
    let [mut hour, minute, second, hundredths] = parts;
    match meridian {
        Some(b'p') if hour < 12 => hour += 12,
        Some(b'a') if hour == 12 => hour = 0,
        _ => {}
    }
    Some((hour, minute, second, hundredths))
}

locals! {
    /// `DATE`: the date said and a new one asked for, or the one given set.
    pub struct DateCommand {}
}

impl Routine for DateCommand {
    const ID: Id = Id::DateCommand;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        clock_command(p, phase, child, true)
    }
}

locals! {
    /// `TIME`: the time said and a new one asked for, or the one given set.
    pub struct TimeCommand {}
}

impl Routine for TimeCommand {
    const ID: Id = Id::TimeCommand;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        clock_command(p, phase, child, false)
    }
}

mod clock {
    pub const SHOWN: u8 = 1;
    pub const ASK: u8 = 2;
    pub const ASKED: u8 = 3;
    pub const TYPED: u8 = 4;
    pub const SET: u8 = 5;
    pub const DONE: u8 = 6;
}

/// `DATE` when `date`, else `TIME`.
fn clock_command(p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome, date: bool) -> Flow {
    let Writing {
        order,
        date_separator: separator,
        ..
    } = Writing::read(p, at::COUNTRY);
    match *phase {
        0 => {
            let args = trimmed(p);
            if !args.is_empty() {
                return set_clock(p, phase, &args, date);
            }
            *phase = clock::SHOWN;
            Flow::Call(Frame::of(&Int::dos(
                p.own,
                if date { 0x2A00 } else { 0x2C00 },
            )))
        }
        clock::SHOWN => {
            let line = if date {
                [&b"Current date is "[..], &date_text(p), text::NEW_LINE].concat()
            } else {
                [&b"Current time is "[..], &time_text(p), text::NEW_LINE].concat()
            };
            *phase = clock::ASK;
            Flow::Call(say(p, &line))
        }
        clock::ASK => {
            let question = if date {
                let order: [&[u8]; 3] = match order {
                    1 => [b"dd", b"mm", b"yy"],
                    2 => [b"yy", b"mm", b"dd"],
                    _ => [b"mm", b"dd", b"yy"],
                };
                [
                    &b"Enter new date ("[..],
                    order[0],
                    &[separator],
                    order[1],
                    &[separator],
                    order[2],
                    b"): ",
                ]
                .concat()
            } else {
                b"Enter new time: ".to_vec()
            };
            *phase = clock::ASKED;
            Flow::Call(say(p, &question))
        }
        clock::ASKED => {
            p.set8(at::LINE, at::LINE_ROOM);
            *phase = clock::TYPED;
            Flow::Call(call(p, 0x0A00, 0, 0, at::LINE))
        }
        clock::TYPED => {
            take!(child);
            let typed = usize::from(p.get8(at::LINE + 1).min(at::LINE_ROOM - 1));
            let mut line = alloc::vec![0u8; typed];
            p.g.read_bytes(p.own, at::LINE + 2, &mut line);
            set_text(p, at::PATH2, parse::trim(&line));
            *phase = clock::SET;
            Flow::Call(say(p, b"\n"))
        }
        clock::SET => {
            let typed = text_at(p, at::PATH2);
            if typed.is_empty() {
                return Flow::Return(Ok(0));
            }
            set_clock(p, phase, &typed, date)
        }
        clock::DONE => {
            if p.result(Returned::Ax).to_le_bytes()[0] == 0 {
                return Flow::Return(Ok(0));
            }
            *phase = clock::ASK;
            Flow::Call(say(p, invalid(date)))
        }
        _ => Flow::Return(Ok(0)),
    }
}

/// Sets the date or time `typed` spells, or says it is invalid and asks
/// again.
fn set_clock(p: &mut Program<'_, '_>, phase: &mut u8, typed: &[u8], date: bool) -> Flow {
    let writing = Writing::read(p, at::COUNTRY);
    *phase = clock::DONE;
    let frame = if date {
        parse_date(typed, writing.order, writing.date_separator)
            .map(|(year, month, day)| call(p, 0x2B00, 0, year, u16::from_le_bytes([day, month])))
    } else {
        parse_time(typed, writing.time_separator, writing.decimal).map(
            |(hour, minute, second, hundredths)| {
                call(
                    p,
                    0x2D00,
                    0,
                    u16::from_le_bytes([minute, hour]),
                    u16::from_le_bytes([hundredths, second]),
                )
            },
        )
    };
    if let Some(frame) = frame {
        return Flow::Call(frame);
    }
    *phase = clock::ASK;
    Flow::Call(say(p, invalid(date)))
}

/// What a date, or a time, typed wrong is told.
const fn invalid(date: bool) -> &'static [u8] {
    if date {
        message::BAD_DATE
    } else {
        b"Invalid time\r\n"
    }
}

locals! {
    /// `CLS`: the screen cleared through the BIOS, the cursor home.
    pub struct Cls {}
}

impl Routine for Cls {
    const ID: Id = Id::Cls;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, _child: Outcome) -> Flow {
        match *phase {
            0 => {
                *phase = 1;
                Flow::Call(Frame::of(&Int::bios(p.own, 0x10, 0x0F00)))
            }
            1 => {
                let columns = p.result(Returned::Ax).to_le_bytes()[1].max(1);
                let page = p.result(Returned::Bx).to_le_bytes()[1];
                let rows = match p.g.read8(0x40, 0x84) {
                    0 => 25,
                    last => last.saturating_add(1),
                };
                p.set8(at::PATH, page);
                *phase = 2;
                Flow::Call(Frame::of(&Int {
                    bx: 0x0700,
                    cx: 0,
                    dx: u16::from_le_bytes([columns - 1, rows - 1]),
                    ..Int::bios(p.own, 0x10, 0x0600)
                }))
            }
            2 => {
                let page = p.get8(at::PATH);
                *phase = 3;
                Flow::Call(Frame::of(&Int {
                    bx: u16::from_le_bytes([0, page]),
                    dx: 0,
                    ..Int::bios(p.own, 0x10, 0x0200)
                }))
            }
            _ => Flow::Return(Ok(0)),
        }
    }
}

/// `BREAK` and `VERIFY`: the state said, `ON` or `OFF` set, else told how.
fn switch_command(
    p: &mut Program<'_, '_>,
    phase: &mut u8,
    name: &[u8],
    ask: u16,
    set: fn(bool) -> (u16, u16),
    on: fn(&mut Program<'_, '_>) -> bool,
) -> Flow {
    match *phase {
        0 => {
            let args = trimmed(p);
            *phase = 2;
            if args.is_empty() {
                *phase = 1;
                return Flow::Call(call(p, ask, 0, 0, 0));
            }
            let turn = if args.eq_ignore_ascii_case(b"ON") {
                true
            } else if args.eq_ignore_ascii_case(b"OFF") {
                false
            } else {
                return Flow::Call(say(p, b"Must specify ON or OFF\r\n"));
            };
            let (ax, dx) = set(turn);
            Flow::Call(call(p, ax, 0, 0, dx))
        }
        1 => {
            let state = if on(p) {
                &b" is on\r\n"[..]
            } else {
                b" is off\r\n"
            };
            *phase = 2;
            Flow::Call(say(p, &[name, state].concat()))
        }
        _ => Flow::Return(Ok(0)),
    }
}

locals! {
    /// `BREAK`: whether DOS looks for Ctrl+C at every call.
    pub struct Break {}
}

impl Routine for Break {
    const ID: Id = Id::Break;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, _child: Outcome) -> Flow {
        switch_command(
            p,
            phase,
            b"BREAK",
            0x3300,
            |on| (0x3301, u16::from(on)),
            |p| p.result(Returned::Dx) & 0xFF != 0,
        )
    }
}

locals! {
    /// `VERIFY`: whether DOS verifies what it writes.
    pub struct Verify {}
}

impl Routine for Verify {
    const ID: Id = Id::Verify;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, _child: Outcome) -> Flow {
        switch_command(
            p,
            phase,
            b"VERIFY",
            0x5400,
            |on| (0x2E00 | u16::from(on), 0),
            |p| p.result(Returned::Ax) & 0xFF != 0,
        )
    }
}

locals! {
    /// `EXIT`: a shell that may end, ends.
    pub struct Exit {}
}

impl Routine for Exit {
    const ID: Id = Id::Exit;

    fn step(&mut self, p: &mut Program<'_, '_>, _phase: &mut u8, _child: Outcome) -> Flow {
        set_flag(p, flag::EXITING, true);
        Flow::Return(Ok(0))
    }
}

locals! {
    /// `CTTY device`: the standard handles moved to the device.
    pub struct Ctty {
        handle: u16,
        moved: u8,
    }
}

/// `CTTY`'s phases.
mod tty {
    pub const OPENED: u8 = 1;
    pub const KNOWN: u8 = 2;
    pub const MOVED: u8 = 3;
    pub const REFUSED: u8 = 4;
    pub const DONE: u8 = 5;
}

impl Routine for Ctty {
    const ID: Id = Id::Ctty;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        match *phase {
            0 => {
                let args = trimmed(p);
                if args.is_empty() {
                    *phase = tty::DONE;
                    return Flow::Call(say(p, text::MISSING));
                }
                set_asciz(p, at::PATH, &args, 128);
                *phase = tty::OPENED;
                Flow::Call(call(p, 0x3D02, 0, 0, at::PATH))
            }
            tty::OPENED => {
                let Ok(handle) = child else {
                    *phase = tty::DONE;
                    return Flow::Call(say(p, text::BAD_DEVICE));
                };
                self.handle = u16::try_from(handle).unwrap_or(0);
                *phase = tty::KNOWN;
                Flow::Call(call(p, 0x4400, self.handle, 0, 0))
            }
            // Only a character device can be the console: a file named is
            // closed again.
            tty::KNOWN if child.is_err() || p.result(Returned::Dx) & 0x80 == 0 => {
                *phase = tty::REFUSED;
                Flow::Call(call(p, 0x3E00, self.handle, 0, 0))
            }
            tty::KNOWN | tty::MOVED if self.moved < 3 => {
                *phase = tty::MOVED;
                let to = u16::from(self.moved);
                self.moved += 1;
                Flow::Call(call(p, 0x4600, self.handle, to, 0))
            }
            tty::MOVED => {
                *phase = tty::DONE;
                Flow::Call(call(p, 0x3E00, self.handle, 0, 0))
            }
            tty::REFUSED => {
                *phase = tty::DONE;
                Flow::Call(say(p, text::BAD_DEVICE))
            }
            _ => Flow::Return(Ok(0)),
        }
    }
}

locals! {
    /// `CHCP`: the code page said, or set.
    pub struct Chcp {}
}

impl Routine for Chcp {
    const ID: Id = Id::Chcp;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        match *phase {
            0 => {
                let args = trimmed(p);
                if args.is_empty() {
                    *phase = 1;
                    return Flow::Call(call(p, 0x6601, 0, 0, 0));
                }
                let page = args.iter().try_fold(0u16, |n, &b| {
                    b.is_ascii_digit()
                        .then(|| n.saturating_mul(10).saturating_add(u16::from(b - b'0')))
                });
                let Some(page) = page else {
                    *phase = 3;
                    return Flow::Call(say(p, text::BAD_PARAMETER));
                };
                *phase = 2;
                Flow::Call(call(p, 0x6602, page, 0, 0))
            }
            1 => {
                let page = p.result(Returned::Bx);
                *phase = 3;
                Flow::Call(say(
                    p,
                    &[
                        &b"Active code page: "[..],
                        &message::decimal(u32::from(page)),
                        text::NEW_LINE,
                    ]
                    .concat(),
                ))
            }
            2 => {
                *phase = 3;
                if child.is_err() {
                    return Flow::Call(say(p, b"Invalid code page\r\n"));
                }
                Flow::Return(Ok(0))
            }
            _ => Flow::Return(Ok(0)),
        }
    }
}

locals! {
    /// `TRUENAME`: the canonical name of the path given, or of `.`.
    pub struct Truename {}
}

impl Routine for Truename {
    const ID: Id = Id::Truename;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        match *phase {
            0 => {
                let args = trimmed(p);
                set_asciz(p, at::PATH, if args.is_empty() { b"." } else { &args }, 128);
                *phase = 1;
                Flow::Call(Frame::of(&Int {
                    si: at::PATH,
                    di: at::PATH2,
                    ..Int::dos(p.own, 0x6000)
                }))
            }
            1 => {
                *phase = 2;
                if child.is_err() {
                    return Flow::Call(say(p, message::PATH_NOT_FOUND));
                }
                let name = asciz_at(p, at::PATH2, 128);
                Flow::Call(say(p, &[&name[..], text::NEW_LINE].concat()))
            }
            _ => Flow::Return(Ok(0)),
        }
    }
}

locals! {
    /// `TYPE file`: the file written to the standard output, as far as a
    /// Ctrl+Z.
    pub struct TypeFile {
        handle: u16,
        ended: bool,
    }
}

impl Routine for TypeFile {
    const ID: Id = Id::TypeFile;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        match *phase {
            0 => {
                let args = text_at(p, at::ARGS);
                let Some(name) = parse::arguments(&args).first().map(|a| a.to_vec()) else {
                    *phase = 9;
                    return Flow::Call(say(p, text::MISSING));
                };
                if name::wild(&name) {
                    *phase = 9;
                    return Flow::Call(say(p, b"Invalid filename or file not found\r\n"));
                }
                set_asciz(p, at::PATH, &name, 128);
                *phase = 1;
                Flow::Call(call(p, 0x3D00, 0, 0, at::PATH))
            }
            1 if answered(child) => Flow::Return(Ok(0)),
            1 => {
                let Ok(handle) = child else {
                    *phase = 9;
                    return Flow::Call(say(p, text::FILE_NOT_FOUND));
                };
                self.handle = u16::try_from(handle).unwrap_or(0);
                *phase = 2;
                self.step(p, phase, Ok(0))
            }
            2 => {
                if self.ended {
                    *phase = 9;
                    return Flow::Call(call(p, 0x3E00, self.handle, 0, 0));
                }
                *phase = 3;
                Flow::Call(call(p, 0x3F00, self.handle, at::BUFFER_LEN, at::BUFFER))
            }
            3 => {
                let read = u16::try_from(child.unwrap_or(0))
                    .unwrap_or(0)
                    .min(at::BUFFER_LEN);
                let mut bytes = alloc::vec![0u8; usize::from(read)];
                p.g.read_bytes(p.own, at::BUFFER, &mut bytes);
                let shown = bytes.iter().position(|&b| b == 0x1A).unwrap_or(bytes.len());
                // A device gives a line a read, so only nothing read, or a
                // Ctrl+Z, ends it.
                self.ended = read == 0 || shown < bytes.len();
                *phase = 2;
                if shown == 0 {
                    return self.step(p, phase, Ok(0));
                }
                Flow::Call(call(
                    p,
                    0x4000,
                    1,
                    u16::try_from(shown).unwrap_or(0),
                    at::BUFFER,
                ))
            }
            _ => Flow::Return(Ok(0)),
        }
    }
}

/// Whether a file name's base and extension are all wildcard: every file.
fn every_file(name: &[u8]) -> bool {
    name::field(name, true).is_ok_and(|field| field.iter().all(|&b| b == b'?'))
}

locals! {
    /// `DEL` and `ERASE`: the files a name matches deleted; every file in a
    /// directory only once asked, and each one asked for with `/P`.
    pub struct Delete {
        asking: bool,
        searched: bool,
        any: bool,
    }
}

mod delete {
    pub const ATTRIBUTES: u8 = 1;
    pub const SURE: u8 = 2;
    pub const ANSWERED: u8 = 3;
    pub const SEARCH_FROM: u8 = 4;
    pub const SEARCH: u8 = 5;
    pub const FOUND: u8 = 6;
    pub const ASKED: u8 = 7;
    pub const KEYED: u8 = 8;
    pub const DELETE: u8 = 9;
    pub const DELETED: u8 = 10;
    pub const DONE: u8 = 11;
}

impl Routine for Delete {
    const ID: Id = Id::Delete;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        if answered(child) {
            return Flow::Return(Ok(0));
        }
        match *phase {
            0 => {
                let args = text_at(p, at::ARGS);
                let arguments = parse::arguments(&args);
                self.asking = arguments.iter().any(|a| a.eq_ignore_ascii_case(b"/P"));
                let Some(pattern) = arguments
                    .iter()
                    .find(|a| !a.starts_with(b"/"))
                    .map(|a| a.to_vec())
                else {
                    *phase = delete::DONE;
                    return Flow::Call(say(p, text::MISSING));
                };
                set_asciz(p, at::PATH, &pattern, 128);
                *phase = delete::ATTRIBUTES;
                Flow::Call(call(p, 0x4300, 0, 0, at::PATH))
            }
            delete::ATTRIBUTES => {
                let mut pattern = asciz_at(p, at::PATH, 128);
                if child.is_ok() && p.result(Returned::Cx) & 0x10 != 0 {
                    if !pattern.ends_with(b"\\") {
                        pattern.push(b'\\');
                    }
                    pattern.extend_from_slice(b"*.*");
                    set_asciz(p, at::PATH, &pattern, 128);
                }
                let name = &pattern[directory_of(&pattern).len()..];
                if every_file(name) && !self.asking {
                    *phase = delete::SURE;
                    return Flow::Call(say(
                        p,
                        b"All files in directory will be deleted!\r\nAre you sure (Y/N)?",
                    ));
                }
                *phase = delete::SEARCH_FROM;
                self.step(p, phase, Ok(0))
            }
            delete::SURE => {
                p.set8(at::LINE, at::LINE_ROOM);
                *phase = delete::ANSWERED;
                Flow::Call(call(p, 0x0A00, 0, 0, at::LINE))
            }
            delete::ANSWERED => {
                let yes =
                    p.get8(at::LINE + 1) > 0 && p.get8(at::LINE + 2).eq_ignore_ascii_case(&b'Y');
                *phase = if yes {
                    delete::SEARCH_FROM
                } else {
                    delete::DONE
                };
                Flow::Call(say(p, b"\n"))
            }
            delete::SEARCH_FROM => {
                *phase = delete::SEARCH;
                Flow::Call(call(p, 0x1A00, 0, 0, at::DTA))
            }
            delete::SEARCH => {
                *phase = delete::FOUND;
                if self.searched {
                    return Flow::Call(call(p, 0x4F00, 0, 0, 0));
                }
                self.searched = true;
                Flow::Call(call(p, 0x4E00, 0, 0, at::PATH))
            }
            delete::FOUND => self.found(p, phase, child),
            delete::ASKED => {
                *phase = delete::KEYED;
                Flow::Call(call(p, 0x0C01, 0, 0, 0))
            }
            delete::KEYED => {
                let yes = take!(child).to_le_bytes()[0].eq_ignore_ascii_case(&b'Y');
                *phase = if yes { delete::DELETE } else { delete::SEARCH };
                Flow::Call(say(p, text::NEW_LINE))
            }
            delete::DELETE => {
                *phase = delete::DELETED;
                Flow::Call(call(p, 0x4100, 0, 0, at::PATH2))
            }
            delete::DELETED => {
                *phase = delete::SEARCH;
                if child == Err(Fault::Error(crate::dos::fs::error::ACCESS_DENIED)) {
                    return Flow::Call(say(p, text::ACCESS_DENIED));
                }
                self.step(p, phase, Ok(0))
            }
            _ => Flow::Return(Ok(0)),
        }
    }
}

impl Delete {
    /// What a search for the next match answered: the end of the matches, or
    /// one to delete, or to ask about first.
    fn found(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        if child.is_err() {
            *phase = delete::DONE;
            if self.any {
                return Flow::Return(Ok(0));
            }
            return Flow::Call(say(p, text::FILE_NOT_FOUND));
        }
        self.any = true;
        let pattern = asciz_at(p, at::PATH, 128);
        let name = asciz_at(p, at::DTA + finding::NAME, 13);
        let full = [directory_of(&pattern), &name].concat();
        set_asciz(p, at::PATH2, &full, 128);
        if !self.asking {
            *phase = delete::DELETE;
            return self.step(p, phase, Ok(0));
        }
        let mut question = name;
        question.resize(12, b' ');
        question.extend_from_slice(b",    Delete (Y/N)?");
        *phase = delete::ASKED;
        Flow::Call(say(p, &question))
    }
}

locals! {
    /// `REN old new`: the files `old` matches renamed, a `?` in `new`
    /// keeping the byte the old name has there.
    pub struct Rename {
        found: bool,
        any: bool,
    }
}

mod rename {
    pub const DTA_SET: u8 = 1;
    pub const SEARCHED: u8 = 2;
    pub const RENAMED: u8 = 3;
    pub const DONE: u8 = 4;
}

/// `name` renamed by `pattern` as DOS renames: each `?` keeps the byte the
/// name has there.
fn renamed(name: &[u8], pattern: &[u8; 11]) -> Option<Vec<u8>> {
    let old = name::field(name, false).ok()?;
    let mut field = *pattern;
    for (slot, &byte) in field.iter_mut().zip(&old) {
        if *slot == b'?' {
            *slot = byte;
        }
    }
    let mut out = [0u8; 13];
    let len = name::spell(&field, &mut out);
    (len > 0).then(|| out[..len].to_vec())
}

impl Routine for Rename {
    const ID: Id = Id::Rename;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        if answered(child) {
            return Flow::Return(Ok(0));
        }
        let args = text_at(p, at::ARGS);
        let arguments: Vec<Vec<u8>> = parse::arguments(&args)
            .into_iter()
            .map(<[u8]>::to_vec)
            .collect();
        match *phase {
            0 => {
                let [old, new, ..] = arguments.as_slice() else {
                    *phase = rename::DONE;
                    return Flow::Call(say(p, text::MISSING));
                };
                if new.iter().any(|&b| b == b'\\' || b == b':') {
                    *phase = rename::DONE;
                    return Flow::Call(say(p, text::BAD_PARAMETER));
                }
                set_asciz(p, at::PATH, old, 128);
                *phase = rename::DTA_SET;
                Flow::Call(call(p, 0x1A00, 0, 0, at::DTA))
            }
            rename::DTA_SET => {
                *phase = rename::SEARCHED;
                if self.found {
                    return Flow::Call(call(p, 0x4F00, 0, 0, 0));
                }
                self.found = true;
                Flow::Call(call(p, 0x4E00, 0, 0, at::PATH))
            }
            rename::SEARCHED => {
                let Ok(_) = child else {
                    *phase = rename::DONE;
                    if self.any {
                        return Flow::Return(Ok(0));
                    }
                    return Flow::Call(say(p, DUPLICATE));
                };
                self.any = true;
                let old = asciz_at(p, at::PATH, 128);
                let directory = directory_of(&old).to_vec();
                let name = asciz_at(p, at::DTA + finding::NAME, 13);
                let pattern = arguments.get(1).and_then(|new| name::field(new, true).ok());
                let Some(new) = pattern.and_then(|pattern| renamed(&name, &pattern)) else {
                    *phase = rename::DONE;
                    return Flow::Call(say(p, text::BAD_PARAMETER));
                };
                set_asciz(p, at::PATH2, &[&directory[..], &name].concat(), 128);
                set_asciz(p, at::NAME_OUT, &[&directory[..], &new].concat(), 80);
                *phase = rename::RENAMED;
                Flow::Call(Frame::of(&Int {
                    dx: at::PATH2,
                    di: at::NAME_OUT,
                    ..Int::dos(p.own, 0x5600)
                }))
            }
            rename::RENAMED => {
                *phase = rename::DTA_SET;
                if child.is_err() {
                    *phase = rename::DONE;
                    return Flow::Call(say(p, DUPLICATE));
                }
                self.step(p, phase, Ok(0))
            }
            _ => Flow::Return(Ok(0)),
        }
    }
}
