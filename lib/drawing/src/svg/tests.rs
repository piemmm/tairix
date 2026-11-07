//! Unit tests for the SVG reader and writer.

#![allow(
    clippy::float_cmp,
    reason = "the values compared are written and read exactly"
)]

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use core::fmt::Write as _;

use tairix_bezier::{Affine, FillRule, Point};
use tairix_colour::Rgba;

use super::{read, write, ReadError};
use crate::children::Children;
use crate::drawing::{Drawing, Grid, Guide, GuideAxis, Page, Unit};
use crate::id::Ids;
use crate::node::{Kind, Node};
use crate::shape::{
    ArcKind, ArcPart, Contour, EllipseShape, NodeJoin, PathNode, PathShape, RectShape, Shape,
    StarShape,
};
use crate::style::{
    ArrowShape, Arrowhead, Cap, Gradient, GradientKind, Join, Paint, PaintOrder, Spread, Stop,
    Stroke, Style,
};
use crate::text::{Text, TextAlign};
use crate::unkept::Lost;
use crate::DrawingError;

fn black() -> Paint {
    Paint::Colour(Rgba::new(0, 0, 0, 255))
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

fn group(ids: &mut Ids, children: Vec<Arc<Node>>, clip: Option<Arc<Node>>) -> Node {
    Node::new(
        ids.issue(),
        Kind::Group {
            children: Children::from_nodes(children).expect("held"),
            clip,
        },
    )
    .expect("a group")
}

fn page() -> Page {
    Page {
        width: 4.0,
        height: 3.0,
        unit: Unit::In,
        background: Some(Rgba::new(250, 250, 240, 255)),
    }
}

/// A drawing holding one of everything the writer writes.
#[allow(
    clippy::too_many_lines,
    reason = "one of everything, set out in one place"
)]
fn everything(ids: &mut Ids) -> Drawing {
    let gradient = Arc::new(Gradient {
        kind: GradientKind::Linear {
            start: Point::new(10.0, 10.0),
            end: Point::new(90.0, 10.0),
        },
        stops: vec![
            Stop {
                offset: 0.0,
                colour: Rgba::new(255, 0, 0, 255),
            },
            Stop {
                offset: 1.0,
                colour: Rgba::new(0, 0, 255, 128),
            },
        ],
        spread: Spread::Reflect,
        transform: Affine::IDENTITY,
    });
    let filled = Style {
        fill: Paint::Gradient(gradient.clone()),
        ..Style::default()
    };
    let stroked = Style {
        fill: Paint::None,
        fill_rule: FillRule::EvenOdd,
        stroke: Stroke {
            paint: Paint::Colour(Rgba::new(10, 20, 30, 255)),
            width: 2.5,
            join: Join::Round,
            miter_limit: 6.0,
            cap: Cap::Round,
            dashes: Arc::from(&[4.0, 2.0][..]),
            dash_offset: 1.0,
            start: Arrowhead {
                shape: ArrowShape::Circle,
                width: 2.0,
                length: 2.0,
            },
            end: Arrowhead {
                shape: ArrowShape::Triangle,
                width: 3.0,
                length: 4.0,
            },
        },
        paint_order: PaintOrder::StrokeFirst,
    };
    let rect = shape(
        ids,
        Shape::Rect(RectShape {
            x: 10.0,
            y: 10.0,
            width: 80.0,
            height: 40.0,
            rx: 5.0,
            ry: 5.0,
        }),
        filled.clone(),
    );
    let arc = shape(
        ids,
        Shape::Ellipse(EllipseShape {
            centre: Point::new(150.0, 50.0),
            radii: Point::new(30.0, 20.0),
            arc: Some(ArcPart {
                start: 10.0,
                end: 250.0,
                kind: ArcKind::Pie,
            }),
        }),
        filled,
    );
    let star = shape(
        ids,
        Shape::Star(StarShape {
            centre: Point::new(250.0, 60.0),
            points: 6,
            outer: 30.0,
            inner_ratio: 0.5,
            rotation: 15.0,
            rounding: 0.25,
            polygon: false,
        }),
        Style::default(),
    );
    let curve = shape(
        ids,
        Shape::Path(Arc::new(PathShape {
            contours: vec![Contour {
                nodes: vec![
                    PathNode {
                        point: Point::new(20.0, 150.0),
                        before: None,
                        after: Some(Point::new(40.0, 120.0)),
                        join: NodeJoin::Corner,
                    },
                    PathNode {
                        point: Point::new(80.0, 150.0),
                        before: Some(Point::new(60.0, 130.0)),
                        after: Some(Point::new(100.0, 170.0)),
                        join: NodeJoin::Symmetric,
                    },
                    PathNode::corner(Point::new(140.0, 150.0)),
                ],
                closed: false,
            }],
        })),
        stroked,
    );
    let text = Arc::new(
        Node::new(
            ids.issue(),
            Kind::Text {
                text: Arc::new(Text {
                    content: Arc::from("Hello <world> & \"friends\"\nSecond line"),
                    family: Arc::from("Noto Serif, serif"),
                    size: 24.0,
                    weight: 700,
                    italic: true,
                    letter_spacing: 1.5,
                    line_height: 1.5,
                    align: TextAlign::Middle,
                    origin: Point::new(200.0, 220.0),
                }),
                style: Arc::new(Style {
                    fill: black(),
                    ..Style::default()
                }),
            },
        )
        .expect("text")
        .with_name(Some(Arc::from("Greeting")))
        .expect("named"),
    );
    let window = shape(
        ids,
        Shape::Ellipse(EllipseShape {
            centre: Point::new(60.0, 60.0),
            radii: Point::new(40.0, 40.0),
            arc: None,
        }),
        Style::default(),
    );
    let clipped = Arc::new(
        group(ids, vec![rect, arc], Some(window))
            .with_transform(Affine::translate(5.0, 7.0))
            .expect("moved")
            .with_opacity(0.5)
            .expect("faded"),
    );
    let hidden = Arc::new(Node::clone(&star).with_visible(false).with_locked(true));
    let first = Arc::new(
        group(ids, vec![clipped, hidden], None)
            .with_name(Some(Arc::from("Shapes")))
            .expect("named"),
    );
    let second = Arc::new(
        group(ids, vec![curve, text], None)
            .with_name(Some(Arc::from("Lines & words")))
            .expect("named"),
    );
    Drawing::new(
        page(),
        Children::from_nodes(vec![first, second]).expect("held"),
        Arc::from(
            &[
                Guide {
                    axis: GuideAxis::Horizontal,
                    position: 100.0,
                },
                Guide {
                    axis: GuideAxis::Vertical,
                    position: 37.5,
                },
            ][..],
        ),
        Grid {
            spacing: 24.0,
            subdivisions: 3,
        },
    )
    .expect("a drawing")
}

/// Whether two nodes hold exactly the same thing, identities aside.
fn same_node(a: &Node, b: &Node) -> bool {
    a.name() == b.name()
        && a.visible() == b.visible()
        && a.locked() == b.locked()
        && a.opacity() == b.opacity()
        && a.transform() == b.transform()
        && match (a.kind(), b.kind()) {
            (
                Kind::Shape {
                    shape: sa,
                    style: ya,
                },
                Kind::Shape {
                    shape: sb,
                    style: yb,
                },
            ) => sa == sb && ya == yb,
            (
                Kind::Text {
                    text: ta,
                    style: ya,
                },
                Kind::Text {
                    text: tb,
                    style: yb,
                },
            ) => ta == tb && ya == yb,
            (
                Kind::Group {
                    children: ca,
                    clip: la,
                },
                Kind::Group {
                    children: cb,
                    clip: lb,
                },
            ) => {
                ca.len() == cb.len()
                    && ca.iter().zip(cb.iter()).all(|(x, y)| same_node(x, y))
                    && match (la, lb) {
                        (Some(x), Some(y)) => same_clip(x, y),
                        (None, None) => true,
                        _ => false,
                    }
            }
            _ => false,
        }
}

/// A clip reads back as a group of its shapes.
fn same_clip(written: &Node, read: &Node) -> bool {
    match read.kind() {
        Kind::Group { children, .. } if children.len() == 1 => {
            same_node(written, children.get(0).expect("one"))
        }
        _ => same_node(written, read),
    }
}

fn written(drawing: &Drawing) -> String {
    write(drawing).expect("written")
}

#[test]
fn a_drawing_survives_a_round_trip_through_svg() {
    let mut ids = Ids::new();
    let drawing = everything(&mut ids);
    let svg = written(&drawing);
    let back = read(svg.as_bytes(), &mut ids).expect("read back");
    assert!(back.unkept.is_empty(), "nothing lost: {:?}", back.unkept);
    let read = back.drawing;
    assert_eq!(read.page(), drawing.page());
    assert_eq!(read.guides(), drawing.guides());
    assert_eq!(read.grid(), drawing.grid());
    assert_eq!(read.layers().len(), 2);
    for (a, b) in drawing.layers().iter().zip(read.layers().iter()) {
        assert!(
            same_node(a, b),
            "layer {:?} differs:\n{a:#?}\n{b:#?}",
            a.name()
        );
    }
    // Writing what was read writes the same document.
    assert_eq!(written(&read), svg);
}

#[test]
fn an_inkscape_document_reads_into_layers_on_its_page() {
    let document = r#"<svg xmlns="http://www.w3.org/2000/svg"
        xmlns:inkscape="http://www.inkscape.org/namespaces/inkscape"
        width="210mm" height="297mm" viewBox="0 0 210 297">
      <g inkscape:groupmode="layer" inkscape:label="Background">
        <rect x="10" y="10" width="50" height="20" style="fill:#ff0000;stroke:none"/>
      </g>
      <g inkscape:groupmode="layer" inkscape:label="Ink" style="display:none">
        <circle cx="100" cy="100" r="25" fill="none" stroke="blue" stroke-width="2"/>
      </g>
    </svg>"#;
    let mut ids = Ids::new();
    let read = read(document.as_bytes(), &mut ids).expect("read");
    assert!(read.unkept.is_empty());
    let page = read.drawing.page();
    assert_eq!(page.unit, Unit::Mm);
    assert_eq!(
        (page.width, page.height),
        (210.0, 297.0),
        "the page is held as it was stated"
    );
    let layers = read.drawing.layers();
    assert_eq!(layers.len(), 2);
    assert_eq!(
        layers.get(0).and_then(|layer| layer.name()),
        Some("Background")
    );
    let ink = layers.get(1).expect("second layer");
    assert!(!ink.visible(), "a hidden layer is kept, hidden");
    // The millimetre viewBox places each layer on the page.
    assert!((ink.transform().a - Unit::Mm.pixels()).abs() < 1e-9);
    let circle = ink.children().and_then(|c| c.get(0)).expect("the circle");
    match circle.kind() {
        Kind::Shape {
            shape: Shape::Ellipse(ellipse),
            style,
        } => {
            assert_eq!(ellipse.radii, Point::new(25.0, 25.0));
            assert_eq!(style.fill, Paint::None);
            assert_eq!(style.stroke.paint, Paint::Colour(Rgba::new(0, 0, 255, 255)));
        }
        other => panic!("expected an ellipse, got {other:?}"),
    }
}

#[test]
fn stylesheets_uses_symbols_and_switches_read_as_drawn() {
    let document = r##"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink"
        width="200" height="200">
      <style>.warm { fill: orange } #special { stroke: green; stroke-width: 3 }</style>
      <defs>
        <symbol id="tile" viewBox="0 0 10 10"><rect class="warm" width="10" height="10"/></symbol>
        <circle id="dot" r="4" fill="purple"/>
      </defs>
      <use xlink:href="#tile" x="20" y="20" width="40" height="40"/>
      <use href="#dot" x="100" y="100"/>
      <switch>
        <rect systemLanguage="xx" width="1" height="1"/>
        <rect id="special" x="150" y="150" width="20" height="20" fill="white"/>
      </switch>
    </svg>"##;
    let mut ids = Ids::new();
    let read = read(document.as_bytes(), &mut ids).expect("read");
    assert!(read.unkept.contains(Lost::Alternatives));
    let layer = read.drawing.layers().get(0).expect("one layer");
    let children = layer.children().expect("content");
    assert_eq!(children.len(), 3);
    // The symbol's tile, its class's fill, fitted to the slot the use gave.
    let tile = children.get(0).expect("the tile");
    let bounds = tile.bounds();
    assert!((bounds.min.x - 20.0).abs() < 1e-9 && (bounds.max.x - 60.0).abs() < 1e-9);
    let mut found_orange = false;
    walk(tile, &mut |node| {
        if let Some(style) = node.style() {
            found_orange |= style.fill == Paint::Colour(Rgba::new(255, 165, 0, 255));
        }
    });
    assert!(
        found_orange,
        "the stylesheet's class reached the symbol's content"
    );
    // The switch drew its fallback, with the id rule's stroke.
    let special = children.get(2).expect("the switch");
    let mut stroked = false;
    walk(special, &mut |node| {
        if let Some(style) = node.style() {
            stroked |= style.stroke.width == 3.0 && style.stroke.paints();
        }
    });
    assert!(stroked);
}

#[test]
fn gradients_in_bounding_box_units_land_in_the_shapes_own_space() {
    let document = r#"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100">
      <linearGradient id="g"><stop offset="0" stop-color="red"/><stop offset="1" stop-color="blue"/></linearGradient>
      <rect x="20" y="40" width="60" height="10" fill="url(#g)" fill-opacity="0.5"/>
    </svg>"#;
    let mut ids = Ids::new();
    let read = read(document.as_bytes(), &mut ids).expect("read");
    let rect = read
        .drawing
        .layers()
        .get(0)
        .and_then(|l| l.children()?.get(0).cloned())
        .expect("rect");
    let Some(Paint::Gradient(gradient)) = rect.style().map(|style| style.fill.clone()) else {
        panic!("a gradient fill");
    };
    let GradientKind::Linear { start, end } = gradient.kind else {
        panic!("linear");
    };
    let (start, end) = (gradient.transform.map(start), gradient.transform.map(end));
    assert!(start.distance(Point::new(20.0, 40.0)) < 1e-9);
    assert!(end.distance(Point::new(80.0, 40.0)) < 1e-9);
    assert_eq!(
        gradient.stops[1].colour.a, 128,
        "the fill's opacity weakens every stop"
    );
}

#[test]
fn text_lines_come_from_their_tspans() {
    let document = r#"<svg xmlns="http://www.w3.org/2000/svg" width="300" height="300">
      <text x="10" y="40" font-family="Inter" font-size="20" text-anchor="end">
        <tspan x="10" y="40">First   line</tspan>
        <tspan x="10" y="64">Second</tspan>
      </text>
    </svg>"#;
    let mut ids = Ids::new();
    let read = read(document.as_bytes(), &mut ids).expect("read");
    let node = read
        .drawing
        .layers()
        .get(0)
        .and_then(|l| l.children()?.get(0).cloned())
        .expect("text");
    let Kind::Text { text, .. } = node.kind() else {
        panic!("a text object");
    };
    assert_eq!(&*text.content, "First line\nSecond");
    assert!(
        (text.line_height - 1.2).abs() < 1e-9,
        "{}",
        text.line_height
    );
    assert_eq!(text.align, TextAlign::End);
    assert_eq!(&*text.family, "Inter");
    assert_eq!(text.origin, Point::new(10.0, 40.0));
}

#[test]
fn what_a_drawing_cannot_hold_is_listed_not_dropped_silently() {
    let document = r#"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100">
      <filter id="f"><feGaussianBlur stdDeviation="2"/></filter>
      <mask id="m"><rect width="50" height="50" fill="white"/></mask>
      <pattern id="p" width="4" height="4"><rect width="2" height="2"/></pattern>
      <rect width="10" height="10" filter="url(#f)"/>
      <rect width="10" height="10" mask="url(#m)"/>
      <rect width="10" height="10" fill="url(#p) red"/>
      <image href="data:image/png;base64,AAAA" width="5" height="5"/>
      <text x="0" y="10"><tspan font-weight="bold">bold</tspan> plain</text>
    </svg>"#;
    let mut ids = Ids::new();
    let read = read(document.as_bytes(), &mut ids).expect("read");
    for lost in [
        Lost::Filters,
        Lost::Masks,
        Lost::Patterns,
        Lost::Images,
        Lost::TextStyling,
    ] {
        assert!(read.unkept.contains(lost), "{lost:?} not noted");
    }
    // The pattern's fallback colour stands in for it.
    let children = read
        .drawing
        .layers()
        .get(0)
        .and_then(|l| l.children().cloned())
        .expect("content");
    assert_eq!(
        children
            .get(2)
            .and_then(|node| node.style())
            .map(|style| style.fill.clone()),
        Some(Paint::Colour(Rgba::new(255, 0, 0, 255)))
    );
}

#[test]
fn a_clip_path_clips_in_the_clipped_elements_own_space() {
    let document = r#"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100">
      <clipPath id="c"><rect width="10" height="10"/></clipPath>
      <rect transform="translate(50 50)" width="40" height="40" clip-path="url(#c)"/>
    </svg>"#;
    let mut ids = Ids::new();
    let read = read(document.as_bytes(), &mut ids).expect("read");
    let clipped = read
        .drawing
        .layers()
        .get(0)
        .and_then(|l| l.children()?.get(0).cloned())
        .expect("clipped");
    let bounds = clipped.bounds();
    assert!(
        (bounds.min.x - 50.0).abs() < 1e-9 && (bounds.max.x - 60.0).abs() < 1e-9,
        "{bounds:?}"
    );
}

#[test]
fn documents_that_are_not_svg_or_are_broken_are_refused() {
    let mut ids = Ids::new();
    assert_eq!(read(b"\xff\xfe", &mut ids).err(), Some(ReadError::NotText));
    assert_eq!(read(b"<html/>", &mut ids).err(), Some(ReadError::NotSvg));
    assert_eq!(
        read(b"<svg><rect", &mut ids).err(),
        Some(ReadError::Malformed)
    );
    assert_eq!(
        read(
            b"<svg width=\"10\" height=\"10\"><path d=\"M0 0 L1\"/></svg>",
            &mut ids
        )
        .err(),
        Some(ReadError::Malformed)
    );
    // Each level uses the one below twice: 2^20 rectangles from 21 elements.
    let mut bomb = String::from(
        "<svg width=\"10\" height=\"10\"><g id=\"l0\"><rect width=\"1\" height=\"1\"/></g>",
    );
    for level in 1..=20 {
        let below = level - 1;
        write!(
            bomb,
            "<g id=\"l{level}\"><use href=\"#l{below}\"/><use href=\"#l{below}\"/></g>"
        )
        .expect("a String takes any text");
    }
    bomb.push_str("</svg>");
    assert_eq!(
        read(bomb.as_bytes(), &mut ids).err(),
        Some(ReadError::TooComplex)
    );
}

#[test]
fn a_reference_back_to_itself_is_refused_not_followed() {
    let mut ids = Ids::new();
    let clip_cycle = r#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10">
      <clipPath id="k"><rect width="5" height="5" clip-path="url(#k)"/></clipPath>
      <rect width="9" height="9" clip-path="url(#k)"/></svg>"#;
    assert_eq!(
        read(clip_cycle.as_bytes(), &mut ids).err(),
        Some(ReadError::Malformed)
    );
    let use_cycle = r##"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10">
      <g id="a"><rect width="1" height="1"/><use href="#a" x="2"/></g></svg>"##;
    assert_eq!(
        read(use_cycle.as_bytes(), &mut ids).err(),
        Some(ReadError::Malformed)
    );
}

#[test]
fn nesting_deeper_than_a_drawing_holds_is_refused() {
    let mut ids = Ids::new();
    let nest = |depth: usize| {
        let mut document =
            String::from(r#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10">"#);
        for _ in 0..depth {
            document.push_str("<g>");
        }
        document.push_str(r#"<rect width="1" height="1"/>"#);
        for _ in 0..depth {
            document.push_str("</g>");
        }
        document.push_str("</svg>");
        document
    };
    assert!(read(nest(64).as_bytes(), &mut ids).is_ok());
    assert_eq!(
        read(nest(200).as_bytes(), &mut ids).err(),
        Some(ReadError::TooComplex)
    );
}

/// A drawing of the one layer `layer`.
fn one_layer(layer: Node) -> Drawing {
    Drawing::new(
        page(),
        Children::from_nodes(vec![Arc::new(layer)]).expect("held"),
        Arc::from(&[][..]),
        Grid::default(),
    )
    .expect("a drawing")
}

#[test]
fn a_stroke_that_paints_nothing_keeps_everything_it_was_given() {
    let mut ids = Ids::new();
    let rect = Shape::Rect(RectShape {
        x: 1.0,
        y: 2.0,
        width: 3.0,
        height: 4.0,
        rx: 0.0,
        ry: 0.0,
    });
    let mut zero_width = Style::default();
    zero_width.stroke.paint = black();
    zero_width.stroke.width = 0.0;
    let mut none_but_shaped = Style::default();
    none_but_shaped.stroke.paint = Paint::None;
    none_but_shaped.stroke.width = 5.0;
    none_but_shaped.stroke.join = Join::Round;
    none_but_shaped.stroke.end = Arrowhead {
        shape: ArrowShape::Triangle,
        width: 3.0,
        length: 4.0,
    };
    let shapes = vec![
        shape(&mut ids, rect.clone(), zero_width),
        shape(&mut ids, rect, none_but_shaped),
    ];
    let layer = group(&mut ids, shapes, None);
    let drawing = one_layer(layer);
    let svg = written(&drawing);
    let read = read(svg.as_bytes(), &mut ids).expect("read").drawing;
    let (Some(before), Some(after)) = (drawing.layers().get(0), read.layers().get(0)) else {
        panic!("a layer each");
    };
    assert!(same_node(before, after), "{svg}\n{before:#?}\n{after:#?}");
    assert!(
        !svg.contains("marker-end"),
        "an arrowhead is drawn only where its stroke is"
    );
}

#[test]
fn a_clipped_layer_keeps_its_clip() {
    let mut ids = Ids::new();
    let window = shape(
        &mut ids,
        Shape::Ellipse(EllipseShape {
            centre: Point::new(50.0, 50.0),
            radii: Point::new(40.0, 30.0),
            arc: None,
        }),
        Style::default(),
    );
    let content = shape(
        &mut ids,
        Shape::Rect(RectShape {
            x: 0.0,
            y: 0.0,
            width: 100.0,
            height: 100.0,
            rx: 0.0,
            ry: 0.0,
        }),
        Style::default(),
    );
    let layer = group(&mut ids, vec![content], Some(window));
    let drawing = one_layer(layer);
    let svg = written(&drawing);
    let read = read(svg.as_bytes(), &mut ids).expect("read").drawing;
    let (Some(before), Some(after)) = (drawing.layers().get(0), read.layers().get(0)) else {
        panic!("a layer each");
    };
    assert!(same_node(before, after), "{svg}\n{before:#?}\n{after:#?}");
}

#[test]
fn numbers_reopen_exactly_as_they_were_saved() {
    let mut ids = Ids::new();
    let third = 1.0 / 3.0;
    let turned = shape(
        &mut ids,
        Shape::Rect(RectShape {
            x: third,
            y: -2.0 * third,
            width: 10.0 + third,
            height: 1e-7,
            rx: 0.0,
            ry: 0.0,
        }),
        Style::default(),
    );
    let turned = Arc::new(
        turned
            .with_transform(Affine::rotate_degrees(17.0).then(Affine::translate(third, 7.0)))
            .expect("placed"),
    );
    let layer = group(&mut ids, vec![turned], None);
    let drawing = one_layer(layer);
    let svg = written(&drawing);
    let read = read(svg.as_bytes(), &mut ids).expect("read").drawing;
    let (Some(before), Some(after)) = (drawing.layers().get(0), read.layers().get(0)) else {
        panic!("a layer each");
    };
    assert!(same_node(before, after), "{svg}\n{before:#?}\n{after:#?}");
}

#[test]
fn a_documents_own_title_and_description_are_kept_and_others_listed() {
    let mut ids = Ids::new();
    let document = "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"10\" height=\"10\">\
        <title>  A   river\n map </title><desc>Rivers &amp; roads.\n\tDrawn 2026.</desc>\
        <rect width=\"1\" height=\"1\"/></svg>";
    let read_back = read(document.as_bytes(), &mut ids).expect("read");
    assert!(read_back.unkept.is_empty());
    let info = read_back.drawing.info();
    assert_eq!(info.title.as_deref(), Some("A river map"));
    assert_eq!(
        info.description.as_deref(),
        Some("Rivers & roads.\n\tDrawn 2026.")
    );
    let svg = written(&read_back.drawing);
    let again = read(svg.as_bytes(), &mut ids).expect("read again");
    assert_eq!(again.drawing.info(), info);
    assert_eq!(written(&again.drawing), svg);

    let annotated = r#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10">
        <metadata>rdf</metadata><rect width="1" height="1"><title>A box</title></rect></svg>"#;
    let read_back = read(annotated.as_bytes(), &mut ids).expect("read");
    assert_eq!(
        read_back.unkept.iter().collect::<Vec<_>>(),
        vec![Lost::Annotations]
    );
}

#[test]
fn a_title_must_be_one_trimmed_line() {
    let mut ids = Ids::new();
    let drawing = one_layer(group(&mut ids, Vec::new(), None));
    let titled = |title: &str| {
        drawing.with_info(crate::drawing::Info {
            title: Some(Arc::from(title)),
            description: None,
        })
    };
    assert!(titled("Fine").is_ok());
    for refused in ["", " padded", "two\nlines", "bell\u{7}"] {
        assert_eq!(
            titled(refused).err(),
            Some(DrawingError::Invalid),
            "{refused:?}"
        );
    }
    let kept = titled("Fine")
        .expect("titled")
        .with_grid(Grid::default())
        .expect("a grid");
    assert_eq!(
        kept.info().title.as_deref(),
        Some("Fine"),
        "a change elsewhere keeps the title"
    );
}

fn walk(node: &Node, visit: &mut dyn FnMut(&Node)) {
    visit(node);
    if let Kind::Group { children, clip } = node.kind() {
        if let Some(clip) = clip {
            walk(clip, visit);
        }
        for child in children.iter() {
            walk(child, visit);
        }
    }
}
