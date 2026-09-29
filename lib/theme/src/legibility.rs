//! WCAG 2.1 contrast: the measure a test holds a colour pair to when it
//! claims the pair is legible.

/// The contrast ratio of two opaque sRGB colours, in hundredths rounded
/// down, so a pair just short of a threshold never passes it: 2100 for black
/// on white, 100 for a colour on itself.
#[must_use]
pub fn contrast_hundredths(a: [u8; 3], b: [u8; 3]) -> u32 {
    let (la, lb) = (relative_luminance(a), relative_luminance(b));
    let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a contrast ratio lies in 1..=21, so its hundredths fit a u32"
    )]
    let ratio = (((hi + 0.05) / (lo + 0.05)) * 100.0) as u32;
    ratio
}

/// The relative luminance of an sRGB colour: 0 for black, 1 for white.
#[must_use]
pub fn relative_luminance([r, g, b]: [u8; 3]) -> f64 {
    0.2126 * linear(r) + 0.7152 * linear(g) + 0.0722 * linear(b)
}

/// One sRGB channel's share of linear light.
fn linear(value: u8) -> f64 {
    let c = f64::from(value) / 255.0;
    if c <= 0.040_45 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

#[cfg(test)]
mod tests {
    use super::{contrast_hundredths, relative_luminance};

    #[test]
    fn the_ratio_meets_the_reference_points() {
        assert_eq!(contrast_hundredths([0, 0, 0], [255, 255, 255]), 2100);
        assert_eq!(
            contrast_hundredths([0x77, 0x77, 0x77], [255, 255, 255]),
            447
        );
        assert_eq!(contrast_hundredths([9, 90, 200], [9, 90, 200]), 100);
        assert_eq!(
            contrast_hundredths([255, 255, 255], [0, 0, 0]),
            contrast_hundredths([0, 0, 0], [255, 255, 255]),
            "the order of the pair does not matter"
        );
    }

    #[test]
    fn luminance_runs_from_black_to_white() {
        assert!(relative_luminance([0, 0, 0]).abs() < 1e-12);
        assert!((relative_luminance([255, 255, 255]) - 1.0).abs() < 1e-12);
    }
}
