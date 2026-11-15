//! A program as BASIC keeps it: from PAGE, a CR, then each line's number high
//! byte first, its length — the bytes from its CR to the next — and its
//! tokenised text, each line in order of its number; a CR and `&FF` end it.

use alloc::vec::Vec;

use tairix_util::fallible;

use crate::keyword::word;
use crate::list::Options;
use crate::{basic2, basic5, Dialect, Error, CR};

/// The byte a program's end stands in a line number's high byte at.
pub const END: u8 = 0xFF;
/// A line's bytes before its text: its CR, number and length.
pub const HEADER: usize = 4;
/// The most bytes of text a line holds, its length fitting a byte.
pub const MOST_TEXT: usize = 255 - HEADER;

/// One line of a program.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Line<'a> {
    /// Its number.
    pub number: u16,
    /// Its tokenised text, without the CR after it.
    pub text: &'a [u8],
}

/// The highest line number `dialect` keeps.
#[must_use]
pub const fn last_line(dialect: Dialect) -> u16 {
    match dialect {
        Dialect::BasicII => basic2::LAST_LINE,
        Dialect::BasicV => basic5::LAST_LINE,
    }
}

/// The lines of the program `image` holds, to its end; `BadProgram` where
/// a line does not chain to the next.
#[must_use]
pub fn lines(image: &[u8]) -> Lines<'_> {
    Lines {
        image,
        at: 0,
        done: false,
    }
}

/// The lines of a program image.
#[derive(Clone, Debug)]
pub struct Lines<'a> {
    image: &'a [u8],
    at: usize,
    done: bool,
}

impl<'a> Iterator for Lines<'a> {
    type Item = Result<Line<'a>, Error>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        let rest = &self.image[self.at..];
        let line = match rest {
            [CR, END, ..] => {
                self.done = true;
                return None;
            }
            [CR, high, low, length, ..] if usize::from(*length) >= HEADER => {
                let length = usize::from(*length);
                // The next line's CR, or the end's, must follow.
                (rest.get(length) == Some(&CR)).then(|| Line {
                    number: u16::from_be_bytes([*high, *low]),
                    text: &rest[HEADER..length],
                })
            }
            _ => None,
        };
        if let Some(line) = line {
            self.at += HEADER + line.text.len();
            Some(Ok(line))
        } else {
            self.done = true;
            Some(Err(Error::BadProgram))
        }
    }
}

/// An image of `lines`, which must come in ascending order of number.
pub fn image<'a>(lines: impl IntoIterator<Item = Line<'a>>) -> Result<Vec<u8>, Error> {
    let mut out = Vec::new();
    let mut last: Option<u16> = None;
    for line in lines {
        if last.is_some_and(|last| line.number <= last) || line.number >= u16::from(END) << 8 {
            return Err(Error::BadProgram);
        }
        if line.text.len() > MOST_TEXT {
            return Err(Error::LineTooLong);
        }
        let [high, low] = line.number.to_be_bytes();
        let length = u8::try_from(HEADER + line.text.len()).map_err(|_| Error::LineTooLong)?;
        extend(&mut out, &[CR, high, low, length])?;
        extend(&mut out, line.text)?;
        last = Some(line.number);
    }
    extend(&mut out, &[CR, END])?;
    Ok(out)
}

/// `bytes` appended, the room growing as a pushed buffer's does, so an image
/// built a line at a time costs time in proportion to its length.
fn extend(out: &mut Vec<u8>, bytes: &[u8]) -> Result<(), Error> {
    out.try_reserve(bytes.len()).map_err(|_| Error::NoMemory)?;
    out.extend_from_slice(bytes);
    Ok(())
}

/// A typed line's text as `dialect` stores it once its line number is taken
/// off: BASIC II's as it was tokenised; BASIC V's without leading spaces
/// where `LISTO` is not 0, then without trailing ones but for its first byte,
/// and with a first `ELSE` made the block one. An empty text deletes its line
/// before this is asked; one this leaves empty is kept as a line.
pub fn stored(dialect: Dialect, text: &[u8], listo: Options) -> Result<Vec<u8>, Error> {
    let text = match dialect {
        Dialect::BasicII => text,
        Dialect::BasicV => {
            let start = if listo == Options::default() {
                0
            } else {
                text.iter().take_while(|&&b| b == b' ').count()
            };
            let kept = &text[start..];
            // Trailing spaces go back to the first byte kept, never it, as
            // RISC OS 5 trims them.
            let end = kept
                .get(1..)
                .and_then(|rest| rest.iter().rposition(|&b| b != b' '))
                .map_or(kept.len().min(1), |last| last + 2);
            &kept[..end]
        }
    };
    if text.len() > MOST_TEXT {
        return Err(Error::LineTooLong);
    }
    let mut out = fallible::copied(text).ok_or(Error::NoMemory)?;
    if dialect == Dialect::BasicV {
        if let Some(first) = out.iter_mut().find(|b| **b != b' ') {
            if *first == word::ELSE.token {
                *first = basic5::ELSE_BLOCK.token;
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
#[path = "program_tests.rs"]
mod tests;
