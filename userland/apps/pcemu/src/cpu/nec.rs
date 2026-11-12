//! The NEC V20/V30 extended operations (`0F xx`).

use super::alu::{self, width, BYTE};
use super::decode::{Insn, Nec};
use super::exec::Loc;
use super::regs::{flag, gpr, Seg};
use super::{Bus, Cpu, Flow};
use crate::bits::lo;

/// Cycle counts the V20 takes for its extended operations, measured from its
/// hardware-captured bus traces; word transfers add their four cycles where
/// they happen.
mod cost {
    pub const TEST1_CL_REG: u32 = 6;
    pub const BIT_REG: u32 = 7;
    pub const TEST1_MEM: u32 = 17;
    pub const BIT_MEM: u32 = 24;
    pub const ADD_SUB4S_SETUP: u32 = 12;
    pub const ADD_SUB4S_EACH: u32 = 22;
    pub const CMP4S_SETUP: u32 = 13;
    pub const CMP4S_EACH: u32 = 21;
    pub const ROL4_REG: u32 = 16;
    pub const ROL4_MEM: u32 = 30;
    pub const ROR4_REG: u32 = 20;
    pub const ROR4_MEM: u32 = 34;
    pub const INS: u32 = 79;
    pub const EXT: u32 = 45;
    pub const UNASSIGNED: u32 = 2;
}

/// Clocks the V20's sequencer idles on opcode `63`, which its decoder does
/// not match, before running on as `66`.
pub(super) const UNDEFINED_63_IDLE: u32 = 59;

/// Clocks the V20's sequencer idles on `D6`, which it lacks, before running
/// on into XLAT.
pub(super) const XLAT_ALIAS_IDLE: u32 = 15;

impl Cpu {
    pub(super) fn nec<B: Bus>(&mut self, bus: &mut B, insn: &Insn, op: Nec) -> Flow {
        match op {
            Nec::Test1 | Nec::Clr1 | Nec::Set1 | Nec::Not1 => self.nec_bit(bus, insn, op),
            Nec::Add4s | Nec::Sub4s | Nec::Cmp4s => self.nec_bcd_string(bus, op),
            Nec::Rol4 | Nec::Ror4 => self.nec_rotate_digit(bus, insn, op == Nec::Rol4),
            Nec::InsBits => self.nec_insert(bus, insn),
            Nec::ExtBits => self.nec_extract(bus, insn),
            Nec::Brkem => {
                self.tick(u32::from(super::timing::costs(self.model.family()).int));
                self.interrupt(bus, lo(insn.imm));
                self.regs.flags &= !flag::MD;
                self.mode = super::Mode::Emulation;
                Flow::Branch
            }
            Nec::Unassigned => {
                self.tick(cost::UNASSIGNED);
                Flow::Next
            }
        }
    }

    fn nec_bit<B: Bus>(&mut self, bus: &mut B, insn: &Insn, op: Nec) -> Flow {
        let w = insn.w;
        let source = if insn.opcode & 8 != 0 {
            lo(insn.imm)
        } else {
            self.regs.get8(1)
        };
        let bit = source & if w { 15 } else { 7 };
        let mask = 1u16 << bit;
        let loc = self.locate(insn);
        let value = self.load(bus, loc, w);
        match op {
            Nec::Test1 => {
                alu::logic(&mut self.regs.flags, value & mask, width(w));
            }
            Nec::Clr1 => self.store(bus, loc, w, value & !mask),
            Nec::Set1 => self.store(bus, loc, w, value | mask),
            _ => self.store(bus, loc, w, value ^ mask),
        }
        let test1 = op == Nec::Test1;
        self.tick(match (loc, test1) {
            (Loc::Reg(_), true) if insn.opcode & 8 == 0 => cost::TEST1_CL_REG,
            (Loc::Reg(_), _) => cost::BIT_REG,
            (Loc::Mem(..), true) => cost::TEST1_MEM,
            (Loc::Mem(..), false) => cost::BIT_MEM,
        });
        Flow::Next
    }

    /// ADD4S, SUB4S and CMP4S: the packed-BCD string at DS:SI combined into
    /// ES:DI, least significant byte first, a byte for every two of CL's
    /// digits, each byte as ADC/SBB then DAA/DAS from a clear carry. The
    /// hardware then leaves SF, AF and CF together as the final carry, and ZF
    /// for an all-zero result.
    fn nec_bcd_string<B: Bus>(&mut self, bus: &mut B, op: Nec) -> Flow {
        let bytes = u16::from(self.regs.get8(1)).div_ceil(2);
        let source = self.prefixes.segment.unwrap_or(Seg::Ds);
        let si = self.regs.gpr[gpr::SI];
        let di = self.regs.gpr[gpr::DI];
        let mut carry = false;
        let mut zero = true;
        for i in 0..bytes {
            let s = self.read8(bus, source, si.wrapping_add(i));
            let d = self.read8(bus, Seg::Es, di.wrapping_add(i));
            let mut flags = 0;
            let r = if op == Nec::Add4s {
                let sum = alu::add(&mut flags, u16::from(d), u16::from(s), carry, BYTE);
                alu::daa(&mut flags, lo(sum))
            } else {
                let diff = alu::sub(&mut flags, u16::from(d), u16::from(s), carry, BYTE);
                alu::das(&mut flags, lo(diff))
            };
            carry = flags & flag::CF != 0;
            zero &= r == 0;
            if op != Nec::Cmp4s {
                self.write8(bus, Seg::Es, di.wrapping_add(i), r);
            }
        }
        self.regs.flags &= !flag::STATUS;
        self.regs.set_flag(flag::SF | flag::AF | flag::CF, carry);
        self.regs.set_flag(flag::ZF, zero);
        let (setup, each) = if op == Nec::Cmp4s {
            (cost::CMP4S_SETUP, cost::CMP4S_EACH)
        } else {
            (cost::ADD_SUB4S_SETUP, cost::ADD_SUB4S_EACH)
        };
        self.tick(setup + each * u32::from(bytes));
        Flow::Next
    }

    /// ROL4/ROR4: a digit rotated through AL's low nibble and the byte
    /// operand. The hardware writes all of AL — ROL4 shifts AL's old low
    /// nibble into its high one, ROR4 copies the whole old operand — and
    /// writes the operand first.
    fn nec_rotate_digit<B: Bus>(&mut self, bus: &mut B, insn: &Insn, left: bool) -> Flow {
        let loc = self.locate(insn);
        let operand = lo(self.load(bus, loc, false));
        let al = self.regs.get8(0);
        let (new_operand, new_al) = if left {
            ((operand << 4) | (al & 0x0F), (al << 4) | (operand >> 4))
        } else {
            ((al << 4) | (operand >> 4), operand)
        };
        self.store(bus, loc, false, u16::from(new_operand));
        self.regs.set8(0, new_al);
        self.tick(match (loc, left) {
            (Loc::Reg(_), true) => cost::ROL4_REG,
            (Loc::Mem(..), true) => cost::ROL4_MEM,
            (Loc::Reg(_), false) => cost::ROR4_REG,
            (Loc::Mem(..), false) => cost::ROR4_MEM,
        });
        Flow::Next
    }

    /// The field length INS and EXT take from the `ModRM` `reg` register or
    /// the immediate; the bit offset is the `r/m` register.
    fn field_length(&self, insn: &Insn) -> u8 {
        let raw = if insn.opcode & 8 != 0 {
            lo(insn.imm)
        } else {
            self.regs.get8(insn.reg)
        };
        (raw & 15) + 1
    }

    /// INS: AX's low bits into the word at ES:DI, at the bit offset the
    /// `r/m` register holds; the offset advances past the field, and DI by a
    /// word when the field reaches the next one. The hardware advances the
    /// offset register before it reads AX, so an offset kept in AL or AH is
    /// part of the field it inserts. The flags are those of
    /// `15 - (offset + length)`.
    fn nec_insert<B: Bus>(&mut self, bus: &mut B, insn: &Insn) -> Flow {
        let offset = self.regs.get8(insn.ea_reg()) & 15;
        let length = self.field_length(insn);
        let di = self.regs.gpr[gpr::DI];
        self.regs.set8(insn.ea_reg(), (offset + length) & 15);
        let field = u32::from(self.regs.ax()) & ((1u32 << length) - 1);
        let lo_word = self.read16(bus, Seg::Es, di);
        let hi_word = self.read16(bus, Seg::Es, di.wrapping_add(2));
        let mask = ((1u32 << length) - 1) << offset;
        let both = (crate::bits::dword(lo_word, hi_word) & !mask) | (field << offset);
        self.write16(bus, Seg::Es, di, crate::bits::low16(both));
        if offset + length > 16 {
            self.write16(bus, Seg::Es, di.wrapping_add(2), crate::bits::high16(both));
        }
        if offset + length >= 16 {
            self.regs.gpr[gpr::DI] = di.wrapping_add(2);
        }
        alu::sub(
            &mut self.regs.flags,
            15,
            u16::from(offset + length),
            false,
            BYTE,
        );
        self.tick(cost::INS);
        Flow::Next
    }

    /// EXT: the bit field at DS:SI into AX, zero-extended; the offset and SI
    /// advance as INS advances DI. The flags are those of
    /// `16 - (offset + length)`, with AF clear.
    fn nec_extract<B: Bus>(&mut self, bus: &mut B, insn: &Insn) -> Flow {
        let offset = self.regs.get8(insn.ea_reg()) & 15;
        let length = self.field_length(insn);
        let source = self.prefixes.segment.unwrap_or(Seg::Ds);
        let si = self.regs.gpr[gpr::SI];
        let lo_word = self.read16(bus, source, si);
        let hi_word = self.read16(bus, source, si.wrapping_add(2));
        let both = crate::bits::dword(lo_word, hi_word);
        let field = (both >> offset) & ((1u32 << length) - 1);
        self.regs.gpr[gpr::AX] = crate::bits::low16(field);
        self.advance_field(insn.ea_reg(), offset, length, gpr::SI);
        alu::sub(
            &mut self.regs.flags,
            16,
            u16::from(offset + length),
            false,
            BYTE,
        );
        self.regs.flags &= !flag::AF;
        self.tick(cost::EXT);
        Flow::Next
    }

    fn advance_field(&mut self, offset_reg: u8, offset: u8, length: u8, pointer: usize) {
        let end = offset + length;
        self.regs.set8(offset_reg, end & 15);
        if end >= 16 {
            self.regs.gpr[pointer] = self.regs.gpr[pointer].wrapping_add(2);
        }
    }
}

impl Insn {
    /// The register the `r/m` field names, for INS and EXT, which take only
    /// registers there.
    fn ea_reg(&self) -> u8 {
        match self.ea {
            super::decode::Ea::Reg(r) => r,
            super::decode::Ea::Mem(_) => 0,
        }
    }
}
