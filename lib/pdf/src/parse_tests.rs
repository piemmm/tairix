//! Objects from tokens, indirect objects, and where a stream's bytes are.

use alloc::vec;

use super::{find, indirect, object, rfind, stream_span, stream_start, Endstreams, Indirect};
use crate::lex::Lexer;
use crate::limits::MAX_NESTING;
use crate::object::{Name, ObjRef, Object};
use crate::PdfError;

fn read(text: &[u8]) -> Result<Object, PdfError> {
    object(&mut Lexer::new(text, 0), 0)
}

fn name(text: &str) -> Object {
    Object::Name(Name(text.as_bytes().to_vec()))
}

#[test]
fn an_integer_pair_followed_by_r_is_a_reference() {
    assert_eq!(read(b"12 0 R"), Ok(Object::Ref(ObjRef { num: 12, gen: 0 })));
    assert_eq!(
        read(b"[1 2 3 0 R 4]"),
        Ok(Object::Array(vec![
            Object::Int(1),
            Object::Int(2),
            Object::Ref(ObjRef { num: 3, gen: 0 }),
            Object::Int(4),
        ]))
    );
}

#[test]
fn an_integer_pair_not_followed_by_r_is_two_integers() {
    let mut lexer = Lexer::new(b"12 0 obj", 0);
    assert_eq!(object(&mut lexer, 0), Ok(Object::Int(12)));
    assert_eq!(object(&mut lexer, 0), Ok(Object::Int(0)));
}

#[test]
fn a_negative_number_is_never_a_reference() {
    assert_eq!(
        read(b"[-1 0 R]"),
        Err(PdfError::Malformed),
        "the R stands alone once -1 is no object number"
    );
}

#[test]
fn dictionaries_nest_and_read_every_kind_of_value() {
    let dict = read(b"<</Type/Page/Kids[]/Sub<</On true/Off false/Nil null>>/Size 3.5/S(x)>>")
        .expect("a dictionary");
    let dict = dict.as_dict().expect("a dictionary");
    assert!(dict.is_type(b"Page"));
    assert_eq!(dict.get(b"Kids"), Some(&Object::Array(vec![])));
    assert_eq!(dict.get(b"Size"), Some(&Object::Real(3.5)));
    assert_eq!(dict.get(b"S"), Some(&Object::String(b"x".to_vec())));
    let sub = dict
        .get(b"Sub")
        .and_then(Object::as_dict)
        .expect("a nested dictionary");
    assert_eq!(sub.get(b"On"), Some(&Object::Bool(true)));
    assert_eq!(sub.get(b"Off"), Some(&Object::Bool(false)));
    assert_eq!(sub.get(b"Nil"), Some(&Object::Null));
    assert_eq!(dict.get(b"Type"), Some(&name("Page")));
}

#[test]
fn nesting_past_the_bound_is_too_complex() {
    let deep = alloc::format!(
        "{}{}",
        "[".repeat(MAX_NESTING + 1),
        "]".repeat(MAX_NESTING + 1)
    );
    assert_eq!(read(deep.as_bytes()), Err(PdfError::TooComplex));
    let fits = alloc::format!("{}{}", "[".repeat(MAX_NESTING), "]".repeat(MAX_NESTING));
    assert!(read(fits.as_bytes()).is_ok());
}

#[test]
fn a_dictionary_key_must_be_a_name() {
    assert_eq!(read(b"<<1 2>>"), Err(PdfError::Malformed));
    assert_eq!(read(b"<</A>>"), Err(PdfError::Malformed));
    assert_eq!(read(b"[1 2"), Err(PdfError::Malformed));
}

#[test]
fn an_indirect_object_is_read_with_its_number() {
    let mut lexer = Lexer::new(b"7 2 obj (seven) endobj", 0);
    let (reference, body) = indirect(&mut lexer).expect("an indirect object");
    assert_eq!(reference, ObjRef { num: 7, gen: 2 });
    assert!(matches!(body, Indirect::Object(Object::String(text)) if text == b"seven"));
}

#[test]
fn a_stream_is_told_from_a_dictionary_by_its_keyword() {
    let text = b"3 0 obj <</Length 5>> stream\r\nhello\nendstream endobj";
    let (_, body) = indirect(&mut Lexer::new(text, 0)).expect("an indirect object");
    let Indirect::Stream { dict, data } = body else {
        panic!("a stream");
    };
    assert_eq!(dict.get(b"Length"), Some(&Object::Int(5)));
    assert_eq!(&text[data..data + 5], b"hello");
    let (_, body) = indirect(&mut Lexer::new(b"3 0 obj <</A 1>> endobj", 0)).expect("an object");
    assert!(matches!(body, Indirect::Object(Object::Dict(_))));
}

#[test]
fn a_stream_starts_past_its_end_of_line() {
    assert_eq!(stream_start(b"stream\r\nx", 6), 8);
    assert_eq!(stream_start(b"stream\nx", 6), 7);
    assert_eq!(stream_start(b"stream\rx", 6), 7);
    assert_eq!(stream_start(b"streamx", 6), 6);
}

#[test]
fn a_stream_of_the_stated_length_ends_there() {
    let data = b"0123456789\nendstream";
    assert_eq!(
        stream_span(data, 0, Some(10), &Endstreams::default()),
        Ok(Some((0, 10)))
    );
}

#[test]
fn a_wrong_length_falls_back_to_the_endstream_keyword() {
    let data = b"0123456789\r\nendstream";
    for wrong in [Some(4), Some(30), Some(usize::MAX), None] {
        assert_eq!(
            stream_span(data, 0, wrong, &Endstreams::default()),
            Ok(Some((0, 10))),
            "{wrong:?}"
        );
    }
    assert_eq!(
        stream_span(b"no end here", 0, None, &Endstreams::default()),
        Ok(None)
    );
}

#[test]
fn a_wrong_length_finds_the_next_endstream_after_its_start_from_one_index() {
    let data = b"aa\nendstream bbbb\nendstream cc\nendstream";
    let ends = Endstreams::default();
    assert_eq!(stream_span(data, 0, None, &ends), Ok(Some((0, 2))));
    assert_eq!(stream_span(data, 13, None, &ends), Ok(Some((13, 17))));
    assert_eq!(stream_span(data, 28, None, &ends), Ok(Some((28, 30))));
    assert_eq!(stream_span(data, 40, None, &ends), Ok(None));
}

#[test]
fn find_and_rfind_answer_the_first_and_last_match() {
    assert_eq!(find(b"abcabc", b"bc"), Some(1));
    assert_eq!(rfind(b"abcabc", b"bc"), Some(4));
    assert_eq!(find(b"ab", b"abc"), None);
    assert_eq!(find(b"abc", b""), None);
}
