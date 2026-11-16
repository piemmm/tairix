//! System variables: the values the expansions read, the store they read them
//! from, and how each type `OS_SetVarVal` takes becomes a value.

use alloc::vec::Vec;

use tairix_util::fallible;

use crate::expr::{evaluate, Answer};
use crate::gstrans::{translated, Flags};
use crate::number::decimal;
use crate::{is_terminator, name, Budget, Error};

/// A variable's value, as the expansions read it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Value {
    /// Delivered as it is: a string variable's bytes, or what a code
    /// variable's read entry answers.
    String(Vec<u8>),
    /// Delivered in signed decimal.
    Number(i32),
    /// Read again, as GSTrans reads text.
    Macro(Vec<u8>),
}

impl Value {
    /// A copy; `None` where the memory for it is refused.
    #[must_use]
    pub fn try_clone(&self) -> Option<Self> {
        Some(match self {
            Self::String(bytes) => Self::String(fallible::copied(bytes)?),
            Self::Number(n) => Self::Number(*n),
            Self::Macro(bytes) => Self::Macro(fallible::copied(bytes)?),
        })
    }

    /// The bytes it is stored in.
    #[must_use]
    pub fn len(&self) -> usize {
        match self {
            Self::String(bytes) | Self::Macro(bytes) => bytes.len(),
            Self::Number(_) => 4,
        }
    }

    /// Whether it is an empty string or macro.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// The store the expansions read variables from and `Set`, `SetMacro` and
/// `Unset` write: the kernel's over guest memory, or a [`Store`].
///
/// Names match without regard to case ([`name::matches`]) and sort by
/// [`name::compare`]. A pattern matching several means the first in that
/// order. Each pattern is a name as [`name::name`] cuts it.
pub trait Variables {
    /// The value of the first variable `pattern` matches, or `None`; an
    /// error is the host's, no memory for the value.
    fn lookup(&mut self, pattern: &[u8]) -> Result<Option<Value>, Error>;

    /// Whether [`lookup`](Self::lookup) of `pattern` finds a value, without
    /// copying it.
    fn exists(&mut self, pattern: &[u8]) -> Result<bool, Error> {
        Ok(self.lookup(pattern)?.is_some())
    }

    /// The references a lookup of `pattern` counts against a reading's
    /// budget: one, or one for each variable a wildcard may examine.
    fn cost(&self, pattern: &[u8]) -> usize {
        let _ = pattern;
        1
    }

    /// `value` given to the first variable `pattern` matches, its spelling
    /// kept; or, where none does, to a new variable called `pattern`, which
    /// must be [`name::is_creatable`].
    fn assign(&mut self, pattern: &[u8], value: Value) -> Result<(), Error>;

    /// The first variable `pattern` matches deleted, as `OS_SetVarVal`
    /// deletes one; whether there was one.
    fn delete(&mut self, pattern: &[u8]) -> Result<bool, Error>;

    /// Every variable `pattern` matches deleted, as `*Unset` deletes them;
    /// how many.
    fn unset(&mut self, pattern: &[u8]) -> Result<usize, Error> {
        let mut deleted = 0;
        while self.delete(pattern)? {
            deleted += 1;
        }
        Ok(deleted)
    }
}

/// A value `OS_SetVarVal` is asked to set, by the type it gives it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Request<'a> {
    /// Type 0: the text GSTrans'd with its escapes and quotes read.
    String(&'a [u8]),
    /// Type 1.
    Number(i32),
    /// Type 2: the text as written, to its first control character, which
    /// must be CR, LF or NUL.
    Macro(&'a [u8]),
    /// Type 3: the expression's value, an integer or a literal string.
    Expanded(&'a [u8]),
    /// Type 4: the bytes as they are.
    Literal(&'a [u8]),
}

/// The type a value was stored with.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    /// A string.
    String,
    /// A number.
    Number,
    /// A macro.
    Macro,
}

/// `OS_SetVarVal`: `request` set on the variable `name` names, no value
/// longer than `longest` bytes. A string expanded at the time sees the old
/// value of a variable it names, its own included.
pub fn set(
    vars: &mut dyn Variables,
    name: &[u8],
    request: Request<'_>,
    longest: usize,
    budget: &mut Budget,
) -> Result<Kind, Error> {
    let name = name::name(name);
    let too_long = |bytes: &[u8]| bytes.len() > longest;
    let value = match request {
        Request::String(text) => {
            let (bytes, _) =
                translated(text, Flags::NONE, vars, budget, longest).map_err(|e| match e {
                    Error::BufferOverflow => Error::NoRoomForVariable,
                    other => other,
                })?;
            Value::String(bytes)
        }
        Request::Number(n) => Value::Number(n),
        Request::Macro(text) => {
            let end = text.iter().position(|&b| b < b' ').unwrap_or(text.len());
            if text.get(end).is_some_and(|&b| !is_terminator(b)) {
                return Err(Error::BadMacroValue);
            }
            if too_long(&text[..end]) {
                return Err(Error::NoRoomForVariable);
            }
            Value::Macro(fallible::copied(&text[..end]).ok_or(Error::SystemHeapFull)?)
        }
        Request::Expanded(text) => match evaluate(text, vars, budget)? {
            Answer::Integer(n) => Value::Number(n),
            Answer::String(bytes) if too_long(&bytes) => return Err(Error::NoRoomForVariable),
            Answer::String(bytes) => Value::String(bytes),
        },
        Request::Literal(bytes) => {
            if too_long(bytes) {
                return Err(Error::NoRoomForVariable);
            }
            Value::String(fallible::copied(bytes).ok_or(Error::SystemHeapFull)?)
        }
    };
    let kind = match value {
        Value::String(_) => Kind::String,
        Value::Number(_) => Kind::Number,
        Value::Macro(_) => Kind::Macro,
    };
    vars.assign(name, value)?;
    Ok(kind)
}

/// `OS_ReadVarVal` asked for a string: the first variable `pattern` matches,
/// a number in signed decimal and a macro GSTrans'd, as a path variable is
/// read; at most `longest` bytes, `BufferOverflow` past that.
pub fn read_expanded(
    vars: &mut dyn Variables,
    pattern: &[u8],
    budget: &mut Budget,
    longest: usize,
) -> Result<Option<Vec<u8>>, Error> {
    budget.charge(vars.cost(pattern))?;
    let bytes = match vars.lookup(pattern)? {
        None => return Ok(None),
        Some(Value::String(bytes)) => bytes,
        Some(Value::Number(n)) => decimal(n)?,
        Some(Value::Macro(text)) => translated(&text, Flags::NONE, vars, budget, longest)?.0,
    };
    if bytes.len() > longest {
        return Err(Error::BufferOverflow);
    }
    Ok(Some(bytes))
}

/// Variables in host memory, bounded for text that cannot be trusted: no
/// more than a fixed number of them, none longer than a fixed length and no
/// name longer than another, which bounds what a wildcard's matching costs.
#[derive(Debug)]
pub struct Store {
    /// Sorted by [`name::compare`].
    entries: Vec<(Vec<u8>, Value)>,
    most: usize,
    longest: usize,
    names: usize,
}

impl Store {
    /// An empty store with room for `most` variables, none longer than
    /// `longest` bytes nor named with more than `names`; `None` where that
    /// room is refused.
    #[must_use]
    pub fn with_room(most: usize, longest: usize, names: usize) -> Option<Self> {
        let mut entries = Vec::new();
        entries.try_reserve_exact(most).ok()?;
        Some(Self {
            entries,
            most,
            longest,
            names,
        })
    }

    /// The most bytes a value it holds may have.
    #[must_use]
    pub const fn longest(&self) -> usize {
        self.longest
    }

    /// Whether a wildcard `pattern` can match a name at all: none holds
    /// more literal bytes than the bound on names.
    fn may_match(&self, pattern: &[u8]) -> bool {
        pattern.iter().filter(|&&b| b != b'*').count() <= self.names
    }

    /// Where the first variable `pattern` matches is.
    fn position(&self, pattern: &[u8]) -> Option<usize> {
        if name::is_wild(pattern) {
            if !self.may_match(pattern) {
                return None;
            }
            self.entries
                .iter()
                .position(|(name, _)| name::matches(pattern, name))
        } else {
            self.entries
                .binary_search_by(|(name, _)| name::compare(name, pattern))
                .ok()
        }
    }
}

impl Variables for Store {
    fn cost(&self, pattern: &[u8]) -> usize {
        let pattern = name::name(pattern);
        if name::is_wild(pattern) && self.may_match(pattern) {
            self.entries.len().max(1)
        } else {
            1
        }
    }

    fn exists(&mut self, pattern: &[u8]) -> Result<bool, Error> {
        Ok(self.position(name::name(pattern)).is_some())
    }

    fn lookup(&mut self, pattern: &[u8]) -> Result<Option<Value>, Error> {
        let Some(at) = self.position(name::name(pattern)) else {
            return Ok(None);
        };
        let value = self.entries[at]
            .1
            .try_clone()
            .ok_or(Error::SystemHeapFull)?;
        Ok(Some(value))
    }

    fn assign(&mut self, pattern: &[u8], value: Value) -> Result<(), Error> {
        let pattern = name::name(pattern);
        if value.len() > self.longest {
            return Err(Error::NoRoomForVariable);
        }
        if let Some(at) = self.position(pattern) {
            self.entries[at].1 = value;
            return Ok(());
        }
        if !name::is_creatable(pattern) {
            return Err(Error::BadVariableName);
        }
        if self.entries.len() >= self.most || pattern.len() > self.names {
            return Err(Error::NoRoomForVariable);
        }
        let at = self
            .entries
            .binary_search_by(|(name, _)| name::compare(name, pattern))
            .unwrap_or_else(|at| at);
        let name = fallible::copied(pattern).ok_or(Error::SystemHeapFull)?;
        // Within the room reserved at creation, so this never allocates.
        self.entries.insert(at, (name, value));
        Ok(())
    }

    fn delete(&mut self, pattern: &[u8]) -> Result<bool, Error> {
        let Some(at) = self.position(name::name(pattern)) else {
            return Ok(false);
        };
        self.entries.remove(at);
        Ok(true)
    }

    fn unset(&mut self, pattern: &[u8]) -> Result<usize, Error> {
        let pattern = name::name(pattern);
        let before = self.entries.len();
        self.entries
            .retain(|(name, _)| !name::matches(pattern, name));
        Ok(before - self.entries.len())
    }
}

#[cfg(test)]
#[path = "vars_tests.rs"]
mod tests;
