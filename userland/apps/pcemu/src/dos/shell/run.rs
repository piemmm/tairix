//! Running a line: its pipeline's stages one after another through
//! temporary files, each stage's redirections set up and undone, and the
//! command each names — a drive, an internal command, or a program or batch
//! file found in the current directory or along `PATH`.

use super::batch::{self, BatchStart, Echo, ForLoop, Goto, IfCommand, Pause, Shift};
use super::copy::Copy;
use super::dir::Dir;
use super::internal;
use super::parse::{self, Internal};
use super::{
    answered, asciz_at, at, flag, flags, say, set_asciz, set_flag, set_text, text, text_at,
    variable, Flow, Frame, Id, Int, Program, Prompt, Returned, Routine,
};
use crate::dos::dirops::finding;
use crate::frames::{locals, take, Fault, Outcome};
use alloc::vec::Vec;
use tairix_fat::dir::attr;

locals! {
    /// Runs the line in [`at::WORK`], each stage of its pipeline in turn; a
    /// batch file's line is echoed first while echoing is on and it does not
    /// begin with `@`, and a label is passed over.
    pub struct Execute {
        batch: bool,
        stage: u8,
    }
}

mod phase {
    pub const ECHO_PROMPTED: u8 = 1;
    pub const ECHO_LINE: u8 = 9;
    pub const SPLIT: u8 = 2;
    pub const NEXT: u8 = 3;
    pub const TEMPORARY: u8 = 4;
    pub const RUN: u8 = 5;
    pub const RAN: u8 = 6;
    pub const DELETED: u8 = 7;
    pub const DONE: u8 = 8;
}

/// Where pipe file `n` lies, of the two a pipeline alternates between.
const fn pipe(n: u8) -> u16 {
    at::PIPES + (n as u16 & 1) * at::PIPE_LEN
}

impl Routine for Execute {
    const ID: Id = Id::Execute;

    #[allow(
        clippy::too_many_lines,
        reason = "one arm a phase, in the order the routine runs"
    )]
    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        if child == Err(Fault::Break) {
            return Flow::Return(child);
        }
        match *phase {
            0 => {
                let line = text_at(p, at::WORK);
                let trimmed = parse::trim(&line);
                let after_null = flags(p) & flag::NULL_LINE != 0;
                let label = self.batch && trimmed.first() == Some(&b':');
                if !label {
                    set_flag(p, flag::NULL_LINE, trimmed.is_empty());
                }
                if !self.batch {
                    *phase = phase::SPLIT;
                    return self.step(p, phase, Ok(0));
                }
                if trimmed.first() == Some(&b':') {
                    return Flow::Return(Ok(0));
                }
                let silent = trimmed.first() == Some(&b'@');
                if silent {
                    let rest = trimmed[1..].to_vec();
                    set_text(p, at::WORK, &rest);
                }
                *phase = phase::SPLIT;
                if silent
                    || flags(p) & flag::ECHO == 0
                    || parse::trim(&text_at(p, at::WORK)).is_empty()
                {
                    return self.step(p, phase, Ok(0));
                }
                // COMMAND.COM ends the last command's output before echoing
                // the next, unless the last had none.
                *phase = phase::ECHO_LINE;
                if after_null {
                    return self.step(p, phase, Ok(0));
                }
                Flow::Call(say(p, text::NEW_LINE))
            }
            phase::ECHO_LINE => {
                *phase = phase::ECHO_PROMPTED;
                Flow::Call(Frame::of(&Prompt::default()))
            }
            phase::ECHO_PROMPTED => {
                let mut line = text_at(p, at::WORK);
                line.extend_from_slice(text::NEW_LINE);
                *phase = phase::SPLIT;
                Flow::Call(say(p, &line))
            }
            phase::SPLIT => {
                let line = text_at(p, at::WORK);
                let Ok(stages) = parse::stages(&line) else {
                    *phase = phase::DONE;
                    return Flow::Call(say(p, text::SYNTAX));
                };
                p.set8(
                    at::PIPE_COUNT,
                    u8::try_from(stages.len()).unwrap_or(u8::MAX),
                );
                self.stage = 0;
                *phase = phase::NEXT;
                self.step(p, phase, Ok(0))
            }
            phase::NEXT => {
                let count = p.get8(at::PIPE_COUNT);
                if self.stage >= count {
                    return Flow::Return(Ok(0));
                }
                *phase = phase::RUN;
                if self.stage + 1 == count {
                    return self.step(p, phase, Ok(0));
                }
                let mut directory = variable(p, b"TEMP").unwrap_or_else(|| b"\\".to_vec());
                if !directory.ends_with(b"\\") && !directory.ends_with(b":") {
                    directory.push(b'\\');
                }
                set_asciz(p, pipe(self.stage), &directory, at::PIPE_LEN - 13);
                *phase = phase::TEMPORARY;
                Flow::Call(Frame::of(&Int {
                    dx: pipe(self.stage),
                    ..Int::dos(p.own, 0x5A00)
                }))
            }
            phase::TEMPORARY => {
                let Ok(handle) = child else {
                    *phase = phase::DONE;
                    return Flow::Call(say(p, text::CREATION));
                };
                *phase = phase::RUN;
                Flow::Call(Frame::of(&Int {
                    bx: u16::try_from(handle).unwrap_or(0),
                    ..Int::dos(p.own, 0x3E00)
                }))
            }
            phase::RUN => {
                let line = text_at(p, at::WORK);
                let count = p.get8(at::PIPE_COUNT);
                let Some(stage) = parse::stages(&line)
                    .ok()
                    .and_then(|stages| stages.get(usize::from(self.stage)).map(|s| s.to_vec()))
                else {
                    return Flow::Return(Ok(0));
                };
                let mut text = stage;
                if self.stage > 0 {
                    text.extend_from_slice(b" <");
                    text.extend_from_slice(&asciz_at(p, pipe(self.stage - 1), at::PIPE_LEN));
                }
                if self.stage + 1 < count {
                    text.extend_from_slice(b" >");
                    text.extend_from_slice(&asciz_at(p, pipe(self.stage), at::PIPE_LEN));
                }
                set_text(p, at::STAGE, &text);
                *phase = phase::RAN;
                Flow::Call(Frame::of(&Stage::default()))
            }
            phase::RAN => {
                *phase = phase::DELETED;
                if self.stage == 0 {
                    return self.step(p, phase, Ok(0));
                }
                Flow::Call(Frame::of(&Int {
                    dx: pipe(self.stage - 1),
                    ..Int::dos(p.own, 0x4100)
                }))
            }
            phase::DELETED => {
                self.stage = self.stage.saturating_add(1);
                *phase = phase::NEXT;
                self.step(p, phase, Ok(0))
            }
            _ => Flow::Return(Ok(0)),
        }
    }
}

locals! {
    /// Runs the stage in [`at::STAGE`]: its standard input read from the
    /// file `<` names, its standard output written to the one `>` or `>>`
    /// names, both given back once its command is done.
    pub struct Stage {
        file: u16,
        outcome: u8,
    }
}

mod stage {
    pub const IN_OPENED: u8 = 1;
    pub const IN_SAVED: u8 = 2;
    pub const IN_FORCED: u8 = 3;
    pub const OUTPUT: u8 = 4;
    pub const APPEND_OPENED: u8 = 5;
    pub const OUT_OPENED: u8 = 6;
    pub const OUT_SAVED: u8 = 7;
    pub const OUT_FORCED: u8 = 8;
    pub const RUN: u8 = 9;
    pub const RAN: u8 = 10;
    pub const RESTORE: u8 = 11;
    pub const DONE: u8 = 12;
}

impl Routine for Stage {
    const ID: Id = Id::Stage;

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
        match *phase {
            0 => {
                let line = text_at(p, at::STAGE);
                let Ok(parsed) = parse::redirections(&line) else {
                    *phase = stage::DONE;
                    return Flow::Call(say(p, text::SYNTAX));
                };
                set_text(p, at::COMMAND, &parsed.command);
                set_asciz(
                    p,
                    at::NAME_IN,
                    parsed.input.as_deref().unwrap_or_default(),
                    80,
                );
                let (output, append) = parsed.output.unwrap_or_default();
                set_asciz(p, at::NAME_OUT, &output, 80);
                p.set8(at::APPEND, u8::from(append));
                if p.get8(at::NAME_IN) == 0 {
                    *phase = stage::OUTPUT;
                    return self.step(p, phase, Ok(0));
                }
                *phase = stage::IN_OPENED;
                Flow::Call(call(p, 0x3D00, 0, 0, at::NAME_IN))
            }
            stage::IN_OPENED => {
                let Ok(handle) = child else {
                    *phase = stage::DONE;
                    return Flow::Call(say(p, text::FILE_NOT_FOUND));
                };
                self.file = u16::try_from(handle).unwrap_or(0);
                *phase = stage::IN_SAVED;
                Flow::Call(call(p, 0x4500, 0, 0, 0))
            }
            stage::IN_SAVED => {
                let saved = u16::try_from(take!(child)).unwrap_or(0xFFFF);
                p.set16(at::SAVED_IN, saved);
                *phase = stage::IN_FORCED;
                Flow::Call(call(p, 0x4600, self.file, 0, 0))
            }
            stage::IN_FORCED => {
                *phase = stage::OUTPUT;
                Flow::Call(call(p, 0x3E00, self.file, 0, 0))
            }
            stage::OUTPUT => {
                if p.get8(at::NAME_OUT) == 0 {
                    *phase = stage::RUN;
                    return self.step(p, phase, Ok(0));
                }
                if p.get8(at::APPEND) != 0 {
                    *phase = stage::APPEND_OPENED;
                    return Flow::Call(call(p, 0x3D01, 0, 0, at::NAME_OUT));
                }
                *phase = stage::OUT_OPENED;
                Flow::Call(call(p, 0x3C00, 0, 0, at::NAME_OUT))
            }
            stage::APPEND_OPENED => {
                *phase = stage::OUT_OPENED;
                if let Ok(handle) = child {
                    self.file = u16::try_from(handle).unwrap_or(0);
                    return Flow::Call(call(p, 0x4202, self.file, 0, 0));
                }
                p.set8(at::APPEND, 0);
                Flow::Call(call(p, 0x3C00, 0, 0, at::NAME_OUT))
            }
            stage::OUT_OPENED => {
                let Ok(handle) = child else {
                    *phase = stage::RESTORE;
                    return Flow::Call(say(p, text::CREATION));
                };
                if p.get8(at::APPEND) == 0 {
                    self.file = u16::try_from(handle).unwrap_or(0);
                }
                *phase = stage::OUT_SAVED;
                Flow::Call(call(p, 0x4500, 1, 0, 0))
            }
            stage::OUT_SAVED => {
                let saved = u16::try_from(take!(child)).unwrap_or(0xFFFF);
                p.set16(at::SAVED_OUT, saved);
                *phase = stage::OUT_FORCED;
                Flow::Call(call(p, 0x4600, self.file, 1, 0))
            }
            stage::OUT_FORCED => {
                *phase = stage::RUN;
                Flow::Call(call(p, 0x3E00, self.file, 0, 0))
            }
            stage::RUN => {
                *phase = stage::RAN;
                Flow::Call(Frame::of(&Command {}))
            }
            stage::RAN => {
                if child == Err(Fault::Break) {
                    return Flow::Return(child);
                }
                *phase = stage::RESTORE;
                self.step(p, phase, Ok(0))
            }
            stage::RESTORE => {
                *phase = stage::DONE;
                Flow::Call(Frame::of(&Restore {}))
            }
            _ => Flow::Return(Ok(0)),
        }
    }
}

locals! {
    /// Gives standard input and output back their handles from before a
    /// stage redirected them.
    pub struct Restore {}
}

locals! {
    /// Frees the blocks the command that ran holds, a call each.
    pub struct Release {}
}

impl Routine for Release {
    const ID: Id = Id::Release;

    fn step(&mut self, p: &mut Program<'_, '_>, _phase: &mut u8, _child: Outcome) -> Flow {
        let held = (0..at::HELD_COUNT)
            .map(|n| at::HELD + n * 2)
            .find(|&slot| p.get16(slot) != 0);
        let Some(slot) = held else {
            return Flow::Return(Ok(0));
        };
        let segment = p.get16(slot);
        p.set16(slot, 0);
        Flow::Call(Frame::of(&Int {
            es: segment,
            ..Int::dos(p.own, 0x4900)
        }))
    }
}

impl Routine for Restore {
    const ID: Id = Id::Restore;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, _child: Outcome) -> Flow {
        let call = |p: &mut Program<'_, '_>, ax: u16, bx: u16, cx: u16| {
            Frame::of(&Int {
                bx,
                cx,
                ..Int::dos(p.own, ax)
            })
        };
        loop {
            let (field, handle) = if *phase < 2 {
                (at::SAVED_IN, 0)
            } else {
                (at::SAVED_OUT, 1)
            };
            let saved = p.get16(field);
            match *phase {
                0 | 2 if saved != 0xFFFF => {
                    *phase += 1;
                    return Flow::Call(call(p, 0x4600, saved, handle));
                }
                1 | 3 => {
                    p.set16(field, 0xFFFF);
                    *phase += 1;
                    return Flow::Call(call(p, 0x3E00, saved, 0));
                }
                0 | 2 => *phase += 2,
                _ => return Flow::Return(Ok(0)),
            }
        }
    }
}

locals! {
    /// Runs the command in [`at::COMMAND`]: a drive made current, an
    /// internal command, or a program.
    pub struct Command {}
}

impl Routine for Command {
    const ID: Id = Id::Command;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        match *phase {
            0 => {
                let line = text_at(p, at::COMMAND);
                let (word, tail) = parse::command_word(&line);
                if word.is_empty() {
                    return Flow::Return(Ok(0));
                }
                if let [letter, b':'] = word {
                    if letter.is_ascii_alphabetic() {
                        let drive = letter.to_ascii_uppercase() - b'A';
                        p.set8(at::PATH, drive);
                        *phase = 1;
                        return Flow::Call(Frame::of(&Int {
                            dx: u16::from(drive),
                            ..Int::dos(p.own, 0x0E00)
                        }));
                    }
                }
                if let Some((command, end)) = parse::internal(word) {
                    let mut args = word[end..].to_vec();
                    args.extend_from_slice(tail);
                    set_text(p, at::ARGS, &args);
                    *phase = 3;
                    return Flow::Call(internal_frame(command));
                }
                let word = word.to_vec();
                set_text(p, at::WORD, &word);
                set_text(p, at::ARGS, tail);
                *phase = 3;
                Flow::Call(Frame::of(&External::default()))
            }
            1 => {
                *phase = 2;
                Flow::Call(Frame::of(&Int::dos(p.own, 0x1900)))
            }
            2 => {
                let current = take!(child).to_le_bytes()[0];
                *phase = 3;
                if current == p.get8(at::PATH) {
                    return Flow::Return(Ok(0));
                }
                Flow::Call(say(p, text::BAD_DRIVE))
            }
            _ => Flow::Return(if child == Err(Fault::Break) {
                child
            } else {
                Ok(0)
            }),
        }
    }
}

/// The routine an internal command is.
fn internal_frame(command: Internal) -> Frame {
    use internal::{
        Break, Cd, Chcp, Cls, Ctty, DateCommand, Delete, Exit, MakeDir, PathCommand, PromptCommand,
        RemoveDir, Rename, SetCommand, TimeCommand, Truename, TypeFile, Verify, Version, Volume,
    };
    match command {
        Internal::Break => Frame::of(&Break::default()),
        Internal::Call => Frame::of(&batch::Call::default()),
        Internal::Cd => Frame::of(&Cd::default()),
        Internal::Chcp => Frame::of(&Chcp::default()),
        Internal::Cls => Frame::of(&Cls::default()),
        Internal::Copy => Frame::of(&Copy::default()),
        Internal::Ctty => Frame::of(&Ctty::default()),
        Internal::Date => Frame::of(&DateCommand::default()),
        Internal::Del => Frame::of(&Delete::default()),
        Internal::Dir => Frame::of(&Dir::default()),
        Internal::Echo => Frame::of(&Echo::default()),
        Internal::Exit => Frame::of(&Exit::default()),
        Internal::For => Frame::of(&ForLoop::default()),
        Internal::Goto => Frame::of(&Goto::default()),
        Internal::If => Frame::of(&IfCommand::default()),
        Internal::LoadHigh => Frame::of(&LoadHigh::default()),
        Internal::Md => Frame::of(&MakeDir::default()),
        Internal::Path => Frame::of(&PathCommand::default()),
        Internal::Pause => Frame::of(&Pause::default()),
        Internal::Prompt => Frame::of(&PromptCommand::default()),
        Internal::Rd => Frame::of(&RemoveDir::default()),
        Internal::Rem => Frame::of(&Remark {}),
        Internal::Ren => Frame::of(&Rename::default()),
        Internal::Set => Frame::of(&SetCommand::default()),
        Internal::Shift => Frame::of(&Shift::default()),
        Internal::Time => Frame::of(&TimeCommand::default()),
        Internal::Truename => Frame::of(&Truename::default()),
        Internal::Type => Frame::of(&TypeFile::default()),
        Internal::Ver => Frame::of(&Version::default()),
        Internal::Verify => Frame::of(&Verify::default()),
        Internal::Vol => Frame::of(&Volume::default()),
    }
}

locals! {
    /// `REM`: nothing.
    pub struct Remark {}
}

impl Routine for Remark {
    const ID: Id = Id::Remark;

    fn step(&mut self, _p: &mut Program<'_, '_>, _phase: &mut u8, _child: Outcome) -> Flow {
        Flow::Return(Ok(0))
    }
}

/// The extensions a program's name may have, in the order they are tried.
const EXTENSIONS: [&[u8]; 3] = [b"COM", b"EXE", b"BAT"];

/// What a program's search matches, as `COMMAND.COM` searches: a hidden or
/// read-only file runs, and a directory found by the name is passed over.
const PROGRAMS: u16 = (attr::DIRECTORY | attr::HIDDEN | attr::READ_ONLY) as u16;

locals! {
    /// Runs the program [`at::WORD`] names with the tail in [`at::ARGS`]:
    /// found by its name, or with `.COM`, `.EXE` and `.BAT` in turn, in its
    /// own directory if it names one, else the current directory and then
    /// each of `PATH`'s; a batch file starts, nested when `call`.
    pub struct External {
        call: bool,
        directory: u8,
        extension: u8,
    }
}

mod external {
    pub const DTA_SET: u8 = 1;
    pub const TRIED: u8 = 2;
    pub const FIRST_FCB: u8 = 3;
    pub const SECOND_FCB: u8 = 4;
    pub const RAN: u8 = 5;
    pub const CODE: u8 = 6;
    pub const DONE: u8 = 7;
}

impl External {
    /// The path of the candidate the search has reached, if any is left.
    fn candidate(self, p: &mut Program<'_, '_>) -> Option<(Vec<u8>, usize)> {
        let word = text_at(p, at::WORD).to_ascii_uppercase();
        let split = word
            .iter()
            .rposition(|&b| b == b'\\' || b == b':')
            .map_or(0, |at| at + 1);
        let (directory, name) = word.split_at(split);
        let named: Option<usize> = match name.iter().position(|&b| b == b'.') {
            Some(dot) => Some(EXTENSIONS.iter().position(|e| *e == &name[dot + 1..])?),
            None => None,
        };
        let stem = name.split(|&b| b == b'.').next().unwrap_or(name);
        let extension = match named {
            Some(n) if self.extension == 0 => n,
            Some(_) => return None,
            None => usize::from(self.extension),
        };
        let ext = EXTENSIONS.get(extension)?;
        let base: Vec<u8> = if self.directory == 0 {
            directory.to_vec()
        } else if !directory.is_empty() {
            return None;
        } else {
            let path = variable(p, b"PATH").unwrap_or_default();
            let entry = path
                .split(|&b| b == b';')
                .filter(|e| !e.is_empty())
                .nth(usize::from(self.directory) - 1)?;
            let mut entry = entry.to_vec();
            if !entry.ends_with(b"\\") && !entry.ends_with(b":") {
                entry.push(b'\\');
            }
            entry
        };
        let mut path = base;
        path.extend_from_slice(stem);
        path.push(b'.');
        path.extend_from_slice(ext);
        Some((path, extension))
    }

    /// Moves the search to the next candidate: the directory's next name,
    /// else the next directory's first, as when `leaving` the directory,
    /// whose drive has failed.
    fn advance(&mut self, p: &mut Program<'_, '_>, leaving: bool) {
        let word = text_at(p, at::WORD);
        let named = word
            .iter()
            .rposition(|&b| b == b'\\' || b == b':')
            .map_or(&word[..], |at| &word[at + 1..])
            .contains(&b'.');
        if leaving || named || usize::from(self.extension) + 1 >= EXTENSIONS.len() {
            self.extension = 0;
            self.directory = self.directory.saturating_add(1);
        } else {
            self.extension += 1;
        }
    }
}

impl Routine for External {
    const ID: Id = Id::External;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        match *phase {
            0 => {
                *phase = external::DTA_SET;
                Flow::Call(Frame::of(&Int {
                    dx: at::DTA,
                    ..Int::dos(p.own, 0x1A00)
                }))
            }
            external::DTA_SET => {
                let Some((path, _)) = self.candidate(p) else {
                    *phase = external::DONE;
                    return Flow::Call(say(p, text::BAD_COMMAND));
                };
                set_asciz(p, at::PATH, &path, 128);
                *phase = external::TRIED;
                Flow::Call(Frame::of(&Int {
                    cx: PROGRAMS,
                    dx: at::PATH,
                    ..Int::dos(p.own, 0x4E00)
                }))
            }
            external::TRIED => {
                let directory =
                    child.is_ok() && p.get8(at::DTA + finding::ATTRIBUTE) & attr::DIRECTORY != 0;
                if child.is_err() || directory {
                    self.advance(p, answered(child));
                    *phase = external::DTA_SET;
                    return self.step(p, phase, Ok(0));
                }
                let batch = self
                    .candidate(p)
                    .is_some_and(|(_, extension)| extension == 2);
                if batch {
                    *phase = external::DONE;
                    return Flow::Call(Frame::of(&BatchStart { call: self.call }));
                }
                let tail = text_at(p, at::ARGS);
                let len = tail.len().min(126);
                p.set8(at::TAIL, u8::try_from(len).unwrap_or(0));
                p.g.write_bytes(p.own, at::TAIL + 1, &tail[..len]);
                p.set8(at::TAIL + 1 + u16::try_from(len).unwrap_or(0), b'\r');
                *phase = external::FIRST_FCB;
                Flow::Call(Frame::of(&Int {
                    si: at::TAIL + 1,
                    di: at::FCB1,
                    ..Int::dos(p.own, 0x2901)
                }))
            }
            external::FIRST_FCB => {
                let si = p.result(Returned::Si);
                *phase = external::SECOND_FCB;
                Flow::Call(Frame::of(&Int {
                    si,
                    di: at::FCB2,
                    ..Int::dos(p.own, 0x2901)
                }))
            }
            external::SECOND_FCB => {
                let environment = p.get16(at::ENVIRONMENT);
                let own = p.own;
                p.set16(at::EXEC, environment);
                for (n, offset) in [(1u16, at::TAIL), (2, at::FCB1), (3, at::FCB2)] {
                    p.set16(at::EXEC + n * 4 - 2, offset);
                    p.set16(at::EXEC + n * 4, own);
                }
                *phase = external::RAN;
                Flow::Call(Frame::of(&Int {
                    bx: at::EXEC,
                    dx: at::PATH,
                    ..Int::dos(p.own, 0x4B00)
                }))
            }
            external::RAN => match child {
                Ok(_) => {
                    *phase = external::CODE;
                    Flow::Call(Frame::of(&Int::dos(p.own, 0x4D00)))
                }
                _ if answered(child) => Flow::Return(Ok(0)),
                Err(Fault::Error(code)) => {
                    *phase = external::DONE;
                    let message = if code == crate::dos::fs::error::NO_MEMORY {
                        text::TOO_BIG
                    } else {
                        text::BAD_COMMAND
                    };
                    Flow::Call(say(p, message))
                }
                Err(fault) => Flow::Return(Err(fault)),
            },
            external::CODE => {
                let code = take!(child).to_le_bytes()[0];
                p.set16(at::ERRORLEVEL, u16::from(code));
                Flow::Return(Ok(0))
            }
            _ => Flow::Return(if child == Err(Fault::Break) {
                child
            } else {
                Ok(0)
            }),
        }
    }
}

locals! {
    /// `LOADHIGH` and `LH`: the program run as any is, as a machine with no
    /// upper memory loads it.
    pub struct LoadHigh {}
}

impl Routine for LoadHigh {
    const ID: Id = Id::LoadHigh;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        if *phase != 0 {
            return Flow::Return(child);
        }
        let args = text_at(p, at::ARGS);
        let (word, tail) = parse::command_word(&args);
        let (word, tail) = (word.to_vec(), tail.to_vec());
        set_text(p, at::WORD, &word);
        set_text(p, at::ARGS, &tail);
        *phase = 1;
        if word.is_empty() {
            return Flow::Call(say(p, text::MISSING));
        }
        Flow::Call(Frame::of(&External::default()))
    }
}
