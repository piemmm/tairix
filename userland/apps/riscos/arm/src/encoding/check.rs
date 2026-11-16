//! The UNPREDICTABLE rules both instruction sets state alike, decided by an
//! instruction's operands alone.

use crate::instruction::{BitfieldOp, CoRegisterOp, CoRegistersOp, Op, SimdFp, Source};
use crate::reg::Reg;
use crate::vfp::System;

/// Whether one of the forms both instruction sets judge alike is
/// UNPREDICTABLE: a core register the form may not name, `MSR` of no field,
/// `MRRC` of one register twice, a bitfield past bit 31. Any other form is
/// its instruction set's to judge.
pub(crate) fn shared(op: &Op) -> bool {
    let pc = Reg::PC;
    match *op {
        Op::Wide { rd, .. } | Op::Status { rd, .. } => rd == pc,
        Op::Parallel { rd, rn, rm, .. } | Op::Pack { rd, rn, rm, .. } => {
            rd == pc || rn == pc || rm == pc
        }
        Op::Unary { rd, rm, .. } | Op::Extend { rd, rm, .. } | Op::Saturate { rd, rn: rm, .. } => {
            rd == pc || rm == pc
        }
        Op::Bitfield {
            op,
            rd,
            rn,
            lsb,
            top,
        } => rd == pc || rn == Some(pc) || bitfield(op, lsb, top),
        Op::StatusWrite { fields, source, .. } => fields == 0 || source == Source::Register(pc),
        Op::ReturnFromException { rn, .. } => rn == pc,
        Op::CoprocessorRegister { op, rt, .. } => {
            rt == pc && matches!(op, CoRegisterOp::Mcr | CoRegisterOp::Mcr2)
        }
        Op::CoprocessorRegisters { op, rt, rt2, .. } => {
            let read = matches!(op, CoRegistersOp::Mrrc | CoRegistersOp::Mrrc2);
            rt == pc || rt2 == pc || (read && rt == rt2)
        }
        _ => false,
    }
}

/// Whether a `CPS` is UNPREDICTABLE: a mode given without the change, A, I
/// and F named without an enable or disable or not named with one, or the
/// reserved `imod`.
pub(crate) const fn change_state(imod: u8, change_mode: bool, interrupts: u8, mode: u8) -> bool {
    let changes_interrupts = imod & 2 != 0;
    (mode != 0 && !change_mode)
        || (changes_interrupts && interrupts == 0)
        || (!changes_interrupts && interrupts != 0)
        || (imod == 0 && !change_mode)
        || imod == 1
}

/// Whether a bitfield operation is UNPREDICTABLE: an extraction reaching past
/// bit 31, or an insertion or clear whose top bit is below its bottom.
pub(crate) const fn bitfield(op: BitfieldOp, lsb: u8, top: u8) -> bool {
    match op {
        BitfieldOp::Sbfx | BitfieldOp::Ubfx => lsb as u32 + top as u32 > 31,
        BitfieldOp::Bfi | BitfieldOp::Bfc => top < lsb,
    }
}

/// Whether a floating-point or Advanced SIMD instruction is UNPREDICTABLE:
/// the PC as a core register, the same register twice where two are
/// written, a transfer reaching past the register file, or a PC base where
/// the instruction set forbids one. ARMv8 permits R13.
pub(crate) const fn simdfp(op: &SimdFp, thumb: bool) -> bool {
    match *op {
        SimdFp::CoreSingle { rt, .. }
        | SimdFp::CoreScalar { rt, .. }
        | SimdFp::CoreDuplicate { rt, .. } => is_pc(rt),
        SimdFp::CorePair {
            to_core,
            double,
            m,
            rt,
            rt2,
        } => {
            is_pc(rt)
                || is_pc(rt2)
                || (!double && m == 31)
                || (to_core && rt.number() == rt2.number())
        }
        SimdFp::System { read, register, rt } => {
            is_pc(rt) && !(read && matches!(register, System::Fpscr))
        }
        SimdFp::Transfer { load, rn, .. } => !load && thumb && is_pc(rn),
        SimdFp::Multiple {
            double,
            d,
            rn,
            writeback,
            words,
            ..
        } => {
            let registers = if double { words / 2 } else { words };
            let end = d as u32 + registers as u32;
            (is_pc(rn) && (writeback || thumb))
                || registers == 0
                || (double && registers > 16)
                || end > 32
                || (double && words & 1 != 0 && end > 16)
        }
        SimdFp::Table { length, n, .. } => n as u32 + length as u32 > 32,
        SimdFp::Structure { shape, d, rn, .. } => {
            is_pc(rn)
                || match shape.registers().checked_sub(1) {
                    Some(last) => d as u32 + shape.register(last) > 31,
                    None => true,
                }
        }
        _ => false,
    }
}

const fn is_pc(reg: Reg) -> bool {
    reg.number() == 15
}
