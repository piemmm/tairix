//! A device's special fields: the validation string its driver registered,
//! read once, and the words a stream's special field and its device's
//! options give the driver, one for each escape the string declares.
//!
//! The string is commands one after another, each keywords separated by
//! commas, then a `/` and an escape character for each word it fills: `N` a
//! number, `S` which of its keywords was given. A special field is
//! keywords separated by semicolons. Each command takes the first keyword
//! given, case aside, that is not followed by a letter, as RISC OS 5's
//! DeviceFS 0.73 reads them.

use core::ops::Range;

use alloc::vec::Vec;

use tairix_obey::number::{read_unsigned, Reading};
use tairix_util::fallible;

/// What a word no keyword was given for holds.
pub(crate) const ABSENT: u32 = 0xDEAD_DEAD;

/// The longest validation string read: a containment bound on text a
/// driver hands over, far past any device's.
pub(crate) const MOST: usize = 256;

/// What one escape fills its word with.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Escape {
    /// A number, read as `OS_ReadUnsigned` reads one.
    Number,
    /// Which of the command's keywords was given, from nought.
    Switch,
    /// A character the syntax gives no meaning: its word stays absent.
    Other,
}

impl Escape {
    const fn of(character: u8) -> Self {
        match character {
            b'N' | b'n' => Self::Number,
            b'S' | b's' => Self::Switch,
            _ => Self::Other,
        }
    }
}

/// One command: its keywords, by their place in the string, and its
/// escapes.
#[derive(Clone, Debug)]
struct Command {
    keywords: Vec<Range<usize>>,
    escapes: Vec<Escape>,
    /// Its first word's place in the block.
    first: usize,
}

/// A validation string, read.
#[derive(Clone, Debug)]
pub(crate) struct Validation {
    text: Vec<u8>,
    commands: Vec<Command>,
    words: usize,
}

impl Validation {
    /// `text` read: each part after a `/` an escape, what follows its
    /// character the next command's keywords. None where there is no room
    /// for it.
    pub(crate) fn parse(text: &[u8]) -> Option<Self> {
        let mut commands = Vec::new();
        let mut words = 0;
        let mut parts = Spans::new(text, b'/');
        let mut keywords = match parts.next() {
            Some(keys) => split(text, keys)?,
            None => Vec::new(),
        };
        let mut escapes = Vec::new();
        for part in parts {
            let escape = text.get(part.start).filter(|_| !part.is_empty());
            fallible::try_push(
                &mut escapes,
                escape.map_or(Escape::Other, |&c| Escape::of(c)),
            )
            .ok()?;
            let rest = part.start + 1..part.end;
            if escape.is_some() && !rest.is_empty() {
                words = close(&mut commands, keywords, escapes, words)?;
                keywords = split(text, rest)?;
                escapes = Vec::new();
            }
        }
        let words = close(&mut commands, keywords, escapes, words)?;
        Some(Self {
            text: fallible::copied(text)?,
            commands,
            words,
        })
    }

    /// The words the driver is given: each command's from the first of
    /// `fields` — the special field, then the device's options — that names
    /// one of its keywords. None where there is no room for them.
    ///
    /// # Errors
    ///
    /// The reading's error, for a keyword whose number does not read.
    pub(crate) fn words(&self, fields: &[&[u8]]) -> Result<Option<Vec<u32>>, tairix_obey::Error> {
        let Some(mut words) = fallible::filled(self.words, ABSENT) else {
            return Ok(None);
        };
        for command in &self.commands {
            let given = fields
                .iter()
                .flat_map(|field| field.split(|&b| b == b';'))
                .find_map(|item| self.keyword_in(command, item));
            if let Some((keyword, rest)) = given {
                fill(command, keyword, rest, &mut words)?;
            }
        }
        Ok(Some(words))
    }

    /// Which of `command`'s keywords `item` begins with, and what follows it.
    fn keyword_in<'a>(&self, command: &Command, item: &'a [u8]) -> Option<(usize, &'a [u8])> {
        command.keywords.iter().enumerate().find_map(|(at, range)| {
            given_as(&self.text[range.clone()], item).map(|rest| (at, rest))
        })
    }
}

/// What follows `keyword` at the start of `item`, case aside, where the next
/// byte is not a letter; an empty keyword begins every item.
pub(crate) fn given_as<'a>(keyword: &[u8], item: &'a [u8]) -> Option<&'a [u8]> {
    let (head, rest) = item.split_at_checked(keyword.len())?;
    let whole = keyword.is_empty() || !rest.first().is_some_and(u8::is_ascii_alphabetic);
    (head.eq_ignore_ascii_case(keyword) && whole).then_some(rest)
}

/// The number a keyword's `rest` gives, an `=` before it passed over, and
/// what follows it.
///
/// # Errors
///
/// The reading's error.
pub(crate) fn number(rest: &[u8]) -> Result<(u32, &[u8]), tairix_obey::Error> {
    let text = rest.strip_prefix(b"=").unwrap_or(rest);
    let (value, used) = read_unsigned(text, Reading::plain(10))?;
    let value = u32::try_from(value).map_err(|_| tairix_obey::Error::NumberTooBig)?;
    Ok((value, &text[used..]))
}

/// `command`'s words from `rest`, its keyword the one at `keyword`: the
/// first number required, each after it absent where nothing is left. A
/// number after a number follows one separator — a comma or any character
/// out of place in a number, as the PRM gives it.
fn fill(
    command: &Command,
    keyword: usize,
    mut rest: &[u8],
    words: &mut [u32],
) -> Result<(), tairix_obey::Error> {
    let mut after_number = false;
    for (index, escape) in command.escapes.iter().enumerate() {
        let word = &mut words[command.first + index];
        match escape {
            Escape::Number => {
                if index > 0 {
                    rest = if after_number {
                        rest.get(1..).unwrap_or_default()
                    } else {
                        rest.strip_prefix(b",").unwrap_or(rest)
                    };
                    if rest.is_empty() {
                        continue;
                    }
                }
                let (value, after) = number(rest)?;
                *word = value;
                rest = after;
                after_number = true;
            }
            Escape::Switch => *word = u32::try_from(keyword).unwrap_or(u32::MAX),
            Escape::Other => {}
        }
    }
    Ok(())
}

/// A command ended: kept where it fills a word. The words counted so far;
/// none where there is no room for it.
fn close(
    commands: &mut Vec<Command>,
    keywords: Vec<Range<usize>>,
    escapes: Vec<Escape>,
    words: usize,
) -> Option<usize> {
    if escapes.is_empty() {
        return Some(words);
    }
    let first = words;
    let words = words + escapes.len();
    fallible::try_push(
        commands,
        Command {
            keywords,
            escapes,
            first,
        },
    )
    .ok()?;
    Some(words)
}

/// The keywords in `range` of `text`, split at its commas.
fn split(text: &[u8], range: Range<usize>) -> Option<Vec<Range<usize>>> {
    let mut keywords = Vec::new();
    for span in Spans::new(&text[range.clone()], b',') {
        fallible::try_push(
            &mut keywords,
            range.start + span.start..range.start + span.end,
        )
        .ok()?;
    }
    Some(keywords)
}

/// The places of the parts of `text` between `separator`s.
struct Spans<'a> {
    text: &'a [u8],
    separator: u8,
    at: Option<usize>,
}

impl<'a> Spans<'a> {
    const fn new(text: &'a [u8], separator: u8) -> Self {
        Self {
            text,
            separator,
            at: Some(0),
        }
    }
}

impl Iterator for Spans<'_> {
    type Item = Range<usize>;

    fn next(&mut self) -> Option<Range<usize>> {
        let start = self.at?;
        let end = self.text[start..]
            .iter()
            .position(|&b| b == self.separator)
            .map_or(self.text.len(), |offset| start + offset);
        self.at = (end < self.text.len()).then_some(end + 1);
        Some(start..end)
    }
}

#[cfg(test)]
mod tests;
