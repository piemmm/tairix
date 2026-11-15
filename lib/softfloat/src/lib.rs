//! Software floating point: exact binary32, binary64 and double-extended
//! arithmetic, each result rounded once in any of IEEE 754's modes, in
//! integers alone so every target computes the same bits.
//!
//! Two layers, so one core serves units whose rules differ:
//!
//! * The **core** ([`exact`]) holds a result exactly — a 64-bit significand,
//!   62 guard bits and a sticky bit — over operands in [`Parts`], unnormals
//!   among them, and rounds it at 24, 53 or 64 bits ([`Exact::rounded`]). It
//!   decides nothing about overflow, underflow or NaNs, so the 8087 builds its
//!   own register and memory rules on it.
//! * The **IEEE layer** ([`Ieee`]) is IEEE 754's arithmetic on binary32 and
//!   binary64 values with default exception handling, the [`Flags`] each
//!   operation raises, and the choices the standard leaves to an
//!   implementation made by a [`Policy`].
//!
//! [`Extended`] is the double-extended value both the 8087's registers and the
//! FPA's extended precision hold, with the x87's ten-byte and the FPA's
//! three-word memory layouts; [`Binary`] the interchange formats, with the
//! FPA's word order for binary64 ([`fpa_words`]). [`Decimal`] converts
//! between binary values and decimal ones exactly, and [`Packed`] is the
//! FPA's packed decimal memory format.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod big;
pub mod binary;
pub mod decimal;
pub mod exact;
pub mod extended;
mod flags;
pub mod ieee;
mod mode;
pub mod packed;

pub use binary::{fpa_words, from_fpa_words, Binary, Unpacked};
pub use decimal::{Beyond, Decimal};
pub use exact::{Exact, Parts, RoundedSig};
pub use extended::Extended;
pub use flags::Flags;
pub use ieee::{Ieee, Integral, Nans, Outcome, Policy, Tininess};
pub use mode::{Mode, Rounding};
pub use packed::Packed;

#[cfg(test)]
mod tests;
