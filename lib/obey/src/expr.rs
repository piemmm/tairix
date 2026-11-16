//! `OS_EvaluateExpression`: integer and string expressions over system
//! variables, with BBC BASIC's operators at its priorities.
//!
//! References outside quotes are expanded first, then the text is read item by
//! item onto one stack of values and operators. An arriving operator applies
//! those on the stack whose left priority exceeds its right one; which error a
//! malformed expression gives follows from that stack's shape.

use alloc::vec::Vec;

use tairix_util::fallible;

use crate::gstrans::{translated, Flags, Read, Reader};
use crate::number::{decimal, read_unsigned, Reading};
use crate::vars::{Value, Variables};
use crate::{Budget, Error};

/// The longest expression, its references expanded.
const MOST_TEXT: usize = 1022;

/// The longest string an expression holds.
pub const MOST_STRING: usize = 1023;

/// The bytes the working stack overflows on reaching. Each entry counts 8,
/// and a string its bytes besides, rounded up to a word.
const STACK_BYTES: usize = 15_160;

/// What an expression evaluates to.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Answer {
    /// An integer: TRUE is -1, FALSE 0.
    Integer(i32),
    /// A string of at most [`MOST_STRING`] bytes.
    String(Vec<u8>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Op {
    Or,
    Eor,
    And,
    Eq,
    Ne,
    Lt,
    Gt,
    Le,
    Ge,
    LShift,
    RShift,
    LrShift,
    Plus,
    Minus,
    Times,
    Divide,
    Mod,
    UnaryPlus,
    UnaryMinus,
    Not,
    Len,
    Str,
    Val,
    Left,
    Right,
    Bra,
    Ket,
    /// The bottom of the stack, and the end of the expression.
    Bottom,
}

/// How an operator takes its operands.
enum Takes {
    /// One on each side, at a level of the priority table, loosest 1.
    Binary(u8),
    /// One after it, binding tighter than any binary operator.
    Prefix,
}

impl Op {
    /// What it takes, or `None` for a bracket or the end.
    const fn takes(self) -> Option<Takes> {
        Some(match self {
            Self::Or | Self::Eor => Takes::Binary(1),
            Self::And => Takes::Binary(2),
            Self::Eq
            | Self::Ne
            | Self::Lt
            | Self::Gt
            | Self::Le
            | Self::Ge
            | Self::LShift
            | Self::RShift
            | Self::LrShift => Takes::Binary(3),
            Self::Plus | Self::Minus => Takes::Binary(4),
            Self::Times | Self::Divide | Self::Mod => Takes::Binary(5),
            Self::UnaryPlus
            | Self::UnaryMinus
            | Self::Not
            | Self::Len
            | Self::Str
            | Self::Val
            | Self::Left
            | Self::Right => Takes::Prefix,
            Self::Bra | Self::Ket | Self::Bottom => return None,
        })
    }

    /// The priority an operator on the stack has: it is applied while this
    /// exceeds an arriving operator's [`Op::right`]. A binary operator's
    /// exceeds its own right one, so its level associates to the left; a
    /// prefix operator's falls short of its own, so prefixes nest to the
    /// right; `(` stays until a `)` or the end, which apply all above it.
    const fn left(self) -> u8 {
        match self.takes() {
            Some(Takes::Binary(level)) => level + 3,
            Some(Takes::Prefix) => 9,
            None => match self {
                Self::Bra => 2,
                _ => 1,
            },
        }
    }

    /// The priority an arriving operator has.
    const fn right(self) -> u8 {
        match self.takes() {
            Some(Takes::Binary(level)) => level + 2,
            Some(Takes::Prefix) => 10,
            None => match self {
                Self::Bra => 11,
                Self::Ket => 0,
                _ => 1,
            },
        }
    }
}

/// The operators' spellings: the first whose whole spelling the text begins
/// with is the one, letters matching exactly, so each spelling comes before
/// any other that begins it.
const OPERATORS: [(&[u8], Op); 25] = [
    (b"OR", Op::Or),
    (b"EOR", Op::Eor),
    (b"AND", Op::And),
    (b"=", Op::Eq),
    (b"<>", Op::Ne),
    (b"<=", Op::Le),
    (b"<<", Op::LShift),
    (b"<", Op::Lt),
    (b">=", Op::Ge),
    (b">>>", Op::LrShift),
    (b">>", Op::RShift),
    (b">", Op::Gt),
    (b"+", Op::Plus),
    (b"-", Op::Minus),
    (b"*", Op::Times),
    (b"/", Op::Divide),
    (b"MOD", Op::Mod),
    (b"NOT", Op::Not),
    (b"LEN", Op::Len),
    (b"STR", Op::Str),
    (b"VAL", Op::Val),
    (b"LEFT", Op::Left),
    (b"RIGHT", Op::Right),
    (b"(", Op::Bra),
    (b")", Op::Ket),
];

/// What the stack holds.
enum Entry {
    Op(Op),
    Value(Answer),
}

/// What the text holds next.
enum Item {
    Op(Op),
    Value(Answer),
}

/// Whether `byte` may be part of a variable's name in an expression.
const fn is_name_byte(byte: u8) -> bool {
    byte > b' '
        && !matches!(
            byte,
            b'"' | b'(' | b')' | b'*' | b'+' | b'-' | b'/' | b'<' | b'=' | b'>'
        )
}

/// `OS_EvaluateExpression`: the expression `text` starts with, to its first
/// CR, LF or NUL.
pub fn evaluate(
    text: &[u8],
    vars: &mut dyn Variables,
    budget: &mut Budget,
) -> Result<Answer, Error> {
    evaluate_in_stages(text, vars, budget).map_err(|failure| match failure {
        Failure::Expanding(error) | Failure::Evaluating(error) => error,
    })
}

/// Where an expression failed.
pub(crate) enum Failure {
    /// While its references outside quotes were expanded.
    Expanding(Error),
    /// While it was evaluated.
    Evaluating(Error),
}

/// [`evaluate`], saying where a failure came.
pub(crate) fn evaluate_in_stages(
    text: &[u8],
    vars: &mut dyn Variables,
    budget: &mut Budget,
) -> Result<Answer, Failure> {
    let text = expanded(text, vars, budget).map_err(Failure::Expanding)?;
    let mut evaluator = Evaluator {
        text: &text,
        at: 0,
        vars,
        budget,
        stack: Vec::new(),
        used: 0,
    };
    evaluator.run().map_err(Failure::Evaluating)
}

/// The expression with its references outside quotes expanded, ended by a
/// CR. Inside quotes it is copied as it is, for its strings to expand later.
fn expanded(text: &[u8], vars: &mut dyn Variables, budget: &mut Budget) -> Result<Vec<u8>, Error> {
    let mut reader = Reader::new(text, Flags::NO_QUOTES.union(Flags::NO_ESCAPES));
    let mut out = Vec::new();
    if !fallible::reserve(&mut out, MOST_TEXT + 1) {
        return Err(Error::SystemHeapFull);
    }
    let mut quoted = false;
    loop {
        let c = if quoted && !reader.expanding() {
            u32::from(reader.take_raw())
        } else {
            match reader.read(vars, budget)? {
                Read::Char(c) => c,
                Read::End(_) => break,
            }
        };
        // The whole character is tested, its low byte stored.
        if matches!(c, 0 | 10 | 13) {
            break;
        }
        if c == u32::from(b'"') {
            quoted = !quoted;
        }
        if out.len() == MOST_TEXT {
            return Err(Error::BufferOverflow);
        }
        out.push(c.to_le_bytes()[0]);
    }
    out.push(b'\r');
    Ok(out)
}

struct Evaluator<'a> {
    text: &'a [u8],
    at: usize,
    vars: &'a mut dyn Variables,
    budget: &'a mut Budget,
    stack: Vec<Entry>,
    /// The stack's bytes, as RISC OS counts them.
    used: usize,
}

/// The bytes an entry takes on the stack.
fn cost(entry: &Entry) -> usize {
    8 + match entry {
        Entry::Value(Answer::String(bytes)) => bytes.len().div_ceil(4) * 4,
        _ => 0,
    }
}

impl Evaluator<'_> {
    fn run(&mut self) -> Result<Answer, Error> {
        self.push(Entry::Op(Op::Bottom))?;
        let mut tos = Op::Bottom;
        let mut open = 0u32;
        loop {
            let op = match self.item()? {
                Item::Value(value) => {
                    self.push(Entry::Value(value))?;
                    continue;
                }
                Item::Op(op) => op,
            };
            if op == Op::Ket {
                open = open.checked_sub(1).ok_or(Error::MismatchedBrackets)?;
                while tos != Op::Bra {
                    tos = self.apply()?;
                }
                // What the brackets hold moves down over the `(`.
                let Some(Entry::Value(inside)) = self.pop() else {
                    return Err(Error::MissingOperand);
                };
                if !matches!(self.pop(), Some(Entry::Op(_))) {
                    return Err(Error::MissingOperator);
                }
                tos = self.top_op().ok_or(Error::MissingOperator)?;
                self.push(Entry::Value(inside))?;
                continue;
            }
            if op == Op::Bra {
                open += 1;
            }
            let op = match op {
                Op::Plus if matches!(self.stack.last(), Some(Entry::Op(_))) => Op::UnaryPlus,
                Op::Minus if matches!(self.stack.last(), Some(Entry::Op(_))) => Op::UnaryMinus,
                other => other,
            };
            while tos.left() > op.right() {
                tos = self.apply()?;
            }
            self.push(Entry::Op(op))?;
            tos = op;
            if op == Op::Bottom {
                self.pop();
                let Some(Entry::Value(result)) = self.pop() else {
                    return Err(Error::MissingOperand);
                };
                if !matches!(self.pop(), Some(Entry::Op(_))) {
                    return Err(Error::MissingOperand);
                }
                return Ok(result);
            }
        }
    }

    fn push(&mut self, entry: Entry) -> Result<(), Error> {
        let cost = cost(&entry);
        if self.used + cost >= STACK_BYTES {
            return Err(Error::ExpressionStackOverflow);
        }
        if !fallible::push(&mut self.stack, entry) {
            return Err(Error::SystemHeapFull);
        }
        self.used += cost;
        Ok(())
    }

    fn pop(&mut self) -> Option<Entry> {
        let entry = self.stack.pop()?;
        self.used -= cost(&entry);
        Some(entry)
    }

    /// The operator on top of the stack, where an operator is.
    fn top_op(&self) -> Option<Op> {
        match self.stack.last() {
            Some(Entry::Op(op)) => Some(*op),
            _ => None,
        }
    }

    /// The value under the operator being applied, its left operand.
    fn left_operand(&mut self) -> Result<Answer, Error> {
        match self.pop() {
            Some(Entry::Value(value)) => Ok(value),
            _ => Err(Error::MissingOperand),
        }
    }

    /// The operator under the value on top applied; the operator under its
    /// result, which is what the stack is next reduced by.
    fn apply(&mut self) -> Result<Op, Error> {
        let Some(Entry::Value(right)) = self.pop() else {
            return Err(Error::MissingOperand);
        };
        let Some(Entry::Op(op)) = self.pop() else {
            return Err(Error::MissingOperator);
        };
        let result = match op {
            Op::UnaryPlus | Op::Val => Answer::Integer(integer(right)?),
            Op::UnaryMinus => Answer::Integer(integer(right)?.wrapping_neg()),
            Op::Not => Answer::Integer(!integer(right)?),
            Op::Str => Answer::String(string(right)?),
            Op::Len => Answer::Integer(length(&string(right)?)),
            Op::Plus => self.plus(right)?,
            Op::Minus
            | Op::Times
            | Op::Divide
            | Op::Mod
            | Op::And
            | Op::Or
            | Op::Eor
            | Op::RShift
            | Op::LShift
            | Op::LrShift => {
                let b = integer(right)?;
                let a = integer(self.left_operand()?)?;
                Answer::Integer(arithmetic(op, a, b)?)
            }
            Op::Eq | Op::Ne | Op::Lt | Op::Gt | Op::Le | Op::Ge => {
                let order = compared(self.left_operand()?, right)?;
                let holds = match op {
                    Op::Eq => order.is_eq(),
                    Op::Ne => order.is_ne(),
                    Op::Lt => order.is_lt(),
                    Op::Gt => order.is_gt(),
                    Op::Le => order.is_le(),
                    _ => order.is_ge(),
                };
                Answer::Integer(if holds { -1 } else { 0 })
            }
            Op::Left | Op::Right => {
                let count = integer(right)?.cast_unsigned();
                let mut text = string(self.left_operand()?)?;
                let len = u32::try_from(text.len()).unwrap_or(u32::MAX);
                if op == Op::Left && count < len {
                    text.truncate(count as usize);
                } else if op == Op::Right && count <= len {
                    text.drain(..(len - count) as usize);
                }
                Answer::String(text)
            }
            // A `(` reduced is one never closed; the bottom and `)` are
            // never on the stack to be applied.
            Op::Bra => return Err(Error::MismatchedBrackets),
            Op::Ket | Op::Bottom => return Err(Error::MissingOperator),
        };
        let below = self.top_op().ok_or(Error::MissingOperator)?;
        self.push(Entry::Value(result))?;
        Ok(below)
    }

    /// `+`: two integers added, two strings joined, a string with an integer
    /// read as one.
    fn plus(&mut self, right: Answer) -> Result<Answer, Error> {
        Ok(match (self.left_operand()?, right) {
            (Answer::String(mut a), Answer::String(b)) => {
                if a.len() + b.len() > MOST_STRING {
                    return Err(Error::StringTooLong);
                }
                a.try_reserve(b.len()).map_err(|_| Error::SystemHeapFull)?;
                a.extend_from_slice(&b);
                Answer::String(a)
            }
            (a, b) => Answer::Integer(integer(a)?.wrapping_add(integer(b)?)),
        })
    }

    fn byte(&self) -> u8 {
        crate::byte(self.text, self.at)
    }

    /// The next item, or the end as the bottom operator.
    fn item(&mut self) -> Result<Item, Error> {
        while self.byte() == b' ' {
            self.at += 1;
        }
        let rest = self.text.get(self.at..).unwrap_or(&[]);
        let b = self.byte();
        if b == b'\r' {
            return Ok(Item::Op(Op::Bottom));
        }
        if b.is_ascii_digit() || b == b'&' {
            let (value, used) = read_unsigned(rest, Reading::plain(10))?;
            self.at += used;
            let value = u32::try_from(value).unwrap_or(u32::MAX);
            return Ok(Item::Value(Answer::Integer(value.cast_signed())));
        }
        if b == b'"' {
            let (bytes, used) = translated(
                rest,
                Flags::NO_ESCAPES.union(Flags::SPACE_ENDS),
                self.vars,
                self.budget,
                MOST_STRING,
            )?;
            self.at += used;
            return Ok(Item::Value(Answer::String(bytes)));
        }
        if let Some(&(word, op)) = OPERATORS.iter().find(|(word, _)| rest.starts_with(word)) {
            self.at += word.len();
            return Ok(Item::Op(op));
        }
        if !is_name_byte(b) {
            return Err(Error::UnknownOperand);
        }
        let len = rest.iter().take_while(|&&b| is_name_byte(b)).count();
        self.at += len;
        variable(&rest[..len], self.vars, self.budget).map(Item::Value)
    }
}

/// A variable's value as an operand: a number an integer, anything else a
/// string, a macro's expanded.
fn variable(name: &[u8], vars: &mut dyn Variables, budget: &mut Budget) -> Result<Answer, Error> {
    budget.charge(vars.cost(name))?;
    match vars.lookup(name) {
        Err(error) => Err(error),
        Ok(None) => Err(Error::UnknownOperand),
        Ok(Some(Value::Number(n))) => Ok(Answer::Integer(n)),
        Ok(Some(Value::String(bytes))) if bytes.len() > MOST_STRING => Err(Error::StringTooLong),
        Ok(Some(Value::String(bytes))) => Ok(Answer::String(bytes)),
        Ok(Some(Value::Macro(text))) => {
            translated(&text, Flags::NONE, vars, budget, MOST_STRING - 1)
                .map(|(bytes, _)| Answer::String(bytes))
                .map_err(|error| {
                    if error.contains() {
                        error
                    } else {
                        Error::StringTooLong
                    }
                })
        }
    }
}

/// An operand as an integer: a string must be one, signed, with spaces about
/// it; an empty one is 0.
fn integer(value: Answer) -> Result<i32, Error> {
    let text = match value {
        Answer::Integer(n) => return Ok(n),
        Answer::String(text) => text,
    };
    let mut at = text.iter().take_while(|&&b| b == b' ').count();
    let negative = text.get(at) == Some(&b'-');
    if negative || text.get(at) == Some(&b'+') {
        at += 1;
    }
    // A failed read leaves the place where it was, and reads as 0.
    let value = match read_unsigned(&text[at..], Reading::plain(10)) {
        Ok((value, used)) => {
            at += used;
            u32::try_from(value).unwrap_or(u32::MAX).cast_signed()
        }
        Err(_) => 0,
    };
    at += text[at..].iter().take_while(|&&b| b == b' ').count();
    if at != text.len() {
        return Err(Error::NotAnInteger);
    }
    Ok(if negative {
        value.wrapping_neg()
    } else {
        value
    })
}

/// An operand as a string: an integer in signed decimal.
fn string(value: Answer) -> Result<Vec<u8>, Error> {
    match value {
        Answer::String(text) => Ok(text),
        Answer::Integer(n) => decimal(n),
    }
}

fn length(text: &[u8]) -> i32 {
    i32::try_from(text.len()).unwrap_or(i32::MAX)
}

/// How two operands compare: two strings byte by byte, anything else as
/// integers.
fn compared(left: Answer, right: Answer) -> Result<core::cmp::Ordering, Error> {
    Ok(match (left, right) {
        (Answer::String(a), Answer::String(b)) => a.cmp(&b),
        (a, b) => integer(a)?.cmp(&integer(b)?),
    })
}

/// `a op b` for the operators of two integers.
fn arithmetic(op: Op, a: i32, b: i32) -> Result<i32, Error> {
    Ok(match op {
        Op::Minus => a.wrapping_sub(b),
        Op::Times => a.wrapping_mul(b),
        Op::And => a & b,
        Op::Or => a | b,
        Op::Eor => a ^ b,
        Op::Divide | Op::Mod => divided(a, b, op == Op::Mod)?,
        Op::RShift | Op::LrShift if b < 0 => shifted_left(a, b.wrapping_neg()),
        Op::RShift => shifted_right(a, b),
        Op::LShift if b < 0 => shifted_right(a, b.wrapping_neg()),
        Op::LShift => shifted_left(a, b),
        Op::LrShift if b >= 32 => 0,
        Op::LrShift => (a.cast_unsigned() >> b).cast_signed(),
        _ => return Err(Error::MissingOperator),
    })
}

/// The quotient or remainder of the magnitudes, the quotient negated where
/// exactly one side is negative, the remainder where the dividend is.
fn divided(a: i32, b: i32, remainder: bool) -> Result<i32, Error> {
    if b == 0 {
        return Err(Error::DivisionByZero);
    }
    let (magnitude, negative) = if remainder {
        (a.unsigned_abs() % b.unsigned_abs(), a < 0)
    } else {
        (a.unsigned_abs() / b.unsigned_abs(), (a < 0) != (b < 0))
    };
    let value = magnitude.cast_signed();
    Ok(if negative {
        value.wrapping_neg()
    } else {
        value
    })
}

/// The shift count's low byte, which is all a count below 32 is; the one
/// count that is still negative once negated, `i32::MIN`, shifts by it as 0.
fn count_of(n: i32) -> u32 {
    u32::from(n.to_le_bytes()[0])
}

/// `a` shifted left `n` places, a count past 31 clearing it.
fn shifted_left(a: i32, n: i32) -> i32 {
    if n >= 32 {
        0
    } else {
        a.wrapping_shl(count_of(n))
    }
}

/// `a` shifted right `n` places with its sign, a count past 31 leaving only
/// the sign.
fn shifted_right(a: i32, n: i32) -> i32 {
    if n >= 32 {
        a >> 31
    } else {
        a >> count_of(n)
    }
}

#[cfg(test)]
#[path = "expr_tests.rs"]
mod tests;
