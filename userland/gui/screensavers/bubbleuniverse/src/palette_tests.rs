//! Host tests of the palettes: the original's colours, every palette's word,
//! and every designed ramp in gamut, lit, and even.

extern crate std;

use alloc::vec::Vec;

use tairix_colour::srgb_to_linear;

use super::{Palette, CURVES, STEPS};

const ALL: [(&str, Palette); 6] = [
    ("original", Palette::Original),
    ("nebula", Palette::Nebula),
    ("aurora", Palette::Aurora),
    ("ember", Palette::Ember),
    ("ice", Palette::Ice),
    ("spectrum", Palette::Spectrum),
];

/// Every point's light in `palette`, curve by curve.
fn lights(palette: Palette) -> Vec<[f64; 3]> {
    let mut all = Vec::new();
    let mut expected = (0, 0);
    palette.each(|curve, step, light| {
        assert_eq!((curve, step), expected, "curve by curve, step by step");
        expected = if step + 1 == STEPS {
            (curve + 1, 0)
        } else {
            (curve, step + 1)
        };
        all.push(light);
    });
    assert_eq!(all.len(), CURVES * STEPS);
    all
}

#[test]
fn every_palette_has_its_word_and_no_other_word_names_one() {
    for (word, palette) in ALL {
        assert_eq!(Palette::from_word(word), Some(palette));
    }
    for word in ["", "Original", "classic", "rainbow", "nebula "] {
        assert_eq!(Palette::from_word(word), None, "{word:?}");
    }
}

/// The original colours a point as the first Bubble Universe did: its curve
/// and its step as red and green levels, over a blue of 99.
#[test]
fn the_original_colours_each_point_by_its_curve_and_step() {
    let lights = lights(Palette::Original);
    let level = |level: f64| srgb_to_linear(level / 255.0);
    let at = |curve: usize, step: usize| lights[curve * STEPS + step];
    let alike = |a: [f64; 3], b: [f64; 3]| (0..3).all(|c| (a[c] - b[c]).abs() < 1e-12);
    assert!(alike(at(0, 0), [0.0, 0.0, level(99.0)]));
    assert!(alike(at(120, 45), [level(120.0), level(45.0), level(99.0)]));
    assert!(alike(
        at(CURVES - 1, STEPS - 1),
        [level(199.0), level(199.0), level(99.0)]
    ));
}

/// Every designed ramp lies inside sRGB, lights every point it colours, and
/// moves evenly from one step to the next and one curve to the next, so no
/// curve shows a seam.
#[test]
fn every_palette_is_in_gamut_lit_and_even() {
    for (word, palette) in ALL {
        let lights = lights(palette);
        for light in &lights {
            assert!(
                light.iter().all(|c| (0.0..=1.0).contains(c)),
                "{word}: {light:?}"
            );
            assert!(light.iter().any(|&c| c > 0.0), "{word}: a point left dark");
        }
        let jump =
            |a: [f64; 3], b: [f64; 3]| (0..3).map(|c| (a[c] - b[c]).abs()).fold(0.0, f64::max);
        for curve in 0..CURVES {
            for step in 1..STEPS {
                let (a, b) = (
                    lights[curve * STEPS + step - 1],
                    lights[curve * STEPS + step],
                );
                assert!(jump(a, b) < 0.02, "{word}: curve {curve} step {step}");
            }
        }
        for curve in 1..CURVES {
            for step in [0, STEPS / 2, STEPS - 1] {
                let (a, b) = (
                    lights[(curve - 1) * STEPS + step],
                    lights[curve * STEPS + step],
                );
                assert!(jump(a, b) < 0.03, "{word}: curve {curve} step {step}");
            }
        }
    }
}
