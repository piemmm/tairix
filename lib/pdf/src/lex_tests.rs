//! The lexer: every token kind, and its tolerance of what real files hold.

use alloc::vec::Vec;

use super::{Lexer, Token};
use crate::PdfError;

fn tokens(text: &[u8]) -> Vec<Token<'_>> {
    let mut lexer = Lexer::new(text, 0);
    let mut found = Vec::new();
    while let Some(token) = lexer.next().expect("a token") {
        found.push(token);
    }
    found
}

#[test]
fn numbers_read_as_integers_where_they_have_no_point() {
    assert_eq!(
        tokens(b"0 +17 -98 34.5 -3.62 +123.6 4. -.002 .5"),
        [
            Token::Integer(0),
            Token::Integer(17),
            Token::Integer(-98),
            Token::Real(34.5),
            Token::Real(-3.62),
            Token::Real(123.6),
            Token::Real(4.0),
            Token::Real(-0.002),
            Token::Real(0.5),
        ]
    );
}

#[test]
fn an_integer_too_large_for_one_reads_as_a_real() {
    assert_eq!(tokens(b"100000000000000000000"), [Token::Real(1e20)]);
}

#[test]
fn reals_are_correctly_rounded() {
    assert_eq!(
        tokens(b"0.1 3.62 0.3"),
        [Token::Real(0.1), Token::Real(3.62), Token::Real(0.3)]
    );
}

#[test]
fn an_exponent_some_files_write_is_read() {
    assert_eq!(
        tokens(b"1e9 -2.5E-3 4e+2"),
        [Token::Real(1e9), Token::Real(-2.5e-3), Token::Real(400.0)]
    );
}

#[test]
fn a_number_run_into_what_follows_is_split_from_it() {
    assert_eq!(
        tokens(b"0cm 1.2.3 1e 1e-"),
        [
            Token::Integer(0),
            Token::Keyword(b"cm"),
            Token::Real(1.2),
            Token::Real(0.3),
            Token::Integer(1),
            Token::Keyword(b"e"),
            Token::Integer(1),
            Token::Keyword(b"e-"),
        ]
    );
}

#[test]
fn a_doubled_minus_is_read_as_one() {
    assert_eq!(tokens(b"--5 --.5"), [Token::Integer(-5), Token::Real(-0.5)]);
}

#[test]
fn a_run_with_no_digit_to_begin_a_number_is_a_keyword() {
    assert_eq!(
        tokens(b"- . -. e5 1e999 inf"),
        [
            Token::Keyword(b"-"),
            Token::Keyword(b"."),
            Token::Keyword(b"-."),
            Token::Keyword(b"e5"),
            Token::Keyword(b"1e999"),
            Token::Keyword(b"inf"),
        ]
    );
}

#[test]
fn names_resolve_their_escapes() {
    assert_eq!(
        tokens(b"/Name1 /A;Name_With-Various***Characters? /lime#20Green /paired#28#29 /#zz /"),
        [
            Token::Name(b"Name1".to_vec()),
            Token::Name(b"A;Name_With-Various***Characters?".to_vec()),
            Token::Name(b"lime Green".to_vec()),
            Token::Name(b"paired()".to_vec()),
            // An escape that is not two hexadecimal digits stands for itself.
            Token::Name(b"#zz".to_vec()),
            Token::Name(Vec::new()),
        ]
    );
}

#[test]
fn literal_strings_keep_balanced_parentheses_and_resolve_escapes() {
    assert_eq!(
        tokens(b"(a (b) c) (\\(\\)\\\\) (\\n\\r\\t\\b\\f) (\\101\\7\\0612) (\\q)"),
        [
            Token::String(b"a (b) c".to_vec()),
            Token::String(b"()\\".to_vec()),
            Token::String(b"\n\r\t\x08\x0c".to_vec()),
            Token::String(b"A\x0712".to_vec()),
            Token::String(b"q".to_vec()),
        ]
    );
}

#[test]
fn an_octal_escape_past_a_byte_keeps_its_low_byte() {
    assert_eq!(tokens(b"(\\777)"), [Token::String(vec_of(0xff))]);
}

fn vec_of(byte: u8) -> Vec<u8> {
    alloc::vec![byte]
}

#[test]
fn a_literal_string_reads_any_end_of_line_as_a_line_feed() {
    assert_eq!(
        tokens(b"(one\r\ntwo\rthree\nfour)"),
        [Token::String(b"one\ntwo\nthree\nfour".to_vec())]
    );
}

#[test]
fn a_backslash_before_an_end_of_line_continues_the_string() {
    assert_eq!(
        tokens(b"(con\\\r\ntin\\\nued)"),
        [Token::String(b"continued".to_vec())]
    );
}

#[test]
fn hexadecimal_strings_skip_white_space_and_pad_an_odd_digit() {
    assert_eq!(
        tokens(b"<4E6F 7620 73686D6F7A> <901FA> <>"),
        [
            Token::String(b"Nov shmoz".to_vec()),
            Token::String(alloc::vec![0x90, 0x1f, 0xa0]),
            Token::String(Vec::new()),
        ]
    );
}

#[test]
fn delimiters_and_keywords_are_tokens_of_their_own() {
    assert_eq!(
        tokens(b"<</A[1]>>{true}obj R"),
        [
            Token::DictOpen,
            Token::Name(b"A".to_vec()),
            Token::ArrayOpen,
            Token::Integer(1),
            Token::ArrayClose,
            Token::DictClose,
            Token::BraceOpen,
            Token::Keyword(b"true"),
            Token::BraceClose,
            Token::Keyword(b"obj"),
            Token::Keyword(b"R"),
        ]
    );
}

#[test]
fn comments_are_white_space() {
    assert_eq!(
        tokens(b"1 % a comment ( not a string\r2%\n3"),
        [Token::Integer(1), Token::Integer(2), Token::Integer(3)]
    );
}

#[test]
fn a_stray_delimiter_is_refused_and_passed_over() {
    let mut lexer = Lexer::new(b") 5", 0);
    assert_eq!(lexer.next(), Err(PdfError::Malformed));
    assert_eq!(lexer.next(), Ok(Some(Token::Integer(5))));
    let mut lexer = Lexer::new(b"> 6", 0);
    assert_eq!(lexer.next(), Err(PdfError::Malformed));
    assert_eq!(lexer.next(), Ok(Some(Token::Integer(6))));
}

#[test]
fn an_unterminated_string_is_refused() {
    for text in [&b"(open"[..], b"(open\\", b"<4E6F", b"<4G>"] {
        assert_eq!(
            Lexer::new(text, 0).next(),
            Err(PdfError::Malformed),
            "{text:?}"
        );
    }
}

#[test]
fn the_end_of_the_data_is_no_token() {
    assert_eq!(Lexer::new(b"  % only a comment", 0).next(), Ok(None));
}
