//! The executor: one decoded instruction against the registers and the bus.

use super::alu::{self, width, BYTE, WORD};
use super::bus::Escape;
use super::decode::{Alu, Cond, Count, Ea, Insn, Mem, Op, Rep, Shift, Unary};
use super::model::Family;
use super::regs::{flag, gpr, Regs, Seg};
use super::timing::{self, Costs};
use super::{linear, Bus, Cpu, Flow, Wait};
use crate::bits::{hi, high16, lo, low16, sext8, word};

/// Clocks between a waiting WAIT's looks at TEST.
const WAIT_LOOK: u64 = 5;

/// Where an instruction's `r/m` operand lives.
#[derive(Clone, Copy, Debug)]
pub(super) enum Loc {
    Reg(u8),
    Mem(Seg, u16),
}

/// The string instructions, in the order [`Costs::rep_setup`] lists them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StringOp {
    Movs,
    Cmps,
    Scas,
    Lods,
    Stos,
    Ins,
    Outs,
}

impl Cpu {
    fn costs(&self) -> &'static Costs {
        timing::costs(self.model.family())
    }

    fn family(&self) -> Family {
        self.model.family()
    }

    pub(super) fn reg_value(&self, reg: u8, w: bool) -> u16 {
        if w {
            self.regs.get16(reg)
        } else {
            u16::from(self.regs.get8(reg))
        }
    }

    pub(super) fn set_reg(&mut self, reg: u8, w: bool, value: u16) {
        if w {
            self.regs.set16(reg, value);
        } else {
            self.regs.set8(reg, lo(value));
        }
    }

    pub(super) fn segment_for(&self, mem: Mem) -> Seg {
        self.prefixes.segment.unwrap_or(mem.base.default_segment())
    }

    pub(super) fn offset_of(&self, mem: Mem) -> u16 {
        use super::decode::Base;
        let r = &self.regs;
        let base = match mem.base {
            Base::BxSi => r.gpr[gpr::BX].wrapping_add(r.gpr[gpr::SI]),
            Base::BxDi => r.gpr[gpr::BX].wrapping_add(r.gpr[gpr::DI]),
            Base::BpSi => r.gpr[gpr::BP].wrapping_add(r.gpr[gpr::SI]),
            Base::BpDi => r.gpr[gpr::BP].wrapping_add(r.gpr[gpr::DI]),
            Base::Si => r.gpr[gpr::SI],
            Base::Di => r.gpr[gpr::DI],
            Base::Bp => r.gpr[gpr::BP],
            Base::Bx => r.gpr[gpr::BX],
            Base::Absolute => 0,
        };
        base.wrapping_add(mem.disp)
    }

    /// Resolves the `r/m` operand, charging the 8086 family's address
    /// calculation.
    pub(super) fn locate(&mut self, insn: &Insn) -> Loc {
        match insn.ea {
            Ea::Reg(r) => Loc::Reg(r),
            Ea::Mem(mem) => {
                let offset = self.offset_of(mem);
                self.ind = offset;
                self.tick(timing::ea(self.family(), mem));
                Loc::Mem(self.segment_for(mem), offset)
            }
        }
    }

    pub(super) fn load<B: Bus>(&mut self, bus: &mut B, loc: Loc, w: bool) -> u16 {
        match loc {
            Loc::Reg(r) => self.reg_value(r, w),
            Loc::Mem(seg, offset) => {
                let value = if w {
                    self.read16(bus, seg, offset)
                } else {
                    u16::from(self.read8(bus, seg, offset))
                };
                self.opr = value;
                value
            }
        }
    }

    pub(super) fn store<B: Bus>(&mut self, bus: &mut B, loc: Loc, w: bool, value: u16) {
        match loc {
            Loc::Reg(r) => self.set_reg(r, w, value),
            Loc::Mem(seg, offset) => {
                self.opr = value;
                if w {
                    self.write16(bus, seg, offset, value);
                } else {
                    self.write8(bus, seg, offset, lo(value));
                }
            }
        }
    }

    fn acc(&self, w: bool) -> u16 {
        self.reg_value(0, w)
    }

    fn set_acc(&mut self, w: bool, value: u16) {
        self.set_reg(0, w, value);
    }

    fn cost_rm(&mut self, loc: Loc, reg: u16, mem: u16) {
        self.tick(u32::from(if matches!(loc, Loc::Reg(_)) {
            reg
        } else {
            mem
        }));
    }

    fn condition(&self, cond: Cond) -> bool {
        let f = |m| self.regs.flag(m);
        match cond {
            Cond::O => f(flag::OF),
            Cond::No => !f(flag::OF),
            Cond::B => f(flag::CF),
            Cond::Nb => !f(flag::CF),
            Cond::E => f(flag::ZF),
            Cond::Ne => !f(flag::ZF),
            Cond::Be => f(flag::CF) || f(flag::ZF),
            Cond::A => !(f(flag::CF) || f(flag::ZF)),
            Cond::S => f(flag::SF),
            Cond::Ns => !f(flag::SF),
            Cond::P => f(flag::PF),
            Cond::Np => !f(flag::PF),
            Cond::L => f(flag::SF) != f(flag::OF),
            Cond::Ge => f(flag::SF) == f(flag::OF),
            Cond::Le => f(flag::ZF) || f(flag::SF) != f(flag::OF),
            Cond::G => !f(flag::ZF) && f(flag::SF) == f(flag::OF),
        }
    }

    fn jump_relative(&mut self, rel: u16) -> Flow {
        self.regs.ip = self.regs.ip.wrapping_add(rel);
        Flow::Branch
    }

    /// Loads a segment register, inhibiting interrupts for one instruction
    /// where the model does: after any segment load on the 8086 family, after
    /// SS elsewhere.
    fn load_segment<B: Bus>(&mut self, bus: &mut B, seg: Seg, value: u16) {
        if seg == Seg::Cs {
            self.carry_queue_into_new_cs(bus, value);
        }
        self.regs.set_sreg(seg, value);
        if seg == Seg::Ss || matches!(self.family(), Family::Intel8086) {
            self.inhibit = true;
        }
    }

    /// A CS load that is not a jump leaves the queue full of the old code
    /// path, which keeps executing until it drains.
    fn carry_queue_into_new_cs<B: Bus>(&mut self, bus: &mut B, new_cs: u16) {
        let old_cs = self.regs.sreg(Seg::Cs);
        let ip = self.regs.ip;
        let len = self.queue.len();
        let mut queued = [0u8; 6];
        for (i, slot) in queued.iter_mut().enumerate().take(usize::from(len)) {
            let offset = ip.wrapping_add(u16::try_from(i).unwrap_or(0));
            let addr = linear(old_cs, offset);
            *slot = self.queue.take(addr).unwrap_or_else(|| bus.peek(addr));
        }
        self.queue.flush();
        for (i, &byte) in queued.iter().enumerate().take(usize::from(len)) {
            let offset = ip.wrapping_add(u16::try_from(i).unwrap_or(0));
            self.queue.hold(linear(new_cs, offset), byte);
        }
    }

    fn load_far_pointer<B: Bus>(&mut self, bus: &mut B, insn: &Insn) -> (u16, u16) {
        match self.locate(insn) {
            Loc::Mem(seg, offset) => {
                let off = self.read16(bus, seg, offset);
                let segment = self.read16(bus, seg, offset.wrapping_add(2));
                (off, segment)
            }
            // A register operand runs no address calculation: the 8086 reuses
            // the operand and address it last held.
            Loc::Reg(_) => {
                let seg = self.prefixes.segment.unwrap_or(Seg::Ds);
                let segment = self.read16(bus, seg, self.ind.wrapping_add(2));
                (self.opr, segment)
            }
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one arm per instruction form reads as the opcode map"
    )]
    pub(super) fn execute<B: Bus>(
        &mut self,
        bus: &mut B,
        insn: &Insn,
        ip: u16,
        limit: u64,
    ) -> Flow {
        let w = insn.w;
        let width = width(w);
        let cost = self.costs();
        match insn.op {
            Op::Prefix(_) => Flow::Next,
            Op::AluRmReg(op) => {
                let loc = self.locate(insn);
                let a = self.load(bus, loc, w);
                let b = self.reg_value(insn.reg, w);
                let r = alu::alu(&mut self.regs.flags, op, a, b, width);
                if op != Alu::Cmp {
                    self.store(bus, loc, w, r);
                }
                let mem = if op == Alu::Cmp {
                    cost.cmp_mr
                } else {
                    cost.alu_mr
                };
                self.cost_rm(loc, cost.alu_rr, mem);
                Flow::Next
            }
            Op::AluRegRm(op) => {
                let loc = self.locate(insn);
                let b = self.load(bus, loc, w);
                let a = self.reg_value(insn.reg, w);
                let r = alu::alu(&mut self.regs.flags, op, a, b, width);
                if op != Alu::Cmp {
                    self.set_reg(insn.reg, w, r);
                }
                self.cost_rm(loc, cost.alu_rr, cost.alu_rm);
                Flow::Next
            }
            Op::AluAccImm(op) => {
                let a = self.acc(w);
                let r = alu::alu(&mut self.regs.flags, op, a, insn.imm, width);
                if op != Alu::Cmp {
                    self.set_acc(w, r);
                }
                self.tick(u32::from(cost.alu_ai));
                Flow::Next
            }
            Op::AluRmImm(op) => {
                let loc = self.locate(insn);
                let a = self.load(bus, loc, w);
                let r = alu::alu(&mut self.regs.flags, op, a, insn.imm, width);
                if op != Alu::Cmp {
                    self.store(bus, loc, w, r);
                }
                let mem = if op == Alu::Cmp {
                    cost.cmp_mi
                } else {
                    cost.alu_mi
                };
                self.cost_rm(loc, cost.alu_ri, mem);
                Flow::Next
            }
            Op::PushSeg(seg) => {
                self.push(bus, self.regs.sreg(seg));
                self.tick(u32::from(cost.push_seg));
                Flow::Next
            }
            Op::PopSeg(seg) => {
                let value = self.pop(bus);
                self.load_segment(bus, seg, value);
                self.tick(u32::from(cost.pop_seg));
                Flow::Next
            }
            Op::Daa => {
                let al = self.regs.get8(0);
                let r = alu::daa(&mut self.regs.flags, al);
                self.regs.set8(0, r);
                self.tick(u32::from(cost.daa));
                Flow::Next
            }
            Op::Das => {
                let al = self.regs.get8(0);
                let r = alu::das(&mut self.regs.flags, al);
                self.regs.set8(0, r);
                self.tick(u32::from(cost.das));
                Flow::Next
            }
            Op::Aaa => {
                let ax = self.regs.ax();
                let r = alu::aaa(&mut self.regs.flags, ax);
                self.regs.gpr[gpr::AX] = r;
                self.tick(u32::from(cost.aaa));
                Flow::Next
            }
            Op::Aas => {
                let ax = self.regs.ax();
                let r = alu::aas(&mut self.regs.flags, ax);
                self.regs.gpr[gpr::AX] = r;
                self.tick(u32::from(cost.aas));
                Flow::Next
            }
            Op::IncReg => {
                let value = self.regs.get16(insn.reg);
                let r = alu::inc(&mut self.regs.flags, value, WORD);
                self.regs.set16(insn.reg, r);
                self.tick(u32::from(cost.inc_reg16));
                Flow::Next
            }
            Op::DecReg => {
                let value = self.regs.get16(insn.reg);
                let r = alu::dec(&mut self.regs.flags, value, WORD);
                self.regs.set16(insn.reg, r);
                self.tick(u32::from(cost.inc_reg16));
                Flow::Next
            }
            Op::PushReg => {
                // The 8086 and its contemporaries push SP after decrementing it.
                let value = if usize::from(insn.reg) == gpr::SP {
                    self.regs.sp().wrapping_sub(2)
                } else {
                    self.regs.get16(insn.reg)
                };
                self.push(bus, value);
                self.tick(u32::from(cost.push_reg));
                Flow::Next
            }
            Op::PopReg => {
                let value = self.pop(bus);
                self.regs.set16(insn.reg, value);
                self.tick(u32::from(cost.pop_reg));
                Flow::Next
            }
            Op::Jcc(cond) => {
                if self.condition(cond) {
                    self.tick(u32::from(cost.jcc_taken));
                    self.jump_relative(insn.imm)
                } else {
                    self.tick(u32::from(cost.jcc_not));
                    Flow::Next
                }
            }
            Op::TestRmReg => {
                let loc = self.locate(insn);
                let a = self.load(bus, loc, w);
                let b = self.reg_value(insn.reg, w);
                alu::logic(&mut self.regs.flags, a & b, width);
                self.cost_rm(loc, cost.test_rr, cost.test_mr);
                Flow::Next
            }
            Op::XchgRmReg => {
                let loc = self.locate(insn);
                let a = self.load(bus, loc, w);
                let b = self.reg_value(insn.reg, w);
                self.store(bus, loc, w, b);
                self.set_reg(insn.reg, w, a);
                self.cost_rm(loc, cost.xchg_rr, cost.xchg_mem);
                Flow::Next
            }
            Op::MovRmReg => {
                let loc = self.locate(insn);
                let value = self.reg_value(insn.reg, w);
                self.store(bus, loc, w, value);
                self.cost_rm(loc, cost.mov_rr, cost.mov_mr);
                Flow::Next
            }
            Op::MovRegRm => {
                let loc = self.locate(insn);
                let value = self.load(bus, loc, w);
                self.set_reg(insn.reg, w, value);
                self.cost_rm(loc, cost.mov_rr, cost.mov_rm);
                Flow::Next
            }
            Op::MovRmSeg => {
                let loc = self.locate(insn);
                let value = self.regs.sreg(Seg::from_field(insn.reg));
                self.store(bus, loc, true, value);
                self.cost_rm(loc, cost.mov_r_seg, cost.mov_m_seg);
                Flow::Next
            }
            Op::Lea => {
                let value = match insn.ea {
                    Ea::Mem(mem) => {
                        let offset = self.offset_of(mem);
                        self.ind = offset;
                        self.tick(timing::ea(self.family(), mem));
                        offset
                    }
                    Ea::Reg(_) => self.ind,
                };
                self.regs.set16(insn.reg, value);
                self.tick(u32::from(cost.lea));
                Flow::Next
            }
            Op::MovSegRm => {
                let loc = self.locate(insn);
                let value = self.load(bus, loc, true);
                self.load_segment(bus, Seg::from_field(insn.reg), value);
                self.cost_rm(loc, cost.mov_seg_r, cost.mov_seg_m);
                Flow::Next
            }
            Op::PopRm => {
                let value = self.pop(bus);
                let loc = self.locate(insn);
                self.store(bus, loc, true, value);
                self.cost_rm(loc, cost.pop_reg, cost.pop_mem);
                Flow::Next
            }
            Op::XchgAx => {
                let a = self.regs.ax();
                let b = self.regs.get16(insn.reg);
                self.regs.gpr[gpr::AX] = b;
                self.regs.set16(insn.reg, a);
                self.tick(u32::from(if insn.reg == 0 {
                    cost.nop
                } else {
                    cost.xchg_acc
                }));
                Flow::Next
            }
            Op::Cbw => {
                self.regs.gpr[gpr::AX] = sext8(self.regs.get8(0));
                self.tick(u32::from(cost.cbw));
                Flow::Next
            }
            Op::Cwd => {
                self.regs.gpr[gpr::DX] = if self.regs.ax() & 0x8000 != 0 {
                    0xFFFF
                } else {
                    0
                };
                self.tick(u32::from(cost.cwd));
                Flow::Next
            }
            Op::CallFar => {
                self.push(bus, self.regs.sreg(Seg::Cs));
                self.push(bus, self.regs.ip);
                self.regs.set_sreg(Seg::Cs, insn.imm2);
                self.regs.ip = insn.imm;
                self.tick(u32::from(cost.call_far));
                Flow::Branch
            }
            Op::Wait => self.wait(
                bus,
                Wait {
                    began: self.cycles,
                    ip,
                },
            ),
            Op::Pushf => {
                self.push(bus, self.regs.flags);
                self.tick(u32::from(cost.pushf));
                Flow::Next
            }
            Op::Popf => {
                let value = self.pop(bus);
                self.regs.flags = self.normalize_flags(value, false);
                self.tick(u32::from(cost.popf));
                Flow::Next
            }
            Op::Sahf => {
                let ah = u16::from(self.regs.get8(4));
                let mask = flag::SF | flag::ZF | flag::AF | flag::PF | flag::CF;
                self.regs.flags = (self.regs.flags & !mask) | (ah & mask);
                self.tick(u32::from(cost.sahf));
                Flow::Next
            }
            Op::Lahf => {
                self.regs.set8(4, lo(self.regs.flags));
                self.tick(u32::from(cost.lahf));
                Flow::Next
            }
            Op::MovAccMem => {
                let seg = self.prefixes.segment.unwrap_or(Seg::Ds);
                let value = if w {
                    self.read16(bus, seg, insn.imm)
                } else {
                    u16::from(self.read8(bus, seg, insn.imm))
                };
                self.set_acc(w, value);
                self.tick(u32::from(cost.mov_acc_mem));
                Flow::Next
            }
            Op::MovMemAcc => {
                let seg = self.prefixes.segment.unwrap_or(Seg::Ds);
                let value = self.acc(w);
                if w {
                    self.write16(bus, seg, insn.imm, value);
                } else {
                    self.write8(bus, seg, insn.imm, lo(value));
                }
                self.tick(u32::from(cost.mov_mem_acc));
                Flow::Next
            }
            Op::Movs => self.string(bus, insn, ip, limit, StringOp::Movs),
            Op::Cmps => self.string(bus, insn, ip, limit, StringOp::Cmps),
            Op::Scas => self.string(bus, insn, ip, limit, StringOp::Scas),
            Op::Lods => self.string(bus, insn, ip, limit, StringOp::Lods),
            Op::Stos => self.string(bus, insn, ip, limit, StringOp::Stos),
            Op::Ins => self.string(bus, insn, ip, limit, StringOp::Ins),
            Op::Outs => self.string(bus, insn, ip, limit, StringOp::Outs),
            Op::TestAccImm => {
                let a = self.acc(w);
                alu::logic(&mut self.regs.flags, a & insn.imm, width);
                self.tick(u32::from(cost.test_ai));
                Flow::Next
            }
            Op::MovRegImm => {
                self.set_reg(insn.reg, w, insn.imm);
                self.tick(u32::from(cost.mov_ri));
                Flow::Next
            }
            Op::Ret { pops } => {
                self.regs.ip = self.pop(bus);
                if pops {
                    self.regs.gpr[gpr::SP] = self.regs.sp().wrapping_add(insn.imm);
                }
                self.tick(u32::from(if pops { cost.ret_imm } else { cost.ret }));
                Flow::Branch
            }
            Op::Les | Op::Lds => {
                let (offset, segment) = self.load_far_pointer(bus, insn);
                self.regs.set16(insn.reg, offset);
                let seg = if insn.op == Op::Les { Seg::Es } else { Seg::Ds };
                self.regs.set_sreg(seg, segment);
                self.tick(u32::from(cost.lds));
                Flow::Next
            }
            Op::MovRmImm => {
                let loc = self.locate(insn);
                self.store(bus, loc, w, insn.imm);
                self.cost_rm(loc, cost.mov_ri, cost.mov_mi);
                Flow::Next
            }
            Op::Retf { pops } => {
                self.regs.ip = self.pop(bus);
                let cs = self.pop(bus);
                self.regs.set_sreg(Seg::Cs, cs);
                if pops {
                    self.regs.gpr[gpr::SP] = self.regs.sp().wrapping_add(insn.imm);
                }
                self.tick(u32::from(if pops { cost.retf_imm } else { cost.retf }));
                Flow::Branch
            }
            Op::Int3 => {
                self.tick(u32::from(cost.int3));
                Flow::Trap(3)
            }
            Op::Int => {
                self.tick(u32::from(cost.int));
                Flow::Trap(lo(insn.imm))
            }
            Op::Into => {
                if self.regs.flag(flag::OF) {
                    self.tick(u32::from(cost.into_taken));
                    Flow::Trap(4)
                } else {
                    self.tick(u32::from(cost.into_not));
                    Flow::Next
                }
            }
            Op::Iret => {
                self.regs.ip = self.pop(bus);
                let cs = self.pop(bus);
                self.regs.set_sreg(Seg::Cs, cs);
                let popped = self.pop(bus);
                // MD stays locked until BRKEM unlocks it and RETEM locks it
                // again, so an IRET re-enters 8080 mode only in between.
                let unlocked = self.mode == super::Mode::NativeUnlocked;
                self.regs.flags = self.normalize_flags(popped, unlocked);
                if unlocked && self.regs.flags & flag::MD == 0 {
                    self.mode = super::Mode::Emulation;
                }
                bus.interrupt_return();
                self.tick(u32::from(cost.iret));
                Flow::Branch
            }
            Op::Shift(op, count) => self.shift(bus, insn, op, count),
            Op::Aam => {
                let base = lo(insn.imm);
                self.tick(u32::from(cost.aam));
                // The Intel parts raise a divide error where the V-series
                // divides by zero to an all-ones quotient.
                if base == 0 && self.family() != Family::Nec {
                    self.tick(u32::from(cost.int));
                    return Flow::Trap(0);
                }
                let family = self.family();
                aam(&mut self.regs, family, base);
                Flow::Next
            }
            Op::Aad => {
                let family = self.family();
                aad(&mut self.regs, family, lo(insn.imm));
                self.tick(u32::from(cost.aad));
                Flow::Next
            }
            Op::Salc => {
                self.regs
                    .set8(0, if self.regs.flag(flag::CF) { 0xFF } else { 0 });
                self.tick(u32::from(cost.flag_op));
                Flow::Next
            }
            Op::Xlat => {
                let seg = self.prefixes.segment.unwrap_or(Seg::Ds);
                let offset = self.regs.gpr[gpr::BX].wrapping_add(u16::from(self.regs.get8(0)));
                let value = self.read8(bus, seg, offset);
                self.regs.set8(0, value);
                self.tick(u32::from(cost.xlat));
                if insn.opcode == 0xD6 {
                    self.tick(super::nec::XLAT_ALIAS_IDLE);
                }
                Flow::Next
            }
            Op::Esc => self.escape(bus, insn, ip),
            Op::Loopne | Op::Loope | Op::Loop => {
                let cx = self.regs.cx().wrapping_sub(1);
                self.regs.gpr[gpr::CX] = cx;
                let zf = self.regs.flag(flag::ZF);
                let (taken, yes, no) = match insn.op {
                    Op::Loopne => (cx != 0 && !zf, cost.loopne_taken, cost.loopne_not),
                    Op::Loope => (cx != 0 && zf, cost.loope_taken, cost.loope_not),
                    _ => (cx != 0, cost.loop_taken, cost.loop_not),
                };
                if taken {
                    self.tick(u32::from(yes));
                    self.jump_relative(insn.imm)
                } else {
                    self.tick(u32::from(no));
                    Flow::Next
                }
            }
            Op::Jcxz => {
                if self.regs.cx() == 0 {
                    self.tick(u32::from(cost.jcxz_taken));
                    self.jump_relative(insn.imm)
                } else {
                    self.tick(u32::from(cost.jcxz_not));
                    Flow::Next
                }
            }
            Op::InImm => {
                let value = self.port_in(bus, insn.imm, w);
                self.set_acc(w, value);
                self.tick(u32::from(cost.in_imm));
                Flow::Next
            }
            Op::OutImm => {
                self.port_out(bus, insn.imm, w, self.acc(w));
                self.tick(u32::from(cost.out_imm));
                Flow::Next
            }
            Op::InDx => {
                let value = self.port_in(bus, self.regs.gpr[gpr::DX], w);
                self.set_acc(w, value);
                self.tick(u32::from(cost.in_dx));
                Flow::Next
            }
            Op::OutDx => {
                self.port_out(bus, self.regs.gpr[gpr::DX], w, self.acc(w));
                self.tick(u32::from(cost.out_dx));
                Flow::Next
            }
            Op::CallNear => {
                self.push(bus, self.regs.ip);
                self.tick(u32::from(cost.call_near));
                self.jump_relative(insn.imm)
            }
            Op::JmpNear => {
                self.tick(u32::from(cost.jmp_near));
                self.jump_relative(insn.imm)
            }
            Op::JmpShort => {
                self.tick(u32::from(cost.jmp_short));
                self.jump_relative(insn.imm)
            }
            Op::JmpFar => {
                self.regs.set_sreg(Seg::Cs, insn.imm2);
                self.regs.ip = insn.imm;
                self.tick(u32::from(cost.jmp_far));
                Flow::Branch
            }
            Op::Hlt => {
                self.tick(u32::from(cost.hlt));
                Flow::Halt
            }
            Op::Cmc => self.flag_op(cost, flag::CF, !self.regs.flag(flag::CF)),
            Op::Clc => self.flag_op(cost, flag::CF, false),
            Op::Stc => self.flag_op(cost, flag::CF, true),
            Op::Cli => self.flag_op(cost, flag::IF, false),
            Op::Sti => {
                if !self.regs.flag(flag::IF) {
                    self.inhibit = true;
                }
                self.flag_op(cost, flag::IF, true)
            }
            Op::Cld => self.flag_op(cost, flag::DF, false),
            Op::Std => self.flag_op(cost, flag::DF, true),
            Op::Unary(op) => self.unary(bus, insn, op),
            Op::IncRm | Op::DecRm => {
                let loc = self.locate(insn);
                let value = self.load(bus, loc, w);
                let r = if insn.op == Op::IncRm {
                    alu::inc(&mut self.regs.flags, value, width)
                } else {
                    alu::dec(&mut self.regs.flags, value, width)
                };
                self.store(bus, loc, w, r);
                self.cost_rm(loc, cost.inc_r, cost.inc_m);
                Flow::Next
            }
            Op::CallRm => {
                let loc = self.locate(insn);
                let target = self.load(bus, loc, w);
                self.push(bus, self.regs.ip);
                self.regs.ip = target;
                self.cost_rm(loc, cost.call_r, cost.call_m);
                Flow::Branch
            }
            Op::CallFarRm | Op::JmpFarRm if self.halts_on_register_far(insn) => Flow::Halt,
            Op::CallFarRm => {
                let (offset, segment) = self.load_far_pointer(bus, insn);
                self.push(bus, self.regs.sreg(Seg::Cs));
                self.push(bus, self.regs.ip);
                self.regs.set_sreg(Seg::Cs, segment);
                self.regs.ip = offset;
                self.tick(u32::from(cost.call_far_m));
                Flow::Branch
            }
            Op::JmpRm => {
                let loc = self.locate(insn);
                self.regs.ip = self.load(bus, loc, w);
                self.cost_rm(loc, cost.jmp_r, cost.jmp_m);
                Flow::Branch
            }
            Op::JmpFarRm => {
                let (offset, segment) = self.load_far_pointer(bus, insn);
                self.regs.set_sreg(Seg::Cs, segment);
                self.regs.ip = offset;
                self.tick(u32::from(cost.jmp_far_m));
                Flow::Branch
            }
            Op::PushRm => {
                let loc = self.locate(insn);
                let value = self.load(bus, loc, w);
                let value = if matches!(loc, Loc::Reg(r) if usize::from(r) == gpr::SP) && w {
                    value.wrapping_sub(2)
                } else {
                    value
                };
                self.push(bus, value);
                self.cost_rm(loc, cost.push_reg, cost.push_mem);
                Flow::Next
            }
            Op::Pusha => {
                let sp = self.regs.sp();
                for r in [gpr::AX, gpr::CX, gpr::DX, gpr::BX] {
                    self.push(bus, self.regs.gpr[r]);
                }
                self.push(bus, sp);
                for r in [gpr::BP, gpr::SI, gpr::DI] {
                    self.push(bus, self.regs.gpr[r]);
                }
                self.tick(u32::from(cost.pusha));
                Flow::Next
            }
            Op::Popa => {
                for r in [gpr::DI, gpr::SI, gpr::BP] {
                    self.regs.gpr[r] = self.pop(bus);
                }
                self.pop(bus);
                for r in [gpr::BX, gpr::DX, gpr::CX, gpr::AX] {
                    self.regs.gpr[r] = self.pop(bus);
                }
                self.tick(u32::from(cost.popa));
                Flow::Next
            }
            Op::Bound => {
                let (lower, upper) = self.load_far_pointer(bus, insn);
                let index = self.regs.get16(insn.reg).cast_signed();
                self.tick(u32::from(cost.bound));
                if index >= lower.cast_signed() && index <= upper.cast_signed() {
                    Flow::Next
                } else {
                    self.tick(u32::from(cost.int));
                    if self.family() == Family::Nec {
                        // The V-series returns past BOUND, so its handler
                        // cannot retry it.
                        Flow::Trap(5)
                    } else {
                        Flow::Fault(5)
                    }
                }
            }
            Op::PushImm => {
                self.push(bus, insn.imm);
                self.tick(u32::from(cost.push_imm));
                Flow::Next
            }
            Op::ImulImm => {
                let loc = self.locate(insn);
                let a = self.load(bus, loc, true);
                let r = imul_imm(&mut self.regs, a, insn.imm);
                self.regs.set16(insn.reg, r);
                self.cost_rm(loc, cost.imul_imm_r, cost.imul_imm_m);
                Flow::Next
            }
            Op::Enter => {
                let level = lo(insn.imm2) & 0x1F;
                let bp = self.regs.gpr[gpr::BP];
                self.push(bus, bp);
                let frame = self.regs.sp();
                if level > 0 {
                    let mut walk = bp;
                    for _ in 1..level {
                        walk = walk.wrapping_sub(2);
                        let value = self.read16(bus, Seg::Ss, walk);
                        self.push(bus, value);
                    }
                    self.push(bus, frame);
                }
                self.regs.gpr[gpr::BP] = frame;
                self.regs.gpr[gpr::SP] = self.regs.sp().wrapping_sub(insn.imm);
                let cost = match level {
                    0 => u32::from(cost.enter0),
                    1 => u32::from(cost.enter1),
                    n => u32::from(cost.enter1) + u32::from(cost.enter_each) * u32::from(n - 1),
                };
                self.tick(cost);
                Flow::Next
            }
            Op::Leave => {
                self.regs.gpr[gpr::SP] = self.regs.gpr[gpr::BP];
                self.regs.gpr[gpr::BP] = self.pop(bus);
                self.tick(u32::from(cost.leave));
                Flow::Next
            }
            Op::Nec(op) => self.nec(bus, insn, op),
            Op::Nop => {
                if let Loc::Mem(seg, offset) = self.locate(insn) {
                    self.read16(bus, seg, offset);
                }
                self.tick(super::nec::UNDEFINED_63_IDLE);
                Flow::Next
            }
            Op::Invalid => {
                self.tick(u32::from(cost.int));
                Flow::Fault(6)
            }
            Op::HostCall => Flow::Host(insn.imm),
        }
    }

    /// The V-series stops dead on the byte forms of far CALL and JMP with a
    /// register operand.
    fn halts_on_register_far(&self, insn: &Insn) -> bool {
        self.family() == Family::Nec && !insn.w && matches!(insn.ea, Ea::Reg(_))
    }

    fn flag_op(&mut self, cost: &Costs, mask: u16, on: bool) -> Flow {
        self.regs.set_flag(mask, on);
        self.tick(u32::from(cost.flag_op));
        Flow::Next
    }

    fn port_in<B: Bus>(&mut self, bus: &mut B, port: u16, w: bool) -> u16 {
        self.bus_transfer(w, port);
        let at = self.cycles;
        if !w {
            return u16::from(bus.input(port, at));
        }
        self.word_penalty(port);
        bus.input_word(port, at)
    }

    fn port_out<B: Bus>(&mut self, bus: &mut B, port: u16, w: bool, value: u16) {
        self.bus_transfer(w, port);
        let at = self.cycles;
        if w {
            self.word_penalty(port);
            bus.output_word(port, value, at);
        } else {
            bus.output(port, lo(value), at);
        }
    }

    /// WAIT: `3 + 5n` clocks, n the looks at TEST that found it high. Where
    /// TEST will not read low now, the CPU idles waiting, to be resumed once
    /// time has passed.
    pub(super) fn wait<B: Bus>(&mut self, bus: &mut B, wait: Wait) -> Flow {
        let low = bus.test_low_at(self.cycles);
        if low > self.cycles {
            self.waiting = Some(wait);
            return Flow::Wait;
        }
        self.waiting = None;
        let looks = low.saturating_sub(wait.began).div_ceil(WAIT_LOOK);
        let done = wait.began + u64::from(self.costs().wait) + WAIT_LOOK * looks;
        self.cycles = self.cycles.max(done);
        Flow::Next
    }

    /// ESC: the operand's dummy read, which a coprocessor captures from the
    /// bus, then the instruction handed to it.
    fn escape<B: Bus>(&mut self, bus: &mut B, insn: &Insn, ip: u16) -> Flow {
        let cost = self.costs();
        if self.model.traps_unused() && bus.escape_traps() {
            self.tick(u32::from(cost.int));
            return Flow::Fault(7);
        }
        let operand = match self.locate(insn) {
            Loc::Mem(seg, offset) => {
                let word = self.read16(bus, seg, offset);
                self.tick(u32::from(cost.esc_m));
                Some((linear(self.regs.sreg(seg), offset), word))
            }
            Loc::Reg(_) => {
                self.tick(u32::from(cost.esc_r));
                None
            }
        };
        let escape = Escape {
            opcode: (u16::from(insn.opcode & 7) << 8) | u16::from(insn.modrm.unwrap_or(0)),
            address: linear(self.regs.sreg(Seg::Cs), ip),
            operand,
        };
        bus.escape(escape, self.cycles);
        Flow::Next
    }

    fn shift<B: Bus>(&mut self, bus: &mut B, insn: &Insn, op: Shift, count: Count) -> Flow {
        let cost = self.costs();
        let w = insn.w;
        let mut n = match count {
            Count::One => 1,
            Count::Cl => self.regs.get8(1),
            Count::Imm => lo(insn.imm),
        };
        if self.model.masks_shift_count() {
            n &= 0x1F;
        }
        let loc = self.locate(insn);
        let value = self.load(bus, loc, w);
        if n != 0 {
            let family = self.family();
            let r = shifted(&mut self.regs.flags, family, op, value, n, w);
            self.store(bus, loc, w, r);
        }
        let mem = matches!(loc, Loc::Mem(..));
        let cost = match count {
            Count::One => u32::from(if mem { cost.shift1_m } else { cost.shift1_r }),
            _ => {
                u32::from(if mem { cost.shiftn_m } else { cost.shiftn_r })
                    + u32::from(cost.shift_step) * u32::from(n)
            }
        };
        self.tick(cost);
        Flow::Next
    }

    fn unary<B: Bus>(&mut self, bus: &mut B, insn: &Insn, op: Unary) -> Flow {
        let cost = self.costs();
        let w = insn.w;
        let width = width(w);
        let loc = self.locate(insn);
        let mem = matches!(loc, Loc::Mem(..));
        let value = self.load(bus, loc, w);
        let rep_negates = self.prefixes.rep.is_some();
        let slot = usize::from(w) + if mem { 2 } else { 0 };
        match op {
            Unary::Test => {
                alu::logic(&mut self.regs.flags, value & insn.imm, width);
                self.cost_rm(loc, cost.test_ri, cost.test_mi);
            }
            Unary::Not => {
                self.store(bus, loc, w, !value & width.mask);
                self.cost_rm(loc, cost.neg_r, cost.neg_m);
            }
            Unary::Neg => {
                let r = alu::neg(&mut self.regs.flags, value, width);
                self.store(bus, loc, w, r);
                self.cost_rm(loc, cost.neg_r, cost.neg_m);
            }
            Unary::Mul | Unary::Imul => {
                let signed = op == Unary::Imul;
                let base = if signed {
                    cost.imul[slot]
                } else {
                    cost.mul[slot]
                };
                let extra = if self.family() == Family::Intel8086 {
                    Self::multiply_8086(&mut self.regs, value, w, signed, rep_negates)
                } else {
                    Self::multiply(&mut self.regs, value, w, signed);
                    multiply_extra(value, w)
                };
                self.tick(u32::from(base) + extra);
            }
            Unary::Div | Unary::Idiv => {
                let signed = op == Unary::Idiv;
                let base = u32::from(if signed {
                    cost.idiv[slot]
                } else {
                    cost.div[slot]
                });
                if self.family() != Family::Intel8086 {
                    let family = self.family();
                    if Self::divide(&mut self.regs, family, value, w, signed) {
                        self.tick(base);
                        return Flow::Next;
                    }
                    let spent = if signed {
                        IDIV_EARLY_EXIT_V
                    } else {
                        DIV_EARLY_EXIT
                    };
                    self.tick(spent + u32::from(cost.int));
                    return Flow::Trap(0);
                }
                match Self::divide_8086(&mut self.regs, value, w, signed, rep_negates) {
                    Ok(extra) => self.tick(base + extra),
                    Err(error) => {
                        // The microcode leaves at the check that failed, then
                        // runs the interrupt entry.
                        let spent = match error {
                            alu::DivideError::Early { negations } => {
                                (if signed {
                                    IDIV_EARLY_EXIT
                                } else {
                                    DIV_EARLY_EXIT
                                }) + 6 * u32::from(negations)
                            }
                            alu::DivideError::Late => base,
                        };
                        self.tick(spent + u32::from(cost.int));
                        return Flow::Trap(0);
                    }
                }
            }
        }
        Flow::Next
    }

    /// The 8086 family's microcoded multiply, returning the cycles its loop
    /// spends beyond the base: one for each set bit of the accumulator's
    /// magnitude, which CORX adds on, and one for each operand PREIMUL
    /// negates.
    pub(super) fn multiply_8086(
        regs: &mut Regs,
        src: u16,
        w: bool,
        signed: bool,
        rep: bool,
    ) -> u32 {
        let width = width(w);
        let multiplicand = accumulator(regs, w);
        let (high, low) = alu::mul_8086(&mut regs.flags, multiplicand, src, width, signed, rep);
        if w {
            regs.gpr[gpr::AX] = low;
            regs.gpr[gpr::DX] = high;
        } else {
            regs.gpr[gpr::AX] = word(lo(low), lo(high));
        }
        let negative = |v: u16| signed && v & width.sign != 0;
        let magnitude = if negative(multiplicand) {
            multiplicand.wrapping_neg() & width.mask
        } else {
            multiplicand
        };
        magnitude.count_ones() + u32::from(negative(multiplicand)) + u32::from(negative(src))
    }

    /// The 8086 family's microcoded divide, returning the cycles its loop
    /// spends beyond the base — one for each pass that subtracts, one for
    /// each operand PREIDIV negates — or where it raised a divide error.
    pub(super) fn divide_8086(
        regs: &mut Regs,
        src: u16,
        w: bool,
        signed: bool,
        rep: bool,
    ) -> Result<u32, alu::DivideError> {
        let width = width(w);
        let (high, low) = if w {
            (regs.gpr[gpr::DX], regs.ax())
        } else {
            (u16::from(regs.get8(4)), u16::from(regs.get8(0)))
        };
        let flags = &mut regs.flags;
        let division = if signed {
            alu::idiv_8086(flags, high, low, src, width, rep)
        } else {
            alu::cord(flags, high, low, src, width)
        }?;
        if w {
            regs.gpr[gpr::AX] = division.quotient;
            regs.gpr[gpr::DX] = division.remainder;
        } else {
            regs.gpr[gpr::AX] = word(lo(division.quotient), lo(division.remainder));
        }
        Ok(u32::from(division.subtracted) + u32::from(division.negations))
    }

    /// Multiplication as the 80186 and V-series compute it.
    pub(super) fn multiply(regs: &mut Regs, src: u16, w: bool, signed: bool) {
        if w {
            let a = regs.ax();
            let product: u32 = if signed {
                (i32::from(a.cast_signed()) * i32::from(src.cast_signed())).cast_unsigned()
            } else {
                u32::from(a) * u32::from(src)
            };
            let (low, high) = (low16(product), high16(product));
            regs.gpr[gpr::AX] = low;
            regs.gpr[gpr::DX] = high;
            let overflow = if signed {
                high != if low & 0x8000 != 0 { 0xFFFF } else { 0 }
            } else {
                high != 0
            };
            regs.set_flag(flag::CF | flag::OF, overflow);
        } else {
            let a = regs.get8(0);
            let s = lo(src);
            let product: u16 = if signed {
                (i16::from(a.cast_signed()) * i16::from(s.cast_signed())).cast_unsigned()
            } else {
                u16::from(a) * u16::from(s)
            };
            regs.gpr[gpr::AX] = product;
            let overflow = if signed {
                product != sext8(lo(product))
            } else {
                hi(product) != 0
            };
            regs.set_flag(flag::CF | flag::OF, overflow);
        }
    }

    /// Division as the 80186 and V-series compute it, on magnitudes; false
    /// is a divide error. The V-series leaves the flags of the check that
    /// failed: `high - divisor` when the quotient cannot fit at all, else the
    /// quotient's magnitude passed through the ALU.
    pub(super) fn divide(regs: &mut Regs, family: Family, src: u16, w: bool, signed: bool) -> bool {
        let width = width(w);
        let bits = u32::from(width.bits);
        let (high, low) = if w {
            (regs.gpr[gpr::DX], regs.ax())
        } else {
            (u16::from(regs.get8(4)), u16::from(regs.get8(0)))
        };
        let wide = u32::from(high) << bits | u32::from(low);
        let sign_bit = 1u32 << (2 * bits - 1);
        let (dividend, divisor) = if signed {
            let n = if wide & sign_bit != 0 {
                i64::from(wide) - (1i64 << (2 * bits))
            } else {
                i64::from(wide)
            };
            let d = if src & width.sign != 0 {
                i64::from(src) - (1i64 << bits)
            } else {
                i64::from(src)
            };
            (n, d)
        } else {
            (i64::from(wide), i64::from(src & width.mask))
        };
        let magnitude = dividend.unsigned_abs();
        let divisor_magnitude = divisor.unsigned_abs();
        let high_magnitude =
            u16::try_from((magnitude >> bits) & u64::from(width.mask)).unwrap_or(0);
        let divisor_low = u16::try_from(divisor_magnitude & u64::from(width.mask)).unwrap_or(0);
        let nec = family == Family::Nec;
        let mut probe = regs.flags;
        alu::sub(&mut probe, high_magnitude, divisor_low, false, width);
        if probe & flag::CF == 0 {
            if nec {
                regs.flags = probe;
            }
            return false;
        }
        let quotient = magnitude / divisor_magnitude;
        let remainder = magnitude % divisor_magnitude;
        let negative = signed && (dividend < 0) != (divisor < 0);
        let limit = if !signed {
            u64::from(width.mask)
        } else if negative {
            1 << (bits - 1)
        } else {
            (1 << (bits - 1)) - 1
        };
        if quotient > limit {
            if nec {
                alu::logic(
                    &mut regs.flags,
                    u16::try_from(quotient & u64::from(width.mask)).unwrap_or(0),
                    width,
                );
            }
            return false;
        }
        let to_word = |v: u64, neg: bool| {
            let v = u16::try_from(v & 0xFFFF).unwrap_or(0);
            if neg {
                v.wrapping_neg() & width.mask
            } else {
                v
            }
        };
        let q = to_word(quotient, negative);
        let r = to_word(remainder, signed && dividend < 0);
        if w {
            regs.gpr[gpr::AX] = q;
            regs.gpr[gpr::DX] = r;
        } else {
            regs.gpr[gpr::AX] = word(lo(q), lo(r));
        }
        true
    }

    fn string<B: Bus>(
        &mut self,
        bus: &mut B,
        insn: &Insn,
        ip: u16,
        limit: u64,
        op: StringOp,
    ) -> Flow {
        let cost = self.costs();
        let index = op as usize;
        let w = insn.w;
        let step: u16 = if w { 2 } else { 1 };
        let delta = if self.regs.flag(flag::DF) {
            step.wrapping_neg()
        } else {
            step
        };
        let Some(rep) = self.prefixes.rep else {
            self.string_once(bus, op, w, delta);
            self.tick(u32::from(match op {
                StringOp::Movs => cost.movs,
                StringOp::Cmps => cost.cmps,
                StringOp::Scas => cost.scas,
                StringOp::Lods => cost.lods,
                StringOp::Stos => cost.stos,
                StringOp::Ins => cost.ins,
                StringOp::Outs => cost.outs,
            }));
            return Flow::Next;
        };
        self.tick(u32::from(cost.rep_setup[index]));
        let compares = matches!(op, StringOp::Cmps | StringOp::Scas);
        let trap = self.regs.flag(flag::TF);
        while self.regs.cx() != 0 {
            self.string_once(bus, op, w, delta);
            self.regs.gpr[gpr::CX] = self.regs.cx().wrapping_sub(1);
            self.tick(u32::from(cost.rep_each[index]));
            if compares {
                let done = match rep {
                    Rep::Z => !self.regs.flag(flag::ZF),
                    Rep::Nz => self.regs.flag(flag::ZF),
                    Rep::Nc => self.regs.flag(flag::CF),
                    Rep::C => !self.regs.flag(flag::CF),
                };
                if done {
                    break;
                }
            }
            if self.regs.cx() == 0 {
                break;
            }
            if trap || bus.nmi() || (self.regs.flag(flag::IF) && bus.intr()) {
                self.regs.ip = self.resume_ip(ip);
                return Flow::Yield;
            }
            if self.cycles >= limit {
                // A pause the program cannot see: it resumes with every
                // prefix, and an interrupt the pause lets in first returns
                // where one taken inside the instruction would.
                self.paused = Some(self.resume_ip(ip));
                self.regs.ip = self.prefixes.first_ip;
                return Flow::Yield;
            }
        }
        Flow::Next
    }

    /// Where an interrupted string instruction resumes: the 8086 and 80186
    /// return to the last prefix only, losing any before it; the V-series
    /// returns to the first.
    fn resume_ip(&self, opcode_ip: u16) -> u16 {
        if self.prefixes.count == 0 {
            opcode_ip
        } else if self.family() == Family::Nec {
            self.prefixes.first_ip
        } else {
            self.prefixes.last_ip
        }
    }

    fn string_once<B: Bus>(&mut self, bus: &mut B, op: StringOp, w: bool, delta: u16) {
        let source = self.prefixes.segment.unwrap_or(Seg::Ds);
        let si = self.regs.gpr[gpr::SI];
        let di = self.regs.gpr[gpr::DI];
        let width = width(w);
        let read = |cpu: &mut Self, bus: &mut B, seg: Seg, off: u16| {
            if w {
                cpu.read16(bus, seg, off)
            } else {
                u16::from(cpu.read8(bus, seg, off))
            }
        };
        let write = |cpu: &mut Self, bus: &mut B, off: u16, value: u16| {
            if w {
                cpu.write16(bus, Seg::Es, off, value);
            } else {
                cpu.write8(bus, Seg::Es, off, lo(value));
            }
        };
        match op {
            StringOp::Movs => {
                let value = read(self, bus, source, si);
                write(self, bus, di, value);
                self.regs.gpr[gpr::SI] = si.wrapping_add(delta);
                self.regs.gpr[gpr::DI] = di.wrapping_add(delta);
            }
            StringOp::Cmps => {
                let a = read(self, bus, source, si);
                let b = read(self, bus, Seg::Es, di);
                alu::sub(&mut self.regs.flags, a, b, false, width);
                self.regs.gpr[gpr::SI] = si.wrapping_add(delta);
                self.regs.gpr[gpr::DI] = di.wrapping_add(delta);
            }
            StringOp::Scas => {
                let b = read(self, bus, Seg::Es, di);
                let a = self.acc(w);
                alu::sub(&mut self.regs.flags, a, b, false, width);
                self.regs.gpr[gpr::DI] = di.wrapping_add(delta);
            }
            StringOp::Lods => {
                let value = read(self, bus, source, si);
                self.set_acc(w, value);
                self.regs.gpr[gpr::SI] = si.wrapping_add(delta);
            }
            StringOp::Stos => {
                write(self, bus, di, self.acc(w));
                self.regs.gpr[gpr::DI] = di.wrapping_add(delta);
            }
            StringOp::Ins => {
                let value = self.port_in(bus, self.regs.gpr[gpr::DX], w);
                write(self, bus, di, value);
                self.regs.gpr[gpr::DI] = di.wrapping_add(delta);
            }
            StringOp::Outs => {
                let value = read(self, bus, source, si);
                self.port_out(bus, self.regs.gpr[gpr::DX], w, value);
                self.regs.gpr[gpr::SI] = si.wrapping_add(delta);
            }
        }
    }
}

/// The group 2 `op` of `value`, `n` (not zero) steps, its flags into
/// `flags`, as `family` runs it: field 6 is SHL off the 8086 family, and the
/// V-series' shifts clear AF.
pub(super) fn shifted(
    flags: &mut u16,
    family: Family,
    op: Shift,
    value: u16,
    n: u8,
    w: bool,
) -> u16 {
    let op = if op == Shift::Setmo && family != Family::Intel8086 {
        Shift::Shl
    } else {
        op
    };
    let r = alu::shift(flags, op, value, n, width(w));
    if family == Family::Nec && matches!(op, Shift::Shl | Shift::Shr | Shift::Sar) {
        *flags &= !flag::AF;
    }
    r
}

/// AAM by `base`, which on an Intel part is not zero: the V-series divides
/// by zero to an all-ones quotient and leaves AL.
pub(super) fn aam(regs: &mut Regs, family: Family, base: u8) {
    let al = regs.get8(0);
    if base == 0 && family == Family::Nec {
        regs.set8(4, 0xFF);
        alu::pass(&mut regs.flags, u16::from(al), BYTE);
        return;
    }
    let base = base.max(1);
    regs.set8(4, al / base);
    regs.set8(0, al % base);
    alu::pass(&mut regs.flags, u16::from(al % base), BYTE);
}

/// AAD by `base`: the V-series ignores the immediate and multiplies by ten.
pub(super) fn aad(regs: &mut Regs, family: Family, base: u8) {
    let base = if family == Family::Nec { 10 } else { base };
    let product = u16::from(regs.get8(4)) * u16::from(base);
    let al = u16::from(regs.get8(0));
    regs.gpr[gpr::AX] = alu::add(&mut regs.flags, al, product, false, BYTE);
}

/// IMUL's three-operand form: the product's low word, CF and OF set where
/// it overflowed, as the 80186 and V-series multipliers leave them.
pub(super) fn imul_imm(regs: &mut Regs, a: u16, imm: u16) -> u16 {
    let product = i32::from(a.cast_signed()) * i32::from(imm.cast_signed());
    let r = low16(product.cast_unsigned());
    regs.set_flag(flag::CF | flag::OF, i32::from(r.cast_signed()) != product);
    r
}

/// AL, or AX, as a word.
fn accumulator(regs: &Regs, w: bool) -> u16 {
    if w {
        regs.ax()
    } else {
        u16::from(regs.get8(0))
    }
}

/// Cycles DIV and IDIV spend before a divide error at their opening check,
/// measured on the 8086 and V20; each operand the 8086's IDIV negated first
/// adds six.
const DIV_EARLY_EXIT: u32 = 9;
const IDIV_EARLY_EXIT: u32 = 18;
const IDIV_EARLY_EXIT_V: u32 = 20;

/// The data-dependent part of an 80186 or V-series multiply, whose array
/// varies by at most two cycles.
pub(super) fn multiply_extra(multiplier: u16, w: bool) -> u32 {
    let bits = if w {
        multiplier.count_ones()
    } else {
        u16::from(lo(multiplier)).count_ones()
    };
    (bits * 2) / if w { 16 } else { 8 }
}
