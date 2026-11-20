//! Variables in the heap, laid out as BASIC V lays them.
//!
//! Each name's chain hangs from the head its first character indexes. An
//! entry is the next entry's address, the name less its first character, a
//! zero, then the value: an integer or array pointer on the next word, a real
//! or string information block at once (BASIC VI's real on the next word).
//! Entries are appended at the variables' end and linked last in their chain.
//!
//! A string's block holds its length rounded up to a word; a string that
//! needs a different block frees its old one to the free list of that size
//! and takes one from the new size's list or the variables' end, but a block
//! at the end is resized where it lies.

use super::lvalue::{Element, Lvalue, Place};
use super::{len32, Machine, Step, MARGIN, NAME_MOST};
use crate::messages::Error;
use crate::value::{Dialect, Text};
use crate::workspace::{self as ws, FIRST_HEAD, LAST_HEAD};

/// What a name's suffix makes it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Type {
    /// No suffix.
    Real,
    /// `%`.
    Int,
    /// `$`.
    Str,
}

impl Type {
    /// An array of these's elements.
    pub(crate) const fn element(self) -> Element {
        match self {
            Self::Real => Element::Real,
            Self::Int => Element::Int,
            Self::Str => Element::Str,
        }
    }
}

/// A variable's name as the program spells it: its characters, its suffix,
/// and a bracket after them where it names an array.
#[derive(Clone, Copy)]
pub(crate) struct Name {
    len: u8,
    bytes: [u8; NAME_MOST],
}

impl Name {
    /// Its bytes.
    pub(crate) fn as_bytes(&self) -> &[u8] {
        &self.bytes[..usize::from(self.len)]
    }

    /// Its first character, which picks its chain.
    pub(crate) fn first(&self) -> u8 {
        self.bytes[0]
    }

    /// The rest, as an entry keeps it.
    pub(crate) fn rest(&self) -> &[u8] {
        &self.as_bytes()[1..]
    }

    /// Whether it names an array.
    pub(crate) fn is_array(&self) -> bool {
        self.as_bytes().last() == Some(&b'(')
    }

    /// Its type.
    pub(crate) fn kind(&self) -> Type {
        let bytes = self.as_bytes();
        let end = if self.is_array() {
            bytes.len() - 1
        } else {
            bytes.len()
        };
        match bytes.get(end.wrapping_sub(1)) {
            Some(b'%') => Type::Int,
            Some(b'$') => Type::Str,
            _ => Type::Real,
        }
    }

    /// The name of `bytes`.
    pub(crate) fn of(bytes: &[u8]) -> Option<Self> {
        let mut name = Self {
            len: 0,
            bytes: [0; NAME_MOST],
        };
        name.bytes.get_mut(..bytes.len())?.copy_from_slice(bytes);
        name.len = u8::try_from(bytes.len()).ok()?;
        (!bytes.is_empty()).then_some(name)
    }
}

/// Whether `b` may begin a name.
pub(crate) const fn starts_name(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_' || b == b'`'
}

/// Whether `b` may go on a name.
pub(crate) const fn in_name(b: u8) -> bool {
    starts_name(b) || b.is_ascii_digit()
}

/// `n` rounded up to a word.
const fn align(n: u32) -> u32 {
    n.next_multiple_of(4)
}

/// The block a string of `length` bytes takes: none for an empty string.
pub(crate) const fn capacity(length: u32) -> u32 {
    align(length)
}

/// Where an entry's value lies, past its link, its name's rest of `rest`
/// bytes and the name's zero.
const fn value_offset(rest: u32, place: Place, dialect: Dialect) -> u32 {
    let after = 4 + rest + 1;
    match (place, dialect) {
        (Place::Real, Dialect::V) | (Place::Sib, _) => after,
        _ => align(after),
    }
}

/// What looking a name up found.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Found {
    /// The entry.
    Entry(u32),
    /// No entry: the word a new one is linked from — the head, or the last
    /// entry's link.
    Missing(u32),
}

impl Machine<'_, '_> {
    /// The name at the pointer, which moves past it: a name's characters, a
    /// `%` or `$`, and a `(` where one follows; none where no name starts.
    pub(crate) fn read_name(&mut self) -> Step<Option<Name>> {
        let start = self.ptr;
        if !starts_name(self.peek()?) && self.peek()? != b'@' {
            return Ok(None);
        }
        let mut name = Name {
            len: 0,
            bytes: [0; NAME_MOST],
        };
        let push = |name: &mut Name, b: u8| -> Step {
            let slot = name
                .bytes
                .get_mut(usize::from(name.len))
                .ok_or(Error::LineTooLong)?;
            *slot = b;
            name.len += 1;
            Ok(())
        };
        let first = self.next()?;
        push(&mut name, first)?;
        if first == b'@' {
            if self.peek()? != b'%' {
                self.ptr = start;
                return Ok(None);
            }
        } else {
            while in_name(self.peek()?) {
                let b = self.next()?;
                push(&mut name, b)?;
            }
        }
        if matches!(self.peek()?, b'%' | b'$') {
            let b = self.next()?;
            push(&mut name, b)?;
        }
        if self.peek()? == b'(' {
            self.ptr += 1;
            push(&mut name, b'(')?;
        }
        Ok(Some(name))
    }

    /// The workspace word a resident integer is, for `@%` and `A%` to `Z%`.
    fn resident(name: &Name) -> Option<u32> {
        match name.as_bytes() {
            [b'@', b'%'] => Some(ws::AT_PERCENT.at()),
            [c @ b'A'..=b'Z', b'%'] => Some(ws::RESIDENT.at() + 4 * u32::from(c - b'A')),
            _ => None,
        }
    }

    /// The head word of `first`'s chain.
    pub(crate) fn head(first: u8) -> Step<u32> {
        if !(FIRST_HEAD..=LAST_HEAD).contains(&first) {
            return Err(Error::NoSuchVariable.into());
        }
        Ok(ws::HEADS.at() + 4 * u32::from(first))
    }

    /// The entry `rest` names in the chain at `head`, or where a new one is
    /// linked. The walk stops at an entry outside the variables, as a chain
    /// the program overwrote would lead it, and at the most entries the
    /// variables hold.
    pub(crate) fn find(&mut self, head: u32, rest: &[u8]) -> Step<Found> {
        let mut link = head;
        let most = self.fsa.saturating_sub(self.lomem) / 8 + 1;
        for _ in 0..most {
            let entry = self.os.read_word(link)?;
            let inside = entry >= self.lomem && entry < self.fsa && entry % 4 == 0;
            if entry == 0 || !inside {
                return Ok(Found::Missing(link));
            }
            if self.name_is(entry.wrapping_add(4), rest)? {
                return Ok(Found::Entry(entry));
            }
            link = entry;
        }
        Ok(Found::Missing(link))
    }

    /// Whether the name kept at `at` is `rest`, its zero after it.
    fn name_is(&mut self, at: u32, rest: &[u8]) -> Step<bool> {
        let mut bytes = [0; NAME_MOST + 1];
        let n = rest.len() + 1;
        self.os.read_bytes(at, &mut bytes[..n])?;
        Ok(&bytes[..rest.len()] == rest && bytes[rest.len()] == 0)
    }

    /// `bytes` taken from the variables' end, or `error`.
    pub(crate) fn allocate(&mut self, bytes: u32, error: Error) -> Step<u32> {
        let at = self.fsa;
        let end = at
            .checked_add(bytes)
            .filter(|&end| end.saturating_add(MARGIN) <= self.sp)
            .ok_or(error)?;
        self.fsa = end;
        self.os.write_word(ws::FSA.at(), end)?;
        Ok(at)
    }

    /// A new entry for the name of `rest` in its chain, linked from `link`,
    /// its value of `place` zero: the value's address.
    pub(crate) fn create(&mut self, link: u32, rest: &[u8], place: Place) -> Step<u32> {
        let dialect = self.dialect();
        let offset = value_offset(len32(rest.len()), place, dialect);
        let size = align(offset + place.bytes(dialect));
        let entry = self.allocate(size, Error::NoRoom)?;
        self.os.fill(entry, size, 0)?;
        self.os.write_bytes(entry + 4, rest)?;
        self.os.write_word(link, entry)?;
        Ok(entry + offset)
    }

    /// The value address of the entry at `entry` for the name of `rest`.
    pub(crate) fn value_of(&self, entry: u32, rest: &[u8], place: Place) -> u32 {
        entry + value_offset(len32(rest.len()), place, self.dialect())
    }

    /// The l-value of the variable `name`, made where `create` asks and it
    /// is missing; none where it is missing and not made. An array's is its
    /// pointer word's.
    pub(crate) fn variable(&mut self, name: &Name, create: bool) -> Step<Option<Lvalue>> {
        if let Some(address) = Self::resident(name) {
            return Ok(Some(Lvalue {
                address,
                place: Place::Int,
            }));
        }
        let place = if name.is_array() {
            Place::Array(name.kind().element())
        } else {
            match name.kind() {
                Type::Int => Place::Int,
                Type::Real => Place::Real,
                Type::Str => Place::Sib,
            }
        };
        let head = Self::head(name.first())?;
        let address = match self.find(head, name.rest())? {
            Found::Entry(entry) => self.value_of(entry, name.rest(), place),
            Found::Missing(_) if !create => return Ok(None),
            Found::Missing(link) => self.create(link, name.rest(), place)?,
        };
        Ok(Some(Lvalue { address, place }))
    }

    // Strings.

    /// `text` assigned to the string whose information block is at `sib`.
    pub(crate) fn assign_sib(&mut self, sib: u32, text: &Text) -> Step {
        let old = self.os.read_word(sib)?;
        let old_length = u32::from(self.os.read_byte(sib.wrapping_add(4))?);
        let old_cap = if old == 0 { 0 } else { capacity(old_length) };
        let length = u32::from(text.len());
        let cap = capacity(length);
        let block = if cap == old_cap {
            old
        } else if old_cap > 0 && old.checked_add(old_cap) == Some(self.fsa) {
            self.resize_last(old, cap)?
        } else {
            if old_cap > 0 {
                self.free_block(old, old_cap)?;
            }
            if cap == 0 {
                0
            } else {
                self.string_block(cap)?
            }
        };
        if cap > 0 {
            let room = &text.room()[..cap.min(len32(text.room().len())) as usize];
            self.os.write_bytes(block, room)?;
        }
        self.os.write_word(sib, if cap == 0 { 0 } else { block })?;
        self.os.write_byte(sib.wrapping_add(4), text.len())?;
        Ok(())
    }

    /// The block at the variables' end at `block` made `cap` bytes where it
    /// lies.
    fn resize_last(&mut self, block: u32, cap: u32) -> Step<u32> {
        let end = block
            .checked_add(cap)
            .filter(|&end| end.saturating_add(MARGIN) <= self.sp)
            .ok_or(Error::NoRoom)?;
        self.fsa = end;
        self.os.write_word(ws::FSA.at(), end)?;
        Ok(if cap == 0 { 0 } else { block })
    }

    /// The head word of the free list for blocks of `cap` bytes.
    fn free_list(cap: u32) -> Step<u32> {
        let index = (cap / 4)
            .checked_sub(1)
            .filter(|&i| i < ws::FREE_LIST_COUNT)
            .ok_or(Error::StringTooLong)?;
        Ok(ws::FREE_LISTS.at() + 4 * index)
    }

    /// The block at `block` of `cap` bytes put on its free list.
    fn free_block(&mut self, block: u32, cap: u32) -> Step {
        let list = Self::free_list(cap)?;
        let next = self.os.read_word(list)?;
        self.os.write_word(block, next)?;
        Ok(self.os.write_word(list, block)?)
    }

    /// A block of `cap` bytes: its free list's first, or one from the
    /// variables' end. A free block outside the variables, as a list the
    /// program overwrote would hold, is left where it is.
    fn string_block(&mut self, cap: u32) -> Step<u32> {
        let list = Self::free_list(cap)?;
        let block = self.os.read_word(list)?;
        let inside = block >= self.lomem && block.checked_add(cap).is_some_and(|e| e <= self.fsa);
        if block != 0 && inside && block % 4 == 0 {
            let next = self.os.read_word(block)?;
            self.os.write_word(list, next)?;
            return Ok(block);
        }
        self.allocate(cap, Error::NoRoom)
    }
}
