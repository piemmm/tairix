//! Device CMYK: cyan, magenta, yellow and key as the complement of sRGB with
//! the black drawn out — what a CMYK value means without an output profile.

use crate::model::Fraction;
use crate::rgb::Rgb;

/// A colour as the four process inks lay it down, each a [`Fraction`] of full
/// coverage.
///
/// Uncalibrated by design: with no press or proof profile there is no other
/// meaning to give the numbers, and a picker that claimed one would be naming
/// a conversion it does not perform.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct Cmyk {
    /// Cyan, the complement of red.
    pub cyan: Fraction,
    /// Magenta, the complement of green.
    pub magenta: Fraction,
    /// Yellow, the complement of blue.
    pub yellow: Fraction,
    /// Key: black, drawn out of the three.
    pub key: Fraction,
}

/// Full coverage, squared: the scale of a product of two fractions.
const FULL_SQUARED: u64 = 65_535 * 65_535;

impl Cmyk {
    /// The inks `cyan`, `magenta`, `yellow` and `key`.
    #[must_use]
    pub const fn new(cyan: Fraction, magenta: Fraction, yellow: Fraction, key: Fraction) -> Self {
        Self {
            cyan,
            magenta,
            yellow,
            key,
        }
    }

    /// The inks that lay `rgb` down: as much black as the colour holds, and
    /// the three colours for what is left.
    #[must_use]
    pub fn from_rgb(rgb: Rgb) -> Self {
        let lightest = u32::from(rgb.r.max(rgb.g).max(rgb.b));
        if lightest == 0 {
            return Self::new(
                Fraction::NONE,
                Fraction::NONE,
                Fraction::NONE,
                Fraction::ALL,
            );
        }
        let share = |part: u32, whole: u32| {
            Fraction::from_raw(
                u16::try_from((part * 65_535 + whole / 2) / whole).unwrap_or(u16::MAX),
            )
        };
        let ink = |channel: u8| share(lightest - u32::from(channel), lightest);
        Self::new(
            ink(rgb.r),
            ink(rgb.g),
            ink(rgb.b),
            share(255 - lightest, 255),
        )
    }

    /// The sRGB colour these inks lay down: each channel what its complement
    /// and the black leave of white.
    #[must_use]
    pub fn to_rgb(self) -> Rgb {
        let paper = u64::from(65_535 - self.key.raw());
        let channel = |ink: Fraction| {
            let left = u64::from(65_535 - ink.raw()) * paper;
            u8::try_from((left * 255 + FULL_SQUARED / 2) / FULL_SQUARED).unwrap_or(u8::MAX)
        };
        Rgb::new(
            channel(self.cyan),
            channel(self.magenta),
            channel(self.yellow),
        )
    }
}
