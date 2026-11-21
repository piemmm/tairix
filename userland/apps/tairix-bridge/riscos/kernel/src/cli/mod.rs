//! The command line interpreter (`plans/RISCOS-EMULATOR.md` §5): `OS_CLI`
//! as RISC OS 5's kernel performs it, through `CliV`.
//!
//! A line is stripped of its leading spaces and `*`s, redirected if it holds
//! a `{ > file }` group, offered to the filing system's prefixes and to a
//! module prefix, then run as an alias, a module's command, the current
//! filing system's command, a module's answer to `Service_UKCommand`, or a
//! file the filing system runs.
//!
//! A `-module-` prefix names a module, as the PRM has it; a line of an old
//! command's form found in no list is looked for among the kernel's
//! commands; and nesting is bounded, where an alias that runs itself brings
//! genuine RISC OS 5 down.

mod alias;
pub(crate) mod commands;
pub(crate) mod configure;
pub(crate) mod files;
pub(crate) mod help;
mod redirect;
pub(crate) mod supervisor;
pub(crate) mod table;

use crate::error::{Error, BAD_COMMAND};
use crate::module::ModuleKey;
use crate::os::{len32, Entry, Fail, Os, Regs};
use crate::swi::os::fs_control::{
    CAT as FS_CAT, PREFIX as FS_PREFIX, RESTORE as FS_RESTORE, RUN as FS_RUN,
};
use crate::swi::X;
use crate::vectors::{self, Carry};

/// The longest line: its terminator comes within this many bytes.
pub(crate) const LONGEST: usize = tairix_obey::command::MOST_LINE;
/// How deeply `OS_CLI` nests before the supervisor stack counts as full.
const NEST: u32 = 32;
/// The buffers expansions are written into, and each one's size.
const BUFFERS: u32 = 16;
pub(crate) const BUFFER: u32 = 1024;

/// `OS_Byte` 117: the VDU status; VDU 14 and 15: paged mode on and off.
const VDU_STATUS: u32 = 117;
const PAGED: u8 = 14;
const UNPAGED: u8 = 15;

/// "No room on supervisor stack".
const STACK_FULL: Error = Error {
    number: 0x1E7,
    token: "SupervisorStackFull",
};
/// "Command expands beyond its limits".
const TOO_HARD: Error = crate::error::obey_error(tairix_obey::Error::ExpansionTooComplex);

/// The interpreter's state: the ring of buffers, the buffer an alias's
/// value is expanded into, and how deeply it nests.
pub(crate) struct State {
    /// The ring's first buffer.
    base: u32,
    /// Where an alias's value is read to.
    expansion: u32,
    /// The buffer handed out last.
    current: u32,
    /// The newest buffer's number, and the oldest's still unused since.
    top: u32,
    bottom: u32,
    depth: u32,
}

impl State {
    pub(crate) const fn new() -> Self {
        Self {
            base: 0,
            expansion: 0,
            current: 0,
            top: 0,
            bottom: 0,
            depth: 0,
        }
    }

    /// The ring at `base`, `BUFFERS` buffers of `BUFFER` bytes, and the
    /// expansion buffer at `expansion`, `LONGEST` bytes.
    pub(crate) const fn at(base: u32, expansion: u32) -> Self {
        Self {
            base,
            expansion,
            ..Self::new()
        }
    }

    /// The ring's size.
    pub(crate) const fn size() -> u32 {
        BUFFERS * BUFFER
    }

    /// The next buffer and its number. Once every buffer has been handed out
    /// since the oldest still in use, that one counts as used again.
    fn take(&mut self) -> (u32, u32) {
        self.current = (self.current + 1) % BUFFERS;
        self.top = self.top.wrapping_add(1);
        if self.top.wrapping_sub(self.bottom) >= BUFFERS {
            self.bottom = self.bottom.wrapping_add(1);
        }
        (self.base + self.current * BUFFER, self.top)
    }

    /// Gives back buffer `number`, if it is the newest.
    fn release(&mut self, number: u32) {
        if self.top != self.bottom && number == self.top {
            self.top = self.top.wrapping_sub(1);
            self.current = (self.current + BUFFERS - 1) % BUFFERS;
        }
    }

    /// Whether buffer `number` has not been handed out again.
    const fn intact(&self, number: u32) -> bool {
        number.wrapping_sub(self.bottom).cast_signed() > 0
    }
}

/// How a module prefix is written.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Prefix {
    /// `[MODULE#]name:command`.
    Colon,
    /// `-name-command`.
    Dashed,
}

impl Os<'_> {
    /// `OS_CLI`, through `CliV` with interrupts enabled: C and V come back
    /// from it.
    pub(crate) fn os_cli(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        self.vectored(vectors::CLI, regs, Entry::SupervisorIrq, Carry::Vector)
    }

    /// `CliV`'s default: the line at R0 interpreted.
    pub(crate) fn default_cli(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        if self.world.cli.depth >= NEST {
            return Err(self.error(STACK_FULL));
        }
        self.world.cli.depth += 1;
        let result = self.interpret(regs.r[0]);
        self.world.cli.depth -= 1;
        result
    }

    /// Performs the line at `address` as a command.
    pub(crate) fn command_line(&mut self, address: u32) -> Result<(), Fail> {
        let mut regs = Regs::default();
        regs.r[0] = address;
        self.perform(X | crate::swi::os::CLI, &mut regs)
    }

    /// Performs `line` as a command, as `OS_CLI` performs one.
    ///
    /// # Errors
    ///
    /// The command's error.
    pub fn perform_line(&mut self, line: &[u8]) -> Result<(), Fail> {
        self.with_text(line, b'\r', Os::command_line)
    }

    fn interpret(&mut self, address: u32) -> Result<(), Fail> {
        let mut start = address;
        let mut skipped = 0;
        while matches!(self.read_byte(start)?, b' ' | b'*') {
            start = start.wrapping_add(1);
            skipped += 1;
            self.yield_point()?;
        }
        let room = (LONGEST - 1).saturating_sub(skipped);
        let (line, ended) = self.read_until(start, room, is_end)?;
        if !ended {
            return Err(self.error(crate::error::LINE_TOO_LONG));
        }
        let tested = if line.first() == Some(&b'%') {
            line.get(1)
        } else {
            line.first()
        };
        if matches!(tested, None | Some(&b'|')) {
            return Ok(());
        }
        let Some(group) = redirect::find(&line) else {
            return self.interpret_from(start);
        };
        let (buffer, number) = self.world.cli.take();
        let rewritten = redirect::rewrite(&line, &group);
        if rewritten.len() >= BUFFER as usize {
            return Err(self.error(crate::error::LINE_TOO_LONG));
        }
        let terminator = self.read_byte(start.wrapping_add(len32(line.len())))?;
        self.write_terminated(buffer, &rewritten, terminator, BUFFER as usize)?;
        let result = match self.redirect(&line, &group) {
            Ok(()) => self.interpret_from(buffer),
            Err(fail) => Err(fail),
        };
        self.world.cli.release(number);
        if !matches!(result, Err(Fail::Unwind(_))) {
            self.end_redirection();
        }
        result
    }

    /// The line at `start` interpreted from its prefixes on.
    fn interpret_from(&mut self, start: u32) -> Result<(), Fail> {
        let mut prefix = Regs::default();
        prefix.r[..4].copy_from_slice(&[FS_PREFIX, start, u32::MAX, 0]);
        let at = match self.perform(X | crate::swi::os::FS_CONTROL, &mut prefix) {
            Err(Fail::Error(error)) => {
                // FileSwitch found no filing system of a `-name-`: a module,
                // perhaps.
                return match self.module_prefix(start, Prefix::Dashed)? {
                    Some((key, rest)) => self.run_in_module(key, rest),
                    None => Err(Fail::Error(error)),
                };
            }
            Err(unwind) => return Err(unwind),
            Ok(()) if prefix.r[3] != 0 => return self.fs_reason(FS_RUN, start),
            Ok(()) if prefix.r[2] == u32::MAX => {
                if let Some((key, rest)) = self.module_prefix(start, Prefix::Colon)? {
                    return self.run_in_module(key, rest);
                }
                start
            }
            Ok(()) => prefix.r[1],
        };
        let result = self.dispatch(at);
        if prefix.r[2] != u32::MAX {
            let mut restore = Regs::default();
            restore.r[0] = FS_RESTORE;
            if let Err(Fail::Unwind(unwind)) =
                self.perform(X | crate::swi::os::FS_CONTROL, &mut restore)
            {
                return Err(Fail::Unwind(unwind));
            }
        }
        result
    }

    /// `OS_FSControl` `reason` with R1 the text at `at`.
    fn fs_reason(&mut self, reason: u32, at: u32) -> Result<(), Fail> {
        let mut regs = Regs::default();
        regs.r[..2].copy_from_slice(&[reason, at]);
        self.perform(X | crate::swi::os::FS_CONTROL, &mut regs)
    }

    /// The command at `at`, after any prefix: the special characters, then
    /// an alias, then a command.
    fn dispatch(&mut self, at: u32) -> Result<(), Fail> {
        let (first, at) = self.skip_spaces(at)?;
        match first {
            b'/' => self.fs_reason(FS_RUN, at.wrapping_add(1)),
            b'%' => {
                let after = at.wrapping_add(1);
                if self.read_byte(after)? == b'.' {
                    self.fs_reason(FS_CAT, after.wrapping_add(1))
                } else {
                    self.command(after)
                }
            }
            b'.' => match self.alias_of_dot(at) {
                Some(result) => result,
                None => self.fs_reason(FS_CAT, at.wrapping_add(1)),
            },
            _ => match self.alias(at)? {
                Some(result) => result,
                None => self.command(at),
            },
        }
    }

    /// The command at `at`: a module's, then the filing system's, then one
    /// a module claims, then a file the filing system runs.
    fn command(&mut self, at: u32) -> Result<(), Fail> {
        if let Some(result) = self.module_command(at, None)? {
            return result;
        }
        if let Some(result) = self.filing_command(at)? {
            return result;
        }
        let mut unknown = Regs::default();
        unknown.r[..2].copy_from_slice(&[at, crate::service::UK_COMMAND]);
        self.service(&mut unknown)?;
        if unknown.r[1] == 0 {
            return match unknown.r[0] {
                0 => Ok(()),
                block => Err(Fail::Error(block)),
            };
        }
        self.fs_reason(FS_RUN, at)
    }

    /// Runs the command at `at` in `key`'s table alone, as a module prefix
    /// asks.
    fn run_in_module(&mut self, key: ModuleKey, at: u32) -> Result<(), Fail> {
        match self.module_command(at, Some(key))? {
            Some(result) => result,
            None => Err(self.error(BAD_COMMAND)),
        }
    }

    /// The module a prefix at `at` names, made its preferred incarnation
    /// for good, and where its command starts; none if there is no prefix
    /// or no such module.
    fn module_prefix(&mut self, at: u32, form: Prefix) -> Result<Option<(ModuleKey, u32)>, Fail> {
        let (line, _) = self.read_until(at, LONGEST, is_end)?;
        let (name, rest) = match form {
            Prefix::Dashed => {
                let Some(body) = line.strip_prefix(b"-") else {
                    return Ok(None);
                };
                let Some(end) = body.iter().position(|&b| b == b'-') else {
                    return Ok(None);
                };
                (&body[..end], 1 + end + 1)
            }
            Prefix::Colon => {
                let skip = if line.len() >= 7 && line[..7].eq_ignore_ascii_case(b"MODULE#") {
                    7
                } else {
                    0
                };
                let body = &line[skip..];
                let Some(end) = body
                    .iter()
                    .position(|&b| b == b':' || b == b'.' || b <= b' ')
                else {
                    return Ok(None);
                };
                if body[end] != b':' {
                    return Ok(None);
                }
                (&body[..end], skip + end + 1)
            }
        };
        let Ok((key, index)) = self.look_up(name)? else {
            return Ok(None);
        };
        self.prefer_incarnation(key, index);
        Ok(Some((key, at.wrapping_add(len32(rest)))))
    }

    /// Hands out one of the ring's buffers, its number to check it by.
    pub(crate) fn cli_buffer(&mut self) -> (u32, u32) {
        self.world.cli.take()
    }

    /// A step of a command that runs for as long as its work does: an
    /// interrupt due taken, as RISC OS takes one in a command, then Escape,
    /// acknowledged, if it has been pressed.
    pub(crate) fn escape_point(&mut self) -> Result<(), Fail> {
        self.take_interrupt()?;
        if self.escape_set() {
            return Err(self.escape_acknowledged());
        }
        self.yield_point()?;
        Ok(())
    }

    /// Paged mode turned on; the VDU status it was turned on over, nought
    /// where no VDU drivers answer for it, so a command never fails for want
    /// of them.
    pub(crate) fn paged(&mut self) -> Result<u32, Fail> {
        let mut status = Regs::default();
        status.r[0] = VDU_STATUS;
        let known = self.perform_quietly(X | crate::swi::os::BYTE, &mut status)?;
        self.write_c(PAGED)?;
        Ok(if known { status.r[1] } else { 0 })
    }

    /// Paged mode turned off again, unless `status`, the VDU status from
    /// before, had it on, or the printer.
    pub(crate) fn unpaged(&mut self, status: u32) -> Result<(), Fail> {
        if status & 0b101 == 0 {
            self.write_c(UNPAGED)?;
        }
        Ok(())
    }

    /// Each character `OS_GSRead` reads from the string at `at`, with
    /// `OS_GSInit`'s `flags`, passed to `each`.
    pub(crate) fn gs_each(
        &mut self,
        at: u32,
        flags: u32,
        mut each: impl FnMut(&mut Self, u8) -> Result<(), Fail>,
    ) -> Result<(), Fail> {
        let mut state = Regs::default();
        state.r[..3].copy_from_slice(&[at, 0, flags]);
        self.perform(X | crate::swi::os::GS_INIT, &mut state)?;
        loop {
            let mut read = Regs::default();
            read.r[0] = state.r[0];
            read.r[2] = state.r[2];
            self.perform(X | crate::swi::os::GS_READ, &mut read)?;
            if read.flag(tairix_riscos_arm::psr::C) {
                return Ok(());
            }
            each(self, read.byte(1))?;
            (state.r[0], state.r[2]) = (read.r[0], read.r[2]);
            self.yield_point()?;
        }
    }

    /// The tail at `tail` read by `OS_ReadArgs` against the template `keys`
    /// into at most `most` bytes of the ring's `buffer`, the template at its
    /// end.
    pub(crate) fn read_args_into(
        &mut self,
        keys: &[u8],
        tail: u32,
        buffer: u32,
        most: u32,
    ) -> Result<(), Fail> {
        let room = most.min(BUFFER - len32(keys.len()));
        let template = buffer.wrapping_add(room);
        self.write_bytes(template, keys)?;
        let mut read = Regs::default();
        read.r[..4].copy_from_slice(&[template, tail, buffer, room]);
        self.perform(X | crate::swi::os::READ_ARGS, &mut read)
    }

    /// Gives back buffer `number` once the work done in it ended with
    /// `result`: its error, else "Command expands beyond its limits" if the buffer was
    /// handed out again while in use. An error is made only to be returned,
    /// so none is made that could take the place of a live one in the ring.
    pub(crate) fn cli_buffer_done(
        &mut self,
        number: u32,
        result: Result<(), Fail>,
    ) -> Result<(), Fail> {
        let intact = self.world.cli.intact(number);
        self.world.cli.release(number);
        match result {
            Ok(()) if !intact => Err(self.error(TOO_HARD)),
            other => other,
        }
    }
}

/// Whether `byte` ends a command line.
pub(crate) const fn is_end(byte: u8) -> bool {
    matches!(byte, 0 | b'\n' | b'\r')
}

/// `byte` as the interpreter reads a command's name: a letter in upper case;
/// none for a character that ends the name.
pub(crate) const fn name_byte(byte: u8) -> Option<u8> {
    match byte {
        0..=b' '
        | b'"'
        | b'$'
        | b'%'
        | b'&'
        | b','
        | b':'
        | b'<'
        | b'>'
        | b'\\'
        | b'^'
        | b'|'
        | 0x7F => None,
        b'a'..=b'z' => Some(byte - 0x20),
        _ => Some(byte),
    }
}

#[cfg(test)]
pub(crate) mod tests;
