//! Command tables: each module's `*` commands as its table lists them, a
//! command found by name as RISC OS's interpreter finds one, its parameters
//! counted and, where its table asks, GSTrans'd, and the command called.
//!
//! A table is read from the module's code, a built-in's from its image, so
//! every module is found the same way.

use alloc::vec::Vec;

use tairix_obey::name::upper;

use super::{is_end, name_byte, BUFFER, LONGEST, LONG_LINE};
use crate::error::Error;
use crate::module::{Command, Field, Help, ModuleKey};
use crate::msgtrans::{CLOSE_FILE, LOOKUP, OPEN_FILE};
use crate::os::{len32, Entry as CallEntry, Fail, Os, Regs, Target};
use crate::swi::os::fs_control::{MODULE_BASE, SECONDARY_BASE};
use crate::swi::X;

/// An information word's flags: a filing system's command, a configuration
/// keyword, help that is code, help and syntax that are message tokens.
pub(crate) const FILING: u32 = 1 << 31;
pub(crate) const CONFIGURE: u32 = 1 << 30;
pub(crate) const HELP_IS_CODE: u32 = 1 << 29;
pub(crate) const INTERNATIONAL: u32 = 1 << 28;
const KIND: u32 = FILING | CONFIGURE;

/// The longest name in a command table the kernel reads.
const NAME_MOST: usize = 256;

/// `Service_SyntaxError`.
const SYNTAX_SERVICE: u32 = 0x8C;
/// The token of the message a command with no syntax of its own gives.
const NO_SYNTAX: &[u8] = b"WrongNumberOfParameters";
/// The kernel's commands whose number may run on from their name.
const FUDGED: [&str; 4] = ["FX", "Key", "Opt", "TV"];

/// "Unexpected character in parameter expansion".
const BAD_PARAMETER: Error = Error {
    number: 0xFD,
    token: "BadParameterExpansion",
};

/// One entry of a command table.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Item {
    /// The module of the chain it is in, if it is one.
    pub(crate) key: Option<ModuleKey>,
    /// The module's code.
    pub(crate) base: u32,
    /// Its place in the table.
    pub(crate) index: usize,
    /// Its name's address.
    pub(crate) name: u32,
    /// Its four words' address.
    pub(crate) words: u32,
    /// The offset of its code, zero for none.
    pub(crate) code: u32,
    /// Its information word.
    pub(crate) info: u32,
    /// The offsets of its syntax and help, zero for none.
    pub(crate) syntax: u32,
    pub(crate) help: u32,
}

/// What a lookup wants: an ordinary command, a filing system's or a
/// configuration keyword.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Wanted {
    Command,
    Filing,
    Configure,
}

impl Wanted {
    const fn flags(self) -> u32 {
        match self {
            Self::Command => 0,
            Self::Filing => FILING,
            Self::Configure => CONFIGURE,
        }
    }
}

/// Where the command `typed` starts with ends if `name`, a table's, names
/// it: both ending together, or a `.` in `typed` while `name` goes on —
/// past it then — or, when `messy`, `name` ending where `typed` goes on
/// with no letter, as `FX0` names `FX`.
pub(crate) fn matches(typed: &[u8], name: &[u8], messy: bool) -> Option<usize> {
    let mut i = 0;
    loop {
        let t = typed.get(i).copied().and_then(name_byte);
        let n = name.get(i).copied().filter(|&b| b > b' ').map(upper);
        match (t, n) {
            (None, None) => return Some(i),
            (Some(b'.'), Some(_)) => return Some(i + 1),
            (t, None) if messy && !t.is_some_and(|c| c.is_ascii_uppercase()) => return Some(i),
            (Some(t), Some(n)) if t == n => i += 1,
            _ => return None,
        }
    }
}

impl Os<'_> {
    /// Each entry of `key`'s command table in turn, until `visit` says
    /// enough: what it found then.
    pub(crate) fn walk_table<T>(
        &mut self,
        key: ModuleKey,
        visit: impl FnMut(&mut Self, &Item, &[u8]) -> Result<Option<T>, Fail>,
    ) -> Result<Option<T>, Fail> {
        let Some(shape) = self.world.modules.get(key).map(|m| m.shape) else {
            return Ok(None);
        };
        let table = shape.header.get(Field::Commands);
        self.walk_table_at(Some(key), shape.code, shape.header.size(), table, visit)
    }

    /// Each entry of the command table at `table` in the module at `base`,
    /// `size` bytes, in turn, until `visit` says enough.
    fn walk_table_at<T>(
        &mut self,
        key: Option<ModuleKey>,
        base: u32,
        size: u32,
        table: u32,
        mut visit: impl FnMut(&mut Self, &Item, &[u8]) -> Result<Option<T>, Fail>,
    ) -> Result<Option<T>, Fail> {
        if table == 0 {
            return Ok(None);
        }
        let mut offset = table;
        for index in 0.. {
            if offset >= size {
                break;
            }
            let at = base.wrapping_add(offset);
            let (name, ended) = self.read_until(at, NAME_MOST, |b| b == 0)?;
            if name.is_empty() || !ended {
                break;
            }
            let words_offset = (offset + len32(name.len()) + 1).next_multiple_of(4);
            if words_offset.saturating_add(16) > size {
                break;
            }
            let words = base.wrapping_add(words_offset);
            let mut field = [0; 4];
            for (slot, word) in (words..).step_by(4).zip(field.iter_mut()) {
                *word = self.read_word(slot)?;
            }
            let item = Item {
                key,
                base,
                index,
                name: at,
                words,
                code: field[0],
                info: field[1],
                syntax: field[2],
                help: field[3],
            };
            if let Some(found) = visit(self, &item, &name)? {
                return Ok(Some(found));
            }
            offset = words_offset + 16;
            self.yield_point()?;
        }
        Ok(None)
    }

    /// The entry of `key`'s table that names the command `typed` as
    /// `wanted`, and where its tail starts in `typed`.
    pub(crate) fn find_in_table(
        &mut self,
        key: ModuleKey,
        typed: &[u8],
        wanted: Wanted,
    ) -> Result<Option<(Item, usize)>, Fail> {
        self.walk_table(key, |_, item, name| {
            Ok(named(item, name, typed, wanted, false))
        })
    }

    /// The module command at `at`, run: in `only`'s table, or wherever RISC
    /// OS looks — the kernel's table and then the modules in the order their
    /// commands came, or for an abbreviation the chain in order. None if no
    /// module has it.
    pub(crate) fn module_command(
        &mut self,
        at: u32,
        only: Option<ModuleKey>,
    ) -> Result<Option<Result<(), Fail>>, Fail> {
        let (typed, _) = self.read_until(at, LONGEST, is_end)?;
        let name = typed
            .iter()
            .position(|&b| name_byte(b).is_none())
            .map_or(&typed[..], |end| &typed[..end]);
        let found = match only {
            Some(key) => self.find_in_table(key, &typed, Wanted::Command)?,
            None if name.contains(&b'.') => {
                let chain = self.world.modules.chain().to_vec();
                self.first_in(&chain, &typed)?
            }
            None => self.find_exact(&typed)?,
        };
        let Some((item, end)) = found else {
            return Ok(None);
        };
        let private = self.private_of(item.key);
        Ok(Some(self.run_item(
            &item,
            at.wrapping_add(len32(end)),
            private,
        )))
    }

    /// The first of `keys` whose table names the command `typed`.
    fn first_in(
        &mut self,
        keys: &[ModuleKey],
        typed: &[u8],
    ) -> Result<Option<(Item, usize)>, Fail> {
        for &key in keys {
            if let Some(found) = self.find_in_table(key, typed, Wanted::Command)? {
                return Ok(Some(found));
            }
        }
        Ok(None)
    }

    /// The command `typed` names exactly: one of the kernel's old commands
    /// whose number runs on from its name, as `FX0` does; then the
    /// modules', the kernel's first, in the module list's order.
    fn find_exact(&mut self, typed: &[u8]) -> Result<Option<(Item, usize)>, Fail> {
        let kernel = self.world.modules.nth(0);
        let numbered = typed
            .iter()
            .skip(2)
            .take(3)
            .take_while(|&&b| b > b' ')
            .any(|&b| b == b'&' || b.is_ascii_digit());
        if let (true, Some(kernel)) = (numbered, kernel) {
            let fudged = self.walk_table(kernel, |_, item, name| {
                let old = FUDGED.iter().any(|f| f.as_bytes() == name);
                Ok(old
                    .then(|| named(item, name, typed, Wanted::Command, true))
                    .flatten())
            })?;
            if fudged.is_some() {
                return Ok(fudged);
            }
        }
        let mut order = Vec::new();
        order
            .try_reserve(self.world.modules.chain().len())
            .map_err(|_| self.no_memory())?;
        order.extend_from_slice(self.world.modules.chain());
        self.first_in(&order, typed)
    }

    /// The current filing system's command at `at`, run, then the secondary
    /// filing system's; none if neither has it.
    pub(crate) fn filing_command(&mut self, at: u32) -> Result<Option<Result<(), Fail>>, Fail> {
        let (typed, _) = self.read_until(at, LONGEST, is_end)?;
        for reason in [MODULE_BASE, SECONDARY_BASE] {
            let mut base = Regs::default();
            base.r[0] = reason;
            match self.perform(X | crate::swi::os::FS_CONTROL, &mut base) {
                Ok(()) => {}
                Err(Fail::Error(_)) => continue,
                Err(unwind) => return Err(unwind),
            }
            let (module, private) = (base.r[1], base.r[2]);
            if module == 0 {
                if reason == MODULE_BASE {
                    return Ok(None);
                }
                continue;
            }
            let key = self.world.modules.chain().iter().copied().find(|&key| {
                self.world
                    .modules
                    .get(key)
                    .is_some_and(|m| m.shape.code == module)
            });
            let size = match key.and_then(|key| self.world.modules.get(key)) {
                Some(m) => m.shape.header.size(),
                None => self.read_word(module.wrapping_sub(4))?,
            };
            let table = self.read_word(module.wrapping_add(Field::Commands.offset()))?;
            let found = self.walk_table_at(key, module, size, table, |_, item, name| {
                Ok(named(item, name, &typed, Wanted::Filing, false))
            })?;
            if let Some((item, end)) = found {
                return Ok(Some(self.run_item(
                    &item,
                    at.wrapping_add(len32(end)),
                    private,
                )));
            }
        }
        Ok(None)
    }

    /// The command `item` runs, its tail at `tail`: counted, GSTrans'd as its
    /// table asks, held to the number its table allows, and called with R12
    /// at `private`.
    pub(crate) fn run_item(&mut self, item: &Item, tail: u32, private: u32) -> Result<(), Fail> {
        let mut tail = tail;
        while self.read_byte(tail)? == b' ' {
            tail = tail.wrapping_add(1);
        }
        let (text, _) = self.read_until(tail, LONGEST, is_end)?;
        let words = parameters(&text);
        let map = (item.info >> 8) & 0xFF;
        let (address, held) = if map == 0 {
            (tail, None)
        } else {
            let (buffer, number) = self.cli_buffer();
            match self.translate_parameters(tail, &text, &words, map, buffer) {
                Ok(()) => (buffer, Some(number)),
                Err(fail) => {
                    self.world.cli.release(number);
                    return Err(fail);
                }
            }
        };
        let count = len32(words.len());
        let (min, max) = (item.info & 0xFF, (item.info >> 16) & 0xFF);
        let result = if count < min || count > max {
            Err(self.syntax_error(item, address))
        } else {
            self.call_item(item, address, count, private)
        };
        if let Some(number) = held {
            self.world.cli.release(number);
        }
        result
    }

    /// The private word of `key`'s preferred incarnation, as its code is
    /// given it; zero for an item no module holds.
    pub(crate) fn private_of(&self, key: Option<ModuleKey>) -> u32 {
        key.and_then(|key| self.world.modules.get(key))
            .and_then(|m| m.incarnations.first().copied())
            .map_or(0, crate::module::Incarnation::private)
    }

    /// The built-in command `item` is, if a ROM module started from the
    /// kernel's own holds it.
    fn builtin_command(&self, item: &Item) -> Option<&'static Command> {
        let rom = item
            .key
            .and_then(|key| self.world.modules.get(key))?
            .shape
            .rom?;
        self.world.modules.builtin(rom)?.commands.get(item.index)
    }

    /// Calls the code at `offset` in `item`'s module with `regs`, as its
    /// module's profile runs it, in Supervisor mode with interrupts enabled.
    fn call_module_code(&mut self, item: &Item, offset: u32, regs: &mut Regs) -> Result<(), Fail> {
        let bits = item
            .key
            .and_then(|key| self.world.modules.get(key))
            .map_or_else(|| self.world.processor.bits(), |m| m.shape.bits);
        let target = Target {
            address: item.base.wrapping_add(offset),
            bits,
        };
        self.call(target, regs, CallEntry::SupervisorIrq)?;
        if regs.flag(tairix_riscos_arm::psr::V) {
            return Err(Fail::Error(regs.r[0]));
        }
        Ok(())
    }

    /// Calls `item`'s code with `regs`: a built-in's Rust, or the module's
    /// code.
    pub(crate) fn call_code(&mut self, item: &Item, regs: &mut Regs) -> Result<(), Fail> {
        if let Some(run) = self.builtin_command(item).and_then(|command| command.run) {
            return run(self, regs);
        }
        self.call_module_code(item, item.code, regs)
    }

    /// Calls `item`'s code as a command with its tail at `tail`, `count`
    /// parameters, and R12 at `private`.
    fn call_item(&mut self, item: &Item, tail: u32, count: u32, private: u32) -> Result<(), Fail> {
        let mut regs = Regs::default();
        regs.r[..2].copy_from_slice(&[tail, count]);
        regs.r[12] = private;
        self.call_code(item, &mut regs)
    }

    /// Calls `item`'s help code with a buffer of `size` bytes at `buffer`:
    /// the text it gives, zero for none.
    pub(crate) fn call_help(&mut self, item: &Item, buffer: u32, size: u32) -> Result<u32, Fail> {
        let mut regs = Regs::default();
        regs.r[..3].copy_from_slice(&[buffer, size, item.base]);
        regs.r[12] = self.private_of(item.key);
        match self.builtin_command(item).map(|command| &command.help) {
            Some(&Help::Code(code)) => code(self, &mut regs)?,
            _ => self.call_module_code(item, item.help, &mut regs)?,
        }
        Ok(regs.r[0])
    }

    /// The tail at `tail` rebuilt into `buffer` with the parameters `map`
    /// names GSTrans'd: each parameter and a space, then the terminator.
    fn translate_parameters(
        &mut self,
        tail: u32,
        text: &[u8],
        words: &[(usize, usize)],
        map: u32,
        buffer: u32,
    ) -> Result<(), Fail> {
        let mut written = 0;
        for (n, &(start, end)) in words.iter().enumerate() {
            if n < 8 && map & (1 << n) != 0 {
                let room = BUFFER.saturating_sub(written + 2);
                let mut translate = Regs::default();
                translate.r[..3].copy_from_slice(&[
                    tail.wrapping_add(len32(start)),
                    buffer.wrapping_add(written),
                    room | tairix_obey::gstrans::Flags::SPACE_ENDS.bits(),
                ]);
                match self.perform(X | crate::swi::os::GS_TRANS, &mut translate) {
                    Ok(()) if translate.flag(tairix_riscos_arm::psr::C) => {
                        return Err(self.error(LONG_LINE))
                    }
                    Ok(()) => {}
                    Err(Fail::Error(_)) => return Err(self.error(BAD_PARAMETER)),
                    Err(unwind) => return Err(unwind),
                }
                let length = translate.r[2];
                let (out, _) =
                    self.read_until(buffer.wrapping_add(written), length as usize, |_| false)?;
                if out.is_empty() || out.iter().any(|&b| b <= b' ' || b == 0x7F) {
                    return Err(self.error(BAD_PARAMETER));
                }
                written += length;
            } else {
                let word = &text[start..end];
                if written + len32(word.len()) + 2 > BUFFER {
                    return Err(self.error(LONG_LINE));
                }
                self.write_bytes(buffer.wrapping_add(written), word)?;
                written += len32(word.len());
            }
            self.write_byte(buffer.wrapping_add(written), b' ')?;
            written += 1;
        }
        let terminator = self.read_byte(tail.wrapping_add(len32(text.len())))?;
        self.write_byte(buffer.wrapping_add(written), terminator)
    }

    /// The error a command given the wrong number of parameters gives: a
    /// module's answer to `Service_SyntaxError`, or its syntax message.
    pub(crate) fn syntax_error(&mut self, item: &Item, tail: u32) -> Fail {
        let mut regs = Regs::default();
        regs.r[..5].copy_from_slice(&[tail, SYNTAX_SERVICE, item.words, item.base, item.name]);
        match self.service(&mut regs) {
            Err(unwind) => return Fail::Unwind(unwind),
            Ok(()) if regs.r[1] == 0 => return Fail::Error(regs.r[0]),
            Ok(()) => {}
        }
        match self.syntax_text(item) {
            Ok(text) => self.raw_error(crate::error::SYNTAX, &text),
            Err(fail) => fail,
        }
    }

    /// `item`'s syntax message: its own string, its name for an `ESC 0` in
    /// it; or, international or absent, its message looked up.
    pub(crate) fn syntax_text(&mut self, item: &Item) -> Result<Vec<u8>, Fail> {
        let (name, _) = self.read_until(item.name, NAME_MOST, |b| b == 0)?;
        if item.syntax == 0 || item.info & INTERNATIONAL != 0 {
            let token = if item.syntax == 0 {
                tairix_util::fallible::copied(NO_SYNTAX).ok_or_else(|| self.no_memory())?
            } else {
                self.read_until(item.base.wrapping_add(item.syntax), NAME_MOST, |b| b == 0)?
                    .0
            };
            return self.looked_up(item, &token);
        }
        let (syntax, _) = self.read_until(
            item.base.wrapping_add(item.syntax),
            crate::error::TEXT,
            |b| b == 0,
        )?;
        let mut text = Vec::new();
        let mut bytes = syntax.iter();
        while let Some(&b) = bytes.next() {
            if b == 0x1B {
                // The kernel's own dictionary is empty, so only the name
                // expands.
                if bytes.next() == Some(&0) {
                    text.try_reserve(name.len()).map_err(|_| self.no_memory())?;
                    text.extend_from_slice(&name);
                }
                continue;
            }
            text.try_reserve(1).map_err(|_| self.no_memory())?;
            text.push(b);
        }
        Ok(text)
    }

    /// The message `token` from `item`'s module's message file, through
    /// `MessageTrans`, or from the global messages where the module has no
    /// file to open; the text of `MessageTrans`'s error if it has none.
    pub(crate) fn looked_up(&mut self, item: &Item, token: &[u8]) -> Result<Vec<u8>, Fail> {
        let (buffer, number) = self.cli_buffer();
        let result = self.looked_up_in(item, token, buffer);
        self.world.cli.release(number);
        result
    }

    /// [`Os::looked_up`], with `buffer` for the token and the file's
    /// descriptor.
    fn looked_up_in(&mut self, item: &Item, token: &[u8], buffer: u32) -> Result<Vec<u8>, Fail> {
        let descriptor = buffer.wrapping_add(BUFFER - 16);
        self.write_terminated(buffer, token, 0, NAME_MOST)?;
        let size = self.read_word(item.base.wrapping_sub(4))?;
        let file = self.read_word(item.base.wrapping_add(Field::Messages.offset()))?;
        let mut opened = false;
        if file != 0 && file.is_multiple_of(4) && file < size {
            let mut open = Regs::default();
            open.r[..3].copy_from_slice(&[descriptor, item.base.wrapping_add(file), 0]);
            opened = match self.perform(X | OPEN_FILE, &mut open) {
                Ok(()) => true,
                Err(Fail::Error(_)) => false,
                Err(unwind) => return Err(unwind),
            };
        }
        let mut lookup = Regs::default();
        lookup.r[..4].copy_from_slice(&[if opened { descriptor } else { 0 }, buffer, 0, 0]);
        let found = self.perform(X | LOOKUP, &mut lookup);
        if opened {
            let mut close = Regs::default();
            close.r[0] = descriptor;
            if let Err(Fail::Unwind(unwind)) = self.perform(X | CLOSE_FILE, &mut close) {
                return Err(Fail::Unwind(unwind));
            }
        }
        match found {
            Ok(()) => {
                let length = lookup.r[3] as usize;
                let mut text = alloc::vec![0; length.min(crate::error::TEXT)];
                self.read_bytes(lookup.r[2], &mut text)?;
                Ok(text)
            }
            Err(Fail::Error(block)) => {
                let (text, _) =
                    self.read_until(block.wrapping_add(4), crate::error::TEXT, |b| b == 0)?;
                Ok(text)
            }
            Err(unwind) => Err(unwind),
        }
    }
}

/// The parameters of `tail`: each one's start and end, spaces between them,
/// one starting with `"` running to its closing `"` and spaces in it kept.
pub(crate) fn parameters(tail: &[u8]) -> Vec<(usize, usize)> {
    let mut words = Vec::new();
    let mut at = 0;
    while at < tail.len() {
        if tail[at] == b' ' {
            at += 1;
            continue;
        }
        let start = at;
        let mut quoted = tail[at] == b'"';
        let toggles = quoted;
        at += 1;
        while at < tail.len() {
            match tail[at] {
                b'"' if toggles => quoted = !quoted,
                b' ' if !quoted => break,
                _ => {}
            }
            at += 1;
        }
        if words.try_reserve(1).is_err() {
            break;
        }
        words.push((start, at));
    }
    words
}

/// Whether `item`, named `name`, names the command `typed` as `wanted`, with
/// code to run: where its tail starts then.
fn named(
    item: &Item,
    name: &[u8],
    typed: &[u8],
    wanted: Wanted,
    messy: bool,
) -> Option<(Item, usize)> {
    let fits = item.code != 0 && item.info & KIND == wanted.flags();
    fits.then(|| matches(typed, name, messy))
        .flatten()
        .map(|end| (*item, end))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_matches_whole_abbreviated_or_messily() {
        assert_eq!(matches(b"echo hello", b"Echo", false), Some(4));
        assert_eq!(matches(b"Echo\"x\"", b"Echo", false), Some(4));
        assert_eq!(matches(b"Ec. x", b"Echo", false), Some(3));
        assert_eq!(matches(b"Echo. x", b"Echo", false), None);
        assert_eq!(matches(b"Echoes", b"Echo", false), None);
        assert_eq!(matches(b"FX0", b"FX", true), Some(2));
        assert_eq!(matches(b"FX0", b"FX", false), None);
        assert_eq!(matches(b"FXA", b"FX", true), None);
    }

    #[test]
    fn parameters_split_at_spaces_but_inside_quotes() {
        let tail = b"a  \"b c\" d\"e f";
        let words: Vec<&[u8]> = parameters(tail).iter().map(|&(s, e)| &tail[s..e]).collect();
        assert_eq!(words, [&b"a"[..], b"\"b c\"", b"d\"e", b"f"]);
        assert_eq!(parameters(b"   ").len(), 0);
        assert_eq!(parameters(b"\"unended quote").len(), 1);
    }
}
