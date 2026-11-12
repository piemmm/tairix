//! `COMMAND.COM`: TAIRiX DOS's command interpreter, a program whose
//! behaviour is Rust (`dos::program`). It reads a line at its prompt or from
//! a batch file, splits it into a pipeline's stages, redirects each stage's
//! standard input and output, and runs an internal command or a program
//! found along `PATH`. Its state — the line, the master environment's
//! segment, the batch files running, a `FOR` in progress — lies in its own
//! memory, and it reaches DOS through INT 21h like any program.
//!
//! The primary shell (`/P`) runs `AUTOEXEC.BAT`, or asks for the date and
//! time without one, and never ends; a secondary one ends at `EXIT`, and
//! `/C` runs one command and ends. Its INT 24h handler asks "Abort, Retry,
//! Fail?", and its INT 23h handler ends a batch file when asked to and stops
//! the command running, the shell's own or a program's.

pub(crate) mod batch;
pub(crate) mod copy;
pub(crate) mod dates;
pub(crate) mod dir;
pub(crate) mod handlers;
pub(crate) mod internal;
pub mod parse;
pub(crate) mod run;

use batch::{BatchLine, BatchStart};
use internal::{DateCommand, TimeCommand};
use run::{Execute, Release, Restore};

use super::program::{self, say, Flow, Frame, Id, Int, Line, Program, Returned, Routine, ENDED};
use crate::asm::AsmError;
use crate::dos::utility::doskey::multiplex;
use crate::frames::{locals, take, Fault, Outcome};
use alloc::vec::Vec;

/// The shell's program number.
pub const NUMBER: u16 = 1;

/// `COMMAND.COM`'s file.
///
/// # Errors
///
/// [`AsmError`] should its stub not assemble, a defect of its own.
pub fn image() -> Result<Vec<u8>, AsmError> {
    program::image(NUMBER)
}

/// Where the shell's state lies in its memory.
pub mod at {
    use crate::dos::program::at::STATE;

    /// [`super::flag`]s.
    pub const FLAGS: u16 = STATE;
    /// The master environment's segment and its size in bytes.
    pub const ENVIRONMENT: u16 = STATE + 0x02;
    pub const ENV_SIZE: u16 = STATE + 0x04;
    /// The batch file running's block, zero for none.
    pub const BATCH: u16 = STATE + 0x06;
    /// The code the last program ended with.
    pub const ERRORLEVEL: u16 = STATE + 0x08;
    /// Standard input's and output's handles while a stage redirects them,
    /// `FFFFh` while they are not.
    pub const SAVED_IN: u16 = STATE + 0x0A;
    pub const SAVED_OUT: u16 = STATE + 0x0C;
    /// A `FOR` running: its variable, where its set has reached, whether a
    /// wildcard's search is under way.
    pub const FOR_RUNNING: u16 = STATE + 0x0E;
    pub const FOR_VARIABLE: u16 = STATE + 0x0F;
    pub const FOR_AT: u16 = STATE + 0x10;
    pub const FOR_FINDING: u16 = STATE + 0x12;
    /// The drive the shell was started from, for `AUTOEXEC.BAT`.
    pub const BOOT_DRIVE: u16 = STATE + 0x13;
    /// The environment size `/E` asks for.
    pub const ENV_WANTED: u16 = STATE + 0x14;
    /// Blocks the running command holds, zero for none: the shell frees
    /// them however the command ends.
    pub const HELD: u16 = STATE + 0x16;
    pub const HELD_COUNT: u16 = 2;
    /// While a handler asks the console: the handles that keep the standard
    /// input and output of the process it interrupted, and that process's
    /// PSP, zero for none.
    pub const ASKING: u16 = STATE + 0x1A;
    pub const ASKING_PSP: u16 = STATE + 0x1C;
    /// DOS's line input buffer: its room, what was typed, the line.
    pub const LINE: u16 = STATE + 0x20;
    pub const LINE_ROOM: u8 = 128;
    /// Length-prefixed texts: the line being run, the stage being run, the
    /// command left once its redirections are taken out, its arguments, and
    /// the command word of a program.
    pub const WORK: u16 = STATE + 0x0A2;
    pub const STAGE: u16 = STATE + 0x124;
    pub const COMMAND: u16 = STATE + 0x1A6;
    pub const ARGS: u16 = STATE + 0x228;
    pub const WORD: u16 = STATE + 0x2AA;
    /// The files a stage redirects from and to, as ASCIZ, and whether it
    /// appends.
    pub const NAME_IN: u16 = STATE + 0x32C;
    pub const NAME_OUT: u16 = STATE + 0x37C;
    pub const APPEND: u16 = STATE + 0x3CC;
    /// ASCIZ paths a command works with.
    pub const PATH: u16 = STATE + 0x3D0;
    pub const PATH2: u16 = STATE + 0x450;
    /// EXEC's command tail, parameter block and FCBs.
    pub const TAIL: u16 = STATE + 0x4D0;
    pub const EXEC: u16 = STATE + 0x550;
    pub const FCB1: u16 = STATE + 0x560;
    pub const FCB2: u16 = STATE + 0x588;
    /// The DTA searches report into.
    pub const DTA: u16 = STATE + 0x5B0;
    /// The DTA a `FOR`'s search keeps.
    pub const FOR_DTA: u16 = STATE + 0x600;
    /// A `FOR`'s set and the command it runs, length-prefixed.
    pub const FOR_SET: u16 = STATE + 0x650;
    pub const FOR_COMMAND: u16 = STATE + 0x6D2;
    /// The temporary files a pipeline passes its output through, ASCIZ.
    pub const PIPES: u16 = STATE + 0x754;
    pub const PIPE_LEN: u16 = 0x50;
    /// A pipeline's stages.
    pub const PIPE_COUNT: u16 = STATE + 0x7F4;
    /// The shell's own path, for `COMSPEC`.
    pub const COMSPEC: u16 = STATE + 0x800;
    /// The country's information, as function `38h` gives it.
    pub const COUNTRY: u16 = STATE + 0x850;
    /// A buffer files are read through: `TYPE`, `COPY`, batch lines.
    pub const BUFFER: u16 = STATE + 0x880;
    pub const BUFFER_LEN: u16 = 0x800;
    /// `DIR`'s state, which outgrows a routine's locals: see `dir`.
    pub const DIR: u16 = STATE + 0x1080;
    pub const DIR_LEN: u16 = 0x100;
    /// Bytes of the shell's state.
    pub const LEN: u16 = 0x1180;
}

/// What [`at::FLAGS`] holds.
pub mod flag {
    /// The primary shell, `/P`: it runs `AUTOEXEC.BAT` and never ends.
    pub const PRIMARY: u16 = 0x01;
    /// Batch lines are echoed.
    pub const ECHO: u16 = 0x02;
    /// `/C`: one command, then the shell ends.
    pub const ONE: u16 = 0x04;
    /// `EXIT` has asked a shell that may end to.
    pub const EXITING: u16 = 0x08;
    /// The INT 23h handler was told to end the batch files running.
    pub const BATCH_BROKEN: u16 = 0x10;
    /// The line last run, labels passed over, was empty: no new line comes
    /// before the next prompt or echoed line, as there was no output to end.
    pub const NULL_LINE: u16 = 0x20;
}

/// The handlers the shell installs, by their stub's number.
pub mod handler {
    pub const BREAK: u16 = 0;
    pub const CRITICAL: u16 = 1;
}

/// The version DOS gives itself, as `VER` and the shell's banner spell it.
macro_rules! version {
    () => {
        "TAIRiX DOS Version 5.00"
    };
}

/// What the shell says.
pub mod text {
    pub use crate::dos::message::{
        ACCESS_DENIED, BAD_DRIVE, BAD_PARAMETER, CREATION, FILE_NOT_FOUND, MISSING, NEW_LINE,
    };
    pub const BAD_COMMAND: &[u8] = b"Bad command or file name\r\n";
    pub const BAD_DEVICE: &[u8] = b"Invalid device\r\n";
    pub const BAD_CURRENT_DRIVE: &[u8] = b"Current drive is no longer valid";
    pub const SYNTAX: &[u8] = b"Syntax error\r\n";
    pub const BAD_DIRECTORY: &[u8] = b"Invalid directory\r\n";
    pub const NO_ROOM: &[u8] = b"Out of environment space\r\n";
    pub const BAD_COMSPEC: &[u8] = b"Specified COMMAND search directory bad\r\n";
    pub const TOO_BIG: &[u8] = b"Program too big to fit in memory\r\n";
    pub const BANNER: &[u8] = concat!("\r\n", version!(), "\r\n").as_bytes();
    /// What `VER` prints.
    pub const VERSION: &[u8] = version!().as_bytes();
}

/// The command interpreter's file.
pub(crate) const FILE: &[u8] = b"COMMAND.COM";

/// The text a length-prefixed buffer at `at` holds.
pub(crate) fn text_at(p: &mut Program<'_, '_>, at: u16) -> Vec<u8> {
    let len = p
        .get8(at)
        .min(u8::try_from(parse::LINE_MAX).unwrap_or(u8::MAX));
    let mut out = alloc::vec![0u8; usize::from(len)];
    p.g.read_bytes(p.own, at + 1, &mut out);
    out
}

/// Sets the length-prefixed buffer at `at` to `text`, cut to a line.
pub(crate) fn set_text(p: &mut Program<'_, '_>, at: u16, text: &[u8]) {
    let len = text.len().min(parse::LINE_MAX);
    p.set8(at, u8::try_from(len).unwrap_or(0));
    p.g.write_bytes(p.own, at + 1, &text[..len]);
}

/// The ASCIZ text at `at`, at most `max` bytes of it.
pub(crate) fn asciz_at(p: &mut Program<'_, '_>, at: u16, max: u16) -> Vec<u8> {
    let mut out = Vec::new();
    for n in 0..max {
        let byte = p.get8(at + n);
        if byte == 0 {
            break;
        }
        out.push(byte);
    }
    out
}

/// The label a volume-label search reported in the DTA at `dta`: the name
/// found, with the dot set after a long label's eighth character taken out.
pub(crate) fn found_label(p: &mut Program<'_, '_>, dta: u16) -> Vec<u8> {
    let name = asciz_at(p, dta + crate::dos::dirops::finding::NAME, 13);
    name.into_iter().filter(|&b| b != b'.').collect()
}

/// Sets the ASCIZ text at `at` to `text`, cut to `max` bytes with its NUL.
pub(crate) fn set_asciz(p: &mut Program<'_, '_>, at: u16, text: &[u8], max: u16) {
    let len = text.len().min(usize::from(max.saturating_sub(1)));
    p.g.write_bytes(p.own, at, &text[..len]);
    p.set8(at + u16::try_from(len).unwrap_or(0), 0);
}

/// Whether `outcome` is a failure the critical-error handler has told of
/// and been answered for: the command stops there, asking the device
/// nothing more and saying nothing more of it.
pub(crate) fn answered(outcome: Outcome) -> bool {
    outcome == Err(Fault::Error(crate::dos::fs::error::FAIL_ON_24))
}

pub(crate) fn flags(p: &mut Program<'_, '_>) -> u16 {
    p.get16(at::FLAGS)
}

pub(crate) fn set_flag(p: &mut Program<'_, '_>, mask: u16, on: bool) {
    let flags = flags(p);
    p.set16(at::FLAGS, if on { flags | mask } else { flags & !mask });
}

/// The variables of the environment at `segment`, `NAME=value` each, as far
/// as its empty one, within `size` bytes.
fn variables_at(p: &mut Program<'_, '_>, segment: u16, size: u16) -> Vec<Vec<u8>> {
    let mut block = alloc::vec![0u8; usize::from(size)];
    p.g.read_bytes(segment, 0, &mut block);
    let mut out = Vec::new();
    let mut rest = block.as_slice();
    while let Some(end) = rest.iter().position(|&b| b == 0) {
        if end == 0 {
            break;
        }
        out.push(rest[..end].to_vec());
        rest = &rest[end + 1..];
    }
    out
}

/// Whether `entry`, `NAME=value`, is variable `name`'s, case aside.
fn names(entry: &[u8], name: &[u8]) -> bool {
    entry
        .iter()
        .position(|&b| b == b'=')
        .is_some_and(|equals| entry[..equals].eq_ignore_ascii_case(name))
}

/// Bytes `entries` take in an environment, its closing empty one included.
fn environment_bytes(entries: &[Vec<u8>]) -> usize {
    entries.iter().map(|entry| entry.len() + 1).sum::<usize>() + 1
}

/// The master environment's variables, `NAME=value` each.
pub(crate) fn environment(p: &mut Program<'_, '_>) -> Vec<Vec<u8>> {
    let (segment, size) = (p.get16(at::ENVIRONMENT), p.get16(at::ENV_SIZE));
    variables_at(p, segment, size)
}

/// Writes `entries` as the master environment, as many whole ones as it
/// holds: whether all of them fitted.
fn write_environment(p: &mut Program<'_, '_>, entries: &[Vec<u8>]) -> bool {
    let size = usize::from(p.get16(at::ENV_SIZE));
    let mut block = Vec::with_capacity(size);
    let mut all = true;
    for entry in entries {
        if block.len() + entry.len() + 2 > size {
            all = false;
            break;
        }
        block.extend_from_slice(entry);
        block.push(0);
    }
    block.push(0);
    let segment = p.get16(at::ENVIRONMENT);
    p.g.write_bytes(segment, 0, &block);
    all
}

/// The value of the environment variable `name`.
pub(crate) fn variable(p: &mut Program<'_, '_>, name: &[u8]) -> Option<Vec<u8>> {
    environment(p)
        .into_iter()
        .find(|entry| names(entry, name))
        .map(|entry| entry[name.len() + 1..].to_vec())
}

/// Variable `name`'s value in the environment a program was given: the
/// block its PSP names, as far as that block holds.
pub(crate) fn program_variable(p: &mut Program<'_, '_>, name: &[u8]) -> Option<Vec<u8>> {
    let segment = p.get16(crate::dos::memory::psp::ENVIRONMENT);
    if segment == 0 {
        return None;
    }
    let paragraphs =
        p.g.read16(segment.wrapping_sub(1), crate::dos::memory::mcb::SIZE)
            .min(0x800);
    variables_at(p, segment, paragraphs * 16)
        .into_iter()
        .find(|entry| names(entry, name))
        .map(|entry| entry[name.len() + 1..].to_vec())
}

/// Sets the environment variable `name` to `value`, or removes it for an
/// empty value: `false`, nothing changed, when the environment has no room
/// for it.
pub(crate) fn set_variable(p: &mut Program<'_, '_>, name: &[u8], value: &[u8]) -> bool {
    let name = name.to_ascii_uppercase();
    let mut entries = environment(p);
    entries.retain(|entry| !names(entry, &name));
    if !value.is_empty() {
        entries.push([&name[..], b"=", value].concat());
    }
    environment_bytes(&entries) <= usize::from(p.get16(at::ENV_SIZE))
        && write_environment(p, &entries)
}

/// Drive `drive`'s letter, as COMMAND.COM makes it: `A` plus the number in
/// a byte, whatever number DOS or a program hooking it gave.
pub(crate) const fn letter(drive: u8) -> u8 {
    b'A'.wrapping_add(drive)
}

locals! {
    /// The shell: its start, then a line at a time from the prompt or the
    /// batch file running, until a shell that may end is told to.
    pub struct Shell {
        old_environment: u16,
    }
}

/// The root's phases.
mod phase {
    pub const BOOTED: u8 = 1;
    pub const OPENED: u8 = 2;
    pub const CHECKED: u8 = 3;
    pub const SHRUNK: u8 = 4;
    pub const ALLOCATED: u8 = 5;
    pub const ROOM_TOLD: u8 = 6;
    pub const FREED: u8 = 7;
    pub const BREAK_SET: u8 = 8;
    pub const CRITICAL_SET: u8 = 9;
    pub const COUNTRY: u8 = 10;
    pub const DTA_SET: u8 = 11;
    pub const LOOKED: u8 = 12;
    pub const DATED: u8 = 13;
    pub const TIMED: u8 = 14;
    pub const BANNER: u8 = 15;
    pub const LOOP: u8 = 16;
    pub const PROMPTED: u8 = 17;
    pub const READ: u8 = 18;
    pub const BATCH_READ: u8 = 19;
    pub const RAN: u8 = 20;
    pub const BROKEN: u8 = 21;
    pub const RELEASED: u8 = 22;
    pub const PROMPT_LINE: u8 = 23;
    pub const SETTLED: u8 = 24;
}

/// The environment's default size, and the range `/E` may ask for, which
/// also bounds an environment the shell is given.
const ENV_DEFAULT: u16 = 256;
const ENV_RANGE: core::ops::RangeInclusive<u16> = 160..=32768;

impl Routine for Shell {
    const ID: Id = Id::Shell;

    #[allow(
        clippy::too_many_lines,
        reason = "one arm a phase, in the order the routine runs"
    )]
    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        if child == Err(Fault::Break) && *phase >= phase::LOOP {
            // A Ctrl+C abandoned the command: its redirections are undone,
            // and the prompt comes again.
            *phase = phase::BROKEN;
            return Flow::Call(Frame::of(&Restore {}));
        }
        match *phase {
            0 => self.start(p, phase),
            phase::BOOTED => {
                let drive = p.result(Returned::Dx).to_le_bytes()[0].saturating_sub(1);
                p.set8(at::BOOT_DRIVE, drive);
                let mut path = asciz_at(p, at::COMSPEC, 80);
                *phase = phase::CHECKED;
                if path.is_empty() {
                    return self.step(p, phase, Ok(0));
                }
                if !path.ends_with(b"\\") && !path.ends_with(b":") {
                    path.push(b'\\');
                }
                path.extend_from_slice(FILE);
                set_asciz(p, at::COMSPEC, &path.to_ascii_uppercase(), 80);
                *phase = phase::OPENED;
                Flow::Call(Frame::of(&Int {
                    dx: at::COMSPEC,
                    ..Int::dos(p.own, 0x3D00)
                }))
            }
            // A directory the shell is not in names no shell to come back to.
            phase::OPENED => {
                *phase = phase::CHECKED;
                let Ok(handle) = child else {
                    p.set8(at::COMSPEC, 0);
                    return Flow::Call(say(p, text::BAD_COMSPEC));
                };
                Flow::Call(Frame::of(&Int {
                    bx: u16::try_from(handle).unwrap_or(0),
                    ..Int::dos(p.own, 0x3E00)
                }))
            }
            phase::CHECKED => {
                *phase = phase::SHRUNK;
                Flow::Call(p.shrink())
            }
            phase::SHRUNK => {
                take!(child);
                let given = environment_bytes(&self.starting(p));
                let given = u16::try_from(given.next_multiple_of(16)).unwrap_or(u16::MAX);
                let size = p.get16(at::ENV_WANTED).max(given).min(*ENV_RANGE.end());
                p.set16(at::ENV_WANTED, size);
                *phase = phase::ALLOCATED;
                Flow::Call(Frame::of(&Int {
                    bx: size.div_ceil(16),
                    ..Int::dos(p.own, 0x4800)
                }))
            }
            phase::ALLOCATED => {
                let segment = u16::try_from(take!(child)).unwrap_or(0);
                let whole = self.make_environment(p, segment);
                *phase = phase::ROOM_TOLD;
                if whole {
                    return self.step(p, phase, Ok(0));
                }
                Flow::Call(say(p, text::NO_ROOM))
            }
            phase::ROOM_TOLD => {
                *phase = phase::FREED;
                if self.old_environment == 0 {
                    return self.step(p, phase, Ok(0));
                }
                Flow::Call(Frame::of(&Int {
                    es: self.old_environment,
                    ..Int::dos(p.own, 0x4900)
                }))
            }
            phase::FREED => {
                *phase = phase::BREAK_SET;
                Flow::Call(Frame::of(&Int {
                    dx: program::handler_entry(handler::BREAK),
                    ..Int::dos(p.own, 0x2523)
                }))
            }
            phase::BREAK_SET => {
                *phase = phase::CRITICAL_SET;
                Flow::Call(Frame::of(&Int {
                    dx: program::handler_entry(handler::CRITICAL),
                    ..Int::dos(p.own, 0x2524)
                }))
            }
            phase::CRITICAL_SET => {
                *phase = phase::COUNTRY;
                Flow::Call(Frame::of(&Int {
                    dx: at::COUNTRY,
                    ..Int::dos(p.own, 0x3800)
                }))
            }
            phase::COUNTRY => Self::first_command(p, phase),
            phase::DTA_SET => {
                let drive = p.get8(at::BOOT_DRIVE);
                let mut path = alloc::vec![letter(drive), b':', b'\\'];
                path.extend_from_slice(b"AUTOEXEC.BAT");
                set_asciz(p, at::PATH, &path, 128);
                *phase = phase::LOOKED;
                Flow::Call(Frame::of(&Int {
                    dx: at::PATH,
                    ..Int::dos(p.own, 0x4E00)
                }))
            }
            phase::LOOKED if child.is_ok() => {
                *phase = phase::LOOP;
                set_text(p, at::WORD, b"AUTOEXEC");
                set_text(p, at::ARGS, b"");
                Flow::Call(Frame::of(&BatchStart { call: false }))
            }
            phase::LOOKED => {
                *phase = phase::DATED;
                set_text(p, at::ARGS, b"");
                Flow::Call(Frame::of(&DateCommand::default()))
            }
            phase::DATED => {
                *phase = phase::TIMED;
                set_text(p, at::ARGS, b"");
                Flow::Call(Frame::of(&TimeCommand::default()))
            }
            phase::TIMED => {
                *phase = phase::LOOP;
                Flow::Call(say(p, text::BANNER))
            }
            phase::BROKEN => {
                p.set8(at::FOR_RUNNING, 0);
                *phase = phase::RAN;
                self.step(p, phase, Ok(0))
            }
            phase::BANNER | phase::LOOP => self.next_line(p, phase, false),
            // However the command ended, what it held is given back: a
            // handler's console a break left, then its blocks.
            phase::RAN => {
                *phase = phase::SETTLED;
                Flow::Call(Frame::of(&handlers::Console::undo()))
            }
            phase::SETTLED => {
                *phase = phase::RELEASED;
                Flow::Call(Frame::of(&Release {}))
            }
            phase::RELEASED => self.next_line(p, phase, true),
            phase::PROMPT_LINE => {
                *phase = phase::PROMPTED;
                Flow::Call(Frame::of(&Prompt::default()))
            }
            phase::PROMPTED => {
                *phase = phase::READ;
                p.set8(at::LINE, at::LINE_ROOM);
                Flow::Call(Frame::of(&ReadLine {}))
            }
            phase::READ | phase::BATCH_READ => {
                let got = take!(child);
                let batch = *phase == phase::BATCH_READ;
                *phase = phase::RAN;
                if batch && got == 0 {
                    return self.step(p, phase, Ok(0));
                }
                Flow::Call(Frame::of(&Execute { batch, stage: 0 }))
            }
            _ => Flow::Return(Ok(0)),
        }
    }
}

impl Shell {
    /// Reads the shell's switches, and gives back the memory the shell does
    /// not keep.
    fn start(&mut self, p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        let tail = p.tail();
        let tail = tail.as_slice();
        let (switches, command) = match find_switch(tail, b'C') {
            Some(at) => (&tail[..at], Some(&tail[at + 2..])),
            None => (tail, None),
        };
        let mut flags = flag::ECHO;
        let mut wanted = ENV_DEFAULT;
        let mut comspec: Option<Vec<u8>> = None;
        for argument in parse::arguments(switches) {
            match argument.to_ascii_uppercase().as_slice() {
                b"/P" => flags |= flag::PRIMARY,
                b"/MSG" => {}
                [b'/', b'E', b':', digits @ ..] => {
                    if let Some(size) = parse::number(digits).and_then(|s| u16::try_from(s).ok()) {
                        wanted = size.clamp(*ENV_RANGE.start(), *ENV_RANGE.end());
                    }
                }
                _ if comspec.is_none() && !argument.starts_with(b"/") => {
                    comspec = Some(argument.to_vec());
                }
                _ => {}
            }
        }
        if let Some(command) = command {
            flags |= flag::ONE;
            set_text(p, at::WORK, parse::trim(command));
        }
        p.set16(at::FLAGS, flags);
        p.set16(at::ENV_WANTED, wanted.next_multiple_of(16));
        p.set16(at::BATCH, 0);
        p.set16(at::ERRORLEVEL, 0);
        p.set16(at::SAVED_IN, 0xFFFF);
        p.set16(at::SAVED_OUT, 0xFFFF);
        p.set8(at::FOR_RUNNING, 0);
        for n in 0..at::HELD_COUNT {
            p.set16(at::HELD + n * 2, 0);
        }
        set_asciz(p, at::COMSPEC, comspec.as_deref().unwrap_or_default(), 80);
        self.old_environment = p.get16(super::memory::psp::ENVIRONMENT);
        *phase = phase::BOOTED;
        Flow::Call(Frame::of(&Int::dos(p.own, 0x3305)))
    }

    /// The variables the shell starts with: those it was given, `COMSPEC`
    /// naming the directory it was told it is in, else the one it was
    /// given, else the boot drive's root.
    fn starting(self, p: &mut Program<'_, '_>) -> Vec<Vec<u8>> {
        let mut entries = if self.old_environment == 0 {
            Vec::new()
        } else {
            variables_at(p, self.old_environment, *ENV_RANGE.end())
        };
        let told = asciz_at(p, at::COMSPEC, 80);
        if told.is_empty() && entries.iter().any(|entry| names(entry, b"COMSPEC")) {
            return entries;
        }
        entries.retain(|entry| !names(entry, b"COMSPEC"));
        let path = if told.is_empty() {
            [&[letter(p.get8(at::BOOT_DRIVE)), b':', b'\\'][..], FILE].concat()
        } else {
            told
        };
        entries.push([&b"COMSPEC="[..], &path].concat());
        entries
    }

    /// The master environment in its new block at `segment`: whether every
    /// variable the shell starts with fitted.
    fn make_environment(self, p: &mut Program<'_, '_>, segment: u16) -> bool {
        let entries = self.starting(p);
        p.set16(at::ENVIRONMENT, segment);
        let size = p.get16(at::ENV_WANTED);
        p.set16(at::ENV_SIZE, size);
        p.set16(super::memory::psp::ENVIRONMENT, segment);
        write_environment(p, &entries)
    }

    /// The command `/C` gave, `AUTOEXEC.BAT` or the date and time for the
    /// primary shell, or the banner for another.
    fn first_command(p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        let flags = flags(p);
        if flags & flag::ONE != 0 {
            *phase = phase::RAN;
            return Flow::Call(Frame::of(&Execute {
                batch: false,
                stage: 0,
            }));
        }
        if flags & flag::PRIMARY != 0 {
            *phase = phase::DTA_SET;
            return Flow::Call(Frame::of(&Int {
                dx: at::DTA,
                ..Int::dos(p.own, 0x1A00)
            }));
        }
        *phase = phase::BANNER;
        Flow::Call(say(p, text::BANNER))
    }

    /// The next line: from the batch file running, else from the prompt;
    /// or the end, for a shell that has run its one command, `ran` saying a
    /// command just ended, or been told to exit.
    fn next_line(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, ran: bool) -> Flow {
        let flags = flags(p);
        if flags & flag::ONE != 0 && ran && p.get16(at::BATCH) == 0 {
            let code = p.get8(at::ERRORLEVEL);
            return Flow::Return(Ok(u32::from(code)));
        }
        if flags & flag::EXITING != 0 && flags & flag::PRIMARY == 0 {
            return Flow::Return(Ok(0));
        }
        set_flag(p, flag::EXITING, false);
        if p.get16(at::BATCH) != 0 {
            *phase = phase::BATCH_READ;
            return Flow::Call(Frame::of(&BatchLine::default()));
        }
        *phase = phase::PROMPTED;
        if flags & flag::ECHO == 0 {
            return self.step(p, phase, Ok(0));
        }
        if flags & flag::NULL_LINE == 0 {
            *phase = phase::PROMPT_LINE;
            return Flow::Call(say(p, text::NEW_LINE));
        }
        Flow::Call(Frame::of(&Prompt::default()))
    }
}

/// Where `/letter` begins in `tail`, case aside.
fn find_switch(tail: &[u8], letter: u8) -> Option<usize> {
    tail.windows(2)
        .position(|pair| pair[0] == b'/' && pair[1].eq_ignore_ascii_case(&letter))
}

locals! {
    /// Reads a line at the prompt into [`at::LINE`], then the line being run:
    /// the bytes typed. `DOSKEY` is asked for it first, as MS-DOS 5's shell
    /// asks, so macros expand. An input with no line left ends the shell as
    /// `EXIT` would, one that may end.
    pub struct ReadLine {}
}

mod reading {
    pub const READ: u8 = 1;
    pub const SAID: u8 = 2;
    pub const CHECKED: u8 = 3;
    pub const KEYED: u8 = 4;
}

impl ReadLine {
    fn read(phase: &mut u8) -> Flow {
        *phase = reading::READ;
        Flow::Call(Frame::of(&Line::at(at::LINE)))
    }

    /// The line the buffer holds taken as the one to run.
    fn take_line(p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        let typed = usize::from(p.get8(at::LINE + 1).min(at::LINE_ROOM - 1));
        let mut line = alloc::vec![0u8; typed];
        p.g.read_bytes(p.own, at::LINE + 2, &mut line);
        set_text(p, at::WORK, &line);
        *phase = reading::SAID;
        Flow::Call(say(p, b"\n"))
    }
}

impl Routine for ReadLine {
    const ID: Id = Id::ReadLine;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        match *phase {
            reading::CHECKED => {
                if !child.is_ok_and(|ax| ax.to_le_bytes()[0] == multiplex::INSTALLED) {
                    return Self::read(phase);
                }
                *phase = reading::KEYED;
                Flow::Call(Frame::of(&Int {
                    dx: at::LINE,
                    ..Int::bios(
                        p.own,
                        0x2F,
                        u16::from_le_bytes([multiplex::READ, multiplex::AH]),
                    )
                }))
            }
            reading::KEYED => {
                if child.is_ok() && p.result(Returned::Ax) == 0 {
                    return Self::take_line(p, phase);
                }
                Self::read(phase)
            }
            reading::READ => {
                if take!(child) == ENDED {
                    set_flag(p, flag::EXITING, true);
                    set_text(p, at::WORK, b"");
                    *phase = reading::SAID;
                    return Flow::Call(say(p, text::NEW_LINE));
                }
                Self::take_line(p, phase)
            }
            reading::SAID => Flow::Return(Ok(1)),
            _ => {
                *phase = reading::CHECKED;
                Flow::Call(Frame::of(&Int::bios(p.own, 0x2F, multiplex::CHECK)))
            }
        }
    }
}

locals! {
    /// Writes the prompt `PROMPT` spells, `$N$G` without one, asking DOS
    /// for the directory, date and time only when the prompt shows them.
    pub struct Prompt {
        lost: bool,
    }
}

impl Routine for Prompt {
    const ID: Id = Id::Prompt;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        let spec = variable(p, b"PROMPT").unwrap_or_else(|| b"$N$G".to_vec());
        let upper = spec.to_ascii_uppercase();
        let wants = |code: u8| upper.windows(2).any(|pair| pair == [b'$', code]);
        match *phase {
            0 => {
                *phase = 1;
                Flow::Call(Frame::of(&Int::dos(p.own, 0x1900)))
            }
            1 => {
                let drive = take!(child).to_le_bytes()[0];
                p.set8(at::PATH, letter(drive));
                p.g.write_bytes(p.own, at::PATH + 1, b":\\");
                p.set8(at::PATH + 3, 0);
                *phase = 2;
                if !wants(b'P') {
                    return self.step(p, phase, Ok(0));
                }
                Flow::Call(Frame::of(&Int {
                    si: at::PATH + 3,
                    ..Int::dos(p.own, 0x4700)
                }))
            }
            2 => {
                self.lost = child.is_err();
                *phase = 3;
                if !wants(b'D') {
                    return self.step(p, phase, Ok(0));
                }
                Flow::Call(Frame::of(&Int::dos(p.own, 0x2A00)))
            }
            3 => {
                if wants(b'D') {
                    let date = internal::date_text(p);
                    set_text(p, at::PATH2, &date);
                }
                *phase = 4;
                if !wants(b'T') {
                    return self.step(p, phase, Ok(0));
                }
                Flow::Call(Frame::of(&Int::dos(p.own, 0x2C00)))
            }
            4 => {
                let time = if wants(b'T') {
                    internal::time_text(p)
                } else {
                    Vec::new()
                };
                let date = if wants(b'D') {
                    text_at(p, at::PATH2)
                } else {
                    Vec::new()
                };
                let directory = asciz_at(p, at::PATH, 80);
                let now = parse::Now {
                    drive: p.get8(at::PATH),
                    directory: (!self.lost).then_some(directory.as_slice()),
                    date: &date,
                    time: &time,
                    version: text::VERSION,
                };
                let shown = parse::prompt(&spec, &now);
                *phase = 5;
                Flow::Call(say(p, &shown))
            }
            _ => Flow::Return(Ok(0)),
        }
    }
}

#[cfg(test)]
#[path = "shell_tests.rs"]
mod tests;
