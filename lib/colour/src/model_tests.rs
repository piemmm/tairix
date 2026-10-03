use crate::{Fraction, Hsl, Hsv, Hue, Rgb};

/// Every 8-bit colour, in channel order.
fn every_colour() -> impl Iterator<Item = Rgb> {
    (0..=0x00FF_FFFFu32).map(|packed| {
        let [_, r, g, b] = packed.to_be_bytes();
        Rgb::new(r, g, b)
    })
}

#[test]
fn every_colour_survives_hue_saturation_and_value() {
    for rgb in every_colour() {
        assert_eq!(Hsv::from_rgb(rgb, Hsv::default()).to_rgb(), rgb, "{rgb:?}");
    }
}

#[test]
fn every_colour_survives_hue_saturation_and_lightness() {
    for rgb in every_colour() {
        assert_eq!(Hsl::from_rgb(rgb, Hsl::default()).to_rgb(), rgb, "{rgb:?}");
    }
}

#[test]
fn the_primaries_and_secondaries_sit_on_the_sextants() {
    let corners = [
        (Rgb::new(255, 0, 0), 0),
        (Rgb::new(255, 255, 0), 1),
        (Rgb::new(0, 255, 0), 2),
        (Rgb::new(0, 255, 255), 3),
        (Rgb::new(0, 0, 255), 4),
        (Rgb::new(255, 0, 255), 5),
    ];
    for (rgb, sixths) in corners {
        let hue = Hue::of(rgb).expect("a pure hue has a hue");
        assert_eq!(hue.steps(), sixths * Hue::SEXTANT, "{rgb:?}");
        assert_eq!(hue.degrees(), sixths * 60);
        let full = Hsv::new(hue, Fraction::ALL, Fraction::ALL);
        assert_eq!(full.to_rgb(), rgb);
    }
}

#[test]
fn a_grey_has_no_hue_and_keeps_the_one_it_is_given() {
    let near = Hsv::new(
        Hue::from_degrees(200),
        Fraction::from_percent(40),
        Fraction::ALL,
    );
    for level in [1, 128, 255] {
        let grey = Rgb::new(level, level, level);
        assert_eq!(Hue::of(grey), None);
        let hsv = Hsv::from_rgb(grey, near);
        assert_eq!(hsv.hue, near.hue);
        assert_eq!(hsv.saturation, Fraction::NONE, "a grey has no saturation");
        assert_eq!(hsv.value, Fraction::from_byte(level));
    }
}

#[test]
fn black_keeps_the_hue_and_saturation_it_is_given() {
    let near = Hsv::new(
        Hue::from_degrees(300),
        Fraction::from_percent(70),
        Fraction::ALL,
    );
    let black = Hsv::from_rgb(Rgb::BLACK, near);
    assert_eq!((black.hue, black.saturation), (near.hue, near.saturation));
    assert_eq!(black.value, Fraction::NONE);
    assert_eq!(black.to_rgb(), Rgb::BLACK);

    let lit = Hsl::new(
        Hue::from_degrees(30),
        Fraction::from_percent(80),
        Fraction::ALL,
    );
    for end in [Rgb::BLACK, Rgb::WHITE] {
        let hsl = Hsl::from_rgb(end, lit);
        assert_eq!(
            (hsl.hue, hsl.saturation),
            (lit.hue, lit.saturation),
            "{end:?}"
        );
        assert_eq!(hsl.to_rgb(), end);
    }
}

#[test]
fn hsl_meets_the_css_reference_conversions() {
    let css = |degrees, saturation, lightness| {
        Hsl::new(
            Hue::from_degrees(degrees),
            Fraction::from_percent(saturation),
            Fraction::from_percent(lightness),
        )
        .to_rgb()
    };
    assert_eq!(css(0, 100, 50), Rgb::new(255, 0, 0));
    assert_eq!(css(120, 100, 50), Rgb::new(0, 255, 0));
    assert_eq!(css(240, 100, 50), Rgb::new(0, 0, 255));
    assert_eq!(css(210, 50, 40), Rgb::new(0x33, 0x66, 0x99));
    assert_eq!(css(0, 0, 50), Rgb::new(128, 128, 128));
    assert_eq!(css(120, 100, 25), Rgb::new(0, 128, 0));
    assert_eq!(css(0, 100, 100), Rgb::WHITE);
    assert_eq!(css(0, 100, 0), Rgb::BLACK);
}

#[test]
fn whole_degrees_and_percentages_read_back_as_written() {
    for degrees in 0..360 {
        assert_eq!(Hue::from_degrees(degrees).degrees(), degrees);
    }
    assert_eq!(Hue::from_degrees(360), Hue::RED);
    assert_eq!(Hue::from_degrees(725), Hue::from_degrees(5));
    for percent in 0..=100 {
        assert_eq!(Fraction::from_percent(percent).percent(), percent);
    }
    assert_eq!(Fraction::from_percent(250), Fraction::ALL);
    for byte in 0..=u8::MAX {
        assert_eq!(Fraction::from_byte(byte).byte(), byte);
        assert_eq!(
            u32::from(Fraction::from_byte(byte).raw()),
            u32::from(byte) * 257
        );
    }
}

#[test]
fn a_hue_just_short_of_a_turn_is_red_again() {
    assert_eq!(Hue::from_steps(Hue::TURN - 1).degrees(), 0);
    assert_eq!(Hue::from_steps(Hue::TURN), Hue::RED);
}

#[test]
fn a_css_angle_of_any_size_or_sign_wraps_onto_the_circle() {
    assert_eq!(Hue::from_degrees_f64(480.0), Hue::from_degrees(120));
    assert_eq!(Hue::from_degrees_f64(-120.0), Hue::from_degrees(240));
    assert_eq!(Hue::from_degrees_f64(3600.0), Hue::RED);
    assert_eq!(
        Hue::from_degrees_f64(180.0),
        Hue::from_steps(3 * Hue::SEXTANT)
    );
    assert_eq!(Hue::from_degrees_f64(-0.000_000_1), Hue::RED);
    for odd in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert_eq!(Hue::from_degrees_f64(odd), Hue::RED);
    }
}

#[test]
fn a_css_proportion_is_held_to_none_and_all() {
    assert_eq!(Fraction::from_f64(-0.5), Fraction::NONE);
    assert_eq!(Fraction::from_f64(1.5), Fraction::ALL);
    assert_eq!(Fraction::from_f64(f64::NAN), Fraction::NONE);
    assert_eq!(Fraction::from_f64(0.5), Fraction::from_raw(0x8000));
}

#[test]
fn value_is_the_brightest_channel_and_lightness_the_mean() {
    let rgb = Rgb::new(200, 100, 50);
    let hsv = Hsv::from_rgb(rgb, Hsv::default());
    assert_eq!(hsv.value, Fraction::from_byte(200));
    assert_eq!(hsv.saturation.percent(), 75);
    let hsl = Hsl::from_rgb(rgb, Hsl::default());
    assert_eq!(hsl.lightness.percent(), 49);
    assert_eq!(hsl.saturation.percent(), 60);
    assert_eq!(hsv.hue, hsl.hue);
    assert_eq!(hsv.hue.degrees(), 20);
}
