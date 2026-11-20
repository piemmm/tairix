//! The loads and stores: single transfers and their addresses, preloads,
//! block transfers and the stack's, swaps, and the exclusive and ordered
//! transfers.

use tairix_riscos_arm::cond::Condition;
use tairix_riscos_arm::instruction::{
    BlockMode, BlockOp, DualOp, ExclusiveOp, Index, Instruction, Offset, Op, PreloadOp, Shift,
    SwapOp, TransferOp,
};
use tairix_riscos_arm::reg::{Reg, RegList};

use super::core::{encoded, offset_shift, start, transfer_op};
use super::mnemonic::Word;
use super::parse::{Parse, Parser, Want};
use crate::messages::Error;

/// What an address's offset may be.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Addressing {
    /// A word's or a byte's: twelve bits, or a register shifted by an
    /// immediate.
    Word,
    /// The rest: eight bits, or a register.
    Half,
}

impl Addressing {
    const fn most(self) -> u16 {
        match self {
            Self::Word => 4095,
            Self::Half => 255,
        }
    }
}

/// An address read.
struct Address {
    rn: Reg,
    up: bool,
    offset: Offset,
    index: Index,
    /// Given as `[Rn]` alone.
    bare: bool,
}

/// An immediate offset's magnitude, `most` at most: zero for a larger one
/// under suppressed errors.
fn magnitude(p: &Parser<'_>, value: u32, most: u16) -> Parse<u16> {
    match u16::try_from(value).ok().filter(|&value| value <= most) {
        Some(value) => Ok(value),
        None if p.context.suppress => Ok(0),
        None => Err(Error::BadOffset.into()),
    }
}

/// An address: in brackets, or an expression the PC reaches.
fn address(p: &mut Parser<'_>, addressing: Addressing, unprivileged: bool) -> Parse<Address> {
    if !p.take(b'[') {
        let target = p.int(Want::Expression)?;
        let distance = target
            .cast_unsigned()
            .wrapping_sub(p.context.address.wrapping_add(8))
            .cast_signed();
        let offset = magnitude(p, distance.unsigned_abs(), addressing.most())?;
        return Ok(Address {
            rn: Reg::PC,
            up: distance >= 0,
            offset: Offset::Immediate(offset),
            index: Index::Offset,
            bare: false,
        });
    }
    let rn = p.register()?;
    if p.take(b']') {
        if p.take(b',') {
            let (up, offset) = offset(p, addressing)?;
            return Ok(Address {
                rn,
                up,
                offset,
                index: Index::PostIndexed,
                bare: false,
            });
        }
        let index = if unprivileged {
            Index::PostIndexed
        } else {
            Index::Offset
        };
        return Ok(Address {
            rn,
            up: true,
            offset: Offset::Immediate(0),
            index,
            bare: true,
        });
    }
    if unprivileged || !p.take(b',') {
        return Err(Error::MissingSquare.into());
    }
    let (up, offset) = offset(p, addressing)?;
    p.expect(b']', Error::MissingSquare)?;
    let index = if p.writeback() {
        Index::PreIndexed
    } else {
        Index::Offset
    };
    Ok(Address {
        rn,
        up,
        offset,
        index,
        bare: false,
    })
}

/// An offset after a comma: `#` and an immediate, its sign the value's, or
/// a register, `-` before it subtracting it.
fn offset(p: &mut Parser<'_>, addressing: Addressing) -> Parse<(bool, Offset)> {
    if p.take(b'#') {
        let value = p.int(Want::Expression)?;
        let offset = magnitude(p, value.unsigned_abs(), addressing.most())?;
        return Ok((value >= 0, Offset::Immediate(offset)));
    }
    let up = !p.take(b'-');
    let rm = p.register()?;
    let shift = if addressing == Addressing::Word && p.take(b',') {
        offset_shift(p)?
    } else {
        Shift::NONE
    };
    Ok((up, Offset::Register { rm, shift }))
}

/// `LDR` and `STR`: the suffix after the condition, then the register and
/// the address.
pub(super) fn transfer(
    p: &mut Parser<'_>,
    word: &mut Word,
    cond: Condition,
    load: bool,
) -> Parse<u32> {
    if word.take("D") {
        return dual(p, word, cond, load);
    }
    let suffix = ["SBT", "SHT", "BT", "HT", "SB", "SH", "B", "H", "T"]
        .into_iter()
        .find(|&s| transfer_op(load, s).is_some() && word.take(s))
        .unwrap_or("");
    let op = transfer_op(load, suffix).ok_or(Error::NoSuchMnemonic)?;
    start(p, word)?;
    let rt = p.register()?;
    p.comma()?;
    let addressing = match op {
        TransferOp::Ldr
        | TransferOp::Str
        | TransferOp::Ldrb
        | TransferOp::Strb
        | TransferOp::Ldrt
        | TransferOp::Strt
        | TransferOp::Ldrbt
        | TransferOp::Strbt => Addressing::Word,
        _ => Addressing::Half,
    };
    let Address {
        rn,
        up,
        offset,
        index,
        ..
    } = address(p, addressing, op.unprivileged())?;
    encoded(
        &Instruction {
            cond,
            op: Op::Transfer {
                op,
                rt,
                rn,
                up,
                offset,
                index,
            },
        },
        0,
    )
}

/// `LDRD` and `STRD`: an even register, and the next one with it.
fn dual(p: &mut Parser<'_>, word: &Word, cond: Condition, load: bool) -> Parse<u32> {
    start(p, word)?;
    let rt = p.register()?;
    if rt.number() % 2 != 0 {
        return Err(Error::BadRegister.into());
    }
    p.comma()?;
    let Address {
        rn,
        up,
        offset,
        index,
        ..
    } = address(p, Addressing::Half, false)?;
    let op = if load { DualOp::Ldrd } else { DualOp::Strd };
    encoded(
        &Instruction {
            cond,
            op: Op::Dual {
                op,
                rt,
                rt2: rt.next(),
                rn,
                up,
                offset,
                index,
            },
        },
        0,
    )
}

/// `PLD`, `PLDW` and `PLI`. BASIC sets `[Rn]`'s pre-index bit, which makes
/// `PLI [Rn]` a `PLD`.
pub(super) fn preload(p: &mut Parser<'_>, word: &Word, op: PreloadOp) -> Parse<u32> {
    start(p, word)?;
    let Address {
        rn,
        up,
        offset,
        index,
        bare,
    } = address(p, Addressing::Word, false)?;
    if index != Index::Offset {
        return Err(Error::Syntax.into());
    }
    let preindexed = if bare { 1 << 24 } else { 0 };
    encoded(
        &Instruction {
            cond: Condition::Al,
            op: Op::Preload { op, rn, up, offset },
        },
        preindexed,
    )
}

/// The block transfer modes, by name, as a load and a store take each.
const BLOCK_MODES: [(&str, BlockMode, BlockMode); 8] = [
    ("IA", BlockMode::IncrementAfter, BlockMode::IncrementAfter),
    ("IB", BlockMode::IncrementBefore, BlockMode::IncrementBefore),
    ("DA", BlockMode::DecrementAfter, BlockMode::DecrementAfter),
    ("DB", BlockMode::DecrementBefore, BlockMode::DecrementBefore),
    ("FD", BlockMode::IncrementAfter, BlockMode::DecrementBefore),
    ("FA", BlockMode::DecrementAfter, BlockMode::IncrementBefore),
    ("ED", BlockMode::IncrementBefore, BlockMode::DecrementAfter),
    ("EA", BlockMode::DecrementBefore, BlockMode::IncrementAfter),
];

/// The block mode next in `word`, as a load or a store takes it.
pub(super) fn block_mode(word: &mut Word, load: bool) -> Option<BlockMode> {
    BLOCK_MODES
        .into_iter()
        .find(|&(name, ..)| word.take(name))
        .map(|(_, loads, stores)| if load { loads } else { stores })
}

/// `LDM` and `STM`: a mode, then the base, its writeback and the list.
pub(super) fn block(
    p: &mut Parser<'_>,
    word: &mut Word,
    cond: Condition,
    load: bool,
) -> Parse<u32> {
    let mode = block_mode(word, load).ok_or(Error::Syntax)?;
    start(p, word)?;
    let rn = p.register()?;
    let writeback = p.writeback();
    p.comma()?;
    let list = RegList(p.register_list()?);
    let user = p.take(b'^');
    let op = if load { BlockOp::Ldm } else { BlockOp::Stm };
    encoded(
        &Instruction {
            cond,
            op: Op::Block {
                op,
                rn,
                mode,
                writeback,
                user,
                list,
            },
        },
        0,
    )
}

/// `PUSH` and `POP`: a block transfer on R13, or for one register a single
/// transfer.
pub(super) fn stack(p: &mut Parser<'_>, word: &Word, cond: Condition, load: bool) -> Parse<u32> {
    start(p, word)?;
    let list = RegList(p.register_list()?);
    let op = match (list.len(), list.lowest()) {
        (1, Some(rt)) if load => Op::Transfer {
            op: TransferOp::Ldr,
            rt,
            rn: Reg::SP,
            up: true,
            offset: Offset::Immediate(4),
            index: Index::PostIndexed,
        },
        (1, Some(rt)) => Op::Transfer {
            op: TransferOp::Str,
            rt,
            rn: Reg::SP,
            up: false,
            offset: Offset::Immediate(4),
            index: Index::PreIndexed,
        },
        _ => Op::Block {
            op: if load { BlockOp::Ldm } else { BlockOp::Stm },
            rn: Reg::SP,
            mode: if load {
                BlockMode::IncrementAfter
            } else {
                BlockMode::DecrementBefore
            },
            writeback: true,
            user: false,
            list,
        },
    };
    encoded(&Instruction { cond, op }, 0)
}

/// `SWP` and, its `B` after the condition, `SWPB`.
pub(super) fn swap(p: &mut Parser<'_>, word: &mut Word, cond: Condition) -> Parse<u32> {
    let op = if word.take("B") {
        SwapOp::Swpb
    } else {
        SwapOp::Swp
    };
    start(p, word)?;
    let rt = p.register()?;
    p.comma()?;
    let rt2 = p.register()?;
    p.comma()?;
    let rn = bracketed(p)?;
    encoded(
        &Instruction {
            cond,
            op: Op::Swap { op, rt, rt2, rn },
        },
        0,
    )
}

/// A base in brackets.
fn bracketed(p: &mut Parser<'_>) -> Parse<Reg> {
    p.expect(b'[', Error::Syntax)?;
    let rn = p.register()?;
    p.expect(b']', Error::MissingSquare)?;
    Ok(rn)
}

/// The exclusive, load-acquire and store-release transfers, their suffixes
/// in their stems.
pub(super) fn exclusive(
    p: &mut Parser<'_>,
    word: &Word,
    cond: Condition,
    op: ExclusiveOp,
) -> Parse<u32> {
    exclusive_operands(p, word, cond, op)
}

/// An exclusive or ordered transfer's operands: the status register a store
/// exclusive writes, the register or even pair, and the base.
pub(super) fn exclusive_operands(
    p: &mut Parser<'_>,
    word: &Word,
    cond: Condition,
    op: ExclusiveOp,
) -> Parse<u32> {
    use ExclusiveOp as E;
    start(p, word)?;
    let status = matches!(
        op,
        E::Strex | E::Strexb | E::Strexh | E::Strexd | E::Stlex | E::Stlexb | E::Stlexh | E::Stlexd
    );
    let pair = matches!(op, E::Ldrexd | E::Strexd | E::Ldaexd | E::Stlexd);
    let rd = if status {
        let rd = p.register()?;
        p.comma()?;
        Some(rd)
    } else {
        None
    };
    let rt = p.register()?;
    p.comma()?;
    let rt2 = if pair {
        if rt.number() % 2 != 0 || rt.number() == 14 {
            return Err(Error::BadRegister.into());
        }
        let rt2 = p.register()?;
        if rt2 != rt.next() {
            return Err(Error::BadRegister.into());
        }
        p.comma()?;
        Some(rt2)
    } else {
        None
    };
    let rn = bracketed(p)?;
    encoded(
        &Instruction {
            cond,
            op: Op::Exclusive {
                op,
                rd,
                rt,
                rt2,
                rn,
                offset: 0,
            },
        },
        0,
    )
}
