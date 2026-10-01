use alloc::vec;
use alloc::vec::Vec;

use super::{capsule_cover, line_pixels, Bounds, Point, Shape, Span, FX};

/// Every row of `shape`'s bounds, a coverage a pixel.
fn raster(shape: &Shape, aa: bool) -> (Bounds, Vec<Vec<u8>>) {
    let bounds = shape.bounds();
    let width = usize::try_from(bounds.x1 - bounds.x0).expect("wide");
    let rows = (bounds.y0..bounds.y1)
        .map(|y| {
            let mut row = vec![0u8; width];
            shape.row(y, bounds.x0, aa, &mut row);
            row
        })
        .collect();
    (bounds, rows)
}

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
            assert_eq!(ring[y][x], whole[y][x].saturating_sub(inner), "({x}, {y})");
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

/// A row measures only the run the capsule can reach, and covers exactly
/// what measuring every pixel of it would, at every slope and size.
#[test]
fn a_capsules_bounded_rows_cover_what_every_pixel_would() {
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
                let width = usize::try_from(bounds.x1 - bounds.x0).expect("wide");
                for (y, row) in (bounds.y0..).zip(&rows) {
                    let every: Vec<u8> = (bounds.x0
                        ..bounds.x0 + i64::try_from(width).expect("wide"))
                        .map(|x| capsule_cover(a, b, radius, x, y, aa))
                        .collect();
                    assert_eq!(row, &every, "{a:?}–{b:?} r{radius} aa {aa} row {y}");
                }
            }
        }
    }
}

/// Each pixel of an ellipse or a rectangle by its definition alone: the
/// centre inside with smoothing off, else the share of its sixteen samples
/// inside, a ring being its ellipse less the one inside it — what the rows
/// must match however they skip the work.
fn by_definition(shape: &Shape, x: i64, y: i64, aa: bool) -> u8 {
    let cover = |inside: &dyn Fn(Point) -> bool| {
        if aa {
            super::sampled(x, y, inside)
        } else if inside(Point::centre_of(x, y)) {
            255
        } else {
            0
        }
    };
    match *shape {
        Shape::Ellipse { span, outline } => {
            let (x0, y0, x1, y1) = span.corners();
            let oval = |inset: i64| {
                let (cx, cy) = ((x0 + x1 + 1) * FX / 2, (y0 + y1 + 1) * FX / 2);
                let rx = (x1 - x0 + 1) * FX / 2 - inset * FX;
                let ry = (y1 - y0 + 1) * FX / 2 - inset * FX;
                move |p: Point| {
                    rx > 0 && ry > 0 && {
                        let dx = i128::from(p.x - cx) * i128::from(ry);
                        let dy = i128::from(p.y - cy) * i128::from(rx);
                        let r = i128::from(rx) * i128::from(ry);
                        dx * dx + dy * dy <= r * r
                    }
                }
            };
            let whole = cover(&oval(0));
            outline.map_or(whole, |w| whole.saturating_sub(cover(&oval(i64::from(w)))))
        }
        Shape::Rect { span, outline } => {
            let (x0, y0, x1, y1) = span.corners();
            let at = |w: i64| (x0 + w..=x1 - w).contains(&x) && (y0 + w..=y1 - w).contains(&y);
            if at(0) && !outline.is_some_and(|w| at(i64::from(w))) {
                255
            } else {
                0
            }
        }
        Shape::Capsule { .. } => unreachable!("capsules are held to their own definition"),
    }
}

/// Rectangles and ellipses, filled and ringed, smoothed or not, read as rows
/// exactly as each pixel's definition says, the row starting anywhere.
#[test]
fn rectangle_and_ellipse_rows_match_each_pixels_definition() {
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
            from: (-7, 2),
            to: (9, 3),
        },
    ];
    for span in spans {
        for outline in [None, Some(1), Some(3)] {
            for shape in [
                Shape::Rect { span, outline },
                Shape::Ellipse { span, outline },
            ] {
                for aa in [false, true] {
                    let bounds = shape.bounds();
                    for from in [bounds.x0 - 5, bounds.x0, bounds.x0 + 3] {
                        for y in (bounds.y0 - 1)..=bounds.y1 {
                            let mut row = vec![0u8; 40];
                            shape.row(y, from, aa, &mut row);
                            let every: Vec<u8> = (from..from + 40)
                                .map(|x| by_definition(&shape, x, y, aa))
                                .collect();
                            assert_eq!(
                                row, every,
                                "{span:?} {outline:?} aa {aa} row {y} from {from}"
                            );
                        }
                    }
                }
            }
        }
    }
}
