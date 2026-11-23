//! The token engine's tests: RISC OS's rules for finding a token's value.

use super::*;

const FILE: &[u8] = b"# A comment: not a token\n\
    Bad:Bad thing\n\
    Two/Deux:Either name\r\n\
    Err?:Wild %0 and %1\n\
    Alone\n\
    Next:Shared\n\
    Empty:\n\
    Esc:a\x1B\nb\n";

fn value<'a>(data: &'a [u8], token: &[u8]) -> Option<&'a [u8]> {
    let at = find(data, token)?;
    Some(&data[at..at + value_length(data, at)?])
}

#[test]
fn a_token_finds_its_line_by_any_name_or_wildcard() {
    assert_eq!(value(FILE, b"Bad"), Some(&b"Bad thing"[..]));
    assert_eq!(
        value(FILE, b"Deux"),
        Some(&b"Either name"[..]),
        "an alternative, CR ending it"
    );
    assert_eq!(value(FILE, b"Err7"), Some(&b"Wild %0 and %1"[..]));
    assert_eq!(value(FILE, b"Empty"), Some(&b""[..]));
    assert_eq!(value(FILE, b"Ba"), None, "whole tokens only");
    assert_eq!(value(FILE, b"bad"), None, "case matters");
    assert_eq!(value(FILE, b"# A comment"), None);
}

#[test]
fn a_token_alone_on_its_line_names_the_next_value() {
    assert_eq!(value(FILE, b"Alone"), Some(&b"Shared"[..]));
    assert_eq!(value(FILE, b"Next"), Some(&b"Shared"[..]));
}

#[test]
fn an_escape_takes_its_byte_with_it() {
    assert_eq!(value(FILE, b"Esc"), Some(&b"a\x1B\nb"[..]));
}

#[test]
fn a_value_the_file_ends_inside_is_not_there() {
    assert_eq!(
        find(b"A:runs on", b"A").and_then(|at| value_length(b"A:runs on", at)),
        None
    );
    assert_eq!(find(b"A", b"A"), None, "no colon after it");
}

#[test]
fn a_hash_where_a_token_is_compared_skips_its_line() {
    assert_eq!(
        value(b"B#x/B:no\nB:yes\n", b"Bx"),
        None,
        "past the hash, the line is gone"
    );
    assert_eq!(
        value(b"A#x/B:no\nB:yes\n", b"B"),
        Some(&b"no"[..]),
        "a hash a mismatch passes is never compared"
    );
    assert_eq!(value(b"#B:no\nB:yes\n", b"B"), Some(&b"yes"[..]));
}

#[test]
fn a_file_token_ends_at_a_slash_even_where_the_caller_s_goes_on() {
    assert_eq!(value(b"A/B:v\n", b"A/B"), None);
    assert_eq!(
        value(b"A:x\0B:y\n", b"B"),
        Some(&b"y"[..]),
        "a value's end begins the next token"
    );
}

#[test]
fn a_caller_s_token_ends_at_space_comma_bracket_or_colon() {
    assert_eq!(token_end(b"Tok:default"), 3);
    assert_eq!(token_end(b"Tok,x"), 3);
    assert_eq!(token_end(b"Tok)x"), 3);
    assert_eq!(token_end(b"Tok rest"), 3);
    assert_eq!(token_end(b"Tok"), 3);
}

#[test]
fn a_default_follows_the_first_colon_before_a_control_character() {
    assert_eq!(default_of(b"Tok:the default\nmore"), Some(4..15));
    assert_eq!(
        default_of(b"Tok more:x"),
        Some(9..10),
        "a space does not stop the search"
    );
    assert_eq!(default_of(b"Tok\n:x"), None);
    assert_eq!(default_of(b"Tok"), None);
}

/// The tokens `pattern` names in `data`, in order.
fn tokens<'a>(data: &'a [u8], pattern: &[u8]) -> alloc::vec::Vec<&'a [u8]> {
    let mut at = 0;
    let mut seen = alloc::vec::Vec::new();
    while let Some((range, next)) = next_token(data, pattern, at) {
        seen.push(&data[range]);
        at = next;
    }
    seen
}

#[test]
fn tokens_come_one_by_one_in_order() {
    assert_eq!(
        tokens(FILE, b"*"),
        [
            &b"Bad"[..],
            b"Two",
            b"Deux",
            b"Err?",
            b"Alone",
            b"Next",
            b"Empty",
            b"Esc"
        ]
    );
    assert_eq!(
        tokens(b"/A:x\n", b"*"),
        [&b"A"[..]],
        "an empty token is passed over"
    );
}

#[test]
fn patterns_match_as_enumeration_takes_them() {
    assert_eq!(
        tokens(FILE, b"Err7"),
        [&b"Err?"[..]],
        "a file's ? matches too"
    );
    assert_eq!(tokens(FILE, b"E*"), [&b"Err?"[..], b"Empty", b"Esc"]);
    assert_eq!(tokens(FILE, b"?ad"), [&b"Bad"[..]]);
    assert_eq!(tokens(FILE, b"E"), alloc::vec::Vec::<&[u8]>::new());
    assert!(pattern_misplaced_star(b"E*r"));
    assert!(!pattern_misplaced_star(b"Er*"));
}

#[test]
fn expansion_substitutes_parameters_and_dictionary_words() {
    let dictionary = b"\x06word\0\x07again\0\x00";
    let expansion = Expansion {
        parameters: [Some(&b"A\n"[..]), Some(&b"BC"[..]), None, None],
        dictionary,
    };
    let mut out = Held::new(64).expect("room");
    expansion.expand(
        b"%0 %1 %2 %% %x \x1B\x01 \x1B\x02 \x1B\x00 \x1B\x09.\nnot",
        &mut out,
    );
    assert_eq!(out.bytes, b"A BC %2 % %x word again A .");
    let mut short = Held::new(3).expect("room");
    expansion.expand(b"%1 long", &mut short);
    assert_eq!(short.bytes, b"BC ", "cut short silently");
}

#[test]
fn a_dictionary_that_names_itself_is_bounded() {
    let dictionary = b"\x04\x1B\x01\0\x00";
    let expansion = Expansion {
        parameters: [None; 4],
        dictionary,
    };
    let mut out = Held::new(8).expect("room");
    expansion.expand(b"\x1B\x01", &mut out);
    assert!(out.bytes.is_empty());
}

#[test]
fn a_value_s_end_is_found_the_same_however_it_is_cut_into_pieces() {
    let data = b"ab\x1B\ncd\x1B\x1B\x1Be\x1B\rf\ng";
    let whole = value_length(data, 0);
    assert_eq!(whole, Some(13), "the escaped line feed and return carried");
    for cut in 0..=data.len() {
        let mut end = ValueEnd::default();
        let (first, second) = data.split_at(cut);
        let found = end
            .find(first)
            .or_else(|| end.find(second).map(|n| cut + n));
        assert_eq!(found, whole, "cut at {cut}");
    }
    assert_eq!(
        value_length(b"ab\x1B", 0),
        None,
        "an escape the file ends after"
    );
    assert_eq!(value_length(b"ab", 3), None);
}
