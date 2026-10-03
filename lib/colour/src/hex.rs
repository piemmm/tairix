//! Hexadecimal colour notation: CSS's four digit spellings, read and written.
//!
//! The digits are the notation; whether a `#` comes before them is the
//! surrounding grammar's choice. CSS and a colour field write one, while a
//! settings document cannot, since its grammar starts a comment there.

use core::fmt;

use crate::rgb::{Rgb, Rgba};

/// How many digits a hex colour is spelled in.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum HexForm {
    /// `rgb`: a digit a channel, each doubled (`f` is `ff`).
    Short,
    /// `rgba`.
    ShortAlpha,
    /// `rrggbb`.
    Long,
    /// `rrggbbaa`.
    LongAlpha,
}

impl HexForm {
    /// Whether the form spells an alpha.
    #[must_use]
    pub const fn has_alpha(self) -> bool {
        matches!(self, Self::ShortAlpha | Self::LongAlpha)
    }
}

/// `digits` read as a colour in one of the four [`HexForm`]s, with the form
/// it was spelled in: three, four, six or eight ASCII hex digits of either
/// case, and nothing else — no `#`, sign or space. A form without alpha is
/// opaque.
#[must_use]
pub fn parse_hex(digits: &str) -> Option<(Rgba, HexForm)> {
    let bytes = digits.as_bytes();
    let (form, width) = match bytes.len() {
        3 => (HexForm::Short, 1),
        4 => (HexForm::ShortAlpha, 1),
        6 => (HexForm::Long, 2),
        8 => (HexForm::LongAlpha, 2),
        _ => return None,
    };
    let mut channels = [u8::MAX; 4];
    for (channel, spelled) in channels.iter_mut().zip(bytes.chunks(width)) {
        *channel = match *spelled {
            [digit] => nibble(digit)? * 17,
            [high, low] => (nibble(high)? << 4) | nibble(low)?,
            _ => return None,
        };
    }
    Some((Rgba::from_array(channels), form))
}

/// One ASCII hex digit's value.
fn nibble(digit: u8) -> Option<u8> {
    char::from(digit)
        .to_digit(16)
        .and_then(|value| u8::try_from(value).ok())
}

impl Rgb {
    /// The colour six hex digits spell, `rrggbb` of either case and nothing
    /// else; `None` for any other spelling, a shorter one included.
    #[must_use]
    pub fn from_hex(digits: &str) -> Option<Self> {
        match parse_hex(digits)? {
            (colour, HexForm::Long) => Some(colour.without_alpha()),
            _ => None,
        }
    }

    /// The colour's six lowercase hex digits.
    #[must_use]
    pub const fn hex(self) -> Hex {
        Hex {
            colour: self.opaque(),
            alpha: false,
            hash: false,
        }
    }
}

impl Rgba {
    /// The colour's lowercase hex digits: six, or eight where it is
    /// translucent.
    #[must_use]
    pub const fn hex(self) -> Hex {
        Hex {
            colour: self,
            alpha: !self.is_opaque(),
            hash: false,
        }
    }
}

/// A colour spelled in lowercase hexadecimal, written by its
/// [`Display`](fmt::Display).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Hex {
    colour: Rgba,
    alpha: bool,
    hash: bool,
}

impl Hex {
    /// The same digits after CSS's `#`.
    #[must_use]
    pub const fn hashed(self) -> Self {
        Self { hash: true, ..self }
    }
}

impl fmt::Display for Hex {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Rgba { r, g, b, a } = self.colour;
        if self.hash {
            out.write_str("#")?;
        }
        write!(out, "{r:02x}{g:02x}{b:02x}")?;
        if self.alpha {
            write!(out, "{a:02x}")?;
        }
        Ok(())
    }
}
