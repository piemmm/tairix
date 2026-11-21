//! Host tests of the tone curve: darkness stays black, light rises evenly and
//! whitens without clipping, and each entry is the curve's own.

extern crate std;

use tairix_colour::linear_to_srgb;
use tairix_util::mathf;

use super::{Tone, LIGHT_ONE};
use crate::EXPOSURE;

fn tone() -> Tone {
    Tone::new(EXPOSURE).expect("a table")
}

/// Ground no light reaches is black at every bias the dither rounds with, so
/// the bubble's surroundings are the screen's own black.
#[test]
fn no_light_shows_black_whatever_the_dither() {
    let tone = tone();
    for bias in 0..=255 {
        assert_eq!(tone.level(0, bias), 0, "{bias}");
    }
}

#[test]
fn more_light_never_shows_darker_and_a_crowd_whitens() {
    let tone = tone();
    let mut last = 0;
    for light in 0..=u16::MAX {
        let level = tone.level(light, 128);
        assert!(level >= last, "{light}");
        last = level;
    }
    assert_eq!(tone.level(u16::MAX, 128), u8::MAX, "a crowd shows white");
}

/// Each entry is the curve `1 − e^(−light)` encoded in sRGB, to within the
/// rounding the dither moves.
#[test]
fn the_table_follows_the_curve() {
    let tone = tone();
    for light in [1, 40, 900, 4095, 4096, 12_288, 40_000, 65_535] {
        let exact =
            linear_to_srgb(1.0 - mathf::exp(-EXPOSURE * f64::from(light) / f64::from(LIGHT_ONE)));
        let shown = f64::from(tone.level(light, 128));
        assert!(
            (shown - exact * 255.0).abs() <= 1.0,
            "{light}: {shown} against {}",
            exact * 255.0
        );
    }
}

/// A lone point of full colour is far from white, so it keeps its hue.
#[test]
fn one_light_is_shown_well_short_of_white() {
    let one = u16::try_from(LIGHT_ONE).expect("one light fits a channel");
    let level = tone().level(one, 128);
    assert!((180..230).contains(&level), "{level}");
}
