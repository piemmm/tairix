//! A statement's parse: a pure function of its text and of the values of the
//! expressions in it read so far. It runs again from the statement's start
//! each time another value arrives, so an operand may call a function that
//! runs BASIC and the machine holds nothing of the parse between steps.

use tairix_riscos_arm::reg::Reg;

use crate::machine::{in_name, Trap};
use crate::messages::Error;
use crate::token;
use crate::value::{Arith, Number, Text, Value};
use crate::Fault;

/// How an expression in a statement is read.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Want {
    /// Its first factor alone: a register, a coprocessor or its register.
    Factor,
    /// The whole expression.
    Expression,
    /// A SWI: a number, or a name the machine looks up.
    Swi,
}

impl Want {
    /// The word a continuation carries it as.
    pub(super) const fn word(self) -> u32 {
        self as u32
    }

    /// The want a continuation's word is.
    pub(super) const fn of(word: u32) -> Option<Self> {
        Some(match word {
            0 => Self::Factor,
            1 => Self::Expression,
            2 => Self::Swi,
            _ => return None,
        })
    }
}

/// An expression's value, and the address its text ended at.
#[derive(Clone, Copy, Debug)]
pub(super) struct Evaluated {
    pub(super) value: Value,
    pub(super) end: u32,
}

/// Why a parse stopped short of its statement's end.
#[derive(Clone, Copy, Debug)]
pub(super) enum Halt {
    /// The expression at `at` is to be evaluated as `want` says.
    Need { at: u32, want: Want },
    /// An error.
    Fail(Trap),
}

impl From<Error> for Halt {
    fn from(error: Error) -> Self {
        Self::Fail(Trap::Basic(error))
    }
}

impl From<Fault> for Halt {
    fn from(fault: Fault) -> Self {
        Self::Fail(Trap::from(fault))
    }
}

/// A parse's result.
pub(super) type Parse<T> = Result<T, Halt>;

/// What a statement lays down.
#[derive(Clone, Copy, Debug)]
#[expect(
    clippy::large_enum_variant,
    reason = "a string is carried inline, so laying one down never allocates"
)]
pub(super) enum Item {
    /// Nothing: an empty statement, or a comment.
    Nothing,
    /// An instruction's word, at a word boundary.
    Instruction(u32),
    /// Bytes, where `P%` is.
    Data { bytes: [u8; 12], len: u8 },
    /// A string's bytes.
    Text(Text),
    /// `OPT`'s new value.
    Opt(i32),
    /// `ALIGN`.
    Align,
}

impl Item {
    /// The item laying down `bytes`, at most twelve.
    pub(super) fn data(bytes: &[u8]) -> Self {
        let mut held = [0; 12];
        let len = bytes.len().min(held.len());
        held[..len].copy_from_slice(&bytes[..len]);
        Self::Data {
            bytes: held,
            len: u8::try_from(len).unwrap_or(0),
        }
    }

    /// The bytes it lays down.
    pub(super) fn bytes(&self) -> &[u8] {
        match self {
            Self::Data { bytes, len } => &bytes[..usize::from(*len)],
            Self::Text(text) => text.as_bytes(),
            Self::Nothing | Self::Instruction(_) | Self::Opt(_) | Self::Align => &[],
        }
    }
}

/// A statement parsed: what it lays down, and its terminator's address.
#[derive(Clone, Copy, Debug)]
pub(super) struct Parsed {
    pub(super) item: Item,
    pub(super) end: u32,
}

/// What the parse knows of where it runs.
#[derive(Clone, Copy, Debug)]
pub(super) struct Context {
    /// Where an instruction goes: `P%`, aligned to a word.
    pub(super) address: u32,
    /// Whether `OPT`'s bit 1 is clear, so that the errors it covers leave
    /// what BASIC V lays down for them instead.
    pub(super) suppress: bool,
}

/// A statement being parsed.
pub(super) struct Parser<'a> {
    /// The statement's text from the parse's start to its line's CR.
    text: &'a [u8],
    /// The address `text` starts at.
    base: u32,
    at: usize,
    tape: &'a [Evaluated],
    used: usize,
    pub(super) arith: &'a mut Arith,
    pub(super) context: Context,
}

impl<'a> Parser<'a> {
    /// The parse of `text`, at `base`, with `tape` evaluated so far.
    pub(super) fn new(
        text: &'a [u8],
        base: u32,
        tape: &'a [Evaluated],
        arith: &'a mut Arith,
        context: Context,
    ) -> Self {
        Self {
            text,
            base,
            at: 0,
            tape,
            used: 0,
            arith,
            context,
        }
    }

    // The text.

    /// The byte at the cursor; a CR past the text's end.
    pub(super) fn peek(&self) -> u8 {
        self.byte(0)
    }

    /// The byte `n` past the cursor; a CR past the text's end.
    pub(super) fn byte(&self, n: usize) -> u8 {
        self.text.get(self.at + n).copied().unwrap_or(b'\r')
    }

    /// The cursor moved past `n` bytes.
    pub(super) fn advance(&mut self, n: usize) {
        self.at = (self.at + n).min(self.text.len());
    }

    /// The cursor's offset from the parse's start.
    pub(super) const fn offset(&self) -> usize {
        self.at
    }

    /// The cursor moved to `offset` from the parse's start.
    pub(super) fn seek(&mut self, offset: usize) {
        self.at = offset.min(self.text.len());
    }

    /// The text from the cursor on.
    pub(super) fn rest(&self) -> &'a [u8] {
        self.text.get(self.at..).unwrap_or(&[])
    }

    /// The cursor's address.
    pub(super) fn address(&self) -> u32 {
        self.base.wrapping_add(super::super::len32(self.at))
    }

    /// The first byte at or after the cursor that is not a space, which the
    /// cursor is left at.
    pub(super) fn spaces(&mut self) -> u8 {
        while self.peek() == b' ' {
            self.advance(1);
        }
        self.peek()
    }

    /// Whether `b` is next after spaces, taking it if so.
    pub(super) fn take(&mut self, b: u8) -> bool {
        if self.spaces() == b {
            self.advance(1);
            return true;
        }
        false
    }

    /// `b` next after spaces, or `error`.
    pub(super) fn expect(&mut self, b: u8, error: Error) -> Parse<()> {
        if self.take(b) {
            Ok(())
        } else {
            Err(error.into())
        }
    }

    /// A comma next after spaces, or "Missing ,".
    pub(super) fn comma(&mut self) -> Parse<()> {
        self.expect(b',', Error::MissingComma)
    }

    /// Whether `word`, in either case, is at the cursor, taking it if so.
    pub(super) fn word(&mut self, word: &str) -> bool {
        let n = word.len();
        let matches = word
            .bytes()
            .enumerate()
            .all(|(i, w)| self.byte(i).to_ascii_uppercase() == w);
        if matches {
            self.advance(n);
        }
        matches
    }

    /// Whether `word` is at the cursor and no name character follows it,
    /// taking it if so.
    pub(super) fn whole_word(&mut self, word: &str) -> bool {
        let n = word.len();
        let matches = word
            .bytes()
            .enumerate()
            .all(|(i, w)| self.byte(i).to_ascii_uppercase() == w)
            && !in_name(self.byte(n));
        if matches {
            self.advance(n);
        }
        matches
    }

    /// The statement's end after what it parsed: spaces, then a comment if
    /// one is there, to a colon or the line's CR, whose address it is; any
    /// other byte is a syntax error.
    pub(super) fn end(&mut self) -> Parse<u32> {
        match self.spaces() {
            b':' | b'\r' => Ok(self.address()),
            b';' | b'\\' | token::REM => Ok(self.comment()),
            _ => Err(Error::Syntax.into()),
        }
    }

    /// A comment skipped to the colon or CR that ends it, whatever quotes it
    /// holds: that byte's address.
    pub(super) fn comment(&mut self) -> u32 {
        while !matches!(self.peek(), b':' | b'\r') {
            self.advance(1);
        }
        self.address()
    }

    // Values.

    /// The next expression's value: the tape's, or the machine asked for it.
    pub(super) fn value(&mut self, want: Want) -> Parse<Value> {
        self.spaces();
        let Some(&Evaluated { value, end }) = self.tape.get(self.used) else {
            return Err(Halt::Need {
                at: self.address(),
                want,
            });
        };
        self.used += 1;
        let offset = end.wrapping_sub(self.base) as usize;
        self.seek(offset);
        Ok(value)
    }

    /// The next expression's number: "Type mismatch: number needed" for a
    /// string.
    pub(super) fn number(&mut self, want: Want) -> Parse<Number> {
        self.value(want)?
            .number()
            .ok_or_else(|| Error::NeedNumber.into())
    }

    /// The next expression's integer, a real truncated.
    pub(super) fn int(&mut self, want: Want) -> Parse<i32> {
        let number = self.number(want)?;
        Ok(self.arith.int(number)?)
    }

    /// The next expression's string: "Type mismatch: string needed" for a
    /// number.
    pub(super) fn string(&mut self) -> Parse<Text> {
        match self.value(Want::Expression)? {
            Value::Str(text) => Ok(text),
            _ => Err(Error::NeedString.into()),
        }
    }

    /// `#` and an integer after it.
    pub(super) fn hash_int(&mut self) -> Parse<i32> {
        self.expect(b'#', Error::Syntax)?;
        self.int(Want::Expression)
    }

    // Registers.

    /// A core register: R0 to R15, PC, LR or SP in either case, or a factor
    /// that numbers one.
    pub(super) fn register(&mut self) -> Parse<Reg> {
        self.spaces();
        if let Some(reg) = self.register_name(true) {
            return Ok(reg);
        }
        let n = self.int(Want::Factor)?;
        reg_of(n)
    }

    /// A register's name at the cursor, taken; with `aliases`, PC, LR and SP
    /// as well as R0 to R15. A name character after it makes it a variable's
    /// name instead.
    pub(super) fn register_name(&mut self, aliases: bool) -> Option<Reg> {
        let (number, len) = match self.peek().to_ascii_uppercase() {
            b'R' => self.digits_to_fifteen(1)?,
            b'P' if aliases && self.byte(1).eq_ignore_ascii_case(&b'C') => (15, 2),
            b'L' if aliases && self.byte(1).eq_ignore_ascii_case(&b'R') => (14, 2),
            b'S' if aliases && self.byte(1).eq_ignore_ascii_case(&b'P') => (13, 2),
            _ => return None,
        };
        if in_name(self.byte(len)) {
            return None;
        }
        self.advance(len);
        Reg::new(number)
    }

    /// The number from 0 to 15 the digits `from` bytes on spell, a 1 taking
    /// a second digit to 5, and the bytes to its end; none where no digit is
    /// there.
    pub(super) fn digits_to_fifteen(&self, from: usize) -> Option<(u32, usize)> {
        let first = self.byte(from);
        if !first.is_ascii_digit() {
            return None;
        }
        let second = self.byte(from + 1);
        if first == b'1' && (b'0'..=b'5').contains(&second) {
            return Some((10 + u32::from(second - b'0'), from + 2));
        }
        Some((u32::from(first - b'0'), from + 1))
    }

    /// The register numbers in a list after `{`, to its `}`: each a register
    /// or a range of them, a range's ends in either order.
    pub(super) fn register_list(&mut self) -> Parse<u16> {
        self.expect(b'{', Error::MissingBrace)?;
        let mut mask = 0u16;
        loop {
            let first = self.register()?;
            let last = if self.take(b'-') {
                self.register()?
            } else {
                first
            };
            let (low, high) = if first.number() <= last.number() {
                (first.number(), last.number())
            } else {
                (last.number(), first.number())
            };
            for n in low..=high {
                mask |= 1 << n;
            }
            if !self.take(b',') {
                break;
            }
        }
        self.expect(b'}', Error::MissingCloseBrace)?;
        Ok(mask)
    }

    /// Whether `!` follows, taking it.
    pub(super) fn writeback(&mut self) -> bool {
        self.take(b'!')
    }
}

/// Register `n`, or "Bad register".
pub(super) fn reg_of(n: i32) -> Parse<Reg> {
    u32::try_from(n)
        .ok()
        .and_then(Reg::new)
        .ok_or_else(|| Error::BadRegister.into())
}
