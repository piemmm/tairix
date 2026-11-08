//! Deterministic fuzz harness for the drawing's SVG reader, which reads
//! documents a user opened or pasted, and for the renderer that draws what it
//! read: reading never panics, whatever it accepts writes, reads back and
//! writes again to the same bytes — the writer says nothing the reader cannot
//! take back — renders at any zoom and square, and writes as PDF and as
//! seven-bit EPS within its line bound, without panicking. A plain
//! `cargo test` runs a fixed sweep from a fresh, logged seed; `cargo xtask
//! fuzz` extends it to a wall-clock budget.

use std::sync::Arc;

use core::fmt::Write as _;

use tairix_bezier::{Affine, Path, PathEl, Point, Subpath};
use tairix_colour::Rgba;
use tairix_drawing::render::{artwork, Afresh, Mode, View};
use tairix_drawing::svg::{read, write};
use tairix_drawing::{Drawing, Glyph, Glyphs, Ids};
use tairix_fuzzseed::Prng;
use tairix_raster::Surface;

/// Fixed-iteration sweep run once by a plain `cargo test` (no budget set).
const SMOKE_ITERATIONS: u64 = 2_000;

/// Largest arbitrary byte string fed straight to the reader.
const MAX_NOISE: usize = 2048;

/// Real documents the harness mutates, each reaching a different part of the
/// reader: Draw's own namespace, Inkscape's layers, the cascade, `use` and
/// `symbol`, gradients, clips, text with runs and along paths, pictures and
/// what the drawing cannot hold.
const TEMPLATES: &[&str] = &[
    r##"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="64" height="64"><clipPath id="k"><image xlink:href="#c" width="9" height="9"/><rect width="40" height="30" clip-rule="evenodd"/></clipPath><image id="c" xlink:href="data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAMAAAACCAYAAACddGYaAAAAHUlEQVR4nGNgYDjxP4DhRMMCIM3AUAHkVAA5QBoAlAwM9VmVMmEAAAAASUVORK5CYII=" width="30" height="20" preserveAspectRatio="xMidYMid slice" opacity="0.5"/><g clip-path="url(#k)"><use xlink:href="#c" x="5" y="5" transform="rotate(20)"/></g></svg>"##,
    r##"<svg xmlns="http://www.w3.org/2000/svg" xmlns:draw="urn:tairix:draw" width="210mm" height="297mm" viewBox="0 0 793.7 1122.5" draw:unit="mm" draw:grid="37.8 4" draw:background="#fffff0" draw:guides="x 10 y 20.5"><g draw:layer="true" draw:name="Ink"><path d="M10 10 C20 0 30 20 40 10 Z" draw:nodes="csz" fill="#123456" stroke="#000" stroke-width="2" stroke-dasharray="4 2" stroke-linejoin="round"/><path d="M0 0 L10 0" draw:shape="arc" draw:cx="5" draw:cy="5" draw:rx="5" draw:ry="3" draw:start="0" draw:end="90" draw:arc="pie"/><path d="M0 0 L1 1" draw:shape="star" draw:cx="50" draw:cy="50" draw:points="5" draw:outer="40" draw:inner="0.5" draw:rotation="0" draw:rounding="0.1"/></g></svg>"##,
    r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:inkscape="http://www.inkscape.org/namespaces/inkscape" width="100" height="80"><g inkscape:groupmode="layer" inkscape:label="One" style="opacity:0.5"><rect x="1" y="2" width="30" height="20" rx="4" fill="red"/></g><g inkscape:groupmode="layer" style="display:none"><circle cx="40" cy="40" r="10"/></g></svg>"#,
    r##"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="64" height="64"><style>.a{fill:orange}#b{stroke:green;stroke-width:3}</style><defs><symbol id="s" viewBox="0 0 10 10"><rect class="a" width="10" height="10"/></symbol><circle id="c" r="4"/></defs><use xlink:href="#s" x="2" y="2" width="20" height="20"/><use href="#c" x="30" y="30"/><switch><rect requiredFeatures="x" width="1" height="1"/><ellipse id="b" cx="9" cy="9" rx="3" ry="2"/></switch></svg>"##,
    r##"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100"><defs><linearGradient id="g" x1="0" y1="0" x2="1" y2="1" spreadMethod="reflect"><stop offset="0" stop-color="#f00"/><stop offset="0.5" stop-color="#0f0" stop-opacity="0.5"/><stop offset="1" stop-color="#00f"/></linearGradient><radialGradient id="r" href="#g" cx="50" cy="50" r="40" fx="40" fy="45" gradientUnits="userSpaceOnUse" gradientTransform="rotate(30)"/></defs><rect width="50" height="50" fill="url(#g)"/><circle cx="50" cy="50" r="30" fill="url(#r)" stroke="url(#g)"/></svg>"##,
    r#"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100"><clipPath id="k" clipPathUnits="objectBoundingBox"><circle cx="0.5" cy="0.5" r="0.5"/></clipPath><g clip-path="url(#k)" transform="translate(10 10) scale(2)"><rect width="20" height="20"/><text x="1" y="12" font-family="serif" font-size="10" font-weight="bold" font-style="italic" letter-spacing="1" text-anchor="middle">Hi<tspan x="1" dy="12">there</tspan></text></g></svg>"#,
    r##"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="100" height="100"><defs><path id="w" d="M5 50 C30 10 70 90 95 50" pathLength="10"/><linearGradient id="g"><stop offset="0" stop-color="#f00"/></linearGradient></defs><text font-size="8"><textPath xlink:href="#w" startOffset="25%" side="right">Al<tspan font-size="12" fill="#0a0" baseline-shift="super">on</tspan>g<tspan fill="url(#g)" stroke-width="3">!</tspan></textPath></text><text x="5" y="20" font-size="10">a<tspan font-family="serif" font-weight="700" font-style="italic" letter-spacing="2" fill-opacity="0.5" stroke="#00f" baseline-shift="-30%">b<tspan baseline-shift="0.5em" font-size="20">c</tspan></tspan><tspan x="5" y="40">d</tspan><textPath href="#w">e</textPath></text></svg>"##,
    r##"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"><filter id="f"/><mask id="m"/><pattern id="p"/><marker id="n"/><image href="x.png" width="1" height="1"/><a href="#"><rect width="1" height="1" filter="url(#f)" mask="url(#m)" fill="url(#p)" marker-end="url(#n)" vector-effect="non-scaling-stroke"/></a><script>x</script><animate/><foreignObject/></svg>"##,
];

/// Values spliced into generated documents, ordinary and hostile.
const NUMBERS: &[&str] = &[
    "0",
    "1",
    "-1",
    "0.5",
    "1e3",
    "-2.5e-3",
    "1e300",
    "-1e300",
    "NaN",
    "inf",
    "1e-320",
    "99999999999",
    "3.",
    ".5",
    "",
    "x",
];

/// Element names a generated document nests.
const ELEMENTS: &[&str] = &[
    "g", "rect", "ellipse", "circle", "line", "polyline", "polygon", "path", "text", "use", "svg",
    "a", "switch", "clipPath", "symbol", "defs", "title", "desc", "metadata",
];

/// Attributes a generated element carries, each with a value drawn from
/// [`NUMBERS`] or spelled out here.
const ATTRIBUTES: &[&str] = &[
    "x",
    "y",
    "width",
    "height",
    "rx",
    "ry",
    "r",
    "cx",
    "cy",
    "x1",
    "y1",
    "x2",
    "y2",
    "stroke-width",
    "opacity",
    "fill-opacity",
    "stroke-miterlimit",
    "font-size",
    "stroke-dashoffset",
    "letter-spacing",
];

/// Whole attributes with structured values.
const STRUCTURED: &[&str] = &[
    r#"transform="matrix(1 2 3 4 5 6)""#,
    r#"transform="rotate(45 10 10) skewX(80)""#,
    r#"transform="scale(0)""#,
    r#"transform="scale(1e9)""#,
    r#"d="M0 0 C1 2 3 4 5 6 S7 8 9 10 Q1 1 2 2 T3 3 A5 5 0 1 1 9 9 Z""#,
    r#"d="M0 0 L1""#,
    r#"points="0,0 10,0 10,10""#,
    r##"fill="#abc""##,
    r#"fill="none""#,
    r#"stroke="currentColor""#,
    r#"stroke-dasharray="1 2 3""#,
    r#"stroke-dasharray="-1""#,
    r#"fill-rule="evenodd""#,
    r#"style="fill:rgb(10%,20%,30%);stroke:hsl(10 50% 50%)""#,
    r#"viewBox="0 0 10 10""#,
    r#"viewBox="0 0 0 0""#,
    r#"id="a""#,
    r##"href="#a""##,
    r#"clip-path="url(#a)""#,
    r#"xmlns:draw="urn:tairix:draw" draw:layer="true""#,
    r#"xmlns:draw="urn:tairix:draw" draw:locked="true" draw:name="n""#,
    r#"xmlns:draw="urn:tairix:draw" draw:nodes="cszq""#,
    r#"xmlns:draw="urn:tairix:draw" draw:start-arrow="triangle 3 4""#,
    r#"xml:space="preserve""#,
    r#"xmlns:draw="urn:tairix:draw" draw:role="background""#,
    r#"display="none""#,
    r#"visibility="hidden""#,
];

/// Glyphs for the lower-case letters, a diamond each; the rest unknown.
struct Letters;

impl Glyphs for Letters {
    fn glyph(&self, _family: &str, _weight: u16, _italic: bool, c: char) -> Option<Arc<Glyph>> {
        c.is_ascii_lowercase().then(|| {
            let mut ring = Subpath::new(Point::new(0.3, -0.7));
            ring.elements.extend([
                PathEl::Line(Point::new(0.6, -0.35)),
                PathEl::Cubic(
                    Point::new(0.5, 0.0),
                    Point::new(0.4, 0.0),
                    Point::new(0.3, 0.0),
                ),
                PathEl::Line(Point::new(0.0, -0.35)),
            ]);
            ring.closed = true;
            Arc::new(Glyph::new(
                Path {
                    subpaths: vec![ring],
                },
                0.6,
            ))
        })
    }
}

/// `drawing` drawn in a random square at a random zoom.
fn renders(drawing: &Drawing, rng: &mut Prng) {
    const SIDE: u32 = 32;
    let zoom = [1e-3, 0.25, 1.0, 3.0, 256.0, 1e5][rng.below(6)];
    let page = drawing.page().size();
    let at = Point::new(
        page.x * zoom * f64::from(rng.next_u16()) / 65535.0,
        page.y * zoom * f64::from(rng.next_u16()) / 65535.0,
    );
    let mode = if rng.below(4) == 0 {
        Mode::Outline(Rgba::new(0, 0, 0, 255))
    } else {
        Mode::Painted
    };
    let view = View {
        to_device: Affine::scale(zoom, zoom).then(Affine::rotate_degrees(f64::from(rng.next_u8()))),
        origin: at,
        side: f64::from(SIDE),
        design: SIDE * 256,
        mode,
        ground: rng.below(2) == 0,
    };
    if let Ok(art) = artwork(drawing, &view, &Letters, &Afresh) {
        let mut surface = Surface::new(SIDE, SIDE).expect("a surface");
        let _ = surface.draw_artwork(&art.nodes, view.design);
    }
}

/// Reads `bytes`; if they are accepted, writes the drawing, reads that back
/// and writes it again, which must say the same, renders it, and writes it
/// as PDF and EPS.
fn read_never_panics(bytes: &[u8], rng: &mut Prng) {
    let mut ids = Ids::new();
    let Ok(first) = read(bytes, &mut ids) else {
        return;
    };
    let Ok(written) = write(&first.drawing) else {
        return;
    };
    let again = match read(written.as_bytes(), &mut ids) {
        Ok(again) => again,
        Err(error) => panic!("the writer wrote what the reader refuses ({error:?}):\n{written}"),
    };
    assert!(
        again.unkept.is_empty(),
        "the writer wrote what the drawing cannot hold:\n{written}"
    );
    let rewritten = write(&again.drawing).expect("a drawing that was written writes again");
    assert_eq!(
        rewritten, written,
        "a second round trip changed the document"
    );
    renders(&first.drawing, rng);
    let _ = tairix_drawing::pdf::pdf(&first.drawing, &Letters);
    if let Ok(eps) = tairix_drawing::eps::eps(&first.drawing, &Letters) {
        assert!(eps.bytes.iter().all(u8::is_ascii), "EPS stays seven-bit");
        assert!(
            eps.bytes
                .split(|&byte| byte == b'\n')
                .all(|line| line.len() <= 255),
            "EPS keeps its lines within the conventions"
        );
    }
}

/// A random nest of elements with random attributes in a well-formed frame.
fn generated(rng: &mut Prng) -> String {
    let mut body = String::new();
    let mut open: Vec<&str> = Vec::new();
    for _ in 0..rng.at_most(24) {
        if !open.is_empty() && rng.below(3) == 0 {
            if let Some(name) = open.pop() {
                let _ = write!(body, "</{name}>");
            }
            continue;
        }
        let name = *rng.pick(ELEMENTS);
        let _ = write!(body, "<{name}");
        for _ in 0..rng.at_most(5) {
            if rng.below(2) == 0 {
                let _ = write!(body, r#" {}="{}""#, rng.pick(ATTRIBUTES), rng.pick(NUMBERS));
            } else {
                let _ = write!(body, " {}", rng.pick(STRUCTURED));
            }
        }
        if matches!(name, "text" | "title" | "desc") {
            let _ = write!(body, ">word\n {}\t</{name}>", rng.pick(NUMBERS));
        } else if rng.below(2) == 0 {
            body.push_str("/>");
        } else {
            body.push('>');
            open.push(name);
        }
    }
    while let Some(name) = open.pop() {
        let _ = write!(body, "</{name}>");
    }
    format!(r#"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100">{body}</svg>"#)
}

#[test]
fn reading_never_panics_and_what_is_written_reads_back() {
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "reading_never_panics_and_what_is_written_reads_back",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    for template in TEMPLATES {
        read_never_panics(template.as_bytes(), &mut rng);
    }
    let mut iteration: u64 = 0;
    loop {
        let mut mutated = rng.pick(TEMPLATES).as_bytes().to_vec();
        for _ in 0..rng.at_most(8) {
            let at = rng.below(mutated.len());
            mutated[at] ^= rng.next_u8();
        }
        read_never_panics(&mutated, &mut rng);

        let document = generated(&mut rng);
        read_never_panics(document.as_bytes(), &mut rng);

        let mut noise = vec![0u8; rng.at_most(MAX_NOISE)];
        rng.fill(&mut noise);
        read_never_panics(&noise, &mut rng);

        iteration += 1;
        if !tairix_fuzzseed::within_budget(deadline) && iteration >= SMOKE_ITERATIONS {
            break;
        }
    }
}

/// Words as a Draw file holds them.
fn draw_words(out: &mut Vec<u8>, words: &[u32]) {
    for word in words {
        out.extend_from_slice(&word.to_le_bytes());
    }
}

/// A word that is ordinary or hostile in a Draw file.
fn draw_word(rng: &mut Prng) -> u32 {
    match rng.below(6) {
        0 => 0,
        1 => u32::MAX,
        2 => rng.next_u32() % 4096,
        3 => rng.next_u32() % (480 * 2000),
        4 => 0x8000_0000 | (rng.next_u32() % 4096),
        _ => rng.next_u32(),
    }
}

/// An object of `kind` around `body`, its size and bounds right.
fn draw_object(kind: u32, body: &[u8]) -> Vec<u8> {
    let mut object = Vec::new();
    let size = u32::try_from(24 + body.len()).expect("small");
    draw_words(&mut object, &[kind, size, 0, 0, 0, 0]);
    object.extend_from_slice(body);
    while object.len() % 4 != 0 {
        object.push(0);
    }
    let size = u32::try_from(object.len()).expect("small");
    object[4..8].copy_from_slice(&size.to_le_bytes());
    object
}

/// A random, well-framed object of any kind Draw files hold, its groups and
/// tagged objects nesting `depth` further at most.
fn generated_draw_object(rng: &mut Prng, depth: usize) -> Vec<u8> {
    let mut body = Vec::new();
    let kind = match rng.below(10) {
        0 | 1 => generated_path(rng, &mut body),
        2 | 3 => generated_text(rng, &mut body),
        4 if depth > 0 => {
            for _ in 0..12 {
                body.push(*rng.pick(b"Ab \0\x9f\xe9"));
            }
            for _ in 0..rng.at_most(3) {
                body.extend(generated_draw_object(rng, depth - 1));
            }
            6
        }
        5 if depth > 0 => {
            draw_words(&mut body, &[draw_word(rng)]);
            body.extend(generated_draw_object(rng, depth - 1));
            for _ in 0..rng.at_most(2) {
                let data = draw_word(rng);
                draw_words(&mut body, &[data]);
            }
            7
        }
        6 => return generated_font_table(rng),
        7 => generated_options(rng, &mut body),
        _ => {
            for _ in 0..rng.at_most(16) {
                body.push(rng.next_u8());
            }
            *rng.pick(&[5_u32, 9, 10, 13, 16, 3, 99])
        }
    };
    draw_object(kind, &body)
}

/// A random path's body onto `body`, its components of every tag and some
/// no path holds; its object type.
fn generated_path(rng: &mut Prng, body: &mut Vec<u8>) -> u32 {
    let style = draw_word(rng);
    draw_words(
        body,
        &[draw_word(rng), draw_word(rng), draw_word(rng), style],
    );
    if style & (1 << 7) != 0 {
        let count = rng.at_most(6);
        draw_words(
            body,
            &[draw_word(rng), u32::try_from(count).expect("small")],
        );
        for _ in 0..count {
            let length = draw_word(rng);
            draw_words(body, &[length]);
        }
    }
    for _ in 0..rng.at_most(12) {
        let tag = *rng.pick(&[2_u32, 2, 8, 8, 6, 7, 5, 4, 3, 9]);
        draw_words(body, &[tag]);
        let points = match tag {
            2 | 7 | 8 => 1,
            6 => 3,
            _ => 0,
        };
        for _ in 0..2 * points {
            let coordinate = draw_word(rng);
            draw_words(body, &[coordinate]);
        }
    }
    draw_words(body, &[0]);
    2
}

/// A random text's or transformed text's body onto `body`; its object type.
fn generated_text(rng: &mut Prng, body: &mut Vec<u8>) -> u32 {
    let transformed = rng.below(2) == 0;
    if transformed {
        for _ in 0..6 {
            let entry = draw_word(rng);
            draw_words(body, &[entry]);
        }
        let flags = draw_word(rng);
        draw_words(body, &[flags]);
    }
    let font = u32::try_from(rng.at_most(4)).expect("small");
    let (across, up) = (draw_word(rng) % 100_000, draw_word(rng) % 100_000);
    draw_words(body, &[draw_word(rng), draw_word(rng), font, across, up]);
    draw_words(body, &[draw_word(rng), draw_word(rng)]);
    for _ in 0..rng.at_most(12) {
        body.push(rng.next_u8().max(1));
    }
    body.push(0);
    if transformed {
        12
    } else {
        1
    }
}

/// A random font table, framed.
fn generated_font_table(rng: &mut Prng) -> Vec<u8> {
    let mut table = Vec::new();
    for _ in 0..rng.at_most(3) {
        table.push(u8::try_from(rng.at_most(4)).expect("small"));
        table.extend_from_slice(rng.pick(&[
            &b"Trinity.Medium"[..],
            b"Homerton.Bold.Oblique",
            b"Corpus",
            b"",
            b".Bold",
            b"Odd,\"Name\".Black",
        ]));
        table.push(0);
    }
    while table.len() % 4 != 0 {
        table.push(0);
    }
    let mut object = Vec::new();
    let size = u32::try_from(8 + table.len()).expect("small");
    draw_words(&mut object, &[0, size]);
    object.extend(table);
    object
}

/// A random options object's body onto `body`; its object type.
fn generated_options(rng: &mut Prng, body: &mut Vec<u8>) -> u32 {
    let paper = u32::try_from(rng.at_most(8)).expect("small") << 8;
    let spacing = [1.0_f64, 0.0, -1.0, f64::NAN, 1e300, 2.54][rng.below(6)].to_bits();
    let [high, low] = [
        u32::try_from(spacing >> 32).expect("high"),
        u32::try_from(spacing & u64::from(u32::MAX)).expect("low"),
    ];
    let division = draw_word(rng);
    let unit = draw_word(rng);
    draw_words(
        body,
        &[paper, draw_word(rng), high, low, division, 0, 0, 0, 0, unit],
    );
    draw_words(body, &[1, 1, 0, 1, 0x80, 5000]);
    11
}

/// A random Draw file of well-framed objects.
fn generated_draw(rng: &mut Prng) -> Vec<u8> {
    let mut file = b"Draw".to_vec();
    draw_words(&mut file, &[201, 0]);
    file.extend_from_slice(b"Fuzz        ");
    draw_words(&mut file, &[0; 4]);
    for _ in 0..rng.at_most(8) {
        file.extend(generated_draw_object(rng, 3));
    }
    file
}

/// Reads `bytes` as a Draw file; if they are accepted, writes the drawing as
/// one and reads that back whole, renders it and writes it as SVG, PDF and
/// EPS, without panicking.
fn draw_read_never_panics(bytes: &[u8], rng: &mut Prng) {
    let Ok(first) = tairix_drawing::riscos::read(bytes, &mut Ids::new()) else {
        return;
    };
    let copy = match tairix_drawing::riscos::write(&first.drawing, &Letters) {
        Ok(copy) => copy,
        Err(tairix_drawing::DrawingError::OutOfMemory) => {
            panic!("a small drawing's copy refused for memory")
        }
        Err(_) => return,
    };
    let again = match tairix_drawing::riscos::read(&copy.bytes, &mut Ids::new()) {
        Ok(again) => again,
        Err(error) => panic!("the writer wrote a Draw file the reader refuses ({error:?})"),
    };
    assert!(
        again.unkept.is_empty(),
        "the writer wrote what the drawing cannot keep: {:?}",
        again.unkept
    );
    renders(&first.drawing, rng);
    let _ = write(&first.drawing);
    let _ = tairix_drawing::pdf::pdf(&first.drawing, &Letters);
    let _ = tairix_drawing::eps::eps(&first.drawing, &Letters);
}

#[test]
fn reading_a_draw_file_never_panics_and_its_copy_reads_back() {
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "reading_a_draw_file_never_panics_and_its_copy_reads_back",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let seeds: Vec<Vec<u8>> = TEMPLATES
        .iter()
        .filter_map(|template| read(template.as_bytes(), &mut Ids::new()).ok())
        .filter_map(|read| tairix_drawing::riscos::write(&read.drawing, &Letters).ok())
        .map(|written| written.bytes)
        .collect();
    assert!(!seeds.is_empty(), "the templates write as Draw files");
    for seed in &seeds {
        assert!(
            tairix_drawing::riscos::read(seed, &mut Ids::new()).is_ok(),
            "a Draw file the writer wrote reads"
        );
        draw_read_never_panics(seed, &mut rng);
    }
    let mut iteration: u64 = 0;
    loop {
        let mut mutated = rng.pick(&seeds).clone();
        for _ in 0..rng.at_most(8) {
            let at = rng.below(mutated.len());
            mutated[at] ^= rng.next_u8();
        }
        draw_read_never_panics(&mutated, &mut rng);

        let file = generated_draw(&mut rng);
        draw_read_never_panics(&file, &mut rng);
        let _ = tairix_drawing::read(&file, &mut Ids::new());

        let mut noise = b"Draw\xc9\0\0\0".to_vec();
        noise.resize(8 + rng.at_most(MAX_NOISE), 0);
        rng.fill(&mut noise[8..]);
        draw_read_never_panics(&noise, &mut rng);

        iteration += 1;
        if !tairix_fuzzseed::within_budget(deadline) && iteration >= SMOKE_ITERATIONS {
            break;
        }
    }
}
