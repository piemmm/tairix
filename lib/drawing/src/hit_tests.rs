//! Unit tests for hit testing.

use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;

use tairix_bezier::{Affine, Bounds, Path, PathEl, Point, Subpath};
use tairix_colour::Rgba;

use super::{hit, within_area};
use crate::children::Children;
use crate::drawing::{Drawing, Grid, Page, Unit};
use crate::id::Ids;
use crate::node::{Kind, Node};
use crate::shape::{PathShape, RectShape, Shape};
use crate::style::{Paint, Stroke, Style};
use crate::testing::{shared, NoGlyphs};
use crate::text::{Baseline, Text, TextAlign};
use crate::SharedSlice;

fn styled(fill: bool, stroke_width: f64) -> Style {
    Style {
        fill: if fill {
            Paint::Colour(Rgba::new(0, 0, 0, 255))
        } else {
            Paint::None
        },
        stroke: Stroke {
            paint: if stroke_width > 0.0 {
                Paint::Colour(Rgba::new(0, 0, 0, 255))
            } else {
                Paint::None
            },
            width: stroke_width,
            ..Stroke::default()
        },
        ..Style::default()
    }
}

fn shape(ids: &mut Ids, shape: Shape, style: Style) -> Arc<Node> {
    Arc::new(
        Node::new(
            ids.issue(),
            Kind::Shape {
                shape,
                style: Arc::new(style),
            },
        )
        .expect("a shape"),
    )
}

fn rect(ids: &mut Ids, x: f64, style: Style) -> Arc<Node> {
    shape(
        ids,
        Shape::Rect(RectShape {
            x,
            y: 0.0,
            width: 10.0,
            height: 10.0,
            rx: 0.0,
            ry: 0.0,
        }),
        style,
    )
}

fn group(children: Vec<Arc<Node>>, clip: Option<Arc<Node>>, ids: &mut Ids) -> Node {
    Node::new(
        ids.issue(),
        Kind::Group {
            children: Children::from_nodes(children).expect("held"),
            clip,
        },
    )
    .expect("a group")
}

fn drawing(layers: Vec<Node>) -> Drawing {
    Drawing::new(
        Page {
            width: 100.0,
            height: 100.0,
            unit: Unit::Px,
            background: None,
        },
        Children::from_nodes(layers.into_iter().map(Arc::new).collect()).expect("held"),
        SharedSlice::new(),
        Grid::default(),
    )
    .expect("a drawing")
}

fn at(drawing: &Drawing, x: f64, y: f64) -> Option<Vec<usize>> {
    hit(drawing, Point::new(x, y), 0.5, &NoGlyphs).expect("held")
}

#[test]
fn the_topmost_object_under_the_point_is_met() {
    let mut ids = Ids::new();
    let lower = rect(&mut ids, 0.0, styled(true, 0.0));
    let upper = rect(&mut ids, 5.0, styled(true, 0.0));
    let layer = group(vec![lower, upper], None, &mut ids);
    let drawing = drawing(vec![layer]);
    assert_eq!(at(&drawing, 2.0, 5.0), Some(vec![0, 0]));
    assert_eq!(
        at(&drawing, 7.0, 5.0),
        Some(vec![0, 1]),
        "the upper covers the lower"
    );
    assert_eq!(at(&drawing, 50.0, 50.0), None);
}

#[test]
fn an_unfilled_outline_is_met_on_its_line_and_not_inside() {
    let mut ids = Ids::new();
    let ring = rect(&mut ids, 20.0, styled(false, 4.0));
    let bare = rect(&mut ids, 40.0, styled(false, 0.0));
    let drawing = drawing(vec![group(vec![ring, bare], None, &mut ids)]);
    assert_eq!(
        at(&drawing, 21.5, 5.0),
        Some(vec![0, 0]),
        "within half the stroke"
    );
    assert_eq!(at(&drawing, 25.0, 5.0), None, "the hollow inside");
    assert_eq!(
        at(&drawing, 40.3, 5.0),
        Some(vec![0, 1]),
        "an unpainted outline, within tolerance"
    );
}

#[test]
fn a_transformed_or_clipped_group_is_met_where_it_is_drawn() {
    let mut ids = Ids::new();
    let inner = rect(&mut ids, 0.0, styled(true, 0.0));
    let moved = Arc::new(
        group(vec![inner], None, &mut ids)
            .with_transform(Affine::translate(50.0, 50.0))
            .expect("placed"),
    );
    let content = rect(&mut ids, 0.0, styled(true, 0.0));
    let window = shape(
        &mut ids,
        Shape::Rect(RectShape {
            x: 0.0,
            y: 0.0,
            width: 4.0,
            height: 10.0,
            rx: 0.0,
            ry: 0.0,
        }),
        styled(true, 0.0),
    );
    let clipped = Arc::new(group(vec![content], Some(window), &mut ids));
    let drawing = drawing(vec![group(vec![moved, clipped], None, &mut ids)]);
    assert_eq!(at(&drawing, 55.0, 55.0), Some(vec![0, 0, 0]));
    assert_eq!(at(&drawing, 2.0, 5.0), Some(vec![0, 1, 0]));
    assert_eq!(at(&drawing, 7.0, 5.0), None, "outside the clip");
}

#[test]
fn hidden_objects_are_passed_over_and_text_is_met_by_its_box() {
    let mut ids = Ids::new();
    let hidden = Arc::new(rect(&mut ids, 0.0, styled(true, 0.0)).with_visible(false));
    let text = Arc::new(
        Node::new(
            ids.issue(),
            Kind::Text {
                text: Arc::new(Text {
                    content: shared("Words"),
                    family: shared("sans-serif"),
                    size: 10.0,
                    weight: 400,
                    italic: false,
                    letter_spacing: 0.0,
                    align: TextAlign::Start,
                    baseline: Baseline::Lines {
                        origin: Point::new(20.0, 50.0),
                        line_height: 1.25,
                    },
                    runs: SharedSlice::new(),
                }),
                style: Arc::new(styled(true, 0.0)),
            },
        )
        .expect("text"),
    );
    let drawing = drawing(vec![group(vec![hidden, text], None, &mut ids)]);
    assert_eq!(at(&drawing, 5.0, 5.0), None);
    assert_eq!(at(&drawing, 25.0, 47.0), Some(vec![0, 1]));
}

#[test]
fn a_marquee_takes_what_it_holds_or_touches() {
    let mut ids = Ids::new();
    let mut zigzag = Subpath::new(Point::new(0.0, 30.0));
    zigzag.elements.extend([
        PathEl::Line(Point::new(10.0, 40.0)),
        PathEl::Line(Point::new(20.0, 30.0)),
    ]);
    let line = shape(
        &mut ids,
        Shape::Path(Arc::new(
            PathShape::from_path(&Path {
                subpaths: vec![zigzag],
            })
            .expect("a path"),
        )),
        styled(false, 1.0),
    );
    let layer = group(
        vec![rect(&mut ids, 0.0, styled(true, 0.0)), line],
        None,
        &mut ids,
    );
    let children = match layer.kind() {
        Kind::Group { children, .. } => children.clone(),
        _ => unreachable!("a group"),
    };
    let area = Bounds::new(Point::new(-1.0, -1.0), Point::new(12.0, 35.0));
    assert_eq!(
        within_area(&children, Affine::IDENTITY, area, false).expect("held"),
        vec![0]
    );
    assert_eq!(
        within_area(&children, Affine::IDENTITY, area, true).expect("held"),
        vec![0, 1]
    );
}
