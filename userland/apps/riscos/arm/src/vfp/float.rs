//! The floating-point unit's arithmetic: `lib/softfloat`'s, rounded and
//! flushed as its controls say, with ARM's NaN rules and the operations ARM
//! defines itself — maximum and minimum, the reciprocal estimates and steps,
//! and the conversions to and from fixed point and half precision.

use core::cmp::Ordering;

use tairix_softfloat::{Binary, Flags, Ieee, Integral, Mode, Nans, Outcome, Policy, Tininess};

use super::fpscr;

/// The FPSCR's rounding modes, in its encoding order.
const MODES: [Mode; 4] = [Mode::Nearest, Mode::Up, Mode::Down, Mode::Zero];

/// The single-precision constants the reciprocal steps use.
const TWO: u64 = 0x4000_0000;
const ONE_AND_A_HALF: u64 = 0x3FC0_0000;
const HALF: u64 = 0x3F00_0000;

/// The FPSCR flags softfloat's `flags` are.
const fn raised(flags: Flags) -> u32 {
    let mut out = 0;
    if flags.contains(Flags::INVALID) {
        out |= fpscr::IOC;
    }
    if flags.contains(Flags::DIVIDE_BY_ZERO) {
        out |= fpscr::DZC;
    }
    if flags.contains(Flags::OVERFLOW) {
        out |= fpscr::OFC;
    }
    if flags.contains(Flags::UNDERFLOW) {
        out |= fpscr::UFC;
    }
    if flags.contains(Flags::INEXACT) {
        out |= fpscr::IXC;
    }
    out
}

/// What a value of a format is, as the unit's special cases need it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Class {
    Zero,
    Finite,
    Infinity,
    Quiet,
    Signalling,
}

const fn class(format: Binary, bits: u64) -> Class {
    let magnitude = bits & !format.sign_bit();
    let infinity = format.infinity(false);
    if magnitude == 0 {
        Class::Zero
    } else if magnitude < infinity {
        Class::Finite
    } else if magnitude == infinity {
        Class::Infinity
    } else if bits & format.quiet_bit() != 0 {
        Class::Quiet
    } else {
        Class::Signalling
    }
}

/// A comparison Advanced SIMD makes, which answers a lane's truth.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Comparison {
    /// Equal: only a signalling NaN signals.
    Equal,
    /// Greater than or equal: any NaN signals.
    GreaterOrEqual,
    /// Greater than: any NaN signals.
    Greater,
}

/// The unit for one instruction: the controls it runs under, and the
/// cumulative flags it has raised.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Unit {
    mode: Mode,
    flush: bool,
    default_nan: bool,
    alternative_half: bool,
    /// The FPSCR's cumulative flags raised so far.
    pub flags: u32,
}

impl Unit {
    /// Under the FPSCR's controls.
    #[must_use]
    pub const fn new(fpscr: u32) -> Self {
        Self {
            mode: MODES[((fpscr >> fpscr::RMODE_SHIFT) & 3) as usize],
            flush: fpscr & fpscr::FZ != 0,
            default_nan: fpscr & fpscr::DN != 0,
            alternative_half: fpscr & fpscr::AHP != 0,
            flags: 0,
        }
    }

    /// Under Advanced SIMD's standard controls: to nearest, flushing, the
    /// default NaN; the FPSCR's half-precision format.
    #[must_use]
    pub const fn standard(fpscr: u32) -> Self {
        Self {
            mode: Mode::Nearest,
            flush: true,
            default_nan: true,
            ..Self::new(fpscr)
        }
    }

    /// The rounding mode the controls set.
    #[must_use]
    pub const fn mode(&self) -> Mode {
        self.mode
    }

    /// Whether denormals of `format` flush: never half precision's.
    fn flushes(self, format: Binary) -> bool {
        self.flush && format != Binary::HALF
    }

    /// Arithmetic on `from`'s values, rounding to `to` in `mode`.
    fn rounder(self, from: Binary, to: Binary, mode: Mode) -> Ieee {
        Ieee {
            format: from,
            mode,
            policy: Policy {
                tininess: Tininess::BeforeRounding,
                nans: if self.default_nan {
                    Nans::Default
                } else {
                    Nans::Propagate
                },
                flush_to_zero: self.flushes(to),
            },
        }
    }

    fn ieee(self, format: Binary) -> Ieee {
        self.rounder(format, format, self.mode)
    }

    /// An operand as the unit reads it: a denormal flushed to a zero of its
    /// sign where the controls say.
    fn input(&mut self, format: Binary, bits: u64) -> u64 {
        let magnitude = bits & !format.sign_bit();
        if self.flushes(format) && magnitude != 0 && magnitude >> format.fraction_bits() == 0 {
            self.flags |= fpscr::IDC;
            return bits & format.sign_bit();
        }
        bits
    }

    fn take(&mut self, outcome: Outcome) -> u64 {
        self.flags |= raised(outcome.flags);
        outcome.bits
    }

    /// `a + b`.
    pub fn add(&mut self, format: Binary, a: u64, b: u64) -> u64 {
        let (a, b) = (self.input(format, a), self.input(format, b));
        let outcome = self.ieee(format).add(a, b);
        self.take(outcome)
    }

    /// `a − b`.
    pub fn sub(&mut self, format: Binary, a: u64, b: u64) -> u64 {
        let (a, b) = (self.input(format, a), self.input(format, b));
        let outcome = self.ieee(format).sub(a, b);
        self.take(outcome)
    }

    /// `a × b`.
    pub fn mul(&mut self, format: Binary, a: u64, b: u64) -> u64 {
        let (a, b) = (self.input(format, a), self.input(format, b));
        let outcome = self.ieee(format).mul(a, b);
        self.take(outcome)
    }

    /// `a ÷ b`.
    pub fn div(&mut self, format: Binary, a: u64, b: u64) -> u64 {
        let (a, b) = (self.input(format, a), self.input(format, b));
        let outcome = self.ieee(format).div(a, b);
        self.take(outcome)
    }

    /// `√a`.
    pub fn sqrt(&mut self, format: Binary, a: u64) -> u64 {
        let a = self.input(format, a);
        let outcome = self.ieee(format).sqrt(a);
        self.take(outcome)
    }

    /// `addend + a × b`, rounded once.
    pub fn mul_add(&mut self, format: Binary, addend: u64, a: u64, b: u64) -> u64 {
        let addend = self.input(format, addend);
        let (a, b) = (self.input(format, a), self.input(format, b));
        let outcome = self.ieee(format).fused_multiply_add(addend, a, b);
        self.take(outcome)
    }

    /// `VCMP` and `VCMPE`: N, Z, C and V in bits 31 to 28, unordered `0011`.
    /// A quiet NaN signals where `signalling`.
    pub fn compare(&mut self, format: Binary, a: u64, b: u64, signalling: bool) -> u32 {
        let (a, b) = (self.input(format, a), self.input(format, b));
        let (order, flags) = self.ieee(format).compare(a, b, signalling);
        self.flags |= raised(flags);
        match order {
            None => 0x3 << 28,
            Some(Ordering::Equal) => 0x6 << 28,
            Some(Ordering::Less) => 0x8 << 28,
            Some(Ordering::Greater) => 0x2 << 28,
        }
    }

    /// An Advanced SIMD comparison's truth.
    pub fn test(&mut self, format: Binary, a: u64, b: u64, comparison: Comparison) -> bool {
        let (a, b) = (self.input(format, a), self.input(format, b));
        let signalling = comparison != Comparison::Equal;
        let (order, flags) = self.ieee(format).compare(a, b, signalling);
        self.flags |= raised(flags);
        match (comparison, order) {
            (_, None) => false,
            (Comparison::Equal, Some(order)) => order == Ordering::Equal,
            (Comparison::GreaterOrEqual, Some(order)) => order != Ordering::Less,
            (Comparison::Greater, Some(order)) => order == Ordering::Greater,
        }
    }

    /// The NaN an operation over `operands` answers, if any is one: the first
    /// signalling one quieted, else the first quiet one, or the default.
    fn nans(&mut self, format: Binary, operands: &[u64]) -> Option<u64> {
        let mut chosen = None;
        let mut signalled = false;
        for &op in operands {
            match class(format, op) {
                Class::Signalling if !signalled => {
                    signalled = true;
                    chosen = Some(op);
                }
                Class::Quiet if chosen.is_none() => chosen = Some(op),
                _ => {}
            }
        }
        if signalled {
            self.flags |= fpscr::IOC;
        }
        let chosen = chosen?;
        Some(if self.default_nan {
            format.default_nan()
        } else {
            chosen | format.quiet_bit()
        })
    }

    /// `VMAX`: the greater, a zero of each sign giving positive zero.
    pub fn max(&mut self, format: Binary, a: u64, b: u64) -> u64 {
        self.extreme(format, a, b, true)
    }

    /// `VMIN`: the lesser, a zero of each sign giving negative zero.
    pub fn min(&mut self, format: Binary, a: u64, b: u64) -> u64 {
        self.extreme(format, a, b, false)
    }

    fn extreme(&mut self, format: Binary, a: u64, b: u64, greatest: bool) -> u64 {
        let (a, b) = (self.input(format, a), self.input(format, b));
        if let Some(nan) = self.nans(format, &[a, b]) {
            return nan;
        }
        if matches!(
            (class(format, a), class(format, b)),
            (Class::Zero, Class::Zero)
        ) {
            let (left, right) = (format.negative(a), format.negative(b));
            return format.zero(if greatest {
                left && right
            } else {
                left || right
            });
        }
        let (order, _) = self.ieee(format).compare(a, b, false);
        let first = match order {
            Some(Ordering::Greater) => greatest,
            Some(Ordering::Less) => !greatest,
            _ => false,
        };
        if first {
            a
        } else {
            b
        }
    }

    /// `VMAXNM`: as [`Unit::max`], but a lone quiet NaN loses.
    pub fn max_number(&mut self, format: Binary, a: u64, b: u64) -> u64 {
        let (a, b) = number_operands(format, a, b, true);
        self.max(format, a, b)
    }

    /// `VMINNM`: as [`Unit::min`], but a lone quiet NaN loses.
    pub fn min_number(&mut self, format: Binary, a: u64, b: u64) -> u64 {
        let (a, b) = number_operands(format, a, b, false);
        self.min(format, a, b)
    }

    /// `a × 2^fraction` as a `width`-bit integer, signed or not, rounded in
    /// `mode` and saturated: a NaN is zero, and either raises invalid. The
    /// result is extended to 32 bits as its signedness says.
    pub fn to_fixed(
        &mut self,
        format: Binary,
        a: u64,
        fraction: u32,
        unsigned: bool,
        width: u32,
        mode: Mode,
    ) -> u32 {
        let a = self.input(format, a);
        let top = 1u64 << (width - u32::from(!unsigned));
        let (negative, magnitude, inexact) = match self.ieee(format).to_fixed(a, fraction, mode) {
            Integral::Nan => {
                self.flags |= fpscr::IOC;
                return 0;
            }
            Integral::OutOfRange { negative } => (negative, u64::MAX, false),
            Integral::Value {
                negative,
                magnitude,
                inexact,
            } => (negative, magnitude, inexact),
        };
        let (value, saturated) = match (negative, unsigned) {
            (false, _) if magnitude >= top => (top - 1, true),
            (false, _) => (magnitude, false),
            (true, true) => (0, magnitude != 0),
            (true, false) if magnitude > top => (top.wrapping_neg(), true),
            (true, false) => (magnitude.wrapping_neg(), false),
        };
        if saturated {
            self.flags |= fpscr::IOC;
        } else if inexact {
            self.flags |= fpscr::IXC;
        }
        let unused = 32 - width;
        let value = crate::word::low(value) << unused;
        if unsigned {
            value >> unused
        } else {
            (value.cast_signed() >> unused).cast_unsigned()
        }
    }

    /// The `width`-bit fixed-point `value` with `fraction` fraction bits,
    /// signed or not, rounded to `format` in `mode`.
    pub fn from_fixed(
        &mut self,
        format: Binary,
        value: u32,
        width: u32,
        fraction: u32,
        unsigned: bool,
        mode: Mode,
    ) -> u64 {
        let unused = 32 - width;
        let (negative, magnitude) = if unsigned {
            (false, u64::from(value << unused >> unused))
        } else {
            let signed = (value << unused).cast_signed() >> unused;
            (signed < 0, u64::from(signed.unsigned_abs()))
        };
        let outcome = self
            .rounder(format, format, mode)
            .from_fixed(negative, magnitude, fraction);
        self.take(outcome)
    }

    /// `a` in `from` converted to `to`, the alternative half-precision
    /// format where the controls name it. Only a single or double operand
    /// flushes, and only a single or double result.
    pub fn convert(&mut self, from: Binary, to: Binary, a: u64) -> u64 {
        if from == Binary::HALF && self.alternative_half {
            let single = self.alternative_to_single(a);
            let outcome = self
                .rounder(Binary::SINGLE, to, self.mode)
                .convert(single, to);
            return self.take(outcome);
        }
        let a = self.input(from, a);
        if to == Binary::HALF && self.alternative_half {
            return self.round_alternative(from, a);
        }
        let outcome = self.rounder(from, to, self.mode).convert(a, to);
        self.take(outcome)
    }

    /// An alternative half-precision value as the single-precision value it
    /// equals exactly: its top exponent holds numbers like the rest.
    fn alternative_to_single(self, a: u64) -> u64 {
        if (a >> 10) & 0x1F != 0x1F {
            return self
                .rounder(Binary::HALF, Binary::SINGLE, Mode::Nearest)
                .convert(a, Binary::SINGLE)
                .bits;
        }
        (a >> 15 & 1) << 31 | (16 + 127) << 23 | (a & 0x3FF) << 13
    }

    /// `a` rounded to the alternative half-precision format, which has no
    /// infinity or NaN and holds numbers to just under 2^17.
    fn round_alternative(&mut self, from: Binary, a: u64) -> u64 {
        let negative = from.negative(a);
        let largest = u64::from(negative) << 15 | 0x7FFF;
        match class(from, a) {
            Class::Quiet | Class::Signalling => {
                self.flags |= fpscr::IOC;
                return u64::from(negative) << 15;
            }
            Class::Infinity => {
                self.flags |= fpscr::IOC;
                return largest;
            }
            Class::Zero | Class::Finite => {}
        }
        let rounder = self.rounder(from, Binary::HALF, self.mode);
        let outcome = rounder.convert(a, Binary::HALF);
        if !outcome.flags.contains(Flags::OVERFLOW) {
            return self.take(outcome);
        }
        // Past IEEE's largest: the value halved rounds as the top exponent's
        // numbers do, and doubles back.
        let halved = rounder.convert(a - (1 << from.fraction_bits()), Binary::HALF);
        if halved.flags.contains(Flags::OVERFLOW) {
            self.flags |= fpscr::IOC;
            return largest;
        }
        self.take(halved) + (1 << 10)
    }

    /// `a` rounded to an integral value of its format in `mode`, raising
    /// inexact only where `exact`.
    pub fn round_integral(&mut self, format: Binary, a: u64, mode: Mode, exact: bool) -> u64 {
        let a = self.input(format, a);
        let outcome = self
            .rounder(format, format, mode)
            .round_to_integral(a, exact);
        self.take(outcome)
    }

    /// `VRECPE.F32`: an estimate of the reciprocal to eight bits.
    pub fn recip_estimate(&mut self, a: u32) -> u32 {
        let format = Binary::SINGLE;
        let bits = self.input(format, u64::from(a));
        if let Some(nan) = self.nans(format, &[bits]) {
            return crate::word::low(nan);
        }
        let a = crate::word::low(bits);
        let negative = a >> 31 != 0;
        let sign = a & 0x8000_0000;
        let magnitude = a & 0x7FFF_FFFF;
        match class(format, bits) {
            Class::Infinity => return sign,
            Class::Zero => {
                self.flags |= fpscr::DZC;
                return sign | 0x7F80_0000;
            }
            _ => {}
        }
        // Below 2^-128 the reciprocal overflows.
        if magnitude < 0x0020_0000 {
            self.flags |= fpscr::OFC | fpscr::IXC;
            let finite = self.mode.overflows_to_finite(negative);
            return sign | if finite { 0x7F7F_FFFF } else { 0x7F80_0000 };
        }
        if self.flush && magnitude >= 0x7E80_0000 {
            self.flags |= fpscr::UFC;
            return sign;
        }
        let (exp, fraction) = normalised_fraction(magnitude, false);
        let estimate = u64::from(recip(1 << 8 | crate::word::low(fraction >> 44)) & 0xFF);
        let (exp, fraction) = match 253 - exp {
            0 => (0, 1 << 51 | estimate << 43),
            -1 => (0, 1 << 50 | estimate << 42),
            exp => (exp, estimate << 44),
        };
        sign | (exp.cast_unsigned() & 0xFF) << 23 | crate::word::low(fraction >> 29)
    }

    /// `VRSQRTE.F32`: an estimate of the reciprocal square root to eight
    /// bits.
    pub fn rsqrt_estimate(&mut self, a: u32) -> u32 {
        let format = Binary::SINGLE;
        let bits = self.input(format, u64::from(a));
        if let Some(nan) = self.nans(format, &[bits]) {
            return crate::word::low(nan);
        }
        let a = crate::word::low(bits);
        match class(format, bits) {
            Class::Zero => {
                self.flags |= fpscr::DZC;
                return (a & 0x8000_0000) | 0x7F80_0000;
            }
            _ if a >> 31 != 0 => {
                self.flags |= fpscr::IOC;
                return crate::word::low(format.default_nan());
            }
            Class::Infinity => return 0,
            _ => {}
        }
        let (exp, fraction) = normalised_fraction(a, true);
        let scaled = if exp & 1 == 0 {
            1 << 8 | fraction >> 44
        } else {
            1 << 7 | fraction >> 45
        };
        let estimate = recip_sqrt(crate::word::low(scaled));
        let exp = (380 - exp) / 2;
        (exp.cast_unsigned() & 0xFF) << 23 | (estimate & 0xFF) << 15
    }

    /// `VRECPS`: `2 − a × b`, infinity times zero giving two.
    pub fn recip_step(&mut self, a: u32, b: u32) -> u32 {
        let result = match self.step_product(a, b) {
            Ok(product) => self.sub(Binary::SINGLE, TWO, product),
            Err(nan) => nan,
        };
        crate::word::low(result)
    }

    /// `VRSQRTS`: `(3 − a × b) ÷ 2`, infinity times zero giving one and a
    /// half; the difference is halved before its one rounding.
    pub fn rsqrt_step(&mut self, a: u32, b: u32) -> u32 {
        let result = match self.step_product(a, b) {
            Ok(product) => {
                let negated = product ^ Binary::SINGLE.sign_bit();
                self.mul_add(Binary::SINGLE, ONE_AND_A_HALF, negated, HALF)
            }
            Err(nan) => nan,
        };
        crate::word::low(result)
    }

    /// A reciprocal step's product, a zero for infinity times zero; or the
    /// NaN an operand makes the result.
    fn step_product(&mut self, a: u32, b: u32) -> Result<u64, u64> {
        let format = Binary::SINGLE;
        let (a, b) = (
            self.input(format, u64::from(a)),
            self.input(format, u64::from(b)),
        );
        if let Some(nan) = self.nans(format, &[a, b]) {
            return Err(nan);
        }
        Ok(match (class(format, a), class(format, b)) {
            (Class::Infinity, Class::Zero) | (Class::Zero, Class::Infinity) => 0,
            _ => self.mul(format, a, b),
        })
    }
}

/// The operands with a lone quiet NaN made the infinity that loses.
const fn number_operands(format: Binary, a: u64, b: u64, greatest: bool) -> (u64, u64) {
    let loser = format.infinity(greatest);
    match (
        matches!(class(format, a), Class::Quiet),
        matches!(class(format, b), Class::Quiet),
    ) {
        (true, false) => (loser, b),
        (false, true) => (a, loser),
        _ => (a, b),
    }
}

/// A nonzero finite single's biased exponent and its fraction at the top of
/// 52 bits, a denormal's normalised: one place past its leading one for the
/// reciprocal, as `FPRecipEstimate` takes it, or wholly for the root.
const fn normalised_fraction(a: u32, wholly: bool) -> (i32, u64) {
    let exp = ((a >> 23) & 0xFF).cast_signed();
    let fraction = ((a & 0x7F_FFFF) as u64) << 29;
    let mask = (1 << 52) - 1;
    if exp != 0 {
        (exp, fraction)
    } else if wholly {
        let zeros = fraction.leading_zeros() - 12;
        (-zeros.cast_signed(), (fraction << (zeros + 1)) & mask)
    } else if fraction >> 51 == 0 {
        (-1, (fraction << 2) & mask)
    } else {
        (0, (fraction << 1) & mask)
    }
}

/// `RecipEstimate`: the reciprocal of `a` in 256 to 511, a value in
/// `[0.5, 1)`, as a value in 256 to 511 in `[1, 2)`.
const fn recip(a: u32) -> u32 {
    let a = a * 2 + 1;
    let b = (1 << 19) / a;
    b.div_ceil(2)
}

/// `RecipSqrtEstimate`: the reciprocal square root of `a` in 128 to 511, a
/// value in `[0.25, 1)`, as a value in 256 to 511 in `[1, 2)`.
const fn recip_sqrt(a: u32) -> u32 {
    let a = if a < 256 {
        a * 2 + 1
    } else {
        ((a >> 1) << 1) * 2 + 2
    };
    // The largest b with a × b² below 2^28, which is never below 512.
    let b = (((1u64 << 28) - 1) / a as u64).isqrt();
    crate::word::low(b.div_ceil(2))
}

/// `VRECPE.U32`: an estimate of a fixed-point reciprocal.
#[must_use]
pub const fn unsigned_recip_estimate(a: u32) -> u32 {
    if a >> 31 == 0 {
        return u32::MAX;
    }
    recip(a >> 23) << 23
}

/// `VRSQRTE.U32`: an estimate of a fixed-point reciprocal square root.
#[must_use]
pub const fn unsigned_rsqrt_estimate(a: u32) -> u32 {
    if a >> 30 == 0 {
        return u32::MAX;
    }
    recip_sqrt(a >> 23) << 23
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `RecipSqrtEstimate` as the architecture writes it, a search upward.
    fn searched(a: u32) -> u32 {
        let a = u64::from(if a < 256 {
            a * 2 + 1
        } else {
            ((a >> 1) << 1) * 2 + 2
        });
        let mut b = 512u64;
        while a * (b + 1) * (b + 1) < 1 << 28 {
            b += 1;
        }
        u32::try_from(b.div_ceil(2)).expect("the estimate is nine bits")
    }

    #[test]
    fn the_root_estimate_is_the_architecture_s_search() {
        for a in 128..512 {
            assert_eq!(recip_sqrt(a), searched(a), "{a}");
        }
    }

    #[test]
    fn the_reciprocal_estimates_span_one_to_two() {
        assert_eq!(recip(256), 511);
        assert_eq!(recip(511), 256);
        assert_eq!(recip_sqrt(128), 511);
        assert_eq!(recip_sqrt(511), 256);
    }
}
