//! Which bytes begin a UTF-8 sequence, and how long it is (Unicode 15.0,
//! Table 3-7), for the decoders that must step over or validate text by hand;
//! and which characters must be escaped when text from elsewhere is shown.

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

#[cfg(test)]
mod tests {
    use super::{is_hidden, sequence_len};

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
