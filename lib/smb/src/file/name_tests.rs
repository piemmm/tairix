//! Unit tests for the names and paths both sides carry.

use alloc::string::String;
use alloc::vec::Vec;

use super::{decode_name, is_name, SharePath, MAX_PATH_UNITS};
use crate::wire::{utf16le, Encoder};
use crate::SmbError;

fn utf16(text: &str) -> Vec<u8> {
    let mut enc = Encoder::new();
    utf16le(text, &mut enc);
    enc.finish().expect("encodes")
}

fn path(names: &[&str]) -> SharePath {
    names
        .iter()
        .try_fold(SharePath::root(), |path, name| path.join(name))
        .expect("a path both sides carry")
}

#[test]
fn a_name_crosses_only_when_both_sides_carry_it() {
    let longest = "n".repeat(255);
    let widest = "\u{1f600}".repeat(63);
    for carried in [
        "report.txt",
        "ünïcødé",
        "a b",
        ".hidden",
        "...",
        longest.as_str(),
        widest.as_str(),
    ] {
        assert!(is_name(carried), "{carried}");
    }
    let too_long = "n".repeat(256);
    let too_wide = "\u{1f600}".repeat(64);
    for refused in [
        "",
        ".",
        "..",
        "a/b",
        "a\\b",
        "a:stream",
        "a*",
        "a?",
        "a|b",
        "<a>",
        "\"a\"",
        "a\0b",
        "tab\there",
        "\u{1f}",
        too_long.as_str(),
        too_wide.as_str(),
    ] {
        assert!(!is_name(refused), "{refused:?}");
    }
}

#[test]
fn a_server_name_is_decoded_only_when_both_sides_carry_it() {
    assert_eq!(
        decode_name(&utf16("report.txt")),
        Ok(String::from("report.txt"))
    );
    for refused in ["a:b", ".", "..", "a\\b", "x*"] {
        assert_eq!(
            decode_name(&utf16(refused)),
            Err(SmbError::Name),
            "{refused}"
        );
    }
    assert_eq!(decode_name(&[]), Err(SmbError::Name));
    assert_eq!(
        decode_name(&[0x00, 0xd8]),
        Err(SmbError::Name),
        "a lone surrogate"
    );
    assert_eq!(decode_name(b"a"), Err(SmbError::Malformed));
}

#[test]
fn a_path_joins_names_with_a_backslash_beneath_the_root() {
    let joined = path(&["docs", "report.txt"]);
    assert_eq!(joined.as_str(), "docs\\report.txt");
    assert_eq!(joined.names().collect::<Vec<_>>(), ["docs", "report.txt"]);
    assert_eq!(joined.utf16_len(), 30);
    let mut enc = Encoder::new();
    joined.encode(&mut enc);
    assert_eq!(enc.finish(), Ok(utf16("docs\\report.txt")));
    assert_eq!(SharePath::decode(&utf16("docs\\report.txt")), Ok(joined));

    let root = SharePath::root();
    assert!(root.is_root());
    assert_eq!(root.names().count(), 0);
    assert_eq!(root.utf16_len(), 0);
    assert_eq!(SharePath::decode(&[]), Ok(root));
}

#[test]
fn a_join_refuses_a_name_either_side_refuses() {
    for refused in ["", ".", "..", "a\\b", "a:b", "a/b"] {
        assert_eq!(
            SharePath::root().join(refused),
            Err(SmbError::Name),
            "{refused}"
        );
    }
}

#[test]
fn a_server_path_is_names_a_single_backslash_apart() {
    for refused in [
        "\\a", "a\\", "a\\\\b", "a\\.\\b", "a\\..", "a/b", "a:b\\c", "\\",
    ] {
        assert_eq!(
            SharePath::decode(&utf16(refused)),
            Err(SmbError::Name),
            "{refused}"
        );
    }
    assert_eq!(SharePath::decode(b"a"), Err(SmbError::Malformed));
}

#[test]
fn a_path_is_bounded_in_utf16_units_both_ways() {
    let name = "n".repeat(255);
    let names: Vec<&str> = core::iter::repeat_n(name.as_str(), 127).collect();
    let deep = path(&names);
    assert_eq!(deep.utf16_len(), (127 * 255 + 126) * 2);
    let last = MAX_PATH_UNITS - deep.utf16_len() / 2 - 1;
    assert!(deep.join(&"x".repeat(last)).is_ok());
    assert_eq!(deep.join(&"x".repeat(last + 1)), Err(SmbError::Name));

    let at_bound = "a\\".repeat(MAX_PATH_UNITS / 2 - 1) + "ab";
    assert_eq!(at_bound.len(), MAX_PATH_UNITS);
    assert!(SharePath::decode(&utf16(&at_bound)).is_ok());
    let past = at_bound + "c";
    assert_eq!(SharePath::decode(&utf16(&past)), Err(SmbError::Name));
}

#[test]
fn a_path_parsed_from_its_text_is_the_one_its_names_join_to() {
    let joined = path(&["Photos", "2026", "été.jpg"]);
    assert_eq!(SharePath::parse(joined.as_str()), Ok(joined));
    assert_eq!(SharePath::parse(""), Ok(SharePath::root()));
    for refused in [
        "\\a", "a\\", "a\\\\b", "a\\.\\b", "a\\..", "a/b", "a:b\\c", "\\", "a*",
    ] {
        assert_eq!(SharePath::parse(refused), Err(SmbError::Name), "{refused}");
    }
    let at_bound = "a\\".repeat(MAX_PATH_UNITS / 2 - 1) + "ab";
    assert!(SharePath::parse(&at_bound).is_ok());
    assert_eq!(SharePath::parse(&(at_bound + "c")), Err(SmbError::Name));
}
