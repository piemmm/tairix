//! Listing: a program's lines as `LIST` shows them under `LISTO`, each token
//! spelt and each line reference its number, the structures they open
//! indented. Each output line ends with an LF; a screen shows it as LF CR.

use alloc::vec::Vec;

use crate::keyword::{word, Token};
use crate::{basic2, basic5, byte, lineref, program, Dialect, Error, CR};

/// `LISTO`'s options.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Options(u8);

impl Options {
    /// A space after each line number.
    pub const SPACE: u8 = 1;
    /// BASIC II: lines inside a `FOR` loop indented two spaces a loop.
    pub const FOR_LOOPS: u8 = 1 << 1;
    /// BASIC II: lines inside a `REPEAT` loop indented two spaces a loop.
    pub const REPEAT_LOOPS: u8 = 1 << 2;
    /// BASIC II's kinds of loop, in the order a listing counts them.
    pub const LOOPS: [u8; 2] = [Self::FOR_LOOPS, Self::REPEAT_LOOPS];
    /// BASIC V: lines inside a structure indented two spaces a level.
    pub const INDENT: u8 = 1 << 1;
    /// BASIC V: each statement on a line of its own.
    pub const SPLIT: u8 = 1 << 2;
    /// BASIC V: no line numbers; a line reference stops the listing.
    pub const NO_NUMBERS: u8 = 1 << 3;
    /// BASIC V: keywords in lower case.
    pub const LOWER_CASE: u8 = 1 << 4;

    /// The options `bits` hold.
    #[must_use]
    pub const fn new(bits: u8) -> Self {
        Self(bits)
    }

    const fn has(self, option: u8) -> bool {
        self.0 & option != 0
    }
}

const QUOTE: u8 = b'"';

/// The deepest indent before a line's text: BASIC II's two kinds of loop at
/// 127 each, one more of a kind listing as one space.
const DEEPEST_INDENT: usize = Options::LOOPS.len() * 2 * 127;

/// The most a line lists as besides its text: its number's room, a space,
/// the deepest indent and the new line.
pub const MOST_LISTED_BEFORE: usize = 5 + 1 + DEEPEST_INDENT + 1;

/// The most one byte of a line's text lists as: under LISTO 4 a colon that
/// splits the statement, the new line, the number's room, a space and the
/// deepest indent.
pub const MOST_LISTED_EACH: usize = 1 + 1 + 5 + 1 + 255;

/// A program being listed, a line at a time, the depth of its structures
/// carried from each line to the next.
#[derive(Clone, Debug)]
pub struct Lister {
    options: Options,
    carried: Carried,
}

/// What a listing carries from line to line, as each dialect counts it.
#[derive(Clone, Copy, Debug)]
enum Carried {
    /// The `FOR` and `REPEAT` loops open, a byte each as BASIC II's ROM
    /// keeps them.
    BasicII([u8; 2]),
    /// The depth of the structures open, two a level.
    BasicV(i32),
}

impl Lister {
    /// A listing that starts at no depth.
    #[must_use]
    pub const fn new(dialect: Dialect, options: Options) -> Self {
        let carried = match dialect {
            Dialect::BasicII => Carried::BasicII([0; 2]),
            Dialect::BasicV => Carried::BasicV(0),
        };
        Self { options, carried }
    }

    /// Line `number`, whose tokenised text is `text` to its CR, listed onto
    /// `out`. Under BASIC V's [`Options::NO_NUMBERS`], a line reference is
    /// listed as its number and the line, and the listing, end there with
    /// `ReferenceListed`. Refused memory leaves the listing and `out` as they
    /// were, so the line can be listed again. It lists as at most
    /// [`MOST_LISTED_BEFORE`] and [`MOST_LISTED_EACH`] a byte of its text.
    pub fn line(&mut self, number: u16, text: &[u8], out: &mut Vec<u8>) -> Result<(), Error> {
        let end = text.iter().position(|&b| b == CR).unwrap_or(text.len());
        let text = &text[..end];
        let (carried, listed) = (self.carried, out.len());
        let outcome = match &mut self.carried {
            Carried::BasicII(loops) => basic2(self.options, loops, number, text, &mut Out(out)),
            Carried::BasicV(depth) => basic5(self.options, depth, number, text, &mut Out(out)),
        };
        if outcome == Err(Error::NoMemory) {
            self.carried = carried;
            out.truncate(listed);
        }
        outcome
    }
}

fn basic2(
    options: Options,
    loops: &mut [u8; 2],
    number: u16,
    text: &[u8],
    out: &mut Out<'_>,
) -> Result<(), Error> {
    out.number(number, true)?;
    out.spaces(usize::from(options.has(Options::SPACE)))?;
    // Two spaces a loop of each kind shown, but one for 128 or more: BASIC
    // II is seen to print them so.
    for (&open, option) in loops.iter().zip(Options::LOOPS) {
        if options.has(option) {
            let indent = if open < 0x80 {
                usize::from(open) * 2
            } else {
                1
            };
            out.spaces(indent)?;
        }
    }
    let mut at = 0;
    while let Some(&b) = text.get(at) {
        at += 1;
        if b < 0x80 {
            out.put(b)?;
            continue;
        }
        if b == lineref::MARKER {
            out.number(reference(text, at), false)?;
            at += lineref::ENCODED;
            continue;
        }
        if b == word::FOR.token {
            loops[0] = loops[0].wrapping_add(1);
        } else if b == word::REPEAT.token {
            loops[1] = loops[1].wrapping_add(1);
        } else if b == word::NEXT.token {
            loops[0] = loops[0].saturating_sub(1);
        } else if b == word::UNTIL.token {
            loops[1] = loops[1].saturating_sub(1);
        }
        if let Some(name) = basic2::SPELLINGS.of_token(Token::One(b)) {
            out.text(name.as_bytes(), false)?;
        }
    }
    out.put(b'\n')
}

fn basic5(
    options: Options,
    depth: &mut i32,
    number: u16,
    text: &[u8],
    out: &mut Out<'_>,
) -> Result<(), Error> {
    let before = (*depth).max(0);
    let after = before.wrapping_add(depth_change(text));
    *depth = after;
    // The lesser depth, but a line that closes more than are open keeps the
    // depth before it; its low byte's worth of spaces.
    let indent = if options.has(Options::INDENT) {
        let lesser = if after < 0 { before } else { before.min(after) };
        usize::from(lesser.to_le_bytes()[0])
    } else {
        0
    };
    let numbered = !options.has(Options::NO_NUMBERS);
    if numbered {
        out.number(number, true)?;
    }
    let space = usize::from(options.has(Options::SPACE));
    out.spaces(space)?;
    out.spaces(indent)?;
    let lower = options.has(Options::LOWER_CASE);
    let (mut quoted, mut remark) = (false, false);
    let mut at = 0;
    while let Some(&b) = text.get(at) {
        at += 1;
        if quoted || remark || b < 0x7F {
            if b == QUOTE && !remark {
                quoted = !quoted;
            }
            out.put(b)?;
            if b == b':' && !quoted && !remark && options.has(Options::SPLIT) {
                out.put(b'\n')?;
                out.spaces(space + if numbered { 5 } else { 0 })?;
                out.spaces(indent)?;
            }
            continue;
        }
        if b == lineref::MARKER {
            out.number(reference(text, at), false)?;
            at += lineref::ENCODED;
            if !numbered {
                out.put(b'\n')?;
                return Err(Error::ReferenceListed);
            }
            continue;
        }
        let token = if basic5::PREFIXES.contains(&b) {
            let second = byte(text, at);
            at += 1;
            Token::Two(b, second)
        } else {
            Token::One(b)
        };
        if token == Token::One(word::REM.token) {
            remark = true;
        }
        if let Some(name) = basic5::SPELLINGS.of_token(token) {
            out.text(name.as_bytes(), lower)?;
        }
    }
    out.put(b'\n')
}

/// The line number encoded at `at`, its marker before it.
fn reference(text: &[u8], at: usize) -> u16 {
    lineref::decode(core::array::from_fn(|i| byte(text, at + i)))
}

/// The tokens that open a BASIC V structure, and those that close one.
const OPENS: [Token; 4] = [
    word::FOR.either().token,
    word::REPEAT.either().token,
    basic5::WHILE.token,
    basic5::CASE.token,
];
const CLOSES: [Token; 5] = [
    word::NEXT.either().token,
    word::UNTIL.either().token,
    basic5::ENDWHILE.either().token,
    basic5::ENDCASE.either().token,
    basic5::ENDIF.either().token,
];

/// How a BASIC V line changes the depth its structures indent to: two in
/// for each that opens, two out for each that closes, outside strings and
/// before any `REM`, and two in where the line ends at `THEN`.
fn depth_change(text: &[u8]) -> i32 {
    let mut change = 0_i32;
    let mut quoted = false;
    // A two-byte opener is known by the byte before its second, which before
    // the first is the line's length as its record holds it.
    let mut previous = (program::HEADER + text.len()).to_le_bytes()[0];
    for &b in text {
        // A `REM` byte ends the count even inside a string.
        if b == word::REM.token {
            break;
        }
        if b == QUOTE {
            quoted = !quoted;
        }
        if !quoted {
            if CLOSES.contains(&Token::One(b)) {
                change = change.wrapping_sub(2);
            } else if OPENS.contains(&Token::One(b)) || OPENS.contains(&Token::Two(previous, b)) {
                change = change.wrapping_add(2);
            }
        }
        previous = b;
    }
    if text.last() == Some(&word::THEN.token) {
        change = change.wrapping_add(2);
    }
    change
}

/// A listing's bytes, each run of them given room at once and the room
/// growing as a pushed buffer's does.
struct Out<'a>(&'a mut Vec<u8>);

impl Out<'_> {
    fn room(&mut self, count: usize) -> Result<(), Error> {
        Ok(self.0.try_reserve(count)?)
    }

    fn put(&mut self, b: u8) -> Result<(), Error> {
        self.room(1)?;
        self.0.push(b);
        Ok(())
    }

    fn spaces(&mut self, count: usize) -> Result<(), Error> {
        self.room(count)?;
        self.0.resize(self.0.len() + count, b' ');
        Ok(())
    }

    fn text(&mut self, bytes: &[u8], lower: bool) -> Result<(), Error> {
        self.room(bytes.len())?;
        self.0.extend(
            bytes
                .iter()
                .map(|&b| if lower { b.to_ascii_lowercase() } else { b }),
        );
        Ok(())
    }

    /// `n` in decimal, padded to five columns where `padded`.
    fn number(&mut self, n: u16, padded: bool) -> Result<(), Error> {
        let mut digits = [b' '; 5];
        let mut rest = n;
        let mut at = digits.len();
        loop {
            at -= 1;
            digits[at] = b'0' + (rest % 10).to_le_bytes()[0];
            rest /= 10;
            if rest == 0 {
                break;
            }
        }
        self.text(&digits[if padded { 0 } else { at }..], false)
    }
}

#[cfg(test)]
#[path = "list_tests.rs"]
mod tests;
