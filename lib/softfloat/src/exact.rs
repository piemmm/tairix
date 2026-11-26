//! Exact results: each operation's result held exactly over its operands'
//! significands, then rounded once.
//!
//! An [`Exact`] keeps 63 bits below a 64-bit significand and a sticky bit for
//! anything further down, which is enough to round any sum, product, quotient
//! or root correctly at every precision. The operations take unnormal
//! operands as the 8087 and the FPA hold them — an integer bit clear above
//! the least exponent — and keep the 8087's rule for what an unnormal result
//! is; a caller wanting IEEE 754's normal operands normalizes them first.

use core::cmp::Ordering;

use crate::extended::{Extended, BIAS, INTEGER};
use crate::mode::{Mode, Rounding};

/// The integer bit's place in an [`Exact`] significand.
pub const POINT: u32 = 126;

/// A finite value's parts: `sig × 2^(exp − 63)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Parts {
    /// The sign.
    pub negative: bool,
    /// The exponent of the significand's integer bit, unbiased.
    pub exp: i32,
    /// The significand, its integer bit the top one.
    pub sig: u64,
}

impl Parts {
    /// The parts of a finite double-extended value, a denormal read as the
    /// unnormal it equals.
    #[must_use]
    pub fn of(x: Extended) -> Self {
        let field = i32::from(x.exponent()).max(1);
        Self {
            negative: x.negative(),
            exp: field - BIAS,
            sig: x.sig,
        }
    }

    /// Whether the integer bit is set.
    #[must_use]
    pub const fn normal(self) -> bool {
        self.sig & INTEGER != 0
    }

    /// Whether the value is zero, as a pseudo-zero's is.
    #[must_use]
    pub const fn is_zero(self) -> bool {
        self.sig == 0
    }

    /// These parts with the significand's leading zeros shifted out, the
    /// exponent lowered to match; zero stays as it is.
    #[must_use]
    pub fn normalized(self) -> Self {
        if self.sig == 0 || self.normal() {
            return self;
        }
        let zeros = self.sig.leading_zeros();
        Self {
            exp: self.exp - exponent_of(zeros),
            sig: self.sig << zeros,
            ..self
        }
    }

    /// The magnitudes compared, each normalized first.
    #[must_use]
    pub fn cmp_magnitude(self, other: Self) -> Ordering {
        let (a, b) = (self.normalized(), other.normalized());
        match (a.is_zero(), b.is_zero()) {
            (true, true) => Ordering::Equal,
            (true, false) => Ordering::Less,
            (false, true) => Ordering::Greater,
            (false, false) => a.exp.cmp(&b.exp).then(a.sig.cmp(&b.sig)),
        }
    }

    /// The values compared, zeros of either sign equal.
    #[must_use]
    pub fn cmp_value(self, other: Self) -> Ordering {
        if self.is_zero() && other.is_zero() {
            return Ordering::Equal;
        }
        match (self.negative, other.negative) {
            (false, true) => Ordering::Greater,
            (true, false) => Ordering::Less,
            (false, false) => self.cmp_magnitude(other),
            (true, true) => other.cmp_magnitude(self),
        }
    }

    /// Whether this nonzero value is an integer, and if it is, whether it is
    /// odd: every value of 2^64 or more is an even one.
    #[must_use]
    pub fn parity(self) -> Option<bool> {
        let p = self.normalized();
        if p.exp >= 64 {
            return Some(false);
        }
        let fraction_bits = u32::try_from(63 - p.exp).ok().filter(|&bits| bits < 64)?;
        let fraction = p.sig & ((1 << fraction_bits) - 1);
        (fraction == 0).then_some(p.sig >> fraction_bits & 1 != 0)
    }
}

/// A count of places as an exponent step: no count here reaches `i32`'s
/// range.
fn exponent_of(places: u32) -> i32 {
    i32::try_from(places).unwrap_or(i32::MAX)
}

/// An exact result before rounding: `sig × 2^(exp − 126)`, with more bits
/// below `sig`'s last one where `sticky`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Exact {
    /// The sign.
    pub negative: bool,
    /// The exponent of bit 126, the integer bit's place.
    pub exp: i32,
    /// The significand, bit 127 free for a carry.
    pub sig: u128,
    /// Whether anything nonzero lies below `sig`.
    pub sticky: bool,
}

/// A significand rounded at its integer bit's fixed place.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RoundedSig {
    /// The 64-bit significand.
    pub sig: u64,
    /// Whether rounding carried out of the integer bit, the exponent then one
    /// higher.
    pub carried: bool,
    /// Whether the result differs from the exact value.
    pub inexact: bool,
}

impl Exact {
    /// The exact value of `p`.
    #[must_use]
    pub fn of(p: Parts) -> Self {
        Self {
            negative: p.negative,
            exp: p.exp,
            sig: u128::from(p.sig) << (POINT - 63),
            sticky: false,
        }
    }

    /// Whether the value is exactly zero.
    #[must_use]
    pub const fn is_zero(self) -> bool {
        self.sig == 0 && !self.sticky
    }

    /// Shifted right `n` places, what falls off kept as sticky.
    #[must_use]
    pub fn shifted_right(self, n: u32) -> Self {
        if n == 0 {
            return self;
        }
        let (sig, lost) = if n >= 128 {
            (0, self.sig != 0)
        } else {
            (self.sig >> n, self.sig & ((1 << n) - 1) != 0)
        };
        Self {
            exp: self.exp.saturating_add(exponent_of(n)),
            sig,
            sticky: self.sticky || lost,
            ..self
        }
    }

    /// A carry out of the integer bit's place brought down; with `full`,
    /// leading zeros shifted up too.
    #[must_use]
    pub fn normalized(self, full: bool) -> Self {
        let mut x = self;
        if x.sig >> (POINT + 1) != 0 {
            x = x.shifted_right(1);
        }
        if full && x.sig != 0 && x.sig >> POINT == 0 {
            let zeros = x.sig.leading_zeros().saturating_sub(1);
            x.sig <<= zeros;
            x.exp = x.exp.saturating_sub(exponent_of(zeros));
        }
        x
    }

    /// The 64-bit significand, chopped.
    #[must_use]
    pub const fn chopped(self) -> u64 {
        low_word(self.sig >> (POINT - 63))
    }

    /// The significand rounded to `rounding`'s bits at the integer bit's
    /// fixed place.
    #[must_use]
    pub fn rounded(self, rounding: Rounding) -> RoundedSig {
        let lsb = POINT + 1 - rounding.bits();
        let below = self.sig & ((1 << lsb) - 1);
        let half = 1 << (lsb - 1);
        let inexact = below != 0 || self.sticky;
        let up = match rounding.mode() {
            Mode::Nearest => {
                below > half || (below == half && (self.sticky || (self.sig >> lsb) & 1 != 0))
            }
            Mode::NearestAway => below >= half,
            mode => inexact && mode.away(self.negative),
        };
        let mut kept = self.sig & !((1 << lsb) - 1);
        if up {
            kept += 1 << lsb;
        }
        let carried = kept >> (POINT + 1) != 0;
        if carried {
            kept >>= 1;
        }
        RoundedSig {
            sig: Self { sig: kept, ..self }.chopped(),
            carried,
            inexact,
        }
    }
}

/// A remainder, and the quotient it was taken with.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Remainder {
    /// The remainder, exact.
    pub rest: Parts,
    /// The quotient's magnitude, the whole number nearest `a ÷ b`, ties to
    /// the even one: its low 64 bits.
    pub quotient: u64,
}

/// The remainder of `a` by `b`, both normalized and nonzero, as IEEE 754
/// defines it: exact, its magnitude at most half of `b`'s.
#[must_use]
pub fn remainder(a: Parts, b: Parts) -> Remainder {
    let gap = a.exp - b.exp;
    if gap < -1 {
        return Remainder {
            rest: a,
            quotient: 0,
        };
    }
    let divisor = u128::from(b.sig);
    // `rest` stays below `divisor`; each step brings down its quotient bits.
    let (mut rest, mut quotient) = if gap == -1 {
        (u128::from(a.sig), 0)
    } else {
        (
            u128::from(a.sig) % divisor,
            low_word(u128::from(a.sig) / divisor),
        )
    };
    let mut left = gap.max(0).cast_unsigned();
    while left > 0 {
        let step = left.min(63);
        let shifted = rest << step;
        quotient = quotient << step | low_word(shifted / divisor);
        rest = shifted % divisor;
        left -= step;
    }
    let odd = quotient & 1 != 0;
    // `rest` is at `b`'s scale, or at half of it where `a` is the smaller,
    // so half of `b` is the divisor once or twice `rest`'s units.
    let (scale, twice) = if gap == -1 {
        (b.exp - 1, rest)
    } else {
        (b.exp, rest << 1)
    };
    let flip = twice > divisor || (twice == divisor && odd);
    let magnitude = match (flip, gap == -1) {
        (false, _) => rest,
        (true, true) => (divisor << 1) - rest,
        (true, false) => divisor - rest,
    };
    let rest = Parts {
        negative: a.negative != flip,
        exp: scale,
        sig: u64::try_from(magnitude).unwrap_or(u64::MAX),
    }
    .normalized();
    Remainder {
        rest,
        quotient: quotient.wrapping_add(u64::from(flip)),
    }
}

/// The low 64 bits of `x`.
pub(crate) const fn low_word(x: u128) -> u64 {
    let bytes = x.to_le_bytes();
    u64::from_le_bytes([
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
    ])
}

/// `a + b`, or `a − b` where `subtract`, exactly. The result is normalized as
/// the operand of larger magnitude is, as the 8087's unnormal arithmetic
/// leaves it; an exactly zero result is the caller's to sign.
#[must_use]
pub fn add(a: Parts, b: Parts, subtract: bool) -> Exact {
    let b = Parts {
        negative: b.negative != subtract,
        ..b
    };
    let (big, small) = if a.cmp_magnitude(b) == Ordering::Less {
        (b, a)
    } else {
        (a, b)
    };
    let full = big.normal();
    let (big, small) = if full {
        (big.normalized(), small.normalized())
    } else {
        (big, small)
    };
    // Aligned at the larger exponent, which an unnormal of the larger
    // magnitude need not have when the other is unnormal too; a pseudo-zero
    // adds nothing and aligns nothing.
    let exp = if small.is_zero() {
        big.exp
    } else {
        big.exp.max(small.exp)
    };
    let aligned = |p: Parts| {
        let gap = u32::try_from(exp - p.exp).unwrap_or(u32::MAX);
        Exact::of(p).shifted_right(gap)
    };
    let (x, y) = (aligned(big), aligned(small));
    // At most one side lost bits, the one shifted; they stay below the sum.
    let (sig, sticky) = if x.negative == y.negative {
        (x.sig + y.sig, x.sticky || y.sticky)
    } else if y.sticky {
        // Below `y`'s last bit lies a little more of it to take away.
        (x.sig - y.sig - 1, true)
    } else {
        (x.sig - y.sig, x.sticky)
    };
    Exact {
        negative: big.negative,
        exp,
        sig,
        sticky,
    }
    .normalized(full)
}

/// `a × b` exactly: normalized where both are, unnormal where either is.
#[must_use]
pub fn multiply(a: Parts, b: Parts) -> Exact {
    Exact {
        negative: a.negative != b.negative,
        exp: a.exp + b.exp,
        sig: u128::from(a.sig) * u128::from(b.sig),
        sticky: false,
    }
    .normalized(false)
}

/// `a × b + c`, each normalized and nonzero, exactly enough to round
/// correctly at any precision: none where they cancel to exactly zero.
#[must_use]
pub fn fused(a: Parts, b: Parts, c: Parts) -> Option<Exact> {
    let (product_negative, product_exp) = (a.negative != b.negative, a.exp + b.exp + 1);
    // Each at the top of a 256-bit window: bit 254 the place whose exponent
    // each names, bit 255 free for a carry.
    let product = Wide::of(u128::from(a.sig) * u128::from(b.sig)).shl(127);
    let addend = Wide::of(u128::from(c.sig)).shl(191);
    let ((big_negative, big_bits), (small_negative, small_bits, gap), exp) = if product_exp >= c.exp
    {
        (
            (product_negative, product),
            (c.negative, addend, product_exp - c.exp),
            product_exp,
        )
    } else {
        (
            (c.negative, addend),
            (product_negative, product, c.exp - product_exp),
            c.exp,
        )
    };
    let (small_bits, sticky) = small_bits.shr_sticky(u32::try_from(gap).unwrap_or(u32::MAX));
    let (negative, bits, sticky) = if big_negative == small_negative {
        (big_negative, big_bits.add(small_bits), sticky)
    } else {
        match big_bits.cmp(small_bits) {
            Ordering::Greater if sticky => (big_negative, big_bits.sub(small_bits).dec(), true),
            Ordering::Greater => (big_negative, big_bits.sub(small_bits), false),
            Ordering::Less => (small_negative, small_bits.sub(big_bits), sticky),
            Ordering::Equal if sticky => (small_negative, Wide::ZERO, true),
            Ordering::Equal => return None,
        }
    };
    Some(bits.exact(negative, exp, sticky))
}

/// `a ÷ b` exactly, `b` normal: unnormal where `a` is. None where `b`'s
/// significand is zero.
#[must_use]
pub fn divide(a: Parts, b: Parts) -> Option<Exact> {
    if b.sig == 0 {
        return None;
    }
    // sig × 2^127 / b.sig, two 64-bit steps of the long division.
    let divisor = u128::from(b.sig);
    let first = u128::from(a.sig) << 63;
    let (high, rest) = (first / divisor, first % divisor);
    let second = rest << 64;
    let (low, rest) = (second / divisor, second % divisor);
    Some(
        Exact {
            negative: a.negative != b.negative,
            exp: a.exp - b.exp - 1,
            sig: (high << 64) | low,
            sticky: rest != 0,
        }
        .normalized(false),
    )
}

/// The square root of `a`, positive and normal, exactly.
#[must_use]
pub fn square_root(a: Parts) -> Exact {
    // a = m × 2^e with an even e; sqrt(m) taken to 34 places below the point,
    // as many bits as rounding needs and two more.
    let odd = (a.exp - 63) & 1 != 0;
    let (m, e) = if odd {
        (u128::from(a.sig) << 1, a.exp - 64)
    } else {
        (u128::from(a.sig), a.exp - 63)
    };
    let (root, rest) = root_scaled(m);
    Exact {
        negative: false,
        exp: e / 2 - 34 + 126,
        sig: root,
        sticky: rest,
    }
    .normalized(true)
}

/// `⌊√(m × 4^34)⌋` for `m < 2^66`, and whether a remainder was left.
fn root_scaled(m: u128) -> (u128, bool) {
    // The radicand is 64 × a, a = m × 4^31 within 128 bits: with √a = r + f,
    // the root is 8r + d for the largest d below 8 that leaves no deficit.
    let a = m << 62;
    let r = a.isqrt();
    let room = (a - r * r) << 6;
    let taken = |d: u128| 16 * r * d + d * d;
    let d = (0..8).rev().find(|&d| taken(d) <= room).unwrap_or(0);
    (8 * r + d, room != taken(d))
}

/// `p`, normalized, rounded to an integer in `mode`: its magnitude, none past
/// `u64`'s; and whether it was inexact.
#[must_use]
pub fn to_integer(p: Parts, mode: Mode) -> (Option<u64>, bool) {
    if p.is_zero() {
        return (Some(0), false);
    }
    if p.exp >= 64 {
        return (None, false);
    }
    if p.exp >= 63 {
        return (Some(p.sig), false);
    }
    let shift = u32::try_from(63 - p.exp).unwrap_or(u32::MAX);
    let (whole, below, half, sticky) = if shift >= 65 {
        (0, 0, 1, true)
    } else if shift == 64 {
        (0, p.sig, INTEGER, false)
    } else {
        let below = p.sig & ((1 << shift) - 1);
        (p.sig >> shift, below, 1 << (shift - 1), false)
    };
    let inexact = below != 0 || sticky;
    let up = match mode {
        Mode::Nearest => !sticky && (below > half || (below == half && whole & 1 != 0)),
        Mode::NearestAway => !sticky && below >= half,
        mode => inexact && mode.away(p.negative),
    };
    (whole.checked_add(u64::from(up)), inexact)
}

/// A 256-bit unsigned integer, high half first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Wide {
    high: u128,
    low: u128,
}

impl Wide {
    const ZERO: Self = Self { high: 0, low: 0 };

    const fn of(low: u128) -> Self {
        Self { high: 0, low }
    }

    fn shl(self, n: u32) -> Self {
        match n {
            0 => self,
            1..=127 => Self {
                high: self.high << n | self.low >> (128 - n),
                low: self.low << n,
            },
            128..=255 => Self {
                high: self.low << (n - 128),
                low: 0,
            },
            _ => Self::ZERO,
        }
    }

    /// Shifted right `n` places, and whether anything nonzero fell off.
    fn shr_sticky(self, n: u32) -> (Self, bool) {
        match n {
            0 => (self, false),
            1..=127 => (
                Self {
                    high: self.high >> n,
                    low: self.low >> n | self.high << (128 - n),
                },
                self.low & ((1 << n) - 1) != 0,
            ),
            128 => (Self::of(self.high), self.low != 0),
            129..=255 => (
                Self::of(self.high >> (n - 128)),
                self.low != 0 || self.high & ((1 << (n - 128)) - 1) != 0,
            ),
            _ => (Self::ZERO, self != Self::ZERO),
        }
    }

    fn add(self, other: Self) -> Self {
        let (low, carry) = self.low.overflowing_add(other.low);
        Self {
            high: self.high + other.high + u128::from(carry),
            low,
        }
    }

    /// `self − other`, `other` no larger.
    fn sub(self, other: Self) -> Self {
        let (low, borrow) = self.low.overflowing_sub(other.low);
        Self {
            high: self.high - other.high - u128::from(borrow),
            low,
        }
    }

    /// One less, from a nonzero value.
    fn dec(self) -> Self {
        self.sub(Self::of(1))
    }

    fn cmp(self, other: Self) -> Ordering {
        self.high.cmp(&other.high).then(self.low.cmp(&other.low))
    }

    fn leading_zeros(self) -> u32 {
        if self.high == 0 {
            128 + self.low.leading_zeros()
        } else {
            self.high.leading_zeros()
        }
    }

    /// The value `self × 2^(exp − 254)`, `sticky` below it, as an exact
    /// result, normalized.
    fn exact(self, negative: bool, exp: i32, sticky: bool) -> Exact {
        if self == Self::ZERO {
            // Only a sticky remnant: below every bit the window holds.
            return Exact {
                negative,
                exp: exp - 256,
                sig: 0,
                sticky,
            };
        }
        // Brought to bit 254, the window's integer place; a carry into 255
        // is brought down by the exact result's own normalizing.
        let zeros = self.leading_zeros();
        let (bits, sticky, exp) = if zeros == 0 {
            let (bits, lost) = self.shr_sticky(1);
            (bits, sticky || lost, exp + 1)
        } else {
            let up = zeros - 1;
            (self.shl(up), sticky, exp - i32::try_from(up).unwrap_or(0))
        };
        // Bits 254..128 are the result's 127 bits with bit 126 its integer
        // place.
        Exact {
            negative,
            exp,
            sig: bits.high,
            sticky: sticky || bits.low != 0,
        }
        .normalized(false)
    }
}

const _: () = assert!(POINT == 126);

#[cfg(test)]
#[path = "exact_tests.rs"]
mod tests;
