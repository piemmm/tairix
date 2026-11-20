//! Whole arrays: an array assigned from another, from one negated, from two
//! combined element by element or as matrices, from a factor or a list of
//! values; and the functions of an array — `SUM`, `SUMLEN`, `MOD` and
//! `DIM(` — as the BBC BASIC Reference Manual's arrays chapter gives them.

use super::eval::Op;
use super::functions::{SUBSCRIPTS_MOST, UNDIMENSIONED_BELOW};
use super::lvalue::{Element, Lvalue, Place};
use super::next::{Delivered, Next, Payload};
use super::statements::{cont, Assign};
use super::{len32, Machine, Mode, Step};
use crate::messages::Error;
use crate::stack::Kind;
use crate::token;
use crate::value::{Number, Text, Value};

/// An array's shape: its sizes, its element count and where its elements
/// start.
#[derive(Clone, Copy)]
pub(crate) struct Shape {
    sizes: [u32; SUBSCRIPTS_MOST],
    dimensions: usize,
    total: u32,
    first: u32,
    element: Element,
}

impl Shape {
    /// Each dimension's size, one more than its highest subscript.
    pub(crate) fn sizes(&self) -> &[u32] {
        &self.sizes[..self.dimensions]
    }

    /// Its element count.
    pub(crate) const fn total(&self) -> u32 {
        self.total
    }

    /// Element `index`'s address, its elements `size` bytes each.
    pub(crate) const fn address(&self, index: u32, size: u32) -> u32 {
        self.first.wrapping_add(index.wrapping_mul(size))
    }

    fn same_as(&self, other: &Self) -> bool {
        self.element == other.element && self.sizes() == other.sizes()
    }

    /// The error its use with `other`, of a different shape, gives.
    fn mismatch(&self, other: &Self) -> Error {
        if self.element == other.element {
            Error::ArraySize
        } else {
            Error::ArraysMismatch
        }
    }
}

/// The stages of a whole-array assignment's right-hand side.
mod stage {
    /// `+=` or `-=`'s value.
    pub(super) const STEP_ALL: u32 = 1;
    /// A factor read first: the whole value, a list's first, or the left of
    /// an operation with an array.
    pub(super) const FACTOR: u32 = 2;
    /// A list's later value.
    pub(super) const LIST: u32 = 3;
    /// The factor after an array and its operator.
    pub(super) const RIGHT_FACTOR: u32 = 4;
    /// `DIM(`'s dimension.
    pub(super) const DIM_NUMBER: u32 = 5;
}

/// The array operators.
const fn array_op(b: u8) -> Option<Op> {
    match b {
        b'+' => Some(Op::Add),
        b'-' => Some(Op::Sub),
        b'*' => Some(Op::Mul),
        b'/' => Some(Op::Div),
        _ => None,
    }
}

impl Machine<'_, '_> {
    /// The shape of the array whose pointer word is at `pointer_at`.
    pub(crate) fn shape(&mut self, pointer_at: u32, element: Element) -> Step<Shape> {
        let block = self.os.read_word(pointer_at)?;
        if block < UNDIMENSIONED_BELOW {
            return Err(Error::Undimensioned.into());
        }
        let mut sizes = [0; SUBSCRIPTS_MOST];
        let mut dimensions = 0;
        loop {
            let size = self.os.read_word(block + 4 * len32(dimensions))?;
            if size == 0 {
                break;
            }
            if dimensions == SUBSCRIPTS_MOST {
                return Err(Error::ArrayReference.into());
            }
            sizes[dimensions] = size;
            dimensions += 1;
        }
        let total = self.os.read_word(block + 4 * len32(dimensions) + 4)?;
        Ok(Shape {
            sizes,
            dimensions,
            total,
            first: block + 4 * len32(dimensions) + 8,
            element,
        })
    }

    /// Element `index` of `shape`'s array.
    pub(crate) fn element_at(&self, shape: &Shape, index: u32) -> Lvalue {
        let size = shape.element.place().bytes(self.dialect());
        Lvalue {
            address: shape.address(index, size),
            place: shape.element.place(),
        }
    }

    /// The array named at the pointer, `name()`, its brackets taken: its
    /// pointer word's address and element; none where no array is named.
    fn array_name(&mut self) -> Step<Option<(u32, Element)>> {
        let start = self.ptr;
        let Some(name) = self.read_name()? else {
            return Ok(None);
        };
        if !name.is_array() || self.peek()? != b')' {
            self.ptr = start;
            return Ok(None);
        }
        self.ptr += 1;
        let lvalue = self.variable(&name, false)?.ok_or(Error::UnknownArray)?;
        match lvalue.place {
            Place::Array(element) => Ok(Some((lvalue.address, element))),
            _ => Err(Error::NeedArray.into()),
        }
    }

    /// A whole array assigned, its operator `op`.
    pub(crate) fn array_assignment(&mut self, target: Lvalue, op: Assign) -> Step<Next> {
        let Place::Array(element) = target.place else {
            return Err(Error::NeedArray.into());
        };
        let payload = Payload::of(&[0, target.address, element as u32, op as u32]);
        if op != Assign::Set {
            return self.evaluate(cont::ARRAY, payload.with(0, stage::STEP_ALL));
        }
        let b = self.skip_spaces()?;
        let negated = b == b'-' && {
            let at = self.ptr;
            self.ptr += 1;
            self.skip_spaces()?;
            let array = self.array_name()?.is_some();
            self.ptr = at;
            array
        };
        if negated {
            self.ptr += 1;
            self.skip_spaces()?;
        }
        if let Some((source, source_element)) = self.array_name()? {
            return self.array_from_array(target.address, element, source, source_element, negated);
        }
        self.evaluate_factor(cont::ARRAY, payload.with(0, stage::FACTOR))
    }

    /// `target() = [-]source() [op operand]`.
    fn array_from_array(
        &mut self,
        target: u32,
        element: Element,
        source: u32,
        source_element: Element,
        negated: bool,
    ) -> Step<Next> {
        let left = self.shape(source, source_element)?;
        let into = self.shape(target, element)?;
        let b = self.skip_spaces()?;
        if !negated && b == b'.' {
            self.ptr += 1;
            self.skip_spaces()?;
            let (right, right_element) = self.array_name()?.ok_or(Error::NeedArray)?;
            let right = self.shape(right, right_element)?;
            self.matrix_multiply(&into, &left, &right)?;
            return Ok(Next::Done);
        }
        let op = if negated { None } else { array_op(b) };
        let Some(op) = op else {
            if !left.same_as(&into) {
                return Err(left.mismatch(&into).into());
            }
            for i in 0..into.total {
                let value = self.read(self.element_at(&left, i))?;
                let value = if negated {
                    let n = value.number().ok_or(Error::NeedNumericArray)?;
                    self.arith.negate(n)?.into()
                } else {
                    value
                };
                self.write(self.element_at(&into, i), &value)?;
                self.os.yield_point()?;
            }
            return Ok(Next::Done);
        };
        self.ptr += 1;
        self.skip_spaces()?;
        if let Some((right, right_element)) = self.array_name()? {
            let right = self.shape(right, right_element)?;
            if !left.same_as(&into) || !right.same_as(&into) {
                return Err(left.mismatch(&into).into());
            }
            for i in 0..into.total {
                let a = self.read(self.element_at(&left, i))?;
                let b = self.read(self.element_at(&right, i))?;
                let value = self.array_binary(op, &a, &b, element)?;
                self.write(self.element_at(&into, i), &value)?;
                self.os.yield_point()?;
            }
            return Ok(Next::Done);
        }
        let payload = Payload::of(&[
            stage::RIGHT_FACTOR,
            target,
            element as u32,
            op as u32,
            source,
            source_element as u32,
        ]);
        self.evaluate_factor(cont::ARRAY, payload)
    }

    /// `a op b` for an array's elements: only `+` joins strings.
    fn array_binary(&mut self, op: Op, a: &Value, b: &Value, element: Element) -> Step<Value> {
        if element == Element::Str && op != Op::Add {
            return Err(Error::NeedNumericArray.into());
        }
        self.binary(op, a, b)
    }

    /// A whole-array assignment's operand delivered, by its stage.
    pub(crate) fn array_operand(&mut self, payload: Payload, delivered: &Delivered) -> Step<Next> {
        let Delivered::Value(value) = delivered else {
            return Err(Error::StackCorrupt.into());
        };
        let target = payload.word(1);
        let element = element_of(payload.word(2));
        match payload.word(0) {
            stage::STEP_ALL => {
                let into = self.shape(target, element)?;
                let op = if payload.word(3) == Assign::Add as u32 {
                    Op::Add
                } else {
                    Op::Sub
                };
                for i in 0..into.total {
                    let current = self.read(self.element_at(&into, i))?;
                    let result = self.array_binary(op, &current, value, element)?;
                    self.write(self.element_at(&into, i), &result)?;
                    self.os.yield_point()?;
                }
                Ok(Next::Done)
            }
            stage::FACTOR => self.array_after_factor(target, element, value),
            stage::LIST => {
                let into = self.shape(target, element)?;
                let index = payload.word(3);
                if index >= into.total {
                    return Err(Error::ArraySize.into());
                }
                self.write(self.element_at(&into, index), value)?;
                if self.take(b',')? {
                    return self.evaluate(cont::ARRAY, payload.with(3, index + 1));
                }
                Ok(Next::Done)
            }
            stage::RIGHT_FACTOR => {
                let into = self.shape(target, element)?;
                let source = self.shape(payload.word(4), element_of(payload.word(5)))?;
                if !source.same_as(&into) {
                    return Err(source.mismatch(&into).into());
                }
                let op = op_of(payload.word(3))?;
                for i in 0..into.total {
                    let a = self.read(self.element_at(&source, i))?;
                    let result = self.array_binary(op, &a, value, element)?;
                    self.write(self.element_at(&into, i), &result)?;
                    self.os.yield_point()?;
                }
                Ok(Next::Done)
            }
            stage::DIM_NUMBER => {
                let n = self.int_value(value)?;
                self.expect(b')', Error::MissingBracket)?;
                let shape = self.shape(payload.word(1), element_of(payload.word(2)))?;
                let index = usize::try_from(n)
                    .ok()
                    .and_then(|n| n.checked_sub(1))
                    .filter(|&i| i < shape.dimensions)
                    .ok_or(Error::SubscriptRange)?;
                let highest = shape.sizes()[index] - 1;
                self.mode = Mode::Operator;
                self.operand_is(&Value::Int(highest.cast_signed()))
            }
            _ => Err(Error::StackCorrupt.into()),
        }
    }

    /// A factor read first: then an operator and an array, a list, or every
    /// element set to it.
    fn array_after_factor(&mut self, target: u32, element: Element, value: &Value) -> Step<Next> {
        let into = self.shape(target, element)?;
        let b = self.skip_spaces()?;
        if let Some(op) = array_op(b) {
            let at = self.ptr;
            self.ptr += 1;
            self.skip_spaces()?;
            if let Some((source, source_element)) = self.array_name()? {
                let source = self.shape(source, source_element)?;
                if !source.same_as(&into) {
                    return Err(source.mismatch(&into).into());
                }
                for i in 0..into.total {
                    let b = self.read(self.element_at(&source, i))?;
                    let result = self.array_binary(op, value, &b, element)?;
                    self.write(self.element_at(&into, i), &result)?;
                    self.os.yield_point()?;
                }
                return Ok(Next::Done);
            }
            self.ptr = at;
        }
        if self.take(b',')? {
            if into.total == 0 {
                return Err(Error::ArraySize.into());
            }
            self.write(self.element_at(&into, 0), value)?;
            let payload = Payload::of(&[stage::LIST, target, element as u32, 1]);
            return self.evaluate(cont::ARRAY, payload);
        }
        for i in 0..into.total {
            self.write(self.element_at(&into, i), value)?;
            self.os.yield_point()?;
        }
        Ok(Next::Done)
    }

    /// `into() = left().right()`: matrices multiplied, a vector either side
    /// taken as a row or a column; through room on the stack where `into`
    /// is one of them.
    fn matrix_multiply(&mut self, into: &Shape, left: &Shape, right: &Shape) -> Step {
        if left.element == Element::Str || right.element == Element::Str {
            return Err(Error::NeedNumericArray.into());
        }
        let (rows, inner, columns) = match (left.dimensions, right.dimensions) {
            (2, 2) => (left.sizes[0], left.sizes[1], right.sizes[1]),
            (2, 1) => (left.sizes[0], left.sizes[1], 1),
            (1, 2) => (1, left.sizes[0], right.sizes[1]),
            _ => return Err(Error::ArraySize.into()),
        };
        // Each array's count must be its sizes', which one a program has
        // overwritten need not be.
        let elements = |a: u32, b: u32| a.checked_mul(b);
        if inner != right.sizes[0]
            || elements(rows, columns) != Some(into.total)
            || elements(rows, inner) != Some(left.total)
            || elements(inner, columns) != Some(right.total)
        {
            return Err(Error::ArraySize.into());
        }
        let aliased = into.first == left.first || into.first == right.first;
        let size = into.element.place().bytes(self.dialect());
        let room = into.total.checked_mul(size).ok_or(Error::ArraySize)?;
        let (result, saved_sp) = if aliased {
            let saved = self.sp;
            let at = self.local_block(room.div_ceil(4) as usize, 1, Error::MatrixSame)?;
            (at, Some(saved))
        } else {
            (into.first, None)
        };
        let out = Shape {
            first: result,
            ..*into
        };
        for r in 0..rows {
            for c in 0..columns {
                let mut sum = Number::Int(0);
                for k in 0..inner {
                    let a = self.read(self.element_at(left, r * inner + k))?;
                    let b = self.read(self.element_at(right, k * columns + c))?;
                    let product = self
                        .binary(Op::Mul, &a, &b)?
                        .number()
                        .ok_or(Error::NeedNumber)?;
                    sum = self.arith.add(sum, product)?;
                }
                self.write(self.element_at(&out, r * columns + c), &sum.into())?;
                self.os.yield_point()?;
            }
        }
        if let Some(saved) = saved_sp {
            self.os.copy_bytes(result, into.first, room)?;
            self.sp = saved;
        }
        Ok(())
    }

    // The functions of an array.

    /// `SUM a()`, `SUMLEN a$()`.
    pub(crate) fn sum_operand(&mut self) -> Step<Next> {
        let length = self.take(token::LEN)?;
        let (array, element) = self.array_argument()?;
        let shape = self.shape(array, element)?;
        let value = if length {
            if element != Element::Str {
                return Err(Error::NeedStringArray.into());
            }
            let mut total: i32 = 0;
            for i in 0..shape.total {
                let text = self.read_sib(self.element_at(&shape, i).address)?;
                total = total.wrapping_add(i32::from(text.len()));
            }
            Value::Int(total)
        } else if element == Element::Str {
            let mut joined = Text::EMPTY;
            for i in 0..shape.total {
                let text = self.read_sib(self.element_at(&shape, i).address)?;
                joined
                    .push_bytes(text.as_bytes())
                    .ok_or(Error::StringTooLong)?;
            }
            Value::Str(joined)
        } else {
            let mut sum = Number::Int(0);
            for i in 0..shape.total {
                let value = self.read(self.element_at(&shape, i))?;
                sum = self
                    .arith
                    .add(sum, value.number().ok_or(Error::NeedNumber)?)?;
                self.os.yield_point()?;
            }
            sum.into()
        };
        self.operand_is(&value)
    }

    /// `MOD a()`: the square root of the sum of its elements' squares.
    pub(crate) fn modulus_operand(&mut self) -> Step<Next> {
        let (array, element) = self.array_argument()?;
        if element == Element::Str {
            return Err(Error::NeedNumericArray.into());
        }
        let shape = self.shape(array, element)?;
        let mut sum = Number::Int(0);
        for i in 0..shape.total {
            let value = self.read(self.element_at(&shape, i))?;
            let n = value.number().ok_or(Error::NeedNumber)?;
            let square = self.arith.mul(n, n)?;
            sum = self.arith.add(sum, square)?;
            self.os.yield_point()?;
        }
        let root =
            self.arith
                .function(sum, crate::five::Five::sqrt, crate::six::Arithmetic::sqrt)?;
        self.operand_is(&Value::Real(root))
    }

    /// The array a function of one takes, `a()`, bracketed or not.
    fn array_argument(&mut self) -> Step<(u32, Element)> {
        self.skip_spaces()?;
        let bracketed = self.take(b'(')?;
        if bracketed {
            self.skip_spaces()?;
        }
        let array = self.array_name()?.ok_or(Error::NeedArray)?;
        if bracketed {
            self.expect(b')', Error::MissingBracket)?;
        }
        Ok(array)
    }

    /// `DIM(a())` and `DIM(a(), n)`.
    pub(crate) fn dim_operand(&mut self) -> Step<Next> {
        self.skip_spaces()?;
        let (array, element) = match self.array_name() {
            Err(super::Trap::Basic(Error::UnknownArray)) => {
                return Err(Error::UnknownArrayInDim.into())
            }
            named => named?.ok_or(Error::DimNeedsArray)?,
        };
        if self.take(b',')? {
            let payload = Payload::of(&[stage::DIM_NUMBER, array, element as u32]);
            self.push(Kind::Continuation, cont::ARRAY, payload.words())?;
            self.push(Kind::Expression, 0, &[])?;
            self.mode = Mode::Operand;
            return Ok(Next::Moved);
        }
        self.expect(b')', Error::MissingBracket)?;
        let shape = self.shape(array, element)?;
        self.operand_is(&Value::Int(len32(shape.dimensions).cast_signed()))
    }
}

/// The element kind a payload word names.
const fn element_of(word: u32) -> Element {
    match word {
        0 => Element::Int,
        1 => Element::Real,
        _ => Element::Str,
    }
}

/// The operator a payload word names.
fn op_of(word: u32) -> Step<Op> {
    [Op::Add, Op::Sub, Op::Mul, Op::Div]
        .into_iter()
        .find(|&op| op as u32 == word)
        .ok_or_else(|| Error::StackCorrupt.into())
}
