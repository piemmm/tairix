//! What a statement or an evaluation hands the machine back: done, moved
//! elsewhere, leaving for User mode, or a result to deliver to a waiting
//! statement at once, which the machine's loop does rather than a nested
//! call, so no statement's length deepens the host's stack.

use super::{Leave, Lvalue};
use crate::messages::Error;
use crate::value::Value;

/// A statement waiting on a value: which, by its number in the
/// continuation record.
pub(crate) type Cont = u8;

/// What a waiting statement carries: up to [`super::PAYLOAD_MOST`] words.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) struct Payload {
    words: [u32; super::PAYLOAD_MOST],
    len: usize,
}

impl Payload {
    /// The payload of `words`, as many as fit.
    pub(crate) fn of(words: &[u32]) -> Self {
        let mut payload = Self::default();
        let n = words.len().min(super::PAYLOAD_MOST);
        payload.words[..n].copy_from_slice(&words[..n]);
        payload.len = n;
        payload
    }

    /// Its words.
    pub(crate) fn words(&self) -> &[u32] {
        &self.words[..self.len]
    }

    /// Its `n`th word, zero past its end.
    pub(crate) fn word(&self, n: usize) -> u32 {
        self.words().get(n).copied().unwrap_or(0)
    }

    /// The byte its `n`th word carries: a token or a character.
    pub(crate) fn byte(&self, n: usize) -> u8 {
        self.word(n).to_le_bytes()[0]
    }

    /// It with word `n` made `value`, grown to hold it.
    pub(crate) fn with(mut self, n: usize, value: u32) -> Self {
        if n < super::PAYLOAD_MOST {
            self.words[n] = value;
            self.len = self.len.max(n + 1);
        }
        self
    }
}

/// What a waiting statement is given.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[expect(
    clippy::large_enum_variant,
    reason = "a string is carried inline, so delivering one never allocates"
)]
pub(crate) enum Delivered {
    /// An expression's value.
    Value(Value),
    /// An l-value.
    Lvalue(Lvalue),
}

impl Delivered {
    /// The value delivered: an l-value where one was awaited means the
    /// stack is corrupt.
    pub(crate) const fn value(&self) -> Result<&Value, Error> {
        match self {
            Self::Value(value) => Ok(value),
            Self::Lvalue(_) => Err(Error::StackCorrupt),
        }
    }

    /// The l-value delivered: a value where one was awaited means the stack
    /// is corrupt.
    pub(crate) const fn lvalue(&self) -> Result<Lvalue, Error> {
        match *self {
            Self::Lvalue(lvalue) => Ok(lvalue),
            Self::Value(_) => Err(Error::StackCorrupt),
        }
    }
}

/// What the machine does after a step.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[expect(
    clippy::large_enum_variant,
    reason = "a string is carried inline, so delivering one never allocates"
)]
pub(crate) enum Next {
    /// The statement is done: its end is checked and the next one begun.
    Done,
    /// The machine is elsewhere: the step set its mode and pointer.
    Moved,
    /// The slice ends there.
    Leave(Leave),
    /// `cont` given what it waits on now, with what it carries.
    Resume(Cont, Payload, Delivered),
}
