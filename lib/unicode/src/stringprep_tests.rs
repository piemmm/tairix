//! SASLprep held to RFC 4013's examples and to each step of RFC 3454.

extern crate std;

use std::string::String;
use std::vec;
use std::vec::Vec;

use super::{saslprep, SaslprepError, Unassigned};
use crate::tables::{MAPPED_TO_NOTHING, NON_ASCII_SPACES};
use crate::{in_ranges, MAX_DECOMPOSITION};

fn prepared(text: &str, unassigned: Unassigned) -> Result<String, SaslprepError> {
    let mut out = vec!['\0'; text.chars().count() * MAX_DECOMPOSITION];
    let len = saslprep(text, unassigned, &mut out)?;
    Ok(out[..len].iter().collect())
}

fn query(text: &str) -> Result<String, SaslprepError> {
    prepared(text, Unassigned::Allowed)
}

/// RFC 4013 §3.
#[test]
fn the_rfcs_examples_prepare_as_it_says() {
    assert_eq!(query("I\u{ad}X").as_deref(), Ok("IX"), "soft hyphen");
    assert_eq!(query("user").as_deref(), Ok("user"));
    assert_eq!(query("USER").as_deref(), Ok("USER"), "case is kept");
    assert_eq!(query("\u{aa}").as_deref(), Ok("a"));
    assert_eq!(query("\u{2168}").as_deref(), Ok("IX"));
    assert_eq!(query("\u{7}"), Err(SaslprepError::Prohibited));
    assert_eq!(query("\u{627}\u{31}"), Err(SaslprepError::Bidi));
}

#[test]
fn spaces_become_plain_and_the_invisible_go() {
    assert_eq!(query("a\u{a0}b\u{3000}c").as_deref(), Ok("a b c"));
    assert_eq!(
        query("a\u{200b}b").as_deref(),
        Ok("ab"),
        "a zero-width space is mapped to nothing before it is a space"
    );
    assert_eq!(
        query("e\u{fe0f}").as_deref(),
        Ok("e"),
        "a variation selector"
    );
    assert_eq!(query("").as_deref(), Ok(""));
}

#[test]
fn compatibility_forms_are_normalized() {
    assert_eq!(query("\u{fb01}x").as_deref(), Ok("fix"), "a ligature");
    assert_eq!(query("\u{ff21}\u{ff22}").as_deref(), Ok("AB"), "full-width");
    assert_eq!(query("e\u{301}").as_deref(), Ok("\u{e9}"), "composed");
}

#[test]
fn what_saslprep_prohibits_is_refused() {
    for text in [
        "\u{0}",
        "a\u{7f}",
        "\u{85}",
        "\u{e000}",
        "\u{f0000}",
        "\u{fdd0}",
        "\u{ffff}",
        "\u{fffd}",
        "\u{2ff0}",
        "a\u{200e}",
        "\u{202e}",
        "\u{e0001}",
    ] {
        assert_eq!(query(text), Err(SaslprepError::Prohibited), "{text:?}");
    }
}

/// RFC 3454 §6.
#[test]
fn right_to_left_text_must_stand_alone_and_bound_the_string() {
    assert_eq!(
        query("\u{627}1\u{628}").as_deref(),
        Ok("\u{627}1\u{628}"),
        "a digit between two Arabic letters"
    );
    assert_eq!(query("\u{627}a\u{628}"), Err(SaslprepError::Bidi));
    assert_eq!(query("1\u{627}"), Err(SaslprepError::Bidi));
    assert_eq!(query("\u{5d0}").as_deref(), Ok("\u{5d0}"));
}

/// RFC 3454 §7: a query passes a code point Unicode 3.2 does not assign
/// untouched — not normalized as today's tables would — and a string to be
/// stored refuses it.
#[test]
fn a_code_point_unicode_3_2_does_not_assign_passes_a_query_alone() {
    assert_eq!(query("\u{1f100}").as_deref(), Ok("\u{1f100}"));
    assert_eq!(
        prepared("\u{1f100}", Unassigned::Refused),
        Err(SaslprepError::Unassigned)
    );
    assert_eq!(
        prepared("\u{2168}", Unassigned::Refused).as_deref(),
        Ok("IX")
    );
    assert_eq!(
        query("\u{2f868}").as_deref(),
        Ok("\u{2136a}"),
        "Unicode 3.2's own decomposition, before Corrigendum #4"
    );
}

#[test]
fn a_buffer_as_long_as_the_prepared_string_suffices() {
    for (text, result) in [
        ("I", "I"),
        ("\u{ac00}", "\u{ac00}"),
        ("e\u{301}", "\u{e9}"),
        ("I\u{ad}X", "IX"),
        ("\u{2168}", "IX"),
        ("a\u{a0}b", "a b"),
    ] {
        let result: Vec<char> = result.chars().collect();
        let mut exact = vec!['\0'; result.len()];
        assert_eq!(
            saslprep(text, Unassigned::Allowed, &mut exact),
            Ok(result.len()),
            "{text:?}"
        );
        assert_eq!(exact, result);
        let mut short = vec!['\0'; result.len() - 1];
        assert_eq!(
            saslprep(text, Unassigned::Allowed, &mut short),
            Err(SaslprepError::Overflow),
            "{text:?}"
        );
    }
}

/// One pass judges every character, and a prohibited or unassigned one
/// refuses the string before its bidirectional text can.
#[test]
fn a_refused_character_says_why_before_the_bidirectional_rules_do() {
    assert_eq!(query("\u{627}a\u{7}"), Err(SaslprepError::Prohibited));
    assert_eq!(query("\u{7}\u{627}a"), Err(SaslprepError::Prohibited));
    assert_eq!(
        prepared("\u{627}a\u{1f100}", Unassigned::Refused),
        Err(SaslprepError::Unassigned)
    );
    assert_eq!(
        prepared("\u{1f100}\u{7}", Unassigned::Refused),
        Err(SaslprepError::Unassigned),
        "the first refused character says why"
    );
}

/// RFC 3454 lists U+200B both among the characters it maps to nothing and
/// among the spaces, the only character it lists in both, which SASLprep maps
/// to nothing.
#[test]
fn the_zero_width_space_alone_is_both_mapped_to_nothing_and_a_space() {
    let both: Vec<u32> = MAPPED_TO_NOTHING
        .iter()
        .flat_map(|&(first, last)| first..=last)
        .filter(|&code| in_ranges(NON_ASCII_SPACES, code))
        .collect();
    assert_eq!(both, [0x200b]);
}
