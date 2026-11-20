//! The T32 encodings the architecture leaves UNPREDICTABLE, as ARMv8-A's
//! decode pseudocode states them for each encoding. ARMv8 permits R13 where
//! ARMv7 forbade it; executing such an encoding is among the behaviours
//! ARMv7 allowed, so one set of rules serves every profile.

use super::form::{self, Shape};
use super::table::Row;
use super::Position;
use crate::cond::Condition;
use crate::encoding::check;
use crate::encoding::layout::Field;
use crate::instruction::{
    BlockOp, CoIndex, CoTransferOp, CompareOp, DataOp, DualOp, ExchangeOp, Index, Instruction,
    Offset, Op, Operand, TransferOp,
};
use crate::reg::Reg;

const PC: Reg = Reg::PC;

/// Whether `instruction`, decoded by `row`, is UNPREDICTABLE where it
/// stands.
pub(super) fn unpredictable(row: &Row, instruction: &Instruction, position: Position) -> bool {
    let op = &instruction.op;
    placed(row.outside, op, position)
        || if row.wide {
            wide(row, op)
        } else {
            narrow(row, op)
        }
}

/// The rules of the IT block: a condition no `IT` gives, an encoding
/// forbidden in a block (`outside`), and a write to the PC before a block's
/// last instruction.
pub(super) fn placed(outside: bool, op: &Op, position: Position) -> bool {
    match position {
        Position::Outside => false,
        Position::Inside(cond) => cond == Condition::Nv || outside || writes_pc(op),
        Position::Last(cond) => cond == Condition::Nv || outside,
    }
}

/// Whether the instruction branches by writing the PC.
const fn writes_pc(op: &Op) -> bool {
    match *op {
        Op::Data { rd, .. } | Op::Move { rd, .. } => rd.number() == 15,
        Op::Transfer { op, rt, .. } => rt.number() == 15 && !op.stores(),
        Op::Block {
            op: BlockOp::Ldm,
            list,
            ..
        } => list.contains(PC),
        Op::Branch { .. }
        | Op::Exchange { .. }
        | Op::CompareBranch { .. }
        | Op::TableBranch { .. }
        | Op::ReturnFromException { .. } => true,
        _ => false,
    }
}

/// Whether a T32 coprocessor instruction is UNPREDICTABLE: a core register
/// its shared rules forbid, a store from the PC, or a literal load but at an
/// offset.
pub(super) fn coprocessor(op: &Op) -> bool {
    match *op {
        Op::CoprocessorTransfer { op, rn, index, .. } => {
            let store = matches!(op, CoTransferOp::Stc | CoTransferOp::Stc2);
            rn == PC && (store || index != CoIndex::Offset)
        }
        _ => check::shared(op),
    }
}

/// Whether a 16-bit row names high registers: a register field four bits
/// wide.
fn high(row: &Row) -> bool {
    [Field::Rd, Field::Rn]
        .into_iter()
        .any(|field| row.layout.field(field).count_ones() == 4)
}

fn narrow(row: &Row, op: &Op) -> bool {
    match *op {
        Op::Data {
            op: DataOp::Add,
            rn,
            operand: Operand::Register { rm, .. },
            ..
        } => rn == PC && rm == PC,
        Op::Compare {
            op: CompareOp::Cmp,
            rn,
            operand: Operand::Register { rm, .. },
            ..
        } if high(row) => (rn.number() < 8 && rm.number() < 8) || rn == PC || rm == PC,
        Op::Exchange {
            op: ExchangeOp::Blx,
            rm,
        } => rm == PC,
        Op::Block { list, .. } => list.is_empty(),
        Op::ChangeState {
            imod,
            change_mode,
            interrupts,
            mode,
        } => check::change_state(imod, change_mode, interrupts, mode),
        Op::IfThen { firstcond, mask } => {
            firstcond == Condition::Nv || (firstcond == Condition::Al && mask.count_ones() != 1)
        }
        _ => false,
    }
}

/// Whether an operand names the PC, or is a modified immediate the
/// architecture leaves UNPREDICTABLE.
fn operand_unpredictable(operand: Operand) -> bool {
    match operand {
        Operand::Register { rm, .. } => rm == PC,
        Operand::Shifted { rm, rs, .. } => rm == PC || rs == PC,
        Operand::Expanded(immediate) => immediate.unpredictable(),
        Operand::Immediate(_) | Operand::Literal(_) => false,
    }
}

fn offset_pc(offset: Offset) -> bool {
    matches!(offset, Offset::Register { rm, .. } if rm == PC)
}

/// Thumb-2's rule that a register field may not name the PC, with the
/// encodings that may.
#[allow(clippy::too_many_lines)] // One arm per form, as the manual lists them.
fn wide(row: &Row, op: &Op) -> bool {
    match *op {
        Op::SimdFp(op) => check::simdfp(&op, true),
        Op::Data {
            rd, rn, operand, ..
        } => {
            // `SUBS PC, LR` writes the PC and `ADR` reads it by encoding.
            let (destination_field, operand_field) = match row.shape {
                Shape::Data { rd, rn, .. } => (rd == form::Rd::Field, rn == form::Rn::Field),
                _ => (true, true),
            };
            (destination_field && rd == PC)
                || (operand_field && rn == PC)
                || operand_unpredictable(operand)
        }
        Op::Compare { rn, operand, .. } => rn == PC || operand_unpredictable(operand),
        Op::Move { rd, operand, .. } => rd == PC || operand_unpredictable(operand),
        Op::Multiply { rd, rn, rm, ra, .. } => rd == PC || rn == PC || rm == PC || ra == Some(PC),
        Op::MultiplyLong { lo, hi, rn, rm, .. } => {
            lo == PC || hi == PC || rn == PC || rm == PC || lo == hi
        }
        Op::Arithmetic { rd, rn, rm, .. } => rd == PC || rn == PC || rm == PC,
        Op::Wide { .. }
        | Op::Status { .. }
        | Op::Parallel { .. }
        | Op::Pack { .. }
        | Op::Unary { .. }
        | Op::Extend { .. }
        | Op::Saturate { .. }
        | Op::Bitfield { .. }
        | Op::StatusWrite { .. }
        | Op::ReturnFromException { .. } => check::shared(op),
        Op::Coprocessor { .. }
        | Op::CoprocessorRegister { .. }
        | Op::CoprocessorRegisters { .. }
        | Op::CoprocessorTransfer { .. } => coprocessor(op),
        Op::Exchange { rm, .. } | Op::TableBranch { rm, .. } => rm == PC,
        Op::Transfer {
            op,
            rt,
            rn,
            offset,
            index,
            ..
        } => {
            let writeback = index != Index::Offset;
            (rt == PC && op != TransferOp::Ldr) || offset_pc(offset) || (writeback && rn == rt)
        }
        Op::Dual {
            op,
            rt,
            rt2,
            rn,
            index,
            ..
        } => {
            let writeback = index != Index::Offset;
            let base_conflict = writeback && (rn == PC || rn == rt || rn == rt2);
            rt == PC || rt2 == PC || base_conflict || (op == DualOp::Ldrd && rt == rt2)
        }
        Op::Preload { offset, .. } => offset_pc(offset),
        Op::Block {
            op,
            rn,
            writeback,
            list,
            ..
        } => {
            let returns_and_links =
                op == BlockOp::Ldm && list.contains(PC) && list.contains(Reg::LR);
            rn == PC
                || list.len() < 2
                || list.contains(Reg::SP)
                || (writeback && list.contains(rn))
                || returns_and_links
        }
        Op::Exclusive {
            rd, rt, rt2, rn, ..
        } => {
            let status_conflict = rd.is_some_and(|rd| rd == rn || rd == rt || Some(rd) == rt2);
            let loaded_twice = rd.is_none() && rt2 == Some(rt);
            rt == PC
                || rn == PC
                || rt2 == Some(PC)
                || rd == Some(PC)
                || status_conflict
                || loaded_twice
        }
        Op::ChangeState {
            imod,
            change_mode,
            interrupts,
            mode,
        } => check::change_state(imod, change_mode, interrupts, mode),
        Op::Branch { .. }
        | Op::CompareBranch { .. }
        | Op::IfThen { .. }
        | Op::Call { .. }
        | Op::Hint { .. }
        | Op::Barrier { .. }
        | Op::ClearExclusive
        | Op::ExceptionReturn
        | Op::Swap { .. }
        | Op::SetEndian { .. }
        | Op::SaveState { .. }
        | Op::FpaDyadic { .. }
        | Op::FpaMonadic { .. }
        | Op::FpaRegister { .. }
        | Op::FpaCompare { .. }
        | Op::FpaTransfer { .. }
        | Op::FpaMultiple { .. } => false,
    }
}
