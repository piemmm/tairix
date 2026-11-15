//! Tokenising a line: each keyword becomes its token and each number where a
//! line number may stand its reference, while strings, the text after `REM`
//! and `DATA`, star commands and the names after `FN` and `PROC` stay as they
//! are typed. Each dialect has its own tokeniser's rules.

use alloc::vec::Vec;

use tairix_util::fallible;

use crate::keyword::{word, Flags, Keyword, Token, Word, PSEUDO_LHS};
use crate::{basic2, basic5, byte, lineref, Dialect, Error, CR};

/// Where tokenising starts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Start {
    /// A typed line: a statement's start, where numbers are line numbers.
    Line,
    /// A statement's start, where numbers are not: the text `AUTO` reads.
    Statement,
    /// The middle of a statement: an expression, as `EVAL` reads one.
    Expression,
}

/// What BASIC V warns of once a typed line is tokenised.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Warnings {
    /// Its brackets do not pair.
    pub brackets: bool,
    /// A line number reached 65 280 and was left as typed.
    pub line_number: bool,
    /// It ends inside a string.
    pub quote: bool,
}

/// A tokenised line.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Tokenised {
    /// Its bytes, without the CR that ends it.
    pub bytes: Vec<u8>,
    /// BASIC V's warnings; none for BASIC II.
    pub warnings: Warnings,
}

const LF: u8 = b'\n';
/// The most of a text a line's first room is reserved for: more than any
/// line a program holds.
const MOST_READ: usize = 256;

/// Whether `b` may be part of a name: a letter, a digit, `_` or the BBC's
/// `£`, which is `` ` `` here.
const fn is_name(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'`'
}

/// `text`, to its first CR (or for BASIC V its first LF), tokenised as
/// `dialect` does from `start`.
pub fn tokenise(dialect: Dialect, text: &[u8], start: Start) -> Result<Tokenised, Error> {
    // A line's tokens seldom outgrow its text, and grow as pushed where they do.
    let mut out = Vec::new();
    out.try_reserve_exact(text.len().min(MOST_READ))
        .map_err(|_| Error::NoMemory)?;
    let mut line = Line {
        text,
        at: 0,
        out,
        start: !matches!(start, Start::Expression),
        numbers: start == Start::Line,
    };
    let warnings = match dialect {
        Dialect::BasicII => {
            line.basic2()?;
            Warnings::default()
        }
        Dialect::BasicV => line.basic5()?,
    };
    Ok(Tokenised {
        bytes: line.out,
        warnings,
    })
}

/// A line being tokenised.
struct Line<'a> {
    text: &'a [u8],
    at: usize,
    out: Vec<u8>,
    /// Whether a statement starts here.
    start: bool,
    /// Whether a number here is a line number.
    numbers: bool,
}

impl Line<'_> {
    fn peek(&self) -> u8 {
        byte(self.text, self.at)
    }

    fn ended(&self) -> bool {
        self.at >= self.text.len()
    }

    fn put(&mut self, b: u8) -> Result<(), Error> {
        if fallible::push(&mut self.out, b) {
            Ok(())
        } else {
            Err(Error::NoMemory)
        }
    }

    fn copy(&mut self) -> Result<(), Error> {
        let b = self.peek();
        self.at += 1;
        self.put(b)
    }

    fn put_token(&mut self, token: Token) -> Result<(), Error> {
        match token {
            Token::One(b) => self.put(b),
            Token::Two(prefix, b) => {
                self.put(prefix)?;
                self.put(b)
            }
        }
    }

    /// The name characters from here.
    fn copy_name(&mut self) -> Result<(), Error> {
        while !self.ended() && is_name(self.peek()) {
            self.copy()?;
        }
        Ok(())
    }

    /// The text from here as it is, to the first byte `ends` takes.
    fn copy_rest(&mut self, ends: fn(u8) -> bool) -> Result<(), Error> {
        while !self.ended() && !ends(self.peek()) {
            self.copy()?;
        }
        Ok(())
    }

    /// A number that is no line number: its digits and points.
    fn copy_number(&mut self) -> Result<(), Error> {
        self.copy()?;
        while !self.ended() && (self.peek().is_ascii_digit() || self.peek() == b'.') {
            self.copy()?;
        }
        Ok(())
    }

    /// The digits from here as a number, while it stays below `limit`, and
    /// where they end; `None` where it reaches the limit.
    fn line_number(&self, limit: u32) -> Option<(u16, usize)> {
        let mut n = 0u32;
        let mut at = self.at;
        while let Some(d) = self.text.get(at).filter(|b| b.is_ascii_digit()) {
            n = n * 10 + u32::from(d - b'0');
            if n >= limit {
                return None;
            }
            at += 1;
        }
        Some((u16::try_from(n).ok()?, at))
    }

    fn put_reference(&mut self, n: u16) -> Result<(), Error> {
        self.put(lineref::MARKER)?;
        for b in lineref::encode(n) {
            self.put(b)?;
        }
        Ok(())
    }

    /// BASIC II's tokeniser, as its ROM's.
    fn basic2(&mut self) -> Result<(), Error> {
        let is_cr = |b: u8| b == CR;
        while !self.ended() && self.peek() != CR {
            let c = self.peek();
            if c.is_ascii_uppercase() {
                if let Some((keyword, end)) = basic2_keyword(self.text, self.at) {
                    self.at = end;
                    self.basic2_keyword(keyword)?;
                    if keyword.flags.has(Flags::RAW) {
                        return self.copy_rest(is_cr);
                    }
                    continue;
                }
            }
            if c >= b'A' && is_name(c) {
                (self.start, self.numbers) = (false, false);
                self.copy_name()?;
                continue;
            }
            match c {
                b' ' | b',' => self.copy()?,
                b':' => {
                    (self.start, self.numbers) = (true, false);
                    self.copy()?;
                }
                b'"' => {
                    self.copy()?;
                    self.copy_rest(|b| b == b'"' || b == CR)?;
                    if self.ended() || self.peek() == CR {
                        return Ok(());
                    }
                    (self.start, self.numbers) = (false, false);
                    self.copy()?;
                }
                b'&' => {
                    self.copy()?;
                    self.copy_rest(|b| !(b.is_ascii_digit() || (b'A'..=b'F').contains(&b)))?;
                }
                b'*' if self.start => return self.copy_rest(is_cr),
                b'0'..=b'9' if self.numbers => {
                    if let Some((n, end)) = self.line_number(0x1_0000) {
                        self.at = end;
                        self.put_reference(n)?;
                    } else {
                        self.copy_number()?;
                        self.start = false;
                    }
                }
                b'0'..=b'9' | b'.' => {
                    self.copy_number()?;
                    self.start = false;
                }
                _ => {
                    (self.start, self.numbers) = (false, false);
                    self.copy()?;
                }
            }
        }
        Ok(())
    }

    /// What a BASIC II keyword does: its token, then the state its flags
    /// leave and the name it takes.
    fn basic2_keyword(&mut self, keyword: Keyword<u8>) -> Result<(), Error> {
        let assigned = self.start && keyword.flags.has(Flags::PSEUDO);
        self.put(keyword.word.token | if assigned { PSEUDO_LHS } else { 0 })?;
        self.numbers = false;
        if keyword.flags.has(Flags::MIDDLE) {
            self.start = false;
        }
        if keyword.flags.has(Flags::START) {
            self.start = true;
        }
        if keyword.flags.has(Flags::LINE_NUMBER) {
            self.numbers = true;
        }
        if keyword.flags.has(Flags::NAME) {
            self.copy_name()?;
        }
        Ok(())
    }

    /// BASIC V's tokeniser, as RISC OS 5's, and the warnings it gives.
    fn basic5(&mut self) -> Result<Warnings, Error> {
        let ends = |b: u8| b == CR || b == LF;
        let mut warnings = Warnings::default();
        // Brackets open, uncounted once more close than opened.
        let mut open: Option<u32> = Some(0);
        // Where a `VDU` spelt whole ended, for `VDUP.`.
        let mut vdu_ends = None;
        while !self.ended() && !ends(self.peek()) {
            let c = self.peek();
            match c {
                b' ' | b',' => self.copy()?,
                b'"' => {
                    self.copy()?;
                    self.copy_rest(|b| b == b'"' || b == CR || b == LF)?;
                    if self.ended() || ends(self.peek()) {
                        warnings.quote = true;
                        break;
                    }
                    (self.start, self.numbers) = (false, false);
                    self.copy()?;
                }
                b'&' => {
                    self.copy()?;
                    self.copy_rest(|b| !b.is_ascii_hexdigit())?;
                }
                b':' => {
                    (self.start, self.numbers) = (true, false);
                    self.copy()?;
                }
                b'*' if self.start => {
                    self.copy_rest(ends)?;
                    break;
                }
                b'0'..=b'9' if self.numbers => {
                    if let Some((n, end)) = self.line_number(0xFF00) {
                        self.at = end;
                        self.put_reference(n)?;
                    } else {
                        warnings.line_number = true;
                        self.copy_number()?;
                        (self.start, self.numbers) = (false, false);
                    }
                }
                b'0'..=b'9' | b'.' => {
                    self.copy_number()?;
                    (self.start, self.numbers) = (false, false);
                }
                b'A'..=b'W' => {
                    let Some((keyword, end, whole)) = basic5_keyword(self.text, self.at) else {
                        self.copy_name()?;
                        (self.start, self.numbers) = (false, false);
                        continue;
                    };
                    if is(keyword, word::PRINT)
                        && !whole
                        && end == self.at + 2
                        && !self.start
                        && vdu_ends == Some(self.out.len())
                    {
                        // `VDUP.` is VDU and a name, as an assembler's
                        // mnemonic would be, not VDU PRINT.
                        self.copy()?;
                        self.copy()?;
                        (self.start, self.numbers) = (true, false);
                        continue;
                    }
                    if keyword.flags.has(Flags::CONDITIONAL) && is_name(byte(self.text, end)) {
                        self.copy_name()?;
                        (self.start, self.numbers) = (false, false);
                        continue;
                    }
                    let middle = !self.start;
                    self.at = end;
                    self.basic5_keyword(keyword, middle)?;
                    if is(keyword, word::VDU) && whole {
                        vdu_ends = Some(self.out.len());
                    }
                    if keyword.flags.has(Flags::BRACKET) {
                        open = open.and_then(|n| n.checked_add(1));
                    }
                    if keyword.flags.has(Flags::RAW) {
                        self.copy_rest(ends)?;
                        break;
                    }
                }
                _ if c > b'W' && is_name(c) => {
                    self.copy_name()?;
                    (self.start, self.numbers) = (false, false);
                }
                _ => {
                    open = match (c, open) {
                        (b'(', Some(n)) => n.checked_add(1),
                        (b')', Some(n)) => n.checked_sub(1),
                        (_, open) => open,
                    };
                    (self.start, self.numbers) = (false, false);
                    self.copy()?;
                }
            }
        }
        warnings.brackets = open != Some(0);
        Ok(warnings)
    }

    /// What a BASIC V keyword does: its token, then the state its flags
    /// leave and, after `FN` and `PROC`, the name.
    fn basic5_keyword(&mut self, keyword: Keyword, middle: bool) -> Result<(), Error> {
        let token = match keyword.word.token {
            Token::One(b) if self.start && keyword.flags.has(Flags::PSEUDO) => {
                Token::One(b | PSEUDO_LHS)
            }
            token => token,
        };
        self.put_token(token)?;
        if keyword.flags.has(Flags::MIDDLE) {
            (self.start, self.numbers) = (false, false);
        }
        if keyword.flags.has(Flags::START) {
            (self.start, self.numbers) = (true, false);
        }
        if is(keyword, word::FN) || is(keyword, word::PROC) {
            self.copy_name()?;
        }
        // TRACE in an expression is the trace file's handle, which takes
        // no line number.
        let traced = is(keyword, word::TRACE) && middle;
        if keyword.flags.has(Flags::LINE_NUMBER) && !traced {
            self.numbers = true;
        }
        Ok(())
    }
}

/// Whether `keyword` is `word`.
fn is(keyword: Keyword, word: Word<u8>) -> bool {
    keyword.word.token == Token::One(word.token)
}

/// How many letters of `name` the text at `at` matches, and whether it then
/// stands at a `.`.
fn matched(text: &[u8], at: usize, name: &str) -> (usize, bool) {
    let name = name.as_bytes();
    let same = name
        .iter()
        .zip(text.get(at..).unwrap_or(&[]))
        .take_while(|(a, b)| a == b)
        .count();
    (same, byte(text, at + same) == b'.')
}

/// The BASIC II keyword the text at `at` begins, and where it ends: the first
/// in table order it spells whole, a conditional one only before no name
/// character, or the first it abbreviates with a `.`, unchecked.
fn basic2_keyword(text: &[u8], at: usize) -> Option<(Keyword<u8>, usize)> {
    basic2::TABLE
        .beginning(byte(text, at))
        .iter()
        .find_map(|&keyword| {
            let name = keyword.word.name;
            let (same, point) = matched(text, at, name);
            if same == name.len() {
                let conditional = keyword.flags.has(Flags::CONDITIONAL);
                (!conditional || !is_name(byte(text, at + same))).then_some((keyword, at + same))
            } else {
                (same > 0 && point).then_some((keyword, at + same + 1))
            }
        })
}

/// The BASIC V keyword the text at `at` begins: the first in its letter's run
/// that it spells whole or leaves at a `.`; where it ends; and whether it was
/// whole.
fn basic5_keyword(text: &[u8], at: usize) -> Option<(Keyword, usize, bool)> {
    basic5::TABLE
        .beginning(byte(text, at))
        .iter()
        .find_map(|&keyword| {
            let name = keyword.word.name;
            let (same, point) = matched(text, at, name);
            if same == name.len() {
                Some((keyword, at + same, true))
            } else {
                (same > 0 && point).then_some((keyword, at + same + 1, false))
            }
        })
}

#[cfg(test)]
#[path = "tokenise_tests.rs"]
mod tests;
