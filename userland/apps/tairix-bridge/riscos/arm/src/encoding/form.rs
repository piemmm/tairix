//! What each row decodes to: a [`Kind`] names the operation, and builds an
//! [`Op`] from the fields its row's layout read. [`Op::fields`] is the
//! inverse, from which a row's layout writes a word.

use super::layout::{Field, Fields, Layout};
use super::simdfp::SimdFpKind;
use crate::cond::Condition;
use crate::fpa::{Format, Precision, Rounding};
use crate::instruction::{
    ArithmeticOp, BarrierOp, BitfieldOp, BlockMode, BlockOp, BranchOp, CallOp, CoIndex,
    CoRegisterOp, CoRegistersOp, CoTransferOp, CompareOp, CoprocessorOp, DataOp, DualOp,
    ExchangeOp, ExclusiveOp, ExtendOp, FpaCompareOp, FpaDyadicOp, FpaMonadicOp, FpaMultipleOp,
    FpaOperand, FpaRegisterOp, FpaTransferOp, HintOp, Immediate, Index, Lanes, LongOp, MoveOp,
    MultiplyOp, Offset, Op, Operand, PackOp, Prefix, PreloadOp, SaturateOp, Shift, ShiftType,
    Source, SwapOp, TransferOp, UnaryOp, WideOp,
};
use crate::reg::{Reg, RegList};

/// The operation a row decodes: an [`Op`]'s variant and operation, without
/// its operands.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) enum Kind {
    Data(DataOp),
    Compare(CompareOp),
    Move(MoveOp),
    Wide(WideOp),
    Multiply(MultiplyOp),
    MultiplyLong(LongOp),
    Parallel,
    Arithmetic(ArithmeticOp),
    Unary(UnaryOp),
    Saturate(SaturateOp),
    Extend(ExtendOp),
    Pack(PackOp),
    Bitfield(BitfieldOp),
    Status,
    StatusWrite,
    Branch(BranchOp),
    Exchange(ExchangeOp),
    Call(CallOp),
    Hint(HintOp),
    Barrier(BarrierOp),
    ClearExclusive,
    ExceptionReturn,
    Transfer(TransferOp),
    Dual(DualOp),
    Preload(PreloadOp),
    Block(BlockOp),
    Swap(SwapOp),
    Exclusive(ExclusiveOp),
    ChangeState,
    SetEndian,
    SaveState,
    ReturnFromException,
    Coprocessor(CoprocessorOp),
    CoprocessorRegister(CoRegisterOp),
    CoprocessorRegisters(CoRegistersOp),
    CoprocessorTransfer(CoTransferOp),
    CompareBranch,
    IfThen,
    TableBranch,
    FpaDyadic(FpaDyadicOp),
    FpaMonadic(FpaMonadicOp),
    FpaRegister(FpaRegisterOp),
    FpaCompare(FpaCompareOp),
    FpaTransfer(FpaTransferOp),
    FpaMultiple(FpaMultipleOp),
    SimdFp(SimdFpKind),
}

/// An FPA precision; the fourth value has none.
fn precision(fields: &Fields) -> Option<Precision> {
    Precision::ALL
        .get(fields.get(Field::Precision) as usize)
        .copied()
}

fn rounding(fields: &Fields) -> Rounding {
    Rounding::ALL[fields.get(Field::Rounding) as usize & 3]
}

/// An FPA operand: a register, or by bit 3 a constant.
fn fpa_operand(fields: &Fields) -> FpaOperand {
    let m = fields.byte(Field::CRm);
    if m & 8 == 0 {
        FpaOperand::Register(m & 7)
    } else {
        FpaOperand::Constant(m & 7)
    }
}

/// An FPA transfer's addressing: with P and W clear, at the base without
/// writeback whichever way U points.
fn fpa_index(fields: &Fields) -> CoIndex {
    CoIndex::of(fields.bit(Field::P), fields.bit(Field::W))
}

fn operand(layout: &Layout, fields: &Fields) -> Operand {
    if layout.has(Field::Rotate) {
        Operand::Immediate(Immediate {
            value: fields.byte(Field::Imm),
            rotation: fields.byte(Field::Rotate),
        })
    } else if layout.has(Field::Rs) {
        Operand::Shifted {
            rm: fields.reg(Field::Rm),
            kind: ShiftType::field(fields.get(Field::Type)),
            rs: fields.reg(Field::Rs),
        }
    } else {
        Operand::Register {
            rm: fields.reg(Field::Rm),
            shift: Shift::decode(fields.get(Field::Type), fields.get(Field::Amount)),
        }
    }
}

fn offset(layout: &Layout, fields: &Fields) -> Option<Offset> {
    Some(if layout.has(Field::Imm) {
        Offset::Immediate(u16::try_from(fields.get(Field::Imm)).ok()?)
    } else {
        Offset::Register {
            rm: fields.reg(Field::Rm),
            shift: if layout.has(Field::Type) {
                Shift::decode(fields.get(Field::Type), fields.get(Field::Amount))
            } else {
                Shift::NONE
            },
        }
    })
}

/// The index P and W select; P clear with W set is a different instruction.
fn index(fields: &Fields) -> Option<Index> {
    match (fields.bit(Field::P), fields.bit(Field::W)) {
        (true, false) => Some(Index::Offset),
        (true, true) => Some(Index::PreIndexed),
        (false, false) => Some(Index::PostIndexed),
        (false, true) => None,
    }
}

/// A 24-bit word offset, as a byte offset.
const fn branch_offset(imm24: u32) -> i32 {
    (imm24 << 8).cast_signed() >> 6
}

impl Kind {
    /// Whether the kind is a coprocessor form every coprocessor number
    /// shares, which a number a unit claims never decodes to.
    pub const fn generic_coprocessor(self) -> bool {
        matches!(
            self,
            Self::Coprocessor(_)
                | Self::CoprocessorRegister(_)
                | Self::CoprocessorRegisters(_)
                | Self::CoprocessorTransfer(_)
        )
    }

    /// The instruction `fields` hold under `layout`, or `None` when they hold
    /// a combination the form excludes.
    #[allow(clippy::too_many_lines)] // One arm per form: splitting would scatter the inverse of `Op::fields`.
    pub fn build(self, layout: &Layout, f: &Fields) -> Option<Op> {
        let r = |field| f.reg(field);
        let optional = |field| layout.has(field).then(|| f.reg(field));
        Some(match self {
            Self::Data(op) => Op::Data {
                op,
                s: f.bit(Field::S),
                rd: r(Field::Rd),
                rn: r(Field::Rn),
                operand: operand(layout, f),
            },
            Self::Compare(op) => Op::Compare {
                op,
                rd: r(Field::Rd),
                rn: r(Field::Rn),
                operand: operand(layout, f),
            },
            Self::Move(op) => Op::Move {
                op,
                s: f.bit(Field::S),
                rd: r(Field::Rd),
                operand: operand(layout, f),
            },
            Self::Wide(op) => Op::Wide {
                op,
                rd: r(Field::Rd),
                value: u16::try_from(f.get(Field::Imm)).ok()?,
            },
            Self::Multiply(op) => Op::Multiply {
                op,
                s: f.bit(Field::S),
                x: f.bit(Field::X),
                y: f.bit(Field::Y),
                rd: r(Field::Rd),
                rn: r(Field::Rn),
                rm: r(Field::Rm),
                ra: optional(Field::Ra),
            },
            Self::MultiplyLong(op) => Op::MultiplyLong {
                op,
                s: f.bit(Field::S),
                x: f.bit(Field::X),
                y: f.bit(Field::Y),
                lo: r(Field::Lo),
                hi: r(Field::Hi),
                rn: r(Field::Rn),
                rm: r(Field::Rm),
            },
            Self::Parallel => Op::Parallel {
                prefix: match f.get(Field::Opc1) {
                    0b001 => Prefix::Signed,
                    0b010 => Prefix::SignedSaturating,
                    0b011 => Prefix::SignedHalving,
                    0b101 => Prefix::Unsigned,
                    0b110 => Prefix::UnsignedSaturating,
                    0b111 => Prefix::UnsignedHalving,
                    _ => return None,
                },
                lanes: match f.get(Field::Opc2) {
                    0b000 => Lanes::Add16,
                    0b001 => Lanes::Asx,
                    0b010 => Lanes::Sax,
                    0b011 => Lanes::Sub16,
                    0b100 => Lanes::Add8,
                    0b111 => Lanes::Sub8,
                    _ => return None,
                },
                rd: r(Field::Rd),
                rn: r(Field::Rn),
                rm: r(Field::Rm),
            },
            Self::Arithmetic(op) => Op::Arithmetic {
                op,
                rd: r(Field::Rd),
                rn: r(Field::Rn),
                rm: r(Field::Rm),
            },
            Self::Unary(op) => Op::Unary {
                op,
                rd: r(Field::Rd),
                rm: r(Field::Rm),
            },
            Self::Saturate(op) => Op::Saturate {
                op,
                rd: r(Field::Rd),
                saturate: f.byte(Field::Imm),
                rn: r(Field::Rn),
                shift: if layout.has(Field::Type) {
                    Shift::decode(f.get(Field::Type) << 1, f.get(Field::Amount))
                } else {
                    Shift::NONE
                },
            },
            Self::Extend(op) => Op::Extend {
                op,
                rd: r(Field::Rd),
                rn: optional(Field::Rn),
                rm: r(Field::Rm),
                rotation: f.byte(Field::Rotation),
            },
            Self::Pack(op) => Op::Pack {
                op,
                rd: r(Field::Rd),
                rn: r(Field::Rn),
                rm: r(Field::Rm),
                shift: match op {
                    PackOp::Pkhbt => Shift::decode(0, f.get(Field::Amount)),
                    PackOp::Pkhtb => Shift::decode(2, f.get(Field::Amount)),
                },
            },
            Self::Bitfield(op) => Op::Bitfield {
                op,
                rd: r(Field::Rd),
                rn: optional(Field::Rn),
                lsb: f.byte(Field::Lsb),
                top: f.byte(Field::Msb),
            },
            Self::Status => Op::Status {
                spsr: f.bit(Field::Spsr),
                rd: r(Field::Rd),
            },
            Self::StatusWrite => Op::StatusWrite {
                spsr: f.bit(Field::Spsr),
                fields: f.byte(Field::Mask),
                source: if layout.has(Field::Rotate) {
                    Source::Immediate(Immediate {
                        value: f.byte(Field::Imm),
                        rotation: f.byte(Field::Rotate),
                    })
                } else {
                    Source::Register(r(Field::Rm))
                },
            },
            Self::Branch(op) => Op::Branch {
                op,
                offset: branch_offset(f.get(Field::Imm)) | i32::from(f.bit(Field::H)) << 1,
            },
            Self::Exchange(op) => Op::Exchange {
                op,
                rm: r(Field::Rm),
            },
            Self::Call(op) => Op::Call {
                op,
                value: f.get(Field::Imm),
            },
            Self::Hint(op) => Op::Hint {
                op,
                value: f.byte(Field::Imm),
            },
            Self::Barrier(op) => Op::Barrier {
                op,
                option: f.byte(Field::Imm),
            },
            Self::ClearExclusive => Op::ClearExclusive,
            Self::ExceptionReturn => Op::ExceptionReturn,
            Self::Transfer(op) => Op::Transfer {
                op,
                rt: r(Field::Rt),
                rn: r(Field::Rn),
                up: f.bit(Field::U),
                offset: offset(layout, f)?,
                index: if op.unprivileged() {
                    Index::PostIndexed
                } else {
                    index(f)?
                },
            },
            Self::Dual(op) => {
                let rt = r(Field::Rt);
                Op::Dual {
                    op,
                    rt,
                    rt2: optional(Field::Rt2).unwrap_or_else(|| rt.next()),
                    rn: r(Field::Rn),
                    up: f.bit(Field::U),
                    offset: offset(layout, f)?,
                    index: index(f)?,
                }
            }
            Self::Preload(op) => Op::Preload {
                op,
                rn: r(Field::Rn),
                up: f.bit(Field::U),
                offset: offset(layout, f)?,
            },
            Self::Block(op) => Op::Block {
                op,
                rn: r(Field::Rn),
                mode: BlockMode::of(f.bit(Field::P), f.bit(Field::U)),
                writeback: f.bit(Field::W),
                user: f.bit(Field::B),
                list: RegList(u16::try_from(f.get(Field::List)).ok()?),
            },
            Self::Swap(op) => Op::Swap {
                op,
                rt: r(Field::Rt),
                rt2: r(Field::Rt2),
                rn: r(Field::Rn),
            },
            Self::Exclusive(op) => {
                let rt = r(Field::Rt);
                let pair = matches!(
                    op,
                    ExclusiveOp::Ldrexd
                        | ExclusiveOp::Strexd
                        | ExclusiveOp::Ldaexd
                        | ExclusiveOp::Stlexd
                );
                Op::Exclusive {
                    op,
                    rd: optional(Field::Rd),
                    rt,
                    rt2: optional(Field::Rt2).or_else(|| pair.then(|| rt.next())),
                    rn: r(Field::Rn),
                    offset: 0,
                }
            }
            Self::CompareBranch => Op::CompareBranch {
                nonzero: f.bit(Field::X),
                rn: r(Field::Rn),
                offset: f.get(Field::Imm) << 1,
            },
            Self::IfThen => Op::IfThen {
                firstcond: Condition::field(f.get(Field::Opc1)),
                mask: f.byte(Field::Opc2),
            },
            Self::TableBranch => Op::TableBranch {
                half: f.bit(Field::X),
                rn: r(Field::Rn),
                rm: r(Field::Rm),
            },
            Self::ChangeState => Op::ChangeState {
                imod: f.byte(Field::Imod),
                change_mode: f.bit(Field::ChangeMode),
                interrupts: f.byte(Field::Interrupts),
                mode: f.byte(Field::Mode),
            },
            Self::SetEndian => Op::SetEndian {
                big: f.bit(Field::Endian),
            },
            Self::SaveState => Op::SaveState {
                mode: BlockMode::of(f.bit(Field::P), f.bit(Field::U)),
                writeback: f.bit(Field::W),
                bank: f.byte(Field::Mode),
            },
            Self::ReturnFromException => Op::ReturnFromException {
                rn: r(Field::Rn),
                mode: BlockMode::of(f.bit(Field::P), f.bit(Field::U)),
                writeback: f.bit(Field::W),
            },
            Self::Coprocessor(op) => Op::Coprocessor {
                op,
                cp: f.byte(Field::Cp),
                opc1: f.byte(Field::Opc1),
                crd: f.byte(Field::CRd),
                crn: f.byte(Field::CRn),
                crm: f.byte(Field::CRm),
                opc2: f.byte(Field::Opc2),
            },
            Self::CoprocessorRegister(op) => Op::CoprocessorRegister {
                op,
                cp: f.byte(Field::Cp),
                opc1: f.byte(Field::Opc1),
                rt: r(Field::Rt),
                crn: f.byte(Field::CRn),
                crm: f.byte(Field::CRm),
                opc2: f.byte(Field::Opc2),
            },
            Self::CoprocessorRegisters(op) => Op::CoprocessorRegisters {
                op,
                cp: f.byte(Field::Cp),
                opc1: f.byte(Field::Opc1),
                rt: r(Field::Rt),
                rt2: r(Field::Rt2),
                crm: f.byte(Field::CRm),
            },
            Self::CoprocessorTransfer(op) => Op::CoprocessorTransfer {
                op,
                cp: f.byte(Field::Cp),
                crd: f.byte(Field::CRd),
                rn: r(Field::Rn),
                long: f.bit(Field::B),
                up: f.bit(Field::U),
                index: match CoIndex::of(f.bit(Field::P), f.bit(Field::W)) {
                    // Unindexed down is undefined.
                    CoIndex::Unindexed if !f.bit(Field::U) => return None,
                    index => index,
                },
                offset: f.byte(Field::Imm),
            },
            Self::FpaDyadic(op) => Op::FpaDyadic {
                op,
                precision: precision(f)?,
                rounding: rounding(f),
                fd: f.byte(Field::CRd),
                fn_: f.byte(Field::CRn),
                fm: fpa_operand(f),
            },
            Self::FpaMonadic(op) => Op::FpaMonadic {
                op,
                precision: precision(f)?,
                rounding: rounding(f),
                fd: f.byte(Field::CRd),
                fm: fpa_operand(f),
            },
            Self::FpaRegister(op) => Op::FpaRegister {
                op,
                precision: precision(f)?,
                rounding: rounding(f),
                fn_: f.byte(Field::CRn),
                fm: fpa_operand(f),
                rd: r(Field::Rt),
            },
            Self::FpaCompare(op) => Op::FpaCompare {
                op,
                precision: precision(f)?,
                fn_: f.byte(Field::CRn),
                fm: fpa_operand(f),
                rd: r(Field::Rt),
            },
            Self::FpaTransfer(op) => Op::FpaTransfer {
                op,
                format: Format::ALL[f.get(Field::Precision) as usize & 3],
                fd: f.byte(Field::CRd),
                rn: r(Field::Rn),
                up: f.bit(Field::U),
                index: fpa_index(f),
                offset: f.byte(Field::Imm),
            },
            Self::FpaMultiple(op) => Op::FpaMultiple {
                op,
                fd: f.byte(Field::CRd),
                count: match f.byte(Field::Count) {
                    0 => 4,
                    n => n,
                },
                rn: r(Field::Rn),
                up: f.bit(Field::U),
                index: fpa_index(f),
                offset: f.byte(Field::Imm),
            },
            Self::SimdFp(kind) => Op::SimdFp(kind.build(layout, f)?),
        })
    }
}

fn set_operand(f: &mut Fields, operand: Operand) {
    match operand {
        Operand::Immediate(immediate) => {
            f.set(Field::Imm, u32::from(immediate.value));
            f.set(Field::Rotate, u32::from(immediate.rotation));
        }
        Operand::Register { rm, shift } => {
            f.set(Field::Rm, rm.number());
            set_shift(f, shift);
        }
        Operand::Shifted { rm, kind, rs } => {
            f.set(Field::Rm, rm.number());
            f.set(Field::Type, kind.bits());
            f.set(Field::Rs, rs.number());
        }
        // The T32 forms: no A32 row holds them, so whatever is deposited
        // decodes to another operand and is refused as unencodable.
        Operand::Expanded(_) | Operand::Literal(_) => {}
    }
}

/// Writes `shift`'s type and amount, or a type no row has when the shift
/// cannot be encoded, so the encoding fails.
fn set_shift(f: &mut Fields, shift: Shift) {
    let (kind, amount) = shift.encode().unwrap_or((u32::MAX, u32::MAX));
    f.set(Field::Type, kind);
    f.set(Field::Amount, amount);
}

fn set_offset(f: &mut Fields, offset: Offset) {
    match offset {
        Offset::Immediate(value) => f.set(Field::Imm, u32::from(value)),
        Offset::Register { rm, shift } => {
            f.set(Field::Rm, rm.number());
            set_shift(f, shift);
        }
    }
}

fn set_index(f: &mut Fields, index: Index) {
    let (p, w) = match index {
        Index::Offset => (true, false),
        Index::PreIndexed => (true, true),
        Index::PostIndexed => (false, false),
    };
    f.flag(Field::P, p);
    f.flag(Field::W, w);
}

fn set_block_mode(f: &mut Fields, mode: BlockMode) {
    f.flag(Field::P, mode.before());
    f.flag(Field::U, mode.up());
}

impl Op {
    /// The operation without its operands.
    pub(crate) const fn kind(&self) -> Kind {
        match *self {
            Self::Data { op, .. } => Kind::Data(op),
            Self::Compare { op, .. } => Kind::Compare(op),
            Self::Move { op, .. } => Kind::Move(op),
            Self::Wide { op, .. } => Kind::Wide(op),
            Self::Multiply { op, .. } => Kind::Multiply(op),
            Self::MultiplyLong { op, .. } => Kind::MultiplyLong(op),
            Self::Parallel { .. } => Kind::Parallel,
            Self::Arithmetic { op, .. } => Kind::Arithmetic(op),
            Self::Unary { op, .. } => Kind::Unary(op),
            Self::Saturate { op, .. } => Kind::Saturate(op),
            Self::Extend { op, .. } => Kind::Extend(op),
            Self::Pack { op, .. } => Kind::Pack(op),
            Self::Bitfield { op, .. } => Kind::Bitfield(op),
            Self::Status { .. } => Kind::Status,
            Self::StatusWrite { .. } => Kind::StatusWrite,
            Self::Branch { op, .. } => Kind::Branch(op),
            Self::Exchange { op, .. } => Kind::Exchange(op),
            Self::Call { op, .. } => Kind::Call(op),
            Self::Hint { op, .. } => Kind::Hint(op),
            Self::Barrier { op, .. } => Kind::Barrier(op),
            Self::ClearExclusive => Kind::ClearExclusive,
            Self::ExceptionReturn => Kind::ExceptionReturn,
            Self::Transfer { op, .. } => Kind::Transfer(op),
            Self::Dual { op, .. } => Kind::Dual(op),
            Self::Preload { op, .. } => Kind::Preload(op),
            Self::Block { op, .. } => Kind::Block(op),
            Self::Swap { op, .. } => Kind::Swap(op),
            Self::Exclusive { op, .. } => Kind::Exclusive(op),
            Self::ChangeState { .. } => Kind::ChangeState,
            Self::SetEndian { .. } => Kind::SetEndian,
            Self::SaveState { .. } => Kind::SaveState,
            Self::ReturnFromException { .. } => Kind::ReturnFromException,
            Self::Coprocessor { op, .. } => Kind::Coprocessor(op),
            Self::CoprocessorRegister { op, .. } => Kind::CoprocessorRegister(op),
            Self::CoprocessorRegisters { op, .. } => Kind::CoprocessorRegisters(op),
            Self::CoprocessorTransfer { op, .. } => Kind::CoprocessorTransfer(op),
            Self::CompareBranch { .. } => Kind::CompareBranch,
            Self::IfThen { .. } => Kind::IfThen,
            Self::TableBranch { .. } => Kind::TableBranch,
            Self::FpaDyadic { op, .. } => Kind::FpaDyadic(op),
            Self::FpaMonadic { op, .. } => Kind::FpaMonadic(op),
            Self::FpaRegister { op, .. } => Kind::FpaRegister(op),
            Self::FpaCompare { op, .. } => Kind::FpaCompare(op),
            Self::FpaTransfer { op, .. } => Kind::FpaTransfer(op),
            Self::FpaMultiple { op, .. } => Kind::FpaMultiple(op),
            Self::SimdFp(op) => Kind::SimdFp(op.kind()),
        }
    }

    /// Every field the operands set. A row writes those its layout has; the
    /// encoder decodes the result to refuse operands a row cannot hold.
    #[allow(clippy::too_many_lines)] // The inverse of `Kind::build`, arm for arm.
    pub(crate) fn fields(&self) -> Fields {
        let mut f = Fields::default();
        match *self {
            Self::Data {
                s, rd, rn, operand, ..
            } => {
                f.flag(Field::S, s);
                f.set(Field::Rd, rd.number());
                f.set(Field::Rn, rn.number());
                set_operand(&mut f, operand);
            }
            Self::Compare {
                rd, rn, operand, ..
            } => {
                f.set(Field::Rd, rd.number());
                f.set(Field::Rn, rn.number());
                set_operand(&mut f, operand);
            }
            Self::Move { s, rd, operand, .. } => {
                f.flag(Field::S, s);
                f.set(Field::Rd, rd.number());
                set_operand(&mut f, operand);
            }
            Self::Wide { rd, value, .. } => {
                f.set(Field::Rd, rd.number());
                f.set(Field::Imm, u32::from(value));
            }
            Self::Multiply {
                s,
                x,
                y,
                rd,
                rn,
                rm,
                ra,
                ..
            } => {
                f.flag(Field::S, s);
                f.flag(Field::X, x);
                f.flag(Field::Y, y);
                f.set(Field::Rd, rd.number());
                f.set(Field::Rn, rn.number());
                f.set(Field::Rm, rm.number());
                if let Some(ra) = ra {
                    f.set(Field::Ra, ra.number());
                }
            }
            Self::MultiplyLong {
                s,
                x,
                y,
                lo,
                hi,
                rn,
                rm,
                ..
            } => {
                f.flag(Field::S, s);
                f.flag(Field::X, x);
                f.flag(Field::Y, y);
                f.set(Field::Lo, lo.number());
                f.set(Field::Hi, hi.number());
                f.set(Field::Rn, rn.number());
                f.set(Field::Rm, rm.number());
            }
            Self::Parallel {
                prefix,
                lanes,
                rd,
                rn,
                rm,
            } => {
                f.set(
                    Field::Opc1,
                    match prefix {
                        Prefix::Signed => 0b001,
                        Prefix::SignedSaturating => 0b010,
                        Prefix::SignedHalving => 0b011,
                        Prefix::Unsigned => 0b101,
                        Prefix::UnsignedSaturating => 0b110,
                        Prefix::UnsignedHalving => 0b111,
                    },
                );
                f.set(
                    Field::Opc2,
                    match lanes {
                        Lanes::Add16 => 0b000,
                        Lanes::Asx => 0b001,
                        Lanes::Sax => 0b010,
                        Lanes::Sub16 => 0b011,
                        Lanes::Add8 => 0b100,
                        Lanes::Sub8 => 0b111,
                    },
                );
                f.set(Field::Rd, rd.number());
                f.set(Field::Rn, rn.number());
                f.set(Field::Rm, rm.number());
            }
            Self::Arithmetic { rd, rn, rm, .. } => {
                f.set(Field::Rd, rd.number());
                f.set(Field::Rn, rn.number());
                f.set(Field::Rm, rm.number());
            }
            Self::Unary { rd, rm, .. } => {
                f.set(Field::Rd, rd.number());
                f.set(Field::Rm, rm.number());
            }
            Self::Saturate {
                rd,
                saturate,
                rn,
                shift,
                ..
            } => {
                f.set(Field::Rd, rd.number());
                f.set(Field::Imm, u32::from(saturate));
                f.set(Field::Rn, rn.number());
                let (kind, amount) = shift.encode().unwrap_or((u32::MAX, u32::MAX));
                f.set(Field::Type, kind >> 1);
                f.set(Field::Amount, amount);
            }
            Self::Extend {
                rd,
                rn,
                rm,
                rotation,
                ..
            } => {
                f.set(Field::Rd, rd.number());
                if let Some(rn) = rn {
                    f.set(Field::Rn, rn.number());
                }
                f.set(Field::Rm, rm.number());
                f.set(Field::Rotation, u32::from(rotation));
            }
            Self::Pack {
                rd, rn, rm, shift, ..
            } => {
                f.set(Field::Rd, rd.number());
                f.set(Field::Rn, rn.number());
                f.set(Field::Rm, rm.number());
                let (_, amount) = shift.encode().unwrap_or((0, u32::MAX));
                f.set(Field::Amount, amount);
            }
            Self::Bitfield {
                rd, rn, lsb, top, ..
            } => {
                f.set(Field::Rd, rd.number());
                if let Some(rn) = rn {
                    f.set(Field::Rn, rn.number());
                }
                f.set(Field::Lsb, u32::from(lsb));
                f.set(Field::Msb, u32::from(top));
            }
            Self::Status { spsr, rd } => {
                f.flag(Field::Spsr, spsr);
                f.set(Field::Rd, rd.number());
            }
            Self::StatusWrite {
                spsr,
                fields,
                source,
            } => {
                f.flag(Field::Spsr, spsr);
                f.set(Field::Mask, u32::from(fields));
                match source {
                    Source::Immediate(immediate) => {
                        f.set(Field::Imm, u32::from(immediate.value));
                        f.set(Field::Rotate, u32::from(immediate.rotation));
                    }
                    Source::Register(rm) => f.set(Field::Rm, rm.number()),
                }
            }
            Self::Branch { offset, .. } => {
                // A misaligned offset loses its low bits here, so the word
                // decodes to another branch and is refused as unencodable.
                if (-(1 << 25)..1 << 25).contains(&offset) {
                    let bits = offset.cast_unsigned();
                    f.set(Field::Imm, (bits >> 2) & 0x00FF_FFFF);
                    f.set(Field::H, (bits >> 1) & 1);
                } else {
                    f.set(Field::Imm, u32::MAX);
                }
            }
            Self::Exchange { rm, .. } => f.set(Field::Rm, rm.number()),
            Self::Call { value, .. } => f.set(Field::Imm, value),
            Self::Hint { value, .. } => f.set(Field::Imm, u32::from(value)),
            Self::Barrier { option, .. } => f.set(Field::Imm, u32::from(option)),
            Self::ClearExclusive | Self::ExceptionReturn => {}
            Self::Transfer {
                op,
                rt,
                rn,
                up,
                offset,
                index,
            } => {
                f.set(Field::Rt, rt.number());
                f.set(Field::Rn, rn.number());
                f.flag(Field::U, up);
                set_offset(&mut f, offset);
                if !op.unprivileged() {
                    set_index(&mut f, index);
                }
            }
            Self::Dual {
                rt,
                rt2,
                rn,
                up,
                offset,
                index,
                ..
            } => {
                f.set(Field::Rt, rt.number());
                f.set(Field::Rt2, rt2.number());
                f.set(Field::Rn, rn.number());
                f.flag(Field::U, up);
                set_offset(&mut f, offset);
                set_index(&mut f, index);
            }
            Self::Preload { rn, up, offset, .. } => {
                f.set(Field::Rn, rn.number());
                f.flag(Field::U, up);
                set_offset(&mut f, offset);
            }
            Self::Block {
                rn,
                mode,
                writeback,
                user,
                list,
                ..
            } => {
                f.set(Field::Rn, rn.number());
                set_block_mode(&mut f, mode);
                f.flag(Field::W, writeback);
                f.flag(Field::B, user);
                f.set(Field::List, u32::from(list.0));
            }
            Self::Swap { rt, rt2, rn, .. } => {
                f.set(Field::Rt, rt.number());
                f.set(Field::Rt2, rt2.number());
                f.set(Field::Rn, rn.number());
            }
            Self::Exclusive {
                rd, rt, rt2, rn, ..
            } => {
                if let Some(rd) = rd {
                    f.set(Field::Rd, rd.number());
                }
                f.set(Field::Rt, rt.number());
                if let Some(rt2) = rt2 {
                    f.set(Field::Rt2, rt2.number());
                }
                f.set(Field::Rn, rn.number());
            }
            Self::CompareBranch {
                nonzero,
                rn,
                offset,
            } => {
                f.flag(Field::X, nonzero);
                f.set(Field::Rn, rn.number());
                f.set(
                    Field::Imm,
                    if offset & 1 == 0 {
                        offset >> 1
                    } else {
                        u32::MAX
                    },
                );
            }
            Self::IfThen { firstcond, mask } => {
                f.set(Field::Opc1, firstcond.bits());
                f.set(Field::Opc2, u32::from(mask));
            }
            Self::TableBranch { half, rn, rm } => {
                f.flag(Field::X, half);
                f.set(Field::Rn, rn.number());
                f.set(Field::Rm, rm.number());
            }
            Self::ChangeState {
                imod,
                change_mode,
                interrupts,
                mode,
            } => {
                f.set(Field::Imod, u32::from(imod));
                f.flag(Field::ChangeMode, change_mode);
                f.set(Field::Interrupts, u32::from(interrupts));
                f.set(Field::Mode, u32::from(mode));
            }
            Self::SetEndian { big } => f.flag(Field::Endian, big),
            Self::SaveState {
                mode,
                writeback,
                bank,
            } => {
                set_block_mode(&mut f, mode);
                f.flag(Field::W, writeback);
                f.set(Field::Mode, u32::from(bank));
            }
            Self::ReturnFromException {
                rn,
                mode,
                writeback,
            } => {
                f.set(Field::Rn, rn.number());
                set_block_mode(&mut f, mode);
                f.flag(Field::W, writeback);
            }
            Self::Coprocessor {
                cp,
                opc1,
                crd,
                crn,
                crm,
                opc2,
                ..
            } => {
                f.set(Field::Cp, u32::from(cp));
                f.set(Field::Opc1, u32::from(opc1));
                f.set(Field::CRd, u32::from(crd));
                f.set(Field::CRn, u32::from(crn));
                f.set(Field::CRm, u32::from(crm));
                f.set(Field::Opc2, u32::from(opc2));
            }
            Self::CoprocessorRegister {
                cp,
                opc1,
                rt,
                crn,
                crm,
                opc2,
                ..
            } => {
                f.set(Field::Cp, u32::from(cp));
                f.set(Field::Opc1, u32::from(opc1));
                f.set(Field::Rt, rt.number());
                f.set(Field::CRn, u32::from(crn));
                f.set(Field::CRm, u32::from(crm));
                f.set(Field::Opc2, u32::from(opc2));
            }
            Self::CoprocessorRegisters {
                cp,
                opc1,
                rt,
                rt2,
                crm,
                ..
            } => {
                f.set(Field::Cp, u32::from(cp));
                f.set(Field::Opc1, u32::from(opc1));
                f.set(Field::Rt, rt.number());
                f.set(Field::Rt2, rt2.number());
                f.set(Field::CRm, u32::from(crm));
            }
            Self::CoprocessorTransfer {
                cp,
                crd,
                rn,
                long,
                up,
                index,
                offset,
                ..
            } => {
                f.set(Field::Cp, u32::from(cp));
                f.set(Field::CRd, u32::from(crd));
                f.set(Field::Rn, rn.number());
                f.flag(Field::B, long);
                let (p, w) = index.bits();
                f.flag(Field::P, p);
                f.flag(Field::W, w);
                f.flag(Field::U, up);
                f.set(Field::Imm, u32::from(offset));
            }
            Self::FpaDyadic {
                precision,
                rounding,
                fd,
                fn_,
                fm,
                ..
            } => {
                set_rounded(&mut f, precision, rounding);
                f.set(Field::CRd, u32::from(fd));
                f.set(Field::CRn, u32::from(fn_));
                set_fpa_operand(&mut f, fm);
            }
            Self::FpaMonadic {
                precision,
                rounding,
                fd,
                fm,
                ..
            } => {
                set_rounded(&mut f, precision, rounding);
                f.set(Field::CRd, u32::from(fd));
                set_fpa_operand(&mut f, fm);
            }
            Self::FpaRegister {
                precision,
                rounding,
                fn_,
                fm,
                rd,
                ..
            } => {
                set_rounded(&mut f, precision, rounding);
                f.set(Field::CRn, u32::from(fn_));
                set_fpa_operand(&mut f, fm);
                f.set(Field::Rt, rd.number());
            }
            Self::FpaCompare {
                precision,
                fn_,
                fm,
                rd,
                ..
            } => {
                f.set(Field::Precision, precision as u32);
                f.set(Field::CRn, u32::from(fn_));
                set_fpa_operand(&mut f, fm);
                f.set(Field::Rt, rd.number());
            }
            Self::FpaTransfer {
                format,
                fd,
                rn,
                up,
                index,
                offset,
                ..
            } => {
                f.set(Field::Precision, format as u32);
                f.set(Field::CRd, u32::from(fd));
                set_fpa_address(&mut f, rn, up, index, offset);
            }
            Self::FpaMultiple {
                fd,
                count,
                rn,
                up,
                index,
                offset,
                ..
            } => {
                f.set(Field::Count, u32::from(count) & 3);
                f.set(Field::CRd, u32::from(fd));
                set_fpa_address(&mut f, rn, up, index, offset);
            }
            Self::SimdFp(op) => op.fields(&mut f),
        }
        f
    }
}

// An FPA precision, rounding or format is encoded as its place in its
// `ALL` list, which decoding indexes: their declaration order.
const _: () = {
    let mut i = 0;
    while i < Precision::ALL.len() {
        assert!(Precision::ALL[i] as usize == i);
        i += 1;
    }
    let mut i = 0;
    while i < Rounding::ALL.len() {
        assert!(Rounding::ALL[i] as usize == i);
        i += 1;
    }
    let mut i = 0;
    while i < Format::ALL.len() {
        assert!(Format::ALL[i] as usize == i);
        i += 1;
    }
};

fn set_rounded(f: &mut Fields, precision: Precision, rounding: Rounding) {
    f.set(Field::Precision, precision as u32);
    f.set(Field::Rounding, rounding as u32);
}

fn set_fpa_operand(f: &mut Fields, operand: FpaOperand) {
    f.set(
        Field::CRm,
        match operand {
            FpaOperand::Register(n) => u32::from(n),
            FpaOperand::Constant(n) => 8 | u32::from(n),
        },
    );
}

fn set_fpa_address(f: &mut Fields, rn: Reg, up: bool, index: CoIndex, offset: u8) {
    f.set(Field::Rn, rn.number());
    f.flag(Field::U, up);
    let (p, w) = index.bits();
    f.flag(Field::P, p);
    f.flag(Field::W, w);
    f.set(Field::Imm, u32::from(offset));
}
