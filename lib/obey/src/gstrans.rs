//! GSTrans: a string read as `OS_GSInit` and `OS_GSRead` read it, with `|`
//! escapes, quotes, `<n>` character codes and `<variable>` references, each
//! variable delivered as its type says; and `OS_GSTrans`, which reads one
//! whole into a buffer.

use alloc::vec::Vec;

use tairix_util::fallible;

use crate::number::{decimal, read_unsigned, Reading};
use crate::vars::{Value, Variables};
use crate::{byte, is_terminator, Budget, Error};

/// The most expansions active at once; one more expands to nothing.
const MOST_ACTIVE: usize = 127;

/// The longest text between `<` and `>` that is a reference.
const MOST_REFERENCE: usize = 255;

/// `OS_GSInit`'s flags, bits 29 to 31 of its R2.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Flags(u32);

impl Flags {
    /// Spaces kept, `|` escapes and quotes read.
    pub const NONE: Self = Self(0);
    /// A space outside quotes ends the string.
    pub const SPACE_ENDS: Self = Self(1 << 29);
    /// `|` is an ordinary character.
    pub const NO_ESCAPES: Self = Self(1 << 30);
    /// `"` is an ordinary character.
    pub const NO_QUOTES: Self = Self(1 << 31);

    /// The flags of `r2`, its other bits ignored.
    #[must_use]
    pub const fn from_bits(r2: u32) -> Self {
        Self(r2 & 0xE000_0000)
    }

    /// The bits.
    #[must_use]
    pub const fn bits(self) -> u32 {
        self.0
    }

    /// These and `other`.
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    const fn has(self, flag: Self) -> bool {
        self.0 & flag.0 != 0
    }
}

/// What one `OS_GSRead` produced.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Read {
    /// A character: a byte, or for `<n>` the whole number, bit 7 set where
    /// a `|!` came before it.
    Char(u32),
    /// The end, and the byte that ended it: CR, LF, NUL or a space; or after
    /// a closing quote, the first byte after the quote and its spaces.
    End(u8),
}

/// A variable's value being delivered.
struct Frame {
    bytes: Vec<u8>,
    at: usize,
    /// Whether it is read as GSTrans reads text, as a macro is, rather than
    /// delivered byte for byte.
    scanned: bool,
}

/// What a `<` began.
enum Reference {
    /// A character code.
    Char(u32),
    /// Nothing: the `<` is itself.
    Literal,
    /// A variable, now expanding, or expanding to nothing.
    Expanded,
}

/// A reading's progress, given up between `OS_GSRead` calls by a caller that
/// cannot keep the reader itself: where it is in its text, and the values it
/// is part way through delivering.
pub struct Progress {
    at: usize,
    quoted: bool,
    first: u8,
    frames: Vec<Frame>,
}

/// A string being read: begun as `OS_GSInit` begins it, and continued, one
/// character a call, as `OS_GSRead` continues it.
pub struct Reader<'t> {
    text: &'t [u8],
    at: usize,
    flags: Flags,
    quoted: bool,
    first: u8,
    frames: Vec<Frame>,
}

impl<'t> Reader<'t> {
    /// `OS_GSInit`: `text` from its first byte that is not a space. That
    /// byte, where it is `"` and quotes are read, opens a quoted string.
    #[must_use]
    pub fn new(text: &'t [u8], flags: Flags) -> Self {
        let at = text.iter().take_while(|&&b| b == b' ').count();
        let first = byte(text, at);
        let quoted = !flags.has(Flags::NO_QUOTES) && first == b'"';
        Self {
            text,
            at: at + usize::from(quoted),
            flags,
            quoted,
            first,
            frames: Vec::new(),
        }
    }

    /// The reading's progress, given up so a later call can resume it.
    #[must_use]
    pub fn into_progress(self) -> Progress {
        Progress {
            at: self.at,
            quoted: self.quoted,
            first: self.first,
            frames: self.frames,
        }
    }

    /// A reading of `text` under `flags` resumed from `progress`, which a
    /// reading of the same text and flags gave up.
    #[must_use]
    pub fn resume(text: &'t [u8], flags: Flags, progress: Progress) -> Self {
        Self {
            text,
            at: progress.at.min(text.len() + 1),
            flags,
            quoted: progress.quoted,
            first: progress.first,
            frames: progress.frames,
        }
    }

    /// The first character, the R1 `OS_GSInit` answers: before any
    /// translation, an opening quote itself.
    #[must_use]
    pub const fn first(&self) -> u8 {
        self.first
    }

    /// Whether the string is empty: it starts at a CR, LF or NUL.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        is_terminator(self.first)
    }

    /// Where in the text reading has reached: the offset of the next byte it
    /// takes, one past the text's end once a terminator past it is read.
    #[must_use]
    pub const fn position(&self) -> usize {
        self.at
    }

    /// Whether the next read continues a variable's value.
    #[must_use]
    pub fn expanding(&self) -> bool {
        !self.frames.is_empty()
    }

    /// The next byte of the text taken as it is, untranslated, as the
    /// expression evaluator copies the inside of a quoted string. NUL past
    /// the end.
    pub fn take_raw(&mut self) -> u8 {
        let b = byte(self.text, self.at);
        self.at = (self.at + 1).min(self.text.len() + 1);
        b
    }

    /// The source the next byte comes from: the macro being expanded, else
    /// the text.
    fn next_source(&mut self) -> u8 {
        match self.frames.last_mut() {
            Some(frame) => {
                let b = byte(&frame.bytes, frame.at);
                frame.at += 1;
                b
            }
            None => self.take_raw(),
        }
    }

    fn peek_source(&self) -> u8 {
        match self.frames.last() {
            Some(frame) => byte(&frame.bytes, frame.at),
            None => byte(self.text, self.at),
        }
    }

    /// The rest of the source, from the next byte.
    fn rest_of_source(&self) -> &[u8] {
        match self.frames.last() {
            Some(frame) => frame.bytes.get(frame.at..).unwrap_or(&[]),
            None => self.text.get(self.at..).unwrap_or(&[]),
        }
    }

    fn advance_source(&mut self, by: usize) {
        match self.frames.last_mut() {
            Some(frame) => frame.at += by,
            None => self.at += by,
        }
    }

    /// `OS_GSRead`: the next character, or the end.
    pub fn read(&mut self, vars: &mut dyn Variables, budget: &mut Budget) -> Result<Read, Error> {
        let mut mask = 0u32;
        loop {
            if let Some(frame) = self.frames.last_mut().filter(|frame| !frame.scanned) {
                if let Some(&b) = frame.bytes.get(frame.at) {
                    frame.at += 1;
                    return Ok(Read::Char(u32::from(b) | mask));
                }
                self.frames.pop();
                continue;
            }
            let in_macro = !self.frames.is_empty();
            let b = self.next_source();
            match b {
                0 | 10 | 13 if in_macro => {
                    self.frames.pop();
                }
                0 | 10 | 13 if self.quoted => return Err(Error::BadString),
                0 | 10 | 13 => return Ok(Read::End(b)),
                b' ' if self.flags.has(Flags::SPACE_ENDS) && !self.quoted && !in_macro => {
                    return Ok(Read::End(b' '));
                }
                0x01..=0x1F => return Err(Error::BadString),
                b'"' if self.quoted => {
                    if self.peek_source() == b'"' {
                        self.next_source();
                        return Ok(Read::Char(u32::from(b'"') | mask));
                    }
                    while self.peek_source() == b' ' {
                        self.next_source();
                    }
                    self.quoted = false;
                    return Ok(Read::End(self.peek_source()));
                }
                b'|' if !self.flags.has(Flags::NO_ESCAPES) => match escaped(self.next_source()) {
                    Escape::Char(c) => return Ok(Read::Char(u32::from(c) | mask)),
                    Escape::TopBit => mask = 0x80,
                    Escape::Bad => return Err(Error::BadString),
                },
                b'<' => match self.reference(vars, budget)? {
                    Reference::Char(c) => return Ok(Read::Char(c | mask)),
                    Reference::Literal => return Ok(Read::Char(u32::from(b'<') | mask)),
                    Reference::Expanded => {}
                },
                other => return Ok(Read::Char(u32::from(other) | mask)),
            }
        }
    }

    /// What follows a `<`: a character code or a variable up to the `>`, or
    /// nothing where no `>` comes before a space, a control character or the
    /// longest reference.
    fn reference(
        &mut self,
        vars: &mut dyn Variables,
        budget: &mut Budget,
    ) -> Result<Reference, Error> {
        let rest = self.rest_of_source();
        let Some(end) = rest
            .iter()
            .take(MOST_REFERENCE + 1)
            .position(|&b| b == b'>' || b <= b' ')
            .filter(|&end| end > 0 && rest[end] == b'>')
        else {
            return Ok(Reference::Literal);
        };
        let mut content = [0u8; MOST_REFERENCE];
        content[..end].copy_from_slice(&rest[..end]);
        let content = &content[..end];
        self.advance_source(end + 1);
        if let Ok((code, used)) = read_unsigned(content, Reading::plain(10)) {
            if used == content.len() {
                return Ok(Reference::Char(u32::try_from(code).unwrap_or(u32::MAX)));
            }
        }
        budget.charge(vars.cost(content))?;
        if self.frames.len() >= MOST_ACTIVE {
            return Ok(Reference::Expanded);
        }
        let Some(value) = vars.lookup(content)? else {
            return Ok(Reference::Expanded);
        };
        let frame = match value {
            Value::String(bytes) => Frame {
                bytes,
                at: 0,
                scanned: false,
            },
            Value::Number(n) => Frame {
                bytes: decimal(n)?,
                at: 0,
                scanned: false,
            },
            Value::Macro(bytes) => Frame {
                bytes,
                at: 0,
                scanned: true,
            },
        };
        if !fallible::push(&mut self.frames, frame) {
            return Err(Error::SystemHeapFull);
        }
        Ok(Reference::Expanded)
    }
}

/// What a `|` makes of the byte after it.
enum Escape {
    Char(u8),
    /// `|!`: the next character with its top bit set.
    TopBit,
    /// A control character, the string's end among them.
    Bad,
}

fn escaped(b: u8) -> Escape {
    Escape::Char(match b {
        0x00..=0x1F => return Escape::Bad,
        b'!' => return Escape::TopBit,
        b'?' | 0x7F => 0x7F,
        0x20..=0x3F | b'|' => b,
        b'`' => 0x1F,
        0x40..=0x7E => b & 0x1F,
        0x80..=0xFF => b ^ 0x20,
    })
}

/// What `OS_GSTrans` did.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Translation {
    /// The bytes stored, a terminator after them not counted.
    pub stored: usize,
    /// Where reading stopped in the text; `None` where it stopped inside a
    /// variable's value, as a quoted string a macro closes does.
    pub next: Option<usize>,
    /// The byte that ended the string, stored after the others; `None` where
    /// the buffer filled first, a result needing one byte more than it has.
    pub ended: Result<Option<u8>, Error>,
}

/// `OS_GSTrans`: `text` read whole into `buffer`, each character's low byte
/// stored.
pub fn gstrans(
    text: &[u8],
    flags: Flags,
    vars: &mut dyn Variables,
    budget: &mut Budget,
    buffer: &mut [u8],
) -> Translation {
    let mut reader = Reader::new(text, flags);
    let mut stored = 0;
    let ended = loop {
        let Some(slot) = buffer.get_mut(stored) else {
            break Ok(None);
        };
        match reader.read(vars, budget) {
            Ok(Read::Char(c)) => {
                *slot = c.to_le_bytes()[0];
                stored += 1;
            }
            Ok(Read::End(terminator)) => {
                *slot = terminator;
                break Ok(Some(terminator));
            }
            Err(e) => break Err(e),
        }
    };
    Translation {
        stored,
        next: (!reader.expanding()).then(|| reader.position()),
        ended,
    }
}

/// `text` read whole, at most `longest` bytes of it, and where reading
/// stopped; `BufferOverflow` where it is longer.
pub(crate) fn translated(
    text: &[u8],
    flags: Flags,
    vars: &mut dyn Variables,
    budget: &mut Budget,
    longest: usize,
) -> Result<(Vec<u8>, usize), Error> {
    let mut reader = Reader::new(text, flags);
    let mut out = Vec::new();
    loop {
        match reader.read(vars, budget)? {
            Read::Char(c) => {
                if out.len() == longest {
                    return Err(Error::BufferOverflow);
                }
                if !fallible::push(&mut out, c.to_le_bytes()[0]) {
                    return Err(Error::SystemHeapFull);
                }
            }
            Read::End(_) => return Ok((out, reader.position())),
        }
    }
}

#[cfg(test)]
#[path = "gstrans_tests.rs"]
mod tests;
