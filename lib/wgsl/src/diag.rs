//! What the front end reports: a diagnostic's severity, its closed code, the
//! span of source it names, and its message; and the stop that ends a
//! compile.

use alloc::string::String;
use core::fmt::{self, Write as _};

/// A span of the source, `start..end` in bytes.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Ord, PartialOrd)]
pub struct Span {
    /// The first byte.
    pub start: u32,
    /// One past the last byte.
    pub end: u32,
}

impl Span {
    /// The span of `start..end`.
    #[must_use]
    pub const fn new(start: u32, end: u32) -> Self {
        Self { start, end }
    }

    /// The span from this one's start to `other`'s end.
    #[must_use]
    pub const fn to(self, other: Self) -> Self {
        Self {
            start: self.start,
            end: if other.end > self.end {
                other.end
            } else {
                self.end
            },
        }
    }
}

/// How much a diagnostic matters.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum Severity {
    /// A note on the source; the module compiles.
    Info,
    /// A warning; the module compiles.
    Warning,
    /// A shader-creation error; no module.
    Error,
}

/// What a diagnostic is about: a closed set, each with a stable number.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
#[repr(u16)]
pub enum Code {
    /// A character no token begins with.
    InvalidCharacter = 1,
    /// A block comment never closed.
    UnterminatedComment = 2,
    /// A reserved word where a name goes.
    ReservedWord = 3,
    /// A name spelled `_` alone or beginning `__`.
    InvalidName = 4,
    /// Tokens the grammar does not admit where they stand.
    Syntax = 5,
    /// Statements, expressions or types nested past the bound.
    NestingTooDeep = 6,
    /// More parameters, members, case selectors or elements than the bound.
    TooMany = 7,
    /// A literal its type cannot hold.
    LiteralRange = 8,
    /// A directive after a declaration, or one naming what it may not.
    Directive = 9,
    /// An extension TAIRiX does not offer.
    UnsupportedExtension = 10,
    /// A name no declaration in scope gives.
    Undeclared = 11,
    /// A name declared twice in one scope.
    Redeclared = 12,
    /// Declarations that depend on themselves.
    Cycle = 13,
    /// A name used as what it does not denote: a value as a type, a type as
    /// a value, a function as either.
    Misused = 14,
    /// A type that is not well formed, or not of the kind its place needs.
    Type = 15,
    /// A value of the wrong type for its place.
    Mismatch = 16,
    /// A call no overload of its function takes.
    NoOverload = 17,
    /// A constant or override expression's evaluation failed: overflow, a
    /// value not finite, a division by zero, or a domain error.
    Evaluation = 18,
    /// An attribute misplaced, repeated, or given what it may not hold.
    Attribute = 19,
    /// An entry point's interface not as its stage requires.
    Interface = 20,
    /// A resource or memory declaration not as its address space requires.
    AddressSpace = 21,
    /// A memory layout its address space does not take.
    Layout = 22,
    /// A statement where it may not stand, or a function whose behaviour is
    /// not what its signature requires.
    Behaviour = 23,
    /// A pointer or reference used in a way the language forbids.
    Pointer = 24,
    /// A derivative, barrier or texture sample in non-uniform control flow.
    Uniformity = 25,
    /// A failed `const_assert`.
    Assertion = 26,
    /// An override value the request names that the module does not declare,
    /// or one its override cannot hold.
    OverrideValue = 27,
    /// A construct the language admits that TAIRiX's model does not.
    Unsupported = 28,
    /// A module larger than the front end reads or writes.
    ModuleTooLarge = 29,
    /// A diagnostic rule name the front end does not know.
    UnknownRule = 30,
    /// More memory of an address space than the specification's limit for
    /// one function or one entry point.
    MemoryLimit = 31,
}

impl Code {
    /// Every code, in order.
    pub const ALL: [Self; 31] = [
        Self::InvalidCharacter,
        Self::UnterminatedComment,
        Self::ReservedWord,
        Self::InvalidName,
        Self::Syntax,
        Self::NestingTooDeep,
        Self::TooMany,
        Self::LiteralRange,
        Self::Directive,
        Self::UnsupportedExtension,
        Self::Undeclared,
        Self::Redeclared,
        Self::Cycle,
        Self::Misused,
        Self::Type,
        Self::Mismatch,
        Self::NoOverload,
        Self::Evaluation,
        Self::Attribute,
        Self::Interface,
        Self::AddressSpace,
        Self::Layout,
        Self::Behaviour,
        Self::Pointer,
        Self::Uniformity,
        Self::Assertion,
        Self::OverrideValue,
        Self::Unsupported,
        Self::ModuleTooLarge,
        Self::UnknownRule,
        Self::MemoryLimit,
    ];

    /// Its stable number.
    #[must_use]
    pub const fn number(self) -> u16 {
        self as u16
    }

    /// The code numbered `number`, if one is.
    #[must_use]
    pub fn from_number(number: u16) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|code| code.number() == number)
    }
}

/// One diagnostic: its severity and code, the span it names and where that
/// begins, and what it says.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Diagnostic {
    /// How much it matters.
    pub severity: Severity,
    /// What it is about.
    pub code: Code,
    /// The source it names.
    pub span: Span,
    /// The line the span begins on, from 1.
    pub line: u32,
    /// The column it begins at, in code points from 1.
    pub column: u32,
    /// What it says: fixed text and the source's own names, at most
    /// [`MAX_MESSAGE_BYTES`] bytes.
    pub message: String,
}

/// The longest message a diagnostic carries, in bytes.
pub const MAX_MESSAGE_BYTES: usize = 512;

/// The most characters of a name a message quotes.
const MAX_QUOTED: usize = 64;

/// Why a compile stopped before writing a module.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Stop {
    /// A shader-creation error: its code, span and message.
    Error(Code, Span, String),
    /// The allocator refused memory the work needed.
    OutOfMemory,
    /// One of the front end's own invariants failed: a defect in it, never
    /// an answer about the source.
    Internal,
}

/// What the front end's own steps answer.
pub(crate) type Outcome<T> = Result<T, Stop>;

/// The stop for `code` at `span`, its message written by `write`.
pub(crate) fn error(
    code: Code,
    span: Span,
    write: impl FnOnce(&mut Message) -> fmt::Result,
) -> Stop {
    let mut message = Message::default();
    if write(&mut message).is_err() && message.text.is_empty() {
        return Stop::OutOfMemory;
    }
    Stop::Error(code, span, message.text)
}

/// A message being written, held to [`MAX_MESSAGE_BYTES`], its allocations
/// fallible: what does not fit is dropped at a character boundary.
#[derive(Default)]
pub(crate) struct Message {
    text: String,
}

impl Message {
    /// The message written.
    pub(crate) fn into_text(self) -> String {
        self.text
    }

    /// Write `name` quoted, at most [`MAX_QUOTED`] of its characters.
    pub(crate) fn quote(&mut self, name: &str) -> fmt::Result {
        self.write_char('`')?;
        let mut chars = name.chars();
        for ch in chars.by_ref().take(MAX_QUOTED) {
            self.write_char(ch)?;
        }
        if chars.next().is_some() {
            self.write_str("…")?;
        }
        self.write_char('`')
    }
}

impl fmt::Write for Message {
    fn write_str(&mut self, piece: &str) -> fmt::Result {
        let room = MAX_MESSAGE_BYTES.saturating_sub(self.text.len());
        let mut take = piece.len().min(room);
        while !piece.is_char_boundary(take) {
            take -= 1;
        }
        let kept = &piece[..take];
        self.text.try_reserve(kept.len()).map_err(|_| fmt::Error)?;
        self.text.push_str(kept);
        Ok(())
    }
}

/// Push `item`, its room taken fallibly.
pub(crate) fn push<T>(list: &mut alloc::vec::Vec<T>, item: T) -> Outcome<()> {
    list.try_reserve(1).map_err(|_| Stop::OutOfMemory)?;
    list.push(item);
    Ok(())
}

/// The item at `index` in `list`, which grows to hold it, filled with
/// `fill`, its room taken fallibly.
pub(crate) fn slot<T: Clone>(
    list: &mut alloc::vec::Vec<T>,
    index: usize,
    fill: T,
) -> Outcome<&mut T> {
    if list.len() <= index {
        reserve(list, index + 1 - list.len())?;
        list.resize(index + 1, fill);
    }
    list.get_mut(index).ok_or(Stop::Internal)
}

/// Room for `count` more items in `list`, taken fallibly.
pub(crate) fn reserve<T>(list: &mut alloc::vec::Vec<T>, count: usize) -> Outcome<()> {
    list.try_reserve(count).map_err(|_| Stop::OutOfMemory)
}
