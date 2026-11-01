//! The test assembler: SPIR-V's assembly text to a module, through the
//! encoder, so a test reads as the shader it is. Test scaffolding; no TAIRiX
//! build enables it.
//!
//! One instruction a line, `;` comments. `%name` is an id, assigned on first
//! sight so a forward reference is written as one; `"text"` is a string.
//! Every other operand is read as the kind the grammar gives its position:
//!
//! - an enumerant by its grammar name, or its value, followed by the
//!   parameters it takes; a set of bits joins names with `|`;
//! - a literal number as a decimal or `0x` integer, or with a point or an
//!   exponent as a float, as wide as its type: a constant's result type, or a
//!   switch's selector;
//! - an extended instruction by its `GLSL.std.450` name, and the operation of
//!   an `OpSpecConstantOp` by its instruction's name without `Op`.
//!
//! `%result = OpName …` places the result after the result type where the
//! instruction has one. The text is the test's own, so a line the assembler
//! cannot read panics naming it.

use alloc::collections::{BTreeMap, VecDeque};
use alloc::string::String;
use alloc::vec::Vec;

use tairix_util::mathf::float_bits;

use crate::grammar::{self, Quantity, Shape};
use crate::tables::{ENUMERANT_NAMES, GLSL_STD_450, GLSL_STD_450_NAMES, INSTRUCTIONS};
use crate::validate::allow::Class;
use crate::{Admitted, Arg, Encoder, Id, Kind, Module, Op, Refusal, Version};

/// Assemble `text` as a module of `version`.
///
/// # Panics
///
/// On a line it cannot read, naming it.
#[must_use]
pub fn assemble(text: &str, version: Version) -> Vec<u8> {
    let mut assembler = Assembler {
        encoder: Encoder::new(version, 0).expect("a header"),
        ids: BTreeMap::new(),
        widths: BTreeMap::new(),
        types: BTreeMap::new(),
    };
    for line in text.lines() {
        let line = line.split(';').next().unwrap_or("").trim();
        if !line.is_empty() {
            assembler.line(line);
        }
    }
    assembler.encoder.finish()
}

/// Every instruction the validator admits in a function's body: what a
/// consumer's tests of the instructions it runs are held to cover.
pub fn admitted_body_ops() -> impl Iterator<Item = Op> {
    INSTRUCTIONS
        .iter()
        .map(|spec| spec.op)
        .filter(|&op| matches!(crate::validate::allow::class(op), Some(Class::Body(_))))
}

/// Every `GLSL.std.450` instruction the validator has a rule for, by
/// number.
pub fn admitted_glsl() -> impl Iterator<Item = u32> {
    GLSL_STD_450
        .iter()
        .map(|inst| inst.number)
        .filter(|&number| crate::validate::glsl::rule_of(number).is_some())
}

/// Assemble `text` as a module of the newest version and validate it.
///
/// # Errors
///
/// The validator's refusal.
///
/// # Panics
///
/// On a line it cannot read, or a module the decoder refuses.
pub fn admit(text: &str) -> Result<Admitted, Refusal> {
    admit_version(text, Version::NEWEST)
}

/// Assemble `text` as a module of `version` and validate it.
///
/// # Errors
///
/// The validator's refusal.
///
/// # Panics
///
/// On a line it cannot read, or a module the decoder refuses.
pub fn admit_version(text: &str, version: Version) -> Result<Admitted, Refusal> {
    let module = Module::decode(&assemble(text, version)).expect("a module the decoder takes");
    Admitted::new(module)
}

struct Assembler {
    encoder: Encoder,
    ids: BTreeMap<String, Id>,
    /// The width of each numeric type declared.
    widths: BTreeMap<Id, u32>,
    /// The type of each value whose instruction names one.
    types: BTreeMap<Id, Id>,
}

/// One line being read: its tokens still to read, the words read so far, and
/// the width a literal number takes.
struct Reading<'l> {
    line: &'l str,
    tokens: VecDeque<&'l str>,
    args: Vec<Arg<'l>>,
    width: u32,
}

impl Assembler {
    fn id(&mut self, name: &str) -> Id {
        if let Some(&id) = self.ids.get(name) {
            return id;
        }
        let id = self.encoder.next_id().expect("an id");
        self.ids.insert(name.into(), id);
        id
    }

    fn line(&mut self, line: &str) {
        let tokens = tokenize(line);
        let (result, tokens) = match tokens.as_slice() {
            [result, "=", rest @ ..] if result.starts_with('%') => (Some(*result), rest),
            all => (None, all),
        };
        let (name, operands) = tokens
            .split_first()
            .unwrap_or_else(|| panic!("no opcode in {line:?}"));
        let spec = INSTRUCTIONS
            .iter()
            .find(|spec| spec.name == *name)
            .unwrap_or_else(|| panic!("unknown instruction {name} in {line:?}"));
        let result = result.map(|r| self.id(r.trim_start_matches('%')));
        let mut reading = Reading {
            line,
            tokens: operands.iter().copied().collect(),
            args: Vec::new(),
            width: 32,
        };
        if spec.op == Op::SWITCH {
            if let Some(selector) = operands.first() {
                let selector = self.reference(selector, line);
                reading.width = self.width_of_value(selector);
            }
        }
        for &(kind, quantity) in spec.operands {
            match (kind, quantity) {
                (Kind::IdResult, _) => {
                    let id = result.unwrap_or_else(|| panic!("{line:?} names no result"));
                    reading.args.push(Arg::Id(id));
                }
                (Kind::IdResultType, _) => {
                    let token = reading
                        .tokens
                        .pop_front()
                        .unwrap_or_else(|| panic!("{line:?} names no result type"));
                    let ty = self.reference(token, line);
                    reading.width = self.widths.get(&ty).copied().unwrap_or(32);
                    reading.args.push(Arg::Id(ty));
                    if let Some(result) = result {
                        self.types.insert(result, ty);
                    }
                }
                (_, quantity) => self.quantified(&[(kind, quantity)], &mut reading),
            }
        }
        if let Some(extra) = reading.tokens.front() {
            panic!("{line:?} has an operand {extra} past its last");
        }
        if let (Some(result), Some(width)) = (result, operands.first()) {
            if spec.op == Op::TYPE_INT || spec.op == Op::TYPE_FLOAT {
                let width = number(width).unwrap_or_else(|| panic!("{line:?} names no width"));
                self.widths.insert(result, width);
            }
        }
        self.encoder
            .emit(spec.op, &reading.args)
            .unwrap_or_else(|e| panic!("{line:?}: {e:?}"));
    }

    fn reference(&mut self, token: &str, line: &str) -> Id {
        let name = token
            .strip_prefix('%')
            .unwrap_or_else(|| panic!("{token} in {line:?} is not an id"));
        self.id(name)
    }

    /// The width of the numeric type of the value `id`, or 32.
    fn width_of_value(&self, id: Id) -> u32 {
        self.types
            .get(&id)
            .and_then(|ty| self.widths.get(ty))
            .copied()
            .unwrap_or(32)
    }

    /// Read one operand of `kind`, and every operand its value brings with
    /// it.
    fn operand(&mut self, kind: Kind, reading: &mut Reading<'_>) {
        let line = reading.line;
        let token = reading
            .tokens
            .pop_front()
            .unwrap_or_else(|| panic!("{line:?} lacks a {kind:?} operand"));
        match kind.shape() {
            Shape::Id => {
                let id = self.reference(token, line);
                reading.args.push(Arg::Id(id));
            }
            Shape::Text => {
                let text = token
                    .strip_prefix('"')
                    .and_then(|t| t.strip_suffix('"'))
                    .unwrap_or_else(|| panic!("{token} in {line:?} is not a string"));
                reading.args.push(Arg::Text(text));
            }
            Shape::Number => literal(token, reading),
            Shape::Word if kind == Kind::LiteralExtInstInteger => {
                let number = number(token).unwrap_or_else(|| glsl(token, line));
                reading.args.push(Arg::Word(number));
            }
            Shape::Word => {
                let word = number(token).unwrap_or_else(|| panic!("{token} in {line:?}"));
                reading.args.push(Arg::Word(word));
            }
            Shape::SpecOp => {
                let spec = INSTRUCTIONS
                    .iter()
                    .find(|spec| spec.name.strip_prefix("Op") == Some(token))
                    .unwrap_or_else(|| panic!("unknown operation {token} in {line:?}"));
                reading.args.push(Arg::Word(u32::from(spec.op.code())));
                self.quantified(spec.operands.get(spec.results()..).unwrap_or(&[]), reading);
            }
            Shape::SwitchTarget => {
                literal(token, reading);
                self.operand(Kind::IdRef, reading);
            }
            Shape::Pair(first, second) => {
                reading.tokens.push_front(token);
                self.operand(first, reading);
                self.operand(second, reading);
            }
            Shape::Enum => {
                let value = enumerant(kind, token, line);
                reading.args.push(Arg::Word(value));
                self.parameters(kind, value, reading);
            }
            Shape::Bits => {
                let mask = token
                    .split('|')
                    .fold(0, |mask, name| mask | enumerant(kind, name, line));
                reading.args.push(Arg::Word(mask));
                for bit in (0..32).map(|b| 1u32 << b).filter(|b| mask & b != 0) {
                    self.parameters(kind, bit, reading);
                }
            }
        }
    }

    /// Read operands of `kinds`, each as often as its quantity says.
    fn quantified(&mut self, kinds: &[(Kind, Quantity)], reading: &mut Reading<'_>) {
        for &(kind, quantity) in kinds {
            match quantity {
                Quantity::One => self.operand(kind, reading),
                Quantity::Optional => {
                    if !reading.tokens.is_empty() {
                        self.operand(kind, reading);
                    }
                }
                Quantity::Many => {
                    while !reading.tokens.is_empty() {
                        self.operand(kind, reading);
                    }
                }
            }
        }
    }

    fn parameters(&mut self, kind: Kind, value: u32, reading: &mut Reading<'_>) {
        if let Some(enumerant) = grammar::enumerant(kind, value) {
            for &parameter in enumerant.parameters {
                self.operand(parameter, reading);
            }
        }
    }
}

/// The value of `kind`'s enumerant `token`: its grammar name, or a number.
fn enumerant(kind: Kind, token: &str, line: &str) -> u32 {
    if let Some(value) = number(token) {
        return value;
    }
    let from = ENUMERANT_NAMES.partition_point(|&(name, _, _)| name < token);
    ENUMERANT_NAMES
        .get(from..)
        .unwrap_or(&[])
        .iter()
        .take_while(|&&(name, _, _)| name == token)
        .find(|&&(_, of, _)| of == kind)
        .map_or_else(
            || panic!("{token} in {line:?} is no {kind:?}"),
            |&(_, _, value)| value,
        )
}

/// The number of the `GLSL.std.450` instruction `name`.
fn glsl(name: &str, line: &str) -> u32 {
    glsl_number(name).unwrap_or_else(|| panic!("{name} in {line:?} is no GLSL.std.450 instruction"))
}

/// The number of the `GLSL.std.450` instruction `name`, if it is one.
#[must_use]
pub fn glsl_number(name: &str) -> Option<u32> {
    GLSL_STD_450_NAMES
        .binary_search_by_key(&name, |&(n, _)| n)
        .ok()
        .and_then(|at| GLSL_STD_450_NAMES.get(at))
        .map(|&(_, number)| number)
}

/// A literal as wide as `reading` says: an integer, or with a point or an
/// exponent a float; one word, or two with the low first.
fn literal(token: &str, reading: &mut Reading<'_>) {
    let line = reading.line;
    let float = token.contains(['.', 'e', 'E']) && !token.starts_with("0x");
    let bits = if float {
        let value: f64 = token
            .parse()
            .unwrap_or_else(|_| panic!("{token} in {line:?} is not a float"));
        float_bits(reading.width, value)
    } else {
        wide(token).unwrap_or_else(|| panic!("{token} in {line:?} is not a number"))
    };
    let bytes = bits.to_le_bytes();
    let (low, high) = bytes.split_at(4);
    let word = |bytes: &[u8]| Arg::Word(u32::from_le_bytes(bytes.try_into().unwrap_or_default()));
    reading.args.push(word(low));
    if reading.width == 64 {
        reading.args.push(word(high));
    }
}

fn number(token: &str) -> Option<u32> {
    match token.strip_prefix("0x") {
        Some(hex) => u32::from_str_radix(&hex.replace('_', ""), 16).ok(),
        None => token
            .parse::<u32>()
            .ok()
            .or_else(|| token.parse::<i32>().ok().map(i32::cast_unsigned)),
    }
}

fn wide(token: &str) -> Option<u64> {
    match token.strip_prefix("0x") {
        Some(hex) => u64::from_str_radix(&hex.replace('_', ""), 16).ok(),
        None => token
            .parse::<u64>()
            .ok()
            .or_else(|| token.parse::<i64>().ok().map(i64::cast_unsigned)),
    }
}

/// The line's tokens, a quoted string kept whole.
fn tokenize(line: &str) -> Vec<&str> {
    let mut tokens = Vec::new();
    let mut rest = line.trim_start();
    while !rest.is_empty() {
        let end = if rest.starts_with('"') {
            rest.get(1..)
                .and_then(|r| r.find('"'))
                .map_or(rest.len(), |e| e + 2)
        } else {
            rest.find(char::is_whitespace).unwrap_or(rest.len())
        };
        tokens.push(rest.get(..end).unwrap_or(rest));
        rest = rest.get(end..).unwrap_or("").trim_start();
    }
    tokens
}

#[cfg(test)]
#[path = "assemble_tests.rs"]
mod tests;
