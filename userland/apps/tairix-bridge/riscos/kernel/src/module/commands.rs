//! The module handler's `*` commands, which the `UtilityModule` provides: each
//! an `OS_Module` call, or a listing of the chain or the ROM's modules laid
//! out as RISC OS lays it out.

use alloc::vec::Vec;

use super::header::Field;
use super::version;
use crate::error::Error;
use crate::os::{Fail, Os, Regs};
use crate::swi::os::read_unsigned::{DECIMAL, TERMINATED};
use crate::swi::{os, X};

/// "No ROM module has that name".
const NOT_IN_ROM: Error = Error {
    number: 0x106,
    token: "ModuleNotInRom",
};

/// "Module … is an older version than required".
const TOO_OLD: Error = Error {
    number: 0x10F,
    token: "ModuleVersionTooOld",
};

/// The longest help string a version is read from.
const HELP_MOST: usize = 256;

pub(crate) fn rm_load(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    os.module_reason(1, regs)
}

pub(crate) fn rm_run(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    os.module_reason(0, regs)
}

pub(crate) fn rm_kill(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    os.module_reason(4, regs)
}

pub(crate) fn rm_reinit(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    os.module_reason(3, regs)
}

pub(crate) fn rm_clear(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    os.module_reason(9, regs)
}

pub(crate) fn rm_tidy(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    os.module_reason(8, regs)
}

pub(crate) fn rm_faster(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    os.rm_faster(regs)
}

pub(crate) fn rm_ensure(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    os.rm_ensure(regs)
}

pub(crate) fn modules_command(os: &mut Os<'_>, _regs: &mut Regs) -> Result<(), Fail> {
    os.list_modules()
}

pub(crate) fn rom_modules_command(os: &mut Os<'_>, _regs: &mut Regs) -> Result<(), Fail> {
    os.list_rom_modules()
}

pub(crate) fn unplug(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    if regs.r[1] == 0 {
        os.list_unplugged()
    } else {
        os.plug(regs.r[0], true)
    }
}

pub(crate) fn rm_insert(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    os.plug(regs.r[0], false)
}

impl Os<'_> {
    /// `OS_Module` `reason` with R1 the command's tail.
    fn module_reason(&mut self, reason: u32, regs: &Regs) -> Result<(), Fail> {
        let mut call = Regs::default();
        call.r[..2].copy_from_slice(&[reason, regs.r[0]]);
        self.perform(os::MODULE, &mut call)
    }

    /// `*RMFaster`: the ROM module the tail names copied into the RMA, as
    /// `OS_Module` 11 copies it.
    fn rm_faster(&mut self, regs: &Regs) -> Result<(), Fail> {
        let mut look = Regs::default();
        look.r[..2].copy_from_slice(&[18, regs.r[0]]);
        self.perform(os::MODULE, &mut look)?;
        let code = look.r[3];
        if !self.world.modules.roms.iter().any(|rom| rom.image == code) {
            return Err(self.error(NOT_IN_ROM));
        }
        let size = self.read_word(code.wrapping_sub(4))?;
        let mut copy = Regs::default();
        copy.r[..3].copy_from_slice(&[11, code, size]);
        self.perform(os::MODULE, &mut copy)
    }

    /// `*RMEnsure`: nothing if the module is there at the version or later;
    /// otherwise the command after the version, or, with none, the error.
    fn rm_ensure(&mut self, regs: &Regs) -> Result<(), Fail> {
        let (tail, count) = (regs.r[0], regs.r[1]);
        let mut look = Regs::default();
        look.r[..2].copy_from_slice(&[18, tail]);
        let absent = match self.perform(os::MODULE, &mut look) {
            Ok(()) => None,
            Err(Fail::Error(block)) => Some(block),
            Err(unwind) => return Err(unwind),
        };
        let (text, _) = self.read_until(tail, crate::cli::LONGEST, |b| b < b' ')?;
        let wanted_at = text
            .iter()
            .position(|&b| b == b' ')
            .map_or(text.len(), |at| at + 1);
        let wanted = version::parse(&text[wanted_at..]);
        if absent.is_none() {
            // A module with no help string has no version: older than any.
            let code = look.r[3];
            let help = self.read_word(code.wrapping_add(Field::Help.offset()))?;
            if help != 0 {
                let (string, _) =
                    self.read_until(code.wrapping_add(help), HELP_MOST, |b| b == 0)?;
                if version::of_help(&string).cast_signed() >= wanted.cast_signed() {
                    return Ok(());
                }
            }
        }
        if count <= 2 {
            if let Some(block) = absent {
                return Err(Fail::Error(block));
            }
            let code = look.r[3];
            let title = self.read_word(code.wrapping_add(Field::Title.offset()))?;
            let (title, _) = self.read_until(code.wrapping_add(title), HELP_MOST, |b| b <= b' ')?;
            return Err(self.error_with(TOO_OLD, &[&title]));
        }
        let after_title = text.iter().position(|&b| b == b' ').unwrap_or(text.len());
        let version_at = after_title
            + text[after_title..]
                .iter()
                .take_while(|&&b| b == b' ')
                .count();
        let command = version_at
            + text[version_at..]
                .iter()
                .take_while(|&&b| b != b' ')
                .count();
        let mut cli = Regs::default();
        cli.r[0] = tail.wrapping_add(crate::os::len32(command));
        self.perform(X | os::CLI, &mut cli)
    }

    /// `*Modules`: a line for each incarnation of each module of the chain.
    fn list_modules(&mut self) -> Result<(), Fail> {
        let heading = self.message_with(b"ModulesHeading", &[])?;
        self.write_line(&heading)?;
        let (mut at, mut index, mut number) = (0, 0, 0u32);
        let mut continuing = false;
        loop {
            if self.escape_set() {
                return Err(self.escape_acknowledged());
            }
            let mut names = Regs::default();
            names.r[..3].copy_from_slice(&[12, at, index]);
            if self.perform(os::MODULE, &mut names).is_err() {
                return Ok(());
            }
            if continuing {
                self.write_text(b"   ")?;
            } else {
                number += 1;
                let text = crate::convert::Decimal::of(number.cast_signed());
                let digits = text.as_bytes();
                for _ in digits.len()..3 {
                    self.write_c(b' ')?;
                }
                self.write_text(digits)?;
            }
            self.write_c(b' ')?;
            self.write_text(&crate::env::hex8(names.r[3]))?;
            self.write_c(b' ')?;
            self.write_text(&crate::env::hex8(names.r[4]))?;
            self.write_text(b"  ")?;
            let code = names.r[3];
            let title = self.read_word(code.wrapping_add(Field::Title.offset()))?;
            if title == 0 {
                let untitled = self.message_with(b"NoModuleTitle", &[])?;
                self.write_text(&untitled)?;
            } else {
                let (title, _) =
                    self.read_until(code.wrapping_add(title), HELP_MOST, |b| b == 0)?;
                self.write_text(&title)?;
            }
            let more = names.r[2] != 0;
            if continuing || more {
                let (postfix, _) = self.read_until(names.r[5], HELP_MOST, |b| b == 0)?;
                self.write_c(b'%')?;
                self.write_text(&postfix)?;
            }
            self.write_text(b"\n\r")?;
            continuing = more;
            (at, index) = (names.r[1], names.r[2]);
        }
    }

    /// Each ROM module `OS_Module` 20 enumerates, its answer handed to
    /// `visit`: the main ROM's, then the expansion card's, each answer naming
    /// the next. Escape stops it.
    fn each_rom_module(
        &mut self,
        mut visit: impl FnMut(&mut Self, &Regs) -> Result<(), Fail>,
    ) -> Result<(), Fail> {
        let (mut at, mut section) = (0, u32::MAX);
        loop {
            if self.escape_set() {
                return Err(self.escape_acknowledged());
            }
            let mut rom = Regs::default();
            rom.r[..3].copy_from_slice(&[20, at, section]);
            if self.perform(os::MODULE, &mut rom).is_err() {
                return Ok(());
            }
            visit(self, &rom)?;
            (at, section) = (rom.r[1], rom.r[2]);
        }
    }

    /// `*ROMModules`: a line for each ROM module, laid out as
    /// `OS_PrettyPrint` lays it out.
    fn list_rom_modules(&mut self) -> Result<(), Fail> {
        let mut heading = self.message_with(b"RomModulesHeading", &[])?;
        heading.push(b'\r');
        self.pretty(&heading)?;
        self.each_rom_module(|os, rom| {
            let line = os.rom_line(rom)?;
            os.pretty(&line)
        })
    }

    /// `*ROMModules`' line for the module `OS_Module` 20 answered with
    /// `rom`: its number in its section, a hard space padding it to three
    /// columns, where it is, its title tabbed to a column of its own, its
    /// version and status.
    fn rom_line(&mut self, rom: &Regs) -> Result<Vec<u8>, Fail> {
        let number = crate::convert::Decimal::of(rom.r[1].cast_signed());
        let number = number.as_bytes();
        let pad = if number.len() > 3 {
            1
        } else {
            3 - number.len()
        };
        let place = if rom.r[2] == u32::MAX {
            self.message_with(b"SystemRomSection", &[])?
        } else {
            let card = crate::convert::Decimal::of(rom.r[2].cast_signed());
            self.message_with(b"PoduleSection", &[card.as_bytes()])?
        };
        let (title, _) = self.read_until(rom.r[3], HELP_MOST, |b| b == 0)?;
        let tabs = (24usize.saturating_sub(title.len())).div_ceil(8);
        let status: &[u8] = match rom.r[4] {
            u32::MAX => b"ModuleUnplugged",
            0 => b"ModuleDormant",
            1 => b"ModuleActive",
            _ => b"ModuleRunning",
        };
        let status = self.message_with(status, &[])?;
        let version = version::text(rom.r[6]);
        let mut line = Vec::new();
        let length = pad
            + number.len()
            + place.len()
            + title.len()
            + tabs
            + version.len()
            + status.len()
            + 4;
        if line.try_reserve_exact(length).is_err() {
            return Err(self.no_memory());
        }
        line.resize(pad, 31);
        line.extend_from_slice(number);
        line.push(b' ');
        line.extend_from_slice(&place);
        line.push(b'\t');
        line.extend_from_slice(&title);
        line.resize(line.len() + tabs, b'\t');
        line.extend_from_slice(&version);
        line.push(b'\t');
        line.extend_from_slice(&status);
        line.push(b'\r');
        Ok(line)
    }

    /// Unplugs or plugs in the ROM module the tail at `tail` names: every
    /// module of the title, or only one in the section a number after it
    /// names — the main ROM's being -1, the only one there is. Unplugging
    /// kills every copy of it, each incarnation of each.
    fn plug(&mut self, tail: u32, unplug: bool) -> Result<(), Fail> {
        let (text, _) = self.read_until(tail, crate::cli::LONGEST, |b| b < b' ')?;
        let title_end = text
            .iter()
            .position(|&b| b <= b' ' || b == 0x7F)
            .unwrap_or(text.len());
        let rest = &text[title_end..];
        let rest = &rest[rest.iter().take_while(|&&b| b == b' ').count()..];
        if let Some(&first) = rest.first().filter(|&&b| b > b' ' && b != 0x7F) {
            let (negative, number) = match first {
                b'-' => (true, &rest[1..]),
                _ => (false, rest),
            };
            let at = tail.wrapping_add(crate::os::len32(text.len() - number.len()));
            let (read, _) = self.number_at(at, TERMINATED | DECIMAL, 0)?;
            let section = if negative { read.wrapping_neg() } else { read };
            if section != u32::MAX {
                return Err(self.error(NOT_IN_ROM));
            }
        }
        let Some(place) = self.rom_named(&text[..title_end]) else {
            return Err(self.error(NOT_IN_ROM));
        };
        self.set_unplugged(place, unplug)?;
        if !unplug {
            return Ok(());
        }
        let title = self.world.modules.roms[place].builtin.title.as_bytes();
        while let Some(key) = self.find_module(title)? {
            self.kill_and_free(key)?;
        }
        Ok(())
    }

    /// `*Unplug` with no title: the unplugged ROM modules, or that there are
    /// none.
    fn list_unplugged(&mut self) -> Result<(), Fail> {
        let mut any = false;
        self.each_rom_module(|os, rom| {
            if rom.r[4] == u32::MAX {
                if !any {
                    let heading = os.message_with(b"UnpluggedModulesHeading", &[])?;
                    os.write_line(&heading)?;
                    any = true;
                }
                let (title, _) = os.read_until(rom.r[3], HELP_MOST, |b| b == 0)?;
                os.write_line(&title)?;
            }
            Ok(())
        })?;
        if !any {
            let none = self.message_with(b"NoModulesUnplugged", &[])?;
            self.write_line(&none)?;
        }
        Ok(())
    }
}
