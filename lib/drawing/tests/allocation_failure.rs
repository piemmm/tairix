//! The drawing under real allocation failure.
//!
//! The shared metered allocator refuses, on the calling thread only, every
//! request past a chosen count, so the work an editor does with a drawing —
//! rendering it, writing it in each format, and reading a Draw file — can be
//! made to fail at each request it makes in turn: each refusal is answered as
//! a refusal — never an abort, which is what a single allocation that cannot
//! refuse would make of it — and nothing the work held outlives it.

use std::sync::Arc;

use tairix_bezier::{Affine, Path, PathEl, Point, Subpath};
use tairix_colour::Rgba;
use tairix_drawing::render::{artwork, Afresh, Mode, View};
use tairix_drawing::{eps, pdf, riscos, svg, Drawing, DrawingError, Glyph, Glyphs, Ids, ReadError};
use tairix_fuzzseed::meter::refused_at_each;

#[global_allocator]
static ALLOCATOR: tairix_fuzzseed::meter::Metered = tairix_fuzzseed::meter::Metered;

/// A drawing of most of what is shared: layers, a named group, a clip, a
/// gradient, dashes and arrowheads, text with runs and along a path, guides,
/// swatches and a title.
const DOCUMENT: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" xmlns:draw="urn:tairix:draw"
    width="210mm" height="297mm" viewBox="0 0 793.7007874015749 1122.5196850393702"
    draw:grid="20 5" draw:background="#fffff0">
  <title>Shared</title>
  <draw:guides><draw:guide axis="horizontal" position="100"/></draw:guides>
  <draw:swatches><draw:swatch colour="#c81e28"/></draw:swatches>
  <defs>
    <linearGradient id="g" x1="0" y1="0" x2="100" y2="0" gradientUnits="userSpaceOnUse">
      <stop offset="0" stop-color="#f00"/><stop offset="1" stop-color="#00f"/>
    </linearGradient>
    <clipPath id="k"><circle cx="50" cy="50" r="40"/></clipPath>
    <path id="way" d="M20 600 C80 560 160 640 240 600"/>
  </defs>
  <g draw:layer="true" draw:name="Back">
    <rect x="10" y="10" width="100" height="50" fill="url(#g)" stroke="#000" stroke-width="3"/>
    <g clip-path="url(#k)" draw:name="Window"><rect width="100" height="100" fill="#0a0"/></g>
  </g>
  <g draw:layer="true" draw:name="Front">
    <text x="20" y="200" font-family="serif" font-size="24">Line <tspan font-weight="bold" fill="#c00" baseline-shift="0.2em">one</tspan><tspan x="20" y="230">Line two</tspan></text>
    <text font-size="18"><textPath href="#way" startOffset="10">Along <tspan font-size="24" stroke="#00f">the</tspan> way</textPath></text>
    <path d="M10 500 C20 480 40 480 50 500 L120 520" fill="none" stroke="#123456" stroke-width="2"
        stroke-dasharray="4 2" draw:end-arrow="diamond 3 4"/>
  </g>
</svg>"##;

/// No glyph is known.
struct NoGlyphs;

impl Glyphs for NoGlyphs {
    fn glyph(&self, _family: &str, _weight: u16, _italic: bool, _c: char) -> Option<Arc<Glyph>> {
        None
    }
}

/// Every character is one glyph, a box with a hole, made before any
/// measurement so handing it out allocates nothing.
struct BoxGlyphs(Arc<Glyph>);

impl BoxGlyphs {
    fn new() -> Self {
        let ring = |corners: [(f64, f64); 4]| {
            let mut ring = Subpath::new(Point::new(corners[0].0, corners[0].1));
            for (x, y) in &corners[1..] {
                ring.elements.push(PathEl::Line(Point::new(*x, *y)));
            }
            ring.closed = true;
            ring
        };
        let outline = Path {
            subpaths: vec![
                ring([(0.05, -0.7), (0.55, -0.7), (0.55, 0.0), (0.05, 0.0)]),
                ring([(0.15, -0.6), (0.15, -0.1), (0.45, -0.1), (0.45, -0.6)]),
            ],
        };
        Self(Arc::new(Glyph::new(outline, 0.6)))
    }
}

impl Glyphs for BoxGlyphs {
    fn glyph(&self, _family: &str, _weight: u16, _italic: bool, _c: char) -> Option<Arc<Glyph>> {
        Some(Arc::clone(&self.0))
    }
}

/// `bytes` as an `<image>` laid at `place`, a base64 data URL.
fn image_element(bytes: &[u8], place: &str) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut url = String::from("data:image/png;base64,");
    for group in bytes.chunks(3) {
        let word = (u32::from(group[0]) << 16)
            | (u32::from(*group.get(1).unwrap_or(&0)) << 8)
            | u32::from(*group.get(2).unwrap_or(&0));
        for at in 0..4 {
            url.push(if at <= group.len() {
                char::from(ALPHABET[((word >> (18 - 6 * at)) & 63) as usize])
            } else {
                '='
            });
        }
    }
    format!(r#"<image xlink:href="{url}" {place}/>"#)
}

/// The document, with a translucent PNG, a JPEG and a turned picture laid in
/// its second layer.
fn drawing() -> Drawing {
    let mut rgba = Vec::new();
    for y in 0..6_u8 {
        for x in 0..8_u8 {
            rgba.extend_from_slice(&[x * 30, y * 40, 90, if (x + y) % 3 == 0 { 0 } else { 200 }]);
        }
    }
    let picture = tairix_image::Picture::rgba(8, 6, rgba).expect("a picture");
    let png = tairix_image::encode_png(&picture).expect("a png");
    let options = tairix_image::JpegOptions::new(90, [255, 255, 255]).expect("options");
    let jpeg = tairix_image::encode_jpeg(&picture, options).expect("a jpeg");
    let pictures = [
        image_element(&png, r#"x="300" y="300" width="80" height="60""#),
        image_element(&jpeg, r#"x="400" y="300" width="80" height="60""#),
        image_element(
            &png,
            r#"x="0" y="0" width="40" height="30" transform="rotate(30) translate(500 400)""#,
        ),
    ]
    .concat();
    let document = DOCUMENT
        .replacen(
            r#"xmlns:draw="urn:tairix:draw""#,
            r#"xmlns:draw="urn:tairix:draw" xmlns:xlink="http://www.w3.org/1999/xlink""#,
            1,
        )
        .replacen("</g>\n</svg>", &format!("{pictures}</g>\n</svg>"), 1);
    let read = svg::read(document.as_bytes(), &mut Ids::new()).expect("reads");
    assert!(read.unkept.is_empty(), "{:?}", read.unkept);
    read.drawing
}

#[test]
fn a_drawing_written_as_the_heap_runs_out_is_refused_never_aborted() {
    let drawing = drawing();
    let out_of_memory = |error: &DrawingError| *error == DrawingError::OutOfMemory;
    refused_at_each(|| svg::write(&drawing), out_of_memory);
    let boxes = BoxGlyphs::new();
    for glyphs in [&NoGlyphs as &dyn Glyphs, &boxes] {
        refused_at_each(|| riscos::write(&drawing, glyphs), out_of_memory);
        refused_at_each(|| pdf::pdf(&drawing, glyphs), out_of_memory);
        refused_at_each(|| eps::eps(&drawing, glyphs), out_of_memory);
    }
}

#[test]
fn a_drawing_rendered_as_the_heap_runs_out_is_refused_never_aborted() {
    let drawing = drawing();
    let boxes = BoxGlyphs::new();
    for mode in [Mode::Painted, Mode::Outline(Rgba::new(0, 0, 0, 255))] {
        let view = View {
            to_device: Affine::scale(0.5, 0.5),
            origin: Point::ZERO,
            side: 256.0,
            design: 256 * 256,
            mode,
            ground: true,
        };
        for glyphs in [&NoGlyphs as &dyn Glyphs, &boxes] {
            refused_at_each(
                || artwork(&drawing, &view, glyphs, &Afresh),
                |error| *error == DrawingError::OutOfMemory,
            );
        }
    }
}

#[test]
fn a_draw_file_read_as_the_heap_runs_out_is_refused_never_aborted() {
    let file = riscos::write(&drawing(), &NoGlyphs).expect("writes").bytes;
    refused_at_each(
        || riscos::read(&file, &mut Ids::new()),
        |error| *error == ReadError::OutOfMemory,
    );
}
