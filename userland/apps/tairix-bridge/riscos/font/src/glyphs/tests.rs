use super::*;
use crate::outlines::Point as Design;

/// A font at 16 points and 90 dots an inch: a pixel is 50 thousandths of
/// an em.
const SIXTEEN_POINT: Placing = Placing {
    points: (256, 256),
    transform: Transform::IDENTITY,
    resolution: (90, 90),
    shift: (0.0, 0.0),
};

fn design(fill: &[outlines::Element], strokes: &[outlines::Element]) -> Traced {
    let glyph = outlines::Glyph {
        fill: fill.to_vec(),
        strokes: strokes.to_vec(),
        bbox: None,
    };
    trace(
        &Outline::Design {
            glyph,
            design_size: 1000,
        },
        &SIXTEEN_POINT,
    )
}

fn rect(x0: i32, y0: i32, x1: i32, y1: i32) -> [outlines::Element; 4] {
    let p = |x, y| Design { x, y };
    [
        outlines::Element::Move(p(x0, y0)),
        outlines::Element::Line(p(x1, y0)),
        outlines::Element::Line(p(x1, y1)),
        outlines::Element::Line(p(x0, y1)),
    ]
}

fn levels(traced: &Traced, whole: bool) -> Bitmap {
    let reach = traced.reach().expect("a reach");
    traced
        .levels(&reach, FillRule::EvenOdd, whole, &mut Scratch::default())
        .expect("room")
        .expect("levels")
}

#[test]
fn a_rectangle_on_pixel_edges_covers_its_pixels_wholly() {
    let traced = design(&rect(0, 0, 200, 400), &[]);
    assert_eq!(
        traced.reach(),
        Some(Reach {
            left: 0,
            bottom: 0,
            right: 4,
            top: 8
        })
    );
    let bitmap = levels(&traced, false);
    assert_eq!((bitmap.left, bitmap.top, bitmap.width), (0, 7, 4));
    assert!(bitmap.levels.iter().all(|&level| level == WHOLE));
}

#[test]
fn an_edge_through_a_pixel_s_middle_half_covers_it() {
    let bitmap = levels(&design(&rect(25, 0, 75, 50), &[]), false);
    assert_eq!(bitmap.width, 2);
    assert_eq!(
        bitmap.levels,
        [8, 8],
        "half a pixel each side, rounded to the nearer level"
    );
}

#[test]
fn in_whole_pixels_a_pixel_is_set_where_its_centre_is_inside() {
    let p = |x, y| Design { x, y };
    let triangle = [
        outlines::Element::Move(p(0, 0)),
        outlines::Element::Line(p(300, 0)),
        outlines::Element::Line(p(0, 300)),
    ];
    let bitmap = levels(&design(&triangle, &[]), true);
    let rows: alloc::vec::Vec<usize> = bitmap
        .levels
        .chunks(bitmap.width)
        .map(|row| {
            row.iter()
                .fold(0, |n, &level| n + usize::from(level == WHOLE))
        })
        .collect();
    assert_eq!(
        rows,
        [0, 1, 2, 3, 4, 5],
        "top row first, centres on the slope outside"
    );
    assert!(bitmap
        .levels
        .iter()
        .all(|&level| level == 0 || level == WHOLE));
}

#[test]
fn a_stroke_is_drawn_a_pixel_wide_over_the_fill() {
    let p = |x, y| Design { x, y };
    let stroke = [
        outlines::Element::Move(p(0, 225)),
        outlines::Element::Line(p(400, 225)),
    ];
    let traced = design(&[], &stroke);
    let bitmap = levels(&traced, false);
    let row_of = |y: i32| usize::try_from(bitmap.top - y).expect("within") * bitmap.width;
    let column_of = |x: i32| usize::try_from(x - bitmap.left).expect("within");
    let line = &bitmap.levels[row_of(4) + column_of(0)..row_of(4) + column_of(8)];
    assert_eq!(
        line, [WHOLE; 8],
        "row 4 from its left to its right, ends square"
    );
    let elsewhere: u32 = bitmap
        .levels
        .iter()
        .map(|&level| u32::from(level))
        .sum::<u32>()
        - 8 * u32::from(WHOLE);
    assert_eq!(elsewhere, 0);
}

#[test]
fn the_matrix_and_the_resolution_carry_the_outline() {
    let glyph = outlines::Glyph {
        fill: rect(0, 0, 200, 400).to_vec(),
        strokes: alloc::vec::Vec::new(),
        bbox: None,
    };
    let outline = Outline::Design {
        glyph,
        design_size: 1000,
    };
    let doubled = Placing {
        transform: Transform::of(crate::identifier::Matrix([131_072, 0, 0, 65_536, 800, 0])),
        ..SIXTEEN_POINT
    };
    let reach = trace(&outline, &doubled).reach();
    assert_eq!(
        reach,
        Some(Reach {
            left: 1,
            bottom: 0,
            right: 9,
            top: 8
        }),
        "twice as wide, a pixel's millipoints across"
    );
    let finer = Placing {
        resolution: (180, 90),
        ..SIXTEEN_POINT
    };
    assert_eq!(trace(&outline, &finer).reach().map(|r| r.right), Some(8));
}

#[test]
fn a_face_s_outline_in_ems_traces_as_its_thousandths() {
    let mut path = tairix_bezier::Path::new();
    let mut subpath = tairix_bezier::Subpath::new(Vector::new(0.0, 0.0));
    for (x, y) in [(0.2, 0.0), (0.2, 0.4), (0.0, 0.4)] {
        subpath.elements.push(PathEl::Line(Vector::new(x, y)));
    }
    subpath.closed = true;
    path.subpaths.push(subpath);
    let traced = trace(&Outline::Face(&path), &SIXTEEN_POINT);
    assert_eq!(traced, design(&rect(0, 0, 200, 400), &[]));
}

#[test]
fn draw_s_path_is_the_fill_in_the_units_it_is_mapped_to() {
    let traced = design(&rect(0, 0, 200, 400), &[]);
    let path = traced.draw_path(&|(x, y)| Point::new(rounded(x * 512.0), rounded(y * 512.0)));
    assert_eq!(
        path,
        [
            Element::Move {
                to: Point::new(0, 0),
                winds: true
            },
            Element::Line {
                to: Point::new(2048, 0)
            },
            Element::Line {
                to: Point::new(2048, 4096)
            },
            Element::Line {
                to: Point::new(0, 4096)
            },
        ]
    );
}

#[test]
fn a_reach_is_held_to_another_and_counts_its_pixels() {
    let reach = Reach {
        left: -3,
        bottom: 0,
        right: 5,
        top: 8,
    };
    assert_eq!(reach.area(), 64);
    let window = Reach {
        left: 0,
        bottom: 2,
        right: 100,
        top: 100,
    };
    assert_eq!(
        reach.within(&window),
        Some(Reach {
            left: 0,
            bottom: 2,
            right: 5,
            top: 8
        })
    );
    let far = Reach { left: 10, ..window };
    assert_eq!(reach.within(&far), None);
}
