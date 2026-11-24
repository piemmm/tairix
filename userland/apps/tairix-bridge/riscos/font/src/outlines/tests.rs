use alloc::collections::BTreeMap;
use alloc::vec;

use super::*;
use crate::build::{Character, Font};

fn square(size: i32) -> Vec<Element> {
    vec![
        Element::Move(Point::new(0, 0)),
        Element::Line(Point::new(size, 0)),
        Element::Line(Point::new(size, size)),
        Element::Line(Point::new(0, size)),
    ]
}

fn font() -> Font {
    let mut characters = BTreeMap::new();
    characters.insert(
        b'A',
        Character {
            advance: 700,
            fill: square(100),
            ..Character::default()
        },
    );
    characters.insert(
        b'B',
        Character {
            advance: 600,
            fill: vec![
                Element::Move(Point::new(0, 0)),
                Element::Curve(Point::new(10, 50), Point::new(50, 90), Point::new(100, 100)),
                Element::Line(Point::new(0, 100)),
            ],
            ..Character::default()
        },
    );
    characters.insert(
        b'W',
        Character {
            advance: 1000,
            fill: square(900),
            ..Character::default()
        },
    );
    characters.insert(
        b'S',
        Character {
            advance: 300,
            fill: square(20),
            strokes: vec![
                Element::Move(Point::new(5, 5)),
                Element::Line(Point::new(5, 90)),
            ],
            inclusions: vec![(b'A', Point::new(200, 0))],
        },
    );
    characters.insert(
        200,
        Character {
            advance: 500,
            fill: square(10),
            ..Character::default()
        },
    );
    Font {
        name: b"Probe".to_vec(),
        design_size: 1000,
        characters,
        kerns: vec![(b'A', b'B', -50)],
    }
}

fn outlines(bytes: Vec<u8>) -> Outlines {
    match File::read(bytes) {
        Ok(File::Outlines(outlines)) => outlines,
        other => panic!("{other:?}"),
    }
}

#[test]
fn each_character_is_its_moves_lines_and_curves_in_design_units() {
    let made = font();
    let file = outlines(made.outlines());
    assert_eq!(file.design_size, 1000);
    let a = file
        .glyph(u32::from(b'A'))
        .expect("sound")
        .expect("defined");
    assert_eq!(a.fill, square(100));
    assert_eq!(a.bbox, Some([0, 0, 101, 101]));
    let b = file
        .glyph(u32::from(b'B'))
        .expect("sound")
        .expect("defined");
    assert_eq!(
        b.fill[1],
        Element::Curve(Point::new(10, 50), Point::new(50, 90), Point::new(100, 100))
    );
    let w = file
        .glyph(u32::from(b'W'))
        .expect("sound")
        .expect("defined");
    assert_eq!(w.fill, square(900), "twelve-bit coordinates");
    assert_eq!(
        file.glyph(200).expect("sound").map(|g| g.fill),
        Some(square(10)),
        "a later chunk"
    );
    assert_eq!(file.glyph(u32::from(b'Z')), Ok(None));
    assert_eq!(file.glyph(0), Ok(None));
    assert_eq!(file.glyph(9999), Ok(None));
}

#[test]
fn strokes_and_inclusions_join_the_character() {
    let file = outlines(font().outlines());
    let s = file
        .glyph(u32::from(b'S'))
        .expect("sound")
        .expect("defined");
    assert_eq!(
        s.strokes,
        vec![
            Element::Move(Point::new(5, 5)),
            Element::Line(Point::new(5, 90))
        ]
    );
    assert_eq!(s.fill.len(), 8, "its square and A's, moved");
    assert_eq!(s.fill[4], Element::Move(Point::new(200, 0)));
    assert_eq!(s.bbox, Some([0, 0, 301, 101]));
}

#[test]
fn a_short_file_names_another_font() {
    match File::read(b"Homerton.Medium\x0d".to_vec()) {
        Ok(File::Redirect(name)) => assert_eq!(name, b"Homerton.Medium"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_file_that_does_not_hold_together_is_refused() {
    let good = font().outlines();
    for cut in REDIRECT_BELOW..good.len() {
        if let Ok(File::Outlines(file)) = File::read(good[..cut].to_vec()) {
            for code in 0..256 {
                let _ = file.glyph(code);
            }
        }
    }
    let mut bad_magic = good.clone();
    bad_magic[0] = b'X';
    assert!(File::read(bad_magic).is_err());
    let mut bad_version = good.clone();
    bad_version[VERSION] = 3;
    assert!(File::read(bad_version).is_err());
}

#[test]
fn every_byte_changed_to_every_value_is_read_without_fault() {
    let good = font().outlines();
    for at in 0..good.len() {
        for value in [0, 1, 0x7F, 0x80, 0xFF] {
            let mut bytes = good.clone();
            bytes[at] = value;
            if let Ok(File::Outlines(file)) = File::read(bytes) {
                for code in [
                    0,
                    u32::from(b'A'),
                    u32::from(b'B'),
                    u32::from(b'S'),
                    u32::from(b'W'),
                    200,
                ] {
                    let _ = file.glyph(code);
                }
            }
        }
    }
}

#[test]
fn a_composite_that_includes_itself_is_refused_not_followed() {
    let mut made = font();
    made.characters.insert(
        b'R',
        Character {
            advance: 100,
            fill: square(5),
            inclusions: vec![(b'R', Point::new(1, 1))],
            ..Character::default()
        },
    );
    let file = outlines(made.outlines());
    assert_eq!(file.glyph(u32::from(b'R')), Err(Malformed));
}

#[test]
fn a_path_of_more_elements_than_any_glyph_holds_is_malformed() {
    let file_of = |elements: usize| {
        let mut fill = vec![Element::Move(Point::new(0, 0))];
        fill.extend((1..elements).map(|i| Element::Line(Point::new(i32::from(i % 2 == 0), 1))));
        let mut made = font();
        made.characters.insert(
            b'L',
            Character {
                advance: 100,
                fill,
                ..Character::default()
            },
        );
        outlines(made.outlines())
    };
    let longest = file_of(ELEMENTS_MOST).glyph(u32::from(b'L'));
    assert_eq!(
        longest.map(|g| g.map(|g| g.fill.len())),
        Ok(Some(ELEMENTS_MOST))
    );
    assert_eq!(
        file_of(ELEMENTS_MOST + 1).glyph(u32::from(b'L')),
        Err(Malformed)
    );
}

#[test]
fn a_file_of_more_chunks_than_its_metrics_can_index_is_refused() {
    let with_chunks = |count: usize| {
        let mut bytes = font().outlines();
        let table = bytes.len();
        for _ in 0..=count {
            bytes.extend_from_slice(&u32::try_from(table).expect("short").to_le_bytes());
        }
        let words = [table, count].map(|word| u32::try_from(word).expect("short").to_le_bytes());
        bytes[CHUNK_TABLE..CHUNK_TABLE + 4].copy_from_slice(&words[0]);
        bytes[CHUNK_COUNT..CHUNK_COUNT + 4].copy_from_slice(&words[1]);
        File::read(bytes)
    };
    assert!(matches!(with_chunks(CHUNKS_MOST), Ok(File::Outlines(_))));
    assert!(with_chunks(CHUNKS_MOST + 1).is_err());
}

/// `nibbles` two to a byte, the first of each two low.
fn nibbled(nibbles: &[u8]) -> Vec<u8> {
    nibbles
        .chunks(2)
        .map(|two| two[0] | two.get(1).map_or(0, |high| high << 4))
        .collect()
}

fn repeat() -> u8 {
    u8::try_from(REPEAT).expect("a nibble")
}

/// The bitmap `nibbles` pack at packing value 8, black first, `width` by
/// `rows`.
fn unpacked(nibbles: &[u8], width: usize, rows: usize) -> Result<Vec<u8>, Undecoded> {
    let mut levels = vec![0; width * rows];
    packed(&nibbled(nibbles), 8, true, width, &mut levels).map(|()| levels)
}

#[test]
fn a_long_run_of_more_zeros_than_any_bitmap_needs_is_malformed() {
    let longest = [0, 0, 0, 0, 0, 0, 0xF, 0xF, 0xF, 0xF, 0xF, 0xF, 0xF];
    assert_eq!(unpacked(&longest, 2, 3), Ok(vec![WHOLE; 6]), "past its end");
    for zeros in [7, 8, 9] {
        let mut longer = vec![0; zeros];
        longer.extend([1; 12]);
        assert_eq!(unpacked(&longer, 2, 3), Err(Undecoded::Malformed));
    }
}

#[test]
fn a_repeat_of_a_repeat_is_malformed_however_many_follow() {
    assert_eq!(
        unpacked(&vec![repeat(); 200_000], 4, 4),
        Err(Undecoded::Malformed)
    );
}

#[test]
fn a_row_repeated_past_the_last_row_fills_to_it_and_stops() {
    let mut nibbles = vec![repeat(), 0, 0, 0, 0, 0, 0];
    nibbles.extend([0xF; 7]);
    nibbles.push(2);
    assert_eq!(unpacked(&nibbles, 2, 3), Ok(vec![WHOLE; 6]));
}
