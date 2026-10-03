//! The sRGB colour (`lib/colour`): its 8-bit value, the transfer its channels
//! are encoded in, its hue coordinates, and its hexadecimal spelling.
//!
//! Every colour conversion and colour notation in TAIRiX is defined here once.
//! A theme authors its palette in [`Rgba`], a colour picker edits in [`Hsv`],
//! CSS `hsl()` resolves through [`Hsl`], and a settings document, an SVG asset
//! and a colour field read and write the same [`parse_hex`] and [`Hex`]
//! digits.
//!
//! The coordinates are exact integer arithmetic, rounded once: every 8-bit
//! colour comes back unchanged from [`Hsv`] and from [`Hsl`], and a grey or a
//! black keeps whatever hue and saturation the caller held, since the colour
//! itself has none to give.
//!
//! The crate allocates nothing and depends only on `lib/util`, so the lowest
//! layer that draws a colour can use it.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

#[cfg(any(test, feature = "test-util"))]
extern crate std;

mod hex;
#[cfg(any(test, feature = "test-util"))]
pub mod legibility;
mod model;
mod rgb;
mod srgb;

pub use hex::{parse_hex, Hex, HexForm};
pub use model::{Fraction, Hsl, Hsv, Hue};
pub use rgb::{Rgb, Rgba};
pub use srgb::{linear_to_srgb, srgb_to_linear};

#[cfg(test)]
mod hex_tests;
#[cfg(test)]
mod model_tests;
#[cfg(test)]
mod rgb_tests;
