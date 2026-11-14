//! Member names crossing between an archive's spelling and a served name.

use alloc::vec;
use alloc::vec::Vec;

use super::{from_riscos, spelled, to_riscos, Charset, Spelled};
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
fn latin_1_and_code_page_437_lack_risc_os_s_own_characters() {
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
    assert_eq!(spelled(b"a/b/c", b'/'), within(&[b"a", b"b", b"c"]));
    assert_eq!(spelled(b"/a//./b/", b'/'), within(&[b"a", b"b"]));
    assert_eq!(spelled(b"./", b'/'), Spelled::Root);
    assert_eq!(spelled(b"", b'/'), Spelled::Root);
    for climbing in [&b"../a"[..], b"a/../../b", b"a/.."] {
        assert_eq!(spelled(climbing, b'/'), Spelled::Refused);
    }
    assert_eq!(spelled(b"a/b\0c", b'/'), Spelled::Refused);
    assert_eq!(spelled(b"a/\xff", b'/'), Spelled::Refused);
}

#[test]
fn a_risc_os_name_trades_its_dots_and_slashes() {
    assert_eq!(from_riscos(b"ReadMe/txt"), Some(b"ReadMe.txt".to_vec()));
    assert_eq!(to_riscos(b"ReadMe.txt", 10), Some(b"ReadMe/txt".to_vec()));
    assert_eq!(from_riscos(b"\x80uro"), Some("€uro".as_bytes().to_vec()));
    assert_eq!(to_riscos("€uro".as_bytes(), 10), Some(b"\x80uro".to_vec()));
    // A served name holding `/` would be RISC OS's path separator `.`.
    assert_eq!(from_riscos(b"a.b"), None);
    // Nor is a name read that RISC OS itself could not have written.
    for unwritable in [&b"wild*card"[..], b"a b", b"dollar$", b"Bell\x07"] {
        assert_eq!(from_riscos(unwritable), None, "{unwritable:?}");
    }
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
        spelled(&deepest, b'/'),
        Spelled::Within(parts) if parts.len() == MAX_DEPTH + 1
    ));
    let deeper = b"d/".repeat(MAX_DEPTH + 2);
    assert_eq!(spelled(&deeper, b'/'), Spelled::Refused);
}
