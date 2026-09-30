//! Host tests of the filmic curve and the sRGB encoding.

use tairix_theme::color::linear_to_srgb as srgb;
use tairix_util::mathf;

use super::super::vector::Vec3;
use super::{display, filmic, Encoder};

#[test]
fn the_filmic_curve_rises_from_black_and_rolls_off_into_white() {
    assert!(filmic(0.0).abs() < 1e-12);
    assert!(filmic(-3.0).abs() < 1e-12, "no light is less than none");
    let mut last = 0.0;
    for step in 1..=400u32 {
        let exposed = f64::from(step) * 0.05;
        let now = filmic(exposed);
        assert!(now <= 1.0);
        // Strictly rising until the fit meets white, a little past seven.
        if exposed < 7.0 {
            assert!(now > last, "{exposed}");
        } else {
            assert!(now >= last, "{exposed}");
        }
        last = now;
    }
    assert!(filmic(7.0) > 0.99 && filmic(7.0) < 1.0);
    assert!((filmic(1e6) - 1.0).abs() < 1e-12);
    assert!(display(Vec3::new(f64::NAN, 1.0, 1.0)).max_element() <= 0.0);
    assert!(display(Vec3::splat(f64::INFINITY)).max_element() <= 0.0);
}

#[test]
fn the_encoding_matches_the_srgb_curve() {
    let encoder = Encoder::new().expect("the table");
    assert_eq!(encoder.level(0.0), 0);
    assert_eq!(encoder.level(1.0), 255 * 256);
    assert_eq!(encoder.level(2.0), 255 * 256);
    for step in 0..=1000u32 {
        let linear = f64::from(step) / 1000.0;
        let exact = srgb(linear) * 255.0 * 256.0;
        let level = f64::from(encoder.level(linear));
        assert!(
            (level - exact).abs() < 2.0,
            "{linear}: {level} against {exact}"
        );
    }
}

/// Dithered, a flat field's pixels average to exactly the level it lies
/// between, so a slow gradient shows no bands.
#[test]
fn a_dithered_field_averages_to_its_true_level() {
    let encoder = Encoder::new().expect("the table");
    for linear in [0.002, 0.018, 0.2, 0.21, 0.5, 0.93] {
        let mut total = 0u32;
        for y in 0..8 {
            for x in 0..8 {
                let pixel = encoder.pixel(Vec3::splat(linear), (x, y));
                assert_eq!(pixel.a, u8::MAX);
                assert!(pixel.r == pixel.g && pixel.g == pixel.b);
                total += u32::from(pixel.r);
            }
        }
        let mean = f64::from(total) / 64.0;
        let exact = srgb(linear) * 255.0;
        assert!(
            (mean - exact).abs() < 0.1,
            "{linear}: {mean} against {exact}"
        );
        // Never more than one level either side of it.
        assert!(mathf::fabs(mean - exact) < 1.0);
    }
    let white = encoder.pixel(Vec3::ONE, (3, 5));
    let black = encoder.pixel(Vec3::ZERO, (3, 5));
    assert_eq!((white.r, black.r), (255, 0));
}
