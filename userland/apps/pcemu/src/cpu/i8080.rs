//! The NEC V-series' 8080 emulation mode.
//!
//! The 8080's registers live in the native ones: A in AL, BC in CX, DE in
//! DX, HL in BX, SP in BP, PC in IP. Code is fetched from CS, data and the
//! stack from DS (DS0), and the 8080's flag byte is FLAGS' low byte, which
//! matches it bit for bit. Arithmetic runs on the native ALU, so its flags
//! are the V20's. `ED ED n` (CALLN) runs native interrupt `n`; `ED FD`
//! (RETEM) leaves emulation.
//!
//! NEC published no per-instruction timing for this mode in the material this
//! model was built from, so each instruction takes the 8080's own state count.

use super::alu::{self, BYTE};
use super::regs::{flag, gpr, Seg};
use super::{linear, Bus, Cpu, Stop};
use crate::bits::{hi, lo, word};

/// The 8080 register numbers opcodes encode, mapped to native 8-bit
/// registers: B, C, D, E, H, L, (M), A.
const NATIVE8: [u8; 8] = [5, 1, 6, 2, 7, 3, 0xFF, 0];

/// The 8080's state count for `op`.
const fn states(op: u8) -> u8 {
    match op {
        0x40..=0x7F => {
            if op & 7 == 6 || op & 0x38 == 0x30 {
                7
            } else {
                5
            }
        }
        0x80..=0xBF => {
            if op & 7 == 6 {
                7
            } else {
                4
            }
        }
        0xE3 => 18,
        0xCD | 0xDD | 0xED | 0xFD => 17,
        0x22 | 0x2A => 16,
        0x32 | 0x3A => 13,
        0xC4 | 0xCC | 0xD4 | 0xDC | 0xE4 | 0xEC | 0xF4 | 0xFC | 0xC5 | 0xD5 | 0xE5 | 0xF5
        | 0xC7 | 0xCF | 0xD7 | 0xDF | 0xE7 | 0xEF | 0xF7 | 0xFF => 11,
        0x01
        | 0x11
        | 0x21
        | 0x31
        | 0x34..=0x36
        | 0x09
        | 0x19
        | 0x29
        | 0x39
        | 0xC1
        | 0xD1
        | 0xE1
        | 0xF1
        | 0xC2
        | 0xC3
        | 0xCA
        | 0xCB
        | 0xD2
        | 0xDA
        | 0xE2
        | 0xEA
        | 0xF2
        | 0xFA
        | 0xC9
        | 0xD9
        | 0xD3
        | 0xDB => 10,
        0x02 | 0x12 | 0x0A | 0x1A | 0x06 | 0x0E | 0x16 | 0x1E | 0x26 | 0x2E | 0x3E | 0xC6
        | 0xCE | 0xD6 | 0xDE | 0xE6 | 0xEE | 0xF6 | 0xFE => 7,
        0x03..=0x05
        | 0x13..=0x15
        | 0x23..=0x25
        | 0x33
        | 0x0B..=0x0D
        | 0x1B..=0x1D
        | 0x2B..=0x2D
        | 0x3B..=0x3D
        | 0xC0
        | 0xC8
        | 0xD0
        | 0xD8
        | 0xE0
        | 0xE8
        | 0xF0
        | 0xF8
        | 0xE9
        | 0xF9 => 5,
        _ => 4,
    }
}

impl Cpu {
    fn r8(&self, r: u8) -> u8 {
        self.regs.get8(NATIVE8[usize::from(r & 7)])
    }

    fn set_r8(&mut self, r: u8, value: u8) {
        self.regs.set8(NATIVE8[usize::from(r & 7)], value);
    }

    fn pair(&self, p: u8) -> u16 {
        match p & 3 {
            0 => self.regs.gpr[gpr::CX],
            1 => self.regs.gpr[gpr::DX],
            2 => self.regs.gpr[gpr::BX],
            _ => self.regs.gpr[gpr::BP],
        }
    }

    fn set_pair(&mut self, p: u8, value: u16) {
        let r = match p & 3 {
            0 => gpr::CX,
            1 => gpr::DX,
            2 => gpr::BX,
            _ => gpr::BP,
        };
        self.regs.gpr[r] = value;
    }

    fn fetch80<B: Bus>(&mut self, bus: &mut B) -> u8 {
        let addr = linear(self.regs.sreg(Seg::Cs), self.regs.ip);
        self.regs.ip = self.regs.ip.wrapping_add(1);
        self.queue
            .take(addr)
            .unwrap_or_else(|| bus.fetch(addr, self.cycles))
    }

    fn fetch80_word<B: Bus>(&mut self, bus: &mut B) -> u16 {
        let lo = self.fetch80(bus);
        let hi = self.fetch80(bus);
        word(lo, hi)
    }

    fn mem80<B: Bus>(&mut self, bus: &mut B, addr: u16) -> u8 {
        self.read8(bus, Seg::Ds, addr)
    }

    fn set_mem80<B: Bus>(&mut self, bus: &mut B, addr: u16, value: u8) {
        self.write8(bus, Seg::Ds, addr, value);
    }

    fn operand80<B: Bus>(&mut self, bus: &mut B, r: u8) -> u8 {
        if r & 7 == 6 {
            self.mem80(bus, self.regs.gpr[gpr::BX])
        } else {
            self.r8(r)
        }
    }

    fn set_operand80<B: Bus>(&mut self, bus: &mut B, r: u8, value: u8) {
        if r & 7 == 6 {
            self.set_mem80(bus, self.regs.gpr[gpr::BX], value);
        } else {
            self.set_r8(r, value);
        }
    }

    fn push80<B: Bus>(&mut self, bus: &mut B, value: u16) {
        let sp = self.regs.gpr[gpr::BP].wrapping_sub(2);
        self.regs.gpr[gpr::BP] = sp;
        self.write16(bus, Seg::Ds, sp, value);
    }

    fn pop80<B: Bus>(&mut self, bus: &mut B) -> u16 {
        let sp = self.regs.gpr[gpr::BP];
        let value = self.read16(bus, Seg::Ds, sp);
        self.regs.gpr[gpr::BP] = sp.wrapping_add(2);
        value
    }

    /// Whether the 8080's condition `cc` holds: NZ, Z, NC, C, PO, PE, P, M.
    pub(crate) fn condition80(&self, cc: u8) -> bool {
        let f = |m| self.regs.flag(m);
        match cc & 7 {
            0 => !f(flag::ZF),
            1 => f(flag::ZF),
            2 => !f(flag::CF),
            3 => f(flag::CF),
            4 => !f(flag::PF),
            5 => f(flag::PF),
            6 => !f(flag::SF),
            _ => f(flag::SF),
        }
    }

    fn alu80(&mut self, op: u8, value: u8) {
        use super::decode::Alu;
        let a = u16::from(self.regs.get8(0));
        let b = u16::from(value);
        let alu = match op & 7 {
            0 => Alu::Add,
            1 => Alu::Adc,
            2 => Alu::Sub,
            3 => Alu::Sbb,
            4 => Alu::And,
            5 => Alu::Xor,
            6 => Alu::Or,
            _ => Alu::Cmp,
        };
        let r = alu::alu(&mut self.regs.flags, alu, a, b, BYTE);
        if alu != Alu::Cmp {
            self.regs.set8(0, lo(r));
        }
    }

    /// Runs one 8080 instruction.
    #[allow(
        clippy::too_many_lines,
        reason = "one arm per 8080 opcode group reads as its opcode map"
    )]
    pub(super) fn step_8080<B: Bus>(&mut self, bus: &mut B) -> Option<Stop> {
        let op = self.fetch80(bus);
        self.tick(u32::from(states(op)));
        match op {
            0x00 | 0x08 | 0x10 | 0x18 | 0x20 | 0x28 | 0x30 | 0x38 => {}
            0x01 | 0x11 | 0x21 | 0x31 => {
                let value = self.fetch80_word(bus);
                self.set_pair(op >> 4, value);
            }
            0x02 | 0x12 => {
                let addr = self.pair(op >> 4);
                let a = self.regs.get8(0);
                self.set_mem80(bus, addr, a);
            }
            0x0A | 0x1A => {
                let addr = self.pair(op >> 4);
                let value = self.mem80(bus, addr);
                self.regs.set8(0, value);
            }
            0x03 | 0x13 | 0x23 | 0x33 => self.set_pair(op >> 4, self.pair(op >> 4).wrapping_add(1)),
            0x0B | 0x1B | 0x2B | 0x3B => self.set_pair(op >> 4, self.pair(op >> 4).wrapping_sub(1)),
            0x04 | 0x0C | 0x14 | 0x1C | 0x24 | 0x2C | 0x34 | 0x3C => {
                let r = op >> 3;
                let value = self.operand80(bus, r);
                let out = alu::inc(&mut self.regs.flags, u16::from(value), BYTE);
                self.set_operand80(bus, r, lo(out));
            }
            0x05 | 0x0D | 0x15 | 0x1D | 0x25 | 0x2D | 0x35 | 0x3D => {
                let r = op >> 3;
                let value = self.operand80(bus, r);
                let out = alu::dec(&mut self.regs.flags, u16::from(value), BYTE);
                self.set_operand80(bus, r, lo(out));
            }
            0x06 | 0x0E | 0x16 | 0x1E | 0x26 | 0x2E | 0x36 | 0x3E => {
                let value = self.fetch80(bus);
                self.set_operand80(bus, op >> 3, value);
            }
            0x07 | 0x0F | 0x17 | 0x1F => {
                let a = self.regs.get8(0);
                let cf = self.regs.flag(flag::CF);
                let (r, out) = match op {
                    0x07 => (a.rotate_left(1), a & 0x80 != 0),
                    0x0F => (a.rotate_right(1), a & 1 != 0),
                    0x17 => ((a << 1) | u8::from(cf), a & 0x80 != 0),
                    _ => ((a >> 1) | if cf { 0x80 } else { 0 }, a & 1 != 0),
                };
                self.regs.set8(0, r);
                self.regs.set_flag(flag::CF, out);
            }
            0x09 | 0x19 | 0x29 | 0x39 => {
                let sum = u32::from(self.regs.gpr[gpr::BX]) + u32::from(self.pair(op >> 4));
                self.regs.gpr[gpr::BX] = crate::bits::low16(sum);
                self.regs.set_flag(flag::CF, sum > 0xFFFF);
            }
            0x22 => {
                let addr = self.fetch80_word(bus);
                let hl = self.regs.gpr[gpr::BX];
                self.write16(bus, Seg::Ds, addr, hl);
            }
            0x2A => {
                let addr = self.fetch80_word(bus);
                self.regs.gpr[gpr::BX] = self.read16(bus, Seg::Ds, addr);
            }
            0x27 => {
                let a = self.regs.get8(0);
                let r = alu::daa(&mut self.regs.flags, a);
                self.regs.set8(0, r);
            }
            0x2F => self.regs.set8(0, !self.regs.get8(0)),
            0x32 => {
                let addr = self.fetch80_word(bus);
                let a = self.regs.get8(0);
                self.set_mem80(bus, addr, a);
            }
            0x3A => {
                let addr = self.fetch80_word(bus);
                let value = self.mem80(bus, addr);
                self.regs.set8(0, value);
            }
            0x37 => self.regs.set_flag(flag::CF, true),
            0x3F => self.regs.set_flag(flag::CF, !self.regs.flag(flag::CF)),
            0x76 => {
                self.halted = true;
                return Some(Stop::Halted);
            }
            0x40..=0x7F => {
                let value = self.operand80(bus, op);
                self.set_operand80(bus, op >> 3, value);
            }
            0x80..=0xBF => {
                let value = self.operand80(bus, op);
                self.alu80(op >> 3, value);
            }
            0xC6 | 0xCE | 0xD6 | 0xDE | 0xE6 | 0xEE | 0xF6 | 0xFE => {
                let value = self.fetch80(bus);
                self.alu80(op >> 3, value);
            }
            0xC0 | 0xC8 | 0xD0 | 0xD8 | 0xE0 | 0xE8 | 0xF0 | 0xF8 => {
                if self.condition80(op >> 3) {
                    self.regs.ip = self.pop80(bus);
                    self.queue.flush();
                    self.tick(6);
                }
            }
            0xC9 | 0xD9 => {
                self.regs.ip = self.pop80(bus);
                self.queue.flush();
            }
            0xC1 | 0xD1 | 0xE1 => {
                let value = self.pop80(bus);
                self.set_pair(op >> 4, value);
            }
            0xF1 => {
                let psw = self.pop80(bus);
                self.regs.set8(0, hi(psw));
                let mask = flag::SF | flag::ZF | flag::AF | flag::PF | flag::CF;
                self.regs.flags = (self.regs.flags & !mask) | (u16::from(lo(psw)) & mask);
            }
            0xC5 | 0xD5 | 0xE5 => self.push80(bus, self.pair(op >> 4)),
            0xF5 => {
                let psw = word(lo(self.regs.flags), self.regs.get8(0));
                self.push80(bus, psw);
            }
            0xC2 | 0xCA | 0xD2 | 0xDA | 0xE2 | 0xEA | 0xF2 | 0xFA => {
                let target = self.fetch80_word(bus);
                if self.condition80(op >> 3) {
                    self.regs.ip = target;
                    self.queue.flush();
                }
            }
            0xC3 | 0xCB => {
                self.regs.ip = self.fetch80_word(bus);
                self.queue.flush();
            }
            0xC4 | 0xCC | 0xD4 | 0xDC | 0xE4 | 0xEC | 0xF4 | 0xFC => {
                let target = self.fetch80_word(bus);
                if self.condition80(op >> 3) {
                    self.push80(bus, self.regs.ip);
                    self.regs.ip = target;
                    self.queue.flush();
                    self.tick(6);
                }
            }
            0xED => return self.ed_prefix(bus),
            0xCD | 0xDD | 0xFD => {
                let target = self.fetch80_word(bus);
                self.push80(bus, self.regs.ip);
                self.regs.ip = target;
                self.queue.flush();
            }
            0xC7 | 0xCF | 0xD7 | 0xDF | 0xE7 | 0xEF | 0xF7 | 0xFF => {
                self.push80(bus, self.regs.ip);
                self.regs.ip = u16::from(op & 0x38);
                self.queue.flush();
            }
            0xD3 => {
                let port = self.fetch80(bus);
                bus.output(u16::from(port), self.regs.get8(0), self.cycles);
            }
            0xDB => {
                let port = self.fetch80(bus);
                let value = bus.input(u16::from(port), self.cycles);
                self.regs.set8(0, value);
            }
            0xE3 => {
                let sp = self.regs.gpr[gpr::BP];
                let top = self.read16(bus, Seg::Ds, sp);
                let hl = self.regs.gpr[gpr::BX];
                self.write16(bus, Seg::Ds, sp, hl);
                self.regs.gpr[gpr::BX] = top;
            }
            0xE9 => {
                self.regs.ip = self.regs.gpr[gpr::BX];
                self.queue.flush();
            }
            0xEB => self.regs.gpr.swap(gpr::BX, gpr::DX),
            0xF3 => self.regs.set_flag(flag::IF, false),
            0xFB => {
                if !self.regs.flag(flag::IF) {
                    self.inhibit = true;
                }
                self.regs.set_flag(flag::IF, true);
            }
            0xF9 => self.regs.gpr[gpr::BP] = self.regs.gpr[gpr::BX],
        }
        None
    }

    /// `ED`: CALLN and RETEM, or the 8080's CALL alias for any other byte.
    fn ed_prefix<B: Bus>(&mut self, bus: &mut B) -> Option<Stop> {
        match self.fetch80(bus) {
            0xED => {
                let vector = self.fetch80(bus);
                self.interrupt(bus, vector);
            }
            0xFD => {
                self.regs.ip = self.pop(bus);
                let cs = self.pop(bus);
                self.regs.set_sreg(Seg::Cs, cs);
                let popped = self.pop(bus);
                self.regs.flags = self.normalize_flags(popped, false);
                self.mode = super::Mode::Native;
                self.queue.flush();
            }
            low => {
                let high = self.fetch80(bus);
                self.push80(bus, self.regs.ip);
                self.regs.ip = word(low, high);
                self.queue.flush();
            }
        }
        None
    }
}
