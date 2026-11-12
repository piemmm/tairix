//! A model of the x86_64 instructions the backend emits, run on their own
//! terms — `div` refusing a zero divisor and an overflowing quotient, `cmov`
//! writing its destination either way, the flags only where an instruction
//! defines them, a call leaving the caller-saved registers and everything
//! below the stack pointer undefined — so lowered code can be held to the IR
//! interpreter. A register the code reads before anything defined it, flags
//! read where no instruction left them defined, a stack misaligned or a
//! frame record missing at a call, and a callee-saved register not restored
//! are each a fault: what a real machine would leave to chance, the model
//! refuses.

use super::inst::{reg, Alu, Bytes, Cc, Count, Inst, Shift, ShiftX, Src};
use super::ARGS;
use crate::bits::{low16, low32, low64, low8, sext16, sext32, sext8};
use crate::interp::Memory;
use crate::ir::Width;
use crate::model::{Calls, Fault, Places, Space, GARBAGE, RETURN, SAVED_SEED, STACK_TOP};

#[allow(clippy::struct_excessive_bools)] // The four flags the code reads, a bit each.
#[derive(Clone, Copy, Debug)]
struct Flags {
    cf: bool,
    zf: bool,
    sf: bool,
    of: bool,
}

struct Machine<'a> {
    regs: [Option<u64>; 16],
    flags: Option<Flags>,
    space: Space<'a>,
}

impl Machine<'_> {
    fn read(&self, r: u8) -> Result<u64, Fault> {
        self.regs[usize::from(r)].ok_or(Fault::Undefined(r))
    }

    fn write(&mut self, r: u8, value: u64) {
        self.regs[usize::from(r)] = Some(value);
    }

    fn load(&mut self, addr: u64, bytes: Bytes) -> Result<u64, Fault> {
        self.space.load(addr, width_of(bytes))
    }

    fn store(&mut self, addr: u64, bytes: Bytes, value: u64) -> Result<(), Fault> {
        self.space.store(addr, width_of(bytes), value)
    }

    fn holds(&self, cc: Cc) -> Result<bool, Fault> {
        let f = self.flags.ok_or(Fault::Flags)?;
        Ok(match cc {
            Cc::B => f.cf,
            Cc::Ae => !f.cf,
            Cc::E => f.zf,
            Cc::Ne => !f.zf,
            Cc::Be => f.cf || f.zf,
            Cc::A => !f.cf && !f.zf,
            Cc::L => f.sf != f.of,
            Cc::Ge => f.sf == f.of,
            Cc::Le => f.zf || f.sf != f.of,
            Cc::G => !f.zf && f.sf == f.of,
        })
    }
}

const fn width_of(bytes: Bytes) -> Width {
    match bytes {
        Bytes::B1 => Width::W8,
        Bytes::B2 => Width::W16,
        Bytes::B4 => Width::W32,
        Bytes::B8 => Width::W64,
    }
}

/// `value` cut to the operation's width.
const fn cut(wide: bool, value: u64) -> u64 {
    if wide {
        value
    } else {
        value & 0xFFFF_FFFF
    }
}

/// The flags of `result` of the operation's width: its sign and zero.
const fn sign(wide: bool, result: u64) -> bool {
    if wide {
        result >> 63 == 1
    } else {
        result >> 31 & 1 == 1
    }
}

/// The flags an addition or subtraction of the width leaves.
fn arith(wide: bool, a: u64, b: u64, subtract: bool) -> (u64, Flags) {
    let (a, b) = (cut(wide, a), cut(wide, b));
    let (result, cf) = if subtract {
        (cut(wide, a.wrapping_sub(b)), b > a)
    } else {
        let full = u128::from(a) + u128::from(b);
        let limit = if wide {
            u128::from(u64::MAX)
        } else {
            u128::from(u32::MAX)
        };
        (cut(wide, a.wrapping_add(b)), full > limit)
    };
    let (sa, sb, sr) = (sign(wide, a), sign(wide, b), sign(wide, result));
    let of = if subtract {
        sa != sb && sr != sa
    } else {
        sa == sb && sr != sa
    };
    (
        result,
        Flags {
            cf,
            zf: result == 0,
            sf: sr,
            of,
        },
    )
}

fn logic(wide: bool, result: u64) -> Flags {
    Flags {
        cf: false,
        zf: cut(wide, result) == 0,
        sf: sign(wide, result),
        of: false,
    }
}

/// Run `insts` as a function called with `args` and `RSP` at the model's
/// stack, at most `fuel` instructions; where it `answers`, its answer in
/// RAX.
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
        regs: [None; 16],
        flags: None,
        space: Space::new(memory),
    };
    // The convention's: the allocated ones and the frame pointer.
    let mut saved = [reg::RBP; super::CALLEE_SAVED.len() + 1];
    saved[1..].copy_from_slice(&super::CALLEE_SAVED);
    for (n, &r) in saved.iter().enumerate() {
        m.write(r, SAVED_SEED + n as u64);
    }
    for (&r, &arg) in ARGS.iter().zip(args) {
        m.write(r, arg);
    }
    // The return address, pushed by the call that entered.
    let entry_sp = STACK_TOP - 8;
    m.write(reg::RSP, entry_sp);
    m.store(entry_sp, Bytes::B8, RETURN)?;
    let mut pc = 0usize;
    loop {
        fuel = fuel.checked_sub(1).ok_or(Fault::Fuel)?;
        let inst = *insts.get(pc).ok_or(Fault::Unbound)?;
        pc += 1;
        match inst {
            Inst::Mov { wide, dst, src } => {
                let value = m.read(src)?;
                m.write(dst, cut(wide, value));
            }
            Inst::MovImm { wide, dst, imm } => m.write(dst, cut(wide, imm)),
            Inst::Alu {
                op: Alu::Xor | Alu::Sub,
                wide,
                dst,
                src: Src::Reg(src),
            } if src == dst => {
                // The idiom that zeroes a register reads none of it.
                m.write(dst, 0);
                m.flags = Some(logic(wide, 0));
            }
            Inst::Alu { op, wide, dst, src } => {
                let a = m.read(dst)?;
                let b = match src {
                    Src::Reg(r) => m.read(r)?,
                    Src::Imm(imm) => i64::from(imm).cast_unsigned(),
                };
                let (result, flags) = match op {
                    Alu::Add => arith(wide, a, b, false),
                    Alu::Sub | Alu::Cmp => arith(wide, a, b, true),
                    Alu::And => (cut(wide, a & b), logic(wide, a & b)),
                    Alu::Or => (cut(wide, a | b), logic(wide, a | b)),
                    Alu::Xor => (cut(wide, a ^ b), logic(wide, a ^ b)),
                };
                m.flags = Some(flags);
                if op != Alu::Cmp {
                    m.write(dst, result);
                }
            }
            Inst::Imul { wide, dst, src } => {
                let value = m.read(dst)?.wrapping_mul(m.read(src)?);
                m.write(dst, cut(wide, value));
                m.flags = None;
            }
            Inst::Shift {
                op,
                wide,
                dst,
                count,
            } => {
                let mask = if wide { 63 } else { 31 };
                let n = match count {
                    Count::Cl => low32(m.read(reg::RCX)?) & mask,
                    Count::Imm(n) => u32::from(n) & mask,
                };
                let a = cut(wide, m.read(dst)?);
                let bits = if wide { 64 } else { 32 };
                let value = match op {
                    Shift::Shl => a << n,
                    Shift::Shr => a >> n,
                    Shift::Sar if wide => (a.cast_signed() >> n).cast_unsigned(),
                    Shift::Sar => (sext32(a) >> n).cast_unsigned(),
                    Shift::Rol | Shift::Ror if n == 0 => a,
                    Shift::Rol => (a << n) | (a >> (bits - n)),
                    Shift::Ror => (a >> n) | (a << (bits - n)),
                };
                m.write(dst, cut(wide, value));
                m.flags = None;
            }
            Inst::ShiftX {
                op,
                wide,
                dst,
                src,
                count,
            } => {
                let n = low32(m.read(count)?) & if wide { 63 } else { 31 };
                let a = cut(wide, m.read(src)?);
                let value = match op {
                    ShiftX::Shl => a << n,
                    ShiftX::Shr => a >> n,
                    ShiftX::Sar if wide => (a.cast_signed() >> n).cast_unsigned(),
                    ShiftX::Sar => (sext32(a) >> n).cast_unsigned(),
                };
                m.write(dst, cut(wide, value));
            }
            Inst::Rorx {
                wide,
                dst,
                src,
                imm,
            } => {
                let a = cut(wide, m.read(src)?);
                let value = if wide {
                    a.rotate_right(u32::from(imm))
                } else {
                    u64::from(low32(a).rotate_right(u32::from(imm)))
                };
                m.write(dst, value);
            }
            Inst::Neg { wide, dst } => {
                let value = m.read(dst)?.wrapping_neg();
                m.write(dst, cut(wide, value));
                m.flags = None;
            }
            Inst::Not { wide, dst } => {
                let value = !m.read(dst)?;
                m.write(dst, cut(wide, value));
            }
            Inst::Test { wide, a, b } => {
                let value = m.read(a)? & m.read(b)?;
                m.flags = Some(logic(wide, value));
            }
            Inst::Movzx { from, dst, src } => {
                let value = m.read(src)?;
                m.write(
                    dst,
                    if from == Bytes::B1 {
                        u64::from(low8(value))
                    } else {
                        u64::from(low16(value))
                    },
                );
            }
            Inst::Movsx {
                from,
                wide,
                dst,
                src,
            } => {
                let value = m.read(src)?;
                let extended = match from {
                    Bytes::B1 => sext8(value),
                    Bytes::B2 => sext16(value),
                    Bytes::B4 | Bytes::B8 => sext32(value),
                };
                m.write(
                    dst,
                    cut(wide || from == Bytes::B4, extended.cast_unsigned()),
                );
            }
            Inst::Setcc { cc, dst } => {
                let held = m.regs[usize::from(dst)].unwrap_or(GARBAGE);
                let bit = u64::from(m.holds(cc)?);
                m.write(dst, (held & !0xFF) | bit);
            }
            Inst::Cmov { cc, wide, dst, src } => {
                let value = if m.holds(cc)? {
                    m.read(src)?
                } else {
                    m.read(dst)?
                };
                m.write(dst, cut(wide, value));
            }
            Inst::Div { signed, wide, src } => {
                let divisor = cut(wide, m.read(src)?);
                let (high, low) = (cut(wide, m.read(reg::RDX)?), cut(wide, m.read(reg::RAX)?));
                let (q, r) = divide(signed, wide, (high, low), divisor)?;
                m.write(reg::RAX, q);
                m.write(reg::RDX, r);
                m.flags = None;
            }
            Inst::SignExtend { wide } => {
                let a = m.read(reg::RAX)?;
                let high = if sign(wide, a) {
                    cut(wide, u64::MAX)
                } else {
                    0
                };
                m.write(reg::RDX, high);
            }
            Inst::Load {
                bytes,
                signed,
                wide,
                dst,
                base,
                disp,
            } => {
                let addr = m.read(base)?.wrapping_add(i64::from(disp).cast_unsigned());
                let raw = m.load(addr, bytes)?;
                let value = match (bytes, signed) {
                    (Bytes::B1, true) => cut(wide, sext8(raw).cast_unsigned()),
                    (Bytes::B2, true) => cut(wide, sext16(raw).cast_unsigned()),
                    (Bytes::B4, true) if wide => sext32(raw).cast_unsigned(),
                    _ => raw,
                };
                m.write(dst, value);
            }
            Inst::Store {
                bytes,
                src,
                base,
                disp,
            } => {
                let addr = m.read(base)?.wrapping_add(i64::from(disp).cast_unsigned());
                let value = m.read(src)?;
                m.store(addr, bytes, value)?;
            }
            Inst::StoreImm {
                bytes,
                imm,
                base,
                disp,
            } => {
                let addr = m.read(base)?.wrapping_add(i64::from(disp).cast_unsigned());
                m.store(addr, bytes, i64::from(imm).cast_unsigned())?;
            }
            Inst::Push(src) => {
                let value = m.read(src)?;
                let sp = m.read(reg::RSP)?.wrapping_sub(8);
                m.write(reg::RSP, sp);
                m.store(sp, Bytes::B8, value)?;
            }
            Inst::Pop(dst) => {
                let sp = m.read(reg::RSP)?;
                let value = m.load(sp, Bytes::B8)?;
                m.write(reg::RSP, sp.wrapping_add(8));
                m.write(dst, value);
            }
            Inst::Call(target) => {
                let target = m.read(target)?;
                let sp = m.read(reg::RSP)?;
                if !sp.is_multiple_of(16) {
                    return Err(Fault::Misaligned);
                }
                // A walk from the callee finds this function's record: the
                // caller's frame pointer, then where this one returns to.
                let record = entry_sp - 8;
                if m.read(reg::RBP)? != record
                    || m.load(record, Bytes::B8)? != SAVED_SEED
                    || m.load(entry_sp, Bytes::B8)? != RETURN
                {
                    return Err(Fault::Frame);
                }
                m.space.scribble_below(sp);
                let mut passed = [None; 6];
                for (slot, &r) in passed.iter_mut().zip(&ARGS) {
                    *slot = m.regs[usize::from(r)];
                }
                let answer = calls.call(target, passed)?;
                // Every register the convention does not keep, but RAX the
                // answer.
                for (r, held) in m.regs.iter_mut().enumerate() {
                    if !saved.iter().any(|&s| usize::from(s) == r) && r != usize::from(reg::RSP) {
                        *held = None;
                    }
                }
                m.write(reg::RAX, answer);
                m.flags = None;
            }
            Inst::Ret => {
                let sp = m.read(reg::RSP)?;
                let to = m.load(sp, Bytes::B8)?;
                if to != RETURN || sp != entry_sp {
                    return Err(Fault::Clobbered(reg::RSP));
                }
                for (n, &r) in saved.iter().enumerate() {
                    if m.regs[usize::from(r)] != Some(SAVED_SEED + n as u64) {
                        return Err(Fault::Clobbered(r));
                    }
                }
                return if answers {
                    m.read(reg::RAX).map(Some)
                } else {
                    Ok(None)
                };
            }
            Inst::Jmp(label) => pc = places.of(label)?,
            Inst::Jcc(cc, label) => {
                if m.holds(cc)? {
                    pc = places.of(label)?;
                }
            }
            Inst::Bind(_) => {}
        }
    }
}

/// `div` or `idiv` of the double-width dividend `high:low` by `divisor`.
fn divide(
    signed: bool,
    wide: bool,
    (high, low): (u64, u64),
    divisor: u64,
) -> Result<(u64, u64), Fault> {
    if divisor == 0 {
        return Err(Fault::Divide);
    }
    let bits = if wide { 64 } else { 32 };
    let dividend = u128::from(high) << bits | u128::from(low);
    if !signed {
        let q = dividend / u128::from(divisor);
        let limit = if wide {
            u128::from(u64::MAX)
        } else {
            u128::from(u32::MAX)
        };
        if q > limit {
            return Err(Fault::Divide);
        }
        let r = dividend % u128::from(divisor);
        return Ok((cut(wide, low64(q)), cut(wide, low64(r))));
    }
    let to_signed = |v: u128| -> i128 {
        if wide {
            v.cast_signed()
        } else {
            // A 64-bit dividend of two 32-bit halves.
            i128::from(low64(v).cast_signed())
        }
    };
    let n = to_signed(dividend);
    let d = if wide {
        i128::from(divisor.cast_signed())
    } else {
        i128::from(sext32(divisor))
    };
    let q = n / d;
    let r = n % d;
    let (lo, hi) = if wide {
        (i128::from(i64::MIN), i128::from(i64::MAX))
    } else {
        (i128::from(i32::MIN), i128::from(i32::MAX))
    };
    if q < lo || q > hi {
        return Err(Fault::Divide);
    }
    Ok((
        cut(wide, low64(q.cast_unsigned())),
        cut(wide, low64(r.cast_unsigned())),
    ))
}
