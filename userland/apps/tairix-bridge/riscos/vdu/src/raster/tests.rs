use super::*;

/// Everything within a thousand pixels of the centre.
const NEAR: View = View {
    left: -1000,
    bottom: -1000,
    right: 1000,
    top: 1000,
};

fn rows(bottom: i32, top: i32) -> View {
    View {
        left: i32::MIN,
        bottom,
        right: i32::MAX,
        top,
    }
}

/// Every pixel of a walk, in order from its first point.
fn walked(walk: &Walk) -> Vec<Point> {
    (0..walk.len()).map(|place| walk.at(place)).collect()
}

/// Each row's pixels among runs.
fn pixels(runs: impl IntoIterator<Item = Span>) -> Vec<Point> {
    runs.into_iter()
        .flat_map(|(y, left, right)| (left..=right).map(move |x| (x, y)))
        .collect()
}

/// The lines of a deterministic sweep: every direction and length up to a
/// few dozen pixels, from a few origins.
fn sweep() -> impl Iterator<Item = (Point, Point)> {
    [(0, 0), (-7, 3), (12, -5)].into_iter().flat_map(|a| {
        (-23..=23).step_by(2).flat_map(move |dx| {
            (-17..=17)
                .step_by(3)
                .map(move |dy| (a, (a.0 + dx, a.1 + dy)))
        })
    })
}

#[test]
fn a_line_is_walked_from_its_left_or_lower_end_but_given_in_order() {
    assert_eq!(
        walked(&Walk::new((0, 0), (4, 2))),
        [(0, 0), (1, 0), (2, 1), (3, 1), (4, 2)]
    );
    assert_eq!(
        walked(&Walk::new((4, 2), (0, 0))),
        [(4, 2), (3, 1), (2, 1), (1, 0), (0, 0)],
        "the same pixels backward"
    );
    assert_eq!(
        walked(&Walk::new((0, 0), (1, 3))),
        [(0, 0), (0, 1), (1, 2), (1, 3)]
    );
    assert_eq!(walked(&Walk::new((5, 5), (5, 5))), [(5, 5)]);
}

#[test]
fn a_line_steps_its_minor_axis_as_the_error_passes_half() {
    for (a, b) in sweep() {
        let (dx, dy) = ((b.0 - a.0).abs(), (b.1 - a.1).abs());
        let reversed = (dx >= dy && b.0 < a.0) || (dy > dx && b.1 < a.1);
        let (from, to) = if reversed { (b, a) } else { (a, b) };
        let (sx, sy) = ((to.0 - from.0).signum(), (to.1 - from.1).signum());
        let (major, minor) = (dx.max(dy), dx.min(dy));
        let (mut x, mut y, mut error) = (from.0, from.1, 2 * minor - major);
        let mut stepped = Vec::new();
        for _ in 0..=major {
            stepped.push((x, y));
            if error > 0 {
                if dx >= dy {
                    y += sy;
                } else {
                    x += sx;
                }
                error -= 2 * major;
            }
            error += 2 * minor;
            if dx >= dy {
                x += sx;
            } else {
                y += sy;
            }
        }
        if reversed {
            stepped.reverse();
        }
        assert_eq!(walked(&Walk::new(a, b)), stepped, "{a:?} to {b:?}");
    }
}

#[test]
fn a_line_within_a_window_is_the_part_of_it_there() {
    let windows = [
        Rect {
            left: -3,
            bottom: -2,
            right: 6,
            top: 4,
        },
        Rect {
            left: 10,
            bottom: -20,
            right: 10,
            top: 20,
        },
        Rect {
            left: -30,
            bottom: 0,
            right: 30,
            top: 0,
        },
        Rect {
            left: 100,
            bottom: 100,
            right: 200,
            top: 200,
        },
    ];
    for (a, b) in sweep() {
        let walk = Walk::new(a, b);
        for window in windows {
            let expected: Vec<(u64, Point)> = (0..walk.len())
                .map(|place| (place, walk.at(place)))
                .filter(|&(_, p)| window.holds(p))
                .collect();
            assert_eq!(
                walk.within(window).collect::<Vec<_>>(),
                expected,
                "{a:?} to {b:?} in {window:?}"
            );
        }
    }
}

#[test]
fn a_line_s_row_is_its_pixels_there_however_many() {
    for (a, b) in sweep() {
        let walk = Walk::new(a, b);
        let all = walked(&walk);
        for y in -25..=25 {
            let xs = all.iter().filter(|p| p.1 == y).map(|p| p.0);
            let expected = xs.clone().min().zip(xs.max());
            assert_eq!(walk.row(y), expected, "{a:?} to {b:?}, row {y}");
            let half = (walk.len() - 1) / 2;
            let xs = all[..=usize::try_from(half).unwrap()]
                .iter()
                .filter(|p| p.1 == y)
                .map(|p| p.0);
            let expected = xs.clone().min().zip(xs.max());
            assert_eq!(
                walk.row_within(y, (0, half)),
                expected,
                "{a:?} to {b:?}, row {y}, the first half"
            );
        }
    }
}

#[test]
fn a_line_far_longer_than_a_window_costs_what_the_window_holds() {
    let walk = Walk::new((-(1 << 28), -3), (1 << 28, 5));
    let window = Rect {
        left: 0,
        bottom: 0,
        right: 639,
        top: 479,
    };
    let seen: Vec<_> = walk.within(window).collect();
    assert!(!seen.is_empty() && seen.len() <= 640);
    assert!(seen.iter().all(|&(_, p)| window.holds(p)));
}

#[test]
fn a_ray_is_the_line_to_its_point_repeated_beyond_it() {
    for to in [
        (4, 2),
        (-4, -2),
        (1, 3),
        (-1, 3),
        (7, -3),
        (-5, -8),
        (0, 6),
        (-6, 0),
        (3, 3),
        (-17, 5),
        (2, -9),
    ] {
        let short = walked(&Walk::new((0, 0), to));
        let long = walked(&Walk::new((0, 0), (to.0 * 3, to.1 * 3)));
        for (n, &p) in (0u64..).zip(&long) {
            assert_eq!(
                ray_pixel(to, n),
                p,
                "pixel {n} toward {to:?}, as the line three times as long"
            );
        }
        assert_eq!(
            short.as_slice(),
            &long[..short.len()],
            "{to:?}: a longer line starts as the shorter one"
        );
    }
    assert_eq!(ray_pixel((0, 0), 5), (0, 0));
}

#[test]
fn rows_grow_either_way_and_keep_each_row_s_extremes() {
    let mut rows = Rows::default();
    for point in [(5, 3), (1, -2), (9, 3), (4, 6)] {
        rows.add(point).unwrap();
    }
    assert_eq!(rows.get(3), Some((5, 9)));
    assert_eq!(rows.get(0), None, "a row between those reached");
    assert_eq!(rows.get(7), None);
    assert_eq!(
        rows.spans().collect::<Vec<_>>(),
        [(-2, 1, 1), (3, 5, 9), (6, 4, 4)]
    );
}

#[test]
fn a_polygon_fills_between_its_edges_on_each_row_asked_for() {
    let filled = polygon(&[(0, 0), (4, 0), (0, 4)], rows(-10, 10)).unwrap();
    assert_eq!(filled.get(0), Some((0, 4)));
    assert_eq!(filled.get(4), Some((0, 0)));
    assert_eq!(filled.spans().count(), 5);
    let shallow = polygon(&[(-(1 << 28), 0), (1 << 28, 2), (0, 3)], rows(1, 1)).unwrap();
    assert_eq!(
        shallow.spans().count(),
        1,
        "a row's edges are had whatever their length"
    );
    let (left, right) = shallow.get(1).unwrap();
    assert!(left < -(1 << 26) && right > 1 << 26);
}

#[test]
fn a_circle_s_radius_is_softened_by_its_root() {
    assert_eq!(soft_radius2((3, 4), 1, 1), 30);
    assert_eq!(soft_radius2((10, 0), 1, 1), 110);
    assert_eq!(
        soft_radius2((10, 0), 2, 1),
        110,
        "a radius across is not stretched by tall pixels"
    );
    assert_eq!(soft_radius2((0, 5), 2, 1), 110, "but one up is");
    assert_eq!(
        soft_radius2((i32::MIN, i32::MAX), 8, 8),
        u64::MAX,
        "a radius past the range held at its end"
    );
    let round = Round::circle(110, 1, 1);
    assert_eq!(round.top(), 10, "row 10 is within 110");
    assert_eq!(round.span(0), Some((-10, 10)));
    assert_eq!(round.span(4), Some((-9, 9)), "isqrt(110 - 16)");
    assert_eq!(round.span(-4), Some((-9, 9)));
    assert_eq!(round.span(11), None);
    let tall = Round::circle(110, 2, 1);
    assert_eq!(tall.top(), 5, "half the rows in pixels twice as tall");
    assert_eq!(tall.span(2), Some((-9, 9)), "isqrt(110 - 4 * 4)");
}

#[test]
fn a_column_s_top_is_the_highest_row_reaching_it() {
    let round = Round::circle(soft_radius2((9, 4), 1, 1), 1, 1);
    for x in -12..=12 {
        let highest = (0..=round.top()).rev().find(|&k| round.contains((x, k)));
        assert_eq!(round.column_top(x), highest, "column {x}");
    }
}

#[test]
fn an_outline_is_each_row_less_what_both_rows_beside_it_cover() {
    let rounds = [
        Round::circle(110, 1, 1),
        Round::circle(3180, 1, 1),
        Round::circle(4000, 2, 1),
        Round::circle(4000, 1, 2),
        Round::ellipse(25, (10, 75)),
        Round::ellipse(0, (15, 40)),
        Round::ellipse(112, (-85, 73)),
    ];
    for round in rounds {
        let mut expected = Vec::new();
        for (k, left, right) in round.fill(NEAR) {
            expected.extend(
                (left..=right)
                    .filter(|&x| round.on_outline((x, k)))
                    .map(|x| (x, k)),
            );
        }
        assert_eq!(pixels(round.outline(NEAR)), expected);
    }
}

#[test]
fn a_round_is_worked_out_only_across_the_view() {
    let round = Round::circle(soft_radius2((1 << 28, 0), 1, 1), 1, 1);
    let view = View {
        left: -50,
        bottom: 2,
        right: 50,
        top: 4,
    };
    assert!(round.outline(view).all(|(y, _, _)| view.holds(y)));
    assert_eq!(round.fill(view).count(), 3);
    let top = round.top();
    let polar = View {
        left: -50,
        bottom: top - 2,
        right: 50,
        top,
    };
    assert_eq!(
        round.outline(polar).count(),
        3 * 2 - 1,
        "the pole's rows, however wide, are runs"
    );
}

#[test]
fn a_run_reaching_past_a_view_keeps_a_pixel_beside_each_edge() {
    let view = View {
        left: 0,
        bottom: 0,
        right: 9,
        top: 0,
    };
    assert_eq!(view.cut((-100, 100)), (-1, 10));
    assert_eq!(
        view.cut((-100, -50)),
        (-1, -1),
        "a run wholly to one side keeps the pixel beside"
    );
    assert_eq!(view.cut((3, 5)), (3, 5));
}

#[test]
fn an_ellipse_holds_its_width_s_ratio_to_its_height_to_eight_bits() {
    let tall = Round::ellipse(25, (0, 75));
    assert_eq!(tall.span(13), Some((-25, 25)));
    assert_eq!(
        tall.span(14),
        Some((-24, 24)),
        "25/75 held as 85/256, so 24.4"
    );
    assert_eq!(tall.span(-14), Some((-24, 24)));
    assert_eq!(tall.span(74), Some((-4, 4)));
    assert_eq!(tall.span(75), Some((0, 0)));
    let narrow = Round::ellipse(5, (0, 100));
    assert_eq!(narrow.span(28), Some((-4, 5)), "4.5 each way, rounded up");
    assert_eq!(
        narrow.span(-28),
        Some((-5, 4)),
        "and the row below the centre its mirror"
    );
    let round = Round::ellipse(101, (0, 101));
    assert_eq!(
        round.span(10),
        Some((-100, 101)),
        "the root cut to 100.5 at eight bits"
    );
    let vast = Round::ellipse(1 << 28, (0, 1 << 28));
    assert_eq!(
        vast.span(0),
        Some((-(1 << 28), 1 << 28)),
        "the root held past what a word of sixteen bits more holds"
    );
}

#[test]
fn an_ellipse_s_shear_steps_at_sixteen_bits_cut_toward_nought() {
    let slim = Round::ellipse(13, (5, 170));
    assert_eq!(slim.span(164), Some((1, 8)));
    assert_eq!(slim.span(-164), Some((-8, -1)));
    let leaning = Round::ellipse(112, (-85, 73));
    assert_eq!(leaning.span(56), Some((-137, 7)));
    assert_eq!(leaning.span(73), Some((-85, -85)));
    let sliver = Round::ellipse(8, (-46, 108));
    assert_eq!(sliver.span(105), Some((-46, -43)));
}

#[test]
fn an_ellipse_s_rows_meet_where_its_shear_would_part_them() {
    let line = Round::ellipse(0, (15, 40));
    assert_eq!(
        line.span(30),
        Some((11, 12)),
        "11.25 widened to the 12 above"
    );
    assert_eq!(line.span(31), Some((12, 12)));
    assert_eq!(line.span(-30), Some((-12, -11)));
    assert_eq!(line.span(40), Some((15, 15)));
}

#[test]
fn a_flat_or_upside_down_ellipse_is_the_one_it_matches() {
    let flat = Round::ellipse(40, (15, 0));
    assert_eq!(
        (flat.top(), flat.span(0)),
        (0, Some((-40, 40))),
        "a flat ellipse is its centre's row"
    );
    let below = Round::ellipse(40, (10, -30));
    assert_eq!(below, Round::ellipse(40, (-10, 30)));
    assert_eq!(below.span(-30), Some((10, 10)));
    assert_eq!(Round::ellipse(-40, (10, 30)), Round::ellipse(40, (10, 30)));
}
