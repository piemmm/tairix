//! A text area read through its commands, as RISC OS 5.30's DrawFile reads
//! one: its columns, its colours, then its text. To paint it, the text must
//! open `\!` with version 1, may change to a font only after defining it,
//! and holds no command but the format's; to declare its fonts, only a
//! version given must be 1, and no command may be foreign.

use tairix_drawing::riscos::format::{self, Object, OBJECT_HEADER, TEXT_COLUMN};
use tairix_riscos_kernel::os::{Fail, Os};

/// `Font_SetFontColours`'s R3: the most anti-aliasing shades.
pub(super) const COLOUR_OFFSET: u32 = 14;

/// The words between a text area's columns and its colours: the columns'
/// end and two reserved.
const AFTER_COLUMNS: usize = 12;

/// The text area's version the format has.
const VERSION: u32 = 1;

/// Why a text area is refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Refusal {
    NoVersion,
    Version(u32),
    Command(u8),
    UndefinedFont(u32),
    /// The host refused memory.
    NoRoom,
}

/// A text area's colours and its text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Area<'a> {
    pub(super) foreground: u32,
    pub(super) background: u32,
    text: &'a [u8],
}

impl<'a> Area<'a> {
    /// The text area `object`.
    pub(super) fn of(object: Object<'a>) -> Self {
        let bytes = object.bytes;
        let mut at = OBJECT_HEADER;
        while format::word(bytes, at) == Some(TEXT_COLUMN) {
            let size = format::word(bytes, at + 4).unwrap_or(0);
            at += usize::try_from(size).unwrap_or(usize::MAX).max(8);
        }
        at += AFTER_COLUMNS;
        let foreground = format::word(bytes, at).unwrap_or(0);
        let background = format::word(bytes, at + 4).unwrap_or(0);
        let text = bytes.get(at + 8..).unwrap_or(&[]);
        let end = text.iter().position(|&b| b == 0).unwrap_or(text.len());
        Self {
            foreground,
            background,
            text: &text[..end],
        }
    }

    /// Its text read through to paint it, found sound or refused.
    pub(super) fn read(&self) -> Result<(), Refusal> {
        read(self.text, Purpose::Paint, None)
    }

    /// The fonts its text defines, as names, read through to declare them.
    pub(super) fn fonts(&self) -> Result<alloc::vec::Vec<&'a [u8]>, Refusal> {
        let mut names = alloc::vec::Vec::new();
        read(self.text, Purpose::DeclareFonts, Some(&mut names))?;
        let text = self.text;
        Ok(names.into_iter().map(|range| &text[range]).collect())
    }
}

/// What a text area is read for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Purpose {
    Paint,
    DeclareFonts,
}

/// Reads through a text's commands.
struct Reader<'t, 'n> {
    text: &'t [u8],
    at: usize,
    purpose: Purpose,
    defined: [bool; 100],
    /// The fonts `\F` defines: each name's place in the text.
    names: Option<&'n mut alloc::vec::Vec<core::ops::Range<usize>>>,
}

impl Reader<'_, '_> {
    fn peek(&self) -> Option<u8> {
        self.text.get(self.at).copied()
    }

    fn take(&mut self) -> Option<u8> {
        let byte = self.peek()?;
        self.at += 1;
        Some(byte)
    }

    fn spaces(&mut self) {
        while self.peek() == Some(b' ') {
            self.at += 1;
        }
    }

    fn number(&mut self) -> u32 {
        self.spaces();
        if self.peek() == Some(b'-') {
            self.at += 1;
        }
        let mut value: u32 = 0;
        while let Some(digit @ b'0'..=b'9') = self.peek() {
            value = value
                .saturating_mul(10)
                .saturating_add(u32::from(digit - b'0'));
            self.at += 1;
        }
        value
    }

    fn word(&mut self) -> core::ops::Range<usize> {
        self.spaces();
        let start = self.at;
        while self
            .peek()
            .is_some_and(|b| !matches!(b, b' ' | b'/' | b'\n'))
        {
            self.at += 1;
        }
        start..self.at
    }

    fn version(&mut self) -> Result<(), Refusal> {
        let version = self.number();
        if version != VERSION {
            return Err(Refusal::Version(version));
        }
        self.end();
        Ok(())
    }

    /// A command's end: a solidus or the line's end.
    fn end(&mut self) {
        self.spaces();
        if matches!(self.peek(), Some(b'/' | b'\n')) {
            self.at += 1;
        }
    }

    fn command(&mut self) -> Result<(), Refusal> {
        let Some(command) = self.take() else {
            return Ok(());
        };
        match command {
            b'!' => self.version()?,
            b'A' => {
                self.take();
                self.end();
            }
            b'B' | b'C' => {
                for _ in 0..3 {
                    self.number();
                }
                self.end();
            }
            b'D' | b'L' | b'P' | b'V' => {
                self.number();
                self.end();
            }
            b'M' => {
                self.number();
                self.number();
                self.end();
            }
            b'U' => {
                self.spaces();
                if self.peek() == Some(b'.') {
                    self.at += 1;
                } else {
                    self.number();
                    self.number();
                }
                self.end();
            }
            b'F' => {
                let number = self.number();
                let name = self.word();
                if let Some(names) = self.names.as_deref_mut() {
                    tairix_util::fallible::try_push(names, name).map_err(|_| Refusal::NoRoom)?;
                }
                self.number();
                self.spaces();
                if self.peek().is_some_and(|b| b.is_ascii_digit()) {
                    self.number();
                }
                self.end();
                if let Some(slot) = usize::try_from(number)
                    .ok()
                    .and_then(|n| self.defined.get_mut(n))
                {
                    *slot = true;
                }
            }
            b'0'..=b'9' => {
                self.at -= 1;
                let number = self.number();
                let defined = usize::try_from(number)
                    .ok()
                    .and_then(|n| self.defined.get(n))
                    .copied()
                    .unwrap_or(false);
                if !defined && self.purpose == Purpose::Paint {
                    return Err(Refusal::UndefinedFont(number));
                }
                if self.peek() == Some(b'/') {
                    self.at += 1;
                }
            }
            b';' => while self.take().is_some_and(|b| b != b'\n') {},
            b'-' | b'\n' | b'\\' => {}
            other => return Err(Refusal::Command(other)),
        }
        Ok(())
    }
}

/// `text` read through for `purpose`, the fonts it defines kept in
/// `names` where given.
fn read(
    text: &[u8],
    purpose: Purpose,
    names: Option<&mut alloc::vec::Vec<core::ops::Range<usize>>>,
) -> Result<(), Refusal> {
    if purpose == Purpose::Paint && !text.starts_with(b"\\!") {
        return Err(Refusal::NoVersion);
    }
    let mut reader = Reader {
        text,
        at: 0,
        purpose,
        defined: [false; 100],
        names,
    };
    while let Some(byte) = reader.take() {
        if byte == b'\\' {
            reader.command()?;
        }
    }
    Ok(())
}

/// The error a refused text area raises.
pub(super) fn refuse(os: &mut Os<'_>, refusal: Refusal) -> Fail {
    super::raise_area(os, refusal)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paint(text: &[u8]) -> Result<(), Refusal> {
        read(text, Purpose::Paint, None)
    }

    fn declare(text: &[u8]) -> Result<alloc::vec::Vec<alloc::vec::Vec<u8>>, Refusal> {
        let mut names = alloc::vec::Vec::new();
        read(text, Purpose::DeclareFonts, Some(&mut names))?;
        Ok(names
            .into_iter()
            .map(|range| text[range].to_vec())
            .collect())
    }

    #[test]
    fn the_recorded_text_areas_are_read_as_genuine_risc_os_read_them() {
        assert_eq!(paint(b"\\! 1\nHello"), Ok(()));
        assert_eq!(paint(b"Hello"), Err(Refusal::NoVersion));
        assert_eq!(paint(b"\\! 2\nHello"), Err(Refusal::Version(2)));
        assert_eq!(paint(b"\\! 1\n\\9Hello"), Err(Refusal::UndefinedFont(9)));
        assert_eq!(paint(b"\\! 1\n\\QHello"), Err(Refusal::Command(b'Q')));
        assert_eq!(declare(b"\\! 1\nHello"), Ok(alloc::vec::Vec::new()));
        assert_eq!(declare(b"Hello"), Ok(alloc::vec::Vec::new()));
        assert_eq!(declare(b"\\! 2\nHello"), Err(Refusal::Version(2)));
        assert_eq!(declare(b"\\! 1\n\\9Hello"), Ok(alloc::vec::Vec::new()));
        assert_eq!(declare(b"\\! 1\n\\QHello"), Err(Refusal::Command(b'Q')));
    }

    #[test]
    fn a_font_defined_first_may_be_changed_to_and_is_declared() {
        let defined = b"\\! 1\n\\F 3 Homerton.Medium 12\n\\3Hi\\A C/\\C 255 0 0/x\\;note\nmore";
        assert_eq!(paint(defined), Ok(()));
        assert_eq!(
            declare(defined),
            Ok(alloc::vec![b"Homerton.Medium".to_vec()])
        );
        assert_eq!(paint(b"\\! 1/\\F12Trinity.Bold 10 12/\\12/x"), Ok(()));
        assert_eq!(paint(b"\\! 1\n\\U 5 1/\\U./\\V-2\\-\\\\"), Ok(()));
    }
}
