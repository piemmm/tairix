//! Host tests of the desktop resolutions an output offers.

use alloc::vec::Vec;

use super::{desktop_modes, DESKTOP_MIN, STANDARD_SIZES};

fn offered(native: (u32, u32)) -> Vec<(u32, u32)> {
    desktop_modes(native).collect()
}

#[test]
fn the_standard_table_is_largest_first_and_names_each_size_once() {
    for pair in STANDARD_SIZES.windows(2) {
        let area = |(w, h): (u32, u32)| u64::from(w) * u64::from(h);
        assert!(area(pair[0]) > area(pair[1]), "{pair:?}");
    }
    assert!(STANDARD_SIZES
        .iter()
        .all(|&(w, h)| w >= DESKTOP_MIN.0 && h >= DESKTOP_MIN.1));
}

#[test]
fn a_wide_output_offers_its_own_shape_only_below_itself() {
    assert_eq!(
        offered((1920, 1080)),
        [
            (1920, 1080),
            (1600, 900),
            (1366, 768),
            (1280, 720),
            (1024, 576),
            (960, 540),
            (854, 480),
            (640, 360),
        ]
    );
}

#[test]
fn sixteen_by_ten_and_four_by_three_outputs_offer_theirs() {
    assert_eq!(offered((1280, 800)), [(1280, 800), (1024, 640), (640, 400)]);
    assert_eq!(offered((1024, 768)), [(1024, 768), (800, 600), (640, 480)]);
}

#[test]
fn an_odd_native_size_is_offered_first_and_near_shapes_follow() {
    // 1366 x 768 is not exactly 16:9, and the 16:9 sizes below it are within
    // a percent of it.
    assert_eq!(
        offered((1366, 768)),
        [
            (1366, 768),
            (1280, 720),
            (1024, 576),
            (960, 540),
            (854, 480),
            (640, 360)
        ]
    );
    // A size no standard shares still offers itself.
    assert_eq!(offered((1000, 1000)), [(1000, 1000)]);
}

#[test]
fn nothing_below_the_floor_is_offered_but_a_small_native_size_is() {
    assert_eq!(offered((800, 450)), [(800, 450), (640, 360)]);
    assert_eq!(offered((640, 360)), [(640, 360)]);
    assert_eq!(
        offered((320, 240)),
        [(320, 240)],
        "the native size, whatever it is"
    );
}

#[test]
fn an_output_with_no_pixels_offers_nothing() {
    assert!(offered((0, 0)).is_empty());
    assert!(offered((1920, 0)).is_empty());
    assert!(offered((0, 1080)).is_empty());
}

#[test]
fn every_offered_size_fits_inside_the_output_at_its_shape() {
    for native in [
        (3840, 2160),
        (2560, 1600),
        (1920, 1280),
        (3440, 1440),
        (2048, 1536),
    ] {
        let mut modes = desktop_modes(native);
        assert_eq!(modes.next(), Some(native));
        for (w, h) in modes {
            assert!(w < native.0 && h < native.1, "{native:?}: {w}x{h}");
            let skew =
                (u64::from(w) * u64::from(native.1)).abs_diff(u64::from(h) * u64::from(native.0));
            assert!(skew * 100 <= u64::from(h) * u64::from(native.0));
        }
    }
}
