//! Built-in functions, each written as WGSL defines it: through
//! `GLSL.std.450` and SPIR-V's own instructions where their definition is
//! WGSL's for every argument, and from defined instructions where it is
//! not — clamps and minima whose bounds may cross, the integer absolute
//! value, smoothstep, the bit counts and fields, the packings and the
//! integer dot products.

use tairix_spirv::enumerant::{Capability, ImageOperands, MemorySemantics, Scope};
use tairix_spirv::{Arg, GlslStd450, Id, Op};

use super::expr::{float_bits, Lowered};
use super::function::Writer;
use super::{built, Flavour, Index, Place};
use crate::diag::{Outcome, Stop};
use crate::predeclared::Builtin;
use crate::syntax::ExprId;
use crate::types::{Scalar, Space, Texture, Ty, TyId};

/// The value of argument `at`.
fn value(args: &[Lowered], at: usize) -> Outcome<Id> {
    args.get(at).ok_or(Stop::Internal)?.value()
}

/// The `GLSL.std.450` instruction a built-in is where that instruction's
/// definition is WGSL's, with the same operands in the same order.
const fn same_as_glsl(builtin: Builtin) -> Option<GlslStd450> {
    use Builtin as B;
    use GlslStd450 as G;
    Some(match builtin {
        B::Acos => G::ACOS,
        B::Acosh => G::ACOSH,
        B::Asin => G::ASIN,
        B::Asinh => G::ASINH,
        B::Atan => G::ATAN,
        B::Atanh => G::ATANH,
        B::Atan2 => G::ATAN2,
        B::Ceil => G::CEIL,
        B::Cos => G::COS,
        B::Cosh => G::COSH,
        B::Cross => G::CROSS,
        B::Degrees => G::DEGREES,
        B::Determinant => G::DETERMINANT,
        B::Distance => G::DISTANCE,
        B::Exp => G::EXP,
        B::Exp2 => G::EXP2,
        B::FaceForward => G::FACE_FORWARD,
        B::FirstTrailingBit => G::FIND_I_LSB,
        B::Floor => G::FLOOR,
        B::Fma => G::FMA,
        B::Fract => G::FRACT,
        B::Frexp => G::FREXP_STRUCT,
        B::InverseSqrt => G::INVERSE_SQRT,
        B::Ldexp => G::LDEXP,
        B::Length => G::LENGTH,
        B::Log => G::LOG,
        B::Log2 => G::LOG2,
        B::Modf => G::MODF_STRUCT,
        B::Normalize => G::NORMALIZE,
        B::Pack2x16Float => G::PACK_HALF2X16,
        B::Pow => G::POW,
        B::Radians => G::RADIANS,
        B::Reflect => G::REFLECT,
        B::Refract => G::REFRACT,
        B::Round => G::ROUND_EVEN,
        B::Sin => G::SIN,
        B::Sinh => G::SINH,
        B::Sqrt => G::SQRT,
        B::Step => G::STEP,
        B::Tan => G::TAN,
        B::Tanh => G::TANH,
        B::Trunc => G::TRUNC,
        B::Unpack2x16Float => G::UNPACK_HALF2X16,
        B::Unpack2x16Snorm => G::UNPACK_SNORM2X16,
        B::Unpack2x16Unorm => G::UNPACK_UNORM2X16,
        B::Unpack4x8Snorm => G::UNPACK_SNORM4X8,
        B::Unpack4x8Unorm => G::UNPACK_UNORM4X8,
        _ => return None,
    })
}

/// A derivative's instruction, and whether it names its precision.
const fn derivative(builtin: Builtin) -> Option<(Op, bool)> {
    use Builtin as B;
    Some(match builtin {
        B::Dpdx => (Op::D_PDX, false),
        B::DpdxCoarse => (Op::D_PDX_COARSE, true),
        B::DpdxFine => (Op::D_PDX_FINE, true),
        B::Dpdy => (Op::D_PDY, false),
        B::DpdyCoarse => (Op::D_PDY_COARSE, true),
        B::DpdyFine => (Op::D_PDY_FINE, true),
        B::Fwidth => (Op::FWIDTH, false),
        B::FwidthCoarse => (Op::FWIDTH_COARSE, true),
        B::FwidthFine => (Op::FWIDTH_FINE, true),
        _ => return None,
    })
}

/// How a packing built-in turns each component into its field: a float's
/// clamped bounds and scale, an integer's bounds where it saturates.
#[derive(Copy, Clone)]
enum Field {
    Float { low: f32, scale: f32 },
    Integer { saturate: Option<(i32, i32)> },
}

/// A packing built-in's lanes, each lane's width in bits, and its field.
const fn packing(builtin: Builtin) -> Option<(u8, u32, Field)> {
    use Builtin as B;
    Some(match builtin {
        B::Pack4x8Snorm => (
            4,
            8,
            Field::Float {
                low: -1.0,
                scale: 127.0,
            },
        ),
        B::Pack4x8Unorm => (
            4,
            8,
            Field::Float {
                low: 0.0,
                scale: 255.0,
            },
        ),
        B::Pack2x16Snorm => (
            2,
            16,
            Field::Float {
                low: -1.0,
                scale: 32767.0,
            },
        ),
        B::Pack2x16Unorm => (
            2,
            16,
            Field::Float {
                low: 0.0,
                scale: 65535.0,
            },
        ),
        B::Pack4xI8 | B::Pack4xU8 => (4, 8, Field::Integer { saturate: None }),
        B::Pack4xI8Clamp => (
            4,
            8,
            Field::Integer {
                saturate: Some((-128, 127)),
            },
        ),
        B::Pack4xU8Clamp => (
            4,
            8,
            Field::Integer {
                saturate: Some((0, 255)),
            },
        ),
        _ => return None,
    })
}

impl Writer<'_, '_, '_> {
    /// The type argument `at` was taken as.
    fn arg_type(&self, exprs: &[ExprId], at: usize) -> Outcome<TyId> {
        exprs
            .get(at)
            .map(|&e| self.c.info(e).converted)
            .ok_or(Stop::Internal)
    }

    fn lanes(&self, ty: TyId) -> Option<u8> {
        match self.c.types.get(ty) {
            Ty::Vector(n, _) => Some(n),
            _ => None,
        }
    }

    fn scalar(&self, ty: TyId) -> Outcome<Scalar> {
        self.c.types.scalar_of(ty).ok_or(Stop::Internal)
    }

    fn ext(&mut self, inst: GlslStd450, ty: Id, operands: &[Id]) -> Outcome<Id> {
        self.f.glsl(inst, ty, operands).map_err(built)
    }

    /// The float `x` of `ty`'s scalar, across its lanes.
    fn float_of(&mut self, x: f32, ty: TyId) -> Outcome<Id> {
        let scalar = self.scalar(ty)?;
        self.splat_constant(float_bits(x, scalar), self.lanes(ty), scalar)
    }

    /// Built-in `builtin` called at `id` over `args`.
    #[expect(clippy::too_many_lines, reason = "one arm per built-in family")]
    pub(crate) fn builtin(
        &mut self,
        id: ExprId,
        builtin: Builtin,
        exprs: &[ExprId],
        args: &[Lowered],
    ) -> Outcome<Lowered> {
        use Builtin as B;
        let result = self.c.info(id).ty;
        if let Some(inst) = same_as_glsl(builtin) {
            let ty = self.value_type(result)?;
            let mut operands = [value(args, 0)?; 3];
            for (slot, lowered) in operands.iter_mut().zip(args) {
                *slot = lowered.value()?;
            }
            let operands = operands.get(..args.len()).ok_or(Stop::Internal)?;
            return self.ext(inst, ty, operands).map(Lowered::Value);
        }
        if let Some((op, control)) = derivative(builtin) {
            if control {
                self.f
                    .module()
                    .capability(Capability::DERIVATIVE_CONTROL)
                    .map_err(built)?;
            }
            let ty = self.value_type(result)?;
            return Ok(Lowered::Value(
                self.f.op(op, ty, &[value(args, 0)?]).map_err(built)?,
            ));
        }
        if let Some(form) = packing(builtin) {
            let ty = self.arg_type(exprs, 0)?;
            return self.pack(value(args, 0)?, ty, form).map(Lowered::Value);
        }
        let written = match builtin {
            B::Bitcast => {
                let from = self.arg_type(exprs, 0)?;
                let x = value(args, 0)?;
                if from == result {
                    x
                } else {
                    let ty = self.value_type(result)?;
                    self.f.op(Op::BITCAST, ty, &[x]).map_err(built)?
                }
            }
            B::All | B::Any => {
                let x = value(args, 0)?;
                if self.lanes(self.arg_type(exprs, 0)?).is_none() {
                    x
                } else {
                    let op = if builtin == B::All { Op::ALL } else { Op::ANY };
                    let ty = self.value_type(result)?;
                    self.f.op(op, ty, &[x]).map_err(built)?
                }
            }
            B::Select => {
                let (f, t, condition) = (value(args, 0)?, value(args, 1)?, value(args, 2)?);
                let condition = match (self.lanes(result), self.lanes(self.arg_type(exprs, 2)?)) {
                    // SPIR-V 1.3 selects between vectors only by a vector.
                    (Some(n), None) => self.splat(condition, n, Scalar::Bool)?,
                    _ => condition,
                };
                let ty = self.value_type(result)?;
                self.f
                    .op(Op::SELECT, ty, &[condition, t, f])
                    .map_err(built)?
            }
            B::ArrayLength => self.array_length(args.first().ok_or(Stop::Internal)?.place()?)?,
            B::Abs | B::Sign | B::Max | B::Min | B::Clamp => self.numeric(builtin, args, result)?,
            B::Saturate => {
                let ty = self.value_type(result)?;
                let (zero, one) = (self.float_of(0.0, result)?, self.float_of(1.0, result)?);
                self.ext(GlslStd450::N_CLAMP, ty, &[value(args, 0)?, zero, one])?
            }
            B::Mix => {
                let ty = self.value_type(result)?;
                let mut t = value(args, 2)?;
                if let (Some(n), None) = (self.lanes(result), self.lanes(self.arg_type(exprs, 2)?))
                {
                    t = self.splat(t, n, self.scalar(result)?)?;
                }
                self.ext(
                    GlslStd450::F_MIX,
                    ty,
                    &[value(args, 0)?, value(args, 1)?, t],
                )?
            }
            B::Smoothstep => {
                self.smoothstep([value(args, 0)?, value(args, 1)?, value(args, 2)?], result)?
            }
            B::Dot => self.dot(
                value(args, 0)?,
                value(args, 1)?,
                self.arg_type(exprs, 0)?,
                result,
            )?,
            B::Dot4U8Packed | B::Dot4I8Packed => {
                self.packed_dot(value(args, 0)?, value(args, 1)?, builtin == B::Dot4I8Packed)?
            }
            B::Transpose | B::QuantizeToF16 | B::CountOneBits | B::ReverseBits => {
                let op = match builtin {
                    B::Transpose => Op::TRANSPOSE,
                    B::QuantizeToF16 => Op::QUANTIZE_TO_F16,
                    B::CountOneBits => Op::BIT_COUNT,
                    _ => Op::BIT_REVERSE,
                };
                let ty = self.value_type(result)?;
                self.f.op(op, ty, &[value(args, 0)?]).map_err(built)?
            }
            B::CountLeadingZeros => {
                // The most significant set bit counted down from 31; none, -1,
                // counts 32.
                let ty = self.value_type(result)?;
                let msb = self.ext(GlslStd450::FIND_U_MSB, ty, &[value(args, 0)?])?;
                let top = self.splat_constant(31, self.lanes(result), self.scalar(result)?)?;
                self.f.op(Op::I_SUB, ty, &[top, msb]).map_err(built)?
            }
            B::CountTrailingZeros => {
                // The least significant set bit; none, -1 as unsigned, counts 32.
                let ty = self.value_type(result)?;
                let lsb = self.ext(GlslStd450::FIND_I_LSB, ty, &[value(args, 0)?])?;
                let width = self.splat_constant(32, self.lanes(result), self.scalar(result)?)?;
                self.ext(GlslStd450::U_MIN, ty, &[lsb, width])?
            }
            B::FirstLeadingBit => {
                let ty = self.value_type(result)?;
                let inst = if self.scalar(result)? == Scalar::I32 {
                    GlslStd450::FIND_S_MSB
                } else {
                    GlslStd450::FIND_U_MSB
                };
                self.ext(inst, ty, &[value(args, 0)?])?
            }
            B::ExtractBits | B::InsertBits => self.bit_field(builtin, args, result)?,
            B::Unpack4xI8 | B::Unpack4xU8 => {
                self.unpack_bytes(value(args, 0)?, builtin == B::Unpack4xI8)?
            }
            B::StorageBarrier => {
                return self
                    .barrier(MemorySemantics::UNIFORM_MEMORY)
                    .map(|()| Lowered::Void)
            }
            B::WorkgroupBarrier => {
                return self
                    .barrier(MemorySemantics::WORKGROUP_MEMORY)
                    .map(|()| Lowered::Void)
            }
            B::TextureBarrier => {
                return self
                    .barrier(MemorySemantics::IMAGE_MEMORY)
                    .map(|()| Lowered::Void)
            }
            B::WorkgroupUniformLoad => {
                let place = args.first().ok_or(Stop::Internal)?.place()?;
                self.barrier(MemorySemantics::WORKGROUP_MEMORY)?;
                let loaded = match self.c.types.get(place.ty) {
                    Ty::Atomic(scalar) => self.atomic_load(place, scalar)?,
                    _ => self.load(place)?,
                };
                self.barrier(MemorySemantics::WORKGROUP_MEMORY)?;
                loaded
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
            | B::AtomicCompareExchangeWeak => return self.atomic(builtin, args, result),
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
            | B::TextureStore => return self.texture(builtin, exprs, args, result),
            _ => return Err(Stop::Internal),
        };
        Ok(Lowered::Value(written))
    }

    /// `abs`, `sign`, `max`, `min` and `clamp`: a float's minima and maxima
    /// return the operand that is not NaN, and a clamp whose bounds cross is
    /// the minimum of the maximum, as WGSL defines both.
    fn numeric(&mut self, builtin: Builtin, args: &[Lowered], result: TyId) -> Outcome<Id> {
        use Builtin as B;
        use GlslStd450 as G;
        let ty = self.value_type(result)?;
        let scalar = self.scalar(result)?;
        let x = value(args, 0)?;
        let (min, max) = match scalar {
            s if s.is_float() => (G::N_MIN, G::N_MAX),
            Scalar::I32 => (G::S_MIN, G::S_MAX),
            _ => (G::U_MIN, G::U_MAX),
        };
        match (builtin, scalar) {
            (B::Abs, s) if s.is_float() => self.ext(G::F_ABS, ty, &[x]),
            (B::Abs, Scalar::I32) => {
                // Negation wraps, so the most negative value is its own.
                let bool_ty = self.bool_type(self.lanes(result))?;
                let zero = self.splat_constant(0, self.lanes(result), scalar)?;
                let negative = self
                    .f
                    .op(Op::S_LESS_THAN, bool_ty, &[x, zero])
                    .map_err(built)?;
                let negated = self.f.op(Op::S_NEGATE, ty, &[x]).map_err(built)?;
                self.f
                    .op(Op::SELECT, ty, &[negative, negated, x])
                    .map_err(built)
            }
            (B::Abs, _) => Ok(x),
            (B::Sign, s) if s.is_float() => self.ext(G::F_SIGN, ty, &[x]),
            (B::Sign, _) => self.ext(G::S_SIGN, ty, &[x]),
            (B::Max, _) => self.ext(max, ty, &[x, value(args, 1)?]),
            (B::Min, _) => self.ext(min, ty, &[x, value(args, 1)?]),
            _ => {
                let above = self.ext(max, ty, &[x, value(args, 1)?])?;
                self.ext(min, ty, &[above, value(args, 2)?])
            }
        }
    }

    /// `smoothstep(low, high, x)`: `t * t * (3 - 2 t)` for `t` the clamped
    /// position of `x` between the edges, whichever way they lie.
    fn smoothstep(&mut self, [low, high, x]: [Id; 3], result: TyId) -> Outcome<Id> {
        let ty = self.value_type(result)?;
        let (zero, one) = (self.float_of(0.0, result)?, self.float_of(1.0, result)?);
        let (two, three) = (self.float_of(2.0, result)?, self.float_of(3.0, result)?);
        let along = self.f.op(Op::F_SUB, ty, &[x, low]).map_err(built)?;
        let span = self.f.op(Op::F_SUB, ty, &[high, low]).map_err(built)?;
        let ratio = self.f.op(Op::F_DIV, ty, &[along, span]).map_err(built)?;
        let t = self.ext(GlslStd450::N_CLAMP, ty, &[ratio, zero, one])?;
        let squared = self.f.op(Op::F_MUL, ty, &[t, t]).map_err(built)?;
        let doubled = self.f.op(Op::F_MUL, ty, &[two, t]).map_err(built)?;
        let rest = self.f.op(Op::F_SUB, ty, &[three, doubled]).map_err(built)?;
        self.f.op(Op::F_MUL, ty, &[squared, rest]).map_err(built)
    }

    /// `dot`: a float's by its instruction, an integer's as the wrapping
    /// sum of its products, which SPIR-V 1.3 has no instruction for.
    fn dot(&mut self, a: Id, b: Id, vector: TyId, result: TyId) -> Outcome<Id> {
        let ty = self.value_type(result)?;
        let scalar = self.scalar(result)?;
        if scalar.is_float() {
            return self.f.op(Op::DOT, ty, &[a, b]).map_err(built);
        }
        let lanes = self.lanes(vector).ok_or(Stop::Internal)?;
        let mut sum = None;
        for lane in 0..u32::from(lanes) {
            let x = self
                .f
                .op_args(Op::COMPOSITE_EXTRACT, ty, &[Arg::Id(a), Arg::Word(lane)])
                .map_err(built)?;
            let y = self
                .f
                .op_args(Op::COMPOSITE_EXTRACT, ty, &[Arg::Id(b), Arg::Word(lane)])
                .map_err(built)?;
            let product = self.f.op(Op::I_MUL, ty, &[x, y]).map_err(built)?;
            sum = Some(match sum {
                Some(total) => self.f.op(Op::I_ADD, ty, &[total, product]).map_err(built)?,
                None => product,
            });
        }
        sum.ok_or(Stop::Internal)
    }

    /// `dot4U8Packed` and `dot4I8Packed`: the sum of the products of the
    /// four bytes of each word, signed or unsigned.
    fn packed_dot(&mut self, a: Id, b: Id, signed: bool) -> Outcome<Id> {
        let scalar = if signed { Scalar::I32 } else { Scalar::U32 };
        let ty = self.lower().scalar(scalar)?;
        let (a, b) = if signed {
            (
                self.f.op(Op::BITCAST, ty, &[a]).map_err(built)?,
                self.f.op(Op::BITCAST, ty, &[b]).map_err(built)?,
            )
        } else {
            (a, b)
        };
        let extract = if signed {
            Op::BIT_FIELD_S_EXTRACT
        } else {
            Op::BIT_FIELD_U_EXTRACT
        };
        let width = self.lower().uint(8)?;
        let mut sum = None;
        for byte in 0..4 {
            let offset = self.lower().uint(byte * 8)?;
            let x = self.f.op(extract, ty, &[a, offset, width]).map_err(built)?;
            let y = self.f.op(extract, ty, &[b, offset, width]).map_err(built)?;
            let product = self.f.op(Op::I_MUL, ty, &[x, y]).map_err(built)?;
            sum = Some(match sum {
                Some(total) => self.f.op(Op::I_ADD, ty, &[total, product]).map_err(built)?,
                None => product,
            });
        }
        sum.ok_or(Stop::Internal)
    }

    /// `extractBits` and `insertBits`: the offset clamped to the width and
    /// the count to what is left above it, as WGSL defines both.
    fn bit_field(&mut self, builtin: Builtin, args: &[Lowered], result: TyId) -> Outcome<Id> {
        let ty = self.value_type(result)?;
        let u32_ty = self.lower().scalar(Scalar::U32)?;
        let at = if builtin == Builtin::ExtractBits {
            1
        } else {
            2
        };
        let width = self.lower().uint(32)?;
        let offset = self.ext(GlslStd450::U_MIN, u32_ty, &[value(args, at)?, width])?;
        let room = self
            .f
            .op(Op::I_SUB, u32_ty, &[width, offset])
            .map_err(built)?;
        let count = self.ext(GlslStd450::U_MIN, u32_ty, &[value(args, at + 1)?, room])?;
        let e = value(args, 0)?;
        if builtin == Builtin::InsertBits {
            return self
                .f
                .op(
                    Op::BIT_FIELD_INSERT,
                    ty,
                    &[e, value(args, 1)?, offset, count],
                )
                .map_err(built);
        }
        let op = if self.scalar(result)? == Scalar::I32 {
            Op::BIT_FIELD_S_EXTRACT
        } else {
            Op::BIT_FIELD_U_EXTRACT
        };
        self.f.op(op, ty, &[e, offset, count]).map_err(built)
    }

    /// A packing into a `u32`: each lane made its field — a float clamped,
    /// scaled and rounded half up, an integer saturated where the built-in
    /// saturates — and laid at its place, the first lane lowest.
    fn pack(&mut self, x: Id, ty: TyId, (lanes, bits, field): (u8, u32, Field)) -> Outcome<Id> {
        let u32_ty = self.lower().scalar(Scalar::U32)?;
        let words = self.lower().vector_of(lanes, Scalar::U32)?;
        let source = self.value_type(ty)?;
        let fields = match field {
            Field::Float { low, scale } => {
                let unsigned = low == 0.0;
                let (low, high) = (self.float_of(low, ty)?, self.float_of(1.0, ty)?);
                let (scale, half) = (self.float_of(scale, ty)?, self.float_of(0.5, ty)?);
                let above = self.ext(GlslStd450::N_MAX, source, &[low, x])?;
                let clamped = self.ext(GlslStd450::N_MIN, source, &[high, above])?;
                let scaled = self
                    .f
                    .op(Op::F_MUL, source, &[clamped, scale])
                    .map_err(built)?;
                let raised = self
                    .f
                    .op(Op::F_ADD, source, &[scaled, half])
                    .map_err(built)?;
                let rounded = self.ext(GlslStd450::FLOOR, source, &[raised])?;
                if unsigned {
                    self.f
                        .op(Op::CONVERT_F_TO_U, words, &[rounded])
                        .map_err(built)?
                } else {
                    let signed = self.lower().vector_of(lanes, Scalar::I32)?;
                    let integers = self
                        .f
                        .op(Op::CONVERT_F_TO_S, signed, &[rounded])
                        .map_err(built)?;
                    self.f.op(Op::BITCAST, words, &[integers]).map_err(built)?
                }
            }
            Field::Integer { saturate } => {
                let scalar = self.scalar(ty)?;
                // Constant bounds that never cross, so the clamp is defined.
                let saturated = match saturate {
                    Some((low, high)) => {
                        let inst = if scalar == Scalar::I32 {
                            GlslStd450::S_CLAMP
                        } else {
                            GlslStd450::U_CLAMP
                        };
                        let low = self.splat_constant(low.cast_unsigned(), Some(lanes), scalar)?;
                        let high =
                            self.splat_constant(high.cast_unsigned(), Some(lanes), scalar)?;
                        self.ext(inst, source, &[x, low, high])?
                    }
                    None => x,
                };
                if scalar == Scalar::I32 {
                    self.f.op(Op::BITCAST, words, &[saturated]).map_err(built)?
                } else {
                    saturated
                }
            }
        };
        let width = self.lower().uint(bits)?;
        let mut word = self.lower().uint(0)?;
        for lane in 0..u32::from(lanes) {
            let part = self
                .f
                .op_args(
                    Op::COMPOSITE_EXTRACT,
                    u32_ty,
                    &[Arg::Id(fields), Arg::Word(lane)],
                )
                .map_err(built)?;
            let offset = self.lower().uint(lane * bits)?;
            word = self
                .f
                .op(Op::BIT_FIELD_INSERT, u32_ty, &[word, part, offset, width])
                .map_err(built)?;
        }
        Ok(word)
    }

    /// `unpack4xI8` and `unpack4xU8`: each byte of the word a lane, signed
    /// or not.
    fn unpack_bytes(&mut self, word: Id, signed: bool) -> Outcome<Id> {
        let scalar = if signed { Scalar::I32 } else { Scalar::U32 };
        let ty = self.lower().scalar(scalar)?;
        let vector = self.lower().vector_of(4, scalar)?;
        let (word, extract) = if signed {
            (
                self.f.op(Op::BITCAST, ty, &[word]).map_err(built)?,
                Op::BIT_FIELD_S_EXTRACT,
            )
        } else {
            (word, Op::BIT_FIELD_U_EXTRACT)
        };
        let width = self.lower().uint(8)?;
        let mut bytes = [word; 4];
        for (byte, slot) in (0..4).zip(&mut bytes) {
            let offset = self.lower().uint(byte * 8)?;
            *slot = self
                .f
                .op(extract, ty, &[word, offset, width])
                .map_err(built)?;
        }
        self.f
            .op(Op::COMPOSITE_CONSTRUCT, vector, &bytes)
            .map_err(built)
    }

    /// `arrayLength`: the runtime-sized array a pointer reaches is the last
    /// member of its buffer's block — the wrapper's only one, or the
    /// structure that is the block.
    fn array_length(&mut self, place: &Place) -> Outcome<Id> {
        let member = match (place.wrapped, place.indexes.as_slice()) {
            (true, []) => 0,
            (false, [Index::Const(member)]) => *member,
            _ => return Err(Stop::Internal),
        };
        let u32_ty = self.lower().scalar(Scalar::U32)?;
        self.f
            .op_args(
                Op::ARRAY_LENGTH,
                u32_ty,
                &[Arg::Id(place.base), Arg::Word(member)],
            )
            .map_err(built)
    }

    /// A workgroup-wide control barrier ordering the memory `memory` names.
    fn barrier(&mut self, memory: MemorySemantics) -> Outcome<()> {
        let workgroup = self.lower().uint(Scope::WORKGROUP.0)?;
        let semantics = self
            .lower()
            .uint(MemorySemantics::ACQUIRE_RELEASE.0 | memory.0)?;
        self.f
            .emit(Op::CONTROL_BARRIER, &[workgroup, workgroup, semantics])
            .map_err(built)
    }

    /// The scope an atomic in `space` is shared within.
    fn atomic_scope(&mut self, space: Space) -> Outcome<Id> {
        let scope = if space == Space::Workgroup {
            Scope::WORKGROUP
        } else {
            Scope::DEVICE
        };
        self.lower().uint(scope.0)
    }

    fn atomic_load(&mut self, place: &Place, scalar: Scalar) -> Outcome<Id> {
        let ty = self.lower().scalar(scalar)?;
        let pointer = self.pointer_of(place)?;
        let scope = self.atomic_scope(place.space)?;
        let relaxed = self.lower().uint(MemorySemantics::RELAXED.0)?;
        self.f
            .op(Op::ATOMIC_LOAD, ty, &[pointer, scope, relaxed])
            .map_err(built)
    }

    /// An atomic built-in, relaxed as WGSL's atomics are; one on storage
    /// memory makes no change once the invocation has discarded.
    fn atomic(&mut self, builtin: Builtin, args: &[Lowered], result: TyId) -> Outcome<Lowered> {
        use Builtin as B;
        let place = args.first().ok_or(Stop::Internal)?.place()?;
        let Ty::Atomic(scalar) = self.c.types.get(place.ty) else {
            return Err(Stop::Internal);
        };
        if builtin == B::AtomicLoad {
            return self.atomic_load(place, scalar).map(Lowered::Value);
        }
        let ty = self.lower().scalar(scalar)?;
        let pointer = self.pointer_of(place)?;
        let scope = self.atomic_scope(place.space)?;
        let relaxed = self.lower().uint(MemorySemantics::RELAXED.0)?;
        let masked = place.space == Space::Storage;
        let operand = value(args, 1)?;
        if builtin == B::AtomicStore {
            self.unless_discarded(masked, |w| {
                w.f.emit(Op::ATOMIC_STORE, &[pointer, scope, relaxed, operand])
                    .map_err(built)
            })?;
            return Ok(Lowered::Void);
        }
        if builtin == B::AtomicCompareExchangeWeak {
            let replacement = value(args, 2)?;
            let old = self.discarded_or(masked, ty, |w| {
                w.f.op(
                    Op::ATOMIC_COMPARE_EXCHANGE,
                    ty,
                    &[pointer, scope, relaxed, relaxed, replacement, operand],
                )
                .map_err(built)
            })?;
            let bool_ty = self.lower().scalar(Scalar::Bool)?;
            let exchanged = self
                .f
                .op(Op::I_EQUAL, bool_ty, &[old, operand])
                .map_err(built)?;
            let pair = self.value_type(result)?;
            return Ok(Lowered::Value(
                self.f
                    .op(Op::COMPOSITE_CONSTRUCT, pair, &[old, exchanged])
                    .map_err(built)?,
            ));
        }
        let signed = scalar == Scalar::I32;
        let op = match builtin {
            B::AtomicAdd => Op::ATOMIC_I_ADD,
            B::AtomicSub => Op::ATOMIC_I_SUB,
            B::AtomicMax if signed => Op::ATOMIC_S_MAX,
            B::AtomicMax => Op::ATOMIC_U_MAX,
            B::AtomicMin if signed => Op::ATOMIC_S_MIN,
            B::AtomicMin => Op::ATOMIC_U_MIN,
            B::AtomicAnd => Op::ATOMIC_AND,
            B::AtomicOr => Op::ATOMIC_OR,
            B::AtomicXor => Op::ATOMIC_XOR,
            B::AtomicExchange => Op::ATOMIC_EXCHANGE,
            _ => return Err(Stop::Internal),
        };
        self.discarded_or(masked, ty, |w| {
            w.f.op(op, ty, &[pointer, scope, relaxed, operand])
                .map_err(built)
        })
        .map(Lowered::Value)
    }

    /// The image a texture's handle holds, and the texture's shape.
    fn image(&mut self, lowered: &Lowered, ty: TyId) -> Outcome<(Id, Texture)> {
        let Ty::Texture(texture) = self.c.types.get(ty) else {
            return Err(Stop::Internal);
        };
        let image_ty = self.lower().ty(ty, Flavour::Plain)?;
        let image = self
            .f
            .op(Op::LOAD, image_ty, &[lowered.handle()?])
            .map_err(built)?;
        Ok((image, texture))
    }

    /// A texture built-in, its arguments from the texture on: the
    /// coordinates, an array index where the texture is arrayed, what the
    /// function reads beside, and an offset where one is given.
    fn texture(
        &mut self,
        builtin: Builtin,
        exprs: &[ExprId],
        args: &[Lowered],
        result: TyId,
    ) -> Outcome<Lowered> {
        use Builtin as B;
        let gathers_component = builtin == B::TextureGather
            && !matches!(self.c.types.get(self.arg_type(exprs, 0)?), Ty::Texture(_));
        let component = if gathers_component {
            Some(value(args, 0)?)
        } else {
            None
        };
        let at = usize::from(gathers_component);
        let texture_ty = self.arg_type(exprs, at)?;
        let (image, texture) = self.image(args.get(at).ok_or(Stop::Internal)?, texture_ty)?;
        let args = args.get(at..).ok_or(Stop::Internal)?;
        let exprs = exprs.get(at..).ok_or(Stop::Internal)?;
        let ty = self.value_type(result)?;
        Ok(Lowered::Value(match builtin {
            B::TextureDimensions | B::TextureNumLayers => {
                self.f
                    .module()
                    .capability(Capability::IMAGE_QUERY)
                    .map_err(built)?;
                let dim = texture.dim();
                let components =
                    dim.coordinates() - u8::from(dim.is_cube()) + u8::from(dim.is_arrayed());
                let size = if components == 1 {
                    ty
                } else {
                    self.lower().vector_of(components, Scalar::U32)?
                };
                let lod = matches!(
                    texture,
                    Texture::Sampled { .. } | Texture::Depth { .. } | Texture::External
                );
                let queried = if lod {
                    let level = match args.get(1) {
                        Some(level) => level.value()?,
                        None => self.lower().uint(0)?,
                    };
                    self.f
                        .op(Op::IMAGE_QUERY_SIZE_LOD, size, &[image, level])
                        .map_err(built)?
                } else {
                    self.f
                        .op(Op::IMAGE_QUERY_SIZE, size, &[image])
                        .map_err(built)?
                };
                match (builtin, dim.is_arrayed()) {
                    (B::TextureNumLayers, _) => {
                        let last = u32::from(components - 1);
                        self.f
                            .op_args(
                                Op::COMPOSITE_EXTRACT,
                                ty,
                                &[Arg::Id(queried), Arg::Word(last)],
                            )
                            .map_err(built)?
                    }
                    (_, true) => self.shuffle(queried, ty, &[0, 1])?,
                    (_, false) => queried,
                }
            }
            B::TextureNumLevels | B::TextureNumSamples => {
                self.f
                    .module()
                    .capability(Capability::IMAGE_QUERY)
                    .map_err(built)?;
                let op = if builtin == B::TextureNumLevels {
                    Op::IMAGE_QUERY_LEVELS
                } else {
                    Op::IMAGE_QUERY_SAMPLES
                };
                self.f.op(op, ty, &[image]).map_err(built)?
            }
            B::TextureLoad => self.texture_load(image, texture, exprs, args, ty)?,
            B::TextureStore => {
                let (coords, next) = self.integer_coordinates(texture, exprs, args)?;
                let texel = value(args, next)?;
                self.unless_discarded(true, |w| {
                    w.f.emit(Op::IMAGE_WRITE, &[image, coords, texel])
                        .map_err(built)
                })?;
                return Ok(Lowered::Void);
            }
            _ => self.sample(
                builtin,
                (image, texture, texture_ty),
                component,
                exprs,
                args,
                ty,
            )?,
        }))
    }

    /// The texel scalar a texture's reads give.
    const fn texel(texture: Texture) -> Scalar {
        match texture {
            Texture::Sampled { texel, .. } | Texture::Multisampled { texel } => texel,
            Texture::Storage { format, .. } => format.channel(),
            Texture::Depth { .. } | Texture::DepthMultisampled | Texture::External => Scalar::F32,
        }
    }

    /// `coords`, `n` components, with the array index `layer` of type
    /// `layer_ty` appended as a `scalar`.
    fn with_layer(
        &mut self,
        coords: Id,
        n: u8,
        (layer, layer_ty): (Id, TyId),
        scalar: Scalar,
    ) -> Outcome<Id> {
        let from = self.scalar(layer_ty)?;
        let target = self.lower().scalar(scalar)?;
        let layer = match (from, scalar) {
            (from, to) if from == to => layer,
            (Scalar::I32, to) if to.is_float() => self
                .f
                .op(Op::CONVERT_S_TO_F, target, &[layer])
                .map_err(built)?,
            (_, to) if to.is_float() => self
                .f
                .op(Op::CONVERT_U_TO_F, target, &[layer])
                .map_err(built)?,
            _ => self.f.op(Op::BITCAST, target, &[layer]).map_err(built)?,
        };
        let vector = self.lower().vector_of(n + 1, scalar)?;
        self.f
            .op(Op::COMPOSITE_CONSTRUCT, vector, &[coords, layer])
            .map_err(built)
    }

    /// A load's or store's integer coordinates, an arrayed texture's layer
    /// appended, and the argument after them.
    fn integer_coordinates(
        &mut self,
        texture: Texture,
        exprs: &[ExprId],
        args: &[Lowered],
    ) -> Outcome<(Id, usize)> {
        let coords = value(args, 1)?;
        let dim = texture.dim();
        if !dim.is_arrayed() {
            return Ok((coords, 2));
        }
        let scalar = self.scalar(self.arg_type(exprs, 1)?)?;
        let layer = (value(args, 2)?, self.arg_type(exprs, 2)?);
        Ok((
            self.with_layer(coords, dim.coordinates(), layer, scalar)?,
            3,
        ))
    }

    /// `textureLoad`: a storage texture's texel read, any other's fetched
    /// at its level or sample — an external texture's at its only level —
    /// a depth texture's depth its first component.
    fn texture_load(
        &mut self,
        image: Id,
        texture: Texture,
        exprs: &[ExprId],
        args: &[Lowered],
        ty: Id,
    ) -> Outcome<Id> {
        let (coords, next) = self.integer_coordinates(texture, exprs, args)?;
        let vec4 = self.lower().vector_of(4, Self::texel(texture))?;
        let (operand, at) = match texture {
            Texture::Storage { .. } => {
                return self
                    .f
                    .op(Op::IMAGE_READ, vec4, &[image, coords])
                    .map_err(built);
            }
            Texture::External => (ImageOperands::LOD, self.lower().uint(0)?),
            Texture::Multisampled { .. } | Texture::DepthMultisampled => {
                (ImageOperands::SAMPLE, value(args, next)?)
            }
            Texture::Sampled { .. } | Texture::Depth { .. } => {
                (ImageOperands::LOD, value(args, next)?)
            }
        };
        let fetched = self
            .f
            .op_args(
                Op::IMAGE_FETCH,
                vec4,
                &[
                    Arg::Id(image),
                    Arg::Id(coords),
                    Arg::Word(operand.0),
                    Arg::Id(at),
                ],
            )
            .map_err(built)?;
        match texture {
            Texture::Depth { .. } | Texture::DepthMultisampled => self
                .f
                .op_args(Op::COMPOSITE_EXTRACT, ty, &[Arg::Id(fetched), Arg::Word(0)])
                .map_err(built),
            _ => Ok(fetched),
        }
    }

    /// The sampling and gathering built-ins: the image sampled through its
    /// sampler at its coordinates, an arrayed texture's layer appended as a
    /// float; a depth texture sampled without comparison gives its depth as
    /// the first component.
    #[expect(clippy::too_many_lines, reason = "one arm per sampling built-in")]
    fn sample(
        &mut self,
        builtin: Builtin,
        (image, texture, texture_ty): (Id, Texture, TyId),
        component: Option<Id>,
        exprs: &[ExprId],
        args: &[Lowered],
        ty: Id,
    ) -> Outcome<Id> {
        use Builtin as B;
        let sampler_ty = self.arg_type(exprs, 1)?;
        let sampler_type = self.lower().ty(sampler_ty, Flavour::Plain)?;
        let sampler = self
            .f
            .op(
                Op::LOAD,
                sampler_type,
                &[args.get(1).ok_or(Stop::Internal)?.handle()?],
            )
            .map_err(built)?;
        let image_type = self.lower().ty(texture_ty, Flavour::Plain)?;
        let pair_type = self.f.module().sampled_image(image_type).map_err(built)?;
        let combined = self
            .f
            .op(Op::SAMPLED_IMAGE, pair_type, &[image, sampler])
            .map_err(built)?;
        let dim = texture.dim();
        let mut coords = value(args, 2)?;
        let mut next = 3;
        if dim.is_arrayed() {
            let layer = (value(args, 3)?, self.arg_type(exprs, 3)?);
            coords = self.with_layer(coords, dim.coordinates(), layer, Scalar::F32)?;
            next = 4;
        }
        let depth = matches!(texture, Texture::Depth { .. });
        let vec4 = self.lower().vector_of(4, Self::texel(texture))?;
        if builtin == B::TextureSampleBaseClampToEdge {
            return self.sample_clamped(image, combined, coords, vec4);
        }
        let mut mask = 0;
        let mut operands = [coords; 3];
        let mut count = 0;
        let mut third = None;
        let op = match builtin {
            B::TextureSample => Op::IMAGE_SAMPLE_IMPLICIT_LOD,
            B::TextureSampleBias => {
                mask |= ImageOperands::BIAS.0;
                operands[0] = value(args, next)?;
                count = 1;
                next += 1;
                Op::IMAGE_SAMPLE_IMPLICIT_LOD
            }
            B::TextureSampleCompare => {
                third = Some(value(args, next)?);
                next += 1;
                Op::IMAGE_SAMPLE_DREF_IMPLICIT_LOD
            }
            B::TextureSampleCompareLevel => {
                third = Some(value(args, next)?);
                next += 1;
                mask |= ImageOperands::LOD.0;
                operands[0] = self.f.module().real(0.0).map_err(built)?;
                count = 1;
                Op::IMAGE_SAMPLE_DREF_EXPLICIT_LOD
            }
            B::TextureSampleGrad => {
                mask |= ImageOperands::GRAD.0;
                operands[0] = value(args, next)?;
                operands[1] = value(args, next + 1)?;
                count = 2;
                next += 2;
                Op::IMAGE_SAMPLE_EXPLICIT_LOD
            }
            B::TextureSampleLevel => {
                mask |= ImageOperands::LOD.0;
                let level_ty = self.arg_type(exprs, next)?;
                let f32_ty = self.lower().scalar(Scalar::F32)?;
                // A depth texture's level is an integer; a sampled one's is
                // a float.
                operands[0] = match self.scalar(level_ty)? {
                    Scalar::I32 => self
                        .f
                        .op(Op::CONVERT_S_TO_F, f32_ty, &[value(args, next)?])
                        .map_err(built)?,
                    Scalar::U32 => self
                        .f
                        .op(Op::CONVERT_U_TO_F, f32_ty, &[value(args, next)?])
                        .map_err(built)?,
                    _ => value(args, next)?,
                };
                count = 1;
                next += 1;
                Op::IMAGE_SAMPLE_EXPLICIT_LOD
            }
            B::TextureGather => {
                third = Some(match component {
                    Some(component) => component,
                    None => self.lower().uint(0)?,
                });
                Op::IMAGE_GATHER
            }
            B::TextureGatherCompare => {
                third = Some(value(args, next)?);
                next += 1;
                Op::IMAGE_DREF_GATHER
            }
            _ => return Err(Stop::Internal),
        };
        if let Some(offset) = args.get(next) {
            mask |= ImageOperands::CONST_OFFSET.0;
            *operands.get_mut(count).ok_or(Stop::Internal)? = offset.value()?;
            count += 1;
        }
        let mut list = [Arg::Id(combined); 7];
        let mut length = 2;
        list[1] = Arg::Id(coords);
        if let Some(third) = third {
            list[2] = Arg::Id(third);
            length = 3;
        }
        if mask != 0 {
            *list.get_mut(length).ok_or(Stop::Internal)? = Arg::Word(mask);
            length += 1;
            for &operand in operands.get(..count).ok_or(Stop::Internal)? {
                *list.get_mut(length).ok_or(Stop::Internal)? = Arg::Id(operand);
                length += 1;
            }
        }
        let list = list.get(..length).ok_or(Stop::Internal)?;
        let compares = matches!(
            op,
            Op::IMAGE_SAMPLE_DREF_IMPLICIT_LOD | Op::IMAGE_SAMPLE_DREF_EXPLICIT_LOD
        );
        let gathers = matches!(op, Op::IMAGE_GATHER | Op::IMAGE_DREF_GATHER);
        if compares || gathers || !depth {
            return self.f.op_args(op, ty, list).map_err(built);
        }
        let texel = self.f.op_args(op, vec4, list).map_err(built)?;
        self.f
            .op_args(Op::COMPOSITE_EXTRACT, ty, &[Arg::Id(texel), Arg::Word(0)])
            .map_err(built)
    }

    /// `textureSampleBaseClampToEdge`: the base level sampled with the
    /// coordinates held half a texel inside each edge.
    fn sample_clamped(&mut self, image: Id, combined: Id, coords: Id, vec4: Id) -> Outcome<Id> {
        self.f
            .module()
            .capability(Capability::IMAGE_QUERY)
            .map_err(built)?;
        let sizes = self.lower().vector_of(2, Scalar::U32)?;
        let floats = self.lower().vector_of(2, Scalar::F32)?;
        let zero = self.lower().uint(0)?;
        let size = self
            .f
            .op(Op::IMAGE_QUERY_SIZE_LOD, sizes, &[image, zero])
            .map_err(built)?;
        let size = self
            .f
            .op(Op::CONVERT_U_TO_F, floats, &[size])
            .map_err(built)?;
        let half = self.splat_constant(0.5f32.to_bits(), Some(2), Scalar::F32)?;
        let one = self.splat_constant(1.0f32.to_bits(), Some(2), Scalar::F32)?;
        let inset = self.f.op(Op::F_DIV, floats, &[half, size]).map_err(built)?;
        let far = self.f.op(Op::F_SUB, floats, &[one, inset]).map_err(built)?;
        let held = self.ext(GlslStd450::N_CLAMP, floats, &[coords, inset, far])?;
        let level = self.f.module().real(0.0).map_err(built)?;
        self.f
            .op_args(
                Op::IMAGE_SAMPLE_EXPLICIT_LOD,
                vec4,
                &[
                    Arg::Id(combined),
                    Arg::Id(held),
                    Arg::Word(ImageOperands::LOD.0),
                    Arg::Id(level),
                ],
            )
            .map_err(built)
    }
}
