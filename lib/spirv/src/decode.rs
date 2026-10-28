//! The decoder: a module's words checked against the grammar in two passes.
//!
//! The first frames every instruction and records each id it defines; the
//! definitions are then sorted, so a second definition is refused and any
//! later lookup is a binary search over what the input actually defined,
//! never a table sized by a value it chose. The second pass reads every
//! operand, resolving the literals whose width is their context's — a
//! constant's, by its result type, and a switch's, by its selector's — through
//! definitions that precede them.

use alloc::vec::Vec;

use crate::grammar::{self, Quantity, Shape, Spec};
use crate::module::{text, Instruction, Module, Operand, Version};
use crate::{DecodeError, Fault, Id, Kind, Op, MAX_MODULE_BYTES};

/// The SPIR-V magic number.
pub(crate) const MAGIC: u32 = 0x0723_0203;

/// Words in a module's header.
pub(crate) const HEADER_WORDS: usize = 5;

/// The one extended instruction set the grammar recognises.
const GLSL_STD_450: &str = "GLSL.std.450";

fn refused(word: usize, fault: Fault) -> DecodeError {
    DecodeError {
        word: u32::try_from(word).unwrap_or(u32::MAX),
        fault,
    }
}

/// Word `index` of `bytes`, which hold whole little-endian words; zero past
/// their end.
pub(crate) fn word(bytes: &[u8], index: usize) -> u32 {
    index
        .checked_mul(4)
        .and_then(|at| bytes.get(at..at.checked_add(4)?))
        .and_then(|w| <[u8; 4]>::try_from(w).ok())
        .map_or(0, u32::from_le_bytes)
}

/// `input`'s words in little-endian byte order.
fn normalised(input: &[u8], swapped: bool) -> Result<Vec<u8>, DecodeError> {
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(input.len())
        .map_err(|_| refused(0, Fault::OutOfMemory))?;
    if swapped {
        for &[a, b, c, d] in input.as_chunks::<4>().0 {
            bytes.extend_from_slice(&[d, c, b, a]);
        }
    } else {
        bytes.extend_from_slice(input);
    }
    Ok(bytes)
}

fn push<T>(list: &mut Vec<T>, item: T, at: usize) -> Result<(), DecodeError> {
    list.try_reserve(1)
        .map_err(|_| refused(at, Fault::OutOfMemory))?;
    list.push(item);
    Ok(())
}

/// Decode `input`, refusing one of more than `max_instructions`.
pub(crate) fn decode(input: &[u8], max_instructions: usize) -> Result<Module, DecodeError> {
    if input.len() > MAX_MODULE_BYTES {
        return Err(refused(0, Fault::TooLarge));
    }
    if !input.len().is_multiple_of(4) || input.len() < HEADER_WORDS * 4 {
        return Err(refused(0, Fault::NotWords));
    }
    let swapped = match word(input, 0) {
        MAGIC => false,
        magic if magic == MAGIC.swap_bytes() => true,
        _ => return Err(refused(0, Fault::Magic)),
    };
    let bytes = normalised(input, swapped)?;
    let version = Version::from_word(word(&bytes, 1))
        .filter(|v| v.major == 1 && *v <= Version::NEWEST)
        .ok_or_else(|| refused(1, Fault::Version))?;
    let bound = word(&bytes, 3);
    if word(&bytes, 4) != 0 {
        return Err(refused(4, Fault::Schema));
    }
    let generator = word(&bytes, 2);
    let (mut instructions, definitions) = frame(&bytes, bound, max_instructions)?;
    // Every operand takes a word, and every instruction's first word is none.
    let most = bytes.len() / 4 - HEADER_WORDS - instructions.len();
    let mut operands = Vec::new();
    operands
        .try_reserve_exact(most)
        .map_err(|_| refused(0, Fault::OutOfMemory))?;
    for index in 0..instructions.len() {
        let instruction = instructions[index];
        let spec = grammar::instruction(instruction.op)
            .ok_or_else(|| refused(instruction.at as usize, Fault::UnknownOpcode))?;
        let first = operands.len();
        let start = instruction.at as usize;
        let mut walk = Walk {
            spec,
            bytes: &bytes,
            bound,
            instructions: &instructions,
            definitions: &definitions,
            index,
            start,
            first,
            cursor: start + 1,
            end: start + usize::from(instruction.words),
            operands: &mut operands,
        };
        walk.instruction()?;
        let count = operands.len() - first;
        let entry = &mut instructions[index];
        entry.first =
            u32::try_from(first).map_err(|_| refused(start, Fault::TooManyInstructions))?;
        entry.count = u16::try_from(count).map_err(|_| refused(start, Fault::TrailingWords))?;
    }
    Ok(Module {
        bytes,
        version,
        generator,
        bound,
        instructions,
        operands,
        definitions,
    })
}

/// Each id a module defines and the index of the instruction defining it.
type Definitions = Vec<(Id, u32)>;

/// The first pass: every instruction framed and its opcode recognised, and
/// every id defined recorded, sorted, and refused if defined twice.
fn frame(
    bytes: &[u8],
    bound: u32,
    max_instructions: usize,
) -> Result<(Vec<Instruction>, Definitions), DecodeError> {
    let words = bytes.len() / 4;
    let mut instructions = Vec::new();
    let mut definitions = Vec::new();
    let mut at = HEADER_WORDS;
    while at < words {
        let [op_low, op_high, count_low, count_high] = word(bytes, at).to_le_bytes();
        let op = Op(u16::from_le_bytes([op_low, op_high]));
        let count = usize::from(u16::from_le_bytes([count_low, count_high]));
        if count == 0 || at + count > words {
            return Err(refused(at, Fault::WordCount));
        }
        if instructions.len() == max_instructions {
            return Err(refused(at, Fault::TooManyInstructions));
        }
        let spec = grammar::instruction(op).ok_or_else(|| refused(at, Fault::UnknownOpcode))?;
        let index = u32::try_from(instructions.len())
            .map_err(|_| refused(at, Fault::TooManyInstructions))?;
        if let Some(position) = spec.result_word() {
            let word_at = at + 1 + position;
            if word_at >= at + count {
                return Err(refused(at + count, Fault::MissingOperand));
            }
            let id = id_within(word(bytes, word_at), bound)
                .ok_or_else(|| refused(word_at, Fault::IdOutOfBound))?;
            push(&mut definitions, (id, index), word_at)?;
        }
        let word_count = u16::try_from(count).map_err(|_| refused(at, Fault::WordCount))?;
        let at_word = u32::try_from(at).map_err(|_| refused(at, Fault::TooLarge))?;
        push(
            &mut instructions,
            Instruction {
                op,
                words: word_count,
                at: at_word,
                first: 0,
                count: 0,
            },
            at,
        )?;
        at += count;
    }
    definitions.sort_unstable_by_key(|&(id, _)| id);
    if let Some(pair) = definitions.windows(2).find(|pair| pair[0].0 == pair[1].0) {
        let later = pair[0].1.max(pair[1].1) as usize;
        let at = instructions.get(later).map_or(0, |i| i.at as usize);
        return Err(refused(at, Fault::DuplicateId));
    }
    Ok((instructions, definitions))
}

fn id_within(value: u32, bound: u32) -> Option<Id> {
    Id::new(value).filter(|_| value < bound)
}

/// The second pass over one instruction.
struct Walk<'d> {
    spec: &'static Spec,
    bytes: &'d [u8],
    bound: u32,
    instructions: &'d [Instruction],
    definitions: &'d [(Id, u32)],
    /// The instruction walked.
    index: usize,
    /// Its first word.
    start: usize,
    /// Its first operand's index.
    first: usize,
    cursor: usize,
    end: usize,
    operands: &'d mut Vec<Operand>,
}

impl Walk<'_> {
    fn fault(&self, fault: Fault) -> DecodeError {
        refused(self.cursor, fault)
    }

    fn word(&self) -> u32 {
        word(self.bytes, self.cursor)
    }

    fn instruction(&mut self) -> Result<(), DecodeError> {
        self.list(self.spec.operands)?;
        if self.cursor != self.end {
            return Err(self.fault(Fault::TrailingWords));
        }
        match self.spec.op {
            Op::EXT_INST_IMPORT => self.import(),
            Op::EXT_INST | Op::EXT_INST_WITH_FORWARD_REFS_KHR => self.ext_inst(),
            _ => Ok(()),
        }
    }

    fn list(&mut self, specs: &[(Kind, Quantity)]) -> Result<(), DecodeError> {
        for &(kind, quantity) in specs {
            match quantity {
                Quantity::One => self.one(kind)?,
                Quantity::Optional => {
                    if self.cursor < self.end {
                        self.one(kind)?;
                    }
                }
                Quantity::Many => {
                    while self.cursor < self.end {
                        self.one(kind)?;
                    }
                }
            }
        }
        Ok(())
    }

    fn push(&mut self, kind: Kind, words: usize) -> Result<(), DecodeError> {
        if self.cursor + words > self.end {
            return Err(self.fault(Fault::MissingOperand));
        }
        let operand = Operand {
            kind,
            words: u16::try_from(words).map_err(|_| self.fault(Fault::WordCount))?,
            at: u32::try_from(self.cursor).map_err(|_| self.fault(Fault::TooLarge))?,
        };
        push(self.operands, operand, self.cursor)?;
        self.cursor += words;
        Ok(())
    }

    fn one(&mut self, kind: Kind) -> Result<(), DecodeError> {
        if self.cursor >= self.end {
            return Err(self.fault(Fault::MissingOperand));
        }
        match kind.shape() {
            Shape::Id => {
                if id_within(self.word(), self.bound).is_none() {
                    return Err(self.fault(Fault::IdOutOfBound));
                }
                self.push(kind, 1)
            }
            Shape::Word => self.push(kind, 1),
            Shape::Text => {
                let words = self.text_words()?;
                self.push(kind, words)
            }
            Shape::Number => {
                let ty = self.result_type()?;
                let words = self.numeric_words(ty, false)?;
                self.push(kind, words)
            }
            Shape::SwitchTarget => {
                let selector = self.first_operand_id()?;
                let ty = self.value_type(selector)?;
                let words = self.numeric_words(ty, true)?;
                self.push(Kind::LiteralContextDependentNumber, words)?;
                self.one(Kind::IdRef)
            }
            Shape::Pair(first, second) => {
                self.one(first)?;
                self.one(second)
            }
            Shape::SpecOp => self.spec_op(kind),
            Shape::Enum => {
                let enumerant = grammar::enumerant(kind, self.word())
                    .ok_or_else(|| self.fault(Fault::UnknownEnumerant))?;
                self.push(kind, 1)?;
                for &parameter in enumerant.parameters {
                    self.one(parameter)?;
                }
                Ok(())
            }
            Shape::Bits => {
                let bits = self.word();
                let mut parameters: [&[Kind]; 32] = [&[]; 32];
                for (bit, slot) in parameters.iter_mut().enumerate() {
                    let value = 1u32 << bit;
                    if bits & value != 0 {
                        *slot = grammar::enumerant(kind, value)
                            .ok_or_else(|| self.fault(Fault::UnknownEnumerant))?
                            .parameters;
                    }
                }
                self.push(kind, 1)?;
                for list in parameters {
                    for &parameter in list {
                        self.one(parameter)?;
                    }
                }
                Ok(())
            }
        }
    }

    /// The words a string at the cursor takes: its bytes, its terminator, and
    /// zero padding to the next word.
    fn text_words(&self) -> Result<usize, DecodeError> {
        let start = self.cursor * 4;
        let packed = self
            .bytes
            .get(start..self.end * 4)
            .ok_or_else(|| self.fault(Fault::BadString))?;
        let len = packed
            .iter()
            .position(|&b| b == 0)
            .ok_or_else(|| self.fault(Fault::BadString))?;
        let words = len / 4 + 1;
        let padding = packed.get(len..words * 4).unwrap_or(&[]);
        if padding.iter().any(|&b| b != 0) || core::str::from_utf8(&packed[..len]).is_err() {
            return Err(self.fault(Fault::BadString));
        }
        Ok(words)
    }

    /// `id`'s definition, which must come before the instruction walked.
    fn earlier(&self, id: u32) -> Result<&Instruction, DecodeError> {
        let id = Id::new(id).ok_or_else(|| self.fault(Fault::Unresolved))?;
        self.definitions
            .binary_search_by_key(&id, |&(defined, _)| defined)
            .ok()
            .and_then(|at| self.definitions.get(at))
            .filter(|&&(_, index)| (index as usize) < self.index)
            .and_then(|&(_, index)| self.instructions.get(index as usize))
            .ok_or_else(|| self.fault(Fault::Unresolved))
    }

    /// The result type of the instruction walked.
    fn result_type(&self) -> Result<u32, DecodeError> {
        if self.spec.has_result_type() {
            Ok(word(self.bytes, self.start + 1))
        } else {
            Err(self.fault(Fault::NumericWidth))
        }
    }

    /// The id the walked instruction's first operand names.
    fn first_operand_id(&self) -> Result<u32, DecodeError> {
        self.operands
            .get(self.first)
            .map(|operand| word(self.bytes, operand.at as usize))
            .ok_or_else(|| self.fault(Fault::Unresolved))
    }

    /// The type of the value `id` names, defined before the instruction
    /// walked.
    fn value_type(&self, id: u32) -> Result<u32, DecodeError> {
        let definition = self.earlier(id)?;
        let typed = grammar::instruction(definition.op).is_some_and(Spec::has_result_type);
        if typed {
            Ok(word(self.bytes, definition.at as usize + 1))
        } else {
            Err(self.fault(Fault::NumericWidth))
        }
    }

    /// The words a literal of type `ty` takes: one up to 32 bits, two up to
    /// 64. A switch's selector is an integer; a constant may be a float.
    fn numeric_words(&self, ty: u32, integer_only: bool) -> Result<usize, DecodeError> {
        let definition = self.earlier(ty)?;
        let numeric =
            definition.op == Op::TYPE_INT || (!integer_only && definition.op == Op::TYPE_FLOAT);
        if !numeric || definition.words < 3 {
            return Err(self.fault(Fault::NumericWidth));
        }
        match word(self.bytes, definition.at as usize + 2) {
            1..=32 => Ok(1),
            33..=64 => Ok(2),
            _ => Err(self.fault(Fault::NumericWidth)),
        }
    }

    /// An operation's opcode, then its operands but its result type and
    /// result. A nested operation is one whose operands are fixed in shape;
    /// any other is refused.
    fn spec_op(&mut self, kind: Kind) -> Result<(), DecodeError> {
        let nested = grammar::instruction(Op(u16::try_from(self.word()).unwrap_or(u16::MAX)))
            .ok_or_else(|| self.fault(Fault::UnknownOpcode))?;
        let operands = nested.operands.get(nested.results()..).unwrap_or(&[]);
        let fixed = operands.iter().all(|&(kind, _)| {
            !matches!(
                kind.shape(),
                Shape::SpecOp | Shape::Number | Shape::SwitchTarget
            )
        });
        if !fixed {
            return Err(self.fault(Fault::UnknownOpcode));
        }
        self.push(kind, 1)?;
        self.list(operands)
    }

    /// An import must name a recognised set.
    fn import(&self) -> Result<(), DecodeError> {
        let name = self
            .operands
            .get(self.first + 1)
            .filter(|operand| operand.kind == Kind::LiteralString)
            .ok_or_else(|| self.fault(Fault::MissingOperand))?;
        if text(self.bytes, *name) == GLSL_STD_450 {
            Ok(())
        } else {
            Err(refused(name.at as usize, Fault::UnknownExtInstSet))
        }
    }

    /// An extended instruction must name an import, an instruction of its
    /// set, and exactly the operands that instruction takes.
    fn ext_inst(&self) -> Result<(), DecodeError> {
        let operand = |position: usize| {
            self.operands
                .get(self.first + position)
                .ok_or_else(|| self.fault(Fault::MissingOperand))
        };
        let set = operand(2)?;
        let set_at = set.at as usize;
        if self.earlier(word(self.bytes, set_at))?.op != Op::EXT_INST_IMPORT {
            return Err(refused(set_at, Fault::UnknownExtInstSet));
        }
        let number = operand(3)?;
        let ext = grammar::glsl_std_450(word(self.bytes, number.at as usize))
            .ok_or_else(|| refused(number.at as usize, Fault::UnknownExtInst))?;
        let given = self.operands.len() - self.first - 4;
        if given == ext.operands {
            Ok(())
        } else {
            Err(refused(self.start, Fault::ExtInstArity))
        }
    }
}

#[cfg(test)]
#[path = "decode_tests.rs"]
mod tests;
