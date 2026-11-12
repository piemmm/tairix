use super::*;

fn raw_short() -> [u8; ENTRY] {
    let mut raw = [0u8; ENTRY];
    raw[..11].copy_from_slice(b"README  TXT");
    raw[11] = attr::ARCHIVE;
    raw[12] = 0x18;
    raw[13] = 137;
    raw[14..16].copy_from_slice(&0x6A5Bu16.to_le_bytes());
    raw[16..18].copy_from_slice(&0x5B4Eu16.to_le_bytes());
    raw[18..20].copy_from_slice(&0x5B4Fu16.to_le_bytes());
    raw[20..22].copy_from_slice(&0x0012u16.to_le_bytes());
    raw[22..24].copy_from_slice(&0x6A60u16.to_le_bytes());
    raw[24..26].copy_from_slice(&0x5B50u16.to_le_bytes());
    raw[26..28].copy_from_slice(&0x3456u16.to_le_bytes());
    raw[28..32].copy_from_slice(&123_456u32.to_le_bytes());
    raw
}

#[test]
fn a_short_entry_decodes_every_field_and_encodes_back_byte_for_byte() {
    let raw = raw_short();
    let entry = ShortEntry::decode(&raw);
    assert_eq!(&entry.name, b"README  TXT");
    assert_eq!((entry.attributes, entry.case), (attr::ARCHIVE, 0x18));
    assert_eq!(entry.cluster, 0x0012_3456, "high word at 20, low at 26");
    assert_eq!(entry.size, 123_456);
    assert_eq!(
        entry.stamps.created,
        crate::stamp::Stamp {
            date: 0x5B4E,
            time: 0x6A5B,
            centis: 137
        }
    );
    assert_eq!(
        (
            entry.stamps.accessed,
            entry.stamps.written.date,
            entry.stamps.written.time
        ),
        (0x5B4F, 0x5B50, 0x6A60)
    );
    assert_eq!(entry.encode(), raw);
    assert!(!entry.is_directory());
}

#[test]
fn only_a_directorys_two_links_open_with_a_dot() {
    for (spelt, name) in [(&b"."[..], DOT), (&b".."[..], DOTDOT)] {
        assert_eq!(crate::name::field(spelt), Some(name));
        let link = ShortEntry {
            name,
            attributes: attr::DIRECTORY,
            ..ShortEntry::default()
        };
        assert!(link.is_dot());
    }
    assert!(!ShortEntry::decode(&raw_short()).is_dot());
}

#[test]
fn a_slot_is_told_by_its_first_byte_and_attributes() {
    let mut raw = raw_short();
    assert_eq!(slot(&raw), Slot::Short);
    raw[11] = attr::VOLUME_ID | attr::ARCHIVE;
    assert_eq!(slot(&raw), Slot::Label);
    raw[11] = attr::LONG_NAME;
    assert_eq!(slot(&raw), Slot::Long);
    raw[11] = 0x40 | attr::LONG_NAME;
    assert_eq!(slot(&raw), Slot::Long, "the reserved bits ignored");
    raw[0] = DELETED;
    assert_eq!(slot(&raw), Slot::Deleted);
    raw[0] = END;
    assert_eq!(slot(&raw), Slot::End);
}

#[test]
fn the_checksum_rotates_and_adds_over_the_stored_bytes() {
    assert_eq!(checksum(&[b'A'; 11]), 0x1C, "worked by hand");
    assert_ne!(checksum(b"README  TXT"), checksum(b"README  TXU"));
}

/// The UTF-16 of `text`.
fn units(text: &str, out: &mut [u16; LONG_NAME_UNITS]) -> usize {
    encode_utf16(text.as_bytes(), out).unwrap()
}

#[test]
fn long_entries_list_the_last_place_first_and_pad_after_one_terminator() {
    let mut buffer = [0u16; LONG_NAME_UNITS];
    let len = units("Fifteen chars!!", &mut buffer);
    let sum = checksum(b"FIFTEE~1   ");
    let mut entries = long_entries(&buffer[..len], sum);
    let first = entries.next().unwrap();
    let second = entries.next().unwrap();
    assert!(entries.next().is_none());
    assert_eq!((first[0], second[0]), (LAST | 2, 1));
    assert_eq!((first[11], first[13]), (attr::LONG_NAME, sum));
    assert_eq!(
        u16::from_le_bytes([first[UNIT_AT[2]], first[UNIT_AT[2] + 1]]),
        0x0000,
        "after the fifteenth unit"
    );
    assert_eq!(
        u16::from_le_bytes([first[UNIT_AT[3]], first[UNIT_AT[3] + 1]]),
        0xFFFF
    );
}

#[test]
fn a_long_name_is_reassembled_only_whole_in_order_and_bound_to_its_short_name() {
    let mut buffer = [0u16; LONG_NAME_UNITS];
    let len = units("A letter to grandma 🎉.txt", &mut buffer);
    let short = *b"ALETTE~1TXT";
    assert_eq!(len, 26, "the emoji takes two units");
    let entries: [[u8; ENTRY]; 2] = {
        let mut list = long_entries(&buffer[..len], checksum(&short));
        let entries = core::array::from_fn(|_| list.next().unwrap());
        assert!(list.next().is_none());
        entries
    };
    let mut name = LongName::new();
    for raw in &entries {
        name.push(raw);
    }
    let mut out = [0u8; 255 * 3];
    let got = name.finish(&short, &mut out).unwrap();
    assert_eq!(&out[..got], "A letter to grandma 🎉.txt".as_bytes());
    assert_eq!(
        name.finish(b"OTHER~1 TXT", &mut out),
        None,
        "bound to its short name"
    );
    let mut shuffled = LongName::new();
    shuffled.push(&entries[1]);
    shuffled.push(&entries[0]);
    assert_eq!(shuffled.finish(&short, &mut out), None);
    let mut cut = LongName::new();
    cut.push(&entries[0]);
    assert_eq!(cut.finish(&short, &mut out), None, "a place missing");
    let mut deleted = entries[1];
    deleted[0] = DELETED;
    let mut reset = name.clone();
    reset.push(&deleted);
    assert_eq!(
        reset.finish(&short, &mut out),
        None,
        "a deleted entry forgets the set"
    );
    assert_eq!(name.finish(&short, &mut [0u8; 8]), None, "no room for it");
}

#[test]
fn utf16_conversions_refuse_what_is_no_name() {
    let mut out = [0u8; 16];
    assert_eq!(
        decode_utf16(&[0x0041, 0xD83C, 0xDF89, 0x0000, 0x0042], &mut out),
        Some(5)
    );
    assert_eq!(&out[..5], "A🎉".as_bytes());
    assert_eq!(
        decode_utf16(&[0x0041, 0x0042, 0x0000, 0xFFFF, 0x0043], &mut out),
        Some(2),
        "to the terminator"
    );
    for unpaired in [&[0xDC00u16][..], &[0xD83D], &[0xD83D, 0x0041]] {
        assert_eq!(
            decode_utf16(unpaired, &mut out),
            None,
            "an unpaired surrogate"
        );
    }
    assert_eq!(
        decode_utf16(&[0x0041, 0x0042, 0x0043], &mut [0u8; 2]),
        None,
        "too long for its room"
    );
    let mut units = [0u16; LONG_NAME_UNITS];
    assert_eq!(encode_utf16(&[0xFF], &mut units), None, "no UTF-8");
    assert_eq!(encode_utf16(&[b'x'; 256], &mut units), None, "too long");
    assert_eq!(encode_utf16(&[b'x'; 255], &mut units), Some(255));
}
