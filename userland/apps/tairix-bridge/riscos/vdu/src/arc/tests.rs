use super::*;

/// Everything within a thousand pixels of the centre.
const EVERY_ROW: View = View {
    left: -1000,
    bottom: -1000,
    right: 1000,
    top: 1000,
};

fn circle(start: Point) -> Circle {
    Circle::through(start, 1, 1)
}

/// Row `y`'s pixels among `spans`.
fn row(spans: &[Span], y: i32) -> Vec<i32> {
    spans
        .iter()
        .filter(|&&(row, _, _)| row == y)
        .flat_map(|&(_, left, right)| left..=right)
        .collect()
}

fn arc_pixels(start: Point, toward: Point) -> Vec<Point> {
    let mut runs = Vec::new();
    arc(&circle(start), start, toward, EVERY_ROW, &mut runs).unwrap();
    runs.iter()
        .flat_map(|&(y, left, right)| (left..=right).map(move |x| (x, y)))
        .collect()
}

#[test]
fn an_arc_ends_where_the_ray_toward_its_end_leaves_the_circle() {
    let pixels = arc_pixels((33, 44), (30, 47));
    assert!(
        pixels.contains(&(29, 47)),
        "the ray's last pixel inside is (29, 46), its column's outline (29, 47)"
    );
    assert!(!pixels.contains(&(28, 47)));
    let pixels = arc_pixels((-6, -2), (5, 3));
    assert!(
        pixels.contains(&(5, 4)),
        "moved up from the ray's (5, 3) to the outline"
    );
    assert!(!pixels.contains(&(4, 5)));
}

#[test]
fn the_arc_runs_anticlockwise_from_its_start_to_its_end() {
    let (start, end) = ((10, 0), (0, 10));
    assert!(on_arc((7, 7), start, end));
    assert!(on_arc((10, 0), start, end), "its start");
    assert!(!on_arc((7, -7), start, end));
    assert!(!on_arc((-7, 7), start, end));
    let around = ((10, 2), (10, -2));
    assert!(
        on_arc((-10, 0), around.0, around.1),
        "nearly all the way round"
    );
    assert!(!on_arc((10, 0), around.0, around.1), "but not the gap");
    assert!(reflex(around.0, around.1));
    assert!(!reflex(start, end));
}

#[test]
fn the_furthest_vectors_are_compared_without_overflowing() {
    let most = (i32::MIN, i32::MIN);
    assert!(!reflex(most, most), "no turn at all");
    assert!(reflex(most, (i32::MAX, i32::MAX)), "pointing opposite ways");
}

#[test]
fn a_quarter_sector_fills_between_its_rays() {
    let spans = sector(&circle((10, 0)), (10, 0), (0, 10), EVERY_ROW).unwrap();
    assert_eq!(row(&spans, 0), (0..=10).collect::<Vec<_>>());
    assert_eq!(row(&spans, 5).first(), Some(&0));
    assert!(row(&spans, -1).is_empty(), "nothing below its start");
}

#[test]
fn a_sector_s_ray_runs_a_pixel_on_where_the_sweep_closes_at_a_corner() {
    let spans = sector(&circle((33, 44)), (33, 44), (30, 47), EVERY_ROW).unwrap();
    assert_eq!(
        row(&spans, 47),
        [29, 30],
        "the end ray's (30, 47) beyond the circle, in the first quadrant"
    );
    assert!(row(&spans, 48).is_empty());
    let spans = sector(&circle((-5, 4)), (-5, 4), (-2, -2), EVERY_ROW).unwrap();
    assert_eq!(row(&spans, -5), [-5, -4], "and in the third");
    let spans = sector(&circle((-20, 40)), (-20, 40), (-40, -20), EVERY_ROW).unwrap();
    assert_eq!(
        row(&spans, 41),
        [-20, -19],
        "the start ray's, in the second"
    );
}

#[test]
fn a_sector_s_ray_stops_at_the_circle_elsewhere() {
    let spans = sector(&circle((50, 25)), (50, 25), (50, 40), EVERY_ROW).unwrap();
    assert_eq!(row(&spans, 35), [44]);
    assert!(
        row(&spans, 36).is_empty(),
        "(45, 36) touches the circle's row but not its column"
    );
    let spans = sector(&circle((23, 12)), (23, 12), (-22, 15), EVERY_ROW).unwrap();
    assert_eq!(
        row(&spans, 15).first(),
        Some(&-21),
        "the end ray's (-22, 15) is in the second quadrant"
    );
}

#[test]
fn a_sector_of_more_than_half_the_circle_leaves_out_the_one_completing_it() {
    let spans = sector(&circle((0, 33)), (0, 33), (87, 50), EVERY_ROW).unwrap();
    assert_eq!(row(&spans, 30), (-14..=0).collect::<Vec<_>>());
    assert_eq!(row(&spans, 20), (-26..=0).collect::<Vec<_>>());
    let spans = sector(&circle((10, 1)), (10, 1), (10, -1), EVERY_ROW).unwrap();
    let parts = spans.iter().filter(|&&(y, _, _)| y == 0).count();
    assert_eq!(parts, 1, "the gap at the start is narrower than a pixel");
}

#[test]
fn a_segment_s_chord_runs_on_to_the_row_nearest_the_true_meeting() {
    let spans = segment(&circle((33, 44)), (33, 44), (30, 47), EVERY_ROW).unwrap();
    assert_eq!(
        row(&spans, 47),
        [27, 28, 29],
        "the chord to (29, 46) carried on to row 47"
    );
    assert!(row(&spans, 48).is_empty());
    let spans = segment(&circle((40, 0)), (40, 0), (59, 81), EVERY_ROW).unwrap();
    assert_eq!(row(&spans, 33), [22, 23]);
    let spans = segment(&circle((40, 0)), (40, 0), (-14, 99), EVERY_ROW).unwrap();
    assert_eq!(
        row(&spans, 40),
        (-6..=6).collect::<Vec<_>>(),
        "not on in the second quadrant"
    );
    assert_eq!(row(&spans, 39), (-4..=10).collect::<Vec<_>>());
}

#[test]
fn a_cut_is_worked_out_only_across_the_view() {
    let big = circle((90_000, 30_000));
    let view = View {
        left: -40,
        bottom: 10,
        right: 40,
        top: 12,
    };
    let spans = sector(&big, (90_000, 30_000), (-50_000, 80_000), view).unwrap();
    assert!(spans.iter().all(|&(y, _, _)| view.holds(y)));
    assert!(!spans.is_empty());
    let mut runs = Vec::new();
    arc(&big, (90_000, 30_000), (0, -100), view, &mut runs).unwrap();
    assert!(runs
        .iter()
        .all(|&(y, left, right)| view.holds(y) && left >= -41 && right <= 41));
}

#[test]
fn a_circle_far_larger_than_its_view_is_cut_quickly_and_fully() {
    let start = (1 << 27, 1 << 26);
    let vast = circle(start);
    let view = View {
        left: -320,
        bottom: -240,
        right: 319,
        top: 239,
    };
    let spans = sector(&vast, start, (-(1 << 26), 1 << 27), view).unwrap();
    let row_200: Vec<_> = spans.iter().filter(|&&(y, _, _)| y == 200).collect();
    assert_eq!(
        row_200,
        [&(200, -100, 320)],
        "from the end ray to past the view's edge, where the start ray is"
    );
    let spans = segment(&vast, start, (-(1 << 26), 1 << 27), view).unwrap();
    assert!(spans.iter().all(|&(y, _, _)| view.holds(y)));
    let mut runs = Vec::new();
    arc(&vast, start, (-(1 << 26), 1 << 27), view, &mut runs).unwrap();
    assert!(runs.is_empty(), "the outline is far beyond the view");
}

#[test]
fn a_run_s_parts_on_the_arc_are_its_pixels_on_it() {
    let points = [
        (10, 0),
        (7, 7),
        (0, 10),
        (-7, 7),
        (-10, 0),
        (-7, -7),
        (0, -10),
        (7, -7),
        (10, 2),
        (10, -2),
        (3, 9),
        (-9, -3),
    ];
    for &start in &points {
        for &end in &points {
            for k in -12..=12 {
                let run = (-14, 14);
                let mut parts = Vec::new();
                on_arc_parts(k, run, start, end, |part| {
                    parts.push(part);
                    Ok(())
                })
                .unwrap();
                let from_parts: Vec<i32> = parts.iter().flat_map(|&(from, to)| from..=to).collect();
                let each: Vec<i32> = (run.0..=run.1)
                    .filter(|&x| on_arc((x, k), start, end))
                    .collect();
                assert_eq!(from_parts, each, "row {k} from {start:?} to {end:?}");
                assert!(
                    parts.windows(2).all(|pair| pair[0].1 + 1 < pair[1].0),
                    "parts are whole runs"
                );
            }
        }
    }
}

#[test]
fn pieces_of_a_row_merge_where_they_meet() {
    let mut out = Vec::new();
    merge(
        4,
        &mut [(5, 7), (1, 2), (3, 4), (9, 9), (8, 6), (6, 6)],
        &mut out,
    )
    .unwrap();
    assert_eq!(
        out,
        [(4, 1, 7), (4, 9, 9)],
        "runs meeting or overlapping join; an empty one is nothing"
    );
    out.clear();
    merge(0, &mut [(1, 3), (5, 7)], &mut out).unwrap();
    assert_eq!(
        out,
        [(0, 1, 3), (0, 5, 7)],
        "a gap of a pixel keeps two runs"
    );
}
