//! System variables (`plans/RISCOS-EMULATOR.md` §4.4): each a node in the
//! system heap — its name, a zero, its type and its value — found through an
//! index the kernel keeps in name order.
//!
//! A code variable's value is guest code, called to read or write it. The
//! expansions — GSTrans, expressions, `OS_SetVarVal`'s types — are
//! `lib/obey`'s, run against these variables.

use alloc::vec::Vec;

use crate::convert::Decimal;
use tairix_obey::gstrans::{gstrans, Flags};
use tairix_obey::name;
use tairix_obey::vars::{self as obey_vars, Kind, Request, Value, Variables};
use tairix_obey::Budget;
use tairix_riscos_arm::psr;

use crate::error::{obey_error, Error};
use crate::memory::Bits;
use crate::os::{len32, Entry as CallEntry, Fail, Os, Regs, Target};

/// The variable references one reading may expand: generous for a guest's
/// own text, and a bound on the work a hostile one can ask for.
pub(crate) const BUDGET: usize = 4096;
/// The longest value a variable holds: past what the system heap could,
/// which bounds the host's buffer for one.
const LONGEST: usize = 1 << 20;
/// The longest name.
pub(crate) const LONGEST_NAME: usize = 255;

/// The types `OS_ReadVarVal` and `OS_SetVarVal` name, a node's among them.
pub(crate) const STRING: u8 = 0;
pub(crate) const NUMBER: u8 = 1;
pub(crate) const MACRO: u8 = 2;
pub(crate) const EXPANDED: u8 = 3;
pub(crate) const LITERAL: u8 = 4;
pub(crate) const CODE: u8 = 16;
/// A string or macro value's length: three bytes, little-endian, before
/// its text.
const LENGTH_FIELD: u32 = 3;

/// "Variable not found".
pub(crate) const NOT_FOUND: Error = Error {
    number: 0x124,
    token: "VariableNotFound",
};
/// "Bad variable type".
const BAD_TYPE: Error = Error {
    number: 0x122,
    token: "BadVariableType",
};
/// "Variable value too long": a code variable's refusal.
pub(crate) const TOO_LONG: Error = Error {
    number: 0x125,
    token: "VariableValueTooLong",
};
/// "Bad name".
const BAD_NAME: Error = obey_error(tairix_obey::Error::BadVariableName);
/// "No room for variable".
const NO_ROOM: Error = obey_error(tairix_obey::Error::NoRoomForVariable);

/// An index entry: a variable's name, its node, and the profile of the code
/// that set it, which a code variable's code runs under.
#[derive(Clone, Debug)]
struct Entry {
    name: Vec<u8>,
    node: u32,
    bits: Bits,
}

/// The index: every variable, in name order.
pub(crate) struct Index {
    entries: Vec<Entry>,
    /// Where the kernel's code variables make their values.
    pub(crate) buffer: u32,
}

impl Index {
    pub(crate) const fn new() -> Self {
        Self {
            entries: Vec::new(),
            buffer: 0,
        }
    }

    /// Where `name` is, or would go.
    fn search(&self, name: &[u8]) -> Result<usize, usize> {
        self.entries
            .binary_search_by(|entry| name::compare(&entry.name, name))
    }

    /// The first entry `pattern` matches at or after `from`.
    fn first_match(&self, pattern: &[u8], from: usize) -> Option<usize> {
        if !name::is_wild(pattern) {
            return self.search(pattern).ok().filter(|&at| at >= from);
        }
        (from..self.entries.len()).find(|&at| name::matches(pattern, &self.entries[at].name))
    }

    /// The entry whose node is `node`.
    fn position_of(&self, node: u32) -> Option<usize> {
        self.entries.iter().position(|entry| entry.node == node)
    }

    /// The name of the variable called `name`, any wildcard in `name`
    /// standing for itself.
    pub(crate) fn exact(&self, name: &[u8]) -> Option<Vec<u8>> {
        let at = self.search(name).ok()?;
        tairix_util::fallible::copied(&self.entries[at].name)
    }

    /// The names of the variables that begin with `prefix`, in order.
    pub(crate) fn named_from<'a>(
        &'a self,
        prefix: &'a [u8],
    ) -> impl Iterator<Item = &'a [u8]> + 'a {
        let first = self
            .entries
            .partition_point(|entry| name::compare(&entry.name, prefix).is_lt());
        self.entries[first..]
            .iter()
            .map(|entry| entry.name.as_slice())
            .take_while(move |name| {
                name.len() >= prefix.len() && name::compare(&name[..prefix.len()], prefix).is_eq()
            })
    }
}

/// A node as read back: its type, and where its value starts.
#[derive(Clone, Copy, Debug)]
struct Node {
    kind: u8,
    value: u32,
}

/// The variables as `lib/obey` reads and writes them, over the kernel; a
/// code variable's error, or an unwind, kept for the SWI to give.
struct Store<'o, 'k> {
    os: &'o mut Os<'k>,
    /// Where the next lookup starts: past a context's entry.
    from: usize,
    failed: Option<Fail>,
}

impl Store<'_, '_> {
    /// Keeps `fail`, standing in for it one of `lib/obey`'s errors that
    /// every reading passes on.
    fn keep(&mut self, fail: Fail) -> tairix_obey::Error {
        self.failed.get_or_insert(fail);
        tairix_obey::Error::SystemHeapFull
    }

    /// The first variable `pattern` matches from `from` that is not a code
    /// variable, which only its own type deletes: its index and node.
    fn deletable(&mut self, pattern: &[u8], mut from: usize) -> Option<(usize, u32)> {
        while let Some(at) = self.os.world.vars.first_match(pattern, from) {
            let node = self.os.world.vars.entries[at].node;
            if self.os.node(node).is_none_or(|n| n.kind != CODE) {
                return Some((at, node));
            }
            from = at + 1;
        }
        None
    }

    /// Deletes entry `at`, whose node is `node`.
    fn remove(&mut self, at: usize, node: u32) {
        self.os.world.vars.entries.remove(at);
        self.os.free_system_node(node);
    }
}

impl Variables for Store<'_, '_> {
    fn lookup(&mut self, pattern: &[u8]) -> Result<Option<Value>, tairix_obey::Error> {
        let Some(at) = self.os.world.vars.first_match(pattern, self.from) else {
            return Ok(None);
        };
        let entry = self.os.world.vars.entries[at].clone();
        self.os
            .value_of(&entry)
            .map(Some)
            .map_err(|fail| self.keep(fail))
    }

    fn assign(&mut self, pattern: &[u8], value: Value) -> Result<(), tairix_obey::Error> {
        let from = self.from;
        self.os
            .assign_variable(pattern, value, from)
            .map_err(|fail| self.keep(fail))
    }

    fn delete(&mut self, pattern: &[u8]) -> Result<bool, tairix_obey::Error> {
        let Some((at, node)) = self.deletable(pattern, 0) else {
            return Ok(false);
        };
        self.remove(at, node);
        Ok(true)
    }

    fn unset(&mut self, pattern: &[u8]) -> Result<usize, tairix_obey::Error> {
        let mut deleted = 0;
        let mut from = 0;
        while let Some((at, node)) = self.deletable(pattern, from) {
            self.remove(at, node);
            deleted += 1;
            from = at;
        }
        Ok(deleted)
    }
}

impl Os<'_> {
    /// The node at `at`, its name read past and its type checked; none if
    /// the guest has overwritten it past reading.
    fn node(&mut self, at: u32) -> Option<Node> {
        let mut offset = 0;
        while self.peek_byte(at + offset)? != 0 {
            offset += 1;
            if offset as usize > LONGEST_NAME {
                return None;
            }
        }
        let kind = self.peek_byte(at + offset + 1)?;
        let value = at + offset + 2;
        match kind {
            STRING | NUMBER | MACRO => Some(Node { kind, value }),
            CODE => Some(Node {
                kind,
                value: (value + 3) & !3,
            }),
            _ => None,
        }
    }

    /// The bytes of a string or macro node's value.
    fn stored_text(&mut self, node: Node) -> Result<Vec<u8>, Fail> {
        let mut length = [0; 4];
        self.read_bytes(node.value, &mut length[..LENGTH_FIELD as usize])?;
        let length = u32::from_le_bytes(length) as usize;
        if length > LONGEST {
            return Err(self.error(NO_ROOM));
        }
        let mut text = Vec::new();
        if text.try_reserve_exact(length).is_err() {
            return Err(self.no_memory());
        }
        text.resize(length, 0);
        self.read_bytes(node.value + LENGTH_FIELD, &mut text)?;
        Ok(text)
    }

    /// A variable's value as the expansions read it; a code variable's read
    /// entry is called for it.
    fn value_of(&mut self, entry: &Entry) -> Result<Value, Fail> {
        let Some(node) = self.node(entry.node) else {
            return Ok(Value::String(Vec::new()));
        };
        Ok(match node.kind {
            NUMBER => Value::Number(self.read_word(node.value)?.cast_signed()),
            MACRO => Value::Macro(self.stored_text(node)?),
            CODE => Value::String(self.read_code_variable(entry, node, Regs::default())?),
            _ => Value::String(self.stored_text(node)?),
        })
    }

    /// Calls a code variable's read entry with `regs`, R3 its name: its
    /// value's bytes.
    fn read_code_variable(
        &mut self,
        entry: &Entry,
        node: Node,
        mut regs: Regs,
    ) -> Result<Vec<u8>, Fail> {
        regs.r[3] = entry.node;
        let target = Target {
            address: node.value + 4,
            bits: entry.bits,
        };
        self.call(target, &mut regs, CallEntry::SupervisorIrq)?;
        if regs.flag(psr::V) {
            return Err(Fail::Error(regs.r[0]));
        }
        let length = regs.r[2] as usize;
        if length > LONGEST {
            return Err(self.buffer_overflow());
        }
        let mut value = Vec::new();
        if value.try_reserve_exact(length).is_err() {
            return Err(self.no_memory());
        }
        value.resize(length, 0);
        self.read_bytes(regs.r[0], &mut value)?;
        Ok(value)
    }

    /// Calls a code variable's write entry with `text`, R3 its name.
    fn write_code_variable(&mut self, entry: &Entry, node: Node, text: &[u8]) -> Result<(), Fail> {
        let length = len32(text.len());
        let buffer = self.system_node(length + 1)?;
        let result = self.write_bytes(buffer, text).and_then(|()| {
            let mut regs = Regs::new([
                entry.node, buffer, length, entry.node, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            ]);
            let target = Target {
                address: node.value,
                bits: entry.bits,
            };
            self.call(target, &mut regs, CallEntry::SupervisorIrq)?;
            if regs.flag(psr::V) {
                return Err(Fail::Error(regs.r[0]));
            }
            Ok(())
        });
        self.free_system_node(buffer);
        result
    }

    /// Writes a variable: `value` given to the first match of `pattern` from
    /// `from` (a code variable's through its write entry), or to a new
    /// variable called `pattern`.
    fn assign_variable(&mut self, pattern: &[u8], value: Value, from: usize) -> Result<(), Fail> {
        let existing = self
            .world
            .vars
            .first_match(pattern, from)
            .map(|at| self.world.vars.entries[at].clone());
        if let Some(entry) = &existing {
            if let Some(node) = self.node(entry.node).filter(|n| n.kind == CODE) {
                let text = match value {
                    Value::Number(n) => tairix_util::fallible::copied(Decimal::of(n).as_bytes())
                        .ok_or_else(|| self.no_memory())?,
                    Value::String(bytes) | Value::Macro(bytes) => bytes,
                };
                return self.write_code_variable(entry, node, &text);
            }
        }
        let number;
        let (kind, data): (u8, &[u8]) = match &value {
            Value::String(text) => (STRING, text),
            Value::Number(n) => {
                number = n.to_le_bytes();
                (NUMBER, &number)
            }
            Value::Macro(text) => (MACRO, text),
        };
        self.store_variable(pattern, existing, kind, data)
    }

    /// Stores a variable of `kind` holding `data`: under `existing`'s name,
    /// replacing it, or as a new one called `pattern`.
    fn store_variable(
        &mut self,
        pattern: &[u8],
        existing: Option<Entry>,
        kind: u8,
        data: &[u8],
    ) -> Result<(), Fail> {
        let name = match &existing {
            Some(entry) => entry.name.clone(),
            None if name::is_creatable(pattern) => {
                tairix_util::fallible::copied(pattern).ok_or_else(|| self.no_memory())?
            }
            None => return Err(self.error(BAD_NAME)),
        };
        let node = self.write_node(&name, kind, data)?;
        let bits = self.caller_bits();
        self.place_node(Entry { name, node, bits }, existing.map(|entry| entry.node))
    }

    /// A new node of `name`, `kind` and `data` in the system heap.
    fn write_node(&mut self, name: &[u8], kind: u8, data: &[u8]) -> Result<u32, Fail> {
        let header = len32(name.len()) + 2;
        let length = len32(data.len());
        let body = match kind {
            NUMBER => 4,
            MACRO => LENGTH_FIELD + length + 1,
            // At most three bytes' padding aligns the code to a word.
            CODE => 3 + length,
            _ => LENGTH_FIELD + length,
        };
        let node = match self.system_node(header + body) {
            Ok(node) => node,
            Err(Fail::Error(_)) => return Err(self.error(NO_ROOM)),
            Err(unwind) => return Err(unwind),
        };
        self.write_bytes(node, name)?;
        self.write_bytes(node + len32(name.len()), &[0, kind])?;
        let value = node + header;
        match kind {
            NUMBER => self.write_bytes(value, data)?,
            CODE => self.write_bytes((value + 3) & !3, data)?,
            _ => {
                self.write_bytes(value, &length.to_le_bytes()[..LENGTH_FIELD as usize])?;
                self.write_bytes(value + LENGTH_FIELD, data)?;
                if kind == MACRO {
                    self.write_byte(value + LENGTH_FIELD + length, 0)?;
                }
            }
        }
        Ok(node)
    }

    /// Enters `entry`, replacing the node `old`.
    fn place_node(&mut self, entry: Entry, old: Option<u32>) -> Result<(), Fail> {
        if let Some(at) = old.and_then(|old| self.world.vars.position_of(old)) {
            let replaced = core::mem::replace(&mut self.world.vars.entries[at], entry);
            self.free_system_node(replaced.node);
            return Ok(());
        }
        let at = match self.world.vars.search(&entry.name) {
            Ok(at) | Err(at) => at,
        };
        if self.world.vars.entries.try_reserve(1).is_err() {
            self.free_system_node(entry.node);
            return Err(self.error(NO_ROOM));
        }
        self.world.vars.entries.insert(at, entry);
        Ok(())
    }

    /// Runs `lib/obey` work over the variables from index position `from`:
    /// a code variable's error, or an unwind, wins over the error it stood
    /// in for.
    pub(crate) fn with_variables<T>(
        &mut self,
        from: usize,
        work: impl FnOnce(&mut dyn Variables, &mut Budget) -> Result<T, tairix_obey::Error>,
    ) -> Result<T, Fail> {
        let mut budget = Budget::new(BUDGET);
        let mut store = Store {
            os: self,
            from,
            failed: None,
        };
        let result = work(&mut store, &mut budget);
        match (store.failed.take(), result) {
            (Some(fail), _) => Err(fail),
            (None, Ok(value)) => Ok(value),
            (None, Err(error)) => Err(self.error(obey_error(error))),
        }
    }

    /// The index position past the entry a context names, or the start for
    /// a context that names none.
    fn after_context(&self, context: u32) -> usize {
        if context == 0 {
            return 0;
        }
        self.world.vars.position_of(context).map_or(0, |at| at + 1)
    }

    /// The variable name at `address`, as the variable SWIs read one: to a
    /// control character or a space.
    fn variable_name(&mut self, address: u32) -> Result<Vec<u8>, Fail> {
        match self.read_until(address, LONGEST_NAME, |b| b <= b' ')? {
            (name, true) => Ok(name),
            (_, false) => Err(self.buffer_overflow()),
        }
    }

    /// `OS_ReadVarVal`.
    pub(crate) fn read_var_val(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        let pattern = self.variable_name(regs.r[0])?;
        let from = self.after_context(regs.r[3]);
        let Some(at) = self.world.vars.first_match(&pattern, from) else {
            regs.r[2] = 0;
            return Err(self.error_with(NOT_FOUND, &[&pattern]));
        };
        let entry = self.world.vars.entries[at].clone();
        let expand = regs.r[4] == u32::from(EXPANDED);
        let (bytes, kind) = match self.node(entry.node) {
            None => (Vec::new(), STRING),
            Some(node) => match node.kind {
                CODE => (self.read_code_variable(&entry, node, *regs)?, STRING),
                NUMBER if expand => {
                    let n = self.read_word(node.value)?.cast_signed();
                    let text = tairix_util::fallible::copied(Decimal::of(n).as_bytes())
                        .ok_or_else(|| self.no_memory())?;
                    (text, STRING)
                }
                NUMBER => (self.read_word(node.value)?.to_le_bytes().to_vec(), NUMBER),
                MACRO if expand => return self.read_expanded_macro(regs, node, entry.node),
                MACRO => (self.stored_text(node)?, MACRO),
                _ => (self.stored_text(node)?, STRING),
            },
        };
        regs.r[3] = entry.node;
        regs.r[4] = u32::from(kind);
        self.answer_read(regs, &bytes)
    }

    /// Hands a read value to `OS_ReadVarVal`'s caller: copied into its
    /// buffer as far as it holds, or for a probe, its length's complement.
    fn answer_read(&mut self, regs: &mut Regs, bytes: &[u8]) -> Result<(), Fail> {
        let Ok(room) = usize::try_from(regs.r[2].cast_signed()) else {
            regs.r[2] = !len32(bytes.len());
            return Err(self.buffer_overflow());
        };
        if bytes.len() > room {
            self.write_bytes(regs.r[1], &bytes[..room])?;
            return Err(self.buffer_overflow());
        }
        self.write_bytes(regs.r[1], bytes)?;
        regs.r[2] = len32(bytes.len());
        Ok(())
    }

    /// A macro read expanded: GSTrans'd into the caller's buffer with its
    /// terminator, R4 left 2 as RISC OS 5 leaves it.
    fn read_expanded_macro(
        &mut self,
        regs: &mut Regs,
        node: Node,
        node_at: u32,
    ) -> Result<(), Fail> {
        let text = self.stored_text(node)?;
        regs.r[3] = node_at;
        regs.r[4] = u32::from(MACRO);
        let Ok(room) = usize::try_from(regs.r[2].cast_signed()) else {
            return Err(self.buffer_overflow());
        };
        let mut buffer = Vec::new();
        let room = room.min(LONGEST);
        if buffer.try_reserve_exact(room).is_err() {
            return Err(self.no_memory());
        }
        buffer.resize(room, 0);
        let translation = self.with_variables(0, |vars, budget| {
            Ok(gstrans(&text, Flags::NONE, vars, budget, &mut buffer))
        })?;
        let stored = translation.stored.min(buffer.len());
        self.write_bytes(regs.r[1], &buffer[..stored])?;
        match translation.ended {
            Ok(Some(end)) => {
                self.write_byte(regs.r[1].wrapping_add(len32(stored)), end)?;
                regs.r[2] = len32(stored);
                Ok(())
            }
            Ok(None) => Err(self.buffer_overflow()),
            Err(error) => Err(self.error(obey_error(error))),
        }
    }

    /// `OS_SetVarVal`.
    pub(crate) fn set_var_val(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        let name = self.variable_name(regs.r[0])?;
        let from = self.after_context(regs.r[3]);
        if regs.r[2].cast_signed() < 0 {
            return self.delete_variable(regs, &name, from);
        }
        let Ok(kind) = u8::try_from(regs.r[4]) else {
            return Err(self.error(BAD_TYPE));
        };
        let text = match kind {
            // To and including the first control character, which the
            // reading of each judges.
            STRING | MACRO | EXPANDED => self.text_and_end(regs.r[1])?,
            NUMBER => self.read_word(regs.r[1])?.to_le_bytes().to_vec(),
            LITERAL => self.exact_bytes(regs.r[1], regs.r[2])?,
            CODE => return self.set_code_variable(regs, &name, from),
            _ => return Err(self.error(BAD_TYPE)),
        };
        let stored = self.with_variables(from, |vars, budget| {
            let request = match kind {
                STRING => Request::String(&text),
                NUMBER => Request::Number(i32::from_le_bytes([text[0], text[1], text[2], text[3]])),
                MACRO => Request::Macro(&text),
                EXPANDED => Request::Expanded(&text),
                _ => Request::Literal(&text),
            };
            obey_vars::set(vars, &name, request, LONGEST, budget)
        })?;
        regs.r[4] = u32::from(match (kind, stored) {
            (EXPANDED, Kind::Number) => NUMBER,
            (EXPANDED, _) => LITERAL,
            (other, _) => other,
        });
        Ok(())
    }

    /// The bytes at `address` to and including the first control character.
    fn text_and_end(&mut self, address: u32) -> Result<Vec<u8>, Fail> {
        let (mut text, ended) = self.read_until(address, LONGEST, |b| b < b' ')?;
        if !ended {
            return Err(self.error(NO_ROOM));
        }
        let end = self.read_byte(address.wrapping_add(len32(text.len())))?;
        if text.try_reserve(1).is_err() {
            return Err(self.no_memory());
        }
        text.push(end);
        Ok(text)
    }

    /// Exactly `length` bytes at `address`.
    fn exact_bytes(&mut self, address: u32, length: u32) -> Result<Vec<u8>, Fail> {
        let length = length as usize;
        if length > LONGEST {
            return Err(self.error(NO_ROOM));
        }
        let mut bytes = Vec::new();
        if bytes.try_reserve_exact(length).is_err() {
            return Err(self.no_memory());
        }
        bytes.resize(length, 0);
        self.read_bytes(address, &mut bytes)?;
        Ok(bytes)
    }

    /// `OS_SetVarVal` deleting: the first match from the context; a code
    /// variable only by its own type. R3 the entry before it.
    fn delete_variable(&mut self, regs: &mut Regs, name: &[u8], from: usize) -> Result<(), Fail> {
        let Some(at) = self.world.vars.first_match(name, from) else {
            return Err(self.error_with(NOT_FOUND, &[name]));
        };
        let node = self.world.vars.entries[at].node;
        if self.node(node).is_some_and(|n| n.kind == CODE) && regs.r[4] != u32::from(CODE) {
            // Kept, with the context past it so that a walk deleting what a
            // wildcard matches goes on.
            regs.r[3] = node;
            return Ok(());
        }
        self.world.vars.entries.remove(at);
        self.free_system_node(node);
        regs.r[3] = at
            .checked_sub(1)
            .map_or(0, |before| self.world.vars.entries[before].node);
        Ok(())
    }

    /// `OS_SetVarVal` type 16: a code variable of R2 bytes of code from R1.
    fn set_code_variable(&mut self, regs: &mut Regs, name: &[u8], from: usize) -> Result<(), Fail> {
        let code = self.exact_bytes(regs.r[1], regs.r[2])?;
        let existing = self
            .world
            .vars
            .first_match(name, from)
            .map(|at| self.world.vars.entries[at].clone());
        self.store_variable(name, existing, CODE, &code)
    }

    /// Sets a string variable to `value` as it is, from the kernel's or a
    /// built-in module's own code.
    pub fn set_string_variable(&mut self, name: &[u8], value: &[u8]) -> Result<(), Fail> {
        let text = tairix_util::fallible::copied(value).ok_or_else(|| self.no_memory())?;
        self.assign_variable(name, Value::String(text), 0)
    }

    /// A variable's value read as a path variable is: a number in decimal, a
    /// macro expanded; none if no variable is called that.
    pub fn variable_text(&mut self, name: &[u8]) -> Result<Option<Vec<u8>>, Fail> {
        self.with_variables(0, |vars, budget| {
            obey_vars::read_expanded(vars, name, budget, LONGEST)
        })
    }
}

pub(crate) mod preset;

#[cfg(test)]
mod tests;
