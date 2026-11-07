//! Unit tests for shapes and their paths.

use alloc::sync::Arc;
use alloc::vec;

use core::f64::consts::PI;

use tairix_bezier::{FillRule, Path, PathEl, Point, Subpath};

use super::{
    ArcKind, ArcPart, Contour, EllipseShape, NodeJoin, PathNode, PathShape, RectShape, Shape,
    StarShape,
};
use crate::DrawingError;

fn p(x: f64, y: f64) -> Point {
    Point::new(x, y)
}

fn area(shape: &Shape) -> f64 {
    shape.to_path().expect("traced").signed_area()
}

#[test]
fn a_rectangle_is_its_area_rounded_or_not() {
    let base = RectShape {
        x: 10.0,
        y: 20.0,
        width: 30.0,
        height: 40.0,
        rx: 0.0,
        ry: 0.0,
    };
    assert!(
        (area(&Shape::Rect(base)) - 1200.0).abs() < 1e-9,
        "clockwise as drawn"
    );
    let rounded = Shape::Rect(RectShape {
        rx: 5.0,
        ry: 5.0,
        ..base
    });
    let corners_cut = 4.0 * 25.0 - PI * 25.0;
    assert!((area(&rounded) - (1200.0 - corners_cut)).abs() < 0.05);
    let bounds = rounded.bounds().expect("bounds");
    assert_eq!((bounds.min, bounds.max), (p(10.0, 20.0), p(40.0, 60.0)));
}

#[test]
fn corner_radii_are_held_to_half_the_sides() {
    let pill = Shape::Rect(RectShape {
        x: 0.0,
        y: 0.0,
        width: 20.0,
        height: 10.0,
        rx: 50.0,
        ry: 50.0,
    });
    // Each radius is held to its own half side, as SVG holds them, so these
    // corners meet as the whole ellipse within the rectangle.
    let traced = pill.to_path().expect("traced");
    let want = PI * 10.0 * 5.0;
    assert!(
        (traced.signed_area() - want).abs() < 0.1,
        "{}",
        traced.signed_area()
    );
}

#[test]
fn an_ellipse_and_its_arcs_cover_what_they_should() {
    let whole = EllipseShape {
        centre: p(0.0, 0.0),
        radii: p(10.0, 5.0),
        arc: None,
    };
    assert!((area(&Shape::Ellipse(whole)) - PI * 50.0).abs() < 0.05);
    let pie = Shape::Ellipse(EllipseShape {
        arc: Some(ArcPart {
            start: 0.0,
            end: 90.0,
            kind: ArcKind::Pie,
        }),
        ..whole
    });
    assert!((area(&pie) - PI * 50.0 / 4.0).abs() < 0.05);
    let open = Shape::Ellipse(EllipseShape {
        arc: Some(ArcPart {
            start: 0.0,
            end: 180.0,
            kind: ArcKind::Open,
        }),
        ..whole
    });
    assert!(!open.to_path().expect("traced").subpaths[0].closed);
    // An arc running past a whole turn wraps, as angles do.
    let wrapped = Shape::Ellipse(EllipseShape {
        arc: Some(ArcPart {
            start: 270.0,
            end: 0.0,
            kind: ArcKind::Pie,
        }),
        ..whole
    });
    assert!((area(&wrapped) - PI * 50.0 / 4.0).abs() < 0.05);
}

#[test]
fn a_star_has_its_points_and_a_polygon_its_sides() {
    let star = StarShape {
        centre: p(0.0, 0.0),
        points: 5,
        outer: 10.0,
        inner_ratio: 0.5,
        rotation: 0.0,
        rounding: 0.0,
        polygon: false,
    };
    let traced = Shape::Star(star).to_path().expect("traced");
    assert_eq!(
        traced.subpaths[0].elements.len(),
        9,
        "ten corners, the last closing"
    );
    let top = traced.subpaths[0].start;
    assert!(
        (top.x).abs() < 1e-9 && (top.y + 10.0).abs() < 1e-9,
        "first point straight up"
    );
    assert!(traced.contains(p(0.0, 0.0), FillRule::NonZero));
    let hexagon = Shape::Star(StarShape {
        points: 6,
        polygon: true,
        ..star
    });
    let side = 10.0;
    let want = 1.5 * tairix_util::mathf::sqrt(3.0) * side * side;
    assert!((area(&hexagon) - want).abs() < 1e-9, "{}", area(&hexagon));
    let rounded = Shape::Star(StarShape {
        rounding: 0.5,
        ..star
    });
    assert!(matches!(
        rounded.to_path().expect("traced").subpaths[0].elements[0],
        PathEl::Cubic(..)
    ));
}

#[test]
fn a_path_survives_a_trip_through_contours() {
    let mut subpath = Subpath::new(p(0.0, 0.0));
    subpath.elements = vec![
        PathEl::Line(p(10.0, 0.0)),
        PathEl::Cubic(p(15.0, 0.0), p(20.0, 5.0), p(20.0, 10.0)),
        PathEl::Cubic(p(20.0, 15.0), p(-5.0, 5.0), p(0.0, 0.0)),
    ];
    subpath.closed = true;
    let path = Path {
        subpaths: vec![subpath],
    };
    let shape = PathShape::from_path(&path).expect("read");
    assert_eq!(
        shape.contours[0].nodes.len(),
        3,
        "the closing node is the first"
    );
    assert_eq!(shape.to_path().expect("traced"), path);
}

#[test]
fn handles_in_line_make_a_smooth_node() {
    let mut subpath = Subpath::new(p(0.0, 0.0));
    subpath.elements = vec![
        PathEl::Cubic(p(0.0, 5.0), p(5.0, 10.0), p(10.0, 10.0)),
        PathEl::Cubic(p(15.0, 10.0), p(20.0, 5.0), p(20.0, 0.0)),
        PathEl::Cubic(p(20.0, -5.0), p(14.0, -8.0), p(10.0, -8.0)),
        PathEl::Cubic(p(4.0, -8.0), p(0.0, -10.0), p(-3.0, -12.0)),
    ];
    let shape = PathShape::from_path(&Path {
        subpaths: vec![subpath],
    })
    .expect("read");
    let joins: alloc::vec::Vec<NodeJoin> = shape.contours[0].nodes.iter().map(|n| n.join).collect();
    assert_eq!(
        joins,
        [
            NodeJoin::Corner,
            NodeJoin::Symmetric,
            NodeJoin::Symmetric,
            NodeJoin::Smooth,
            NodeJoin::Corner,
        ]
    );
}

#[test]
fn shapes_refuse_what_a_drawing_cannot_hold() {
    let bad_rect = Shape::Rect(RectShape {
        x: 0.0,
        y: 0.0,
        width: -1.0,
        height: 1.0,
        rx: 0.0,
        ry: 0.0,
    });
    assert_eq!(bad_rect.check(), Err(DrawingError::Invalid));
    let huge = Shape::Ellipse(EllipseShape {
        centre: p(2e9, 0.0),
        radii: p(1.0, 1.0),
        arc: None,
    });
    assert_eq!(huge.check(), Err(DrawingError::Invalid));
    let spiky = Shape::Star(StarShape {
        centre: p(0.0, 0.0),
        points: crate::limits::MAX_STAR_POINTS + 1,
        outer: 1.0,
        inner_ratio: 0.5,
        rotation: 0.0,
        rounding: 0.0,
        polygon: false,
    });
    assert_eq!(spiky.check(), Err(DrawingError::TooComplex));
    let nan_path = Shape::Path(Arc::new(PathShape {
        contours: vec![Contour {
            nodes: vec![PathNode::corner(p(f64::NAN, 0.0))],
            closed: false,
        }],
    }));
    assert_eq!(nan_path.check(), Err(DrawingError::Invalid));
}

#[test]
fn one_node_is_replaced_and_every_other_kept() {
    let shape = PathShape {
        contours: vec![
            Contour {
                nodes: vec![
                    PathNode::corner(p(0.0, 0.0)),
                    PathNode::corner(p(10.0, 0.0)),
                ],
                closed: false,
            },
            Contour {
                nodes: vec![PathNode::corner(p(0.0, 5.0)), PathNode::corner(p(5.0, 5.0))],
                closed: true,
            },
        ],
    };
    let moved = shape
        .with_node(1, 0, PathNode::corner(p(-3.0, 7.0)))
        .expect("replaced");
    assert_eq!(moved.contours[1].nodes[0].point, p(-3.0, 7.0));
    assert_eq!(moved.contours[0], shape.contours[0]);
    assert_eq!(moved.contours[1].nodes[1], shape.contours[1].nodes[1]);
    assert!(moved.contours[1].closed);
    assert_eq!(
        shape.with_node(1, 2, PathNode::corner(p(0.0, 0.0))),
        Err(DrawingError::Invalid)
    );
    assert_eq!(
        shape.with_node(2, 0, PathNode::corner(p(0.0, 0.0))),
        Err(DrawingError::Invalid)
    );
}
