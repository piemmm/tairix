//! Unit tests for the Direct TCP framing and compound splitting.

use alloc::vec;
use alloc::vec::Vec;

use super::{messages, prefix, Framer, MAX_COMPOUND, MAX_FRAME};
use crate::header::HEADER_LEN;
use crate::SmbError;

fn framed(body: &[u8]) -> Vec<u8> {
    let mut out = prefix(body.len()).expect("fits").to_vec();
    out.extend_from_slice(body);
    out
}

#[test]
fn a_frame_ending_the_buffer_is_handed_over_in_place() {
    let body: Vec<u8> = (0..=255).collect();
    let mut framer = Framer::new();
    framer.push(&framed(&body)).expect("pushes");
    let buffered = framer.buf.as_ptr();
    let (buf, at) = framer
        .next_frame()
        .expect("frames")
        .expect("a frame")
        .into_parts();
    assert_eq!(buf.as_ptr(), buffered, "not copied");
    assert_eq!(buf[at..], body[..]);
    assert!(framer.buf.is_empty());
    assert!(framer.next_frame().expect("frames").is_none());
}

#[test]
fn of_a_frame_and_what_follows_it_the_shorter_is_copied() {
    let short = [1u8; 8];
    let long = [2u8; 64];

    let mut framer = Framer::new();
    framer
        .push(&[framed(&short), framed(&long)].concat())
        .expect("pushes");
    let buffered = framer.buf.as_ptr();
    let (buf, at) = framer
        .next_frame()
        .expect("frames")
        .expect("short")
        .into_parts();
    assert_ne!(buf.as_ptr(), buffered, "the short frame is copied out");
    assert_eq!(buf[at..], short);
    let (buf, at) = framer
        .next_frame()
        .expect("frames")
        .expect("long")
        .into_parts();
    assert_eq!(buf.as_ptr(), buffered, "the long one leaves in place");
    assert_eq!(buf[at..], long);

    let mut framer = Framer::new();
    framer
        .push(&[framed(&long), framed(&short)].concat())
        .expect("pushes");
    let buffered = framer.buf.as_ptr();
    let (buf, at) = framer
        .next_frame()
        .expect("frames")
        .expect("long")
        .into_parts();
    assert_eq!(buf.as_ptr(), buffered, "the long frame leaves in place");
    assert_eq!(buf[at..], long);
    assert_eq!(framer.buf, framed(&short), "only the short tail was copied");
    let (buf, at) = framer
        .next_frame()
        .expect("frames")
        .expect("short")
        .into_parts();
    assert_eq!(buf[at..], short);
}

#[test]
fn a_frame_waits_for_its_last_byte_with_its_room_reserved() {
    let body = vec![7u8; 1 << 20];
    let whole = framed(&body);
    let mut framer = Framer::new();
    framer.push(&whole[..100]).expect("pushes");
    assert!(framer.next_frame().expect("frames").is_none());
    assert!(
        framer.buf.capacity() >= whole.len(),
        "room for the whole frame"
    );
    let reserved = framer.buf.as_ptr();
    framer.push(&whole[100..whole.len() - 1]).expect("pushes");
    assert!(framer.next_frame().expect("frames").is_none());
    framer.push(&whole[whole.len() - 1..]).expect("pushes");
    let (buf, at) = framer
        .next_frame()
        .expect("frames")
        .expect("a frame")
        .into_parts();
    assert_eq!(buf.as_ptr(), reserved, "never regrown on the way in");
    assert_eq!(buf[at..], body[..]);
}

#[test]
fn a_bad_prefix_or_an_oversized_frame_is_refused() {
    let mut framer = Framer::new();
    framer.push(&[1, 0, 0, 4]).expect("pushes");
    assert!(matches!(framer.next_frame(), Err(SmbError::Malformed)));

    let mut framer = Framer::new();
    let over = u32::try_from(MAX_FRAME + 1).expect("fits").to_be_bytes();
    framer.push(&over).expect("pushes");
    assert!(matches!(framer.next_frame(), Err(SmbError::TooLarge)));

    let mut framer = Framer::new();
    framer
        .push(&prefix(MAX_FRAME).expect("the bound itself fits"))
        .expect("pushes");
    assert!(framer.next_frame().expect("frames").is_none());

    assert_eq!(prefix(MAX_FRAME + 1), Err(SmbError::TooLarge));
}

/// A stand-in message of `len` bytes whose `NextCommand` is `next`.
fn message(len: usize, next: u32) -> Vec<u8> {
    let mut out = vec![0u8; len];
    out[20..24].copy_from_slice(&next.to_le_bytes());
    out
}

#[test]
fn a_compound_splits_where_each_next_command_says() {
    let frame = [message(72, 72), message(80, 80), message(70, 0)].concat();
    assert_eq!(messages(&frame), Ok(vec![0..72, 72..152, 152..222]));
    let whole = 0..HEADER_LEN;
    assert_eq!(messages(&message(HEADER_LEN, 0)), Ok(vec![whole]));
}

#[test]
fn a_next_command_breaking_the_rules_is_refused() {
    for next in [68, 8, 40, 200] {
        let frame = [message(72, next), message(128, 0)].concat();
        assert_eq!(messages(&frame), Err(SmbError::Malformed), "{next}");
    }
    assert_eq!(messages(&[0; 23]), Err(SmbError::Malformed));
    let past_the_end = [message(72, 72)].concat();
    assert_eq!(messages(&past_the_end), Err(SmbError::Malformed));
}

#[test]
fn a_compound_past_its_bound_is_refused() {
    let mut frame: Vec<u8> = (0..MAX_COMPOUND).flat_map(|_| message(72, 72)).collect();
    frame.extend(message(72, 0));
    assert_eq!(messages(&frame), Err(SmbError::TooLarge));
    let at_the_bound: Vec<u8> = (1..MAX_COMPOUND)
        .flat_map(|_| message(72, 72))
        .chain(message(72, 0))
        .collect();
    assert_eq!(
        messages(&at_the_bound).map(|ranges| ranges.len()),
        Ok(MAX_COMPOUND)
    );
}
