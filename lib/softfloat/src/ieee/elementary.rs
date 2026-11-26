//! IEEE 754's recommended elementary functions (its clause 9.2) on the
//! interchange formats: each value correctly rounded from the
//! [`elementary`] functions, with the special cases and
//! the exceptions they signal as the standard gives them.

use core::cmp::Ordering;

use crate::binary::Unpacked;
use crate::elementary;
use crate::exact::{Exact, Parts};
use crate::extended::INTEGER;
use crate::flags::Flags;

use super::{Ieee, Outcome};

const ONE: Parts = Parts {
    negative: false,
    exp: 0,
    sig: INTEGER,
};

impl Ieee {
    /// sin a.
    #[must_use]
    pub fn sin(self, a: u64) -> Outcome {
        self.circular(a, elementary::sin, false)
    }

    /// cos a.
    #[must_use]
    pub fn cos(self, a: u64) -> Outcome {
        self.circular(a, elementary::cos, true)
    }

    /// tan a.
    #[must_use]
    pub fn tan(self, a: u64) -> Outcome {
        self.circular(a, elementary::tan, false)
    }

    /// A circular function, one at zero where `even` and odd otherwise.
    fn circular(self, a: u64, function: fn(Parts) -> Exact, even: bool) -> Outcome {
        match self.format.unpack(a) {
            Unpacked::Nan { .. } => self.nan(&[a]),
            Unpacked::Infinity { .. } => self.invalid(),
            Unpacked::Zero { .. } if even => Outcome::exact(self.one()),
            Unpacked::Zero { .. } => Outcome::exact(a),
            Unpacked::Finite { parts, .. } => self.round(function(parts.normalized())),
        }
    }

    /// asin a.
    #[must_use]
    pub fn asin(self, a: u64) -> Outcome {
        match self.format.unpack(a) {
            Unpacked::Nan { .. } => self.nan(&[a]),
            Unpacked::Zero { .. } => Outcome::exact(a),
            Unpacked::Finite { parts, .. } if within_one(parts) => {
                self.round(elementary::asin(parts.normalized()))
            }
            Unpacked::Finite { .. } | Unpacked::Infinity { .. } => self.invalid(),
        }
    }

    /// acos a.
    #[must_use]
    pub fn acos(self, a: u64) -> Outcome {
        match self.format.unpack(a) {
            Unpacked::Nan { .. } => self.nan(&[a]),
            Unpacked::Zero { .. } => self.turns(2, false),
            Unpacked::Finite { .. } if a == self.one() => Outcome::exact(self.format.zero(false)),
            Unpacked::Finite { parts, .. } if within_one(parts) => {
                self.round(elementary::acos(parts.normalized()))
            }
            Unpacked::Finite { .. } | Unpacked::Infinity { .. } => self.invalid(),
        }
    }

    /// atan a.
    #[must_use]
    pub fn atan(self, a: u64) -> Outcome {
        match self.format.unpack(a) {
            Unpacked::Nan { .. } => self.nan(&[a]),
            Unpacked::Zero { .. } => Outcome::exact(a),
            Unpacked::Infinity { negative } => self.turns(2, negative),
            Unpacked::Finite { parts, .. } => {
                self.round(elementary::atan2(parts.normalized(), ONE))
            }
        }
    }

    /// The angle of the point (`x`, `y`) over the whole circle, signed as
    /// `y`: atan2(y, x).
    #[must_use]
    pub fn atan2(self, y: u64, x: u64) -> Outcome {
        let format = self.format;
        let negative = format.negative(y);
        let behind = format.negative(x);
        match (format.unpack(y), format.unpack(x)) {
            (Unpacked::Nan { .. }, _) | (_, Unpacked::Nan { .. }) => self.nan(&[y, x]),
            (Unpacked::Infinity { .. }, Unpacked::Infinity { .. }) => {
                self.turns(if behind { 3 } else { 1 }, negative)
            }
            (Unpacked::Infinity { .. }, _) | (Unpacked::Finite { .. }, Unpacked::Zero { .. }) => {
                self.turns(2, negative)
            }
            (Unpacked::Zero { .. }, _) | (Unpacked::Finite { .. }, Unpacked::Infinity { .. }) => {
                if behind {
                    self.turns(4, negative)
                } else {
                    Outcome::exact(format.zero(negative))
                }
            }
            (Unpacked::Finite { parts: p, .. }, Unpacked::Finite { parts: q, .. }) => {
                self.round(elementary::atan2(p.normalized(), q.normalized()))
            }
        }
    }

    /// ln a.
    #[must_use]
    pub fn log(self, a: u64) -> Outcome {
        self.logarithm(a, elementary::ln)
    }

    /// log10 a: exact at a power of ten.
    #[must_use]
    pub fn log10(self, a: u64) -> Outcome {
        self.logarithm(a, elementary::log10)
    }

    fn logarithm(self, a: u64, function: fn(Parts) -> Exact) -> Outcome {
        match self.format.unpack(a) {
            Unpacked::Nan { .. } => self.nan(&[a]),
            Unpacked::Zero { .. } => Outcome {
                bits: self.format.infinity(true),
                flags: Flags::DIVIDE_BY_ZERO,
            },
            Unpacked::Infinity { negative: false } => Outcome::exact(a),
            Unpacked::Finite { .. } if a == self.one() => Outcome::exact(self.format.zero(false)),
            Unpacked::Finite { parts, .. } if !parts.negative => {
                self.round(function(parts.normalized()))
            }
            Unpacked::Finite { .. } | Unpacked::Infinity { negative: true } => self.invalid(),
        }
    }

    /// e^a.
    #[must_use]
    pub fn exp(self, a: u64) -> Outcome {
        match self.format.unpack(a) {
            Unpacked::Nan { .. } => self.nan(&[a]),
            Unpacked::Zero { .. } => Outcome::exact(self.one()),
            Unpacked::Infinity { negative: true } => Outcome::exact(self.format.zero(false)),
            Unpacked::Infinity { negative: false } => Outcome::exact(a),
            Unpacked::Finite { parts, .. } => self.round(elementary::exp(parts.normalized())),
        }
    }

    /// x^y, IEEE 754's `pow`: one wherever y is a zero or x is one, a quiet
    /// NaN among them; a negative x's power only where y is an integer.
    #[must_use]
    pub fn pow(self, x: u64, y: u64) -> Outcome {
        let format = self.format;
        let (base, power) = (format.unpack(x), format.unpack(y));
        let signalling = |value| {
            matches!(
                value,
                Unpacked::Nan {
                    signalling: true,
                    ..
                }
            )
        };
        if signalling(base) || signalling(power) {
            return self.nan(&[x, y]);
        }
        let one = self.one();
        let odd = matches!(power, Unpacked::Finite { parts, .. } if parts.parity() == Some(true));
        let shrinking = format.negative(y);
        match (base, power) {
            (_, Unpacked::Zero { .. }) => Outcome::exact(one),
            _ if x == one => Outcome::exact(one),
            (Unpacked::Nan { .. }, _) | (_, Unpacked::Nan { .. }) => self.nan(&[x, y]),
            (Unpacked::Zero { negative }, _) => {
                let negative = negative && odd;
                match power {
                    _ if !shrinking => Outcome::exact(format.zero(negative)),
                    Unpacked::Infinity { .. } => Outcome::exact(format.infinity(false)),
                    _ => Outcome {
                        bits: format.infinity(negative),
                        flags: Flags::DIVIDE_BY_ZERO,
                    },
                }
            }
            (_, Unpacked::Infinity { .. }) => {
                let magnitude = x & !format.sign_bit();
                if magnitude == one {
                    return Outcome::exact(one);
                }
                Outcome::exact(if (magnitude > one) == shrinking {
                    format.zero(false)
                } else {
                    format.infinity(false)
                })
            }
            (Unpacked::Infinity { negative }, _) => {
                let negative = negative && odd;
                Outcome::exact(if shrinking {
                    format.zero(negative)
                } else {
                    format.infinity(negative)
                })
            }
            (Unpacked::Finite { parts: x_parts, .. }, Unpacked::Finite { parts: y_parts, .. }) => {
                let integral = y_parts.parity();
                if x_parts.negative && integral.is_none() {
                    return self.invalid();
                }
                let negative = x_parts.negative && integral == Some(true);
                if x & !format.sign_bit() == one {
                    return Outcome::exact(format.with_sign(one, negative));
                }
                let magnitude = Parts {
                    negative: false,
                    ..x_parts.normalized()
                };
                self.round(Exact {
                    negative,
                    ..elementary::pow(magnitude, y_parts.normalized())
                })
            }
        }
    }

    /// One, in this format.
    fn one(self) -> u64 {
        self.format
            .pack(false, u64::from(self.format.bias().unsigned_abs()), INTEGER)
    }

    /// π × `quarters` / 4 of sign `negative`, rounded.
    fn turns(self, quarters: u32, negative: bool) -> Outcome {
        self.round(Exact {
            negative,
            ..elementary::quarter_pis(quarters)
        })
    }
}

/// Whether the magnitude of finite `p` is one at most.
fn within_one(p: Parts) -> bool {
    p.cmp_magnitude(ONE) != Ordering::Greater
}

#[cfg(test)]
#[path = "elementary_tests.rs"]
mod tests;
