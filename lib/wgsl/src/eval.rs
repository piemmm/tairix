//! Constant evaluation: the values const- and override-expressions take,
//! held in a pool and shared — a composite names its components rather than
//! copying them, and a zero composite holds none — with each operation's
//! result as the specification defines it, every overflow, non-finite float
//! and domain error a stop, and all work charged to a fixed budget.

use alloc::vec::Vec;
use core::fmt::Write as _;

use tairix_util::mathf;

use crate::diag::{error, push, reserve, Code, Outcome, Span, Stop};
use crate::syntax::{BinaryOp, UnaryOp};
use crate::types::{Count, Scalar, Ty, TyId, Types};

/// The most component operations one module's constant evaluation may make.
pub(crate) const MAX_EVALUATION: u64 = 1 << 24;

/// A value, by its index in the pool.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub(crate) struct ValueId(pub(crate) u32);

/// A value.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) enum Value {
    Bool(bool),
    AbstractInt(i64),
    AbstractFloat(f64),
    I32(i32),
    U32(u32),
    F32(f32),
    /// A binary16, by its bits.
    F16(u16),
    /// A vector, matrix, array or structure of type `ty`: its components'
    /// values, `len` from `start` in the pool's parts.
    Composite {
        ty: TyId,
        start: u32,
        len: u32,
    },
    /// A composite of type `ty` whose every component is zero.
    Zero(TyId),
}

/// The values a module's constant evaluation made, and the work it has
/// left.
pub(crate) struct Values {
    list: Vec<Value>,
    parts: Vec<ValueId>,
    budget: u64,
}

impl Default for Values {
    fn default() -> Self {
        Self {
            list: Vec::new(),
            parts: Vec::new(),
            budget: MAX_EVALUATION,
        }
    }
}

/// A scalar's value read as a wide number, for comparison and conversion.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) enum Number {
    Int(i128),
    Float(f64),
}

impl Values {
    pub(crate) fn get(&self, id: ValueId) -> Value {
        self.list
            .get(id.0 as usize)
            .copied()
            .unwrap_or(Value::Bool(false))
    }

    pub(crate) fn add(&mut self, value: Value) -> Outcome<ValueId> {
        let id = ValueId(u32::try_from(self.list.len()).map_err(|_| Stop::OutOfMemory)?);
        push(&mut self.list, value)?;
        Ok(id)
    }

    /// Charge `work` component operations, stopping at `span` when the
    /// budget is spent.
    pub(crate) fn charge(&mut self, work: u64, span: Span) -> Outcome<()> {
        if work > self.budget {
            return Err(error(Code::Evaluation, span, |m| {
                write!(
                    m,
                    "constant evaluation takes more than {MAX_EVALUATION} operations"
                )
            }));
        }
        self.budget -= work;
        Ok(())
    }

    /// The composite of type `ty` holding `parts`.
    pub(crate) fn composite(&mut self, ty: TyId, parts: &[ValueId]) -> Outcome<ValueId> {
        let start = u32::try_from(self.parts.len()).map_err(|_| Stop::OutOfMemory)?;
        let len = u32::try_from(parts.len()).map_err(|_| Stop::OutOfMemory)?;
        reserve(&mut self.parts, parts.len())?;
        self.parts.extend_from_slice(parts);
        self.add(Value::Composite { ty, start, len })
    }

    /// The zero value of `ty`.
    pub(crate) fn zero(&mut self, types: &Types, ty: TyId) -> Outcome<ValueId> {
        let value = match types.get(ty) {
            Ty::Scalar(scalar) => zero_scalar(scalar),
            _ => Value::Zero(ty),
        };
        self.add(value)
    }

    /// How many components a composite of type `ty` has: a vector's, a
    /// matrix's columns, an array's elements, a structure's members.
    pub(crate) fn arity(types: &Types, ty: TyId) -> u32 {
        match types.get(ty) {
            Ty::Vector(n, _) => u32::from(n),
            Ty::Matrix { columns, .. } => u32::from(columns),
            // Only an array of fixed count is constructible, so has values.
            Ty::Array {
                count: Count::Fixed(n),
                ..
            } => n,
            Ty::Struct(s) => u32::try_from(types.members_of(s).len()).unwrap_or(0),
            _ => 0,
        }
    }

    /// The type of component `index` of a composite of type `ty`.
    pub(crate) fn component_type(types: &mut Types, ty: TyId, index: u32) -> Outcome<TyId> {
        match types.get(ty) {
            Ty::Vector(_, scalar) => types.scalar(scalar),
            Ty::Matrix { rows, scalar, .. } => types.intern(Ty::Vector(rows, scalar)),
            Ty::Array { element, .. } => Ok(element),
            Ty::Struct(s) => types
                .members_of(s)
                .get(index as usize)
                .map(|m| m.ty)
                .ok_or(Stop::Internal),
            _ => Ok(ty),
        }
    }

    /// Component `index` of the composite `id` of type `ty`.
    pub(crate) fn component(
        &mut self,
        types: &mut Types,
        (id, ty): (ValueId, TyId),
        index: u32,
    ) -> Outcome<ValueId> {
        match self.get(id) {
            Value::Composite { start, len, .. } => {
                if index >= len {
                    return Err(Stop::Internal);
                }
                self.parts
                    .get((start + index) as usize)
                    .copied()
                    .ok_or(Stop::Internal)
            }
            Value::Zero(_) => {
                let part = Self::component_type(types, ty, index)?;
                self.zero(types, part)
            }
            // A scalar splat across a composite is each of its components.
            _ => Ok(id),
        }
    }

    /// Every component of `id`, of type `ty`, into `out`.
    pub(crate) fn components(
        &mut self,
        types: &mut Types,
        (id, ty): (ValueId, TyId),
        out: &mut Vec<ValueId>,
    ) -> Outcome<()> {
        let arity = Self::arity(types, ty);
        self.charge(u64::from(arity), Span::default())?;
        reserve(out, arity as usize)?;
        for index in 0..arity {
            let part = self.component(types, (id, ty), index)?;
            out.push(part);
        }
        Ok(())
    }

    /// `id`, of type `from`, converted to `to`: an automatic conversion of
    /// an abstract value, lifted through vectors, matrices and arrays of one
    /// shape. A value its new type cannot hold stops at `span`.
    pub(crate) fn convert(
        &mut self,
        types: &mut Types,
        (id, from): (ValueId, TyId),
        to: TyId,
        span: Span,
    ) -> Outcome<ValueId> {
        if from == to {
            return Ok(id);
        }
        if let (Ty::Scalar(_), Ty::Scalar(target)) = (types.get(from), types.get(to)) {
            self.charge(1, span)?;
            let value = convert_scalar(self.get(id), target, span)?;
            return self.add(value);
        }
        if let Value::Zero(_) = self.get(id) {
            return self.add(Value::Zero(to));
        }
        let mut parts = Vec::new();
        self.components(types, (id, from), &mut parts)?;
        for (index, part) in parts.iter_mut().enumerate() {
            let index = u32::try_from(index).map_err(|_| Stop::OutOfMemory)?;
            let part_from = Self::component_type(types, from, index)?;
            let part_to = Self::component_type(types, to, index)?;
            *part = self.convert(types, (*part, part_from), part_to, span)?;
        }
        self.composite(to, &parts)
    }

    /// `f` applied at each component of `args` — each of `result`'s shape,
    /// or a scalar splat across it — giving a value of `result`.
    pub(crate) fn lift(
        &mut self,
        types: &mut Types,
        args: &[(ValueId, TyId)],
        (result, span): (TyId, Span),
        f: &mut dyn FnMut(&[Value]) -> Outcome<Value>,
    ) -> Outcome<ValueId> {
        const MOST: usize = 4;
        let mut scalars = [Value::Bool(false); MOST];
        let count = args.len();
        if count > MOST {
            return Err(Stop::Internal);
        }
        let lanes = match types.get(result) {
            Ty::Vector(n, _) => u32::from(n),
            Ty::Scalar(_) => {
                self.charge(1, span)?;
                for (slot, &(id, _)) in scalars.iter_mut().zip(args) {
                    *slot = self.get(id);
                }
                let value = f(&scalars[..count])?;
                return self.add(value);
            }
            _ => return Err(Stop::Internal),
        };
        let mut parts = Vec::new();
        reserve(&mut parts, lanes as usize)?;
        for lane in 0..lanes {
            self.charge(1, span)?;
            for (slot, &(id, ty)) in scalars.iter_mut().zip(args) {
                let part = if matches!(types.get(ty), Ty::Vector(..)) {
                    self.component(types, (id, ty), lane)?
                } else {
                    id
                };
                *slot = self.get(part);
            }
            let value = f(&scalars[..count])?;
            parts.push(self.add(value)?);
        }
        self.composite(result, &parts)
    }

    /// The scalar components of `id`, of type `ty`: itself for a scalar.
    pub(crate) fn scalars(
        &mut self,
        types: &mut Types,
        (id, ty): (ValueId, TyId),
        out: &mut Vec<Value>,
    ) -> Outcome<()> {
        out.clear();
        if let Some(value) = self.scalar(id) {
            return push(out, value);
        }
        let mut parts = Vec::new();
        self.components(types, (id, ty), &mut parts)?;
        reserve(out, parts.len())?;
        for part in parts {
            out.push(self.get(part));
        }
        Ok(())
    }

    /// The scalar `id` holds, if it is one.
    pub(crate) fn scalar(&self, id: ValueId) -> Option<Value> {
        let value = self.get(id);
        (!matches!(value, Value::Composite { .. } | Value::Zero(_))).then_some(value)
    }

    /// `id` as an integer, if it is an integer scalar.
    pub(crate) fn integer(&self, id: ValueId) -> Option<i128> {
        match self.scalar(id).map(number) {
            Some(Number::Int(n)) => Some(n),
            _ => None,
        }
    }

    /// `id` as a bool, if it is one.
    pub(crate) fn boolean(&self, id: ValueId) -> Option<bool> {
        match self.get(id) {
            Value::Bool(b) => Some(b),
            _ => None,
        }
    }

    /// `op` applied to `operand` of type `ty`, component by component.
    pub(crate) fn unary(
        &mut self,
        types: &mut Types,
        op: UnaryOp,
        (operand, ty): (ValueId, TyId),
        span: Span,
    ) -> Outcome<ValueId> {
        if let Ty::Scalar(_) = types.get(ty) {
            self.charge(1, span)?;
            let value = unary_scalar(op, self.get(operand), span)?;
            return self.add(value);
        }
        let mut parts = Vec::new();
        self.components(types, (operand, ty), &mut parts)?;
        let part_ty = Self::component_type(types, ty, 0)?;
        for part in &mut parts {
            *part = self.unary(types, op, (*part, part_ty), span)?;
        }
        self.composite(ty, &parts)
    }

    /// `op` applied to `lhs` and `rhs`, their types those the operator's
    /// overload takes, giving a value of `result`: scalars, vectors
    /// component by component with a scalar operand splat, and matrices as
    /// linear algebra.
    pub(crate) fn binary(
        &mut self,
        types: &mut Types,
        op: BinaryOp,
        (lhs, lhs_ty): (ValueId, TyId),
        (rhs, rhs_ty): (ValueId, TyId),
        result: TyId,
        span: Span,
    ) -> Outcome<ValueId> {
        match (types.get(lhs_ty), types.get(rhs_ty)) {
            (Ty::Scalar(_), Ty::Scalar(_)) => {
                self.charge(1, span)?;
                let value = binary_scalar(op, self.get(lhs), self.get(rhs), span)?;
                self.add(value)
            }
            (Ty::Matrix { .. }, _) | (_, Ty::Matrix { .. })
                if op == BinaryOp::Multiply
                    && !matches!(
                        (types.get(lhs_ty), types.get(rhs_ty)),
                        (Ty::Matrix { .. }, Ty::Scalar(_)) | (Ty::Scalar(_), Ty::Matrix { .. })
                    ) =>
            {
                self.product(types, (lhs, lhs_ty), (rhs, rhs_ty), result, span)
            }
            _ => self.componentwise(types, op, (lhs, lhs_ty), (rhs, rhs_ty), result, span),
        }
    }

    /// `op` component by component, a scalar operand splat across the
    /// other's components.
    fn componentwise(
        &mut self,
        types: &mut Types,
        op: BinaryOp,
        (lhs, lhs_ty): (ValueId, TyId),
        (rhs, rhs_ty): (ValueId, TyId),
        result: TyId,
        span: Span,
    ) -> Outcome<ValueId> {
        let arity = Self::arity(types, result);
        let mut parts = Vec::new();
        reserve(&mut parts, arity as usize)?;
        let splat_l = matches!(types.get(lhs_ty), Ty::Scalar(_));
        let splat_r = matches!(types.get(rhs_ty), Ty::Scalar(_));
        let lhs_part = if splat_l {
            lhs_ty
        } else {
            Self::component_type(types, lhs_ty, 0)?
        };
        let rhs_part = if splat_r {
            rhs_ty
        } else {
            Self::component_type(types, rhs_ty, 0)?
        };
        let result_part = Self::component_type(types, result, 0)?;
        for index in 0..arity {
            let l = if splat_l {
                lhs
            } else {
                self.component(types, (lhs, lhs_ty), index)?
            };
            let r = if splat_r {
                rhs
            } else {
                self.component(types, (rhs, rhs_ty), index)?
            };
            let part = self.binary(types, op, (l, lhs_part), (r, rhs_part), result_part, span)?;
            parts.push(part);
        }
        self.composite(result, &parts)
    }

    /// A matrix product: matrix by vector, vector by matrix, or matrix by
    /// matrix, each a sum of products in the result's scalar.
    fn product(
        &mut self,
        types: &mut Types,
        (lhs, lhs_ty): (ValueId, TyId),
        (rhs, rhs_ty): (ValueId, TyId),
        result: TyId,
        span: Span,
    ) -> Outcome<ValueId> {
        // Every operand becomes a list of its columns' components: a vector
        // on the left is one row, on the right one column.
        let left = self.grid(types, (lhs, lhs_ty), true)?;
        let right = self.grid(types, (rhs, rhs_ty), false)?;
        let scalar = types.scalar_of(result).ok_or(Stop::Internal)?;
        let scalar_ty = types.scalar(scalar)?;
        // left: rows × k, right: k × columns, each stored column-major.
        let (rows, inner) = (left.rows, left.columns);
        let columns = right.columns;
        if right.rows != inner {
            return Err(Stop::Internal);
        }
        let mut cells = Vec::new();
        reserve(&mut cells, (rows * columns) as usize)?;
        for column in 0..columns {
            for row in 0..rows {
                let mut sum: Option<ValueId> = None;
                for k in 0..inner {
                    let a = left.at(row, k);
                    let b = right.at(k, column);
                    let term = self.binary(
                        types,
                        BinaryOp::Multiply,
                        (a, scalar_ty),
                        (b, scalar_ty),
                        scalar_ty,
                        span,
                    )?;
                    sum = Some(match sum {
                        None => term,
                        Some(total) => self.binary(
                            types,
                            BinaryOp::Add,
                            (total, scalar_ty),
                            (term, scalar_ty),
                            scalar_ty,
                            span,
                        )?,
                    });
                }
                cells.push(sum.ok_or(Stop::Internal)?);
            }
        }
        match types.get(result) {
            Ty::Vector(..) => self.composite(result, &cells),
            Ty::Matrix {
                rows: result_rows,
                scalar,
                ..
            } => {
                let column_ty = types.intern(Ty::Vector(result_rows, scalar))?;
                let mut built = Vec::new();
                reserve(&mut built, columns as usize)?;
                for column in cells.chunks(rows as usize) {
                    built.push(self.composite(column_ty, column)?);
                }
                self.composite(result, &built)
            }
            _ => Err(Stop::Internal),
        }
    }

    /// A matrix's or vector's components as a grid, column-major: a vector
    /// is one row where `row_vector`, else one column.
    fn grid(
        &mut self,
        types: &mut Types,
        (id, ty): (ValueId, TyId),
        row_vector: bool,
    ) -> Outcome<Grid> {
        let mut cells = Vec::new();
        match types.get(ty) {
            Ty::Vector(n, _) => {
                self.components(types, (id, ty), &mut cells)?;
                let n = u32::from(n);
                Ok(if row_vector {
                    Grid {
                        cells,
                        rows: 1,
                        columns: n,
                    }
                } else {
                    Grid {
                        cells,
                        rows: n,
                        columns: 1,
                    }
                })
            }
            Ty::Matrix { columns, rows, .. } => {
                let mut column_values = Vec::new();
                self.components(types, (id, ty), &mut column_values)?;
                let column_ty = Self::component_type(types, ty, 0)?;
                for column in column_values {
                    self.components(types, (column, column_ty), &mut cells)?;
                }
                Ok(Grid {
                    cells,
                    rows: u32::from(rows),
                    columns: u32::from(columns),
                })
            }
            _ => Err(Stop::Internal),
        }
    }
}

/// A grid of components, column-major.
struct Grid {
    cells: Vec<ValueId>,
    rows: u32,
    columns: u32,
}

impl Grid {
    fn at(&self, row: u32, column: u32) -> ValueId {
        self.cells
            .get((column * self.rows + row) as usize)
            .copied()
            .unwrap_or(ValueId(0))
    }
}

/// The zero of `scalar`.
pub(crate) const fn zero_scalar(scalar: Scalar) -> Value {
    match scalar {
        Scalar::Bool => Value::Bool(false),
        Scalar::AbstractInt => Value::AbstractInt(0),
        Scalar::AbstractFloat => Value::AbstractFloat(0.0),
        Scalar::I32 => Value::I32(0),
        Scalar::U32 => Value::U32(0),
        Scalar::F32 => Value::F32(0.0),
        Scalar::F16 => Value::F16(0),
    }
}

/// A scalar value as a wide number; a bool as zero or one.
pub(crate) fn number(value: Value) -> Number {
    match value {
        Value::Bool(b) => Number::Int(i128::from(b)),
        Value::AbstractInt(n) => Number::Int(i128::from(n)),
        Value::I32(n) => Number::Int(i128::from(n)),
        Value::U32(n) => Number::Int(i128::from(n)),
        Value::AbstractFloat(x) => Number::Float(x),
        Value::F32(x) => Number::Float(f64::from(x)),
        Value::F16(bits) => Number::Float(f64::from(mathf::from_half(bits))),
        Value::Composite { .. } | Value::Zero(_) => Number::Int(0),
    }
}

/// The stop for a value its type cannot hold.
fn unrepresentable(span: Span, what: &str) -> Stop {
    error(Code::Evaluation, span, |m| {
        write!(m, "the value is not representable in {what}")
    })
}

/// The stop for a float result that is not finite.
pub(crate) fn not_finite(span: Span) -> Stop {
    error(Code::Evaluation, span, |m| {
        m.write_str("a constant evaluates to an infinity or a NaN")
    })
}

/// `x` as `target`, a float, checked finite.
pub(crate) fn float_of(x: f64, target: Scalar, span: Span) -> Outcome<Value> {
    if !x.is_finite() {
        return Err(not_finite(span));
    }
    let value = match target {
        Scalar::AbstractFloat => Value::AbstractFloat(x),
        Scalar::F32 => {
            let narrowed = mathf::single(x);
            if !narrowed.is_finite() {
                return Err(unrepresentable(span, "f32"));
            }
            Value::F32(narrowed)
        }
        Scalar::F16 => {
            let bits = half(x);
            if !f64::from(mathf::from_half(bits)).is_finite() {
                return Err(unrepresentable(span, "f16"));
            }
            Value::F16(bits)
        }
        _ => return Err(unrepresentable(span, "a float")),
    };
    Ok(value)
}

/// `x` rounded once to the nearest binary16, ties to even, as its bits:
/// infinite past its range. Rounding through binary32 instead would round
/// twice, and some values just past a half's tie would land on the tie.
pub(crate) fn half(x: f64) -> u16 {
    let [low, high, ..] = mathf::float_bits(16, x).to_le_bytes();
    u16::from_le_bytes([low, high])
}

/// The integer `n` as the float `target`, rounded once from its exact
/// value.
fn int_float(n: i128, target: Scalar, span: Span) -> Outcome<Value> {
    #[allow(
        clippy::cast_precision_loss,
        reason = "an integer to float cast rounds once to nearest, ties to even"
    )]
    match target {
        Scalar::F32 => Ok(Value::F32(n as f32)),
        // Exact below 2^53, and past it no binary16 is finite.
        Scalar::F16 => float_of(n as f64, target, span),
        Scalar::AbstractFloat => Ok(Value::AbstractFloat(n as f64)),
        _ => Err(unrepresentable(span, "a float")),
    }
}

/// `n` as the integer scalar `target`, checked in range.
pub(crate) fn int_of(n: i128, target: Scalar, span: Span) -> Outcome<Value> {
    match target {
        Scalar::AbstractInt => i64::try_from(n)
            .map(Value::AbstractInt)
            .map_err(|_| unrepresentable(span, "abstract-int")),
        Scalar::I32 => i32::try_from(n)
            .map(Value::I32)
            .map_err(|_| unrepresentable(span, "i32")),
        Scalar::U32 => u32::try_from(n)
            .map(Value::U32)
            .map_err(|_| unrepresentable(span, "u32")),
        _ => Err(unrepresentable(span, "an integer")),
    }
}

/// A scalar converted automatically: an abstract value to the concrete
/// scalar its use asks for.
fn convert_scalar(value: Value, target: Scalar, span: Span) -> Outcome<Value> {
    match number(value) {
        Number::Int(n) if target.is_integer() => int_of(n, target, span),
        Number::Int(n) if target.is_float() => int_float(n, target, span),
        Number::Float(x) if target.is_float() => float_of(x, target, span),
        _ => Err(unrepresentable(span, target.name())),
    }
}

/// A scalar converted explicitly by its type's constructor: an integer
/// reinterpreted between `i32` and `u32`, a float truncated toward zero and
/// held to the integer's range, a bool as zero or one, and back.
pub(crate) fn construct_scalar(value: Value, target: Scalar, span: Span) -> Outcome<Value> {
    match (value, target) {
        (Value::I32(n), Scalar::U32) => Ok(Value::U32(n.cast_unsigned())),
        (Value::U32(n), Scalar::I32) => Ok(Value::I32(n.cast_signed())),
        (Value::Bool(b), Scalar::Bool) => Ok(Value::Bool(b)),
        (other, Scalar::Bool) => Ok(Value::Bool(match number(other) {
            Number::Int(n) => n != 0,
            Number::Float(x) => x != 0.0,
        })),
        (Value::Bool(b), target) if target.is_float() => {
            float_of(if b { 1.0 } else { 0.0 }, target, span)
        }
        (Value::Bool(b), target) => int_of(i128::from(b), target, span),
        (other, target) if target.is_integer() && target != Scalar::AbstractInt => {
            match number(other) {
                Number::Int(n) => int_of(n, target, span),
                Number::Float(x) => Ok(saturate(x, target)),
            }
        }
        (other, target) => convert_scalar(other, target, span),
    }
}

/// `x` truncated toward zero and held to the range of `target`, `i32` or
/// `u32`: the nearest representable in the source float, as the
/// specification has it.
fn saturate(x: f64, target: Scalar) -> Value {
    let truncated = mathf::trunc(x);
    if target == Scalar::U32 {
        // The largest float below 2^32 a source of f32 or wider holds.
        let high = 4_294_967_040.0;
        let held = mathf::clamp(truncated, 0.0, high);
        // In range by the clamp.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        Value::U32(held as u32)
    } else {
        let high = 2_147_483_520.0;
        let held = mathf::clamp(truncated, -2_147_483_648.0, high);
        #[allow(clippy::cast_possible_truncation)]
        Value::I32(held as i32)
    }
}

/// `op` applied to a scalar.
fn unary_scalar(op: UnaryOp, value: Value, span: Span) -> Outcome<Value> {
    Ok(match (op, value) {
        (UnaryOp::Negate, Value::AbstractInt(n)) => Value::AbstractInt(
            n.checked_neg()
                .ok_or_else(|| unrepresentable(span, "abstract-int"))?,
        ),
        (UnaryOp::Negate, Value::I32(n)) => Value::I32(n.wrapping_neg()),
        (UnaryOp::Negate, Value::AbstractFloat(x)) => Value::AbstractFloat(-x),
        (UnaryOp::Negate, Value::F32(x)) => Value::F32(-x),
        (UnaryOp::Negate, Value::F16(bits)) => Value::F16(bits ^ 0x8000),
        (UnaryOp::Not, Value::Bool(b)) => Value::Bool(!b),
        (UnaryOp::Complement, Value::AbstractInt(n)) => Value::AbstractInt(!n),
        (UnaryOp::Complement, Value::I32(n)) => Value::I32(!n),
        (UnaryOp::Complement, Value::U32(n)) => Value::U32(!n),
        _ => return Err(unrepresentable(span, "its type")),
    })
}

/// The integer type of a scalar value, its bounds and width.
fn bounds(value: Value) -> Option<(Scalar, i128, i128, u32)> {
    match value {
        Value::AbstractInt(_) => Some((
            Scalar::AbstractInt,
            i128::from(i64::MIN),
            i128::from(i64::MAX),
            64,
        )),
        Value::I32(_) => Some((Scalar::I32, i128::from(i32::MIN), i128::from(i32::MAX), 32)),
        Value::U32(_) => Some((Scalar::U32, 0, i128::from(u32::MAX), 32)),
        _ => None,
    }
}

/// `op` applied to two scalars of one type (a shift's amount aside), with
/// every error the specification names for constant operands.
fn binary_scalar(op: BinaryOp, left: Value, right: Value, span: Span) -> Outcome<Value> {
    use BinaryOp::{
        And, Equal, Greater, GreaterEqual, Less, LessEqual, LogicalAnd, LogicalOr, NotEqual, Or,
        Xor,
    };
    match op {
        LogicalAnd | LogicalOr | And | Or | Xor
            if matches!((left, right), (Value::Bool(_), Value::Bool(_))) =>
        {
            let (Value::Bool(first), Value::Bool(second)) = (left, right) else {
                return Err(unrepresentable(span, "bool"));
            };
            return Ok(Value::Bool(match op {
                LogicalAnd | And => first && second,
                LogicalOr | Or => first || second,
                _ => first != second,
            }));
        }
        Less | LessEqual | Greater | GreaterEqual | Equal | NotEqual => {
            let ordering = match (number(left), number(right)) {
                (Number::Int(first), Number::Int(second)) => Some(first.cmp(&second)),
                (Number::Float(first), Number::Float(second)) => first.partial_cmp(&second),
                _ => None,
            };
            let holds = match op {
                Less => ordering == Some(core::cmp::Ordering::Less),
                LessEqual => ordering.is_some_and(|o| o != core::cmp::Ordering::Greater),
                Greater => ordering == Some(core::cmp::Ordering::Greater),
                GreaterEqual => ordering.is_some_and(|o| o != core::cmp::Ordering::Less),
                Equal => ordering == Some(core::cmp::Ordering::Equal),
                _ => ordering != Some(core::cmp::Ordering::Equal),
            };
            return Ok(Value::Bool(holds));
        }
        _ => {}
    }
    if let Some((scalar, low, high, width)) = bounds(left) {
        return integer_binary(op, (left, right), (scalar, low, high, width), span);
    }
    float_binary(op, left, right, span)
}

/// An integer operation: `i32` and `u32` arithmetic wrapping, abstract
/// overflow a stop, division and remainder by zero and of the most negative
/// by −1 stops, shifts held to their width.
fn integer_binary(
    op: BinaryOp,
    (a, b): (Value, Value),
    (scalar, low, high, width): (Scalar, i128, i128, u32),
    span: Span,
) -> Outcome<Value> {
    use BinaryOp::{
        Add, And, Divide, Multiply, Or, Remainder, ShiftLeft, ShiftRight, Subtract, Xor,
    };
    let (Number::Int(x), Number::Int(y)) = (number(a), number(b)) else {
        return Err(unrepresentable(span, scalar.name()));
    };
    let wraps = scalar != Scalar::AbstractInt;
    let fit = |n: i128| -> Outcome<Value> {
        if wraps {
            let modulus = 1i128 << width;
            let mut wrapped = n.rem_euclid(modulus);
            if scalar == Scalar::I32 && wrapped > high {
                wrapped -= modulus;
            }
            int_of(wrapped, scalar, span)
        } else if n < low || n > high {
            Err(unrepresentable(span, scalar.name()))
        } else {
            int_of(n, scalar, span)
        }
    };
    match op {
        Add => fit(x + y),
        Subtract => fit(x - y),
        Multiply => fit(x * y),
        Divide | Remainder => {
            if y == 0 {
                return Err(error(Code::Evaluation, span, |m| {
                    m.write_str("an integer division or remainder by zero")
                }));
            }
            if x == low && y == -1 && scalar != Scalar::U32 {
                return Err(error(Code::Evaluation, span, |m| {
                    m.write_str("the most negative value divided by -1 overflows")
                }));
            }
            // Truncating division, the remainder taking the dividend's sign.
            if op == Divide {
                fit(x / y)
            } else {
                fit(x % y)
            }
        }
        And => fit(x & y),
        Or => fit(x | y),
        Xor => fit(x ^ y),
        ShiftLeft | ShiftRight => shift(op, (x, y), (scalar, width), span),
        _ => Err(unrepresentable(span, scalar.name())),
    }
}

/// A shift of `x` by `amount`: concrete shifts held under their width, a
/// left shift that would lose a significant bit a stop.
fn shift(
    op: BinaryOp,
    (x, amount): (i128, i128),
    (scalar, width): (Scalar, u32),
    span: Span,
) -> Outcome<Value> {
    if scalar == Scalar::AbstractInt && amount >= i128::from(width) {
        // An abstract shift is not taken modulo its width: past it, only a
        // zero shifts left unchanged, and a right shift leaves the sign.
        return match op {
            BinaryOp::ShiftRight => int_of(if x < 0 { -1 } else { 0 }, scalar, span),
            _ if x == 0 => int_of(0, scalar, span),
            _ => Err(error(Code::Evaluation, span, |m| {
                m.write_str("a left shift of constants overflows")
            })),
        };
    }
    if amount < 0 || amount >= i128::from(width) {
        return Err(error(Code::Evaluation, span, |m| {
            write!(m, "a shift of {amount} is not less than the width, {width}")
        }));
    }
    let amount = u32::try_from(amount).unwrap_or(0);
    let signed = scalar != Scalar::U32;
    if op == BinaryOp::ShiftRight {
        // Arithmetic for a signed value, logical for an unsigned one.
        return int_of(x >> amount, scalar, span);
    }
    // The bits shifted out, and for a signed value the new sign bit, must
    // all equal the old sign: nothing significant is lost.
    let kept = width - amount;
    let overflow = if signed {
        let top = x >> (kept - 1);
        top != 0 && top != -1
    } else {
        x >> kept != 0
    };
    if overflow {
        return Err(error(Code::Evaluation, span, |m| {
            m.write_str("a left shift of constants overflows")
        }));
    }
    let shifted = x << amount;
    if signed {
        int_of(shifted, scalar, span)
    } else {
        int_of(shifted & ((1i128 << width) - 1), scalar, span)
    }
}

/// A float operation, its result rounded to its type and checked finite.
fn float_binary(op: BinaryOp, a: Value, b: Value, span: Span) -> Outcome<Value> {
    use BinaryOp::{Add, Divide, Multiply, Remainder, Subtract};
    let target = match a {
        Value::AbstractFloat(_) => Scalar::AbstractFloat,
        Value::F32(_) => Scalar::F32,
        Value::F16(_) => Scalar::F16,
        _ => return Err(unrepresentable(span, "a float")),
    };
    if let (Value::F32(x), Value::F32(y)) = (a, b) {
        // f32 arithmetic is IEEE's own, correctly rounded.
        let result = match op {
            Add => x + y,
            Subtract => x - y,
            Multiply => x * y,
            Divide => x / y,
            Remainder => x % y,
            _ => return Err(unrepresentable(span, "f32")),
        };
        return float_of(f64::from(result), Scalar::F32, span);
    }
    let (Number::Float(x), Number::Float(y)) = (number(a), number(b)) else {
        return Err(unrepresentable(span, target.name()));
    };
    let result = match op {
        Add => x + y,
        Subtract => x - y,
        Multiply => x * y,
        Divide => x / y,
        Remainder => x % y,
        _ => return Err(unrepresentable(span, target.name())),
    };
    float_of(result, target, span)
}

#[cfg(test)]
#[path = "eval_tests.rs"]
mod tests;
