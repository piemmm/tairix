//! How a T32 row's fields become an [`Op`]. Rows whose letters mean what the
//! A32 rows' do are built as those are; the rest have a [`Shape`] saying
//! where their operands come from, and [`fields`] is its inverse.

use crate::encoding::form::Kind;
use crate::encoding::layout::{Field, Fields, Layout};
use crate::instruction::{
    BlockMode, BranchOp, DualOp, Expanded, HintOp, Index, Lanes, Offset, Op, Operand, Prefix,
    Shift, ShiftType,
};
use crate::reg::{Reg, RegList};

/// Where a data-processing row's destination is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Rd {
    /// The `d` field.
    Field,
    /// SP.
    Sp,
    /// The PC.
    Pc,
}

/// Where a data-processing row's first operand is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Rn {
    /// The `n` field.
    Field,
    /// The destination: the 16-bit forms' Rdn.
    Rd,
    /// SP.
    Sp,
    /// The PC, word-aligned: `ADR`.
    Pc,
    /// LR: `SUBS PC, LR, #imm8`.
    Lr,
    /// None: a move.
    None,
}

/// Where a data-processing row's second operand comes from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Source {
    /// Rm, shifted as `y` and `k` say when the row has them.
    Register,
    /// The destination shifted by Rs: the 16-bit register shifts.
    ShiftedRdn(ShiftType),
    /// A modified immediate: `i:imm3:imm8` from `H`, `k` and `i`.
    Expanded,
    /// An unrotated immediate, scaled left by this: `i:imm3:imm8` from `H`,
    /// `k` and `i` where the row has the first two, else `i` alone.
    Literal(u8),
}

/// When a data-processing row sets the flags.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Flags {
    /// As `S` says.
    Field,
    /// Outside an IT block: the 16-bit forms.
    OutsideIt,
    /// Never.
    Never,
    /// Always: `SUBS PC, LR`.
    Always,
}

/// Where a transfer's or preload's base is when its row has no `n` field.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Base {
    /// The `n` field.
    Field,
    /// The PC, word-aligned: literal addressing.
    Pc,
    /// SP.
    Sp,
}

/// The shapes of T32's block transfers.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Block {
    /// `PUSH`: SP, decrementing, LR as the `X` bit.
    Push,
    /// `POP`: SP, incrementing, the PC as the `X` bit.
    Pop,
    /// 16-bit `STM`: incrementing, always writing back.
    Store,
    /// 16-bit `LDM`: incrementing, writing back unless the base is loaded.
    Load,
    /// 32-bit: this mode, writing back as `W` says.
    Wide(BlockMode),
}

/// The shapes of T32's branch offsets.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Branch {
    /// 16-bit conditional: `imm8:'0'`.
    Cond8,
    /// 16-bit: `imm11:'0'`.
    Imm11,
    /// 32-bit conditional: `S:J2:J1:imm6:imm11:'0'`.
    Cond20,
    /// 32-bit `B` and `BL`: `S:I1:I2:imm10:imm11:'0'`, each `I` its `J`
    /// made relative to the sign.
    Imm24,
    /// `BLX`: `S:I1:I2:imm10H:imm10L:'00'`.
    Blx,
}

impl Branch {
    /// The offsets the shape holds: its least and greatest, and the multiple
    /// they are of.
    const fn range(self) -> (i32, i32) {
        match self {
            Self::Cond8 => (-(1 << 8), (1 << 8) - 2),
            Self::Imm11 => (-(1 << 11), (1 << 11) - 2),
            Self::Cond20 => (-(1 << 20), (1 << 20) - 2),
            Self::Imm24 | Self::Blx => (-(1 << 24), (1 << 24) - 2),
        }
    }

    /// Whether the shape is a conditional branch's.
    pub(super) const fn conditional(self) -> bool {
        matches!(self, Self::Cond8 | Self::Cond20)
    }
}

/// How a T32 row's fields become operands.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Shape {
    /// As the A32 rows with the same letters.
    Common,
    /// Data processing.
    Data {
        rd: Rd,
        rn: Rn,
        source: Source,
        flags: Flags,
    },
    /// 16-bit `MUL Rdm, Rn, Rdm`, setting the flags outside an IT block.
    Mul16,
    /// `MOVW` and `MOVT`: `imm4:i:imm3:imm8` from `e`, `H`, `k` and `i`.
    Wide,
    /// The parallel additions and subtractions: the prefix in `q`, the lanes
    /// in `o`.
    Parallel,
    /// A one-source operation naming its source twice, which must agree.
    Twice,
    /// A single transfer, its immediate scaled left by `scale`.
    Transfer { base: Base, scale: u8 },
    /// `LDRD` and `STRD`: a word-scaled immediate with `P`, `U` and `W`.
    Dual,
    /// `LDREX` and `STREX`: a word-scaled immediate.
    ExclusiveOffset,
    /// A preload: its sign fixed, or as `U` says when `None`.
    Preload { up: Option<bool>, base: Base },
    /// A block transfer.
    Block(Block),
    /// `SRS` and `RFE` in this mode.
    State(BlockMode),
    /// 16-bit `CPS`.
    Cps16,
    /// A branch.
    Branch(Branch),
    /// A reserved memory hint, which behaves as `NOP` whatever its fields
    /// hold.
    Nop,
}

/// `i:imm3:imm8` from `H`, `k` and `i`; each absent field is zero.
const fn immediate(f: &Fields) -> u32 {
    (f.get(Field::H) << 11) | (f.get(Field::Amount) << 8) | f.get(Field::Imm)
}

/// The index `P` and `W` select; both clear is another instruction.
const fn index(f: &Fields) -> Option<Index> {
    match (f.bit(Field::P), f.bit(Field::W)) {
        (true, false) => Some(Index::Offset),
        (true, true) => Some(Index::PreIndexed),
        (false, true) => Some(Index::PostIndexed),
        (false, false) => None,
    }
}

const fn sign_extend(value: u32, bits: u32) -> i32 {
    (value << (32 - bits)).cast_signed() >> (32 - bits)
}

/// An `I` bit from its `J` and the sign, or the `J` from its `I`: the
/// relation is its own inverse.
const fn relative(bit: u32, sign: u32) -> u32 {
    !(bit ^ sign) & 1
}

const fn prefix(field: u32) -> Option<Prefix> {
    Some(match field {
        0b000 => Prefix::Signed,
        0b001 => Prefix::SignedSaturating,
        0b010 => Prefix::SignedHalving,
        0b100 => Prefix::Unsigned,
        0b101 => Prefix::UnsignedSaturating,
        0b110 => Prefix::UnsignedHalving,
        _ => return None,
    })
}

const fn prefix_field(prefix: Prefix) -> u32 {
    match prefix {
        Prefix::Signed => 0b000,
        Prefix::SignedSaturating => 0b001,
        Prefix::SignedHalving => 0b010,
        Prefix::Unsigned => 0b100,
        Prefix::UnsignedSaturating => 0b101,
        Prefix::UnsignedHalving => 0b110,
    }
}

const fn lanes(field: u32) -> Option<Lanes> {
    Some(match field {
        0b001 => Lanes::Add16,
        0b010 => Lanes::Asx,
        0b110 => Lanes::Sax,
        0b101 => Lanes::Sub16,
        0b000 => Lanes::Add8,
        0b100 => Lanes::Sub8,
        _ => return None,
    })
}

const fn lanes_field(lanes: Lanes) -> u32 {
    match lanes {
        Lanes::Add16 => 0b001,
        Lanes::Asx => 0b010,
        Lanes::Sax => 0b110,
        Lanes::Sub16 => 0b101,
        Lanes::Add8 => 0b000,
        Lanes::Sub8 => 0b100,
    }
}

fn base(base: Base, f: &Fields) -> Reg {
    match base {
        Base::Field => f.reg(Field::Rn),
        Base::Pc => Reg::PC,
        Base::Sp => Reg::SP,
    }
}

/// A transfer's or preload's offset: Rm shifted left, or the immediate
/// scaled left.
fn offset(layout: &Layout, f: &Fields, scale: u8) -> Option<Offset> {
    Some(if layout.has(Field::Rm) {
        Offset::Register {
            rm: f.reg(Field::Rm),
            shift: Shift::Lsl(u8::try_from(f.get(Field::Amount)).ok()?),
        }
    } else {
        Offset::Immediate(u16::try_from(f.get(Field::Imm) << scale).ok()?)
    })
}

/// The instruction `f` holds under `layout` for a row of `shape` and `kind`,
/// in an IT block when `in_it`, or `None` when the fields hold a combination
/// the form excludes.
#[allow(clippy::too_many_lines)] // One arm per shape, the inverse of `fields`.
pub(super) fn build(
    shape: Shape,
    kind: Kind,
    layout: &Layout,
    f: &Fields,
    in_it: bool,
) -> Option<Op> {
    let r = |field| f.reg(field);
    Some(match shape {
        Shape::Common => kind.build(layout, f)?,
        Shape::Data {
            rd,
            rn,
            source,
            flags,
        } => {
            let rd = match rd {
                Rd::Field => r(Field::Rd),
                Rd::Sp => Reg::SP,
                Rd::Pc => Reg::PC,
            };
            let rn = match rn {
                Rn::Field => r(Field::Rn),
                Rn::Rd => rd,
                Rn::Sp => Reg::SP,
                Rn::Pc => Reg::PC,
                Rn::Lr => Reg::LR,
                Rn::None => Reg::field(0),
            };
            let operand = match source {
                Source::Register => Operand::Register {
                    rm: r(Field::Rm),
                    shift: Shift::decode(f.get(Field::Type), f.get(Field::Amount)),
                },
                Source::ShiftedRdn(kind) => Operand::Shifted {
                    rm: rd,
                    kind,
                    rs: r(Field::Rs),
                },
                Source::Expanded => Operand::Expanded(Expanded(u16::try_from(immediate(f)).ok()?)),
                Source::Literal(scale) => {
                    Operand::Literal(u16::try_from(immediate(f) << scale).ok()?)
                }
            };
            let s = match flags {
                Flags::Field => f.bit(Field::S),
                Flags::OutsideIt => !in_it,
                Flags::Never => false,
                Flags::Always => true,
            };
            match kind {
                Kind::Data(op) => Op::Data {
                    op,
                    s,
                    rd,
                    rn,
                    operand,
                },
                Kind::Compare(op) => Op::Compare {
                    op,
                    rd: Reg::field(0),
                    rn,
                    operand,
                },
                Kind::Move(op) => Op::Move { op, s, rd, operand },
                _ => return None,
            }
        }
        Shape::Mul16 => {
            let Kind::Multiply(op) = kind else {
                return None;
            };
            let rdm = r(Field::Rd);
            Op::Multiply {
                op,
                s: !in_it,
                x: false,
                y: false,
                rd: rdm,
                rn: r(Field::Rn),
                rm: rdm,
                ra: None,
            }
        }
        Shape::Wide => {
            let Kind::Wide(op) = kind else {
                return None;
            };
            Op::Wide {
                op,
                rd: r(Field::Rd),
                value: u16::try_from((f.get(Field::Msb) << 12) | immediate(f)).ok()?,
            }
        }
        Shape::Parallel => Op::Parallel {
            prefix: prefix(f.get(Field::Opc2))?,
            lanes: lanes(f.get(Field::Opc1))?,
            rd: r(Field::Rd),
            rn: r(Field::Rn),
            rm: r(Field::Rm),
        },
        Shape::Twice => {
            let Kind::Unary(op) = kind else {
                return None;
            };
            Op::Unary {
                op,
                rd: r(Field::Rd),
                rm: r(Field::Rm),
            }
        }
        Shape::Transfer { base: at, scale } => {
            let Kind::Transfer(op) = kind else {
                return None;
            };
            let rn = base(at, f);
            // Every 32-bit store addressing from the PC is undefined.
            if op.stores() && rn == Reg::PC {
                return None;
            }
            Op::Transfer {
                op,
                rt: r(Field::Rt),
                rn,
                up: !layout.has(Field::U) || f.bit(Field::U),
                offset: offset(layout, f, scale)?,
                index: if layout.has(Field::P) {
                    index(f)?
                } else {
                    Index::Offset
                },
            }
        }
        Shape::Dual => {
            let Kind::Dual(op) = kind else {
                return None;
            };
            let rn = r(Field::Rn);
            // Only the load has a literal form.
            if op == DualOp::Strd && rn == Reg::PC {
                return None;
            }
            Op::Dual {
                op,
                rt: r(Field::Rt),
                rt2: r(Field::Rt2),
                rn,
                up: f.bit(Field::U),
                offset: offset(layout, f, 2)?,
                index: index(f)?,
            }
        }
        Shape::ExclusiveOffset => {
            let Kind::Exclusive(op) = kind else {
                return None;
            };
            Op::Exclusive {
                op,
                rd: layout.has(Field::Rd).then(|| r(Field::Rd)),
                rt: r(Field::Rt),
                rt2: None,
                rn: r(Field::Rn),
                offset: u16::try_from(f.get(Field::Imm) << 2).ok()?,
            }
        }
        Shape::Preload { up, base: at } => {
            let Kind::Preload(op) = kind else {
                return None;
            };
            Op::Preload {
                op,
                rn: base(at, f),
                up: up.unwrap_or_else(|| f.bit(Field::U)),
                offset: offset(layout, f, 0)?,
            }
        }
        Shape::Block(block) => {
            let Kind::Block(op) = kind else {
                return None;
            };
            let low = u16::try_from(f.get(Field::List)).ok()?;
            let extra = u16::from(f.bit(Field::X));
            let (rn, mode, list) = match block {
                Block::Push => (Reg::SP, BlockMode::DecrementBefore, low | (extra << 14)),
                Block::Pop => (Reg::SP, BlockMode::IncrementAfter, low | (extra << 15)),
                Block::Store | Block::Load => (r(Field::Rn), BlockMode::IncrementAfter, low),
                Block::Wide(mode) => (r(Field::Rn), mode, low),
            };
            let list = RegList(list);
            Op::Block {
                op,
                rn,
                mode,
                writeback: match block {
                    Block::Push | Block::Pop | Block::Store => true,
                    Block::Load => !list.contains(rn),
                    Block::Wide(_) => f.bit(Field::W),
                },
                user: false,
                list,
            }
        }
        Shape::State(mode) => match kind {
            Kind::SaveState => Op::SaveState {
                mode,
                writeback: f.bit(Field::W),
                bank: u8::try_from(f.get(Field::Mode)).ok()?,
            },
            Kind::ReturnFromException => Op::ReturnFromException {
                rn: r(Field::Rn),
                mode,
                writeback: f.bit(Field::W),
            },
            _ => return None,
        },
        Shape::Cps16 => Op::ChangeState {
            imod: 0b10 | u8::from(f.bit(Field::Imod)),
            change_mode: false,
            interrupts: u8::try_from(f.get(Field::Interrupts)).ok()?,
            mode: 0,
        },
        Shape::Branch(branch) => {
            let Kind::Branch(op) = kind else {
                return None;
            };
            if branch.conditional() && (op != BranchOp::B || f.get(Field::Cond) >> 1 == 0b111) {
                return None;
            }
            Op::Branch {
                op,
                offset: branch_offset(branch, f),
            }
        }
        Shape::Nop => match kind {
            Kind::Hint(HintOp::Nop) => Op::Hint {
                op: HintOp::Nop,
                value: 0,
            },
            _ => return None,
        },
    })
}

fn branch_offset(branch: Branch, f: &Fields) -> i32 {
    let sign = f.get(Field::H);
    let (j1, j2) = (f.get(Field::J1), f.get(Field::J2));
    match branch {
        Branch::Cond8 => sign_extend(f.get(Field::Imm) << 1, 9),
        Branch::Imm11 => sign_extend(f.get(Field::Imm) << 1, 12),
        Branch::Cond20 => sign_extend(
            (sign << 20)
                | (j2 << 19)
                | (j1 << 18)
                | (f.get(Field::Msb) << 12)
                | (f.get(Field::Imm) << 1),
            21,
        ),
        Branch::Imm24 | Branch::Blx => {
            let low = if branch == Branch::Blx {
                f.get(Field::Imm) << 2
            } else {
                f.get(Field::Imm) << 1
            };
            sign_extend(
                (sign << 24)
                    | (relative(j1, sign) << 23)
                    | (relative(j2, sign) << 22)
                    | (f.get(Field::Msb) << 12)
                    | low,
                25,
            )
        }
    }
}

/// Every field the operands set for a row of `shape` with `layout`; the
/// encoder decodes the word the row writes from them, refusing operands the
/// row cannot hold.
#[allow(clippy::too_many_lines)] // The inverse of `build`, arm for arm.
pub(super) fn fields(shape: Shape, layout: &Layout, op: &Op) -> Fields {
    let mut f = Fields::default();
    match (shape, *op) {
        (Shape::Common, op) => return op.fields(),
        (
            Shape::Data { source, .. },
            Op::Data {
                s, rd, rn, operand, ..
            },
        ) => {
            f.flag(Field::S, s);
            f.set(Field::Rd, rd.number());
            f.set(Field::Rn, rn.number());
            set_operand(&mut f, layout, source, operand);
        }
        (Shape::Data { source, .. }, Op::Compare { rn, operand, .. }) => {
            f.set(Field::Rn, rn.number());
            set_operand(&mut f, layout, source, operand);
        }
        (Shape::Data { source, .. }, Op::Move { s, rd, operand, .. }) => {
            f.flag(Field::S, s);
            f.set(Field::Rd, rd.number());
            set_operand(&mut f, layout, source, operand);
        }
        (Shape::Mul16, Op::Multiply { rd, rn, .. }) => {
            f.set(Field::Rd, rd.number());
            f.set(Field::Rn, rn.number());
        }
        (Shape::Wide, Op::Wide { rd, value, .. }) => {
            let value = u32::from(value);
            f.set(Field::Rd, rd.number());
            f.set(Field::Msb, value >> 12);
            set_split(&mut f, value & 0xFFF);
        }
        (
            Shape::Parallel,
            Op::Parallel {
                prefix,
                lanes,
                rd,
                rn,
                rm,
            },
        ) => {
            f.set(Field::Opc2, prefix_field(prefix));
            f.set(Field::Opc1, lanes_field(lanes));
            f.set(Field::Rd, rd.number());
            f.set(Field::Rn, rn.number());
            f.set(Field::Rm, rm.number());
        }
        (Shape::Twice, Op::Unary { rd, rm, .. }) => {
            f.set(Field::Rd, rd.number());
            f.set(Field::Rm, (rm.number() << 4) | rm.number());
        }
        (
            Shape::Transfer { scale, .. },
            Op::Transfer {
                rt,
                rn,
                up,
                offset,
                index,
                ..
            },
        ) => {
            f.set(Field::Rt, rt.number());
            f.set(Field::Rn, rn.number());
            f.flag(Field::U, up);
            set_index(&mut f, index);
            set_offset(&mut f, offset, scale);
        }
        (
            Shape::Dual,
            Op::Dual {
                rt,
                rt2,
                rn,
                up,
                offset,
                index,
                ..
            },
        ) => {
            f.set(Field::Rt, rt.number());
            f.set(Field::Rt2, rt2.number());
            f.set(Field::Rn, rn.number());
            f.flag(Field::U, up);
            set_index(&mut f, index);
            set_offset(&mut f, offset, 2);
        }
        (
            Shape::ExclusiveOffset,
            Op::Exclusive {
                rd, rt, rn, offset, ..
            },
        ) => {
            if let Some(rd) = rd {
                f.set(Field::Rd, rd.number());
            }
            f.set(Field::Rt, rt.number());
            f.set(Field::Rn, rn.number());
            set_scaled(&mut f, Field::Imm, u32::from(offset), 2);
        }
        (Shape::Preload { .. }, Op::Preload { rn, up, offset, .. }) => {
            f.set(Field::Rn, rn.number());
            f.flag(Field::U, up);
            set_offset(&mut f, offset, 0);
        }
        (
            Shape::Block(block),
            Op::Block {
                rn,
                writeback,
                list,
                ..
            },
        ) => {
            f.set(Field::Rn, rn.number());
            f.flag(Field::W, writeback);
            let bits = u32::from(list.0);
            match block {
                Block::Push => {
                    f.set(Field::List, bits & 0xFF);
                    f.set(Field::X, (bits >> 14) & 1);
                }
                Block::Pop => {
                    f.set(Field::List, bits & 0xFF);
                    f.set(Field::X, (bits >> 15) & 1);
                }
                Block::Store | Block::Load | Block::Wide(_) => f.set(Field::List, bits),
            }
        }
        (
            Shape::State(_),
            Op::SaveState {
                writeback, bank, ..
            },
        ) => {
            f.flag(Field::W, writeback);
            f.set(Field::Mode, u32::from(bank));
        }
        (Shape::State(_), Op::ReturnFromException { rn, writeback, .. }) => {
            f.set(Field::Rn, rn.number());
            f.flag(Field::W, writeback);
        }
        (
            Shape::Cps16,
            Op::ChangeState {
                imod, interrupts, ..
            },
        ) => {
            f.set(Field::Imod, u32::from(imod & 1));
            f.set(Field::Interrupts, u32::from(interrupts));
        }
        (Shape::Branch(branch), Op::Branch { offset, .. }) => set_branch(&mut f, branch, offset),
        _ => {}
    }
    f
}

fn set_operand(f: &mut Fields, layout: &Layout, source: Source, operand: Operand) {
    match operand {
        Operand::Register { rm, shift } => {
            f.set(Field::Rm, rm.number());
            let (kind, amount) = shift.encode().unwrap_or((u32::MAX, u32::MAX));
            f.set(Field::Type, kind);
            f.set(Field::Amount, amount);
        }
        Operand::Shifted { rs, .. } => f.set(Field::Rs, rs.number()),
        Operand::Expanded(Expanded(field)) => set_split(f, u32::from(field)),
        Operand::Literal(value) => {
            let value = u32::from(value);
            let scale = match source {
                Source::Literal(scale) => u32::from(scale),
                _ => 0,
            };
            if value & ((1 << scale) - 1) != 0 {
                f.set(Field::Imm, u32::MAX);
            } else if layout.has(Field::H) {
                set_split(f, value >> scale);
            } else {
                f.set(Field::Imm, value >> scale);
            }
        }
        // A32's rotated immediate: no T32 row holds it, so whatever is
        // deposited decodes to another operand and is refused as unencodable.
        Operand::Immediate(_) => {}
    }
}

/// `i:imm3:imm8` into `H`, `k` and `i`, or a value no field holds when it
/// is wider than twelve bits.
fn set_split(f: &mut Fields, value: u32) {
    if value >> 12 != 0 {
        f.set(Field::Imm, u32::MAX);
        return;
    }
    f.set(Field::H, value >> 11);
    f.set(Field::Amount, (value >> 8) & 7);
    f.set(Field::Imm, value & 0xFF);
}

fn set_index(f: &mut Fields, index: Index) {
    let (p, w) = match index {
        Index::Offset => (true, false),
        Index::PreIndexed => (true, true),
        Index::PostIndexed => (false, true),
    };
    f.flag(Field::P, p);
    f.flag(Field::W, w);
}

fn set_offset(f: &mut Fields, offset: Offset, scale: u8) {
    match offset {
        Offset::Immediate(value) => set_scaled(f, Field::Imm, u32::from(value), scale),
        Offset::Register { rm, shift } => {
            f.set(Field::Rm, rm.number());
            match shift {
                Shift::Lsl(amount) => f.set(Field::Amount, u32::from(amount)),
                _ => f.set(Field::Amount, u32::MAX),
            }
        }
    }
}

/// `value` scaled down into `field`, or a value no field holds when it is
/// not a multiple of the scale.
fn set_scaled(f: &mut Fields, field: Field, value: u32, scale: u8) {
    let scale = u32::from(scale);
    if value & ((1 << scale) - 1) == 0 {
        f.set(field, value >> scale);
    } else {
        f.set(field, u32::MAX);
    }
}

fn set_branch(f: &mut Fields, branch: Branch, offset: i32) {
    let (least, greatest) = branch.range();
    if offset < least || offset > greatest {
        f.set(Field::Imm, u32::MAX);
        return;
    }
    // A misaligned offset loses its low bits here, so the word decodes to
    // another branch and is refused as unencodable.
    let bits = offset.cast_unsigned();
    let sign = bits >> 31;
    match branch {
        Branch::Cond8 => f.set(Field::Imm, (bits >> 1) & 0xFF),
        Branch::Imm11 => f.set(Field::Imm, (bits >> 1) & 0x7FF),
        Branch::Cond20 => {
            f.set(Field::H, sign);
            f.set(Field::J2, (bits >> 19) & 1);
            f.set(Field::J1, (bits >> 18) & 1);
            f.set(Field::Msb, (bits >> 12) & 0x3F);
            f.set(Field::Imm, (bits >> 1) & 0x7FF);
        }
        Branch::Imm24 | Branch::Blx => {
            f.set(Field::H, sign);
            f.set(Field::J1, relative((bits >> 23) & 1, sign));
            f.set(Field::J2, relative((bits >> 22) & 1, sign));
            f.set(Field::Msb, (bits >> 12) & 0x3FF);
            if branch == Branch::Blx {
                f.set(Field::Imm, (bits >> 2) & 0x3FF);
            } else {
                f.set(Field::Imm, (bits >> 1) & 0x7FF);
            }
        }
    }
}
