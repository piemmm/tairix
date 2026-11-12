//! The architectural registers.

use crate::bits::{hi, lo, word};

/// A segment register, numbered as instructions encode it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Seg {
    /// The extra segment.
    Es = 0,
    /// The code segment.
    Cs = 1,
    /// The stack segment.
    Ss = 2,
    /// The data segment.
    Ds = 3,
}

impl Seg {
    /// The segment register a two-bit field names; the 8086 family ignores the
    /// field's third bit.
    #[must_use]
    pub const fn from_field(field: u8) -> Self {
        match field & 3 {
            0 => Self::Es,
            1 => Self::Cs,
            2 => Self::Ss,
            _ => Self::Ds,
        }
    }

    /// The register's index into [`Regs::seg`].
    #[must_use]
    pub const fn index(self) -> usize {
        self as usize
    }
}

/// General-register indices in encoding order.
pub mod gpr {
    /// AX, the accumulator.
    pub const AX: usize = 0;
    /// CX, the count register.
    pub const CX: usize = 1;
    /// DX, the data register.
    pub const DX: usize = 2;
    /// BX, the base register.
    pub const BX: usize = 3;
    /// SP, the stack pointer.
    pub const SP: usize = 4;
    /// BP, the base pointer.
    pub const BP: usize = 5;
    /// SI, the source index.
    pub const SI: usize = 6;
    /// DI, the destination index.
    pub const DI: usize = 7;
}

/// The FLAGS register's bits.
pub mod flag {
    /// Carry.
    pub const CF: u16 = 0x0001;
    /// Parity of the result's low byte.
    pub const PF: u16 = 0x0004;
    /// Auxiliary (nibble) carry.
    pub const AF: u16 = 0x0010;
    /// Zero.
    pub const ZF: u16 = 0x0040;
    /// Sign.
    pub const SF: u16 = 0x0080;
    /// Single-step trap.
    pub const TF: u16 = 0x0100;
    /// Interrupt enable.
    pub const IF: u16 = 0x0200;
    /// String direction.
    pub const DF: u16 = 0x0400;
    /// Overflow.
    pub const OF: u16 = 0x0800;
    /// The NEC V20/V30 mode flag: set in native mode, clear in 8080 emulation.
    pub const MD: u16 = 0x8000;
    /// The bits an instruction may change on every modelled CPU.
    pub const WRITABLE: u16 = CF | PF | AF | ZF | SF | TF | IF | DF | OF;
    /// The arithmetic status flags.
    pub const STATUS: u16 = CF | PF | AF | ZF | SF | OF;
    /// Bit 1 reads as one; bits 3 and 5 read as zero.
    pub const RESERVED_ONE: u16 = 0x0002;
    /// Bits 12–15 read as one on the 8086 family and the 80186.
    pub const HIGH_ONES: u16 = 0xF000;
}

/// The architectural register file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Regs {
    /// AX, CX, DX, BX, SP, BP, SI, DI.
    pub gpr: [u16; 8],
    /// ES, CS, SS, DS.
    pub seg: [u16; 4],
    /// The instruction pointer.
    pub ip: u16,
    /// FLAGS.
    pub flags: u16,
}

impl Regs {
    /// The 8-bit register an encoding names: AL, CL, DL, BL, AH, CH, DH, BH.
    #[must_use]
    pub fn get8(&self, reg: u8) -> u8 {
        let word = self.gpr[usize::from(reg & 3)];
        if reg & 4 == 0 {
            lo(word)
        } else {
            hi(word)
        }
    }

    /// Sets the 8-bit register an encoding names.
    pub fn set8(&mut self, reg: u8, value: u8) {
        let slot = &mut self.gpr[usize::from(reg & 3)];
        *slot = if reg & 4 == 0 {
            word(value, hi(*slot))
        } else {
            word(lo(*slot), value)
        };
    }

    /// The 16-bit register an encoding names.
    #[must_use]
    pub fn get16(&self, reg: u8) -> u16 {
        self.gpr[usize::from(reg & 7)]
    }

    /// Sets the 16-bit register an encoding names.
    pub fn set16(&mut self, reg: u8, value: u16) {
        self.gpr[usize::from(reg & 7)] = value;
    }

    /// A segment register's value.
    #[must_use]
    pub fn sreg(&self, seg: Seg) -> u16 {
        self.seg[seg.index()]
    }

    /// Sets a segment register.
    pub fn set_sreg(&mut self, seg: Seg, value: u16) {
        self.seg[seg.index()] = value;
    }

    /// Whether every bit of `mask` is set in FLAGS.
    #[must_use]
    pub fn flag(&self, mask: u16) -> bool {
        self.flags & mask == mask
    }

    /// Sets or clears the FLAGS bits in `mask`.
    pub fn set_flag(&mut self, mask: u16, on: bool) {
        if on {
            self.flags |= mask;
        } else {
            self.flags &= !mask;
        }
    }

    /// AX.
    #[must_use]
    pub fn ax(&self) -> u16 {
        self.gpr[gpr::AX]
    }

    /// CX.
    #[must_use]
    pub fn cx(&self) -> u16 {
        self.gpr[gpr::CX]
    }

    /// SP.
    #[must_use]
    pub fn sp(&self) -> u16 {
        self.gpr[gpr::SP]
    }
}

crate::snapshot::state!(Regs {
    gpr,
    seg,
    ip,
    flags
});

crate::snapshot::state_enum!(Seg {
    0 => Es,
    1 => Cs,
    2 => Ss,
    3 => Ds,
});
