//! Where a case may part from FPEmulator without the FPA being wrong.
//!
//! The FPA rounds each transcendental value and packed decimal conversion
//! correctly. FPEmulator does neither, so a case there agrees when the two lie
//! within FPEmulator's error. Some operands FPEmulator mishandles, and the data
//! sheet leaves some undefined; a case reading one is not held to FPEmulator.

use tairix_riscos_arm::fpa::{fpsr, Exception, Format, Internal, Precision};
use tairix_riscos_arm::instruction::{FpaDyadicOp, FpaMonadicOp, FpaOperand, FpaTransferOp, Op};
use tairix_riscos_arm::vectors::FpaState;
use tairix_softfloat::{exact, Decimal, Exact, Mode, Packed, Parts, Rounding};

/// FPEmulator's transcendental error, in places of the result: a fixed
/// allowance, and so many places of the arguments, scaled by how far the
/// function magnifies them.
const TRANSCENDENTAL_PLACES: f64 = 16.0;
const ARGUMENT_PLACES: f64 = 2.0;

/// FPEmulator's packed decimal error, in places of the value: some 150 at the
/// far ends of the exponent range, measured over 400 000 conversions.
const PACKED_PLACES: f64 = 256.0;

/// What a run left, as the comparison saw it.
pub(super) struct Left {
    /// The exception it trapped on.
    pub trap: Option<Exception>,
    /// The FPA's registers and FPSR.
    pub fpa: FpaState,
    /// The words the instruction read or wrote, at the addresses the native
    /// run moved them at.
    pub moved: Vec<u32>,
}

/// One differing case.
pub(super) struct Difference<'a> {
    pub op: &'a Op,
    /// The FPA's state before.
    pub before: &'a FpaState,
    pub fpe: Left,
    pub native: Left,
    /// Whether everything but the FPA's state and the moved words agreed:
    /// the processor's registers or a trap's dump, and the rest of memory.
    pub rest_same: bool,
}

/// How a differing case is accepted.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Accepted {
    /// Within FPEmulator's error.
    Accurate,
    /// Not held to FPEmulator.
    Unheld(Unheld),
}

/// Why a case is not held to FPEmulator.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Unheld {
    /// A transcendental function of an unnormal, which the data sheet calls
    /// illegal, or of or to a value in the denormal range, where FPEmulator
    /// leaves unnormalised forms and garbage and flags no underflow.
    Transcendental,
    /// A remainder with an uncommon operand, which FPEmulator misreads.
    UncommonRemainder,
    /// A packed load of digits above nine, which the data sheet leaves
    /// undefined.
    UndefinedPacked,
}

impl Unheld {
    pub(super) const ALL: [Self; 3] = [
        Self::Transcendental,
        Self::UncommonRemainder,
        Self::UndefinedPacked,
    ];

    pub(super) const fn reason(self) -> &'static str {
        match self {
            Self::Transcendental => "transcendentals of unnormals or in the denormal range",
            Self::UncommonRemainder => "remainders of uncommon operands",
            Self::UndefinedPacked => "packed loads of digits above nine",
        }
    }
}

const UNCOMMON: u32 = 1 << 30;
const UNITS: u64 = 1 << 63;

/// Whether a register's value lies in the denormal range: an extended
/// exponent of zero with a fraction, or a single or double denormal's marker.
fn denormal_range(words: [u32; 3]) -> bool {
    let r = Internal::from_words(words);
    let exponent = r.head & 0x7FFF;
    let marked = r.head & UNCOMMON != 0
        && matches!(exponent, 0x3F80 | 0x3C00)
        && r.mantissa & UNITS != 0
        && r.mantissa != UNITS;
    (exponent == 0 && r.mantissa != 0) || marked
}

/// Whether a register holds an unnormal: a number with an exponent but no
/// units bit, which only `URD` and a load of an illegal value leave.
fn unnormal(words: [u32; 3]) -> bool {
    let r = Internal::from_words(words);
    let exponent = r.head & 0x7FFF;
    r.value().is_some() && exponent != 0 && r.mantissa & UNITS == 0 && r.mantissa != 0
}

/// `x`'s magnitude in places of `of`'s significand.
fn places(x: Exact, of: Parts) -> f64 {
    let x = x.normalized(true);
    // An allowance, not a value: a double holds enough of the significand.
    #[allow(clippy::cast_precision_loss)]
    let sig = x.sig as f64;
    sig * 2f64.powi((x.exp - 126 - (of.normalized().exp - 63)).clamp(-2000, 2000))
}

/// A value's nearest double, for scaling an allowance.
fn approximate(p: Parts) -> f64 {
    let p = p.normalized();
    #[allow(clippy::cast_precision_loss)] // An allowance, not a value.
    let sig = p.sig as f64;
    let magnitude = sig * 2f64.powi((p.exp - 63).clamp(-1100, 1100));
    if p.negative {
        -magnitude
    } else {
        magnitude
    }
}

/// A transcendental data operation and the operands it reads.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Function {
    Monadic(FpaMonadicOp),
    Dyadic(FpaDyadicOp),
}

impl Function {
    fn of(op: &Op) -> Option<(Self, Precision, u8, [Option<FpaOperand>; 2])> {
        match *op {
            Op::FpaMonadic {
                op,
                precision,
                fd,
                fm,
                ..
            } if matches!(
                op,
                FpaMonadicOp::Log
                    | FpaMonadicOp::Lgn
                    | FpaMonadicOp::Exp
                    | FpaMonadicOp::Sin
                    | FpaMonadicOp::Cos
                    | FpaMonadicOp::Tan
                    | FpaMonadicOp::Asn
                    | FpaMonadicOp::Acs
                    | FpaMonadicOp::Atn
            ) =>
            {
                Some((Self::Monadic(op), precision, fd, [Some(fm), None]))
            }
            Op::FpaDyadic {
                op,
                precision,
                fd,
                fn_,
                fm,
                ..
            } if matches!(op, FpaDyadicOp::Pow | FpaDyadicOp::Rpw | FpaDyadicOp::Pol) => Some((
                Self::Dyadic(op),
                precision,
                fd,
                [Some(FpaOperand::Register(fn_)), Some(fm)],
            )),
            _ => None,
        }
    }

    /// How many places of the result one place of the arguments moves.
    fn condition(self, args: &[f64]) -> f64 {
        let ratio = |n: f64, d: f64| {
            if d == 0.0 {
                f64::INFINITY
            } else {
                (n / d).abs()
            }
        };
        match (self, args) {
            (Self::Monadic(op), &[x]) => match op {
                FpaMonadicOp::Sin => ratio(x * x.cos(), x.sin()),
                FpaMonadicOp::Cos => ratio(x * x.sin(), x.cos()),
                FpaMonadicOp::Tan => ratio(x, x.sin() * x.cos()),
                FpaMonadicOp::Exp => x.abs(),
                FpaMonadicOp::Log | FpaMonadicOp::Lgn => ratio(1.0, x.abs().ln()),
                FpaMonadicOp::Asn => ratio(x, (1.0 - x * x).sqrt() * x.asin()),
                FpaMonadicOp::Acs => ratio(x, (1.0 - x * x).sqrt() * x.acos()),
                _ => 1.0,
            },
            (Self::Dyadic(op @ (FpaDyadicOp::Pow | FpaDyadicOp::Rpw)), &[a, b]) => {
                let (base, power) = if op == FpaDyadicOp::Pow {
                    (a, b)
                } else {
                    (b, a)
                };
                power.abs() * (1.0 + base.abs().ln().abs())
            }
            _ => 1.0,
        }
    }
}

/// An operand's register words before the instruction.
fn operand(before: &FpaState, operand: FpaOperand) -> [u32; 3] {
    match operand {
        FpaOperand::Register(n) => before.registers[usize::from(n & 7)],
        FpaOperand::Constant(n) => Internal::constant(n).words(),
    }
}

/// Whether the two FPA states are the same but for register `fd` and, where
/// `inexact` allows, FPEmulator's inexact flag.
fn same_but(fpe: &FpaState, native: &FpaState, fd: u8, inexact: bool) -> bool {
    let mask = if inexact {
        !Exception::Inexact.flag()
    } else {
        u32::MAX
    };
    (0..8).all(|n| n == usize::from(fd) || fpe.registers[n] == native.registers[n])
        && fpe.fpsr & mask == native.fpsr & mask
}

/// Whether `case` differs from FPEmulator only where FPEmulator is less
/// accurate than the FPA, or reads what FPEmulator is no reference for.
/// `untrapped` runs the FPA again with no trap enabled, and `extended`
/// FPEmulator with none, at extended precision.
pub(super) fn accepted(
    case: &Difference<'_>,
    untrapped: impl FnOnce() -> Option<FpaState>,
    extended: impl FnOnce() -> Option<FpaState>,
) -> Option<Accepted> {
    if let Some((function, precision, fd, operands)) = Function::of(case.op) {
        return transcendental(case, function, precision, fd, operands, untrapped, extended);
    }
    match *case.op {
        Op::FpaDyadic {
            op: FpaDyadicOp::Rmf,
            fn_,
            fm,
            ..
        } => [FpaOperand::Register(fn_), fm]
            .into_iter()
            .any(|n| {
                let value = Internal::from_words(operand(case.before, n));
                value.head & UNCOMMON != 0 && value.value().is_some()
            })
            .then_some(Accepted::Unheld(Unheld::UncommonRemainder)),
        Op::FpaTransfer {
            op: FpaTransferOp::Ldf,
            format: Format::Packed,
            fd,
            ..
        } => packed_load(case, fd),
        Op::FpaTransfer {
            op: FpaTransferOp::Stf,
            format: Format::Packed,
            fd,
            ..
        } => packed_store(case, fd),
        _ => None,
    }
}

/// A transcendental case: not held where an operand, or either side's value
/// with no trap and the most precision, lies in the denormal range; else
/// within FPEmulator's error, or not.
fn transcendental(
    case: &Difference<'_>,
    function: Function,
    precision: Precision,
    fd: u8,
    operands: [Option<FpaOperand>; 2],
    untrapped: impl FnOnce() -> Option<FpaState>,
    extended: impl FnOnce() -> Option<FpaState>,
) -> Option<Accepted> {
    let args: Vec<[u32; 3]> = operands
        .into_iter()
        .flatten()
        .map(|n| operand(case.before, n))
        .collect();
    let at = usize::from(fd);
    let theirs = case.fpe.fpa.registers[at];
    let unheld = Some(Accepted::Unheld(Unheld::Transcendental));
    let unusual = |r: [u32; 3]| denormal_range(r) || unnormal(r);
    if args.iter().any(|&r| unusual(r)) || (case.fpe.trap.is_none() && denormal_range(theirs)) {
        return unheld;
    }
    let tiny = |free: Option<FpaState>| {
        free.is_some_and(|free| {
            free.fpsr & !case.before.fpsr & Exception::Underflow.flag() != 0
                || denormal_range(free.registers[at])
        })
    };
    if tiny(untrapped()) || tiny(extended()) {
        return unheld;
    }
    if case.fpe.trap.is_some()
        || case.native.trap.is_some()
        || !case.rest_same
        || case.fpe.moved != case.native.moved
        || !same_but(&case.fpe.fpa, &case.native.fpa, fd, false)
    {
        return None;
    }
    let ours = Internal::from_words(case.native.fpa.registers[at]).value()?;
    let theirs = Internal::from_words(theirs).value()?;
    if ours.negative != theirs.negative || ours.sig == 0 || theirs.sig == 0 {
        return None;
    }
    let apart = places(exact::add(theirs, ours, true), ours);
    let magnified = args
        .iter()
        .map(|&r| Internal::from_words(r).value().map(approximate))
        .collect::<Option<Vec<f64>>>()
        .map_or(f64::INFINITY, |values| function.condition(&values));
    // One place of the precision too, where FPEmulator's error carries a
    // result across a rounding boundary of the precision's.
    let place = 2f64.powi(64 - i32::try_from(precision.bits()).unwrap_or(64));
    let allowed = TRANSCENDENTAL_PLACES + ARGUMENT_PLACES * magnified + place;
    (apart <= allowed).then_some(Accepted::Accurate)
}

/// Whether nibbles `0..count` of `digits` are all decimal digits.
fn decimal(digits: u128, count: u32) -> bool {
    (0..count).all(|n| digits >> (4 * n) & 0xF <= 9)
}

/// Whether a packed value is one the data sheet leaves undefined: a digit
/// above nine, but for the first of a NaN's, which carries its quiet mark.
fn undefined<const W: usize>(p: Packed<W>) -> bool {
    let special = (1 << (4 * Packed::<W>::EXPONENT_DIGITS)) - 1;
    if p.exponent == special {
        return !decimal(p.digits, Packed::<W>::DIGITS - 1);
    }
    p.value().is_none()
}

/// The packed value `words` hold, in the format the FPSR names.
fn packed(expanded: bool, words: &[u32]) -> Option<(bool, Option<Decimal>)> {
    Some(if expanded {
        let p = Packed::<4>::from_words(words.try_into().ok()?);
        (undefined(p), p.value())
    } else {
        let p = Packed::<3>::from_words(words.get(..3)?.try_into().ok()?);
        (undefined(p), p.value())
    })
}

/// A decimal value as the nearest extended one.
fn parts(d: Decimal) -> Option<Parts> {
    let x = d.exact().ok()?.normalized(true);
    let rounded = x.rounded(Rounding::extended(Mode::Nearest));
    Some(Parts {
        negative: x.negative,
        exp: x.exp + i32::from(rounded.carried),
        sig: rounded.sig,
    })
}

/// A packed load: an undefined encoding is not held, and a number within
/// FPEmulator's error agrees.
fn packed_load(case: &Difference<'_>, fd: u8) -> Option<Accepted> {
    let expanded = case.before.fpsr & fpsr::EP != 0;
    let (undefined, _) = packed(expanded, &case.native.moved)?;
    if undefined {
        return Some(Accepted::Unheld(Unheld::UndefinedPacked));
    }
    within_packed_error(case, fd, || {
        let at = usize::from(fd);
        let ours = Internal::from_words(case.native.fpa.registers[at]).value()?;
        let theirs = Internal::from_words(case.fpe.fpa.registers[at]).value()?;
        if ours.negative != theirs.negative {
            return Some(false);
        }
        Some(places(exact::add(theirs, ours, true), ours) <= PACKED_PLACES)
    })
}

/// A packed store: agreeing where the FPA's digits lie within half a digit of
/// the value and FPEmulator's within that and its error.
fn packed_store(case: &Difference<'_>, fd: u8) -> Option<Accepted> {
    let value = Internal::from_words(case.before.registers[usize::from(fd)]).value()?;
    let expanded = case.before.fpsr & fpsr::EP != 0;
    within_packed_error(case, fd, || {
        let (_, theirs) = packed(expanded, &case.fpe.moved)?;
        let (_, ours) = packed(expanded, &case.native.moved)?;
        let (theirs, ours) = (theirs?, ours?);
        let unstored = if expanded { 24 - 21 } else { 19 - 17 };
        // Half the last stored digit, as a share of the value the digits
        // round, then in the value's places: past the range's bottom it lies
        // below what a decimal can be read exactly at.
        #[allow(clippy::cast_precision_loss)] // An allowance, not a value.
        let share = 0.5 * 10f64.powi(unstored) / ours.significand as f64;
        #[allow(clippy::cast_precision_loss)]
        let half = share * value.normalized().sig as f64;
        let error = |d: Decimal| Some(places(exact::add(parts(d)?, value, true), value));
        // The FPA's digits round the value correctly, to a place of reading
        // them back; FPEmulator's may stray by its error besides.
        Some(error(ours)? <= half + 1.0 && error(theirs)? <= half + PACKED_PLACES)
    })
}

/// Whether a packed transfer differs only within FPEmulator's error, as
/// `close` judges it: FPEmulator may also flag inexact, or trap on it, where
/// the FPA's exact conversion does not.
fn within_packed_error(
    case: &Difference<'_>,
    fd: u8,
    close: impl FnOnce() -> Option<bool>,
) -> Option<Accepted> {
    let native_exact = case.native.fpa.fpsr & !case.before.fpsr & Exception::Inexact.flag() == 0;
    if case.fpe.trap == Some(Exception::Inexact) && case.native.trap.is_none() && native_exact {
        return Some(Accepted::Accurate);
    }
    if case.fpe.trap.is_some()
        || case.native.trap.is_some()
        || !case.rest_same
        || !same_but(&case.fpe.fpa, &case.native.fpa, fd, true)
    {
        return None;
    }
    close()?.then_some(Accepted::Accurate)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_place_apart_is_one() {
        let one = Parts {
            negative: false,
            exp: 0,
            sig: UNITS,
        };
        let next = Parts {
            sig: UNITS | 1,
            ..one
        };
        assert!((places(exact::add(next, one, true), one) - 1.0).abs() < 1e-9);
        assert!(places(exact::add(one, one, true), one).abs() < 1e-9);
    }

    #[test]
    fn the_denormal_range_and_an_unnormal_are_told_apart() {
        assert!(denormal_range([0x4000_0000, 0, 1]), "an extended denormal");
        assert!(
            denormal_range([0, 0x0000_0001, 0]),
            "the exponent-zero form"
        );
        assert!(
            denormal_range([0x4000_3F80, 0x8000_0100, 0]),
            "a single one"
        );
        assert!(
            !denormal_range([0x4000_3F80, 0x8000_0000, 0]),
            "a marker of zero"
        );
        assert!(!denormal_range([0x3FFF, 0x8000_0000, 0]), "one");
        assert!(unnormal([0x4001, 0x4000_0000, 0]), "two, unnormalised");
        assert!(
            unnormal([0x4000_4001, 0x4000_0000, 0]),
            "loaded so, uncommon"
        );
        assert!(!unnormal([0x3FFF, 0x8000_0000, 0]), "one");
        assert!(!unnormal([0x4000_7FFF, 0x4000_0000, 0]), "a NaN");
        assert!(!unnormal([0x4000, 0, 0]), "a zero");
    }

    #[test]
    fn a_digit_above_nine_is_undefined_but_for_a_nan_s_mark() {
        let p = |words: [u32; 3]| undefined(Packed::<3>::from_words(words));
        assert!(!p([0x0000_0150, 0, 0]), "1.5");
        assert!(p([0x0000_01A0, 0, 0]), "a digit of ten");
        assert!(p([0x000A_0100, 0, 0]), "an exponent digit of ten");
        assert!(
            !p([0x0FFF_FC61, 0x1686_0184, 0x2738_7903]),
            "a quiet NaN, marked"
        );
        assert!(p([0x0FFF_F80A, 0, 0]), "a NaN with a digit of ten");
        assert!(!p([0x0FFF_F000, 0, 0]), "an infinity");
    }

    #[test]
    fn a_function_s_condition_scales_its_allowance() {
        let sin = Function::Monadic(FpaMonadicOp::Sin);
        assert!((sin.condition(&[1e-3]) - 1.0).abs() < 1e-3, "sin x ≈ x");
        assert!(
            sin.condition(&[core::f64::consts::PI]) > 1e15,
            "near a zero"
        );
        let pow = Function::Dyadic(FpaDyadicOp::Pow);
        let rpw = Function::Dyadic(FpaDyadicOp::Rpw);
        assert!((pow.condition(&[core::f64::consts::E, 3.0]) - 6.0).abs() < 1e-9);
        assert!((rpw.condition(&[3.0, core::f64::consts::E]) - 6.0).abs() < 1e-9);
    }
}
