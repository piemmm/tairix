//! The structure report through a loopback worker: what a whole picture
//! reports, every refusal, and every reply a parent must not believe.

use alloc::string::ToString;
use alloc::vec::Vec;

use tairix_image::{encode_png, Picture};

use super::{
    inspect_document, InspectFailure, InspectRefusal, InspectService, OP_INSPECT, REPLY_INSPECTED,
};
use crate::host::ParserSandbox;
use crate::imagerender::{send_document, ImageRenderService, ViewFormat};
use crate::loopback::LoopbackLauncher;
use crate::testing::{loopback, scripted, tampering, NullSink};
use crate::wire::Writer;
use crate::worker::Service;

type TestSandbox = ParserSandbox<LoopbackLauncher<fn() -> ImageRenderService>, NullSink>;

fn png() -> Vec<u8> {
    let picture = Picture::rgba(2, 2, alloc::vec![200; 16]).expect("a picture");
    encode_png(&picture).expect("encodes")
}

#[test]
fn a_picture_handed_over_comes_back_as_its_report_and_is_let_go() {
    let mut sandbox: TestSandbox = loopback();
    send_document(&mut sandbox, &png()).expect("handed over");
    let lines = inspect_document(&mut sandbox, None).expect("a report");
    assert!(
        lines[0].heading && lines[0].text.starts_with("PNG image"),
        "{lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.text.contains("header: 2 \u{d7} 2")),
        "{lines:?}"
    );
    assert_eq!(
        inspect_document(&mut sandbox, None),
        Err(InspectFailure::Refused(InspectRefusal::NoDocument)),
        "the report consumed the document"
    );
}

/// The worker an editor reads reports in serves the image service's report,
/// over the same upload, and nothing else.
#[test]
fn the_report_worker_answers_as_the_image_service_does_and_serves_nothing_more() {
    let mut lean: ParserSandbox<LoopbackLauncher<fn() -> InspectService>, NullSink> = loopback();
    let mut full: TestSandbox = loopback();
    send_document(&mut lean, &png()).expect("handed over");
    send_document(&mut full, &png()).expect("handed over");
    assert_eq!(
        inspect_document(&mut lean, None),
        inspect_document(&mut full, None)
    );
    let mut service = InspectService::default();
    let refused = service.handle(&[1, 0, 0, 0, 16]);
    assert_eq!(refused.first(), Some(&crate::imagerender::REPLY_ERROR));
}

#[test]
fn a_named_format_is_read_as_named_and_vector_art_or_noise_is_refused() {
    let mut sandbox: TestSandbox = loopback();
    send_document(&mut sandbox, &png()).expect("handed over");
    let lines = inspect_document(&mut sandbox, Some(ViewFormat::Gif)).expect("a report");
    assert!(
        lines
            .iter()
            .any(|line| line.text.contains("no GIF signature")),
        "read as the format named: {lines:?}"
    );
    send_document(&mut sandbox, &png()).expect("handed over");
    assert_eq!(
        inspect_document(&mut sandbox, Some(ViewFormat::Svg)),
        Err(InspectFailure::Refused(InspectRefusal::MalformedRequest))
    );
    send_document(&mut sandbox, b"plain words").expect("handed over");
    assert_eq!(
        inspect_document(&mut sandbox, None),
        Err(InspectFailure::Refused(InspectRefusal::NotAPicture))
    );
}

/// A reply of `lines`, each `(heading, text)`, under `count`.
fn reply(count: u32, lines: &[(u8, &[u8])]) -> Vec<u8> {
    let mut w = Writer::new();
    w.u8(REPLY_INSPECTED);
    w.u32(count);
    for (heading, text) in lines {
        w.u8(*heading);
        w.bytes(text);
    }
    w.finish()
}

#[test]
fn a_reply_that_breaks_its_bounds_is_not_believed() {
    let too_long = alloc::vec![b'x'; tairix_image::inspect::MAX_LINE_BYTES + 1];
    let too_many = u32::try_from(tairix_image::inspect::MAX_REPORT_LINES + 1).expect("small");
    for hostile in [
        reply(1, &[(0, b"a \x1b[2Jcontrol")]),
        reply(1, &[(0, b"\xff\xfe")]),
        reply(1, &[(2, b"a flag that is no flag")]),
        reply(1, &[(0, &too_long)]),
        reply(too_many, &[]),
        reply(2, &[(0, b"one line of two")]),
        {
            let mut trailing = reply(1, &[(0, b"fine")]);
            trailing.push(0);
            trailing
        },
    ] {
        let mut sandbox = scripted(hostile);
        assert_eq!(
            inspect_document(&mut sandbox, None),
            Err(InspectFailure::ReplyMalformed)
        );
    }
}

#[test]
fn a_worker_that_turns_hostile_at_the_report_is_contained() {
    let mut sandbox = tampering::<ImageRenderService>(OP_INSPECT, |mut reply| {
        reply.truncate(reply.len() / 2);
        reply
    });
    send_document(&mut sandbox, &png()).expect("handed over");
    assert_eq!(
        inspect_document(&mut sandbox, None),
        Err(InspectFailure::ReplyMalformed)
    );
    assert!(InspectFailure::ReplyMalformed
        .to_string()
        .contains("could not be believed"));
    assert!(InspectFailure::NoMemory.passing());
    assert!(!InspectFailure::Refused(InspectRefusal::NotAPicture).passing());
}
