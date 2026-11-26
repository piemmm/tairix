use alloc::vec::Vec;

use super::*;

fn words_of(line: &[u8]) -> Vec<Vec<u8>> {
    parse(line).expect("parsed").words
}

fn file(stream: Standard, name: &[u8], append: bool) -> Redirection {
    Redirection::File {
        stream,
        name: name.to_vec(),
        append,
    }
}

#[test]
fn words_part_at_spaces_and_a_double_quote_toggles_quoting_anywhere() {
    assert_eq!(
        words_of(b"Prog one \"two three\" fo\"ur fi\"ve"),
        [&b"Prog"[..], b"one", b"two three", b"four five"]
    );
    assert_eq!(
        words_of(b"Prog 'six seven'"),
        [&b"Prog"[..], b"'six", b"seven'"],
        "single quotes are ordinary"
    );
    assert_eq!(
        words_of(b"Prog \"ten\"\"eleven\" z"),
        [&b"Prog"[..], b"teneleven", b"z"]
    );
    assert_eq!(
        words_of(b"Prog  lead  \"\" trail "),
        [&b"Prog"[..], b"lead", b"", b"trail"],
        "an empty quoted word"
    );
    assert_eq!(
        words_of(b"Prog a b\0ignored"),
        [&b"Prog"[..], b"a", b"b"],
        "to the first control character"
    );
}

#[test]
fn an_unclosed_quote_is_refused_and_a_backslash_escapes_nothing() {
    assert_eq!(
        parse(b"Prog \"unterminated x"),
        Err(Malformed::MissingQuote)
    );
    assert_eq!(parse(b"Prog eight\\\"nine x"), Err(Malformed::MissingQuote));
}

#[test]
fn a_word_starting_with_a_redirection_is_one_its_name_joined_or_next() {
    let line = parse(b"Prog x >o1 y 2>e1 z").expect("parsed");
    assert_eq!(line.words, [&b"Prog"[..], b"x", b"y", b"z"]);
    assert_eq!(
        line.redirections,
        [
            file(Standard::Output, b"o1", false),
            file(Standard::Error, b"e1", false)
        ]
    );
    let line = parse(b"Prog <i1 x >>o2 2>&1 y").expect("parsed");
    assert_eq!(line.words, [&b"Prog"[..], b"x", b"y"]);
    assert_eq!(
        line.redirections,
        [
            file(Standard::Input, b"i1", false),
            file(Standard::Output, b"o2", true),
            Redirection::ErrorToOutput
        ]
    );
    let line = parse(b"Prog > o3 a < i1 b").expect("parsed");
    assert_eq!(line.words, [&b"Prog"[..], b"a", b"b"]);
    assert_eq!(
        line.redirections,
        [
            file(Standard::Output, b"o3", false),
            file(Standard::Input, b"i1", false)
        ]
    );
}

#[test]
fn the_numbered_and_combined_forms_and_what_is_not_a_redirection() {
    let redirected = |line: &[u8]| parse(line).expect("parsed").redirections;
    assert_eq!(
        redirected(b"Prog >&o1 x"),
        [file(Standard::OutputAndError, b"o1", false)]
    );
    assert_eq!(
        redirected(b"Prog 1>o1 x"),
        [file(Standard::Output, b"o1", false)]
    );
    assert_eq!(
        redirected(b"Prog 0<i1 x"),
        [file(Standard::Input, b"i1", false)]
    );
    assert_eq!(
        redirected(b"Prog 2>>e1 x"),
        [file(Standard::Error, b"e1", true)]
    );
    assert_eq!(redirected(b"Prog x >"), [], "a trailing operator dropped");
    assert_eq!(
        words_of(b"Prog a>o1 b"),
        [&b"Prog"[..], b"a>o1", b"b"],
        "not at the word's start"
    );
    assert_eq!(
        words_of(b"Prog \"quoted >o1\" c"),
        [&b"Prog"[..], b"quoted >o1", b"c"],
        "quoted"
    );
    assert_eq!(parse(b"Prog 3>e1 x"), Err(Malformed::IllegalRedirection));
    assert_eq!(parse(b"Prog 2<e1 x"), Err(Malformed::IllegalRedirection));
}
