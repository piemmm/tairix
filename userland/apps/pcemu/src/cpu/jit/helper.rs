//! The helpers translated code calls: the interpreter's own functions for
//! what it computes in microcode loops — shifts and rotates step by step,
//! multiplication and division, the decimal adjusts — and for the status
//! flags of an operation the code left pending, over packed words, so each
//! is the arithmetic the interpreter does and not a second copy of it.
//! Every helper but [`REFRESH`] is pure: [`call`] answers it.

use crate::cpu::decode::Shift;
use crate::cpu::model::Family;
use crate::cpu::regs::{gpr, Regs};
use crate::cpu::{alu, exec, Cpu};

/// `(frame, cycles)`: the refresh cycles the bus charges at `cycles`, the
/// frame's refresh deadline moved on. The machine's own.
pub const REFRESH: u32 = 0;
/// `(value, count, flags, code)` → `result | flags << 16`, `code` the
/// operation's field, the width at bit 3 and the family at bit 4.
pub const SHIFT: u32 = 1;
/// `(ax, dx, source, flags, code)` → `ax | dx << 16 | flags << 32 |
/// extra cycles << 48`, `code` the width, signedness, a REP prefix and the
/// family from bit 0.
pub const MUL: u32 = 2;
/// As [`MUL`], with bit 63 set where the division would raise a divide
/// error and nothing else is answered.
pub const DIV: u32 = 3;
/// `(ax, flags, which)` → `ax | flags << 16`: DAA, DAS, AAA, AAS.
pub const ADJUST: u32 = 4;
/// `(ax, base, flags, family)` → `ax | flags << 16`.
pub const AAM: u32 = 5;
/// As [`AAM`].
pub const AAD: u32 = 6;
/// `(source, immediate, flags)` → `product | flags << 16`.
pub const IMUL_IMM: u32 = 7;
/// `(code, a, b, carry, flags)` → `flags` after the add, subtract, logic,
/// increment or decrement `code` names over `a`, `b` and `carry`, its width
/// at bit 3.
pub const FLAGS: u32 = 8;

/// Helpers in the table.
pub const COUNT: u32 = 9;

/// The arguments each helper takes, by index.
pub const ARITY: [usize; COUNT as usize] = [2, 4, 5, 5, 3, 4, 4, 3, 5];

/// The bit a division's answer sets where it would raise a divide error.
pub const DIVIDE_ERROR: u64 = 1 << 63;

/// An operation whose status flags the code leaves for [`FLAGS`], or the
/// frame for `Cpu::adopt`, to compute.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Add,
    Sub,
    /// AND, OR, XOR and TEST: `a` is the result.
    Logic,
    /// INC and DEC: an addition or subtraction of one that keeps CF.
    Inc,
    Dec,
}

impl Kind {
    /// Its code, the width at bit 3.
    pub(crate) const fn code(self, word: bool) -> u8 {
        let kind = match self {
            Self::Add => 0,
            Self::Sub => 1,
            Self::Logic => 2,
            Self::Inc => 3,
            Self::Dec => 4,
        };
        kind | if word { 8 } else { 0 }
    }

    const fn of(code: u8) -> Option<Self> {
        Some(match code & 7 {
            0 => Self::Add,
            1 => Self::Sub,
            2 => Self::Logic,
            3 => Self::Inc,
            4 => Self::Dec,
            _ => return None,
        })
    }

    /// Whether it leaves CF as it was.
    pub(crate) const fn keeps_carry(self) -> bool {
        matches!(self, Self::Inc | Self::Dec)
    }
}

/// `flags` after the operation `code` names over `a`, `b` and `carry`; as
/// they are for one naming none, such as `frame::NONE`.
pub(crate) fn status(code: u8, a: u16, b: u16, carry: bool, flags: u16) -> u16 {
    let w = alu::width(code >> 3 & 1 != 0);
    let mut flags = flags;
    match Kind::of(code) {
        Some(Kind::Add) => {
            alu::add(&mut flags, a, b, carry, w);
        }
        Some(Kind::Sub) => {
            alu::sub(&mut flags, a, b, carry, w);
        }
        Some(Kind::Logic) => {
            alu::logic(&mut flags, a, w);
        }
        Some(Kind::Inc) => {
            alu::inc(&mut flags, a, w);
        }
        Some(Kind::Dec) => {
            alu::dec(&mut flags, a, w);
        }
        None => {}
    }
    flags
}

/// A family's code in a helper's argument.
pub(crate) const fn family_code(family: Family) -> u64 {
    match family {
        Family::Intel8086 => 0,
        Family::Intel80186 => 1,
        Family::Nec => 2,
    }
}

const fn family_of(code: u64) -> Family {
    match code & 3 {
        0 => Family::Intel8086,
        1 => Family::Intel80186,
        _ => Family::Nec,
    }
}

const fn shift_of(field: u64) -> Shift {
    match field & 7 {
        0 => Shift::Rol,
        1 => Shift::Ror,
        2 => Shift::Rcl,
        3 => Shift::Rcr,
        4 => Shift::Shl,
        5 => Shift::Shr,
        6 => Shift::Setmo,
        _ => Shift::Sar,
    }
}

/// A shift field's code.
pub(crate) const fn shift_code(op: Shift) -> u64 {
    match op {
        Shift::Rol => 0,
        Shift::Ror => 1,
        Shift::Rcl => 2,
        Shift::Rcr => 3,
        Shift::Shl => 4,
        Shift::Shr => 5,
        Shift::Setmo => 6,
        Shift::Sar => 7,
    }
}

fn half(v: u64) -> u16 {
    let [low, high, ..] = v.to_le_bytes();
    u16::from_le_bytes([low, high])
}

fn byte(v: u64) -> u8 {
    v.to_le_bytes()[0]
}

fn regs(ax: u64, dx: u64, flags: u64) -> Regs {
    let mut regs = Regs::default();
    regs.gpr[gpr::AX] = half(ax);
    regs.gpr[gpr::DX] = half(dx);
    regs.flags = half(flags);
    regs
}

fn packed(regs: &Regs, extra: u32) -> u64 {
    u64::from(regs.gpr[gpr::AX])
        | u64::from(regs.gpr[gpr::DX]) << 16
        | u64::from(regs.flags) << 32
        | u64::from(extra & 0x7FFF) << 48
}

/// What pure helper `index` answers for `args`; none for one it does not
/// have, or [`REFRESH`].
#[must_use]
pub fn call(index: u32, args: &[u64]) -> Option<u64> {
    let arg = |n: usize| args.get(n).copied().unwrap_or(0);
    Some(match index {
        SHIFT => {
            let code = arg(3);
            let mut flags = half(arg(2));
            let w = code >> 3 & 1 != 0;
            let value = half(arg(0));
            let n = byte(arg(1));
            let result = if n == 0 {
                value & if w { 0xFFFF } else { 0xFF }
            } else {
                exec::shifted(
                    &mut flags,
                    family_of(code >> 4),
                    shift_of(code),
                    value,
                    n,
                    w,
                )
            };
            u64::from(result) | u64::from(flags) << 16
        }
        MUL | DIV => {
            let code = arg(4);
            let (w, signed, rep) = (code & 1 != 0, code >> 1 & 1 != 0, code >> 2 & 1 != 0);
            let family = family_of(code >> 3);
            let mut regs = regs(arg(0), arg(1), arg(3));
            let src = half(arg(2));
            let extra = if index == MUL {
                if family == Family::Intel8086 {
                    Cpu::multiply_8086(&mut regs, src, w, signed, rep)
                } else {
                    Cpu::multiply(&mut regs, src, w, signed);
                    exec::multiply_extra(src, w)
                }
            } else if family == Family::Intel8086 {
                match Cpu::divide_8086(&mut regs, src, w, signed, rep) {
                    Ok(extra) => extra,
                    Err(_) => return Some(DIVIDE_ERROR),
                }
            } else if Cpu::divide(&mut regs, family, src, w, signed) {
                0
            } else {
                return Some(DIVIDE_ERROR);
            };
            packed(&regs, extra)
        }
        ADJUST => {
            let mut flags = half(arg(1));
            let ax = half(arg(0));
            let ax = match arg(2) {
                0 => crate::bits::word(
                    alu::daa(&mut flags, crate::bits::lo(ax)),
                    crate::bits::hi(ax),
                ),
                1 => crate::bits::word(
                    alu::das(&mut flags, crate::bits::lo(ax)),
                    crate::bits::hi(ax),
                ),
                2 => alu::aaa(&mut flags, ax),
                _ => alu::aas(&mut flags, ax),
            };
            u64::from(ax) | u64::from(flags) << 16
        }
        AAM | AAD => {
            let mut regs = regs(arg(0), 0, arg(2));
            let base = byte(arg(1));
            if index == AAM {
                exec::aam(&mut regs, family_of(arg(3)), base);
            } else {
                exec::aad(&mut regs, family_of(arg(3)), base);
            }
            u64::from(regs.gpr[gpr::AX]) | u64::from(regs.flags) << 16
        }
        IMUL_IMM => {
            let mut regs = regs(0, 0, arg(2));
            let product = exec::imul_imm(&mut regs, half(arg(0)), half(arg(1)));
            u64::from(product) | u64::from(regs.flags) << 16
        }
        FLAGS => u64::from(status(
            byte(arg(0)),
            half(arg(1)),
            half(arg(2)),
            arg(3) != 0,
            half(arg(4)),
        )),
        _ => return None,
    })
}
