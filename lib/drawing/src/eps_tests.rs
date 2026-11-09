//! Unit tests for the EPS writer.

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec;

use tairix_colour::Rgba;

use super::{eps, string_literal, MAX_LINE};
use crate::children::Children;
use crate::drawing::{Drawing, Grid, Page, Unit};
use crate::id::Ids;
use crate::node::{Kind, Node};
use crate::shape::{RectShape, Shape};
use crate::style::{Paint, Stroke, Style};
use crate::testing::OnlyA;
use crate::vector::tests::sample;
use crate::SharedSlice;
use tairix_util::fallible::Text;

#[test]
fn a_drawing_is_one_clean_page_of_level_three_eps() {
    let mut ids = Ids::new();
    let written = eps(&sample(&mut ids), &OnlyA).expect("written");
    assert!(!written.complete, "the b has no glyph");
    assert!(written.flattened, "its opacities are written opaque");
    assert!(
        written.bytes.iter().all(u8::is_ascii),
        "seven-bit clean as it declares"
    );
    let text = String::from_utf8(written.bytes).expect("ascii");
    assert!(text.starts_with("%!PS-Adobe-3.0 EPSF-3.0\n"));
    assert!(text.ends_with("%%EOF\n"));
    for expected in [
        "%%BoundingBox: 0 0 150 75\n",
        "%%HiResBoundingBox: 0 0 150 75\n",
        "%%Title: (Rivers & roads)\n",
        "%%LanguageLevel: 3\n",
        "[0.75 0 0 -0.75 0 75] concat\n",
        "rectfill\n",
        "clip newpath\n",
        " setdash\n",
        "3 setlinewidth",
        " shfill\n",
        "/ShadingType 3",
        "showpage\n",
    ] {
        assert!(text.contains(expected), "{expected:?} in:\n{text}");
    }
    let longest = text.lines().map(str::len).max().unwrap_or(0);
    assert!(longest <= MAX_LINE, "a line of {longest} characters");
}

#[test]
fn an_opaque_drawing_is_written_whole() {
    let mut ids = Ids::new();
    let rect = Node::new(
        ids.issue(),
        Kind::Shape {
            shape: Shape::Rect(RectShape {
                x: 10.0,
                y: 10.0,
                width: 20.0,
                height: 20.0,
                rx: 0.0,
                ry: 0.0,
            }),
            style: Arc::new(Style {
                fill: Paint::Colour(Rgba::new(0, 128, 255, 255)),
                ..Style::default()
            }),
        },
    )
    .expect("a rectangle");
    let layer = Node::new(
        ids.issue(),
        Kind::Group {
            children: Children::from_nodes(vec![Arc::new(rect)]).expect("held"),
            clip: None,
        },
    )
    .expect("a layer");
    let drawing = Drawing::new(
        Page {
            width: 1.0,
            height: 1.0,
            unit: Unit::In,
            background: None,
        },
        Children::from_nodes(vec![Arc::new(layer)]).expect("held"),
        SharedSlice::new(),
        Grid::default(),
    )
    .expect("a drawing");
    let written = eps(&drawing, &OnlyA).expect("written");
    assert!(!written.flattened, "nothing was translucent");
    let text = String::from_utf8(written.bytes).expect("ascii");
    assert!(
        text.contains("%%BoundingBox: 0 0 72 72\n"),
        "an inch is 72 points"
    );
    assert!(text.contains("0 0.502 1 setrgbcolor\n"), "{text}");
}

#[test]
fn each_run_of_a_text_is_painted_in_its_own_paint() {
    use tairix_bezier::Point;

    use crate::text::{Baseline, Run, RunPaint, RunStyle, Text, TextAlign};

    let mut ids = Ids::new();
    let text = Node::new(
        ids.issue(),
        Kind::Text {
            text: Arc::new(Text {
                content: crate::testing::shared("aa"),
                family: crate::testing::shared("sans-serif"),
                size: 20.0,
                weight: 400,
                italic: false,
                letter_spacing: 0.0,
                align: TextAlign::Start,
                baseline: Baseline::Lines {
                    origin: Point::new(10.0, 40.0),
                    line_height: 1.25,
                },
                runs: crate::testing::slice(&[Run {
                    start: 1,
                    end: 2,
                    style: Arc::new(RunStyle {
                        fill: Some(RunPaint::Colour(Rgba::new(0, 0, 255, 255))),
                        ..RunStyle::default()
                    }),
                }]),
            }),
            style: Arc::new(Style {
                fill: Paint::Colour(Rgba::new(255, 0, 0, 255)),
                ..Style::default()
            }),
        },
    )
    .expect("a text")
    .with_opacity(0.5)
    .expect("faded");
    let layer = Node::new(
        ids.issue(),
        Kind::Group {
            children: Children::from_nodes(vec![Arc::new(text)]).expect("held"),
            clip: None,
        },
    )
    .expect("a layer");
    let drawing = Drawing::new(
        Page {
            width: 1.0,
            height: 1.0,
            unit: Unit::In,
            background: None,
        },
        Children::from_nodes(vec![Arc::new(layer)]).expect("held"),
        SharedSlice::new(),
        Grid::default(),
    )
    .expect("a drawing");
    let written = eps(&drawing, &OnlyA).expect("written");
    assert!(
        written.flattened,
        "the runs overlap where they meet, so they fade as one"
    );
    let text = String::from_utf8(written.bytes).expect("ascii");
    let red = text.find("1 0 0 setrgbcolor\n").expect("the object's red");
    let blue = text.find("0 0 1 setrgbcolor\n").expect("the run's blue");
    assert!(red < blue, "in the order they are set:\n{text}");
}

fn literal(text: &str, room: usize) -> alloc::string::String {
    let mut out = tairix_util::fallible::Text::new();
    string_literal(&mut out, text, room).expect("spelled");
    out.into_string()
}

#[test]
fn a_title_is_escaped_seven_bit_and_kept_within_its_line() {
    assert_eq!(literal("(a) \\ é", MAX_LINE), "(\\(a\\) \\\\ \\303\\251)");
    let long = "é".repeat(300);
    let literal = literal(&long, 100);
    assert!(literal.len() <= 100 && literal.ends_with(')'));
    assert_eq!(
        (literal.len() - 2) % 8,
        0,
        "only whole characters, each eight characters of octal"
    );
}

/// The bytes ASCII base-85 `text` spells up to its `~>`.
fn base85(text: &str) -> alloc::vec::Vec<u8> {
    let mut bytes = alloc::vec::Vec::new();
    let mut group = alloc::vec::Vec::new();
    for c in text.chars().filter(|c| !c.is_whitespace()) {
        if c == '~' {
            break;
        }
        if c == 'z' {
            bytes.extend_from_slice(&[0; 4]);
            continue;
        }
        group.push(u32::from(c) - 33);
        if group.len() == 5 {
            let value = group.iter().fold(0_u32, |value, digit| value * 85 + digit);
            bytes.extend_from_slice(&value.to_be_bytes());
            group.clear();
        }
    }
    if !group.is_empty() {
        let kept = group.len() - 1;
        while group.len() < 5 {
            group.push(84);
        }
        let value = group.iter().fold(0_u32, |value, digit| value * 85 + digit);
        bytes.extend_from_slice(&value.to_be_bytes()[..kept]);
    }
    bytes
}

#[test]
fn base85_writes_four_bytes_as_five_digits_and_noughts_as_z() {
    let mut out = tairix_util::fallible::Text::new();
    super::ascii85(&mut out, b"Man \0\0\0\0is").expect("written");
    let text = out.as_str();
    assert_eq!(
        text, "\n 9jqo^zBla~>\n",
        "a group, four noughts, and two bytes in three"
    );
    assert_eq!(base85(text), b"Man \0\0\0\0is");
    let mut long = tairix_util::fallible::Text::new();
    super::ascii85(&mut long, &[0xA5; 1000]).expect("written");
    assert!(long
        .as_str()
        .lines()
        .all(|line| line.len() <= MAX_LINE && !line.starts_with('%')));
    assert_eq!(base85(long.as_str()), [0xA5; 1000]);
}

#[test]
fn pictures_are_images_a_baseline_jpeg_as_it_came_the_rest_deflated() {
    let mut ids = Ids::new();
    let (drawing, baseline) = crate::vector::tests::pictured(&mut ids);
    let written = eps(&drawing, &OnlyA).expect("written");
    assert!(
        !written.flattened,
        "a mask holds what is shown or hidden exactly"
    );
    assert!(written.bytes.iter().all(u8::is_ascii));
    let text = String::from_utf8(written.bytes).expect("ascii");
    assert!(text.lines().all(|line| line.len() <= MAX_LINE));
    assert_eq!(text.matches(" image\n").count(), 5, "every picture shown");
    assert_eq!(
        text.matches("/DCTDecode filter").count(),
        1,
        "the baseline JPEG alone"
    );
    assert_eq!(
        text.matches("/ImageType 3 /InterleaveType 1").count(),
        2,
        "the clear one, twice"
    );
    let jpeg = text
        .split("/DCTDecode filter >> image")
        .nth(1)
        .expect("its data");
    assert_eq!(base85(jpeg), baseline);
}

#[test]
fn a_picture_shown_twice_is_deflated_once_and_written_where_each_is_shown() {
    use crate::vector::Sink as _;
    let place = |x1, y1| {
        tairix_bezier::Bounds::from_points([
            tairix_bezier::Point::ZERO,
            tairix_bezier::Point::new(x1, y1),
        ])
    };
    let colour = |x: u32, y: u32| {
        [
            u8::try_from(x * 40).unwrap_or(0),
            u8::try_from(y * 90).unwrap_or(0),
            7,
            255,
        ]
    };
    let picture =
        crate::image::Image::new(crate::testing::pixels(3, 2, colour), place(3.0, 2.0), None)
            .expect("a picture");
    let again = picture.placed(place(12.0, 8.0)).expect("again");
    let mut sink = super::EpsSink {
        flattened: false,
        encoder: None,
        packed: tairix_collections::OrderedMap::new(),
    };
    let (mut first, mut second) = (Text::new(), Text::new());
    sink.image(&mut first, &picture, 1.0).expect("written");
    sink.image(&mut second, &again, 1.0).expect("written");
    assert_eq!(sink.packed.len(), 1, "deflated once");
    let data = |text: &Text| {
        text.as_str()
            .split_once(" image")
            .map(|(_, data)| String::from(data))
    };
    assert_eq!(
        data(&first),
        data(&second),
        "the same data where each is shown"
    );
    assert_ne!(
        first.as_str(),
        second.as_str(),
        "each laid where it is shown"
    );
}

#[test]
fn a_partly_shown_pixel_is_written_shown_or_not_and_said_to_be() {
    let mut ids = Ids::new();
    let picture = crate::image::Image::new(
        crate::testing::pixels(1, 1, |_, _| [0, 0, 0, 100]),
        tairix_bezier::Bounds::from_points([
            tairix_bezier::Point::ZERO,
            tairix_bezier::Point::new(5.0, 5.0),
        ]),
        None,
    )
    .expect("a picture");
    let layer = Node::new(
        ids.issue(),
        Kind::Group {
            children: Children::from_nodes(vec![crate::testing::image(
                &mut ids,
                picture,
                tairix_bezier::Affine::IDENTITY,
            )])
            .expect("held"),
            clip: None,
        },
    )
    .expect("a layer");
    let drawing = Drawing::new(
        Page {
            width: 10.0,
            height: 10.0,
            unit: Unit::Px,
            background: None,
        },
        Children::from_nodes(vec![Arc::new(layer)]).expect("held"),
        SharedSlice::new(),
        Grid::default(),
    )
    .expect("a drawing");
    assert!(eps(&drawing, &OnlyA).expect("written").flattened);
}

/// A stroke dashed as long as a pattern may be still writes every line within
/// the conventions, its pattern whole and in order.
#[test]
fn the_longest_dash_pattern_keeps_every_line_within_the_conventions() {
    let dashes: alloc::vec::Vec<f64> = (0..crate::limits::MAX_DASH_ENTRIES)
        .map(|at| 123_456_789.125 + f64::from(u32::try_from(at).expect("few")))
        .collect();
    let mut ids = Ids::new();
    let rect = Node::new(
        ids.issue(),
        Kind::Shape {
            shape: Shape::Rect(RectShape {
                x: 10.0,
                y: 10.0,
                width: 20.0,
                height: 20.0,
                rx: 0.0,
                ry: 0.0,
            }),
            style: Arc::new(Style {
                stroke: Stroke {
                    paint: Paint::Colour(Rgba::new(0, 0, 0, 255)),
                    dashes: SharedSlice::try_from_slice(&dashes).expect("held"),
                    ..Stroke::default()
                },
                ..Style::default()
            }),
        },
    )
    .expect("a rectangle");
    let layer = Node::new(
        ids.issue(),
        Kind::Group {
            children: Children::from_nodes(vec![Arc::new(rect)]).expect("held"),
            clip: None,
        },
    )
    .expect("a layer");
    let drawing = Drawing::new(
        Page {
            width: 1.0,
            height: 1.0,
            unit: Unit::In,
            background: None,
        },
        Children::from_nodes(vec![Arc::new(layer)]).expect("held"),
        SharedSlice::new(),
        Grid::default(),
    )
    .expect("a drawing");
    let text = String::from_utf8(eps(&drawing, &OnlyA).expect("written").bytes).expect("ascii");
    let longest = text.lines().map(str::len).max().unwrap_or(0);
    assert!(longest <= MAX_LINE, "a line of {longest} characters");
    let tokens: alloc::vec::Vec<&str> = text.split_whitespace().collect();
    let open = tokens
        .iter()
        .position(|&token| token == "[")
        .expect("a pattern");
    let pattern: alloc::vec::Vec<&str> = tokens[open + 1..]
        .iter()
        .take_while(|&&token| token != "]")
        .copied()
        .collect();
    let expected: alloc::vec::Vec<String> = dashes
        .iter()
        .map(|dash| alloc::format!("{}", crate::vector::Real(*dash)))
        .collect();
    assert_eq!(pattern, expected, "the pattern whole and in order");
}
