//! Packed decimal loads and stores, as FPEmulator converts them.
//!
//! Neither direction is exact. A store scales the value into one decimal
//! place by a power of ten its own FPA computes, then multiplies the
//! remainder by ten for each digit: it writes seventeen significant digits
//! (twenty-one expanded), the rest zero, and breaks a tie toward zero. A load
//! reads the digits as an integer, nibbles above nine taken at face value,
//! and scales it the same way. The expanded format, four words, is chosen by
//! the FPSR's EP bit.

use super::arith::Unit;
use super::internal::{denormalise, Direction, Internal, Rounded, BIAS, SIGN, UNCOMMON, UNITS};
use super::machine::{loaded, Machine, Src};
use super::trans::power_of_ten;
use super::{fpsr, Exception, Precision, Rounding};
use crate::word::{high, low, low64};

/// The exponents of `2^∓12288`, by which a scaling is kept in range.
const BIAS_DOWN: u32 = BIAS.cast_unsigned() - 0x3000;
const BIAS_UP: u32 = BIAS.cast_unsigned() + 0x3000;

/// `log10 2` rounded down to a multiple of `2^−18`: one more is `log10 2`
/// rounded up.
const LOG10_2: i32 = 0x1_3441;

impl Machine {
    /// `MakePowerOfTenInF1`: `10^n` in F1, from the exact table by squaring
    /// and multiplying by ten, each step rounded to nearest.
    fn power_of_ten(&mut self, n: u32) {
        let mut shift = 0;
        while n >> shift >= 28 {
            shift += 1;
        }
        self.f[1] = power_of_ten(n >> shift).unwrap_or(Internal::ZERO);
        while shift >= 1 {
            self.mul(1, Src::F(1), Src::F(1));
            if n >> (shift - 1) & 1 != 0 {
                self.mul(1, Src::F(1), super::machine::constant(7));
            }
            shift -= 1;
        }
    }

    /// `10^n` in F1, from the table where it holds it.
    fn ten_to(&mut self, n: u32) {
        match power_of_ten(n) {
            Some(power) => self.f[1] = power,
            None => self.power_of_ten(n),
        }
    }
}

/// `LDFConvert_Packed_Digits`: `count` nibbles from the top of `word` added
/// to `total` as decimal digits, each taken at its face value.
fn digits(total: u128, word: u32, count: u32) -> u128 {
    let mut total = total;
    let mut word = word;
    for _ in 0..count {
        total = total.wrapping_mul(10).wrapping_add(u128::from(word >> 28)) & ((1 << 96) - 1);
        word <<= 4;
    }
    total
}

impl Unit {
    /// `LDFP`.
    pub fn load_packed(&mut self, words_: [u32; 4]) -> Result<Internal, Exception> {
        let expanded = self.fpsr & fpsr::EP != 0;
        let [head, w1, w2, w3] = words_;
        let exponent = digits(0, head << 4, if expanded { 7 } else { 4 });
        let (first, count, rest) = if expanded {
            (w1, 8, [w2, w3])
        } else {
            (head << 20, 3, [w1, w2])
        };
        let marked = first & SIGN != 0;
        let mut total = digits(0, first, count);
        for word in rest {
            total = digits(total, word, 8);
        }
        let limit = if expanded { 0xFE_0000 } else { 0x4100 };
        if exponent >= limit {
            return Ok(nan_or_infinity(head, total, marked, expanded));
        }
        if total == 0 {
            return Ok(Internal::zero(head & SIGN != 0));
        }
        let mut power = i32::try_from(exponent).unwrap_or(i32::MAX);
        if head & 0x4000_0000 != 0 {
            power = -power;
        }
        power -= if expanded { 23 } else { 18 };
        // The digits as a mantissa over the 32 bits below it, normalised.
        let (mut mantissa, mut below, mut biased) = if expanded {
            (
                low64(total >> 16),
                (low(low64(total)) & 0xFFFF) << 16,
                BIAS + 1 + 78,
            )
        } else {
            (low64(total), 0, BIAS + 1 + 62)
        };
        while mantissa & UNITS == 0 {
            mantissa = mantissa << 1 | u64::from(below >> 31);
            below <<= 1;
            biased -= 1;
        }
        let inexact = below != 0;
        let up = below > 0x8000_0000 || (below == 0x8000_0000 && mantissa & 1 != 0);
        // A tie leaves FPEmulator's working register holding the complement
        // of the low bit; a massive underflow shifts by what it holds.
        let leftover = if below == 0x8000_0000 {
            !(low(mantissa) << 31)
        } else {
            below
        };
        if up {
            mantissa = if let Some(sum) = mantissa.checked_add(1) {
                sum
            } else {
                biased += 1;
                UNITS
            };
        }
        let negative = head & SIGN != 0;
        let mut m = Machine::new(self.fpsr);
        m.f[0] = loaded((head & SIGN) | biased.cast_unsigned(), mantissa);
        if inexact {
            m.fpsr |= Exception::Inexact.flag();
        }
        let magnitude = power.unsigned_abs();
        if power > 0 {
            if magnitude > 4096 {
                return self.packed_out_of_range(&mut m, magnitude, negative, true, leftover);
            }
            m.ten_to(magnitude);
            m.mul(0, Src::F(0), Src::F(1));
        } else if power < 0 {
            if magnitude > 4096 {
                return self.packed_out_of_range(&mut m, magnitude, negative, false, leftover);
            }
            m.ten_to(magnitude);
            m.div(0, Src::F(0), Src::F(1));
        }
        self.packed_loaded(&m)
    }

    /// A loaded value in F0, inexact as the nested FPSR says.
    fn packed_loaded(&mut self, m: &Machine) -> Result<Internal, Exception> {
        if m.flags() & Exception::Inexact.flag() != 0 {
            self.raise(Exception::Inexact)?;
        }
        Ok(m.f[0])
    }

    /// A load whose power of ten passes 4096 either way: the value scaled by
    /// halves or quarters of it, held in range by `2^∓12288`, and taken out
    /// of range by the round stage where it stays there.
    fn packed_out_of_range(
        &mut self,
        m: &mut Machine,
        n: u32,
        negative: bool,
        up: bool,
        leftover: u32,
    ) -> Result<Internal, Exception> {
        let step = if up {
            super::FpaDyadicOp::Muf
        } else {
            super::FpaDyadicOp::Dvf
        };
        let (into, back) = if up {
            (BIAS_DOWN, BIAS_UP)
        } else {
            (BIAS_UP, BIAS_DOWN)
        };
        let range = if up {
            Exception::Overflow
        } else {
            Exception::Underflow
        };
        if n >= 16384 {
            return self.packed_massive(negative, up, leftover);
        }
        let scale = loaded(into, UNITS);
        if n <= 8192 {
            m.power_of_ten(n >> 1);
            m.f[2] = scale;
            m.mul(0, Src::F(0), Src::F(2));
            m.dyadic(step, Rounding::Nearest, 0, Src::F(0), Src::F(1));
            if n & 1 != 0 {
                m.mul(1, Src::F(1), super::machine::constant(7));
            }
            m.dyadic(step, Rounding::Nearest, 1, Src::F(0), Src::F(1));
            m.f[0] = loaded(back, UNITS);
            m.mul(0, Src::F(1), Src::F(0));
            if m.flags() & range.flag() == 0 {
                return self.packed_loaded(m);
            }
            m.mul(0, Src::F(1), Src::F(2));
        } else {
            m.power_of_ten(n >> 2);
            m.f[2] = scale;
            m.mul(0, Src::F(0), Src::F(2));
            m.dyadic(step, Rounding::Nearest, 0, Src::F(0), Src::F(1));
            m.dyadic(step, Rounding::Nearest, 0, Src::F(0), Src::F(1));
            m.mul(0, Src::F(0), Src::F(2));
            if n & 2 != 0 {
                m.mul(1, Src::F(1), super::machine::constant(7));
            }
            m.dyadic(step, Rounding::Nearest, 0, Src::F(0), Src::F(1));
            if n & 1 != 0 {
                m.mul(1, Src::F(1), super::machine::constant(7));
            }
            m.dyadic(step, Rounding::Nearest, 0, Src::F(0), Src::F(1));
            if m.flags() & range.flag() != 0 {
                return self.packed_massive(m.f[0].negative(), up, leftover);
            }
        }
        let value = m.f[0];
        let offset = if up { 0x6000 } else { -0x6000 };
        let r = Rounded {
            negative: value.negative(),
            exponent: value.exponent().cast_signed() + offset,
            mantissa: value.mantissa,
            direction: if m.flags() & Exception::Inexact.flag() != 0 {
                Direction::Down
            } else {
                Direction::Exact
            },
        };
        self.packed_finish(r)
    }

    /// A load far past the exponent range. Above it, an overflow; below, the
    /// underflow FPEmulator calls without setting the shift it reads, so its
    /// working register's leftover is the denormalising shift and the
    /// exponent's offset.
    fn packed_massive(
        &mut self,
        negative: bool,
        up: bool,
        leftover: u32,
    ) -> Result<Internal, Exception> {
        if up {
            return self.packed_finish(Rounded {
                negative,
                exponent: 0x100_0000,
                mantissa: UNITS,
                direction: Direction::Down,
            });
        }
        if self.fpsr & Exception::Underflow.enable() != 0 {
            return Err(Exception::Underflow);
        }
        let r = Rounded {
            negative,
            exponent: 0,
            mantissa: UNITS,
            direction: Direction::Down,
        };
        let (mut mantissa, second) = denormalise(r, leftover, 64, Rounding::Nearest);
        let mut lost = second != Direction::Exact;
        if self.fpsr & fpsr::ND != 0 && mantissa != 0 && mantissa != UNITS {
            mantissa = 0;
            lost = true;
        }
        if lost {
            self.fpsr |= Exception::Underflow.flag();
        }
        let exponent = 0xFF00_0000u32.wrapping_add(leftover);
        let sign = if negative { SIGN } else { 0 };
        let value = if mantissa == 0 {
            Internal::zero(negative)
        } else if mantissa & UNITS != 0 {
            Internal {
                head: sign | exponent,
                mantissa,
            }
        } else {
            Internal {
                head: sign | UNCOMMON | exponent.wrapping_sub(1),
                mantissa: mantissa | UNITS,
            }
        };
        self.raise(Exception::Inexact)?;
        Ok(value)
    }

    fn packed_finish(&mut self, r: Rounded) -> Result<Internal, Exception> {
        self.precision = Precision::Extended;
        self.rounding = Rounding::Nearest;
        self.finish_rounded(r)
    }

    /// `STFP`.
    pub fn store_packed(&mut self, a: Internal) -> Result<[u32; 4], Exception> {
        let expanded = self.fpsr & fpsr::EP != 0;
        if a.special() {
            return Ok(stored_nan_or_infinity(a, expanded));
        }
        if a.is_zero() {
            return Ok([a.head & SIGN, 0, 0, 0]);
        }
        // The exponent the value is scaled from, and how far the register's
        // own exponent field stands from it.
        let field = a.exponent().cast_signed();
        let (exponent, mantissa, offset) = if a.uncommon() {
            let x = a.number();
            (x.exponent, x.mantissa, field - x.exponent)
        } else {
            (field, a.mantissa, 0)
        };
        let head = a.head & !UNCOMMON;
        let binary = exponent - (BIAS - 1);
        // FPEmulator picks the bound by an unsigned comparison, so a negative
        // exponent takes the upper one.
        let log = if exponent.cast_unsigned() > (BIAS - 1).cast_unsigned() {
            LOG10_2 + 1
        } else {
            LOG10_2
        };
        let mut decimal = binary.wrapping_mul(log) >> 18;
        let (value, scaling_inexact) = if decimal == 0 {
            (Internal { head, mantissa }, false)
        } else {
            let mut m = Machine::new(self.fpsr);
            m.f[0] = loaded(head, mantissa);
            let n = decimal.unsigned_abs();
            let step = if decimal >= 0 {
                super::FpaDyadicOp::Dvf
            } else {
                super::FpaDyadicOp::Muf
            };
            if n < 28 {
                m.ten_to(n);
            } else {
                m.power_of_ten(n >> 1);
                m.dyadic(step, Rounding::Nearest, 0, Src::F(0), Src::F(1));
                if n & 1 != 0 {
                    m.mul(1, Src::F(1), super::machine::constant(7));
                }
            }
            m.dyadic(step, Rounding::Nearest, 0, Src::F(0), Src::F(1));
            (m.f[0], m.flags() & Exception::Inexact.flag() != 0)
        };
        let scaled = value.exponent().cast_signed() - offset;
        // Four bits above the binary point hold the first digit.
        let shift = u32::try_from(BIAS + 3 - scaled).unwrap_or(0).min(32);
        let wide = u128::from(value.mantissa) << 32;
        let mut fraction = wide >> shift;
        let mut out: u128 = 0;
        let count = if expanded { 21 } else { 17 };
        if fraction >> 92 == 0 {
            decimal -= 1;
            fraction *= 10;
        }
        for produced in 1..=count {
            out = (out << 4 | (fraction >> 92)) & ((1 << 96) - 1);
            fraction &= (1 << 92) - 1;
            if produced != count {
                fraction *= 10;
            }
        }
        let rest = fraction;
        let inexact = scaling_inexact || rest != 0;
        let half = rest >> 91 & 1 != 0;
        let beyond = rest & ((1 << 91) - 1) != 0;
        if half && beyond {
            out = bcd_increment(out);
            let top = if expanded { 1 << 84 } else { 1 << 68 };
            if out & top != 0 {
                out >>= 4;
                decimal += 1;
            }
        }
        let words_ = layout(head & SIGN, decimal, out, expanded);
        if inexact {
            self.raise(Exception::Inexact)?;
        }
        Ok(words_)
    }
}

/// A packed number's digits and decimal exponent as memory holds them, the
/// digits' last places zero.
fn layout(sign: u32, decimal: i32, out: u128, expanded: bool) -> [u32; 4] {
    let mut first = sign;
    if decimal < 0 {
        first |= 0x4000_0000;
    }
    let exponent = bcd(decimal.unsigned_abs());
    let [high, middle, low] = words96(out);
    if expanded {
        [
            first | exponent,
            high << 12 | middle >> 20,
            middle << 12 | low >> 20,
            low << 12,
        ]
    } else {
        [
            first | exponent << 12 | high << 8 | middle >> 24,
            middle << 8 | low >> 24,
            low << 8,
            0,
        ]
    }
}

/// A binary exponent below 8000 as four BCD digits.
fn bcd(n: u32) -> u32 {
    let mut n = n;
    let mut out = 0;
    for (place, value) in [
        (0x4000, 4000),
        (0x2000, 2000),
        (0x1000, 1000),
        (0x800, 800),
        (0x400, 400),
        (0x200, 200),
        (0x100, 100),
        (0x80, 80),
        (0x40, 40),
        (0x20, 20),
    ] {
        if n >= value {
            n -= value;
            out |= place;
        }
    }
    if n >= 10 {
        n += 0x10 - 10;
    }
    out + n
}

/// A BCD number plus one, the carry passing through nines.
fn bcd_increment(out: u128) -> u128 {
    let mut out = out;
    let mut place = 0;
    loop {
        let digit = (out >> place) & 0xF;
        if digit < 9 {
            return out + (1 << place);
        }
        out &= !(0xF << place);
        place += 4;
        if place >= 96 {
            return out;
        }
    }
}

/// `LDFP` of an infinity or a NaN: the digits as a binary fraction.
fn nan_or_infinity(head: u32, total: u128, marked: bool, expanded: bool) -> Internal {
    let mut value = low64(total);
    if marked {
        let bias: u64 = if expanded {
            0x16EF_0A57_B400_0000
        } else {
            0x6F05_B59D_3B20_0000
        };
        value = value.wrapping_sub(bias);
    }
    let (upper, lower) = (high(value), low(value));
    let middle = (lower >> 22 | upper << 10) & !0xE000_0000;
    let mantissa_low = (upper & !0xC000_0000) >> 19 | middle << 11;
    let mut mantissa_high = (lower << 8) & !0xC000_0000 | middle >> 21;
    if marked {
        mantissa_high |= 0x4000_0000;
    }
    Internal {
        head: (head & SIGN) | UNCOMMON | 0x7FFF,
        mantissa: u64::from(mantissa_high) << 32 | u64::from(mantissa_low),
    }
}

/// `STFP` of an infinity or a NaN: its fraction as decimal digits, the
/// exponent all nines' worth of `F`, a NaN marked in the first digit's top
/// bit. A signalling NaN signals nothing: FPEmulator's test for one reads the
/// uncommon bit.
fn stored_nan_or_infinity(a: Internal, expanded: bool) -> [u32; 4] {
    let (high_word, low_word) = (high(a.mantissa), low(a.mantissa));
    let middle = (low_word >> 11 | high_word << 21) & !0xE000_0000;
    let lower = (high_word & !0xC000_0000) >> 8 | middle << 22;
    let upper = (low_word << 19) & !0xC000_0000 | middle >> 10;
    let mut x = u64::from(upper) << 32 | u64::from(lower);
    let mut first = (a.head & SIGN) | 0x0FFF_F000;
    if a.mantissa != 0 {
        first |= 0x800;
    }
    // Digits enter at the top of a 96-bit register until the marker that
    // starts in the first digit's place leaves its bottom.
    let mut register: u128 = 0x800 << 64;
    loop {
        let quotient = divide_by_ten(x);
        let digit = u128::from(low(x).wrapping_sub(low(quotient).wrapping_mul(10)));
        let (quotient, digit) = if digit >= 10 {
            (quotient + 1, digit - 10)
        } else {
            (quotient, digit)
        };
        x = quotient;
        let leaving = register & 0x8 != 0;
        register = register >> 4 | digit << 92;
        if leaving {
            break;
        }
    }
    let [r_high, r_mid, r_low] = words96(register);
    if expanded {
        [
            first | 0xFFF,
            r_high >> 20 | first << 20,
            r_mid >> 20 | r_high << 12,
            r_low >> 20 | r_mid << 12,
        ]
    } else {
        [
            first | r_high >> 20,
            r_high << 12 | r_mid >> 20,
            r_mid << 12 | r_low >> 20,
            0,
        ]
    }
}

/// A 96-bit register's three words, the most significant first.
fn words96(x: u128) -> [u32; 3] {
    let bottom = low64(x);
    [low(low64(x >> 64)), high(bottom), low(bottom)]
}

/// FPEmulator's estimate of `x / 10`, one short at most.
fn divide_by_ten(x: u64) -> u64 {
    let mut q = x - (x >> 2);
    q += q >> 4;
    q += q >> 8;
    q += q >> 16;
    q += q >> 32;
    q >> 3
}
