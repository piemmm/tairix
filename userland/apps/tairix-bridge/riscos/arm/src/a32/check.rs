//! The encodings the architecture leaves UNPREDICTABLE, as the ARMv7-A
//! architecture manual's decode pseudocode states them for each
//! instruction, with the rules ARMv6 relaxed applied before it.

use crate::cond::Condition;
use crate::encoding::check;
use crate::instruction::{
    BlockOp, CoIndex, DualOp, ExchangeOp, Index, Instruction, LongOp, MultiplyOp, Offset, Op,
    Operand, TransferOp,
};
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
        Op::SimdFp(op) => check::simdfp(&op, false),
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
            rd == PC || rn == PC || rm == PC || (op.checksum() && instruction.cond != Condition::Al)
        }
        Op::Wide { .. }
        | Op::Status { .. }
        | Op::Parallel { .. }
        | Op::Pack { .. }
        | Op::Unary { .. }
        | Op::Extend { .. }
        | Op::Saturate { .. }
        | Op::Bitfield { .. }
        | Op::StatusWrite { .. }
        | Op::ReturnFromException { .. }
        | Op::CoprocessorRegister { .. }
        | Op::CoprocessorRegisters { .. } => check::shared(&instruction.op),
        // Only BX may name the PC, and only from ARMv7.
        Op::Exchange { op, rm } => rm == PC && (op != ExchangeOp::Bx || isa.version() < 7),
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
        Op::Exclusive {
            rd, rt, rt2, rn, ..
        } => exclusive(rd, rt, rt2, rn),
        Op::Dual {
            op,
            rt,
            rt2,
            rn,
            offset,
            index,
            ..
        } => {
            let writeback = index != Index::Offset;
            let rm = match offset {
                Offset::Register { rm, .. } => Some(rm),
                Offset::Immediate(_) => None,
            };
            rt.number() & 1 != 0
                || rt == Reg::LR
                || rm == Some(PC)
                || (before_v6 && writeback && rm == Some(rn))
                || (writeback && (rn == PC || rn == rt || rn == rt2))
                || (op == DualOp::Ldrd && (rm == Some(rt) || rm == Some(rt2)))
        }
        Op::ChangeState {
            imod,
            change_mode,
            interrupts,
            mode,
        } => check::change_state(imod, change_mode, interrupts, mode),
        Op::CoprocessorTransfer { rn, index, .. } => {
            rn == PC && !matches!(index, CoIndex::Offset | CoIndex::Unindexed)
        }
        Op::Branch { .. }
        | Op::CompareBranch { .. }
        | Op::IfThen { .. }
        | Op::TableBranch { .. }
        | Op::Call { .. }
        | Op::Hint { .. }
        | Op::Barrier { .. }
        | Op::ClearExclusive
        | Op::ExceptionReturn
        | Op::SetEndian { .. }
        | Op::SaveState { .. }
        | Op::Coprocessor { .. }
        | Op::FpaDyadic { .. }
        | Op::FpaMonadic { .. }
        | Op::FpaRegister { .. }
        | Op::FpaCompare { .. }
        | Op::FpaTransfer { .. }
        | Op::FpaMultiple { .. } => false,
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
    let writeback = index != Index::Offset;
    let rm = match offset {
        Offset::Register { rm, .. } => Some(rm),
        Offset::Immediate(_) => None,
    };
    let offset_unpredictable = rm == Some(PC) || (before_v6 && writeback && rm == Some(rn));
    let destination_pc = !matches!(op, T::Ldr | T::Str | T::Strt) && rt == PC;
    let base_conflict = if op.unprivileged() {
        rn == PC || rn == rt
    } else {
        writeback && (rn == PC || rn == rt)
    };
    offset_unpredictable || destination_pc || base_conflict
}

fn exclusive(rd: Option<Reg>, rt: Reg, rt2: Option<Reg>, rn: Reg) -> bool {
    let pair_bad = rt2.is_some() && (rt.number() & 1 != 0 || rt == Reg::LR);
    let status_bad = rd.is_some_and(|rd| rd == PC || rd == rn || rd == rt || Some(rd) == rt2);
    rt == PC || rn == PC || pair_bad || status_bad
}
