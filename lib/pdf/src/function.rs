//! Functions (ISO 32000-2 §7.10): sampled, exponential, stitching and
//! PostScript calculator, mapping inputs to outputs for shadings and tint
//! transforms.

use alloc::vec::Vec;
use core::cell::Cell;

use tairix_inline::ArrayVec;
use tairix_util::fallible::try_push;
use tairix_util::mathf;

use crate::document::Document;
use crate::lex::{Lexer, Token};
use crate::limits::MAX_FUNCTION_STEPS;
use crate::object::{real, Dict, Object};
use crate::PdfError;

/// The most components a colour has, and so the most values a function
/// answers: `DeviceN`'s limit.
pub(crate) const MAX_COMPONENTS: usize = 32;

/// Values in or out of a function.
pub(crate) type Values = ArrayVec<f64, MAX_COMPONENTS>;

/// How deeply stitching functions nest.
const MAX_FUNCTION_DEPTH: usize = 8;

/// The most functions one stitching function joins.
const MAX_STITCHED: usize = 256;

/// The most inputs a sampled function interpolates between: past it a sample
/// is taken nearest, as interpolation's cost doubles with every input.
const MAX_INTERPOLATED_INPUTS: usize = 8;

/// The most operators a calculator program holds, and the deepest its operand
/// stack goes: the specification's own limit.
const MAX_PROGRAM: usize = 65_536;
const MAX_STACK: usize = 100;

/// The steps a document's functions may still take: every evaluation is
/// charged its cost, so no file can make its colours unbounded work.
pub(crate) struct Work(Cell<u64>);

impl Work {
    pub(crate) const fn new() -> Self {
        Self(Cell::new(MAX_FUNCTION_STEPS))
    }

    /// Take `steps` from what is left.
    ///
    /// # Errors
    ///
    /// [`PdfError::TooComplex`] once they are spent.
    pub(crate) fn charge(&self, steps: usize) -> Result<(), PdfError> {
        let steps = u64::try_from(steps).map_err(|_| PdfError::TooComplex)?;
        let left = self
            .0
            .get()
            .checked_sub(steps)
            .ok_or(PdfError::TooComplex)?;
        self.0.set(left);
        Ok(())
    }
}

/// `items` as values, past the most a colour has dropped: every caller
/// bounds what it collects to that first.
pub(crate) fn values(items: impl IntoIterator<Item = f64>) -> Values {
    let mut values = Values::new();
    for item in items.into_iter().take(MAX_COMPONENTS) {
        let _ = values.try_push(item);
    }
    values
}

/// A function.
#[derive(Debug)]
pub(crate) struct Function {
    domain: Vec<(f64, f64)>,
    range: Option<Vec<(f64, f64)>>,
    kind: Kind,
}

#[derive(Debug)]
enum Kind {
    Sampled {
        size: Vec<u32>,
        encode: Vec<(f64, f64)>,
        decode: Vec<(f64, f64)>,
        /// Every sample as read, outputs innermost.
        samples: Vec<u32>,
        /// A sample's largest value.
        full: f64,
    },
    Exponential {
        c0: Vec<f64>,
        c1: Vec<f64>,
        exponent: f64,
    },
    Stitching {
        functions: Vec<Function>,
        bounds: Vec<f64>,
        encode: Vec<(f64, f64)>,
    },
    Calculator(Vec<Op>),
}

impl Function {
    /// The function `object` is: a dictionary or a stream.
    pub(crate) fn load(document: &Document<'_>, object: &Object) -> Result<Self, PdfError> {
        Self::load_at(document, object, 0)
    }

    fn load_at(document: &Document<'_>, object: &Object, depth: usize) -> Result<Self, PdfError> {
        // Stitching nests no deeper in any file written in earnest: past it
        // lies a function that stitches itself.
        if depth > MAX_FUNCTION_DEPTH {
            return Err(PdfError::Malformed);
        }
        let object = document.resolve(object)?;
        let dict = object.as_dict().ok_or(PdfError::Malformed)?;
        let domain =
            pairs(document, dict, b"Domain", MAX_COMPONENTS)?.ok_or(PdfError::Malformed)?;
        let range = pairs(document, dict, b"Range", MAX_COMPONENTS)?;
        let kind = match document.get(dict, b"FunctionType")?.as_int() {
            Some(0) => sampled(document, &object, dict, &domain, range.as_deref())?,
            Some(2) => exponential(document, dict, domain.len())?,
            Some(3) => stitching(document, dict, &domain, depth)?,
            Some(4) => {
                let stream = object.as_stream().ok_or(PdfError::Malformed)?;
                Kind::Calculator(compile(&document.decode(stream)?.bytes)?)
            }
            _ => return Err(PdfError::Unsupported),
        };
        if domain.is_empty() || domain.len() > MAX_COMPONENTS {
            return Err(PdfError::Malformed);
        }
        Ok(Self {
            domain,
            range,
            kind,
        })
    }

    /// How many inputs it takes.
    pub(crate) fn inputs(&self) -> usize {
        self.domain.len()
    }

    /// Its outputs for `inputs` into `outputs`, each clipped to its range,
    /// the steps taken charged to `work`. An input past the domain is clipped
    /// to it, and a calculator that fails answers nothing. Written in place:
    /// a colour's components are a long array to move on every evaluation.
    ///
    /// # Errors
    ///
    /// [`PdfError::TooComplex`] once `work` is spent.
    pub(crate) fn evaluate(
        &self,
        inputs: &[f64],
        work: &Work,
        outputs: &mut Values,
    ) -> Result<(), PdfError> {
        self.evaluate_side(inputs, false, work, outputs)
    }

    /// Its outputs approaching `inputs` from below: where a stitching
    /// function's pieces meet, the value the piece ending there reaches, so a
    /// sharp step is told from a smooth join.
    ///
    /// # Errors
    ///
    /// As [`Self::evaluate`].
    pub(crate) fn evaluate_left(
        &self,
        inputs: &[f64],
        work: &Work,
        outputs: &mut Values,
    ) -> Result<(), PdfError> {
        self.evaluate_side(inputs, true, work, outputs)
    }

    /// Where a stitching function's pieces meet, in its domain; none for any
    /// other function.
    pub(crate) fn bounds(&self) -> &[f64] {
        match &self.kind {
            Kind::Stitching { bounds, .. } => bounds,
            _ => &[],
        }
    }

    fn evaluate_side(
        &self,
        inputs: &[f64],
        left: bool,
        work: &Work,
        outputs: &mut Values,
    ) -> Result<(), PdfError> {
        let mut clipped = Values::new();
        for (index, &(low, high)) in self.domain.iter().enumerate() {
            let value = inputs.get(index).copied().unwrap_or(low);
            let _ = clipped.try_push(clamp(value, low, high));
        }
        match &self.kind {
            Kind::Sampled {
                size,
                encode,
                decode,
                samples,
                full,
            } => {
                let grid = Grid {
                    domain: &self.domain,
                    size,
                    encode,
                    decode,
                };
                work.charge(grid.corners() * decode.len())?;
                *outputs = sample(&clipped, &grid, samples, *full);
            }
            Kind::Exponential { c0, c1, exponent } => {
                work.charge(c0.len())?;
                let scale = mathf::power(clipped[0], *exponent);
                outputs.clear();
                for (&a, &b) in c0.iter().zip(c1) {
                    let _ = outputs.try_push(a + scale * (b - a));
                }
            }
            Kind::Stitching {
                functions,
                bounds,
                encode,
            } => {
                let (low, high) = self.domain[0];
                let x = clipped[0];
                let piece = bounds
                    .iter()
                    .position(|&bound| if left { x <= bound } else { x < bound })
                    .unwrap_or(bounds.len());
                let start = if piece == 0 { low } else { bounds[piece - 1] };
                let end = bounds.get(piece).copied().unwrap_or(high);
                work.charge(1)?;
                match (functions.get(piece), encode.get(piece)) {
                    (Some(function), Some(&(e0, e1))) => {
                        let inputs = [interpolate(x, start, end, e0, e1)];
                        function.evaluate_side(&inputs, left, work, outputs)?;
                    }
                    _ => outputs.clear(),
                }
            }
            Kind::Calculator(program) => {
                // Its jumps run only forwards, so no run is longer than it.
                work.charge(program.len().max(1))?;
                *outputs = run(program, &clipped).unwrap_or_default();
            }
        }
        if let Some(range) = &self.range {
            outputs.truncate(range.len());
            for (value, &(low, high)) in outputs.iter_mut().zip(range) {
                *value = clamp(*value, low, high);
            }
        }
        Ok(())
    }
}

/// `value` held within `low..=high`, a NaN taken as `low`.
fn clamp(value: f64, low: f64, high: f64) -> f64 {
    if value >= low {
        value.min(high)
    } else {
        low
    }
}

/// `x` mapped from `x0..x1` onto `y0..y1`.
pub(crate) fn interpolate(x: f64, x0: f64, x1: f64, y0: f64, y1: f64) -> f64 {
    let span = x1 - x0;
    if span.abs() < f64::MIN_POSITIVE {
        y0
    } else {
        y0 + (x - x0) * (y1 - y0) / span
    }
}

/// `key`'s array of numbers in `dict`, resolved.
pub(crate) fn numbers(
    document: &Document<'_>,
    dict: &Dict,
    key: &[u8],
) -> Result<Option<Vec<f64>>, PdfError> {
    let object = document.get(dict, key)?;
    let Some(items) = object.as_array() else {
        return Ok(None);
    };
    let mut values = Vec::new();
    values.try_reserve_exact(items.len())?;
    for item in items {
        let value = document
            .resolve(item)?
            .as_number()
            .ok_or(PdfError::Malformed)?;
        if !value.is_finite() {
            return Err(PdfError::Malformed);
        }
        values.push(value);
    }
    Ok(Some(values))
}

/// `key`'s array of numbers in `dict` read as pairs, at most `most` of them.
fn pairs(
    document: &Document<'_>,
    dict: &Dict,
    key: &[u8],
    most: usize,
) -> Result<Option<Vec<(f64, f64)>>, PdfError> {
    let Some(values) = numbers(document, dict, key)? else {
        return Ok(None);
    };
    if values.len() % 2 != 0 || values.len() / 2 > most {
        return Err(PdfError::Malformed);
    }
    Ok(Some(
        values
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&[low, high]| (low, high))
            .collect(),
    ))
}

fn sampled(
    document: &Document<'_>,
    object: &Object,
    dict: &Dict,
    domain: &[(f64, f64)],
    range: Option<&[(f64, f64)]>,
) -> Result<Kind, PdfError> {
    let stream = object.as_stream().ok_or(PdfError::Malformed)?;
    let range = range.ok_or(PdfError::Malformed)?;
    let sizes = document.get(dict, b"Size")?;
    let sizes = sizes.as_array().ok_or(PdfError::Malformed)?;
    let mut size = Vec::new();
    size.try_reserve_exact(sizes.len())?;
    for item in sizes {
        let item = document.resolve(item)?.as_int();
        size.push(
            item.and_then(|item| u32::try_from(item).ok())
                .filter(|&item| item >= 1)
                .ok_or(PdfError::Malformed)?,
        );
    }
    if size.len() != domain.len() {
        return Err(PdfError::Malformed);
    }
    let bits = document
        .get(dict, b"BitsPerSample")?
        .as_int()
        .ok_or(PdfError::Malformed)?;
    let Ok(bits @ (1 | 2 | 4 | 8 | 12 | 16 | 24 | 32)) = u32::try_from(bits) else {
        return Err(PdfError::Malformed);
    };
    let encode = pairs(document, dict, b"Encode", MAX_COMPONENTS)?.unwrap_or_else(|| {
        size.iter()
            .map(|&size| (0.0, f64::from(size - 1)))
            .collect()
    });
    let decode =
        pairs(document, dict, b"Decode", MAX_COMPONENTS)?.unwrap_or_else(|| range.to_vec());
    if encode.len() != domain.len() || decode.len() != range.len() {
        return Err(PdfError::Malformed);
    }
    let count = size
        .iter()
        .try_fold(range.len(), |count, &size| {
            count.checked_mul(usize::try_from(size).ok()?)
        })
        .ok_or(PdfError::TooComplex)?;
    let data = document.decode(stream)?.bytes;
    let needed = usize::try_from(bits)
        .ok()
        .and_then(|bits| count.checked_mul(bits))
        .ok_or(PdfError::TooComplex)?;
    if needed > data.len().saturating_mul(8) {
        return Err(PdfError::Malformed);
    }
    // Each sample is held as a word however few bits it was read from.
    let held = count
        .checked_mul(core::mem::size_of::<u32>())
        .ok_or(PdfError::TooComplex)?;
    crate::filter::charge(document.budget(), held)?;
    let mut samples = Vec::new();
    samples.try_reserve_exact(count)?;
    let mut reader = Bits::new(&data);
    samples.extend((0..count).map(|_| reader.take(bits)));
    Ok(Kind::Sampled {
        size,
        encode,
        decode,
        samples,
        full: f64::from(u32::MAX >> (32 - bits)),
    })
}

/// Bits read most significant first.
pub(crate) struct Bits<'a> {
    data: &'a [u8],
    /// The byte the cursor is in, and how many of its bits are taken.
    byte: usize,
    used: u32,
}

impl<'a> Bits<'a> {
    pub(crate) const fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            byte: 0,
            used: 0,
        }
    }

    /// The next `count` bits, at most 32; zeros past the end.
    pub(crate) fn take(&mut self, count: u32) -> u32 {
        let mut value = 0u32;
        let mut left = count.min(32);
        // As many bits a step as the byte under the cursor still holds.
        while left > 0 {
            let byte = u32::from(self.data.get(self.byte).copied().unwrap_or(0));
            let held = 8 - self.used;
            let taken = held.min(left);
            value = value << taken | byte >> (held - taken) & ((1 << taken) - 1);
            self.used += taken;
            if self.used == 8 {
                self.used = 0;
                self.byte += 1;
            }
            left -= taken;
        }
        value
    }
}

/// How a sampled function's inputs land among its samples, and how its
/// samples read as outputs.
struct Grid<'a> {
    domain: &'a [(f64, f64)],
    size: &'a [u32],
    encode: &'a [(f64, f64)],
    decode: &'a [(f64, f64)],
}

impl Grid<'_> {
    /// How many samples an output is interpolated between.
    fn corners(&self) -> usize {
        if self.domain.len() <= MAX_INTERPOLATED_INPUTS {
            1 << self.domain.len()
        } else {
            1
        }
    }
}

/// A sampled function's outputs at `inputs`, interpolated linearly between
/// the samples about them.
fn sample(inputs: &[f64], grid: &Grid<'_>, samples: &[u32], full: f64) -> Values {
    let Grid {
        domain,
        size,
        encode,
        decode,
    } = *grid;
    let outputs = decode.len();
    // Each input's sample below it and how far towards the next it lies.
    let mut cells: ArrayVec<(usize, f64), MAX_COMPONENTS> = ArrayVec::new();
    for (index, &input) in inputs.iter().enumerate() {
        let (low, high) = domain[index];
        let (e0, e1) = encode[index];
        let last = f64::from(size[index] - 1);
        let at = clamp(interpolate(input, low, high, e0, e1), 0.0, last);
        let below = mathf::floor(at).min(last - 1.0).max(0.0);
        let _ = cells.try_push((
            sample_index(below),
            if last == 0.0 { 0.0 } else { at - below },
        ));
    }
    let interpolated = inputs.len() <= MAX_INTERPOLATED_INPUTS;
    let corners = grid.corners();
    let mut values = Values::new();
    for (output, &(d0, d1)) in decode.iter().enumerate() {
        let mut total = 0.0;
        for corner in 0..corners {
            let mut weight = 1.0;
            let mut offset = 0usize;
            let mut stride = outputs;
            for (index, &(below, fraction)) in cells.iter().enumerate() {
                let (step, share) = if !interpolated {
                    (usize::from(fraction >= 0.5), 1.0)
                } else if corner >> index & 1 == 1 {
                    (1, fraction)
                } else {
                    (0, 1.0 - fraction)
                };
                let extent = size[index] as usize;
                offset += (below + step).min(extent - 1) * stride;
                stride *= extent;
                weight *= share;
            }
            if weight > 0.0 {
                total +=
                    weight * f64::from(samples.get(offset + output).copied().unwrap_or(0)) / full;
            }
        }
        let _ = values.try_push(d0 + total * (d1 - d0));
    }
    values
}

/// `below`, a sample position already floored and held within a size, as an
/// index.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "floored and held within 0..size, and a size is a u32, before the cast"
)]
fn sample_index(below: f64) -> usize {
    below as usize
}

fn exponential(document: &Document<'_>, dict: &Dict, inputs: usize) -> Result<Kind, PdfError> {
    if inputs != 1 {
        return Err(PdfError::Malformed);
    }
    let read = |key: &[u8], default: f64| -> Result<Vec<f64>, PdfError> {
        match numbers(document, dict, key)? {
            Some(read) if read.len() <= MAX_COMPONENTS => Ok(read),
            Some(_) => Err(PdfError::Malformed),
            None => {
                let mut one = Vec::new();
                try_push(&mut one, default)?;
                Ok(one)
            }
        }
    };
    let (c0, c1) = (read(b"C0", 0.0)?, read(b"C1", 1.0)?);
    let exponent = document
        .get(dict, b"N")?
        .as_number()
        .ok_or(PdfError::Malformed)?;
    if c0.len() != c1.len() || !exponent.is_finite() {
        return Err(PdfError::Malformed);
    }
    Ok(Kind::Exponential { c0, c1, exponent })
}

fn stitching(
    document: &Document<'_>,
    dict: &Dict,
    domain: &[(f64, f64)],
    depth: usize,
) -> Result<Kind, PdfError> {
    if domain.len() != 1 {
        return Err(PdfError::Malformed);
    }
    let listed = document.get(dict, b"Functions")?;
    let listed = listed.as_array().ok_or(PdfError::Malformed)?;
    if listed.is_empty() {
        return Err(PdfError::Malformed);
    }
    if listed.len() > MAX_STITCHED {
        return Err(PdfError::TooComplex);
    }
    let mut functions = Vec::new();
    functions.try_reserve_exact(listed.len())?;
    for function in listed {
        let function = Function::load_at(document, function, depth + 1)?;
        if function.inputs() != 1 {
            return Err(PdfError::Malformed);
        }
        functions.push(function);
    }
    let bounds = numbers(document, dict, b"Bounds")?.ok_or(PdfError::Malformed)?;
    let encode = pairs(document, dict, b"Encode", MAX_STITCHED)?.ok_or(PdfError::Malformed)?;
    if bounds.len() + 1 != functions.len() || encode.len() != functions.len() {
        return Err(PdfError::Malformed);
    }
    if bounds.windows(2).any(|pair| pair[0] > pair[1]) {
        return Err(PdfError::Malformed);
    }
    Ok(Kind::Stitching {
        functions,
        bounds,
        encode,
    })
}

/// A calculator operator; `If` and `IfElse` hold the extent of what they
/// guard, so a program runs straight through with forward jumps only.
#[derive(Copy, Clone, Debug, PartialEq)]
enum Op {
    Number(f64),
    Bool(bool),
    Abs,
    Add,
    Atan,
    Ceiling,
    Cos,
    Cvi,
    Cvr,
    Div,
    Exp,
    Floor,
    Idiv,
    Ln,
    Log,
    Mod,
    Mul,
    Neg,
    Round,
    Sin,
    Sqrt,
    Sub,
    Truncate,
    And,
    Bitshift,
    Eq,
    Ge,
    Gt,
    Le,
    Lt,
    Ne,
    Not,
    Or,
    Xor,
    Copy,
    Dup,
    Exch,
    Index,
    Pop,
    Roll,
    /// Pop a condition and skip the next `skip` operators unless it holds.
    JumpUnless(usize),
    /// Skip the next `skip` operators.
    Jump(usize),
}

/// The program a calculator function's stream spells: one procedure.
fn compile(source: &[u8]) -> Result<Vec<Op>, PdfError> {
    let mut lexer = Lexer::new(source, 0);
    if lexer.next()? != Some(Token::BraceOpen) {
        return Err(PdfError::Malformed);
    }
    let mut program = Vec::new();
    procedure(&mut lexer, &mut program, 0)?;
    Ok(program)
}

/// The operators of a procedure, after its `{` through its `}`, onto
/// `program`.
fn procedure(lexer: &mut Lexer<'_>, program: &mut Vec<Op>, depth: usize) -> Result<(), PdfError> {
    if depth > MAX_FUNCTION_DEPTH * 4 {
        return Err(PdfError::Malformed);
    }
    loop {
        if program.len() > MAX_PROGRAM {
            return Err(PdfError::TooComplex);
        }
        let op = match lexer.next()?.ok_or(PdfError::Malformed)? {
            Token::BraceClose => return Ok(()),
            Token::Integer(value) => Op::Number(real(value)),
            Token::Real(value) => Op::Number(value),
            Token::BraceOpen => {
                conditional(lexer, program, depth)?;
                continue;
            }
            Token::Keyword(word) => operator(word).ok_or(PdfError::Malformed)?,
            _ => return Err(PdfError::Malformed),
        };
        try_push(program, op)?;
    }
}

/// A `{…} if` or `{…} {…} ifelse` whose first `{` has been read.
fn conditional(lexer: &mut Lexer<'_>, program: &mut Vec<Op>, depth: usize) -> Result<(), PdfError> {
    let test = program.len();
    try_push(program, Op::JumpUnless(0))?;
    procedure(lexer, program, depth + 1)?;
    match lexer.next()?.ok_or(PdfError::Malformed)? {
        Token::Keyword(b"if") => {
            program[test] = Op::JumpUnless(program.len() - test - 1);
            Ok(())
        }
        Token::BraceOpen => {
            let jump = program.len();
            try_push(program, Op::Jump(0))?;
            program[test] = Op::JumpUnless(program.len() - test - 1);
            procedure(lexer, program, depth + 1)?;
            if lexer.next()? != Some(Token::Keyword(b"ifelse")) {
                return Err(PdfError::Malformed);
            }
            program[jump] = Op::Jump(program.len() - jump - 1);
            Ok(())
        }
        _ => Err(PdfError::Malformed),
    }
}

fn operator(word: &[u8]) -> Option<Op> {
    Some(match word {
        b"true" => Op::Bool(true),
        b"false" => Op::Bool(false),
        b"abs" => Op::Abs,
        b"add" => Op::Add,
        b"atan" => Op::Atan,
        b"ceiling" => Op::Ceiling,
        b"cos" => Op::Cos,
        b"cvi" => Op::Cvi,
        b"cvr" => Op::Cvr,
        b"div" => Op::Div,
        b"exp" => Op::Exp,
        b"floor" => Op::Floor,
        b"idiv" => Op::Idiv,
        b"ln" => Op::Ln,
        b"log" => Op::Log,
        b"mod" => Op::Mod,
        b"mul" => Op::Mul,
        b"neg" => Op::Neg,
        b"round" => Op::Round,
        b"sin" => Op::Sin,
        b"sqrt" => Op::Sqrt,
        b"sub" => Op::Sub,
        b"truncate" => Op::Truncate,
        b"and" => Op::And,
        b"bitshift" => Op::Bitshift,
        b"eq" => Op::Eq,
        b"ge" => Op::Ge,
        b"gt" => Op::Gt,
        b"le" => Op::Le,
        b"lt" => Op::Lt,
        b"ne" => Op::Ne,
        b"not" => Op::Not,
        b"or" => Op::Or,
        b"xor" => Op::Xor,
        b"copy" => Op::Copy,
        b"dup" => Op::Dup,
        b"exch" => Op::Exch,
        b"index" => Op::Index,
        b"pop" => Op::Pop,
        b"roll" => Op::Roll,
        _ => return None,
    })
}

/// A calculator stack value: a boolean is held as its own kind so the
/// logical operators tell it from an integer.
#[derive(Copy, Clone, Debug, PartialEq)]
enum Value {
    Number(f64),
    Bool(bool),
}

impl Value {
    fn number(self) -> Option<f64> {
        match self {
            Self::Number(value) => Some(value),
            Self::Bool(_) => None,
        }
    }
}

/// `program` run over `inputs`: the stack it leaves, or `None` where an
/// operator met the wrong operands or the stack overflowed.
fn run(program: &[Op], inputs: &[f64]) -> Option<Values> {
    let mut stack = Stack::new();
    for &input in inputs {
        stack.try_push(Value::Number(input)).ok()?;
    }
    let mut at = 0;
    while let Some(&op) = program.get(at) {
        at += 1;
        match op {
            Op::Jump(skip) => at += skip,
            Op::JumpUnless(skip) => {
                if stack.pop()? != Value::Bool(true) {
                    at += skip;
                }
            }
            op => step(op, &mut stack)?,
        }
    }
    let mut values = Values::new();
    for value in stack.iter().take(MAX_COMPONENTS) {
        let _ = values.try_push(value.number()?);
    }
    Some(values)
}

/// The calculator's operand stack.
type Stack = ArrayVec<Value, MAX_STACK>;

fn step(op: Op, stack: &mut Stack) -> Option<()> {
    let result = match op {
        Op::Number(value) => Value::Number(value),
        Op::Bool(value) => Value::Bool(value),
        Op::Abs
        | Op::Ceiling
        | Op::Cos
        | Op::Cvi
        | Op::Cvr
        | Op::Floor
        | Op::Ln
        | Op::Log
        | Op::Neg
        | Op::Round
        | Op::Sin
        | Op::Sqrt
        | Op::Truncate => Value::Number(unary(op, stack.pop()?.number()?)),
        Op::Add | Op::Atan | Op::Div | Op::Exp | Op::Idiv | Op::Mod | Op::Mul | Op::Sub => {
            let b = stack.pop()?.number()?;
            let a = stack.pop()?.number()?;
            Value::Number(binary(op, a, b)?)
        }
        Op::Eq | Op::Ne => {
            let b = stack.pop()?;
            let a = stack.pop()?;
            Value::Bool((a == b) == (op == Op::Eq))
        }
        Op::Ge | Op::Gt | Op::Le | Op::Lt => {
            let b = stack.pop()?.number()?;
            let a = stack.pop()?.number()?;
            Value::Bool(match op {
                Op::Ge => a >= b,
                Op::Gt => a > b,
                Op::Le => a <= b,
                _ => a < b,
            })
        }
        Op::And | Op::Or | Op::Xor | Op::Not | Op::Bitshift => logic(op, stack)?,
        Op::Copy | Op::Dup | Op::Exch | Op::Index | Op::Pop | Op::Roll => {
            return arrange(op, stack)
        }
        Op::Jump(_) | Op::JumpUnless(_) => return None,
    };
    stack.try_push(result).ok()
}

/// A one-operand arithmetic operator's result on `x`; angles in degrees.
fn unary(op: Op, x: f64) -> f64 {
    match op {
        Op::Abs => x.abs(),
        Op::Ceiling => mathf::ceil(x),
        Op::Cos => mathf::cos(x.to_radians()),
        Op::Cvi | Op::Truncate => mathf::trunc(x),
        Op::Floor => mathf::floor(x),
        Op::Ln => mathf::ln(x),
        Op::Log => mathf::ln(x) / core::f64::consts::LN_10,
        Op::Neg => -x,
        Op::Round => mathf::floor(x + 0.5),
        Op::Sin => mathf::sin(x.to_radians()),
        Op::Sqrt => mathf::sqrt(x),
        _ => x,
    }
}

/// A two-operand arithmetic operator's result on `a` and `b`; `None` for an
/// integer division by zero.
fn binary(op: Op, a: f64, b: f64) -> Option<f64> {
    Some(match op {
        Op::Add => a + b,
        Op::Atan => {
            let angle = mathf::atan2(a, b).to_degrees();
            if angle < 0.0 {
                angle + 360.0
            } else {
                angle
            }
        }
        Op::Div => a / b,
        Op::Exp => mathf::power(a, b),
        Op::Idiv => real(integer(a)?.checked_div(integer(b)?)?),
        Op::Mod => real(integer(a)?.checked_rem(integer(b)?)?),
        Op::Mul => a * b,
        _ => a - b,
    })
}

/// A logical or bitwise operator's result, booleans and integers each as
/// their own kind.
fn logic(op: Op, stack: &mut Stack) -> Option<Value> {
    if op == Op::Not {
        return Some(match stack.pop()? {
            Value::Bool(value) => Value::Bool(!value),
            Value::Number(value) => Value::Number(real(!integer(value)?)),
        });
    }
    if op == Op::Bitshift {
        let shift = integer(stack.pop()?.number()?)?;
        let value = integer(stack.pop()?.number()?)?;
        let shifted = if shift >= 0 {
            value.checked_shl(u32::try_from(shift).ok()?).unwrap_or(0)
        } else {
            value.checked_shr(u32::try_from(-shift).ok()?).unwrap_or(0)
        };
        return Some(Value::Number(real(shifted)));
    }
    let b = stack.pop()?;
    let a = stack.pop()?;
    Some(match (a, b) {
        (Value::Bool(a), Value::Bool(b)) => Value::Bool(match op {
            Op::And => a & b,
            Op::Or => a | b,
            _ => a ^ b,
        }),
        (Value::Number(a), Value::Number(b)) => {
            let (a, b) = (integer(a)?, integer(b)?);
            Value::Number(real(match op {
                Op::And => a & b,
                Op::Or => a | b,
                _ => a ^ b,
            }))
        }
        _ => return None,
    })
}

/// A stack operator carried out in place.
fn arrange(op: Op, stack: &mut Stack) -> Option<()> {
    match op {
        Op::Dup => {
            let top = *stack.last()?;
            stack.try_push(top).ok()?;
        }
        Op::Exch => {
            let len = stack.len();
            if len < 2 {
                return None;
            }
            stack.swap(len - 2, len - 1);
        }
        Op::Pop => {
            stack.pop()?;
        }
        Op::Copy => {
            let count = usize::try_from(integer(stack.pop()?.number()?)?).ok()?;
            let start = stack.len().checked_sub(count)?;
            for at in start..start + count {
                let value = stack[at];
                stack.try_push(value).ok()?;
            }
        }
        Op::Index => {
            let depth = usize::try_from(integer(stack.pop()?.number()?)?).ok()?;
            let at = stack.len().checked_sub(depth + 1)?;
            let value = stack[at];
            stack.try_push(value).ok()?;
        }
        _ => {
            let shift = integer(stack.pop()?.number()?)?;
            let count = usize::try_from(integer(stack.pop()?.number()?)?).ok()?;
            let start = stack.len().checked_sub(count)?;
            if count > 0 {
                let turn = usize::try_from(shift.rem_euclid(i64::try_from(count).ok()?)).ok()?;
                stack[start..].rotate_right(turn);
            }
        }
    }
    Some(())
}

/// `value` as an integer, where it is one in range.
#[allow(
    clippy::cast_possible_truncation,
    reason = "truncated, and held below 2^53 in magnitude, before the cast"
)]
fn integer(value: f64) -> Option<i64> {
    (value.is_finite() && value.abs() < 9.0e15).then(|| mathf::trunc(value) as i64)
}

#[cfg(test)]
#[path = "function_tests.rs"]
mod tests;
