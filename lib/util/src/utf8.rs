//! Which bytes begin a UTF-8 sequence, and how long it is (Unicode 15.0,
//! Table 3-7), for the decoders that must step over or validate text by hand;
//! which characters must be escaped when text from elsewhere is shown; and the
//! letters a reader looks a name up by.

/// How many bytes the sequence `lead` begins, or `None` for a byte no
/// well-formed sequence begins with: a continuation byte, `0xC0`/`0xC1`
/// (always overlong), or `0xF5..=0xFF` (past U+10FFFF).
#[must_use]
pub const fn sequence_len(lead: u8) -> Option<usize> {
    match lead {
        0x00..=0x7f => Some(1),
        0xc2..=0xdf => Some(2),
        0xe0..=0xef => Some(3),
        0xf0..=0xf4 => Some(4),
        _ => None,
    }
}

/// The letter a reader looks `ch` up by: a Latin-1 letter folded to the
/// letter it is written on, and every letter in lower case, so `São` finds
/// and sorts beside `Sao`.
#[must_use]
pub fn fold(ch: char) -> char {
    match ch {
        'À'..='Å' | 'à'..='å' => 'a',
        'Ç' | 'ç' => 'c',
        'È'..='Ë' | 'è'..='ë' => 'e',
        'Ì'..='Ï' | 'ì'..='ï' => 'i',
        'Ñ' | 'ñ' => 'n',
        'Ò'..='Ö' | 'Ø' | 'ò'..='ö' | 'ø' => 'o',
        'Ù'..='Ü' | 'ù'..='ü' => 'u',
        'Ý' | 'ý' | 'ÿ' => 'y',
        other => other.to_lowercase().next().unwrap_or(other),
    }
}

/// `text` as a reader looks it up: each character [`fold`]ed.
pub fn folded(text: &str) -> impl Iterator<Item = char> + '_ {
    text.chars().map(fold)
}

/// The order a reader looks names up in: their letters [`folded`], then
/// their bytes, so distinct names never compare equal.
#[must_use]
pub fn reader_order(a: &str, b: &str) -> core::cmp::Ordering {
    folded(a).cmp(folded(b)).then_with(|| a.cmp(b))
}

/// Whether `ch` draws nothing where it stands yet can hide, reorder or break
/// the text around it: a C0 or C1 control, a line or paragraph separator, an
/// interlinear-annotation control, or a default-ignorable code point (Unicode
/// `DerivedCoreProperties.txt`) — the bidirectional controls, the zero-width
/// characters, the fillers, the variation selectors and the tags. Text from
/// elsewhere is shown with these escaped, so what is drawn is what is there.
#[must_use]
pub const fn is_hidden(ch: char) -> bool {
    matches!(
        ch,
        '\u{0}'..='\u{1f}'
            | '\u{7f}'..='\u{9f}'
            | '\u{ad}'
            | '\u{34f}'
            | '\u{61c}'
            | '\u{115f}'..='\u{1160}'
            | '\u{17b4}'..='\u{17b5}'
            | '\u{180b}'..='\u{180f}'
            | '\u{200b}'..='\u{200f}'
            | '\u{2028}'..='\u{202e}'
            | '\u{2060}'..='\u{206f}'
            | '\u{3164}'
            | '\u{fe00}'..='\u{fe0f}'
            | '\u{feff}'
            | '\u{ffa0}'
            | '\u{fff0}'..='\u{fffb}'
            | '\u{1bca0}'..='\u{1bca3}'
            | '\u{1d173}'..='\u{1d17a}'
            | '\u{e0000}'..='\u{e0fff}'
    )
}

/// `text` as it is shown where what it says must be what is drawn, as a
/// name is: each [`is_hidden`] character replaced by U+FFFD, so two names
/// that differ only by one never look alike. Borrowed when there is none.
#[must_use]
pub fn shown(text: &str) -> alloc::borrow::Cow<'_, str> {
    if !text.chars().any(is_hidden) {
        return alloc::borrow::Cow::Borrowed(text);
    }
    alloc::borrow::Cow::Owned(
        text.chars()
            .map(|ch| {
                if is_hidden(ch) {
                    char::REPLACEMENT_CHARACTER
                } else {
                    ch
                }
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use core::cmp::Ordering;

    use super::{fold, folded, is_hidden, reader_order, sequence_len, shown};

    #[test]
    fn a_name_is_shown_with_what_hides_in_it_made_visible() {
        assert!(matches!(
            shown("report.pdf"),
            alloc::borrow::Cow::Borrowed("report.pdf")
        ));
        assert_eq!(shown("report\u{200b}.pdf"), "report\u{fffd}.pdf");
        assert_eq!(shown("a\u{202e}b\nc"), "a\u{fffd}b\u{fffd}c");
        assert_ne!(
            shown("ab"),
            shown("a\u{200b}b"),
            "two names that differ never look alike"
        );
    }

    #[test]
    fn names_sort_by_their_folded_letters_and_never_compare_equal_unless_they_are() {
        let mut names = ["Zürich", "sao Paulo", "São Paulo", "Aden", "Sao Tome"];
        names.sort_by(|a, b| reader_order(a, b));
        assert_eq!(
            names,
            ["Aden", "São Paulo", "sao Paulo", "Sao Tome", "Zürich"],
            "folded letters first, then the bytes break the tie"
        );
        let mut regions = ["Zambia", "Åland Islands", "albania", "Curaçao", "Cuba"];
        regions.sort_by(|a, b| reader_order(a, b));
        assert_eq!(
            regions,
            ["Åland Islands", "albania", "Cuba", "Curaçao", "Zambia"]
        );
        assert_eq!(reader_order("Lima", "Lima"), Ordering::Equal);
    }

    #[test]
    fn a_name_folds_to_the_letters_a_reader_types() {
        let fold_all = |text: &str| folded(text).collect::<alloc::string::String>();
        assert_eq!(fold_all("São Paulo"), "sao paulo");
        assert_eq!(fold_all("Curaçao"), "curacao");
        assert_eq!(fold_all("ÅLAND Øy"), "aland oy");
        assert_eq!(fold_all("Réunion"), "reunion");
        assert_eq!(fold('Ł'), 'ł', "beyond Latin-1, case alone folds");
        assert_eq!(fold('-'), '-');
    }

    #[test]
    fn what_hides_or_reorders_text_is_hidden() {
        let hiding = [
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
        ];
        for ch in hiding {
            assert!(is_hidden(ch), "U+{:04X} hides", u32::from(ch));
        }
        let shown = [
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
        ];
        for ch in shown {
            assert!(!is_hidden(ch), "U+{:04X} is shown", u32::from(ch));
        }
    }

    #[test]
    fn every_lead_matches_the_standard_decoder() {
        for lead in 0..=u8::MAX {
            // Only the second byte is constrained beyond being a continuation.
            let decoded = (1..=4).find(|&len| {
                (0x80..=0xbf)
                    .any(|second| core::str::from_utf8(&[lead, second, 0x80, 0x80][..len]).is_ok())
            });
            assert_eq!(sequence_len(lead), decoded, "lead {lead:#04x}");
        }
    }
}
