use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use super::*;

/// One OS unit's 256ths across a pixel at eigen factor 1.
const PIXEL: i128 = 512;
/// One in 16.16.
const ONE: i128 = 0x1_0000;

/// What the rectangle `corners` of a 12 by 12 sprite mapped onto the
/// parallelogram `points` covers on a 640 by 480 screen: each pixel's source
/// column and row from the top.
fn plotted(points: [i128; 8], corners: [i128; 4]) -> BTreeMap<(i32, i32), (u32, u32)> {
    let window = Rect {
        left: 0,
        bottom: 0,
        right: 639,
        top: 479,
    };
    let mut covered = BTreeMap::new();
    let Some(mut transform) = Transform::of(corners, points, (12, 12), (PIXEL, PIXEL), window)
    else {
        return covered;
    };
    let bounds = transform.bounds;
    let read = transform.source_rows();
    for y in bounds.bottom..=bounds.top {
        if let Some((first, last)) = transform.row(y, bounds.left, bounds.right) {
            for x in first..=last {
                let taken = transform.next();
                assert!(
                    read.contains(&taken.1),
                    "row {} among those read, {read:?}",
                    taken.1
                );
                covered.insert((x, y), taken);
            }
        }
    }
    covered
}

/// The parallelogram the matrix `m` makes of the rectangle `corners` of a
/// sprite at `eig`, as Draw moves its corners, for a matrix that maps each
/// exactly.
fn through(matrix: [i128; 6], corners: [i128; 4], eig: (u32, u32)) -> [i128; 8] {
    let [x_scale, y_shear, x_shear, y_scale, x_shift, y_shift] = matrix;
    let path = corner_path(corners, eig);
    let image = |corner: usize| {
        let along = i128::from(path[3 * corner + 1]);
        let up = i128::from(path[3 * corner + 2]);
        let across = x_scale * along + x_shear * up;
        let rise = y_shear * along + y_scale * up;
        assert!(across % ONE == 0 && rise % ONE == 0, "an exact image");
        [across / ONE + x_shift, rise / ONE + y_shift]
    };
    let ([x0, y0], [x1, y1], [x3, y3]) = (image(0), image(1), image(2));
    [x0, y0, x1, y1, 0, 0, x3, y3]
}

/// [`through`] for the whole sprite at eigen factor 1.
fn matrix(entries: [i128; 6]) -> [i128; 8] {
    through(entries, WHOLE, (1, 1))
}

const WHOLE: [i128; 4] = [0, 12, 12, 0];

#[test]
fn the_identity_at_a_whole_pixel_maps_each_pixel_to_its_own() {
    let covered = plotted(matrix([ONE, 0, 0, ONE, 20 * PIXEL, 20 * PIXEL]), WHOLE);
    assert_eq!(covered.len(), 144);
    for ((x, y), (column, row)) in covered {
        assert_eq!(
            (column, row),
            ((x - 20).cast_unsigned(), (31 - y).cast_unsigned())
        );
    }
}

#[test]
fn a_sprite_s_eigen_factors_size_its_pixels_through_a_matrix() {
    let identity = [ONE, 0, 0, ONE, 20 * PIXEL, 20 * PIXEL];
    let wide = plotted(through(identity, WHOLE, (2, 1)), WHOLE);
    assert_eq!(wide.len(), 288);
    for ((x, y), (column, row)) in wide {
        assert_eq!(
            (column, row),
            (((x - 20) / 2).cast_unsigned(), (31 - y).cast_unsigned())
        );
    }
    let tall = plotted(through(identity, WHOLE, (1, 0)), WHOLE);
    assert_eq!(tall.len(), 72, "half as tall at YEig 0, rows tying down");
    for ((x, y), (column, row)) in tall {
        assert_eq!(
            (column, row),
            ((x - 20).cast_unsigned(), (51 - 2 * y).cast_unsigned())
        );
    }
}

#[test]
fn a_half_pixel_origin_samples_on_its_own_grid_and_spans_its_upper_edge() {
    let covered = plotted(
        matrix([ONE, 0, 0, ONE, 180 * PIXEL + 256, 20 * PIXEL]),
        WHOLE,
    );
    assert_eq!(
        covered.get(&(180, 20)),
        None,
        "a centre on the lower edge is outside"
    );
    assert_eq!(covered.get(&(181, 20)), Some(&(1, 11)));
    assert_eq!(
        covered.get(&(192, 20)),
        Some(&(11, 11)),
        "a centre on the upper edge is in, held to the sprite"
    );
}

#[test]
fn rows_tie_down_and_a_half_pixel_row_origin_spans_its_top() {
    let covered = plotted(
        matrix([ONE, 0, 0, ONE, 500 * PIXEL, 20 * PIXEL + 256]),
        WHOLE,
    );
    assert_eq!(covered.get(&(500, 20)), None);
    assert_eq!(
        covered.get(&(500, 21)),
        Some(&(0, 11)),
        "row 1.0 from the bottom is row 0"
    );
    assert_eq!(covered.get(&(500, 32)), Some(&(0, 0)));
}

#[test]
fn a_mirror_samples_at_its_left_edges() {
    let covered = plotted(matrix([-ONE, 0, 0, ONE, 20 * PIXEL, 60 * PIXEL]), WHOLE);
    let row: Vec<u32> = (8..20).map(|x| covered[&(x, 60)].0).collect();
    assert_eq!(row, [11, 11, 10, 9, 8, 7, 6, 5, 4, 3, 2, 1]);
}

#[test]
fn a_right_angle_turn_counts_rows_from_the_top() {
    let covered = plotted(matrix([0, ONE, -ONE, 0, 200 * PIXEL, 50 * PIXEL]), WHOLE);
    assert_eq!(
        covered.get(&(199, 50)),
        Some(&(0, 11)),
        "source row 0 from the bottom"
    );
    assert_eq!(covered.get(&(188, 50)), Some(&(0, 0)));
    assert_eq!(covered.get(&(199, 61)), Some(&(11, 11)));
}

#[test]
fn a_shear_along_rows_keeps_the_origin_s_grid() {
    let covered = plotted(
        matrix([ONE, 0, ONE / 2, ONE, 20 * PIXEL, 20 * PIXEL]),
        WHOLE,
    );
    let row: Vec<u32> = (20..32).map(|x| covered[&(x, 20)].0).collect();
    assert_eq!(
        row,
        [0, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10],
        "RISC OS 5's first row"
    );
    let row: Vec<u32> = (21..33).map(|x| covered[&(x, 21)].0).collect();
    assert_eq!(row, [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11]);
}

#[test]
fn a_scale_steps_its_columns_in_rounded_16_16() {
    let covered = plotted(
        matrix([ONE * 3 / 4, 0, 0, ONE * 3 / 4, 140 * PIXEL, 140 * PIXEL]),
        WHOLE,
    );
    let columns: Vec<u32> = (140..149).map(|x| covered[&(x, 140)].0).collect();
    assert_eq!(
        columns,
        [0, 1, 2, 3, 5, 6, 7, 9, 10],
        "4/3 rounds to 87381/65536, short of each third"
    );
    let rows: Vec<u32> = (140..149).map(|y| 11 - covered[&(140, y)].1).collect();
    assert_eq!(rows, [0, 1, 3, 4, 5, 7, 8, 9, 11]);
}

#[test]
fn the_parallelogram_form_maps_the_sprite_s_top_left_to_its_first_point() {
    let at = |x: i128, y: i128| [x * PIXEL, y * PIXEL];
    let points: Vec<i128> = [at(180, 72), at(192, 72), at(192, 60), at(180, 60)].concat();
    let parallelogram = plotted(points.try_into().unwrap(), WHOLE);
    let identity = plotted(matrix([ONE, 0, 0, ONE, 180 * PIXEL, 60 * PIXEL]), WHOLE);
    assert_eq!(parallelogram, identity);
}

#[test]
fn a_source_rectangle_clips_without_moving_the_sprite() {
    let part = [1, 1, 4, 3];
    let covered = plotted(
        through([ONE, 0, 0, ONE, 340 * PIXEL, 60 * PIXEL], part, (1, 1)),
        part,
    );
    let mut pixels: Vec<_> = covered.into_iter().collect();
    pixels.sort_unstable();
    assert_eq!(pixels.len(), 6);
    assert_eq!(pixels[0], ((341, 61), (1, 10)));
    assert_eq!(pixels[5], ((343, 62), (3, 9)));
    let thin = [3, 3, 3, 8];
    assert!(
        plotted(through([ONE, 0, 0, ONE, 0, 0], thin, (1, 1)), thin).is_empty(),
        "no width, no plot"
    );
}

#[test]
fn a_singular_or_extreme_parallelogram_draws_nothing_rather_than_failing() {
    assert!(plotted(matrix([ONE, ONE, ONE, ONE, 0, 0]), WHOLE).is_empty());
    let huge = i128::from(i32::MAX);
    let _ = plotted([huge, -huge, -huge, huge, huge, huge, -huge, -huge], WHOLE);
    let _ = plotted([huge, huge, huge - 1, -huge, 0, 0, huge, huge], WHOLE);
}

#[test]
fn exact_division_helpers_round_as_named() {
    assert_eq!(floor_div(-7, 2), Some(-4));
    assert_eq!(floor_div(7, -2), Some(-4));
    assert_eq!(nearest_div(5, 2), Some(3));
    assert_eq!(nearest_div(-5, 2), Some(-2));
    assert_eq!(common_factor(&[12, -18]), Some(6));
    assert_eq!(common_factor(&[0, 0]), Some(1));
    assert_eq!(common_factor(&[-12, 18, 8]), Some(2));
    assert_eq!(
        common_factor(&[i128::MIN, i128::MIN]),
        None,
        "past the signed range"
    );
    assert_eq!(floor_div(i128::MIN, -1), None);
}

#[test]
fn a_transform_too_wide_to_carry_exactly_draws_nothing_rather_than_wrapping() {
    let points = [
        722_263_651,
        1_741_310_710,
        -1_228_469_590,
        -2_028_150_568,
        0,
        0,
        -193,
        157,
    ];
    let corners = [1_966_757_470, -628_653_697, -1_969_186_531, 820_605_320];
    assert!(plotted(points, corners).is_empty());
}

#[test]
fn draw_is_handed_the_corners_held_to_the_sprite_in_their_order() {
    assert_eq!(
        corner_path(WHOLE, (1, 1)),
        [MOVE, 0, 0x1800, MOVE, 0x1800, 0x1800, MOVE, 0, 0, END],
        "the whole sprite's first corner is its top left"
    );
    assert_eq!(
        corner_path(held([-4, -4, 20, 20], (12, 12)), (2, 1)),
        [MOVE, 0, 0, MOVE, 0x3000, 0, MOVE, 0, 0x1800, END]
    );
    assert_eq!(
        held([6, 7, 2, 1], (12, 12)),
        [6, 7, 2, 1],
        "never reordered"
    );
}
