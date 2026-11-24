use super::*;

fn axis(scale: i32, eig: u32) -> Axis {
    Axis::new(scale, eig).expect("an axis")
}

#[test]
fn a_scale_of_nought_or_less_has_no_pixels() {
    assert_eq!(Axis::new(0, 1), None);
    assert_eq!(Axis::new(-400, 1), None);
    assert_eq!(Grid::of((400, 0), (1, 1)), None);
}

#[test]
fn os_units_are_floored_to_their_pixel_as_risc_os_5_30_takes_a_start() {
    let mode_28 = axis(400, 1);
    assert_eq!(mode_28.os_units(111), 110 * 400);
    assert_eq!(mode_28.os_units(-1), -2 * 400, "down, not toward nought");
    assert_eq!(axis(400, 3).os_units(111), 104 * 400);
}

#[test]
fn a_glyph_is_put_at_the_quarter_of_a_pixel_its_pen_is_in() {
    let mode_28 = axis(400, 1);
    assert_eq!(mode_28.quarter(101_100), 505, "101100 mp is 126.375 pixels");
    assert_eq!(mode_28.quarter(-900), -5, "down, not toward nought");
    assert_eq!(axis(400, 2).quarter(101_100), 252);
    assert_eq!(Axis::quarter_parts(505), 505 * 128);
}

#[test]
fn an_outline_s_512ths_of_a_pixel_are_draw_s_units_halves_up() {
    let units = |eig: u32, parts: i64| axis(400, eig).draw_units(parts);
    assert_eq!(
        [units(0, 447), units(0, -443), units(0, 640)],
        [224, -221, 320]
    );
    assert_eq!(
        [units(1, 319), units(2, 159), units(3, 78)],
        [319, 318, 312]
    );
}

#[test]
fn a_translation_is_the_nearest_512th_of_a_pixel() {
    let parts = |eig: u32, mp: i64| axis(400, eig).parts(mp);
    assert_eq!(
        [12, -12, 25, 7, -7].map(|mp| parts(1, mp)),
        [8, -8, 16, 4, -4]
    );
    assert_eq!([11, 3, -3].map(|mp| parts(2, mp)), [4, 1, -1]);
    assert_eq!([13, -12, -13].map(|mp| parts(0, mp)), [17, -15, -17]);
}

#[test]
fn the_screen_s_dots_follow_the_scale_in_whole_dots() {
    assert_eq!(
        [axis(400, 1).dots(), axis(400, 3).dots()],
        [90, 22],
        "180 >> 3 is 22"
    );
    assert_eq!(
        [axis(333, 1).dots(), axis(800, 1).dots()],
        [108, 45],
        "72000 / 333 is 216"
    );
    assert_eq!(axis(100_000, 0).dots(), 1, "never none");
    let grid = Grid::of((400, 400), (1, 2)).expect("a grid");
    assert_eq!(grid.resolution((0, 0)), (90, 45));
    assert_eq!(
        grid.resolution((180, -90)),
        (180, 90),
        "a negative one its magnitude"
    );
}

#[test]
fn a_rub_out_corner_is_the_nearest_pixel_boundary() {
    let mode_28 = axis(400, 1);
    assert_eq!(
        [100_003, 130_003, 189_003].map(|mp| mode_28.nearest(mp)),
        [125, 163, 236]
    );
    assert_eq!(axis(400, 2).nearest(100_003), 63, "62.5 pixels, halves up");
    assert_eq!(mode_28.pixel_draw_units(125), 64000);
}

#[test]
fn an_underline_s_millipoints_are_draw_s_units_floored_at_the_scale() {
    assert_eq!(axis(400, 2).draw_units_of(101_103), 64705, "not a pixel's");
    assert_eq!(axis(800, 1).draw_units_of(189_318), 60581);
    assert_eq!(axis(400, 1).draw_units_of(-1), -1);
}

#[test]
fn far_points_saturate_rather_than_wrap() {
    let far = axis(i32::MAX, MOST_EIG);
    assert_eq!(far.pixel_draw_units(i64::MAX), i32::MAX);
    assert_eq!(far.draw_units(i64::MIN), i32::MIN);
    assert_eq!(axis(1, 0).quarter(i64::MAX), i64::MAX);
}
