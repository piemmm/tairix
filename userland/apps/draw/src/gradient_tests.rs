//! Gradients fitted to boxes, turned, and their stops edited.

use alloc::sync::Arc;
use alloc::vec;

use tairix_bezier::{Affine, Bounds, Point};
use tairix_colour::Rgba;
use tairix_drawing::limits::MAX_STOPS;
use tairix_drawing::{DrawingError, GradientKind, Paint, Spread, Stop, Style};

use super::{
    added, colour_at, first_stops, fitted, fitted_style, form_of, moved, normalised, removed,
    reversed, Form,
};

const BLACK: Rgba = Rgba::new(0, 0, 0, 255);
const WHITE: Rgba = Rgba::new(255, 255, 255, 255);

fn ends() -> vec::Vec<Stop> {
    vec![
        Stop {
            offset: 0.0,
            colour: BLACK,
        },
        Stop {
            offset: 1.0,
            colour: WHITE,
        },
    ]
}

fn near(a: Point, b: Point) -> bool {
    (a - b).length() < 1e-9
}

fn linear_ends(form: Form, bounds: Bounds) -> (Point, Point) {
    match fitted(form, ends(), Spread::Pad, bounds).kind {
        GradientKind::Linear { start, end } => (start, end),
        GradientKind::Radial { .. } => panic!("a linear gradient"),
    }
}

#[test]
fn a_linear_gradient_crosses_its_box_from_corner_to_farthest_corner() {
    let wide = Bounds::new(Point::new(0.0, 0.0), Point::new(100.0, 50.0));
    let (start, end) = linear_ends(Form::Linear(0.0), wide);
    assert!(near(start, Point::new(0.0, 25.0)) && near(end, Point::new(100.0, 25.0)));
    let (start, end) = linear_ends(Form::Linear(90.0), wide);
    assert!(
        near(start, Point::new(50.0, 50.0)) && near(end, Point::new(50.0, 0.0)),
        "anticlockwise runs up the page: {start:?} {end:?}"
    );
    let (start, end) = linear_ends(Form::Linear(45.0), wide);
    let along = (end - start) / (end - start).length();
    let reach = |corner: Point| (corner - start).dot(along) / (end - start).length();
    assert!(
        (reach(Point::new(0.0, 50.0))).abs() < 1e-9,
        "offset 0 at one corner"
    );
    assert!(
        (reach(Point::new(100.0, 0.0)) - 1.0).abs() < 1e-9,
        "1 at the far one"
    );
}

#[test]
fn a_radial_gradient_reaches_from_the_centre_to_the_corners() {
    let square = Bounds::new(Point::new(10.0, 10.0), Point::new(40.0, 50.0));
    let GradientKind::Radial {
        centre,
        radius,
        focus,
    } = fitted(Form::Radial, ends(), Spread::Pad, square).kind
    else {
        panic!("a radial gradient");
    };
    assert!(near(centre, Point::new(25.0, 30.0)) && near(focus, centre));
    assert!(
        (radius - 25.0).abs() < 1e-9,
        "half of the 30 by 40 diagonal"
    );
}

#[test]
fn an_object_with_no_extent_still_gets_a_gradient_that_runs() {
    let flat = Bounds::new(Point::new(5.0, 5.0), Point::new(5.0, 5.0));
    let (start, end) = linear_ends(Form::Linear(0.0), flat);
    assert!((end - start).length() > 0.0);
    let gradient = fitted(Form::Radial, ends(), Spread::Pad, Bounds::EMPTY);
    assert!(gradient.check().is_ok());
}

#[test]
fn a_gradients_angle_is_read_back_through_its_transform() {
    let wide = Bounds::new(Point::new(0.0, 0.0), Point::new(100.0, 50.0));
    let mut gradient = fitted(Form::Linear(30.0), ends(), Spread::Pad, wide);
    let Form::Linear(angle) = form_of(&gradient) else {
        panic!("linear");
    };
    assert!((angle - 30.0).abs() < 1e-9, "{angle}");
    // A clockwise quarter turn on the page is a quarter less anticlockwise.
    gradient.transform = Affine::rotate_degrees(90.0);
    let Form::Linear(turned) = form_of(&gradient) else {
        panic!("linear");
    };
    assert!((turned + 60.0).abs() < 1e-9, "{turned}");
    assert_eq!(
        form_of(&fitted(Form::Radial, ends(), Spread::Pad, wide)),
        Form::Radial
    );
    assert!((normalised(540.0) - 180.0).abs() < 1e-9);
    assert!((normalised(-190.0) - 170.0).abs() < 1e-9);
}

#[test]
fn a_stop_added_takes_the_colour_the_gradient_has_there() {
    let (stops, at) = added(&ends(), 0.5).expect("added");
    assert_eq!(at, 1);
    assert_eq!(stops.len(), 3);
    let grey = stops[1].colour;
    assert!((127..=128).contains(&grey.r) && grey.r == grey.g && grey.g == grey.b);
    assert_eq!(colour_at(&stops, 0.5), Some(grey), "the look is unchanged");
    let full = vec![
        Stop {
            offset: 0.5,
            colour: BLACK,
        };
        MAX_STOPS
    ];
    assert_eq!(added(&full, 0.25).err(), Some(DrawingError::TooComplex));
}

#[test]
fn a_stop_moves_only_as_far_as_its_neighbours() {
    let (stops, _) = added(&ends(), 0.5).expect("added");
    let held = moved(&stops, 1, 2.0).expect("moved");
    assert!((held[1].offset - 1.0).abs() < 1e-12, "not past the end");
    let (four, at) = added(&stops, 0.25).expect("added");
    assert_eq!(at, 1);
    let blocked = moved(&four, 1, 0.9).expect("moved");
    assert!(
        (blocked[1].offset - 0.5).abs() < 1e-12,
        "not past the next, at 0.5"
    );
    let back = moved(&four, 2, 0.1).expect("moved");
    assert!(
        (back[2].offset - 0.25).abs() < 1e-12,
        "nor before the one before"
    );
}

#[test]
fn a_gradient_keeps_its_two_ends_and_reverses_whole() {
    assert_eq!(removed(&ends(), 0).expect("answered"), None);
    let (three, _) = added(&ends(), 0.25).expect("added");
    let two = removed(&three, 1).expect("answered").expect("removed");
    assert_eq!(two, ends());
    let back = reversed(&three).expect("reversed");
    assert_eq!(back[0].colour, WHITE);
    assert!((back[1].offset - 0.75).abs() < 1e-12);
    assert_eq!(back[2].colour, BLACK);
}

#[test]
fn a_first_gradient_runs_from_the_colour_to_the_end_it_stands_out_against() {
    assert_eq!(first_stops(BLACK).expect("stops")[1].colour, WHITE);
    assert_eq!(first_stops(WHITE).expect("stops")[1].colour, BLACK);
    let yellow = Rgba::new(250, 220, 0, 255);
    assert_eq!(first_stops(yellow).expect("stops")[1].colour, BLACK);
}

#[test]
fn a_style_drawn_with_takes_its_gradient_fitted_to_the_new_object() {
    let unit = Bounds::new(Point::new(0.0, 0.0), Point::new(1.0, 1.0));
    let style = Style {
        fill: Paint::Gradient(Arc::new(fitted(
            Form::Linear(90.0),
            ends(),
            Spread::Reflect,
            unit,
        ))),
        ..Style::default()
    };
    let placed = Bounds::new(Point::new(200.0, 100.0), Point::new(300.0, 300.0));
    let fitted_style = fitted_style(&style, placed).expect("fitted");
    let Paint::Gradient(gradient) = &fitted_style.fill else {
        panic!("a gradient");
    };
    assert_eq!(gradient.stops, ends(), "the stops kept");
    assert_eq!(gradient.spread, Spread::Reflect);
    let GradientKind::Linear { start, end } = gradient.kind else {
        panic!("linear");
    };
    assert!(near(start, Point::new(250.0, 300.0)) && near(end, Point::new(250.0, 100.0)));
}

#[test]
fn choosing_a_form_keeps_a_gradient_already_of_it_and_starts_one_from_a_colour() {
    let unit = || Bounds::new(Point::new(0.0, 0.0), Point::new(10.0, 10.0));
    let linear = super::formed(&Paint::Colour(BLACK), Form::Linear(0.0), &unit).expect("formed");
    let Paint::Gradient(gradient) = &linear else {
        panic!("a gradient");
    };
    assert_eq!(gradient.stops[0].colour, BLACK);
    assert_eq!(gradient.stops[1].colour, WHITE);
    let again = super::formed(&linear, Form::Linear(0.0), &unit).expect("formed");
    assert_eq!(again, linear, "already linear at that angle: kept");
    let radial = super::formed(&linear, Form::Radial, &unit).expect("formed");
    let Paint::Gradient(round) = &radial else {
        panic!("a gradient");
    };
    assert!(matches!(round.kind, GradientKind::Radial { .. }));
    assert_eq!(round.stops, gradient.stops, "its stops kept");
}

#[test]
fn new_stops_keep_where_a_gradient_lies_and_fit_one_where_there_was_none() {
    let moved_box = || Bounds::new(Point::new(100.0, 0.0), Point::new(200.0, 10.0));
    let mut placed = fitted(Form::Linear(0.0), ends(), Spread::Pad, moved_box());
    placed.transform = Affine::translate(5.0, 5.0);
    let painted = Paint::Gradient(Arc::new(placed.clone()));
    let (three, _) = added(&ends(), 0.5).expect("added");
    let restopped = super::restopped(&painted, (&three, Spread::Repeat, Form::Radial), &moved_box)
        .expect("restopped");
    let Paint::Gradient(kept) = restopped else {
        panic!("a gradient");
    };
    assert_eq!((kept.kind, kept.transform), (placed.kind, placed.transform));
    assert_eq!((kept.stops.len(), kept.spread), (3, Spread::Repeat));
    let fresh = super::restopped(
        &Paint::None,
        (&three, Spread::Pad, Form::Radial),
        &moved_box,
    )
    .expect("restopped");
    let Paint::Gradient(fresh) = fresh else {
        panic!("a gradient");
    };
    assert!(matches!(fresh.kind, GradientKind::Radial { .. }));
}

#[test]
fn a_gradient_is_laid_along_a_line_or_about_its_start() {
    let line = (Point::new(10.0, 10.0), Point::new(40.0, 50.0));
    let laid = super::laid(&Paint::Colour(WHITE), line, false).expect("laid");
    let Paint::Gradient(linear) = &laid else {
        panic!("a gradient");
    };
    assert_eq!(
        linear.kind,
        GradientKind::Linear {
            start: line.0,
            end: line.1
        }
    );
    assert_eq!(linear.stops[1].colour, BLACK, "from white to black");
    let round = super::laid(&laid, line, true).expect("laid");
    let Paint::Gradient(radial) = &round else {
        panic!("a gradient");
    };
    let GradientKind::Radial {
        centre,
        radius,
        focus,
    } = radial.kind
    else {
        panic!("radial");
    };
    assert!(near(centre, line.0) && near(focus, line.0) && (radius - 50.0).abs() < 1e-9);
    assert_eq!(radial.stops, linear.stops, "its stops kept");
    let again = super::laid(&round, line, false).expect("laid");
    let Paint::Gradient(again) = again else {
        panic!("a gradient");
    };
    assert!(
        matches!(again.kind, GradientKind::Radial { .. }),
        "a radial gradient stays radial"
    );
}
