use alloc::vec::Vec;

use super::super::colours::nearest;
use super::*;

#[test]
fn a_blend_takes_each_share_rounded_down_apart() {
    let white_over =
        |under: u32, alpha: u32| mixed([255; 3], [under; 3], effective_alpha(alpha, OPAQUE))[0];
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
    assert_eq!(mixed([64; 3], [128; 3], effective_alpha(3, OPAQUE))[0], 126);
}

#[test]
fn translucency_scales_the_widened_alpha() {
    assert_eq!(effective_alpha(255, OPAQUE - 0x40), 192);
    assert_eq!(effective_alpha(3, OPAQUE - 0x40), 2);
    assert_eq!(effective_alpha(255, OPAQUE - 0xFF), 1);
    assert_eq!(
        mixed([255; 3], [128; 3], effective_alpha(255, OPAQUE - 0xC0))[0],
        159
    );
}

#[test]
fn a_palettised_blend_takes_the_nearest_colour() {
    let palette = [[0, 0, 0], [119, 119, 119], [255, 255, 255]];
    let mut nearest = NearestKept::of(&palette).expect("room");
    assert_eq!(blend_in_palette(&mut nearest, 0, 2, 128), 1);
    assert_eq!(blend_in_palette(&mut nearest, 2, 0, OPAQUE), 2);
}

#[test]
fn kept_answers_are_the_palette_search_s_own() {
    let palette: Vec<[u8; 3]> = (0u8..=255)
        .map(|i| [i, i.wrapping_mul(7), i.wrapping_mul(13)])
        .collect();
    let mut kept = NearestKept::of(&palette).expect("room");
    let colours = (0u32..3000).map(|n| {
        let [r, g, b, _] = n.wrapping_mul(0x9E37_79B9).to_le_bytes();
        [r, g, b]
    });
    for rgb in colours.clone().chain(colours) {
        assert_eq!(kept.index(rgb), nearest(&palette, rgb), "{rgb:?}");
    }
    for _ in 0..2 {
        for rgb in [[16, 32, 48], [17, 33, 49]] {
            assert_eq!(
                kept.index(rgb),
                nearest(&palette, rgb),
                "one group, two colours"
            );
        }
    }
}
