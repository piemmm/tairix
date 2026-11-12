//! The document: bytes put in and taken out across the gap, lines found by
//! their ends, a search that wraps, and the room past the document resized.

use super::super::fixture::{empty, store, whole, Bytes};
use super::*;
use alloc::vec;

#[test]
fn bytes_go_in_and_out_wherever_the_gap_was_left() {
    let mut bytes = store(2000);
    let mut text = empty(&mut bytes);
    text.insert(0, b"world").unwrap();
    text.insert(0, b"hello ").unwrap();
    text.insert(11, b"!").unwrap();
    assert_eq!(whole(&mut text), b"hello world!");
    text.delete(5..11);
    assert_eq!(whole(&mut text), b"hello!");
    let long = vec![b'x'; 1500];
    text.insert(3, &long).unwrap();
    text.insert(0, b"<").unwrap();
    assert_eq!(text.len(), 1507, "the gap carried past a chunk's length");
    assert_eq!(text.read(0..5), b"<helx");
    assert_eq!(text.read(1500..1507), b"xxxxlo!");
    assert_eq!((text.byte(1506), text.byte(1507)), (Some(b'!'), None));
    assert_eq!(
        text.insert(0, &[0; 600]),
        Err(Full),
        "the store's room, not the gap's place, bounds it"
    );
    assert_eq!(text.len(), 1507, "nothing changed");
    let (gap, capacity) = (text.gap(), text.capacity());
    let mut again = Text::new(&mut bytes, gap, capacity);
    assert_eq!(
        again.read(0..4),
        b"<hel",
        "a layout kept and given back reads the same"
    );
    let mut small = Bytes(vec![1; 8]);
    assert_eq!(
        Text::new(&mut small, Gap { start: 5, end: 3 }, 8).len(),
        0,
        "a layout that does not fit is an empty document"
    );
    assert_eq!(
        Text::new(&mut small, Gap { start: 0, end: 8 }, 9).capacity(),
        8,
        "nor one larger than the store"
    );
}

fn document(bytes: &[u8]) -> Bytes {
    let mut store = store(4096);
    empty(&mut store).insert(0, bytes).unwrap();
    store
}

fn text_of(store: &mut Bytes, len: u32) -> Text<'_, Bytes> {
    Text::new(
        store,
        Gap {
            start: len,
            end: 4096,
        },
        4096,
    )
}

#[test]
fn lines_end_at_cr_lf_or_a_lone_lf_and_are_counted_between_places() {
    let mut store = document(b"one\r\ntwo\nthree\r\n");
    let mut text = text_of(&mut store, 16);
    assert_eq!(
        (text.line_start(6), text.line_end(6)),
        (5, 8),
        "two, from inside it"
    );
    assert_eq!(text.line_end(0), 3, "at the CR");
    assert_eq!(text.line_end(4), 3, "from the LF of its CR LF");
    assert_eq!(text.next_line(0), Some(5));
    assert_eq!(text.next_line(5), Some(9), "after a lone LF");
    assert_eq!(
        text.next_line(9),
        Some(16),
        "the empty line after the last CR LF"
    );
    assert_eq!(text.next_line(16), None, "the last");
    assert_eq!(text.previous_line(9), Some(5));
    assert_eq!(text.previous_line(2), None);
    assert_eq!(
        (
            text.breaks_in(0..16),
            text.breaks_in(5..10),
            text.breaks_in(9..99)
        ),
        (3, 1, 1)
    );
    let mut store = document(b"no end\r");
    assert_eq!(
        text_of(&mut store, 7).line_end(0),
        7,
        "a lone CR is a character, even at the very end"
    );
    let long: Vec<u8> = (0..700).map(|n| if n == 650 { LF } else { b'x' }).collect();
    let mut store = document(&long);
    let mut text = text_of(&mut store, 700);
    assert_eq!(
        (text.line_start(699), text.line_end(3)),
        (651, 650),
        "found a chunk away"
    );
}

#[test]
fn a_search_wraps_and_matches_case_and_words_as_asked() {
    let mut text_bytes = b"Alpha beta alphabet ALPHA".to_vec();
    text_bytes.extend(vec![b'.'; 1200]);
    text_bytes.extend_from_slice(b" alpha");
    let mut store = document(&text_bytes);
    let len = u32::try_from(text_bytes.len()).unwrap();
    let mut text = text_of(&mut store, len);
    assert_eq!(text.find(b"alpha", 0, false, false), Some(0));
    assert_eq!(
        text.find(b"alpha", 1, false, false),
        Some(11),
        "inside alphabet"
    );
    assert_eq!(
        text.find(b"alpha", 1, false, true),
        Some(20),
        "a whole word"
    );
    assert_eq!(
        text.find(b"alpha", 1, true, true),
        Some(len - 5),
        "the exact case, past a chunk"
    );
    assert_eq!(
        text.find(b"alpha", len - 4, true, false),
        Some(11),
        "from the end, round to the start"
    );
    assert_eq!(text.find(b"gamma", 0, false, false), None);
    assert_eq!(text.find(b"", 0, false, false), None);
    assert_eq!(text.find_in(b"alpha", 1..20, false, false), Some(11));
    assert_eq!(
        text.find_in(b"alpha", 12..20, false, false),
        None,
        "bounded, never wrapping"
    );
}

#[test]
fn the_room_past_the_document_is_made_and_given_back() {
    let mut bytes = store(64);
    let mut text = empty(&mut bytes);
    text.insert(0, b"0123456789").unwrap();
    text.insert(4, b"-").unwrap();
    assert_eq!(text.resize(40), Ok(()));
    assert_eq!(
        (whole(&mut text), text.room()),
        (b"0123-456789".to_vec(), 29),
        "the bytes past the gap carried down"
    );
    text.write_end(8, b"clipping");
    let mut clip = [0u8; 8];
    text.read_end(8, &mut clip);
    assert_eq!(&clip, b"clipping");
    text.write_end(30, b"x");
    assert_eq!(text.resize(10), Err(Full), "the document would not fit");
    assert_eq!(text.resize(65), Err(Full), "nor the store hold it");
    assert_eq!(text.resize(11), Ok(()));
    assert_eq!(
        (whole(&mut text), text.room()),
        (b"0123-456789".to_vec(), 0)
    );
    assert_eq!(text.insert(0, b"!"), Err(Full));
    assert_eq!(text.resize(64 - 8), Ok(()));
    assert_eq!(whole(&mut text), b"0123-456789", "carried back up");
    text.read_end(8, &mut clip);
    assert_eq!(&clip, b"clipping", "the end untouched");
    text.write_end(9, b"over");
    text.read_end(8, &mut clip);
    assert_eq!(&clip, b"clipping", "nothing written into the document");
}
