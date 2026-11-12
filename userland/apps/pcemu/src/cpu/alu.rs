//! Flag-exact arithmetic and logic.
//!
//! Every function takes the FLAGS word to update, so a caller can't forget a
//! flag the operation defines. Byte operations take and return values in the
//! low byte of a word.

use super::regs::flag::{AF, CF, OF, PF, SF, ZF};
use crate::bits::lo;

/// The operand-width constants an operation works at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Width {
    /// All the width's bits.
    pub mask: u16,
    /// Its sign bit.
    pub sign: u16,
    /// Its bit count.
    pub bits: u8,
}

/// A byte operand.
pub const BYTE: Width = Width {
    mask: 0x00FF,
    sign: 0x0080,
    bits: 8,
};
/// A word operand.
pub const WORD: Width = Width {
    mask: 0xFFFF,
    sign: 0x8000,
    bits: 16,
};

/// The width a decoded `w` bit selects.
#[must_use]
pub const fn width(w: bool) -> Width {
    if w {
        WORD
    } else {
        BYTE
    }
}

/// Whether a byte has an even number of set bits.
#[must_use]
pub const fn even_parity(byte: u8) -> bool {
    byte.count_ones().is_multiple_of(2)
}

/// Sets SF, ZF and PF from `result`.
pub fn set_szp(flags: &mut u16, result: u16, w: Width) {
    let r = result & w.mask;
    *flags &= !(SF | ZF | PF);
    if r & w.sign != 0 {
        *flags |= SF;
    }
    if r == 0 {
        *flags |= ZF;
    }
    if even_parity(lo(r)) {
        *flags |= PF;
    }
}

fn set(flags: &mut u16, mask: u16, on: bool) {
    if on {
        *flags |= mask;
    } else {
        *flags &= !mask;
    }
}

/// `a + b + carry`, setting every status flag.
pub fn add(flags: &mut u16, a: u16, b: u16, carry: bool, w: Width) -> u16 {
    let (a, b) = (u32::from(a & w.mask), u32::from(b & w.mask));
    let sum = a + b + u32::from(carry);
    let r = crate::bits::low16(sum) & w.mask;
    set(flags, CF, sum > u32::from(w.mask));
    set(flags, AF, (a ^ b ^ sum) & 0x10 != 0);
    set(
        flags,
        OF,
        (u32::from(r) ^ a) & (u32::from(r) ^ b) & u32::from(w.sign) != 0,
    );
    set_szp(flags, r, w);
    r
}

/// `a - b - borrow`, setting every status flag.
pub fn sub(flags: &mut u16, a: u16, b: u16, borrow: bool, w: Width) -> u16 {
    let (a, b) = (u32::from(a & w.mask), u32::from(b & w.mask));
    let diff = a.wrapping_sub(b).wrapping_sub(u32::from(borrow));
    let r = crate::bits::low16(diff) & w.mask;
    set(flags, CF, a < b + u32::from(borrow));
    set(flags, AF, (a ^ b ^ diff) & 0x10 != 0);
    set(
        flags,
        OF,
        (a ^ b) & (a ^ u32::from(r)) & u32::from(w.sign) != 0,
    );
    set_szp(flags, r, w);
    r
}

/// The flags of a logical result: CF, OF and AF clear, SF, ZF and PF from it.
pub fn logic(flags: &mut u16, result: u16, w: Width) -> u16 {
    let r = result & w.mask;
    *flags &= !(CF | OF | AF);
    set_szp(flags, r, w);
    r
}

/// One of the eight group 1 operations.
pub fn alu(flags: &mut u16, op: super::decode::Alu, a: u16, b: u16, w: Width) -> u16 {
    use super::decode::Alu;
    let carry = *flags & CF != 0;
    match op {
        Alu::Add => add(flags, a, b, false, w),
        Alu::Adc => add(flags, a, b, carry, w),
        Alu::Sub | Alu::Cmp => sub(flags, a, b, false, w),
        Alu::Sbb => sub(flags, a, b, carry, w),
        Alu::And => logic(flags, a & b, w),
        Alu::Or => logic(flags, a | b, w),
        Alu::Xor => logic(flags, a ^ b, w),
    }
}

/// INC: ADD 1 that leaves CF alone.
pub fn inc(flags: &mut u16, a: u16, w: Width) -> u16 {
    let carry = *flags & CF;
    let r = add(flags, a, 1, false, w);
    *flags = (*flags & !CF) | carry;
    r
}

/// DEC: SUB 1 that leaves CF alone.
pub fn dec(flags: &mut u16, a: u16, w: Width) -> u16 {
    let carry = *flags & CF;
    let r = sub(flags, a, 1, false, w);
    *flags = (*flags & !CF) | carry;
    r
}

/// NEG: `0 - a`.
pub fn neg(flags: &mut u16, a: u16, w: Width) -> u16 {
    sub(flags, 0, a, false, w)
}

/// A group 2 operation repeated `count` times, step by step as the 8086
/// microcode loops, so CF, OF and AF are those of the final step. A count of
/// zero changes nothing. AF is the 8086 family's; the V-series clears it.
pub fn shift(flags: &mut u16, op: super::decode::Shift, value: u16, count: u8, w: Width) -> u16 {
    use super::decode::Shift;
    if count == 0 {
        return value & w.mask;
    }
    let mut v = value & w.mask;
    let top = w.sign;
    for _ in 0..count {
        let cf = *flags & CF != 0;
        let (next, out) = match op {
            Shift::Rol => {
                let out = v & top != 0;
                (((v << 1) | u16::from(out)) & w.mask, out)
            }
            Shift::Ror => {
                let out = v & 1 != 0;
                ((v >> 1) | if out { top } else { 0 }, out)
            }
            Shift::Rcl => {
                let out = v & top != 0;
                (((v << 1) | u16::from(cf)) & w.mask, out)
            }
            Shift::Rcr => {
                let out = v & 1 != 0;
                ((v >> 1) | if cf { top } else { 0 }, out)
            }
            Shift::Shl => (v << 1 & w.mask, v & top != 0),
            Shift::Shr => (v >> 1, v & 1 != 0),
            Shift::Sar => ((v >> 1) | (v & top), v & 1 != 0),
            Shift::Setmo => (w.mask, false),
        };
        let of = match op {
            Shift::Rol | Shift::Rcl | Shift::Shl => (next & top != 0) != out,
            Shift::Ror | Shift::Rcr => (next & top != 0) != (next & (top >> 1) != 0),
            Shift::Shr => v & top != 0,
            Shift::Sar | Shift::Setmo => false,
        };
        set(flags, CF, out);
        set(flags, OF, of);
        v = next;
    }
    match op {
        // SHL runs as an ALU addition of the value to itself, so AF is that
        // addition's nibble carry: bit 4 of the result.
        Shift::Shl => {
            set_szp(flags, v, w);
            set(flags, AF, v & 0x10 != 0);
        }
        Shift::Shr | Shift::Sar => {
            set_szp(flags, v, w);
            *flags &= !AF;
        }
        Shift::Setmo => {
            *flags &= !(AF | CF | OF);
            set_szp(flags, v, w);
        }
        _ => {}
    }
    v
}

/// DAA: adjusts AL after a packed-BCD addition. With AF set on entry the
/// hardware adds 60h only above 9Fh, not above 99h.
pub fn daa(flags: &mut u16, al: u8) -> u8 {
    decimal_adjust(flags, al, false)
}

/// DAS: adjusts AL after a packed-BCD subtraction, with DAA's threshold.
pub fn das(flags: &mut u16, al: u8) -> u8 {
    decimal_adjust(flags, al, true)
}

fn decimal_adjust(flags: &mut u16, al: u8, subtract: bool) -> u8 {
    let carried = *flags & CF != 0;
    let half_carried = *flags & AF != 0;
    let step = |r: u8, by: u8| {
        if subtract {
            r.wrapping_sub(by)
        } else {
            r.wrapping_add(by)
        }
    };
    let low = al & 0x0F > 9 || half_carried;
    let mut r = if low { step(al, 6) } else { al };
    set(flags, AF, low);
    let high = al > if half_carried { 0x9F } else { 0x99 } || carried;
    if high {
        r = step(r, 0x60);
    }
    set(flags, CF, high);
    let sign_flip = (al ^ r) & if subtract { al } else { r } & 0x80 != 0;
    set(flags, OF, sign_flip);
    set_szp(flags, u16::from(r), BYTE);
    r
}

/// AAA: adjusts AX after an unpacked-BCD addition. AL and AH adjust
/// separately, with no carry from one into the other; SF, ZF, PF and OF are
/// those of the 8-bit addition of 6 (or 0) to AL.
pub fn aaa(flags: &mut u16, ax: u16) -> u16 {
    ascii_adjust(flags, ax, false)
}

/// AAS: adjusts AX after an unpacked-BCD subtraction, as AAA does.
pub fn aas(flags: &mut u16, ax: u16) -> u16 {
    ascii_adjust(flags, ax, true)
}

fn ascii_adjust(flags: &mut u16, ax: u16, subtract: bool) -> u16 {
    let (low, high) = (lo(ax), crate::bits::hi(ax));
    let adjust = low & 0x0F > 9 || *flags & AF != 0;
    let by = if adjust { 6 } else { 0 };
    let keep = *flags & !(SF | ZF | PF | OF);
    let mut alu_flags = *flags;
    let r = if subtract {
        sub(&mut alu_flags, u16::from(low), by, false, BYTE)
    } else {
        add(&mut alu_flags, u16::from(low), by, false, BYTE)
    };
    *flags = keep | (alu_flags & (SF | ZF | PF | OF));
    set(flags, AF | CF, adjust);
    let high = match (adjust, subtract) {
        (false, _) => high,
        (true, false) => high.wrapping_add(1),
        (true, true) => high.wrapping_sub(1),
    };
    crate::bits::word(lo(r) & 0x0F, high)
}

/// The 8086's division microcode (CORD) on `high:low` by `divisor`, step by
/// step, so FLAGS end exactly as the hardware leaves them — on a divide error
/// too, where they are what the error pushes.
///
/// After K. Shirriff's reading of A. Jenner's microcode disassembly
/// ("Reverse-engineering the division microcode in the Intel 8086
/// processor", 2023); checked against hardware-captured vectors.
pub fn cord(
    flags: &mut u16,
    high: u16,
    low: u16,
    divisor: u16,
    w: Width,
) -> Result<Division, DivideError> {
    let (mut a, mut c, b) = (high & w.mask, low & w.mask, divisor & w.mask);
    sub(flags, a, b, false, w);
    let mut cf = *flags & CF != 0;
    if !cf {
        return Err(DivideError::Early { negations: 0 });
    }
    let rotate = |v: &mut u16, cf: &mut bool| {
        let out = *v & w.sign != 0;
        *v = ((*v << 1) | u16::from(*cf)) & w.mask;
        *cf = out;
    };
    let mut subtracted = 0;
    for _ in 0..w.bits {
        rotate(&mut c, &mut cf);
        rotate(&mut a, &mut cf);
        let difference = a.wrapping_sub(b) & w.mask;
        if cf {
            cf = false;
            a = difference;
        } else {
            sub(flags, a, b, false, w);
            cf = *flags & CF != 0;
            if !cf {
                a = difference;
                subtracted += 1;
            }
        }
    }
    rotate(&mut c, &mut cf);
    set(flags, CF, c & w.sign != 0);
    Ok(Division {
        quotient: !c & w.mask,
        remainder: a,
        subtracted,
        negations: 0,
    })
}

/// Where the 8086's division microcode raised a divide error.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DivideError {
    /// At CORD's opening check, before the loop: the quotient cannot fit.
    Early {
        /// Operands PREIDIV negated first.
        negations: u8,
    },
    /// In POSTIDIV, after the loop: the signed quotient overflowed.
    Late,
}

/// A completed 8086 division and the microcode paths its timing depends on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Division {
    /// The quotient.
    pub quotient: u16,
    /// The remainder.
    pub remainder: u16,
    /// Loop passes that compared and subtracted.
    pub subtracted: u8,
    /// Operands PREIDIV negated.
    pub negations: u8,
}

/// The 8086's signed division: PREIDIV takes magnitudes and tracks the
/// result's sign in its internal F1 flag, which a REP prefix presets — the
/// reason REP negates the quotient — then CORD, then POSTIDIV.
pub fn idiv_8086(
    flags: &mut u16,
    high: u16,
    low: u16,
    divisor: u16,
    w: Width,
    rep: bool,
) -> Result<Division, DivideError> {
    let negate = |v: u16| v.wrapping_neg() & w.mask;
    let (mut a, mut c, mut b) = (high & w.mask, low & w.mask, divisor & w.mask);
    let mut f1 = rep;
    let mut negations = 0;
    if a & w.sign != 0 {
        c = neg(flags, c, w);
        a = if *flags & CF != 0 {
            !a & w.mask
        } else {
            negate(a)
        };
        f1 = !f1;
        negations += 1;
    }
    set(flags, CF, b & w.sign != 0);
    if b & w.sign != 0 {
        b = negate(b);
        f1 = !f1;
        negations += 1;
    }
    let mut division = cord(flags, a, c, b, w).map_err(|_| DivideError::Early { negations })?;
    if *flags & CF == 0 {
        return Err(DivideError::Late);
    }
    if high & w.sign != 0 {
        division.remainder = negate(division.remainder);
    }
    if f1 {
        division.quotient = negate(division.quotient);
    }
    division.negations = negations;
    *flags &= !(CF | OF);
    Ok(division)
}

/// The 8086's multiplication microcode (CORX, with PREIMUL, NEGATE and the
/// carry/overflow tails), returning `(high, low)` with FLAGS exact. A REP
/// prefix presets F1, so it negates the product.
///
/// After K. Shirriff, "Reverse-engineering the multiplication algorithm in
/// the Intel 8086 processor" (2023); checked against hardware-captured
/// vectors.
pub fn mul_8086(
    flags: &mut u16,
    multiplicand: u16,
    multiplier: u16,
    w: Width,
    signed: bool,
    rep: bool,
) -> (u16, u16) {
    let negate = |v: u16| v.wrapping_neg() & w.mask;
    let (mut c, mut b) = (multiplicand & w.mask, multiplier & w.mask);
    let mut f1 = rep;
    let mut cf = *flags & CF != 0;
    if signed {
        if c & w.sign != 0 {
            c = negate(c);
            f1 = !f1;
        }
        cf = b & w.sign != 0;
        if cf {
            b = negate(b);
            f1 = !f1;
        }
    }
    let rotate = |v: &mut u16, cf: &mut bool| {
        let out = *v & 1 != 0;
        *v = (*v >> 1) | if *cf { w.sign } else { 0 };
        *cf = out;
    };
    let mut a = 0u16;
    rotate(&mut c, &mut cf);
    for _ in 0..w.bits {
        if cf {
            a = add(flags, a, b, false, w);
            cf = *flags & CF != 0;
        }
        rotate(&mut a, &mut cf);
        rotate(&mut c, &mut cf);
    }
    if f1 {
        c = neg(flags, c, w);
        a = if *flags & CF != 0 {
            !a & w.mask
        } else {
            negate(a)
        };
    }
    let carry_in = signed && c & w.sign != 0;
    add(flags, a, 0, carry_in, w);
    set(flags, CF | OF, *flags & ZF == 0);
    (a, c)
}

/// SZP after a value passes through the ALU unchanged, the way AAM writes
/// its remainder: CF, OF and AF clear.
pub fn pass(flags: &mut u16, value: u16, w: Width) {
    logic(flags, value, w);
}

#[cfg(test)]
#[path = "alu_tests.rs"]
mod tests;
