//! Unit tests for the PDF writer.

use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;

use tairix_bezier::{Affine, Path, PathEl, Point, Subpath};
use tairix_colour::Rgba;

use super::{pdf, Real, TextString};
use crate::children::Children;
use crate::drawing::{Drawing, Grid, Info, Page, Unit};
use crate::id::Ids;
use crate::node::{Kind, Node};
use crate::shape::{EllipseShape, RectShape, Shape};
use crate::style::{Gradient, GradientKind, Paint, Spread, Stop, Stroke, Style};
use crate::text::{Glyph, Glyphs, Text, TextAlign};

struct OnlyA;

impl Glyphs for OnlyA {
    fn glyph(&self, _family: &str, _weight: u16, _italic: bool, c: char) -> Option<Arc<Glyph>> {
        (c == 'a').then(|| {
            let mut ring = Subpath::new(Point::new(0.0, -1.0));
            ring.elements.extend([
                PathEl::Line(Point::new(1.0, -1.0)),
                PathEl::Line(Point::new(1.0, 0.0)),
                PathEl::Line(Point::new(0.0, 0.0)),
            ]);
            ring.closed = true;
            Arc::new(Glyph::new(
                Path {
                    subpaths: vec![ring],
                },
                1.0,
            ))
        })
    }
}

fn node(ids: &mut Ids, kind: Kind) -> Arc<Node> {
    Arc::new(Node::new(ids.issue(), kind).expect("a node"))
}

fn rect(ids: &mut Ids, x: f64, style: Style) -> Arc<Node> {
    node(
        ids,
        Kind::Shape {
            shape: Shape::Rect(RectShape {
                x,
                y: 10.0,
                width: 40.0,
                height: 30.0,
                rx: 0.0,
                ry: 0.0,
            }),
            style: Arc::new(style),
        },
    )
}

fn gradient(kind: GradientKind, spread: Spread) -> Paint {
    Paint::Gradient(Arc::new(Gradient {
        kind,
        stops: vec![
            Stop {
                offset: 0.0,
                colour: Rgba::new(255, 0, 0, 255),
            },
            Stop {
                offset: 0.5,
                colour: Rgba::new(0, 255, 0, 128),
            },
            Stop {
                offset: 1.0,
                colour: Rgba::new(0, 0, 255, 255),
            },
        ],
        spread,
        transform: Affine::IDENTITY,
    }))
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

/// A reflected radial fill, stroked with a repeated linear gradient.
fn radial() -> Style {
    Style {
        fill: gradient(
            GradientKind::Radial {
                centre: Point::new(120.0, 25.0),
                radius: 5.0,
                focus: Point::new(121.0, 25.0),
            },
            Spread::Reflect,
        ),
        stroke: Stroke {
            paint: gradient(
                GradientKind::Linear {
                    start: Point::new(0.0, 0.0),
                    end: Point::new(10.0, 10.0),
                },
                Spread::Repeat,
            ),
            width: 2.0,
            ..Stroke::default()
        },
        ..Style::default()
    }
}

/// A drawing of most of what PDF is asked to say.
fn sample(ids: &mut Ids) -> Drawing {
    let stroked = Style {
        fill: Paint::Colour(Rgba::new(10, 20, 30, 255)),
        stroke: Stroke {
            paint: Paint::Colour(Rgba::new(200, 100, 0, 128)),
            width: 3.0,
            dashes: Arc::from(&[4.0, 2.0][..]),
            dash_offset: 1.0,
            ..Stroke::default()
        },
        ..Style::default()
    };
    let linear = Style {
        fill: gradient(
            GradientKind::Linear {
                start: Point::new(0.0, 0.0),
                end: Point::new(40.0, 0.0),
            },
            Spread::Pad,
        ),
        stroke: Stroke {
            paint: Paint::None,
            ..Stroke::default()
        },
        ..Style::default()
    };
    let window_a = rect(ids, 0.0, Style::default());
    let window_b = node(
        ids,
        Kind::Shape {
            shape: Shape::Ellipse(EllipseShape {
                centre: Point::new(60.0, 25.0),
                radii: Point::new(15.0, 15.0),
                arc: None,
            }),
            style: Arc::new(Style::default()),
        },
    );
    let windows = Arc::new(group(ids, vec![window_a, window_b], None));
    let content = vec![rect(ids, 0.0, stroked.clone())];
    let clipped = group(ids, content, Some(windows));
    let pair = vec![rect(ids, 50.0, stroked), rect(ids, 60.0, linear)];
    let half = group(ids, pair, None).with_opacity(0.5).expect("weakened");
    let text = node(
        ids,
        Kind::Text {
            text: Arc::new(Text {
                content: Arc::from("ab"),
                family: Arc::from("sans-serif"),
                size: 12.0,
                weight: 400,
                italic: false,
                letter_spacing: 0.0,
                line_height: 1.25,
                align: TextAlign::Start,
                origin: Point::new(10.0, 80.0),
            }),
            style: Arc::new(Style::default()),
        },
    );
    let shaded = rect(ids, 100.0, radial());
    let layer = group(
        ids,
        vec![Arc::new(clipped), Arc::new(half), shaded, text],
        None,
    );
    Drawing::new(
        Page {
            width: 200.0,
            height: 100.0,
            unit: Unit::Px,
            background: Some(Rgba::new(250, 250, 240, 255)),
        },
        Children::from_nodes(vec![Arc::new(layer)]).expect("held"),
        Arc::from(&[][..]),
        Grid::default(),
    )
    .expect("a drawing")
    .with_info(Info {
        title: Some(Arc::from("Rivers & roads")),
        description: None,
    })
    .expect("titled")
}

/// The file's objects, checked against its cross-reference table: each
/// number's body, a stream's content inflated.
fn objects(file: &[u8]) -> Vec<(usize, String, Option<Vec<u8>>)> {
    assert!(file.starts_with(b"%PDF-1.4\n"));
    let marker = b"startxref\n";
    let start = file
        .windows(marker.len())
        .rposition(|window| window == marker)
        .expect("startxref")
        + marker.len();
    let ascii = |bytes: &[u8]| String::from_utf8(bytes.to_vec()).expect("ascii");
    let table: usize = ascii(&file[start..])
        .lines()
        .next()
        .expect("an offset")
        .parse()
        .expect("a number");
    let tail = ascii(&file[table..]);
    assert!(tail.starts_with("xref\n0 "));
    let mut lines = tail.lines().skip(1);
    let count: usize = lines
        .next()
        .expect("a count")
        .split(' ')
        .nth(1)
        .expect("count")
        .parse()
        .expect("n");
    lines.next();
    let mut found = Vec::new();
    for number in 1..count {
        let entry = lines.next().expect("an entry");
        let offset: usize = entry[..10].parse().expect("an offset");
        let head = alloc::format!("{number} 0 obj\n");
        assert!(
            file[offset..].starts_with(head.as_bytes()),
            "object {number} where its entry says"
        );
        let body_start = offset + head.len();
        let end = body_start + find(&file[body_start..], b"\nendobj\n").expect("endobj");
        let body = &file[body_start..end];
        let stream = find(body, b"\nstream\n").map(|at| {
            let dictionary = String::from_utf8_lossy(&body[..at]).into_owned();
            let length: usize = dictionary
                .split("/Length ")
                .nth(1)
                .and_then(|rest| rest.split(' ').next())
                .expect("a length")
                .parse()
                .expect("a number");
            let data = &body[at + 8..];
            assert_eq!(
                &data[length..],
                b"\nendstream",
                "the length is the stream's"
            );
            let mut inflated = vec![0u8; 1 << 20];
            let size = tairix_compress::zlib::decompress_into(&data[..length], &mut inflated)
                .expect("inflates");
            inflated.truncate(size);
            inflated
        });
        let head_text =
            String::from_utf8_lossy(&body[..find(body, b"\nstream\n").unwrap_or(body.len())])
                .into_owned();
        found.push((number, head_text, stream));
    }
    found
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[test]
fn a_drawing_is_one_page_of_sound_pdf() {
    let mut ids = Ids::new();
    let written = pdf(&sample(&mut ids), &OnlyA).expect("written");
    assert!(!written.complete, "the b has no glyph");
    let objects = objects(&written.bytes);
    let page = objects
        .iter()
        .find(|(number, ..)| *number == super::PAGE)
        .expect("the page");
    assert!(
        page.1.contains("/MediaBox [0 0 150 75]"),
        "points, not pixels: {}",
        page.1
    );
    let info = objects
        .iter()
        .find(|(number, ..)| *number == super::INFO)
        .expect("info");
    assert!(info.1.contains(&TextString("Rivers & roads").to_string()));
    let content = objects
        .iter()
        .find(|(number, ..)| *number == super::CONTENT)
        .and_then(|(_, _, stream)| stream.clone())
        .expect("content");
    let content = String::from_utf8(content).expect("text");
    for operator in [
        "0.75 0 0 -0.75 0 75 cm",
        "re f",
        "W n",
        " d\n",
        "RG 3 w",
        " Do Q",
        " sh",
    ] {
        assert!(content.contains(operator), "{operator:?} in:\n{content}");
    }
    let forms: Vec<_> = objects
        .iter()
        .filter(|(_, head, _)| head.contains("/Subtype /Form"))
        .collect();
    assert!(
        forms
            .iter()
            .any(|(_, head, _)| head.contains("/CS /DeviceGray")),
        "a soft mask for the stops' alpha"
    );
    assert!(
        forms
            .iter()
            .any(|(_, head, _)| head.contains("/CS /DeviceRGB")),
        "a group for the opacity"
    );
    let resources = objects
        .iter()
        .find(|(number, ..)| *number == super::RESOURCES)
        .expect("resources");
    assert!(resources.1.contains("/ShadingType 3"), "a radial shading");
    assert!(
        resources.1.contains("/Encode [ 0 1 1 0"),
        "reflected periods: {}",
        resources.1
    );
    assert!(resources.1.contains("/SMask"));
}

#[test]
fn numbers_are_spelled_as_pdf_reads_them() {
    assert_eq!(Real(1e-300).to_string(), "0");
    assert_eq!(Real(-0.000_01).to_string(), "0");
    assert_eq!(Real(12.5).to_string(), "12.5");
    assert_eq!(Real(1e9).to_string(), "1000000000");
    assert_eq!(Real(f64::NAN).to_string(), "0");
    assert!(!Real(1e20).to_string().contains('e'));
    assert_eq!(
        TextString("é)").to_string(),
        "<FEFF00E90029>",
        "nothing escapes a hex string"
    );
}
