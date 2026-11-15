//! How a result is rounded: the direction, and the significand bits kept.

/// IEEE 754's five rounding directions.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Mode {
    /// To nearest, ties to even.
    Nearest,
    /// Toward minus infinity.
    Down,
    /// Toward plus infinity.
    Up,
    /// Toward zero: the 8087's chop.
    Zero,
    /// To nearest, ties away from zero: IEEE 754's `roundTiesToAway`.
    NearestAway,
}

impl Mode {
    /// Every mode.
    pub const ALL: [Self; 5] = [
        Self::Nearest,
        Self::Down,
        Self::Up,
        Self::Zero,
        Self::NearestAway,
    ];

    /// Whether a result of sign `negative`, inexact by any amount, moves
    /// away from zero.
    #[must_use]
    pub const fn away(self, negative: bool) -> bool {
        match self {
            Self::Up => !negative,
            Self::Down => negative,
            Self::Nearest | Self::Zero | Self::NearestAway => false,
        }
    }

    /// Whether an overflowing result of sign `negative` is the largest finite
    /// value rather than infinity, as IEEE 754 has it.
    #[must_use]
    pub const fn overflows_to_finite(self, negative: bool) -> bool {
        match self {
            Self::Nearest | Self::NearestAway => false,
            Self::Zero => true,
            Self::Up => negative,
            Self::Down => !negative,
        }
    }
}

/// A rounding: its direction, and how many significand bits it keeps, the
/// integer bit counted — 24, 53 or 64 for the three formats.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Rounding {
    /// The direction.
    pub mode: Mode,
    /// Significand bits kept, from 1 to 64.
    pub bits: u32,
}

impl Rounding {
    /// Double-extended precision in `mode`.
    #[must_use]
    pub const fn extended(mode: Mode) -> Self {
        Self { mode, bits: 64 }
    }
}
