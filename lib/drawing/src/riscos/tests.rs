//! Draw files read from bytes built by hand, and drawings written as Draw
//! files and read back.

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;

use tairix_bezier::{Affine, FillRule, Point};
use tairix_colour::Rgba;

use super::{face_of, fpa_double, fpa_words, read, recognised, write, Dropped, Font};
use crate::children::Children;
use crate::drawing::{Drawing, Grid, Page, Unit, PAPER};
use crate::id::Ids;
use crate::limits::MAX_DEPTH;
use crate::node::{Kind, Node};
use crate::shape::{Contour, EllipseShape, PathNode, PathShape, RectShape, Shape};
use crate::style::{ArrowShape, Arrowhead, Cap, Join, Paint, PaintOrder, Stroke, Style};
use crate::testing::OnlyA;
use crate::testing::{origin, shared, slice};
use crate::text::{Baseline, Text, TextAlign};
use crate::unkept::Lost;
use crate::vector::tests::sample;
use crate::SharedSlice;
use crate::{Read, ReadError};

/// Draw units to the pixel.
const PX: i32 = 480;

const RED: u32 = 0x0000_FF00;
const BLUE: u32 = 0xFF00_0000;
const NONE: u32 = 0xFFFF_FFFF;

/// `value` as the word a Draw file holds it in.
fn int(value: i32) -> u32 {
    u32::from_le_bytes(value.to_le_bytes())
}

fn words(words: &[u32]) -> Vec<u8> {
    words.iter().flat_map(|word| word.to_le_bytes()).collect()
}

/// An object of `kind` around `body`, its bounds zero.
fn object(kind: u32, body: &[u8]) -> Vec<u8> {
    let size = u32::try_from(24 + body.len()).expect("small");
    let mut bytes = words(&[kind, size, 0, 0, 0, 0]);
    bytes.extend_from_slice(body);
    bytes
}

/// `text`, ended with a zero and padded to a word.
fn string(text: &[u8]) -> Vec<u8> {
    let mut bytes = text.to_vec();
    bytes.push(0);
    while !bytes.len().is_multiple_of(4) {
        bytes.push(0);
    }
    bytes
}

/// A Draw file of `objects`.
fn file(objects: &[Vec<u8>]) -> Vec<u8> {
    let mut bytes = b"Draw".to_vec();
    bytes.extend(words(&[201, 0]));
    bytes.extend_from_slice(b"Tests       ");
    bytes.extend(words(&[0; 4]));
    for object in objects {
        bytes.extend_from_slice(object);
    }
    bytes
}

/// A path object filled `fill`, outlined `outline` `width` wide, styled
/// `style`, of `components`, each given in whole pixels.
fn path(fill: u32, outline: u32, width: u32, style: u32, components: &[i32]) -> Vec<u8> {
    let mut body = words(&[fill, outline, width, style]);
    let mut at = 0;
    while at < components.len() {
        let tag = components[at];
        let points = match tag {
            2 | 7 | 8 => 1,
            6 => 3,
            _ => 0,
        };
        body.extend(words(&[int(tag)]));
        for coordinate in &components[at + 1..at + 1 + 2 * points] {
            body.extend(words(&[int(coordinate * PX)]));
        }
        at += 1 + 2 * points;
    }
    object(2, &body)
}

/// An options object for A`paper`, turned when `landscape`, its grid
/// `spacing` centimetres apart.
fn options(paper: u32, landscape: bool, spacing: f64) -> Vec<u8> {
    let [high, low] = fpa_words(spacing);
    let turned = if landscape { 1 << 4 } else { 0 };
    object(
        11,
        &words(&[
            (paper + 1) << 8,
            turned,
            high,
            low,
            2,
            0,
            1,
            0,
            0,
            1,
            1,
            1,
            0,
            1,
            0x80,
            5000,
        ]),
    )
}

/// A text object in font `font`, `points` tall and `wide` across, its
/// baseline from the pixel `(x, y)` up from the paper's foot.
fn text(font: u32, (points, wide): (u32, u32), (x, y): (i32, i32), content: &[u8]) -> Vec<u8> {
    let mut body = words(&[
        RED,
        0xFFFF_FF00,
        font,
        wide * 640,
        points * 640,
        int(x * PX),
        int(y * PX),
    ]);
    body.extend(string(content));
    object(1, &body)
}

fn font_table(entries: &[(u8, &[u8])]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for (number, name) in entries {
        bytes.push(*number);
        bytes.extend_from_slice(name);
        bytes.push(0);
    }
    while !bytes.len().is_multiple_of(4) {
        bytes.push(0);
    }
    let mut table = words(&[0, u32::try_from(8 + bytes.len()).expect("small")]);
    table.extend(bytes);
    table
}

fn read_file(objects: &[Vec<u8>]) -> Read {
    read(&file(objects), &mut Ids::new()).expect("read")
}

/// The objects of a drawing's one layer.
fn objects(drawing: &Drawing) -> Vec<Arc<Node>> {
    assert_eq!(drawing.layers().len(), 1, "one layer");
    drawing
        .layers()
        .get(0)
        .and_then(|layer| layer.children())
        .map(|children| children.iter().cloned().collect())
        .unwrap_or_default()
}

fn shape_of(node: &Node) -> (&PathShape, &Style) {
    match node.kind() {
        Kind::Shape {
            shape: Shape::Path(path),
            style,
        } => (path, style),
        other => panic!("a path, not {other:?}"),
    }
}

fn text_of(node: &Node) -> (&Text, &Style) {
    match node.kind() {
        Kind::Text { text, style } => (text, style),
        other => panic!("a text, not {other:?}"),
    }
}

fn points(contour: &Contour) -> Vec<Point> {
    contour.nodes.iter().map(|node| node.point).collect()
}

fn near(a: Point, b: Point) -> bool {
    a.distance(b) <= 1.0 / f64::from(PX)
}

#[test]
fn a_draw_file_is_told_apart_by_its_signature() {
    let draw = file(&[]);
    assert!(recognised(&draw));
    assert!(!recognised(b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>"));
    let mut ids = Ids::new();
    assert!(crate::read(&draw, &mut ids).is_ok(), "read as Draw");
    let svg = b"<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"10\" height=\"10\"/>";
    assert!(crate::read(svg, &mut ids).is_ok(), "read as SVG");
}

#[test]
fn a_path_reads_as_its_points_on_the_paper_its_options_name() {
    let style = 1 | 1 << 2 | 1 << 4 | 1 << 6;
    let read = read_file(&[
        options(5, true, 1.0),
        path(
            RED,
            BLUE,
            480,
            style,
            &[2, 0, 0, 8, 10, 0, 6, 20, 0, 20, 10, 10, 10, 5, 0],
        ),
    ]);
    assert!(read.unkept.is_empty());
    let page = read.drawing.page();
    assert_eq!(
        (page.width, page.height, page.unit),
        (210.0, 148.0, Unit::Mm),
        "A5 turned"
    );
    assert!((read.drawing.grid().spacing - Unit::Cm.pixels()).abs() < 1e-9);
    assert_eq!(read.drawing.grid().subdivisions, 2);
    let height = page.size().y;
    let objects = objects(&read.drawing);
    let (shape, style) = shape_of(&objects[0]);
    assert_eq!(style.fill, Paint::Colour(Rgba::new(255, 0, 0, 255)));
    assert_eq!(style.fill_rule, FillRule::EvenOdd);
    let stroke = &style.stroke;
    assert_eq!(stroke.paint, Paint::Colour(Rgba::new(0, 0, 255, 255)));
    assert_eq!(
        (stroke.width, stroke.join, stroke.cap),
        (1.0, Join::Round, Cap::Round)
    );
    assert!(!stroke.start.draws() && !stroke.end.draws());
    assert_eq!(stroke.miter_limit.to_bits(), 10.0_f64.to_bits());
    let [contour] = shape.contours.as_slice() else {
        panic!("one contour");
    };
    assert!(contour.closed);
    assert_eq!(
        points(contour),
        [
            Point::new(0.0, height),
            Point::new(10.0, height),
            Point::new(10.0, height - 10.0)
        ]
    );
    assert_eq!(contour.nodes[1].after, Some(Point::new(20.0, height)));
    assert_eq!(
        contour.nodes[2].before,
        Some(Point::new(20.0, height - 10.0))
    );
}

#[test]
fn a_shape_closed_at_its_start_is_one_contour_closed_there() {
    let read = read_file(&[path(
        RED,
        NONE,
        0,
        0,
        &[2, 0, 0, 8, 10, 0, 8, 10, 10, 8, 0, 0, 5, 0],
    )]);
    let objects = objects(&read.drawing);
    let (shape, _) = shape_of(&objects[0]);
    let [contour] = shape.contours.as_slice() else {
        panic!("one contour");
    };
    assert!(contour.closed);
    assert_eq!(
        contour.nodes.len(),
        3,
        "the last node merged into the first"
    );
}

#[test]
fn a_gap_breaks_a_stroke_but_edges_its_fill() {
    let gapped = [2, 0, 0, 8, 10, 0, 7, 10, 10, 8, 0, 10, 5, 0];
    let both = read_file(&[path(RED, BLUE, 480, 0, &gapped)]);
    let objects = objects(&both.drawing);
    let parts: Vec<Arc<Node>> = objects[0]
        .children()
        .expect("the fill and the stroke, grouped")
        .iter()
        .cloned()
        .collect();
    let (fill, fill_style) = shape_of(&parts[0]);
    assert!(!fill_style.stroke.paints() && fill_style.fill.paints());
    assert_eq!(fill.contours.len(), 1);
    assert_eq!(
        fill.contours[0].nodes.len(),
        4,
        "the gap an edge like any other"
    );
    let (stroke, stroke_style) = shape_of(&parts[1]);
    assert!(stroke_style.stroke.paints() && !stroke_style.fill.paints());
    let height = both.drawing.page().size().y;
    let [run] = stroke.contours.as_slice() else {
        panic!("the closing line carries the last run on into the first");
    };
    assert!(!run.closed);
    assert_eq!(
        points(run),
        [
            Point::new(10.0, height - 10.0),
            Point::new(0.0, height - 10.0),
            Point::new(0.0, height),
            Point::new(10.0, height),
        ]
    );
    let outlined = objects_of(&read_file(&[path(NONE, BLUE, 480, 0, &gapped)]));
    let (alone, _) = shape_of(&outlined[0]);
    assert_eq!(
        alone.contours.len(),
        1,
        "an outline alone is the stroke's reading"
    );
    let filled = objects_of(&read_file(&[path(RED, NONE, 0, 0, &gapped)]));
    let (alone, _) = shape_of(&filled[0]);
    assert_eq!(
        alone.contours[0].nodes.len(),
        4,
        "a fill alone is the fill's reading"
    );
}

fn objects_of(read: &Read) -> Vec<Arc<Node>> {
    objects(&read.drawing)
}

#[test]
fn a_sub_path_closed_by_a_gap_is_stroked_open() {
    let objects = objects_of(&read_file(&[path(
        RED,
        BLUE,
        480,
        0,
        &[2, 0, 0, 8, 10, 0, 8, 10, 10, 4, 0],
    )]));
    let (shape, _) = shape_of(&objects[0]);
    assert!(!shape.contours[0].closed, "its fill closes it all the same");
    assert_eq!(shape.contours[0].nodes.len(), 3);
}

#[test]
fn the_thinnest_outline_is_a_pixel_wide_and_said_to_scale() {
    let thin = read_file(&[path(NONE, BLUE, 0, 0, &[2, 0, 0, 8, 10, 0, 0])]);
    let width = shape_of(&objects_of(&thin)[0]).1.stroke.width;
    assert_eq!(width.to_bits(), 1.0_f64.to_bits());
    assert!(thin.unkept.contains(Lost::NonScalingStrokes));
    let unstroked = read_file(&[path(RED, NONE, 0, 0, &[2, 0, 0, 8, 10, 0, 8, 0, 10, 0])]);
    assert!(unstroked.unkept.is_empty(), "no outline, no hairline");
}

#[test]
fn caps_read_as_the_drawings_cap_or_as_its_arrowheads() {
    let cap = |style: u32| {
        let read = read_file(&[path(NONE, BLUE, 960, style, &[2, 0, 0, 8, 10, 0, 0])]);
        let stroke = shape_of(&objects_of(&read)[0]).1.stroke.clone();
        (stroke.cap, stroke.start, stroke.end)
    };
    assert_eq!(
        cap(2 << 2 | 2 << 4),
        (Cap::Square, Arrowhead::NONE, Arrowhead::NONE)
    );
    let triangle = 3 << 4 | 32 << 16 | 48 << 24;
    let (plain, start, end) = cap(triangle);
    assert_eq!(plain, Cap::Butt);
    assert_eq!(
        start,
        Arrowhead {
            shape: ArrowShape::Triangle,
            width: 4.0,
            length: 3.0,
        },
        "two widths each side, three along"
    );
    assert_eq!(end, Arrowhead::NONE);
    let (plain, start, end) = cap(1 << 4);
    assert_eq!(
        (plain, start.shape, start.width),
        (Cap::Butt, ArrowShape::Circle, 1.0)
    );
    assert_eq!(end, Arrowhead::NONE, "a round start, a butt end");
}

#[test]
fn a_dash_pattern_reads_in_pixels() {
    let mut body = words(&[NONE, BLUE, 480, 1 << 7, 480, 2, 960, 480]);
    body.extend(words(&[2, 0, 0, 8, int(10 * PX), 0, 0]));
    let read = read_file(&[object(2, &body)]);
    let stroke = shape_of(&objects_of(&read)[0]).1.stroke.clone();
    assert_eq!(&*stroke.dashes, &[2.0, 1.0]);
    assert_eq!(stroke.dash_offset.to_bits(), 1.0_f64.to_bits());
}

#[test]
fn text_is_set_in_the_font_its_table_names_wherever_the_table_stands() {
    let read = read_file(&[
        text(1, (12, 12), (10, 20), b"Caf\xe9 \x8c"),
        text(7, (10, 10), (10, 40), b"System"),
        text(2, (10, 20), (10, 60), b"Wide"),
        font_table(&[(1, b"Trinity.Bold.Italic"), (2, b"Fancy.Light")]),
    ]);
    let height = read.drawing.page().size().y;
    let objects = objects_of(&read);
    let (first, style) = text_of(&objects[0]);
    assert_eq!(&*first.content, "Café …");
    assert_eq!(
        (&*first.family, first.weight, first.italic),
        ("serif", 700, true)
    );
    assert_eq!(first.size.to_bits(), 16.0_f64.to_bits(), "twelve points");
    assert_eq!(origin(first), Point::new(10.0, height - 20.0));
    assert_eq!(objects[0].transform(), Affine::IDENTITY);
    assert_eq!(style.fill, Paint::Colour(Rgba::new(255, 0, 0, 255)));
    let (system, _) = text_of(&objects[1]);
    assert_eq!(
        &*system.family, "monospace",
        "a number the table lacks is the system font"
    );
    let (wide, _) = text_of(&objects[2]);
    assert_eq!((&*wide.family, wide.weight), ("Fancy, sans-serif", 300));
    assert_eq!(origin(wide), Point::ZERO);
    let placed = objects[2].transform();
    assert!(near(
        placed.map(Point::ZERO),
        Point::new(10.0, height - 60.0)
    ));
    assert!(
        near(
            placed.map_vector(Point::new(1.0, 0.0)),
            Point::new(2.0, 0.0)
        ),
        "twice as wide"
    );
}

#[test]
fn transformed_text_is_turned_as_its_matrix_says() {
    let turned = |matrix: [i32; 6], flags: u32| {
        let mut body = words(&matrix.map(int));
        body.extend(words(&[
            flags,
            BLUE,
            0xFFFF_FF00,
            0,
            6400,
            6400,
            int(10 * PX),
            int(10 * PX),
        ]));
        body.extend(string(b"Up"));
        read_file(&[object(12, &body)])
    };
    let quarter = turned([0, 0x1_0000, -0x1_0000, 0, 0, 0], 0);
    assert!(quarter.unkept.is_empty());
    let objects = objects_of(&quarter);
    let along = objects[0].transform().map_vector(Point::new(1.0, 0.0));
    assert!(
        near(along, Point::new(0.0, -1.0)),
        "a quarter turn up the page: {along:?}"
    );
    let plain = turned([0x1_0000, 0, 0, 0x1_0000, 0, 0], 1 << 1);
    let objects = objects_of(&plain);
    assert_eq!(
        objects[0].transform(),
        Affine::IDENTITY,
        "an untransformed matrix"
    );
    assert!(
        plain.unkept.contains(Lost::TextPositioning),
        "right to left"
    );
}

#[test]
fn what_a_drawing_cannot_hold_is_said_to_be_lost() {
    let line = path(NONE, BLUE, 480, 0, &[2, 0, 0, 8, 10, 0, 0]);
    let mut tagged = words(&[0x1234]);
    tagged.extend_from_slice(&line);
    tagged.extend(words(&[0xAAAA_AAAA]));
    let mut jpeg = words(&[100, 100, 90, 90, 0x1_0000, 0, 0, 0x1_0000, 0, 0, 4]);
    jpeg.extend_from_slice(&[1, 2, 3, 4]);
    let read = read_file(&[
        object(5, &[0; 16]),
        object(16, &jpeg),
        object(9, &words(&[10, 24, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0])),
        object(7, &tagged),
        object(99, &[]),
    ]);
    for lost in [
        Lost::Images,
        Lost::TextAreas,
        Lost::Annotations,
        Lost::ForeignContent,
    ] {
        assert!(read.unkept.contains(lost), "{lost:?}");
    }
    assert_eq!(objects_of(&read).len(), 1, "the tagged object's path");
}

#[test]
fn a_groups_name_is_its_bytes_without_their_padding() {
    let line = path(NONE, BLUE, 480, 0, &[2, 0, 0, 8, 10, 0, 0]);
    let named = |name: &[u8; 12]| {
        let mut body = name.to_vec();
        body.extend_from_slice(&line);
        object(6, &body)
    };
    let read = read_file(&[named(b"Fred        "), named(b"            ")]);
    let objects = objects_of(&read);
    assert_eq!(objects[0].name(), Some("Fred"));
    assert_eq!(objects[0].children().map(Children::len), Some(1));
    assert_eq!(objects[1].name(), None);
}

#[test]
fn the_first_options_name_the_paper_and_a_file_without_is_a4() {
    let read = read_file(&[options(3, false, 0.5), options(1, true, 2.0)]);
    let page = read.drawing.page();
    assert_eq!((page.width, page.height), (297.0, 420.0), "A3, the first");
    let bare = read_file(&[]);
    assert_eq!(*bare.drawing.page(), Page::of(&PAPER[0]));
}

#[test]
fn a_broken_file_is_refused_whole() {
    let line = |components: &[i32]| path(NONE, BLUE, 480, 0, components);
    let refused = |bytes: &[u8]| read(bytes, &mut Ids::new()).err();
    assert_eq!(refused(b"<svg/>"), Some(ReadError::Unrecognised));
    let mut later = file(&[]);
    later[4] = 202;
    assert_eq!(
        refused(&later),
        Some(ReadError::Unrecognised),
        "a later version"
    );
    assert_eq!(
        refused(b"Draw\xc9\0\0\0"),
        Some(ReadError::Malformed),
        "no whole header"
    );
    let mut odd = file(&[line(&[2, 0, 0, 8, 1, 1, 0])]);
    odd[44] = 45;
    assert_eq!(
        refused(&odd),
        Some(ReadError::Malformed),
        "a size not of words"
    );
    let mut long = file(&[line(&[2, 0, 0, 8, 1, 1, 0])]);
    long[44] = 200;
    assert_eq!(
        refused(&long),
        Some(ReadError::Malformed),
        "a size past the file"
    );
    for components in [
        &[8, 1, 1, 0][..],
        &[2, 0, 0, 3, 1, 1, 0],
        &[2, 0, 0, 8, 1, 1],
        &[2, 0, 0, 5, 8, 1, 1, 0],
    ] {
        assert_eq!(
            refused(&file(&[line(components)])),
            Some(ReadError::Malformed),
            "{components:?}"
        );
    }
    let bad_join = path(NONE, BLUE, 480, 3, &[2, 0, 0, 8, 1, 1, 0]);
    assert_eq!(refused(&file(&[bad_join])), Some(ReadError::Malformed));
    let flat = text(0, (0, 10), (0, 0), b"Flat");
    assert_eq!(
        refused(&file(&[flat])),
        Some(ReadError::Malformed),
        "no height"
    );
    let mut endless = words(&[RED, 0, 0, 6400, 6400, 0, 0]);
    endless.extend_from_slice(b"abcd");
    assert_eq!(
        refused(&file(&[object(1, &endless)])),
        Some(ReadError::Malformed)
    );
    assert_eq!(
        refused(&file(&[object(7, &words(&[1]))])),
        Some(ReadError::Malformed)
    );
    let mut deep = line(&[2, 0, 0, 8, 1, 1, 0]);
    for _ in 0..=MAX_DEPTH {
        let mut body = b"            ".to_vec();
        body.extend(deep);
        deep = object(6, &body);
    }
    assert_eq!(refused(&file(&[deep])), Some(ReadError::TooComplex));
}

#[test]
fn doubles_are_stored_sign_and_exponent_first() {
    assert_eq!(fpa_words(1.0), [0x3FF0_0000, 0]);
    for value in [0.0, 2.54, -1e-300, 1e300] {
        assert_eq!(fpa_double(fpa_words(value)).to_bits(), value.to_bits());
    }
}

#[test]
fn risc_os_fonts_map_to_the_drawings_families_and_back() {
    let face = face_of("Homerton.Medium.Oblique").expect("a face");
    assert_eq!(
        (&*face.family, face.weight, face.italic),
        ("sans-serif", 400, true)
    );
    let blank = face_of(".Bold").expect("a face");
    assert_eq!(&*blank.family, "sans-serif");
    let quoted = face_of("Odd,\"Name\".Black").expect("a face");
    assert_eq!(
        (&*quoted.family, quoted.weight),
        ("OddName, sans-serif", 900)
    );
    let spelled = |(font, first): (Font, bool)| (font.spelling().concat(), first);
    assert_eq!(
        spelled(Font::of("serif", 700, true)),
        (String::from("Trinity.Bold.Italic"), true)
    );
    assert_eq!(
        spelled(Font::of("\"Fira Sans\", monospace", 400, false)),
        (String::from("Corpus.Medium"), false),
        "a family RISC OS lacks"
    );
    assert_eq!(
        spelled(Font::of("Anything", 600, true)),
        (String::from("Homerton.Bold.Oblique"), false)
    );
}

fn node(ids: &mut Ids, kind: Kind) -> Arc<Node> {
    Arc::new(Node::new(ids.issue(), kind).expect("a node"))
}

fn shape(ids: &mut Ids, shape: Shape, style: Style) -> Arc<Node> {
    node(
        ids,
        Kind::Shape {
            shape,
            style: Arc::new(style),
        },
    )
}

fn polyline(points: &[(f64, f64)], closed: bool) -> Shape {
    Shape::Path(Arc::new(PathShape {
        contours: vec![Contour {
            nodes: points
                .iter()
                .map(|&(x, y)| PathNode::corner(Point::new(x, y)))
                .collect(),
            closed,
        }],
    }))
}

fn outlined(colour: Rgba, width: f64) -> Stroke {
    Stroke {
        paint: Paint::Colour(colour),
        width,
        miter_limit: 10.0,
        ..Stroke::default()
    }
}

/// An A4 page holding `objects` in one layer.
fn a4(ids: &mut Ids, objects: Vec<Arc<Node>>) -> Drawing {
    let layer = node(
        ids,
        Kind::Group {
            children: Children::from_nodes(objects).expect("held"),
            clip: None,
        },
    );
    Drawing::new(
        Page::of(&PAPER[0]),
        Children::from_nodes(vec![layer]).expect("held"),
        SharedSlice::new(),
        Grid::default(),
    )
    .expect("a drawing")
}

fn round_trip(drawing: &Drawing) -> (Dropped, Read) {
    let written = write(drawing, &OnlyA).expect("written");
    let read = read(&written.bytes, &mut Ids::new()).expect("read back");
    (written.dropped, read)
}

fn label(content: &str, align: TextAlign) -> Text {
    Text {
        content: shared(content),
        family: shared("sans-serif"),
        size: 16.0,
        weight: 400,
        italic: false,
        letter_spacing: 0.0,
        align,
        baseline: Baseline::Lines {
            origin: Point::new(100.0, 400.0),
            line_height: 1.25,
        },
        runs: SharedSlice::new(),
    }
}

/// A filled and outlined rectangle, an arrowed line, a dashed line, a label
/// and a named group, in that order.
fn sampler(ids: &mut Ids, red: Rgba, blue: Rgba) -> Vec<Arc<Node>> {
    let boxed = Style {
        fill: Paint::Colour(red),
        stroke: outlined(blue, 2.0),
        ..Style::default()
    };
    let rect = RectShape {
        x: 10.0,
        y: 20.0,
        width: 30.0,
        height: 40.0,
        rx: 0.0,
        ry: 0.0,
    };
    let arrow = Style {
        fill: Paint::None,
        stroke: Stroke {
            end: Arrowhead {
                shape: ArrowShape::Triangle,
                width: 3.0,
                length: 3.0,
            },
            ..outlined(blue, 2.0)
        },
        ..Style::default()
    };
    let dashed = Style {
        fill: Paint::None,
        stroke: Stroke {
            dashes: slice(&[4.0, 2.0]),
            dash_offset: 1.0,
            ..outlined(red, 1.0)
        },
        ..Style::default()
    };
    let disc = shape(
        ids,
        Shape::Ellipse(EllipseShape {
            centre: Point::new(300.0, 300.0),
            radii: Point::new(20.0, 10.0),
            arc: None,
        }),
        Style::default(),
    );
    let parts = Node::new(
        ids.issue(),
        Kind::Group {
            children: Children::from_nodes(vec![disc]).expect("held"),
            clip: None,
        },
    )
    .and_then(|group| group.with_name(Some(shared("Parts"))))
    .expect("a group");
    vec![
        shape(ids, Shape::Rect(rect), boxed),
        shape(ids, polyline(&[(50.0, 50.0), (150.0, 80.0)], false), arrow),
        shape(
            ids,
            polyline(&[(50.0, 100.0), (150.0, 100.0)], false),
            dashed,
        ),
        node(
            ids,
            Kind::Text {
                text: Arc::new(label("Hello", TextAlign::Start)),
                style: Arc::new(Style::default()),
            },
        ),
        Arc::new(parts),
    ]
}

#[test]
fn a_drawing_written_reads_back_as_itself() {
    let mut ids = Ids::new();
    let red = Rgba::new(255, 0, 0, 255);
    let blue = Rgba::new(0, 0, 255, 255);
    let objects = sampler(&mut ids, red, blue);
    let drawing = a4(&mut ids, objects);
    let (dropped, back) = round_trip(&drawing);
    assert!(dropped.is_empty(), "{dropped:?}");
    assert!(back.unkept.is_empty(), "{:?}", back.unkept);
    assert_eq!(*back.drawing.page(), *drawing.page());
    let objects = objects_of(&back);
    assert_eq!(objects.len(), 5);
    let (rect, style) = shape_of(&objects[0]);
    assert_eq!(style.fill, Paint::Colour(red));
    assert_eq!((style.stroke.width, style.stroke.join), (2.0, Join::Miter));
    let corners = [(10.0, 20.0), (40.0, 20.0), (40.0, 60.0), (10.0, 60.0)];
    assert!(rect.contours[0].closed);
    for (node, (x, y)) in rect.contours[0].nodes.iter().zip(corners) {
        assert!(near(node.point, Point::new(x, y)), "{:?}", node.point);
    }
    let (_, arrow) = shape_of(&objects[1]);
    assert_eq!(arrow.stroke.cap, Cap::Butt);
    assert_eq!(
        arrow.stroke.end,
        Arrowhead {
            shape: ArrowShape::Triangle,
            width: 3.0,
            length: 3.0,
        },
        "written as Draw's own triangle cap"
    );
    let (_, dashed) = shape_of(&objects[2]);
    assert_eq!(
        (&*dashed.stroke.dashes, dashed.stroke.dash_offset),
        (&[4.0, 2.0][..], 1.0)
    );
    let (hello, _) = text_of(&objects[3]);
    assert_eq!(
        (&*hello.content, hello.size, &*hello.family),
        ("Hello", 16.0, "sans-serif")
    );
    assert!(near(origin(hello), Point::new(100.0, 400.0)));
    assert_eq!(objects[4].name(), Some("Parts"));
}

#[test]
fn a_stroke_draw_cannot_draw_is_written_as_the_area_it_paints() {
    let mut ids = Ids::new();
    let blue = Rgba::new(0, 0, 255, 255);
    let stretched = Node::new(
        ids.issue(),
        Kind::Shape {
            shape: polyline(&[(10.0, 10.0), (60.0, 10.0)], false),
            style: Arc::new(Style {
                fill: Paint::None,
                stroke: outlined(blue, 4.0),
                ..Style::default()
            }),
        },
    )
    .and_then(|line| line.with_transform(Affine::scale(1.0, 3.0)))
    .expect("stretched");
    // A corner of twenty degrees mitres at 5.8 widths: past four, within ten.
    let sharp = Style {
        fill: Paint::None,
        stroke: Stroke {
            miter_limit: 4.0,
            ..outlined(blue, 2.0)
        },
        ..Style::default()
    };
    let spike = polyline(&[(100.0, 100.0), (200.0, 100.0), (100.0, 135.0)], false);
    let objects = vec![Arc::new(stretched), shape(&mut ids, spike, sharp)];
    let (_, read) = round_trip(&a4(&mut ids, objects));
    for area in objects_of(&read) {
        let (_, style) = shape_of(&area);
        assert_eq!(style.fill, Paint::Colour(blue), "the stroke's area, filled");
        assert!(!style.stroke.paints());
    }
}

#[test]
fn an_arrowhead_draw_cannot_cap_is_written_as_a_shape() {
    let mut ids = Ids::new();
    let blue = Rgba::new(0, 0, 255, 255);
    let diamond = Style {
        fill: Paint::None,
        stroke: Stroke {
            start: Arrowhead {
                shape: ArrowShape::Diamond,
                width: 3.0,
                length: 3.0,
            },
            ..outlined(blue, 2.0)
        },
        ..Style::default()
    };
    let line = polyline(&[(10.0, 10.0), (60.0, 10.0)], false);
    let line = shape(&mut ids, line, diamond);
    let (_, read) = round_trip(&a4(&mut ids, vec![line]));
    let objects = objects_of(&read);
    let parts: Vec<Arc<Node>> = objects[0]
        .children()
        .expect("the stroke and its head, grouped")
        .iter()
        .cloned()
        .collect();
    let (_, stroke) = shape_of(&parts[0]);
    assert!(stroke.stroke.paints() && !stroke.stroke.start.draws());
    let (head, head_style) = shape_of(&parts[1]);
    assert_eq!(head_style.fill, Paint::Colour(blue));
    assert_eq!(head.contours[0].nodes.len(), 4, "a diamond");
}

#[test]
fn stroke_first_is_written_stroke_then_fill() {
    let mut ids = Ids::new();
    let style = Style {
        fill: Paint::Colour(Rgba::new(255, 0, 0, 255)),
        stroke: outlined(Rgba::new(0, 0, 255, 255), 6.0),
        paint_order: PaintOrder::StrokeFirst,
        ..Style::default()
    };
    let square = polyline(&[(10.0, 10.0), (60.0, 10.0), (60.0, 60.0)], true);
    let square = shape(&mut ids, square, style);
    let (_, read) = round_trip(&a4(&mut ids, vec![square]));
    let parts: Vec<Arc<Node>> = objects_of(&read)[0]
        .children()
        .expect("grouped")
        .iter()
        .cloned()
        .collect();
    let (_, under) = shape_of(&parts[0]);
    let (_, over) = shape_of(&parts[1]);
    assert!(
        under.stroke.paints() && !under.fill.paints(),
        "the stroke beneath"
    );
    assert!(
        over.fill.paints() && !over.stroke.paints(),
        "the fill over it"
    );
}

#[test]
fn text_is_written_in_latin_1_a_line_to_an_object() {
    let mut ids = Ids::new();
    let mut centred = label("Café €\n日本", TextAlign::Middle);
    centred.family = shared("\"Fira Sans\", serif");
    centred.weight = 700;
    let turned = Node::new(
        ids.issue(),
        Kind::Text {
            text: Arc::new(label("Turned", TextAlign::Start)),
            style: Arc::new(Style::default()),
        },
    )
    .and_then(|text| text.with_transform(Affine::rotate_degrees(30.0)))
    .expect("turned");
    let objects = vec![
        node(
            &mut ids,
            Kind::Text {
                text: Arc::new(centred),
                style: Arc::new(Style::default()),
            },
        ),
        Arc::new(turned),
    ];
    let drawing = a4(&mut ids, objects);
    let written = write(&drawing, &OnlyA).expect("written");
    assert!(written.dropped.characters && written.dropped.fonts);
    let bytes = &written.bytes;
    let holds = |needle: &[u8]| bytes.windows(needle.len()).any(|window| window == needle);
    assert!(holds(b"Trinity.Bold\0"), "the serif the list names second");
    assert!(holds(b"Caf\xe9 \x80\0"), "RISC OS's Latin-1");
    assert!(holds(b"??\0"), "characters it has no code for");
    let read = read(bytes, &mut Ids::new()).expect("read back");
    let objects = objects_of(&read);
    let lines = objects[0].children().expect("its two lines, grouped");
    assert_eq!(lines.len(), 2);
    let along = objects[1].transform().map_vector(Point::new(1.0, 0.0));
    let expected = Affine::rotate_degrees(30.0).map_vector(Point::new(1.0, 0.0));
    assert!(
        along.distance(expected) < 1e-4,
        "{along:?} against {expected:?}"
    );
}

#[test]
fn a_copy_says_what_draw_could_not_keep() {
    let mut ids = Ids::new();
    let written = write(&sample(&mut ids), &OnlyA).expect("written");
    let dropped = written.dropped;
    assert!(dropped.gradients && dropped.transparency && dropped.clips && dropped.page_size);
    assert!(!dropped.characters && !dropped.fonts && !dropped.letter_spacing);
    let listed: Vec<&str> = dropped.iter().collect();
    assert_eq!(
        listed,
        ["gradients", "transparency", "clipping", "page size"]
    );
    let read = read(&written.bytes, &mut Ids::new()).expect("its own copy reads");
    assert!(read.unkept.is_empty());
}

/// Every picture a read drawing's one layer holds, at any depth.
fn pictures_of(read: &Read) -> Vec<(Affine, crate::image::Image)> {
    fn walk(node: &Node, outer: Affine, found: &mut Vec<(Affine, crate::image::Image)>) {
        let placed = node.transform().then(outer);
        match node.kind() {
            Kind::Image { image } => found.push((placed, (**image).clone())),
            Kind::Group { children, .. } => {
                for child in children.iter() {
                    walk(child, placed, found);
                }
            }
            _ => {}
        }
    }
    let mut found = Vec::new();
    for node in objects_of(read) {
        walk(&node, Affine::IDENTITY, &mut found);
    }
    found
}

fn colours(x: u32, y: u32) -> [u8; 4] {
    [
        u8::try_from(x * 30 % 256).expect("a level"),
        u8::try_from(y * 50 % 256).expect("a level"),
        90,
        255,
    ]
}

/// Where `placed` lays the corners of `image`'s rectangle, in drawing pixels.
fn corners(placed: Affine, image: &crate::image::Image) -> [Point; 4] {
    image.place().corners().map(|corner| placed.map(corner))
}

fn near_corners(a: [Point; 4], b: [Point; 4]) -> bool {
    // A draw unit is a 480th of a pixel; a matrix entry holds sixteen bits.
    a.iter()
        .zip(&b)
        .all(|(p, q)| (p.x - q.x).abs() < 0.02 && (p.y - q.y).abs() < 0.02)
}

/// A sprite object of `sprite` filling the draw-unit rectangle `bounds`.
fn sprite_object(sprite: &[u8], bounds: [i32; 4]) -> Vec<u8> {
    let mut bytes = object(5, sprite);
    for (at, corner) in bounds.iter().enumerate() {
        bytes[8 + 4 * at..12 + 4 * at].copy_from_slice(&corner.to_le_bytes());
    }
    bytes
}

fn sprite_of(pixels: &tairix_raster::Surface, eig: (u8, u8)) -> Vec<u8> {
    tairix_image::encode_sprite(&tairix_image::SpriteInput::Picture {
        name: tairix_image::SpriteName::new("photo").expect("a name"),
        mode: tairix_image::SpriteMode::truecolour(eig, false),
        palette: &tairix_image::SpritePalette::Implied,
        masked: false,
        source: &tairix_image::SurfacePicture::new(pixels),
    })
    .expect("a sprite")
}

#[test]
fn a_sprite_fills_its_objects_bounds_and_is_kept_as_its_bytes() {
    let pixels = crate::testing::pixels(4, 3, colours);
    let sprite = sprite_of(&pixels, (1, 1));
    let read = read_file(&[sprite_object(&sprite, [480, 960, 480 * 41, 960 + 480 * 30])]);
    assert!(read.unkept.is_empty(), "{:?}", read.unkept);
    let [(placed, image)] = pictures_of(&read).try_into().expect("one picture");
    assert_eq!(placed, Affine::IDENTITY);
    let height = read.drawing.page().size().y;
    let expected = tairix_bezier::Bounds::from_points([
        Point::new(1.0, height - 2.0),
        Point::new(41.0, height - 32.0),
    ]);
    assert_eq!(image.place(), expected);
    assert_eq!(image.pixels().pixels(), pixels.pixels());
    let source = image.source().expect("kept");
    assert_eq!(
        source.encoding,
        crate::image::Encoding::Sprite { eig: (1, 1) }
    );
    assert_eq!(&*source.bytes, &sprite[..]);
}

#[test]
fn a_transformed_sprite_spans_its_pixels_size_and_its_matrix_lays_it() {
    let pixels = crate::testing::pixels(2, 2, colours);
    // Four OS units a pixel across, two down: 1024 and 512 draw units.
    let sprite = sprite_of(&pixels, (2, 1));
    let mut body = words(&[int(2 << 16), 0, 0, int(3 << 16), int(4800), int(9600)]);
    body.extend_from_slice(&sprite);
    let read = read_file(&[object(13, &body)]);
    let [(placed, image)] = pictures_of(&read).try_into().expect("one picture");
    let height = read.drawing.page().size().y;
    let draw = |x: f64, y: f64| Point::new(x / 480.0, height - y / 480.0);
    // The raster's foot on the origin, its pixels twice and thrice their size.
    let want = [
        draw(4800.0, 9600.0 + 3.0 * 1024.0),
        draw(4800.0 + 2.0 * 2048.0, 9600.0 + 3.0 * 1024.0),
        draw(4800.0 + 2.0 * 2048.0, 9600.0),
        draw(4800.0, 9600.0),
    ];
    assert!(
        near_corners(corners(placed, &image), want),
        "{:?} vs {want:?}",
        corners(placed, &image)
    );
}

#[test]
fn a_transformed_sprite_whose_length_is_no_whole_number_of_words_still_reads() {
    let pixels = crate::testing::pixels(2, 2, colours);
    let mut sprite = sprite_of(&pixels, (1, 1));
    let length = u32::try_from(sprite.len() + 1).expect("small");
    sprite[..4].copy_from_slice(&length.to_le_bytes());
    sprite.push(0);
    let mut body = words(&[int(1 << 16), 0, 0, int(1 << 16), 0, 0]);
    body.extend_from_slice(&sprite);
    while !body.len().is_multiple_of(4) {
        body.push(0);
    }
    let read = read_file(&[object(13, &body)]);
    assert!(read.unkept.is_empty(), "{:?}", read.unkept);
    let [(_, image)] = pictures_of(&read).try_into().expect("one picture");
    assert_eq!(image.pixels().pixels(), pixels.pixels());
    assert_eq!(
        image.source().map(|source| &*source.bytes),
        Some(&sprite[..]),
        "kept as its own bytes"
    );
}

#[test]
fn a_jpeg_spans_the_size_its_object_states_and_is_drawn_as_stored() {
    // Stored 4 wide and 2 high, its metadata asking for a quarter turn.
    let stored = crate::testing::pixels(4, 2, |x, _| {
        if x < 2 {
            [255, 0, 0, 255]
        } else {
            [0, 0, 255, 255]
        }
    });
    let jpeg = with_turn(&crate::testing::jpeg(&stored), 6);
    let mut body = words(&[
        4 * 512,
        2 * 512,
        90,
        90,
        int(1 << 16),
        0,
        0,
        int(1 << 16),
        0,
        0,
    ]);
    body.extend(words(&[u32::try_from(jpeg.len()).expect("small")]));
    body.extend_from_slice(&jpeg);
    while !body.len().is_multiple_of(4) {
        body.push(0);
    }
    let read = read_file(&[object(16, &body)]);
    assert!(read.unkept.is_empty(), "{:?}", read.unkept);
    let [(placed, image)] = pictures_of(&read).try_into().expect("one picture");
    assert_eq!(
        (image.pixels().width(), image.pixels().height()),
        (2, 4),
        "the picture as its metadata turns it"
    );
    // Drawn as stored: 4 by 2 pixels of 512 draw units, foot on the origin,
    // its first stored pixel at the top left.
    let height = read.drawing.page().size().y;
    let stored_top_left = Point::new(0.0, height - 1024.0 / 480.0);
    let first = placed.map(image.to_place().map(Point::new(1.5, 0.5)));
    let stored_first = Point::new(0.5 * 512.0 / 480.0, stored_top_left.y + 0.5 * 512.0 / 480.0);
    assert!(
        (first.x - stored_first.x).abs() < 1e-6 && (first.y - stored_first.y).abs() < 1e-6,
        "{first:?} vs {stored_first:?}"
    );
    assert_eq!(image.source().map(|source| &*source.bytes), Some(&jpeg[..]));
}

#[test]
fn a_jpeg_object_cut_short_breaks_the_file() {
    assert_eq!(
        read(&file(&[object(16, &[0; 8])]), &mut Ids::new()).err(),
        Some(ReadError::Malformed)
    );
}

/// `jpeg` with an EXIF block stating `orientation` after its SOI.
fn with_turn(jpeg: &[u8], orientation: u16) -> Vec<u8> {
    let mut block = b"Exif\0\0MM".to_vec();
    block.extend_from_slice(&42u16.to_be_bytes());
    block.extend_from_slice(&8u32.to_be_bytes());
    block.extend_from_slice(&1u16.to_be_bytes());
    block.extend_from_slice(&274u16.to_be_bytes());
    block.extend_from_slice(&3u16.to_be_bytes());
    block.extend_from_slice(&1u32.to_be_bytes());
    block.extend_from_slice(&orientation.to_be_bytes());
    block.extend_from_slice(&[0; 6]);
    let mut out = jpeg[..2].to_vec();
    out.extend_from_slice(&[0xFF, 0xE1]);
    out.extend_from_slice(&u16::try_from(block.len() + 2).expect("small").to_be_bytes());
    out.extend_from_slice(&block);
    out.extend_from_slice(&jpeg[2..]);
    out
}

#[test]
fn pictures_are_written_as_draw_draws_them_and_read_back_where_they_stood() {
    use crate::testing::{image, jpeg, pixels, source_of};
    let mut ids = Ids::new();
    let rect = |x0, y0, x1, y1| {
        tairix_bezier::Bounds::from_points([Point::new(x0, y0), Point::new(x1, y1)])
    };
    let upright =
        crate::image::Image::new(pixels(4, 3, colours), rect(10.0, 20.0, 50.0, 50.0), None)
            .expect("a picture");
    let ring = pixels(3, 3, |x, y| {
        if x == 1 && y == 1 {
            [0, 0, 0, 0]
        } else {
            [10, 200, 30, 128]
        }
    });
    let turned =
        crate::image::Image::new(ring, rect(0.0, 0.0, 30.0, 30.0), None).expect("a picture");
    let photo = pixels(4, 2, colours);
    let photo_bytes = jpeg(&photo);
    let decoded = tairix_image::decode(
        &photo_bytes,
        &tairix_image::DecodeLimits::new(64, 64, 4096, 4096),
    )
    .expect("decoded");
    let photo = crate::image::Image::new(
        tairix_raster::Surface::from_rgba8(4, 2, decoded.pixels()).expect("pixels"),
        rect(0.0, 0.0, 40.0, 20.0),
        Some(source_of(&photo_bytes)),
    )
    .expect("a photograph");
    let objects = vec![
        image(&mut ids, upright, Affine::IDENTITY),
        image(
            &mut ids,
            turned,
            Affine::rotate_degrees(30.0).then(Affine::translate(200.0, 100.0)),
        ),
        image(
            &mut ids,
            photo,
            Affine::scale(-1.0, 1.0).then(Affine::translate(300.0, 300.0)),
        ),
    ];
    let drawing = a4(&mut ids, objects);
    let written = write(&drawing, &OnlyA).expect("written");
    let mut kinds = Vec::new();
    let mut at = 40;
    while at + 8 <= written.bytes.len() {
        let word =
            |at: usize| u32::from_le_bytes(written.bytes[at..at + 4].try_into().expect("a word"));
        if !matches!(word(at), 0 | 11) {
            kinds.push(word(at));
        }
        at += usize::try_from(word(at + 4)).expect("a size");
    }
    assert_eq!(kinds, [5, 13, 16], "upright, turned, and a JPEG as it came");
    let (dropped, back) = round_trip(&drawing);
    assert!(dropped.is_empty(), "{dropped:?}");
    assert!(back.unkept.is_empty(), "{:?}", back.unkept);
    let before = {
        let wrapped = Read {
            drawing: drawing.clone(),
            unkept: crate::unkept::Unkept::NONE,
        };
        pictures_of(&wrapped)
    };
    let after = pictures_of(&back);
    assert_eq!(after.len(), 3);
    for ((placed_a, a), (placed_b, b)) in before.iter().zip(&after) {
        assert!(
            near_corners(corners(*placed_a, a), corners(*placed_b, b)),
            "{:?} vs {:?}",
            corners(*placed_a, a),
            corners(*placed_b, b)
        );
        assert_eq!(a.pixels().pixels(), b.pixels().pixels(), "every pixel kept");
    }
    assert_eq!(
        after[2].1.source().map(|source| &*source.bytes),
        Some(&photo_bytes[..])
    );
}

#[test]
fn a_sprite_at_the_coarsest_pixels_a_mode_states_reads_and_writes_back() {
    let pixels = crate::testing::pixels(2, 1, colours);
    // A RISC OS 3.5 mode word of eleven dots to the inch: four OS units of
    // eigen factor past the finest.
    let picture = tairix_image::SurfacePicture::new(&pixels);
    let mut sprite = tairix_image::encode_sprite(&tairix_image::SpriteInput::Picture {
        name: tairix_image::SpriteName::new("coarse").expect("a name"),
        mode: tairix_image::SpriteMode::truecolour((3, 3), false),
        palette: &tairix_image::SpritePalette::Implied,
        masked: false,
        source: &picture,
    })
    .expect("a sprite");
    let word = u32::from_le_bytes(sprite[40..44].try_into().expect("the mode word"));
    let coarse = (word & !(0x1FFF << 14 | 0x1FFF << 1)) | 11 << 14 | 11 << 1;
    sprite[40..44].copy_from_slice(&coarse.to_le_bytes());
    let read = read_file(&[sprite_object(&sprite, [0, 0, 480 * 32, 480 * 16])]);
    assert!(read.unkept.is_empty(), "{:?}", read.unkept);
    let [(_, image)] = pictures_of(&read).try_into().expect("one picture");
    assert_eq!(
        image.source().map(|source| source.encoding),
        Some(crate::image::Encoding::Sprite {
            eig: (tairix_image::MAX_SPRITE_EIG, tairix_image::MAX_SPRITE_EIG)
        })
    );
    let (_, back) = round_trip(&read.drawing);
    assert_eq!(pictures_of(&back).len(), 1);
}

#[test]
fn each_run_of_a_line_is_an_object_and_each_glyph_along_a_path_one_turned_with_it() {
    use crate::text::{Run, RunPaint, RunStyle, TextPath};

    let mut ids = Ids::new();
    let red = Rgba::new(255, 0, 0, 255);
    let mut styled = label("ab\ncd", TextAlign::Start);
    styled.runs = slice(&[
        Run {
            start: 1,
            end: 2,
            style: Arc::new(RunStyle {
                size: Some(32.0),
                fill: Some(RunPaint::Colour(red)),
                ..RunStyle::default()
            }),
        },
        Run {
            start: 3,
            end: 4,
            style: Arc::new(RunStyle {
                weight: Some(700),
                ..RunStyle::default()
            }),
        },
    ]);
    let Shape::Path(down) = polyline(&[(400.0, 100.0), (400.0, 300.0)], false) else {
        panic!("a path");
    };
    let mut along = label("aa", TextAlign::Start);
    along.baseline = Baseline::Path(Arc::new(TextPath {
        path: down,
        start: 0.0,
    }));
    let objects = vec![
        node(
            &mut ids,
            Kind::Text {
                text: Arc::new(styled),
                style: Arc::new(Style::default()),
            },
        ),
        node(
            &mut ids,
            Kind::Text {
                text: Arc::new(along),
                style: Arc::new(Style::default()),
            },
        ),
    ];
    let (dropped, back) = round_trip(&a4(&mut ids, objects));
    assert!(!dropped.letter_spacing);
    let objects = objects_of(&back);
    let parts = objects[0].children().expect("its runs, grouped");
    let contents: Vec<String> = parts
        .iter()
        .map(|part| String::from(&*text_of(part).0.content))
        .collect();
    assert_eq!(contents, ["a", "b", "c", "d"]);
    let (b, b_style) = text_of(parts.get(1).expect("b"));
    assert!((b.size - 32.0).abs() < 1e-9);
    assert_eq!(b_style.fill, Paint::Colour(red));
    assert!(
        near(origin(b), Point::new(116.0, 400.0)),
        "after an em of a"
    );
    let (c, _) = text_of(parts.get(2).expect("c"));
    assert_eq!(c.weight, 700);
    assert!(near(origin(c), Point::new(100.0, 420.0)), "the second line");
    let glyphs = objects[1].children().expect("its glyphs, grouped");
    assert_eq!(glyphs.len(), 2);
    for (index, glyph) in glyphs.iter().enumerate() {
        let (text, _) = text_of(glyph);
        assert_eq!(&*text.content, "a");
        let placed = glyph.transform();
        let at = placed.map(origin(text));
        let expected = Point::new(
            400.0,
            100.0 + 16.0 * f64::from(u8::try_from(index).expect("two")),
        );
        assert!(at.distance(expected) < 0.01, "{at:?} against {expected:?}");
        let x_axis = placed.map_vector(Point::new(1.0, 0.0));
        assert!(
            x_axis.distance(Point::new(0.0, 1.0)) < 1e-4,
            "turned down the path: {x_axis:?}"
        );
    }
}
