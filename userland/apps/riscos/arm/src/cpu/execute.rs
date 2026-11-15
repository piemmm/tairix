//! What each instruction does, as the architecture manual's pseudocode
//! defines it, with each profile's choices where it leaves one.

use super::alu::{self, add_with_carry, shift_immediate, shift_register, Shifted};
use super::fpa::completed;
use super::{Cpu, Flow, Stop, Unmodelled};
use crate::bus::Bus;
use crate::cp15::{Fault, Outcome};
use crate::instruction::{
    ArithmeticOp, BitfieldOp, BranchOp, CallOp, CoRegisterOp, CompareOp, DataOp, ExchangeOp,
    ExtendOp, HintOp, Instruction, LongOp, MoveOp, MultiplyOp, Op, Operand, PackOp, SaturateOp,
    Shift, Source, UnaryOp, WideOp,
};
use crate::isa::Feature;
use crate::psr;
use crate::reg::Reg;
use crate::word::{high, low};

const PC: Reg = Reg::PC;

/// `value`'s low 32 bits, as the architecture's arithmetic wraps.
pub(super) const fn wrap(value: i64) -> u32 {
    low(value.cast_unsigned())
}

/// A halfword of `value`, the top one when `top`, sign-extended.
const fn half(value: u32, top: bool) -> i64 {
    let bits = if top { value >> 16 } else { value };
    let b = bits.to_le_bytes();
    i16::from_le_bytes([b[0], b[1]]) as i64
}

/// A byte sign-extended.
pub(super) fn sext8(value: u32) -> u32 {
    i32::from(value.to_le_bytes()[0].cast_signed()).cast_unsigned()
}

/// A halfword sign-extended.
pub(super) fn sext16(value: u32) -> u32 {
    let b = value.to_le_bytes();
    i32::from(i16::from_le_bytes([b[0], b[1]])).cast_unsigned()
}

/// The fields of an `MSR` mask, as the PSR bytes they write.
const fn psr_bytes(fields: u8) -> u32 {
    let mut bytes = 0;
    let mut n = 0;
    while n < 4 {
        if fields & (1 << n) != 0 {
            bytes |= 0xFF << (8 * n);
        }
        n += 1;
    }
    bytes
}

/// A data-processing operation, whichever form decoded it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Alu {
    Data(DataOp),
    Compare(CompareOp),
    Move(MoveOp),
}

impl Cpu {
    /// Whether the processor is in Thumb state.
    pub(super) const fn thumb(&self) -> bool {
        self.cpsr & psr::T != 0
    }

    /// R15 as an operand: the instruction's address plus 8, or plus 4 in
    /// Thumb state.
    pub(super) const fn pc_operand(&self) -> u32 {
        if self.thumb() {
            self.pc.wrapping_add(4)
        } else {
            self.advance(self.pc, 8)
        }
    }

    /// A register as a transfer's base or an immediate's first operand: in
    /// Thumb state R15 is the PC aligned to a word, as literal addressing
    /// and `ADR` have it.
    pub(super) fn base(&self, reg: Reg) -> u32 {
        if reg == PC && self.thumb() {
            self.pc_operand() & !3
        } else {
            self.read(reg)
        }
    }

    /// What a branch with link leaves in R14: the next instruction, with
    /// the PSR in a 26-bit mode and bit 0 set in Thumb state.
    const fn link(&self) -> u32 {
        if self.thumb() {
            self.next | 1
        } else {
            self.next | self.r15_psr()
        }
    }

    /// The PSR bits R15 holds beside the PC in a 26-bit mode; none in a
    /// 32-bit one.
    pub(super) const fn r15_psr(&self) -> u32 {
        if self.mode.is_26bit() {
            psr::r15_psr(self.cpsr)
        } else {
            0
        }
    }

    /// A register as a base or first operand: R15 is the PC alone.
    pub(super) fn read(&self, reg: Reg) -> u32 {
        self.r
            .get(reg.index())
            .copied()
            .unwrap_or_else(|| self.pc_operand())
    }

    /// A register as a second operand: in a 26-bit mode R15 carries the PSR.
    fn read_with_psr(&self, reg: Reg) -> u32 {
        self.r
            .get(reg.index())
            .copied()
            .unwrap_or_else(|| self.pc_operand() | self.r15_psr())
    }

    /// A register in a register-shifted operand, where R15 reads one word
    /// further on, as ARMv4 cores' extra cycle had it; the architecture
    /// leaves it UNPREDICTABLE.
    fn read_late(&self, reg: Reg, with_psr: bool) -> u32 {
        match self.r.get(reg.index()) {
            Some(&value) => value,
            None if with_psr => self.advance(self.pc, 12) | self.r15_psr(),
            None => self.advance(self.pc, 12),
        }
    }

    /// R15 as `STR` and `STM` store it.
    pub(super) const fn stored_pc(&self) -> u32 {
        self.advance(self.pc, self.profile.stored_pc) | self.r15_psr()
    }

    /// Writes a register; R15 branches.
    pub(super) fn write(&mut self, reg: Reg, value: u32) {
        match self.r.get_mut(reg.index()) {
            Some(slot) => *slot = value,
            None => self.branch(value),
        }
    }

    /// A branch within the instruction set executing.
    pub(super) const fn branch(&mut self, target: u32) {
        self.next = if self.mode.is_26bit() {
            target & psr::R15_PC
        } else if self.cpsr & psr::T != 0 {
            target & !1
        } else {
            target & !3
        };
    }

    /// A branch that selects Thumb by the target's bit 0.
    pub(super) const fn exchange(&mut self, target: u32) {
        if target & 1 != 0 {
            self.cpsr |= psr::T;
            self.next = target & !1;
        } else {
            self.cpsr &= !psr::T;
            self.next = target & !3;
        }
    }

    /// Whether loads to the PC interwork: from ARMv5.
    pub(super) const fn loads_interwork(&self) -> bool {
        self.profile.isa.has(Feature::V5)
    }

    /// A load to the PC.
    pub(super) const fn load_pc(&mut self, target: u32) {
        if self.loads_interwork() {
            self.exchange(target);
        } else {
            self.branch(target);
        }
    }

    /// Whether an interworking branch to `target`, an ARM address that is
    /// not word-aligned, is refused as the UNPREDICTABLE it is; the caller
    /// asks before anything else changes.
    pub(super) fn misaligned_exchange(&mut self, target: u32) -> bool {
        target & 3 == 2 && self.refuse()
    }

    /// A data-processing write to the PC: interworking from ARMv7 in ARM
    /// state, a plain branch in Thumb state.
    fn alu_pc(&mut self, target: u32) {
        if self.profile.isa.version() >= 7 && !self.thumb() {
            if !self.misaligned_exchange(target) {
                self.exchange(target);
            }
        } else {
            self.branch(target);
        }
    }

    const fn flag(&self, flag: u32) -> bool {
        self.cpsr & flag != 0
    }

    /// Sets N and Z from `result`, and C and V as given.
    fn set_flags(&mut self, result: u32, carry: Option<bool>, overflow: Option<bool>) {
        let mut flags = self.cpsr & (psr::C | psr::V);
        if let Some(carry) = carry {
            flags = (flags & !psr::C) | if carry { psr::C } else { 0 };
        }
        if let Some(overflow) = overflow {
            flags = (flags & !psr::V) | if overflow { psr::V } else { 0 };
        }
        if result >> 31 != 0 {
            flags |= psr::N;
        }
        if result == 0 {
            flags |= psr::Z;
        }
        self.cpsr = (self.cpsr & !psr::NZCV) | flags;
    }

    fn saturated(&mut self, saturated: bool) {
        if saturated {
            self.cpsr |= psr::Q;
        }
    }

    /// Executes `instruction`, whose condition passed, fetched from a host
    /// page when `host`.
    #[allow(clippy::too_many_lines)] // One arm per form, each handing its operands on.
    pub(super) fn execute<B: Bus>(
        &mut self,
        bus: &mut B,
        instruction: &Instruction,
        host: bool,
    ) -> Flow {
        match instruction.op {
            Op::Data {
                op,
                s,
                rd,
                rn,
                operand,
            } => self.data(Alu::Data(op), s, rd, rn, operand),
            Op::Compare {
                op,
                rd,
                rn,
                operand,
            } => self.data(Alu::Compare(op), true, rd, rn, operand),
            Op::Move { op, s, rd, operand } => self.data(Alu::Move(op), s, rd, PC, operand),
            Op::Wide { op, rd, value } => {
                let value = u32::from(value);
                let result = match op {
                    WideOp::Movw => value,
                    WideOp::Movt => (self.read(rd) & 0xFFFF) | (value << 16),
                };
                self.write(rd, result);
                Flow::Next
            }
            Op::Multiply {
                op,
                s,
                x,
                y,
                rd,
                rn,
                rm,
                ra,
            } => {
                self.multiply(op, s, [x, y], rd, rn, rm, ra);
                Flow::Next
            }
            Op::MultiplyLong {
                op,
                s,
                x,
                y,
                lo,
                hi,
                rn,
                rm,
            } => {
                self.multiply_long(op, s, [x, y], lo, hi, rn, rm);
                Flow::Next
            }
            Op::Parallel {
                prefix,
                lanes,
                rd,
                rn,
                rm,
            } => {
                let (result, ge) = alu::parallel(prefix, lanes, self.read(rn), self.read(rm));
                self.write(rd, result);
                if let Some(ge) = ge {
                    self.cpsr = (self.cpsr & !psr::GE) | (ge << 16);
                }
                Flow::Next
            }
            Op::Arithmetic { op, rd, rn, rm } => {
                self.arithmetic(op, rd, rn, rm);
                Flow::Next
            }
            Op::Unary { op, rd, rm } => {
                let m = self.read(rm);
                let result = match op {
                    UnaryOp::Clz => m.leading_zeros(),
                    UnaryOp::Rev => m.swap_bytes(),
                    UnaryOp::Rev16 => ((m & 0x00FF_00FF) << 8) | ((m >> 8) & 0x00FF_00FF),
                    UnaryOp::Revsh => sext16(((m & 0xFF) << 8) | ((m >> 8) & 0xFF)),
                    UnaryOp::Rbit => m.reverse_bits(),
                };
                self.write(rd, result);
                Flow::Next
            }
            Op::Saturate {
                op,
                rd,
                saturate,
                rn,
                shift,
            } => {
                self.saturate(op, rd, saturate, rn, shift);
                Flow::Next
            }
            Op::Extend {
                op,
                rd,
                rn,
                rm,
                rotation,
            } => {
                self.extend(op, rd, rn, rm, rotation);
                Flow::Next
            }
            Op::Pack {
                op,
                rd,
                rn,
                rm,
                shift,
            } => {
                let n = self.read(rn);
                let shifted = shift_immediate(self.read(rm), shift, false).value;
                let result = match op {
                    PackOp::Pkhbt => (n & 0xFFFF) | (shifted & 0xFFFF_0000),
                    PackOp::Pkhtb => (n & 0xFFFF_0000) | (shifted & 0xFFFF),
                };
                self.write(rd, result);
                Flow::Next
            }
            Op::Bitfield {
                op,
                rd,
                rn,
                lsb,
                top,
            } => {
                self.bitfield(op, rd, rn, lsb, top);
                Flow::Next
            }
            Op::Status { spsr, rd } => {
                let value = if spsr {
                    match self.spsr() {
                        Some(value) => value,
                        None if self.refuse() => return Flow::Next,
                        None => self.cpsr,
                    }
                } else {
                    self.cpsr & !(psr::IT_LOW | psr::IT_HIGH | psr::J | psr::T)
                };
                self.write(rd, value);
                Flow::Next
            }
            Op::StatusWrite {
                spsr,
                fields,
                source,
            } => {
                let value = match source {
                    Source::Immediate(immediate) => immediate.value(),
                    Source::Register(rm) => self.read(rm),
                };
                let bytes = psr_bytes(fields);
                if spsr {
                    if !self.write_spsr(value, bytes) {
                        self.refuse();
                    }
                } else {
                    self.write_cpsr(value, bytes, false);
                }
                Flow::Next
            }
            Op::Branch { op, offset } => {
                let target = self.pc_operand().wrapping_add_signed(offset);
                match op {
                    BranchOp::B => self.branch(target),
                    BranchOp::Bl => {
                        self.r[14] = self.link();
                        self.branch(target);
                    }
                    // From Thumb the target is word-aligned ARM code; from
                    // ARM it is Thumb code.
                    BranchOp::Blx if self.thumb() => {
                        self.r[14] = self.link();
                        let aligned = (self.pc_operand() & !3).wrapping_add_signed(offset);
                        self.exchange(aligned);
                    }
                    BranchOp::Blx => {
                        self.r[14] = self.link();
                        self.exchange(target | 1);
                    }
                }
                Flow::Next
            }
            Op::Exchange { op, rm } => {
                let target = self.read(rm);
                if self.misaligned_exchange(target) {
                    return Flow::Next;
                }
                if op == ExchangeOp::Blx {
                    self.r[14] = self.link();
                }
                self.exchange(target);
                Flow::Next
            }
            Op::CompareBranch {
                nonzero,
                rn,
                offset,
            } => {
                if (self.read(rn) != 0) == nonzero {
                    self.branch(self.pc_operand().wrapping_add(offset));
                }
                Flow::Next
            }
            Op::IfThen { firstcond, mask } => {
                self.set_itstate((firstcond.bits().to_le_bytes()[0] << 4) | (mask & 15));
                Flow::Next
            }
            Op::TableBranch { half, rn, rm } => self.table_branch(bus, half, rn, rm),
            Op::Call { op, value } => match op {
                CallOp::Swi => Flow::Stop(Stop::Swi { number: value }),
                CallOp::Udf if host => Flow::Stop(Stop::HostCall { number: value }),
                CallOp::Udf => {
                    self.undefined();
                    Flow::Next
                }
                CallOp::Bkpt => {
                    self.prefetch_abort(Fault::Debug, self.pc, self.pc);
                    Flow::Next
                }
            },
            Op::Hint { op, .. } => self.hint(op),
            Op::Barrier { .. } | Op::Preload { .. } => Flow::Next,
            Op::ClearExclusive => {
                self.exclusive = None;
                Flow::Next
            }
            Op::Transfer {
                op,
                rt,
                rn,
                up,
                offset,
                index,
            } => self.transfer(bus, op, rt, rn, up, offset, index),
            Op::Block {
                op,
                rn,
                mode,
                writeback,
                user,
                list,
            } => self.block(bus, op, rn, mode, writeback, user, list),
            Op::Dual {
                op,
                rt,
                rt2,
                rn,
                up,
                offset,
                index,
            } => self.dual(bus, op, [rt, rt2], rn, up, offset, index),
            Op::Swap { op, rt, rt2, rn } => self.swap(bus, op, rt, rt2, rn),
            Op::Exclusive {
                op,
                rd,
                rt,
                rt2,
                rn,
                offset,
            } => self.exclusive(bus, op, rd, rt, rt2, rn, offset),
            Op::ChangeState {
                imod,
                change_mode,
                interrupts,
                mode,
            } => {
                self.change_state(imod, change_mode, interrupts, mode);
                Flow::Next
            }
            Op::SetEndian { big } => {
                self.cpsr = if big {
                    self.cpsr | psr::E
                } else {
                    self.cpsr & !psr::E
                };
                Flow::Next
            }
            Op::SaveState {
                mode,
                writeback,
                bank,
            } => self.save_state(bus, mode, writeback, bank),
            Op::ReturnFromException {
                rn,
                mode,
                writeback,
            } => self.return_from_exception(bus, rn, mode, writeback),
            Op::CoprocessorRegister {
                op,
                cp,
                opc1,
                rt,
                crn,
                crm,
                opc2,
            } if cp == 15 && matches!(op, CoRegisterOp::Mcr | CoRegisterOp::Mrc) => {
                self.system(op == CoRegisterOp::Mcr, [opc1, crn, crm, opc2], rt)
            }
            Op::FpaDyadic {
                op,
                precision,
                rounding,
                fd,
                fn_,
                fm,
            } => completed(self.fpa.dyadic(op, precision, rounding, fd, fn_, fm)),
            Op::FpaMonadic {
                op,
                precision,
                rounding,
                fd,
                fm,
            } => completed(self.fpa.monadic(op, precision, rounding, fd, fm)),
            Op::FpaRegister {
                op,
                precision,
                rounding,
                fn_,
                fm,
                rd,
            } => self.fpa_register(op, precision, rounding, fn_, fm, rd),
            Op::FpaCompare {
                op, fn_, fm, rd, ..
            } => self.fpa_compare(op, fn_, fm, rd),
            Op::FpaTransfer {
                op,
                format,
                fd,
                rn,
                up,
                index,
                offset,
            } => self.fpa_transfer(bus, op, format, fd, rn, up, index, offset),
            Op::FpaMultiple {
                op,
                fd,
                count,
                rn,
                up,
                index,
                offset,
            } => self.fpa_multiple(bus, op, fd, count, rn, up, index, offset),
            Op::SimdFp(op) if self.profile.isa.has(Feature::Vfp) => self.simdfp(bus, &op),
            Op::SimdFp(_)
            | Op::Coprocessor { .. }
            | Op::CoprocessorRegister { .. }
            | Op::CoprocessorRegisters { .. }
            | Op::CoprocessorTransfer { .. } => {
                self.undefined();
                Flow::Next
            }
        }
    }

    /// The shifter's output for `operand`, and the first operand: R15 one
    /// word further on with a register-shifted operand.
    fn operands(&self, rn: Reg, operand: Operand) -> (u32, Shifted) {
        let carry = self.flag(psr::C);
        match operand {
            Operand::Immediate(immediate) => {
                let value = immediate.value();
                let carry = if immediate.rotation == 0 {
                    carry
                } else {
                    value >> 31 != 0
                };
                (self.base(rn), Shifted { value, carry })
            }
            Operand::Expanded(expanded) => (
                self.base(rn),
                Shifted {
                    value: expanded.value(),
                    carry: expanded.carry(carry),
                },
            ),
            Operand::Literal(value) => (
                self.base(rn),
                Shifted {
                    value: u32::from(value),
                    carry,
                },
            ),
            Operand::Register { rm, shift } => (
                self.read(rn),
                shift_immediate(self.read_with_psr(rm), shift, carry),
            ),
            Operand::Shifted { rm, kind, rs } => {
                let amount = self.read_late(rs, true) & 0xFF;
                let value = self.read_late(rm, true);
                (
                    self.read_late(rn, false),
                    shift_register(value, kind, amount, carry),
                )
            }
        }
    }

    fn data(&mut self, alu: Alu, s: bool, rd: Reg, rn: Reg, operand: Operand) -> Flow {
        let (a, shifted) = self.operands(rn, operand);
        let b = shifted.value;
        let c = self.flag(psr::C);
        let (result, carry, overflow) = match alu {
            Alu::Data(DataOp::And) | Alu::Compare(CompareOp::Tst) => (a & b, shifted.carry, None),
            Alu::Data(DataOp::Eor) | Alu::Compare(CompareOp::Teq) => (a ^ b, shifted.carry, None),
            Alu::Data(DataOp::Orr) => (a | b, shifted.carry, None),
            Alu::Data(DataOp::Bic) => (a & !b, shifted.carry, None),
            Alu::Data(DataOp::Orn) => (a | !b, shifted.carry, None),
            Alu::Move(MoveOp::Mov) => (b, shifted.carry, None),
            Alu::Move(MoveOp::Mvn) => (!b, shifted.carry, None),
            Alu::Data(DataOp::Sub) | Alu::Compare(CompareOp::Cmp) => {
                arithmetic(add_with_carry(a, !b, true))
            }
            Alu::Data(DataOp::Rsb) => arithmetic(add_with_carry(b, !a, true)),
            Alu::Data(DataOp::Add) | Alu::Compare(CompareOp::Cmn) => {
                arithmetic(add_with_carry(a, b, false))
            }
            Alu::Data(DataOp::Adc) => arithmetic(add_with_carry(a, b, c)),
            Alu::Data(DataOp::Sbc) => arithmetic(add_with_carry(a, !b, c)),
            Alu::Data(DataOp::Rsc) => arithmetic(add_with_carry(b, !a, c)),
        };
        if let Alu::Compare(_) = alu {
            if rd == PC && self.profile.modes26() {
                if self.mode.is_26bit() {
                    self.write_r15_psr(result);
                } else if !self.restore_cpsr() {
                    self.set_flags(result, Some(carry), overflow);
                }
            } else {
                self.set_flags(result, Some(carry), overflow);
            }
            return Flow::Next;
        }
        if rd != PC {
            self.r[rd.index()] = result;
            if s {
                self.set_flags(result, Some(carry), overflow);
            }
            return Flow::Next;
        }
        if self.mode.is_26bit() {
            if s {
                self.write_r15_psr(result);
            }
            self.branch(result);
        } else if !s {
            self.alu_pc(result);
        } else if self.restore_cpsr() {
            self.branch(result);
        } else if !self.refuse() {
            self.alu_pc(result);
        }
        Flow::Next
    }

    #[allow(clippy::too_many_arguments)] // One per operand the multiply form has.
    fn multiply(
        &mut self,
        op: MultiplyOp,
        flags: bool,
        [high_n, high_m]: [bool; 2],
        rd: Reg,
        rn: Reg,
        rm: Reg,
        ra: Option<Reg>,
    ) {
        let n = self.read(rn);
        let m = self.read(rm);
        let addend = ra.map_or(0, |ra| self.read(ra));
        let (ns, ms, accumulator) = (
            i64::from(n.cast_signed()),
            i64::from(m.cast_signed()),
            i64::from(addend.cast_signed()),
        );
        let swapped = if high_n { m.rotate_right(16) } else { m };
        let dual = |subtract: bool| -> i64 {
            let low = half(n, false) * half(swapped, false);
            let high = half(n, true) * half(swapped, true);
            if subtract {
                low - high
            } else {
                low + high
            }
        };
        let overflowing = |value: i64| (wrap(value), i32::try_from(value).is_err());
        // The architecture keeps the low 64 bits of the sum and its rounding.
        let most_significant = |product: i64| {
            let rounded = if high_n {
                product.wrapping_add(0x8000_0000)
            } else {
                product
            };
            wrap(rounded >> 32)
        };
        let (result, q) = match op {
            MultiplyOp::Mul => (n.wrapping_mul(m), false),
            MultiplyOp::Mla => (n.wrapping_mul(m).wrapping_add(addend), false),
            MultiplyOp::Mls => (addend.wrapping_sub(n.wrapping_mul(m)), false),
            MultiplyOp::Smla => overflowing(half(n, high_n) * half(m, high_m) + accumulator),
            MultiplyOp::Smlaw => overflowing(((ns * half(m, high_m)) >> 16) + accumulator),
            MultiplyOp::Smulw => (wrap((ns * half(m, high_m)) >> 16), false),
            MultiplyOp::Smul => (wrap(half(n, high_n) * half(m, high_m)), false),
            MultiplyOp::Smlad => overflowing(dual(false) + accumulator),
            MultiplyOp::Smuad => overflowing(dual(false)),
            MultiplyOp::Smlsd => overflowing(dual(true) + accumulator),
            MultiplyOp::Smusd => (wrap(dual(true)), false),
            MultiplyOp::Smmla => (
                most_significant((accumulator << 32).wrapping_add(ns * ms)),
                false,
            ),
            MultiplyOp::Smmul => (most_significant(ns * ms), false),
            MultiplyOp::Smmls => (
                most_significant((accumulator << 32).wrapping_sub(ns * ms)),
                false,
            ),
            MultiplyOp::Usad8 | MultiplyOp::Usada8 => {
                let sum: u32 = n
                    .to_le_bytes()
                    .iter()
                    .zip(m.to_le_bytes())
                    .map(|(&p, q)| u32::from(p.abs_diff(q)))
                    .sum();
                (sum.wrapping_add(addend), false)
            }
            MultiplyOp::Sdiv => (sdiv(n, m), false),
            MultiplyOp::Udiv => (n.checked_div(m).unwrap_or(0), false),
        };
        self.write(rd, result);
        self.saturated(q);
        if flags {
            self.set_flags(result, None, None);
        }
    }

    #[allow(clippy::too_many_arguments)] // One per operand the long form has.
    fn multiply_long(
        &mut self,
        op: LongOp,
        flags: bool,
        [high_n, high_m]: [bool; 2],
        lo: Reg,
        hi: Reg,
        rn: Reg,
        rm: Reg,
    ) {
        let n = self.read(rn);
        let m = self.read(rm);
        let accumulator = (u64::from(self.read(hi)) << 32) | u64::from(self.read(lo));
        let signed = accumulator.cast_signed();
        let swapped = if high_n { m.rotate_right(16) } else { m };
        let dual = |subtract: bool| -> i64 {
            let low = half(n, false) * half(swapped, false);
            let high = half(n, true) * half(swapped, true);
            if subtract {
                low - high
            } else {
                low + high
            }
        };
        let (ns, ms) = (i64::from(n.cast_signed()), i64::from(m.cast_signed()));
        let result = match op {
            LongOp::Umull => u64::from(n) * u64::from(m),
            LongOp::Umlal => (u64::from(n) * u64::from(m)).wrapping_add(accumulator),
            LongOp::Smull => (ns * ms).cast_unsigned(),
            LongOp::Smlal => (ns * ms).wrapping_add(signed).cast_unsigned(),
            LongOp::Umaal => {
                u64::from(n) * u64::from(m) + u64::from(self.read(hi)) + u64::from(self.read(lo))
            }
            LongOp::SmlalHalves => (half(n, high_n) * half(m, high_m))
                .wrapping_add(signed)
                .cast_unsigned(),
            LongOp::Smlald => dual(false).wrapping_add(signed).cast_unsigned(),
            LongOp::Smlsld => dual(true).wrapping_add(signed).cast_unsigned(),
        };
        self.write(lo, low(result));
        self.write(hi, high(result));
        if flags {
            self.cpsr = (self.cpsr & !(psr::N | psr::Z))
                | if result >> 63 != 0 { psr::N } else { 0 }
                | if result == 0 { psr::Z } else { 0 };
        }
    }

    fn arithmetic(&mut self, op: ArithmeticOp, rd: Reg, rn: Reg, rm: Reg) {
        let n = self.read(rn);
        let m = self.read(rm);
        let (ns, ms) = (i64::from(n.cast_signed()), i64::from(m.cast_signed()));
        let doubled = || alu::signed_saturate(2 * ns, 32);
        let (result, q) = match op {
            ArithmeticOp::Qadd => alu::signed_saturate(ms + ns, 32),
            ArithmeticOp::Qsub => alu::signed_saturate(ms - ns, 32),
            ArithmeticOp::Qdadd => {
                let (double, first) = doubled();
                let (sum, second) = alu::signed_saturate(ms + i64::from(double.cast_signed()), 32);
                (sum, first || second)
            }
            ArithmeticOp::Qdsub => {
                let (double, first) = doubled();
                let (sum, second) = alu::signed_saturate(ms - i64::from(double.cast_signed()), 32);
                (sum, first || second)
            }
            ArithmeticOp::Sel => {
                let ge = self.cpsr >> 16;
                let mut result = 0;
                for lane in 0..4 {
                    let source = if ge & (1 << lane) != 0 { n } else { m };
                    result |= source & (0xFF << (8 * lane));
                }
                (result, false)
            }
            ArithmeticOp::Crc32b => (tairix_crc32::update(n, &m.to_le_bytes()[..1]), false),
            ArithmeticOp::Crc32h => (tairix_crc32::update(n, &m.to_le_bytes()[..2]), false),
            ArithmeticOp::Crc32w => (tairix_crc32::update(n, &m.to_le_bytes()), false),
            ArithmeticOp::Crc32cb => (
                tairix_crc32c::portable_update(n, &m.to_le_bytes()[..1]),
                false,
            ),
            ArithmeticOp::Crc32ch => (
                tairix_crc32c::portable_update(n, &m.to_le_bytes()[..2]),
                false,
            ),
            ArithmeticOp::Crc32cw => (tairix_crc32c::portable_update(n, &m.to_le_bytes()), false),
        };
        self.write(rd, result);
        self.saturated(q);
    }

    fn saturate(&mut self, op: SaturateOp, rd: Reg, saturate: u8, rn: Reg, shift: Shift) {
        let n = self.read(rn);
        let bits = u32::from(saturate);
        let (result, q) = match op {
            SaturateOp::Ssat => {
                let operand = shift_immediate(n, shift, false).value.cast_signed();
                alu::signed_saturate(i64::from(operand), bits + 1)
            }
            SaturateOp::Usat => {
                let operand = shift_immediate(n, shift, false).value.cast_signed();
                alu::unsigned_saturate(i64::from(operand), bits)
            }
            SaturateOp::Ssat16 | SaturateOp::Usat16 => {
                let mut result = 0;
                let mut any = false;
                for top in [false, true] {
                    let (lane, saturated) = if op == SaturateOp::Ssat16 {
                        alu::signed_saturate(half(n, top), bits + 1)
                    } else {
                        alu::unsigned_saturate(half(n, top), bits)
                    };
                    result |= (lane & 0xFFFF) << if top { 16 } else { 0 };
                    any |= saturated;
                }
                (result, any)
            }
        };
        self.write(rd, result);
        self.saturated(q);
    }

    fn extend(&mut self, op: ExtendOp, rd: Reg, rn: Option<Reg>, rm: Reg, rotation: u8) {
        let rotated = self.read(rm).rotate_right(8 * u32::from(rotation));
        let addend = rn.map_or(0, |rn| self.read(rn));
        let pair = |lane: fn(u32) -> u32| -> u32 {
            let low = lane(rotated).wrapping_add(addend) & 0xFFFF;
            let high = lane(rotated >> 16).wrapping_add(addend >> 16) & 0xFFFF;
            low | (high << 16)
        };
        let result = match op {
            ExtendOp::Sxtab | ExtendOp::Sxtb => sext8(rotated).wrapping_add(addend),
            ExtendOp::Sxtah | ExtendOp::Sxth => sext16(rotated).wrapping_add(addend),
            ExtendOp::Uxtab | ExtendOp::Uxtb => (rotated & 0xFF).wrapping_add(addend),
            ExtendOp::Uxtah | ExtendOp::Uxth => (rotated & 0xFFFF).wrapping_add(addend),
            ExtendOp::Sxtab16 | ExtendOp::Sxtb16 => pair(sext8),
            ExtendOp::Uxtab16 | ExtendOp::Uxtb16 => pair(|value| value & 0xFF),
        };
        self.write(rd, result);
    }

    fn bitfield(&mut self, op: BitfieldOp, rd: Reg, rn: Option<Reg>, lsb: u8, top: u8) {
        let (lsb, top) = (u32::from(lsb), u32::from(top));
        let source = rn.map_or(0, |rn| self.read(rn));
        let result = match op {
            BitfieldOp::Sbfx | BitfieldOp::Ubfx => {
                let width = top + 1;
                let field = (u64::from(source) >> lsb) & ((1u64 << width) - 1);
                if op == BitfieldOp::Sbfx && width < 32 && field >> (width - 1) != 0 {
                    low(field | (u64::MAX << width))
                } else {
                    low(field)
                }
            }
            BitfieldOp::Bfi | BitfieldOp::Bfc => {
                if top < lsb {
                    self.read(rd)
                } else {
                    let mask = low(((1u64 << (top - lsb + 1)) - 1) << lsb);
                    let inserted = if op == BitfieldOp::Bfi {
                        source << lsb
                    } else {
                        0
                    };
                    (self.read(rd) & !mask) | (inserted & mask)
                }
            }
        };
        self.write(rd, result);
    }

    fn hint(&mut self, op: HintOp) -> Flow {
        match op {
            HintOp::Wfi if self.lines == 0 => Flow::Stop(Stop::WaitForInterrupt),
            HintOp::Wfe if !self.event => Flow::Stop(Stop::WaitForEvent),
            HintOp::Wfe => {
                self.event = false;
                Flow::Next
            }
            HintOp::Sev | HintOp::Sevl => {
                self.event = true;
                Flow::Next
            }
            _ => Flow::Next,
        }
    }

    fn change_state(&mut self, imod: u8, change_mode: bool, interrupts: u8, mode: u8) {
        if !self.mode.privileged() {
            return;
        }
        let mask = u32::from(interrupts & 0b111) << 6;
        let mut cpsr = self.cpsr;
        match imod {
            0b10 => cpsr &= !mask,
            0b11 => cpsr |= mask,
            _ => {}
        }
        let mut bytes = psr::A | psr::I | psr::F;
        if change_mode {
            cpsr = (cpsr & !psr::M) | u32::from(mode);
            bytes |= psr::M;
            if self.valid_mode(cpsr).is_none() && self.refuse() {
                return;
            }
        }
        self.write_cpsr(cpsr, bytes, false);
    }

    /// An `MCR` or `MRC` of CP15.
    fn system(&mut self, write: bool, register: [u8; 4], rt: Reg) -> Flow {
        let privileged = self.mode.privileged();
        let outcome = if write {
            let value = self.read(rt);
            self.cp15.write(register, value, privileged)
        } else {
            self.cp15.read(register, privileged)
        };
        match outcome {
            Outcome::Done(value) => {
                if !write {
                    if rt == PC {
                        self.cpsr = (self.cpsr & !psr::NZCV) | (value & psr::NZCV);
                    } else {
                        self.r[rt.index()] = value;
                    }
                }
                Flow::Next
            }
            Outcome::Wait if self.lines == 0 => Flow::Stop(Stop::WaitForInterrupt),
            Outcome::Wait => Flow::Next,
            Outcome::Undefined => {
                self.undefined();
                Flow::Next
            }
            Outcome::Unmodelled => {
                Flow::Refuse(Stop::Unmodelled(Unmodelled::System { register, write }))
            }
        }
    }
}

/// An arithmetic result and its flags.
const fn arithmetic((result, carry, overflow): (u32, bool, bool)) -> (u32, bool, Option<bool>) {
    (result, carry, Some(overflow))
}

/// Signed division as ARMv7 defines it: by zero gives zero, and the one
/// overflowing quotient wraps.
fn sdiv(n: u32, m: u32) -> u32 {
    let (n, m) = (n.cast_signed(), m.cast_signed());
    if m == 0 {
        0
    } else {
        n.wrapping_div(m).cast_unsigned()
    }
}
