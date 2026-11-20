//! The expression evaluator: operator precedence over the BASIC stack.
//!
//! An expression begins with an [`Kind::Expression`] record above the
//! continuation that waits on it. Reading an operand, unary operators and
//! functions of a factor are pushed as [`Kind::Factor`] records and applied
//! as soon as their factor is read, since they bind tighter than any binary
//! operator; a binary operator waits as a [`Kind::Binary`] record until one
//! of no tighter precedence arrives. Brackets and a function's bracketed
//! arguments are records too, so a function called part way through an
//! expression leaves all of it on the stack, to carry on where it left off.
//!
//! Operators of the same precedence associate to the left, and a comparison
//! or shift is the only one of its group an unbracketed expression holds, as
//! in BASIC V.

use super::functions::{Bracketed, Unary};
use super::next::{Cont, Delivered, Next, Payload};
use super::{Machine, Mode, Step};
use crate::messages::Error;
use crate::stack::Kind;
use crate::token;
use crate::value::{Text, Value};

/// A binary operator.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub(crate) enum Op {
    Pow = 1,
    Mul,
    Div,
    IntDiv,
    Mod,
    Add,
    Sub,
    Eq,
    Ne,
    Lt,
    Gt,
    Le,
    Ge,
    Shl,
    Shr,
    Lsr,
    And,
    Or,
    Eor,
}

impl Op {
    const ALL: [Self; 19] = [
        Self::Pow,
        Self::Mul,
        Self::Div,
        Self::IntDiv,
        Self::Mod,
        Self::Add,
        Self::Sub,
        Self::Eq,
        Self::Ne,
        Self::Lt,
        Self::Gt,
        Self::Le,
        Self::Ge,
        Self::Shl,
        Self::Shr,
        Self::Lsr,
        Self::And,
        Self::Or,
        Self::Eor,
    ];

    fn of(byte: u8) -> Option<Self> {
        Self::ALL.into_iter().find(|&op| op as u8 == byte)
    }

    /// Its priority: 2 binds tightest, 7 loosest.
    const fn priority(self) -> u8 {
        match self {
            Self::Pow => 2,
            Self::Mul | Self::Div | Self::IntDiv | Self::Mod => 3,
            Self::Add | Self::Sub => 4,
            Self::Eq
            | Self::Ne
            | Self::Lt
            | Self::Gt
            | Self::Le
            | Self::Ge
            | Self::Shl
            | Self::Shr
            | Self::Lsr => 5,
            Self::And => 6,
            Self::Or | Self::Eor => 7,
        }
    }
}

/// The comparison and shift group's priority.
const RELATION: u8 = 5;

/// An expression marker's flag: it ends after its first factor.
pub(crate) const FACTOR_ONLY: u8 = 1;

impl Machine<'_, '_> {
    /// An expression evaluated at the pointer, then `cont` resumed with its
    /// value and `payload`.
    pub(crate) fn evaluate(&mut self, cont: Cont, payload: Payload) -> Step<Next> {
        self.push(Kind::Continuation, cont, payload.words())?;
        self.push(Kind::Expression, 0, &[])?;
        self.mode = Mode::Operand;
        Ok(Next::Moved)
    }

    /// A factor evaluated at the pointer, then `cont` resumed with it.
    pub(crate) fn evaluate_factor(&mut self, cont: Cont, payload: Payload) -> Step<Next> {
        self.push(Kind::Continuation, cont, payload.words())?;
        self.push(Kind::Expression, FACTOR_ONLY, &[])?;
        self.mode = Mode::Operand;
        Ok(Next::Moved)
    }

    /// A value the operand being read is, pushed: the factor operations
    /// waiting on it applied, and the operator after it read next.
    pub(crate) fn operand_is(&mut self, value: &Value) -> Step<Next> {
        self.push_value(value)?;
        self.factor_done()
    }

    /// The factor on top complete: the factor operations under it applied,
    /// innermost first.
    pub(crate) fn factor_done(&mut self) -> Step<Next> {
        loop {
            let top = self.top_header()?.ok_or(Error::StackCorrupt)?;
            if !top.kind.is_value() {
                return Err(Error::StackCorrupt.into());
            }
            let below = self.sp + top.length;
            match self.header_at(below)? {
                Some(header) if header.kind == Kind::Factor => {
                    let value = self.pop_value()?;
                    let mut words = [0; 1];
                    let factor = self.pop_record(Kind::Factor, &mut words)?;
                    let op = Unary::of(factor.aux).ok_or(Error::StackCorrupt)?;
                    if let Some(next) = self.apply_unary(op, &value, words[0])? {
                        return Ok(next);
                    }
                }
                _ => break,
            }
        }
        self.mode = Mode::Operator;
        Ok(Next::Moved)
    }

    /// The operand step: reads what begins a factor.
    pub(crate) fn operand(&mut self) -> Step<Next> {
        let b = self.skip_spaces()?;
        match b {
            b'0'..=b'9' | b'.' => {
                let value = self.number_literal()?;
                self.operand_is(&value)
            }
            b'"' => {
                self.ptr += 1;
                let text = self.string_literal()?;
                self.operand_is(&Value::Str(text))
            }
            b'&' => {
                self.ptr += 1;
                let n = self.hex_literal()?;
                self.operand_is(&Value::Int(n))
            }
            b'%' => {
                self.ptr += 1;
                let n = self.binary_literal()?;
                self.operand_is(&Value::Int(n))
            }
            b'(' => {
                self.ptr += 1;
                self.push(Kind::Bracket, 0, &[])?;
                Ok(Next::Moved)
            }
            b'-' => self.unary(Unary::Neg),
            b'+' => self.unary(Unary::Plus),
            b'?' => self.unary(Unary::Byte),
            b'!' => self.unary(Unary::Word),
            b'$' => self.unary(Unary::Dollar),
            b'|' => self.unary(Unary::Real),
            token::NOT => self.unary(Unary::Not),
            b if super::vars::starts_name(b) || b == b'@' => self.variable_operand(),
            _ if b >= 0x7F => self.keyword_operand(),
            _ => Err(Error::NoSuchVariable.into()),
        }
    }

    /// A unary operator taken: the factor after it read next.
    fn unary(&mut self, op: Unary) -> Step<Next> {
        self.ptr += 1;
        self.push(Kind::Factor, op as u8, &[0])?;
        Ok(Next::Moved)
    }

    /// A variable's value as an operand, an element's subscripts read first
    /// and a binary indirection's offset after it.
    fn variable_operand(&mut self) -> Step<Next> {
        let name = self.read_name()?.ok_or(Error::Mistake)?;
        if name.is_array() {
            let lvalue = self.variable(&name, false)?.ok_or(Error::UnknownArray)?;
            let pointer = self.os.read_word(lvalue.address)?;
            let super::Place::Array(place) = lvalue.place else {
                return Err(Error::ArrayHere.into());
            };
            self.push(
                Kind::Arguments,
                Bracketed::Subscript as u8,
                &[0, pointer, place as u32, 0],
            )?;
            return Ok(Next::Moved);
        }
        let Some(lvalue) = self.variable(&name, false)? else {
            if self.unknown_is_p()? {
                let p = self.p_percent()?;
                return self.after_variable(&Value::Int(p.cast_signed()));
            }
            return Err(Error::NoSuchVariable.into());
        };
        let value = self.read(lvalue)?;
        self.after_variable(&value)
    }

    /// A variable's or element's value read: a binary `?` or `!` after it
    /// makes it an indirection's base.
    pub(crate) fn after_variable(&mut self, value: &Value) -> Step<Next> {
        let b = self.peek()?;
        let op = match b {
            b'?' => Unary::BaseByte,
            b'!' => Unary::BaseWord,
            _ => return self.operand_is(value),
        };
        let base = self.int_value(value)?;
        self.ptr += 1;
        self.push(Kind::Factor, op as u8, &[base.cast_unsigned()])?;
        Ok(Next::Moved)
    }

    /// The operator step: a binary operator taken, or the expression's end
    /// met.
    pub(crate) fn operator(&mut self) -> Step<Next> {
        if self.factor_only()? {
            return self.expression_end();
        }
        let at = self.ptr;
        let Some(op) = self.read_operator()? else {
            return self.expression_end();
        };
        if op.priority() == RELATION && self.relation_pending()? {
            self.ptr = at;
            return self.expression_end();
        }
        self.reduce(op.priority())?;
        self.push(Kind::Binary, op as u8, &[])?;
        self.mode = Mode::Operand;
        Ok(Next::Moved)
    }

    /// Whether the innermost marker ends its expression after a factor.
    fn factor_only(&mut self) -> Step<bool> {
        let marker = self.marker()?;
        let header = self.header_at(marker)?.ok_or(Error::StackCorrupt)?;
        Ok(header.kind == Kind::Expression && header.aux & FACTOR_ONLY != 0)
    }

    /// The binary operator at the pointer, taken; none, the pointer where it
    /// was, where none is.
    fn read_operator(&mut self) -> Step<Option<Op>> {
        let b = self.skip_spaces()?;
        let (op, length) = match b {
            b'^' => (Op::Pow, 1),
            b'*' => (Op::Mul, 1),
            b'/' => (Op::Div, 1),
            b'+' => (Op::Add, 1),
            b'-' => (Op::Sub, 1),
            b'=' => (Op::Eq, 1),
            token::DIV => (Op::IntDiv, 1),
            token::MOD => (Op::Mod, 1),
            token::AND => (Op::And, 1),
            token::OR => (Op::Or, 1),
            token::EOR => (Op::Eor, 1),
            b'<' => match self.byte_at(self.ptr + 1)? {
                b'>' => (Op::Ne, 2),
                b'=' => (Op::Le, 2),
                b'<' => (Op::Shl, 2),
                _ => (Op::Lt, 1),
            },
            b'>' => match self.byte_at(self.ptr + 1)? {
                b'=' => (Op::Ge, 2),
                b'>' if self.byte_at(self.ptr + 2)? == b'>' => (Op::Lsr, 3),
                b'>' => (Op::Shr, 2),
                _ => (Op::Gt, 1),
            },
            _ => return Ok(None),
        };
        self.ptr += length;
        Ok(Some(op))
    }

    /// The innermost marker's address: the first record under the values on
    /// top that is not a value or a binary operator.
    fn marker(&mut self) -> Step<u32> {
        let mut at = self.sp;
        loop {
            let header = self.header_at(at)?.ok_or(Error::StackCorrupt)?;
            if !header.kind.is_value() && header.kind != Kind::Binary {
                return Ok(at);
            }
            at += header.length;
        }
    }

    /// Whether a comparison or shift waits within the innermost marker.
    fn relation_pending(&mut self) -> Step<bool> {
        let mut at = self.sp;
        loop {
            let header = self.header_at(at)?.ok_or(Error::StackCorrupt)?;
            match header.kind {
                Kind::Binary => {
                    let op = Op::of(header.aux).ok_or(Error::StackCorrupt)?;
                    if op.priority() == RELATION {
                        return Ok(true);
                    }
                }
                kind if kind.is_value() => {}
                _ => return Ok(false),
            }
            at += header.length;
        }
    }

    /// The binary operators waiting on top applied while none binds looser
    /// than `priority`.
    fn reduce(&mut self, priority: u8) -> Step {
        loop {
            let top = self.top_header()?.ok_or(Error::StackCorrupt)?;
            if !top.kind.is_value() {
                return Err(Error::StackCorrupt.into());
            }
            let below = self.sp + top.length;
            let Some(header) = self.header_at(below)? else {
                return Err(Error::StackCorrupt.into());
            };
            if header.kind != Kind::Binary {
                return Ok(());
            }
            let op = Op::of(header.aux).ok_or(Error::StackCorrupt)?;
            if op.priority() > priority {
                return Ok(());
            }
            let right = self.pop_value()?;
            self.pop_record(Kind::Binary, &mut [])?;
            let left = self.pop_value()?;
            let result = self.binary(op, &left, &right)?;
            self.push_value(&result)?;
        }
    }

    /// The expression ends here: every operator applied, then what the
    /// innermost marker says — a bracket closed, an argument taken, or the
    /// value handed to the statement waiting on it.
    fn expression_end(&mut self) -> Step<Next> {
        self.reduce(u8::MAX)?;
        let marker = self.marker()?;
        let header = self.header_at(marker)?.ok_or(Error::StackCorrupt)?;
        match header.kind {
            Kind::Bracket => {
                self.expect(b')', Error::MissingBracket)?;
                let value = self.pop_value()?;
                self.pop_record(Kind::Bracket, &mut [])?;
                self.operand_is(&value)
            }
            Kind::Arguments => self.argument_end(marker, header.aux),
            Kind::Expression => {
                let value = self.pop_value()?;
                self.pop_record(Kind::Expression, &mut [])?;
                let mut words = [0; super::PAYLOAD_MOST];
                let top = self.top_header()?.ok_or(Error::StackCorrupt)?;
                let carried = ((top.length / 4) as usize - 1).min(super::PAYLOAD_MOST);
                let cont = self.pop_record(Kind::Continuation, &mut words[..carried])?;
                self.mode = Mode::Statement;
                Ok(Next::Resume(
                    cont.aux,
                    Payload::of(&words[..carried]),
                    Delivered::Value(value),
                ))
            }
            _ => Err(Error::StackCorrupt.into()),
        }
    }

    /// An argument of the bracketed function whose record is at `marker`
    /// read: another after a comma, the call at the bracket.
    fn argument_end(&mut self, marker: u32, function: u8) -> Step<Next> {
        let function = Bracketed::of(function).ok_or(Error::StackCorrupt)?;
        let count = self
            .record_word(marker, 0)?
            .checked_add(1)
            .filter(|&count| count <= function.most())
            .ok_or(Error::StackCorrupt)?;
        self.set_record_word(marker, 0, count)?;
        if self.take(b',')? {
            if count >= function.most() {
                return Err(function.too_many().into());
            }
            self.mode = Mode::Operand;
            return Ok(Next::Moved);
        }
        if !self.take(b')')? {
            return Err(Error::MissingBracket.into());
        }
        if count < function.least() {
            return Err(function.too_few().into());
        }
        self.call_bracketed(function, marker, count)
    }

    /// `left op right`.
    pub(crate) fn binary(&mut self, op: Op, left: &Value, right: &Value) -> Step<Value> {
        use core::cmp::Ordering;
        if let (Value::Str(a), Value::Str(b)) = (left, right) {
            let order = a.as_bytes().cmp(b.as_bytes());
            let truth = match op {
                Op::Add => {
                    let mut joined = *a;
                    joined
                        .push_bytes(b.as_bytes())
                        .ok_or(Error::StringTooLong)?;
                    return Ok(Value::Str(joined));
                }
                Op::Eq => order == Ordering::Equal,
                Op::Ne => order != Ordering::Equal,
                Op::Lt => order == Ordering::Less,
                Op::Gt => order == Ordering::Greater,
                Op::Le => order != Ordering::Greater,
                Op::Ge => order != Ordering::Less,
                _ => return Err(Error::NeedNumber.into()),
            };
            return Ok(truth_value(truth));
        }
        let (Some(a), Some(b)) = (left.number(), right.number()) else {
            return Err(match left {
                Value::Str(_) => Error::NeedString,
                _ => Error::NeedNumber,
            }
            .into());
        };
        let arith = &mut self.arith;
        Ok(match op {
            Op::Add => arith.add(a, b)?.into(),
            Op::Sub => arith.sub(a, b)?.into(),
            Op::Mul => arith.mul(a, b)?.into(),
            Op::Div => arith.div(a, b)?.into(),
            Op::Pow => arith.pow(a, b)?.into(),
            Op::IntDiv => Value::Int(arith.div_mod(a, b)?.0),
            Op::Mod => Value::Int(arith.div_mod(a, b)?.1),
            Op::Eq | Op::Ne | Op::Lt | Op::Gt | Op::Le | Op::Ge => {
                let order = arith.compare(a, b)?;
                truth_value(match (op, order) {
                    (_, None) => op == Op::Ne,
                    (Op::Eq, Some(o)) => o == Ordering::Equal,
                    (Op::Ne, Some(o)) => o != Ordering::Equal,
                    (Op::Lt, Some(o)) => o == Ordering::Less,
                    (Op::Gt, Some(o)) => o == Ordering::Greater,
                    (Op::Le, Some(o)) => o != Ordering::Greater,
                    (_, Some(o)) => o != Ordering::Less,
                })
            }
            Op::Shl | Op::Shr | Op::Lsr => {
                let (x, n) = (arith.int(a)?, arith.int(b)?);
                Value::Int(shift(op, x, n))
            }
            Op::And | Op::Or | Op::Eor => {
                let (x, y) = (arith.int(a)?, arith.int(b)?);
                Value::Int(match op {
                    Op::And => x & y,
                    Op::Or => x | y,
                    _ => x ^ y,
                })
            }
        })
    }

    // Literals.

    /// A decimal number at the pointer, taken: an integer where it has no
    /// point or exponent and fits one, a real otherwise.
    fn number_literal(&mut self) -> Step<Value> {
        let mut text = [0; LITERAL_MOST];
        let mut n = 0;
        while n < LITERAL_MOST {
            let b = self.byte_at(self.ptr + super::len32(n))?;
            if !(b.is_ascii_digit() || matches!(b, b'.' | b'E' | b'+' | b'-')) {
                break;
            }
            text[n] = b;
            n += 1;
        }
        let digits = match self.dialect() {
            crate::value::Dialect::V => crate::five::EXPONENT_DIGITS,
            crate::value::Dialect::VI => crate::six::EXPONENT_DIGITS,
        };
        let written = crate::text::Written::literal(&text[..n], digits).ok_or(Error::Mistake)?;
        self.ptr += super::len32(written.length);
        if let Some(n) = self.integer_of(&written) {
            return Ok(Value::Int(n));
        }
        Ok(Value::Real(self.read_real(&written)?))
    }

    /// The integer a number written as `written` is read as, none where the
    /// dialect reads it as a real.
    pub(crate) fn integer_of(&self, written: &crate::text::Written<'_>) -> Option<i32> {
        use crate::text::Integers;
        written.integer(match self.dialect() {
            crate::value::Dialect::V => Integers::Five,
            crate::value::Dialect::VI => Integers::Six,
        })
    }

    /// The real `written` is, as the dialect reads it.
    pub(crate) fn read_real(
        &mut self,
        written: &crate::text::Written<'_>,
    ) -> Step<crate::value::Real> {
        use crate::value::{Arith, Dialect};
        Ok(match self.dialect() {
            Dialect::V => Arith::of_five(crate::five::Five::read(written)?),
            Dialect::VI => Arith::of_six(self.arith.six().read(written)?),
        })
    }

    /// A string literal's characters after its opening quote, taken with its
    /// closing one: `""` stands for a quote.
    fn string_literal(&mut self) -> Step<Text> {
        let mut text = Text::EMPTY;
        loop {
            let b = self.next()?;
            match b {
                b'"' => {
                    if self.peek()? != b'"' {
                        return Ok(text);
                    }
                    self.ptr += 1;
                }
                b'\r' => return Err(Error::MissingQuote.into()),
                _ => {}
            }
            text.push_bytes(&[b]).ok_or(Error::StringTooLong)?;
        }
    }

    /// A hexadecimal number after `&`.
    fn hex_literal(&mut self) -> Step<i32> {
        let mut value: u32 = 0;
        let mut digits = 0;
        while let Some(d) = (self.peek()? as char).to_digit(16) {
            if digits == 8 {
                return Err(Error::HexTooLarge.into());
            }
            value = value << 4 | d;
            digits += 1;
            self.ptr += 1;
        }
        if digits == 0 {
            return Err(Error::BadHex.into());
        }
        Ok(value.cast_signed())
    }

    /// A binary number after `%`.
    fn binary_literal(&mut self) -> Step<i32> {
        let mut value: u32 = 0;
        let mut digits = 0;
        while let b @ (b'0' | b'1') = self.peek()? {
            value = value << 1 | u32::from(b - b'0');
            digits += 1;
            self.ptr += 1;
        }
        if digits == 0 {
            return Err(Error::BadBinary.into());
        }
        Ok(value.cast_signed())
    }
}

/// The longest decimal literal read: more digits than any value needs.
const LITERAL_MOST: usize = 64;

/// BASIC's truth: `TRUE` is -1, `FALSE` 0.
pub(crate) const fn truth_value(truth: bool) -> Value {
    Value::Int(if truth { -1 } else { 0 })
}

/// `x` shifted by `n` as BASIC V shifts: the count's low byte, an ARM
/// register shift.
const fn shift(op: Op, x: i32, n: i32) -> i32 {
    let count = n.cast_unsigned() & 0xFF;
    match op {
        Op::Shl | Op::Lsr if count >= 32 => 0,
        Op::Shl => x << count,
        Op::Lsr => (x.cast_unsigned() >> count).cast_signed(),
        _ if count >= 32 => x >> 31,
        _ => x >> count,
    }
}

#[cfg(test)]
mod tests {
    use super::{shift, Op};

    #[test]
    fn shifts_take_the_count_s_low_byte_as_the_arm_shifts_do() {
        assert_eq!(shift(Op::Shl, 1, 31), i32::MIN);
        assert_eq!(shift(Op::Shl, 1, 32), 0);
        assert_eq!(shift(Op::Shl, 1, 256 + 1), 2, "the low byte");
        assert_eq!(shift(Op::Shr, -8, 1), -4);
        assert_eq!(shift(Op::Shr, -8, 40), -1, "the sign fills");
        assert_eq!(shift(Op::Lsr, -8, 1), 0x7FFF_FFFC);
        assert_eq!(shift(Op::Lsr, -8, 32), 0);
    }
}
