//! The encoder: instructions written out as a module's words.
//!
//! It frames words and packs strings, and checks only what it must to write
//! them: the grammar is the decoder's to check, so whatever an encoder wrote
//! is decoded before anything relies on it, and there is one judge of what a
//! module may hold.

use alloc::vec::Vec;

use crate::decode::{HEADER_WORDS, MAGIC};
use crate::module::{Instruction, Module, Version};
use crate::{EncodeError, Id, Op, MAX_MODULE_BYTES};

/// One operand to write.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Arg<'a> {
    /// An id the encoder issued.
    Id(Id),
    /// One word: a literal or an enumerant's value.
    Word(u32),
    /// A literal two words wide, the low word first.
    Wide(u64),
    /// A string, terminated and padded as it is written.
    Text(&'a str),
}

impl Arg<'_> {
    const fn words(&self) -> usize {
        match self {
            Self::Id(_) | Self::Word(_) => 1,
            Self::Wide(_) => 2,
            Self::Text(text) => text.len() / 4 + 1,
        }
    }
}

/// What follows an instruction's arguments: more ids, or literal words.
#[derive(Copy, Clone, Debug)]
pub(crate) enum Trail<'a> {
    None,
    Ids(&'a [Id]),
    Words(&'a [u32]),
}

impl Trail<'_> {
    const fn len(self) -> usize {
        match self {
            Self::None => 0,
            Self::Ids(ids) => ids.len(),
            Self::Words(words) => words.len(),
        }
    }
}

/// An instruction's operands in order: leading ids, arguments, then the
/// trail.
pub(crate) type Operands<'a> = (&'a [Id], &'a [Arg<'a>], Trail<'a>);

fn put(bytes: &mut Vec<u8>, word: u32) {
    bytes.extend_from_slice(&word.to_le_bytes());
}

/// Append instruction `op` over `operands` to `bytes`, every id below
/// `bound`; `bytes` growing past `limit` is refused. Nothing is written when
/// anything is refused.
///
/// # Errors
///
/// [`EncodeError::IdNotIssued`] for an id at or past `bound`,
/// [`EncodeError::InteriorNul`] for a string holding a nul,
/// [`EncodeError::TooLong`] past one instruction's words,
/// [`EncodeError::TooLarge`] past `limit`, and [`EncodeError::OutOfMemory`].
pub(crate) fn encode(
    bytes: &mut Vec<u8>,
    op: Op,
    (lead, args, trail): Operands<'_>,
    bound: u32,
    limit: usize,
) -> Result<(), EncodeError> {
    let issued = |id: &Id| id.get() < bound;
    let trail_issued = match trail {
        Trail::Ids(ids) => ids.iter().all(issued),
        Trail::None | Trail::Words(_) => true,
    };
    let args_issued = args
        .iter()
        .all(|arg| !matches!(arg, Arg::Id(id) if !issued(id)));
    if !lead.iter().all(issued) || !args_issued || !trail_issued {
        return Err(EncodeError::IdNotIssued);
    }
    let words = 1 + lead.len() + args.iter().map(Arg::words).sum::<usize>() + trail.len();
    let count = u16::try_from(words).map_err(|_| EncodeError::TooLong)?;
    // An over-long string is refused by its length, unread.
    if args
        .iter()
        .any(|arg| matches!(arg, Arg::Text(text) if text.as_bytes().contains(&0)))
    {
        return Err(EncodeError::InteriorNul);
    }
    let size = words * 4;
    if bytes.len().saturating_add(size) > limit {
        return Err(EncodeError::TooLarge);
    }
    bytes
        .try_reserve(size)
        .map_err(|_| EncodeError::OutOfMemory)?;
    put(bytes, u32::from(count) << 16 | u32::from(op.code()));
    for id in lead {
        put(bytes, id.get());
    }
    for arg in args {
        match *arg {
            Arg::Id(id) => put(bytes, id.get()),
            Arg::Word(word) => put(bytes, word),
            Arg::Wide(wide) => bytes.extend_from_slice(&wide.to_le_bytes()),
            Arg::Text(text) => {
                bytes.extend_from_slice(text.as_bytes());
                let padded = (text.len() / 4 + 1) * 4;
                bytes.resize(bytes.len() + padded - text.len(), 0);
            }
        }
    }
    match trail {
        Trail::None => {}
        Trail::Ids(ids) => ids.iter().for_each(|id| put(bytes, id.get())),
        Trail::Words(words) => words.iter().for_each(|&word| put(bytes, word)),
    }
    Ok(())
}

/// A module being written.
#[derive(Debug)]
pub struct Encoder {
    bytes: Vec<u8>,
    /// One past the largest id issued or copied, or the bound of the module
    /// rewritten.
    bound: u32,
}

/// The header word holding the bound.
const BOUND_WORD: usize = 3;

impl Encoder {
    /// A module of `version`, carrying `generator` in its header.
    ///
    /// # Errors
    ///
    /// [`EncodeError::OutOfMemory`] when the header cannot be had.
    pub fn new(version: Version, generator: u32) -> Result<Self, EncodeError> {
        let mut encoder = Self {
            bytes: Vec::new(),
            bound: 1,
        };
        encoder.reserve(HEADER_WORDS)?;
        for word in [MAGIC, version.word(), generator, 1, 0] {
            encoder.put(word);
        }
        Ok(encoder)
    }

    /// A module rewriting `module`: its version, generator and bound, so
    /// every id of it counts as issued, and none of its instructions yet.
    ///
    /// # Errors
    ///
    /// [`EncodeError::OutOfMemory`] when the header cannot be had.
    pub fn rewriting(module: &Module) -> Result<Self, EncodeError> {
        let mut encoder = Self::new(module.version(), module.generator())?;
        encoder.bound = module.bound();
        Ok(encoder)
    }

    fn reserve(&mut self, words: usize) -> Result<(), EncodeError> {
        let bytes = words * 4;
        if self.bytes.len() + bytes > MAX_MODULE_BYTES {
            return Err(EncodeError::TooLarge);
        }
        self.bytes
            .try_reserve(bytes)
            .map_err(|_| EncodeError::OutOfMemory)
    }

    fn put(&mut self, word: u32) {
        put(&mut self.bytes, word);
    }

    /// One past the largest id issued or copied.
    pub(crate) const fn bound(&self) -> u32 {
        self.bound
    }

    /// Room for `bytes` more, exactly, held to the largest module.
    ///
    /// # Errors
    ///
    /// [`EncodeError::TooLarge`] past the largest module, and
    /// [`EncodeError::OutOfMemory`].
    pub(crate) fn room_for(&mut self, bytes: usize) -> Result<(), EncodeError> {
        if self.bytes.len().saturating_add(bytes) > MAX_MODULE_BYTES {
            return Err(EncodeError::TooLarge);
        }
        self.bytes
            .try_reserve_exact(bytes)
            .map_err(|_| EncodeError::OutOfMemory)
    }

    /// Append instructions already encoded against this encoder's ids.
    ///
    /// # Errors
    ///
    /// [`EncodeError::TooLarge`] past the largest module, and
    /// [`EncodeError::OutOfMemory`].
    pub(crate) fn append(&mut self, encoded: &[u8]) -> Result<(), EncodeError> {
        self.reserve(encoded.len() / 4)?;
        self.bytes.extend_from_slice(encoded);
        Ok(())
    }

    /// A fresh id, below no other this encoder has issued or copied.
    ///
    /// # Errors
    ///
    /// [`EncodeError::IdsExhausted`] once every id has been issued.
    pub fn next_id(&mut self) -> Result<Id, EncodeError> {
        let value = self.bound.max(1);
        let id = Id::new(value).ok_or(EncodeError::IdsExhausted)?;
        self.bound = value.checked_add(1).ok_or(EncodeError::IdsExhausted)?;
        Ok(id)
    }

    /// Write `op` with `args`, its operands in the grammar's order.
    ///
    /// # Errors
    ///
    /// [`EncodeError::IdNotIssued`] for an id at or past the bound,
    /// [`EncodeError::InteriorNul`] for a string holding a nul,
    /// [`EncodeError::TooLong`] past one instruction's words, and
    /// [`EncodeError::TooLarge`] past the largest module. Nothing is written
    /// in any of them.
    pub fn emit(&mut self, op: Op, args: &[Arg<'_>]) -> Result<(), EncodeError> {
        encode(
            &mut self.bytes,
            op,
            (&[], args, Trail::None),
            self.bound,
            MAX_MODULE_BYTES,
        )
    }

    /// Write `instruction`, one of `module`'s, as it stands; every id of
    /// `module` counts as issued from then on.
    ///
    /// # Errors
    ///
    /// [`EncodeError::TooLarge`] past the largest module.
    pub fn copy(&mut self, module: &Module, instruction: &Instruction) -> Result<(), EncodeError> {
        let bytes = module.instruction_bytes(instruction);
        self.reserve(bytes.len() / 4)?;
        self.bytes.extend_from_slice(bytes);
        self.bound = self.bound.max(module.bound());
        Ok(())
    }

    /// The module written: its words in little-endian byte order, its bound
    /// one past every id issued.
    #[must_use]
    pub fn finish(mut self) -> Vec<u8> {
        let at = BOUND_WORD * 4;
        if let Some(slot) = self.bytes.get_mut(at..at + 4) {
            slot.copy_from_slice(&self.bound.to_le_bytes());
        }
        self.bytes
    }
}

#[cfg(test)]
#[path = "encode_tests.rs"]
mod tests;
