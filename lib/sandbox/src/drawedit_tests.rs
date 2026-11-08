//! The sandboxed drawing read through a loopback worker: a drawing comes
//! back as it reads in place, however large; every refusal; and every reply
//! a parent must not believe.

use alloc::rc::Rc;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;
use core::cell::RefCell;
use core::fmt::Write as _;

use tairix_drawing::svg::{read, write};
use tairix_drawing::Ids;

use super::{
    put_header, put_point, read_drawing, read_pages, read_placed, receive_picture, take_header,
    Access, Allowance, Builder, DrawEditService, DrawFailure, DrawRefusal, Pages,
    MAX_PASSWORD_BYTES, OP_NEXT, RECORD_GROUP, RECORD_IMAGE, RECORD_RECT, RECORD_STYLE,
    RECORD_TEXT, REPLY_BATCH, REPLY_OPENED, REPLY_PAGES, REPLY_PICTURE, REPLY_PIXELS, REPLY_PLACED,
    RUN_FILL, RUN_SIZE, RUN_STROKE, SOURCE_NONE, SOURCE_PNG, SOURCE_SPRITE,
};
use crate::host::ParserSandbox;
use crate::imagerender::send_document;
use crate::loopback::LoopbackLauncher;
use crate::testing::{loopback, tampering, NullSink};
use crate::wire::{Reader, Writer};
use crate::worker::Service;
use tairix_drawing::limits::{MAX_TEXT_PATH_NODES, MAX_TEXT_RUNS};

type TestSandbox = ParserSandbox<LoopbackLauncher<fn() -> DrawEditService>, NullSink>;

/// A drawing of one of most things: layers, a title, guides, swatches and a grid, a
/// gradient shared twice, a clip, text with runs and along a path, a star, an
/// arc, a path with every join, dashes, arrowheads, and a picture it cannot
/// keep.
const RICH: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" xmlns:draw="urn:tairix:draw"
    width="210mm" height="297mm" viewBox="0 0 793.7007874015749 1122.5196850393702"
    draw:grid="20 5" draw:background="#fffff0" draw:resolution="150">
  <title>Rich</title><desc>One of most things.</desc>
  <draw:guides><draw:guide axis="horizontal" position="100"/><draw:guide axis="vertical" position="50.5"/></draw:guides>
  <draw:swatches><draw:swatch colour="#c81e28"/><draw:swatch colour="#005aa064"/></draw:swatches>
  <defs>
    <linearGradient id="g" x1="0" y1="0" x2="100" y2="0" gradientUnits="userSpaceOnUse" spreadMethod="reflect">
      <stop offset="0" stop-color="#f00"/><stop offset="1" stop-color="#00f" stop-opacity="0.5"/>
    </linearGradient>
    <clipPath id="k"><circle cx="50" cy="50" r="40"/></clipPath>
    <path id="way" d="M20 600 C80 560 160 640 240 600 L300 620"/>
  </defs>
  <g draw:layer="true" draw:name="Back" opacity="0.75">
    <rect x="10" y="10" width="100" height="50" rx="5" fill="url(#g)" stroke="url(#g)" stroke-width="3"/>
    <g clip-path="url(#k)" draw:locked="true"><rect width="100" height="100" fill="#0a0"/></g>
    <image href="x.png" width="10" height="10"/>
  </g>
  <g draw:layer="true" draw:name="Front" display="none">
    <text x="20" y="200" font-family="serif" font-size="24" font-weight="700" font-style="italic" letter-spacing="1.5">Line <tspan font-family="mono" font-size="30" font-weight="400" font-style="normal" letter-spacing="-1" baseline-shift="0.25em" fill="#c00" fill-opacity="0.5" stroke="none">one</tspan><tspan x="20" y="230">Line two</tspan></text>
    <text font-size="18" text-anchor="middle"><textPath href="#way" startOffset="40">Along <tspan stroke="#00f">the</tspan> way</textPath></text>
    <path d="M0 0 L1 1" draw:shape="star" draw:cx="300" draw:cy="300" draw:points="7" draw:outer="80" draw:inner="0.4" draw:rotation="10" draw:rounding="0.2"/>
    <path d="M0 0 L1 1" draw:shape="arc" draw:cx="400" draw:cy="400" draw:rx="50" draw:ry="30" draw:start="20" draw:end="200" draw:arc="chord"/>
    <path d="M10 500 C20 480 40 480 50 500 S80 520 90 500 L120 520 Z" draw:nodes="cszc" fill="none" stroke="#123456" stroke-width="2" stroke-dasharray="4 2" stroke-dashoffset="1" stroke-linecap="round" draw:end-arrow="triangle 3 4" marker-end="url(#m)"/>
  </g>
</svg>"##;

fn in_place(document: &str) -> String {
    let drawing = read(document.as_bytes(), &mut Ids::new())
        .expect("reads in place")
        .drawing;
    write(&drawing).expect("writes")
}

fn through(
    sandbox: &mut TestSandbox,
    document: &[u8],
) -> Result<tairix_drawing::Read, DrawFailure> {
    send_document(sandbox, document).expect("handed over");
    read_drawing(
        sandbox,
        document.len() as u64,
        &Access::default(),
        &mut Ids::new(),
    )
}

/// What a scripted worker answers as though it were handed: a document no
/// source a script names is longer than.
const SCRIPTED: u64 = u64::MAX;

#[test]
fn a_drawing_read_in_the_sandbox_is_the_drawing_read_in_place() {
    let mut sandbox: TestSandbox = loopback();
    let read = through(&mut sandbox, RICH.as_bytes()).expect("a drawing");
    assert_eq!(write(&read.drawing).expect("writes"), in_place(RICH));
    assert_eq!(read.drawing.swatches().len(), 2, "the swatches came across");
    assert_eq!(read.drawing.resolution(), 150, "and the copy resolution");
    let kept = tairix_drawing::svg::read(RICH.as_bytes(), &mut Ids::new())
        .expect("reads")
        .unkept;
    assert_eq!(read.unkept, kept, "what could not be kept is said");
    assert!(!read.unkept.is_empty());
    assert_eq!(
        read_drawing(&mut sandbox, SCRIPTED, &Access::default(), &mut Ids::new()).err(),
        Some(DrawFailure::Refused(DrawRefusal::NoDocument)),
        "the read consumed the document"
    );
}

#[test]
fn a_draw_file_is_read_by_its_content_alone() {
    let drawing = read(RICH.as_bytes(), &mut Ids::new())
        .expect("reads")
        .drawing;
    let written = tairix_drawing::riscos::write(&drawing, &NoGlyphs).expect("a Draw file");
    let in_place = tairix_drawing::riscos::read(&written.bytes, &mut Ids::new()).expect("reads");
    let mut sandbox: TestSandbox = loopback();
    let read = through(&mut sandbox, &written.bytes).expect("a drawing");
    assert_eq!(
        write(&read.drawing).expect("writes"),
        write(&in_place.drawing).expect("writes")
    );
    assert_eq!(read.unkept, in_place.unkept);
    let mut broken = written.bytes;
    broken.truncate(60);
    assert_eq!(
        through(&mut sandbox, &broken).err(),
        Some(DrawFailure::Refused(DrawRefusal::Malformed)),
        "a Draw file cut short is a broken one, never read as SVG"
    );
}

/// No glyph is known: text is laid out from its estimated advances.
struct NoGlyphs;

impl tairix_drawing::Glyphs for NoGlyphs {
    fn glyph(
        &self,
        _family: &str,
        _weight: u16,
        _italic: bool,
        _c: char,
    ) -> Option<alloc::sync::Arc<tairix_drawing::Glyph>> {
        None
    }
}

#[test]
fn a_drawing_larger_than_a_frame_arrives_across_batches() {
    // A path of more nodes than one record holds, and more objects than one
    // batch carries.
    let mut document = String::from(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="1000" height="1000"><path d="M0 0"#,
    );
    for step in 0..200_000_u32 {
        let _ = write!(document, " L{} {}", step % 997, step % 991);
    }
    document.push_str(r#""/>"#);
    for index in 0..40_000_u32 {
        let _ = write!(
            document,
            r##"<rect x="{}" y="1" width="2" height="2" fill="#{:06x}"/>"##,
            index % 900,
            index
        );
    }
    document.push_str("</svg>");
    let mut sandbox: TestSandbox = loopback();
    let read = through(&mut sandbox, document.as_bytes()).expect("a drawing");
    assert_eq!(write(&read.drawing).expect("writes"), in_place(&document));
}

#[test]
fn what_is_no_drawing_is_refused_for_what_it_is() {
    let mut sandbox: TestSandbox = loopback();
    for (document, refusal) in [
        (&b"plain words"[..], DrawRefusal::NotADrawing),
        (&b"\xff\xfe"[..], DrawRefusal::NotADrawing),
        (&b"<svg><rect"[..], DrawRefusal::Malformed),
    ] {
        assert_eq!(
            through(&mut sandbox, document).err(),
            Some(DrawFailure::Refused(refusal))
        );
    }
    assert_eq!(
        read_drawing(&mut sandbox, SCRIPTED, &Access::default(), &mut Ids::new()).err(),
        Some(DrawFailure::Refused(DrawRefusal::NoDocument))
    );
    let mut service = DrawEditService::default();
    let refused = service.handle(&[OP_NEXT]);
    assert_eq!(refused.first(), Some(&crate::imagerender::REPLY_ERROR));
}

/// Answers each request with the next reply it holds.
struct Replies(Vec<Vec<u8>>);

impl Service for Replies {
    fn handle(&mut self, _request: &[u8]) -> Vec<u8> {
        if self.0.is_empty() {
            Vec::new()
        } else {
            self.0.remove(0)
        }
    }
}

fn answering(
    replies: Vec<Vec<u8>>,
) -> ParserSandbox<LoopbackLauncher<impl FnMut() -> Replies>, NullSink> {
    answering_within(replies, u64::MAX)
}

/// [`answering`], its workers handed documents of at most `room` bytes.
fn answering_within(
    replies: Vec<Vec<u8>>,
    room: u64,
) -> ParserSandbox<LoopbackLauncher<impl FnMut() -> Replies>, NullSink> {
    ParserSandbox::new(
        LoopbackLauncher::new(move || Replies(replies.clone())).holding(room),
        NullSink,
    )
}

/// Answers a list of one page, keeping each request it is asked.
struct Recording(Rc<RefCell<Vec<Vec<u8>>>>);

impl Service for Recording {
    fn handle(&mut self, request: &[u8]) -> Vec<u8> {
        self.0.borrow_mut().push(request.to_vec());
        let mut w = Writer::new();
        w.u8(REPLY_PAGES);
        w.u32(1);
        w.u32(0);
        w.finish()
    }
}

/// The open reply for a blank page of `layers` layers.
fn opened(layers: u32) -> Vec<u8> {
    opened_showing(layers, 0)
}

/// The open reply for a blank page of `layers` layers whose objects show
/// `pictures` pictures.
fn opened_showing(layers: u32, pictures: usize) -> Vec<u8> {
    let blank = read(
        br#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"/>"#,
        &mut Ids::new(),
    )
    .expect("a page");
    let mut w = Writer::new();
    w.u8(REPLY_OPENED);
    put_header(&mut w, &blank, pictures);
    let mut reply = w.finish();
    // The layer count is the header's last field but the picture count.
    let at = reply.len() - 8;
    reply[at..at + 4].copy_from_slice(&layers.to_le_bytes());
    reply
}

/// A batch of `records`.
fn batch(last: bool, records: &[Vec<u8>]) -> Vec<u8> {
    let mut w = Writer::new();
    w.u8(REPLY_BATCH);
    w.u8(u8::from(last));
    for record in records {
        w.bytes(record);
    }
    w.finish()
}

fn record(fields: impl FnOnce(&mut Writer)) -> Vec<u8> {
    let mut w = Writer::new();
    fields(&mut w);
    w.finish()
}

fn common(w: &mut Writer, opacity: f64) {
    w.u8(0);
    w.u8(1);
    w.u8(0);
    w.u64(opacity.to_bits());
    for value in [1.0_f64, 0.0, 0.0, 1.0, 0.0, 0.0] {
        w.u64(value.to_bits());
    }
}

fn group(children: u32) -> Vec<u8> {
    record(|w| {
        w.u8(RECORD_GROUP);
        common(w, 1.0);
        w.u32(children);
        w.u8(0);
    })
}

fn style() -> Vec<u8> {
    record(|w| {
        w.u8(RECORD_STYLE);
        w.u8(1);
        w.u32(0xff00_00ff);
        w.u8(0);
        w.u8(0);
        w.u64(1.0_f64.to_bits());
        w.u8(0);
        w.u64(4.0_f64.to_bits());
        w.u8(0);
        w.u32(0);
        w.u64(0.0_f64.to_bits());
        for _ in 0..2 {
            w.u8(0);
            w.u64(3.0_f64.to_bits());
            w.u64(3.0_f64.to_bits());
        }
        w.u8(0);
    })
}

fn rect(style: u32, x: f64) -> Vec<u8> {
    record(|w| {
        w.u8(RECORD_RECT);
        common(w, 1.0);
        w.u32(style);
        for value in [x, 0.0, 5.0, 5.0, 0.0, 0.0] {
            w.u64(value.to_bits());
        }
    })
}

#[test]
fn an_honest_script_is_believed() {
    let mut sandbox = answering(vec![
        opened(1),
        batch(true, &[style(), group(1), rect(0, 1.0)]),
    ]);
    let read = read_drawing(&mut sandbox, SCRIPTED, &Access::default(), &mut Ids::new())
        .expect("believed");
    assert_eq!(read.drawing.objects(), 2);
}

#[test]
fn a_reply_that_breaks_its_bounds_is_not_believed() {
    let mut truncated = rect(0, 1.0);
    truncated.truncate(truncated.len() - 3);
    let mut trailing = rect(0, 1.0);
    trailing.push(0);
    for (case, script) in [
        (
            "a style not sent",
            vec![opened(1), batch(true, &[group(1), rect(0, 1.0)])],
        ),
        (
            "a style past those sent",
            vec![opened(1), batch(true, &[style(), group(1), rect(1, 1.0)])],
        ),
        (
            "a coordinate that is no number",
            vec![
                opened(1),
                batch(true, &[style(), group(1), rect(0, f64::NAN)]),
            ],
        ),
        (
            "a child more than declared",
            vec![
                opened(1),
                batch(true, &[style(), group(1), rect(0, 1.0), rect(0, 2.0)]),
            ],
        ),
        (
            "a child fewer than declared",
            vec![opened(1), batch(true, &[style(), group(2), rect(0, 1.0)])],
        ),
        (
            "a layer more than declared",
            vec![opened(1), batch(true, &[group(0), group(0)])],
        ),
        (
            "a layer that is no group",
            vec![opened(1), batch(true, &[style(), rect(0, 1.0)])],
        ),
        (
            "a record cut short",
            vec![opened(1), batch(true, &[style(), group(1), truncated])],
        ),
        (
            "a record running on",
            vec![opened(1), batch(true, &[style(), group(1), trailing])],
        ),
        (
            "a batch that brings nothing",
            vec![opened(1), batch(false, &[]), batch(true, &[group(0)])],
        ),
        (
            "an unknown record",
            vec![opened(1), batch(true, &[vec![99]])],
        ),
        (
            "an open reply where a batch belongs",
            vec![opened(1), opened(1)],
        ),
    ] {
        let mut sandbox = answering(script);
        assert_eq!(
            read_drawing(&mut sandbox, SCRIPTED, &Access::default(), &mut Ids::new()).err(),
            Some(DrawFailure::ReplyMalformed),
            "{case}"
        );
    }
}

/// A text record of "ab" whose baseline and runs `baseline` and `runs`
/// write.
fn text(baseline: impl FnOnce(&mut Writer), runs: impl FnOnce(&mut Writer)) -> Vec<u8> {
    record(|w| {
        w.u8(RECORD_TEXT);
        common(w, 1.0);
        w.u32(0);
        w.str("ab");
        w.str("serif");
        w.u64(10.0_f64.to_bits());
        w.u32(400);
        w.u8(0);
        w.u64(0.0_f64.to_bits());
        w.u8(0);
        baseline(w);
        runs(w);
    })
}

fn lines(w: &mut Writer) {
    w.u8(0);
    put_point(w, tairix_bezier::Point::ZERO);
    w.u64(1.25_f64.to_bits());
}

/// A path baseline of one open contour of `nodes` corners along the x axis.
fn along(nodes: u32) -> impl FnOnce(&mut Writer) {
    move |w| {
        w.u8(1);
        w.u64(5.0_f64.to_bits());
        w.u32(1);
        w.u8(0);
        w.u32(nodes);
        for node in 0..nodes {
            put_point(w, tairix_bezier::Point::new(f64::from(node), 0.0));
            w.u8(0);
            w.u8(0);
            w.u8(0);
        }
    }
}

/// `spans` of runs each setting a size of 20 and a red fill.
fn runs(spans: &'static [(u32, u32)]) -> impl FnOnce(&mut Writer) {
    move |w| {
        w.u32(u32::try_from(spans.len()).expect("few"));
        for &(start, end) in spans {
            w.u32(start);
            w.u32(end);
            w.u8(RUN_SIZE | RUN_FILL);
            w.u64(20.0_f64.to_bits());
            w.u8(1);
            w.u32(0xff00_00ff);
        }
    }
}

#[test]
fn a_text_s_baseline_and_runs_are_believed_only_within_their_bounds() {
    let mut sandbox = answering(vec![
        opened(1),
        batch(true, &[style(), group(1), text(along(2), runs(&[(1, 2)]))]),
    ]);
    let read = read_drawing(&mut sandbox, SCRIPTED, &Access::default(), &mut Ids::new())
        .expect("believed");
    let layer = read.drawing.layers().get(0).cloned().expect("a layer");
    let node = layer
        .children()
        .and_then(|children| children.get(0).cloned())
        .expect("a text");
    let tairix_drawing::Kind::Text { text: believed, .. } = node.kind() else {
        panic!("a text");
    };
    assert!(
        matches!(&believed.baseline, tairix_drawing::Baseline::Path(along) if along.path.node_count() == 2 && (along.start - 5.0).abs() < 1e-12)
    );
    assert_eq!(believed.runs.len(), 1);
    let past_path = u32::try_from(MAX_TEXT_PATH_NODES + 1).expect("small");
    let past_runs = u32::try_from(MAX_TEXT_RUNS + 1).expect("small");
    for (case, record) in [
        ("a baseline of no kind", text(|w| w.u8(2), runs(&[]))),
        ("a path past its bound", text(along(past_path), runs(&[]))),
        (
            "runs past their bound",
            text(lines, move |w| w.u32(past_runs)),
        ),
        ("overlapping runs", text(lines, runs(&[(0, 2), (1, 2)]))),
        ("a run past the content", text(lines, runs(&[(1, 3)]))),
        (
            "a run paint of no kind",
            text(lines, |w| {
                w.u32(1);
                w.u32(0);
                w.u32(1);
                w.u8(RUN_STROKE);
                w.u8(2);
            }),
        ),
    ] {
        let mut sandbox = answering(vec![opened(1), batch(true, &[style(), group(1), record])]);
        assert_eq!(
            read_drawing(&mut sandbox, SCRIPTED, &Access::default(), &mut Ids::new()).err(),
            Some(DrawFailure::ReplyMalformed),
            "{case}"
        );
    }
}

#[test]
fn a_worker_that_turns_hostile_part_way_is_contained() {
    let mut sandbox = tampering::<DrawEditService>(OP_NEXT, |mut reply| {
        reply.truncate(reply.len() / 2);
        reply
    });
    send_document(&mut sandbox, RICH.as_bytes()).expect("handed over");
    assert_eq!(
        read_drawing(
            &mut sandbox,
            RICH.len() as u64,
            &Access::default(),
            &mut Ids::new()
        )
        .err(),
        Some(DrawFailure::ReplyMalformed)
    );
    assert!(DrawFailure::ReplyMalformed
        .to_string()
        .contains("could not be believed"));
    assert!(DrawFailure::NoMemory.passing());
    assert!(!DrawFailure::Refused(DrawRefusal::Malformed).passing());
}

/// `bytes` as a base64 data URL.
fn data_url(bytes: &[u8]) -> String {
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
    url
}

/// A translucent PNG `width` by `height`.
fn png(width: u32, height: u32) -> Vec<u8> {
    let mut rgba = Vec::new();
    for y in 0..height {
        for x in 0..width {
            let level = u8::try_from((x * 7 + y * 13) % 256).expect("a level");
            rgba.extend_from_slice(&[level, 255 - level, 90, if x % 3 == 0 { 255 } else { 120 }]);
        }
    }
    let picture = tairix_image::Picture::rgba(width, height, rgba).expect("a picture");
    tairix_image::encode_png(&picture).expect("a png")
}

#[test]
fn pictures_come_across_once_however_often_they_are_shown() {
    // Pixels past one frame, so they arrive across replies.
    let big = png(1100, 1000);
    let small = png(3, 2);
    let document = alloc::format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="100" height="100">
             <image xlink:href="{}" width="40" height="30"/>
             <image xlink:href="{}" x="50" width="20" height="10"/>
             <image xlink:href="{}" y="50" width="40" height="30" transform="rotate(10)"/>
           </svg>"#,
        data_url(&big),
        data_url(&small),
        data_url(&big),
    );
    let mut sandbox: TestSandbox = loopback();
    let read = through(&mut sandbox, document.as_bytes()).expect("a drawing");
    assert!(read.unkept.is_empty(), "{:?}", read.unkept);
    assert_eq!(write(&read.drawing).expect("writes"), in_place(&document));
    let layer = read.drawing.layers().get(0).cloned().expect("a layer");
    let shown: Vec<_> = layer
        .children()
        .expect("held")
        .iter()
        .map(|node| match node.kind() {
            tairix_drawing::Kind::Image { image } => image.clone(),
            other => panic!("a picture, not {other:?}"),
        })
        .collect();
    assert!(alloc::sync::Arc::ptr_eq(
        shown[0].pyramid(),
        shown[2].pyramid()
    ));
    let in_place = read_in_place(&document);
    assert_eq!(shown[0].pixels().pixels(), in_place.pixels().pixels());
}

fn read_in_place(document: &str) -> tairix_drawing::Image {
    let drawing = read(document.as_bytes(), &mut Ids::new())
        .expect("reads")
        .drawing;
    let layer = drawing.layers().get(0).cloned().expect("a layer");
    let first = layer
        .children()
        .and_then(|children| children.get(0).cloned());
    match first.expect("a picture").kind() {
        tairix_drawing::Kind::Image { image } => (**image).clone(),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_picture_to_place_comes_across_with_the_size_it_states() {
    let mut sandbox: TestSandbox = loopback();
    let bytes = png(5, 4);
    send_document(&mut sandbox, &bytes).expect("handed over");
    let placed = read_placed(&mut sandbox, bytes.len() as u64).expect("a picture");
    let in_place = tairix_drawing::decode_placed(&bytes).expect("decodes");
    assert_eq!(placed.pixels.pixels(), in_place.pixels.pixels());
    assert_eq!(placed.natural, in_place.natural);
    assert_eq!(placed.source, in_place.source, "kept as it came");
    let words = b"plain words";
    send_document(&mut sandbox, words).expect("handed over");
    assert_eq!(
        read_placed(&mut sandbox, words.len() as u64).err(),
        Some(DrawFailure::Refused(DrawRefusal::NotAPicture))
    );
}

#[test]
fn a_record_refused_room_partway_is_answered_for_want_of_memory() {
    // A name is made before the fields after it are read.
    let named = record(|w| {
        w.u8(RECORD_RECT);
        w.u8(1);
        w.str("A named box");
        w.u8(1);
        w.u8(0);
        w.u64(1.0_f64.to_bits());
        for value in [1.0_f64, 0.0, 0.0, 1.0, 0.0, 0.0] {
            w.u64(value.to_bits());
        }
        w.u32(0);
        for value in [1.0_f64, 0.0, 5.0, 5.0, 0.0, 0.0] {
            w.u64(value.to_bits());
        }
    });
    let reply = batch(true, &[style(), group(1), named]);
    let opened = opened(1);
    tairix_fuzzseed::meter::refused_at_each(
        || {
            let header = take_header(&mut Reader::new(&opened[1..]))?;
            let mut ids = Ids::new();
            Builder::new(header, Vec::new(), &mut ids).batch(&reply)
        },
        |failure| *failure == DrawFailure::NoMemory,
    );
}

/// The reply placing a picture one unit square.
fn placed_reply() -> Vec<u8> {
    let mut w = Writer::new();
    w.u8(REPLY_PLACED);
    put_point(&mut w, tairix_bezier::Point::new(1.0, 1.0));
    w.finish()
}

#[test]
fn a_picture_to_place_kept_as_a_sprite_is_not_believed() {
    let sprite = {
        let mut w = Writer::new();
        w.u8(REPLY_PICTURE);
        w.u32(1);
        w.u32(1);
        w.u8(SOURCE_SPRITE);
        w.u8(1);
        w.u8(1);
        w.u64(4);
        w.finish()
    };
    let mut sandbox = answering(vec![
        placed_reply(),
        sprite,
        pixels(&[9, 9, 9, 9, 1, 2, 3, 4]),
    ]);
    assert_eq!(
        read_placed(&mut sandbox, SCRIPTED).err(),
        Some(DrawFailure::ReplyMalformed),
        "a placed picture keeps only a PNG or a JPEG as it came"
    );
}

/// The shape of a `width` by `height` picture with no source.
fn shape(width: u32, height: u32) -> Vec<u8> {
    let mut w = Writer::new();
    w.u8(REPLY_PICTURE);
    w.u32(width);
    w.u32(height);
    w.u8(SOURCE_NONE);
    w.u64(0);
    w.finish()
}

fn pixels(bytes: &[u8]) -> Vec<u8> {
    let mut reply = vec![REPLY_PIXELS];
    reply.extend_from_slice(bytes);
    reply
}

fn image(number: u32) -> Vec<u8> {
    record(|w| {
        w.u8(RECORD_IMAGE);
        common(w, 1.0);
        w.u32(number);
        for value in [0.0_f64, 0.0, 4.0, 2.0] {
            w.u64(value.to_bits());
        }
    })
}

#[test]
fn a_pictures_pixels_are_held_to_their_alpha_as_they_land() {
    let mut sandbox = answering(vec![
        opened_showing(1, 1),
        shape(2, 1),
        pixels(&[200, 10, 10, 100, 1, 2, 3, 255]),
        batch(true, &[group(1), image(0)]),
    ]);
    let read = read_drawing(&mut sandbox, SCRIPTED, &Access::default(), &mut Ids::new())
        .expect("believed");
    let layer = read.drawing.layers().get(0).cloned().expect("a layer");
    let picture = layer
        .children()
        .and_then(|children| children.get(0).cloned())
        .expect("one");
    let tairix_drawing::Kind::Image { image } = picture.kind() else {
        panic!("a picture");
    };
    let first = image.pixels().pixels()[0];
    assert_eq!((first.r, first.g, first.b, first.a), (100, 10, 10, 100));
}

#[test]
fn a_picture_that_breaks_its_bounds_is_not_believed() {
    let with_source = |kind: u8, length: u64| {
        let mut w = Writer::new();
        w.u8(REPLY_PICTURE);
        w.u32(1);
        w.u32(1);
        w.u8(kind);
        w.u64(length);
        w.finish()
    };
    for (case, script) in [
        (
            "a picture of no pixels",
            vec![opened_showing(1, 1), shape(0, 4)],
        ),
        (
            "a side past a picture's",
            vec![opened_showing(1, 1), shape(1 << 20, 1)],
        ),
        (
            "a source that says nothing of itself",
            vec![opened_showing(1, 1), with_source(SOURCE_NONE, 4)],
        ),
        (
            "a source of no bytes",
            vec![opened_showing(1, 1), with_source(SOURCE_PNG, 0)],
        ),
        (
            "an unknown source",
            vec![opened_showing(1, 1), with_source(9, 4)],
        ),
        (
            "pixels ending within a pixel",
            vec![
                opened_showing(1, 1),
                shape(2, 1),
                pixels(&[1, 2, 3, 4, 5, 6]),
            ],
        ),
        (
            "more pixels than the picture",
            vec![opened_showing(1, 1), shape(1, 1), pixels(&[1; 8])],
        ),
        (
            "no pixels at all",
            vec![opened_showing(1, 1), shape(1, 1), pixels(&[])],
        ),
        (
            "an image of a picture not sent",
            vec![
                opened_showing(1, 1),
                shape(1, 1),
                pixels(&[0; 4]),
                batch(true, &[group(1), image(1)]),
            ],
        ),
        (
            "more pictures than objects may be",
            vec![opened_showing(1, tairix_drawing::limits::MAX_OBJECTS + 1)],
        ),
        (
            "pixels in more exchanges than a frame needs",
            vec![
                opened_showing(1, 1),
                shape(2, 1),
                pixels(&[1, 2, 3, 4]),
                pixels(&[5, 6, 7, 8]),
                batch(true, &[group(1), image(0)]),
            ],
        ),
    ] {
        let mut sandbox = answering(script);
        assert_eq!(
            read_drawing(&mut sandbox, SCRIPTED, &Access::default(), &mut Ids::new()).err(),
            Some(DrawFailure::ReplyMalformed),
            "{case}"
        );
    }
}

#[test]
fn the_sources_of_one_read_together_hold_no_more_than_a_document_does() {
    let with_png = |length: u64| {
        let mut w = Writer::new();
        w.u8(REPLY_PICTURE);
        w.u32(1);
        w.u32(1);
        w.u8(SOURCE_PNG);
        w.u64(length);
        w.finish()
    };
    // The second picture comes whole, so only the bound refuses it.
    let mut sandbox = answering(vec![
        with_png(3),
        pixels(&[9, 9, 9, 9, 1, 2, 3]),
        with_png(3),
        pixels(&[8, 8, 8, 8, 4, 5, 6]),
    ]);
    let mut allowance = Allowance {
        pixels: 2,
        bytes: 5,
    };
    let (_, source) = receive_picture(&mut sandbox, 0, &mut allowance).expect("within");
    assert_eq!(
        source.map(|source| source.bytes.to_vec()),
        Some(vec![1, 2, 3])
    );
    assert_eq!((allowance.pixels, allowance.bytes), (1, 2));
    assert_eq!(
        receive_picture(&mut sandbox, 1, &mut allowance).err(),
        Some(DrawFailure::ReplyMalformed),
        "three bytes more than the two left"
    );
}

/// A source is part of the document it was read from, or what its streams
/// decoded to in the room the worker had beside it, so one longer than both
/// is a worker's invention.
#[test]
fn a_source_longer_than_the_document_and_its_decoding_room_is_not_believed() {
    let mut w = Writer::new();
    w.u8(REPLY_PICTURE);
    w.u32(1);
    w.u32(1);
    w.u8(SOURCE_PNG);
    w.u64(3);
    let sourced = w.finish();
    let script = || {
        vec![
            opened_showing(1, 1),
            sourced.clone(),
            pixels(&[9, 9, 9, 9, 1, 2, 3]),
            batch(true, &[group(1), image(0)]),
        ]
    };
    let read = |room: u64, handed: u64| {
        read_drawing(
            &mut answering_within(script(), room),
            handed,
            &Access::default(),
            &mut Ids::new(),
        )
    };
    assert_eq!(
        read(4, 2).err(),
        Some(DrawFailure::ReplyMalformed),
        "two bytes handed over, two to decode into"
    );
    assert!(
        read(5, 2).is_ok(),
        "decoded in the room beside the document"
    );
    assert!(read(4, 3).is_ok(), "a part of the document");
    let placed = || {
        vec![
            placed_reply(),
            sourced.clone(),
            pixels(&[9, 9, 9, 9, 1, 2, 3]),
        ]
    };
    assert_eq!(
        read_placed(&mut answering(placed()), 2).err(),
        Some(DrawFailure::ReplyMalformed)
    );
    assert!(read_placed(&mut answering(placed()), 3).is_ok());
}

/// Hand `document` over, its upload's failure said as the read's own so a
/// sweep holds the two to the one standard.
fn handed_over<L: crate::host::Launcher>(
    sandbox: &mut ParserSandbox<L, NullSink>,
    document: &[u8],
) -> Result<(), DrawFailure> {
    send_document(sandbox, document).map_err(|failure| match failure {
        crate::imagerender::DocumentFailure::NoMemory => DrawFailure::NoMemory,
        crate::imagerender::DocumentFailure::Sandbox(inner) => DrawFailure::Sandbox(inner),
        _ => DrawFailure::ReplyMalformed,
    })
}

fn draw_refused(failure: DrawFailure) -> bool {
    match failure {
        DrawFailure::NoMemory => true,
        DrawFailure::Sandbox(inner) => crate::testing::this_side(inner),
        _ => false,
    }
}

/// Every allocation the editor makes reading a drawing — its records, its
/// pictures, the model it builds — is answered for want of memory when
/// refused, never an abort.
#[test]
fn a_drawing_this_side_has_no_memory_for_is_refused_as_that() {
    let document = alloc::format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="100" height="100">
             <title>t</title><rect width="10" height="10" fill="red"/>
             <text x="4" y="30" font-size="12">Hi</text>
             <image xlink:href="{0}" width="40" height="30"/>
             <image xlink:href="{0}" y="50" width="40" height="30"/>
           </svg>"#,
        data_url(&png(3, 2)),
    );
    crate::testing::swept::<DrawEditService, _, _>(
        |sandbox| {
            handed_over(sandbox, document.as_bytes())?;
            read_drawing(
                sandbox,
                document.len() as u64,
                &Access::default(),
                &mut Ids::new(),
            )
        },
        |failure| draw_refused(*failure),
    );
    crate::testing::swept::<DrawEditService, _, _>(
        |sandbox| {
            handed_over(sandbox, RICH.as_bytes())?;
            read_drawing(
                sandbox,
                RICH.len() as u64,
                &Access::default(),
                &mut Ids::new(),
            )
        },
        |failure| draw_refused(*failure),
    );
}

#[test]
fn a_picture_to_place_this_side_has_no_memory_for_is_refused_as_that() {
    let bytes = png(5, 4);
    crate::testing::swept::<DrawEditService, _, _>(
        |sandbox| {
            handed_over(sandbox, &bytes)?;
            read_placed(sandbox, bytes.len() as u64)
        },
        |failure| draw_refused(*failure),
    );
}

/// A PDF of pages each `(width, height)` points, its trailer also stating
/// `trailer` and its objects then `extra`, numbered on from the pages.
fn pdf(pages: &[(u32, u32)], trailer: &str, extra: &[&str]) -> Vec<u8> {
    let mut objects = alloc::vec![String::from("<</Type /Catalog /Pages 2 0 R>>")];
    let kids: Vec<String> = (0..pages.len())
        .map(|at| alloc::format!("{} 0 R", at + 3))
        .collect();
    objects.push(alloc::format!(
        "<</Type /Pages /Kids [{}]>>",
        kids.join(" ")
    ));
    for &(width, height) in pages {
        objects.push(alloc::format!(
            "<</Type /Page /Parent 2 0 R /MediaBox [0 0 {width} {height}]>>"
        ));
    }
    objects.extend(extra.iter().map(|&body| String::from(body)));
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (at, body) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(alloc::format!("{} 0 obj\n{body}\nendobj\n", at + 1).as_bytes());
    }
    let table = out.len();
    out.extend_from_slice(
        alloc::format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes(),
    );
    for offset in offsets {
        out.extend_from_slice(alloc::format!("{offset:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        alloc::format!(
            "trailer\n<</Size {} /Root 1 0 R {trailer}>>\nstartxref\n{table}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );
    out
}

#[test]
fn a_pdfs_pages_are_listed_and_the_one_chosen_is_read() {
    let mut sandbox: TestSandbox = loopback();
    let document = pdf(&[(200, 100), (300, 400)], "", &[]);
    send_document(&mut sandbox, &document).expect("handed over");
    let pages =
        read_pages(&mut sandbox, document.len() as u64, &Access::default()).expect("its pages");
    assert_eq!(
        pages,
        Pages {
            count: 2,
            sizes: alloc::vec![(200.0, 100.0), (300.0, 400.0)]
        }
    );
    let second = Access {
        page: 1,
        password: None,
    };
    let read = read_drawing(
        &mut sandbox,
        document.len() as u64,
        &second,
        &mut Ids::new(),
    )
    .expect("page two");
    let size = read.drawing.page().size();
    assert!(
        (size.x - 400.0).abs() < 1e-6 && (size.y - 1600.0 / 3.0).abs() < 1e-6,
        "{size:?}"
    );
}

#[test]
fn a_page_a_pdf_lacks_is_refused() {
    let mut sandbox: TestSandbox = loopback();
    let document = pdf(&[(200, 100)], "", &[]);
    send_document(&mut sandbox, &document).expect("handed over");
    let past = Access {
        page: 3,
        password: None,
    };
    assert_eq!(
        read_drawing(&mut sandbox, document.len() as u64, &past, &mut Ids::new()).err(),
        Some(DrawFailure::Refused(DrawRefusal::NoSuchPage))
    );
}

#[test]
fn any_other_document_lists_one_page_of_no_stated_size() {
    let mut sandbox: TestSandbox = loopback();
    send_document(&mut sandbox, RICH.as_bytes()).expect("handed over");
    assert_eq!(
        read_pages(&mut sandbox, RICH.len() as u64, &Access::default()),
        Ok(Pages {
            count: 1,
            sizes: Vec::new()
        })
    );
    assert!(
        read_drawing(
            &mut sandbox,
            RICH.len() as u64,
            &Access::default(),
            &mut Ids::new()
        )
        .is_ok(),
        "the document was kept to read"
    );
}

#[test]
fn a_pdf_protected_by_a_password_asks_for_it_and_opens_with_it() {
    let mut sandbox: TestSandbox = loopback();
    let encrypt = "<</Filter /Standard /V 2 /R 3 /Length 128 /P -3904 /O <0ba3835f88f90388e74e54584125ce142be0de24c6b0d37746e075b891756671> /U <ad944bba9714b0f0d9da179955e1945000000000000000000000000000000000>>>";
    let document = pdf(
        &[(100, 100), (50, 50)],
        "/Encrypt 5 0 R /ID [<0123456789abcdef0123456789abcdef> <0123456789abcdef0123456789abcdef>]",
        &[encrypt],
    );
    send_document(&mut sandbox, &document).expect("handed over");
    let handed = document.len() as u64;
    assert_eq!(
        read_pages(&mut sandbox, handed, &Access::default()),
        Err(DrawFailure::Refused(DrawRefusal::NeedsPassword))
    );
    let wrong = Access {
        page: 0,
        password: Some(b"guess"),
    };
    assert_eq!(
        read_pages(&mut sandbox, handed, &wrong),
        Err(DrawFailure::Refused(DrawRefusal::WrongPassword))
    );
    let right = Access {
        page: 1,
        password: Some(b"user"),
    };
    assert_eq!(
        read_pages(&mut sandbox, handed, &right).map(|pages| pages.count),
        Ok(2)
    );
    let read = read_drawing(&mut sandbox, handed, &right, &mut Ids::new()).expect("page two");
    assert!((read.drawing.page().size().x - 200.0 / 3.0).abs() < 1e-6);
}

#[test]
fn a_password_past_the_bound_is_refused_unsent() {
    let mut sandbox = answering(Vec::new());
    let long = [b'x'; MAX_PASSWORD_BYTES + 1];
    let access = Access {
        page: 0,
        password: Some(&long),
    };
    assert_eq!(
        read_pages(&mut sandbox, 0, &access),
        Err(DrawFailure::Refused(DrawRefusal::MalformedRequest))
    );
}

#[test]
fn a_worker_decodes_into_what_its_share_holds_beside_the_document() {
    let asked = Rc::new(RefCell::new(Vec::new()));
    let kept = Rc::clone(&asked);
    let mut sandbox = ParserSandbox::new(
        LoopbackLauncher::new(move || Recording(Rc::clone(&kept))).holding(100),
        NullSink,
    );
    for handed in [30, 130] {
        assert!(read_pages(&mut sandbox, handed, &Access::default()).is_ok());
    }
    let rooms: Vec<Option<u64>> = asked
        .borrow()
        .iter()
        .map(|request| Reader::new(request.get(1..).unwrap_or_default()).u64().ok())
        .collect();
    assert_eq!(rooms, [Some(70), Some(0)]);
}

#[test]
fn a_page_list_out_of_its_grammar_is_not_believed() {
    let reply = |count: u32, listed: u32, sizes: &[(f64, f64)]| {
        let mut w = Writer::new();
        w.u8(REPLY_PAGES);
        w.u32(count);
        w.u32(listed);
        for &(width, height) in sizes {
            w.raw(&width.to_le_bytes());
            w.raw(&height.to_le_bytes());
        }
        w.finish()
    };
    for bad in [
        reply(0, 0, &[]),
        reply(1, 2, &[(1.0, 1.0), (1.0, 1.0)]),
        reply(1, 1, &[(f64::NAN, 1.0)]),
        reply(1, 1, &[(-1.0, 1.0)]),
        reply(2, 1, &[]),
        reply(u32::MAX, u32::MAX, &[]),
    ] {
        let mut sandbox = answering(alloc::vec![bad]);
        assert_eq!(
            read_pages(&mut sandbox, 0, &Access::default()),
            Err(DrawFailure::ReplyMalformed)
        );
    }
    let mut sandbox = answering(alloc::vec![reply(3, 1, &[(10.0, 20.0)])]);
    assert_eq!(
        read_pages(&mut sandbox, 0, &Access::default()),
        Ok(Pages {
            count: 3,
            sizes: alloc::vec![(10.0, 20.0)]
        })
    );
}
