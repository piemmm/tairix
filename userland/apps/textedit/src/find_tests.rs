//! Unit tests for patterns, scans and searches, against a plain scan of a
//! byte vector as the model.

use alloc::vec::Vec;
use core::ops::{ControlFlow, Range};

use super::{
    parse_hex, scan, Options, Pattern, PatternError, Search, Step, MAX_PATTERN, MAX_REPLACEMENTS,
};
use crate::document::Document;

/// A document split into many small pieces, so matches straddle them.
fn doc(text: &[u8]) -> Document {
    Document::from_chunks(text.chunks(3).map(<[u8]>::to_vec).collect()).expect("loads")
}

fn exact() -> Options {
    Options {
        match_case: true,
        whole_word: false,
    }
}

fn all(document: &Document, pattern: &Pattern, apart: bool) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    scan(document, pattern, 0..document.len(), 0, apart, |hit| {
        out.push(hit);
        ControlFlow::Continue(())
    });
    out
}

/// Every start of `needle` in `text`, overlapping.
fn model(text: &[u8], needle: &[u8]) -> Vec<Range<usize>> {
    (0..=text.len().saturating_sub(needle.len()))
        .filter(|&at| text[at..].starts_with(needle))
        .map(|at| at..at + needle.len())
        .collect()
}

/// Run a search to its end with a small step budget.
fn run(search: &mut Search, document: &Document) -> Step {
    for _ in 0..10_000 {
        let step = search.step(document, 5);
        if step != Step::Partial {
            return step;
        }
    }
    panic!("the search never ends");
}

#[test]
fn an_exact_scan_finds_what_a_plain_scan_finds_across_pieces() {
    let text = b"abababa xab aab \xe4\xb8\xad ab";
    let document = doc(text);
    for needle in ["ab", "aba", "a", "\u{4e2d} a", "zz"] {
        let pattern = Pattern::text(needle, exact()).expect("a pattern");
        assert_eq!(
            all(&document, &pattern, false),
            model(text, needle.as_bytes()),
            "{needle:?}"
        );
    }
}

#[test]
fn apart_matches_never_overlap() {
    let document = doc(b"aaaaa");
    let pattern = Pattern::text("aa", exact()).expect("a pattern");
    assert_eq!(all(&document, &pattern, true), [0..2, 2..4]);
    assert_eq!(all(&document, &pattern, false), [0..2, 1..3, 2..4, 3..5]);
}

#[test]
fn ignoring_case_folds_letters_beyond_ascii() {
    let document = doc("Straße STRASSE straSSe Émile émile".as_bytes());
    let pattern = Pattern::text("émile", Options::default()).expect("a pattern");
    let found = all(&document, &pattern, true);
    assert_eq!(found.len(), 2);
    let ascii = Pattern::text("strasse", Options::default()).expect("a pattern");
    assert_eq!(
        all(&document, &ascii, true).len(),
        2,
        "folding is by character, not expansion"
    );
}

#[test]
fn whole_words_need_a_boundary_either_side() {
    let document = doc(b"cat concat cat_x cat. (cat)cat");
    let pattern = Pattern::text(
        "cat",
        Options {
            match_case: true,
            whole_word: true,
        },
    )
    .expect("a pattern");
    assert_eq!(
        all(&document, &pattern, true),
        [0..3, 17..20, 23..26, 27..30]
    );
}

#[test]
fn a_hex_pattern_matches_bytes_the_text_view_cannot_type() {
    let document = doc(b"\x00\x01\xff\xfe\x00\x01\xff");
    let pattern = Pattern::hex("00 01 ff").expect("a pattern");
    assert_eq!(all(&document, &pattern, true), [0..3, 4..7]);
    assert_eq!(
        parse_hex("DEADbeef"),
        Ok(alloc::vec![0xde, 0xad, 0xbe, 0xef])
    );
    assert_eq!(parse_hex("4 8"), Err(PatternError::NotHex));
    assert_eq!(parse_hex("123"), Err(PatternError::NotHex));
    assert_eq!(parse_hex("zz"), Err(PatternError::NotHex));
}

#[test]
fn an_empty_or_overlong_pattern_is_refused() {
    assert_eq!(Pattern::text("", exact()), Err(PatternError::Empty));
    let long = "x".repeat(MAX_PATTERN + 1);
    assert_eq!(Pattern::text(&long, exact()), Err(PatternError::TooLong));
    assert_eq!(Pattern::hex(""), Err(PatternError::Empty));
}

#[test]
fn next_and_previous_wrap_around_the_document() {
    let document = doc(b"one two one two one");
    let pattern = Pattern::text("one", exact()).expect("a pattern");
    assert_eq!(
        run(&mut Search::next(pattern.clone(), 1), &document),
        Step::Found(8..11)
    );
    assert_eq!(
        run(&mut Search::next(pattern.clone(), 17), &document),
        Step::Found(0..3),
        "wraps to the start"
    );
    assert_eq!(
        run(&mut Search::previous(pattern.clone(), 16), &document),
        Step::Found(8..11)
    );
    assert_eq!(
        run(&mut Search::previous(pattern.clone(), 0), &document),
        Step::Found(16..19),
        "wraps to the end"
    );
    let missing = Pattern::text("three", exact()).expect("a pattern");
    assert_eq!(
        run(&mut Search::next(missing.clone(), 4), &document),
        Step::Missing
    );
    assert_eq!(
        run(&mut Search::previous(missing, 4), &document),
        Step::Missing
    );
}

#[test]
fn a_search_reads_no_more_than_its_budget_a_step() {
    let mut text = alloc::vec![b'.'; 1000];
    text.extend_from_slice(b"needle");
    let document = doc(&text);
    let mut search = Search::next(Pattern::text("needle", exact()).expect("a pattern"), 0);
    let mut steps = 0;
    loop {
        steps += 1;
        match search.step(&document, 100) {
            Step::Partial => assert!(search.read() <= steps * 100),
            found => {
                assert_eq!(found, Step::Found(1000..1006));
                break;
            }
        }
    }
    assert_eq!(steps, 11);
}

#[test]
fn every_match_is_collected_apart_and_capped() {
    let document = doc(b"xx.xxx.x");
    let mut search = Search::all(Pattern::text("xx", exact()).expect("a pattern"));
    assert_eq!(
        run(&mut search, &document),
        Step::All {
            matches: alloc::vec![0..2, 3..5],
            more: false
        }
    );
    let many = alloc::vec![b'a'; MAX_REPLACEMENTS + 5];
    let document = Document::from_chunks(alloc::vec![many]).expect("loads");
    let mut search = Search::all(Pattern::text("a", exact()).expect("a pattern"));
    let Step::All { matches, more } = search.step(&document, usize::MAX) else {
        panic!("one step reads the whole document");
    };
    assert_eq!((matches.len(), more), (MAX_REPLACEMENTS, true));
}

#[test]
fn a_match_across_a_step_boundary_is_found_once() {
    let document = doc(b"....abcd....abcd");
    let mut search = Search::all(Pattern::text("abcd", exact()).expect("a pattern"));
    let found = loop {
        match search.step(&document, 6) {
            Step::Partial => {}
            Step::All { matches, .. } => break matches,
            other => panic!("{other:?}"),
        }
    };
    assert_eq!(found, [4..8, 12..16]);
}

#[test]
fn stepped_matches_apart_are_the_ones_one_pass_takes() {
    let text = b"aaaaaaaaaaa";
    let document = doc(text);
    for budget in 1..=text.len() {
        for needle in ["aa", "aaa"] {
            let pattern = Pattern::text(needle, exact()).expect("a pattern");
            let whole = all(&document, &pattern, true);
            let mut search = Search::all(Pattern::text(needle, exact()).expect("a pattern"));
            let stepped = loop {
                match search.step(&document, budget) {
                    Step::Partial => {}
                    Step::All { matches, .. } => break matches,
                    other => panic!("{other:?}"),
                }
            };
            assert_eq!(
                stepped, whole,
                "{needle:?} stepped {budget} bytes at a time"
            );
        }
    }
}

#[test]
fn the_selection_is_checked_as_a_match_in_place() {
    let document = doc(b"a cat concat");
    let words = Pattern::text(
        "cat",
        Options {
            match_case: false,
            whole_word: true,
        },
    )
    .expect("a pattern");
    assert!(words.is_match(&document, 2..5));
    assert!(!words.is_match(&document, 9..12), "inside a word");
    assert!(!words.is_match(&document, 2..4));
    let exact_cat = Pattern::text("cat", exact()).expect("a pattern");
    assert!(exact_cat.is_match(&document, 9..12));
}
