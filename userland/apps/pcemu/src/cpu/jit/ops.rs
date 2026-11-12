//! Each instruction the translator takes, lowered as `exec.rs` executes it:
//! the same operands, flags, costs and bus transfers, in an order whose
//! totals are the same. Every access an instruction makes is checked before
//! the instruction changes anything; one that fails leaves the block before
//! it.

use tairix_codegen::ir::{BinOp, Cond as IrCond, Type, UnOp};

use super::emit::V;
use super::helper::{self, Kind};
use super::lower::{Access, Flow, Lower, Unit};
use crate::cpu::decode::{Alu, Base, Cond, Count, Ea, Mem, Op, Shift, Unary};
use crate::cpu::model::{CpuModel, Family};
use crate::cpu::regs::{flag, gpr, Seg};
use crate::cpu::timing::{self, Costs};

const I32: Type = Type::I32;

/// Whether the translator takes `u`.
pub(crate) fn takes(model: CpuModel, u: &Unit) -> bool {
    let insn = &u.insn;
    let pending = u.pending();
    if pending.lock {
        return false;
    }
    let nec = model.family() == Family::Nec;
    match insn.op {
        Op::Prefix(_)
        | Op::Wait
        | Op::Popf
        | Op::Int3
        | Op::Int
        | Op::Into
        | Op::Iret
        | Op::Esc
        | Op::InImm
        | Op::OutImm
        | Op::InDx
        | Op::OutDx
        | Op::Hlt
        | Op::Bound
        | Op::Ins
        | Op::Outs
        | Op::Enter
        | Op::Nec(_)
        | Op::Nop
        | Op::Invalid
        | Op::HostCall
        | Op::PopSeg(Seg::Cs) => false,
        Op::MovSegRm => Seg::from_field(insn.reg) != Seg::Cs,
        Op::Movs | Op::Cmps | Op::Scas | Op::Lods | Op::Stos => pending.rep.is_none(),
        Op::Aam => nec || insn.imm & 0xFF != 0,
        Op::CallFarRm | Op::JmpFarRm => !(nec && !insn.w && matches!(insn.ea, Ea::Reg(_))),
        _ => true,
    }
}

/// Whether `u` ends its block: it transfers control, sets IF, or inhibits
/// interrupts for the instruction after it.
pub(crate) fn ends_block(model: CpuModel, u: &Unit) -> bool {
    match u.insn.op {
        Op::Jcc(_)
        | Op::Loop
        | Op::Loope
        | Op::Loopne
        | Op::Jcxz
        | Op::CallNear
        | Op::CallFar
        | Op::CallRm
        | Op::CallFarRm
        | Op::JmpNear
        | Op::JmpShort
        | Op::JmpFar
        | Op::JmpRm
        | Op::JmpFarRm
        | Op::Ret { .. }
        | Op::Retf { .. }
        | Op::Sti => true,
        Op::MovSegRm => inhibits(model, Seg::from_field(u.insn.reg)),
        Op::PopSeg(seg) => inhibits(model, seg),
        _ => false,
    }
}

/// Whether loading `seg` inhibits interrupts for one instruction: any
/// segment on the 8086 family, SS elsewhere.
fn inhibits(model: CpuModel, seg: Seg) -> bool {
    seg == Seg::Ss || model.family() == Family::Intel8086
}

/// Where an instruction's `r/m` operand is: a register, or memory the page
/// map admitted.
#[derive(Clone, Copy, Debug)]
enum Place {
    Reg(u8),
    Mem { access: Access, offset: V, mem: Mem },
}

fn costs(l: &Lower) -> &'static Costs {
    timing::costs(l.model.family())
}

fn k(n: u16) -> V {
    V::K(u64::from(n))
}

const fn mask(word: bool) -> V {
    V::K(if word { 0xFFFF } else { 0xFF })
}

fn reg(l: &mut Lower, r: u8, word: bool) -> V {
    let full = l.gpr(usize::from(r & if word { 7 } else { 3 }));
    if word {
        return full;
    }
    let shifted = if r & 4 == 0 {
        full
    } else {
        l.e.shr(I32, full, 8)
    };
    l.e.and(I32, shifted, V::K(0xFF))
}

fn set_reg(l: &mut Lower, r: u8, word: bool, value: V) {
    if word {
        let value = l.e.and(I32, value, V::K(0xFFFF));
        l.set_gpr(usize::from(r & 7), value);
        return;
    }
    let slot = usize::from(r & 3);
    let byte = l.e.and(I32, value, V::K(0xFF));
    let (keep, moved) = if r & 4 == 0 {
        (0xFF00, byte)
    } else {
        (0x00FF, l.e.shl(I32, byte, 8))
    };
    let old = l.gpr(slot);
    let kept = l.e.and(I32, old, V::K(keep));
    let merged = l.e.or(I32, kept, moved);
    l.set_gpr(slot, merged);
}

/// Register `n` moved by `by`, a 16-bit two's-complement step.
fn bump(l: &mut Lower, n: usize, by: V) {
    let value = l.gpr(n);
    let moved = l.e.add(I32, value, by);
    let moved = l.e.and(I32, moved, V::K(0xFFFF));
    l.set_gpr(n, moved);
}

fn offset_of(l: &mut Lower, mem: Mem) -> V {
    let (first, second) = match mem.base {
        Base::BxSi => (Some(gpr::BX), Some(gpr::SI)),
        Base::BxDi => (Some(gpr::BX), Some(gpr::DI)),
        Base::BpSi => (Some(gpr::BP), Some(gpr::SI)),
        Base::BpDi => (Some(gpr::BP), Some(gpr::DI)),
        Base::Si => (Some(gpr::SI), None),
        Base::Di => (Some(gpr::DI), None),
        Base::Bp => (Some(gpr::BP), None),
        Base::Bx => (Some(gpr::BX), None),
        Base::Absolute => (None, None),
    };
    let mut sum = V::K(0);
    for n in [first, second].into_iter().flatten() {
        let value = l.gpr(n);
        sum = l.e.add(I32, sum, value);
    }
    let sum = l.e.add(I32, sum, k(mem.disp));
    l.e.and(I32, sum, V::K(0xFFFF))
}

/// The `r/m` operand of `u`, a byte or word for a load or, where `store`,
/// a store: its access checked.
fn place(l: &mut Lower, u: &Unit, word: bool, store: bool) -> Place {
    match u.insn.ea {
        Ea::Reg(r) => Place::Reg(r),
        Ea::Mem(mem) => {
            let seg = u.pending().segment.unwrap_or(mem.base.default_segment());
            let offset = offset_of(l, mem);
            let base = l.seg(seg);
            let access = l.access(base, offset, word, store);
            Place::Mem {
                access,
                offset,
                mem,
            }
        }
    }
}

/// The address calculation `Cpu::locate` charges and records.
fn locate(l: &mut Lower, at: Place) {
    if let Place::Mem { offset, mem, .. } = at {
        l.set_ind(offset);
        l.tick_k(timing::ea(l.model.family(), mem));
    }
}

fn load(l: &mut Lower, at: Place, word: bool) -> V {
    match at {
        Place::Reg(r) => reg(l, r, word),
        Place::Mem { access, .. } => {
            let value = l.read(access, word);
            l.set_opr(value);
            value
        }
    }
}

fn store(l: &mut Lower, at: Place, word: bool, value: V) {
    match at {
        Place::Reg(r) => set_reg(l, r, word, value),
        Place::Mem { access, .. } => {
            let value = l.e.and(I32, value, mask(word));
            l.set_opr(value);
            l.write(access, word, value);
        }
    }
}

fn cost_rm(l: &mut Lower, at: Place, reg_cost: u16, mem_cost: u16) {
    let n = if matches!(at, Place::Reg(_)) {
        reg_cost
    } else {
        mem_cost
    };
    l.tick_k(u32::from(n));
}

fn alu(l: &mut Lower, op: Alu, a: V, b: V, word: bool) -> V {
    match op {
        Alu::Add => l.arith(false, (a, b, V::K(0)), word),
        Alu::Sub | Alu::Cmp => l.arith(true, (a, b, V::K(0)), word),
        Alu::Adc | Alu::Sbb => {
            let carry = l.bit(flag::CF);
            l.arith(op == Alu::Sbb, (a, b, carry), word)
        }
        Alu::And | Alu::Or | Alu::Xor => {
            let op = match op {
                Alu::And => BinOp::And,
                Alu::Or => BinOp::Or,
                _ => BinOp::Xor,
            };
            let result = l.e.bin(I32, op, a, b);
            l.logic(result, word)
        }
    }
}

/// `cond` read straight from the operands of a pending subtraction with no
/// borrow in: one comparison, which the backends fuse with a branch on it.
fn compared(l: &mut Lower, cond: Cond) -> Option<V> {
    let lazy = l
        .status()
        .filter(|lazy| lazy.kind == Kind::Sub && lazy.carry == V::K(0))?;
    let (signed, test) = match cond {
        Cond::E => (false, IrCond::Eq),
        Cond::Ne => (false, IrCond::Ne),
        Cond::B => (false, IrCond::Ult),
        Cond::Nb => (false, IrCond::Uge),
        Cond::Be => (false, IrCond::Ule),
        Cond::A => (false, IrCond::Ugt),
        Cond::L => (true, IrCond::Slt),
        Cond::Ge => (true, IrCond::Sge),
        Cond::Le => (true, IrCond::Sle),
        Cond::G => (true, IrCond::Sgt),
        Cond::O | Cond::No | Cond::S | Cond::Ns | Cond::P | Cond::Np => return None,
    };
    let (a, b) = if signed {
        let extend = if lazy.word { UnOp::Sext16 } else { UnOp::Sext8 };
        (l.e.un(I32, extend, lazy.a), l.e.un(I32, extend, lazy.b))
    } else {
        (lazy.a, lazy.b)
    };
    Some(l.e.cmp(I32, test, a, b))
}

fn condition(l: &mut Lower, cond: Cond) -> V {
    if let Some(compared) = compared(l, cond) {
        return compared;
    }
    let (value, inverted) = match cond {
        Cond::O | Cond::No => (l.bit(flag::OF), cond == Cond::No),
        Cond::B | Cond::Nb => (l.bit(flag::CF), cond == Cond::Nb),
        Cond::E | Cond::Ne => (l.bit(flag::ZF), cond == Cond::Ne),
        Cond::Be | Cond::A => (l.any(flag::CF | flag::ZF), cond == Cond::A),
        Cond::S | Cond::Ns => (l.bit(flag::SF), cond == Cond::Ns),
        Cond::P | Cond::Np => (l.bit(flag::PF), cond == Cond::Np),
        Cond::L | Cond::Ge | Cond::Le | Cond::G => {
            let sf = l.bit(flag::SF);
            let of = l.bit(flag::OF);
            let less = l.e.xor(I32, sf, of);
            let value = if matches!(cond, Cond::Le | Cond::G) {
                let zf = l.bit(flag::ZF);
                l.e.or(I32, zf, less)
            } else {
                less
            };
            (value, matches!(cond, Cond::Ge | Cond::G))
        }
    };
    if inverted {
        l.e.not(value)
    } else {
        value
    }
}

/// Where a push of a word lands, the stack pointer `down` words below SP,
/// checked.
fn stack_slot(l: &mut Lower, down: u16, store: bool) -> Access {
    let sp = l.gpr(gpr::SP);
    let at = l.e.sub(I32, sp, k(2 * down));
    let at = l.e.and(I32, at, V::K(0xFFFF));
    let ss = l.seg(Seg::Ss);
    l.access(ss, at, true, store)
}

/// A word `up` words above SP, checked for a pop.
fn stack_word(l: &mut Lower, up: u16) -> Access {
    let sp = l.gpr(gpr::SP);
    let at = l.e.add(I32, sp, k(2 * up));
    let at = l.e.and(I32, at, V::K(0xFFFF));
    let ss = l.seg(Seg::Ss);
    l.access(ss, at, true, false)
}

fn push(l: &mut Lower, slot: Access, value: V) {
    bump(l, gpr::SP, V::K(0xFFFE));
    l.write(slot, true, value);
}

fn pop(l: &mut Lower, slot: Access) -> V {
    let value = l.read(slot, true);
    bump(l, gpr::SP, V::K(2));
    value
}

fn relative(u: &Unit) -> V {
    k(u.next.wrapping_add(u.insn.imm))
}

/// Lowers `u`; true where it ends the block.
#[allow(clippy::too_many_lines)] // One arm per instruction form reads as the opcode map.
pub(crate) fn lower(l: &mut Lower, u: &Unit) -> bool {
    let insn = u.insn;
    let word = insn.w;
    let cost = costs(l);
    let flow = match insn.op {
        Op::AluRmReg(op) | Op::AluRmImm(op) => {
            let at = place(l, u, word, op != Alu::Cmp);
            l.begin(u);
            locate(l, at);
            let a = load(l, at, word);
            let b = if matches!(insn.op, Op::AluRmReg(_)) {
                reg(l, insn.reg, word)
            } else {
                k(insn.imm)
            };
            let result = alu(l, op, a, b, word);
            if op != Alu::Cmp {
                store(l, at, word, result);
            }
            let (rr, mem) = match (insn.op, op) {
                (Op::AluRmReg(_), Alu::Cmp) => (cost.alu_rr, cost.cmp_mr),
                (Op::AluRmReg(_), _) => (cost.alu_rr, cost.alu_mr),
                (_, Alu::Cmp) => (cost.alu_ri, cost.cmp_mi),
                _ => (cost.alu_ri, cost.alu_mi),
            };
            cost_rm(l, at, rr, mem);
            Flow::Next
        }
        Op::AluRegRm(op) => {
            let at = place(l, u, word, false);
            l.begin(u);
            locate(l, at);
            let b = load(l, at, word);
            let a = reg(l, insn.reg, word);
            let result = alu(l, op, a, b, word);
            if op != Alu::Cmp {
                set_reg(l, insn.reg, word, result);
            }
            cost_rm(l, at, cost.alu_rr, cost.alu_rm);
            Flow::Next
        }
        Op::AluAccImm(op) => {
            l.begin(u);
            let a = reg(l, 0, word);
            let result = alu(l, op, a, k(insn.imm), word);
            if op != Alu::Cmp {
                set_reg(l, 0, word, result);
            }
            l.tick_k(u32::from(cost.alu_ai));
            Flow::Next
        }
        Op::PushSeg(seg) => {
            let slot = stack_slot(l, 1, true);
            l.begin(u);
            let value = l.seg(seg);
            push(l, slot, value);
            l.tick_k(u32::from(cost.push_seg));
            Flow::Next
        }
        Op::PopSeg(seg) => {
            let slot = stack_word(l, 0);
            l.begin(u);
            let value = pop(l, slot);
            l.set_seg(seg, value);
            l.tick_k(u32::from(cost.pop_seg));
            if inhibits(l.model, seg) {
                Flow::Inhibit(V::K(1))
            } else {
                Flow::Next
            }
        }
        Op::Daa | Op::Das | Op::Aaa | Op::Aas => {
            l.begin(u);
            let (which, n) = match insn.op {
                Op::Daa => (0, cost.daa),
                Op::Das => (1, cost.das),
                Op::Aaa => (2, cost.aaa),
                _ => (3, cost.aas),
            };
            let ax = l.gpr(gpr::AX);
            let flags = l.word();
            let [ax, flags, _, _] = l.call(helper::ADJUST, &[ax, flags, V::K(which)]);
            l.set_gpr(gpr::AX, ax);
            l.set_word(flags);
            l.tick_k(u32::from(n));
            Flow::Next
        }
        Op::IncReg | Op::DecReg => {
            l.begin(u);
            let index = usize::from(insn.reg);
            let value = l.gpr(index);
            let result = l.inc_dec(value, true, insn.op == Op::DecReg);
            l.set_gpr(index, result);
            l.tick_k(u32::from(cost.inc_reg16));
            Flow::Next
        }
        Op::PushReg => {
            let slot = stack_slot(l, 1, true);
            l.begin(u);
            let index = usize::from(insn.reg);
            let value = l.gpr(index);
            // The 8086 and its contemporaries push SP after decrementing it.
            let value = if index == gpr::SP {
                let down = l.e.sub(I32, value, V::K(2));
                l.e.and(I32, down, V::K(0xFFFF))
            } else {
                value
            };
            push(l, slot, value);
            l.tick_k(u32::from(cost.push_reg));
            Flow::Next
        }
        Op::PopReg => {
            let slot = stack_word(l, 0);
            l.begin(u);
            let value = pop(l, slot);
            l.set_gpr(usize::from(insn.reg), value);
            l.tick_k(u32::from(cost.pop_reg));
            Flow::Next
        }
        Op::Jcc(cond) => {
            l.begin(u);
            let taken = condition(l, cond);
            branch(l, u, taken, (cost.jcc_taken, cost.jcc_not));
            return true;
        }
        Op::TestRmReg => {
            let at = place(l, u, word, false);
            l.begin(u);
            locate(l, at);
            let a = load(l, at, word);
            let b = reg(l, insn.reg, word);
            let result = l.e.and(I32, a, b);
            l.logic(result, word);
            cost_rm(l, at, cost.test_rr, cost.test_mr);
            Flow::Next
        }
        Op::XchgRmReg => {
            let at = place(l, u, word, true);
            l.begin(u);
            locate(l, at);
            let a = load(l, at, word);
            let b = reg(l, insn.reg, word);
            store(l, at, word, b);
            set_reg(l, insn.reg, word, a);
            cost_rm(l, at, cost.xchg_rr, cost.xchg_mem);
            Flow::Next
        }
        Op::MovRmReg => {
            let at = place(l, u, word, true);
            l.begin(u);
            locate(l, at);
            let value = reg(l, insn.reg, word);
            store(l, at, word, value);
            cost_rm(l, at, cost.mov_rr, cost.mov_mr);
            Flow::Next
        }
        Op::MovRegRm => {
            let at = place(l, u, word, false);
            l.begin(u);
            locate(l, at);
            let value = load(l, at, word);
            set_reg(l, insn.reg, word, value);
            cost_rm(l, at, cost.mov_rr, cost.mov_rm);
            Flow::Next
        }
        Op::MovRmSeg => {
            let at = place(l, u, true, true);
            l.begin(u);
            locate(l, at);
            let value = l.seg(Seg::from_field(insn.reg));
            store(l, at, true, value);
            cost_rm(l, at, cost.mov_r_seg, cost.mov_m_seg);
            Flow::Next
        }
        Op::Lea => {
            l.begin(u);
            let value = match insn.ea {
                Ea::Mem(mem) => {
                    let offset = offset_of(l, mem);
                    l.set_ind(offset);
                    l.tick_k(timing::ea(l.model.family(), mem));
                    offset
                }
                Ea::Reg(_) => l.ind(),
            };
            l.set_gpr(usize::from(insn.reg), value);
            l.tick_k(u32::from(cost.lea));
            Flow::Next
        }
        Op::MovSegRm => {
            let at = place(l, u, true, false);
            l.begin(u);
            locate(l, at);
            let value = load(l, at, true);
            let seg = Seg::from_field(insn.reg);
            l.set_seg(seg, value);
            cost_rm(l, at, cost.mov_seg_r, cost.mov_seg_m);
            if inhibits(l.model, seg) {
                Flow::Inhibit(V::K(1))
            } else {
                Flow::Next
            }
        }
        Op::PopRm => {
            let slot = stack_word(l, 0);
            let at = place(l, u, true, true);
            l.begin(u);
            let value = pop(l, slot);
            locate(l, at);
            store(l, at, true, value);
            cost_rm(l, at, cost.pop_reg, cost.pop_mem);
            Flow::Next
        }
        Op::XchgAx => {
            l.begin(u);
            let index = usize::from(insn.reg);
            let (a, b) = (l.gpr(gpr::AX), l.gpr(index));
            l.set_gpr(gpr::AX, b);
            l.set_gpr(index, a);
            l.tick_k(u32::from(if insn.reg == 0 {
                cost.nop
            } else {
                cost.xchg_acc
            }));
            Flow::Next
        }
        Op::Cbw => {
            l.begin(u);
            let ax = l.gpr(gpr::AX);
            let wide = l.e.un(I32, UnOp::Sext8, ax);
            let ax = l.e.and(I32, wide, V::K(0xFFFF));
            l.set_gpr(gpr::AX, ax);
            l.tick_k(u32::from(cost.cbw));
            Flow::Next
        }
        Op::Cwd => {
            l.begin(u);
            let ax = l.gpr(gpr::AX);
            let sign = l.e.shr(I32, ax, 15);
            let all = l.e.sub(I32, V::K(0), sign);
            let dx = l.e.and(I32, all, V::K(0xFFFF));
            l.set_gpr(gpr::DX, dx);
            l.tick_k(u32::from(cost.cwd));
            Flow::Next
        }
        Op::CallFar => {
            let first = stack_slot(l, 1, true);
            let second = stack_slot(l, 2, true);
            l.begin(u);
            push(l, first, k(l.cs));
            push(l, second, k(u.next));
            l.tick_k(u32::from(cost.call_far));
            Flow::Jump {
                cs: Some(k(insn.imm2)),
                ip: k(insn.imm),
            }
        }
        Op::Pushf => {
            let slot = stack_slot(l, 1, true);
            l.begin(u);
            let flags = l.word();
            push(l, slot, flags);
            l.tick_k(u32::from(cost.pushf));
            Flow::Next
        }
        Op::Sahf => {
            l.begin(u);
            let ax = l.gpr(gpr::AX);
            let ah = l.e.shr(I32, ax, 8);
            let taken_mask = flag::SF | flag::ZF | flag::AF | flag::PF | flag::CF;
            let taken = l.e.and(I32, ah, V::K(u64::from(taken_mask)));
            let flags = l.word();
            let kept = l.e.and(I32, flags, V::K(u64::from(!taken_mask)));
            let flags = l.e.or(I32, kept, taken);
            l.set_word(flags);
            l.tick_k(u32::from(cost.sahf));
            Flow::Next
        }
        Op::Lahf => {
            l.begin(u);
            let flags = l.word();
            let low = l.e.and(I32, flags, V::K(0xFF));
            set_reg(l, 4, false, low);
            l.tick_k(u32::from(cost.lahf));
            Flow::Next
        }
        Op::MovAccMem | Op::MovMemAcc => {
            let seg = u.pending().segment.unwrap_or(Seg::Ds);
            let base = l.seg(seg);
            let into = insn.op == Op::MovMemAcc;
            let a = l.access(base, k(insn.imm), word, into);
            l.begin(u);
            if into {
                let value = reg(l, 0, word);
                l.write(a, word, value);
                l.tick_k(u32::from(cost.mov_mem_acc));
            } else {
                let value = l.read(a, word);
                set_reg(l, 0, word, value);
                l.tick_k(u32::from(cost.mov_acc_mem));
            }
            Flow::Next
        }
        Op::Movs | Op::Cmps | Op::Scas | Op::Lods | Op::Stos => {
            string(l, u);
            Flow::Next
        }
        Op::TestAccImm => {
            l.begin(u);
            let a = reg(l, 0, word);
            let result = l.e.and(I32, a, k(insn.imm));
            l.logic(result, word);
            l.tick_k(u32::from(cost.test_ai));
            Flow::Next
        }
        Op::MovRegImm => {
            l.begin(u);
            set_reg(l, insn.reg, word, k(insn.imm));
            l.tick_k(u32::from(cost.mov_ri));
            Flow::Next
        }
        Op::Ret { pops } => {
            let slot = stack_word(l, 0);
            l.begin(u);
            let ip = pop(l, slot);
            if pops {
                bump(l, gpr::SP, k(insn.imm));
            }
            l.tick_k(u32::from(if pops { cost.ret_imm } else { cost.ret }));
            Flow::Jump { cs: None, ip }
        }
        Op::Les | Op::Lds => {
            let far = far_pointer(l, u);
            l.begin(u);
            let (offset, segment) = load_far(l, far);
            l.set_gpr(usize::from(insn.reg), offset);
            let seg = if insn.op == Op::Les { Seg::Es } else { Seg::Ds };
            l.set_seg(seg, segment);
            l.tick_k(u32::from(cost.lds));
            Flow::Next
        }
        Op::MovRmImm => {
            let at = place(l, u, word, true);
            l.begin(u);
            locate(l, at);
            store(l, at, word, k(insn.imm));
            cost_rm(l, at, cost.mov_ri, cost.mov_mi);
            Flow::Next
        }
        Op::Retf { pops } => {
            let first = stack_word(l, 0);
            let second = stack_word(l, 1);
            l.begin(u);
            let ip = pop(l, first);
            let cs = pop(l, second);
            if pops {
                bump(l, gpr::SP, k(insn.imm));
            }
            l.tick_k(u32::from(if pops { cost.retf_imm } else { cost.retf }));
            Flow::Jump { cs: Some(cs), ip }
        }
        Op::Shift(op, count) => {
            shift(l, u, op, count);
            Flow::Next
        }
        Op::Aam | Op::Aad => {
            l.begin(u);
            let which = if insn.op == Op::Aam {
                helper::AAM
            } else {
                helper::AAD
            };
            let family = V::K(helper::family_code(l.model.family()));
            let ax = l.gpr(gpr::AX);
            let flags = l.word();
            let [ax, flags, _, _] = l.call(which, &[ax, k(insn.imm & 0xFF), flags, family]);
            l.set_gpr(gpr::AX, ax);
            l.set_word(flags);
            l.tick_k(u32::from(if insn.op == Op::Aam {
                cost.aam
            } else {
                cost.aad
            }));
            Flow::Next
        }
        Op::Salc => {
            l.begin(u);
            let carry = l.bit(flag::CF);
            let all = l.e.sub(I32, V::K(0), carry);
            set_reg(l, 0, false, all);
            l.tick_k(u32::from(cost.flag_op));
            Flow::Next
        }
        Op::Xlat => {
            let seg = u.pending().segment.unwrap_or(Seg::Ds);
            let base = l.seg(seg);
            let al = reg(l, 0, false);
            let bx = l.gpr(gpr::BX);
            let offset = l.e.add(I32, bx, al);
            let offset = l.e.and(I32, offset, V::K(0xFFFF));
            let a = l.access(base, offset, false, false);
            l.begin(u);
            let value = l.read(a, false);
            set_reg(l, 0, false, value);
            l.tick_k(u32::from(cost.xlat));
            if insn.opcode == 0xD6 {
                l.tick_k(crate::cpu::nec::XLAT_ALIAS_IDLE);
            }
            Flow::Next
        }
        Op::Loopne | Op::Loope | Op::Loop => {
            l.begin(u);
            bump(l, gpr::CX, V::K(0xFFFF));
            let cx = l.gpr(gpr::CX);
            let more = l.e.cmp(I32, IrCond::Ne, cx, V::K(0));
            let (taken, costs) = match insn.op {
                Op::Loopne => {
                    let zf = l.bit(flag::ZF);
                    let clear = l.e.not(zf);
                    (
                        l.e.and(I32, more, clear),
                        (cost.loopne_taken, cost.loopne_not),
                    )
                }
                Op::Loope => {
                    let zf = l.bit(flag::ZF);
                    (l.e.and(I32, more, zf), (cost.loope_taken, cost.loope_not))
                }
                _ => (more, (cost.loop_taken, cost.loop_not)),
            };
            branch(l, u, taken, costs);
            return true;
        }
        Op::Jcxz => {
            l.begin(u);
            let cx = l.gpr(gpr::CX);
            let zero = l.e.cmp(I32, IrCond::Eq, cx, V::K(0));
            branch(l, u, zero, (cost.jcxz_taken, cost.jcxz_not));
            return true;
        }
        Op::CallNear => {
            let slot = stack_slot(l, 1, true);
            l.begin(u);
            push(l, slot, k(u.next));
            l.tick_k(u32::from(cost.call_near));
            Flow::Jump {
                cs: None,
                ip: relative(u),
            }
        }
        Op::JmpNear | Op::JmpShort => {
            l.begin(u);
            l.tick_k(u32::from(if insn.op == Op::JmpNear {
                cost.jmp_near
            } else {
                cost.jmp_short
            }));
            Flow::Jump {
                cs: None,
                ip: relative(u),
            }
        }
        Op::JmpFar => {
            l.begin(u);
            l.tick_k(u32::from(cost.jmp_far));
            Flow::Jump {
                cs: Some(k(insn.imm2)),
                ip: k(insn.imm),
            }
        }
        Op::Cmc | Op::Clc | Op::Stc => {
            l.begin(u);
            let flags = l.word();
            let cf = V::K(u64::from(flag::CF));
            let flags = match insn.op {
                Op::Cmc => l.e.xor(I32, flags, cf),
                Op::Clc => l.e.and(I32, flags, V::K(u64::from(!flag::CF))),
                _ => l.e.or(I32, flags, cf),
            };
            l.set_word(flags);
            l.tick_k(u32::from(cost.flag_op));
            Flow::Next
        }
        Op::Cli | Op::Cld | Op::Std => {
            l.begin(u);
            let (mask, on) = match insn.op {
                Op::Cli => (flag::IF, false),
                Op::Cld => (flag::DF, false),
                _ => (flag::DF, true),
            };
            l.set_control(mask, on);
            l.tick_k(u32::from(cost.flag_op));
            Flow::Next
        }
        Op::Sti => {
            l.begin(u);
            let enabled = l.bit(flag::IF);
            let was_clear = l.e.not(enabled);
            l.set_control(flag::IF, true);
            l.tick_k(u32::from(cost.flag_op));
            Flow::Inhibit(was_clear)
        }
        Op::Unary(op) => {
            unary(l, u, op);
            Flow::Next
        }
        Op::IncRm | Op::DecRm => {
            let at = place(l, u, word, true);
            l.begin(u);
            locate(l, at);
            let value = load(l, at, word);
            let result = l.inc_dec(value, word, insn.op == Op::DecRm);
            store(l, at, word, result);
            cost_rm(l, at, cost.inc_r, cost.inc_m);
            Flow::Next
        }
        Op::CallRm | Op::JmpRm => {
            let at = place(l, u, word, false);
            let slot = (insn.op == Op::CallRm).then(|| stack_slot(l, 1, true));
            l.begin(u);
            locate(l, at);
            let target = load(l, at, word);
            if let Some(slot) = slot {
                push(l, slot, k(u.next));
                cost_rm(l, at, cost.call_r, cost.call_m);
            } else {
                cost_rm(l, at, cost.jmp_r, cost.jmp_m);
            }
            Flow::Jump {
                cs: None,
                ip: target,
            }
        }
        Op::CallFarRm | Op::JmpFarRm => {
            let far = far_pointer(l, u);
            let call = insn.op == Op::CallFarRm;
            let slots = call.then(|| (stack_slot(l, 1, true), stack_slot(l, 2, true)));
            l.begin(u);
            let (offset, segment) = load_far(l, far);
            if let Some((a, b)) = slots {
                push(l, a, k(l.cs));
                push(l, b, k(u.next));
            }
            l.tick_k(u32::from(if call {
                cost.call_far_m
            } else {
                cost.jmp_far_m
            }));
            Flow::Jump {
                cs: Some(segment),
                ip: offset,
            }
        }
        Op::PushRm => {
            let at = place(l, u, word, false);
            let slot = stack_slot(l, 1, true);
            l.begin(u);
            locate(l, at);
            let value = load(l, at, word);
            let value = if matches!(at, Place::Reg(r) if usize::from(r) == gpr::SP) && word {
                let down = l.e.sub(I32, value, V::K(2));
                l.e.and(I32, down, V::K(0xFFFF))
            } else {
                value
            };
            push(l, slot, value);
            cost_rm(l, at, cost.push_reg, cost.push_mem);
            Flow::Next
        }
        Op::Pusha => {
            let mut slots = [None; 8];
            for (slot, down) in slots.iter_mut().zip(1..) {
                *slot = Some(stack_slot(l, down, true));
            }
            l.begin(u);
            let sp = l.gpr(gpr::SP);
            let order = [
                gpr::AX,
                gpr::CX,
                gpr::DX,
                gpr::BX,
                gpr::SP,
                gpr::BP,
                gpr::SI,
                gpr::DI,
            ];
            for (slot, index) in slots.into_iter().flatten().zip(order) {
                let value = if index == gpr::SP { sp } else { l.gpr(index) };
                push(l, slot, value);
            }
            l.tick_k(u32::from(cost.pusha));
            Flow::Next
        }
        Op::Popa => {
            let mut slots = [None; 8];
            for (slot, up) in slots.iter_mut().zip(0..) {
                *slot = Some(stack_word(l, up));
            }
            l.begin(u);
            let order = [
                gpr::DI,
                gpr::SI,
                gpr::BP,
                gpr::SP,
                gpr::BX,
                gpr::DX,
                gpr::CX,
                gpr::AX,
            ];
            for (slot, index) in slots.into_iter().flatten().zip(order) {
                let value = pop(l, slot);
                if index != gpr::SP {
                    l.set_gpr(index, value);
                }
            }
            l.tick_k(u32::from(cost.popa));
            Flow::Next
        }
        Op::PushImm => {
            let slot = stack_slot(l, 1, true);
            l.begin(u);
            push(l, slot, k(insn.imm));
            l.tick_k(u32::from(cost.push_imm));
            Flow::Next
        }
        Op::ImulImm => {
            let at = place(l, u, true, false);
            l.begin(u);
            locate(l, at);
            let a = load(l, at, true);
            let flags = l.word();
            let [product, flags, _, _] = l.call(helper::IMUL_IMM, &[a, k(insn.imm), flags]);
            l.set_gpr(usize::from(insn.reg), product);
            l.set_word(flags);
            cost_rm(l, at, cost.imul_imm_r, cost.imul_imm_m);
            Flow::Next
        }
        Op::Leave => {
            let bp = l.gpr(gpr::BP);
            let ss = l.seg(Seg::Ss);
            let slot = l.access(ss, bp, true, false);
            l.begin(u);
            l.set_gpr(gpr::SP, bp);
            let value = pop(l, slot);
            l.set_gpr(gpr::BP, value);
            l.tick_k(u32::from(cost.leave));
            Flow::Next
        }
        // `takes` refuses the rest; one it let through is still the
        // interpreter's.
        _ => {
            l.refuse();
            return true;
        }
    };
    let ends = !matches!(flow, Flow::Next);
    l.end(u, flow);
    ends
}

/// A conditional transfer to `u`'s relative target where the 32-bit
/// `taken`, costing the first of `costs`, else the second: the block ends on
/// either path.
fn branch(l: &mut Lower, u: &Unit, taken: V, (yes, no): (u16, u16)) {
    l.commit_state();
    let saved = l.save();
    let other = l.fork(taken);
    l.tick_k(u32::from(yes));
    l.end(
        u,
        Flow::Jump {
            cs: None,
            ip: relative(u),
        },
    );
    l.resume(other, &saved);
    l.tick_k(u32::from(no));
    l.end(u, Flow::Fall);
}

/// A far pointer's two words, checked: at the operand, or, for a register
/// operand, the second at the 8086's last address and the first its last
/// operand.
#[derive(Clone, Copy, Debug)]
struct Far {
    first: Option<(Access, V, Mem)>,
    second: Access,
}

fn far_pointer(l: &mut Lower, u: &Unit) -> Far {
    let (seg, at) = match u.insn.ea {
        Ea::Mem(mem) => {
            let seg = u.pending().segment.unwrap_or(mem.base.default_segment());
            (seg, Some((offset_of(l, mem), mem)))
        }
        Ea::Reg(_) => (u.pending().segment.unwrap_or(Seg::Ds), None),
    };
    let base = l.seg(seg);
    let low = match at {
        Some((offset, _)) => offset,
        None => l.ind(),
    };
    let up = l.e.add(I32, low, V::K(2));
    let up = l.e.and(I32, up, V::K(0xFFFF));
    let first = at.map(|(offset, mem)| (l.access(base, offset, true, false), offset, mem));
    let second = l.access(base, up, true, false);
    Far { first, second }
}

/// The far pointer's offset and segment, as `Cpu::load_far_pointer` reads
/// them.
fn load_far(l: &mut Lower, far: Far) -> (V, V) {
    let offset = match far.first {
        Some((access, offset, mem)) => {
            l.set_ind(offset);
            l.tick_k(timing::ea(l.model.family(), mem));
            l.read(access, true)
        }
        None => l.opr(),
    };
    let segment = l.read(far.second, true);
    (offset, segment)
}

fn string(l: &mut Lower, u: &Unit) {
    let insn = u.insn;
    let word = insn.w;
    let cost = costs(l);
    let source = |l: &mut Lower| {
        let si = l.gpr(gpr::SI);
        let base = l.seg(u.pending().segment.unwrap_or(Seg::Ds));
        l.access(base, si, word, false)
    };
    let dest = |l: &mut Lower, store: bool| {
        let di = l.gpr(gpr::DI);
        let es = l.seg(Seg::Es);
        l.access(es, di, word, store)
    };
    let (n, si_moves, di_moves) = match insn.op {
        Op::Movs => {
            let (from, to) = (source(l), dest(l, true));
            l.begin(u);
            let value = l.read(from, word);
            l.write(to, word, value);
            (cost.movs, true, true)
        }
        Op::Cmps => {
            let (from, to) = (source(l), dest(l, false));
            l.begin(u);
            let a = l.read(from, word);
            let b = l.read(to, word);
            l.arith(true, (a, b, V::K(0)), word);
            (cost.cmps, true, true)
        }
        Op::Scas => {
            let to = dest(l, false);
            l.begin(u);
            let b = l.read(to, word);
            let a = reg(l, 0, word);
            l.arith(true, (a, b, V::K(0)), word);
            (cost.scas, false, true)
        }
        Op::Lods => {
            let from = source(l);
            l.begin(u);
            let value = l.read(from, word);
            set_reg(l, 0, word, value);
            (cost.lods, true, false)
        }
        _ => {
            let to = dest(l, true);
            l.begin(u);
            let value = reg(l, 0, word);
            l.write(to, word, value);
            (cost.stos, false, true)
        }
    };
    let step = if word { 2 } else { 1 };
    let down = l.bit(flag::DF);
    let delta = l.e.select(I32, down, V::K(0x1_0000 - step), V::K(step));
    for (moves, index) in [(si_moves, gpr::SI), (di_moves, gpr::DI)] {
        if moves {
            bump(l, index, delta);
        }
    }
    l.tick_k(u32::from(n));
}

fn shift(l: &mut Lower, u: &Unit, op: Shift, count: Count) {
    let insn = u.insn;
    let word = insn.w;
    let cost = costs(l);
    let at = place(l, u, word, true);
    l.begin(u);
    let raw = match count {
        Count::One => V::K(1),
        Count::Cl => reg(l, 1, false),
        Count::Imm => k(insn.imm & 0xFF),
    };
    let n = if l.model.masks_shift_count() {
        l.e.and(I32, raw, V::K(0x1F))
    } else {
        raw
    };
    locate(l, at);
    let value = load(l, at, word);
    if n != V::K(0) {
        let code = helper::shift_code(op)
            | u64::from(word) << 3
            | helper::family_code(l.model.family()) << 4;
        let flags = l.word();
        let [result, flags, _, _] = l.call(helper::SHIFT, &[value, n, flags, V::K(code)]);
        l.set_word(flags);
        match at {
            Place::Mem { access, .. } => {
                let made = l.e.cmp(I32, IrCond::Ne, n, V::K(0));
                let opr = l.opr();
                let opr = l.e.select(I32, made, result, opr);
                l.set_opr(opr);
                l.write_if(access, word, result, made);
            }
            Place::Reg(reg_at) => set_reg(l, reg_at, word, result),
        }
    }
    let mem = matches!(at, Place::Mem { .. });
    if count == Count::One {
        l.tick_k(u32::from(if mem { cost.shift1_m } else { cost.shift1_r }));
    } else {
        l.tick_k(u32::from(if mem { cost.shiftn_m } else { cost.shiftn_r }));
        let steps = l.e.bin(I32, BinOp::Mul, n, k(cost.shift_step));
        l.tick(steps);
    }
}

fn unary(l: &mut Lower, u: &Unit, op: Unary) {
    let insn = u.insn;
    let word = insn.w;
    let cost = costs(l);
    let at = place(l, u, word, matches!(op, Unary::Not | Unary::Neg));
    l.begin(u);
    locate(l, at);
    let value = load(l, at, word);
    let mem = matches!(at, Place::Mem { .. });
    let slot = usize::from(word) + if mem { 2 } else { 0 };
    match op {
        Unary::Test => {
            let r = l.e.and(I32, value, k(insn.imm));
            l.logic(r, word);
            cost_rm(l, at, cost.test_ri, cost.test_mi);
        }
        Unary::Not => {
            let inverted = l.e.xor(I32, value, mask(word));
            store(l, at, word, inverted);
            cost_rm(l, at, cost.neg_r, cost.neg_m);
        }
        Unary::Neg => {
            let result = l.arith(true, (V::K(0), value, V::K(0)), word);
            store(l, at, word, result);
            cost_rm(l, at, cost.neg_r, cost.neg_m);
        }
        Unary::Mul | Unary::Imul | Unary::Div | Unary::Idiv => {
            let signed = matches!(op, Unary::Imul | Unary::Idiv);
            let rep = u.pending().rep.is_some();
            let code = u64::from(word)
                | u64::from(signed) << 1
                | u64::from(rep) << 2
                | helper::family_code(l.model.family()) << 3;
            let index = if matches!(op, Unary::Mul | Unary::Imul) {
                helper::MUL
            } else {
                helper::DIV
            };
            let ax = l.gpr(gpr::AX);
            // A byte operation neither reads nor sets DX.
            let dx = if word { l.gpr(gpr::DX) } else { V::K(0) };
            let flags = l.word();
            let [ax, dx, flags, extra] = l.call(index, &[ax, dx, value, flags, V::K(code)]);
            if index == helper::DIV {
                let error = l.e.shr(I32, extra, 15);
                l.refuse_if(error);
            }
            l.set_gpr(gpr::AX, ax);
            if word {
                l.set_gpr(gpr::DX, dx);
            }
            l.set_word(flags);
            let base = match op {
                Unary::Mul => cost.mul[slot],
                Unary::Imul => cost.imul[slot],
                Unary::Div => cost.div[slot],
                _ => cost.idiv[slot],
            };
            l.tick_k(u32::from(base));
            l.tick(extra);
        }
    }
}
