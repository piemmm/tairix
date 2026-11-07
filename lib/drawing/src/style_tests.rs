//! Unit tests for styles.

use alloc::sync::Arc;
use alloc::vec;

use tairix_bezier::{Affine, Bounds, FillRule, Point};
use tairix_colour::Rgba;

use super::{
    ArrowShape, Arrowhead, Cap, Gradient, GradientKind, Join, Paint, Spread, Stop, Stroke, Style,
};
use crate::DrawingError;

fn gradient(stops: &[(f64, u8)]) -> Gradient {
    Gradient {
        kind: GradientKind::Linear {
            start: Point::ZERO,
            end: Point::new(100.0, 0.0),
        },
        stops: stops
            .iter()
            .map(|&(offset, alpha)| Stop {
                offset,
                colour: Rgba::new(255, 0, 0, alpha),
            })
            .collect(),
        spread: Spread::Pad,
        transform: Affine::IDENTITY,
    }
}

#[test]
fn a_gradient_holds_its_stops_in_order() {
    assert_eq!(gradient(&[(0.0, 255), (1.0, 0)]).check(), Ok(()));
    assert_eq!(gradient(&[]).check(), Err(DrawingError::Invalid));
    assert_eq!(
        gradient(&[(0.6, 255), (0.4, 0)]).check(),
        Err(DrawingError::Invalid)
    );
    assert_eq!(gradient(&[(1.5, 255)]).check(), Err(DrawingError::Invalid));
    let many: alloc::vec::Vec<(f64, u8)> =
        (0..=crate::limits::MAX_STOPS).map(|_| (0.5, 9)).collect();
    assert_eq!(gradient(&many).check(), Err(DrawingError::TooComplex));
    let mut broken = gradient(&[(0.0, 255)]);
    broken.transform.a = f64::NAN;
    assert_eq!(broken.check(), Err(DrawingError::Invalid));
}

#[test]
fn a_paint_with_nothing_opaque_paints_nothing() {
    assert!(!Paint::None.paints());
    assert!(!Paint::Colour(Rgba::new(1, 2, 3, 0)).paints());
    assert!(Paint::Colour(Rgba::new(1, 2, 3, 1)).paints());
    assert!(!Paint::Gradient(Arc::new(gradient(&[(0.0, 0), (1.0, 0)]))).paints());
    assert!(Paint::Gradient(Arc::new(gradient(&[(0.0, 0), (1.0, 4)]))).paints());
}

#[test]
fn a_stroke_reaches_as_far_as_its_miters_and_arrowheads() {
    let mut stroke = Stroke {
        paint: Paint::Colour(Rgba::new(0, 0, 0, 255)),
        width: 2.0,
        ..Stroke::default()
    };
    assert!(
        (stroke.reach() - 4.0).abs() < 1e-12,
        "a miter of four widths"
    );
    stroke.join = Join::Round;
    assert!((stroke.reach() - 1.0).abs() < 1e-12);
    stroke.cap = Cap::Square;
    assert!((stroke.reach() - core::f64::consts::SQRT_2).abs() < 1e-12);
    stroke.end = Arrowhead {
        shape: ArrowShape::Triangle,
        width: 3.0,
        length: 5.0,
    };
    assert!(
        (stroke.reach() - 10.0).abs() < 1e-12,
        "the arrowhead's length"
    );
    stroke.paint = Paint::None;
    assert!(
        stroke.reach().abs() < 1e-12,
        "an unpainted stroke reaches nowhere"
    );
}

#[test]
fn a_style_refuses_values_a_drawing_cannot_hold() {
    let mut style = Style::default();
    assert_eq!(style.check(), Ok(()));
    style.stroke.width = -1.0;
    assert_eq!(style.check(), Err(DrawingError::Invalid));
    style.stroke.width = 1.0;
    style.stroke.miter_limit = 0.5;
    assert_eq!(style.check(), Err(DrawingError::Invalid));
    style.stroke.miter_limit = 4.0;
    style.stroke.dashes = Arc::from(&[3.0, f64::INFINITY][..]);
    assert_eq!(style.check(), Err(DrawingError::Invalid));
    style.stroke.dashes = Arc::from(vec![1.0; crate::limits::MAX_DASH_ENTRIES + 1]);
    assert_eq!(style.check(), Err(DrawingError::TooComplex));
    assert_eq!(style.fill_rule, FillRule::NonZero);
}

#[test]
fn painted_bounds_hold_an_unpainted_outline() {
    let style = Style {
        fill: Paint::None,
        ..Style::default()
    };
    let outline = Bounds::new(Point::ZERO, Point::new(10.0, 10.0));
    assert_eq!(style.painted_bounds(outline), outline);
}

#[test]
fn a_triangle_arrowhead_points_where_the_path_runs() {
    let head = Arrowhead {
        shape: ArrowShape::Triangle,
        width: 4.0,
        length: 6.0,
    };
    let outline = head
        .outline(Point::new(10.0, 0.0), Point::new(1.0, 0.0), 1.0)
        .expect("it draws");
    assert!(outline.closed);
    let bounds = outline.bounds();
    assert!(
        (bounds.max.x - 16.0).abs() < 1e-12,
        "its tip lies its length past the end"
    );
    assert!(
        (bounds.height() - 4.0).abs() < 1e-12,
        "its base is its width"
    );
    assert_eq!(
        Arrowhead::NONE.outline(Point::ZERO, Point::new(1.0, 0.0), 1.0),
        None
    );
    assert_eq!(
        head.outline(Point::ZERO, Point::ZERO, 1.0),
        None,
        "no direction, no arrow"
    );
}
