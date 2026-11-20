//! L-values: where a value is kept and how, as `CALL`'s parameter list types
//! them, and a value read from or written to one.

use super::{Machine, Step};
use crate::messages::Error;
use crate::value::{Dialect, Text, Value, STRING_MOST};

/// How an l-value keeps its value.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Place {
    /// A byte: `?x`.
    Byte,
    /// A word: an integer variable, `!x`.
    Int,
    /// The dialect's real: a real variable, `|x`.
    Real,
    /// A string information block: a string variable.
    Sib,
    /// A string ending at a CR: `$x`.
    Dollar,
    /// A whole array, by its pointer word, of elements kept as the place
    /// says.
    Array(Element),
}

/// What an array's elements are.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Element {
    /// Integers.
    Int,
    /// Reals.
    Real,
    /// Strings.
    Str,
}

impl Element {
    /// How an element is kept.
    pub(crate) const fn place(self) -> Place {
        match self {
            Self::Int => Place::Int,
            Self::Real => Place::Real,
            Self::Str => Place::Sib,
        }
    }
}

impl Place {
    /// Its type in `CALL`'s parameter list, under `dialect`.
    pub(crate) const fn call_type(self, dialect: Dialect) -> u32 {
        match self {
            Self::Byte => 0,
            Self::Int => 4,
            Self::Real => dialect.real_bytes(),
            Self::Sib => 0x80,
            Self::Dollar => 0x81,
            Self::Array(element) => 0x100 + element.place().call_type(dialect),
        }
    }

    /// The place a `CALL` parameter type names, under `dialect`.
    pub(crate) const fn of_call_type(word: u32, dialect: Dialect) -> Option<Self> {
        let real = dialect.real_bytes();
        Some(match word {
            0 => Self::Byte,
            4 => Self::Int,
            0x80 => Self::Sib,
            0x81 => Self::Dollar,
            0x104 => Self::Array(Element::Int),
            0x180 => Self::Array(Element::Str),
            w if w == real => Self::Real,
            w if w == 0x100 + real => Self::Array(Element::Real),
            _ => return None,
        })
    }

    /// Whether a string goes in it.
    pub(crate) const fn is_string(self) -> bool {
        matches!(self, Self::Sib | Self::Dollar)
    }

    /// The bytes it takes in memory: a whole array's pointer for an array.
    pub(crate) const fn bytes(self, dialect: Dialect) -> u32 {
        match self {
            Self::Byte => 1,
            Self::Int | Self::Array(_) => 4,
            Self::Real => dialect.real_bytes(),
            Self::Sib => 5,
            Self::Dollar => 0,
        }
    }
}

/// Where a value is kept.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Lvalue {
    /// Its address.
    pub(crate) address: u32,
    /// How.
    pub(crate) place: Place,
}

impl Machine<'_, '_> {
    /// The value kept at `lvalue`.
    pub(crate) fn read(&mut self, lvalue: Lvalue) -> Step<Value> {
        let at = lvalue.address;
        Ok(match lvalue.place {
            Place::Byte => Value::Int(i32::from(self.os.read_byte(at)?)),
            Place::Int => Value::Int(self.os.read_word(at)?.cast_signed()),
            Place::Real => {
                let mut bytes = [0; 8];
                let n = self.dialect().real_bytes() as usize;
                self.os.read_bytes(at, &mut bytes[..n])?;
                Value::Real(self.arith.decode(&bytes))
            }
            Place::Sib => Value::Str(self.read_sib(at)?),
            Place::Dollar => Value::Str(self.read_dollar(at)?),
            Place::Array(_) => return Err(Error::ArrayHere.into()),
        })
    }

    /// `value` kept at `lvalue`, a number converted to its place's kind.
    pub(crate) fn write(&mut self, lvalue: Lvalue, value: &Value) -> Step {
        let at = lvalue.address;
        match (lvalue.place, value) {
            (Place::Sib, Value::Str(text)) => self.assign_sib(at, text),
            (Place::Dollar, Value::Str(text)) => {
                self.os.write_bytes(at, text.as_bytes())?;
                self.os
                    .write_byte(at.wrapping_add(u32::from(text.len())), b'\r')?;
                Ok(())
            }
            (Place::Sib | Place::Dollar, _) => Err(Error::NeedString.into()),
            (_, Value::Str(_)) => Err(Error::NeedNumber.into()),
            (Place::Byte, value) => {
                let n = self.int_value(value)?;
                Ok(self.os.write_byte(at, n.to_le_bytes()[0])?)
            }
            (Place::Int, value) => {
                let n = self.int_value(value)?;
                Ok(self.os.write_word(at, n.cast_unsigned())?)
            }
            (Place::Real, value) => {
                let real = self.real_value(value)?;
                let bytes = self.arith.encode(real);
                let n = self.dialect().real_bytes() as usize;
                Ok(self.os.write_bytes(at, &bytes[..n])?)
            }
            (Place::Array(_), _) => Err(Error::ArrayHere.into()),
        }
    }

    /// The string a block at `sib` describes: four bytes of its address, a
    /// byte of its length.
    pub(crate) fn read_sib(&mut self, sib: u32) -> Step<Text> {
        let address = self.os.read_word(sib)?;
        let length = usize::from(self.os.read_byte(sib.wrapping_add(4))?);
        let mut text = Text::EMPTY;
        let os = &mut *self.os;
        let mut read = Ok(());
        text.extend(length, |room| {
            if !room.is_empty() {
                read = os.read_bytes(address, room);
            }
        })
        .ok_or(Error::StackCorrupt)?;
        read?;
        Ok(text)
    }

    /// The string at `at`, to the CR that ends it within a string's length
    /// and a byte: empty where none does, as genuine BASIC reads one.
    pub(crate) fn read_dollar(&mut self, at: u32) -> Step<Text> {
        let mut text = Text::EMPTY;
        for i in 0..=STRING_MOST {
            let b = self.os.read_byte(at.wrapping_add(super::len32(i)))?;
            if b == b'\r' {
                return Ok(text);
            }
            if i < STRING_MOST {
                text.push_bytes(&[b]).ok_or(Error::StringTooLong)?;
            }
        }
        Ok(Text::EMPTY)
    }
}
