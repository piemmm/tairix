//! The one rule for the characters that draw nothing where they stand yet can
//! hide, reorder or break the text around them, and text shown with each of
//! them replaced by U+FFFD. It links no allocator, so the boot console a port
//! links holds every glyph to it too.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

#[cfg(test)]
extern crate alloc;

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

/// `ch` as [`Shown`] shows it: U+FFFD for an [`is_hidden`] character.
#[must_use]
pub const fn shown_char(ch: char) -> char {
    if is_hidden(ch) {
        char::REPLACEMENT_CHARACTER
    } else {
        ch
    }
}

/// `T` displayed with each [`is_hidden`] character replaced by U+FFFD,
/// formatted straight into its destination: a diagnostic quoting text from
/// elsewhere needs no allocation to be safe to print.
#[derive(Clone, Copy, Debug)]
pub struct Shown<T>(pub T);

impl<T: core::fmt::Display> core::fmt::Display for Shown<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        core::fmt::Write::write_fmt(&mut Replacing(f), format_args!("{}", self.0))
    }
}

struct Replacing<'a, 'b>(&'a mut core::fmt::Formatter<'b>);

impl core::fmt::Write for Replacing<'_, '_> {
    fn write_str(&mut self, text: &str) -> core::fmt::Result {
        let mut run = 0;
        for (at, ch) in text.char_indices() {
            if is_hidden(ch) {
                self.0.write_str(&text[run..at])?;
                self.0.write_char(char::REPLACEMENT_CHARACTER)?;
                run = at + ch.len_utf8();
            }
        }
        self.0.write_str(&text[run..])
    }
}

#[cfg(test)]
mod tests {
    use super::{is_hidden, shown_char, Shown};

    #[test]
    fn one_character_is_shown_as_its_text_would_be() {
        assert_eq!(shown_char('a'), 'a');
        assert_eq!(shown_char('\u{202e}'), '\u{fffd}');
        assert_eq!(shown_char('\u{1b}'), '\u{fffd}');
    }
    #[test]
    fn a_formatted_reason_shows_what_hides_in_any_of_its_parts() {
        let name = "a\u{1b}[2Jb";
        assert_eq!(
            alloc::format!("{}", Shown(format_args!("cannot open {name}: {}", 7))),
            "cannot open a\u{fffd}[2Jb: 7"
        );
        assert_eq!(alloc::format!("{}", Shown("plain")), "plain");
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
}
