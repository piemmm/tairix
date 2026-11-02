//! Member names crossing between an archive's spelling and a served name.

use alloc::vec;
use alloc::vec::Vec;

use super::{as_is, from_riscos, spelled, to_riscos, Charset, Spelled};
use crate::tree::MAX_DEPTH;

#[test]
fn every_byte_of_code_page_437_and_latin_1_round_trips_through_utf_8() {
    for charset in [Charset::Cp437, Charset::Latin1] {
        for byte in 0..=u8::MAX {
            let utf8 = charset.decode(&[byte]).expect("every byte is a character");
            assert!(core::str::from_utf8(&utf8).is_ok());
            assert_eq!(
                charset.encode(&utf8),
                Some(vec![byte]),
                "{charset:?} {byte:#04x}"
            );
        }
    }
    assert_eq!(
        Charset::Cp437.decode(b"caf\x82"),
        Some("café".as_bytes().to_vec())
    );
    assert_eq!(
        Charset::Latin1.decode(b"caf\xe9"),
        Some("café".as_bytes().to_vec())
    );
}

#[test]
fn risc_os_latin_1_spells_its_own_characters_and_refuses_its_holes_and_controls() {
    assert_eq!(
        Charset::RiscOs.decode(&[0x80]),
        Some("€".as_bytes().to_vec())
    );
    assert_eq!(
        Charset::RiscOs.decode(&[0x9E]),
        Some("ﬁ".as_bytes().to_vec())
    );
    for hole in [0x00, 0x1F, 0x7F, 0x83, 0x84, 0x87, 0x8B] {
        assert_eq!(Charset::RiscOs.decode(&[hole]), None, "{hole:#04x}");
    }
    for byte in 0..=u8::MAX {
        if let Some(utf8) = Charset::RiscOs.decode(&[byte]) {
            assert_eq!(
                Charset::RiscOs.encode(&utf8),
                Some(vec![byte]),
                "{byte:#04x}"
            );
        }
    }
    assert_eq!(Charset::Latin1.encode("€".as_bytes()), None);
    assert_eq!(Charset::Cp437.encode("€".as_bytes()), None);
}

#[test]
fn utf_8_is_held_to_utf_8() {
    assert_eq!(Charset::Utf8.decode(b"\xff"), None);
    assert_eq!(
        Charset::Utf8.decode("naïve".as_bytes()),
        Some("naïve".as_bytes().to_vec())
    );
}

fn within(parts: &[&[u8]]) -> Spelled {
    Spelled::Within(parts.iter().map(|part| part.to_vec()).collect())
}

#[test]
fn a_path_never_names_anything_outside_the_volume() {
    assert_eq!(spelled(b"a/b/c", b'/', as_is), within(&[b"a", b"b", b"c"]));
    assert_eq!(spelled(b"/a//./b/", b'/', as_is), within(&[b"a", b"b"]));
    assert_eq!(spelled(b"./", b'/', as_is), Spelled::Root);
    assert_eq!(spelled(b"", b'/', as_is), Spelled::Root);
    for climbing in [&b"../a"[..], b"a/../../b", b"a/.."] {
        assert_eq!(spelled(climbing, b'/', as_is), Spelled::Refused);
    }
    assert_eq!(spelled(b"a/b\0c", b'/', as_is), Spelled::Refused);
    assert_eq!(spelled(b"a/\xff", b'/', as_is), Spelled::Refused);
}

#[test]
fn a_risc_os_name_trades_its_dots_and_slashes() {
    assert_eq!(from_riscos(b"ReadMe/txt"), Some(b"ReadMe.txt".to_vec()));
    assert_eq!(to_riscos(b"ReadMe.txt", 10), Some(b"ReadMe/txt".to_vec()));
    assert_eq!(from_riscos(b"\x80uro"), Some("€uro".as_bytes().to_vec()));
    assert_eq!(to_riscos("€uro".as_bytes(), 10), Some(b"\x80uro".to_vec()));
    // A served name holding `/` would be RISC OS's path separator `.`.
    assert_eq!(from_riscos(b"a.b"), None);
}

#[test]
fn a_name_risc_os_could_not_hold_is_not_spelled_for_it() {
    assert_eq!(to_riscos(b"Ten chars!", 10), None, "a space");
    assert_eq!(to_riscos(b"wild*card", 10), None, "a reserved character");
    assert_eq!(to_riscos(b"Elevenchars", 10), None, "too long");
    assert_eq!(
        to_riscos("日本".as_bytes(), 10),
        None,
        "outside its Latin-1"
    );
    assert_eq!(to_riscos(b"", 10), None);
    let longest: Vec<u8> = vec![b'x'; 10];
    assert_eq!(to_riscos(&longest, 10), Some(longest.clone()));
}

#[test]
fn a_path_deeper_than_the_tree_nests_is_refused_as_it_is_read() {
    let deepest = b"d/".repeat(MAX_DEPTH + 1);
    assert!(matches!(
        spelled(&deepest, b'/', as_is),
        Spelled::Within(parts) if parts.len() == MAX_DEPTH + 1
    ));
    let deeper = b"d/".repeat(MAX_DEPTH + 2);
    assert_eq!(spelled(&deeper, b'/', as_is), Spelled::Refused);
}
