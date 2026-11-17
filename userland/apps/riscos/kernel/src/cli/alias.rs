//! Aliases: a command named by an `Alias$` variable, its value — expanded,
//! a macro's GSTrans'd — run a line at a time with the command's parameters
//! substituted for `%0` to `%9` and `%*0` to `%*9`, as `OS_SubstituteArgs`
//! substitutes them.
//!
//! An exact name is looked for as it is; one ending in `.` abbreviates the
//! first alias, in the variables' order, it begins.

use alloc::vec::Vec;

use tairix_obey::name::upper;

use super::{is_end, name_byte, BUFFER, LONGEST, LONG_LINE};
use crate::os::{len32, Fail, Os, Regs};
use crate::swi::X;

/// The prefix every alias's variable has.
const ALIAS: &[u8] = b"Alias$";

impl Os<'_> {
    /// The alias the command at `at` names, run; none if no alias names it.
    pub(super) fn alias(&mut self, at: u32) -> Result<Option<Result<(), Fail>>, Fail> {
        let (line, _) = self.read_until(at, LONGEST, is_end)?;
        let length = line
            .iter()
            .position(|&b| name_byte(b).is_none())
            .unwrap_or(line.len());
        let name = &line[..length];
        if name.is_empty() {
            return Ok(None);
        }
        let found = if name.ends_with(b".") {
            self.abbreviated_alias(name)
        } else {
            let mut variable = Vec::new();
            if variable
                .try_reserve_exact(ALIAS.len() + name.len())
                .is_err()
            {
                return Err(self.no_memory());
            }
            variable.extend_from_slice(ALIAS);
            variable.extend(name.iter().map(|&b| name_byte(b).unwrap_or(b)));
            self.world
                .vars
                .exact(&variable)
                .map(|variable| (variable, length))
        };
        match found {
            Some((variable, parameters)) => Ok(Some(
                self.run_alias(&variable, at.wrapping_add(len32(parameters))),
            )),
            None => Ok(None),
        }
    }

    /// `Alias$.`, run for a command that is a `.` and what follows it; none
    /// if there is no such alias.
    pub(super) fn alias_of_dot(&mut self, at: u32) -> Option<Result<(), Fail>> {
        let variable = self.world.vars.exact(b"Alias$.")?;
        Some(self.run_alias(&variable, at.wrapping_add(1)))
    }

    /// The first alias, in the variables' order, that the abbreviation
    /// `typed` names, and where its parameters start: past the `.`.
    fn abbreviated_alias(&self, typed: &[u8]) -> Option<(Vec<u8>, usize)> {
        let typed: Vec<u8> = typed.iter().map(|&b| name_byte(b).unwrap_or(b)).collect();
        self.world.vars.named_from(ALIAS).find_map(|variable| {
            let name = &variable[ALIAS.len()..];
            let dot = typed.iter().position(|&b| b == b'.')?;
            let matches = dot > 0
                && name.len() > dot
                && typed[..dot].iter().zip(name).all(|(&t, &n)| t == upper(n));
            matches.then(|| (variable.to_vec(), dot + 1))
        })
    }

    /// Runs the alias `variable` with the parameters at `parameters`.
    fn run_alias(&mut self, variable: &[u8], parameters: u32) -> Result<(), Fail> {
        let Some(value) = self.variable_text(variable)? else {
            return Ok(());
        };
        if value.len() >= LONGEST {
            return Err(self.error(LONG_LINE));
        }
        let template = self.world.cli.expansion;
        self.write_terminated(template, &value, b'\r', LONGEST)?;
        let (buffer, number) = self.cli_buffer();
        let mut substitute = Regs::default();
        substitute.r[..6].copy_from_slice(&[
            parameters,
            buffer,
            BUFFER,
            template,
            len32(value.len()),
            0,
        ]);
        let result = match self.perform(X | crate::swi::os::SUBSTITUTE_ARGS32, &mut substitute) {
            Ok(()) => self.run_lines(buffer, number, substitute.r[2]),
            Err(Fail::Error(_)) => Err(self.error(LONG_LINE)),
            Err(unwind) => Err(unwind),
        };
        self.cli_buffer_done(number, result)
    }

    /// Runs each line of the `length` bytes at `buffer`, the ring's buffer
    /// `number`, until one fails or the buffer is handed out again.
    fn run_lines(&mut self, buffer: u32, number: u32, length: u32) -> Result<(), Fail> {
        let mut at = 0;
        while at < length {
            let (line, _) =
                self.read_until(buffer.wrapping_add(at), (length - at) as usize, is_end)?;
            self.command_line(buffer.wrapping_add(at))?;
            if !self.world.cli.intact(number) {
                break;
            }
            at += len32(line.len()) + 1;
        }
        Ok(())
    }
}
