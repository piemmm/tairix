//! The tokens of ISO 32000-2 §7.2: what a file's objects and a page's content
//! are spelled in.

use alloc::vec::Vec;

use tairix_util::fallible::try_push;

use crate::PdfError;

/// One token.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Token<'a> {
    Integer(i64),
    Real(f64),
    /// A literal or hexadecimal string, its escapes resolved.
    String(Vec<u8>),
    /// A name, its `#xx` escapes resolved.
    Name(Vec<u8>),
    ArrayOpen,
    ArrayClose,
    DictOpen,
    DictClose,
    BraceOpen,
    BraceClose,
    /// A run of regular characters that is no number: `true`, `obj`, `R`, a
    /// content operator.
    Keyword(&'a [u8]),
}

/// Whether `byte` is PDF white space.
pub(crate) const fn is_white(byte: u8) -> bool {
    matches!(byte, b'\0' | b'\t' | b'\n' | b'\x0c' | b'\r' | b' ')
}

/// Whether `byte` is a delimiter.
pub(crate) const fn is_delimiter(byte: u8) -> bool {
    matches!(
        byte,
        b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%'
    )
}

/// Whether `byte` is a regular character: neither white space nor a
/// delimiter.
pub(crate) const fn is_regular(byte: u8) -> bool {
    !is_white(byte) && !is_delimiter(byte)
}

/// A cursor over bytes, read a token at a time.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Lexer<'a> {
    data: &'a [u8],
    at: usize,
}

impl<'a> Lexer<'a> {
    /// A lexer over `data` from offset `at`.
    pub(crate) const fn new(data: &'a [u8], at: usize) -> Self {
        Self { data, at }
    }

    /// Where the next byte is read from.
    pub(crate) const fn at(&self) -> usize {
        self.at
    }

    /// The bytes it reads.
    pub(crate) const fn data(&self) -> &'a [u8] {
        self.data
    }

    /// Move to offset `at`, past bytes no token spells.
    pub(crate) fn seek(&mut self, at: usize) {
        self.at = at.min(self.data.len());
    }

    /// Skip white space and comments.
    pub(crate) fn skip_white(&mut self) {
        while let Some(&byte) = self.data.get(self.at) {
            if is_white(byte) {
                self.at += 1;
            } else if byte == b'%' {
                while let Some(&byte) = self.data.get(self.at) {
                    if byte == b'\n' || byte == b'\r' {
                        break;
                    }
                    self.at += 1;
                }
            } else {
                break;
            }
        }
    }

    /// The next token, or `None` at the end of the data.
    ///
    /// # Errors
    ///
    /// [`PdfError::Malformed`] for a token out of the grammar, and
    /// [`PdfError::OutOfMemory`].
    pub(crate) fn next(&mut self) -> Result<Option<Token<'a>>, PdfError> {
        self.skip_white();
        let Some(&byte) = self.data.get(self.at) else {
            return Ok(None);
        };
        let token = match byte {
            b'[' => {
                self.at += 1;
                Token::ArrayOpen
            }
            b']' => {
                self.at += 1;
                Token::ArrayClose
            }
            b'{' => {
                self.at += 1;
                Token::BraceOpen
            }
            b'}' => {
                self.at += 1;
                Token::BraceClose
            }
            b'<' if self.data.get(self.at + 1) == Some(&b'<') => {
                self.at += 2;
                Token::DictOpen
            }
            b'>' if self.data.get(self.at + 1) == Some(&b'>') => {
                self.at += 2;
                Token::DictClose
            }
            b'<' => Token::String(self.hex_string()?),
            b'(' => Token::String(self.literal_string()?),
            b'/' => Token::Name(self.name()?),
            b'+' | b'-' | b'.' | b'0'..=b'9' => match self.number() {
                Some(number) => number,
                None => Token::Keyword(self.regular_run()),
            },
            _ if is_regular(byte) => Token::Keyword(self.regular_run()),
            // A stray `)` or `>`: passed over, so a reader that carries on
            // past the error makes progress.
            _ => {
                self.at += 1;
                return Err(PdfError::Malformed);
            }
        };
        Ok(Some(token))
    }

    /// The run of regular characters at the cursor.
    fn regular_run(&mut self) -> &'a [u8] {
        let start = self.at;
        while self.data.get(self.at).is_some_and(|&byte| is_regular(byte)) {
            self.at += 1;
        }
        &self.data[start..self.at]
    }

    /// The number at the cursor, read as far as it goes so a number run
    /// into what follows is split from it, as every reader splits it; `None`
    /// where no digit begins one. PDF numbers have no exponent, but one some
    /// files write is read, and a doubled minus is read as one, as Acrobat
    /// reads it.
    fn number(&mut self) -> Option<Token<'a>> {
        let data = self.data;
        let negative = data.get(self.at) == Some(&b'-');
        let mut at = self.at;
        if matches!(data.get(at), Some(b'+' | b'-')) {
            at += 1;
        }
        if negative && data.get(at) == Some(&b'-') {
            at += 1;
        }
        let start = at;
        let (mut digits, mut point) = (0usize, false);
        while let Some(&byte) = data.get(at) {
            match byte {
                b'0'..=b'9' => digits += 1,
                b'.' if !point => point = true,
                _ => break,
            }
            at += 1;
        }
        if digits == 0 {
            return None;
        }
        let mut exponent = false;
        if matches!(data.get(at), Some(b'e' | b'E')) {
            let mut probe = at + 1;
            if matches!(data.get(probe), Some(b'+' | b'-')) {
                probe += 1;
            }
            let exponent_digits = probe;
            while data.get(probe).is_some_and(u8::is_ascii_digit) {
                probe += 1;
            }
            if probe > exponent_digits {
                at = probe;
                exponent = true;
            }
        }
        let text = core::str::from_utf8(&data[start..at]).ok()?;
        let token = match text.parse::<i64>() {
            Ok(value) if !point && !exponent => {
                Token::Integer(if negative { -value } else { value })
            }
            _ => {
                let value = text.parse::<f64>().ok().filter(|value| value.is_finite())?;
                Token::Real(if negative { -value } else { value })
            }
        };
        self.at = at;
        Some(token)
    }

    /// A name, the `/` at the cursor.
    fn name(&mut self) -> Result<Vec<u8>, PdfError> {
        self.at += 1;
        let run = self.regular_run();
        let mut name = Vec::new();
        name.try_reserve_exact(run.len())?;
        let mut index = 0;
        while index < run.len() {
            let byte = run[index];
            let escaped = (byte == b'#')
                .then(|| run.get(index + 1..index + 3))
                .flatten()
                .and_then(|pair| Some(hex_value(pair[0])? << 4 | hex_value(pair[1])?));
            if let Some(value) = escaped {
                name.push(value);
                index += 3;
            } else {
                name.push(byte);
                index += 1;
            }
        }
        Ok(name)
    }

    /// A hexadecimal string, the `<` at the cursor.
    fn hex_string(&mut self) -> Result<Vec<u8>, PdfError> {
        self.at += 1;
        let mut bytes = Vec::new();
        let mut high: Option<u8> = None;
        loop {
            let byte = *self.data.get(self.at).ok_or(PdfError::Malformed)?;
            self.at += 1;
            if byte == b'>' {
                break;
            }
            if is_white(byte) {
                continue;
            }
            let value = hex_value(byte).ok_or(PdfError::Malformed)?;
            match high.take() {
                Some(high) => try_push(&mut bytes, high << 4 | value)?,
                None => high = Some(value),
            }
        }
        // An odd final digit stands for its high half.
        if let Some(high) = high {
            try_push(&mut bytes, high << 4)?;
        }
        Ok(bytes)
    }

    /// A literal string, the `(` at the cursor: balanced parentheses kept,
    /// escapes resolved, and an unescaped end of line read as a line feed.
    fn literal_string(&mut self) -> Result<Vec<u8>, PdfError> {
        self.at += 1;
        let mut bytes = Vec::new();
        let mut depth = 0usize;
        loop {
            let byte = *self.data.get(self.at).ok_or(PdfError::Malformed)?;
            self.at += 1;
            match byte {
                b'(' => {
                    depth += 1;
                    try_push(&mut bytes, byte)?;
                }
                b')' if depth == 0 => break,
                b')' => {
                    depth -= 1;
                    try_push(&mut bytes, byte)?;
                }
                b'\\' => self.escape(&mut bytes)?,
                b'\r' => {
                    if self.data.get(self.at) == Some(&b'\n') {
                        self.at += 1;
                    }
                    try_push(&mut bytes, b'\n')?;
                }
                _ => try_push(&mut bytes, byte)?,
            }
        }
        Ok(bytes)
    }

    /// The escape after a backslash in a literal string.
    fn escape(&mut self, bytes: &mut Vec<u8>) -> Result<(), PdfError> {
        let byte = *self.data.get(self.at).ok_or(PdfError::Malformed)?;
        self.at += 1;
        let escaped = match byte {
            b'n' => b'\n',
            b'r' => b'\r',
            b't' => b'\t',
            b'b' => 0x08,
            b'f' => 0x0c,
            b'0'..=b'7' => {
                let mut value = u32::from(byte - b'0');
                for _ in 0..2 {
                    match self.data.get(self.at) {
                        Some(&digit @ b'0'..=b'7') => {
                            value = value * 8 + u32::from(digit - b'0');
                            self.at += 1;
                        }
                        _ => break,
                    }
                }
                // A high-order overflow is ignored, as the specification says.
                (value & 0xff) as u8
            }
            // A backslash before an end of line continues the string.
            b'\r' => {
                if self.data.get(self.at) == Some(&b'\n') {
                    self.at += 1;
                }
                return Ok(());
            }
            b'\n' => return Ok(()),
            // Any other escaped byte stands for itself, `\\`, `\(` and `\)`
            // among them.
            other => other,
        };
        Ok(try_push(bytes, escaped)?)
    }
}

/// The value of hexadecimal digit `byte`.
pub(crate) const fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
#[path = "lex_tests.rs"]
mod tests;
