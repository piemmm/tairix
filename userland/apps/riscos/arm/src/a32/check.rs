//! The encodings the architecture leaves UNPREDICTABLE, as the ARMv7-A
//! architecture manual's decode pseudocode states them for each
//! instruction, with the rules ARMv6 relaxed applied before it.

use super::op::{
    BitfieldOp, BlockOp, ExchangeOp, ExclusiveOp, Index, Instruction, LongOp, MultiplyOp, Offset,
    Op, Operand, Source, TransferOp,
};
use crate::cond::Condition;
use crate::isa::{Feature, Isa};
use crate::reg::Reg;

const PC: Reg = Reg::PC;

fn shifted_by_pc(operand: Operand) -> bool {
    matches!(operand, Operand::Shifted { rm, rs, .. } if rm == PC || rs == PC)
}

fn shifted(operand: Operand) -> bool {
    matches!(operand, Operand::Shifted { .. })
}

/// Whether `instruction`'s encoding is UNPREDICTABLE on `isa`.
#[allow(clippy::too_many_lines)] // One arm per form, as the manual lists them.
pub(super) fn unpredictable(instruction: &Instruction, isa: Isa) -> bool {
    let before_v6 = isa.version() < 6;
    match instruction.op {
        Op::Data {
            rd, rn, operand, ..
        } => shifted(operand) && (rd == PC || rn == PC || shifted_by_pc(operand)),
        Op::Compare {
            rd, rn, operand, ..
        } => {
            let p_form = rd == PC && isa.has(Feature::Modes26);
            (rd.number() != 0 && !p_form)
                || (shifted(operand) && (rn == PC || shifted_by_pc(operand)))
        }
        Op::Move { rd, operand, .. } => shifted(operand) && (rd == PC || shifted_by_pc(operand)),
        Op::Wide { rd, .. } | Op::Status { rd, .. } => rd == PC,
        Op::Multiply {
            op, rd, rn, rm, ra, ..
        } => {
            let early_termination =
                before_v6 && matches!(op, MultiplyOp::Mul | MultiplyOp::Mla) && rd == rn;
            rd == PC || rn == PC || rm == PC || ra == Some(PC) || early_termination
        }
        Op::MultiplyLong {
            op, lo, hi, rn, rm, ..
        } => {
            let early_termination = before_v6
                && matches!(
                    op,
                    LongOp::Umull | LongOp::Umlal | LongOp::Smull | LongOp::Smlal
                )
                && (hi == rn || lo == rn);
            lo == PC || hi == PC || rn == PC || rm == PC || lo == hi || early_termination
        }
        Op::Arithmetic { op, rd, rn, rm } => {
            let checksum = op.name().starts_with("CRC32");
            rd == PC || rn == PC || rm == PC || (checksum && instruction.cond != Condition::Al)
        }
        Op::Parallel { rd, rn, rm, .. } | Op::Pack { rd, rn, rm, .. } => {
            rd == PC || rn == PC || rm == PC
        }
        Op::Unary { rd, rm, .. } | Op::Extend { rd, rm, .. } | Op::Saturate { rd, rn: rm, .. } => {
            rd == PC || rm == PC
        }
        Op::Bitfield {
            op,
            rd,
            rn,
            lsb,
            top,
            ..
        } => {
            rd == PC
                || match op {
                    BitfieldOp::Sbfx | BitfieldOp::Ubfx => {
                        rn == Some(PC) || u32::from(lsb) + u32::from(top) > 31
                    }
                    BitfieldOp::Bfi | BitfieldOp::Bfc => top < lsb,
                }
        }
        Op::StatusWrite { fields, source, .. } => fields == 0 || source == Source::Register(PC),
        Op::Exchange { op, rm } => rm == PC && (op == ExchangeOp::Blx || isa.version() < 7),
        Op::Transfer {
            op,
            rt,
            rn,
            offset,
            index,
            ..
        } => transfer(op, rt, rn, offset, index, before_v6),
        Op::Preload { offset, .. } => matches!(offset, Offset::Register { rm, .. } if rm == PC),
        Op::Block {
            op,
            rn,
            writeback,
            user,
            list,
            ..
        } => {
            let base_listed = writeback && list.contains(rn);
            let unknown_base = match op {
                BlockOp::Ldm => base_listed,
                BlockOp::Stm => base_listed && list.lowest() != Some(rn) && isa.version() < 7,
            };
            let user_bank = user && !(op == BlockOp::Ldm && list.contains(PC));
            rn == PC || list.is_empty() || unknown_base || (user_bank && writeback)
        }
        Op::Swap { rt, rt2, rn, .. } => rt == PC || rt2 == PC || rn == PC || rn == rt || rn == rt2,
        Op::Exclusive { op, rd, rt, rn } => exclusive(op, rd, rt, rn),
        Op::ChangeState {
            imod,
            change_mode,
            interrupts,
            mode,
        } => {
            let enables = imod & 2 != 0;
            (mode != 0 && !change_mode)
                || (enables && interrupts == 0)
                || (!enables && interrupts != 0)
                || (imod == 0 && !change_mode)
                || imod == 1
        }
        Op::ReturnFromException { rn, .. } => rn == PC,
        Op::CoprocessorRegister { op, rt, .. } => {
            rt == PC
                && matches!(
                    op,
                    super::op::CoRegisterOp::Mcr | super::op::CoRegisterOp::Mcr2
                )
        }
        Op::CoprocessorRegisters { op, rt, rt2, .. } => {
            let read = matches!(
                op,
                super::op::CoRegistersOp::Mrrc | super::op::CoRegistersOp::Mrrc2
            );
            rt == PC || rt2 == PC || (read && rt == rt2)
        }
        Op::CoprocessorTransfer { rn, index, .. } => {
            rn == PC
                && !matches!(
                    index,
                    super::op::CoIndex::Offset | super::op::CoIndex::Unindexed
                )
        }
        Op::Branch { .. }
        | Op::Call { .. }
        | Op::Hint { .. }
        | Op::Barrier { .. }
        | Op::ClearExclusive
        | Op::SetEndian { .. }
        | Op::SaveState { .. }
        | Op::Coprocessor { .. } => false,
    }
}

fn transfer(
    op: TransferOp,
    rt: Reg,
    rn: Reg,
    offset: Offset,
    index: Index,
    before_v6: bool,
) -> bool {
    use TransferOp as T;
    let unprivileged = matches!(
        op,
        T::Ldrt | T::Strt | T::Ldrbt | T::Strbt | T::Ldrht | T::Strht | T::Ldrsbt | T::Ldrsht
    );
    let writeback = unprivileged || index != Index::Offset;
    let rm = match offset {
        Offset::Register { rm, .. } => Some(rm),
        Offset::Immediate(_) => None,
    };
    let offset_unpredictable = rm == Some(PC) || (before_v6 && writeback && rm == Some(rn));
    let pair = matches!(op, T::Ldrd | T::Strd);
    let second = rt.next();
    let destination_pc = !matches!(op, T::Ldr | T::Str | T::Strt | T::Ldrd | T::Strd) && rt == PC;
    let base_conflict = if pair {
        writeback && (rn == PC || rn == rt || rn == second)
    } else if unprivileged {
        rn == PC || rn == rt
    } else {
        writeback && (rn == PC || rn == rt)
    };
    let pair_conflict = pair
        && (rt.number() & 1 != 0
            || rt == Reg::LR
            || (op == T::Ldrd && (rm == Some(rt) || rm == Some(second))));
    offset_unpredictable || destination_pc || base_conflict || pair_conflict
}

fn exclusive(op: ExclusiveOp, rd: Option<Reg>, rt: Reg, rn: Reg) -> bool {
    use ExclusiveOp as X;
    let pair = matches!(op, X::Ldrexd | X::Strexd | X::Ldaexd | X::Stlexd);
    let pair_bad = pair && (rt.number() & 1 != 0 || rt == Reg::LR);
    let status_bad =
        rd.is_some_and(|rd| rd == PC || rd == rn || rd == rt || (pair && rd == rt.next()));
    rt == PC || rn == PC || pair_bad || status_bad
}
