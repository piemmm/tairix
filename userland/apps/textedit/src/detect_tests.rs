//! Unit tests for what a freshly opened document is taken to be.

use tairix_syntax::Format;

use super::{format_for_name, indentation, line_ending, looks_binary, Indent, LineEnding};
use crate::document::Document;

fn doc(text: &[u8]) -> Document {
    Document::from_chunks(alloc::vec![text.to_vec()]).expect("loads")
}

fn binary(text: &[u8]) -> bool {
    looks_binary(&doc(text))
}

#[test]
fn text_is_text_however_foreign_its_script() {
    assert!(!binary(b""));
    assert!(!binary(
        "fn main() {\n\tprintln!(\"\u{4e2d}\u{6587}\");\r\n}\x0c\x1b[0m".as_bytes()
    ));
}

#[test]
fn a_nul_or_a_run_of_odd_bytes_is_binary() {
    assert!(binary(b"text\0more"));
    let mut mostly = alloc::vec![b'a'; 80];
    mostly.extend(core::iter::repeat_n(0xff, 20));
    assert!(binary(&mostly), "one byte in five is not UTF-8");
    let mut few = alloc::vec![b'a'; 95];
    few.extend(core::iter::repeat_n(0x01, 5));
    assert!(!binary(&few), "a few control bytes in text stay text");
}

#[test]
fn only_the_head_is_judged() {
    let mut text = alloc::vec![b'a'; super::HEAD_BYTES];
    text.extend(core::iter::repeat_n(0u8, 100));
    assert!(!binary(&text), "a NUL past the head is not looked at");
    // A character the head's end cuts in two is not counted against it.
    let mut cut = alloc::vec![b'a'; super::HEAD_BYTES - 1];
    cut.extend_from_slice("\u{4e2d}".as_bytes());
    assert!(!binary(&cut));
}

#[test]
fn a_store_is_known_by_its_name_before_its_extension() {
    assert_eq!(
        format_for_name("/System/Settings/network.conf"),
        Some(Format::NetworkConfig)
    );
    assert_eq!(format_for_name("other.conf"), Some(Format::PlainText));
    assert_eq!(format_for_name("Cargo.toml"), Some(Format::Toml));
    assert_eq!(format_for_name("page.HTML"), Some(Format::Html));
    assert_eq!(format_for_name("logo.svg"), Some(Format::Xml));
    assert_eq!(format_for_name("tool.py"), Some(Format::Python));
    assert_eq!(format_for_name("app.mjs"), Some(Format::JavaScript));
    assert_eq!(format_for_name("photo.png"), None);
    assert_eq!(
        format_for_name("Makefile"),
        None,
        "a name that says nothing leaves the head to decide"
    );
}

#[test]
fn the_first_line_break_sets_the_convention() {
    assert_eq!(line_ending(&doc(b"one\r\ntwo\nthree")), LineEnding::CrLf);
    assert_eq!(line_ending(&doc(b"one\ntwo\r\n")), LineEnding::Lf);
    assert_eq!(line_ending(&doc(b"no break")), LineEnding::Lf);
}

#[test]
fn indentation_follows_what_the_document_does() {
    assert_eq!(
        indentation(&doc(b"fn a() {\n    x;\n        y;\n}\n")),
        Indent::Spaces(4)
    );
    assert_eq!(indentation(&doc(b"a:\n  b:\n    c\n")), Indent::Spaces(2));
    assert_eq!(indentation(&doc(b"all:\n\tcc -o x x.c\n")), Indent::Tab);
    assert_eq!(indentation(&doc(b"flat\ntext\n")), Indent::Tab);
    assert_eq!(indentation(&doc(b" one space is alignment\n")), Indent::Tab);
}
