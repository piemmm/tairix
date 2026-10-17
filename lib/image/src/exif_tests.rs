use alloc::string::String;
use alloc::vec::Vec;

use super::report;
use crate::ifd::{ASCII, LONG, RATIONAL, SHORT, UNDEFINED};
use crate::inspect::Builder;

/// One entry of a block being built: its tag, type, count and value bytes.
pub(crate) struct Field {
    tag: u16,
    kind: u16,
    count: u32,
    data: Vec<u8>,
}

fn field(tag: u16, kind: u16, count: u32, data: &[u8]) -> Field {
    Field {
        tag,
        kind,
        count,
        data: data.to_vec(),
    }
}

fn ascii(tag: u16, text: &str) -> Field {
    let mut data = text.as_bytes().to_vec();
    data.push(0);
    field(tag, ASCII, u32::try_from(data.len()).expect("short"), &data)
}

fn short(tag: u16, value: u16) -> Field {
    field(tag, SHORT, 1, &value.to_be_bytes())
}

fn long(tag: u16, value: u32) -> Field {
    field(tag, LONG, 1, &value.to_be_bytes())
}

fn rationals(tag: u16, values: &[(u32, u32)]) -> Field {
    let mut data = Vec::new();
    for (numerator, denominator) in values {
        data.extend_from_slice(&numerator.to_be_bytes());
        data.extend_from_slice(&denominator.to_be_bytes());
    }
    field(
        tag,
        RATIONAL,
        u32::try_from(values.len()).expect("few"),
        &data,
    )
}

/// Bytes a directory of `fields` occupies with its out-of-line values.
fn size(fields: &[Field]) -> usize {
    2 + fields.len() * 12
        + 4
        + fields
            .iter()
            .filter(|f| f.data.len() > 4)
            .map(|f| f.data.len())
            .sum::<usize>()
}

/// The directory of `fields` laid out at `at`, linking to `next`.
fn directory(fields: &[Field], at: usize, next: u32) -> Vec<u8> {
    let mut entries = Vec::new();
    let mut data = Vec::new();
    let mut spill = at + 2 + fields.len() * 12 + 4;
    entries.extend_from_slice(&u16::try_from(fields.len()).expect("few").to_be_bytes());
    for f in fields {
        entries.extend_from_slice(&f.tag.to_be_bytes());
        entries.extend_from_slice(&f.kind.to_be_bytes());
        entries.extend_from_slice(&f.count.to_be_bytes());
        if f.data.len() <= 4 {
            let mut inline = [0; 4];
            inline[..f.data.len()].copy_from_slice(&f.data);
            entries.extend_from_slice(&inline);
        } else {
            entries.extend_from_slice(&u32::try_from(spill).expect("small").to_be_bytes());
            data.extend_from_slice(&f.data);
            spill += f.data.len();
        }
    }
    entries.extend_from_slice(&next.to_be_bytes());
    entries.extend_from_slice(&data);
    entries
}

/// A camera's EXIF block: the make and orientation, an exposure, and where
/// the picture was taken.
pub(crate) fn camera_block() -> Vec<u8> {
    let exif_fields = [
        rationals(0x829A, &[(1, 125)]),
        rationals(0x829D, &[(18, 10)]),
        short(0x8827, 200),
        field(0x9000, UNDEFINED, 4, b"0232"),
        short(0x9209, 0x19),
        rationals(0x920A, &[(42, 10)]),
    ];
    let gps_fields = [
        ascii(0x0001, "N"),
        rationals(0x0002, &[(51, 1), (30, 1), (2604, 100)]),
    ];
    let main_len = size(&[
        ascii(0x010F, "Acme"),
        short(0x0112, 6),
        long(0x8769, 0),
        long(0x8825, 0),
    ]);
    let exif_at = 8 + main_len;
    let gps_at = exif_at + size(&exif_fields);
    let main_fields = [
        ascii(0x010F, "Acme"),
        short(0x0112, 6),
        long(0x8769, u32::try_from(exif_at).expect("small")),
        long(0x8825, u32::try_from(gps_at).expect("small")),
    ];
    let mut block = b"MM\0\x2a\0\0\0\x08".to_vec();
    block.extend_from_slice(&directory(&main_fields, 8, 0));
    block.extend_from_slice(&directory(&exif_fields, exif_at, 0));
    block.extend_from_slice(&directory(&gps_fields, gps_at, 0));
    block
}

fn lines(block: &[u8]) -> Vec<String> {
    let mut out = Builder::default();
    report(block, 0, &mut out);
    out.finish()
        .into_lines()
        .into_iter()
        .map(|line| line.text)
        .collect()
}

fn holds(lines: &[String], needle: &str) -> bool {
    lines.iter().any(|line| line.contains(needle))
}

#[test]
fn every_directory_is_read_and_each_tag_named_with_its_value_in_its_unit() {
    let shown = lines(&camera_block());
    for needle in [
        "Big-endian byte order",
        "Image directory at 0x8: 4 entries",
        "Make: \u{201c}Acme\u{201d}",
        "Orientation: 6 (turned a quarter clockwise)",
        "Exif directory",
        "ExposureTime: 1/125 s",
        "FNumber: f/1.8 (18/10)",
        "PhotographicSensitivity: 200 (ISO 200)",
        "ExifVersion: \u{201c}0232\u{201d}",
        "Flash: 25 (fired, auto)",
        "FocalLength: 4.2 mm (42/10)",
        "GPS directory",
        "GPSLatitudeRef: \u{201c}N\u{201d}",
        "GPSLatitude: 51\u{b0} 30\u{2032} 26.04\u{2033}",
    ] {
        assert!(holds(&shown, needle), "{needle}: {shown:#?}");
    }
    let image = shown
        .iter()
        .position(|line| line.starts_with("Image directory"))
        .expect("image");
    let exif = shown
        .iter()
        .position(|line| line.starts_with("Exif directory"))
        .expect("exif");
    let gps = shown
        .iter()
        .position(|line| line.starts_with("GPS directory"))
        .expect("gps");
    assert!(
        image < exif && exif < gps,
        "children under the picture, in order"
    );
}

#[test]
fn a_directory_linked_to_itself_is_read_once() {
    let fields = [short(0x0112, 1)];
    let mut block = b"MM\0\x2a\0\0\0\x08".to_vec();
    block.extend_from_slice(&directory(&fields, 8, 8));
    let shown = lines(&block);
    assert_eq!(
        shown
            .iter()
            .filter(|line| line.contains("directory at 0x8:"))
            .count(),
        1
    );
    assert!(holds(&shown, "not read twice"), "{shown:#?}");
}

#[test]
fn a_cut_entry_list_and_values_past_the_end_are_said_to_be() {
    let fields = [short(0x0112, 1), ascii(0x010E, "a description")];
    let mut block = b"MM\0\x2a\0\0\0\x08".to_vec();
    let mut whole = directory(&fields, 8, 0);
    // Claim a third entry the block does not hold, and lose the text.
    whole[1] = 3;
    whole.truncate(2 + 2 * 12 + 4);
    block.extend_from_slice(&whole);
    let shown = lines(&block);
    assert!(holds(&shown, "Orientation: 1 (upright)"), "{shown:#?}");
    assert!(
        holds(&shown, "ImageDescription: 14 values lying past the end"),
        "{shown:#?}"
    );
    assert!(holds(&shown, "1 entries lie past the end"), "{shown:#?}");
}

#[test]
fn a_block_with_no_tiff_header_is_said_not_to_be_one() {
    assert!(holds(&lines(b"not exif at all"), "no TIFF header"));
    assert!(holds(&lines(b"MM\0\x2b\0\0\0\x08"), "no TIFF header"));
}
