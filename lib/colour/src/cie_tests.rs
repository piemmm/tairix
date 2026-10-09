use crate::{encode_linear, Lab, Lch, Rgb, Xyz};

fn near(a: f64, b: f64, by: f64) -> bool {
    (a - b).abs() <= by
}

#[test]
fn white_and_black_sit_at_the_ends_of_lightness() {
    let white = Lab::from_rgb(Rgb::WHITE);
    assert!(near(white.l, 100.0, 1e-3) && near(white.a, 0.0, 1e-2) && near(white.b, 0.0, 1e-2));
    let black = Lab::from_rgb(Rgb::BLACK);
    assert!(near(black.l, 0.0, 1e-9) && near(black.a, 0.0, 1e-9) && near(black.b, 0.0, 1e-9));
    let xyz = Xyz::from_rgb(Rgb::WHITE);
    assert!(
        near(xyz.x, Xyz::D65.x, 1e-4) && near(xyz.y, 1.0, 1e-6) && near(xyz.z, Xyz::D65.z, 1e-4)
    );
}

#[test]
fn the_primaries_measure_what_the_standard_says() {
    // The CIE values of sRGB's primaries under D65.
    for (rgb, (l, a, b)) in [
        (Rgb::new(255, 0, 0), (53.24, 80.09, 67.20)),
        (Rgb::new(0, 255, 0), (87.73, -86.18, 83.18)),
        (Rgb::new(0, 0, 255), (32.30, 79.19, -107.86)),
    ] {
        let lab = Lab::from_rgb(rgb);
        assert!(
            near(lab.l, l, 0.02) && near(lab.a, a, 0.05) && near(lab.b, b, 0.05),
            "{rgb:?} measured {lab:?}"
        );
    }
}

#[test]
fn every_colour_comes_back_through_lab_and_lch() {
    for packed in (0..=0x00FF_FFFFu32).step_by(97) {
        let [_, r, g, b] = packed.to_be_bytes();
        let rgb = Rgb::new(r, g, b);
        let lab = Lab::from_rgb(rgb).to_rgb();
        assert_eq!(lab.rgb, rgb, "{rgb:?}");
        assert!(!lab.clipped, "{rgb:?} is in sRGB");
        assert_eq!(Lch::from_rgb(rgb).to_rgb().rgb, rgb, "{rgb:?}");
    }
}

#[test]
fn a_colour_outside_srgb_is_clipped_and_says_so() {
    // A chroma no sRGB colour reaches at this lightness.
    let vivid = Lch {
        l: 50.0,
        c: 150.0,
        h: 140.0,
    }
    .to_rgb();
    assert!(vivid.clipped);
    let inside = encode_linear([0.5, 0.25, 0.75]);
    assert!(!inside.clipped);
    let outside = encode_linear([1.2, -0.1, 0.5]);
    assert!(outside.clipped);
    assert_eq!((outside.rgb.r, outside.rgb.g), (255, 0));
    // Past the edge by less than half a level: the same level either way.
    let edge = encode_linear([1.0 + 1e-4, -1e-5, 0.5]);
    assert!(!edge.clipped);
    assert_eq!((edge.rgb.r, edge.rgb.g), (255, 0));
}

#[test]
fn polar_form_keeps_a_grey_at_hue_zero_and_turns_with_the_opponents() {
    let grey = Lch::from_rgb(Rgb::new(119, 119, 119));
    assert!(near(grey.c, 0.0, 1e-2));
    let yellow = Lch::from_lab(Lab {
        l: 50.0,
        a: 0.0,
        b: 40.0,
    });
    assert!(near(yellow.h, 90.0, 1e-9) && near(yellow.c, 40.0, 1e-9));
    let blue = Lch::from_lab(Lab {
        l: 50.0,
        a: 0.0,
        b: -40.0,
    });
    assert!(near(blue.h, 270.0, 1e-9));
}
