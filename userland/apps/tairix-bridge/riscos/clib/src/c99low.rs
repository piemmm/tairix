//! The C99 library's low-level part, chunk 4: storage for variable length
//! arrays, and the 64-bit arithmetic and conversions the compiler calls, as
//! RISC OS 5.30's give them.
//!
//! A 64-bit value is two registers, its low word first. Arithmetic wraps;
//! division truncates toward nought, a divisor of nought a trap; a shift's
//! count is unsigned, so a count past 63, a negative one among them, shifts
//! everything out. A conversion to an integer truncates and, where the value
//! has no place in the type, is the FPA's invalid operation. The three
//! comparisons are not APCS: they answer in the flags, as `CMP`, `SUBS` with
//! `SBCS`, and `RSBS` with `RSCS` would.

use tairix_riscos_arm::fpa::Exception;
use tairix_riscos_arm::psr;
use tairix_riscos_kernel::os::{Fail, Os, Regs};
use tairix_softfloat::{fpa_words, from_fpa_words};

use crate::c::alloc;
use crate::c::fpa::{as_client, is_whole, whole_part, widened};
use crate::chunk::{own, Entry};
use crate::kernel::{context, trap};

/// Chunk 4's entries, in RISC OS 5.30's order.
pub(crate) const ENTRIES: [Entry; 50] = own([
    allocauto,
    freeauto,
    from_unsigned,
    from_signed,
    to_word,
    arith::<LONG, ADD>,
    arith::<LONG_UNSIGNED, ADD>,
    arith::<LONG_SIGNED, ADD>,
    arith::<UNSIGNED, ADD>,
    arith::<SIGNED, ADD>,
    arith::<LONG, SUB>,
    arith::<LONG_UNSIGNED, SUB>,
    arith::<LONG_SIGNED, SUB>,
    arith::<UNSIGNED, SUB>,
    arith::<SIGNED, SUB>,
    arith::<LONG, RSB>,
    arith::<LONG_UNSIGNED, RSB>,
    arith::<LONG_SIGNED, RSB>,
    arith::<UNSIGNED, RSB>,
    reverse_subtract_signed_words,
    arith::<LONG, MUL>,
    arith::<LONG_UNSIGNED, MUL>,
    arith::<LONG_SIGNED, MUL>,
    arith::<UNSIGNED, MUL>,
    arith::<SIGNED, MUL>,
    divide::<false, false>,
    divide::<false, true>,
    divide_by_ten::<false>,
    divide::<true, false>,
    divide::<true, true>,
    divide_by_ten::<true>,
    not,
    negate,
    arith::<LONG, AND>,
    arith::<LONG, OR>,
    arith::<LONG, EOR>,
    shift::<LEFT>,
    shift::<RIGHT_UNSIGNED>,
    shift::<RIGHT_SIGNED>,
    compare_unsigned,
    compare::<false>,
    compare::<true>,
    to_double::<false>,
    to_double::<true>,
    to_single::<false>,
    to_single::<true>,
    from_double::<false>,
    from_double::<true>,
    from_single::<false>,
    from_single::<true>,
]);

/// The 64-bit value R`n` and R`n + 1` hold, its low word first.
fn long(regs: &Regs, n: usize) -> u64 {
    from_fpa_words([regs.r[n + 1], regs.r[n]])
}

/// `value`'s low word and its high word.
const fn words(value: u64) -> [u32; 2] {
    let [high, low] = fpa_words(value);
    [low, high]
}

/// `value` answered in R0 and R1.
fn answer(regs: &mut Regs, value: u64) {
    [regs.r[0], regs.r[1]] = words(value);
}

/// How an arithmetic entry takes its operands: both 64-bit, a 64-bit one
/// and a word unsigned or signed, or two words unsigned or signed.
const LONG: u8 = 0;
const LONG_UNSIGNED: u8 = 1;
const LONG_SIGNED: u8 = 2;
const UNSIGNED: u8 = 3;
const SIGNED: u8 = 4;
/// What it does with them.
const ADD: u8 = 0;
const SUB: u8 = 1;
const RSB: u8 = 2;
const MUL: u8 = 3;
const AND: u8 = 4;
const OR: u8 = 5;
const EOR: u8 = 6;

/// Two operands as `FORM` takes them.
fn operands<const FORM: u8>(regs: &Regs) -> (u64, u64) {
    let signed = |word: u32| i64::from(word.cast_signed()).cast_unsigned();
    match FORM {
        LONG => (long(regs, 0), long(regs, 2)),
        LONG_UNSIGNED => (long(regs, 0), u64::from(regs.r[2])),
        LONG_SIGNED => (long(regs, 0), signed(regs.r[2])),
        UNSIGNED => (u64::from(regs.r[0]), u64::from(regs.r[1])),
        _ => (signed(regs.r[0]), signed(regs.r[1])),
    }
}

/// `_ll_add`, `_ll_sub`, `_ll_rsb`, `_ll_mul` and the bitwise operations, in
/// each of their operand forms.
#[expect(clippy::unnecessary_wraps, reason = "an entry's signature")]
fn arith<const FORM: u8, const OP: u8>(_os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (x, y) = operands::<FORM>(regs);
    let value = match OP {
        ADD => x.wrapping_add(y),
        SUB => x.wrapping_sub(y),
        RSB => y.wrapping_sub(x),
        MUL => x.wrapping_mul(y),
        AND => x & y,
        OR => x | y,
        _ => x ^ y,
    };
    answer(regs, value);
    Ok(())
}

/// `_ll_rsbss(a, b)`: `b − a` of two signed words as RISC OS 5.30 works it,
/// the low words' borrow lost from the high, which is the difference of the
/// operands' signs alone.
#[expect(clippy::unnecessary_wraps, reason = "an entry's signature")]
fn reverse_subtract_signed_words(_os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (a, b) = (regs.r[0], regs.r[1]);
    let sign = |word: u32| (word.cast_signed() >> 31).cast_unsigned();
    regs.r[..2].copy_from_slice(&[b.wrapping_sub(a), sign(b).wrapping_sub(sign(a))]);
    Ok(())
}

/// `__rt_allocauto(size)`: a block from the heap, as `malloc` gives one.
fn allocauto(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let statics = context::statics_at(os, regs.r[10])?;
    regs.r[0] = alloc::allocated(os, statics, regs.r[0])?;
    Ok(())
}

/// `__rt_freeauto(block)`: the block marked free, answering nought.
fn freeauto(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let statics = context::statics_at(os, regs.r[10])?;
    alloc::marked_free(os, statics, regs.r[0])?;
    regs.r[0] = 0;
    Ok(())
}

/// `_ll_from_u(x)`.
#[expect(clippy::unnecessary_wraps, reason = "an entry's signature")]
fn from_unsigned(_os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    regs.r[1] = 0;
    Ok(())
}

/// `_ll_from_l(x)`.
#[expect(clippy::unnecessary_wraps, reason = "an entry's signature")]
fn from_signed(_os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    regs.r[1] = (regs.r[0].cast_signed() >> 31).cast_unsigned();
    Ok(())
}

/// `_ll_to_l(x)`: the low word.
#[expect(clippy::unnecessary_wraps, reason = "an entry's signature")]
fn to_word(_os: &mut Os<'_>, _regs: &mut Regs) -> Result<(), Fail> {
    Ok(())
}

/// `_ll_udiv(x, y)`, `_ll_urdv`, `_ll_sdiv` and `_ll_srdv`: the quotient in
/// R0 and R1, the remainder in R2 and R3; a reversed one divides `y` by `x`.
fn divide<const SIGNED_DIVISION: bool, const REVERSED: bool>(
    os: &mut Os<'_>,
    regs: &mut Regs,
) -> Result<(), Fail> {
    let (x, y) = (long(regs, 0), long(regs, 2));
    let (dividend, divisor) = if REVERSED { (y, x) } else { (x, y) };
    if divisor == 0 {
        return trap::at_call(os, trap::DIVIDE_BY_ZERO);
    }
    let (quotient, remainder) = if SIGNED_DIVISION {
        let (n, d) = (dividend.cast_signed(), divisor.cast_signed());
        (
            n.wrapping_div(d).cast_unsigned(),
            n.wrapping_rem(d).cast_unsigned(),
        )
    } else {
        (dividend / divisor, dividend % divisor)
    };
    answer(regs, quotient);
    [regs.r[2], regs.r[3]] = words(remainder);
    Ok(())
}

/// `_ll_udiv10(x)` and `_ll_sdiv10`: the quotient by ten, the remainder in R2
/// and R3.
#[expect(clippy::unnecessary_wraps, reason = "an entry's signature")]
fn divide_by_ten<const SIGNED_DIVISION: bool>(
    _os: &mut Os<'_>,
    regs: &mut Regs,
) -> Result<(), Fail> {
    let x = long(regs, 0);
    let (quotient, remainder) = if SIGNED_DIVISION {
        let n = x.cast_signed();
        ((n / 10).cast_unsigned(), (n % 10).cast_unsigned())
    } else {
        (x / 10, x % 10)
    };
    answer(regs, quotient);
    [regs.r[2], regs.r[3]] = words(remainder);
    Ok(())
}

/// `_ll_not(x)`.
#[expect(clippy::unnecessary_wraps, reason = "an entry's signature")]
fn not(_os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    answer(regs, !long(regs, 0));
    Ok(())
}

/// `_ll_neg(x)`.
#[expect(clippy::unnecessary_wraps, reason = "an entry's signature")]
fn negate(_os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    answer(regs, long(regs, 0).wrapping_neg());
    Ok(())
}

/// Which way a shift goes.
const LEFT: u8 = 0;
const RIGHT_UNSIGNED: u8 = 1;
const RIGHT_SIGNED: u8 = 2;

/// `_ll_shift_l(x, n)`, `_ll_ushift_r` and `_ll_sshift_r`.
#[expect(clippy::unnecessary_wraps, reason = "an entry's signature")]
fn shift<const WAY: u8>(_os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (x, count) = (long(regs, 0), regs.r[2]);
    let value = match WAY {
        LEFT => x.checked_shl(count).unwrap_or(0),
        RIGHT_UNSIGNED => x.checked_shr(count).unwrap_or(0),
        _ => (x.cast_signed() >> count.min(63)).cast_unsigned(),
    };
    answer(regs, value);
    Ok(())
}

/// The flags `a − b − borrow` leaves on 32-bit words, Z from this word
/// alone, as `SUBS` or `SBCS` leaves them.
fn subtraction_flags(a: u32, b: u32, borrow: bool) -> u32 {
    let (partial, first) = a.overflowing_sub(b);
    let (result, second) = partial.overflowing_sub(u32::from(borrow));
    let overflow = (a ^ b) & (a ^ result) & 1 << 31 != 0;
    let mut flags = 0;
    for (flag, on) in [
        (psr::N, result & 1 << 31 != 0),
        (psr::Z, result == 0),
        (psr::C, !(first || second)),
        (psr::V, overflow),
    ] {
        if on {
            flags |= flag;
        }
    }
    flags
}

/// The routine left with R0 to R12 as `regs` holds them and the flags
/// `flags`, V among them, which a return through R14 would clear.
fn leave_with(os: &mut Os<'_>, regs: &Regs, flags: u32) {
    let mut registers = os.registers();
    registers.r[..13].copy_from_slice(&regs.r);
    registers.flags = flags;
    os.leave_with(registers);
}

/// `_ll_cmpu`: `CMP` of the high words, then of the low where they agree;
/// every register kept.
#[expect(clippy::unnecessary_wraps, reason = "an entry's signature")]
fn compare_unsigned(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let high = subtraction_flags(regs.r[1], regs.r[3], false);
    let flags = if high & psr::Z != 0 {
        subtraction_flags(regs.r[0], regs.r[2], false)
    } else {
        high
    };
    leave_with(os, regs, flags);
    Ok(())
}

/// `_ll_cmpge` (`SUBS` then `SBCS`: R0 and R1 the first less the second) and
/// `_ll_cmple` (`RSBS` then `RSCS`: the second less the first).
#[expect(clippy::unnecessary_wraps, reason = "an entry's signature")]
fn compare<const REVERSED: bool>(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (x, y) = (long(regs, 0), long(regs, 2));
    let (a, b) = if REVERSED { (y, x) } else { (x, y) };
    let ([a_low, a_high], [b_low, b_high]) = (words(a), words(b));
    let flags = subtraction_flags(a_high, b_high, a_low < b_low);
    answer(regs, a.wrapping_sub(b));
    leave_with(os, regs, flags);
    Ok(())
}

/// `exception` raised as RISC OS 5.30's conversions raise it: its flag set in
/// the client's status, then its trap taken where the client enables it.
/// False where a trap was raised and came back.
fn raised(os: &mut Os<'_>, exception: Exception) -> Result<bool, Fail> {
    Ok(as_client(os, |unit| unit.flag_then_raise(exception))?.is_some())
}

/// Whether the 64-bit value of the `magnitude` given spans no more binary
/// digits than a significand of `digits` holds, so converts exactly.
const fn spans_at_most(magnitude: u64, digits: u32) -> bool {
    magnitude == 0 || u64::BITS - magnitude.leading_zeros() - magnitude.trailing_zeros() <= digits
}

/// The magnitude of `x`, taken as signed where `SIGNED_VALUE`.
const fn magnitude<const SIGNED_VALUE: bool>(x: u64) -> u64 {
    if SIGNED_VALUE {
        x.cast_signed().unsigned_abs()
    } else {
        x
    }
}

/// `_ll_uto_d(x)` and `_ll_sto_d`: the double, nearest, in R0 and R1;
/// inexact where it is not `x`.
#[expect(
    clippy::cast_precision_loss,
    reason = "the conversion rounds to nearest, as genuine's does"
)]
fn to_double<const SIGNED_VALUE: bool>(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let x = long(regs, 0);
    let value = if SIGNED_VALUE {
        x.cast_signed() as f64
    } else {
        x as f64
    };
    let exact = spans_at_most(magnitude::<SIGNED_VALUE>(x), f64::MANTISSA_DIGITS);
    if exact || raised(os, Exception::Inexact)? {
        let [high, low] = fpa_words(value.to_bits());
        regs.r[0] = high;
        regs.r[1] = low;
    }
    Ok(())
}

/// `_ll_uto_f(x)` and `_ll_sto_f`: the single, nearest, in R0; inexact
/// where it is not `x`.
#[expect(
    clippy::cast_precision_loss,
    reason = "the conversion rounds to nearest, as genuine's does"
)]
fn to_single<const SIGNED_VALUE: bool>(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let x = long(regs, 0);
    let value = if SIGNED_VALUE {
        x.cast_signed() as f32
    } else {
        x as f32
    };
    let exact = spans_at_most(magnitude::<SIGNED_VALUE>(x), f32::MANTISSA_DIGITS);
    if exact || raised(os, Exception::Inexact)? {
        regs.r[0] = value.to_bits();
    }
    Ok(())
}

/// `x` truncated to a 64-bit integer, signed or not, and whether that was
/// exact; none where it has no place there.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "a whole number checked in range"
)]
pub(crate) fn truncated<const SIGNED_VALUE: bool>(x: f64) -> Option<(u64, bool)> {
    const TWO_63: f64 = 9_223_372_036_854_775_808.0;
    const TWO_64: f64 = 18_446_744_073_709_551_616.0;
    let whole = whole_part(x);
    let exact = is_whole(x);
    if SIGNED_VALUE {
        (-TWO_63..TWO_63)
            .contains(&whole)
            .then_some(((whole as i64).cast_unsigned(), exact))
    } else {
        (whole > -1.0 && whole < TWO_64).then_some((whole as u64, exact))
    }
}

/// What a conversion with no answer gives where its invalid operation is
/// not trapped: nought for no number, else the limit on `x`'s side.
pub(crate) fn saturated<const SIGNED_VALUE: bool>(x: f64) -> u64 {
    match (x.is_nan(), SIGNED_VALUE, x.is_sign_negative()) {
        (true, ..) | (false, false, true) => 0,
        (false, false, false) => u64::MAX,
        (false, true, true) => i64::MIN.cast_unsigned(),
        (false, true, false) => i64::MAX.cast_unsigned(),
    }
}

/// `x` converted, truncated: inexact where it was not whole, and where it
/// has no place in the type the invalid operation, saturating where that is
/// not trapped.
fn converted<const SIGNED_VALUE: bool>(
    os: &mut Os<'_>,
    regs: &mut Regs,
    x: f64,
) -> Result<(), Fail> {
    let value = match truncated::<SIGNED_VALUE>(x) {
        Some((value, true)) => Some(value),
        Some((value, false)) => raised(os, Exception::Inexact)?.then_some(value),
        None => raised(os, Exception::InvalidOperation)?.then(|| saturated::<SIGNED_VALUE>(x)),
    };
    if let Some(value) = value {
        answer(regs, value);
    }
    Ok(())
}

/// `_ll_ufrom_d(x)` and `_ll_sfrom_d`, `x` a double in R0 and R1.
fn from_double<const SIGNED_VALUE: bool>(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let x = f64::from_bits(from_fpa_words([regs.r[0], regs.r[1]]));
    converted::<SIGNED_VALUE>(os, regs, x)
}

/// `_ll_ufrom_f(x)` and `_ll_sfrom_f`, `x` a single in R0 as it stands.
fn from_single<const SIGNED_VALUE: bool>(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    converted::<SIGNED_VALUE>(os, regs, widened(regs.r[0]))
}

#[cfg(test)]
mod tests;
