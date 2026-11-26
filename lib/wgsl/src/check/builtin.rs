//! Built-in function calls: each built-in's overloads resolved against its
//! arguments' types as the specification's overload resolution has it —
//! the least-ranked conversions win, and an abstract parameterization is no
//! candidate where any argument is not a const-expression.

use core::fmt::Write as _;

use super::expr::Form;
use super::{Checker, Denotes, ExprInfo, Stage, NONE};
use crate::diag::{error, push, Code, Outcome, Span, Stop};
use crate::predeclared::Builtin;
use crate::syntax::{ExprId, Ident};
use crate::types::{Access, Dim, Origin, Scalar, Space, Texture, Ty, TyId};

/// The most arguments any built-in takes.
const MAX_ARGS: usize = 7;

/// The overload a call resolved to: the type each argument is taken as,
/// and the result, if it gives one.
struct Overload {
    params: [TyId; MAX_ARGS],
    count: usize,
    result: Option<TyId>,
}

impl Overload {
    const fn gives(result: Option<TyId>) -> Self {
        Self {
            params: [TyId(0); MAX_ARGS],
            count: 0,
            result,
        }
    }

    fn take(mut self, ty: TyId) -> Self {
        if let Some(slot) = self.params.get_mut(self.count) {
            *slot = ty;
            self.count += 1;
        }
        self
    }

    fn taking(result: TyId, params: &[TyId]) -> Self {
        params
            .iter()
            .fold(Self::gives(Some(result)), |overload, &ty| overload.take(ty))
    }
}

/// Which scalars a parameterization admits.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Family {
    /// `AbstractFloat`, `f32`, `f16`.
    Float,
    /// `f32` alone.
    F32,
    /// Every numeric scalar.
    Numeric,
    /// Every numeric scalar but `u32`.
    Signed,
    /// `i32` and `u32`.
    Integer,
    /// `bool` alone.
    Bool,
    /// Every scalar.
    Any,
}

impl Family {
    /// The scalar of this family a value of `scalar` converts to with the
    /// least rank, if one does.
    const fn admit(self, scalar: Scalar) -> Option<Scalar> {
        match (self, scalar) {
            (Self::Bool, Scalar::Bool) => Some(Scalar::Bool),
            (Self::Float, Scalar::AbstractInt) => Some(Scalar::AbstractFloat),
            (Self::Any, s)
            | (Self::Float, s @ (Scalar::AbstractFloat | Scalar::F32 | Scalar::F16)) => Some(s),
            (Self::F32, Scalar::AbstractInt | Scalar::AbstractFloat | Scalar::F32) => {
                Some(Scalar::F32)
            }
            (Self::Numeric, s) if s.is_numeric() => Some(s),
            (Self::Signed, s) if s.is_numeric() && !matches!(s, Scalar::U32) => Some(s),
            (Self::Integer, Scalar::AbstractInt | Scalar::I32) => Some(Scalar::I32),
            (Self::Integer, Scalar::U32) => Some(Scalar::U32),
            _ => None,
        }
    }
}

/// A shape a parameter takes.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Shape {
    Scalar,
    Vector,
    /// A scalar or a vector.
    Either,
}

impl<'a> Checker<'a> {
    /// A call of the built-in `builtin` with `args`.
    pub(crate) fn builtin_call(
        &mut self,
        builtin: Builtin,
        callee: Ident,
        args: &[ExprId],
        span: Span,
    ) -> Outcome<ExprInfo> {
        if let Builtin::Refused(extension) = builtin {
            return Err(error(Code::UnsupportedExtension, callee.name, |m| {
                m.quote(self.spell(callee.name))?;
                write!(m, " needs `{}`, which is not offered", extension.name())
            }));
        }
        if builtin != Builtin::Bitcast && !callee.template.is_empty() {
            return Err(error(Code::Type, callee.name, |m| {
                m.quote(self.spell(callee.name))?;
                m.write_str(" takes no template list")
            }));
        }
        if args.len() > MAX_ARGS {
            return Err(self.no_overload(callee, span));
        }
        let mut types = [TyId(0); MAX_ARGS];
        for (slot, &arg) in types.iter_mut().zip(args) {
            *slot = self.value_of(arg)?;
        }
        let types = &types[..args.len()];
        let all_const = args.iter().all(|&a| self.info(a).stage == Stage::Const);
        let overload = self
            .overload(builtin, callee, types, !all_const)?
            .ok_or_else(|| self.no_overload(callee, span))?;
        if overload.count != args.len() {
            return Err(self.no_overload(callee, span));
        }
        for (&arg, &param) in args.iter().zip(&overload.params[..overload.count]) {
            self.convert_to(arg, param)?;
        }
        self.constant_arguments(builtin, args, types)?;
        self.note_stage_use(builtin, span)?;
        self.note_builtin_access(builtin, args)?;
        self.argument_rules(builtin, args, Stage::Const)?;
        let ty = match overload.result {
            Some(ty) => ty,
            None => self.intern(Ty::Void)?,
        };
        let stage = if is_const(builtin) {
            args.iter()
                .map(|&a| self.info(a).stage)
                .max()
                .unwrap_or(Stage::Const)
        } else {
            Stage::Runtime
        };
        Ok(ExprInfo {
            ty,
            converted: ty,
            stage,
            value: NONE,
            denotes: Denotes::CallBuiltin(builtin),
        })
    }

    fn no_overload(&self, callee: Ident, span: Span) -> Stop {
        error(Code::NoOverload, span, |m| {
            m.write_str("no overload of ")?;
            m.quote(self.spell(callee.name))?;
            m.write_str(" takes these arguments")
        })
    }

    /// The overload of `builtin` that arguments of value types `types`
    /// call; `concrete` where an abstract parameterization is no candidate.
    #[expect(clippy::too_many_lines, reason = "one arm per built-in family")]
    fn overload(
        &mut self,
        builtin: Builtin,
        callee: Ident,
        types: &[TyId],
        concrete: bool,
    ) -> Outcome<Option<Overload>> {
        use Builtin as B;
        let same = |this: &mut Self, family, shape| this.same(types, family, shape, concrete);
        Ok(match builtin {
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
            | B::Smoothstep => same(self, Family::Float, Shape::Either)?,
            B::QuantizeToF16
            | B::Dpdx
            | B::DpdxCoarse
            | B::DpdxFine
            | B::Dpdy
            | B::DpdyCoarse
            | B::DpdyFine
            | B::Fwidth
            | B::FwidthCoarse
            | B::FwidthFine => same(self, Family::F32, Shape::Either)?,
            B::Abs | B::Max | B::Min | B::Clamp => same(self, Family::Numeric, Shape::Either)?,
            B::Sign => same(self, Family::Signed, Shape::Either)?,
            B::CountLeadingZeros
            | B::CountOneBits
            | B::CountTrailingZeros
            | B::FirstLeadingBit
            | B::FirstTrailingBit
            | B::ReverseBits => same(self, Family::Integer, Shape::Either)?,
            B::Normalize | B::Reflect | B::FaceForward => same(self, Family::Float, Shape::Vector)?,
            B::Cross => self
                .same(types, Family::Float, Shape::Vector, concrete)?
                .filter(|o| matches!(o.result.map(|t| self.types.get(t)), Some(Ty::Vector(3, _)))),
            B::Length | B::Distance | B::Dot => {
                let family = if builtin == B::Dot {
                    Family::Numeric
                } else {
                    Family::Float
                };
                let shape = if builtin == B::Dot {
                    Shape::Vector
                } else {
                    Shape::Either
                };
                match self.same(types, family, shape, concrete)? {
                    Some(mut overload) => {
                        let element = overload.params[0];
                        let scalar = self.types.scalar_of(element).ok_or(Stop::Internal)?;
                        overload.result = Some(self.types.scalar(scalar)?);
                        Some(overload)
                    }
                    None => None,
                }
            }
            B::Mix => self.mix(types, concrete)?,
            B::Refract => self.refract(types, concrete)?,
            B::Ldexp => self.ldexp(types, concrete)?,
            B::Frexp | B::Modf => match self.same(types, Family::Float, Shape::Either, concrete)? {
                Some(mut overload) => {
                    let of = overload.params[0];
                    let origin = if builtin == B::Frexp {
                        Origin::Frexp(of)
                    } else {
                        Origin::Modf(of)
                    };
                    overload.result = Some(self.types.result(origin)?);
                    Some(overload)
                }
                None => None,
            },
            B::Determinant | B::Transpose => self.matrix_function(builtin, types, concrete)?,
            B::ExtractBits | B::InsertBits => self.bits(builtin, types)?,
            B::Dot4U8Packed | B::Dot4I8Packed => {
                let unsigned = self.types.scalar(Scalar::U32)?;
                let scalar = if builtin == B::Dot4U8Packed {
                    Scalar::U32
                } else {
                    Scalar::I32
                };
                let result = self.types.scalar(scalar)?;
                Some(Overload::taking(result, &[unsigned, unsigned]))
            }
            B::All | B::Any => match self.same(types, Family::Bool, Shape::Either, concrete)? {
                Some(mut overload) => {
                    overload.result = Some(self.types.scalar(Scalar::Bool)?);
                    Some(overload)
                }
                None => None,
            },
            B::Select => self.select(types, concrete)?,
            B::ArrayLength => self.array_length(types)?,
            B::Bitcast => self.bitcast(callee, types)?,
            B::Pack4x8Snorm | B::Pack4x8Unorm => {
                self.packing((4, Scalar::F32), Scalar::U32, true)?
            }
            B::Pack4xI8 | B::Pack4xI8Clamp => self.packing((4, Scalar::I32), Scalar::U32, true)?,
            B::Pack4xU8 | B::Pack4xU8Clamp => self.packing((4, Scalar::U32), Scalar::U32, true)?,
            B::Pack2x16Snorm | B::Pack2x16Unorm | B::Pack2x16Float => {
                self.packing((2, Scalar::F32), Scalar::U32, true)?
            }
            B::Unpack4x8Snorm | B::Unpack4x8Unorm => {
                self.packing((4, Scalar::F32), Scalar::U32, false)?
            }
            B::Unpack4xI8 => self.packing((4, Scalar::I32), Scalar::U32, false)?,
            B::Unpack4xU8 => self.packing((4, Scalar::U32), Scalar::U32, false)?,
            B::Unpack2x16Snorm | B::Unpack2x16Unorm | B::Unpack2x16Float => {
                self.packing((2, Scalar::F32), Scalar::U32, false)?
            }
            B::StorageBarrier | B::TextureBarrier | B::WorkgroupBarrier => {
                types.is_empty().then(|| Overload::gives(None))
            }
            B::WorkgroupUniformLoad => self.workgroup_uniform_load(types)?,
            B::AtomicLoad
            | B::AtomicStore
            | B::AtomicAdd
            | B::AtomicSub
            | B::AtomicMax
            | B::AtomicMin
            | B::AtomicAnd
            | B::AtomicOr
            | B::AtomicXor
            | B::AtomicExchange
            | B::AtomicCompareExchangeWeak => self.atomic(builtin, types)?,
            B::TextureDimensions
            | B::TextureGather
            | B::TextureGatherCompare
            | B::TextureLoad
            | B::TextureNumLayers
            | B::TextureNumLevels
            | B::TextureNumSamples
            | B::TextureSample
            | B::TextureSampleBias
            | B::TextureSampleCompare
            | B::TextureSampleCompareLevel
            | B::TextureSampleGrad
            | B::TextureSampleLevel
            | B::TextureSampleBaseClampToEdge
            | B::TextureStore => self.texture_function(builtin, types)?,
            B::Refused(_) => return Err(Stop::Internal),
        })
    }

    /// The parameterization of a built-in whose every argument and result
    /// are one type `T` of `family` in `shape`: the scalars joined, the
    /// vector widths equal.
    fn same(
        &mut self,
        types: &[TyId],
        family: Family,
        shape: Shape,
        concrete: bool,
    ) -> Outcome<Option<Overload>> {
        let Some(ty) = self.unify(types, family, shape, concrete)? else {
            return Ok(None);
        };
        let mut overload = Overload::gives(Some(ty));
        for _ in types {
            overload = overload.take(ty);
        }
        Ok(Some(overload))
    }

    /// The one type of `family` in `shape` every value type of `types`
    /// converts to with the least rank: concrete where `concrete`.
    fn unify(
        &mut self,
        types: &[TyId],
        family: Family,
        shape: Shape,
        concrete: bool,
    ) -> Outcome<Option<TyId>> {
        let mut form: Option<Form> = None;
        let mut scalar: Option<Scalar> = None;
        for &ty in types {
            let Some((this_form, this_scalar)) = self.form(ty) else {
                return Ok(None);
            };
            let fits = !matches!(
                (shape, this_form),
                (_, Form::Matrix(..))
                    | (Shape::Scalar, Form::Vector(_))
                    | (Shape::Vector, Form::Scalar)
            );
            if !fits || form.is_some_and(|f| f != this_form) {
                return Ok(None);
            }
            form = Some(this_form);
            scalar = match scalar {
                None => Some(this_scalar),
                Some(seen) => match super::expr::join(seen, this_scalar) {
                    Some(joined) => Some(joined),
                    None => return Ok(None),
                },
            };
        }
        let (Some(form), Some(scalar)) = (form, scalar) else {
            return Ok(None);
        };
        let Some(admitted) = family.admit(scalar) else {
            return Ok(None);
        };
        let admitted = if concrete {
            admitted.concrete()
        } else {
            admitted
        };
        self.of_form(form, admitted).map(Some)
    }

    /// `mix(e1: T, e2: T, e3: T)`, or a vector `T` blended by a scalar.
    fn mix(&mut self, types: &[TyId], concrete: bool) -> Outcome<Option<Overload>> {
        let [a, b, c] = types else {
            return Ok(None);
        };
        let blend_scalar = matches!(self.form(*c), Some((Form::Scalar, _)))
            && matches!(self.form(*a), Some((Form::Vector(_), _)));
        if !blend_scalar {
            return self.same(types, Family::Float, Shape::Either, concrete);
        }
        let Some(vector) = self.unify(&[*a, *b], Family::Float, Shape::Vector, concrete)? else {
            return Ok(None);
        };
        let scalar = self.types.scalar_of(vector).ok_or(Stop::Internal)?;
        let Some(blend) = self.unify(&[*c], Family::Float, Shape::Scalar, concrete)? else {
            return Ok(None);
        };
        let joined = self
            .types
            .scalar_of(blend)
            .and_then(|s| super::expr::join(s, scalar));
        let Some(joined) = joined else {
            return Ok(None);
        };
        let Some((Form::Vector(n), _)) = self.form(vector) else {
            return Ok(None);
        };
        let vector = self.intern(Ty::Vector(n, joined))?;
        let blend = self.types.scalar(joined)?;
        Ok(Some(Overload::taking(vector, &[vector, vector, blend])))
    }

    /// `refract(e1: vecN<S>, e2: vecN<S>, e3: S)`.
    fn refract(&mut self, types: &[TyId], concrete: bool) -> Outcome<Option<Overload>> {
        let [incident, normal, eta] = types else {
            return Ok(None);
        };
        let Some(vector) = self.unify(
            &[*incident, *normal],
            Family::Float,
            Shape::Vector,
            concrete,
        )?
        else {
            return Ok(None);
        };
        let Some(ratio) = self.unify(&[*eta], Family::Float, Shape::Scalar, concrete)? else {
            return Ok(None);
        };
        let (Some((Form::Vector(lanes), lane)), Some(scale)) =
            (self.form(vector), self.types.scalar_of(ratio))
        else {
            return Ok(None);
        };
        let Some(joined) = super::expr::join(lane, scale) else {
            return Ok(None);
        };
        let vector = self.intern(Ty::Vector(lanes, joined))?;
        let ratio = self.types.scalar(joined)?;
        Ok(Some(Overload::taking(vector, &[vector, vector, ratio])))
    }

    /// `ldexp(e1: T, e2: I)`: `T` a float, `I` an `i32` or abstract integer
    /// of its shape, abstract exactly where `T` is.
    fn ldexp(&mut self, types: &[TyId], concrete: bool) -> Outcome<Option<Overload>> {
        let [a, b] = types else {
            return Ok(None);
        };
        let (Some((form_a, sa)), Some((form_b, sb))) = (self.form(*a), self.form(*b)) else {
            return Ok(None);
        };
        if form_a != form_b || matches!(form_a, Form::Matrix(..)) {
            return Ok(None);
        }
        let Some(float) = Family::Float.admit(sa) else {
            return Ok(None);
        };
        if !matches!(sb, Scalar::AbstractInt | Scalar::I32) {
            return Ok(None);
        }
        // Either side concrete makes both so.
        let abstract_pair = !concrete && float.is_abstract() && sb == Scalar::AbstractInt;
        let (float, exponent) = if abstract_pair {
            (Scalar::AbstractFloat, Scalar::AbstractInt)
        } else {
            (float.concrete(), Scalar::I32)
        };
        let value = self.of_form(form_a, float)?;
        let power = self.of_form(form_a, exponent)?;
        Ok(Some(Overload::taking(value, &[value, power])))
    }

    /// `determinant` of a square float matrix, `transpose` of any.
    fn matrix_function(
        &mut self,
        builtin: Builtin,
        types: &[TyId],
        concrete: bool,
    ) -> Outcome<Option<Overload>> {
        let [ty] = types else {
            return Ok(None);
        };
        let Some((Form::Matrix(columns, rows), scalar)) = self.form(*ty) else {
            return Ok(None);
        };
        let scalar = if concrete { scalar.concrete() } else { scalar };
        let param = self.of_form(Form::Matrix(columns, rows), scalar)?;
        let result = if builtin == Builtin::Determinant {
            if columns != rows {
                return Ok(None);
            }
            self.types.scalar(scalar)?
        } else {
            self.of_form(Form::Matrix(rows, columns), scalar)?
        };
        Ok(Some(Overload::taking(result, &[param])))
    }

    /// `extractBits(e: T, offset: u32, count: u32)` and
    /// `insertBits(e: T, newbits: T, offset: u32, count: u32)`.
    fn bits(&mut self, builtin: Builtin, types: &[TyId]) -> Outcome<Option<Overload>> {
        let values = if builtin == Builtin::ExtractBits {
            1
        } else {
            2
        };
        if types.len() != values + 2 {
            return Ok(None);
        }
        let Some(ty) = self.unify(&types[..values], Family::Integer, Shape::Either, true)? else {
            return Ok(None);
        };
        let unsigned = self.types.scalar(Scalar::U32)?;
        let mut overload = Overload::gives(Some(ty));
        for _ in 0..values {
            overload = overload.take(ty);
        }
        Ok(Some(overload.take(unsigned).take(unsigned)))
    }

    /// `select(f: T, t: T, cond: bool)`, or vectors chosen between per
    /// component by a `vecN<bool>`.
    fn select(&mut self, types: &[TyId], concrete: bool) -> Outcome<Option<Overload>> {
        let [f, t, cond] = types else {
            return Ok(None);
        };
        let Some(value) = self.unify(&[*f, *t], Family::Any, Shape::Either, concrete)? else {
            return Ok(None);
        };
        let condition = match (self.form(value), self.form(*cond)) {
            (_, Some((Form::Scalar, Scalar::Bool))) => self.types.scalar(Scalar::Bool)?,
            (Some((Form::Vector(n), _)), Some((Form::Vector(m), Scalar::Bool))) if n == m => {
                self.intern(Ty::Vector(n, Scalar::Bool))?
            }
            _ => return Ok(None),
        };
        Ok(Some(Overload::taking(value, &[value, value, condition])))
    }

    /// `arrayLength(p: ptr<storage, array<E>, AM>) -> u32`.
    fn array_length(&mut self, types: &[TyId]) -> Outcome<Option<Overload>> {
        let [pointer] = types else {
            return Ok(None);
        };
        let Ty::Pointer {
            space: Space::Storage,
            store,
            ..
        } = self.types.get(*pointer)
        else {
            return Ok(None);
        };
        let runtime = matches!(
            self.types.get(store),
            Ty::Array {
                count: crate::types::Count::Runtime,
                ..
            }
        );
        if !runtime {
            return Ok(None);
        }
        let result = self.types.scalar(Scalar::U32)?;
        Ok(Some(Overload::taking(result, &[*pointer])))
    }

    /// `bitcast<T>(e)`: a numeric scalar or vector of the same width in
    /// bits, of `i32`, `u32`, `f32` and `f16` components; an abstract
    /// integer taken as the `u32` the target names, else as an `i32`, and
    /// an abstract float as an `f32`.
    fn bitcast(&mut self, callee: Ident, types: &[TyId]) -> Outcome<Option<Overload>> {
        let [from] = types else {
            return Ok(None);
        };
        let tree: &'a crate::syntax::Tree = self.tree;
        let [target_expr] = tree.expr_list(callee.template) else {
            return Err(error(Code::Type, callee.name, |m| {
                m.write_str("bitcast takes one template argument, its result's type")
            }));
        };
        let target = self.info(*target_expr);
        if target.denotes != Denotes::Type {
            return Err(error(Code::Misused, self.span_of(*target_expr), |m| {
                m.write_str("a type is needed here")
            }));
        }
        let target = target.ty;
        let (Some((target_form, target_scalar)), Some((from_form, from_scalar))) =
            (self.form(target), self.form(*from))
        else {
            return Ok(None);
        };
        let source_scalar = match from_scalar {
            Scalar::AbstractInt if target_scalar == Scalar::U32 => Scalar::U32,
            Scalar::AbstractInt => Scalar::I32,
            Scalar::AbstractFloat => Scalar::F32,
            other => other,
        };
        let widths = |form: Form, scalar: Scalar| -> Option<u32> {
            let lanes = match form {
                Form::Scalar => 1,
                Form::Vector(n) => u32::from(n),
                Form::Matrix(..) => return None,
            };
            matches!(
                scalar,
                Scalar::I32 | Scalar::U32 | Scalar::F32 | Scalar::F16
            )
            .then(|| lanes * scalar.bytes() * 8)
        };
        let fits = widths(target_form, target_scalar)
            .is_some_and(|bits| widths(from_form, source_scalar) == Some(bits));
        if !fits {
            return Ok(None);
        }
        let param = self.of_form(from_form, source_scalar)?;
        Ok(Some(Overload::taking(target, &[param])))
    }

    /// A packing built-in from a `vecN<element>` to `packed`, or, where
    /// `pack` is false, its unpacking back.
    fn packing(
        &mut self,
        (n, element): (u8, Scalar),
        packed: Scalar,
        pack: bool,
    ) -> Outcome<Option<Overload>> {
        let vector = self.intern(Ty::Vector(n, element))?;
        let word = self.types.scalar(packed)?;
        Ok(Some(if pack {
            Overload::taking(word, &[vector])
        } else {
            Overload::taking(vector, &[word])
        }))
    }

    /// `workgroupUniformLoad(p: ptr<workgroup, T>) -> T`, an atomic's value
    /// for an atomic `T`.
    fn workgroup_uniform_load(&mut self, types: &[TyId]) -> Outcome<Option<Overload>> {
        let [pointer] = types else {
            return Ok(None);
        };
        let Ty::Pointer {
            space: Space::Workgroup,
            store,
            ..
        } = self.types.get(*pointer)
        else {
            return Ok(None);
        };
        let result = match self.types.get(store) {
            Ty::Atomic(scalar) => self.types.scalar(scalar)?,
            _ if self.types.is_constructible(store) => store,
            _ => return Ok(None),
        };
        Ok(Some(Overload::taking(result, &[*pointer])))
    }

    /// An atomic built-in: its first argument a `read_write` pointer to an
    /// atomic in storage or workgroup memory.
    fn atomic(&mut self, builtin: Builtin, types: &[TyId]) -> Outcome<Option<Overload>> {
        let Some(&pointer) = types.first() else {
            return Ok(None);
        };
        let Ty::Pointer {
            space: Space::Storage | Space::Workgroup,
            store,
            access: Access::ReadWrite,
        } = self.types.get(pointer)
        else {
            return Ok(None);
        };
        let Ty::Atomic(scalar) = self.types.get(store) else {
            return Ok(None);
        };
        let value = self.types.scalar(scalar)?;
        Ok(Some(match builtin {
            Builtin::AtomicLoad => Overload::taking(value, &[pointer]),
            Builtin::AtomicStore => Overload::gives(None).take(pointer).take(value),
            Builtin::AtomicCompareExchangeWeak => {
                let result = self.types.result(Origin::CompareExchange(scalar))?;
                Overload::taking(result, &[pointer, value, value])
            }
            _ => Overload::taking(value, &[pointer, value]),
        }))
    }

    /// Whether a `textureGather` call's first argument is the component it
    /// reads, rather than a depth texture.
    fn gathers_component(&self, builtin: Builtin, types: &[TyId]) -> bool {
        builtin == Builtin::TextureGather
            && types
                .first()
                .is_some_and(|&t| !matches!(self.types.get(t), Ty::Texture(_)))
    }

    /// Refuse an argument a built-in requires a const-expression for, and
    /// one outside its range: a texel offset's components in -8 to 7, a
    /// gather's component in 0 to 3.
    fn constant_arguments(
        &mut self,
        builtin: Builtin,
        args: &[ExprId],
        types: &[TyId],
    ) -> Outcome<()> {
        let component = self.gathers_component(builtin, types);
        if component {
            self.constant_in(args[0], (0, 3), "a gather's component")?;
        }
        if let Some(offset) = self.offset_argument(builtin, args, usize::from(component)) {
            self.constant_in(offset, (-8, 7), "a texel offset's components")?;
        }
        Ok(())
    }

    /// Refuse `arg` unless it is a const-expression whose every component
    /// lies from `least` to `most`.
    fn constant_in(&mut self, arg: ExprId, (least, most): (i128, i128), what: &str) -> Outcome<()> {
        if self.info(arg).stage != Stage::Const {
            return Err(error(Code::Evaluation, self.span_of(arg), |m| {
                write!(m, "{what} is a const-expression")
            }));
        }
        self.force(arg)?;
        let value = self.value_or_stop(arg)?;
        let ty = self.info(arg).converted;
        let mut parts = alloc::vec::Vec::new();
        let parts = if self.values.integer(value).is_some() {
            core::slice::from_ref(&value)
        } else {
            self.values
                .components(&mut self.types, (value, ty), &mut parts)?;
            &parts[..]
        };
        let outside = parts
            .iter()
            .any(|&p| self.values.integer(p).is_none_or(|n| n < least || n > most));
        if outside {
            return Err(error(Code::Evaluation, self.span_of(arg), |m| {
                write!(m, "{what} lie from {least} to {most}")
            }));
        }
        Ok(())
    }

    /// The offset argument of a texture call, where it has one: the
    /// argument after the ones its form needs, of a signed vector type.
    fn offset_argument(
        &self,
        builtin: Builtin,
        args: &[ExprId],
        texture_at: usize,
    ) -> Option<ExprId> {
        use Builtin as B;
        let takes = matches!(
            builtin,
            B::TextureGather
                | B::TextureGatherCompare
                | B::TextureSample
                | B::TextureSampleBias
                | B::TextureSampleCompare
                | B::TextureSampleCompareLevel
                | B::TextureSampleGrad
                | B::TextureSampleLevel
        );
        if !takes {
            return None;
        }
        let texture = self.types.get(self.info(*args.get(texture_at)?).converted);
        let Ty::Texture(texture) = texture else {
            return None;
        };
        let arrayed = texture.dim().is_arrayed();
        let extra = match builtin {
            B::TextureSampleBias
            | B::TextureSampleCompare
            | B::TextureSampleCompareLevel
            | B::TextureSampleLevel
            | B::TextureGatherCompare => 1,
            B::TextureSampleGrad => 2,
            _ => 0,
        };
        // Texture, sampler and coordinates, then the array index.
        let needed = texture_at + 3 + usize::from(arrayed) + extra;
        (args.len() == needed + 1).then(|| args[needed])
    }

    /// A texture built-in, its forms fixed by the texture it is given.
    #[expect(clippy::too_many_lines, reason = "one arm per texture built-in family")]
    fn texture_function(&mut self, builtin: Builtin, types: &[TyId]) -> Outcome<Option<Overload>> {
        use Builtin as B;
        let gather_component = self.gathers_component(builtin, types);
        let at = usize::from(gather_component);
        let Some(&texture_ty) = types.get(at) else {
            return Ok(None);
        };
        let Ty::Texture(texture) = self.types.get(texture_ty) else {
            return Ok(None);
        };
        let rest = &types[at + 1..];
        let mut overload = Overload::gives(None);
        if gather_component {
            let component = self.integer_like(types[0])?;
            overload = overload.take(component);
        }
        overload = overload.take(texture_ty);
        let u32_ty = self.types.scalar(Scalar::U32)?;
        let result = match builtin {
            B::TextureDimensions => {
                let leveled = matches!(texture, Texture::Sampled { .. } | Texture::Depth { .. });
                match rest {
                    [] => {}
                    [level] if leveled => overload = overload.take(self.integer_like(*level)?),
                    _ => return Ok(None),
                }
                let dim = texture.dim();
                match dim {
                    Dim::D1 => u32_ty,
                    Dim::D3 => self.intern(Ty::Vector(3, Scalar::U32))?,
                    _ => self.intern(Ty::Vector(2, Scalar::U32))?,
                }
            }
            B::TextureNumLayers | B::TextureNumLevels | B::TextureNumSamples => {
                let fits = match builtin {
                    B::TextureNumLayers => texture.dim().is_arrayed(),
                    B::TextureNumLevels => {
                        matches!(texture, Texture::Sampled { .. } | Texture::Depth { .. })
                    }
                    _ => matches!(
                        texture,
                        Texture::Multisampled { .. } | Texture::DepthMultisampled
                    ),
                };
                if !fits || !rest.is_empty() {
                    return Ok(None);
                }
                u32_ty
            }
            B::TextureLoad => match self.texture_load(texture, rest, overload)? {
                Some((loaded, result)) => {
                    overload = loaded;
                    result
                }
                None => return Ok(None),
            },
            B::TextureStore => {
                let Texture::Storage {
                    dim,
                    format,
                    access: Access::Write | Access::ReadWrite,
                } = texture
                else {
                    return Ok(None);
                };
                let Some(&given) = rest.first() else {
                    return Ok(None);
                };
                overload = overload.take(self.integer_coordinates(dim, given)?);
                let mut next = 1;
                if dim.is_arrayed() {
                    let Some(&index) = rest.get(next) else {
                        return Ok(None);
                    };
                    overload = overload.take(self.integer_like(index)?);
                    next += 1;
                }
                let value = self.intern(Ty::Vector(4, format.channel()))?;
                overload = overload.take(value);
                if rest.len() != next + 1 {
                    return Ok(None);
                }
                overload.result = None;
                return Ok(Some(overload));
            }
            B::TextureSampleBaseClampToEdge => {
                let fits = matches!(
                    texture,
                    Texture::External
                        | Texture::Sampled {
                            dim: Dim::D2,
                            texel: Scalar::F32
                        }
                );
                let sampler = self.intern(Ty::Sampler { comparison: false })?;
                let coords = self.intern(Ty::Vector(2, Scalar::F32))?;
                if !fits || rest.len() != 2 {
                    return Ok(None);
                }
                overload = overload.take(sampler).take(coords);
                self.intern(Ty::Vector(4, Scalar::F32))?
            }
            _ => match self.sampling(builtin, texture, rest, (overload, gather_component))? {
                Some((sampled, result)) => {
                    overload = sampled;
                    result
                }
                None => return Ok(None),
            },
        };
        overload.result = Some(result);
        Ok(Some(overload))
    }

    /// `textureLoad`'s forms: coordinates, then an array index, a level or
    /// a sample index as the texture takes them.
    fn texture_load(
        &mut self,
        texture: Texture,
        rest: &[TyId],
        overload: Overload,
    ) -> Outcome<Option<(Overload, TyId)>> {
        let mut overload = overload;
        let dim = texture.dim();
        if dim.is_cube() {
            return Ok(None);
        }
        let Some(&coords) = rest.first() else {
            return Ok(None);
        };
        overload = overload.take(self.integer_coordinates(dim, coords)?);
        let mut taken = 1;
        if dim.is_arrayed() {
            let Some(&index) = rest.get(taken) else {
                return Ok(None);
            };
            overload = overload.take(self.integer_like(index)?);
            taken += 1;
        }
        let needs_level_or_sample = !matches!(texture, Texture::Storage { .. } | Texture::External);
        if needs_level_or_sample {
            let Some(&level) = rest.get(taken) else {
                return Ok(None);
            };
            overload = overload.take(self.integer_like(level)?);
            taken += 1;
        }
        if rest.len() != taken {
            return Ok(None);
        }
        let result = match texture {
            Texture::Sampled { texel, .. } | Texture::Multisampled { texel } => {
                self.intern(Ty::Vector(4, texel))?
            }
            Texture::Depth { .. } | Texture::DepthMultisampled => self.types.scalar(Scalar::F32)?,
            Texture::External => self.intern(Ty::Vector(4, Scalar::F32))?,
            Texture::Storage { format, access, .. } => {
                if !access.reads() {
                    return Ok(None);
                }
                self.intern(Ty::Vector(4, format.channel()))?
            }
        };
        Ok(Some((overload, result)))
    }

    /// The sampling and gathering forms: texture, sampler, coordinates, an
    /// array index for an arrayed texture, what the function reads beside,
    /// and an offset where the dimension takes one.
    fn sampling(
        &mut self,
        builtin: Builtin,
        texture: Texture,
        rest: &[TyId],
        (overload, component): (Overload, bool),
    ) -> Outcome<Option<(Overload, TyId)>> {
        use Builtin as B;
        let mut overload = overload;
        let compare = matches!(
            builtin,
            B::TextureSampleCompare | B::TextureSampleCompareLevel | B::TextureGatherCompare
        );
        let (dim, depth) = match texture {
            Texture::Sampled {
                dim,
                texel: Scalar::F32,
            } if !compare => (dim, false),
            Texture::Sampled { dim, .. } if builtin == B::TextureGather => (dim, false),
            Texture::Depth { dim } => (dim, true),
            _ => return Ok(None),
        };
        let gathers = matches!(builtin, B::TextureGather | B::TextureGatherCompare);
        let dims_fit = match builtin {
            B::TextureSample | B::TextureSampleLevel => !depth || dim != Dim::D1,
            B::TextureSampleBias | B::TextureSampleGrad => !depth && dim != Dim::D1,
            B::TextureGather => !matches!(dim, Dim::D1 | Dim::D3),
            _ => depth,
        };
        // A gather reads a sampled texture's named component, a depth
        // texture's one value.
        let gather_pairs = builtin != B::TextureGather || component != depth;
        if !dims_fit || (compare && !depth) || !gather_pairs {
            return Ok(None);
        }
        let sampler = self.intern(Ty::Sampler {
            comparison: compare,
        })?;
        let coords = self.coordinates(dim)?;
        overload = overload.take(sampler).take(coords);
        let mut taken = 2;
        if dim.is_arrayed() {
            let Some(&index) = rest.get(taken) else {
                return Ok(None);
            };
            overload = overload.take(self.integer_like(index)?);
            taken += 1;
        }
        let f32_ty = self.types.scalar(Scalar::F32)?;
        match builtin {
            B::TextureSampleBias
            | B::TextureSampleCompare
            | B::TextureSampleCompareLevel
            | B::TextureGatherCompare => {
                overload = overload.take(f32_ty);
                taken += 1;
            }
            B::TextureSampleLevel => {
                let level = if depth {
                    let Some(&level) = rest.get(taken) else {
                        return Ok(None);
                    };
                    self.integer_like(level)?
                } else {
                    f32_ty
                };
                overload = overload.take(level);
                taken += 1;
            }
            B::TextureSampleGrad => {
                let gradient = self.coordinates(dim)?;
                overload = overload.take(gradient).take(gradient);
                taken += 2;
            }
            _ => {}
        }
        let offsettable = matches!(dim, Dim::D2 | Dim::D2Array | Dim::D3);
        if rest.len() == taken + 1 && offsettable {
            let lanes = if dim == Dim::D3 { 3 } else { 2 };
            overload = overload.take(self.intern(Ty::Vector(lanes, Scalar::I32))?);
            taken += 1;
        }
        if rest.len() != taken {
            return Ok(None);
        }
        let result = match (gathers, depth, texture) {
            (true, _, Texture::Sampled { texel, .. }) => self.intern(Ty::Vector(4, texel))?,
            (true, _, _) | (false, false, _) => self.intern(Ty::Vector(4, Scalar::F32))?,
            (false, true, _) => f32_ty,
        };
        Ok(Some((overload, result)))
    }

    /// The float coordinates of a texture of `dim`, its array index aside.
    fn coordinates(&mut self, dim: Dim) -> Outcome<TyId> {
        match dim.coordinates() {
            1 => self.types.scalar(Scalar::F32),
            n => self.intern(Ty::Vector(n, Scalar::F32)),
        }
    }

    /// Integer coordinates of `dim`'s shape, of the given argument's
    /// integer type, `i32` for an abstract one.
    fn integer_coordinates(&mut self, dim: Dim, given: TyId) -> Outcome<TyId> {
        let scalar = self
            .types
            .scalar_of(given)
            .and_then(|s| Family::Integer.admit(s))
            .unwrap_or(Scalar::I32);
        match dim.coordinates() {
            1 => self.types.scalar(scalar),
            n => self.intern(Ty::Vector(n, scalar)),
        }
    }

    /// An integer argument's type: `u32` for a `u32`, else `i32`.
    fn integer_like(&mut self, given: TyId) -> Outcome<TyId> {
        let scalar = match self.types.get(given) {
            Ty::Scalar(Scalar::U32) => Scalar::U32,
            _ => Scalar::I32,
        };
        self.types.scalar(scalar)
    }

    /// Record a call of a built-in only some stages may make, for the
    /// entry points that reach it.
    fn note_stage_use(&mut self, builtin: Builtin, span: Span) -> Outcome<()> {
        // At module scope no call stands where a run-time value may.
        let Some(stages) = stages_of(builtin).filter(|_| self.current_fn != NONE) else {
            return Ok(());
        };
        push(
            &mut self.stage_uses,
            super::StageUse {
                function: self.current_fn,
                stages,
                span,
            },
        )
    }
}

/// A texture's dimensionality, the external texture's being 2D.
/// The stages that may call `builtin`, where not every stage may: as a
/// mask of vertex 1, fragment 2 and compute 4.
pub(crate) const fn stages_of(builtin: Builtin) -> Option<u8> {
    use Builtin as B;
    match builtin {
        B::Dpdx
        | B::DpdxCoarse
        | B::DpdxFine
        | B::Dpdy
        | B::DpdyCoarse
        | B::DpdyFine
        | B::Fwidth
        | B::FwidthCoarse
        | B::FwidthFine
        | B::TextureSample
        | B::TextureSampleBias
        | B::TextureSampleCompare => Some(2),
        B::StorageBarrier | B::TextureBarrier | B::WorkgroupBarrier | B::WorkgroupUniformLoad => {
            Some(4)
        }
        B::AtomicLoad
        | B::AtomicStore
        | B::AtomicAdd
        | B::AtomicSub
        | B::AtomicMax
        | B::AtomicMin
        | B::AtomicAnd
        | B::AtomicOr
        | B::AtomicXor
        | B::AtomicExchange
        | B::AtomicCompareExchangeWeak => Some(2 | 4),
        _ => None,
    }
}

/// Whether `builtin` may stand in a const-expression.
pub(crate) const fn is_const(builtin: Builtin) -> bool {
    use Builtin as B;
    !matches!(
        builtin,
        B::ArrayLength
            | B::Dpdx
            | B::DpdxCoarse
            | B::DpdxFine
            | B::Dpdy
            | B::DpdyCoarse
            | B::DpdyFine
            | B::Fwidth
            | B::FwidthCoarse
            | B::FwidthFine
            | B::TextureDimensions
            | B::TextureGather
            | B::TextureGatherCompare
            | B::TextureLoad
            | B::TextureNumLayers
            | B::TextureNumLevels
            | B::TextureNumSamples
            | B::TextureSample
            | B::TextureSampleBias
            | B::TextureSampleCompare
            | B::TextureSampleCompareLevel
            | B::TextureSampleGrad
            | B::TextureSampleLevel
            | B::TextureSampleBaseClampToEdge
            | B::TextureStore
            | B::AtomicLoad
            | B::AtomicStore
            | B::AtomicAdd
            | B::AtomicSub
            | B::AtomicMax
            | B::AtomicMin
            | B::AtomicAnd
            | B::AtomicOr
            | B::AtomicXor
            | B::AtomicExchange
            | B::AtomicCompareExchangeWeak
            | B::StorageBarrier
            | B::TextureBarrier
            | B::WorkgroupBarrier
            | B::WorkgroupUniformLoad
            | B::Refused(_)
    )
}

/// Whether a call of `builtin` must be used as a value, not stand alone
/// as a statement.
pub(crate) const fn must_use(builtin: Builtin) -> bool {
    use Builtin as B;
    !matches!(
        builtin,
        B::TextureStore
            | B::AtomicLoad
            | B::AtomicStore
            | B::AtomicAdd
            | B::AtomicSub
            | B::AtomicMax
            | B::AtomicMin
            | B::AtomicAnd
            | B::AtomicOr
            | B::AtomicXor
            | B::AtomicExchange
            | B::AtomicCompareExchangeWeak
            | B::StorageBarrier
            | B::TextureBarrier
            | B::WorkgroupBarrier
    )
}
