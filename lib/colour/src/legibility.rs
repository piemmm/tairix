//! WCAG 2.1 contrast: the measure a test holds a colour pair to when it
//! claims the pair is legible.

use crate::rgb::Rgb;
use crate::srgb::srgb_to_linear;

/// The contrast ratio of two opaque colours: 21 for black on white, 1 for a
/// colour on itself, whichever order the pair is given in.
#[must_use]
pub fn contrast_ratio(a: Rgb, b: Rgb) -> f64 {
    let (la, lb) = (relative_luminance(a), relative_luminance(b));
    let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

/// [`contrast_ratio`] in hundredths rounded down, so a pair just short of a
/// threshold never passes it: 2100 for black on white, 100 for a colour on
/// itself.
#[must_use]
pub fn contrast_hundredths(a: Rgb, b: Rgb) -> u32 {
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a contrast ratio lies in 1..=21, so its hundredths fit a u32"
    )]
    let ratio = (contrast_ratio(a, b) * 100.0) as u32;
    ratio
}

/// The relative luminance of a colour: 0 for black, 1 for white.
#[must_use]
pub fn relative_luminance(colour: Rgb) -> f64 {
    let linear = |value: u8| srgb_to_linear(f64::from(value) / 255.0);
    0.2126 * linear(colour.r) + 0.7152 * linear(colour.g) + 0.0722 * linear(colour.b)
}

#[cfg(test)]
mod tests {
    use super::{contrast_hundredths, relative_luminance};
    use crate::rgb::Rgb;

    #[test]
    fn the_ratio_meets_the_reference_points() {
        assert_eq!(contrast_hundredths(Rgb::BLACK, Rgb::WHITE), 2100);
        assert_eq!(
            contrast_hundredths(Rgb::new(0x77, 0x77, 0x77), Rgb::WHITE),
            447
        );
        let blue = Rgb::new(9, 90, 200);
        assert_eq!(contrast_hundredths(blue, blue), 100);
        assert_eq!(
            contrast_hundredths(Rgb::WHITE, Rgb::BLACK),
            contrast_hundredths(Rgb::BLACK, Rgb::WHITE),
            "the order of the pair does not matter"
        );
    }

    #[test]
    fn luminance_runs_from_black_to_white() {
        assert!(relative_luminance(Rgb::BLACK).abs() < 1e-12);
        assert!((relative_luminance(Rgb::WHITE) - 1.0).abs() < 1e-12);
    }
}
