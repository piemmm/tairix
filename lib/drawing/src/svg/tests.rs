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

use tairix_bezier::{Affine, FillRule, Path, PathEl, Point, Subpath};
use tairix_colour::Rgba;

use super::{read, write, DRAW_NAMESPACE};
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
use crate::testing::{line_height, origin, shared, slice, NoGlyphs};
use crate::text::{Baseline, Run, RunPaint, RunStyle, Text, TextAlign, TextPath};
use crate::unkept::Lost;
use crate::DrawingError;
use crate::ReadError;
use crate::SharedSlice;

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
            dashes: slice(&[4.0, 2.0]),
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
                    content: shared("Hello <world> & \"friends\"\nSecond line"),
                    family: shared("Noto Serif, serif"),
                    size: 24.0,
                    weight: 700,
                    italic: true,
                    letter_spacing: 1.5,
                    align: TextAlign::Middle,
                    baseline: Baseline::Lines {
                        origin: Point::new(200.0, 220.0),
                        line_height: 1.5,
                    },
                    runs: slice(&[
                        Run {
                            start: 6,
                            end: 13,
                            style: Arc::new(RunStyle {
                                family: Some(shared("Noto Sans, sans-serif")),
                                size: Some(30.0),
                                fill: Some(RunPaint::Colour(Rgba::new(200, 0, 0, 128))),
                                ..RunStyle::default()
                            }),
                        },
                        Run {
                            start: 17,
                            end: 24,
                            style: Arc::new(RunStyle {
                                italic: Some(false),
                                letter_spacing: Some(-0.5),
                                baseline_shift: Some(0.33),
                                ..RunStyle::default()
                            }),
                        },
                        Run {
                            start: 26,
                            end: 32,
                            style: Arc::new(RunStyle {
                                weight: Some(400),
                                fill: Some(RunPaint::None),
                                stroke: Some(RunPaint::Colour(Rgba::new(0, 0, 255, 255))),
                                ..RunStyle::default()
                            }),
                        },
                    ]),
                }),
                style: Arc::new(Style {
                    fill: black(),
                    ..Style::default()
                }),
            },
        )
        .expect("text")
        .with_name(Some(shared("Greeting")))
        .expect("named"),
    );
    let mut way = Subpath::new(Point::new(20.0, 260.0));
    way.elements.push(PathEl::Cubic(
        Point::new(80.0, 200.0),
        Point::new(160.0, 320.0),
        Point::new(240.0, 260.0),
    ));
    let along = Arc::new(
        Node::new(
            ids.issue(),
            Kind::Text {
                text: Arc::new(Text {
                    content: shared("Along the way"),
                    family: shared("serif"),
                    size: 18.0,
                    weight: 400,
                    italic: false,
                    letter_spacing: 0.0,
                    align: TextAlign::Middle,
                    baseline: Baseline::Path(Arc::new(TextPath {
                        path: Arc::new(
                            PathShape::from_path(&Path {
                                subpaths: vec![way],
                            })
                            .expect("a path"),
                        ),
                        start: 12.5,
                    })),
                    runs: slice(&[Run {
                        start: 6,
                        end: 9,
                        style: Arc::new(RunStyle {
                            size: Some(24.0),
                            ..RunStyle::default()
                        }),
                    }]),
                }),
                style: Arc::new(Style {
                    fill: black(),
                    ..Style::default()
                }),
            },
        )
        .expect("text along a path"),
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
            .with_name(Some(shared("Shapes")))
            .expect("named"),
    );
    let second = Arc::new(
        group(ids, vec![curve, text, along], None)
            .with_name(Some(shared("Lines & words")))
            .expect("named"),
    );
    Drawing::new(
        page(),
        Children::from_nodes(vec![first, second]).expect("held"),
        slice(&[
            Guide {
                axis: GuideAxis::Horizontal,
                position: 100.0,
            },
            Guide {
                axis: GuideAxis::Vertical,
                position: 37.5,
            },
        ]),
        Grid {
            spacing: 24.0,
            subdivisions: 3,
        },
    )
    .and_then(|drawing| {
        drawing.with_swatches(slice(&[
            Rgba::new(200, 30, 40, 255),
            Rgba::new(0, 90, 160, 100),
        ]))
    })
    .and_then(|drawing| drawing.with_resolution(300))
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
    assert_eq!(read.swatches(), drawing.swatches());
    assert_eq!(read.resolution(), 300);
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
fn a_translucent_ground_reopens_as_itself_and_other_viewers_see_it_so() {
    let mut ids = Ids::new();
    let ground = Rgba::new(10, 120, 200, 77);
    let drawing = everything(&mut ids)
        .with_page(Page {
            background: Some(ground),
            ..page()
        })
        .expect("a translucent ground");
    let svg = written(&drawing);
    assert!(svg.contains("draw:background=\"#0a78c84d\""), "{svg}");
    assert!(
        svg.contains("fill=\"#0a78c8\" fill-opacity=\"0.3\""),
        "{svg}"
    );
    let back = read(svg.as_bytes(), &mut ids).expect("read back");
    assert_eq!(back.drawing.page().background, Some(ground));
    assert!(back.unkept.is_empty(), "{:?}", back.unkept);
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
        (line_height(text) - 1.2).abs() < 1e-9,
        "{}",
        line_height(text)
    );
    assert_eq!(text.align, TextAlign::End);
    assert_eq!(&*text.family, "Inter");
    assert_eq!(origin(text), Point::new(10.0, 40.0));
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
      <text x="0" y="10"><tspan stroke-width="3">wide</tspan> plain</text>
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
    assert_eq!(
        read(b"\xff\xfe", &mut ids).err(),
        Some(ReadError::Unrecognised),
        "not text"
    );
    assert_eq!(
        read(b"<html/>", &mut ids).err(),
        Some(ReadError::Unrecognised),
        "not SVG"
    );
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
        SharedSlice::new(),
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
            title: Some(shared(title)),
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

/// A picture's colours: a different colour at each pixel, opaque.
fn opaque(x: u32, y: u32) -> [u8; 4] {
    [
        u8::try_from(x * 40 % 256).expect("a level"),
        u8::try_from(y * 70 % 256).expect("a level"),
        200,
        255,
    ]
}

fn rect_bounds(x0: f64, y0: f64, x1: f64, y1: f64) -> tairix_bezier::Bounds {
    tairix_bezier::Bounds::from_points([Point::new(x0, y0), Point::new(x1, y1)])
}

/// Every picture of `drawing`, in order.
fn pictures(drawing: &Drawing) -> Vec<Arc<Node>> {
    let mut found = Vec::new();
    for layer in drawing.layers().iter() {
        walk(layer, &mut |node| {
            if matches!(node.kind(), Kind::Image { .. }) {
                found.push(Arc::new(node.clone()));
            }
        });
    }
    found
}

fn image_of(node: &Node) -> &crate::image::Image {
    match node.kind() {
        Kind::Image { image } => image,
        other => panic!("a picture, not {other:?}"),
    }
}

/// An `<svg>` 100 units square holding `content`.
fn document(content: &str) -> String {
    alloc::format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="100" height="100">{content}</svg>"#
    )
}

/// `bytes` as a base64 data URL.
fn data_url(bytes: &[u8]) -> String {
    let mut url = String::new();
    super::data_url::write(&mut url, "image/png", bytes).expect("written");
    url
}

#[test]
fn a_picture_survives_a_round_trip_through_svg() {
    use crate::testing::{image, pixels, png, source_of};
    let mut ids = Ids::new();
    let plain = pixels(5, 3, opaque);
    let bare = crate::image::Image::new(
        plain.try_clone().expect("pixels"),
        rect_bounds(10.0, 20.0, 60.0, 50.0),
        None,
    )
    .expect("a picture");
    let turned = image(
        &mut ids,
        bare.clone(),
        Affine::rotate_degrees(30.0).then(Affine::translate(5.0, 7.0)),
    );
    let turned = Arc::new(
        turned
            .with_opacity(0.5)
            .and_then(|node| node.with_name(Some(shared("Photo"))))
            .expect("dressed"),
    );
    let again = image(
        &mut ids,
        bare.placed(rect_bounds(0.0, 0.0, 5.0, 3.0)).expect("again"),
        Affine::IDENTITY,
    );
    let encoded = png(&plain);
    let kept = image(
        &mut ids,
        crate::image::Image::new(
            plain,
            rect_bounds(1.0, 1.0, 6.0, 4.0),
            Some(source_of(&encoded)),
        )
        .expect("kept"),
        Affine::IDENTITY,
    );
    let drawing = one_layer(group(&mut ids, vec![turned, again, kept], None));
    let svg = written(&drawing);
    assert!(svg.contains(r#"xmlns:xlink="http://www.w3.org/1999/xlink""#));
    assert_eq!(
        svg.matches("<image ").count(),
        1,
        "pixels shown three times, two of them alike and one written as the same bytes, written once"
    );
    assert_eq!(svg.matches("<use ").count(), 3, "each object a use of them");
    assert!(svg.contains(r#"<symbol id="picture-1" draw:picture="true" viewBox="0 0 5 3""#));
    assert_eq!(
        svg.matches(&data_url(&encoded)).count(),
        1,
        "a picture read from a PNG keeps its bytes, written once"
    );
    let back = read(svg.as_bytes(), &mut ids).expect("read back");
    assert!(back.unkept.is_empty(), "{:?}", back.unkept);
    let before = pictures(&drawing);
    let after = pictures(&back.drawing);
    assert_eq!(after.len(), 3);
    for (a, b) in before.iter().zip(&after) {
        assert_eq!(a.transform(), b.transform());
        assert_eq!(a.opacity(), b.opacity());
        assert_eq!(a.name(), b.name());
        let (a, b) = (image_of(a), image_of(b));
        assert_eq!(a.place(), b.place());
        assert_eq!(
            a.pixels().pixels(),
            b.pixels().pixels(),
            "opaque pixels are exact"
        );
    }
    assert!(
        Arc::ptr_eq(image_of(&after[0]).pyramid(), image_of(&after[1]).pyramid()),
        "a picture shown twice reads back shared"
    );
    assert_eq!(
        image_of(&after[2]).source().map(|source| &*source.bytes),
        Some(&encoded[..])
    );
    assert_eq!(
        written(&back.drawing),
        svg,
        "writing what was read writes the same"
    );
}

#[test]
fn a_picture_is_laid_as_its_ratio_lays_it() {
    let url = data_url(&crate::testing::png(&crate::testing::pixels(4, 2, opaque)));
    let place_of = |attributes: &str| {
        let svg = document(&alloc::format!(
            r#"<image xlink:href="{url}" {attributes}/>"#
        ));
        let read = read(svg.as_bytes(), &mut Ids::new()).expect("read");
        assert!(read.unkept.is_empty(), "{:?}", read.unkept);
        let layer = read.drawing.layers().get(0).cloned().expect("a layer");
        let node = layer
            .children()
            .and_then(|children| children.get(0).cloned())
            .expect("one");
        match node.kind() {
            Kind::Image { image } => (image.place(), None),
            Kind::Group { children, clip } => (
                image_of(children.get(0).expect("the picture")).place(),
                clip.as_ref().map(|clip| clip.bounds()),
            ),
            other => panic!("{other:?}"),
        }
    };
    assert_eq!(
        place_of(r#"width="10" height="10""#),
        (rect_bounds(0.0, 2.5, 10.0, 7.5), None)
    );
    assert_eq!(
        place_of(r#"x="1" y="1" width="10" height="10" preserveAspectRatio="none""#),
        (rect_bounds(1.0, 1.0, 11.0, 11.0), None)
    );
    assert_eq!(
        place_of(r#"width="10" height="10" preserveAspectRatio="xMinYMax slice""#),
        (
            rect_bounds(0.0, 0.0, 20.0, 10.0),
            Some(rect_bounds(0.0, 0.0, 10.0, 10.0))
        ),
        "a sliced picture is cut to its rectangle"
    );
    assert_eq!(
        place_of(""),
        (rect_bounds(0.0, 0.0, 4.0, 2.0), None),
        "its own size"
    );
    assert_eq!(
        place_of(r#"width="8""#),
        (rect_bounds(0.0, 0.0, 8.0, 4.0), None)
    );
    assert_eq!(
        place_of(r#"width="auto" height="auto""#),
        (rect_bounds(0.0, 0.0, 4.0, 2.0), None),
        "auto is the side left unstated"
    );
    assert_eq!(
        place_of(r#"width="8" height="auto""#),
        (rect_bounds(0.0, 0.0, 8.0, 4.0), None)
    );
}

#[test]
fn a_picture_that_meets_its_rectangle_is_never_cut_to_it() {
    let url = data_url(&crate::testing::png(&crate::testing::pixels(3, 7, opaque)));
    for (x, width, height) in [
        (0.1, 10.1, 7.7),
        (0.2, 9.7, 7.7),
        (0.1, 12.34, 7.7),
        (0.7, 7.7, 20.2),
    ] {
        let svg = document(&alloc::format!(
            r#"<image xlink:href="{url}" x="{x}" y="{x}" width="{width}" height="{height}"/>"#
        ));
        let read = read(svg.as_bytes(), &mut Ids::new()).expect("read");
        let layer = read.drawing.layers().get(0).cloned().expect("a layer");
        let node = layer
            .children()
            .and_then(|children| children.get(0).cloned())
            .expect("one");
        assert!(
            matches!(node.kind(), Kind::Image { .. }),
            "{x} {width}×{height}: {:?}",
            node.kind()
        );
    }
}

#[test]
fn a_picture_linked_from_elsewhere_or_of_another_kind_is_lost() {
    for image in [
        r#"<image href="picture.png" width="5" height="5"/>"#,
        r#"<image href="data:image/gif;base64,R0lGODlhAQABAAAAACw=" width="5" height="5"/>"#,
        r#"<image href="data:image/png;base64,!!!!" width="5" height="5"/>"#,
    ] {
        let read = read(document(image).as_bytes(), &mut Ids::new()).expect("read");
        assert!(read.unkept.contains(Lost::Images), "{image}");
    }
    for nothing in [
        r#"<image width="5" height="5"/>"#,
        r#"<image href="data:image/png;base64,!!!!" width="0" height="5"/>"#,
    ] {
        let read = read(document(nothing).as_bytes(), &mut Ids::new()).expect("read");
        assert!(
            read.unkept.is_empty(),
            "{nothing} draws nothing and loses nothing"
        );
    }
}

#[test]
fn a_clip_path_holds_no_picture_and_clips_by_its_clip_rule() {
    let url = data_url(&crate::testing::png(&crate::testing::pixels(2, 2, opaque)));
    let svg = document(&alloc::format!(
        r#"<clipPath id="c"><image xlink:href="{url}" width="50" height="50"/>
             <path clip-rule="evenodd" d="M0 0 H40 V40 H0 Z M10 10 H30 V30 H10 Z"/></clipPath>
           <rect width="100" height="100" clip-path="url(#c)"/>"#
    ));
    let read = read(svg.as_bytes(), &mut Ids::new()).expect("read");
    let layer = read.drawing.layers().get(0).cloned().expect("a layer");
    let clipped = layer
        .children()
        .and_then(|children| children.get(0).cloned())
        .expect("one");
    let Kind::Group {
        clip: Some(clip), ..
    } = clipped.kind()
    else {
        panic!("a clipped group");
    };
    assert_eq!(
        clip.style().map(|style| style.fill_rule),
        Some(FillRule::EvenOdd),
        "the clip is the path alone, by its clip rule"
    );
}

/// `drawing` as a 100-pixel square, for comparing what two drawings show.
fn shown(drawing: &Drawing) -> tairix_raster::Surface {
    use crate::render::{artwork, Afresh, Mode, View};
    let view = View {
        to_device: Affine::IDENTITY,
        origin: Point::ZERO,
        side: 100.0,
        design: 100 * 256,
        mode: Mode::Painted,
        ground: false,
    };
    let art = artwork(drawing, &view, &NoGlyphs, &Afresh).expect("artwork");
    let mut surface = tairix_raster::Surface::new(100, 100).expect("a surface");
    assert!(surface.draw_artwork(&art.nodes, view.design));
    surface
}

#[test]
fn a_clip_of_groups_is_written_as_the_shapes_a_clip_path_holds() {
    let mut ids = Ids::new();
    let filled = |colour: Rgba| Style {
        fill: Paint::Colour(colour),
        stroke: Stroke {
            paint: Paint::None,
            ..Stroke::default()
        },
        ..Style::default()
    };
    let square = |ids: &mut Ids, x: f64, y: f64, side: f64, rule: FillRule| {
        shape(
            ids,
            Shape::Rect(RectShape {
                x,
                y,
                width: side,
                height: side,
                rx: 0.0,
                ry: 0.0,
            }),
            Style {
                fill_rule: rule,
                ..filled(Rgba::new(0, 0, 0, 255))
            },
        )
    };
    // A clip within the clip: a group clipped by a square of its own.
    let window = square(&mut ids, 0.0, 0.0, 30.0, FillRule::NonZero);
    let windowed = square(&mut ids, 10.0, 10.0, 60.0, FillRule::NonZero);
    let inner = Arc::new(
        group(&mut ids, vec![windowed], Some(window))
            .with_transform(Affine::translate(20.0, 0.0))
            .expect("moved"),
    );
    let holed = square(&mut ids, 60.0, 60.0, 30.0, FillRule::EvenOdd);
    let clip = Arc::new(
        group(&mut ids, vec![holed, inner], None)
            .with_transform(Affine::translate(0.0, 5.0))
            .expect("moved"),
    );
    let content = square(&mut ids, 0.0, 0.0, 100.0, FillRule::NonZero);
    let drawing = one_layer(group(&mut ids, vec![content], Some(clip)));
    let svg = written(&drawing);
    let definitions =
        &svg[svg.find("<clipPath").expect("a clip path")..svg.find("</defs>").expect("defs")];
    assert!(
        !definitions.contains("<g"),
        "no group within a clip path:\n{definitions}"
    );
    assert!(definitions.contains(r#"clip-rule="evenodd""#));
    assert!(
        definitions.contains(r#"clip-path="url(#clip-2)""#),
        "{definitions}"
    );
    let back = read(svg.as_bytes(), &mut ids).expect("read back");
    assert_eq!(
        shown(&back.drawing).pixels(),
        shown(&drawing).pixels(),
        "the clip reads back covering what it covered"
    );
}

#[test]
fn a_picture_clipping_a_group_is_written_as_its_frame() {
    let mut ids = Ids::new();
    let picture = crate::image::Image::new(
        crate::testing::pixels(2, 2, opaque),
        rect_bounds(10.0, 10.0, 50.0, 30.0),
        None,
    )
    .expect("a picture");
    let clip = crate::testing::image(&mut ids, picture, Affine::IDENTITY);
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
    let drawing = one_layer(group(&mut ids, vec![content], Some(clip)));
    let svg = written(&drawing);
    let definitions =
        &svg[svg.find("<clipPath").expect("a clip path")..svg.find("</defs>").expect("defs")];
    assert!(
        definitions.contains(r#"<rect x="10" y="10" width="40" height="20""#),
        "{definitions}"
    );
    assert!(
        !svg.contains("<image") && !svg.contains("data:"),
        "a frame is linked to no picture"
    );
    assert!(
        !svg.contains("xmlns:xlink"),
        "no link is written, so none is declared"
    );
    let back = read(svg.as_bytes(), &mut ids).expect("read back");
    assert_eq!(shown(&back.drawing).pixels(), shown(&drawing).pixels());
}

#[test]
fn a_repeated_id_names_the_first_element_bearing_it() {
    let svg = document(
        r##"<defs><rect id="a" width="10" height="10"/><rect id="a" width="70" height="70"/></defs>
            <use href="#a"/>"##,
    );
    let read = read(svg.as_bytes(), &mut Ids::new()).expect("read");
    let layer = read.drawing.layers().get(0).cloned().expect("a layer");
    let bounds = layer.bounds();
    assert!((bounds.max.x - 10.0).abs() < 1e-9, "{bounds:?}");
}

/// A text and its style, as a drawing holds them.
type Lettered = (Arc<Text>, Arc<Style>);

/// The texts of a document, in order, with what reading it lost.
fn texts(document: &str) -> (Vec<Lettered>, crate::unkept::Unkept) {
    let mut ids = Ids::new();
    let read = read(document.as_bytes(), &mut ids).expect("read");
    let mut found = Vec::new();
    for layer in read.drawing.layers().iter() {
        walk(layer, &mut |node| {
            if let Kind::Text { text, style } = node.kind() {
                found.push((text.clone(), style.clone()));
            }
        });
    }
    (found, read.unkept)
}

fn spans(text: &Text) -> Vec<(usize, usize, RunStyle)> {
    text.runs
        .iter()
        .map(|run| (run.start, run.end, (*run.style).clone()))
        .collect()
}

#[test]
fn a_span_styled_otherwise_is_a_run_and_one_over_everything_the_text_s_own() {
    let (texts, unkept) = texts(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="300" height="100">
      <text x="10" y="40" font-family="Inter" font-size="20">Plain <tspan font-weight="bold" fill="#ff0000" fill-opacity="0.5">bold</tspan> <tspan font-size="10" baseline-shift="super">2</tspan></text>
      <text x="10" y="80" font-size="20"><tspan font-size="12" fill="#00f" font-style="italic">Whole</tspan></text>
    </svg>"##,
    );
    assert!(unkept.is_empty(), "{unkept:?}");
    let (plain, _) = &texts[0];
    assert_eq!(&*plain.content, "Plain bold 2");
    let runs = spans(plain);
    assert_eq!(
        runs[0],
        (
            6,
            10,
            RunStyle {
                weight: Some(700),
                fill: Some(RunPaint::Colour(Rgba::new(255, 0, 0, 128))),
                ..RunStyle::default()
            }
        )
    );
    let (start, end, superscript) = &runs[1];
    assert_eq!((*start, *end, superscript.size), (11, 12, Some(10.0)));
    let raised = superscript.baseline_shift.expect("raised");
    assert!(
        (raised - 20.0 / 3.0 / 10.0).abs() < 1e-12,
        "a third of the parent's size, in its own ems: {raised}"
    );
    let (whole, style) = &texts[1];
    assert!(
        whole.runs.is_empty(),
        "folded into the text: {:?}",
        whole.runs
    );
    assert_eq!((whole.size, whole.italic), (12.0, true));
    assert_eq!(style.fill, Paint::Colour(Rgba::new(0, 0, 255, 255)));
}

#[test]
fn baseline_shifts_add_up_through_nested_spans() {
    let (texts, _) = texts(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100">
      <text font-size="10">x<tspan baseline-shift="2">a<tspan baseline-shift="0.5em" font-size="20">b</tspan></tspan><tspan baseline-shift="sub">c</tspan></text>
    </svg>"#,
    );
    let shifts: Vec<Option<f64>> = texts[0]
        .0
        .runs
        .iter()
        .map(|run| run.style.baseline_shift)
        .collect();
    assert_eq!(shifts.len(), 3);
    assert!((shifts[0].expect("a") - 0.2).abs() < 1e-12);
    assert!((shifts[1].expect("b") - 0.6).abs() < 1e-12, "{shifts:?}");
    assert!((shifts[2].expect("c") + 0.2).abs() < 1e-12);
}

#[test]
fn a_text_path_reads_its_start_its_side_and_its_path_s_stated_length() {
    let (texts, unkept) = texts(
        r##"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="200" height="200">
      <defs>
        <path id="p" d="M0 0 H100" transform="translate(10 20)"/>
        <path id="q" d="M0 0 H100" pathLength="50"/>
      </defs>
      <text font-size="10" text-anchor="middle"><textPath href="#p" startOffset="25%">Hi <tspan fill="#f00">there</tspan></textPath></text>
      <text font-size="10"><textPath xlink:href="#q" startOffset="10">Hi</textPath></text>
      <text font-size="10"><textPath href="#p" side="right" startOffset="1em">Hi</textPath></text>
    </svg>"##,
    );
    assert!(unkept.is_empty(), "{unkept:?}");
    let path_of = |text: &Text| match &text.baseline {
        Baseline::Path(along) => Arc::clone(along),
        Baseline::Lines { .. } => panic!("a text along a path"),
    };
    let first = path_of(&texts[0].0);
    assert_eq!(&*texts[0].0.content, "Hi there");
    assert_eq!(texts[0].0.runs.len(), 1);
    assert_eq!(texts[0].0.align, TextAlign::Middle);
    assert_eq!(first.start, 25.0, "a quarter of its length");
    assert_eq!(
        first.first_point(),
        Point::new(10.0, 20.0),
        "placed by its transform"
    );
    assert_eq!(
        path_of(&texts[1].0).start,
        20.0,
        "in the path's stated units"
    );
    let reversed = path_of(&texts[2].0);
    assert_eq!(
        reversed.first_point(),
        Point::new(110.0, 20.0),
        "run backwards"
    );
    assert_eq!(reversed.start, 10.0, "an em of the text path's own size");
}

#[test]
fn what_a_text_cannot_be_set_along_is_set_in_lines_and_noted() {
    let lost = |content: &str| {
        let (texts, unkept) = texts(&alloc::format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="200" height="200">
      <defs><path id="p" d="M0 0 H100"/><path id="broken" d="M0"/><g id="g"/></defs>
      {content}
    </svg>"#
        ));
        let text = Arc::clone(&texts[0].0);
        (text, unkept.contains(Lost::TextPaths))
    };
    let (beside, noted) =
        lost(r##"<text x="5" y="50">Before <textPath href="#p">on</textPath></text>"##);
    assert!(noted && matches!(beside.baseline, Baseline::Lines { .. }));
    assert_eq!(&*beside.content, "Before on");
    let (broken, noted) = lost(r##"<text><textPath href="#broken">round</textPath></text>"##);
    assert!(noted && matches!(broken.baseline, Baseline::Lines { .. }));
    assert_eq!(
        &*broken.content, "round",
        "set in lines rather than dropped"
    );
    let (two, noted) = lost(
        r##"<text><textPath href="#p">one</textPath><textPath href="#p">two</textPath></text>"##,
    );
    assert!(noted && matches!(two.baseline, Baseline::Lines { .. }));
    let (bent, noted) =
        lost(r##"<text><textPath href="#p" method="stretch">bent</textPath></text>"##);
    assert!(
        noted && matches!(bent.baseline, Baseline::Path(_)),
        "set rigidly along it"
    );
    for nothing in [
        r##"<text><textPath href="#nowhere">gone</textPath></text>"##,
        r##"<text><textPath href="#g">gone</textPath></text>"##,
        r##"<text><textPath path="" href="#p">gone</textPath></text>"##,
    ] {
        let (gone, noted) = lost(nothing);
        assert!(
            !noted && gone.content.is_empty(),
            "SVG draws none of {nothing}"
        );
    }
}

#[test]
fn a_text_path_along_a_basic_shape_or_its_own_data_is_kept() {
    let (texts, unkept) = texts(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="200" height="200">
      <defs><circle id="c" cx="50" cy="50" r="40" transform="translate(5 0)"/></defs>
      <text><textPath href="#c" startOffset="50%">round</textPath></text>
      <text><textPath path="M10 20 L110 20">own</textPath></text>
    </svg>"##,
    );
    assert!(unkept.is_empty(), "{unkept:?}");
    let path_of = |text: &Text| match &text.baseline {
        Baseline::Path(along) => Arc::clone(along),
        Baseline::Lines { .. } => panic!("a text along a path"),
    };
    let round = path_of(&texts[0].0);
    assert!(
        round.first_point().distance(Point::new(95.0, 50.0)) < 1e-9,
        "from the circle's rightmost point, placed by its transform"
    );
    let circumference = core::f64::consts::TAU * 40.0;
    assert!(
        (round.start - circumference / 2.0).abs() < 0.05,
        "{}",
        round.start
    );
    assert_eq!(path_of(&texts[1].0).first_point(), Point::new(10.0, 20.0));
}

#[test]
fn runs_and_paths_are_written_as_svg_writes_them() {
    let mut ids = Ids::new();
    let svg = written(&everything(&mut ids));
    for spelled in [
        "xmlns:xlink=",
        "<path id=\"text-path-1\" d=\"",
        "<textPath xlink:href=\"#text-path-1\" startOffset=\"12.5\">Along <tspan font-size=\"24\">the</tspan> way</textPath>",
        "baseline-shift=\"0.33em\"",
        "fill=\"#c80000\" fill-opacity=\"",
        "fill=\"none\" stroke=\"#0000ff\">Second</tspan>",
    ] {
        assert!(svg.contains(spelled), "{spelled} missing from\n{svg}");
    }
}

#[test]
fn a_span_set_at_a_size_or_spacing_no_text_holds_loses_it_not_the_document() {
    let (texts, unkept) = texts(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100">
      <text x="5" y="50" font-size="10">a<tspan font-size="0">b</tspan><tspan letter-spacing="1e300">c</tspan>d</text>
    </svg>"#,
    );
    assert!(unkept.contains(Lost::TextStyling));
    let (text, _) = &texts[0];
    assert_eq!(&*text.content, "abcd");
    assert!(text.runs.is_empty(), "{:?}", text.runs);
}

#[test]
fn a_stretched_picture_reopens_over_exactly_the_rectangle_it_was_saved_over() {
    // A width that scaled to its pixels and back would not come out as itself.
    let width = 692.611_713_572_372_4;
    let mut ids = Ids::new();
    let pixels = || crate::testing::pixels(1306, 1, opaque);
    let place = rect_bounds(3.25, 7.5, 3.25 + width, 90.0);
    let other = crate::testing::pixels(1306, 1, |x, _| [0, 0, 0, if x % 2 == 0 { 255 } else { 0 }]);
    let alone = crate::image::Image::new(other, place, None).expect("a picture");
    let shared = crate::image::Image::new(pixels(), place, None).expect("a picture");
    let nodes = vec![
        crate::testing::image(&mut ids, alone, Affine::IDENTITY),
        crate::testing::image(&mut ids, shared.clone(), Affine::IDENTITY),
        crate::testing::image(
            &mut ids,
            shared
                .placed(rect_bounds(-1.5, 0.1, width, 0.300_000_000_000_000_04))
                .expect("again"),
            Affine::IDENTITY,
        ),
    ];
    let drawing = one_layer(group(&mut ids, nodes, None));
    let svg = written(&drawing);
    assert_eq!(
        (svg.matches("<use ").count(), svg.matches("<image ").count()),
        (2, 2)
    );
    let back = read(svg.as_bytes(), &mut ids).expect("read back");
    let places: Vec<_> = pictures(&back.drawing)
        .iter()
        .map(|node| image_of(node).place())
        .collect();
    let expected: Vec<_> = pictures(&drawing)
        .iter()
        .map(|node| image_of(node).place())
        .collect();
    assert_eq!(places, expected, "inline and shared alike");
}

#[test]
fn a_symbol_draw_did_not_mark_as_its_picture_reads_as_what_it_holds() {
    let png = crate::testing::png(&crate::testing::pixels(2, 2, opaque));
    let link = data_url(&png);
    let document = |marked: &str| {
        alloc::format!(
            r##"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" xmlns:draw="{DRAW_NAMESPACE}" width="100" height="100">
      <defs><symbol id="p" {marked} viewBox="0 0 2 2" preserveAspectRatio="none"><image width="2" height="2" xlink:href="{link}"/></symbol></defs>
      <use xlink:href="#p" x="10" y="10" width="20" height="40"/>
    </svg>"##
        )
    };
    let mut ids = Ids::new();
    let marked = read(document(r#"draw:picture="true""#).as_bytes(), &mut ids).expect("read");
    let shown = marked
        .drawing
        .layers()
        .get(0)
        .and_then(|layer| layer.children()?.get(0).cloned())
        .expect("an object");
    assert_eq!(
        image_of(&shown).place(),
        rect_bounds(10.0, 10.0, 30.0, 50.0),
        "Draw's picture is the object itself, over the rectangle its use states"
    );
    let plain = read(document("").as_bytes(), &mut ids).expect("read");
    let held = plain
        .drawing
        .layers()
        .get(0)
        .and_then(|layer| layer.children()?.get(0).cloned())
        .expect("an object");
    assert!(
        matches!(held.kind(), Kind::Group { .. }),
        "any other symbol is expanded as SVG expands it"
    );
    let clipped = document(r#"draw:picture="true""#).replace(
        r##"<use xlink:href="#p" x="10" y="10" width="20" height="40"/>"##,
        r##"<clipPath id="k"><use xlink:href="#p" x="10" y="10" width="20" height="40"/><rect width="5" height="5"/></clipPath><rect width="50" height="50" clip-path="url(#k)"/>"##,
    );
    let clip = read(clipped.as_bytes(), &mut ids).expect("read");
    assert_eq!(
        pictures(&clip.drawing).len(),
        0,
        "a clip path holds no picture"
    );
    let broken = document(r#"draw:picture="true""#).replace(&link, "data:image/png;base64,AAAA");
    let lost = read(broken.as_bytes(), &mut ids).expect("read");
    assert!(lost.unkept.contains(Lost::Images));
    assert_eq!(pictures(&lost.drawing).len(), 0);
}
