//! The lexer held to the specification: its tokens, literal patterns and
//! comments, and the template lists its discovery algorithm finds in the
//! specification's own examples.

use alloc::vec::Vec;

use super::{number, tokens, Kind, Shape, KEYWORDS, RESERVED};
use crate::diag::{Code, Stop};

/// The kinds of `text`'s tokens, without the end.
fn kinds(text: &str) -> Vec<Kind> {
    let mut kinds: Vec<Kind> = tokens(text)
        .expect("lexed")
        .iter()
        .map(|t| t.kind)
        .collect();
    assert_eq!(kinds.pop(), Some(Kind::End));
    kinds
}

/// The text of `text`'s tokens, without the end.
fn spelled(text: &str) -> Vec<&str> {
    tokens(text)
        .expect("lexed")
        .iter()
        .filter(|t| t.kind != Kind::End)
        .map(|t| &text[t.span.start as usize..t.span.end as usize])
        .collect()
}

/// The code `text` is refused with.
fn refused(text: &str) -> Code {
    match tokens(text) {
        Err(Stop::Error(code, ..)) => code,
        other => panic!("{text:?}: {other:?}"),
    }
}

#[test]
fn the_keyword_and_reserved_tables_are_sorted_for_their_searches() {
    assert!(KEYWORDS.windows(2).all(|w| w[0].0 < w[1].0));
    assert!(RESERVED.windows(2).all(|w| w[0] < w[1]));
}

#[test]
fn a_declaration_reads_as_its_tokens() {
    use Kind::*;
    assert_eq!(
        kinds("@fragment fn main(@location(0) c: vec4<f32>) -> @location(0) vec4f { return c; }"),
        [
            At,
            Ident,
            Fn,
            Ident,
            ParenLeft,
            At,
            Ident,
            ParenLeft,
            Int,
            ParenRight,
            Ident,
            Colon,
            Ident,
            TemplateStart,
            Ident,
            TemplateEnd,
            ParenRight,
            Arrow,
            At,
            Ident,
            ParenLeft,
            Int,
            ParenRight,
            Ident,
            BraceLeft,
            Return,
            Ident,
            Semicolon,
            BraceRight,
        ]
    );
}

#[test]
fn punctuation_is_read_longest_first() {
    assert_eq!(
        spelled("a>>=b<<=c&&d||e->f==g!=h++--+=-=*=/=%=&=|=^=^i"),
        [
            "a", ">>=", "b", "<<=", "c", "&&", "d", "||", "e", "->", "f", "==", "g", "!=", "h",
            "++", "--", "+=", "-=", "*=", "/=", "%=", "&=", "|=", "^=", "^", "i"
        ]
    );
    assert_eq!(
        kinds("_ = x;"),
        [Kind::Underscore, Kind::Equal, Kind::Ident, Kind::Semicolon]
    );
}

#[test]
fn numeric_literals_match_their_patterns_longest_first() {
    let shape = |float, hex, suffix: Option<u8>| Shape { float, hex, suffix };
    let cases: [(&str, usize, Shape); 24] = [
        ("0", 1, shape(false, false, None)),
        ("0i", 2, shape(false, false, Some(b'i'))),
        ("123u", 4, shape(false, false, Some(b'u'))),
        ("012", 1, shape(false, false, None)),
        ("0x3f", 4, shape(false, true, None)),
        ("0X123u", 6, shape(false, true, Some(b'u'))),
        ("0f", 2, shape(true, false, Some(b'f'))),
        ("1h", 2, shape(true, false, Some(b'h'))),
        ("0.e+4f", 6, shape(true, false, Some(b'f'))),
        ("01.", 3, shape(true, false, None)),
        (".01", 3, shape(true, false, None)),
        ("12.34", 5, shape(true, false, None)),
        (".0f", 3, shape(true, false, Some(b'f'))),
        ("1e-3", 4, shape(true, false, None)),
        ("1e", 1, shape(false, false, None)),
        ("1.0i", 3, shape(true, false, None)),
        ("0xa.fp+2", 8, shape(true, true, None)),
        ("0x1P+4f", 7, shape(true, true, Some(b'f'))),
        ("0X.3", 4, shape(true, true, None)),
        ("0x3p+2h", 7, shape(true, true, Some(b'h'))),
        ("0X1.fp-4", 8, shape(true, true, None)),
        ("0x1.8f", 6, shape(true, true, None)),
        ("0x", 1, shape(false, false, None)),
        ("0x.p1", 1, shape(false, false, None)),
    ];
    for (text, length, expected) in cases {
        assert_eq!(
            number(text.as_bytes(), 0),
            Some((length, expected)),
            "{text}"
        );
    }
    assert_eq!(number(b".x", 0), None);
}

#[test]
fn comments_nest_and_end_at_line_breaks() {
    assert_eq!(spelled("a /* b /* c */ d */ e"), ["a", "e"]);
    assert_eq!(spelled("a // b\u{2028}c // d\re"), ["a", "c", "e"]);
    assert_eq!(refused("a /* b /* c */"), Code::UnterminatedComment);
}

#[test]
fn names_are_uax_31_identifiers_never_a_bare_or_doubled_underscore() {
    assert_eq!(
        spelled("Δέλτα réflexion 朝焼け _a a_b"),
        ["Δέλτα", "réflexion", "朝焼け", "_a", "a_b"]
    );
    assert_eq!(kinds("_"), [Kind::Underscore]);
    assert_eq!(refused("__a"), Code::InvalidName);
    assert_eq!(kinds("var asm"), [Kind::Var, Kind::Reserved]);
    assert_eq!(refused("a $ b"), Code::InvalidCharacter);
    assert_eq!(
        refused("\u{feff}a"),
        Code::InvalidCharacter,
        "no byte order mark"
    );
}

#[test]
fn blankspace_is_pattern_white_space() {
    assert_eq!(
        spelled("a\u{85}b\u{200e}c\u{200f}d\u{2029}e\u{b}f\u{c}g"),
        ["a", "b", "c", "d", "e", "f", "g"]
    );
}

/// Where `text`'s template lists open and close, by the token texts between
/// and including the delimiters.
fn delimited(text: &str) -> Vec<(usize, usize)> {
    let tokens = tokens(text).expect("lexed");
    let mut open = Vec::new();
    let mut lists = Vec::new();
    for token in &tokens {
        match token.kind {
            Kind::TemplateStart => open.push(token.span.start as usize),
            Kind::TemplateEnd => {
                lists.push((open.pop().expect("opened"), token.span.start as usize));
            }
            _ => {}
        }
    }
    assert!(open.is_empty(), "{text}: every list closed");
    lists.sort_unstable();
    lists
}

#[test]
fn template_lists_are_found_as_the_specification_s_examples_say() {
    let at = |text: &str, ch: char, nth: usize| {
        text.char_indices()
            .filter(|&(_, c)| c == ch)
            .nth(nth)
            .map(|(i, _)| i)
            .expect("present")
    };
    let text = "A ( B < C, D > ( E ) )";
    assert_eq!(delimited(text), [(at(text, '<', 0), at(text, '>', 0))]);
    let text = "array<i32,select(2,3,a>b)>";
    assert_eq!(delimited(text), [(at(text, '<', 0), at(text, '>', 1))]);
    assert_eq!(delimited("a[b<d]>()"), []);
    for text in ["A<B<<C>", "A<B<=C>", "A<(B>=C)>", "A<(B!=C)>", "A<(B==C)>"] {
        let last = text.rfind('>').expect("closed");
        assert_eq!(delimited(text), [(1, last)], "{text}");
    }
    assert_eq!(delimited("a<b || c>d"), [], "a disjunction of comparisons");
}

#[test]
fn a_shift_closing_two_lists_reads_as_two_ends() {
    use Kind::*;
    assert_eq!(
        kinds("array<vec4<f32>>"),
        [
            Ident,
            TemplateStart,
            Ident,
            TemplateStart,
            Ident,
            TemplateEnd,
            TemplateEnd
        ]
    );
    assert_eq!(
        kinds("var x: vec2<f32>=y;"),
        [
            Var,
            Ident,
            Colon,
            Ident,
            TemplateStart,
            Ident,
            TemplateEnd,
            Equal,
            Ident,
            Semicolon
        ]
    );
    assert_eq!(kinds("a >> b"), [Ident, ShiftRight, Ident]);
    assert_eq!(kinds("x<<y"), [Ident, ShiftLeft, Ident]);
    assert_eq!(kinds("x<=y"), [Ident, LessEqual, Ident]);
    assert_eq!(
        kinds("a < b; c > d"),
        [Ident, Less, Ident, Semicolon, Ident, Greater, Ident],
        "a semicolon ends every candidate"
    );
}

#[test]
fn a_literal_ending_in_a_letter_begins_no_template_list() {
    use Kind::*;
    assert_eq!(kinds("1.0f<a>b"), [Float, Less, Ident, Greater, Ident]);
}

#[test]
fn every_syntactic_token_the_specification_lists_reads_as_one_token() {
    let listed = [
        "!", "!=", "%", "%=", "&&", "&", "&=", "(", ")", "*", "*=", "+", "++", "+=", ",", "-",
        "--", "-=", "->", ".", "/", "/=", ":", ";", "<", "<<", "<<=", "<=", "=", "==", ">", ">=",
        ">>", ">>=", "@", "[", "]", "^", "^=", "_", "{", "|", "|=", "||", "}", "~",
    ];
    for spelling in listed {
        assert_eq!(spelled(spelling), [spelling], "{spelling}");
    }
}
