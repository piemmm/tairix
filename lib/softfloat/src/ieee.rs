//! IEEE 754 arithmetic on binary32 and binary64 values: each result exact,
//! rounded once in the chosen mode, with the exceptions default handling
//! raises.
//!
//! Where the standard leaves a choice to the implementation, [`Policy`] makes
//! it: when an underflow is tiny, and whether a NaN result carries an
//! operand's payload. The operations are those ARM's floating-point units
//! perform; a NaN is chosen from the operands in their argument order.

use core::cmp::Ordering;

use crate::binary::{Binary, Unpacked};
use crate::exact::{self, Exact, Parts, POINT};
use crate::extended::INTEGER;
use crate::flags::Flags;
use crate::mode::{Mode, Rounding};

/// When a nonzero result is judged tiny for underflow.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Tininess {
    /// Its exact value lies strictly between ±2^min, as ARM judges it.
    BeforeRounding,
    /// Rounded as though the exponent were unbounded it still does, as x86
    /// judges it.
    AfterRounding,
}

/// What an operation with a NaN operand answers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Nans {
    /// The first signalling NaN operand, quieted, else the first quiet one.
    Propagate,
    /// The default NaN, whatever the operands held.
    Default,
}

/// The choices IEEE 754 leaves to an implementation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Policy {
    /// When an underflow is tiny.
    pub tininess: Tininess,
    /// What a NaN operand makes the result.
    pub nans: Nans,
}

/// A result and the exceptions computing it raised.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Outcome {
    /// The result's bits.
    pub bits: u64,
    /// The exceptions raised.
    pub flags: Flags,
}

impl Outcome {
    const fn exact(bits: u64) -> Self {
        Self {
            bits,
            flags: Flags::NONE,
        }
    }
}

/// A value rounded to an integer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Integral {
    /// The integer.
    Value {
        /// Its sign: a negative value rounding to zero keeps it.
        negative: bool,
        /// Its magnitude.
        magnitude: u64,
        /// Whether it differs from the value.
        inexact: bool,
    },
    /// The integer's magnitude is past `u64`'s, or the value is infinite.
    OutOfRange {
        /// The value's sign.
        negative: bool,
    },
    /// The value is a NaN.
    Nan,
}

/// IEEE 754 arithmetic in one format, rounding and policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Ieee {
    /// The format operands and results are in.
    pub format: Binary,
    /// How results are rounded.
    pub mode: Mode,
    /// The implementation's choices.
    pub policy: Policy,
}

impl Ieee {
    /// `a + b`.
    #[must_use]
    pub fn add(self, a: u64, b: u64) -> Outcome {
        self.sum(a, b, false)
    }

    /// `a − b`.
    #[must_use]
    pub fn sub(self, a: u64, b: u64) -> Outcome {
        self.sum(a, b, true)
    }

    fn sum(self, a: u64, b: u64, subtract: bool) -> Outcome {
        let format = self.format;
        match (format.unpack(a), format.unpack(b)) {
            (Unpacked::Nan { .. }, _) | (_, Unpacked::Nan { .. }) => self.nan(&[a, b]),
            (Unpacked::Infinity { negative: left }, Unpacked::Infinity { negative: right }) => {
                if left == (right != subtract) {
                    Outcome::exact(format.infinity(left))
                } else {
                    self.invalid()
                }
            }
            (Unpacked::Infinity { negative }, _) => Outcome::exact(format.infinity(negative)),
            (_, Unpacked::Infinity { negative }) => {
                Outcome::exact(format.infinity(negative != subtract))
            }
            (Unpacked::Zero { negative: left }, Unpacked::Zero { negative: right }) => {
                let right = right != subtract;
                Outcome::exact(format.zero(if left == right {
                    left
                } else {
                    self.zero_sign()
                }))
            }
            (Unpacked::Zero { .. }, Unpacked::Finite { parts, .. }) => {
                Outcome::exact(format.with_sign(b, parts.negative != subtract))
            }
            (Unpacked::Finite { .. }, Unpacked::Zero { .. }) => Outcome::exact(a),
            (Unpacked::Finite { parts: left, .. }, Unpacked::Finite { parts: right, .. }) => {
                let sum = exact::add(left.normalized(), right.normalized(), subtract);
                if sum.is_zero() {
                    return Outcome::exact(format.zero(self.zero_sign()));
                }
                self.round(sum)
            }
        }
    }

    /// `a × b`.
    #[must_use]
    pub fn mul(self, a: u64, b: u64) -> Outcome {
        let format = self.format;
        let negative = format.negative(a) != format.negative(b);
        match (format.unpack(a), format.unpack(b)) {
            (Unpacked::Nan { .. }, _) | (_, Unpacked::Nan { .. }) => self.nan(&[a, b]),
            (Unpacked::Infinity { .. }, Unpacked::Zero { .. })
            | (Unpacked::Zero { .. }, Unpacked::Infinity { .. }) => self.invalid(),
            (Unpacked::Infinity { .. }, _) | (_, Unpacked::Infinity { .. }) => {
                Outcome::exact(format.infinity(negative))
            }
            (Unpacked::Zero { .. }, _) | (_, Unpacked::Zero { .. }) => {
                Outcome::exact(format.zero(negative))
            }
            (Unpacked::Finite { parts: left, .. }, Unpacked::Finite { parts: right, .. }) => {
                self.round(exact::multiply(left.normalized(), right.normalized()))
            }
        }
    }

    /// `a ÷ b`.
    #[must_use]
    pub fn div(self, a: u64, b: u64) -> Outcome {
        let format = self.format;
        let negative = format.negative(a) != format.negative(b);
        match (format.unpack(a), format.unpack(b)) {
            (Unpacked::Nan { .. }, _) | (_, Unpacked::Nan { .. }) => self.nan(&[a, b]),
            (Unpacked::Infinity { .. }, Unpacked::Infinity { .. })
            | (Unpacked::Zero { .. }, Unpacked::Zero { .. }) => self.invalid(),
            (Unpacked::Infinity { .. }, _) => Outcome::exact(format.infinity(negative)),
            (_, Unpacked::Infinity { .. }) | (Unpacked::Zero { .. }, _) => {
                Outcome::exact(format.zero(negative))
            }
            (Unpacked::Finite { .. }, Unpacked::Zero { .. }) => Outcome {
                bits: format.infinity(negative),
                flags: Flags::DIVIDE_BY_ZERO,
            },
            (Unpacked::Finite { parts: left, .. }, Unpacked::Finite { parts: right, .. }) => {
                self.round(exact::divide(left.normalized(), right.normalized()))
            }
        }
    }

    /// `√a`.
    #[must_use]
    pub fn sqrt(self, a: u64) -> Outcome {
        let format = self.format;
        match format.unpack(a) {
            Unpacked::Nan { .. } => self.nan(&[a]),
            Unpacked::Zero { .. } | Unpacked::Infinity { negative: false } => Outcome::exact(a),
            Unpacked::Infinity { negative: true } => self.invalid(),
            Unpacked::Finite { parts, .. } if parts.negative => self.invalid(),
            Unpacked::Finite { parts, .. } => self.round(exact::square_root(parts.normalized())),
        }
    }

    /// `addend + a × b`, rounded once. A NaN is chosen in the order the
    /// arguments come, the addend first, as ARM's fused multiply-add takes
    /// them; `∞ × 0` signals invalid even beside a quiet NaN addend, a choice
    /// the standard leaves open.
    #[must_use]
    pub fn fused_multiply_add(self, addend: u64, a: u64, b: u64) -> Outcome {
        let format = self.format;
        let (summand, left, right) = (format.unpack(addend), format.unpack(a), format.unpack(b));
        let infinity_times_zero = matches!(
            (left, right),
            (Unpacked::Infinity { .. }, Unpacked::Zero { .. })
                | (Unpacked::Zero { .. }, Unpacked::Infinity { .. })
        );
        let nan = |value: Unpacked| matches!(value, Unpacked::Nan { .. });
        if nan(summand) || nan(left) || nan(right) {
            let chosen = self.nan(&[addend, a, b]);
            if infinity_times_zero && nan(summand) {
                return Outcome {
                    bits: format.default_nan(),
                    flags: Flags::INVALID,
                };
            }
            return chosen;
        }
        if infinity_times_zero {
            return self.invalid();
        }
        let product_negative = format.negative(a) != format.negative(b);
        let product_infinite =
            matches!(left, Unpacked::Infinity { .. }) || matches!(right, Unpacked::Infinity { .. });
        if product_infinite {
            return match summand {
                Unpacked::Infinity { negative } if negative != product_negative => self.invalid(),
                _ => Outcome::exact(format.infinity(product_negative)),
            };
        }
        if let Unpacked::Infinity { negative } = summand {
            return Outcome::exact(format.infinity(negative));
        }
        let product_zero =
            matches!(left, Unpacked::Zero { .. }) || matches!(right, Unpacked::Zero { .. });
        match (product_zero, summand) {
            (true, Unpacked::Zero { negative }) => {
                Outcome::exact(format.zero(if negative == product_negative {
                    negative
                } else {
                    self.zero_sign()
                }))
            }
            (true, _) => Outcome::exact(addend),
            (false, summand) => {
                let (Unpacked::Finite { parts: left, .. }, Unpacked::Finite { parts: right, .. }) =
                    (left, right)
                else {
                    return self.invalid();
                };
                let product = Product::of(left.normalized(), right.normalized());
                match summand {
                    Unpacked::Finite { parts, .. } => match product.plus(parts.normalized()) {
                        Some(sum) => self.round(sum),
                        None => Outcome::exact(format.zero(self.zero_sign())),
                    },
                    _ => self.round(product.exact()),
                }
            }
        }
    }

    /// The remainder `a − n × b`, `n` the integer nearest `a ÷ b`, ties to
    /// the even one: always exact.
    #[must_use]
    pub fn rem(self, a: u64, b: u64) -> Outcome {
        let format = self.format;
        match (format.unpack(a), format.unpack(b)) {
            (Unpacked::Nan { .. }, _) | (_, Unpacked::Nan { .. }) => self.nan(&[a, b]),
            (Unpacked::Infinity { .. }, _) | (_, Unpacked::Zero { .. }) => self.invalid(),
            (Unpacked::Zero { .. }, _) | (Unpacked::Finite { .. }, Unpacked::Infinity { .. }) => {
                Outcome::exact(a)
            }
            (Unpacked::Finite { parts: left, .. }, Unpacked::Finite { parts: right, .. }) => {
                let rest = remainder(left.normalized(), right.normalized());
                if rest.is_zero() {
                    return Outcome::exact(format.zero(left.negative));
                }
                self.round(Exact::of(rest))
            }
        }
    }

    /// `a` rounded to an integral value in its own format; inexact is raised
    /// only where `signal_inexact`, as `roundToIntegralExact` raises it.
    #[must_use]
    pub fn round_to_integral(self, a: u64, signal_inexact: bool) -> Outcome {
        let format = self.format;
        match format.unpack(a) {
            Unpacked::Nan { .. } => self.nan(&[a]),
            Unpacked::Zero { .. } | Unpacked::Infinity { .. } => Outcome::exact(a),
            Unpacked::Finite { parts, .. } => {
                let parts = parts.normalized();
                // A value this large has no fraction.
                if parts.exp >= i32::try_from(format.fraction_bits()).unwrap_or(i32::MAX) {
                    return Outcome::exact(a);
                }
                let (whole, inexact) = exact::to_integer(parts, self.mode);
                let flags = Flags::when(inexact && signal_inexact, Flags::INEXACT);
                let magnitude = whole.unwrap_or(0);
                let bits = if magnitude == 0 {
                    format.zero(parts.negative)
                } else {
                    self.round(Exact::of(integer_parts(parts.negative, magnitude)))
                        .bits
                };
                Outcome { bits, flags }
            }
        }
    }

    /// `a` rounded to an integer in `mode`, the caller holding it to the range
    /// it converts to.
    #[must_use]
    pub fn to_integer(self, a: u64, mode: Mode) -> Integral {
        match self.format.unpack(a) {
            Unpacked::Nan { .. } => Integral::Nan,
            Unpacked::Infinity { negative } => Integral::OutOfRange { negative },
            Unpacked::Zero { negative } => Integral::Value {
                negative,
                magnitude: 0,
                inexact: false,
            },
            Unpacked::Finite { parts, .. } => {
                let parts = parts.normalized();
                match exact::to_integer(parts, mode) {
                    (Some(magnitude), inexact) => Integral::Value {
                        negative: parts.negative,
                        magnitude,
                        inexact,
                    },
                    (None, _) => Integral::OutOfRange {
                        negative: parts.negative,
                    },
                }
            }
        }
    }

    /// The integer of sign `negative` and magnitude `magnitude`, rounded; a
    /// zero magnitude is positive zero.
    #[must_use]
    pub fn from_integer(self, negative: bool, magnitude: u64) -> Outcome {
        if magnitude == 0 {
            return Outcome::exact(self.format.zero(false));
        }
        self.round(Exact::of(integer_parts(negative, magnitude)))
    }

    /// `a`, in this format, in the format `to`, rounded where `to` is
    /// narrower.
    #[must_use]
    pub fn convert(self, a: u64, to: Binary) -> Outcome {
        let target = Self { format: to, ..self };
        match self.format.unpack(a) {
            Unpacked::Nan {
                negative,
                fraction,
                signalling,
            } => Outcome {
                bits: match self.policy.nans {
                    Nans::Propagate => to.quiet_nan(negative, fraction),
                    Nans::Default => to.default_nan(),
                },
                flags: Flags::when(signalling, Flags::INVALID),
            },
            Unpacked::Infinity { negative } => Outcome::exact(to.infinity(negative)),
            Unpacked::Zero { negative } => Outcome::exact(to.zero(negative)),
            Unpacked::Finite { parts, .. } => target.round(Exact::of(parts.normalized())),
        }
    }

    /// `a` compared with `b`: `None` where unordered. A signalling NaN raises
    /// invalid, and where `signalling` a quiet one does too.
    #[must_use]
    pub fn compare(self, a: u64, b: u64, signalling: bool) -> (Option<Ordering>, Flags) {
        let format = self.format;
        let (left, right) = (format.unpack(a), format.unpack(b));
        let nan = |value: Unpacked| match value {
            Unpacked::Nan { signalling, .. } => Some(signalling),
            _ => None,
        };
        if nan(left).is_some() || nan(right).is_some() {
            let raised = signalling || nan(left) == Some(true) || nan(right) == Some(true);
            return (None, Flags::when(raised, Flags::INVALID));
        }
        (Some(order(left).cmp(&order(right))), Flags::NONE)
    }

    /// `x`, exact and nonzero, rounded to the format.
    pub(crate) fn round(self, x: Exact) -> Outcome {
        let format = self.format;
        let x = x.normalized(true);
        let rounding = Rounding {
            mode: self.mode,
            bits: format.precision,
        };
        if x.exp < format.min_exp() || x.sig == 0 {
            return self.tiny(x, rounding);
        }
        let rounded = x.rounded(rounding);
        let exp = x.exp + i32::from(rounded.carried);
        if exp > format.max_exp() {
            let bits = if self.mode.overflows_to_finite(x.negative) {
                format.largest(x.negative)
            } else {
                format.infinity(x.negative)
            };
            return Outcome {
                bits,
                flags: Flags::OVERFLOW | Flags::INEXACT,
            };
        }
        let field = u64::try_from(exp + format.bias).unwrap_or(0);
        Outcome {
            bits: format.pack(x.negative, field, rounded.sig),
            flags: Flags::when(rounded.inexact, Flags::INEXACT),
        }
    }

    /// `x`, below the least normal exponent, rounded as a subnormal.
    fn tiny(self, x: Exact, rounding: Rounding) -> Outcome {
        let format = self.format;
        let shift = u32::try_from(format.min_exp().saturating_sub(x.exp)).unwrap_or(u32::MAX);
        let subnormal = if x.sig == 0 {
            Exact { sig: 0, ..x }.shifted_right(u32::MAX)
        } else {
            x.shifted_right(shift)
        };
        let rounded = subnormal.rounded(rounding);
        let tiny = match self.policy.tininess {
            Tininess::BeforeRounding => true,
            Tininess::AfterRounding => {
                x.sig == 0 || x.exp + i32::from(x.rounded(rounding).carried) < format.min_exp()
            }
        };
        let mut flags = Flags::when(rounded.inexact, Flags::INEXACT);
        if tiny && rounded.inexact {
            flags |= Flags::UNDERFLOW;
        }
        let field = u64::from(rounded.sig & INTEGER != 0);
        Outcome {
            bits: format.pack(x.negative, field, rounded.sig),
            flags,
        }
    }

    /// The NaN an operation over `operands`, at least one a NaN, answers.
    fn nan(self, operands: &[u64]) -> Outcome {
        let format = self.format;
        let mut quiet = None;
        let mut signalling = None;
        for &bits in operands {
            if let Unpacked::Nan {
                negative,
                fraction,
                signalling: s,
            } = format.unpack(bits)
            {
                let slot = if s { &mut signalling } else { &mut quiet };
                slot.get_or_insert((negative, fraction));
            }
        }
        let bits = match (self.policy.nans, signalling.or(quiet)) {
            (Nans::Propagate, Some((negative, fraction))) => format.quiet_nan(negative, fraction),
            (Nans::Propagate, None) | (Nans::Default, _) => format.default_nan(),
        };
        Outcome {
            bits,
            flags: Flags::when(signalling.is_some(), Flags::INVALID),
        }
    }

    /// The default NaN, an invalid operation's result.
    fn invalid(self) -> Outcome {
        Outcome {
            bits: self.format.default_nan(),
            flags: Flags::INVALID,
        }
    }

    /// The sign of an exact zero sum of operands of opposite sign: negative
    /// only when rounding down.
    fn zero_sign(self) -> bool {
        self.mode == Mode::Down
    }
}

/// The parts of the integer of sign `negative` and nonzero `magnitude`.
fn integer_parts(negative: bool, magnitude: u64) -> Parts {
    Parts {
        negative,
        exp: 63,
        sig: magnitude,
    }
    .normalized()
}

/// A rank that orders unpacked non-NaN values as IEEE 754 compares them,
/// zeros of either sign equal.
fn order(u: Unpacked) -> (i8, i32, u64) {
    match u {
        Unpacked::Zero { .. } | Unpacked::Nan { .. } => (0, 0, 0),
        Unpacked::Infinity { negative } => (if negative { -2 } else { 2 }, 0, 0),
        Unpacked::Finite { parts, .. } => {
            let p = parts.normalized();
            if p.negative {
                // Larger magnitudes order lower: both keys reversed.
                (-1, -p.exp, !p.sig)
            } else {
                (1, p.exp, p.sig)
            }
        }
    }
}

/// The remainder of `a` by `b`, both normalized and nonzero, as IEEE 754
/// defines it: exact, its magnitude at most half of `b`'s.
fn remainder(a: Parts, b: Parts) -> Parts {
    let gap = a.exp - b.exp;
    if gap < -1 {
        return a;
    }
    let divisor = u128::from(b.sig);
    // `rest` stays below `divisor` and `odd` is the quotient's last bit.
    let (mut rest, mut odd) = if gap == -1 {
        (u128::from(a.sig), false)
    } else {
        (
            u128::from(a.sig) % divisor,
            (u128::from(a.sig) / divisor) & 1 != 0,
        )
    };
    let mut left = gap.max(0);
    while left > 0 {
        let step = left.min(63);
        let shifted = rest << u32::try_from(step).unwrap_or(63);
        odd = (shifted / divisor) & 1 != 0;
        rest = shifted % divisor;
        left -= step;
    }
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
    let negative = a.negative != flip;
    Parts {
        negative,
        exp: scale,
        sig: u64::try_from(magnitude).unwrap_or(u64::MAX),
    }
    .normalized()
}

/// An exact product of two normalized values: `sig × 2^(exp − 127)`, its
/// top bit at 127 or 126.
#[derive(Clone, Copy, Debug)]
struct Product {
    negative: bool,
    exp: i32,
    sig: u128,
}

impl Product {
    fn of(a: Parts, b: Parts) -> Self {
        Self {
            negative: a.negative != b.negative,
            exp: a.exp + b.exp + 1,
            sig: u128::from(a.sig) * u128::from(b.sig),
        }
    }

    /// The product as an exact result.
    fn exact(self) -> Exact {
        Exact {
            negative: self.negative,
            exp: self.exp - 1,
            sig: self.sig,
            sticky: false,
        }
        .normalized(false)
    }

    /// The product plus `c`, normalized, exactly enough to round correctly at
    /// any precision: `None` where they cancel to exactly zero.
    fn plus(self, c: Parts) -> Option<Exact> {
        // Each at the top of a 256-bit window: bit 254 the place whose
        // exponent each `exp` names, bit 255 free for a carry.
        let product = Wide::of(self.sig).shl(127);
        let addend = Wide::of(u128::from(c.sig)).shl(191);
        let (big, small, exp) = if self.exp >= c.exp {
            (
                (self.negative, product),
                (c.negative, addend, self.exp - c.exp),
                self.exp,
            )
        } else {
            (
                (c.negative, addend),
                (self.negative, product, c.exp - self.exp),
                c.exp,
            )
        };
        let (small_negative, small_bits, gap) = small;
        let (small_bits, sticky) = small_bits.shr_sticky(u32::try_from(gap).unwrap_or(u32::MAX));
        let (big_negative, big_bits) = big;
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
