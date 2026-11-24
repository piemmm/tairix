//! A text area's text read a piece at a time, as RISC OS 5.30's DrawFile
//! reads it for painting and for declaring its fonts alike.
//!
//! A command with arguments of variable length must end at a solidus or a
//! newline, which it consumes; `\A`, `\V` and a font change take an optional
//! solidus. Numbers may carry a sign. Control characters are dropped but for
//! a tab, which reads as a space, and the newline.

use super::Refusal;

/// The version a text must declare.
const VERSION: u32 = 1;
/// The highest font number; a larger one reads as it.
pub(super) const FONTS: usize = 100;
/// A font size is given in points, taken in sixteenths, and is never less
/// than a point.
const SIXTEENTHS: i32 = 16;

/// How a line is set within its column.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Align {
    Left,
    Right,
    Centre,
    Double,
}

/// A command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Command<'t> {
    Align(Align),
    /// `\B` (the background) or `\C`, each channel within a byte.
    Colour {
        background: bool,
        rgb: [u8; 3],
    },
    /// `\D`, which the column objects override.
    Columns,
    /// `\F`: a number, a name, and a width and height in sixteenths of a
    /// point.
    Define {
        number: u8,
        name: &'t [u8],
        width: u32,
        height: u32,
    },
    /// A change to a defined font.
    Select(u8),
    /// `\L`, `\M` and `\P`, in points as given.
    Leading(i32),
    Margins(i32, i32),
    Paragraph(i32),
    /// `\U`: its position and thickness, or none to turn it off.
    Underline(Option<(i8, u8)>),
    /// `\V`, in points.
    Move(i32),
}

/// A piece of text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Piece<'t> {
    /// A character to print.
    Char(u8),
    /// A newline in the body.
    Newline,
    /// `\` and a newline.
    Break,
    /// `\-`.
    SoftHyphen,
    Command(Command<'t>),
}

/// Reads a text's pieces.
pub(super) struct Reader<'t> {
    text: &'t [u8],
    at: usize,
}

impl<'t> Reader<'t> {
    pub(super) fn new(text: &'t [u8]) -> Self {
        Self { text, at: 0 }
    }

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

    /// A number: spaces, a sign, then at least one digit.
    fn number(&mut self, command: u8) -> Result<i32, Refusal> {
        self.spaces();
        let negative = match self.peek() {
            Some(b'-') => true,
            Some(b'+') => false,
            _ => return self.digits(command),
        };
        self.at += 1;
        let magnitude = self.digits(command)?;
        Ok(if negative {
            magnitude.saturating_neg()
        } else {
            magnitude
        })
    }

    fn digits(&mut self, command: u8) -> Result<i32, Refusal> {
        let mut value: i32 = 0;
        let start = self.at;
        while let Some(digit @ b'0'..=b'9') = self.peek() {
            value = value
                .saturating_mul(10)
                .saturating_add(i32::from(digit - b'0'));
            self.at += 1;
        }
        if self.at == start {
            return Err(Refusal::Command(command));
        }
        Ok(value)
    }

    /// A variable-length command's end: a solidus or a newline, or the
    /// text's own end.
    fn end(&mut self, command: u8) -> Result<(), Refusal> {
        match self.peek() {
            Some(b'/' | b'\n') => {
                self.at += 1;
                Ok(())
            }
            None => Ok(()),
            Some(_) => Err(Refusal::Command(command)),
        }
    }

    fn optional_solidus(&mut self) {
        if self.peek() == Some(b'/') {
            self.at += 1;
        }
    }

    /// The next piece, or none at the text's end.
    pub(super) fn next(&mut self) -> Result<Option<Piece<'t>>, Refusal> {
        loop {
            let Some(byte) = self.take() else {
                return Ok(None);
            };
            return Ok(Some(match byte {
                b'\\' => match self.escape()? {
                    Some(piece) => piece,
                    None => continue,
                },
                b'\n' => Piece::Newline,
                b'\t' => Piece::Char(b' '),
                0..=31 => continue,
                _ => Piece::Char(byte),
            }));
        }
    }

    /// What follows a backslash: none for a comment, or a backslash ending
    /// the text.
    fn escape(&mut self) -> Result<Option<Piece<'t>>, Refusal> {
        let Some(command) = self.take() else {
            return Ok(None);
        };
        let piece = match command {
            b'!' => {
                let version = self.number(command)?;
                self.end(command)?;
                if version.cast_unsigned() != VERSION {
                    return Err(Refusal::Version(version.cast_unsigned()));
                }
                return Ok(None);
            }
            b'A' => {
                let align = match self.take().map(|b| b.to_ascii_uppercase()) {
                    Some(b'L') => Align::Left,
                    Some(b'R') => Align::Right,
                    Some(b'C') => Align::Centre,
                    Some(b'D') => Align::Double,
                    _ => return Err(Refusal::Command(command)),
                };
                self.optional_solidus();
                Command::Align(align)
            }
            b'B' | b'C' => {
                let mut rgb = [0; 3];
                for channel in &mut rgb {
                    *channel = u8::try_from(self.number(command)?.clamp(0, 255)).unwrap_or(u8::MAX);
                }
                self.end(command)?;
                Command::Colour {
                    background: command == b'B',
                    rgb,
                }
            }
            b'D' => {
                self.number(command)?;
                self.end(command)?;
                Command::Columns
            }
            b'F' => self.define()?,
            b'L' | b'P' => {
                let points = self.number(command)?;
                self.end(command)?;
                if command == b'L' {
                    Command::Leading(points)
                } else {
                    Command::Paragraph(points)
                }
            }
            b'M' => {
                let left = self.number(command)?;
                let right = self.number(command)?;
                self.end(command)?;
                Command::Margins(left, right)
            }
            b'U' => {
                self.spaces();
                let underline = if self.peek() == Some(b'.') {
                    self.at += 1;
                    None
                } else {
                    let position = self.number(command)?.clamp(-128, 127);
                    let thickness = self.number(command)?.clamp(0, 255);
                    Some((
                        i8::try_from(position).unwrap_or(0),
                        u8::try_from(thickness).unwrap_or(0),
                    ))
                };
                self.end(command)?;
                Command::Underline(underline)
            }
            b'V' => {
                let points = self.number(command)?;
                self.optional_solidus();
                Command::Move(points)
            }
            b'0'..=b'9' => {
                self.at -= 1;
                let number = font_number(self.digits(command)?);
                self.optional_solidus();
                Command::Select(number)
            }
            b';' => {
                while self.take().is_some_and(|b| b != b'\n') {}
                return Ok(None);
            }
            b'-' => return Ok(Some(Piece::SoftHyphen)),
            b'\n' => return Ok(Some(Piece::Break)),
            b'\\' => return Ok(Some(Piece::Char(b'\\'))),
            other => return Err(Refusal::Command(other)),
        };
        Ok(Some(Piece::Command(piece)))
    }

    /// `\F`'s arguments.
    fn define(&mut self) -> Result<Command<'t>, Refusal> {
        const F: u8 = b'F';
        let number = font_number(self.number(F)?);
        self.spaces();
        let start = self.at;
        while self
            .peek()
            .is_some_and(|b| !matches!(b, b' ' | b'/' | b'\n'))
        {
            self.at += 1;
        }
        let name = &self.text[start..self.at];
        if name.is_empty() {
            return Err(Refusal::Command(F));
        }
        let height = self.number(F)?;
        let width = if self.peek() == Some(b' ') {
            self.number(F)?
        } else {
            height
        };
        self.end(F)?;
        let sixteenths = |points: i32| points.max(1).saturating_mul(SIXTEENTHS).cast_unsigned();
        Ok(Command::Define {
            number,
            name,
            width: sixteenths(width),
            height: sixteenths(height),
        })
    }

    /// Whether the text opens with the version command.
    pub(super) fn versioned(&self) -> bool {
        self.text.starts_with(b"\\!")
    }
}

/// A font number as given, the highest taken for any larger.
fn font_number(given: i32) -> u8 {
    let number = usize::try_from(given).unwrap_or(0).min(FONTS - 1);
    u8::try_from(number).unwrap_or(u8::MAX)
}

#[cfg(test)]
mod tests;
