use super::*;
use crate::fixture::{find, fonts, write, TEXT};
use alloc::vec::Vec;

fn request(form: Form, length: Option<u32>) -> Request {
    Request {
        at: TEXT,
        form,
        length,
        kern: false,
        right_to_left: false,
        limit: Limit::None,
        split: None,
        space: (0, 0),
        letter: (0, 0),
        matrix: None,
    }
}

/// The walk of `bytes` in the font `identifier` names at 12 points.
fn walked(identifier: &[u8], bytes: &[u8], request: &Request) -> Result<Walked, Fail> {
    let mut s = fonts();
    let handle = find(&mut s, identifier, 192);
    write(&mut s, TEXT, bytes);
    write(
        &mut s,
        TEXT + u32::try_from(bytes.len()).expect("short"),
        &[0, 0, 0],
    );
    let mut os = s.kernel.os();
    let font = resolve::font(&mut os, handle)?;
    walk(&mut os, request, font, Colours::INITIAL, &mut Measuring)
}

fn stop(identifier: &[u8], bytes: &[u8], request: &Request) -> (u32, (i32, i32)) {
    let stop = walked(identifier, bytes, request).expect("walked").stop;
    (stop.offset, stop.pen)
}

#[test]
fn a_utf8_sequence_the_length_cuts_is_not_taken() {
    let utf8 = b"Scan\\EUTF8";
    assert_eq!(
        stop(
            utf8,
            &[b'A', 0xE2, 0x82, 0xAC, b'B'],
            &request(Form::Bytes, None)
        ),
        (5, (16200, 0))
    );
    assert_eq!(
        stop(
            utf8,
            &[b'A', 0xE2, 0x82, 0xAC, b'B'],
            &request(Form::Bytes, Some(3))
        ),
        (1, (8400, 0))
    );
    assert_eq!(
        stop(utf8, &[b'A', 0xC0, 0x80, b'B'], &request(Form::Bytes, None)),
        (4, (16200, 0)),
        "overlong"
    );
    assert_eq!(
        stop(utf8, &[b'A', 0xFF, b'B'], &request(Form::Bytes, None)),
        (3, (16200, 0))
    );
    let plain = b"Scan";
    assert_eq!(
        stop(
            plain,
            &[b'A', 0xC2, 0xA3, b'B'],
            &request(Form::Bytes, None)
        ),
        (4, (16200, 0)),
        "bytes, not UTF-8"
    );
}

#[test]
fn wide_control_sequences_take_their_wide_parameters() {
    let halfwords = |units: &[u16]| {
        units
            .iter()
            .flat_map(|u| u.to_le_bytes())
            .collect::<Vec<u8>>()
    };
    let words = |units: &[u32]| {
        units
            .iter()
            .flat_map(|u| u.to_le_bytes())
            .collect::<Vec<u8>>()
    };
    let colours = walked(
        b"Scan",
        &halfwords(&[65, 19, 0x1100, 0x3322, 0x4400, 0x6655, 5, 66, 0]),
        &request(Form::Halfwords, None),
    )
    .expect("walked");
    assert_eq!(
        colours.stop.colours.rgb,
        Some((0x3322_1100, 0x6655_4400, 5))
    );
    assert_eq!(colours.stop.pen, (16200, 0));
    let colours = walked(
        b"Scan",
        &words(&[65, 19, 0x3322_1100, 0x6655_4400, 7, 66, 0]),
        &request(Form::Words, None),
    )
    .expect("walked");
    assert_eq!(
        colours.stop.colours.rgb,
        Some((0x3322_1100, 0x6655_4400, 7))
    );
    let comment = stop(
        b"Scan",
        &words(&[65, 21, 0x41, 0x42, 0x1F, 66, 0]),
        &request(Form::Words, None),
    );
    assert_eq!(comment, (24, (16200, 0)));
    let underline = stop(
        b"Scan",
        &halfwords(&[65, 25, 0xFFF0, 20, 66, 0]),
        &request(Form::Halfwords, None),
    );
    assert_eq!(underline, (10, (16200, 0)));
}

#[test]
fn a_matrix_starts_at_the_word_after_its_code() {
    let mut text = alloc::vec![b'A', b'B', 27, 0];
    for word in [32768i32, 0, 0, 32768] {
        text.extend_from_slice(&word.to_le_bytes());
    }
    text.push(b'A');
    let walked = walked(b"Scan", &text, &request(Form::Bytes, None)).expect("walked");
    assert_eq!((walked.stop.offset, walked.stop.pen), (21, (20400, 0)));
    assert_eq!(
        walked.stop.matrix,
        Some(Matrix([32768, 0, 0, 32768, 0, 0])),
        "four words, no translation"
    );
}

#[test]
fn a_font_change_to_no_font_is_an_error() {
    assert!(matches!(
        walked(b"Scan", &[b'A', 26, 77, b'B'], &request(Form::Bytes, None)),
        Err(Fail::Error(_))
    ));
    assert!(matches!(
        walked(b"Scan", &[b'A', 26, 0, b'B'], &request(Form::Bytes, None)),
        Err(Fail::Error(_))
    ));
}

#[test]
fn past_a_limit_the_walk_says_where_the_step_would_have_left_it() {
    let mut limited = request(Form::Bytes, None);
    limited.limit = Limit::Greater(10000, i32::MAX);
    let walked = walked(b"Scan", b"ABC", &limited).expect("walked");
    assert_eq!((walked.stop.offset, walked.stop.pen), (1, (8400, 0)));
    assert_eq!(
        walked.past.map(|p| (p.offset, p.pen)),
        Some((2, (16200, 0)))
    );
}
