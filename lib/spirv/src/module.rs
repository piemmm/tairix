//! The checked model of a module, as every consumer reads it.

use alloc::vec::Vec;
use core::fmt;
use core::num::NonZeroU32;

use crate::decode::{self, word};
use crate::grammar::{self, Shape};
use crate::{tables, DecodeError, Kind, MAX_INSTRUCTIONS};

/// An instruction's opcode.
#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct Op(pub(crate) u16);

impl Op {
    /// Its number.
    #[must_use]
    pub const fn code(self) -> u16 {
        self.0
    }

    /// Its name, if the grammar recognises it.
    #[must_use]
    pub fn name(self) -> Option<&'static str> {
        grammar::instruction(self).map(|spec| spec.name)
    }
}

impl fmt::Debug for Op {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.name() {
            Some(name) => f.write_str(name),
            None => write!(f, "Op({})", self.0),
        }
    }
}

/// An id: nonzero, and below the bound of the module it belongs to.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct Id(NonZeroU32);

impl Id {
    /// The id numbered `value`, unless it is zero.
    #[must_use]
    pub const fn new(value: u32) -> Option<Self> {
        match NonZeroU32::new(value) {
            Some(value) => Some(Self(value)),
            None => None,
        }
    }

    /// Its number.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0.get()
    }
}

/// A SPIR-V version.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct Version {
    /// The major version: 1.
    pub major: u8,
    /// The minor version.
    pub minor: u8,
}

impl Version {
    /// The newest version the grammar describes, and so the newest a module
    /// may declare.
    pub const NEWEST: Self = Self {
        major: tables::VERSION.0,
        minor: tables::VERSION.1,
    };

    /// The header word that declares it.
    pub(crate) const fn word(self) -> u32 {
        u32::from_le_bytes([0, self.minor, self.major, 0])
    }

    /// The version a header word declares, if its reserved bytes are clear.
    pub(crate) const fn from_word(word: u32) -> Option<Self> {
        match word.to_le_bytes() {
            [0, minor, major, 0] => Some(Self { major, minor }),
            _ => None,
        }
    }
}

/// One operand: its kind in the grammar, and the words it occupies.
///
/// A composite of the grammar is two operands here, each of its own kind; a
/// switch's literal is [`Kind::LiteralContextDependentNumber`], as wide as its
/// selector.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Operand {
    pub(crate) kind: Kind,
    pub(crate) words: u16,
    pub(crate) at: u32,
}

impl Operand {
    /// Its kind in the grammar.
    #[must_use]
    pub const fn kind(self) -> Kind {
        self.kind
    }
}

/// What an operand holds.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Value<'m> {
    /// An id.
    Id(Id),
    /// One word: a literal, an enumerant's value, an extended instruction's
    /// number, or an opcode.
    Word(u32),
    /// A literal as wide as its context's type: one word, or two with the low
    /// word first.
    Number(u64),
    /// A string.
    Text(&'m str),
}

/// One instruction: its opcode, its words, and its operands.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Instruction {
    pub(crate) op: Op,
    pub(crate) words: u16,
    pub(crate) at: u32,
    pub(crate) first: u32,
    pub(crate) count: u16,
}

impl Instruction {
    /// Its opcode.
    #[must_use]
    pub const fn op(self) -> Op {
        self.op
    }
}

/// A module whose every instruction the grammar recognises and every operand
/// it checked: each id below the bound, each enumerant known, each string
/// terminated UTF-8, each literal as wide as its type, and no id defined
/// twice.
///
/// It is checked against the grammar only; whether the module means anything
/// is the validator's question.
pub struct Module {
    pub(crate) bytes: Vec<u8>,
    pub(crate) version: Version,
    pub(crate) generator: u32,
    pub(crate) bound: u32,
    pub(crate) instructions: Vec<Instruction>,
    pub(crate) operands: Vec<Operand>,
    /// Each defined id and its instruction, by ascending id.
    pub(crate) definitions: Vec<(Id, u32)>,
}

impl Module {
    /// Decode a module in either byte order.
    ///
    /// # Errors
    ///
    /// The word the module was refused at and why. A module past
    /// [`MAX_MODULE_BYTES`](crate::MAX_MODULE_BYTES) is refused before
    /// anything is allocated for it.
    pub fn decode(bytes: &[u8]) -> Result<Self, DecodeError> {
        decode::decode(bytes, MAX_INSTRUCTIONS)
    }

    /// The version it declares.
    #[must_use]
    pub const fn version(&self) -> Version {
        self.version
    }

    /// The generator word it carries.
    #[must_use]
    pub const fn generator(&self) -> u32 {
        self.generator
    }

    /// The bound every id is below.
    #[must_use]
    pub const fn bound(&self) -> u32 {
        self.bound
    }

    /// The module, in little-endian byte order whatever order it arrived in.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Its instructions, in order.
    #[must_use]
    pub fn instructions(&self) -> &[Instruction] {
        &self.instructions
    }

    /// The operands of `instruction`, one of this module's.
    #[must_use]
    pub fn operands(&self, instruction: &Instruction) -> &[Operand] {
        let first = instruction.first as usize;
        self.operands
            .get(first..first + usize::from(instruction.count))
            .unwrap_or(&[])
    }

    /// What `operand`, one of this module's, holds.
    #[must_use]
    pub fn value(&self, operand: &Operand) -> Value<'_> {
        let at = operand.at as usize;
        let first = word(&self.bytes, at);
        match operand.kind.shape() {
            Shape::Id => Id::new(first).map_or(Value::Word(first), Value::Id),
            Shape::Number if operand.words == 2 => {
                Value::Number(u64::from(first) | u64::from(word(&self.bytes, at + 1)) << 32)
            }
            Shape::Number => Value::Number(u64::from(first)),
            Shape::Text => Value::Text(text(&self.bytes, *operand)),
            _ => Value::Word(first),
        }
    }

    /// The id `instruction` defines, if it defines one.
    #[must_use]
    pub fn result(&self, instruction: &Instruction) -> Option<Id> {
        let position = grammar::instruction(instruction.op)?.result_word()?;
        self.id_at(instruction, position)
    }

    /// The type of the value `instruction` defines, if it has one.
    #[must_use]
    pub fn result_type(&self, instruction: &Instruction) -> Option<Id> {
        grammar::instruction(instruction.op)?
            .has_result_type()
            .then(|| self.id_at(instruction, 0))
            .flatten()
    }

    fn id_at(&self, instruction: &Instruction, position: usize) -> Option<Id> {
        match self.value(self.operands(instruction).get(position)?) {
            Value::Id(id) => Some(id),
            _ => None,
        }
    }

    /// The instruction that defines `id`.
    #[must_use]
    pub fn definition(&self, id: Id) -> Option<&Instruction> {
        let at = self
            .definitions
            .binary_search_by_key(&id, |&(defined, _)| defined)
            .ok()?;
        let (_, index) = self.definitions.get(at)?;
        self.instructions.get(*index as usize)
    }

    /// The words of `instruction`, one of this module's.
    pub(crate) fn instruction_bytes(&self, instruction: &Instruction) -> &[u8] {
        let start = instruction.at as usize * 4;
        let end = start + usize::from(instruction.words) * 4;
        self.bytes.get(start..end).unwrap_or(&[])
    }
}

/// The string `operand` holds, in `bytes`, which the decoder has proved
/// terminated UTF-8.
pub(crate) fn text(bytes: &[u8], operand: Operand) -> &str {
    let start = operand.at as usize * 4;
    let end = start + usize::from(operand.words) * 4;
    let packed = bytes.get(start..end).unwrap_or(&[]);
    let len = packed.iter().position(|&b| b == 0).unwrap_or(packed.len());
    core::str::from_utf8(packed.get(..len).unwrap_or(&[])).unwrap_or("")
}

impl fmt::Debug for Module {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Module")
            .field("version", &self.version)
            .field("bound", &self.bound)
            .field("instructions", &self.instructions.len())
            .finish_non_exhaustive()
    }
}
