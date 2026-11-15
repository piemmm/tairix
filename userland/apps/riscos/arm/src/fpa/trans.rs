//! The transcendental operations, as FPEmulator computes them.
//!
//! FPEmulator evaluates each with Cody and Waite's methods, every step an
//! extended precision FPA instruction it runs on itself. So its bits are the
//! ones its sequence of operations leaves, and this module runs the same
//! sequence, with the same constants and rounding modes, on [`Machine`].
//! Each routine finishes with its result rounded toward zero or minus
//! infinity and, where extended precision is rounded to nearest, again to
//! nearest: the two together say which way the round stage should go.

use tairix_softfloat::Exact;

use super::arith::{Reason, Unit, Value};
use super::internal::{Internal, Number, BIAS, SIGN, UNITS};
use super::machine::{load, loaded, Machine, Src, HALF, ONE, ROUND_DOWN, ROUND_UP, TWO, ZERO};
use super::transfer::load_internal;
use super::{fpsr, Exception, Precision, Rounding};
use crate::instruction::{FpaCompareOp, FpaDyadicOp, FpaMonadicOp};

/// A finished routine's result before the round stage: its sign, biased
/// exponent and mantissa, and what lies below the mantissa (FPEmulator's
/// round and sticky word).
#[derive(Clone, Copy)]
struct Answer {
    negative: bool,
    exponent: i32,
    mantissa: u64,
    below: u32,
}

impl Answer {
    /// `value` with its exponent field moved by `adjust`.
    fn of(value: Internal, adjust: i32, negative: bool, below: u32) -> Self {
        Self {
            negative,
            exponent: value.exponent().cast_signed().saturating_add(adjust),
            mantissa: value.mantissa,
            below,
        }
    }
}

/// The exact result of an [`Answer`], the round word as the bits below its
/// mantissa.
fn exact(answer: Answer) -> Exact {
    let mut x = Number {
        negative: answer.negative,
        exponent: answer.exponent,
        mantissa: answer.mantissa,
    }
    .exact();
    if answer.below & 0x8000_0000 != 0 {
        x.sig |= 1 << 62;
    }
    if answer.below & 0x7FFF_FFFF != 0 {
        x.sticky = true;
    }
    x
}

/// The answer in register `r`, with its own sign.
fn answered(m: &Machine, r: usize, below: u32) -> Value {
    let value = m.f[r];
    Value::Exact(exact(Answer::of(value, 0, value.negative(), below)))
}

/// The answer in F1, negated where `negate`.
fn answered_negated(m: &mut Machine, negate: bool, below: u32) -> Value {
    if negate {
        m.neg(1, Src::F(1));
    }
    answered(m, 1, below)
}

/// The answer in F1 once F1 and F0 hold the last step rounded to nearest and
/// toward zero: F0's where the two differ, the round word then saying the
/// exact result lies above half way.
fn last_step_answer(m: &mut Machine, negate: bool) -> Value {
    let below = m.below(1, 0);
    if below == ROUND_UP {
        m.mov(1, Src::F(0));
    }
    answered_negated(m, negate, below)
}

/// A rational approximation's last step, `F2·F0 + F0`, taken toward zero
/// into F0 beside F1's to nearest, and its answer.
fn rational_end(m: &mut Machine, negate: bool) -> Value {
    m.dyadic(FpaDyadicOp::Muf, Rounding::Zero, 2, Src::F(2), Src::F(0));
    m.dyadic(FpaDyadicOp::Adf, Rounding::Zero, 0, Src::F(2), Src::F(0));
    last_step_answer(m, negate)
}

/// A result of one far past the exponent range, above it or below by
/// `below_zero`, for the round stage to overflow or underflow.
fn massive(negative: bool, below_zero: bool) -> Value {
    Value::Exact(exact(Answer {
        negative,
        exponent: if below_zero { -0x10_0000 } else { 0x10_0000 },
        mantissa: UNITS,
        below: ROUND_DOWN,
    }))
}

/// One exactly.
fn one() -> Value {
    Value::Exact(
        Number {
            negative: false,
            exponent: BIAS,
            mantissa: UNITS,
        }
        .exact(),
    )
}

/// A zero of the sign given, through the round stage.
fn zero(negative: bool) -> Value {
    Value::Exact(
        Number {
            negative,
            exponent: 0,
            mantissa: 0,
        }
        .exact(),
    )
}

/// The sign FPEmulator gives the NaN of an invalid operation it takes no
/// care over: whatever its working register held, the caller's R6 or what
/// the instruction before left. No program can rely on it; it is positive
/// here.
const UNCARED: bool = false;

const LOG_A: [[u32; 3]; 3] = [
    [0x8000_3FFE, 0xCA20_AD9A, 0xB5E9_46E9],
    [0x0000_4003, 0x8312_5100, 0xB57F_6509],
    [0x8000_4005, 0x803F_F895, 0x9DAC_D228],
];
const LOG_B: [[u32; 3]; 3] = [
    [0x8000_4004, 0x8EAC_025B, 0x3E70_76BB],
    [0x0000_4007, 0x9C04_1FD0, 0xA933_EF60],
    [0x8000_4008, 0xC05F_F4E0, 0x6C83_BB96],
];
/// ln 2 in two parts, each short enough to multiply by any exponent exactly.
const LN2: [[u32; 3]; 2] = [
    [0x0000_3FFE, 0xB172_17F7, 0xD1CF_0000],
    [0x0000_3FCD, 0xF357_93C0, 0x0000_0000],
];
const SQRT_HALF: [u32; 3] = [0x0000_3FFE, 0xB504_F333, 0xF9DE_6484];
const LOG10_E: [u32; 3] = [0x0000_3FFD, 0xDE5B_D8A9, 0x3728_7195];

/// `1/ln 2`, then `ln 2` in a short high part and a low part.
const EXP_REDUCTION: [[u32; 3]; 3] = [
    [0x0000_3FFF, 0xB8AA_3B29, 0x5C17_F0BC],
    [0x0000_3FFE, 0xB180_0000, 0x0000_0000],
    [0x8000_3FF2, 0xDE80_82E3, 0x0865_4362],
];
const EXP_P: [[u32; 3]; 2] = [
    [0x0000_3FF1, 0x845A_2157, 0x3490_F106],
    [0x0000_3FF8, 0xF83A_5F91, 0x5095_2C99],
];
const EXP_Q: [[u32; 3]; 3] = [
    [0x0000_3FEB, 0xC99B_1867, 0x2822_A93E],
    [0x0000_3FF5, 0xA578_62E1, 0x46A6_FB39],
    [0x0000_3FFB, 0xE8B9_428E, 0xFECF_F592],
];
const TWO_TO_MINUS_66: [u32; 3] = [0x0000_3FBD, 0x8000_0000, 0];
const HALF_MINUS_EPS: [u32; 3] = [0x0000_3FFD, 0xFFFF_FFFF, 0xFFFF_FFFF];
const ONE_MINUS_EPS: [u32; 3] = [0x0000_3FFE, 0xFFFF_FFFF, 0xFFFF_FFFF];
const TWO_TO_16: [u32; 3] = [0x0000_400F, 0x8000_0000, 0];
const TWO_TO_MINUS_32: [u32; 3] = [0x0000_3FDF, 0x8000_0000, 0];
const INFINITY: [u32; 3] = [0x4000_7FFF, 0, 0];

/// `1/π`, then π in two parts.
const SINCOS_REDUCTION: [[u32; 3]; 3] = [
    [0x0000_3FFD, 0xA2F9_836E, 0x4E44_152A],
    [0x0000_4000, 0xC910_0000, 0x0000_0000],
    [0x8000_3FEE, 0x9577_7A5C, 0xF72C_ECE6],
];
const SINCOS: [[u32; 3]; 8] = [
    [0x0000_3FCE, 0xC407_FB4C, 0x9EFC_A5FE],
    [0x8000_3FD6, 0xD721_06E0, 0x424C_DF56],
    [0x0000_3FDE, 0xB091_E343, 0x56A1_7FA8],
    [0x8000_3FE5, 0xD732_2A5A, 0xEE05_5B44],
    [0x0000_3FEC, 0xB8EF_1D29, 0x2783_1824],
    [0x8000_3FF2, 0xD00D_00D0, 0x09F0_D114],
    [0x0000_3FF8, 0x8888_8888, 0x8885_8061],
    [0x8000_3FFC, 0xAAAA_AAAA, 0xAAAA_A603],
];
const PI_OVER_2: [u32; 3] = [0x0000_3FFF, 0xC90F_DAA2, 0x2168_C235];

/// `2/π`, then π/2 in two parts.
const TAN_REDUCTION: [[u32; 3]; 3] = [
    [0x0000_3FFE, 0xA2F9_836E, 0x4E44_152A],
    [0x0000_3FFF, 0xC910_0000, 0x0000_0000],
    [0x8000_3FED, 0x9577_7A5C, 0xF72C_ECE6],
];
const TAN_P: [[u32; 3]; 3] = [
    [0x8000_3FEF, 0x95D5_B975, 0x1639_1DA8],
    [0x0000_3FF6, 0xE074_1531, 0xDD56_F650],
    [0x8000_3FFC, 0x8895_AF2A, 0x6847_FCD5],
];
const TAN_Q: [[u32; 3]; 4] = [
    [0x0000_3FEA, 0x85BB_A783, 0xB3C7_48A9],
    [0x8000_3FF3, 0xA37B_24C8, 0x4A42_092E],
    [0x0000_3FF9, 0xD23C_F50B, 0xF10A_CA84],
    [0x8000_3FFD, 0xEEF5_823F, 0xDECE_A969],
];
/// The least normalised extended number, `1 − 2^−64` and `2^8192`.
const TAN_SMALL: [[u32; 3]; 3] = [
    [0x0000_0000, 0x8000_0000, 0],
    [0x0000_3FFE, 0xFFFF_FFFF, 0xFFFF_FFFF],
    [0x0000_5FFF, 0x8000_0000, 0],
];

const ASN_P: [[u32; 3]; 5] = [
    [0x8000_3FFE, 0xB25D_EDAF, 0x30F3_242C],
    [0x0000_4002, 0xA270_BB27, 0x61C9_3957],
    [0x8000_4004, 0x9EC1_654D, 0x36D4_F820],
    [0x0000_4004, 0xE4D5_39B0, 0x56A4_51AD],
    [0x8000_4003, 0xDAF2_AD41, 0xD053_11C4],
];
const ASN_Q: [[u32; 3]; 5] = [
    [0x8000_4003, 0xBE97_4377, 0xCC30_F9E6],
    [0x0000_4006, 0x96F3_E4B2, 0xC8E3_7CBC],
    [0x8000_4007, 0xBEEE_77E2, 0xB542_3CF3],
    [0x0000_4007, 0xD092_7880, 0xF5C2_170B],
    [0x8000_4006, 0xA436_01F1, 0x5C3E_6196],
];
/// π/2 and π, each in two parts.
const HALF_PI: [[u32; 3]; 2] = [
    [0x0000_3FFF, 0xC90F_DAA2, 0x2168_C234],
    [0x0000_3FBF, 0xC4C6_8000, 0x0000_0000],
];
const PI: [[u32; 3]; 2] = [
    [0x0000_4000, 0xC90F_DAA2, 0x2168_C234],
    [0x0000_3FC0, 0xC4C6_8000, 0x0000_0000],
];

const ATN_P: [[u32; 3]; 4] = [
    [0x8000_3FFE, 0xD66B_D6CD, 0x8C3D_E934],
    [0x8000_4002, 0x87E9_FAE4, 0x6B53_1A29],
    [0x8000_4003, 0xA40B_FDCF, 0x15E6_5691],
    [0x8000_4002, 0xDB05_3288, 0x30E7_0EB4],
];
const ATN_Q: [[u32; 3]; 4] = [
    [0x0000_4002, 0xF062_4F0A, 0x5638_8310],
    [0x0000_4004, 0xEE50_5190, 0x6D1E_B4E8],
    [0x0000_4005, 0xAC50_9020, 0x5B6D_243B],
    [0x0000_4004, 0xA443_E5E6, 0x24AD_4B90],
];
const TWO_MINUS_SQRT3: [u32; 3] = [0x0000_3FFD, 0x8930_A2F4, 0xF66A_B18A];
/// π/6, π/3, π/2, π/2, 2π/3, 5π/6 and π, each in two parts: the constant an
/// arctangent's reduction adds back.
const ATN_ANGLES: [[[u32; 3]; 2]; 7] = [
    [
        [0x0000_3FFE, 0x860A_91C1, 0x6B9B_2C23],
        [0x0000_3FBC, 0xB766_AAAA, 0xAAAA_AAAB],
    ],
    [
        [0x0000_3FFF, 0x860A_91C1, 0x6B9B_2C23],
        [0x0000_3FBD, 0xB766_AAAA, 0xAAAA_AAAB],
    ],
    [
        [0x0000_3FFF, 0xC90F_DAA2, 0x2168_C234],
        [0x0000_3FBF, 0xC4C6_8000, 0x0000_0000],
    ],
    [
        [0x0000_3FFF, 0xC90F_DAA2, 0x2168_C234],
        [0x0000_3FBF, 0xC4C6_8000, 0x0000_0000],
    ],
    [
        [0x0000_4000, 0x860A_91C1, 0x6B9B_2C23],
        [0x0000_3FBE, 0xB766_AAAA, 0xAAAA_AAAB],
    ],
    [
        [0x0000_4000, 0xA78D_3631, 0xC681_F72B],
        [0x0000_3FC0, 0xD6EC_D555, 0x5555_5555],
    ],
    [
        [0x0000_4000, 0xC90F_DAA2, 0x2168_C234],
        [0x0000_3FC0, 0xC4C6_8000, 0x0000_0000],
    ],
];

impl Unit {
    /// Whether the round stage will read the round bit: extended precision
    /// rounded to nearest.
    fn cares_for_round_bit(&self) -> bool {
        self.precision == Precision::Extended && self.rounding == Rounding::Nearest
    }

    pub(super) fn transcendental(
        &mut self,
        op: FpaMonadicOp,
        a: Internal,
    ) -> Result<Value, Exception> {
        match op {
            FpaMonadicOp::Log => self.logarithm(true, a),
            FpaMonadicOp::Lgn => self.logarithm(false, a),
            FpaMonadicOp::Exp => self.exponential(a),
            FpaMonadicOp::Sin => self.sine(false, a),
            FpaMonadicOp::Cos => self.sine(true, a),
            FpaMonadicOp::Tan => self.tangent(a),
            FpaMonadicOp::Asn => self.arcsine(false, a),
            FpaMonadicOp::Acs => self.arcsine(true, a),
            _ => self.arctangent(a),
        }
    }

    /// `LGN` and `LOG`.
    fn logarithm(&mut self, base10: bool, a: Internal) -> Result<Value, Exception> {
        if a.special() {
            if a.nan() {
                return self.propagate(a);
            }
            if a.negative() {
                return self.invalid(Reason::LogOfNegative, UNCARED);
            }
            return Ok(Value::Final(Internal::infinity(false)));
        }
        if a.is_zero() {
            self.raise(Exception::DivisionByZero)?;
            return Ok(Value::Final(Internal::infinity(true)));
        }
        if a.negative() {
            return self.invalid(Reason::LogOfNegative, UNCARED);
        }
        let x = a.number();
        // x = f × 2^n with f in [0.5, 1).
        let mut n = x.exponent - (BIAS - 1);
        let mut m = Machine::new(self.fpsr);
        m.f[0] = loaded((BIAS - 1).cast_unsigned(), x.mantissa);
        if base10 {
            let decimal = (19 * (n + 1)) >> 6;
            if let Ok(decimal) = u32::try_from(decimal) {
                if let Some(power) = power_of_ten(decimal) {
                    if power.mantissa == x.mantissa && power.exponent().cast_signed() == x.exponent
                    {
                        m.float(Precision::Single, 1, decimal);
                        return Ok(answered(&m, 1, 0));
                    }
                }
            }
        } else if n == 1 && m.cmf(0, HALF).eq() {
            m.mov(1, ZERO);
            return Ok(answered(&m, 1, 0));
        }
        m.f[1] = load_internal(SQRT_HALF);
        let order = m.cmf(0, Src::F(1));
        if order.le() {
            n -= 1;
            m.sub(0, Src::F(0), HALF);
        }
        m.mul(1, Src::F(0), HALF);
        m.add(1, Src::F(1), HALF);
        if order.gt() {
            m.sub(0, Src::F(0), ONE);
        }
        m.div(0, Src::F(0), Src::F(1));
        m.mul(1, Src::F(0), Src::F(0));
        m.polynomial(&LOG_A);
        m.monic(&LOG_B);
        m.div(2, Src::F(2), Src::F(4));
        m.mul(1, Src::F(2), Src::F(0));
        m.add(1, Src::F(1), Src::F(0));
        if n != 0 {
            m.float(Precision::Extended, 0, n.cast_unsigned());
            let [high, low] = LN2.map(load_internal);
            m.f[2] = high;
            m.f[3] = low;
            m.mul(2, Src::F(2), Src::F(0));
            m.mul(3, Src::F(3), Src::F(0));
            m.add(0, Src::F(1), Src::F(2));
            m.sub(2, Src::F(0), Src::F(2));
            m.sub(2, Src::F(2), Src::F(1));
            m.add(2, Src::F(2), Src::F(3));
            m.add(1, Src::F(0), Src::F(2));
            if base10 {
                return Ok(self.decimal_logarithm(&mut m));
            }
            m.dyadic(FpaDyadicOp::Adf, Rounding::Zero, 0, Src::F(0), Src::F(2));
        } else {
            if base10 {
                return Ok(self.decimal_logarithm(&mut m));
            }
            return Ok(rational_end(&mut m, false));
        }
        Ok(last_step_answer(&mut m, false))
    }

    /// `LOG` from the natural logarithm in F1: times `log10 e`.
    fn decimal_logarithm(&mut self, m: &mut Machine) -> Value {
        m.mov(2, Src::F(1));
        m.f[0] = load_internal(LOG10_E);
        m.dyadic(FpaDyadicOp::Muf, Rounding::Zero, 1, Src::F(0), Src::F(2));
        let mut below = ROUND_DOWN;
        if self.cares_for_round_bit() {
            m.mul(0, Src::F(0), Src::F(2));
            if !m.cmf(0, Src::F(1)).eq() {
                below = ROUND_UP;
            }
        }
        answered(m, 1, below)
    }

    /// `EXP`.
    fn exponential(&mut self, a: Internal) -> Result<Value, Exception> {
        if a.special() {
            if a.nan() {
                return self.propagate(a);
            }
            return Ok(infinite_exponential(a.negative()));
        }
        if a.is_zero() {
            return Ok(one());
        }
        let mut m = Machine::new(self.fpsr);
        m.f[0] = load_internal(a.words());
        Ok(self.exponential_reduced(&mut m, false, a.negative()))
    }

    /// `EXP` from F0 on, shared with `POW`: the result's sign is `negative`,
    /// and `below_zero` says whether the original argument was negative, for
    /// a result too large or small to represent.
    fn exponential_reduced(&mut self, m: &mut Machine, negative: bool, below_zero: bool) -> Value {
        let Ok(n) = m.reduce(&EXP_REDUCTION) else {
            return massive(negative, below_zero);
        };
        let n = n.cast_signed();
        if !(-(1 << 16)..1 << 16).contains(&n) {
            return massive(negative, below_zero);
        }
        let adjust = n + 1;
        m.f[1] = load_internal(TWO_TO_MINUS_66);
        m.abs(2, Src::F(0));
        let (result, below) = if m.cmf(2, Src::F(1)).lt() {
            if m.cmf(0, ZERO).lt() {
                (load(HALF_MINUS_EPS), ROUND_UP)
            } else {
                (HALF, ROUND_DOWN)
            }
        } else {
            m.mul(1, Src::F(0), Src::F(0));
            m.polynomial(&EXP_Q);
            m.add(4, Src::F(2), ONE);
            m.polynomial(&EXP_P);
            m.add(2, Src::F(2), HALF);
            m.mul(2, Src::F(2), Src::F(0));
            m.sub(4, Src::F(4), Src::F(2));
            m.div(1, Src::F(2), Src::F(4));
            m.dyadic(FpaDyadicOp::Adf, Rounding::Zero, 0, Src::F(1), HALF);
            let mut below = ROUND_DOWN;
            if self.cares_for_round_bit() {
                m.add(1, Src::F(1), HALF);
                if !m.cmf(0, Src::F(1)).eq() {
                    below = ROUND_UP;
                }
            }
            (Src::F(0), below)
        };
        Value::Exact(exact(Answer::of(m.value(result), adjust, negative, below)))
    }

    /// `POW` (`fn` to the power `fm`), or `RPW` (`fm` to the power `fn`)
    /// where `reverse`.
    pub(super) fn power(
        &mut self,
        op1: Internal,
        op2: Internal,
        reverse: bool,
    ) -> Result<Value, Exception> {
        let (base, exponent) = if reverse { (op2, op1) } else { (op1, op2) };
        let low_word = op1.mantissa & 0xFFFF_FFFF;
        let mut m = Machine::new(self.fpsr);
        m.f[0] = load_internal(base.words());
        m.f[1] = load_internal(exponent.words());
        if op1.uncommon() || op2.uncommon() {
            // The operands in the instruction's order: a NaN propagates by
            // it, and an infinite power is not an integer.
            if op1.special() {
                if op2.special() {
                    if op1.nan() || op2.nan() {
                        return self.propagate_either(op1, op2);
                    }
                    return self.power_not_integer(&mut m, low_word);
                }
                if op1.nan() {
                    return self.propagate(op1);
                }
                if reverse {
                    return self.power_not_integer(&mut m, low_word);
                }
            } else if op2.special() {
                if op2.nan() {
                    return self.propagate(op2);
                }
                if !reverse {
                    return self.power_not_integer(&mut m, low_word);
                }
            }
        }
        m.monadic(FpaMonadicOp::Urd, 2, Src::F(1));
        m.monadic(FpaMonadicOp::Nrm, 2, Src::F(2));
        if !m.cmf(1, Src::F(2)).eq() {
            return self.power_not_integer(&mut m, low_word);
        }
        let mut odd = base.negative();
        if odd {
            m.mul(2, Src::F(1), HALF);
            m.monadic(FpaMonadicOp::Urd, 3, Src::F(2));
            m.monadic(FpaMonadicOp::Nrm, 3, Src::F(3));
            if m.cmf(2, Src::F(3)).eq() {
                odd = false;
            }
        }
        let sign = m.cmf(1, ZERO);
        if sign.eq() {
            return Ok(one());
        }
        if !sign.gt() && m.cmf(0, ZERO).eq() {
            self.raise(Exception::DivisionByZero)?;
            return Ok(Value::Final(Internal::infinity(odd)));
        }
        let base_sign = m.cmf(0, ZERO);
        if base_sign.lt() {
            m.neg(0, Src::F(0));
        }
        if base_sign.eq() {
            return Ok(zero(odd));
        }
        if m.cmf(0, ONE).eq() {
            return Ok(Value::Exact(
                Number {
                    negative: odd,
                    exponent: BIAS,
                    mantissa: UNITS,
                }
                .exact(),
            ));
        }
        m.f[2] = load_internal(TWO_TO_16);
        if m.cmf(2, Src::F(1)).gt() && m.compare(FpaCompareOp::Cnf, 2, Src::F(1)).gt() {
            if let Some(value) = Self::integer_power(&mut m, odd) {
                return Ok(value);
            }
        }
        Ok(self.power_standard(&mut m, odd, low_word))
    }

    /// A power that is not an integer, or is infinite.
    fn power_not_integer(&mut self, m: &mut Machine, low_word: u64) -> Result<Value, Exception> {
        let sign = m.cmf(0, ZERO);
        if sign.lt() {
            return self.invalid(Reason::NegativeToPower, UNCARED);
        }
        if sign.eq() {
            let power = m.cmf(1, ZERO);
            if power.lt() {
                self.raise(Exception::DivisionByZero)?;
                return Ok(Value::Final(Internal::infinity(false)));
            }
            if power.eq() {
                return self.invalid(Reason::ZeroToNonPositive, UNCARED);
            }
            return Ok(zero(m.f[0].negative()));
        }
        if m.cmf(0, ONE).eq() || m.cmf(1, ZERO).eq() {
            let infinity = load(INFINITY);
            if m.cmf(0, infinity).eq()
                || m.cmf(1, infinity).eq()
                || m.compare(FpaCompareOp::Cnf, 1, infinity).eq()
            {
                return self.invalid(Reason::BadInfinitePower, UNCARED);
            }
            return Ok(one());
        }
        Ok(self.power_standard(m, false, low_word))
    }

    /// `x^n` for an integer `|n| < 2^16`, by repeated squaring; `None` where
    /// it overflows or underflows, for the general method to take over.
    fn integer_power(m: &mut Machine, negative: bool) -> Option<Value> {
        m.mov(2, ONE);
        let n = m.fix(1).cast_signed();
        let magnitude = n.unsigned_abs();
        let mut bit = 1u32 << magnitude.checked_ilog2()?;
        let step = if n < 0 {
            FpaDyadicOp::Dvf
        } else {
            FpaDyadicOp::Muf
        };
        bit >>= 1;
        while bit != 0 {
            m.mul(2, Src::F(2), Src::F(2));
            if magnitude & (bit << 1) != 0 {
                m.dyadic(step, Rounding::Nearest, 2, Src::F(2), Src::F(0));
            }
            bit >>= 1;
        }
        // The last step toward zero and to nearest, for the round word.
        m.mul(3, Src::F(2), Src::F(2));
        if magnitude & 1 != 0 {
            m.dyadic(step, Rounding::Zero, 2, Src::F(3), Src::F(0));
            m.dyadic(step, Rounding::Nearest, 3, Src::F(3), Src::F(0));
        } else {
            m.dyadic(FpaDyadicOp::Muf, Rounding::Zero, 2, Src::F(2), Src::F(2));
        }
        let flags = m.flags();
        if flags & (Exception::Overflow.flag() | Exception::Underflow.flag()) != 0 {
            return None;
        }
        let below = if flags & Exception::Inexact.flag() == 0 {
            0
        } else if m.cmf(2, Src::F(3)).eq() {
            ROUND_DOWN
        } else {
            ROUND_UP
        };
        m.mov(0, Src::F(2));
        Some(Value::Exact(exact(Answer::of(m.f[0], 0, negative, below))))
    }

    /// `x^y` as `exp(y · ln x)`; `low_word` is the instruction's first
    /// operand's low mantissa word.
    fn power_standard(&mut self, m: &mut Machine, negative: bool, low_word: u64) -> Value {
        let mut unit = Unit::new(m.fpsr, Precision::Extended, Rounding::Nearest);
        let lgn = unit.monadic(FpaMonadicOp::Lgn, m.f[0]);
        m.fpsr = unit.fpsr;
        m.f[0] = lgn.unwrap_or(Internal::ZERO);
        m.mul(0, Src::F(0), Src::F(1));
        let product = m.f[0];
        if m.flags() & Exception::Overflow.flag() != 0 {
            return massive(negative, product.negative());
        }
        if m.cmf(0, ZERO).eq() {
            let (value, below) = if product.negative() {
                (load(ONE_MINUS_EPS), ROUND_UP)
            } else {
                (ONE, ROUND_DOWN)
            };
            return Value::Exact(exact(Answer::of(m.value(value), 0, negative, below)));
        }
        let infinity = load(INFINITY);
        if m.cmf(0, infinity).eq() || m.compare(FpaCompareOp::Cnf, 0, infinity).eq() {
            if !product.negative() {
                return Value::Final(Internal::infinity(negative));
            }
            // FPEmulator's zero keeps the first operand's low mantissa word,
            // which it never clears, for the round stage to round.
            return Value::Exact(
                Number {
                    negative,
                    exponent: 0,
                    mantissa: low_word,
                }
                .exact(),
            );
        }
        self.exponential_reduced(m, negative, product.negative())
    }

    /// `SIN`, or `COS` where `cos`.
    fn sine(&mut self, cos: bool, a: Internal) -> Result<Value, Exception> {
        if a.special() {
            if a.nan() {
                return self.propagate(a);
            }
            return self.invalid(Reason::SinCosInfinity, UNCARED);
        }
        if a.is_zero() {
            return Ok(if cos { one() } else { zero(a.negative()) });
        }
        let mut m = Machine::new(self.fpsr);
        m.f[0] = load_internal(a.words());
        let ac = self.fpsr & fpsr::AC != 0;
        let sign = m.cmf(0, ZERO);
        let mut negative = sign.carry(ac);
        if sign.lt() {
            m.neg(0, Src::F(0));
        }
        if cos {
            m.add(0, Src::F(0), load(PI_OVER_2));
            negative = true;
        }
        let Ok(n) = m.reduce(&SINCOS_REDUCTION) else {
            // The quotient's sign, which is the reduced argument's: positive.
            return self.invalid(Reason::SinCosRange, false);
        };
        negative ^= n & 1 != 0;
        let reduced = m.cmf(0, ZERO);
        if reduced.lt() {
            m.neg(0, Src::F(0));
        } else {
            negative = !negative;
        }
        let tiny = load(TWO_TO_MINUS_32);
        let below = if m.cmf(0, tiny).lt() {
            m.dyadic(
                FpaDyadicOp::Muf,
                Rounding::Zero,
                1,
                Src::F(0),
                load(ONE_MINUS_EPS),
            );
            ROUND_UP
        } else {
            m.mul(1, Src::F(0), Src::F(0));
            m.polynomial(&SINCOS);
            m.dyadic(
                FpaDyadicOp::Muf,
                Rounding::MinusInfinity,
                1,
                Src::F(2),
                Src::F(0),
            );
            m.dyadic(
                FpaDyadicOp::Adf,
                Rounding::MinusInfinity,
                1,
                Src::F(1),
                Src::F(0),
            );
            let mut below = ROUND_DOWN;
            if self.cares_for_round_bit() {
                m.mul(2, Src::F(2), Src::F(0));
                m.add(0, Src::F(2), Src::F(0));
                if !m.cmf(0, Src::F(1)).eq() {
                    below = ROUND_UP;
                }
            }
            below
        };
        // FPEmulator takes the result's whole first word for its exponent.
        let value = m.f[1];
        Ok(Value::Exact(exact(Answer {
            negative,
            exponent: value.head.cast_signed(),
            mantissa: value.mantissa,
            below,
        })))
    }

    /// `TAN`.
    fn tangent(&mut self, a: Internal) -> Result<Value, Exception> {
        if a.special() {
            if a.nan() {
                return self.propagate(a);
            }
            return self.invalid(Reason::TanInfinity, UNCARED);
        }
        if a.is_zero() {
            return Ok(zero(a.negative()));
        }
        let mut m = Machine::new(self.fpsr);
        m.f[0] = load_internal(a.words());
        let n = match m.reduce(&TAN_REDUCTION) {
            Ok(n) => n,
            // The NaN takes the sign of the quotient that overflowed.
            Err(quotient) => return self.invalid(Reason::TanRange, quotient & SIGN != 0),
        };
        let odd = n & 1 != 0;
        m.abs(2, Src::F(0));
        let (below, adjust) = if m.cmf(2, load(TWO_TO_MINUS_32)).lt() {
            let [least, one_minus_eps, scale] = TAN_SMALL.map(load_internal);
            m.f[2] = least;
            m.f[3] = one_minus_eps;
            m.f[4] = scale;
            m.abs(1, Src::F(0));
            let order = m.cmf(1, Src::F(2));
            if odd {
                if order.lt() {
                    m.mul(0, Src::F(0), Src::F(4));
                }
                m.neg(0, Src::F(0));
                m.dyadic(FpaDyadicOp::Rdf, Rounding::Nearest, 0, Src::F(0), ONE);
                m.dyadic(FpaDyadicOp::Muf, Rounding::Zero, 0, Src::F(0), Src::F(3));
                (ROUND_UP, if order.lt() { 0x2000 } else { 0 })
            } else if order.lt() {
                m.mul(0, Src::F(0), Src::F(4));
                (ROUND_DOWN, -0x2000)
            } else {
                (ROUND_DOWN, 0)
            }
        } else {
            m.mul(1, Src::F(0), Src::F(0));
            m.polynomial(&TAN_P);
            m.add(2, Src::F(2), ONE);
            m.mul(4, Src::F(0), Src::F(2));
            m.polynomial(&TAN_Q);
            m.add(2, Src::F(2), ONE);
            let (numerator, denominator) = if odd {
                m.neg(4, Src::F(4));
                (2, 4)
            } else {
                (4, 2)
            };
            m.dyadic(
                FpaDyadicOp::Dvf,
                Rounding::MinusInfinity,
                0,
                Src::F(numerator),
                Src::F(denominator),
            );
            let mut below = ROUND_DOWN;
            if self.cares_for_round_bit() {
                m.div(1, Src::F(numerator), Src::F(denominator));
                if !m.cmf(0, Src::F(1)).eq() {
                    below = ROUND_UP;
                }
            }
            (below, 0)
        };
        let value = m.f[0];
        Ok(Value::Exact(exact(Answer::of(
            value,
            adjust,
            value.negative(),
            below,
        ))))
    }

    /// `ASN`, or `ACS` where `acs`.
    fn arcsine(&mut self, acs: bool, a: Internal) -> Result<Value, Exception> {
        if a.special() {
            if a.nan() {
                return self.propagate(a);
            }
            return self.invalid(Reason::AsnAcsInfinity, UNCARED);
        }
        if !acs && a.is_zero() {
            return Ok(zero(a.negative()));
        }
        let mut m = Machine::new(self.fpsr);
        m.f[0] = load_internal(a.words());
        let ac = self.fpsr & fpsr::AC != 0;
        let order = m.cmf(0, ZERO);
        let positive = order.carry(ac);
        if order.lt() {
            m.neg(0, Src::F(0));
        }
        // Bit 31 negates the argument, bit 30 adds a constant, π where bit 29
        // is set and π/2 otherwise, and bit 28 negates the result.
        let flags;
        if m.cmf(0, HALF).lt() {
            flags = match (acs, positive) {
                (false, false) => 0x1000_0000,
                (false, true) => 0,
                (true, false) => 0x4000_0000,
                (true, true) => 0xC000_0000,
            };
            if m.cmf(0, load(TWO_TO_MINUS_32)).lt() {
                if flags & 0x8000_0000 != 0 {
                    m.neg(0, Src::F(0));
                }
                m.mov(1, Src::F(0));
                if flags & 0x4000_0000 != 0 {
                    return Ok(self.arc_with_constant(&mut m, flags));
                }
                return Ok(answered_negated(
                    &mut m,
                    flags & 0x1000_0000 != 0,
                    ROUND_DOWN,
                ));
            }
            m.mul(1, Src::F(0), Src::F(0));
        } else {
            let to_one = m.cmf(0, ONE);
            if to_one.gt() {
                let negative =
                    !acs && (a.mantissa >> 32 | a.mantissa & 0xFFFF_FFFF) & 0x8000_0000 != 0;
                return self.invalid(Reason::AsnAcsRange, negative);
            }
            if to_one.eq() && acs && positive {
                m.mov(1, ZERO);
                return Ok(answered(&m, 1, 0));
            }
            flags = match (acs, positive) {
                (false, false) => 0xD000_0000,
                (false, true) => 0xC000_0000,
                (true, false) => 0xE000_0000,
                (true, true) => 0,
            };
            m.dyadic(FpaDyadicOp::Rsf, Rounding::Nearest, 1, Src::F(0), ONE);
            m.mul(1, Src::F(1), HALF);
            m.monadic(FpaMonadicOp::Sqt, 0, Src::F(1));
            m.add(0, Src::F(0), Src::F(0));
        }
        m.polynomial(&ASN_P);
        m.monic(&ASN_Q);
        m.div(2, Src::F(2), Src::F(4));
        if flags & 0x8000_0000 != 0 {
            m.neg(0, Src::F(0));
        }
        m.mul(1, Src::F(2), Src::F(0));
        m.add(1, Src::F(1), Src::F(0));
        if flags & 0x4000_0000 != 0 {
            return Ok(self.arc_with_constant(&mut m, flags));
        }
        Ok(rational_end(&mut m, flags & 0x1000_0000 != 0))
    }

    /// F1 plus the constant `flags` names.
    fn arc_with_constant(&mut self, m: &mut Machine, flags: u32) -> Value {
        let constant = if flags & 0x2000_0000 != 0 {
            PI
        } else {
            HALF_PI
        };
        self.plus_constant(m, constant, flags & 0x1000_0000 != 0)
    }

    /// The answer F1 plus a constant held as a high and a low part makes, the
    /// low part added after the high part's rounding error is recovered.
    fn plus_constant(
        &mut self,
        m: &mut Machine,
        [high, low]: [[u32; 3]; 2],
        negate: bool,
    ) -> Value {
        m.f[2] = load_internal(high);
        m.f[3] = load_internal(low);
        m.add(0, Src::F(1), Src::F(2));
        m.sub(2, Src::F(0), Src::F(2));
        m.sub(2, Src::F(2), Src::F(1));
        m.add(2, Src::F(2), Src::F(3));
        m.dyadic(FpaDyadicOp::Adf, Rounding::Zero, 1, Src::F(0), Src::F(2));
        let mut below = ROUND_DOWN;
        if self.cares_for_round_bit() {
            m.add(0, Src::F(0), Src::F(2));
            if !m.cmf(0, Src::F(1)).eq() {
                below = ROUND_UP;
            }
        }
        answered_negated(m, negate, below)
    }

    /// `ATN`.
    fn arctangent(&mut self, a: Internal) -> Result<Value, Exception> {
        let mut a = a;
        if a.special() {
            if a.nan() {
                return self.propagate(a);
            }
            // An infinity is taken as a number with its exponent.
            a.mantissa = UNITS;
        }
        if a.is_zero() {
            return Ok(zero(a.negative()));
        }
        let flags = a.head & SIGN;
        let mut m = Machine::new(self.fpsr);
        m.f[0] = loaded(a.head & !SIGN, a.mantissa);
        Ok(self.arctangent_reduced(&mut m, flags))
    }

    /// `POL`: the angle of the point (`fn`, `fm`).
    pub(super) fn polar(&mut self, x: Internal, y: Internal) -> Result<Value, Exception> {
        let (mut x, mut y) = (x, y);
        if x.uncommon() || y.uncommon() {
            if x.special() {
                if y.special() {
                    if x.nan() || y.nan() {
                        return self.propagate_either(x, y);
                    }
                    return self.invalid(Reason::PolarOfInfinities, UNCARED);
                }
                if x.nan() {
                    return self.propagate(x);
                }
                x.mantissa = UNITS;
            } else if y.special() {
                if y.nan() {
                    return self.propagate(y);
                }
                y.mantissa = UNITS;
            }
        }
        if y.is_zero() {
            if x.is_zero() {
                return self.invalid(Reason::PolarOfZeros, false);
            }
            if !x.negative() {
                return Ok(zero(y.negative()));
            }
        }
        let mut flags = y.head & SIGN;
        if x.negative() {
            flags ^= 7;
        }
        let mut m = Machine::new(self.fpsr);
        m.f[0] = loaded(x.head & !SIGN, x.mantissa);
        m.f[1] = loaded(y.head & !SIGN, y.mantissa);
        m.div(0, Src::F(1), Src::F(0));
        Ok(self.arctangent_reduced(&mut m, flags))
    }

    /// The arctangent of F0, non-negative, plus the angle `flags` names: bit
    /// 31 negates the result, bit 1 the reduced one, and bits 2 to 0 pick
    /// the constant.
    fn arctangent_reduced(&mut self, m: &mut Machine, flags: u32) -> Value {
        let mut flags = flags;
        if m.cmf(0, ONE).gt() {
            m.dyadic(FpaDyadicOp::Rdf, Rounding::Nearest, 0, Src::F(0), ONE);
            flags ^= 3;
        }
        m.f[1] = load_internal(TWO_MINUS_SQRT3);
        if !m.cmf(0, Src::F(1)).le() {
            m.dyadic(FpaDyadicOp::Rsf, Rounding::Nearest, 3, Src::F(1), ONE);
            m.mul(2, Src::F(0), Src::F(3));
            m.sub(2, Src::F(2), ONE);
            m.add(2, Src::F(2), Src::F(0));
            m.dyadic(FpaDyadicOp::Rsf, Rounding::Nearest, 1, Src::F(1), TWO);
            m.add(1, Src::F(1), Src::F(0));
            m.div(0, Src::F(2), Src::F(1));
            flags ^= 1;
        }
        m.abs(1, Src::F(0));
        if m.compare(FpaCompareOp::Cmfe, 1, load(TWO_TO_MINUS_32)).lt() {
            if flags.trailing_zeros() >= 3 {
                m.dyadic(
                    FpaDyadicOp::Muf,
                    Rounding::Zero,
                    1,
                    load(ONE_MINUS_EPS),
                    Src::F(0),
                );
                return answered_negated(m, flags & SIGN != 0, ROUND_UP);
            }
            m.mov(1, Src::F(0));
        } else {
            m.mul(1, Src::F(0), Src::F(0));
            m.polynomial(&ATN_P);
            m.monic(&ATN_Q);
            m.div(2, Src::F(2), Src::F(4));
            m.mul(1, Src::F(2), Src::F(0));
            m.add(1, Src::F(1), Src::F(0));
        }
        if flags & 2 != 0 {
            m.neg(1, Src::F(1));
        }
        let negate = flags & SIGN != 0;
        if flags & 7 != 0 {
            return self.plus_constant(m, ATN_ANGLES[(flags & 7) as usize - 1], negate);
        }
        rational_end(m, negate)
    }
}

/// `EXP` of an infinity: infinity for plus, zero for minus.
fn infinite_exponential(minus: bool) -> Value {
    if minus {
        zero(false)
    } else {
        Value::Final(Internal::infinity(false))
    }
}

/// `10^n` exactly, for the `n` below 28 whose value an extended mantissa
/// holds.
pub(super) fn power_of_ten(n: u32) -> Option<Internal> {
    if n >= 28 {
        return None;
    }
    let five = 5u64.checked_pow(n)?;
    let zeros = five.leading_zeros();
    Some(Internal {
        head: BIAS.cast_unsigned() + n + 63 - zeros,
        mantissa: five << zeros,
    })
}
