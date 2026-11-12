//! A model of the A64 instructions the backend emits, run on their own
//! terms — the bitfield moves by the architecture's own bitmask decode,
//! `csinc` and the flags only where an instruction defines them, a call
//! leaving X0–X17 and everything below the stack pointer undefined — so
//! lowered code can be held to the IR interpreter. A register read before
//! anything defined it, flags read where nothing left them, a write to the
//! platform register, the stack pointer misaligned where it is used, a frame
//! record missing at a call, and a register the convention keeps not
//! restored are each a fault.

use super::inst::{reg, Cond, Dp2, Inst, Logic, Offset, Size};
use super::{ARGS, CALLEE_SAVED};
use crate::bits::{low32, low64};
use crate::interp::Memory;
use crate::ir::Width;
use crate::model::{Calls, Fault, Places, Space, RETURN, SAVED_SEED, STACK_TOP};
use crate::CompileError;

/// The platform register, and what it holds, which the code must leave
/// alone.
pub(crate) const X18: u8 = 18;
const PLATFORM: u64 = 0x9_1A7F_0000;
/// What the frame pointer holds at entry: the caller's record.
const CALLER_FRAME: u64 = SAVED_SEED + 29;
/// Where a call returns to in the code, which the code never reads.
const INSIDE: u64 = 0x1A51_DE00;

#[allow(clippy::struct_excessive_bools)] // The processor's four flags, a bit each.
#[derive(Clone, Copy, Debug)]
struct Flags {
    n: bool,
    z: bool,
    c: bool,
    v: bool,
}

struct Machine<'a> {
    x: [Option<u64>; 31],
    sp: u64,
    flags: Option<Flags>,
    space: Space<'a>,
}

const fn cut(wide: bool, value: u64) -> u64 {
    if wide {
        value
    } else {
        value & 0xFFFF_FFFF
    }
}

const fn bits(wide: bool) -> u32 {
    if wide {
        64
    } else {
        32
    }
}

const fn width(size: Size) -> Width {
    match size {
        Size::B => Width::W8,
        Size::H => Width::W16,
        Size::W => Width::W32,
        Size::X => Width::W64,
    }
}

/// The low `n` bits all set, for `n` up to 64.
const fn ones(n: u32) -> u64 {
    if n >= 64 {
        u64::MAX
    } else {
        (1 << n) - 1
    }
}

fn ror(value: u64, by: u32, size: u32) -> u64 {
    let by = by % size;
    if by == 0 {
        value
    } else {
        ((value >> by) | (value << (size - by))) & ones(size)
    }
}

fn replicate(element: u64, size: u32) -> u64 {
    let mut out = 0;
    let mut at = 0;
    while at < 64 {
        out |= element << at;
        at += size;
    }
    out
}

/// The architecture's `DecodeBitMasks`: the masks of an operation of
/// `datasize` bits.
#[allow(clippy::similar_names)] // `immr` and `imms` are the architecture's own field names.
fn decode_masks(
    n: bool,
    imms: u32,
    immr: u32,
    immediate: bool,
    datasize: u32,
) -> Option<(u64, u64)> {
    let combined = u32::from(n) << 6 | (!imms & 0x3F);
    if combined == 0 {
        return None;
    }
    let len = combined.ilog2();
    let esize = 1 << len;
    if esize > datasize {
        return None;
    }
    let levels = esize - 1;
    let s = imms & levels;
    let r = immr & levels;
    if immediate && s == levels {
        return None;
    }
    let diff = s.wrapping_sub(r) & levels;
    let welem = ones(s + 1);
    let telem = ones(diff + 1);
    let wmask = cut(datasize == 64, replicate(ror(welem, r, esize), esize));
    let tmask = cut(datasize == 64, replicate(telem, esize));
    Some((wmask, tmask))
}

impl Machine<'_> {
    /// Register `r`, 31 the zero register.
    fn read(&self, r: u8) -> Result<u64, Fault> {
        if r == reg::ZR {
            return Ok(0);
        }
        self.x[usize::from(r)].ok_or(Fault::Undefined(r))
    }

    /// Register `r`, 31 the stack pointer.
    fn read_sp(&self, r: u8) -> Result<u64, Fault> {
        if r == reg::SP {
            Ok(self.sp)
        } else {
            self.read(r)
        }
    }

    /// Set register `r`, 31 the zero register.
    fn write(&mut self, r: u8, wide: bool, value: u64) -> Result<(), Fault> {
        if r == X18 {
            return Err(Fault::Reserved(r));
        }
        if r != reg::ZR {
            self.x[usize::from(r)] = Some(cut(wide, value));
        }
        Ok(())
    }

    /// Set register `r`, 31 the stack pointer.
    fn write_sp(&mut self, r: u8, wide: bool, value: u64) -> Result<(), Fault> {
        if r == reg::SP {
            self.sp = cut(wide, value);
            Ok(())
        } else {
            self.write(r, wide, value)
        }
    }

    fn holds(&self, cond: Cond) -> Result<bool, Fault> {
        let f = self.flags.ok_or(Fault::Flags)?;
        Ok(match cond {
            Cond::Eq => f.z,
            Cond::Ne => !f.z,
            Cond::Hs => f.c,
            Cond::Lo => !f.c,
            Cond::Hi => f.c && !f.z,
            Cond::Ls => !f.c || f.z,
            Cond::Ge => f.n == f.v,
            Cond::Lt => f.n != f.v,
            Cond::Gt => !f.z && f.n == f.v,
            Cond::Le => f.z || f.n != f.v,
        })
    }

    /// An address from base `rn`, the stack pointer where 31, which must
    /// then be aligned to sixteen.
    fn address(&self, rn: u8, offset: Offset, size: Size) -> Result<u64, Fault> {
        let base = self.read_sp(rn)?;
        if rn == reg::SP && base % 16 != 0 {
            return Err(Fault::Misaligned);
        }
        Ok(match offset {
            Offset::Scaled(imm) => base.wrapping_add(u64::from(imm) << size.log2()),
            Offset::Unscaled(imm) => base.wrapping_add(i64::from(imm).cast_unsigned()),
            Offset::Index(rm) => base.wrapping_add(self.read(rm)?),
        })
    }

    fn load(&mut self, addr: u64, size: Size) -> Result<u64, Fault> {
        self.space.load(addr, width(size))
    }

    fn store(&mut self, addr: u64, size: Size, value: u64) -> Result<(), Fault> {
        self.space.store(addr, width(size), value)
    }

    fn set_flags(&mut self, wide: bool, (a, b, sub): (u64, u64, bool), result: u64) {
        let (a, b) = (cut(wide, a), cut(wide, b));
        let top = bits(wide) - 1;
        let sign = |v: u64| v >> top & 1 == 1;
        let c = if sub {
            a >= b
        } else {
            u128::from(a) + u128::from(b) > u128::from(ones(bits(wide)))
        };
        let v = if sub {
            sign(a) != sign(b) && sign(result) != sign(a)
        } else {
            sign(a) == sign(b) && sign(result) != sign(a)
        };
        self.flags = Some(Flags {
            n: sign(result),
            z: result == 0,
            c,
            v,
        });
    }
}

fn divide(op: Dp2, wide: bool, a: u64, b: u64) -> u64 {
    let (a, b) = (cut(wide, a), cut(wide, b));
    if b == 0 {
        return 0;
    }
    if op == Dp2::Udiv {
        return a / b;
    }
    let signed = |v: u64| {
        if wide {
            v.cast_signed()
        } else {
            i64::from(low32(v).cast_signed())
        }
    };
    let quotient = i128::from(signed(a)) / i128::from(signed(b));
    cut(wide, low64(quotient.cast_unsigned()))
}

fn shift(op: Dp2, wide: bool, a: u64, count: u64) -> u64 {
    let size = bits(wide);
    let n = low32(count % u64::from(size));
    let a = cut(wide, a);
    match op {
        Dp2::Lslv => cut(wide, a << n),
        Dp2::Lsrv => a >> n,
        Dp2::Asrv if wide => (a.cast_signed() >> n).cast_unsigned(),
        Dp2::Asrv => cut(
            false,
            (i64::from(low32(a).cast_signed()) >> n).cast_unsigned(),
        ),
        _ => ror(a, n, size),
    }
}

/// Run `insts` as a function called with `args` and the stack pointer at
/// the model's stack, at most `fuel` instructions; where it `answers`, its
/// answer in X0.
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
        x: [None; 31],
        sp: STACK_TOP,
        flags: None,
        space: Space::new(memory),
    };
    for (&r, &arg) in ARGS.iter().zip(args) {
        m.x[usize::from(r)] = Some(arg);
    }
    m.x[usize::from(X18)] = Some(PLATFORM);
    for (seed, &r) in (SAVED_SEED..).zip(&CALLEE_SAVED) {
        m.x[usize::from(r)] = Some(seed);
    }
    m.x[usize::from(reg::FP)] = Some(CALLER_FRAME);
    m.x[usize::from(reg::LR)] = Some(RETURN);
    let mut pc = 0usize;
    loop {
        fuel = fuel.checked_sub(1).ok_or(Fault::Fuel)?;
        let inst = *insts.get(pc).ok_or(Fault::Unbound)?;
        pc += 1;
        match inst {
            Inst::Movz { wide, rd, imm, hw } => m.write(rd, wide, u64::from(imm) << (16 * hw))?,
            Inst::Movn { wide, rd, imm, hw } => {
                m.write(rd, wide, !(u64::from(imm) << (16 * hw)))?;
            }
            Inst::Movk { wide, rd, imm, hw } => {
                let at = 16 * u32::from(hw);
                let kept = m.read(rd)? & !(0xFFFF << at);
                m.write(rd, wide, kept | u64::from(imm) << at)?;
            }
            Inst::AddImm {
                sub,
                flags,
                wide,
                rd,
                rn,
                imm,
                high,
            } => {
                let a = m.read_sp(rn)?;
                let b = u64::from(imm) << if high { 12 } else { 0 };
                let result = cut(
                    wide,
                    if sub {
                        a.wrapping_sub(b)
                    } else {
                        a.wrapping_add(b)
                    },
                );
                if flags {
                    m.set_flags(wide, (a, b, sub), result);
                    m.write(rd, wide, result)?;
                } else {
                    m.write_sp(rd, wide, result)?;
                }
            }
            Inst::AddReg {
                sub,
                flags,
                wide,
                rd,
                rn,
                rm,
            } => {
                let (a, b) = (m.read(rn)?, m.read(rm)?);
                let result = cut(
                    wide,
                    if sub {
                        a.wrapping_sub(b)
                    } else {
                        a.wrapping_add(b)
                    },
                );
                if flags {
                    m.set_flags(wide, (a, b, sub), result);
                }
                m.write(rd, wide, result)?;
            }
            Inst::AddSp { sub, rd, rn, rm } => {
                let (a, b) = (m.read_sp(rn)?, m.read(rm)?);
                m.write_sp(
                    rd,
                    true,
                    if sub {
                        a.wrapping_sub(b)
                    } else {
                        a.wrapping_add(b)
                    },
                )?;
            }
            Inst::Logic {
                op,
                wide,
                rd,
                rn,
                rm,
            } => {
                let (a, b) = (m.read(rn)?, m.read(rm)?);
                let result = cut(
                    wide,
                    match op {
                        Logic::And => a & b,
                        Logic::Orr => a | b,
                        Logic::Eor => a ^ b,
                        Logic::Orn => a | !b,
                    },
                );
                m.write(rd, wide, result)?;
            }
            Inst::LogicImm {
                op,
                wide,
                rd,
                rn,
                mask,
            } => {
                let (imm, _) = decode_masks(
                    mask.n,
                    u32::from(mask.imms),
                    u32::from(mask.immr),
                    true,
                    bits(wide),
                )
                .ok_or(Fault::Compile(CompileError::Malformed))?;
                let a = m.read(rn)?;
                let result = cut(
                    wide,
                    match op {
                        Logic::And => a & imm,
                        Logic::Orr => a | imm,
                        Logic::Eor => a ^ imm,
                        Logic::Orn => return Err(Fault::Compile(CompileError::Malformed)),
                    },
                );
                m.write_sp(rd, wide, result)?;
            }
            Inst::Dp2 {
                op,
                wide,
                rd,
                rn,
                rm,
            } => {
                let (a, b) = (m.read(rn)?, m.read(rm)?);
                let result = match op {
                    Dp2::Udiv | Dp2::Sdiv => divide(op, wide, a, b),
                    _ => shift(op, wide, a, b),
                };
                m.write(rd, wide, result)?;
            }
            Inst::Madd {
                sub,
                wide,
                rd,
                rn,
                rm,
                ra,
            } => {
                let product = m.read(rn)?.wrapping_mul(m.read(rm)?);
                let base = m.read(ra)?;
                m.write(
                    rd,
                    wide,
                    if sub {
                        base.wrapping_sub(product)
                    } else {
                        base.wrapping_add(product)
                    },
                )?;
            }
            Inst::Bfm {
                signed,
                wide,
                rd,
                rn,
                immr,
                imms,
            } => {
                let size = bits(wide);
                let (wmask, tmask) =
                    decode_masks(wide, u32::from(imms), u32::from(immr), false, size)
                        .ok_or(Fault::Compile(CompileError::Malformed))?;
                let src = cut(wide, m.read(rn)?);
                let bot = ror(src, u32::from(immr), size) & wmask;
                let top = if signed && src >> imms & 1 == 1 {
                    ones(size)
                } else {
                    0
                };
                m.write(rd, wide, (top & !tmask) | (bot & tmask))?;
            }
            Inst::Extr {
                wide,
                rd,
                rn,
                rm,
                lsb,
            } => {
                let size = bits(wide);
                let (high, low) = (cut(wide, m.read(rn)?), cut(wide, m.read(rm)?));
                let lsb = u32::from(lsb);
                let result = if lsb == 0 {
                    low
                } else {
                    (low >> lsb) | (high << (size - lsb))
                };
                m.write(rd, wide, result)?;
            }
            Inst::Csel {
                inc,
                wide,
                rd,
                rn,
                rm,
                cond,
            } => {
                let result = if m.holds(cond)? {
                    m.read(rn)?
                } else {
                    m.read(rm)?.wrapping_add(u64::from(inc))
                };
                m.write(rd, wide, result)?;
            }
            Inst::Load {
                size,
                signed,
                wide,
                rt,
                rn,
                offset,
            } => {
                let addr = m.address(rn, offset, size)?;
                let raw = m.load(addr, size)?;
                let from = 8 << size.log2();
                let value = if signed && from < 64 && raw >> (from - 1) & 1 == 1 {
                    cut(wide, raw | !ones(from))
                } else {
                    raw
                };
                m.write(rt, true, value)?;
            }
            Inst::Store {
                size,
                rt,
                rn,
                offset,
            } => {
                let addr = m.address(rn, offset, size)?;
                let value = m.read(rt)?;
                m.store(addr, size, value)?;
            }
            Inst::PushPair { rt, rt2 } => {
                let sp = m.sp.wrapping_sub(16);
                if !sp.is_multiple_of(16) {
                    return Err(Fault::Misaligned);
                }
                let (a, b) = (m.read(rt)?, m.read(rt2)?);
                m.store(sp, Size::X, a)?;
                m.store(sp.wrapping_add(8), Size::X, b)?;
                m.sp = sp;
            }
            Inst::PopPair { rt, rt2 } => {
                let sp = m.sp;
                if !sp.is_multiple_of(16) {
                    return Err(Fault::Misaligned);
                }
                let (a, b) = (m.load(sp, Size::X)?, m.load(sp.wrapping_add(8), Size::X)?);
                m.write(rt, true, a)?;
                m.write(rt2, true, b)?;
                m.sp = sp.wrapping_add(16);
            }
            Inst::Blr(rn) => {
                let target = m.read(rn)?;
                if !m.sp.is_multiple_of(16) {
                    return Err(Fault::Misaligned);
                }
                // A walk from the callee finds this function's record: the
                // caller's frame pointer, then where this one returns to.
                let record = STACK_TOP - 16;
                if m.read(reg::FP)? != record
                    || m.load(record, Size::X)? != CALLER_FRAME
                    || m.load(record + 8, Size::X)? != RETURN
                {
                    return Err(Fault::Frame);
                }
                m.space.scribble_below(m.sp);
                let mut passed = [None; 6];
                for (slot, &r) in passed.iter_mut().zip(&ARGS) {
                    *slot = m.x[usize::from(r)];
                }
                let answer = calls.call(target, passed)?;
                for held in &mut m.x[..=usize::from(reg::X17)] {
                    *held = None;
                }
                m.x[0] = Some(answer);
                m.x[usize::from(reg::LR)] = Some(INSIDE);
                m.flags = None;
            }
            Inst::Ret => {
                if m.read(reg::LR)? != RETURN {
                    return Err(Fault::Clobbered(reg::LR));
                }
                if m.sp != STACK_TOP {
                    return Err(Fault::Clobbered(reg::SP));
                }
                if m.x[usize::from(reg::FP)] != Some(CALLER_FRAME) {
                    return Err(Fault::Clobbered(reg::FP));
                }
                for (seed, &r) in (SAVED_SEED..).zip(&CALLEE_SAVED) {
                    if m.x[usize::from(r)] != Some(seed) {
                        return Err(Fault::Clobbered(r));
                    }
                }
                return if answers {
                    m.read(0).map(Some)
                } else {
                    Ok(None)
                };
            }
            Inst::B(label) => pc = places.of(label)?,
            Inst::BCond(cond, label) => {
                if m.holds(cond)? {
                    pc = places.of(label)?;
                }
            }
            Inst::Cbz { wide, rt, label } => {
                if cut(wide, m.read(rt)?) == 0 {
                    pc = places.of(label)?;
                }
            }
            Inst::Bind(_) => {}
        }
    }
}
