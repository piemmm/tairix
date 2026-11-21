//! `*Help`: what each module's command table says of the keywords given,
//! what a module title names, and the kernel's own topics listing the
//! commands, filing system commands and modules there are.
//!
//! Help strings are laid out by `OS_PrettyPrint`, so a module's own text is
//! wrapped and tabbed as RISC OS wraps it.

use alloc::vec::Vec;

use super::table::{Item, CONFIGURE, FILING, HELP_IS_CODE, INTERNATIONAL};
use crate::module::{Field, ModuleKey};
use crate::msgtrans::DICTIONARY;
use crate::os::{Fail, Os, Regs};
use crate::swi::{os, X};

/// The buffer help code is given, and the most a list of names gathers
/// before it is written out.
const HELP_BUFFER: u32 = 512;
const LIST_MOST: usize = 512;
/// The keyword `*Help` alone gives help on.
const HELP: &[u8] = b"Help";
/// The bits of an information word a list does not look at: help that is
/// code, and help that is a message token.
const HELP_FLAGS: u32 = HELP_IS_CODE | INTERNATIONAL;

/// A list of a module's table that `*Help` shows.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum List {
    Commands,
    Filing,
    Configure,
    /// Entries that are help alone.
    HelpOnly,
}

impl List {
    /// Whether the entry with code at `code` and information word `info`
    /// is in the list.
    const fn takes(self, code: u32, info: u32) -> bool {
        let kind = (info & !HELP_FLAGS) >> 24;
        match self {
            Self::Commands => code != 0 && kind == 0,
            Self::Filing => code != 0 && kind == FILING >> 24,
            Self::Configure => code != 0 && kind == CONFIGURE >> 24,
            Self::HelpOnly => code == 0,
        }
    }

    /// The token of the heading a module title's help gives the list.
    const fn token(self) -> &'static [u8] {
        match self {
            Self::Commands => b"CommandsHeading",
            Self::Filing => b"FilingCommandsHeading",
            Self::Configure => b"ConfigureKeywordsHeading",
            Self::HelpOnly => b"HelpTopicsHeading",
        }
    }
}

/// What heads a list of a module's entries.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Heading<'a> {
    /// A message's text.
    Text(&'a [u8]),
    /// The module's help string.
    Module,
}

/// Whether `keyword` names `name` as `*Help` reads one: the same letters in
/// either case until both end at a space or a control character, or a `.`
/// where the name goes on — so that `.` alone names everything.
pub(crate) fn names(keyword: &[u8], name: &[u8]) -> bool {
    let mut i = 0;
    loop {
        let k = keyword.get(i).copied().unwrap_or(0);
        let n = name.get(i).copied().unwrap_or(0);
        if k <= b' ' && n <= b' ' {
            return true;
        }
        if !k.eq_ignore_ascii_case(&n) {
            return k == b'.' && n > b' ';
        }
        i += 1;
    }
}

/// Where the keyword after the one at `at` in `line` starts: past a `.` or
/// a space, and the spaces after it; none at the line's end.
pub(crate) fn next_keyword(line: &[u8], at: usize) -> Option<usize> {
    let stop = at
        + line
            .get(at..)?
            .iter()
            .position(|&c| c == b'.' || c <= b' ')?;
    let after = stop + 1;
    Some(after + line.get(after..)?.iter().position(|&c| c != b' ')?)
}

impl Os<'_> {
    /// The line that heads each help `*Help` finds, for `name`, once Escape
    /// has been looked for.
    fn help_found(&mut self, name: &[u8]) -> Result<(), Fail> {
        self.escape_point()?;
        let found = self.message_with(b"HelpHeading", &[name])?;
        self.write_c(b'\r')?;
        self.write_line(&found)
    }

    /// `MessageTrans`'s dictionary, the kernel's empty one without it.
    fn message_dictionary(&mut self) -> Result<u32, Fail> {
        let mut regs = Regs::default();
        match self.perform(X | DICTIONARY, &mut regs) {
            Ok(()) => Ok(regs.r[0]),
            Err(Fail::Error(_)) => Ok(0),
            Err(unwind) => Err(unwind),
        }
    }

    /// `token`'s message laid out as `OS_PrettyPrint` lays one out.
    fn pretty_message(&mut self, token: &[u8]) -> Result<(), Fail> {
        let text = self.message_with(token, &[])?;
        let dictionary = self.message_dictionary()?;
        self.pretty_with(&text, dictionary, b"")
    }

    /// The text at `address` laid out by `OS_PrettyPrint`, with the kernel's
    /// dictionary and `special` for an escape and zero.
    fn pretty_at(&mut self, address: u32, special: u32) -> Result<(), Fail> {
        let mut regs = Regs::default();
        regs.r[..3].copy_from_slice(&[address, 0, special]);
        self.perform(X | os::PRETTY_PRINT, &mut regs)
    }

    /// The help of each entry of `key`'s table that `keyword` names; whether
    /// there was any.
    fn help_in_module(&mut self, key: ModuleKey, keyword: &[u8]) -> Result<bool, Fail> {
        let mut any = false;
        self.walk_table(key, |os, item, name| {
            if item.help != 0 && names(keyword, name) {
                any = true;
                os.entry_help(item, name)?;
            }
            Ok(None::<()>)
        })?;
        Ok(any)
    }

    /// `item`'s help, under its name `name`: what its code gives, its
    /// messages, or its own string, which runs on into its syntax.
    fn entry_help(&mut self, item: &Item, name: &[u8]) -> Result<(), Fail> {
        self.help_found(name)?;
        if item.info & HELP_IS_CODE != 0 {
            let (buffer, held) = self.cli_buffer();
            let given = self.call_help(item, buffer, HELP_BUFFER);
            let shown = match given {
                Ok(0) => Ok(()),
                Ok(text) => self.pretty_at(text, 0),
                Err(fail) => Err(fail),
            };
            self.world.cli.release(held);
            shown?;
            return self.write_text(b"\n\r");
        }
        if item.info & INTERNATIONAL == 0 {
            self.pretty_at(item.base.wrapping_add(item.help), item.name)?;
            return self.write_text(b"\n\r");
        }
        for offset in [item.help, item.syntax] {
            if offset == 0 {
                continue;
            }
            let (token, _) =
                self.read_until(item.base.wrapping_add(offset), super::LONGEST, |b| b < b' ')?;
            let text = self.looked_up(item, &token)?;
            let dictionary = self.message_dictionary()?;
            self.pretty_with(&text, dictionary, name)?;
            self.write_text(b"\n\r")?;
        }
        Ok(())
    }

    /// The help a module's title names: its help string and the lists of
    /// what its table holds.
    fn title_help(&mut self, key: ModuleKey, title: &[u8]) -> Result<(), Fail> {
        self.help_found(title)?;
        let Some(shape) = self.world.modules.get(key).map(|m| m.shape) else {
            return Ok(());
        };
        let help = shape.header.get(Field::Help);
        if help != 0 {
            self.pretty_message(b"ModuleHelpLabel")?;
            self.write_c(b' ')?;
            let (text, _) =
                self.read_until(shape.code.wrapping_add(help), super::LONGEST, |b| b == 0)?;
            let dictionary = self.message_dictionary()?;
            self.pretty_with(&text, dictionary, b"")?;
        }
        for list in [
            List::Commands,
            List::Filing,
            List::Configure,
            List::HelpOnly,
        ] {
            let heading = self.message_with(list.token(), &[])?;
            self.list_entries(key, list, Heading::Text(&heading))?;
        }
        self.write_text(b"\n\r")
    }

    /// A module's help string and a newline, or for a module with none,
    /// that it is untitled.
    fn module_title(&mut self, key: ModuleKey) -> Result<(), Fail> {
        let Some(shape) = self.world.modules.get(key).map(|m| m.shape) else {
            return Ok(());
        };
        let help = shape.header.get(Field::Help);
        if help == 0 {
            let untitled = self.message_with(b"NoModuleTitle", &[])?;
            self.write_text(&untitled)?;
        } else {
            self.pretty_at(shape.code.wrapping_add(help), 0)?;
        }
        self.write_text(b"\n\r")
    }

    /// The entries of `key`'s table in `list`, under `heading`: a blank
    /// line and the heading before the first, then their names, a tab after
    /// each, written out as a paragraph whenever 512 bytes of them gather.
    fn list_entries(
        &mut self,
        key: ModuleKey,
        list: List,
        heading: Heading<'_>,
    ) -> Result<(), Fail> {
        let mut names: Vec<u8> = Vec::new();
        let mut any = false;
        self.walk_table(key, |os, item, name| {
            if !list.takes(item.code, item.info) {
                return Ok(None::<()>);
            }
            if !any {
                any = true;
                os.write_text(b"\n\r\n\r")?;
                match heading {
                    Heading::Text(text) => {
                        os.pretty(text)?;
                        os.write_text(b"\n\r")?;
                    }
                    Heading::Module => os.module_title(key)?,
                }
            }
            if LIST_MOST - names.len() < name.len() + 1 {
                names.pop();
                os.pretty(&names)?;
                os.write_text(b"\n\r")?;
                names.clear();
            }
            names
                .try_reserve(name.len() + 1)
                .map_err(|_| os.no_memory())?;
            names.extend_from_slice(name);
            names.push(b'\t');
            Ok(None)
        })?;
        if any {
            names.pop();
            self.pretty(&names)?;
        }
        Ok(())
    }

    /// The help on each keyword of `line`: every module's entries it names,
    /// then the modules whose titles it names; whether there was any.
    fn help_on(&mut self, line: &[u8]) -> Result<bool, Fail> {
        let mut any = false;
        let mut at = Some(0);
        while let Some(start) = at {
            let keyword = &line[start..];
            let chain = self.world.modules.chain().to_vec();
            for &key in &chain {
                any |= self.help_in_module(key, keyword)?;
            }
            for &key in &chain {
                let titled = self
                    .world
                    .modules
                    .get(key)
                    .is_some_and(|m| m.shape.header.get(Field::Title) != 0);
                if !titled {
                    continue;
                }
                let title = self.title_of(key)?;
                if names(keyword, &title) {
                    any = true;
                    self.title_help(key, &title)?;
                }
            }
            at = next_keyword(line, start);
        }
        Ok(any)
    }

    /// The kernel's lists of a kind of entry: `token`'s message, then each
    /// module's entries in `list` under its help string.
    fn every_module(&mut self, token: &[u8], list: List, regs: &mut Regs) -> Result<(), Fail> {
        self.pretty_message(token)?;
        let chain = self.world.modules.chain().to_vec();
        for key in chain {
            self.list_entries(key, list, Heading::Module)?;
        }
        regs.r[0] = 0;
        Ok(())
    }
}

/// `*Help`: help on each keyword given, `Help` with none, unless a module
/// gives it, paged.
pub(crate) fn help(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let mut service = Regs::default();
    service.r[..3].copy_from_slice(&[regs.r[0], crate::service::HELP, regs.r[1]]);
    os.service(&mut service)?;
    if service.r[1] == 0 {
        return Ok(());
    }
    let line = if regs.r[1] == 0 {
        HELP.to_vec()
    } else {
        os.read_until(regs.r[0], super::LONGEST, |b| b < b' ')?.0
    };
    let status = os.paged()?;
    let result = os.help_on(&line).and_then(|any| {
        if any {
            return Ok(());
        }
        let none = os.message_with(b"NoHelpAvailable", &[])?;
        os.write_line(&none)
    });
    os.unpaged(status)?;
    result
}

/// `*Help Commands`: every module's commands.
pub(crate) fn commands(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    os.every_module(b"AllCommandsIntroduction", List::Commands, regs)
}

/// `*Help FileCommands`: every module's filing system commands.
pub(crate) fn file_commands(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    os.every_module(b"AllFilingCommandsIntroduction", List::Filing, regs)
}

/// `*Help Configure`: every module's configuration keywords, the kernel's
/// own being `*Configure`'s to list.
pub(crate) fn configure(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    os.every_module(b"AllConfigureKeywordsIntroduction", List::Configure, regs)
}

/// `*Help Modules`: each module's help string.
pub(crate) fn modules(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    os.pretty_message(b"AllModulesIntroduction")?;
    let chain = os.world.modules.chain().to_vec();
    for key in chain {
        os.module_title(key)?;
    }
    regs.r[0] = 0;
    Ok(())
}

#[cfg(test)]
mod tests;
