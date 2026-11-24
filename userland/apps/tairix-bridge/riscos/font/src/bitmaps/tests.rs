use alloc::string::String;

use super::*;

#[test]
fn a_size_s_file_is_named_by_its_pixel_sizes_rounded_down() {
    let name = |depth, points, resolution| {
        leaf(depth, points, resolution).map(|leaf| String::from_utf8(leaf).expect("text"))
    };
    assert_eq!(
        name(Depth::Four, (256, 256), (90, 90)).as_deref(),
        Some("f320x320")
    );
    assert_eq!(
        name(Depth::One, (192, 192), (90, 45)).as_deref(),
        Some("b240x120")
    );
    assert_eq!(
        name(Depth::Four, (201, 201), (90, 90)).as_deref(),
        Some("f251x251"),
        "251.25 pixels"
    );
    assert_eq!(
        name(Depth::Four, (256, 256), (180, 45)).as_deref(),
        Some("f640x160")
    );
    assert_eq!(
        name(Depth::Four, (7999, 7999), (90, 90)).as_deref(),
        Some("f9998x9998")
    );
    assert_eq!(
        name(Depth::Four, (8000, 8000), (90, 90)),
        None,
        "10000 pixels has no name"
    );
    assert_eq!(name(Depth::Four, (0, 256), (90, 90)), None);
    assert_eq!(name(Depth::Four, (256, 256), (-90, 90)), None);
}

/// A bitmap from its rows, the top row first.
fn bitmap(left: i32, top: i32, rows: &[&[u8]]) -> Bitmap {
    Bitmap {
        left,
        top,
        width: rows[0].len(),
        levels: rows.concat(),
    }
}

#[test]
fn scaling_down_weighs_each_source_pixel_by_the_share_of_a_pixel_it_covers() {
    // RISC OS 5.30 painting the x90y45 file's 16 point `A` at 12 points: a
    // quarter smaller, every pixel the area-weighted sum, halves up.
    let source = bitmap(1, 3, &[&[10, 11, 12], &[7, 8, 9], &[4, 5, 6], &[1, 2, 3]]);
    let three_quarters = Ratio::of(3, 4).expect("a ratio");
    let scaled = scaled(&source, (three_quarters, three_quarters))
        .expect("room")
        .expect("pixels");
    assert_eq!((scaled.left, scaled.top, scaled.width), (0, 2, 3));
    assert_eq!(scaled.levels, [2, 10, 11, 1, 6, 7, 0, 2, 4]);
}

#[test]
fn scaling_up_by_two_doubles_each_pixel() {
    let source = bitmap(1, 0, &[&[1, 2, 3]]);
    let twice = Ratio::of(2, 1).expect("a ratio");
    let scaled = scaled(&source, (twice, twice))
        .expect("room")
        .expect("pixels");
    assert_eq!((scaled.left, scaled.top, scaled.width), (2, 1, 6));
    assert_eq!(scaled.levels, [1, 1, 2, 2, 3, 3, 1, 1, 2, 2, 3, 3]);
}

#[test]
fn a_scale_past_what_a_glyph_may_span_makes_nothing() {
    let source = bitmap(0, 0, &[&[15]]);
    let one = Ratio::of(1, 1).expect("a ratio");
    let huge = Ratio::of(5000, 1).expect("a ratio");
    assert_eq!(scaled(&source, (huge, one)).expect("room"), None);
    assert_eq!(Ratio::of(0, 1), None, "nought is no scale");
    assert_eq!(Ratio::of(3, -4), None);
    assert_eq!(Ratio::of(12, 16), Ratio::of(3, 4), "in lowest terms");
}

#[test]
fn a_ratio_whose_lowest_terms_pass_a_word_is_no_scale() {
    let most = i64::from(i32::MAX);
    assert_eq!(Ratio::of(most + 1, 1), None);
    assert_eq!(Ratio::of(1, most + 1), None);
    assert!(
        Ratio::of(2 * (most + 1), 2).is_none(),
        "in lowest terms still"
    );
    assert!(Ratio::of(2 * most, 2).is_some());
    let near_one = Ratio::of(most, most - 1).expect("a ratio");
    let source = bitmap(-3, 7, &[&[15, 0, 7], &[1, 2, 3]]);
    let scaled = scaled(&source, (near_one, near_one))
        .expect("room")
        .expect("pixels");
    let inked: Vec<u8> = scaled
        .levels
        .iter()
        .copied()
        .filter(|&level| level != 0)
        .collect();
    assert_eq!(
        inked,
        [15, 7, 1, 2, 3],
        "each pixel its own, the slivers past them none"
    );
}
