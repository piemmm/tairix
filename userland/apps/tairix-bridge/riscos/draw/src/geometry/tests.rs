use super::*;

fn m(x_scale: i32, y_shear: i32, x_shear: i32, y_scale: i32, x_shift: i32, y_shift: i32) -> Matrix {
    Matrix {
        a: x_scale,
        b: y_shear,
        c: x_shear,
        d: y_scale,
        e: x_shift,
        f: y_shift,
    }
}

#[test]
fn a_point_rounds_its_exact_sum_half_up_then_adds_the_translation() {
    let half = m(0x8000, 0, 0, 0x8000, 0, 0);
    let cases = [
        ((1, 1), (1, 1)),
        ((1, -1), (1, 0)),
        ((-1, -1), (0, 0)),
        ((3, 5), (2, 3)),
        ((-3, -5), (-1, -2)),
        ((32767, -32769), (16384, -16384)),
        ((0x7FFF_FFFF, 0), (1 << 30, 0)),
        ((i32::MIN, 0), (-(1 << 30), 0)),
    ];
    for ((x, y), (ex, ey)) in cases {
        assert_eq!(
            half.apply(Point::new(x, y)),
            Some(Point::new(ex, ey)),
            "({x}, {y})"
        );
    }
    let rotate = m(0xC000, 0x4000, -0x4000, 0xC000, 0, 0);
    assert_eq!(
        rotate.apply(Point::new(32767, -32769)),
        Some(Point::new(32768, -16385)),
        "one rounding of the whole sum, not one a term"
    );
    let shifted = m(0x18000, 0x8000, 0x8000, -0x18000, 7, -9);
    assert_eq!(
        shifted.apply(Point::new(0x0100_0000, 0x00FF_FFFF)),
        Some(Point::new(33_554_439, -16_777_223))
    );
}

#[test]
fn a_result_outside_a_word_is_refused() {
    let wide = m(0x10001, 0, 0, 0xFFFF, 0, 0);
    assert_eq!(wide.apply(Point::new(0x7FFF_FFFF, 0)), None);
    let moved = m(0x10000, 0, 0, 0x10000, 0x7FFF_FFFF, i32::MIN);
    assert_eq!(
        moved.apply(Point::new(1, 1)),
        None,
        "the translation overflows"
    );
    assert_eq!(
        moved.apply(Point::new(0, 1)),
        Some(Point::new(0x7FFF_FFFF, i32::MIN + 1))
    );
}

#[test]
fn a_bounding_box_corner_cuts_each_term_toward_nought() {
    let turn = m(0xB505, 0xB505, -0xB505, 0xB505, 0, 0);
    assert_eq!(
        turn.apply_truncating(Point::new(100_000, 0)),
        Point::new(70710, 70710)
    );
    assert_eq!(
        turn.apply_truncating(Point::new(0, 80000)),
        Point::new(-56568, 56568)
    );
    assert_eq!(
        turn.apply_truncating(Point::new(100_000, 80000)),
        Point::new(14142, 127_278),
        "each product cut on its own before they are summed"
    );
    let scale = m(0x20000, 0, 0, 0x8000, 1000, -2000);
    assert_eq!(
        scale.apply_truncating(Point::new(1, 2)),
        Point::new(1002, -1999)
    );
}

#[test]
fn the_block_words_round_trip() {
    let matrix = m(1, -2, 3, -4, 5, -6);
    assert_eq!(Matrix::from_words(matrix.words()), matrix);
    assert!(Matrix::IDENTITY.is_identity());
}
