//! Advanced SIMD's lane arithmetic, as Arm's pseudocode defines each
//! operation, on registers held as 128-bit values: a doubleword in the low
//! half. Integer lanes are computed exactly in 128 bits and then wrapped or
//! saturated; floating-point lanes are single precision under the standard
//! controls a [`Unit`] carries.

use tairix_softfloat::{Binary, Mode};

use super::float::{unsigned_recip_estimate, unsigned_rsqrt_estimate, Comparison, Unit};
use crate::instruction::{SimdLongOp, SimdMiscOp, SimdScalarOp, SimdShiftOp, SimdThreeOp};
use crate::word::low64;

const F32: Binary = Binary::SINGLE;
const SIGN32: u64 = 0x8000_0000;

/// A lane width: 8 << `size` bits.
const fn bits(size: u8) -> u32 {
    8 << size
}

/// The mask of a lane's bits.
const fn mask(size: u8) -> u64 {
    u64::MAX >> (64 - bits(size))
}

/// The lanes of 8 << `size` bits a doubleword or quadword holds.
pub const fn lanes(size: u8, q: bool) -> u32 {
    (if q { 128 } else { 64 }) >> (3 + size)
}

/// Lane `index` of `value`.
pub const fn lane(value: u128, size: u8, index: u32) -> u64 {
    low64(value >> (index * bits(size))) & mask(size)
}

/// `value` with lane `index` replaced by `lane`'s low bits.
pub const fn with_lane(value: u128, size: u8, index: u32, lane: u64) -> u128 {
    let shift = index * bits(size);
    let field = (mask(size) as u128) << shift;
    (value & !field) | ((lane as u128 & mask(size) as u128) << shift)
}

/// A lane read as an integer, signed or not.
const fn int(lane: u64, size: u8, unsigned: bool) -> i128 {
    if unsigned {
        lane as i128
    } else {
        let unused = 64 - bits(size);
        ((lane << unused).cast_signed() >> unused) as i128
    }
}

/// `value` saturated to a lane of 8 << `size` bits, signed or not: the
/// lane's bits and whether it saturated.
pub fn saturate(value: i128, size: u8, unsigned: bool) -> (u64, bool) {
    let width = bits(size);
    let (min, max) = if unsigned {
        (0, (1i128 << width) - 1)
    } else {
        (-(1i128 << (width - 1)), (1i128 << (width - 1)) - 1)
    };
    let clamped = value.clamp(min, max);
    (
        low64(clamped.cast_unsigned()) & mask(size),
        clamped != value,
    )
}

/// A wrapped lane's bits.
const fn wrap(value: i128, size: u8) -> u64 {
    low64(value.cast_unsigned()) & mask(size)
}

/// `value` shifted left by `shift`, right where negative, arithmetically:
/// exact within the lane's reach, and past it a value whose low bits are
/// zero and which saturates any lane.
fn shift_left(value: i128, shift: i32, size: u8) -> i128 {
    if shift >= 0 {
        if value == 0 {
            0
        } else if shift.cast_unsigned() >= bits(size) {
            value.signum() << 100
        } else {
            value << shift
        }
    } else {
        value >> shift.unsigned_abs().min(127)
    }
}

/// `value` shifted right by `shift`, rounding to nearest with ties up.
fn round_right(value: i128, shift: u32) -> i128 {
    match shift {
        0 => value,
        1..=99 => (value + (1i128 << (shift - 1))) >> shift,
        _ => 0,
    }
}

/// Every lane of `n` and `m` through `f`, with whether any saturated.
fn each(
    size: u8,
    q: bool,
    n: u128,
    m: u128,
    mut f: impl FnMut(u64, u64, u32) -> (u64, bool),
) -> (u128, bool) {
    let mut result = 0;
    let mut saturated = false;
    for index in 0..lanes(size, q) {
        let (value, sat) = f(lane(n, size, index), lane(m, size, index), index);
        saturated |= sat;
        result = with_lane(result, size, index, value);
    }
    (result, saturated)
}

/// The carry-less product of two polynomials over GF(2), each of at most
/// 64 terms; constant time in the operands.
pub const fn polynomial_multiply(a: u64, b: u64) -> u128 {
    let mut product = 0u128;
    let mut bit = 0;
    while bit < 64 {
        let take = 0u128.wrapping_sub(((b >> bit) & 1) as u128);
        product ^= ((a as u128) << bit) & take;
        bit += 1;
    }
    product
}

/// An integer operation on three registers of one length: the result and
/// whether any lane saturated. `d` is the destination's old value.
#[allow(clippy::too_many_arguments)] // The form's fields, as the instruction carries them.
pub fn three_integer(
    op: SimdThreeOp,
    unsigned: bool,
    size: u8,
    quad: bool,
    old: u128,
    first: u128,
    second: u128,
) -> (u128, bool) {
    use SimdThreeOp as O;
    let width = bits(size);
    match op {
        O::Vand => return (first & second, false),
        O::Vbic => return (first & !second, false),
        O::Vorr => return (first | second, false),
        O::Vorn => return (first | !second, false),
        O::Veor => return (first ^ second, false),
        O::Vbsl => return ((old & first) | (!old & second), false),
        O::Vbit => return ((first & second) | (old & !second), false),
        O::Vbif => return ((old & second) | (first & !second), false),
        _ => {}
    }
    let previous = |index| lane(old, size, index);
    each(size, quad, first, second, |a_bits, b_bits, index| {
        let (a, b) = (int(a_bits, size, unsigned), int(b_bits, size, unsigned));
        let shift = i32::from(a_bits.to_le_bytes()[0].cast_signed());
        let value = int(b_bits, size, unsigned);
        let truth = |holds: bool| (if holds { mask(size) } else { 0 }, false);
        match op {
            O::Vhadd => (wrap((a + b) >> 1, size), false),
            O::Vrhadd => (wrap((a + b + 1) >> 1, size), false),
            O::Vhsub => (wrap((a - b) >> 1, size), false),
            O::Vqadd => saturate(a + b, size, unsigned),
            O::Vqsub => saturate(a - b, size, unsigned),
            O::Vcgt => truth(a > b),
            O::Vcge => truth(a >= b),
            // The shift is the first operand's low byte, the value the second's.
            O::Vshl => (wrap(shift_left(value, shift, size), size), false),
            O::Vqshl => saturate(shift_left(value, shift, size), size, unsigned),
            O::Vrshl => (wrap(rounding_shift(value, shift, size), size), false),
            O::Vqrshl => saturate(rounding_shift(value, shift, size), size, unsigned),
            O::Vmax => (wrap(a.max(b), size), false),
            O::Vmin => (wrap(a.min(b), size), false),
            O::Vabd => (wrap((a - b).abs(), size), false),
            O::Vaba => (
                wrap(int(previous(index), size, true) + (a - b).abs(), size),
                false,
            ),
            O::Vadd => (wrap(a + b, size), false),
            O::Vsub => (wrap(a - b, size), false),
            O::Vtst => truth(a_bits & b_bits != 0),
            O::Vceq => truth(a_bits == b_bits),
            O::Vmla => (wrap(int(previous(index), size, true) + a * b, size), false),
            O::Vmls => (wrap(int(previous(index), size, true) - a * b, size), false),
            O::Vmul => (wrap(a * b, size), false),
            O::Vmulp => (
                low64(polynomial_multiply(a_bits, b_bits)) & mask(size),
                false,
            ),
            O::Vqdmulh => saturate((2 * a * b) >> width, size, false),
            O::Vqrdmulh => saturate((2 * a * b + (1i128 << (width - 1))) >> width, size, false),
            _ => (0, false),
        }
    })
}

/// `value` shifted by `shift`, rounding a right shift to nearest.
fn rounding_shift(value: i128, shift: i32, size: u8) -> i128 {
    if shift >= 0 {
        shift_left(value, shift, size)
    } else {
        round_right(value, shift.unsigned_abs())
    }
}

/// A pairwise operation on two doublewords: the first half of the result's
/// lanes from adjacent pairs of `n`, the second from `m`'s.
pub fn pairwise(size: u8, n: u64, m: u64, mut f: impl FnMut(u64, u64) -> u64) -> u64 {
    let count = 8 >> size;
    let half = count / 2;
    let mut result = 0u128;
    for index in 0..count {
        let (source, pair) = if index < half {
            (n, index)
        } else {
            (m, index - half)
        };
        let source = u128::from(source);
        let value = f(
            lane(source, size, 2 * pair),
            lane(source, size, 2 * pair + 1),
        );
        result = with_lane(result, size, index, value);
    }
    low64(result)
}

/// `VPADD`, `VPMAX` and `VPMIN` on integer lanes.
pub fn pairwise_integer(op: SimdThreeOp, unsigned: bool, size: u8, n: u64, m: u64) -> u64 {
    pairwise(size, n, m, |a, b| {
        let (a, b) = (int(a, size, unsigned), int(b, size, unsigned));
        wrap(
            match op {
                SimdThreeOp::Vpmax => a.max(b),
                SimdThreeOp::Vpmin => a.min(b),
                _ => a + b,
            },
            size,
        )
    })
}

/// `VPADD`, `VPMAX` and `VPMIN` on single-precision lanes.
pub fn pairwise_float(op: SimdThreeOp, unit: &mut Unit, n: u64, m: u64) -> u64 {
    pairwise(2, n, m, |a, b| match op {
        SimdThreeOp::VpmaxF => unit.max(F32, a, b),
        SimdThreeOp::VpminF => unit.min(F32, a, b),
        _ => unit.add(F32, a, b),
    })
}

/// A single-precision operation on three registers of one length.
pub fn three_float(op: SimdThreeOp, unit: &mut Unit, q: bool, d: u128, n: u128, m: u128) -> u128 {
    use SimdThreeOp as O;
    let (result, _) = each(2, q, n, m, |a, b, index| {
        let old = lane(d, 2, index);
        let truth = |holds: bool| if holds { mask(2) } else { 0 };
        let value = match op {
            O::VaddF => unit.add(F32, a, b),
            O::VsubF => unit.sub(F32, a, b),
            O::VmulF => unit.mul(F32, a, b),
            O::VmlaF => {
                let product = unit.mul(F32, a, b);
                unit.add(F32, old, product)
            }
            O::VmlsF => {
                let product = unit.mul(F32, a, b);
                unit.add(F32, old, product ^ SIGN32)
            }
            O::Vfma => unit.mul_add(F32, old, a, b),
            O::Vfms => unit.mul_add(F32, old, a ^ SIGN32, b),
            O::VabdF => unit.sub(F32, a, b) & !SIGN32,
            O::VceqF => truth(unit.test(F32, a, b, Comparison::Equal)),
            O::VcgeF => truth(unit.test(F32, a, b, Comparison::GreaterOrEqual)),
            O::VcgtF => truth(unit.test(F32, a, b, Comparison::Greater)),
            O::Vacge => truth(unit.test(F32, a & !SIGN32, b & !SIGN32, Comparison::GreaterOrEqual)),
            O::Vacgt => truth(unit.test(F32, a & !SIGN32, b & !SIGN32, Comparison::Greater)),
            O::VmaxF => unit.max(F32, a, b),
            O::VminF => unit.min(F32, a, b),
            O::Vrecps => u64::from(unit.recip_step(lo32(a), lo32(b))),
            O::Vrsqrts => u64::from(unit.rsqrt_step(lo32(a), lo32(b))),
            O::Vmaxnm => unit.max_number(F32, a, b),
            O::Vminnm => unit.min_number(F32, a, b),
            _ => 0,
        };
        (value, false)
    });
    result
}

const fn lo32(value: u64) -> u32 {
    crate::word::low(value)
}

/// An operation whose operands differ in length, `size` the narrower lanes':
/// the result and whether any lane saturated. `d` is the destination's old
/// value, wide or narrow as the operation's result is.
pub fn long(op: SimdLongOp, unsigned: bool, size: u8, d: u128, n: u128, m: u128) -> (u128, bool) {
    use SimdLongOp as O;
    let wide = size + 1;
    let width = bits(size);
    let count = lanes(size, false);
    let mut result = 0u128;
    let mut saturated = false;
    if op == O::Vmullp64 {
        return (polynomial_multiply(low64(n), low64(m)), false);
    }
    for index in 0..count {
        let narrow_n = int(lane(n, size, index), size, unsigned);
        let narrow_m = int(lane(m, size, index), size, unsigned);
        let wide_n = int(lane(n, wide, index), wide, unsigned);
        let wide_m = int(lane(m, wide, index), wide, true);
        let old = int(lane(d, wide, index), wide, true);
        let (value, sat, narrowed) = match op {
            O::Vaddl => (wrap(narrow_n + narrow_m, wide), false, false),
            O::Vaddw => (wrap(wide_n + narrow_m, wide), false, false),
            O::Vsubl => (wrap(narrow_n - narrow_m, wide), false, false),
            O::Vsubw => (wrap(wide_n - narrow_m, wide), false, false),
            O::Vaddhn => (wrap((wide_n + wide_m) >> width, size), false, true),
            O::Vraddhn => (
                wrap((wide_n + wide_m + (1 << (width - 1))) >> width, size),
                false,
                true,
            ),
            O::Vsubhn => (wrap((wide_n - wide_m) >> width, size), false, true),
            O::Vrsubhn => (
                wrap((wide_n - wide_m + (1 << (width - 1))) >> width, size),
                false,
                true,
            ),
            O::Vabal => (wrap(old + (narrow_n - narrow_m).abs(), wide), false, false),
            O::Vabdl => (wrap((narrow_n - narrow_m).abs(), wide), false, false),
            O::Vmlal => (wrap(old + narrow_n * narrow_m, wide), false, false),
            O::Vmlsl => (wrap(old - narrow_n * narrow_m, wide), false, false),
            O::Vmull => (wrap(narrow_n * narrow_m, wide), false, false),
            O::Vqdmull | O::Vqdmlal | O::Vqdmlsl => {
                let (product, first) = saturate(2 * narrow_n * narrow_m, wide, false);
                let product = int(product, wide, false);
                let (value, second) = match op {
                    O::Vqdmlal => saturate(
                        int(lane(d, wide, index), wide, false) + product,
                        wide,
                        false,
                    ),
                    O::Vqdmlsl => saturate(
                        int(lane(d, wide, index), wide, false) - product,
                        wide,
                        false,
                    ),
                    _ => (wrap(product, wide), false),
                };
                (value, first || second, false)
            }
            O::Vmullp => {
                let product = polynomial_multiply(lane(n, size, index), lane(m, size, index));
                (low64(product) & mask(wide), false, false)
            }
            O::Vmullp64 => (0, false, false),
        };
        saturated |= sat;
        result = if narrowed {
            with_lane(result, size, index, value)
        } else {
            with_lane(result, wide, index, value)
        };
    }
    (result, saturated)
}

/// An operation by a scalar: each lane of `n` with `scalar`. Long results
/// are in `size` + 1's lanes. `d` is the destination's old value.
#[allow(clippy::too_many_arguments)] // The form's fields, as the instruction carries them.
pub fn by_scalar(
    op: SimdScalarOp,
    unsigned: bool,
    size: u8,
    q: bool,
    unit: &mut Unit,
    d: u128,
    n: u128,
    scalar: u64,
) -> (u128, bool) {
    use SimdScalarOp as O;
    if op.long() {
        let long_op = match op {
            O::Vmlal => SimdLongOp::Vmlal,
            O::Vmlsl => SimdLongOp::Vmlsl,
            O::Vmull => SimdLongOp::Vmull,
            O::Vqdmlal => SimdLongOp::Vqdmlal,
            O::Vqdmlsl => SimdLongOp::Vqdmlsl,
            _ => SimdLongOp::Vqdmull,
        };
        return long(long_op, unsigned, size, d, n, replicate(scalar, size));
    }
    if op.float() {
        let three = match op {
            O::VmlaF => SimdThreeOp::VmlaF,
            O::VmlsF => SimdThreeOp::VmlsF,
            _ => SimdThreeOp::VmulF,
        };
        return (
            three_float(three, unit, q, d, n, replicate(scalar, 2)),
            false,
        );
    }
    let three = match op {
        O::Vmla => SimdThreeOp::Vmla,
        O::Vmls => SimdThreeOp::Vmls,
        O::Vqdmulh => SimdThreeOp::Vqdmulh,
        O::Vqrdmulh => SimdThreeOp::Vqrdmulh,
        _ => SimdThreeOp::Vmul,
    };
    three_integer(three, false, size, q, d, n, replicate(scalar, size))
}

/// `value`'s low lane in every lane of a quadword.
pub const fn replicate(value: u64, size: u8) -> u128 {
    let mut result = 0u128;
    let mut index = 0;
    while index < lanes(size, true) {
        result = with_lane(result, size, index, value);
        index += 1;
    }
    result
}

/// A shift by an immediate: the result and whether any lane saturated.
/// `size` is the operand's lanes'; `d` the destination's old value.
#[allow(clippy::too_many_arguments)] // The form's fields, as the instruction carries them.
pub fn shift(
    op: SimdShiftOp,
    unsigned: bool,
    size: u8,
    q: bool,
    amount: u8,
    unit: &mut Unit,
    d: u128,
    m: u128,
) -> (u128, bool) {
    use SimdShiftOp as O;
    let amount_bits = u32::from(amount);
    let shift_amount = i32::from(amount);
    match op {
        O::VcvtFromFixed | O::VcvtToFixed => {
            let (result, _) = each(2, q, m, 0, |value, _, _| {
                let value = if op == O::VcvtToFixed {
                    u64::from(unit.to_fixed(F32, value, amount_bits, unsigned, 32, Mode::Zero))
                } else {
                    unit.from_fixed(F32, lo32(value), 32, amount_bits, unsigned, Mode::Nearest)
                };
                (value, false)
            });
            return (result, false);
        }
        O::Vshrn | O::Vrshrn | O::Vqshrun | O::Vqrshrun | O::Vqshrn | O::Vqrshrn => {
            return narrow_shift(op, unsigned, size, amount_bits, m);
        }
        O::Vshll | O::Vmovl => {
            let wide = size + 1;
            let mut result = 0u128;
            for index in 0..lanes(size, false) {
                let value = int(lane(m, size, index), size, unsigned) << amount_bits;
                result = with_lane(result, wide, index, wrap(value, wide));
            }
            return (result, false);
        }
        _ => {}
    }
    let width = bits(size);
    each(size, q, m, d, |value_bits, old, _| {
        let value = int(value_bits, size, unsigned);
        let right = value >> amount_bits.min(127);
        let rounded = round_right(value, amount_bits);
        let insert = |shifted: u64, keep: u64| ((old & keep) | (shifted & !keep)) & mask(size);
        match op {
            O::Vshr => (wrap(right, size), false),
            O::Vsra => (wrap(int(old, size, true) + right, size), false),
            O::Vrshr => (wrap(rounded, size), false),
            O::Vrsra => (wrap(int(old, size, true) + rounded, size), false),
            O::Vsri => {
                let field = if amount_bits >= width {
                    0
                } else {
                    mask(size) >> amount_bits
                };
                let shifted = if amount_bits >= width {
                    0
                } else {
                    value_bits >> amount_bits
                };
                (insert(shifted, !field), false)
            }
            O::Vshl => (wrap(value << amount_bits, size), false),
            O::Vsli => {
                let field = (mask(size) << amount_bits) & mask(size);
                (insert(value_bits << amount_bits, !field), false)
            }
            O::Vqshlu => saturate(
                shift_left(int(value_bits, size, false), shift_amount, size),
                size,
                true,
            ),
            O::Vqshl => saturate(shift_left(value, shift_amount, size), size, unsigned),
            _ => (0, false),
        }
    })
}

/// A narrowing right shift: quadword lanes of `size` + 1 into doubleword
/// lanes of `size`.
fn narrow_shift(op: SimdShiftOp, unsigned: bool, size: u8, amount: u32, m: u128) -> (u128, bool) {
    use SimdShiftOp as O;
    let wide = size + 1;
    let source_unsigned = match op {
        O::Vqshrun | O::Vqrshrun => false,
        O::Vshrn | O::Vrshrn => true,
        _ => unsigned,
    };
    let mut result = 0u128;
    let mut saturated = false;
    for index in 0..lanes(size, false) {
        let value = int(lane(m, wide, index), wide, source_unsigned);
        let shifted = match op {
            O::Vrshrn | O::Vqrshrun | O::Vqrshrn => round_right(value, amount),
            _ => value >> amount,
        };
        let (lane_value, sat) = match op {
            O::Vshrn | O::Vrshrn => (wrap(shifted, size), false),
            O::Vqshrun | O::Vqrshrun => saturate(shifted, size, true),
            _ => saturate(shifted, size, unsigned),
        };
        saturated |= sat;
        result = with_lane(result, size, index, lane_value);
    }
    (result, saturated)
}

/// An operation on two registers: the result and whether any lane
/// saturated. `d` is the destination's old value. The permutations, which
/// write both registers, are [`permute`]'s.
#[allow(clippy::too_many_arguments)] // The form's fields, as the instruction carries them.
#[allow(clippy::too_many_lines)] // One arm per operation, as the manual lists them.
pub fn misc(
    op: SimdMiscOp,
    unsigned: bool,
    size: u8,
    q: bool,
    unit: &mut Unit,
    d: u128,
    m: u128,
) -> (u128, bool) {
    use SimdMiscOp as O;
    let width = bits(size);
    match op {
        O::Vrev64 | O::Vrev32 | O::Vrev16 => {
            let container = match op {
                O::Vrev64 => 3,
                O::Vrev32 => 2,
                _ => 1,
            };
            let per = 1u32 << (container - size);
            let (result, _) = each(size, q, m, 0, |_, _, index| {
                let base = index & !(per - 1);
                let reversed = base + (per - 1 - (index & (per - 1)));
                (lane(m, size, reversed), false)
            });
            return (result, false);
        }
        O::Vpaddl | O::Vpadal => {
            let wide = size + 1;
            let mut result = 0u128;
            for index in 0..lanes(wide, q) {
                let sum = int(lane(m, size, 2 * index), size, unsigned)
                    + int(lane(m, size, 2 * index + 1), size, unsigned);
                let total = if op == O::Vpadal {
                    sum + int(lane(d, wide, index), wide, true)
                } else {
                    sum
                };
                result = with_lane(result, wide, index, wrap(total, wide));
            }
            return (result, false);
        }
        O::Vmvn => return (!m & quad_mask(q), false),
        O::Vmovn | O::Vqmovun | O::Vqmovn => {
            let wide = size + 1;
            let source_unsigned = op == O::Vqmovn && unsigned;
            let mut result = 0u128;
            let mut saturated = false;
            for index in 0..lanes(size, false) {
                let value = int(lane(m, wide, index), wide, source_unsigned);
                let (narrowed, sat) = match op {
                    O::Vmovn => (wrap(value, size), false),
                    O::Vqmovun => saturate(value, size, true),
                    _ => saturate(value, size, unsigned),
                };
                saturated |= sat;
                result = with_lane(result, size, index, narrowed);
            }
            return (result, saturated);
        }
        O::Vshll => {
            let wide = size + 1;
            let mut result = 0u128;
            for index in 0..lanes(size, false) {
                result = with_lane(result, wide, index, lane(m, size, index) << width);
            }
            return (result, false);
        }
        O::VcvtToHalf => {
            let mut result = 0u128;
            for index in 0..4 {
                let half = unit.convert(F32, Binary::HALF, lane(m, 2, index));
                result = with_lane(result, 1, index, half);
            }
            return (result, false);
        }
        O::VcvtFromHalf => {
            let mut result = 0u128;
            for index in 0..4 {
                let single = unit.convert(Binary::HALF, F32, lane(m, 1, index));
                result = with_lane(result, 2, index, single);
            }
            return (result, false);
        }
        _ => {}
    }
    each(size, q, m, 0, |value_bits, _, _| {
        let value = int(value_bits, size, false);
        let truth = |holds: bool| (if holds { mask(size) } else { 0 }, false);
        let mode_of = |op| match op {
            O::Vrinta | O::Vcvta => Mode::NearestAway,
            O::Vrintz => Mode::Zero,
            O::Vrintm | O::Vcvtm => Mode::Down,
            O::Vrintp | O::Vcvtp => Mode::Up,
            _ => Mode::Nearest,
        };
        match op {
            O::Vcls => {
                let unused = 64 - width;
                let shifted = value_bits << unused;
                let ones = if (shifted.cast_signed()) < 0 {
                    (!shifted).leading_zeros()
                } else {
                    shifted.leading_zeros()
                };
                (u64::from(ones.min(width) - 1), false)
            }
            O::Vclz => {
                let unused = 64 - width;
                let count = if value_bits == 0 {
                    width
                } else {
                    value_bits.leading_zeros() - unused
                };
                (u64::from(count), false)
            }
            O::Vcnt => (u64::from(value_bits.count_ones()), false),
            O::Vqabs => saturate(value.abs(), size, false),
            O::Vqneg => saturate(-value, size, false),
            O::Vcgt0 => truth(value > 0),
            O::Vcge0 => truth(value >= 0),
            O::Vceq0 => truth(value == 0),
            O::Vcle0 => truth(value <= 0),
            O::Vclt0 => truth(value < 0),
            O::Vabs => (wrap(value.abs(), size), false),
            O::Vneg => (wrap(-value, size), false),
            O::Vcgt0F => truth(unit.test(F32, value_bits, 0, Comparison::Greater)),
            O::Vcge0F => truth(unit.test(F32, value_bits, 0, Comparison::GreaterOrEqual)),
            O::Vceq0F => truth(unit.test(F32, value_bits, 0, Comparison::Equal)),
            O::Vcle0F => truth(unit.test(F32, 0, value_bits, Comparison::GreaterOrEqual)),
            O::Vclt0F => truth(unit.test(F32, 0, value_bits, Comparison::Greater)),
            O::VabsF => (value_bits & !SIGN32, false),
            O::VnegF => (value_bits ^ SIGN32, false),
            O::Vrintn | O::Vrinta | O::Vrintz | O::Vrintm | O::Vrintp => (
                unit.round_integral(F32, value_bits, mode_of(op), false),
                false,
            ),
            O::Vrintx => (
                unit.round_integral(F32, value_bits, Mode::Nearest, true),
                false,
            ),
            O::Vcvta | O::Vcvtn | O::Vcvtp | O::Vcvtm => (
                u64::from(unit.to_fixed(F32, value_bits, 0, unsigned, 32, mode_of(op))),
                false,
            ),
            O::VcvtToInteger => (
                u64::from(unit.to_fixed(F32, value_bits, 0, unsigned, 32, Mode::Zero)),
                false,
            ),
            O::VcvtToFloat => (
                unit.from_fixed(F32, lo32(value_bits), 32, 0, unsigned, Mode::Nearest),
                false,
            ),
            O::Vrecpe => (u64::from(unsigned_recip_estimate(lo32(value_bits))), false),
            O::Vrsqrte => (u64::from(unsigned_rsqrt_estimate(lo32(value_bits))), false),
            O::VrecpeF => (u64::from(unit.recip_estimate(lo32(value_bits))), false),
            O::VrsqrteF => (u64::from(unit.rsqrt_estimate(lo32(value_bits))), false),
            _ => (0, false),
        }
    })
}

/// A doubleword's or quadword's bits.
const fn quad_mask(q: bool) -> u128 {
    if q {
        u128::MAX
    } else {
        u64::MAX as u128
    }
}

/// `VSWP`, `VTRN`, `VUZP` and `VZIP`: the new values of `d` and `m`.
pub fn permute(op: SimdMiscOp, size: u8, q: bool, d: u128, m: u128) -> (u128, u128) {
    let count = lanes(size, q);
    let get = |value: u128, index: u32| lane(value, size, index);
    let mut new_d = 0u128;
    let mut new_m = 0u128;
    match op {
        SimdMiscOp::Vtrn => {
            for pair in 0..count / 2 {
                let (even, odd) = (2 * pair, 2 * pair + 1);
                new_d = with_lane(new_d, size, even, get(d, even));
                new_d = with_lane(new_d, size, odd, get(m, even));
                new_m = with_lane(new_m, size, even, get(d, odd));
                new_m = with_lane(new_m, size, odd, get(m, odd));
            }
        }
        SimdMiscOp::Vuzp => {
            // The lanes of d then m, the even ones to d and the odd to m.
            for index in 0..count {
                let source = |k: u32| {
                    if k < count {
                        get(d, k)
                    } else {
                        get(m, k - count)
                    }
                };
                new_d = with_lane(new_d, size, index, source(2 * index));
                new_m = with_lane(new_m, size, index, source(2 * index + 1));
            }
        }
        SimdMiscOp::Vzip => {
            for index in 0..count {
                let interleaved = |k: u32| {
                    if k.is_multiple_of(2) {
                        get(d, k / 2)
                    } else {
                        get(m, k / 2)
                    }
                };
                new_d = with_lane(new_d, size, index, interleaved(index));
                new_m = with_lane(new_m, size, index, interleaved(index + count));
            }
        }
        _ => return (m, d),
    }
    (new_d, new_m)
}

/// Advanced SIMD's modified immediate expanded to 64 bits, from the op bit
/// above the four cmode bits.
#[must_use]
pub const fn expand_immediate(cmode: u8, imm8: u8) -> u64 {
    const fn words(word: u64) -> u64 {
        word << 32 | word
    }
    let imm = imm8 as u64;
    match cmode & 0xF {
        0 | 1 => words(imm),
        2 | 3 => words(imm << 8),
        4 | 5 => words(imm << 16),
        6 | 7 => words(imm << 24),
        8 | 9 => imm * 0x0001_0001_0001_0001,
        10 | 11 => (imm << 8) * 0x0001_0001_0001_0001,
        12 => words(imm << 8 | 0xFF),
        13 => words(imm << 16 | 0xFFFF),
        14 if cmode & 0x10 == 0 => imm * 0x0101_0101_0101_0101,
        14 => {
            let mut mask = 0u64;
            let mut bit = 0;
            while bit < 8 {
                if (imm >> bit) & 1 != 0 {
                    mask |= 0xFF << (8 * bit);
                }
                bit += 1;
            }
            mask
        }
        _ => words(super::expand_immediate(imm8, false)),
    }
}

/// `VEXT`: bytes from `start` of the concatenation `m:n`.
pub const fn extract(q: bool, n: u128, m: u128, start: u8) -> u128 {
    let shift = 8 * start as u32;
    if q {
        if shift == 0 {
            n
        } else {
            (n >> shift) | (m << (128 - shift))
        }
    } else {
        let joined = (m & u64::MAX as u128) << 64 | (n & u64::MAX as u128);
        (joined >> shift) & u64::MAX as u128
    }
}

/// `VTBL` and `VTBX`: each byte of `indices` looked up in `table`'s
/// `length` doublewords; an index past them is zero, or `d`'s byte for
/// `VTBX`.
pub fn table(table: &[u64; 4], length: u8, extend: bool, d: u64, indices: u64) -> u64 {
    let mut bytes = [0u8; 32];
    let (chunks, _) = bytes.as_chunks_mut::<8>();
    for (chunk, word) in chunks.iter_mut().zip(table) {
        *chunk = word.to_le_bytes();
    }
    let limit = 8 * usize::from(length);
    let old = d.to_le_bytes();
    let mut result = [0u8; 8];
    for (index, (slot, &byte)) in result.iter_mut().zip(&indices.to_le_bytes()).enumerate() {
        let wanted = usize::from(byte);
        *slot = if wanted < limit {
            bytes[wanted]
        } else if extend {
            old[index]
        } else {
            0
        };
    }
    u64::from_le_bytes(result)
}
