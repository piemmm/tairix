//! Unit tests for rendering.

use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;

use tairix_bezier::{Affine, Path, PathEl, Point, Subpath};
use tairix_colour::Rgba;
use tairix_raster::{Pixel, Surface};

use super::{artwork, Afresh, Mode, View};
use crate::children::Children;
use crate::drawing::{Drawing, Grid, Page, Unit};
use crate::id::Ids;
use crate::node::{Kind, Node};
use crate::shape::{PathShape, RectShape, Shape};
use crate::style::{
    ArrowShape, Arrowhead, Cap, Gradient, GradientKind, Paint, Spread, Stop, Stroke, Style,
};
use crate::text::{Glyph, Glyphs, Text, TextAlign};

const RED: Rgba = Rgba::new(255, 0, 0, 255);
const BLUE: Rgba = Rgba::new(0, 0, 255, 255);

/// Glyphs for nothing but `a`, a square filling its em.
struct OnlyA;

impl Glyphs for OnlyA {
    fn glyph(&self, _family: &str, _weight: u16, _italic: bool, c: char) -> Option<Arc<Glyph>> {
        (c == 'a').then(|| Arc::new(Glyph::new(square(0.0, -1.0, 1.0), 1.0)))
    }
}

fn square(x: f64, y: f64, side: f64) -> Path {
    let mut ring = Subpath::new(Point::new(x, y));
    ring.elements.extend([
        PathEl::Line(Point::new(x + side, y)),
        PathEl::Line(Point::new(x + side, y + side)),
        PathEl::Line(Point::new(x, y + side)),
    ]);
    ring.closed = true;
    Path {
        subpaths: vec![ring],
    }
}

fn filled(colour: Rgba) -> Style {
    Style {
        fill: Paint::Colour(colour),
        stroke: Stroke {
            paint: Paint::None,
            ..Stroke::default()
        },
        ..Style::default()
    }
}

fn stroked(colour: Rgba, width: f64) -> Style {
    Style {
        fill: Paint::None,
        stroke: Stroke {
            paint: Paint::Colour(colour),
            width,
            ..Stroke::default()
        },
        ..Style::default()
    }
}

fn rect(ids: &mut Ids, x: f64, y: f64, width: f64, height: f64, style: Style) -> Arc<Node> {
    Arc::new(
        Node::new(
            ids.issue(),
            Kind::Shape {
                shape: Shape::Rect(RectShape {
                    x,
                    y,
                    width,
                    height,
                    rx: 0.0,
                    ry: 0.0,
                }),
                style: Arc::new(style),
            },
        )
        .expect("a shape"),
    )
}

fn line(ids: &mut Ids, from: Point, to: Point, style: Style) -> Arc<Node> {
    let mut open = Subpath::new(from);
    open.elements.push(PathEl::Line(to));
    let shape = PathShape::from_path(&Path {
        subpaths: vec![open],
    })
    .expect("a path");
    Arc::new(
        Node::new(
            ids.issue(),
            Kind::Shape {
                shape: Shape::Path(Arc::new(shape)),
                style: Arc::new(style),
            },
        )
        .expect("a shape"),
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

fn drawing(layer: Node, background: Option<Rgba>) -> Drawing {
    Drawing::new(
        Page {
            width: 64.0,
            height: 64.0,
            unit: Unit::Px,
            background,
        },
        Children::from_nodes(vec![Arc::new(layer)]).expect("held"),
        Arc::from(&[][..]),
        Grid::default(),
    )
    .expect("a drawing")
}

/// A 64-pixel square at `origin` of the drawing seen through `to_device`.
fn view(to_device: Affine, origin: Point, mode: Mode) -> View {
    View {
        to_device,
        origin,
        side: 64.0,
        design: 64 * 256,
        mode,
    }
}

fn rendered(drawing: &Drawing, view: &View, glyphs: &dyn Glyphs) -> (Surface, bool) {
    let art = artwork(drawing, view, glyphs, &Afresh).expect("artwork");
    let mut surface = Surface::new(64, 64).expect("a surface");
    assert!(surface.draw_artwork(&art.nodes, view.design), "drawn whole");
    (surface, art.complete)
}

fn at(surface: &Surface, x: u32, y: u32) -> Pixel {
    surface.pixels()[(y * surface.width() + x) as usize]
}

fn plain(drawing: &Drawing) -> Surface {
    rendered(
        drawing,
        &view(Affine::IDENTITY, Point::ZERO, Mode::Painted),
        &OnlyA,
    )
    .0
}

#[test]
fn a_filled_rectangle_covers_its_pixels_and_nothing_else() {
    let mut ids = Ids::new();
    let layer = group(
        vec![rect(&mut ids, 10.0, 10.0, 30.0, 20.0, filled(RED))],
        None,
        &mut ids,
    );
    let surface = plain(&drawing(layer, None));
    assert_eq!(
        at(&surface, 20, 20),
        Pixel {
            r: 255,
            g: 0,
            b: 0,
            a: 255
        }
    );
    assert_eq!(at(&surface, 5, 5).a, 0);
    assert_eq!(at(&surface, 45, 20).a, 0);
    assert_eq!(at(&surface, 20, 35).a, 0);
}

#[test]
fn a_square_deep_inside_a_shape_is_filled_at_any_zoom() {
    let mut ids = Ids::new();
    let layer = group(
        vec![rect(&mut ids, 0.0, 0.0, 64.0, 64.0, filled(BLUE))],
        None,
        &mut ids,
    );
    let drawing = drawing(layer, None);
    for zoom in [2.0, 256.0, 1e5] {
        // A square in the middle of the shape, its corners a long way off.
        let middle = 32.0 * zoom;
        let view = view(
            Affine::scale(zoom, zoom),
            Point::new(middle, middle),
            Mode::Painted,
        );
        let (surface, _) = rendered(&drawing, &view, &OnlyA);
        for (x, y) in [(0, 0), (63, 0), (0, 63), (63, 63), (31, 31)] {
            assert_eq!(at(&surface, x, y).a, 255, "zoom {zoom} at {x},{y}");
        }
    }
}

#[test]
fn a_stroke_scales_and_shears_with_its_object() {
    let mut ids = Ids::new();
    let stroked = stroked(RED, 4.0);
    let thin = line(
        &mut ids,
        Point::new(4.0, 32.0),
        Point::new(60.0, 32.0),
        stroked.clone(),
    );
    let surface = plain(&drawing(group(vec![thin], None, &mut ids), None));
    assert_eq!(at(&surface, 30, 31).a, 255, "within the pen");
    assert_eq!(at(&surface, 30, 34).a, 0, "past the pen");
    // Drawn at half the height and stretched back, the pen is stretched too.
    let tall = line(
        &mut ids,
        Point::new(4.0, 16.0),
        Point::new(60.0, 16.0),
        stroked,
    );
    let tall = Arc::new(
        tall.with_transform(Affine::scale(1.0, 2.0))
            .expect("placed"),
    );
    let surface = plain(&drawing(group(vec![tall], None, &mut ids), None));
    assert_eq!(
        at(&surface, 30, 34).a,
        255,
        "a pen stretched with its object"
    );
    assert_eq!(at(&surface, 30, 37).a, 0);
}

#[test]
fn a_gradient_runs_across_its_shape() {
    let mut ids = Ids::new();
    let gradient = Gradient {
        kind: GradientKind::Linear {
            start: Point::new(0.0, 0.0),
            end: Point::new(64.0, 0.0),
        },
        stops: vec![
            Stop {
                offset: 0.0,
                colour: RED,
            },
            Stop {
                offset: 1.0,
                colour: BLUE,
            },
        ],
        spread: Spread::Pad,
        transform: Affine::IDENTITY,
    };
    let mut style = filled(RED);
    style.fill = Paint::Gradient(Arc::new(gradient));
    let layer = group(
        vec![rect(&mut ids, 0.0, 0.0, 64.0, 64.0, style)],
        None,
        &mut ids,
    );
    let surface = plain(&drawing(layer, None));
    let left = at(&surface, 1, 30);
    let right = at(&surface, 62, 30);
    assert!(left.r > 240 && left.b < 15, "{left:?}");
    assert!(right.b > 240 && right.r < 15, "{right:?}");
}

#[test]
fn a_translucent_group_is_weakened_as_one() {
    let mut ids = Ids::new();
    let lower = rect(&mut ids, 0.0, 0.0, 40.0, 40.0, filled(RED));
    let upper = rect(&mut ids, 20.0, 20.0, 40.0, 40.0, filled(BLUE));
    let half = group(vec![lower, upper], None, &mut ids)
        .with_opacity(0.5)
        .expect("weakened");
    let layer = group(vec![Arc::new(half)], None, &mut ids);
    let surface = plain(&drawing(layer, None));
    let overlap = at(&surface, 30, 30);
    assert!(
        (126..=129).contains(&overlap.a),
        "the overlap shows no red: {overlap:?}"
    );
    assert!(overlap.r < 2, "{overlap:?}");
}

#[test]
fn a_clipped_group_shows_only_what_its_clip_covers() {
    let mut ids = Ids::new();
    let content = rect(&mut ids, 0.0, 0.0, 64.0, 64.0, filled(RED));
    let window = rect(&mut ids, 0.0, 0.0, 32.0, 64.0, filled(BLUE));
    let clipped = group(vec![content], Some(window), &mut ids);
    let layer = group(vec![Arc::new(clipped)], None, &mut ids);
    let surface = plain(&drawing(layer, None));
    assert_eq!(
        at(&surface, 16, 16),
        Pixel {
            r: 255,
            g: 0,
            b: 0,
            a: 255
        },
        "the clip paints nothing"
    );
    assert_eq!(at(&surface, 48, 16).a, 0, "outside the clip");
}

#[test]
fn an_arrowhead_is_drawn_at_the_end_of_its_stroke() {
    let mut ids = Ids::new();
    let mut style = stroked(RED, 2.0);
    style.stroke.end = Arrowhead {
        shape: ArrowShape::Triangle,
        width: 4.0,
        length: 4.0,
    };
    let arrow = line(
        &mut ids,
        Point::new(4.0, 32.0),
        Point::new(40.0, 32.0),
        style.clone(),
    );
    let surface = plain(&drawing(group(vec![arrow], None, &mut ids), None));
    assert_eq!(at(&surface, 42, 32).a, 255, "past the end, inside the head");
    assert_eq!(
        at(&surface, 20, 35).a,
        0,
        "the shaft is as wide as its stroke"
    );
    style.stroke.paint = Paint::None;
    let bare = line(
        &mut ids,
        Point::new(4.0, 32.0),
        Point::new(40.0, 32.0),
        style,
    );
    let surface = plain(&drawing(group(vec![bare], None, &mut ids), None));
    assert_eq!(at(&surface, 42, 32).a, 0, "no stroke, no head");
}

#[test]
fn dots_and_dashes_are_cut_alike_in_every_square() {
    let mut ids = Ids::new();
    let mut style = stroked(RED, 2.0);
    style.stroke.cap = Cap::Round;
    style.stroke.dashes = Arc::from(&[0.0, 6.0][..]);
    let dotted = line(
        &mut ids,
        Point::new(2.0, 32.0),
        Point::new(2000.0, 32.0),
        style,
    );
    let drawing = drawing(group(vec![dotted], None, &mut ids), None);
    let whole = plain(&drawing);
    assert_eq!(at(&whole, 2, 32).a, 255, "a dot at the start");
    assert_eq!(at(&whole, 8, 32).a, 255, "and one a period on");
    assert_eq!(at(&whole, 5, 32).a, 0, "nothing between");
    // A square far along the line, reached past many dots it does not draw.
    let far = view(Affine::IDENTITY, Point::new(1202.0, 0.0), Mode::Painted);
    let (surface, _) = rendered(&drawing, &far, &OnlyA);
    assert_eq!(at(&surface, 0, 32).a, 255, "a dot exactly 200 periods on");
    assert_eq!(at(&surface, 3, 32).a, 0);
}

#[test]
fn outlines_are_hairlines_whatever_the_paint() {
    let mut ids = Ids::new();
    let layer = group(
        vec![rect(&mut ids, 10.0, 10.0, 40.0, 40.0, filled(RED))],
        None,
        &mut ids,
    );
    let drawing = drawing(layer, None);
    let ink = Rgba::new(0, 0, 0, 255);
    let (surface, _) = rendered(
        &drawing,
        &view(Affine::IDENTITY, Point::ZERO, Mode::Outline(ink)),
        &OnlyA,
    );
    assert_eq!(at(&surface, 30, 30).a, 0, "nothing filled");
    assert!(at(&surface, 10, 30).a > 100, "the edge drawn");
    let zoomed = view(
        Affine::scale(8.0, 8.0),
        Point::new(60.0, 60.0),
        Mode::Outline(ink),
    );
    let (surface, _) = rendered(&drawing, &zoomed, &OnlyA);
    assert!(
        at(&surface, 20, 40).a > 100 && at(&surface, 22, 40).a == 0,
        "one pixel at any zoom"
    );
}

#[test]
fn hidden_and_far_objects_are_left_out() {
    let mut ids = Ids::new();
    let hidden = Arc::new(rect(&mut ids, 0.0, 0.0, 64.0, 64.0, filled(RED)).with_visible(false));
    let far = rect(&mut ids, 5000.0, 5000.0, 10.0, 10.0, filled(BLUE));
    let layer = group(vec![hidden, far], None, &mut ids);
    let art = artwork(
        &drawing(layer, None),
        &view(Affine::IDENTITY, Point::ZERO, Mode::Painted),
        &OnlyA,
        &Afresh,
    )
    .expect("artwork");
    assert!(art.nodes.is_empty(), "{:?}", art.nodes);
}

#[test]
fn text_is_set_from_its_glyphs_and_says_when_one_is_missing() {
    let mut ids = Ids::new();
    let text = Text {
        content: Arc::from("ab"),
        family: Arc::from("sans-serif"),
        size: 20.0,
        weight: 400,
        italic: false,
        letter_spacing: 0.0,
        line_height: 1.25,
        align: TextAlign::Start,
        origin: Point::new(10.0, 40.0),
    };
    let node = Node::new(
        ids.issue(),
        Kind::Text {
            text: Arc::new(text),
            style: Arc::new(filled(RED)),
        },
    )
    .expect("text");
    let (surface, complete) = rendered(
        &drawing(group(vec![Arc::new(node)], None, &mut ids), None),
        &view(Affine::IDENTITY, Point::ZERO, Mode::Painted),
        &OnlyA,
    );
    assert!(!complete, "the b is not known");
    assert_eq!(
        at(&surface, 20, 30).a,
        255,
        "the a's square, an em on the baseline"
    );
    assert_eq!(at(&surface, 35, 30).a, 0, "nothing drawn for the b");
}

#[test]
fn the_page_background_is_painted_beneath_everything() {
    let mut ids = Ids::new();
    let cream = Rgba::new(250, 240, 220, 255);
    let layer = group(
        vec![rect(&mut ids, 10.0, 10.0, 10.0, 10.0, filled(RED))],
        None,
        &mut ids,
    );
    let surface = plain(&drawing(layer, Some(cream)));
    assert_eq!(
        at(&surface, 40, 40),
        Pixel {
            r: 250,
            g: 240,
            b: 220,
            a: 255
        }
    );
    assert_eq!(
        at(&surface, 15, 15),
        Pixel {
            r: 255,
            g: 0,
            b: 0,
            a: 255
        }
    );
}
