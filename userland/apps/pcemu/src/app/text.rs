//! A machine's text screen as the clipboard's text, and the clipboard's text
//! as what its keyboard can type, both in code page 437.
//!
//! A screen is copied as it draws: every cell's glyph, the faces and arrows
//! of `01h`–`1Fh` included. Typing takes only characters that arrive as
//! themselves: those glyphs are control codes to a PC — the heart is Ctrl+C,
//! the left arrow Escape — so they are left out, as is anything code page 437
//! lacks once typographic punctuation is plainly spelt and a letter's accents
//! the page lacks are dropped.

use alloc::collections::TryReserveError;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use tairix_unicode::{
    is_default_ignorable, is_nonspacing_mark, normalize, Form, MAX_DECOMPOSITION,
};

use crate::front::MAX_TYPED;
use crate::link::typeable;

/// Enter and Tab, as the codes that press them.
const ENTER: u8 = 0x0D;
const TAB: u8 = 0x09;

/// Each UTF-8 byte a glyph takes at most: code page 437 draws nothing past
/// the Basic Multilingual Plane.
const GLYPH_UTF8: usize = 3;

/// The text of a screen `columns` wide whose cells hold `codes`, a line a
/// row: what each cell draws, each row's trailing blanks left off, and the
/// blank rows below the last that holds anything.
///
/// # Errors
///
/// The heap's refusal of the text.
pub fn copied(columns: u16, codes: &[u8]) -> Result<String, TryReserveError> {
    let width = usize::from(columns.max(1));
    let mut text = String::new();
    text.try_reserve_exact(codes.len() * GLYPH_UTF8 + codes.len().div_ceil(width))?;
    for row in codes.chunks(width) {
        let start = text.len();
        text.extend(row.iter().map(|&code| tairix_cp437::glyph_of(code)));
        let kept = text[start..].trim_end_matches(is_blank).len();
        text.truncate(start + kept);
        text.push('\n');
    }
    let kept = text.trim_end_matches('\n').len();
    text.truncate(kept);
    Ok(text)
}

/// Whether a cell drawing `ch` shows nothing.
fn is_blank(ch: char) -> bool {
    matches!(ch, ' ' | '\u{A0}')
}

/// What of some text a machine's keyboard can type.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Typing {
    /// The codes to type, each one [`typeable`] admits.
    pub codes: Vec<u8>,
    /// Characters left out, for no key types them as themselves.
    pub left_out: usize,
    /// Whether the text ran past [`MAX_TYPED`] characters, or codes to type,
    /// and what followed was left.
    pub cut: bool,
}

impl Typing {
    /// What the user is told of the text not typed, a headline and a
    /// sentence; none when all of it is.
    #[must_use]
    pub fn shortfall(&self) -> Option<(&'static str, String)> {
        let part = "Not all of the text is being typed";
        let cut = "Only the start of the text is being typed";
        let characters = |n: usize| {
            if n == 1 {
                String::from("1 character")
            } else {
                format!("{n} characters")
            }
        };
        Some(match (self.left_out, self.cut) {
            (0, false) => return None,
            (1, false) => (
                part,
                String::from("1 character could not be typed on this machine and was left out."),
            ),
            (n, false) => (
                part,
                format!("{n} characters could not be typed on this machine and were left out."),
            ),
            (0, true) => (
                cut,
                format!("A paste types at most {MAX_TYPED} characters."),
            ),
            (n, true) => (
                cut,
                format!(
                    "A paste types at most {MAX_TYPED} characters, and {} of those could not \
                     be typed on this machine.",
                    characters(n)
                ),
            ),
        })
    }
}

/// The most of the clipboard a paste reads: the bytes [`MAX_TYPED`]
/// characters take at most in UTF-8.
const MOST_READ: usize = MAX_TYPED * 4;

/// UTF-8 `bytes` as codes to type, or none for bytes that are not UTF-8: a
/// line break of any spelling pressing Enter, a tab Tab, and each other
/// character its own code where it has one, else its plain spelling, else
/// its letter without the accents the page lacks. A combining accent changes
/// the letter before it where the page has the two as one; characters that
/// draw nothing and change nothing pass unseen. No more than [`MAX_TYPED`]
/// characters are read, so a long clipboard costs no more than a short one.
///
/// # Errors
///
/// The heap's refusal of the codes.
pub fn typing(bytes: &[u8]) -> Result<Option<Typing>, TryReserveError> {
    let read = &bytes[..bytes.len().min(MOST_READ)];
    let text = match core::str::from_utf8(read) {
        Ok(text) => text,
        // A character cut at the end of what is read, not a malformed one.
        Err(cut) if cut.error_len().is_none() => {
            core::str::from_utf8(&read[..cut.valid_up_to()]).unwrap_or_default()
        }
        Err(_) => return Ok(None),
    };
    let mut typing = Typing {
        cut: text.len() < bytes.len(),
        ..Typing::default()
    };
    typing.codes.try_reserve_exact(text.len().min(MAX_TYPED))?;
    let mut chars = text.chars().peekable();
    // The last character typed as a code of its own, which an accent after it
    // may change.
    let mut base = None;
    let mut read_chars = 0;
    while let Some(ch) = chars.next() {
        if read_chars == MAX_TYPED {
            typing.cut = true;
            break;
        }
        read_chars += 1;
        if is_nonspacing_mark(ch) {
            if let Some((with_mark, code)) = base.and_then(|base| composed(base, ch)) {
                typing.codes.pop();
                typing.codes.push(code);
                base = Some(with_mark);
            }
            continue;
        }
        if is_default_ignorable(ch) {
            continue;
        }
        let own = code_of(ch);
        let mut one = [0u8; 1];
        let codes: &[u8] = match (ch, own) {
            ('\r', _) => {
                chars.next_if_eq(&'\n');
                &[ENTER]
            }
            ('\n' | '\u{85}' | '\u{2028}' | '\u{2029}', _) => &[ENTER],
            ('\t', _) => &[TAB],
            (_, Some(code)) => {
                one[0] = code;
                &one
            }
            (_, None) => plainly(ch).unwrap_or_else(|| {
                typing.left_out += 1;
                &[]
            }),
        };
        if typing.codes.len() + codes.len() > MAX_TYPED {
            typing.cut = true;
            break;
        }
        typing.codes.extend_from_slice(codes);
        base = own.map(|_| ch);
    }
    Ok(Some(typing))
}

/// `base` with `mark` as the one character that is both, and its code.
fn composed(base: char, mark: char) -> Option<(char, u8)> {
    let mut out = ['\0'; 2 * MAX_DECOMPOSITION];
    let len = normalize([base, mark], Form::Nfc, &mut out).ok()?;
    match out.get(..len)? {
        [one] => Some((*one, code_of(*one)?)),
        _ => None,
    }
}

/// The code that types `ch` as itself, or its letter's once the accents code
/// page 437 lacks are dropped.
fn code_of(ch: char) -> Option<u8> {
    let own = |ch| tairix_cp437::glyph_byte_of(ch).filter(|&code| typeable(code));
    own(ch).or_else(|| {
        let mut decomposed = ['\0'; MAX_DECOMPOSITION];
        let len = normalize([ch], Form::Nfd, &mut decomposed).ok()?;
        match decomposed.get(..len)? {
            [base, marks @ ..] if marks.iter().all(|&mark| is_nonspacing_mark(mark)) => own(*base),
            _ => None,
        }
    })
}

/// The plain spelling of typographic punctuation code page 437 lacks.
fn plainly(ch: char) -> Option<&'static [u8]> {
    Some(match ch {
        '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}' | '\u{2032}' | '\u{2035}' => b"'",
        '\u{201C}' | '\u{201D}' | '\u{201E}' | '\u{201F}' | '\u{2033}' | '\u{2036}' => b"\"",
        '\u{2010}'..='\u{2015}' | '\u{2212}' => b"-",
        '\u{2026}' => b"...",
        '\u{2000}'..='\u{200A}' | '\u{202F}' | '\u{205F}' | '\u{3000}' => b" ",
        '\u{2039}' => b"<",
        '\u{203A}' => b">",
        // The bullet is 07h, a control; the bullet operator is drawn alike.
        '\u{2022}' => &[0xF9],
        _ => return None,
    })
}

#[cfg(test)]
#[path = "text_tests.rs"]
mod tests;
