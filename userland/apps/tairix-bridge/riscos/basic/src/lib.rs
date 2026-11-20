//! TAIRiX Bridge's BBC BASIC V and VI (`plans/RISCOS-EMULATOR.md` §7): the
//! BASIC and BASIC64 modules, and the numbers they compute on.
//!
//! - [`BASIC`] and [`BASIC64`]: the two modules, `*BASIC` and `*BASIC64`.
//! - [`five`]: BASIC V's five-byte real, its arithmetic and its functions.
//! - [`six`]: BASIC VI's eight-byte real, whose arithmetic is the FPA's.
//! - [`text`]: numbers read from and written as decimal text.
//! - [`corpus`]: the recorded corpora's formats, which the replays and the
//!   reference harness share.
//!
//! The interpreter is a resumable machine whose program, variables and
//! stacks all live in guest memory, laid out as BASIC V lays them. Each rule
//! is RISC OS 5's BASIC V or BASIC VI as recorded black-box; the replays in
//! `tests/` hold the crate to those recordings.

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

pub mod corpus;
pub mod five;
mod help;
mod machine;
mod messages;
mod module;
pub mod six;
mod stack;
mod start;
mod table;
pub mod text;
mod token;
mod value;
mod workspace;

pub use module::{BASIC, BASIC64};
pub use tairix_riscos_arm::fpa::Exception;

/// The session's built-in modules this crate provides.
pub static MODULES: [&tairix_riscos_kernel::module::Builtin; 2] = [&BASIC, &BASIC64];

/// A numeric error BASIC raises.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    /// A division by zero.
    DivisionByZero,
    /// A result past the largest the format holds, or past an integer's
    /// range where an integer was needed.
    TooBig,
    /// An arcsine or arccosine of a number past one.
    ArcRange,
    /// The square root of a negative number.
    NegativeRoot,
    /// The logarithm of a number not above zero.
    LogRange,
    /// A sine, cosine or tangent of a number too large to reduce.
    AccuracyLost,
    /// An exponential of a number too large.
    ExponentRange,
    /// An exception BASIC VI's floating-point arithmetic trapped on: the
    /// floating-point emulator's error, not BASIC's.
    Floating(Exception),
}

impl Fault {
    /// BASIC's error number for the fault; `None` for a floating-point
    /// exception.
    #[must_use]
    pub const fn number(self) -> Option<u32> {
        Some(match self {
            Self::DivisionByZero => 18,
            Self::TooBig | Self::ArcRange => 20,
            Self::NegativeRoot => 21,
            Self::LogRange => 22,
            Self::AccuracyLost => 23,
            Self::ExponentRange => 24,
            Self::Floating(_) => return None,
        })
    }
}
