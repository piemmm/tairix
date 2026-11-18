//! The Supervisor: the `UtilityModule`'s start entry, which `*GOS` enters as
//! the application. It reads a line at a prompt and has `OS_CLI` run it, an
//! error printed and the prompt given again, until a command starts another
//! application in its place.

use tairix_riscos_arm::psr;

use crate::input::ACKNOWLEDGE_ESCAPE;
use crate::os::{len32, Fail, Os, Regs};
use crate::swi::{os, X};
use crate::vars::EXPANDED;

/// `OS_Module` 2: enter a module as the application.
const ENTER: u32 = 2;
/// The prompt's variable, and the prompt without it.
const PROMPT: &[u8] = b"CLI$Prompt\0";
const DEFAULT_PROMPT: &[u8] = b"*";
/// The longest line read: the buffer's, less its terminator.
const LINE_MOST: u32 = super::BUFFER - 1;

impl Os<'_> {
    /// One prompt and the line typed at it, run, using `buffer`.
    fn supervisor_line(&mut self, buffer: u32) -> Result<(), Fail> {
        let room = super::BUFFER - len32(PROMPT.len());
        let name = buffer.wrapping_add(room);
        self.write_bytes(name, PROMPT)?;
        let mut prompt = Regs::default();
        prompt.r[..5].copy_from_slice(&[name, buffer, room, 0, u32::from(EXPANDED)]);
        let text = match self.perform(X | os::READ_VAR_VAL, &mut prompt) {
            Ok(()) => self.read_until(buffer, prompt.r[2] as usize, |_| false)?.0,
            Err(Fail::Error(_)) => DEFAULT_PROMPT.to_vec(),
            Err(unwind) => return Err(unwind),
        };
        self.write_text(&text)?;
        let mut read = Regs::default();
        read.r[..4].copy_from_slice(&[buffer, LINE_MOST, u32::from(b' '), 0xFF]);
        self.perform(X | os::READ_LINE32, &mut read)?;
        if read.flag(psr::C) {
            let mut acknowledge = Regs::default();
            acknowledge.r[0] = ACKNOWLEDGE_ESCAPE;
            self.perform(X | os::BYTE, &mut acknowledge)?;
            let escape = self.message_with(b"Escape", &[])?;
            self.write_text(b"\n\r")?;
            return self.write_line(&escape);
        }
        self.command_line(buffer)
    }

    /// The error block at `block` as the Supervisor prints it: a newline,
    /// its text, and its number in hexadecimal without leading noughts.
    fn supervisor_error(&mut self, block: u32) -> Result<(), Fail> {
        let number = self.read_word(block)?;
        let (text, _) = self.read_until(block.wrapping_add(4), crate::error::TEXT, |b| b == 0)?;
        let hex = crate::env::hex8(number);
        let first = hex.iter().position(|&d| d != b'0').unwrap_or(hex.len() - 1);
        let line = self.message_with(b"SupervisorErrorNumber", &[&hex[first..]])?;
        self.write_text(b"\n\r")?;
        self.write_text(&text)?;
        self.write_line(&line)
    }
}

/// The Supervisor: its banner, then lines read and run for as long as it is
/// the application.
pub(crate) fn start(os: &mut Os<'_>, _regs: &mut Regs) -> Result<(), Fail> {
    let banner = os.message_with(b"SupervisorBanner", &[])?;
    os.write_line(&banner)?;
    os.write_text(b"\n\r")?;
    loop {
        let (buffer, held) = os.cli_buffer();
        let result = os.supervisor_line(buffer);
        os.world.cli.release(held);
        match result {
            Ok(()) => {}
            Err(Fail::Error(block)) => os.supervisor_error(block)?,
            Err(unwind) => return Err(unwind),
        }
    }
}

/// `*GOS`: the Supervisor entered as the application, the tail its
/// parameters.
pub(crate) fn gos(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let title = crate::module::utility::UTILITY.title.as_bytes();
    let (buffer, held) = os.cli_buffer();
    let entered = os
        .write_terminated(buffer, title, 0, super::BUFFER as usize)
        .and_then(|()| {
            let mut enter = Regs::default();
            enter.r[..3].copy_from_slice(&[ENTER, buffer, regs.r[0]]);
            os.perform(X | os::MODULE, &mut enter)
        });
    os.world.cli.release(held);
    entered
}

#[cfg(test)]
mod tests;
