//! An x86-16 encoder for the code the emulator puts in its own ROMs and the
//! programs its tests run. Each instruction is a method, so nothing is
//! assembled from text, and a jump names a [`Label`] the encoder resolves.

use crate::cpu::Seg;
use alloc::vec::Vec;

/// A byte register, numbered as instructions encode it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(missing_docs, reason = "the registers' own names")]
pub enum Reg8 {
    Al,
    Cl,
    Dl,
    Bl,
    Ah,
    Ch,
    Dh,
    Bh,
}

/// A word register, numbered as instructions encode it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(missing_docs, reason = "the registers' own names")]
pub enum Reg16 {
    Ax,
    Cx,
    Dx,
    Bx,
    Sp,
    Bp,
    Si,
    Di,
}

/// A conditional jump's condition, numbered as `Jcc` encodes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(missing_docs, reason = "the conditions' own mnemonics")]
pub enum Cond {
    O,
    No,
    B,
    Ae,
    E,
    Ne,
    Be,
    A,
    S,
    Ns,
    P,
    Np,
    L,
    Ge,
    Le,
    G,
}

/// A place in the code a jump can name, bound once.
#[derive(Debug, PartialEq, Eq)]
pub struct Label(usize);

/// Why the code could not be finished.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AsmError {
    /// A jump names a label never bound.
    Unbound,
    /// A short jump's target is more than 128 bytes away.
    TooFar,
    /// The code runs past the end of its segment.
    TooLong,
}

#[derive(Clone, Copy, Debug)]
enum Fixup {
    /// A displacement byte at `at`, relative to the byte after it.
    Rel8 { at: usize, label: usize },
    /// A displacement word at `at`, relative to the byte after it.
    Rel16 { at: usize, label: usize },
    /// A label's offset in the segment, as a word at `at`.
    Abs16 { at: usize, label: usize },
}

/// Code being written at a fixed offset in its segment.
#[derive(Debug)]
pub struct Asm {
    origin: u16,
    code: Vec<u8>,
    labels: Vec<Option<usize>>,
    fixups: Vec<Fixup>,
}

impl Asm {
    /// Code that will sit at `origin` in its segment.
    #[must_use]
    pub const fn new(origin: u16) -> Self {
        Self {
            origin,
            code: Vec::new(),
            labels: Vec::new(),
            fixups: Vec::new(),
        }
    }

    /// The offset the next byte goes at.
    #[must_use]
    pub fn here(&self) -> u16 {
        // Wraps only past the segment's end, which `finish` refuses.
        self.origin
            .wrapping_add(u16::try_from(self.code.len()).unwrap_or(u16::MAX))
    }

    /// A label to bind later.
    pub fn label(&mut self) -> Label {
        self.labels.push(None);
        Label(self.labels.len() - 1)
    }

    /// Binds `label` here.
    pub fn bind(&mut self, label: &Label) {
        if let Some(slot) = self.labels.get_mut(label.0) {
            *slot = Some(self.code.len());
        }
    }

    /// The finished code, every jump resolved.
    pub fn finish(mut self) -> Result<Vec<u8>, AsmError> {
        if usize::from(self.origin) + self.code.len() > 0x1_0000 {
            return Err(AsmError::TooLong);
        }
        for fixup in core::mem::take(&mut self.fixups) {
            match fixup {
                Fixup::Rel8 { at, label } => {
                    let target = self.labels[label].ok_or(AsmError::Unbound)?;
                    let offset =
                        isize::try_from(target).unwrap_or(0) - isize::try_from(at + 1).unwrap_or(0);
                    let byte = i8::try_from(offset).map_err(|_| AsmError::TooFar)?;
                    self.code[at] = byte.to_le_bytes()[0];
                }
                Fixup::Rel16 { at, label } => {
                    let target = self.labels[label].ok_or(AsmError::Unbound)?;
                    // Both lie in the segment, which the length check holds.
                    let to = u16::try_from(target).unwrap_or(0);
                    let from = u16::try_from(at + 2).unwrap_or(0);
                    self.code[at..at + 2].copy_from_slice(&to.wrapping_sub(from).to_le_bytes());
                }
                Fixup::Abs16 { at, label } => {
                    let target = self.labels[label].ok_or(AsmError::Unbound)?;
                    let offset = self.origin.wrapping_add(u16::try_from(target).unwrap_or(0));
                    self.code[at..at + 2].copy_from_slice(&offset.to_le_bytes());
                }
            }
        }
        Ok(self.code)
    }

    /// Raw bytes.
    pub fn bytes(&mut self, data: &[u8]) {
        self.code.extend_from_slice(data);
    }

    /// A little-endian word.
    pub fn word(&mut self, value: u16) {
        self.bytes(&value.to_le_bytes());
    }

    /// The emulator's host call `n`, honoured only in its own ROM.
    pub fn host_call(&mut self, n: u16) {
        let [low, high] = n.to_le_bytes();
        self.bytes(&[0xFE, 0x38, low, high]);
    }

    /// IRET.
    pub fn iret(&mut self) {
        self.bytes(&[0xCF]);
    }

    /// RETF.
    pub fn retf(&mut self) {
        self.bytes(&[0xCB]);
    }

    /// RETF `n`: a far return dropping `n` bytes of arguments.
    pub fn retf_n(&mut self, n: u16) {
        self.bytes(&[0xCA]);
        self.word(n);
    }

    /// RET.
    pub fn ret(&mut self) {
        self.bytes(&[0xC3]);
    }

    /// STI.
    pub fn sti(&mut self) {
        self.bytes(&[0xFB]);
    }

    /// CLI.
    pub fn cli(&mut self) {
        self.bytes(&[0xFA]);
    }

    /// HLT.
    pub fn hlt(&mut self) {
        self.bytes(&[0xF4]);
    }

    /// CLD.
    pub fn cld(&mut self) {
        self.bytes(&[0xFC]);
    }

    /// STC.
    pub fn stc(&mut self) {
        self.bytes(&[0xF9]);
    }

    /// LODSB.
    pub fn lodsb(&mut self) {
        self.bytes(&[0xAC]);
    }

    /// REPE CMPSB.
    pub fn repe_cmpsb(&mut self) {
        self.bytes(&[0xF3, 0xA6]);
    }

    /// XLAT through CS: AL from the byte at CS:BX+AL.
    pub fn xlat_cs(&mut self) {
        self.bytes(&[0x2E, 0xD7]);
    }

    /// TEST a byte register against itself.
    pub fn test8(&mut self, reg: Reg8) {
        let r = reg as u8;
        self.bytes(&[0x84, 0xC0 | (r << 3) | r]);
    }

    /// CMP a byte register with an immediate.
    pub fn cmp_imm8(&mut self, reg: Reg8, value: u8) {
        self.bytes(&[0x80, 0xF8 | reg as u8, value]);
    }

    /// TEST a byte register against an immediate.
    pub fn test_imm8(&mut self, reg: Reg8, value: u8) {
        match reg {
            Reg8::Al => self.bytes(&[0xA8, value]),
            _ => self.bytes(&[0xF6, 0xC0 | reg as u8, value]),
        }
    }

    /// CMP the byte at CS:`address` with an immediate.
    pub fn cmp_cs_byte(&mut self, address: u16, value: u8) {
        self.bytes(&[0x2E, 0x80, 0x3E]);
        self.word(address);
        self.bytes(&[value]);
    }

    /// MOV the byte at CS:`address`, an immediate.
    pub fn mov_cs_byte(&mut self, address: u16, value: u8) {
        self.bytes(&[0x2E, 0xC6, 0x06]);
        self.word(address);
        self.bytes(&[value]);
    }

    /// INT `n`.
    pub fn int(&mut self, n: u8) {
        self.bytes(&[0xCD, n]);
    }

    /// PUSH a word register.
    pub fn push(&mut self, reg: Reg16) {
        self.bytes(&[0x50 | reg as u8]);
    }

    /// POP a word register.
    pub fn pop(&mut self, reg: Reg16) {
        self.bytes(&[0x58 | reg as u8]);
    }

    /// PUSH a segment register.
    pub fn push_seg(&mut self, seg: Seg) {
        self.bytes(&[0x06 | ((seg as u8) << 3)]);
    }

    /// POP into a segment register other than CS.
    pub fn pop_seg(&mut self, seg: Seg) {
        self.bytes(&[0x07 | ((seg as u8) << 3)]);
    }

    /// MOV a word register, an immediate.
    pub fn mov_imm16(&mut self, reg: Reg16, value: u16) {
        self.bytes(&[0xB8 | reg as u8]);
        self.word(value);
    }

    /// MOV a word register, the offset a label is bound at.
    pub fn mov_label(&mut self, reg: Reg16, label: &Label) {
        self.bytes(&[0xB8 | reg as u8, 0, 0]);
        self.fixups.push(Fixup::Abs16 {
            at: self.code.len() - 2,
            label: label.0,
        });
    }

    /// MOV a byte register, an immediate.
    pub fn mov_imm8(&mut self, reg: Reg8, value: u8) {
        self.bytes(&[0xB0 | reg as u8, value]);
    }

    /// MOV a segment register from a word register.
    pub fn mov_to_seg(&mut self, seg: Seg, reg: Reg16) {
        self.bytes(&[0x8E, 0xC0 | ((seg as u8) << 3) | reg as u8]);
    }

    /// MOV a word register from another.
    pub fn mov16(&mut self, to: Reg16, from: Reg16) {
        self.bytes(&[0x89, 0xC0 | ((from as u8) << 3) | to as u8]);
    }

    /// MOV a word register from the word `displacement` bytes past DS:SI.
    pub fn mov_from_si(&mut self, reg: Reg16, displacement: i8) {
        let reg = (reg as u8) << 3;
        match displacement {
            0 => self.bytes(&[0x8B, reg | 0x04]),
            _ => self.bytes(&[0x8B, 0x44 | reg, displacement.to_le_bytes()[0]]),
        }
    }

    /// CMP the byte at DS:SI with an immediate.
    pub fn cmp_at_si(&mut self, value: u8) {
        self.bytes(&[0x80, 0x3C, value]);
    }

    /// CMP the word at DS:`address` with an immediate.
    pub fn cmp_mem16(&mut self, address: u16, value: u16) {
        self.bytes(&[0x81, 0x3E]);
        self.word(address);
        self.word(value);
    }

    /// ADD a word register, a byte immediate it extends by its sign.
    pub fn add_imm8(&mut self, reg: Reg16, value: i8) {
        self.bytes(&[0x83, 0xC0 | reg as u8, value.to_le_bytes()[0]]);
    }

    /// DEC a byte register.
    pub fn dec8(&mut self, reg: Reg8) {
        self.bytes(&[0xFE, 0xC8 | reg as u8]);
    }

    /// DEC a word register.
    pub fn dec16(&mut self, reg: Reg16) {
        self.bytes(&[0x48 | reg as u8]);
    }

    /// REP MOVSW.
    pub fn rep_movsw(&mut self) {
        self.bytes(&[0xF3, 0xA5]);
    }

    /// XOR a word register with itself, zeroing it.
    pub fn zero(&mut self, reg: Reg16) {
        let r = reg as u8;
        self.bytes(&[0x31, 0xC0 | (r << 3) | r]);
    }

    /// SUB SP, `n`.
    pub fn reserve(&mut self, n: u8) {
        self.bytes(&[0x83, 0xEC, n]);
    }

    /// ADD SP, `n`.
    pub fn release(&mut self, n: u8) {
        self.bytes(&[0x83, 0xC4, n]);
    }

    /// OUT `port`, AL.
    pub fn out_al(&mut self, port: u8) {
        self.bytes(&[0xE6, port]);
    }

    /// IN AL, `port`.
    pub fn in_al(&mut self, port: u8) {
        self.bytes(&[0xE4, port]);
    }

    /// OUT DX, AL: a port past the first 256.
    pub fn out_dx_al(&mut self) {
        self.bytes(&[0xEE]);
    }

    /// A short jump, within 128 bytes.
    pub fn jmp_short(&mut self, target: &Label) {
        self.bytes(&[0xEB, 0]);
        self.fixups.push(Fixup::Rel8 {
            at: self.code.len() - 1,
            label: target.0,
        });
    }

    /// A near jump.
    pub fn jmp(&mut self, target: &Label) {
        self.bytes(&[0xE9, 0, 0]);
        self.fixups.push(Fixup::Rel16 {
            at: self.code.len() - 2,
            label: target.0,
        });
    }

    /// A near jump to `target`, an offset in this segment.
    pub fn jmp_to(&mut self, target: u16) {
        let next = self.here().wrapping_add(3);
        self.bytes(&[0xE9]);
        self.word(target.wrapping_sub(next));
    }

    /// A conditional short jump.
    pub fn jcc(&mut self, cond: Cond, target: &Label) {
        self.bytes(&[0x70 | cond as u8, 0]);
        self.fixups.push(Fixup::Rel8 {
            at: self.code.len() - 1,
            label: target.0,
        });
    }

    /// A near call.
    pub fn call(&mut self, target: &Label) {
        self.bytes(&[0xE8, 0, 0]);
        self.fixups.push(Fixup::Rel16 {
            at: self.code.len() - 2,
            label: target.0,
        });
    }

    /// A far jump to `segment:offset`.
    pub fn jmp_far(&mut self, segment: u16, offset: u16) {
        self.bytes(&[0xEA]);
        self.word(offset);
        self.word(segment);
    }

    /// A far jump to `segment` at the offset a label is bound at.
    pub fn jmp_far_label(&mut self, segment: u16, target: &Label) {
        self.bytes(&[0xEA, 0, 0]);
        self.fixups.push(Fixup::Abs16 {
            at: self.code.len() - 2,
            label: target.0,
        });
        self.word(segment);
    }

    /// A far jump through the far pointer at `address` in the code's own
    /// segment.
    pub fn jmp_far_through(&mut self, address: u16) {
        self.bytes(&[0x2E, 0xFF, 0x2E]);
        self.word(address);
    }

    /// A far call to `segment:offset`.
    pub fn call_far(&mut self, segment: u16, offset: u16) {
        self.bytes(&[0x9A]);
        self.word(offset);
        self.word(segment);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instructions_encode_as_the_processor_decodes_them() {
        let mut asm = Asm::new(0x100);
        asm.host_call(0x1234);
        asm.mov_imm16(Reg16::Bx, 0xBEEF);
        asm.mov_imm8(Reg8::Ah, 0x0E);
        asm.mov_to_seg(Seg::Ds, Reg16::Ax);
        asm.push_seg(Seg::Es);
        asm.pop_seg(Seg::Ds);
        asm.zero(Reg16::Si);
        asm.reserve(4);
        asm.release(4);
        asm.jmp_far(0xF000, 0xE05B);
        asm.stc();
        asm.cmp_imm8(Reg8::Ah, 3);
        asm.lodsb();
        asm.repe_cmpsb();
        asm.test8(Reg8::Al);
        asm.out_dx_al();
        assert_eq!(
            asm.finish().unwrap(),
            [
                0xFE, 0x38, 0x34, 0x12, 0xBB, 0xEF, 0xBE, 0xB4, 0x0E, 0x8E, 0xD8, 0x06, 0x1F, 0x31,
                0xF6, 0x83, 0xEC, 0x04, 0x83, 0xC4, 0x04, 0xEA, 0x5B, 0xE0, 0x00, 0xF0, 0xF9, 0x80,
                0xFC, 0x03, 0xAC, 0xF3, 0xA6, 0x84, 0xC0, 0xEE
            ]
        );
    }

    #[test]
    fn memory_and_string_instructions_encode_as_decoded() {
        let mut asm = Asm::new(0x600);
        let target = asm.label();
        asm.mov16(Reg16::Si, Reg16::Sp);
        asm.mov_from_si(Reg16::Dx, 0);
        asm.mov_from_si(Reg16::Cx, 2);
        asm.mov_from_si(Reg16::Ax, -1);
        asm.cmp_at_si(0x80);
        asm.cmp_mem16(0x7DFE, 0xAA55);
        asm.add_imm8(Reg16::Si, 16);
        asm.add_imm8(Reg16::Bx, -2);
        asm.dec8(Reg8::Bl);
        asm.dec16(Reg16::Di);
        asm.rep_movsw();
        asm.jmp_far_through(0x0D82);
        asm.jmp_far_label(0, &target);
        asm.bind(&target);
        assert_eq!(
            asm.finish().unwrap(),
            [
                0x89, 0xE6, 0x8B, 0x14, 0x8B, 0x4C, 0x02, 0x8B, 0x44, 0xFF, 0x80, 0x3C, 0x80, 0x81,
                0x3E, 0xFE, 0x7D, 0x55, 0xAA, 0x83, 0xC6, 0x10, 0x83, 0xC3, 0xFE, 0xFE, 0xCB, 0x4F,
                0xF3, 0xA5, 0x2E, 0xFF, 0x2E, 0x82, 0x0D, 0xEA, 0x28, 0x06, 0x00, 0x00
            ]
        );
    }

    #[test]
    fn byte_tests_and_stores_through_cs_encode() {
        let mut asm = Asm::new(0);
        asm.test_imm8(Reg8::Al, 0x08);
        asm.test_imm8(Reg8::Bl, 0x04);
        asm.cmp_cs_byte(0x0123, 0);
        asm.mov_cs_byte(0x4567, 1);
        assert_eq!(
            asm.finish().unwrap(),
            [
                0xA8, 0x08, 0xF6, 0xC3, 0x04, 0x2E, 0x80, 0x3E, 0x23, 0x01, 0x00, 0x2E, 0xC6, 0x06,
                0x67, 0x45, 0x01
            ]
        );
    }

    #[test]
    fn jumps_resolve_forward_and_back() {
        let mut asm = Asm::new(0);
        let top = asm.label();
        let out = asm.label();
        asm.bind(&top);
        asm.jcc(Cond::E, &out);
        asm.hlt();
        asm.jmp_short(&top);
        asm.call(&out);
        asm.bind(&out);
        asm.iret();
        asm.mov_label(Reg16::Ax, &top);
        assert_eq!(
            asm.finish().unwrap(),
            [0x74, 0x06, 0xF4, 0xEB, 0xFB, 0xE8, 0x00, 0x00, 0xCF, 0xB8, 0x00, 0x00]
        );
        let mut asm = Asm::new(0x7C00);
        let there = asm.label();
        asm.mov_label(Reg16::Si, &there);
        asm.bind(&there);
        assert_eq!(
            asm.finish().unwrap(),
            [0xBE, 0x03, 0x7C],
            "an offset in the segment, not the buffer"
        );
    }

    #[test]
    fn unresolvable_code_is_refused() {
        let mut asm = Asm::new(0);
        let nowhere = asm.label();
        asm.jmp_short(&nowhere);
        assert_eq!(asm.finish(), Err(AsmError::Unbound));
        let mut asm = Asm::new(0);
        let far = asm.label();
        asm.jcc(Cond::Ne, &far);
        asm.bytes(&[0x90; 200]);
        asm.bind(&far);
        assert_eq!(asm.finish(), Err(AsmError::TooFar));
        let mut asm = Asm::new(0xFFFE);
        asm.bytes(&[0; 4]);
        assert_eq!(asm.finish(), Err(AsmError::TooLong));
    }
}
