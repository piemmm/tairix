use alloc::vec::Vec;

use tairix_drawing::riscos::format::TEXT_AREA;

use super::*;

/// A text area object's bytes.
pub(super) use crate::fixture::text_area as object;

pub(super) fn area(bytes: &[u8]) -> Area<'_> {
    Area::of(Object {
        kind: TEXT_AREA,
        at: 0,
        bytes,
    })
}

#[test]
fn an_area_gives_its_colours_text_and_columns_in_order() {
    let bytes = object(
        [0, 0, 64000, 32000],
        &[[0, 0, 32000, 32000], [32000, 0, 64000, 32000]],
        0xFF00,
        0xFFFF_FF00,
        b"\\! 1\nab",
    );
    let area = area(&bytes);
    assert_eq!((area.foreground, area.background), (0xFF00, 0xFFFF_FF00));
    assert_eq!(area.text, b"\\! 1\nab");
    let columns: Vec<_> = area.columns().collect();
    assert_eq!(columns, [[0, 0, 32000, 32000], [32000, 0, 64000, 32000]]);
}

#[test]
fn an_area_of_no_columns_has_one_of_no_width_at_its_left_as_risc_os_paints_it() {
    let bytes = object([64_000, 64_000, 128_000, 128_000], &[], 0, 0, b"\\! 1\n");
    let columns: Vec<_> = area(&bytes).columns().collect();
    assert_eq!(columns, [[64_000, 64_000, 64_000, 128_000]]);
}

#[test]
fn a_column_claiming_more_than_the_object_holds_ends_the_columns() {
    let mut bytes = object(
        [0, 0, 100, 100],
        &[[0, 0, 100, 100], [1, 2, 3, 4]],
        0,
        0,
        b"x",
    );
    bytes[OBJECT_HEADER + 4..OBJECT_HEADER + 8].copy_from_slice(&u32::MAX.to_le_bytes());
    let area = area(&bytes);
    assert_eq!(area.columns().collect::<Vec<_>>(), [[0, 0, 100, 100]]);
    assert!(
        area.text.is_empty(),
        "no text past a column that runs off the object"
    );
}

#[test]
fn the_fonts_a_text_defines_are_declared_and_a_malformed_command_refuses_it() {
    let declare = |text: &'static [u8]| {
        let bytes = object([0; 4], &[[0; 4]], 0, 0, text);
        area(&bytes)
            .fonts()
            .map(|names| names.iter().map(|n| n.to_vec()).collect::<Vec<_>>())
    };
    assert_eq!(declare(b"\\! 1\nHello"), Ok(Vec::new()));
    assert_eq!(
        declare(b"Hello"),
        Ok(Vec::new()),
        "no version needed to declare"
    );
    assert_eq!(declare(b"\\! 2\nHello"), Err(Refusal::Version(2)));
    assert_eq!(
        declare(b"\\! 1\n\\9Hello"),
        Ok(Vec::new()),
        "an undefined font is no matter"
    );
    assert_eq!(declare(b"\\! 1\n\\QHello"), Err(Refusal::Command(b'Q')));
    // RISC OS 5.30 refused each of these declaring as well as painting.
    assert_eq!(declare(b"\\! 1\n\\L 10.5/x"), Err(Refusal::Command(b'L')));
    assert_eq!(
        declare(b"\\! 1\n\\F 1 Lay 10.5/x"),
        Err(Refusal::Command(b'F'))
    );
    assert_eq!(
        declare(b"\\! 1\n\\F 3 Homerton.Medium 12\n\\3Hi\\F12Trinity.Bold 10 12/"),
        Ok(alloc::vec![
            b"Homerton.Medium".to_vec(),
            b"Trinity.Bold".to_vec()
        ])
    );
}
