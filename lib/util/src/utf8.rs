//! Which bytes begin a UTF-8 sequence, and how long it is (Unicode 15.0,
//! Table 3-7), for the decoders that must step over or validate text by hand;
//! a name as it is shown, as `tairix_visible` shows text from elsewhere; and
//! the letters a reader looks a name up by.

use tairix_visible::shown_chars;

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

/// `text` as it is shown where what it says must be what is drawn, as a
/// name is: as [`tairix_visible::Shown`] shows it, so two names that differ
/// never look alike. Borrowed when it is shown as written.
#[must_use]
pub fn shown(text: &str) -> alloc::borrow::Cow<'_, str> {
    if shown_chars(text).eq(text.chars()) {
        return alloc::borrow::Cow::Borrowed(text);
    }
    let mut out =
        alloc::string::String::with_capacity(text.len() + char::REPLACEMENT_CHARACTER.len_utf8());
    out.extend(shown_chars(text));
    alloc::borrow::Cow::Owned(out)
}

/// Whether `text` mixes scripts the way an impersonation needs — Latin with
/// Cyrillic or Greek, or any mixture no stricter UTS #39 restriction level
/// admits — so a surface showing it can say it may not be what it looks like.
#[must_use]
pub fn mixes_scripts(text: &str) -> bool {
    tairix_unicode::restriction_level(text)
        == tairix_unicode::RestrictionLevel::MinimallyRestrictive
}

#[cfg(test)]
mod tests {
    use alloc::borrow::Cow;
    use core::cmp::Ordering;

    use super::{fold, folded, mixes_scripts, reader_order, sequence_len, shown};

    /// A name shown as written is borrowed — a combining sequence within its
    /// limits and a replacement character it already holds included — and
    /// one shown otherwise is owned.
    #[test]
    fn a_name_is_borrowed_when_shown_as_written_and_owned_when_not() {
        for written in [
            "",
            "report.pdf",
            "e\u{300}\u{301}\u{302}\u{303}x",
            "caf\u{fffd}",
        ] {
            assert!(
                matches!(shown(written), Cow::Borrowed(text) if text == written),
                "{written:?}"
            );
        }
        for (text, seen) in [
            ("report\u{200b}.pdf", "report\u{fffd}.pdf"),
            ("e\u{301}\u{301}\u{301}x", "e\u{301}\u{fffd}x"),
        ] {
            assert!(
                matches!(shown(text), Cow::Owned(ref owned) if owned == seen),
                "{text:?}"
            );
        }
    }

    #[test]
    fn a_name_mixing_scripts_to_impersonate_is_told_apart() {
        for text in [
            "paypal",
            "caf\u{e9}",
            "Ελληνικά",
            "日本語abc",
            "한국어abc",
            "",
        ] {
            assert!(!mixes_scripts(text), "{text:?}");
        }
        // Cyrillic а and о among Latin; Greek Ο among Latin.
        for text in ["p\u{430}yp\u{430}l", "g\u{43e}\u{43e}gle", "\u{39f}ffice"] {
            assert!(mixes_scripts(text), "{text:?}");
        }
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
