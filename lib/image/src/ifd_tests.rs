use super::{entry, type_size, Endian, Ifd, ENTRY_LEN, LONG, RATIONAL, SHORT};
use crate::DecodeError;

/// A big-endian directory of `entries` at offset 8, after a header, its
/// next-directory link zero.
fn directory(entries: &[[u8; ENTRY_LEN]]) -> alloc::vec::Vec<u8> {
    let mut file = alloc::vec![b'M', b'M', 0, 42, 0, 0, 0, 8];
    file.extend_from_slice(&u16::try_from(entries.len()).expect("few").to_be_bytes());
    for raw in entries {
        file.extend_from_slice(raw);
    }
    file.extend_from_slice(&[0; 4]);
    file
}

/// An entry of `tag`, `kind` and `count` whose value field holds `value`.
fn raw(tag: u16, kind: u16, count: u32, value: [u8; 4]) -> [u8; ENTRY_LEN] {
    let mut raw = [0; ENTRY_LEN];
    raw[..2].copy_from_slice(&tag.to_be_bytes());
    raw[2..4].copy_from_slice(&kind.to_be_bytes());
    raw[4..8].copy_from_slice(&count.to_be_bytes());
    raw[8..].copy_from_slice(&value);
    raw
}

#[test]
fn the_byte_order_mark_names_the_order_and_nothing_else_does() {
    assert_eq!(Endian::of(b"II*\0"), Some(Endian::Little));
    assert_eq!(Endian::of(b"MM\0*"), Some(Endian::Big));
    assert_eq!(Endian::of(b"IM"), None);
    assert_eq!(Endian::of(b"I"), None);
    assert_eq!(Endian::Big.u64(&[0, 0, 0, 0, 0, 0, 1, 2], 0), Some(0x0102));
    assert_eq!(
        Endian::Little.u64(&[2, 1, 0, 0, 0, 0, 0, 0], 0),
        Some(0x0102)
    );
    assert_eq!(Endian::Little.u64(&[0; 7], 0), None);
}

#[test]
fn a_value_up_to_four_bytes_lies_in_the_entry_and_a_longer_one_at_its_offset() {
    let file = directory(&[
        raw(274, SHORT, 1, [0, 6, 0, 0]),
        raw(282, RATIONAL, 1, [0, 0, 0, 0x30]),
    ]);
    let mut file = file;
    file.resize(0x30, 0);
    file.extend_from_slice(&[0, 0, 0, 72, 0, 0, 0, 1]);
    let first = entry(&file, Endian::Big, 10).expect("an entry");
    assert_eq!((first.tag, first.kind, first.count), (274, SHORT, 1));
    assert_eq!(first.values.map(|values| values.at), Some(18));
    let second = entry(&file, Endian::Big, 10 + ENTRY_LEN).expect("an entry");
    assert_eq!(second.values.map(|values| values.at), Some(0x30));
    let (ifd, next) = Ifd::read(&file, Endian::Big, 8).expect("a directory");
    assert_eq!(next, 0);
    assert_eq!(ifd.value(274, 1), Ok(6));
    let resolution = ifd.field(282).expect("readable").expect("present");
    assert_eq!(ifd.rational(&resolution, 0), Ok((72, 1)));
}

#[test]
fn values_past_the_end_are_unreadable_and_a_strict_read_refuses_them() {
    let file = directory(&[raw(273, LONG, 4, [0, 0, 0xff, 0])]);
    let loose = entry(&file, Endian::Big, 10).expect("the entry itself is whole");
    assert!(loose.values.is_none(), "its values run past the end");
    let (ifd, _) = Ifd::read(&file, Endian::Big, 8).expect("a directory");
    assert_eq!(ifd.field(273).err(), Some(DecodeError::TiffTruncated));
    assert!(entry(&file, Endian::Big, file.len() - 4).is_none());
}

#[test]
fn an_unknown_type_has_no_reading_and_is_passed_over() {
    assert_eq!(type_size(0), None);
    assert_eq!(type_size(13), None);
    let file = directory(&[raw(700, 99, 1, [0; 4])]);
    let read = entry(&file, Endian::Big, 10).expect("an entry");
    assert!(read.values.is_none());
    let (ifd, _) = Ifd::read(&file, Endian::Big, 8).expect("a directory");
    assert!(matches!(ifd.field(700), Ok(None)));
}
