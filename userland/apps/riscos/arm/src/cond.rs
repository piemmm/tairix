//! Condition codes.

use crate::psr;

/// An instruction's condition field.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Condition {
    /// Equal: Z set.
    Eq,
    /// Not equal: Z clear.
    Ne,
    /// Carry set, unsigned higher or same.
    Cs,
    /// Carry clear, unsigned lower.
    Cc,
    /// Minus: N set.
    Mi,
    /// Plus: N clear.
    Pl,
    /// Overflow: V set.
    Vs,
    /// No overflow: V clear.
    Vc,
    /// Unsigned higher: C set and Z clear.
    Hi,
    /// Unsigned lower or same: C clear or Z set.
    Ls,
    /// Signed greater or equal: N equals V.
    Ge,
    /// Signed less than: N differs from V.
    Lt,
    /// Signed greater than: Z clear and N equals V.
    Gt,
    /// Signed less or equal: Z set or N differs from V.
    Le,
    /// Always.
    Al,
    /// Never, on cores before ARMv5; the field of every unconditional
    /// instruction from ARMv5.
    Nv,
}

impl Condition {
    /// Every condition, by field value.
    pub const ALL: [Self; 16] = [
        Self::Eq,
        Self::Ne,
        Self::Cs,
        Self::Cc,
        Self::Mi,
        Self::Pl,
        Self::Vs,
        Self::Vc,
        Self::Hi,
        Self::Ls,
        Self::Ge,
        Self::Lt,
        Self::Gt,
        Self::Le,
        Self::Al,
        Self::Nv,
    ];

    /// The condition a four-bit field holds.
    #[must_use]
    pub const fn field(bits: u32) -> Self {
        Self::ALL[(bits & 15) as usize]
    }

    /// The field value.
    #[must_use]
    pub const fn bits(self) -> u32 {
        self as u32
    }

    /// Whether the condition passes on `flags`, the PSR whose top four bits
    /// are N, Z, C and V.
    #[must_use]
    pub const fn passes(self, flags: u32) -> bool {
        let n = flags & psr::N != 0;
        let z = flags & psr::Z != 0;
        let c = flags & psr::C != 0;
        let v = flags & psr::V != 0;
        match self {
            Self::Eq => z,
            Self::Ne => !z,
            Self::Cs => c,
            Self::Cc => !c,
            Self::Mi => n,
            Self::Pl => !n,
            Self::Vs => v,
            Self::Vc => !v,
            Self::Hi => c && !z,
            Self::Ls => !c || z,
            Self::Ge => n == v,
            Self::Lt => n != v,
            Self::Gt => !z && n == v,
            Self::Le => z || n != v,
            Self::Al => true,
            Self::Nv => false,
        }
    }

    /// The assembler's suffix: empty for `AL`.
    #[must_use]
    pub const fn suffix(self) -> &'static str {
        match self {
            Self::Eq => "EQ",
            Self::Ne => "NE",
            Self::Cs => "CS",
            Self::Cc => "CC",
            Self::Mi => "MI",
            Self::Pl => "PL",
            Self::Vs => "VS",
            Self::Vc => "VC",
            Self::Hi => "HI",
            Self::Ls => "LS",
            Self::Ge => "GE",
            Self::Lt => "LT",
            Self::Gt => "GT",
            Self::Le => "LE",
            Self::Al => "",
            Self::Nv => "NV",
        }
    }
}
