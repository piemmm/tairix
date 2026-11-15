//! BBC BASIC's tokens, and the tokenising and listing of its programs, for its
//! two dialects: BASIC II, the BBC Micro's and the Electron's ([`basic2`]),
//! and BASIC V and VI, RISC OS's ([`basic5`]).
//!
//! The keywords both dialects spell and number alike are defined once
//! ([`keyword::word`]); each dialect's table orders and flags them as its own
//! tokeniser does. [`tokenise`] turns text into a line's bytes, [`list`] turns
//! a line's bytes back into text under `LISTO`, [`lineref`] is how a line
//! number is held in a line, and [`program`] is the layout lines are kept in.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

extern crate alloc;

pub mod basic2;
pub mod basic5;
pub mod keyword;
pub mod lineref;
pub mod list;
pub mod program;
pub mod tokenise;

/// A dialect of BBC BASIC.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Dialect {
    /// BASIC II.
    BasicII,
    /// BASIC V, and BASIC VI, which tokenises and lists alike.
    BasicV,
}

/// Why a program or a line could not be handled.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Error {
    /// Memory for the work was refused.
    NoMemory,
    /// A program whose lines do not chain from one to the next, or are out of
    /// order.
    BadProgram,
    /// A line with more text than a program's line holds.
    LineTooLong,
    /// A line reference met while listing with no line numbers, which BASIC
    /// V stops at.
    ReferenceListed,
}

/// What ends a line, typed or kept.
const CR: u8 = b'\r';

/// The byte at `at`, NUL past the end.
fn byte(bytes: &[u8], at: usize) -> u8 {
    bytes.get(at).copied().unwrap_or(0)
}
