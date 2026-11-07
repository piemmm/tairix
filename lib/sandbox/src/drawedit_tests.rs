//! The sandboxed drawing read through a loopback worker: a drawing comes
//! back as it reads in place, however large; every refusal; and every reply
//! a parent must not believe.

use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;
use core::fmt::Write as _;

use tairix_drawing::svg::{read, write};
use tairix_drawing::Ids;

use super::{
    put_header, read_drawing, DrawEditService, DrawFailure, DrawFormat, DrawRefusal, OP_NEXT,
    RECORD_GROUP, RECORD_RECT, RECORD_STYLE, REPLY_BATCH, REPLY_OPENED,
};
use crate::host::ParserSandbox;
use crate::imagerender::send_document;
use crate::loopback::LoopbackLauncher;
use crate::testing::{loopback, tampering, NullSink};
use crate::wire::Writer;
use crate::worker::Service;

type TestSandbox = ParserSandbox<LoopbackLauncher<fn() -> DrawEditService>, NullSink>;

/// A drawing of one of most things: layers, a title, guides and a grid, a
/// gradient shared twice, a clip, text, a star, an arc, a path with every
/// join, dashes, arrowheads, and a picture it cannot keep.
const RICH: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" xmlns:draw="urn:tairix:draw"
    width="210mm" height="297mm" viewBox="0 0 793.7007874015749 1122.5196850393702"
    draw:grid="20 5" draw:background="#fffff0">
  <title>Rich</title><desc>One of most things.</desc>
  <draw:guides><draw:guide axis="horizontal" position="100"/><draw:guide axis="vertical" position="50.5"/></draw:guides>
  <defs>
    <linearGradient id="g" x1="0" y1="0" x2="100" y2="0" gradientUnits="userSpaceOnUse" spreadMethod="reflect">
      <stop offset="0" stop-color="#f00"/><stop offset="1" stop-color="#00f" stop-opacity="0.5"/>
    </linearGradient>
    <clipPath id="k"><circle cx="50" cy="50" r="40"/></clipPath>
  </defs>
  <g draw:layer="true" draw:name="Back" opacity="0.75">
    <rect x="10" y="10" width="100" height="50" rx="5" fill="url(#g)" stroke="url(#g)" stroke-width="3"/>
    <g clip-path="url(#k)" draw:locked="true"><rect width="100" height="100" fill="#0a0"/></g>
    <image href="x.png" width="10" height="10"/>
  </g>
  <g draw:layer="true" draw:name="Front" display="none">
    <text x="20" y="200" font-family="serif" font-size="24" font-weight="700" font-style="italic" letter-spacing="1.5">Line one<tspan x="20" y="230">Line two</tspan></text>
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
) -> Result<tairix_drawing::svg::Read, DrawFailure> {
    send_document(sandbox, document).expect("handed over");
    read_drawing(sandbox, DrawFormat::Svg, &mut Ids::new())
}

#[test]
fn a_drawing_read_in_the_sandbox_is_the_drawing_read_in_place() {
    let mut sandbox: TestSandbox = loopback();
    let read = through(&mut sandbox, RICH.as_bytes()).expect("a drawing");
    assert_eq!(write(&read.drawing).expect("writes"), in_place(RICH));
    let kept = tairix_drawing::svg::read(RICH.as_bytes(), &mut Ids::new())
        .expect("reads")
        .unkept;
    assert_eq!(read.unkept, kept, "what could not be kept is said");
    assert!(!read.unkept.is_empty());
    assert_eq!(
        read_drawing(&mut sandbox, DrawFormat::Svg, &mut Ids::new()).err(),
        Some(DrawFailure::Refused(DrawRefusal::NoDocument)),
        "the read consumed the document"
    );
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
        read_drawing(&mut sandbox, DrawFormat::Svg, &mut Ids::new()).err(),
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
    ParserSandbox::new(
        LoopbackLauncher::new(move || Replies(replies.clone())),
        NullSink,
    )
}

/// The open reply for a blank page of `layers` layers.
fn opened(layers: u32) -> Vec<u8> {
    let blank = read(
        br#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"/>"#,
        &mut Ids::new(),
    )
    .expect("a page");
    let mut w = Writer::new();
    w.u8(REPLY_OPENED);
    put_header(&mut w, &blank);
    let mut reply = w.finish();
    // The layer count is the header's last field.
    let at = reply.len() - 4;
    reply[at..].copy_from_slice(&layers.to_le_bytes());
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
    let read = read_drawing(&mut sandbox, DrawFormat::Svg, &mut Ids::new()).expect("believed");
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
            read_drawing(&mut sandbox, DrawFormat::Svg, &mut Ids::new()).err(),
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
        read_drawing(&mut sandbox, DrawFormat::Svg, &mut Ids::new()).err(),
        Some(DrawFailure::ReplyMalformed)
    );
    assert!(DrawFailure::ReplyMalformed
        .to_string()
        .contains("could not be believed"));
    assert!(DrawFailure::NoMemory.passing());
    assert!(!DrawFailure::Refused(DrawRefusal::Malformed).passing());
}
