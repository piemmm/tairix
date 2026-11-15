//! IEEE 754's five exceptions, as the flags an operation raises.

use core::ops::{BitOr, BitOrAssign};

/// The exceptions one operation raised.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Flags(u8);

impl Flags {
    /// None raised.
    pub const NONE: Self = Self(0);
    /// An operation with no meaningful result: a signalling NaN, `∞ − ∞`,
    /// `0 × ∞`, `0 / 0`, `∞ / ∞`, the root of a negative, a remainder by zero
    /// or of an infinity, or an integer conversion out of range.
    pub const INVALID: Self = Self(1);
    /// A finite number divided by zero.
    pub const DIVIDE_BY_ZERO: Self = Self(1 << 1);
    /// A rounded result too large for the format.
    pub const OVERFLOW: Self = Self(1 << 2);
    /// A tiny result that was also inexact.
    pub const UNDERFLOW: Self = Self(1 << 3);
    /// A result that was rounded.
    pub const INEXACT: Self = Self(1 << 4);

    /// Whether every exception `other` holds is raised here.
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// Whether none is raised.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// These and `other` together.
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// `flag` where `raised`, else none.
    #[must_use]
    pub const fn when(raised: bool, flag: Self) -> Self {
        if raised {
            flag
        } else {
            Self::NONE
        }
    }
}

impl BitOr for Flags {
    type Output = Self;

    fn bitor(self, other: Self) -> Self {
        self.union(other)
    }
}

impl BitOrAssign for Flags {
    fn bitor_assign(&mut self, other: Self) {
        *self = self.union(other);
    }
}
