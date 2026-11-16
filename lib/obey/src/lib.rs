//! RISC OS's command-line machinery, pure: GSTrans with every escape
//! ([`gstrans`]), system variables and their expansion ([`vars`]),
//! `OS_EvaluateExpression` ([`expr`]), `OS_SubstituteArgs` ([`args`]) and
//! `OS_ReadArgs` ([`readargs`]), all over a [`Variables`] store; and the
//! reading of an Obey file as data, which runs no command ([`command`]).
//!
//! The behaviour is RISC OS 5's, as its PRMs state it and as genuine RISC OS
//! 5.30 answers what they leave unstated: `tests/riscos5.rs` holds the crate
//! to every answer the reference harness recorded. Where RISC OS 5.30 never
//! returns, the reading is refused instead.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

extern crate alloc;

pub mod args;
pub mod command;
mod error;
pub mod expr;
pub mod gstrans;
pub mod name;
pub mod number;
pub mod readargs;
pub mod vars;

pub use error::Error;
pub use expr::Answer;
pub use gstrans::Flags;
pub use vars::{Store, Value, Variables};

/// The variable references a reading may still expand: a fixed bound on the
/// work a hostile string can ask for, however its macros nest. RISC OS sets
/// none, so a caller running trusted text gives a generous one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Budget(usize);

impl Budget {
    /// Room for `references` references.
    #[must_use]
    pub const fn new(references: usize) -> Self {
        Self(references)
    }

    /// The references left.
    #[must_use]
    pub const fn left(self) -> usize {
        self.0
    }

    /// `references` taken at once; `ExpansionTooComplex` where fewer are
    /// left, none then taken.
    fn charge(&mut self, references: usize) -> Result<(), Error> {
        self.0 = self
            .0
            .checked_sub(references)
            .ok_or(Error::ExpansionTooComplex)?;
        Ok(())
    }
}

/// The byte at `at` in `bytes`, NUL past its end: every string here ends at
/// its first CR, LF or NUL, or where its bytes do.
fn byte(bytes: &[u8], at: usize) -> u8 {
    bytes.get(at).copied().unwrap_or(0)
}

/// Whether `byte` ends a line: CR, LF or NUL.
const fn is_terminator(byte: u8) -> bool {
    matches!(byte, 0 | 10 | 13)
}

/// `text` to its first CR, LF or NUL.
fn line(text: &[u8]) -> &[u8] {
    &text[..text
        .iter()
        .position(|&b| is_terminator(b))
        .unwrap_or(text.len())]
}
