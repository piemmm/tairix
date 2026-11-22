use alloc::vec::Vec;

use tairix_colour::Rgb;

use super::*;

#[test]
fn a_blend_takes_each_share_rounded_down_apart() {
    let white_over = |under: u32, alpha: u32| channel(255, 8, under, 8, weight(alpha, OPAQUE));
    assert_eq!(white_over(0, 0), 0);
    assert_eq!(white_over(0, 1), 0);
    assert_eq!(white_over(0, 2), 1);
    assert_eq!(
        white_over(0, 128),
        128,
        "an alpha of 128 or more widened past itself"
    );
    assert_eq!(white_over(0, 255), 255);
    assert_eq!(
        white_over(128, 1),
        127,
        "the background's share rounded down too"
    );
    assert_eq!(white_over(128, 4), 129);
    assert_eq!(channel(64, 8, 128, 8, weight(3, OPAQUE)), 126);
}

#[test]
fn translucency_scales_the_widened_alpha() {
    assert_eq!(weight(255, OPAQUE - 0x40) >> 8, 192);
    assert_eq!(weight(3, OPAQUE - 0x40) >> 8, 2);
    assert_eq!(weight(255, OPAQUE - 0xFF) >> 8, 1);
    assert_eq!(channel(255, 8, 128, 8, weight(255, OPAQUE - 0xC0)), 159);
}

#[test]
fn an_eight_bit_source_cuts_its_share_to_256ths_onto_five_bit_channels() {
    // A 32-bit sprite's 200 and 100 over black on a 16-bit screen at
    // translucency 128, and a near-transparent pixel over (9, 31, 26), as
    // RISC OS 5 drew them.
    let opacity = OPAQUE - 128;
    assert_eq!(channel(200, 8, 0, 5, weight(20, opacity)), 0);
    assert_eq!(channel(100, 8, 0, 5, weight(42, opacity)), 1);
    assert_eq!(channel(200, 8, 0, 5, weight(42, opacity)), 2);
    let near_transparent = weight(3, opacity);
    let [r, g, b] = [9, 31, 26].map(|d| channel(255, 8, d, 5, near_transparent));
    assert_eq!([r, g, b], [8, 30, 25]);
}

#[test]
fn a_narrower_source_keeps_its_whole_share_and_widens_by_repeating() {
    // A 16-bit sprite's channels at translucency 92: onto 5-bit channels the
    // share is kept whole where cutting it would lose a step, and onto 8-bit
    // ones each channel repeats its bits as RISC OS 5 drew them.
    let opacity = OPAQUE - 92;
    assert_eq!(
        channel(25, 5, 0, 5, weight(16, opacity)),
        1,
        "the share uncut"
    );
    assert_eq!(channel(11, 5, 0, 8, weight(255, opacity)), 58);
    assert_eq!(channel(5, 5, 0, 8, weight(255, opacity)), 26);
}

/// The sixteen colours the reference machine was given for the 16-colour
/// observations.
fn sixteen() -> Vec<Rgb> {
    (0..16u32)
        .map(|i| {
            let [r, ..] = (i * 91 + 40).to_le_bytes();
            let [g, ..] = (i * 53 + 200).to_le_bytes();
            let [b, ..] = (i * 147 + 77).to_le_bytes();
            Rgb::new(r, g, b)
        })
        .collect()
}

/// The 256 colours it was given for the 256-colour observations.
fn two_fifty_six() -> Vec<Rgb> {
    (0..256u32)
        .map(|i| {
            if i == 0 {
                return Rgb::BLACK;
            }
            let [r, ..] = (i * 167 + 13).to_le_bytes();
            let [g, ..] = (i * 73 + (i >> 3) * 29).to_le_bytes();
            let [b, ..] = (i * 11 + (i >> 4) * 97).to_le_bytes();
            Rgb::new(r, g, b)
        })
        .collect()
}

/// Each alpha in `runs` blended at full opacity, what the destination
/// pixel `under` became.
fn alpha_runs(blending: &PaletteBlend<'_>, over: u32, under: u32, runs: &[(u32, u32, u32)]) {
    for &(first, last, number) in runs {
        for alpha in first..=last {
            let drawn = blending.pixel(over, under, alpha, OPAQUE).unwrap_or(under);
            assert_eq!(drawn, number, "alpha {alpha} over {under}");
        }
    }
}

#[test]
fn an_alpha_onto_sixteen_colours_steps_in_thirty_twos() {
    let palettised = Palettised::of(&sixteen()).expect("room");
    let blending = PaletteBlend {
        palettised: &palettised,
        alpha: true,
        full: false,
    };
    let over = palettised.number(Rgb::new(200, 100, 50));
    assert_eq!(over, 7);
    alpha_runs(
        &blending,
        over,
        0,
        &[
            (0, 63, 0),
            (64, 95, 14),
            (96, 159, 9),
            (160, 191, 12),
            (192, 255, 7),
        ],
    );
    alpha_runs(&blending, over, 9, &[(0, 159, 9), (160, 255, 7)]);
    assert_eq!(blending.pixel(over, 0, 31, OPAQUE), None, "below one step");
}

#[test]
fn an_alpha_onto_256_colours_mixes_five_bit_forms_through_the_table() {
    let palettised = Palettised::of(&two_fifty_six()).expect("room");
    let blending = PaletteBlend {
        palettised: &palettised,
        alpha: true,
        full: true,
    };
    let over = palettised.number(Rgb::new(200, 100, 50));
    assert_eq!(over, 18);
    alpha_runs(
        &blending,
        over,
        0,
        &[
            (0, 40, 0),
            (41, 61, 17),
            (62, 71, 198),
            (72, 112, 241),
            (113, 183, 111),
            (184, 214, 168),
            (215, 231, 1),
            (232, 254, 185),
            (255, 255, 18),
        ],
    );
    alpha_runs(
        &blending,
        over,
        77,
        &[
            (0, 8, 77),
            (9, 19, 120),
            (20, 20, 60),
            (21, 28, 120),
            (29, 39, 60),
            (40, 59, 160),
            (60, 61, 100),
            (62, 78, 160),
            (79, 82, 56),
            (83, 88, 42),
            (89, 90, 56),
            (91, 93, 223),
            (94, 98, 56),
            (99, 118, 223),
            (119, 122, 39),
            (123, 131, 223),
            (132, 155, 39),
            (156, 161, 82),
            (162, 164, 39),
            (165, 166, 82),
            (167, 172, 246),
            (173, 176, 82),
            (177, 180, 246),
            (181, 183, 65),
            (184, 185, 159),
            (186, 188, 246),
            (189, 197, 159),
            (198, 224, 142),
            (225, 226, 185),
            (227, 230, 142),
            (231, 231, 185),
            (232, 234, 142),
            (235, 244, 185),
            (245, 245, 18),
            (246, 254, 185),
            (255, 255, 18),
        ],
    );
}

#[test]
fn an_alpha_and_a_translucency_onto_256_colours_keep_the_whole_share() {
    // Colour 18, (200, 100, 50) found, over black and over colour 77 at an
    // opacity and alpha each, as RISC OS 5 drew them: each share kept whole,
    // where cutting it to 256ths would take the colour either side.
    let palettised = Palettised::of(&two_fifty_six()).expect("room");
    let blending = PaletteBlend {
        palettised: &palettised,
        alpha: true,
        full: true,
    };
    let observed = [
        (128, 123, 198, 160),
        (192, 82, 198, 160),
        (192, 245, 168, 159),
        (164, 96, 198, 160),
        (164, 112, 241, 160),
        (164, 175, 111, 223),
        (128, 81, 0, 160),
    ];
    for (opacity, alpha, on_black, on_77) in observed {
        assert_eq!(
            blending.pixel(18, 0, alpha, opacity),
            Some(on_black),
            "{alpha} at {opacity} on black"
        );
        assert_eq!(
            blending.pixel(18, 77, alpha, opacity),
            Some(on_77),
            "{alpha} at {opacity} on 77"
        );
    }
}

#[test]
fn a_translucency_onto_256_colours_mixes_as_blendtable_through_the_table() {
    let palettised = Palettised::of(&two_fifty_six()).expect("room");
    let blending = PaletteBlend {
        palettised: &palettised,
        alpha: false,
        full: true,
    };
    let observed = [
        (
            128,
            [
                (37, 0, 17),
                (188, 0, 241),
                (159, 0, 215),
                (57, 0, 204),
                (128, 0, 34),
                (12, 0, 51),
            ],
        ),
        (
            128,
            [
                (37, 77, 129),
                (188, 77, 223),
                (159, 77, 56),
                (57, 77, 149),
                (128, 77, 59),
                (12, 77, 56),
            ],
        ),
        (
            64,
            [
                (37, 0, 204),
                (188, 0, 171),
                (159, 0, 125),
                (57, 0, 54),
                (128, 0, 68),
                (12, 0, 38),
            ],
        ),
        (
            64,
            [
                (37, 77, 11),
                (188, 77, 82),
                (159, 77, 96),
                (57, 77, 11),
                (128, 77, 42),
                (12, 77, 116),
            ],
        ),
    ];
    for (translucency, cases) in observed {
        for (over, under, drawn) in cases {
            assert_eq!(
                blending.pixel(over, under, 0xFF, OPAQUE - translucency),
                Some(drawn),
                "{over} over {under} at {translucency}"
            );
        }
    }
}

#[test]
fn a_blend_onto_fewer_colours_takes_the_plain_nearest_the_last_of_equally_near() {
    let palette = [
        Rgb::new(100, 0, 0),
        Rgb::new(100, 128, 0),
        Rgb::new(100, 96, 0),
        Rgb::new(140, 64, 0),
        Rgb::new(100, 96, 0),
    ];
    let palettised = Palettised::of(&palette).expect("room");
    let blending = PaletteBlend {
        palettised: &palettised,
        alpha: false,
        full: false,
    };
    // The mix (100, 64, 0) is nearer the green step than the red one by
    // plain squares, though not by the default loadings.
    assert_eq!(blending.pixel(0, 1, 0xFF, OPAQUE / 2), Some(4));
}
