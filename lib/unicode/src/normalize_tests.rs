//! Normalization held to the Unicode conformance file, and to its buffer.

extern crate std;

use std::vec;
use std::vec::Vec;

use super::{normalize, normalize_in, Form, Overflow, Repertoire};
use crate::MAX_DECOMPOSITION;

/// `text` in `form`, into a buffer as long as any normalization of it needs.
fn normalized(text: &[char], form: Form) -> Vec<char> {
    let mut out = vec!['\0'; text.len() * MAX_DECOMPOSITION];
    let len = normalize(text.iter().copied(), form, &mut out).expect("room enough");
    out.truncate(len);
    out
}

fn chars(field: &str) -> Vec<char> {
    field
        .split_whitespace()
        .map(|hex| {
            u32::from_str_radix(hex, 16)
                .ok()
                .and_then(char::from_u32)
                .expect("a scalar value")
        })
        .collect()
}

/// Every case of `NormalizationTest.txt`, UAX #15's conformance file, each column through
/// every form as the file's header states, and every code point its first
/// part does not list unchanged by all four.
#[test]
fn every_case_of_the_conformance_file_normalizes_as_it_says() {
    let mut listed = vec![false; 0x11_0000];
    let mut part = None;
    let mut cases = 0;
    for line in include_str!("../assets/NormalizationTest.txt").lines() {
        if let Some(rest) = line.strip_prefix("@Part") {
            part = rest.split_whitespace().next();
            continue;
        }
        let data = line.split('#').next().unwrap_or_default().trim();
        if data.is_empty() {
            continue;
        }
        let columns: Vec<Vec<char>> = data.split(';').take(5).map(chars).collect();
        let [c1, c2, c3, c4, c5] = &columns[..] else {
            panic!("five columns: {line}");
        };
        if part == Some("1") {
            listed[u32::from(c1[0]) as usize] = true;
        }
        let expected = [
            (Form::Nfc, [c2, c2, c2, c4, c4]),
            (Form::Nfd, [c3, c3, c3, c5, c5]),
            (Form::Nfkc, [c4; 5]),
            (Form::Nfkd, [c5; 5]),
        ];
        for (form, outcomes) in expected {
            for (input, outcome) in [c1, c2, c3, c4, c5].into_iter().zip(outcomes) {
                assert_eq!(&normalized(input, form), outcome, "{form:?} of {line}");
            }
        }
        cases += 1;
    }
    assert!(cases > 19_000, "the whole file was read: {cases}");
    for (code, _) in listed.iter().enumerate().filter(|(_, listed)| !**listed) {
        let Some(ch) = u32::try_from(code).ok().and_then(char::from_u32) else {
            continue;
        };
        for form in [Form::Nfc, Form::Nfd, Form::Nfkc, Form::Nfkd] {
            assert_eq!(normalized(&[ch], form), [ch], "{form:?} of U+{code:04X}");
        }
    }
}

#[test]
fn nothing_normalizes_to_nothing() {
    for form in [Form::Nfc, Form::Nfd, Form::Nfkc, Form::Nfkd] {
        assert_eq!(normalize("".chars(), form, &mut []), Ok(0));
        assert_eq!(normalize("a".chars(), form, &mut []), Err(Overflow));
    }
}

/// U+FDFA is the longest decomposition there is, so the bound is exact.
#[test]
fn the_longest_decomposition_fills_its_bound_exactly() {
    let mut out = ['\0'; MAX_DECOMPOSITION];
    assert_eq!(
        normalize("\u{fdfa}".chars(), Form::Nfkd, &mut out),
        Ok(MAX_DECOMPOSITION)
    );
    assert_eq!(
        normalize(
            "\u{fdfa}".chars(),
            Form::Nfkd,
            &mut out[..MAX_DECOMPOSITION - 1]
        ),
        Err(Overflow)
    );
    assert_eq!(
        normalize("\u{fdfa}".chars(), Form::Nfd, &mut out[..1]),
        Ok(1),
        "it has no canonical decomposition"
    );
}

#[test]
fn hangul_decomposes_and_composes_by_arithmetic() {
    let lvt: Vec<char> = "\u{d4db}".chars().collect();
    let jamo = ['\u{1111}', '\u{1171}', '\u{11b6}'];
    assert_eq!(normalized(&lvt, Form::Nfd), jamo);
    assert_eq!(normalized(&jamo, Form::Nfc), lvt);
    assert_eq!(
        normalized(&['\u{1100}', '\u{1161}'], Form::Nfc),
        ['\u{ac00}'],
        "an LV syllable"
    );
    assert_eq!(
        normalized(&['\u{ac00}', '\u{11a7}'], Form::Nfc),
        ['\u{ac00}', '\u{11a7}'],
        "U+11A7 is no trailing consonant"
    );
}

/// A long run of combining marks in reverse class order is put in order
/// and composed with its base as far as nothing blocks it.
#[test]
fn a_long_run_of_marks_is_ordered_and_composed() {
    let mut text = vec!['a'];
    for _ in 0..200 {
        text.extend(['\u{301}', '\u{323}']);
    }
    let decomposed = normalized(&text, Form::Nfd);
    assert_eq!(decomposed.len(), text.len());
    assert!(decomposed[1..201].iter().all(|&mark| mark == '\u{323}'));
    assert!(decomposed[201..].iter().all(|&mark| mark == '\u{301}'));
    let composed = normalized(&text, Form::Nfc);
    assert_eq!(composed[0], '\u{1ea1}', "a with dot below, the first mark");
    assert_eq!(composed.len(), text.len() - 1);
}

/// What stringprep's Unicode 3.2 does not assign is inert, and the five
/// decompositions corrected since are 3.2's own.
#[test]
fn the_stringprep_repertoire_is_unicode_3_2() {
    let prepared = |text: &str, form: Form| {
        let mut out = vec!['\0'; text.chars().count() * MAX_DECOMPOSITION];
        let len = normalize_in(text.chars(), form, Repertoire::Stringprep, &mut out)
            .expect("room enough");
        out.truncate(len);
        out
    };
    let current = |text: &str, form: Form| normalized(&text.chars().collect::<Vec<_>>(), form);
    // Assigned in Unicode 5.2.
    assert_eq!(prepared("\u{1f100}", Form::Nfkc), ['\u{1f100}']);
    assert_eq!(current("\u{1f100}", Form::Nfkc), ['0', '.']);
    // Assigned in Unicode 5.0 with class 220: under 3.2 a starter, so the
    // acute after it composes with nothing.
    assert_eq!(
        prepared("a\u{1dca}\u{301}", Form::Nfc),
        ['a', '\u{1dca}', '\u{301}']
    );
    assert_eq!(
        current("a\u{1dca}\u{301}", Form::Nfc),
        ['\u{e1}', '\u{1dca}']
    );
    // Corrigendum #4's five.
    assert_eq!(prepared("\u{2f868}", Form::Nfkc), ['\u{2136a}']);
    assert_eq!(current("\u{2f868}", Form::Nfkc), ['\u{36fc}']);
    assert_eq!(prepared("\u{2f9bf}", Form::Nfd), ['\u{4d57}']);
    // Corrigendum #3's fix was 3.2's own.
    assert_eq!(prepared("\u{f951}", Form::Nfkc), ['\u{964b}']);
}
