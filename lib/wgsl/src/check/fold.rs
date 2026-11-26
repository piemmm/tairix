//! Constant evaluation of the `@const` built-ins: each computed in binary64
//! from its arguments' exact values and rounded once to its type, every
//! result outside a type's finite range and every argument outside a
//! function's domain a stop, as the specification's finite math rule has
//! it for const- and override-expressions.

use alloc::vec::Vec;
use core::fmt::Write as _;

use tairix_util::mathf;

use super::{Checker, Stage};
use crate::diag::{error, reserve, Code, Outcome, Span, Stop};
use crate::eval::{float_of, half, int_of, not_finite, number, Number, Value, ValueId, Values};
use crate::literal::pow2;
use crate::predeclared::Builtin;
use crate::syntax::ExprId;
use crate::types::{Scalar, Ty, TyId};

impl Checker<'_> {
    /// The value of the constant built-in call `id`, its arguments
    /// evaluated and converted already.
    #[expect(clippy::too_many_lines, reason = "one arm per built-in family")]
    pub(crate) fn evaluate_builtin(
        &mut self,
        id: ExprId,
        builtin: Builtin,
        args: &[ExprId],
        span: Span,
    ) -> Outcome<ValueId> {
        use Builtin as B;
        let result = self.info(id).ty;
        let mut values: Vec<(ValueId, TyId)> = Vec::new();
        reserve(&mut values, args.len())?;
        for &arg in args {
            values.push((self.value_or_stop(arg)?, self.info(arg).converted));
        }
        let scalar = self.types.scalar_of(result);
        match builtin {
            B::Acos
            | B::Acosh
            | B::Asin
            | B::Asinh
            | B::Atan
            | B::Atanh
            | B::Ceil
            | B::Cos
            | B::Cosh
            | B::Degrees
            | B::Exp
            | B::Exp2
            | B::Floor
            | B::Fract
            | B::InverseSqrt
            | B::Log
            | B::Log2
            | B::Radians
            | B::Round
            | B::Saturate
            | B::Sin
            | B::Sinh
            | B::Sqrt
            | B::Tan
            | B::Tanh
            | B::Trunc
            | B::Atan2
            | B::Pow
            | B::Step
            | B::Fma
            | B::Smoothstep
            | B::Mix
            | B::QuantizeToF16 => {
                let scalar = scalar.ok_or(Stop::Internal)?;
                self.values
                    .lift(&mut self.types, &values, (result, span), &mut |xs| {
                        let x = |i: usize| xs.get(i).map_or(Ok(0.0), |v| float(*v));
                        let y = float_function(builtin, [x(0)?, x(1)?, x(2)?]);
                        if builtin == B::QuantizeToF16 {
                            let bits = half(y);
                            let back = f64::from(mathf::from_half(bits));
                            return float_of(back, scalar, span);
                        }
                        float_of(y, scalar, span)
                    })
            }
            B::Abs | B::Max | B::Min | B::Clamp | B::Sign => {
                let scalar = scalar.ok_or(Stop::Internal)?;
                self.values
                    .lift(&mut self.types, &values, (result, span), &mut |xs| {
                        numeric_function(builtin, xs, scalar, span)
                    })
            }
            B::CountLeadingZeros
            | B::CountOneBits
            | B::CountTrailingZeros
            | B::FirstLeadingBit
            | B::FirstTrailingBit
            | B::ReverseBits
            | B::ExtractBits
            | B::InsertBits => {
                self.values
                    .lift(&mut self.types, &values, (result, span), &mut |xs| {
                        bit_function(builtin, xs)
                    })
            }
            B::Ldexp => {
                let scalar = scalar.ok_or(Stop::Internal)?;
                self.values
                    .lift(&mut self.types, &values, (result, span), &mut |xs| {
                        let (Some(x), Some(Number::Int(e))) =
                            (xs.first(), xs.get(1).map(|v| number(*v)))
                        else {
                            return Err(Stop::Internal);
                        };
                        float_of(scale_by_power(float(*x)?, e), scalar, span)
                    })
            }
            B::Select => self.evaluate_select(&values, result, span),
            B::All | B::Any => {
                let mut lanes = Vec::new();
                self.values
                    .scalars(&mut self.types, first(&values)?, &mut lanes)?;
                let holds = |v: &Value| matches!(v, Value::Bool(true));
                let value = if builtin == B::All {
                    lanes.iter().all(holds)
                } else {
                    lanes.iter().any(holds)
                };
                self.values.add(Value::Bool(value))
            }
            B::Dot
            | B::Length
            | B::Distance
            | B::Cross
            | B::Normalize
            | B::Reflect
            | B::Refract
            | B::FaceForward => self.evaluate_geometric(builtin, &values, result, span),
            B::Determinant | B::Transpose => {
                self.evaluate_matrix(builtin, first(&values)?, result, span)
            }
            B::Frexp | B::Modf => self.evaluate_split(builtin, first(&values)?, result, span),
            B::Bitcast => self.evaluate_bitcast(first(&values)?, result, span),
            B::Dot4U8Packed | B::Dot4I8Packed => {
                let (Some(&(a, _)), Some(&(b, _))) = (values.first(), values.get(1)) else {
                    return Err(Stop::Internal);
                };
                let (Value::U32(a), Value::U32(b)) = (self.values.get(a), self.values.get(b))
                else {
                    return Err(Stop::Internal);
                };
                self.values.charge(4, span)?;
                let (a, b) = (a.to_le_bytes(), b.to_le_bytes());
                let value = if builtin == B::Dot4U8Packed {
                    Value::U32(
                        a.iter()
                            .zip(&b)
                            .map(|(&x, &y)| u32::from(x) * u32::from(y))
                            .sum(),
                    )
                } else {
                    let signed = |byte: u8| i32::from(byte.cast_signed());
                    Value::I32(a.iter().zip(&b).map(|(&x, &y)| signed(x) * signed(y)).sum())
                };
                self.values.add(value)
            }
            B::Pack4x8Snorm
            | B::Pack4x8Unorm
            | B::Pack4xI8
            | B::Pack4xU8
            | B::Pack4xI8Clamp
            | B::Pack4xU8Clamp
            | B::Pack2x16Snorm
            | B::Pack2x16Unorm
            | B::Pack2x16Float => self.evaluate_pack(builtin, first(&values)?, span),
            B::Unpack4x8Snorm
            | B::Unpack4x8Unorm
            | B::Unpack4xI8
            | B::Unpack4xU8
            | B::Unpack2x16Snorm
            | B::Unpack2x16Unorm
            | B::Unpack2x16Float => self.evaluate_unpack(builtin, first(&values)?, result, span),
            _ => Err(Stop::Internal),
        }
    }

    /// `select(f, t, cond)`: the whole of one for a scalar condition,
    /// component by component for a vector one.
    fn evaluate_select(
        &mut self,
        values: &[(ValueId, TyId)],
        result: TyId,
        span: Span,
    ) -> Outcome<ValueId> {
        let [f, t, (cond, cond_ty)] = values else {
            return Err(Stop::Internal);
        };
        if let Some(Value::Bool(pick)) = self.values.scalar(*cond) {
            self.values.charge(1, span)?;
            return Ok(if pick { t.0 } else { f.0 });
        }
        self.values.lift(
            &mut self.types,
            &[*f, *t, (*cond, *cond_ty)],
            (result, span),
            &mut |xs| match xs {
                [f, t, Value::Bool(pick)] => Ok(if *pick { *t } else { *f }),
                _ => Err(Stop::Internal),
            },
        )
    }

    /// The vector functions, computed over the components in binary64 and
    /// each result rounded once.
    fn evaluate_geometric(
        &mut self,
        builtin: Builtin,
        values: &[(ValueId, TyId)],
        result: TyId,
        span: Span,
    ) -> Outcome<ValueId> {
        use Builtin as B;
        let mut lanes: [Vec<Value>; 3] = [Vec::new(), Vec::new(), Vec::new()];
        for (slot, &value) in lanes.iter_mut().zip(values) {
            self.values.scalars(&mut self.types, value, slot)?;
        }
        let [first, second, third] = &lanes;
        self.values.charge((first.len() * 4) as u64, span)?;
        let scalar = self.types.scalar_of(result).ok_or(Stop::Internal)?;
        if builtin == B::Dot && scalar.is_integer() {
            return self.integer_dot(first, second, scalar, span);
        }
        let xs: Vec<f64> = floats(first)?;
        let ys: Vec<f64> = floats(second)?;
        let dot = |p: &[f64], q: &[f64]| p.iter().zip(q).map(|(x, y)| x * y).sum::<f64>();
        let out: Vec<f64> = match builtin {
            B::Dot => one(dot(&xs, &ys))?,
            B::Length => one(length(&xs))?,
            B::Distance => {
                let difference: Vec<f64> = collect(xs.iter().zip(&ys).map(|(x, y)| x - y))?;
                one(length(&difference))?
            }
            B::Cross => {
                let [x0, x1, x2] = xs[..] else {
                    return Err(Stop::Internal);
                };
                let [y0, y1, y2] = ys[..] else {
                    return Err(Stop::Internal);
                };
                collect([x1 * y2 - x2 * y1, x2 * y0 - x0 * y2, x0 * y1 - x1 * y0].into_iter())?
            }
            B::Normalize => {
                let norm = length(&xs);
                collect(xs.iter().map(|x| x / norm))?
            }
            B::Reflect => {
                let along = dot(&ys, &xs);
                collect(xs.iter().zip(&ys).map(|(x, y)| x - 2.0 * along * y))?
            }
            B::Refract => {
                let eta = float(*third.first().ok_or(Stop::Internal)?)?;
                let cosine = dot(&ys, &xs);
                let radicand = 1.0 - eta * eta * (1.0 - cosine * cosine);
                if radicand < 0.0 {
                    collect(xs.iter().map(|_| 0.0))?
                } else {
                    let root = mathf::sqrt(radicand);
                    collect(
                        xs.iter()
                            .zip(&ys)
                            .map(|(x, y)| eta * x - (eta * cosine + root) * y),
                    )?
                }
            }
            B::FaceForward => {
                let zs = floats(third)?;
                let flip = dot(&ys, &zs) < 0.0;
                collect(xs.iter().map(|&x| if flip { x } else { -x }))?
            }
            _ => return Err(Stop::Internal),
        };
        self.float_result(&out, result, scalar, span)
    }

    /// `dot` of integer vectors: `i32` and `u32` wrapping, an abstract
    /// integer's overflow a stop.
    fn integer_dot(
        &mut self,
        a: &[Value],
        b: &[Value],
        scalar: Scalar,
        span: Span,
    ) -> Outcome<ValueId> {
        let mut total: i128 = 0;
        for (x, y) in a.iter().zip(b) {
            let (Number::Int(x), Number::Int(y)) = (number(*x), number(*y)) else {
                return Err(Stop::Internal);
            };
            total = total.checked_add(x * y).ok_or_else(|| not_finite(span))?;
        }
        let value = match scalar {
            Scalar::I32 => Value::I32(wrap(total).cast_signed()),
            Scalar::U32 => Value::U32(wrap(total)),
            _ => int_of(total, scalar, span)?,
        };
        self.values.add(value)
    }

    /// `out`, the components of a value of `result`, each rounded once to
    /// `scalar`.
    fn float_result(
        &mut self,
        out: &[f64],
        result: TyId,
        scalar: Scalar,
        span: Span,
    ) -> Outcome<ValueId> {
        if let [x] = out {
            if matches!(self.types.get(result), Ty::Scalar(_)) {
                let value = float_of(*x, scalar, span)?;
                return self.values.add(value);
            }
        }
        let mut parts = Vec::new();
        reserve(&mut parts, out.len())?;
        for &x in out {
            let value = float_of(x, scalar, span)?;
            parts.push(self.values.add(value)?);
        }
        self.values.composite(result, &parts)
    }

    /// `determinant` and `transpose`.
    fn evaluate_matrix(
        &mut self,
        builtin: Builtin,
        (value, ty): (ValueId, TyId),
        result: TyId,
        span: Span,
    ) -> Outcome<ValueId> {
        let Ty::Matrix {
            columns,
            rows,
            scalar,
        } = self.types.get(ty)
        else {
            return Err(Stop::Internal);
        };
        let (c, r) = (usize::from(columns), usize::from(rows));
        let mut cells: Vec<f64> = Vec::new();
        reserve(&mut cells, c * r)?;
        let mut column_parts = Vec::new();
        self.values
            .components(&mut self.types, (value, ty), &mut column_parts)?;
        let column_ty = Values::component_type(&mut self.types, ty, 0)?;
        let mut lanes = Vec::new();
        for &column in &column_parts {
            self.values
                .scalars(&mut self.types, (column, column_ty), &mut lanes)?;
            for lane in &lanes {
                cells.push(float(*lane)?);
            }
        }
        self.values.charge((c * r) as u64, span)?;
        // Column-major: cell (column, row) at column * rows + row.
        let at = |column: usize, row: usize| cells[column * r + row];
        if builtin == Builtin::Determinant {
            let det = determinant(&cells, c);
            let value = float_of(det, scalar, span)?;
            return self.values.add(value);
        }
        let out: Vec<f64> = collect(
            (0..r)
                .flat_map(|row| (0..c).map(move |column| (row, column)))
                .map(|(row, column)| at(column, row)),
        )?;
        let Ty::Matrix { rows: out_rows, .. } = self.types.get(result) else {
            return Err(Stop::Internal);
        };
        let out_column = self.intern(Ty::Vector(out_rows, scalar))?;
        let mut columns_out = Vec::new();
        let mut cells_out = Vec::new();
        for chunk in out.chunks(usize::from(out_rows)) {
            cells_out.clear();
            for &x in chunk {
                let value = float_of(x, scalar, span)?;
                cells_out.push(self.values.add(value)?);
            }
            columns_out.push(self.values.composite(out_column, &cells_out)?);
        }
        self.values.composite(result, &columns_out)
    }

    /// `frexp` and `modf`: their result structures of two parts.
    fn evaluate_split(
        &mut self,
        builtin: Builtin,
        (value, ty): (ValueId, TyId),
        result: TyId,
        span: Span,
    ) -> Outcome<ValueId> {
        let Ty::Struct(structure) = self.types.get(result) else {
            return Err(Stop::Internal);
        };
        let member = |this: &Self, index: usize| {
            this.types
                .members_of(structure)
                .get(index)
                .map(|m| m.ty)
                .ok_or(Stop::Internal)
        };
        let (first_ty, second_ty) = (member(self, 0)?, member(self, 1)?);
        let (first_scalar, second_scalar) = (
            self.types.scalar_of(first_ty).ok_or(Stop::Internal)?,
            self.types.scalar_of(second_ty).ok_or(Stop::Internal)?,
        );
        let mut lanes = Vec::new();
        self.values
            .scalars(&mut self.types, (value, ty), &mut lanes)?;
        self.values.charge(lanes.len() as u64 * 2, span)?;
        let mut firsts = Vec::new();
        let mut seconds = Vec::new();
        reserve(&mut firsts, lanes.len())?;
        reserve(&mut seconds, lanes.len())?;
        for lane in &lanes {
            let x = float(*lane)?;
            let (first, second) = if builtin == Builtin::Frexp {
                let (fraction, exponent) = frexp(x);
                (
                    float_of(fraction, first_scalar, span)?,
                    int_of(i128::from(exponent), second_scalar, span)?,
                )
            } else {
                let whole = mathf::trunc(x);
                (
                    float_of(x - whole, first_scalar, span)?,
                    float_of(whole, second_scalar, span)?,
                )
            };
            firsts.push(self.values.add(first)?);
            seconds.push(self.values.add(second)?);
        }
        let first = self.gather(&firsts, first_ty)?;
        let second = self.gather(&seconds, second_ty)?;
        self.values.composite(result, &[first, second])
    }

    /// The scalars `parts` as a value of `ty`: the one itself for a scalar.
    fn gather(&mut self, parts: &[ValueId], ty: TyId) -> Outcome<ValueId> {
        match (self.types.get(ty), parts) {
            (Ty::Scalar(_), [one]) => Ok(*one),
            _ => self.values.composite(ty, parts),
        }
    }

    /// `bitcast`: the source's bits, little-endian component after
    /// component, read back as the result's components.
    fn evaluate_bitcast(
        &mut self,
        (value, ty): (ValueId, TyId),
        result: TyId,
        span: Span,
    ) -> Outcome<ValueId> {
        let mut lanes = Vec::new();
        self.values
            .scalars(&mut self.types, (value, ty), &mut lanes)?;
        let mut bits: u128 = 0;
        let mut at = 0u32;
        for lane in &lanes {
            let (word, width) = match *lane {
                Value::I32(n) => (u128::from(n.cast_unsigned()), 32),
                Value::U32(n) => (u128::from(n), 32),
                Value::F32(x) => (u128::from(x.to_bits()), 32),
                Value::F16(b) => (u128::from(b), 16),
                _ => return Err(Stop::Internal),
            };
            bits |= word << at;
            at += width;
        }
        let scalar = self.types.scalar_of(result).ok_or(Stop::Internal)?;
        let width = scalar.bytes() * 8;
        let count = at / width;
        self.values.charge(u64::from(count), span)?;
        let mut parts = Vec::new();
        reserve(&mut parts, count as usize)?;
        for index in 0..count {
            let word = (bits >> (index * width)) & ((1u128 << width) - 1);
            let word = u32::try_from(word).map_err(|_| Stop::Internal)?;
            let value = match scalar {
                Scalar::I32 => Value::I32(word.cast_signed()),
                Scalar::U32 => Value::U32(word),
                Scalar::F32 => {
                    let x = f32::from_bits(word);
                    if !x.is_finite() {
                        return Err(not_finite(span));
                    }
                    Value::F32(x)
                }
                Scalar::F16 => {
                    let bits = u16::try_from(word).map_err(|_| Stop::Internal)?;
                    if !mathf::from_half(bits).is_finite() {
                        return Err(not_finite(span));
                    }
                    Value::F16(bits)
                }
                _ => return Err(Stop::Internal),
            };
            parts.push(self.values.add(value)?);
        }
        match (self.types.get(result), &parts[..]) {
            (Ty::Scalar(_), [one]) => Ok(*one),
            _ => self.values.composite(result, &parts),
        }
    }

    /// The packing built-ins: a vector's components packed into a `u32`.
    fn evaluate_pack(
        &mut self,
        builtin: Builtin,
        value: (ValueId, TyId),
        span: Span,
    ) -> Outcome<ValueId> {
        use Builtin as B;
        let mut lanes = Vec::new();
        self.values.scalars(&mut self.types, value, &mut lanes)?;
        self.values.charge(lanes.len() as u64, span)?;
        let width = 32 / u32::try_from(lanes.len()).map_err(|_| Stop::Internal)?;
        let mask = (1u64 << width) - 1;
        let mut word: u64 = 0;
        for (index, lane) in lanes.iter().enumerate() {
            let index = u32::try_from(index).map_err(|_| Stop::Internal)?;
            let field: i64 = match builtin {
                B::Pack4x8Snorm | B::Pack2x16Snorm => {
                    let scale = if width == 8 { 127.0 } else { 32767.0 };
                    to_integer(mathf::floor(
                        0.5 + scale * mathf::clamp(float(*lane)?, -1.0, 1.0),
                    ))
                }
                B::Pack4x8Unorm | B::Pack2x16Unorm => {
                    let scale = if width == 8 { 255.0 } else { 65535.0 };
                    to_integer(mathf::floor(
                        0.5 + scale * mathf::clamp(float(*lane)?, 0.0, 1.0),
                    ))
                }
                B::Pack2x16Float => {
                    let bits = half(float(*lane)?);
                    if !mathf::from_half(bits).is_finite() {
                        return Err(not_finite(span));
                    }
                    i64::from(bits)
                }
                B::Pack4xI8Clamp => integer(*lane)?.clamp(-128, 127),
                B::Pack4xU8Clamp => integer(*lane)?.clamp(0, 255),
                _ => integer(*lane)?,
            };
            word |= (field.cast_unsigned() & mask) << (index * width);
        }
        let word = u32::try_from(word).map_err(|_| Stop::Internal)?;
        self.values.add(Value::U32(word))
    }

    /// The unpacking built-ins: a `u32`'s fields as a vector.
    fn evaluate_unpack(
        &mut self,
        builtin: Builtin,
        (value, _): (ValueId, TyId),
        result: TyId,
        span: Span,
    ) -> Outcome<ValueId> {
        use Builtin as B;
        let Value::U32(word) = self.values.get(value) else {
            return Err(Stop::Internal);
        };
        let Ty::Vector(n, scalar) = self.types.get(result) else {
            return Err(Stop::Internal);
        };
        let width = 32 / u32::from(n);
        self.values.charge(u64::from(n), span)?;
        let mut parts = Vec::new();
        reserve(&mut parts, usize::from(n))?;
        for index in 0..u32::from(n) {
            let field = (word >> (index * width)) & ((1u32 << width) - 1);
            let signed = |field: u32| -> i32 {
                let shift = 32 - width;
                (field << shift).cast_signed() >> shift
            };
            let value = match builtin {
                B::Unpack4x8Snorm | B::Unpack2x16Snorm => {
                    let scale = if width == 8 { 127.0 } else { 32767.0 };
                    float_of((f64::from(signed(field)) / scale).max(-1.0), scalar, span)?
                }
                B::Unpack4x8Unorm | B::Unpack2x16Unorm => {
                    let scale = if width == 8 { 255.0 } else { 65535.0 };
                    float_of(f64::from(field) / scale, scalar, span)?
                }
                B::Unpack2x16Float => {
                    let bits = u16::try_from(field).map_err(|_| Stop::Internal)?;
                    float_of(f64::from(mathf::from_half(bits)), scalar, span)?
                }
                B::Unpack4xI8 => Value::I32(signed(field)),
                _ => Value::U32(field),
            };
            parts.push(self.values.add(value)?);
        }
        self.values.composite(result, &parts)
    }

    /// Refuse a built-in call whose arguments a rule binds together, where
    /// they are known by `upto` and break it: `clamp`'s `low` above its
    /// `high`, a bit field running past 32 bits, `ldexp`'s exponent past
    /// its type's bias, `smoothstep`'s edges equal.
    pub(crate) fn argument_rules(
        &mut self,
        builtin: Builtin,
        args: &[ExprId],
        upto: Stage,
    ) -> Outcome<()> {
        use Builtin as B;
        let bound: &[usize] = match builtin {
            B::Clamp | B::ExtractBits => &[1, 2],
            B::InsertBits => &[2, 3],
            B::Ldexp => &[1],
            B::Smoothstep => &[0, 1],
            _ => return Ok(()),
        };
        let mut known = Vec::new();
        reserve(&mut known, bound.len())?;
        for &at in bound {
            let arg = *args.get(at).ok_or(Stop::Internal)?;
            if self.info(arg).stage > upto {
                return Ok(());
            }
            if upto == Stage::Const {
                self.force(arg)?;
            } else {
                self.evaluate_range(arg, upto)?;
            }
            let mut lanes = Vec::new();
            let value = (self.value_or_stop(arg)?, self.info(arg).converted);
            self.values.scalars(&mut self.types, value, &mut lanes)?;
            known.push(lanes);
        }
        let span = self.span_of(*args.get(bound[0]).ok_or(Stop::Internal)?);
        let broken = match builtin {
            B::Clamp => known[0]
                .iter()
                .zip(&known[1])
                .any(|(low, high)| compare(*low, *high) == Some(core::cmp::Ordering::Greater)),
            B::ExtractBits | B::InsertBits => known[0].iter().zip(&known[1]).any(|(offset, count)| {
                matches!((number(*offset), number(*count)), (Number::Int(o), Number::Int(c)) if o + c > 32)
            }),
            B::Ldexp => {
                let first = self.info(args[0]).converted;
                let bias = match self.types.scalar_of(first) {
                    Some(Scalar::F16) => 15,
                    Some(Scalar::F32) => 127,
                    _ => 1023,
                };
                known[0]
                    .iter()
                    .any(|e| matches!(number(*e), Number::Int(n) if n > bias + 1))
            }
            _ => known[0]
                .iter()
                .zip(&known[1])
                .any(|(a, b)| compare(*a, *b) == Some(core::cmp::Ordering::Equal)),
        };
        if broken {
            return Err(error(Code::Evaluation, span, |m| match builtin {
                B::Clamp => m.write_str("clamp's low bound is above its high bound"),
                B::ExtractBits | B::InsertBits => {
                    m.write_str("the bit field's offset and count run past 32 bits")
                }
                B::Ldexp => m.write_str("the exponent is past its type's range"),
                _ => write!(m, "smoothstep's edges are equal"),
            }));
        }
        Ok(())
    }
}

/// A float argument's value.
fn float(value: Value) -> Outcome<f64> {
    match number(value) {
        Number::Float(x) => Ok(x),
        Number::Int(_) => Err(Stop::Internal),
    }
}

fn floats(values: &[Value]) -> Outcome<Vec<f64>> {
    let mut out = Vec::new();
    reserve(&mut out, values.len())?;
    for &value in values {
        out.push(float(value)?);
    }
    Ok(out)
}

fn first(values: &[(ValueId, TyId)]) -> Outcome<(ValueId, TyId)> {
    values.first().copied().ok_or(Stop::Internal)
}

fn integer(value: Value) -> Outcome<i64> {
    match number(value) {
        Number::Int(n) => i64::try_from(n).map_err(|_| Stop::Internal),
        Number::Float(_) => Err(Stop::Internal),
    }
}

fn collect(items: impl Iterator<Item = f64>) -> Outcome<Vec<f64>> {
    let mut out = Vec::new();
    for item in items {
        out.try_reserve(1).map_err(|_| Stop::OutOfMemory)?;
        out.push(item);
    }
    Ok(out)
}

fn one(x: f64) -> Outcome<Vec<f64>> {
    collect(core::iter::once(x))
}

/// The low 32 bits of `n`.
fn wrap(n: i128) -> u32 {
    let [b0, b1, b2, b3, ..] = n.to_le_bytes();
    u32::from_le_bytes([b0, b1, b2, b3])
}

/// A float of integral value, truncated to an integer; zero where it does
/// not fit, which no packed field reaches.
fn to_integer(x: f64) -> i64 {
    #[allow(
        clippy::cast_possible_truncation,
        reason = "the packed fields are clamped first"
    )]
    let n = x as i64;
    n
}

/// Two scalars ordered as numbers.
fn compare(a: Value, b: Value) -> Option<core::cmp::Ordering> {
    match (number(a), number(b)) {
        (Number::Int(x), Number::Int(y)) => Some(x.cmp(&y)),
        (Number::Float(x), Number::Float(y)) => x.partial_cmp(&y),
        _ => None,
    }
}

/// A vector's length, scaled by its largest component so its squares
/// neither overflow nor underflow.
fn length(xs: &[f64]) -> f64 {
    let largest = xs.iter().fold(0.0f64, |m, x| m.max(mathf::fabs(*x)));
    if largest == 0.0 || !largest.is_finite() {
        return largest;
    }
    let sum: f64 = xs.iter().map(|x| (x / largest) * (x / largest)).sum();
    largest * mathf::sqrt(sum)
}

/// The determinant of the `n`×`n` column-major matrix `cells`.
fn determinant(cells: &[f64], n: usize) -> f64 {
    let at = |column: usize, row: usize| cells.get(column * n + row).copied().unwrap_or(0.0);
    let det2 = |a: f64, b: f64, c: f64, d: f64| a * d - b * c;
    let det3 = |cols: [usize; 3], rows: [usize; 3]| {
        let m = |i: usize, j: usize| at(cols[i], rows[j]);
        m(0, 0) * det2(m(1, 1), m(1, 2), m(2, 1), m(2, 2))
            - m(1, 0) * det2(m(0, 1), m(0, 2), m(2, 1), m(2, 2))
            + m(2, 0) * det2(m(0, 1), m(0, 2), m(1, 1), m(1, 2))
    };
    match n {
        2 => det2(at(0, 0), at(0, 1), at(1, 0), at(1, 1)),
        3 => det3([0, 1, 2], [0, 1, 2]),
        _ => {
            // Expanded along the first row.
            let minor = |skip: usize| {
                let mut cols = [0usize; 3];
                let mut k = 0;
                for c in 0..4 {
                    if c != skip {
                        cols[k] = c;
                        k += 1;
                    }
                }
                det3(cols, [1, 2, 3])
            };
            at(0, 0) * minor(0) - at(1, 0) * minor(1) + at(2, 0) * minor(2) - at(3, 0) * minor(3)
        }
    }
}

/// `x` as a fraction in [0.5, 1) of magnitude and a power of two: zero for
/// zero, and exact for every finite binary64, a subnormal one included.
fn frexp(x: f64) -> (f64, i32) {
    if x == 0.0 || !x.is_finite() {
        return (x, 0);
    }
    let bits = x.to_bits();
    let biased = i32::try_from((bits >> 52) & 0x7ff).unwrap_or(0);
    if biased == 0 {
        // A subnormal: normalised by 2^64 first.
        let (fraction, exponent) = frexp(x * f64::from_bits(0x43f0_0000_0000_0000));
        return (fraction, exponent - 64);
    }
    let fraction = f64::from_bits((bits & !(0x7ffu64 << 52)) | (1022u64 << 52));
    (fraction, biased - 1022)
}

/// `x` × 2^`e`, rounded once.
fn scale_by_power(x: f64, e: i128) -> f64 {
    if x == 0.0 {
        return x;
    }
    let (fraction, exponent) = frexp(x);
    // Past these bounds every result is infinite or zero alike.
    #[allow(clippy::cast_possible_truncation, reason = "clamped to a few thousand")]
    let total = i128::from(exponent).saturating_add(e).clamp(-2000, 2000) as i64;
    if total > 1024 {
        return x.signum() * f64::INFINITY;
    }
    if total >= -1021 {
        // fraction × 2 × 2^(total − 1): each step exact.
        return fraction * 2.0 * pow2(total - 1);
    }
    // A subnormal result: the fraction rounded once in units of 2^-1074.
    let shift = total + 1074;
    if shift < -1 {
        return 0.0 * x.signum();
    }
    mathf::round_even(fraction * pow2(shift)) * pow2(-1074)
}

/// The float function `builtin` at `x`, outside its domain NaN.
fn float_function(builtin: Builtin, [x, y, z]: [f64; 3]) -> f64 {
    use Builtin as B;
    let within = |low: f64, high: f64, v: f64| {
        if (low..=high).contains(&x) {
            v
        } else {
            f64::NAN
        }
    };
    match builtin {
        B::Acos => within(-1.0, 1.0, mathf::acos(x)),
        B::Asin => within(-1.0, 1.0, mathf::asin(x)),
        B::Acosh => within(1.0, f64::INFINITY, mathf::acosh(x)),
        B::Atanh => within(-1.0, 1.0, mathf::atanh(x)),
        B::Asinh => mathf::asinh(x),
        B::Atan => mathf::atan(x),
        B::Ceil => mathf::ceil(x),
        B::Cos => mathf::cos(x),
        B::Cosh => mathf::cosh(x),
        B::Degrees => x * (180.0 / core::f64::consts::PI),
        B::Exp => mathf::exp(x),
        B::Exp2 => mathf::exp2(x),
        B::Floor => mathf::floor(x),
        B::Fract => x - mathf::floor(x),
        B::InverseSqrt => within(0.0, f64::INFINITY, 1.0 / mathf::sqrt(x)),
        B::Log => within(0.0, f64::INFINITY, mathf::ln(x)),
        B::Log2 => within(0.0, f64::INFINITY, mathf::log2(x)),
        B::Radians => x * (core::f64::consts::PI / 180.0),
        B::Round => mathf::round_even(x),
        B::Saturate => mathf::clamp(x, 0.0, 1.0),
        B::Sin => mathf::sin(x),
        B::Sinh => mathf::sinh(x),
        B::Sqrt => within(0.0, f64::INFINITY, mathf::sqrt(x)),
        B::Tan => mathf::tan(x),
        B::Tanh => mathf::tanh(x),
        B::Trunc => mathf::trunc(x),
        B::Atan2 => mathf::atan2(x, y),
        B::Pow => {
            if x < 0.0 {
                f64::NAN
            } else {
                mathf::power(x, y)
            }
        }
        B::Step => {
            if x <= y {
                1.0
            } else {
                0.0
            }
        }
        B::Fma => x * y + z,
        B::Mix => x * (1.0 - z) + y * z,
        B::Smoothstep => {
            let t = mathf::clamp((z - x) / (y - x), 0.0, 1.0);
            t * t * (3.0 - 2.0 * t)
        }
        _ => x,
    }
}

/// `abs`, `max`, `min`, `clamp` and `sign` of integers or floats.
fn numeric_function(builtin: Builtin, xs: &[Value], scalar: Scalar, span: Span) -> Outcome<Value> {
    use Builtin as B;
    let lead = number(*xs.first().ok_or(Stop::Internal)?);
    let at = |i: usize| xs.get(i).map_or(lead, |v| number(*v));
    let numbers = [lead, at(1), at(2)];
    match numbers {
        [Number::Float(x), Number::Float(y), Number::Float(z)] => {
            let out = match builtin {
                B::Abs => mathf::fabs(x),
                B::Max => {
                    if x < y {
                        y
                    } else {
                        x
                    }
                }
                B::Min => {
                    if y < x {
                        y
                    } else {
                        x
                    }
                }
                B::Clamp => mathf::fmin(mathf::fmax(x, y), z),
                _ => {
                    if x > 0.0 {
                        1.0
                    } else if x < 0.0 {
                        -1.0
                    } else {
                        0.0
                    }
                }
            };
            float_of(out, scalar, span)
        }
        [Number::Int(x), Number::Int(y), Number::Int(z)] => {
            let out = match builtin {
                // The most negative i32's magnitude is itself.
                B::Abs if scalar == Scalar::I32 && x == i128::from(i32::MIN) => x,
                B::Abs => x.abs(),
                B::Max => x.max(y),
                B::Min => x.min(y),
                B::Clamp => x.max(y).min(z),
                _ => x.signum(),
            };
            int_of(out, scalar, span)
        }
        _ => Err(Stop::Internal),
    }
}

/// The bit built-ins, over the 32 bits of an `i32` or `u32`.
fn bit_function(builtin: Builtin, xs: &[Value]) -> Outcome<Value> {
    use Builtin as B;
    let (bits, signed) = match xs.first() {
        Some(Value::I32(n)) => (n.cast_unsigned(), true),
        Some(Value::U32(n)) => (*n, false),
        _ => return Err(Stop::Internal),
    };
    let argument = |at: usize| match xs.get(at) {
        Some(Value::U32(n)) => Ok(*n),
        Some(Value::I32(n)) => Ok(n.cast_unsigned()),
        _ => Err(Stop::Internal),
    };
    let out: u32 = match builtin {
        B::CountLeadingZeros => bits.leading_zeros(),
        B::CountOneBits => bits.count_ones(),
        B::CountTrailingZeros => bits.trailing_zeros(),
        B::FirstLeadingBit if signed => {
            let magnitude = if bits >> 31 == 1 { !bits } else { bits };
            if magnitude == 0 {
                u32::MAX
            } else {
                magnitude.ilog2()
            }
        }
        B::FirstLeadingBit => {
            if bits == 0 {
                u32::MAX
            } else {
                bits.ilog2()
            }
        }
        B::FirstTrailingBit => {
            if bits == 0 {
                u32::MAX
            } else {
                bits.trailing_zeros()
            }
        }
        B::ReverseBits => bits.reverse_bits(),
        B::ExtractBits => {
            let offset = argument(1)?.min(32);
            let count = argument(2)?.min(32 - offset);
            if count == 0 {
                0
            } else {
                let field = (u64::from(bits) >> offset) & ((1u64 << count) - 1);
                let field = u32::try_from(field).map_err(|_| Stop::Internal)?;
                if signed && count < 32 && (field >> (count - 1)) & 1 == 1 {
                    field | (u32::MAX << count)
                } else {
                    field
                }
            }
        }
        B::InsertBits => {
            let insert = argument(1)?;
            let offset = argument(2)?.min(32);
            let count = argument(3)?.min(32 - offset);
            if count == 0 {
                bits
            } else {
                let mask =
                    u32::try_from(((1u64 << count) - 1) << offset).map_err(|_| Stop::Internal)?;
                (bits & !mask) | ((insert << offset) & mask)
            }
        }
        _ => return Err(Stop::Internal),
    };
    Ok(if signed {
        Value::I32(out.cast_signed())
    } else {
        Value::U32(out)
    })
}
