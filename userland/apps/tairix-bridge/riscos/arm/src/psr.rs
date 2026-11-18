//! Program status registers and processor modes.
//!
//! A PSR is held in its 32-bit layout. In a 26-bit mode the same flags and
//! mode bits also live in R15 beside the program counter, and [`r15_psr`]
//! and [`from_r15`] move them between the two layouts.

/// Negative.
pub const N: u32 = 1 << 31;
/// Zero.
pub const Z: u32 = 1 << 30;
/// Carry.
pub const C: u32 = 1 << 29;
/// Overflow.
pub const V: u32 = 1 << 28;
/// Saturation (ARMv5TE).
pub const Q: u32 = 1 << 27;
/// The low two bits of the Thumb-2 `IT` state.
pub const IT_LOW: u32 = 0b11 << 25;
/// Jazelle state; always clear, as no core here has Jazelle.
pub const J: u32 = 1 << 24;
/// The SIMD greater-or-equal flags (ARMv6).
pub const GE: u32 = 0b1111 << 16;
/// The high six bits of the Thumb-2 `IT` state.
pub const IT_HIGH: u32 = 0b11_1111 << 10;
/// Big-endian data (ARMv6).
pub const E: u32 = 1 << 9;
/// Asynchronous aborts masked (ARMv6).
pub const A: u32 = 1 << 8;
/// IRQs masked.
pub const I: u32 = 1 << 7;
/// FIQs masked.
pub const F: u32 = 1 << 6;
/// Thumb state.
pub const T: u32 = 1 << 5;
/// The mode field.
pub const M: u32 = 0b1_1111;
/// The four condition flags.
pub const NZCV: u32 = N | Z | C | V;

/// R15's PSR bits in a 26-bit mode: the flags, I, F and the mode.
pub const R15_PSR: u32 = NZCV | R15_I | R15_F | R15_MODE;
/// R15's program counter bits in a 26-bit mode.
pub const R15_PC: u32 = 0x03FF_FFFC;
/// IRQs masked, as R15 holds it.
pub const R15_I: u32 = 1 << 27;
/// FIQs masked, as R15 holds it.
pub const R15_F: u32 = 1 << 26;
/// The 26-bit mode, as R15 holds it.
pub const R15_MODE: u32 = 0b11;

/// The flags, I, F and mode of `psr` in R15's layout.
#[must_use]
pub const fn r15_psr(psr: u32) -> u32 {
    (psr & NZCV)
        | if psr & I != 0 { R15_I } else { 0 }
        | if psr & F != 0 { R15_F } else { 0 }
        | (psr & R15_MODE)
}

/// `psr` with the flags, I, F and the low mode bits replaced by those
/// `r15` holds; the 26-bit modes' fifth mode bit is always clear.
#[must_use]
pub const fn from_r15(psr: u32, r15: u32) -> u32 {
    (psr & !(NZCV | I | F | M))
        | (r15 & NZCV)
        | if r15 & R15_I != 0 { I } else { 0 }
        | if r15 & R15_F != 0 { F } else { 0 }
        | (r15 & R15_MODE)
}

/// A processor mode.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Mode {
    /// 26-bit user.
    User26 = 0b00000,
    /// 26-bit FIQ.
    Fiq26 = 0b00001,
    /// 26-bit IRQ.
    Irq26 = 0b00010,
    /// 26-bit supervisor.
    Supervisor26 = 0b00011,
    /// User.
    User = 0b10000,
    /// FIQ.
    Fiq = 0b10001,
    /// IRQ.
    Irq = 0b10010,
    /// Supervisor.
    Supervisor = 0b10011,
    /// Abort.
    Abort = 0b10111,
    /// Undefined.
    Undefined = 0b11011,
    /// System: user's registers with privilege (ARMv4).
    System = 0b11111,
}

impl Mode {
    /// The mode the five-bit field `bits` names, if any core has it.
    #[must_use]
    pub const fn from_bits(bits: u32) -> Option<Self> {
        Some(match bits & M {
            0b00000 => Self::User26,
            0b00001 => Self::Fiq26,
            0b00010 => Self::Irq26,
            0b00011 => Self::Supervisor26,
            0b10000 => Self::User,
            0b10001 => Self::Fiq,
            0b10010 => Self::Irq,
            0b10011 => Self::Supervisor,
            0b10111 => Self::Abort,
            0b11011 => Self::Undefined,
            0b11111 => Self::System,
            _ => return None,
        })
    }

    /// The mode field value.
    #[must_use]
    pub const fn bits(self) -> u32 {
        self as u32
    }

    /// Whether this is one of the 26-bit modes.
    #[must_use]
    pub const fn is_26bit(self) -> bool {
        self as u32 & 0b10000 == 0
    }

    /// Whether the mode is privileged.
    #[must_use]
    pub const fn privileged(self) -> bool {
        !matches!(self, Self::User26 | Self::User)
    }

    /// The register bank the mode sees: each 26-bit mode shares its 32-bit
    /// counterpart's, and System shares User's.
    #[must_use]
    pub const fn bank(self) -> Bank {
        match self {
            Self::User26 | Self::User | Self::System => Bank::User,
            Self::Fiq26 | Self::Fiq => Bank::Fiq,
            Self::Irq26 | Self::Irq => Bank::Irq,
            Self::Supervisor26 | Self::Supervisor => Bank::Supervisor,
            Self::Abort => Bank::Abort,
            Self::Undefined => Bank::Undefined,
        }
    }
}

/// A register bank: the R13 and R14, and for FIQ also the R8 to R12, that a
/// group of modes shares, and for each exception bank its SPSR.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Bank {
    /// User and System, which have no SPSR.
    User,
    /// FIQ.
    Fiq,
    /// IRQ.
    Irq,
    /// Supervisor.
    Supervisor,
    /// Abort.
    Abort,
    /// Undefined.
    Undefined,
}

impl Bank {
    /// Every bank.
    pub const ALL: [Self; 6] = [
        Self::User,
        Self::Fiq,
        Self::Irq,
        Self::Supervisor,
        Self::Abort,
        Self::Undefined,
    ];

    /// The bank's index into [`Bank::ALL`].
    #[must_use]
    pub const fn index(self) -> usize {
        self as usize
    }

    /// The index of the bank's SPSR, or `None` for User, which has none.
    #[must_use]
    pub const fn spsr(self) -> Option<usize> {
        match self {
            Self::User => None,
            Self::Fiq => Some(0),
            Self::Irq => Some(1),
            Self::Supervisor => Some(2),
            Self::Abort => Some(3),
            Self::Undefined => Some(4),
        }
    }
}
