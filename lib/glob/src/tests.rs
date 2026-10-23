use alloc::collections::BTreeSet;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::compile::{GroupOp, Node};
use crate::{
    may_hold_wildcards, GlobError, Mark, Options, Pattern, Scratch, MAX_CLASS_ITEMS,
    MAX_EXTENDED_LEN, MAX_GROUPS, MAX_GROUP_DEPTH, MAX_PATTERN_LEN, MAX_TOKENS,
};

fn matches(pattern: &str, candidate: &str) -> bool {
    Pattern::new(pattern)
        .expect("pattern compiles")
        .matches(candidate)
}

fn matches_as(pattern: &str, options: Options, candidate: &str) -> bool {
    Pattern::with_options(pattern, options)
        .expect("pattern compiles")
        .matches(candidate)
}

const EXTENDED: Options = Options::EXTENDED;
const PERIOD: Options = Options::PERIOD;
const NOCASE: Options = Options::CASE_INSENSITIVE;

#[test]
fn empty_pattern_matches_only_empty() {
    assert!(matches("", ""));
    assert!(!matches("", "x"));
}

#[test]
fn literal_is_anchored() {
    assert!(matches("readme.md", "readme.md"));
    assert!(!matches("readme.md", "readme.markdown"));
    assert!(!matches("readme.md", "areadme.md"));
}

#[test]
fn question_mark_matches_exactly_one() {
    assert!(matches("f?o", "foo"));
    assert!(!matches("f?o", "fo"));
    assert!(!matches("f?o", "fooo"));
}

#[test]
fn star_matches_any_run_including_empty() {
    assert!(matches("*", ""));
    assert!(matches("*", "anything at all"));
    assert!(matches("*.rs", "lib.rs"));
    assert!(matches("a*z", "az"));
    assert!(!matches("a*z", "abc"));
    assert!(matches("a**b", "axxxb"));
}

#[test]
fn star_gives_back_what_a_later_literal_needs() {
    assert!(matches("*.tar.gz", "archive.tar.gz"));
    assert!(matches("*abc*abc*", "xxabcyyabczz"));
    assert!(!matches("*abc*abc*", "xxabcyy"));
}

#[test]
fn bracket_members_ranges_and_negation() {
    assert!(matches("[abc]", "b"));
    assert!(!matches("[abc]", "d"));
    assert!(matches("img_[0-9][0-9].png", "img_42.png"));
    assert!(matches("[!0-9]", "a"));
    assert!(!matches("[!0-9]", "5"));
    assert!(matches("[^abc]", "d"));
    assert!(!matches("[^abc]", "a"));
}

#[test]
fn bracket_literal_close_and_dash() {
    assert!(matches("[]]", "]"));
    assert!(matches("[]a]", "a"));
    assert!(matches("[!]]", "x"));
    assert!(!matches("[!]]", "]"));
    assert!(matches("[a-]", "-"));
    assert!(matches("[-a]", "-"));
    assert!(!matches("[a-]", "b"));
}

#[test]
fn escapes_hold_inside_brackets() {
    assert!(matches(r"[\]a]", "]"));
    assert!(matches(r"[a\-c]", "-"));
    assert!(!matches(r"[a\-c]", "b"), "an escaped `-` is no range");
    assert!(matches(r"[\!a]", "!"));
    assert!(!matches(r"[\!a]", "b"), "an escaped `!` does not negate");
}

#[test]
fn escaping_metacharacters() {
    assert!(matches(r"\*", "*"));
    assert!(!matches(r"\*", "x"));
    assert!(matches(r"a\?b", "a?b"));
    assert!(matches(r"\[", "["));
}

#[test]
fn an_unclosed_bracket_is_an_ordinary_character() {
    assert!(matches("[", "["));
    assert!(matches("[abc", "[abc"));
    assert!(matches("[!]", "[!]"));
    assert!(matches("[*", "[anything"));
    assert!(!matches("[*", "anything"));
    // The outer `[` never closes, so it is literal and the inner one opens a
    // three-member expression.
    assert!(matches("[[:alpha:]", "[a"));
}

#[test]
fn posix_classes() {
    assert!(matches("[[:alpha:]]", "q"));
    assert!(matches("[[:alpha:]]", "é"));
    assert!(!matches("[[:alpha:]]", "1"));
    assert!(matches("[[:digit:]]", "7"));
    assert!(
        !matches("[[:digit:]]", "٣"),
        "digit is ASCII in every locale"
    );
    assert!(matches("[[:alnum:]_]*", "_tmp9"));
    assert!(matches("[[:upper:]]", "Q"));
    assert!(!matches("[[:upper:]]", "q"));
    assert!(matches("[[:lower:]]", "q"));
    assert!(matches("[[:space:]]", "\t"));
    assert!(matches("[[:blank:]]", " "));
    assert!(!matches("[[:blank:]]", "\n"));
    assert!(matches("[[:punct:]]", "!"));
    assert!(!matches("[[:punct:]]", "a"));
    assert!(matches("[[:cntrl:]]", "\u{07}"));
    assert!(matches("[[:print:]]", " "));
    assert!(!matches("[[:graph:]]", " "));
    assert!(matches("[[:xdigit:]]", "F"));
    assert!(!matches("[[:xdigit:]]", "g"));
    assert!(matches("[[:word:]]", "_"));
    assert!(matches("[[:ascii:]]", "~"));
    assert!(!matches("[[:ascii:]]", "é"));
    assert!(matches("[![:digit:]]", "x"));
}

#[test]
fn equivalence_classes_and_collating_symbols() {
    assert!(matches("[[=b=]]*", "b.txt"));
    assert!(!matches("[[=b=]]*", "a.txt"));
    assert!(matches("[[.space.]]x", " x"));
    assert!(matches("[[.hyphen.]]", "-"));
    assert!(matches("[[.a.]-[.c.]]", "b"));
    assert!(matches("[[.-.]]", "-"));
}

#[test]
fn malformed_patterns_fail_closed() {
    assert_eq!(Pattern::new("[z-a]"), Err(GlobError::ReversedRange));
    assert_eq!(Pattern::new(r"trailing\"), Err(GlobError::DanglingEscape));
    assert_eq!(Pattern::new("[[:alhpa:]]"), Err(GlobError::UnknownClass));
    assert_eq!(
        Pattern::new("[[.nonesuch.]]"),
        Err(GlobError::UnknownCollatingElement)
    );
    assert_eq!(
        Pattern::new("[[=ab=]]"),
        Err(GlobError::UnknownCollatingElement)
    );
    // A fault inside an expression that never closes is no fault: the `[`
    // is an ordinary character there.
    assert!(Pattern::new("[z-a").is_ok());
    assert!(Pattern::new("[[:alhpa:]").is_ok());
}

#[test]
fn case_insensitive_folds_literals_and_ranges_but_not_classes() {
    assert!(matches_as("*.TXT", NOCASE, "notes.txt"));
    assert!(matches_as("[B-C]*", NOCASE, "c.txt"));
    assert!(matches_as("[b-c]*", NOCASE, "C.TXT"));
    assert!(matches_as("ÉTÉ", NOCASE, "été"));
    // Classes test the character as written, as bash's do.
    assert!(!matches_as("[[:upper:]]", NOCASE, "a"));
    assert!(matches_as("[[:upper:]]", NOCASE, "A"));
    assert!(!matches("*.TXT", "notes.txt"));
    // A literal component folds through its fast path too.
    let literal = Pattern::with_options("ReadMe", NOCASE).expect("compiles");
    assert!(literal.matches("README"));
}

#[test]
fn a_spelled_range_stays_valid_whatever_folding_makes_of_it() {
    // `Z-a` is in order as written; folded it would run backwards, which
    // must leave an empty range rather than refuse the pattern.
    let pattern = Pattern::with_options("[Z-a]", NOCASE).expect("valid as spelled");
    assert!(!pattern.matches("q"));
    assert!(matches("[Z-a]", "_"));
}

#[test]
fn the_period_rule_protects_a_leading_dot() {
    for pattern in ["*", "?*", "[.]*", "[!a]*", "*rc"] {
        assert!(
            !matches_as(pattern, PERIOD, ".bashrc"),
            "{pattern} matched a dotfile"
        );
    }
    assert!(matches_as(".*", PERIOD, ".bashrc"));
    assert!(matches_as(r"\.*", PERIOD, ".bashrc"));
    assert!(
        matches_as("*", PERIOD, "a.b"),
        "only a leading dot is guarded"
    );
    assert!(matches("*", ".bashrc"), "no rule without the option");
}

#[test]
fn pathname_mode_matches_component_by_component() {
    let path = Options::PATHNAME;
    assert!(matches_as("a/*/c", path, "a/b/c"));
    assert!(!matches_as("*", path, "a/b"));
    assert!(!matches_as("a?b", path, "a/b"));
    assert!(matches_as("a/b", path, "a/b"));
    // A bracket cannot hold `/`: split first, the `[` is literal.
    assert!(matches_as("[a/b]", path, "[a/b]"));
    assert!(matches_as("a/", path, "a/"));
    assert!(!matches_as("a/", path, "a"));
    let hidden = Options::PATHNAME.union(Options::PERIOD);
    assert!(!matches_as("dir/*", hidden, "dir/.secret"));
    assert!(matches_as("dir/.*", hidden, "dir/.secret"));
}

#[test]
fn the_separator_is_ordinary_outside_pathname_mode() {
    assert!(matches("a/*/c", "a/b/c"));
    assert!(matches("*", "a/b/c"));
}

#[test]
fn extended_operators() {
    let cases: &[(&str, &str, bool)] = &[
        ("@(a|b)", "a", true),
        ("@(a|b)", "ab", false),
        ("?(a)x", "x", true),
        ("?(a)x", "ax", true),
        ("?(a)x", "aax", false),
        ("*(ab)", "", true),
        ("*(ab)", "ababab", true),
        ("*(ab)", "aba", false),
        ("+(ab)", "", false),
        ("+(ab)", "abab", true),
        ("!(*.o)", "main.c", true),
        ("!(*.o)", "main.o", false),
        ("!(x)", "", true),
        ("*.@(jpg|png)", "cat.png", true),
        ("*.@(jpg|png)", "cat.gif", false),
        ("+([0-9]).txt", "123.txt", true),
        ("+([0-9]).txt", ".txt", false),
        ("a@(|-b)", "a", true),
        ("a@(|-b)", "a-b", true),
        ("@()", "", true),
        ("!()", "x", true),
        ("!()", "", false),
        ("*(a|b*(c))", "abccbcc", true),
        ("@(a(b)c)", "a(b)c", true),
        ("@([|]|x)", "|", true),
        ("@([)]|x)", ")", true),
        ("x@(a\\|b)", "xa|b", true),
    ];
    for &(pattern, candidate, expected) in cases {
        assert_eq!(
            matches_as(pattern, EXTENDED, candidate),
            expected,
            "{pattern} against {candidate:?}"
        );
    }
}

#[test]
fn extended_operators_are_ordinary_without_the_option() {
    assert!(matches("@(a|b)", "@(a|b)"));
    assert!(matches("!(x)", "!(x)"));
    // `*` and `?` keep their basic meaning.
    assert!(matches("*(x)", "anything(x)"));
}

#[test]
fn extended_negation_and_the_period_rule() {
    let options = Options::EXTENDED.union(Options::PERIOD);
    // A negation never consumes a guarded leading dot, as bash's does not.
    assert!(!matches_as("!(.foo)", options, ".bar"));
    assert!(matches_as("!(.foo)", options, "bar"));
    assert!(matches_as("@(.dot|b)", options, ".dot"));
    assert!(matches_as("*(.)dot", options, ".dot"));
    assert!(matches_as("?(.)dot", options, ".dot"));
    assert!(!matches_as("@(*)", options, ".x"));
}

#[test]
fn malformed_extended_patterns_fail_closed() {
    assert_eq!(
        Pattern::with_options("@(a|b", EXTENDED),
        Err(GlobError::UnterminatedGroup)
    );
    let deep = format!(
        "{}x{}",
        "@(".repeat(MAX_GROUP_DEPTH + 1),
        ")".repeat(MAX_GROUP_DEPTH + 1)
    );
    assert_eq!(
        Pattern::with_options(&deep, EXTENDED),
        Err(GlobError::TooDeep)
    );
    let at_depth = format!(
        "{}x{}",
        "@(".repeat(MAX_GROUP_DEPTH),
        ")".repeat(MAX_GROUP_DEPTH)
    );
    assert!(Pattern::with_options(&at_depth, EXTENDED).is_ok());
    let many: String = "@(a)".repeat(MAX_GROUPS + 1);
    assert_eq!(
        Pattern::with_options(&many, EXTENDED),
        Err(GlobError::TooManyGroups)
    );
    assert!(Pattern::with_options(&"@(a)".repeat(MAX_GROUPS), EXTENDED).is_ok());
}

#[test]
fn an_extended_component_matches_names_up_to_the_bound() {
    let pattern = Pattern::with_options("@(*)", EXTENDED).expect("compiles");
    let longest: String = "a".repeat(MAX_EXTENDED_LEN);
    assert!(pattern.matches(&longest));
    assert!(!pattern.matches(&format!("{longest}a")));
    // A plain pattern has no such bound.
    assert!(matches("*", &"a".repeat(MAX_EXTENDED_LEN * 4)));
}

#[test]
fn marked_text_keeps_quoted_characters_literal() {
    let marked: Vec<Mark> = "*\\[a]"
        .chars()
        .enumerate()
        .map(|(at, ch)| Mark {
            ch,
            literal: at == 2 || at == 3,
        })
        .collect();
    let pattern = Pattern::from_marked(&marked, Options::empty()).expect("compiles");
    // `*` is a wildcard; the marked `\` and `[` stand for themselves.
    assert!(pattern.matches("x\\[a]"));
    assert!(!pattern.matches("xa"));
}

#[test]
fn components_report_literals_and_the_double_star() {
    let path = Options::PATHNAME;
    let pattern = Pattern::with_options(r"src/**/\*.rs/[x", path).expect("compiles");
    let components = pattern.components();
    assert_eq!(components.len(), 4);
    assert_eq!(components[0].literal(), Some("src"));
    assert!(components[1].is_double_star());
    assert_eq!(components[1].literal(), None);
    assert_eq!(components[2].literal(), Some("*.rs"));
    assert_eq!(components[3].literal(), Some("[x"));
    assert!(pattern.has_wildcards());
    assert!(!Pattern::with_options(r"a/\*/[b", path)
        .expect("compiles")
        .has_wildcards());
}

#[test]
fn pattern_length_bound_is_enforced() {
    let ok: String = "*".repeat(MAX_PATTERN_LEN);
    assert!(Pattern::new(&ok).is_ok());
    let too_long: String = "*".repeat(MAX_PATTERN_LEN + 1);
    assert_eq!(Pattern::new(&too_long), Err(GlobError::PatternTooLong));
    let marked: Vec<Mark> = too_long
        .chars()
        .map(|ch| Mark { ch, literal: false })
        .collect();
    assert_eq!(
        Pattern::from_marked(&marked, Options::empty()),
        Err(GlobError::PatternTooLong)
    );
}

#[test]
fn token_count_bound_is_enforced() {
    assert!(Pattern::new(&"?".repeat(MAX_TOKENS)).is_ok());
    assert_eq!(
        Pattern::new(&"?".repeat(MAX_TOKENS + 1)),
        Err(GlobError::TooManyTokens)
    );
}

/// A run of unclosed `[` before collating-element openers once cost time
/// cubic in the pattern's length to refuse; it is refused in linear time.
#[test]
fn a_run_of_unclosed_brackets_is_refused_by_the_token_bound() {
    let pattern = alloc::format!("{}{}", "[".repeat(1000), "[:".repeat(1548));
    assert_eq!(pattern.len(), MAX_PATTERN_LEN);
    assert_eq!(Pattern::new(&pattern), Err(GlobError::TooManyTokens));
}

#[test]
fn class_item_bound_is_enforced() {
    let mut body = String::from("[");
    for _ in 0..=MAX_CLASS_ITEMS {
        body.push('a');
    }
    body.push(']');
    assert_eq!(Pattern::new(&body), Err(GlobError::TooManyTokens));
}

#[test]
fn unicode_candidates() {
    assert!(matches("caf?", "café"));
    assert!(matches("*é", "café"));
    assert!(matches("[α-ω]", "β"));
}

#[test]
fn error_display_is_human_readable() {
    for err in [
        GlobError::PatternTooLong,
        GlobError::UnknownClass,
        GlobError::UnterminatedGroup,
    ] {
        assert!(!format!("{err}").is_empty());
    }
}

/// The positions `nodes` can end at from `start`, straight from the
/// definitions: exponential, and so only for short inputs, but independent
/// of the engine's sets, masks, memo and closure.
fn reference(nodes: &[Node], text: &[char], start: usize, dot0: bool) -> BTreeSet<usize> {
    let Some((head, rest)) = nodes.split_first() else {
        return BTreeSet::from([start]);
    };
    let guarded = |at: usize| at == 0 && dot0;
    let mut ends = BTreeSet::new();
    let continue_from = |mid: usize, ends: &mut BTreeSet<usize>| {
        ends.extend(reference(rest, text, mid, dot0));
    };
    match head {
        Node::Literal { ch, .. } => {
            if text.get(start) == Some(ch) {
                continue_from(start + 1, &mut ends);
            }
        }
        Node::AnyOne { .. } => {
            if start < text.len() && !guarded(start) {
                continue_from(start + 1, &mut ends);
            }
        }
        Node::Bracket { set, .. } => {
            if let Some(&c) = text.get(start) {
                if !guarded(start) && set.contains(c, c) {
                    continue_from(start + 1, &mut ends);
                }
            }
        }
        Node::AnySequence => {
            continue_from(start, &mut ends);
            if !guarded(start) {
                for mid in start + 1..=text.len() {
                    continue_from(mid, &mut ends);
                }
            }
        }
        Node::Group { op, alts, .. } => {
            let once = |from: usize| -> BTreeSet<usize> {
                alts.iter()
                    .flat_map(|alt| reference(alt, text, from, dot0))
                    .collect()
            };
            let mut reached = BTreeSet::new();
            match op {
                GroupOp::ExactlyOne => reached = once(start),
                GroupOp::ZeroOrOne => {
                    reached = once(start);
                    reached.insert(start);
                }
                GroupOp::ZeroOrMore | GroupOp::OneOrMore => {
                    let mut frontier: Vec<usize> = if *op == GroupOp::ZeroOrMore {
                        reached.insert(start);
                        Vec::from([start])
                    } else {
                        let first = once(start);
                        reached.extend(first.iter().copied());
                        first.into_iter().collect()
                    };
                    while let Some(at) = frontier.pop() {
                        for next in once(at) {
                            if reached.insert(next) {
                                frontier.push(next);
                            }
                        }
                    }
                }
                GroupOp::Not => {
                    if !guarded(start) {
                        let excluded = once(start);
                        reached = (start..=text.len())
                            .filter(|end| !excluded.contains(end))
                            .collect();
                    }
                }
            }
            for mid in reached {
                continue_from(mid, &mut ends);
            }
        }
    }
    ends
}

/// A small pseudo-random generator: the differential sweep below must be
/// reproducible without a seed source.
struct Lcg(u64);

impl Lcg {
    fn below(&mut self, bound: usize) -> usize {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        usize::try_from(self.0 >> 33).expect("fits") % bound
    }
}

/// A random extended pattern holding at most `groups` groups.
fn random_pattern(rng: &mut Lcg, depth: usize, groups: &mut usize) -> String {
    const ATOMS: &[&str] = &["a", "b", ".", "?", "*", "[ab]", "[!a]", "\\*"];
    let mut out = String::new();
    for _ in 0..=rng.below(4) {
        if depth < 3 && *groups > 0 && rng.below(4) == 0 {
            *groups -= 1;
            out.push(['?', '*', '+', '@', '!'][rng.below(5)]);
            out.push('(');
            for alt in 0..=rng.below(3) {
                if alt > 0 {
                    out.push('|');
                }
                out.push_str(&random_pattern(rng, depth + 1, groups));
            }
            out.push(')');
        } else {
            out.push_str(ATOMS[rng.below(ATOMS.len())]);
        }
    }
    out
}

/// The engine agrees with the definitions on thousands of random extended
/// patterns and short candidates, with and without the period rule.
#[test]
fn the_engine_agrees_with_the_reference_matcher() {
    let mut rng = Lcg(0x5eed_9a7e_2026);
    let mut scratch = Scratch::new();
    for _ in 0..4000 {
        let mut budget = MAX_GROUPS;
        let source = random_pattern(&mut rng, 0, &mut budget);
        let period = rng.below(2) == 0;
        let options = Options::EXTENDED.with(Options::PERIOD, period);
        let pattern = Pattern::with_options(&source, options).expect("generated patterns compile");
        let marks = crate::compile::marks_of(&source).expect("no dangling escape");
        assert!(
            !pattern.has_wildcards() || may_hold_wildcards(&marks, true),
            "{source}: the pre-check missed a wildcard"
        );
        let [component] = pattern.components() else {
            panic!("one component");
        };
        for _ in 0..8 {
            let len = rng.below(7);
            let text: String = (0..len)
                .map(|_| ['a', 'b', '.', '*'][rng.below(4)])
                .collect();
            let chars: Vec<char> = text.chars().collect();
            let dot0 = period && chars.first() == Some(&'.');
            let expected = reference(&component.nodes, &chars, 0, dot0).contains(&chars.len());
            assert_eq!(
                pattern.matches_with(&text, &mut scratch),
                expected,
                "{source} against {text:?} (period {period})"
            );
        }
    }
}
