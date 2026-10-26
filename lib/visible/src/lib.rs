//! The one rule for showing text from elsewhere so that what is drawn is what
//! is there: each character that draws nothing where it stands yet can hide,
//! reorder or break the text around it replaced by U+FFFD, and a combining
//! sequence cut at one U+FFFD where it would carry more than four marks or one
//! twice (UTS #39 §5.4). It links no allocator, so the boot console a port
//! links holds every glyph to it too.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

#[cfg(test)]
extern crate alloc;

pub use tairix_unicode::{is_hidden, Faithful};

/// `ch` shown on its own, as a cell or a glyph is: U+FFFD for an
/// [`is_hidden`] character.
#[must_use]
pub const fn shown_char(ch: char) -> char {
    if is_hidden(ch) {
        char::REPLACEMENT_CHARACTER
    } else {
        ch
    }
}

/// What one character of text from elsewhere shows as.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Step {
    /// The character itself.
    Kept,
    /// U+FFFD in its place.
    Escaped,
    /// Nothing: an escape already shown covers it.
    Covered,
}

/// The rule applied a character at a time: each [`is_hidden`] character
/// escaped, and the first mark that would break a combining sequence's UTS #39
/// limits ([`tairix_unicode::MarkRun`]) escaped, the rest of that sequence's
/// marks covered by the escape.
#[derive(Copy, Clone, Debug, Default)]
struct Showing {
    run: tairix_unicode::MarkRun,
    escaped: bool,
}

impl Showing {
    fn step(&mut self, ch: char) -> Step {
        if is_hidden(ch) {
            *self = Self::default();
            return Step::Escaped;
        }
        if self.escaped && tairix_unicode::is_nonspacing_mark(ch) {
            return Step::Covered;
        }
        self.escaped = !self.run.admit(ch);
        if self.escaped {
            Step::Escaped
        } else {
            Step::Kept
        }
    }

    /// `ch` as shown, or `None` where an escape covers it.
    fn show(&mut self, ch: char) -> Option<char> {
        match self.step(ch) {
            Step::Kept => Some(ch),
            Step::Escaped => Some(char::REPLACEMENT_CHARACTER),
            Step::Covered => None,
        }
    }
}

/// `text` split where [`Showing`] changes it: each run shown as written,
/// paired with whether an escape follows it.
struct Runs<'a, 's> {
    text: &'a str,
    showing: &'s mut Showing,
}

impl<'a> Iterator for Runs<'a, '_> {
    type Item = (&'a str, bool);

    fn next(&mut self) -> Option<Self::Item> {
        if self.text.is_empty() {
            return None;
        }
        for (at, ch) in self.text.char_indices() {
            let step = self.showing.step(ch);
            if step != Step::Kept {
                let run = &self.text[..at];
                self.text = &self.text[at + ch.len_utf8()..];
                return Some((run, step == Step::Escaped));
            }
        }
        Some((core::mem::take(&mut self.text), false))
    }
}

/// The characters [`Shown`] shows of `text`, lazily, for a surface laying
/// them out under a budget of its own.
pub fn shown_chars(text: &str) -> impl Iterator<Item = char> + '_ {
    let mut showing = Showing::default();
    text.chars().filter_map(move |ch| showing.show(ch))
}

/// `T` displayed as text from elsewhere is shown, formatted straight into its
/// destination: each [`is_hidden`] character replaced by U+FFFD, and a
/// combining sequence cut at one U+FFFD where it would carry more than four
/// marks or one twice, so a diagnostic quoting a stranger's text needs no
/// allocation to be safe to print.
#[derive(Clone, Copy, Debug)]
pub struct Shown<T>(pub T);

impl<T: core::fmt::Display> core::fmt::Display for Shown<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let mut replacing = Replacing {
            out: f,
            showing: Showing::default(),
        };
        core::fmt::Write::write_fmt(&mut replacing, format_args!("{}", self.0))
    }
}

/// Writes through to `out` as [`Showing`] shows it; the rule's state spans
/// the pieces a format writes.
struct Replacing<'a, 'b> {
    out: &'a mut core::fmt::Formatter<'b>,
    showing: Showing,
}

impl core::fmt::Write for Replacing<'_, '_> {
    fn write_str(&mut self, text: &str) -> core::fmt::Result {
        let runs = Runs {
            text,
            showing: &mut self.showing,
        };
        for (run, escaped) in runs {
            self.out.write_str(run)?;
            if escaped {
                self.out.write_char(char::REPLACEMENT_CHARACTER)?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use alloc::string::String;

    use super::{shown_char, shown_chars, Shown};

    fn shown(text: &str) -> String {
        alloc::format!("{}", Shown(text))
    }

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
        assert_eq!(shown("plain"), "plain");
        assert_eq!(
            alloc::format!("{}", Shown(format_args!("e{}{}x", "\u{301}", "\u{301}"))),
            "e\u{301}\u{fffd}x",
            "a sequence split across the pieces of a format is judged whole"
        );
    }

    #[test]
    fn a_combining_sequence_past_its_limits_is_cut_at_one_escape() {
        assert_eq!(
            shown("e\u{300}\u{301}\u{302}\u{303}x"),
            "e\u{300}\u{301}\u{302}\u{303}x"
        );
        assert_eq!(
            shown("e\u{300}\u{301}\u{302}\u{303}\u{304}\u{305}x"),
            "e\u{300}\u{301}\u{302}\u{303}\u{fffd}x",
            "a fifth mark is escaped and the rest of the stack covered"
        );
        assert_eq!(shown("e\u{301}\u{301}\u{301}x"), "e\u{301}\u{fffd}x");
        assert_eq!(
            shown("caf\u{e9}\u{301}"),
            "caf\u{e9}\u{fffd}",
            "an acute over é's own is a doubled mark"
        );
        assert_eq!(
            shown("e\u{301}\u{200b}\u{301}"),
            "e\u{301}\u{fffd}\u{301}",
            "an escaped character ends the sequence"
        );
    }

    #[test]
    fn shown_characters_are_the_shown_text() {
        for text in [
            "",
            "plain",
            "a\u{202e}b",
            "e\u{301}\u{301}\u{301}x\u{301}",
            "\u{1ec7}\u{301}\u{300}\u{304}\u{305}",
        ] {
            let lazily: String = shown_chars(text).collect();
            assert_eq!(lazily, shown(text), "{text:?}");
        }
    }
}
