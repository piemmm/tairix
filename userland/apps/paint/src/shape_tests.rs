use alloc::vec;
use alloc::vec::Vec;

use super::{line_pixels, Bounds, Point, Shape, ShapeScratch, Span, FX};

/// Every row of `shape`'s bounds within the picture's own quarter of the
/// plane, a coverage a pixel.
fn raster(shape: &Shape, aa: bool) -> (Bounds, Vec<Vec<u8>>) {
    let mut bounds = shape.bounds();
    bounds.x0 = bounds.x0.max(0);
    bounds.y0 = bounds.y0.max(0);
    let width = usize::try_from(bounds.x1 - bounds.x0).expect("wide");
    let mut scratch = ShapeScratch::default();
    let rows = shape.rows(aa, &mut scratch).expect("room");
    let Some(mut rows) = rows else {
        return (
            bounds,
            vec![vec![0; width]; usize::try_from(bounds.y1 - bounds.y0).expect("tall")],
        );
    };
    let x0 = u32::try_from(bounds.x0).expect("on the picture");
    let lines = (bounds.y0..bounds.y1)
        .map(|y| {
            let mut row = vec![0xAAu8; width];
            rows.row(u32::try_from(y).expect("on the picture"), x0, &mut row);
            row
        })
        .collect();
    (bounds, lines)
}

/// How far a traced curve may stray from its own, and a pixel's half
/// diagonal: what an edge pixel may be off by in each mode.
const BAND: i128 = 20;
const HALF_DIAGONAL: i128 = 182;

fn covered(rows: &[Vec<u8>]) -> u64 {
    rows.iter().flatten().map(|&c| u64::from(c)).sum()
}

/// Whole pixels' worth the coverage adds up to.
fn area_of(rows: &[Vec<u8>]) -> f64 {
    f64::from(u32::try_from(covered(rows)).expect("small")) / 255.0
}

/// How many pixels are wholly covered.
fn whole(rows: &[Vec<u8>]) -> usize {
    rows.iter()
        .flatten()
        .fold(0, |count, &c| count + usize::from(c == 255))
}

#[test]
fn a_dab_covers_about_its_area_and_its_centre_wholly() {
    let centre = Point::centre_of(20, 20);
    let radius = 5 * FX;
    let dab = Shape::Capsule {
        a: centre,
        b: centre,
        radius,
    };
    let (bounds, rows) = raster(&dab, true);
    let area = area_of(&rows);
    let exact = core::f64::consts::PI * 25.0;
    assert!((area - exact).abs() < 1.5, "{area} against {exact}");
    let at = |x: i64, y: i64| {
        let row = usize::try_from(y - bounds.y0).expect("in bounds");
        rows[row][usize::try_from(x - bounds.x0).expect("in bounds")]
    };
    assert_eq!(at(20, 20), 255);
    assert_eq!(at(20, 26), 0);
    assert!(
        at(25, 20) > 0 && at(25, 20) < 255,
        "an edge pixel is part covered"
    );
}

#[test]
fn an_aliased_dab_is_whole_pixels_only() {
    let centre = Point::centre_of(10, 10);
    let dab = Shape::Capsule {
        a: centre,
        b: centre,
        radius: 3 * FX,
    };
    let (_, rows) = raster(&dab, false);
    assert!(rows.iter().flatten().all(|&c| c == 0 || c == 255));
    let pixels = whole(&rows);
    assert_eq!(pixels, 29, "the lattice points within three of a centre");
}

#[test]
fn a_thin_line_is_one_pixel_across() {
    let line = Shape::Capsule {
        a: Point::centre_of(0, 5),
        b: Point::centre_of(30, 5),
        radius: FX / 2,
    };
    let (bounds, rows) = raster(&line, false);
    for (y, row) in (bounds.y0..).zip(&rows) {
        let set = whole(core::slice::from_ref(row));
        assert_eq!(set, if y == 5 { 31 } else { 0 }, "row {y}");
    }
}

#[test]
fn a_rectangle_border_leaves_its_middle_empty() {
    let border = Shape::Rect {
        span: Span {
            from: (9, 9),
            to: (2, 2),
        },
        outline: Some(2),
    };
    let (bounds, rows) = raster(&border, true);
    assert_eq!(
        bounds,
        Bounds {
            x0: 2,
            y0: 2,
            x1: 10,
            y1: 10
        }
    );
    let set = whole(&rows);
    assert_eq!(set, 64 - 16);
    assert_eq!(rows[4][4], 0);
    assert_eq!(rows[1][4], 255);
}

#[test]
fn an_ellipse_fills_about_its_area_and_is_symmetric() {
    let oval = Shape::Ellipse {
        span: Span {
            from: (0, 0),
            to: (39, 19),
        },
        outline: None,
    };
    let (_, rows) = raster(&oval, true);
    let area = area_of(&rows);
    let exact = core::f64::consts::PI * 20.0 * 10.0;
    assert!((area - exact).abs() < 3.0, "{area} against {exact}");
    for row in &rows {
        let mirrored: Vec<u8> = row.iter().rev().copied().collect();
        assert_eq!(row, &mirrored, "left and right match");
    }
    let flipped: Vec<Vec<u8>> = rows.iter().rev().cloned().collect();
    assert_eq!(rows, flipped, "top and bottom match");
    assert_eq!(rows[10][20], 255);
    assert_eq!(rows[0][0], 0);
}

#[test]
fn a_ring_is_the_ellipse_less_the_one_inside() {
    let span = Span {
        from: (0, 0),
        to: (99, 79),
    };
    let ring = Shape::Ellipse {
        span,
        outline: Some(3),
    };
    let whole = Shape::Ellipse {
        span,
        outline: None,
    };
    let hole = Shape::Ellipse {
        span: Span {
            from: (3, 3),
            to: (96, 76),
        },
        outline: None,
    };
    let (_, ring) = raster(&ring, true);
    let (_, whole) = raster(&whole, true);
    let (_, hole) = raster(&hole, true);
    let in_hole_box = |y: usize, x: usize| (3..=76).contains(&y) && (3..=96).contains(&x);
    for y in 0..80 {
        for x in 0..100 {
            let inner = if in_hole_box(y, x) {
                hole[y - 3][x - 3]
            } else {
                0
            };
            // One contour less another, rounded once rather than twice.
            let expected = whole[y][x].saturating_sub(inner);
            assert!(
                ring[y][x].abs_diff(expected) <= 1,
                "({x}, {y}): {} against {expected}",
                ring[y][x]
            );
        }
    }
    assert_eq!(ring[40][50], 0, "the middle is open");
    assert_eq!(ring[40][1], 255, "the left side is solid");
}

#[test]
fn a_sliver_of_an_ellipse_still_covers_something() {
    let sliver = Shape::Ellipse {
        span: Span {
            from: (0, 0),
            to: (30, 0),
        },
        outline: None,
    };
    let (_, rows) = raster(&sliver, true);
    assert!(covered(&rows) > 0);
    let (_, aliased) = raster(&sliver, false);
    assert!(aliased[0][15] == 255, "the centre line is inside");
}

#[test]
fn a_square_is_made_from_the_longer_side_in_the_direction_dragged() {
    let span = Span {
        from: (10, 10),
        to: (4, 13),
    }
    .squared();
    assert_eq!(span.to, (4, 16));
}

#[test]
fn a_pencil_line_marks_every_pixel_once_from_start_to_end() {
    let mut marked = Vec::new();
    line_pixels((0, 0), (5, 2), |x, y| marked.push((x, y)));
    assert_eq!(marked.first(), Some(&(0, 0)));
    assert_eq!(marked.last(), Some(&(5, 2)));
    assert_eq!(marked.len(), 6, "one pixel a column along its long axis");
    for pair in marked.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        assert!(
            (a.0 - b.0).abs() <= 1 && (a.1 - b.1).abs() <= 1,
            "connected"
        );
    }
    let mut single = Vec::new();
    line_pixels((3, 3), (3, 3), |x, y| single.push((x, y)));
    assert_eq!(single, vec![(3, 3)]);
}

#[test]
fn bounds_combine_and_clip() {
    let a = Bounds {
        x0: 0,
        y0: 0,
        x1: 5,
        y1: 5,
    };
    let b = Bounds {
        x0: 3,
        y0: 4,
        x1: 9,
        y1: 6,
    };
    assert_eq!(
        a.union(&b),
        Bounds {
            x0: 0,
            y0: 0,
            x1: 9,
            y1: 6
        }
    );
    assert_eq!(
        a.intersection(&b),
        Bounds {
            x0: 3,
            y0: 4,
            x1: 5,
            y1: 5
        }
    );
    let empty = Bounds {
        x0: 1,
        y0: 1,
        x1: 1,
        y1: 9,
    };
    assert!(empty.is_empty());
    assert_eq!(empty.union(&a), a);
    assert_eq!(Point { x: -1, y: 300 }.pixel(), (-1, 1));
}

/// The squared distance from `p` to the segment from `a` to `b`, the
/// capsule's own definition.
fn segment_distance2(p: Point, a: Point, b: Point) -> i128 {
    let (vx, vy) = (i128::from(b.x - a.x), i128::from(b.y - a.y));
    let (wx, wy) = (i128::from(p.x - a.x), i128::from(p.y - a.y));
    let length2 = vx * vx + vy * vy;
    let dot = wx * vx + wy * vy;
    if length2 == 0 || dot <= 0 {
        return wx * wx + wy * wy;
    }
    if dot >= length2 {
        let (ex, ey) = (i128::from(p.x - b.x), i128::from(p.y - b.y));
        return ex * ex + ey * ey;
    }
    let cross = wx * vy - wy * vx;
    cross * cross / length2
}

/// Whether coverage `cover` agrees with a pixel whose centre lies `d` from a
/// curve of radius `r`: a centre test exact but within the tracing band,
/// an area whole well inside and nothing well outside.
fn agrees(cover: u8, d: i128, r: i128, aa: bool) -> bool {
    if aa {
        if d + HALF_DIAGONAL + BAND <= r {
            return cover == 255;
        }
        if d >= r + HALF_DIAGONAL + BAND {
            return cover == 0;
        }
        return true;
    }
    if (d - r).abs() <= BAND {
        return cover == 0 || cover == 255;
    }
    cover == if d <= r { 255 } else { 0 }
}

/// A capsule's rows agree with its own definition at every slope and size,
/// and their area is the capsule's.
#[test]
fn a_capsules_rows_agree_with_its_definition() {
    let ends = [
        (Point { x: 300, y: 400 }, Point { x: 5000, y: 4100 }),
        (Point { x: 4000, y: 300 }, Point { x: 700, y: 3900 }),
        (Point { x: 1000, y: 2000 }, Point { x: 6000, y: 2000 }),
        (Point { x: 2500, y: 100 }, Point { x: 2500, y: 6000 }),
        (Point { x: 2000, y: 2000 }, Point { x: 2000, y: 2000 }),
        (Point { x: 1234, y: 777 }, Point { x: 5432, y: 1500 }),
    ];
    for (a, b) in ends {
        for radius in [FX / 2, FX, 3 * FX, 9 * FX + 77] {
            for aa in [false, true] {
                let shape = Shape::Capsule { a, b, radius };
                let (bounds, rows) = raster(&shape, aa);
                for (y, row) in (bounds.y0..).zip(&rows) {
                    for (x, &cover) in (bounds.x0..).zip(row) {
                        let d2 = segment_distance2(Point::centre_of(x, y), a, b);
                        let d = d2.isqrt();
                        assert!(
                            agrees(cover, d, i128::from(radius), aa),
                            "{a:?}–{b:?} r{radius} aa {aa} ({x}, {y}): {cover} at {d}"
                        );
                    }
                }
                if aa && bounds.x0 > 0 && bounds.y0 > 0 {
                    let real = |v: i64| f64::from(i32::try_from(v).expect("small"));
                    let (r, length) = (
                        real(radius) / 256.0,
                        real(b.x - a.x).hypot(real(b.y - a.y)) / 256.0,
                    );
                    let exact = core::f64::consts::PI * r * r + 2.0 * r * length;
                    let area = area_of(&rows);
                    assert!(
                        (area - exact).abs() < 0.02 * exact + 0.3,
                        "{area} against {exact}"
                    );
                }
            }
        }
    }
}

/// Rectangles exactly, and ellipses by their definition but at the curve,
/// filled and ringed, smoothed or not.
#[test]
fn rectangle_and_ellipse_rows_agree_with_their_definitions() {
    let spans = [
        Span {
            from: (2, 3),
            to: (17, 11),
        },
        Span {
            from: (0, 0),
            to: (4, 30),
        },
        Span {
            from: (5, 5),
            to: (6, 6),
        },
        Span {
            from: (20, 2),
            to: (49, 3),
        },
    ];
    for span in spans {
        for outline in [None, Some(1), Some(3)] {
            for aa in [false, true] {
                let rect = Shape::Rect { span, outline };
                let (bounds, rows) = raster(&rect, aa);
                let (x0, y0, x1, y1) = span.corners();
                for (y, row) in (bounds.y0..).zip(&rows) {
                    for (x, &cover) in (bounds.x0..).zip(row) {
                        let at = |w: i64| {
                            (x0 + w..=x1 - w).contains(&x) && (y0 + w..=y1 - w).contains(&y)
                        };
                        let inside = at(0) && !outline.is_some_and(|w| at(i64::from(w)));
                        assert_eq!(
                            cover,
                            if inside { 255 } else { 0 },
                            "rect {span:?} {outline:?} ({x}, {y})"
                        );
                    }
                }
                let ellipse = Shape::Ellipse { span, outline };
                let (bounds, rows) = raster(&ellipse, aa);
                let (cx, cy) = ((x0 + x1 + 1) * FX / 2, (y0 + y1 + 1) * FX / 2);
                // A pixel centre's distance past an ellipse inset by `inset`,
                // measured along its shorter radius — never more than the
                // true distance — and that radius; `None` where it vanishes.
                let past = |p: Point, inset: i64| {
                    let rx = i128::from((x1 - x0 + 1) * FX / 2 - inset * FX);
                    let ry = i128::from((y1 - y0 + 1) * FX / 2 - inset * FX);
                    (rx > 0 && ry > 0).then(|| {
                        let (dx, dy) = (i128::from(p.x - cx), i128::from(p.y - cy));
                        let reach = (dx * dx * ry * ry + dy * dy * rx * rx).isqrt();
                        let short = rx.min(ry);
                        (reach * short / (rx * ry), short)
                    })
                };
                for (y, row) in (bounds.y0..).zip(&rows) {
                    for (x, &cover) in (bounds.x0..).zip(row) {
                        let p = Point::centre_of(x, y);
                        let Some((d, r)) = past(p, 0) else {
                            continue;
                        };
                        let hole = outline.and_then(|width| past(p, i64::from(width)));
                        let margin = if aa { HALF_DIAGONAL + BAND } else { BAND };
                        let unsure = (d - r).abs() <= margin
                            || hole.is_some_and(|(dh, rh)| (dh - rh).abs() <= margin);
                        if unsure {
                            assert!(
                                aa || cover == 0 || cover == 255,
                                "ellipse {span:?} ({x}, {y}): {cover}"
                            );
                            continue;
                        }
                        let inside = d < r && hole.is_none_or(|(dh, rh)| dh >= rh);
                        assert_eq!(
                            cover,
                            if inside { 255 } else { 0 },
                            "ellipse {span:?} {outline:?} aa {aa} ({x}, {y})"
                        );
                    }
                }
            }
        }
    }
}

/// A box with rounded corners covers its box less what each corner's curve
/// leaves out, symmetric about both its middles, and its border's hollow is
/// rounded the same less its width.
#[test]
fn a_rounded_box_leaves_its_corners_out_symmetrically() {
    let span = Span {
        from: (2, 2),
        to: (41, 31),
    };
    let rounded = Shape::Rounded {
        span,
        outline: None,
        radius: 8,
    };
    let (_, rows) = raster(&rounded, true);
    let area = area_of(&rows);
    let exact = 40.0 * 30.0 - (4.0 - core::f64::consts::PI) * 64.0;
    assert!((area - exact).abs() < 1.0, "{area} against {exact}");
    for (top, bottom) in rows.iter().zip(rows.iter().rev()) {
        assert_eq!(top, bottom, "symmetric top to bottom");
        let mirrored: Vec<u8> = top.iter().rev().copied().collect();
        assert_eq!(top, &mirrored, "and side to side");
    }
    assert_eq!(rows[0][0], 0, "the corner pixel is left out");
    assert_eq!(rows[0][20], 255, "the top edge between corners is whole");
    let (_, hard) = raster(&rounded, false);
    assert!(hard.iter().flatten().all(|&c| c == 0 || c == 255));
    let border = Shape::Rounded {
        span,
        outline: Some(3),
        radius: 8,
    };
    let (_, ring) = raster(&border, true);
    assert_eq!(ring[15][20], 0, "hollow in the middle");
    assert_eq!(ring[15][1], 255, "the border's side is whole");
    let square = Shape::Rounded {
        span,
        outline: None,
        radius: 0,
    };
    let plain = area_of(&raster(&square, true).1);
    assert!(
        (plain - 1200.0).abs() < 1e-9,
        "no radius is the plain box: {plain}"
    );
}
