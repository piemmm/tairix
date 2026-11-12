//! A model of the RV64 instructions the backend emits, run on their own
//! terms — the word instructions sign-extending, division by zero giving all
//! ones and its remainder the dividend, a call leaving `ra`, the
//! temporaries, the arguments and everything below the stack pointer
//! undefined — so lowered code can be held to the IR interpreter. A
//! register read before anything defined it, a write to `gp` or `tp`, the
//! stack pointer misaligned or a frame record missing at a call, and a
//! register the convention keeps not restored are each a fault.

use super::inst::{reg, Cond, Ext, ImmOp, Inst, Op, Size};
use super::{ARGS, CALLEE_SAVED};
use crate::bits::{low16, low32, sext16, sext32, sext8};
use crate::interp::Memory;
use crate::ir::Width;
use crate::model::{Calls, Fault, Places, Space, RETURN, SAVED_SEED, STACK_TOP};

/// The global and thread pointers, and what they hold, which the code must
/// leave alone.
pub(crate) const GP: u8 = 3;
const TP: u8 = 4;
const GLOBAL: u64 = 0x6106_A100;
const THREAD: u64 = 0x7E2D_0000;
/// What the frame pointer holds at entry: the caller's.
const CALLER_FRAME: u64 = SAVED_SEED + 8;
/// Where a call returns to in the code, which the code never reads.
const INSIDE: u64 = 0x1A51_DE00;

/// The registers a call may change: `ra`, the temporaries and the
/// arguments.
const CALLER_SAVED: [u8; 16] = [1, 5, 6, 7, 10, 11, 12, 13, 14, 15, 16, 17, 28, 29, 30, 31];

struct Machine<'a> {
    x: [Option<u64>; 32],
    space: Space<'a>,
}

const fn width(size: Size) -> Width {
    match size {
        Size::B => Width::W8,
        Size::H => Width::W16,
        Size::W => Width::W32,
        Size::D => Width::W64,
    }
}

impl Machine<'_> {
    fn read(&self, r: u8) -> Result<u64, Fault> {
        if r == reg::ZERO {
            return Ok(0);
        }
        self.x[usize::from(r)].ok_or(Fault::Undefined(r))
    }

    fn write(&mut self, r: u8, value: u64) -> Result<(), Fault> {
        if r == GP || r == TP {
            return Err(Fault::Reserved(r));
        }
        if r != reg::ZERO {
            self.x[usize::from(r)] = Some(value);
        }
        Ok(())
    }
}

/// A word result, sign-extended where `word`.
fn word_of(word: bool, value: u64) -> u64 {
    if word {
        sext32(value).cast_unsigned()
    } else {
        value
    }
}

fn reg_op(op: Op, word: bool, a: u64, b: u64) -> u64 {
    let (shift, mask) = if word {
        (b & 31, 0xFFFF_FFFF)
    } else {
        (b & 63, u64::MAX)
    };
    let n = low32(shift);
    let signed = |v: u64| if word { sext32(v) } else { v.cast_signed() };
    let (ua, ub) = (a & mask, b & mask);
    let value = match op {
        Op::Add => a.wrapping_add(b),
        Op::Sub => a.wrapping_sub(b),
        Op::Sll => a << n,
        Op::Slt => u64::from(a.cast_signed() < b.cast_signed()),
        Op::Sltu => u64::from(a < b),
        Op::Xor => a ^ b,
        Op::Srl => ua >> n,
        Op::Sra => (signed(a) >> n).cast_unsigned(),
        Op::Or => a | b,
        Op::And => a & b,
        Op::Mul => a.wrapping_mul(b),
        Op::Divu if ub == 0 => u64::MAX,
        Op::Divu => ua / ub,
        Op::Remu if ub == 0 => ua,
        Op::Remu => ua % ub,
        Op::Div if signed(b) == 0 => u64::MAX,
        Op::Div => signed(a).wrapping_div(signed(b)).cast_unsigned(),
        Op::Rem if signed(b) == 0 => signed(a).cast_unsigned(),
        Op::Rem => signed(a).wrapping_rem(signed(b)).cast_unsigned(),
        Op::Rol if word => u64::from(low32(a).rotate_left(n)),
        Op::Ror if word => u64::from(low32(a).rotate_right(n)),
        Op::Rol => a.rotate_left(n),
        Op::Ror => a.rotate_right(n),
    };
    match op {
        // The word forms divide 32-bit values, whose overflow is their own.
        Op::Div | Op::Rem if word => {
            let (sa, sb) = (low32(a).cast_signed(), low32(b).cast_signed());
            let value = match (op, sb) {
                (Op::Div, 0) => -1,
                (Op::Rem, 0) => sa,
                (Op::Div, _) => sa.wrapping_div(sb),
                _ => sa.wrapping_rem(sb),
            };
            i64::from(value).cast_unsigned()
        }
        _ => word_of(word, value),
    }
}

fn imm_op(op: ImmOp, word: bool, a: u64, imm: i16) -> u64 {
    let b = i64::from(imm).cast_unsigned();
    let as_reg = match op {
        ImmOp::Addi => Op::Add,
        ImmOp::Slti => Op::Slt,
        ImmOp::Sltiu => Op::Sltu,
        ImmOp::Xori => Op::Xor,
        ImmOp::Ori => Op::Or,
        ImmOp::Andi => Op::And,
        ImmOp::Slli => Op::Sll,
        ImmOp::Srli => Op::Srl,
        ImmOp::Srai => Op::Sra,
        ImmOp::Rori => Op::Ror,
    };
    reg_op(as_reg, word, a, b)
}

/// `a`'s low bits, extended as `ext` extends them.
fn extended(ext: Ext, a: u64) -> u64 {
    match ext {
        Ext::SextB => sext8(a).cast_unsigned(),
        Ext::SextH => sext16(a).cast_unsigned(),
        Ext::ZextH => u64::from(low16(a)),
    }
}

fn holds(cond: Cond, a: u64, b: u64) -> bool {
    match cond {
        Cond::Eq => a == b,
        Cond::Ne => a != b,
        Cond::Lt => a.cast_signed() < b.cast_signed(),
        Cond::Ge => a.cast_signed() >= b.cast_signed(),
        Cond::Ltu => a < b,
        Cond::Geu => a >= b,
    }
}

/// Run `insts` as a function called with `args` and the stack pointer at
/// the model's stack, at most `fuel` instructions; where it `answers`, its
/// answer in A0.
///
/// # Errors
///
/// The fault that stopped it.
#[allow(clippy::too_many_lines)] // One exhaustive instruction match; splitting it would scatter the model.
pub(crate) fn run(
    insts: &[Inst],
    args: &[u64],
    memory: &mut dyn Memory,
    calls: &mut dyn Calls,
    mut fuel: u64,
    answers: bool,
) -> Result<Option<u64>, Fault> {
    let places = Places::new(insts, |inst| match *inst {
        Inst::Bind(label) => Some(label),
        _ => None,
    });
    let mut m = Machine {
        x: [None; 32],
        space: Space::new(memory),
    };
    for (&r, &arg) in ARGS.iter().zip(args) {
        m.x[usize::from(r)] = Some(arg);
    }
    m.x[usize::from(reg::RA)] = Some(RETURN);
    m.x[usize::from(reg::SP)] = Some(STACK_TOP);
    m.x[usize::from(GP)] = Some(GLOBAL);
    m.x[usize::from(TP)] = Some(THREAD);
    m.x[usize::from(reg::S0)] = Some(CALLER_FRAME);
    for (seed, &r) in (SAVED_SEED..).zip(&CALLEE_SAVED) {
        m.x[usize::from(r)] = Some(seed);
    }
    let mut pc = 0usize;
    loop {
        fuel = fuel.checked_sub(1).ok_or(Fault::Fuel)?;
        let inst = *insts.get(pc).ok_or(Fault::Unbound)?;
        pc += 1;
        match inst {
            Inst::Reg {
                op,
                word,
                rd,
                rs1,
                rs2,
            } => {
                let value = reg_op(op, word, m.read(rs1)?, m.read(rs2)?);
                m.write(rd, value)?;
            }
            Inst::Imm {
                op,
                word,
                rd,
                rs1,
                imm,
            } => {
                let value = imm_op(op, word, m.read(rs1)?, imm);
                m.write(rd, value)?;
            }
            Inst::Lui { rd, imm } => m.write(rd, sext32(u64::from(imm) << 12).cast_unsigned())?,
            Inst::Ext { ext, rd, rs1 } => {
                let value = extended(ext, m.read(rs1)?);
                m.write(rd, value)?;
            }
            Inst::Load {
                size,
                signed,
                rd,
                rs1,
                offset,
            } => {
                let addr = m.read(rs1)?.wrapping_add(i64::from(offset).cast_unsigned());
                let raw = m.space.load(addr, width(size))?;
                let bits = width(size).bits();
                let value = if signed && bits < 64 && raw >> (bits - 1) & 1 == 1 {
                    raw | !0 << bits
                } else {
                    raw
                };
                m.write(rd, value)?;
            }
            Inst::Store {
                size,
                rs2,
                rs1,
                offset,
            } => {
                let addr = m.read(rs1)?.wrapping_add(i64::from(offset).cast_unsigned());
                let value = m.read(rs2)?;
                m.space.store(addr, width(size), value)?;
            }
            Inst::Jalr {
                rd: reg::RA,
                rs1,
                offset: 0,
            } => {
                let target = m.read(rs1)?;
                let sp = m.read(reg::SP)?;
                if !sp.is_multiple_of(16) {
                    return Err(Fault::Misaligned);
                }
                // A walk from the callee finds this function's record, just
                // below the frame pointer: the return address, then the
                // caller's frame pointer.
                if m.read(reg::S0)? != STACK_TOP
                    || m.space.load(STACK_TOP - 8, Width::W64)? != RETURN
                    || m.space.load(STACK_TOP - 16, Width::W64)? != CALLER_FRAME
                {
                    return Err(Fault::Frame);
                }
                m.space.scribble_below(sp);
                let mut passed = [None; 6];
                for (slot, &r) in passed.iter_mut().zip(&ARGS) {
                    *slot = m.x[usize::from(r)];
                }
                let answer = calls.call(target, passed)?;
                for &r in &CALLER_SAVED {
                    m.x[usize::from(r)] = None;
                }
                m.x[usize::from(reg::A0)] = Some(answer);
                m.x[usize::from(reg::RA)] = Some(INSIDE);
            }
            Inst::Jalr {
                rd: reg::ZERO,
                rs1: reg::RA,
                offset: 0,
            } => {
                if m.read(reg::RA)? != RETURN {
                    return Err(Fault::Clobbered(reg::RA));
                }
                if m.read(reg::SP)? != STACK_TOP {
                    return Err(Fault::Clobbered(reg::SP));
                }
                if m.x[usize::from(reg::S0)] != Some(CALLER_FRAME) {
                    return Err(Fault::Clobbered(reg::S0));
                }
                for (seed, &r) in (SAVED_SEED..).zip(&CALLEE_SAVED) {
                    if m.x[usize::from(r)] != Some(seed) {
                        return Err(Fault::Clobbered(r));
                    }
                }
                return if answers {
                    m.read(reg::A0).map(Some)
                } else {
                    Ok(None)
                };
            }
            // No other indirect jump is made.
            Inst::Jalr { .. } => return Err(Fault::Unbound),
            Inst::Branch {
                cond,
                rs1,
                rs2,
                label,
            } => {
                if holds(cond, m.read(rs1)?, m.read(rs2)?) {
                    pc = places.of(label)?;
                }
            }
            Inst::Jump(label) => pc = places.of(label)?,
            Inst::Bind(_) => {}
        }
    }
}
