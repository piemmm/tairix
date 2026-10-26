//! Unit tests for the property lookups, held to characters whose properties
//! the UCD states and to UTS #39's own examples.

use super::tables::{
    DECOMPOSED, DECOMPOSED_MARKS, DEFAULT_IGNORABLE, EXTENSIONS, EXTENSION_SETS, NONSPACING_MARKS,
    SCRIPTS,
};
use super::{
    augmented, decomposed_marks, is_default_ignorable, is_hidden, is_nonspacing_mark,
    restriction_level, script, Faithful, MarkRun, RestrictionLevel, Scripts, BOPO, CYRL, GREK,
    HANB, HANI, HNTL, JPAN, KORE, LATN, MAX_MARKS, VERSION,
};

#[test]
fn the_tables_are_unicode_18() {
    assert_eq!(VERSION, (18, 0, 0));
}

#[test]
fn default_ignorables_are_the_invisible_formatters() {
    for ch in [
        '\u{ad}',
        '\u{34f}',
        '\u{61c}',
        '\u{180e}',
        '\u{200b}',
        '\u{200d}',
        '\u{202e}',
        '\u{2066}',
        '\u{feff}',
        '\u{fe0f}',
        '\u{e0001}',
        '\u{e0fff}',
    ] {
        assert!(is_default_ignorable(ch), "U+{:04X}", u32::from(ch));
    }
    // Controls, separators and the interlinear annotation characters are
    // not default-ignorable: a renderer is not free to draw them as nothing.
    for ch in [
        'a',
        ' ',
        '\u{0}',
        '\u{7f}',
        '\u{2028}',
        '\u{fff9}',
        '\u{fffd}',
        '\u{10ffff}',
    ] {
        assert!(!is_default_ignorable(ch), "U+{:04X}", u32::from(ch));
    }
}

#[test]
fn what_hides_or_reorders_text_is_hidden() {
    for ch in [
        '\0',
        '\t',
        '\n',
        '\u{1b}',
        '\u{7f}',
        '\u{85}',
        '\u{9f}',
        '\u{ad}',
        '\u{34f}',
        '\u{61c}',
        '\u{17b4}',
        '\u{180e}',
        '\u{200b}',
        '\u{200d}',
        '\u{200e}',
        '\u{2028}',
        '\u{2029}',
        '\u{202a}',
        '\u{202e}',
        '\u{2060}',
        '\u{2066}',
        '\u{2069}',
        '\u{3164}',
        '\u{fe00}',
        '\u{fe0f}',
        '\u{feff}',
        '\u{fff9}',
        '\u{1bca0}',
        '\u{1d173}',
        '\u{e0001}',
        '\u{e007f}',
        '\u{e0100}',
        '\u{e01ef}',
        '\u{e0fff}',
    ] {
        assert!(is_hidden(ch), "U+{:04X} hides", u32::from(ch));
    }
    for ch in [
        ' ',
        'a',
        '~',
        '\u{a0}',
        '\u{e9}',
        '\u{301}',
        '\u{2013}',
        '\u{3000}',
        '\u{5b57}',
        '\u{ff0c}',
        '\u{fffd}',
        '\u{1f600}',
        '\u{e1000}',
    ] {
        assert!(!is_hidden(ch), "U+{:04X} is shown", u32::from(ch));
    }
}

#[test]
fn nonspacing_marks_are_mn_and_me_but_not_mc() {
    for ch in [
        '\u{300}',
        '\u{301}',
        '\u{36f}',
        '\u{5b0}',
        '\u{93c}',
        '\u{20dd}',
        '\u{e0100}',
    ] {
        assert!(is_nonspacing_mark(ch), "U+{:04X}", u32::from(ch));
    }
    // A spacing mark (Devanagari visarga), a letter, and ASCII.
    for ch in ['\u{903}', 'é', 'a', '^', '\u{2ff}'] {
        assert!(!is_nonspacing_mark(ch), "U+{:04X}", u32::from(ch));
    }
}

#[test]
fn scripts_and_their_augmented_sets() {
    assert_eq!(script(u32::from('A')), LATN);
    assert_eq!(script(u32::from('Ж')), CYRL);
    assert_eq!(script(u32::from('Ω')), GREK);
    assert_eq!(script(u32::from('漢')), HANI);
    // Han joins every writing system it is written in.
    let han = augmented('漢').expect("Han constrains");
    for system in [HANI, HANB, HNTL, JPAN, KORE] {
        assert!(han.contains(system));
    }
    // Bopomofo is written with Han, so augments to Hanb.
    assert!(augmented('ㄅ').expect("Bopomofo constrains").contains(BOPO));
    assert!(augmented('ㄅ').expect("Bopomofo constrains").contains(HANB));
    // Common and Inherited constrain nothing.
    assert_eq!(augmented('1'), None);
    assert_eq!(augmented('\u{20d0}'), None);
    // An Inherited mark with explicit extensions constrains to them: in
    // Unicode 18 the acute accent is written with Latin, Greek, Cyrillic and
    // five others, never with Han.
    let acute = augmented('\u{301}').expect("the acute accent has extensions");
    assert!(acute.contains(LATN) && acute.contains(GREK) && !acute.contains(HANI));
    // A Script_Extensions entry: U+0640 ARABIC TATWEEL is shared by several
    // scripts rather than Common.
    let tatweel = augmented('\u{640}').expect("tatweel has explicit extensions");
    assert!(!tatweel.contains(LATN));
}

#[test]
fn restriction_levels_follow_uts_39() {
    use RestrictionLevel::*;
    for (text, level) in [
        ("", AsciiOnly),
        ("circle", AsciiOnly),
        ("café", SingleScript),
        ("e\u{301}cole", SingleScript),
        ("ελληνικά", SingleScript),
        ("〆切", SingleScript),
        ("©®", SingleScript),
        ("Сirсlе", MinimallyRestrictive),
        ("Ωmega", MinimallyRestrictive),
        ("Toys-Я-Us", MinimallyRestrictive),
        ("HλLF-LIFE", MinimallyRestrictive),
        ("日本語とカタカナのabc", HighlyRestrictive),
        ("한국어abc", HighlyRestrictive),
        ("中文ㄅabc", HighlyRestrictive),
        ("abcالعربية", ModeratelyRestrictive),
        ("abcक", ModeratelyRestrictive),
        ("Ελληνικά-Русский", MinimallyRestrictive),
    ] {
        assert_eq!(restriction_level(text), level, "{text:?}");
    }
}

fn admits(text: &str) -> bool {
    let mut run = MarkRun::default();
    text.chars().all(|ch| run.admit(ch))
}

#[test]
fn a_sequence_carries_at_most_four_marks_and_none_twice() {
    assert!(admits("e\u{300}\u{301}\u{302}\u{303}"));
    assert!(!admits("e\u{300}\u{301}\u{302}\u{303}\u{304}"));
    assert!(!admits("e\u{301}\u{301}"), "a doubled mark draws as one");
    assert!(!admits("e\u{301}\u{300}\u{301}"), "however far apart");
    assert!(
        admits("e\u{301}\u{300}\u{302}a\u{301}\u{300}\u{302}"),
        "each base begins its own sequence"
    );
    // A spacing mark (Devanagari visarga) is no nonspacing mark, so it ends
    // the sequence like any other base.
    assert!(admits("\u{915}\u{93c}\u{903}\u{93c}"));
}

#[test]
fn a_precomposed_letter_counts_the_marks_it_decomposes_to() {
    assert!(admits("\u{e9}"));
    assert!(
        !admits("\u{e9}\u{301}"),
        "é then an acute repeats the acute"
    );
    // U+1EC7 decomposes to e, U+0323 and U+0302: two more fit, a third does not.
    assert!(admits("\u{1ec7}\u{301}\u{300}"));
    assert!(!admits("\u{1ec7}\u{301}\u{300}\u{304}"));
    assert!(!admits("\u{1ec7}\u{323}"));
    // A mark that decomposes to two counts as both.
    assert!(!admits("\u{3b9}\u{344}\u{308}"));
    // Gurung Khema's vowel sign U is two vowel signs AA in one glyph; only an
    // AA after it repeats one.
    assert!(admits("\u{16104}\u{16121}"));
    assert!(!admits("\u{16104}\u{16121}\u{1611e}"));
    assert_eq!(
        decomposed_marks('\u{344}'),
        Some(&['\u{308}', '\u{301}'][..])
    );
    assert_eq!(decomposed_marks('e'), None);
    assert_eq!(decomposed_marks('\u{5b57}'), None);
}

/// The positions of the characters a faithful rendering of `text` escapes.
fn escaped_at(text: &str) -> u32 {
    let mut faithful = Faithful::default();
    text.chars()
        .enumerate()
        .filter(|&(_, ch)| faithful.escapes(ch))
        .fold(0, |mask, (at, _)| mask | 1 << at)
}

#[test]
fn a_faithful_rendering_escapes_each_hidden_character_and_each_mark_past_the_limits() {
    assert_eq!(escaped_at("plain"), 0);
    assert_eq!(escaped_at("a\u{202e}b"), 1 << 1);
    assert_eq!(escaped_at("e\u{301}\u{301}\u{300}"), 1 << 2);
    assert_eq!(
        escaped_at("e\u{300}\u{301}\u{302}\u{303}\u{304}\u{305}"),
        1 << 5 | 1 << 6,
        "each mark past the fourth is escaped, not just the first"
    );
    assert_eq!(escaped_at("\u{e9}\u{301}x\u{301}"), 1 << 1);
}

#[test]
fn the_tables_are_sorted_disjoint_and_complete() {
    assert!(DECOMPOSED.windows(2).all(|pair| pair[0].0 < pair[1].0));
    for &(code, at, len) in DECOMPOSED {
        let at = usize::from(at);
        let marks = DECOMPOSED_MARKS
            .get(at..at + usize::from(len))
            .expect("in the pool");
        let ch = char::from_u32(code).expect("a scalar value");
        // Every decomposition must leave room in its own sequence.
        let mut run = MarkRun::default();
        assert!(run.admit(ch), "U+{code:04X} passes alone");
        assert!(!marks.is_empty() && marks.len() <= MAX_MARKS);
        assert!(marks.iter().all(|&mark| is_nonspacing_mark(mark)));
    }
    assert_eq!(SCRIPTS.first().map(|&(first, _)| first), Some(0));
    assert!(SCRIPTS
        .windows(2)
        .all(|pair| pair[0].0 < pair[1].0 && pair[0].1 != pair[1].1));
    for ranges in [DEFAULT_IGNORABLE, NONSPACING_MARKS] {
        assert!(ranges.iter().all(|&(first, last)| first <= last));
        assert!(ranges.windows(2).all(|pair| pair[0].1 + 1 < pair[1].0));
    }
    assert!(EXTENSIONS.windows(2).all(|pair| pair[0].1 < pair[1].0));
    assert!(EXTENSIONS
        .iter()
        .all(|&(first, last, set)| first <= last && usize::from(set) < EXTENSION_SETS.len()));
    assert!(EXTENSION_SETS
        .iter()
        .all(|&set| Scripts(set) != Scripts([0; super::SET_WORDS])));
}
