//! The T32 encoding table: Thumb's 16-bit encodings from ARMv4T and
//! Thumb-2's 32-bit encodings from ARMv6T2, through ARMv8-A's AArch32
//! integer additions. The floating-point and Advanced SIMD encodings are in
//! the coprocessor spaces this table leaves to them.
//!
//! The 16-bit rows are grouped by their top three bits, the 32-bit rows by
//! [`wide_group`]. T32's encodings cut across one another, a register field
//! holding the PC naming another instruction, so rows may overlap: the first
//! match decides, and a test holds that no row is wholly covered by an
//! earlier one.
//!
//! A core with Thumb but not Thumb-2 runs `BL` and `BLX` as two 16-bit
//! halves. No profile here is one, so the table holds them as Thumb-2 does.

use super::form::{Base, Block, Branch, Flags, Rd, Rn, Shape, Source};
use crate::encoding::form::Kind;
use crate::encoding::layout::Layout;
use crate::instruction::{
    ArithmeticOp as A, BarrierOp, BitfieldOp, BlockMode, BlockOp, BranchOp, CallOp, CompareOp,
    DataOp, DualOp, ExchangeOp, ExclusiveOp as X, ExtendOp as E, HintOp, LongOp, MoveOp,
    MultiplyOp as M, PackOp, PreloadOp, SaturateOp, ShiftType, TransferOp as T, UnaryOp, WideOp,
};
use crate::isa::Feature::{self, Crc32, Divide, Mp, V4T, V5, V6, V6T2, V7, V8};

/// One encoding: what it decodes to and how, the feature it needs and its
/// layout.
#[derive(Debug)]
pub(super) struct Row {
    pub kind: Kind,
    pub shape: Shape,
    pub since: Feature,
    pub layout: Layout,
    /// Whether the encoding is 32 bits: two halfwords.
    pub wide: bool,
    /// Whether the encoding is UNPREDICTABLE in an IT block.
    pub outside: bool,
}

impl Row {
    /// The row, UNPREDICTABLE in an IT block.
    const fn outside(self) -> Self {
        Self {
            outside: true,
            ..self
        }
    }
}

const fn narrow(pattern: &str, kind: Kind, shape: Shape, since: Feature) -> Row {
    Row {
        kind,
        shape,
        since,
        layout: Layout::halfword(pattern),
        wide: false,
        outside: false,
    }
}

const fn wide(pattern: &str, kind: Kind, shape: Shape, since: Feature) -> Row {
    Row {
        kind,
        shape,
        since,
        layout: Layout::of(pattern),
        wide: true,
        outside: false,
    }
}

const C: Shape = Shape::Common;

const fn data(rd: Rd, rn: Rn, source: Source, flags: Flags) -> Shape {
    Shape::Data {
        rd,
        rn,
        source,
        flags,
    }
}

/// Three low registers, or two and a small immediate.
const LOW: Shape = data(Rd::Field, Rn::Field, Source::Register, Flags::OutsideIt);
const LOW_IMMEDIATE: Shape = data(Rd::Field, Rn::Field, Source::Literal(0), Flags::OutsideIt);
/// A low register's shifted or complemented copy.
const MOVE_LOW: Shape = data(Rd::Field, Rn::None, Source::Register, Flags::OutsideIt);
const MOVE_IMMEDIATE8: Shape = data(Rd::Field, Rn::None, Source::Literal(0), Flags::OutsideIt);
/// A two-operand form: Rdn and a register or immediate.
const RDN: Shape = data(Rd::Field, Rn::Rd, Source::Register, Flags::OutsideIt);
const RDN_IMMEDIATE: Shape = data(Rd::Field, Rn::Rd, Source::Literal(0), Flags::OutsideIt);
/// A comparison with a register or immediate.
const TEST: Shape = data(Rd::Field, Rn::Field, Source::Register, Flags::Never);
const TEST_IMMEDIATE8: Shape = data(Rd::Field, Rn::Field, Source::Literal(0), Flags::Never);
/// The high-register forms, which never set the flags.
const ADD_HIGH: Shape = data(Rd::Field, Rn::Rd, Source::Register, Flags::Never);
const MOVE_HIGH: Shape = data(Rd::Field, Rn::None, Source::Register, Flags::Never);
/// The word-scaled additions to the PC and SP.
const ADR: Shape = data(Rd::Field, Rn::Pc, Source::Literal(2), Flags::Never);
const ADD_SP: Shape = data(Rd::Field, Rn::Sp, Source::Literal(2), Flags::Never);
const ADJUST_SP: Shape = data(Rd::Sp, Rn::Sp, Source::Literal(2), Flags::Never);

const fn shifted(kind: ShiftType) -> Shape {
    data(
        Rd::Field,
        Rn::None,
        Source::ShiftedRdn(kind),
        Flags::OutsideIt,
    )
}

/// Thumb-2's data processing by a shifted register.
const DP_REGISTER: Shape = data(Rd::Field, Rn::Field, Source::Register, Flags::Field);
const MOVE_REGISTER: Shape = data(Rd::Field, Rn::None, Source::Register, Flags::Field);
/// By a modified immediate.
const DP_EXPANDED: Shape = data(Rd::Field, Rn::Field, Source::Expanded, Flags::Field);
const MOVE_EXPANDED: Shape = data(Rd::Field, Rn::None, Source::Expanded, Flags::Field);
const TEST_EXPANDED: Shape = data(Rd::Field, Rn::Field, Source::Expanded, Flags::Never);
/// By a plain twelve-bit immediate.
const PLAIN: Shape = data(Rd::Field, Rn::Field, Source::Literal(0), Flags::Never);
const ADR_WIDE: Shape = data(Rd::Field, Rn::Pc, Source::Literal(0), Flags::Never);
const SUBS_PC_LR: Shape = data(Rd::Pc, Rn::Lr, Source::Literal(0), Flags::Always);

const fn transfer(base: Base, scale: u8) -> Shape {
    Shape::Transfer { base, scale }
}

/// A transfer addressed from Rn, by a register or an unscaled immediate.
const SINGLE: Shape = transfer(Base::Field, 0);
const LITERAL: Shape = transfer(Base::Pc, 0);
const PRELOAD_LITERAL: Shape = Shape::Preload {
    up: None,
    base: Base::Pc,
};
const PRELOAD_UP: Shape = Shape::Preload {
    up: Some(true),
    base: Base::Field,
};
const PRELOAD_DOWN: Shape = Shape::Preload {
    up: Some(false),
    base: Base::Field,
};

/// 16-bit rows whose top three bits are `000`.
static NARROW_000: &[Row] = &[
    narrow("0001 100m mmnn nddd", Kind::Data(DataOp::Add), LOW, V4T),
    narrow("0001 101m mmnn nddd", Kind::Data(DataOp::Sub), LOW, V4T),
    narrow(
        "0001 110i iinn nddd",
        Kind::Data(DataOp::Add),
        LOW_IMMEDIATE,
        V4T,
    ),
    narrow(
        "0001 111i iinn nddd",
        Kind::Data(DataOp::Sub),
        LOW_IMMEDIATE,
        V4T,
    ),
    narrow(
        "0000 0000 00mm mddd",
        Kind::Move(MoveOp::Mov),
        MOVE_LOW,
        V4T,
    )
    .outside(),
    narrow(
        "000y ykkk kkmm mddd",
        Kind::Move(MoveOp::Mov),
        MOVE_LOW,
        V4T,
    ),
];

/// 16-bit rows whose top three bits are `001`.
static NARROW_001: &[Row] = &[
    narrow(
        "0010 0ddd iiii iiii",
        Kind::Move(MoveOp::Mov),
        MOVE_IMMEDIATE8,
        V4T,
    ),
    narrow(
        "0010 1nnn iiii iiii",
        Kind::Compare(CompareOp::Cmp),
        TEST_IMMEDIATE8,
        V4T,
    ),
    narrow(
        "0011 0ddd iiii iiii",
        Kind::Data(DataOp::Add),
        RDN_IMMEDIATE,
        V4T,
    ),
    narrow(
        "0011 1ddd iiii iiii",
        Kind::Data(DataOp::Sub),
        RDN_IMMEDIATE,
        V4T,
    ),
];

/// 16-bit rows whose top three bits are `010`.
static NARROW_010: &[Row] = &[
    narrow("0100 0000 00mm mddd", Kind::Data(DataOp::And), RDN, V4T),
    narrow("0100 0000 01mm mddd", Kind::Data(DataOp::Eor), RDN, V4T),
    narrow(
        "0100 0000 10ss sddd",
        Kind::Move(MoveOp::Mov),
        shifted(ShiftType::Lsl),
        V4T,
    ),
    narrow(
        "0100 0000 11ss sddd",
        Kind::Move(MoveOp::Mov),
        shifted(ShiftType::Lsr),
        V4T,
    ),
    narrow(
        "0100 0001 00ss sddd",
        Kind::Move(MoveOp::Mov),
        shifted(ShiftType::Asr),
        V4T,
    ),
    narrow("0100 0001 01mm mddd", Kind::Data(DataOp::Adc), RDN, V4T),
    narrow("0100 0001 10mm mddd", Kind::Data(DataOp::Sbc), RDN, V4T),
    narrow(
        "0100 0001 11ss sddd",
        Kind::Move(MoveOp::Mov),
        shifted(ShiftType::Ror),
        V4T,
    ),
    narrow(
        "0100 0010 00mm mnnn",
        Kind::Compare(CompareOp::Tst),
        TEST,
        V4T,
    ),
    narrow(
        "0100 0010 01nn nddd",
        Kind::Data(DataOp::Rsb),
        LOW_IMMEDIATE,
        V4T,
    ),
    narrow(
        "0100 0010 10mm mnnn",
        Kind::Compare(CompareOp::Cmp),
        TEST,
        V4T,
    ),
    narrow(
        "0100 0010 11mm mnnn",
        Kind::Compare(CompareOp::Cmn),
        TEST,
        V4T,
    ),
    narrow("0100 0011 00mm mddd", Kind::Data(DataOp::Orr), RDN, V4T),
    narrow(
        "0100 0011 01nn nddd",
        Kind::Multiply(M::Mul),
        Shape::Mul16,
        V4T,
    ),
    narrow("0100 0011 10mm mddd", Kind::Data(DataOp::Bic), RDN, V4T),
    narrow(
        "0100 0011 11mm mddd",
        Kind::Move(MoveOp::Mvn),
        MOVE_LOW,
        V4T,
    ),
    narrow(
        "0100 0100 dmmm mddd",
        Kind::Data(DataOp::Add),
        ADD_HIGH,
        V4T,
    ),
    narrow(
        "0100 0101 nmmm mnnn",
        Kind::Compare(CompareOp::Cmp),
        TEST,
        V4T,
    ),
    narrow(
        "0100 0110 dmmm mddd",
        Kind::Move(MoveOp::Mov),
        MOVE_HIGH,
        V4T,
    ),
    narrow(
        "0100 0111 0mmm m---",
        Kind::Exchange(ExchangeOp::Bx),
        C,
        V4T,
    ),
    narrow(
        "0100 0111 1mmm m---",
        Kind::Exchange(ExchangeOp::Blx),
        C,
        V5,
    ),
    narrow(
        "0100 1ttt iiii iiii",
        Kind::Transfer(T::Ldr),
        transfer(Base::Pc, 2),
        V4T,
    ),
    narrow("0101 000m mmnn nttt", Kind::Transfer(T::Str), SINGLE, V4T),
    narrow("0101 001m mmnn nttt", Kind::Transfer(T::Strh), SINGLE, V4T),
    narrow("0101 010m mmnn nttt", Kind::Transfer(T::Strb), SINGLE, V4T),
    narrow("0101 011m mmnn nttt", Kind::Transfer(T::Ldrsb), SINGLE, V4T),
    narrow("0101 100m mmnn nttt", Kind::Transfer(T::Ldr), SINGLE, V4T),
    narrow("0101 101m mmnn nttt", Kind::Transfer(T::Ldrh), SINGLE, V4T),
    narrow("0101 110m mmnn nttt", Kind::Transfer(T::Ldrb), SINGLE, V4T),
    narrow("0101 111m mmnn nttt", Kind::Transfer(T::Ldrsh), SINGLE, V4T),
];

/// 16-bit rows whose top three bits are `011`.
static NARROW_011: &[Row] = &[
    narrow(
        "0110 0iii iinn nttt",
        Kind::Transfer(T::Str),
        transfer(Base::Field, 2),
        V4T,
    ),
    narrow(
        "0110 1iii iinn nttt",
        Kind::Transfer(T::Ldr),
        transfer(Base::Field, 2),
        V4T,
    ),
    narrow("0111 0iii iinn nttt", Kind::Transfer(T::Strb), SINGLE, V4T),
    narrow("0111 1iii iinn nttt", Kind::Transfer(T::Ldrb), SINGLE, V4T),
];

/// 16-bit rows whose top three bits are `100`.
static NARROW_100: &[Row] = &[
    narrow(
        "1000 0iii iinn nttt",
        Kind::Transfer(T::Strh),
        transfer(Base::Field, 1),
        V4T,
    ),
    narrow(
        "1000 1iii iinn nttt",
        Kind::Transfer(T::Ldrh),
        transfer(Base::Field, 1),
        V4T,
    ),
    narrow(
        "1001 0ttt iiii iiii",
        Kind::Transfer(T::Str),
        transfer(Base::Sp, 2),
        V4T,
    ),
    narrow(
        "1001 1ttt iiii iiii",
        Kind::Transfer(T::Ldr),
        transfer(Base::Sp, 2),
        V4T,
    ),
];

/// 16-bit rows whose top three bits are `101`.
static NARROW_101: &[Row] = &[
    narrow("1010 0ddd iiii iiii", Kind::Data(DataOp::Add), ADR, V4T),
    narrow("1010 1ddd iiii iiii", Kind::Data(DataOp::Add), ADD_SP, V4T),
    narrow(
        "1011 0000 0iii iiii",
        Kind::Data(DataOp::Add),
        ADJUST_SP,
        V4T,
    ),
    narrow(
        "1011 0000 1iii iiii",
        Kind::Data(DataOp::Sub),
        ADJUST_SP,
        V4T,
    ),
    narrow("1011 X0i1 iiii innn", Kind::CompareBranch, C, V6T2).outside(),
    narrow("1011 0010 00mm mddd", Kind::Extend(E::Sxth), C, V6),
    narrow("1011 0010 01mm mddd", Kind::Extend(E::Sxtb), C, V6),
    narrow("1011 0010 10mm mddd", Kind::Extend(E::Uxth), C, V6),
    narrow("1011 0010 11mm mddd", Kind::Extend(E::Uxtb), C, V6),
    narrow(
        "1011 010X RRRR RRRR",
        Kind::Block(BlockOp::Stm),
        Shape::Block(Block::Push),
        V4T,
    ),
    narrow("1011 0110 010+ E---", Kind::SetEndian, C, V6).outside(),
    narrow("1011 0110 011I -jjj", Kind::ChangeState, Shape::Cps16, V6).outside(),
    narrow("1011 1010 00mm mddd", Kind::Unary(UnaryOp::Rev), C, V6),
    narrow("1011 1010 01mm mddd", Kind::Unary(UnaryOp::Rev16), C, V6),
    narrow("1011 1010 11mm mddd", Kind::Unary(UnaryOp::Revsh), C, V6),
    narrow(
        "1011 110X RRRR RRRR",
        Kind::Block(BlockOp::Ldm),
        Shape::Block(Block::Pop),
        V4T,
    ),
    narrow("1011 1110 iiii iiii", Kind::Call(CallOp::Bkpt), C, V5),
    narrow("1011 1111 0000 0000", Kind::Hint(HintOp::Nop), C, V6T2),
    narrow("1011 1111 0001 0000", Kind::Hint(HintOp::Yield), C, V7),
    narrow("1011 1111 0010 0000", Kind::Hint(HintOp::Wfe), C, V7),
    narrow("1011 1111 0011 0000", Kind::Hint(HintOp::Wfi), C, V7),
    narrow("1011 1111 0100 0000", Kind::Hint(HintOp::Sev), C, V7),
    narrow("1011 1111 0101 0000", Kind::Hint(HintOp::Sevl), C, V8),
    narrow("1011 1111 iiii 0000", Kind::Hint(HintOp::Hint), C, V6T2),
    narrow("1011 1111 oooo qqqq", Kind::IfThen, C, V6T2).outside(),
];

/// 16-bit rows whose top three bits are `110`.
static NARROW_110: &[Row] = &[
    narrow(
        "1100 0nnn RRRR RRRR",
        Kind::Block(BlockOp::Stm),
        Shape::Block(Block::Store),
        V4T,
    ),
    narrow(
        "1100 1nnn RRRR RRRR",
        Kind::Block(BlockOp::Ldm),
        Shape::Block(Block::Load),
        V4T,
    ),
    narrow("1101 1110 iiii iiii", Kind::Call(CallOp::Udf), C, V4T),
    narrow("1101 1111 iiii iiii", Kind::Call(CallOp::Swi), C, V4T),
    narrow(
        "1101 cccc iiii iiii",
        Kind::Branch(BranchOp::B),
        Shape::Branch(Branch::Cond8),
        V4T,
    )
    .outside(),
];

/// 16-bit rows whose top three bits are `111`: the rest of that space is the
/// first halfword of a 32-bit encoding.
static NARROW_111: &[Row] = &[narrow(
    "1110 0iii iiii iiii",
    Kind::Branch(BranchOp::B),
    Shape::Branch(Branch::Imm11),
    V4T,
)];

/// 32-bit load and store multiple, dual, exclusive and table branch rows.
static WIDE_MULTIPLE: &[Row] = &[
    wide(
        "1110 1000 00W0 ++-+ ++-- ---- ---g gggg",
        Kind::SaveState,
        Shape::State(BlockMode::DecrementBefore),
        V6T2,
    ),
    wide(
        "1110 1001 10W0 ++-+ ++-- ---- ---g gggg",
        Kind::SaveState,
        Shape::State(BlockMode::IncrementAfter),
        V6T2,
    ),
    wide(
        "1110 1000 00W1 nnnn ++-- ---- ---- ----",
        Kind::ReturnFromException,
        Shape::State(BlockMode::DecrementBefore),
        V6T2,
    ),
    wide(
        "1110 1001 10W1 nnnn ++-- ---- ---- ----",
        Kind::ReturnFromException,
        Shape::State(BlockMode::IncrementAfter),
        V6T2,
    ),
    wide(
        "1110 1000 10W0 nnnn -RRR RRRR RRRR RRRR",
        Kind::Block(BlockOp::Stm),
        Shape::Block(Block::Wide(BlockMode::IncrementAfter)),
        V6T2,
    ),
    wide(
        "1110 1000 10W1 nnnn RRRR RRRR RRRR RRRR",
        Kind::Block(BlockOp::Ldm),
        Shape::Block(Block::Wide(BlockMode::IncrementAfter)),
        V6T2,
    ),
    wide(
        "1110 1001 00W0 nnnn -RRR RRRR RRRR RRRR",
        Kind::Block(BlockOp::Stm),
        Shape::Block(Block::Wide(BlockMode::DecrementBefore)),
        V6T2,
    ),
    wide(
        "1110 1001 00W1 nnnn RRRR RRRR RRRR RRRR",
        Kind::Block(BlockOp::Ldm),
        Shape::Block(Block::Wide(BlockMode::DecrementBefore)),
        V6T2,
    ),
    wide(
        "1110 1000 0100 nnnn tttt dddd iiii iiii",
        Kind::Exclusive(X::Strex),
        Shape::ExclusiveOffset,
        V6T2,
    ),
    wide(
        "1110 1000 0101 nnnn tttt ++++ iiii iiii",
        Kind::Exclusive(X::Ldrex),
        Shape::ExclusiveOffset,
        V6T2,
    ),
    wide(
        "1110 1000 1100 nnnn tttt ++++ 0100 dddd",
        Kind::Exclusive(X::Strexb),
        C,
        V7,
    ),
    wide(
        "1110 1000 1100 nnnn tttt ++++ 0101 dddd",
        Kind::Exclusive(X::Strexh),
        C,
        V7,
    ),
    wide(
        "1110 1000 1100 nnnn tttt TTTT 0111 dddd",
        Kind::Exclusive(X::Strexd),
        C,
        V7,
    ),
    wide(
        "1110 1000 1100 nnnn tttt ++++ 1000 ++++",
        Kind::Exclusive(X::Stlb),
        C,
        V8,
    ),
    wide(
        "1110 1000 1100 nnnn tttt ++++ 1001 ++++",
        Kind::Exclusive(X::Stlh),
        C,
        V8,
    ),
    wide(
        "1110 1000 1100 nnnn tttt ++++ 1010 ++++",
        Kind::Exclusive(X::Stl),
        C,
        V8,
    ),
    wide(
        "1110 1000 1100 nnnn tttt ++++ 1100 dddd",
        Kind::Exclusive(X::Stlexb),
        C,
        V8,
    ),
    wide(
        "1110 1000 1100 nnnn tttt ++++ 1101 dddd",
        Kind::Exclusive(X::Stlexh),
        C,
        V8,
    ),
    wide(
        "1110 1000 1100 nnnn tttt ++++ 1110 dddd",
        Kind::Exclusive(X::Stlex),
        C,
        V8,
    ),
    wide(
        "1110 1000 1100 nnnn tttt TTTT 1111 dddd",
        Kind::Exclusive(X::Stlexd),
        C,
        V8,
    ),
    wide(
        "1110 1000 1101 nnnn ++++ ---- 000X mmmm",
        Kind::TableBranch,
        C,
        V6T2,
    ),
    wide(
        "1110 1000 1101 nnnn tttt ++++ 0100 ++++",
        Kind::Exclusive(X::Ldrexb),
        C,
        V7,
    ),
    wide(
        "1110 1000 1101 nnnn tttt ++++ 0101 ++++",
        Kind::Exclusive(X::Ldrexh),
        C,
        V7,
    ),
    wide(
        "1110 1000 1101 nnnn tttt TTTT 0111 ++++",
        Kind::Exclusive(X::Ldrexd),
        C,
        V7,
    ),
    wide(
        "1110 1000 1101 nnnn tttt ++++ 1000 ++++",
        Kind::Exclusive(X::Ldab),
        C,
        V8,
    ),
    wide(
        "1110 1000 1101 nnnn tttt ++++ 1001 ++++",
        Kind::Exclusive(X::Ldah),
        C,
        V8,
    ),
    wide(
        "1110 1000 1101 nnnn tttt ++++ 1010 ++++",
        Kind::Exclusive(X::Lda),
        C,
        V8,
    ),
    wide(
        "1110 1000 1101 nnnn tttt ++++ 1100 ++++",
        Kind::Exclusive(X::Ldaexb),
        C,
        V8,
    ),
    wide(
        "1110 1000 1101 nnnn tttt ++++ 1101 ++++",
        Kind::Exclusive(X::Ldaexh),
        C,
        V8,
    ),
    wide(
        "1110 1000 1101 nnnn tttt ++++ 1110 ++++",
        Kind::Exclusive(X::Ldaex),
        C,
        V8,
    ),
    wide(
        "1110 1000 1101 nnnn tttt TTTT 1111 ++++",
        Kind::Exclusive(X::Ldaexd),
        C,
        V8,
    ),
    // P and W both clear are the rows above.
    wide(
        "1110 100P U1W0 nnnn tttt TTTT iiii iiii",
        Kind::Dual(DualOp::Strd),
        Shape::Dual,
        V6T2,
    ),
    wide(
        "1110 100P U1W1 nnnn tttt TTTT iiii iiii",
        Kind::Dual(DualOp::Ldrd),
        Shape::Dual,
        V6T2,
    ),
];

/// 32-bit data processing by a shifted register.
static WIDE_SHIFTED: &[Row] = &[
    wide(
        "1110 1010 0001 nnnn -kkk 1111 kkyy mmmm",
        Kind::Compare(CompareOp::Tst),
        TEST,
        V6T2,
    ),
    wide(
        "1110 1010 000S nnnn -kkk dddd kkyy mmmm",
        Kind::Data(DataOp::And),
        DP_REGISTER,
        V6T2,
    ),
    wide(
        "1110 1010 001S nnnn -kkk dddd kkyy mmmm",
        Kind::Data(DataOp::Bic),
        DP_REGISTER,
        V6T2,
    ),
    wide(
        "1110 1010 010S 1111 -kkk dddd kkyy mmmm",
        Kind::Move(MoveOp::Mov),
        MOVE_REGISTER,
        V6T2,
    ),
    wide(
        "1110 1010 010S nnnn -kkk dddd kkyy mmmm",
        Kind::Data(DataOp::Orr),
        DP_REGISTER,
        V6T2,
    ),
    wide(
        "1110 1010 011S 1111 -kkk dddd kkyy mmmm",
        Kind::Move(MoveOp::Mvn),
        MOVE_REGISTER,
        V6T2,
    ),
    wide(
        "1110 1010 011S nnnn -kkk dddd kkyy mmmm",
        Kind::Data(DataOp::Orn),
        DP_REGISTER,
        V6T2,
    ),
    wide(
        "1110 1010 1001 nnnn -kkk 1111 kkyy mmmm",
        Kind::Compare(CompareOp::Teq),
        TEST,
        V6T2,
    ),
    wide(
        "1110 1010 100S nnnn -kkk dddd kkyy mmmm",
        Kind::Data(DataOp::Eor),
        DP_REGISTER,
        V6T2,
    ),
    wide(
        "1110 1010 1100 nnnn -kkk dddd kk00 mmmm",
        Kind::Pack(PackOp::Pkhbt),
        C,
        V6T2,
    ),
    wide(
        "1110 1010 1100 nnnn -kkk dddd kk10 mmmm",
        Kind::Pack(PackOp::Pkhtb),
        C,
        V6T2,
    ),
    wide(
        "1110 1011 0001 nnnn -kkk 1111 kkyy mmmm",
        Kind::Compare(CompareOp::Cmn),
        TEST,
        V6T2,
    ),
    wide(
        "1110 1011 000S nnnn -kkk dddd kkyy mmmm",
        Kind::Data(DataOp::Add),
        DP_REGISTER,
        V6T2,
    ),
    wide(
        "1110 1011 010S nnnn -kkk dddd kkyy mmmm",
        Kind::Data(DataOp::Adc),
        DP_REGISTER,
        V6T2,
    ),
    wide(
        "1110 1011 011S nnnn -kkk dddd kkyy mmmm",
        Kind::Data(DataOp::Sbc),
        DP_REGISTER,
        V6T2,
    ),
    wide(
        "1110 1011 1011 nnnn -kkk 1111 kkyy mmmm",
        Kind::Compare(CompareOp::Cmp),
        TEST,
        V6T2,
    ),
    wide(
        "1110 1011 101S nnnn -kkk dddd kkyy mmmm",
        Kind::Data(DataOp::Sub),
        DP_REGISTER,
        V6T2,
    ),
    wide(
        "1110 1011 110S nnnn -kkk dddd kkyy mmmm",
        Kind::Data(DataOp::Rsb),
        DP_REGISTER,
        V6T2,
    ),
];

/// 32-bit data processing by an immediate, and the saturations and
/// bitfields.
static WIDE_IMMEDIATE: &[Row] = &[
    wide(
        "1111 0H00 0001 nnnn 0kkk 1111 iiii iiii",
        Kind::Compare(CompareOp::Tst),
        TEST_EXPANDED,
        V6T2,
    ),
    wide(
        "1111 0H00 000S nnnn 0kkk dddd iiii iiii",
        Kind::Data(DataOp::And),
        DP_EXPANDED,
        V6T2,
    ),
    wide(
        "1111 0H00 001S nnnn 0kkk dddd iiii iiii",
        Kind::Data(DataOp::Bic),
        DP_EXPANDED,
        V6T2,
    ),
    wide(
        "1111 0H00 010S 1111 0kkk dddd iiii iiii",
        Kind::Move(MoveOp::Mov),
        MOVE_EXPANDED,
        V6T2,
    ),
    wide(
        "1111 0H00 010S nnnn 0kkk dddd iiii iiii",
        Kind::Data(DataOp::Orr),
        DP_EXPANDED,
        V6T2,
    ),
    wide(
        "1111 0H00 011S 1111 0kkk dddd iiii iiii",
        Kind::Move(MoveOp::Mvn),
        MOVE_EXPANDED,
        V6T2,
    ),
    wide(
        "1111 0H00 011S nnnn 0kkk dddd iiii iiii",
        Kind::Data(DataOp::Orn),
        DP_EXPANDED,
        V6T2,
    ),
    wide(
        "1111 0H00 1001 nnnn 0kkk 1111 iiii iiii",
        Kind::Compare(CompareOp::Teq),
        TEST_EXPANDED,
        V6T2,
    ),
    wide(
        "1111 0H00 100S nnnn 0kkk dddd iiii iiii",
        Kind::Data(DataOp::Eor),
        DP_EXPANDED,
        V6T2,
    ),
    wide(
        "1111 0H01 0001 nnnn 0kkk 1111 iiii iiii",
        Kind::Compare(CompareOp::Cmn),
        TEST_EXPANDED,
        V6T2,
    ),
    wide(
        "1111 0H01 000S nnnn 0kkk dddd iiii iiii",
        Kind::Data(DataOp::Add),
        DP_EXPANDED,
        V6T2,
    ),
    wide(
        "1111 0H01 010S nnnn 0kkk dddd iiii iiii",
        Kind::Data(DataOp::Adc),
        DP_EXPANDED,
        V6T2,
    ),
    wide(
        "1111 0H01 011S nnnn 0kkk dddd iiii iiii",
        Kind::Data(DataOp::Sbc),
        DP_EXPANDED,
        V6T2,
    ),
    wide(
        "1111 0H01 1011 nnnn 0kkk 1111 iiii iiii",
        Kind::Compare(CompareOp::Cmp),
        TEST_EXPANDED,
        V6T2,
    ),
    wide(
        "1111 0H01 101S nnnn 0kkk dddd iiii iiii",
        Kind::Data(DataOp::Sub),
        DP_EXPANDED,
        V6T2,
    ),
    wide(
        "1111 0H01 110S nnnn 0kkk dddd iiii iiii",
        Kind::Data(DataOp::Rsb),
        DP_EXPANDED,
        V6T2,
    ),
    wide(
        "1111 0H10 0000 1111 0kkk dddd iiii iiii",
        Kind::Data(DataOp::Add),
        ADR_WIDE,
        V6T2,
    ),
    wide(
        "1111 0H10 0000 nnnn 0kkk dddd iiii iiii",
        Kind::Data(DataOp::Add),
        PLAIN,
        V6T2,
    ),
    wide(
        "1111 0H10 0100 eeee 0kkk dddd iiii iiii",
        Kind::Wide(WideOp::Movw),
        Shape::Wide,
        V6T2,
    ),
    wide(
        "1111 0H10 1010 1111 0kkk dddd iiii iiii",
        Kind::Data(DataOp::Sub),
        ADR_WIDE,
        V6T2,
    ),
    wide(
        "1111 0H10 1010 nnnn 0kkk dddd iiii iiii",
        Kind::Data(DataOp::Sub),
        PLAIN,
        V6T2,
    ),
    wide(
        "1111 0H10 1100 eeee 0kkk dddd iiii iiii",
        Kind::Wide(WideOp::Movt),
        Shape::Wide,
        V6T2,
    ),
    wide(
        "1111 0-11 0010 nnnn 0000 dddd 00-- iiii",
        Kind::Saturate(SaturateOp::Ssat16),
        C,
        V6T2,
    ),
    wide(
        "1111 0-11 00y0 nnnn 0kkk dddd kk-i iiii",
        Kind::Saturate(SaturateOp::Ssat),
        C,
        V6T2,
    ),
    wide(
        "1111 0-11 0100 nnnn 0bbb dddd bb-e eeee",
        Kind::Bitfield(BitfieldOp::Sbfx),
        C,
        V6T2,
    ),
    wide(
        "1111 0-11 0110 1111 0bbb dddd bb-e eeee",
        Kind::Bitfield(BitfieldOp::Bfc),
        C,
        V6T2,
    ),
    wide(
        "1111 0-11 0110 nnnn 0bbb dddd bb-e eeee",
        Kind::Bitfield(BitfieldOp::Bfi),
        C,
        V6T2,
    ),
    wide(
        "1111 0-11 1010 nnnn 0000 dddd 00-- iiii",
        Kind::Saturate(SaturateOp::Usat16),
        C,
        V6T2,
    ),
    wide(
        "1111 0-11 10y0 nnnn 0kkk dddd kk-i iiii",
        Kind::Saturate(SaturateOp::Usat),
        C,
        V6T2,
    ),
    wide(
        "1111 0-11 1100 nnnn 0bbb dddd bb-e eeee",
        Kind::Bitfield(BitfieldOp::Ubfx),
        C,
        V6T2,
    ),
];

/// 32-bit branches and miscellaneous control. The miscellaneous rows come
/// first: they are a conditional branch's encoding with the condition `111x`.
static WIDE_BRANCH: &[Row] = &[
    wide(
        "1111 0011 100Z mmmm 10-0 ffff --0- ----",
        Kind::StatusWrite,
        C,
        V6T2,
    ),
    wide(
        "1111 0011 1010 ++++ 10-0 -000 0000 0000",
        Kind::Hint(HintOp::Nop),
        C,
        V6T2,
    ),
    wide(
        "1111 0011 1010 ++++ 10-0 -000 0000 0001",
        Kind::Hint(HintOp::Yield),
        C,
        V7,
    ),
    wide(
        "1111 0011 1010 ++++ 10-0 -000 0000 0010",
        Kind::Hint(HintOp::Wfe),
        C,
        V7,
    ),
    wide(
        "1111 0011 1010 ++++ 10-0 -000 0000 0011",
        Kind::Hint(HintOp::Wfi),
        C,
        V7,
    ),
    wide(
        "1111 0011 1010 ++++ 10-0 -000 0000 0100",
        Kind::Hint(HintOp::Sev),
        C,
        V7,
    ),
    wide(
        "1111 0011 1010 ++++ 10-0 -000 0000 0101",
        Kind::Hint(HintOp::Sevl),
        C,
        V8,
    ),
    wide(
        "1111 0011 1010 ++++ 10-0 -000 0001 0100",
        Kind::Hint(HintOp::Csdb),
        C,
        V8,
    )
    .outside(),
    wide(
        "1111 0011 1010 ++++ 10-0 -000 1111 iiii",
        Kind::Hint(HintOp::Dbg),
        C,
        V7,
    ),
    wide(
        "1111 0011 1010 ++++ 10-0 -000 iiii iiii",
        Kind::Hint(HintOp::Hint),
        C,
        V6T2,
    ),
    wide(
        "1111 0011 1010 ++++ 10-0 -IIu jjjg gggg",
        Kind::ChangeState,
        C,
        V6T2,
    )
    .outside(),
    wide(
        "1111 0011 1011 ++++ 10-0 ++++ 0010 ++++",
        Kind::ClearExclusive,
        C,
        V7,
    ),
    wide(
        "1111 0011 1011 ++++ 10-0 ++++ 0100 0000",
        Kind::Barrier(BarrierOp::Ssbb),
        C,
        V8,
    )
    .outside(),
    wide(
        "1111 0011 1011 ++++ 10-0 ++++ 0100 0100",
        Kind::Barrier(BarrierOp::Pssbb),
        C,
        V8,
    )
    .outside(),
    wide(
        "1111 0011 1011 ++++ 10-0 ++++ 0100 iiii",
        Kind::Barrier(BarrierOp::Dsb),
        C,
        V7,
    ),
    wide(
        "1111 0011 1011 ++++ 10-0 ++++ 0101 iiii",
        Kind::Barrier(BarrierOp::Dmb),
        C,
        V7,
    ),
    wide(
        "1111 0011 1011 ++++ 10-0 ++++ 0110 iiii",
        Kind::Barrier(BarrierOp::Isb),
        C,
        V7,
    ),
    wide(
        "1111 0011 1100 mmmm 10-0 ++++ ---- ----",
        Kind::Exchange(ExchangeOp::Bxj),
        C,
        V6T2,
    ),
    wide(
        "1111 0011 1101 +++- 10-0 ++++ iiii iiii",
        Kind::Data(DataOp::Sub),
        SUBS_PC_LR,
        V6T2,
    ),
    wide(
        "1111 0011 111Z ++++ 10-0 dddd --0- ----",
        Kind::Status,
        C,
        V6T2,
    ),
    wide(
        "1111 0111 1111 iiii 1010 iiii iiii iiii",
        Kind::Call(CallOp::Udf),
        C,
        V6T2,
    ),
    wide(
        "1111 0Hcc ccee eeee 10J0 Kiii iiii iiii",
        Kind::Branch(BranchOp::B),
        Shape::Branch(Branch::Cond20),
        V6T2,
    )
    .outside(),
    wide(
        "1111 0Hee eeee eeee 10J1 Kiii iiii iiii",
        Kind::Branch(BranchOp::B),
        Shape::Branch(Branch::Imm24),
        V6T2,
    ),
    wide(
        "1111 0Hee eeee eeee 11J0 Kiii iiii iii0",
        Kind::Branch(BranchOp::Blx),
        Shape::Branch(Branch::Blx),
        V6T2,
    ),
    wide(
        "1111 0Hee eeee eeee 11J1 Kiii iiii iiii",
        Kind::Branch(BranchOp::Bl),
        Shape::Branch(Branch::Imm24),
        V6T2,
    ),
];

/// 32-bit single loads, stores and memory hints. Each form addressing from
/// the PC comes before the forms whose Rn it fixes.
static WIDE_SINGLE: &[Row] = &[
    wide(
        "1111 1000 U0-1 1111 1111 iiii iiii iiii",
        Kind::Preload(PreloadOp::Pld),
        PRELOAD_LITERAL,
        V6T2,
    ),
    wide(
        "1111 1000 U001 1111 tttt iiii iiii iiii",
        Kind::Transfer(T::Ldrb),
        LITERAL,
        V6T2,
    ),
    wide(
        "1111 1000 U011 1111 tttt iiii iiii iiii",
        Kind::Transfer(T::Ldrh),
        LITERAL,
        V6T2,
    ),
    wide(
        "1111 1000 U101 1111 tttt iiii iiii iiii",
        Kind::Transfer(T::Ldr),
        LITERAL,
        V6T2,
    ),
    wide(
        "1111 1001 U001 1111 1111 iiii iiii iiii",
        Kind::Preload(PreloadOp::Pli),
        PRELOAD_LITERAL,
        V7,
    ),
    wide(
        "1111 1001 U001 1111 tttt iiii iiii iiii",
        Kind::Transfer(T::Ldrsb),
        LITERAL,
        V6T2,
    ),
    wide(
        "1111 1001 U011 1111 1111 iiii iiii iiii",
        Kind::Hint(HintOp::Nop),
        Shape::Nop,
        V6T2,
    ),
    wide(
        "1111 1001 U011 1111 tttt iiii iiii iiii",
        Kind::Transfer(T::Ldrsh),
        LITERAL,
        V6T2,
    ),
    wide(
        "1111 1000 1000 nnnn tttt iiii iiii iiii",
        Kind::Transfer(T::Strb),
        SINGLE,
        V6T2,
    ),
    wide(
        "1111 1000 1001 nnnn 1111 iiii iiii iiii",
        Kind::Preload(PreloadOp::Pld),
        PRELOAD_UP,
        V6T2,
    ),
    wide(
        "1111 1000 1001 nnnn tttt iiii iiii iiii",
        Kind::Transfer(T::Ldrb),
        SINGLE,
        V6T2,
    ),
    wide(
        "1111 1000 1010 nnnn tttt iiii iiii iiii",
        Kind::Transfer(T::Strh),
        SINGLE,
        V6T2,
    ),
    wide(
        "1111 1000 1011 nnnn 1111 iiii iiii iiii",
        Kind::Preload(PreloadOp::Pldw),
        PRELOAD_UP,
        Mp,
    ),
    wide(
        "1111 1000 1011 nnnn tttt iiii iiii iiii",
        Kind::Transfer(T::Ldrh),
        SINGLE,
        V6T2,
    ),
    wide(
        "1111 1000 1100 nnnn tttt iiii iiii iiii",
        Kind::Transfer(T::Str),
        SINGLE,
        V6T2,
    ),
    wide(
        "1111 1000 1101 nnnn tttt iiii iiii iiii",
        Kind::Transfer(T::Ldr),
        SINGLE,
        V6T2,
    ),
    wide(
        "1111 1001 1001 nnnn 1111 iiii iiii iiii",
        Kind::Preload(PreloadOp::Pli),
        PRELOAD_UP,
        V7,
    ),
    wide(
        "1111 1001 1001 nnnn tttt iiii iiii iiii",
        Kind::Transfer(T::Ldrsb),
        SINGLE,
        V6T2,
    ),
    wide(
        "1111 1001 1011 nnnn 1111 iiii iiii iiii",
        Kind::Hint(HintOp::Nop),
        Shape::Nop,
        V6T2,
    ),
    wide(
        "1111 1001 1011 nnnn tttt iiii iiii iiii",
        Kind::Transfer(T::Ldrsh),
        SINGLE,
        V6T2,
    ),
    wide(
        "1111 1000 0000 nnnn tttt 1110 iiii iiii",
        Kind::Transfer(T::Strbt),
        SINGLE,
        V6T2,
    ),
    wide(
        "1111 1000 0001 nnnn tttt 1110 iiii iiii",
        Kind::Transfer(T::Ldrbt),
        SINGLE,
        V6T2,
    ),
    wide(
        "1111 1000 0010 nnnn tttt 1110 iiii iiii",
        Kind::Transfer(T::Strht),
        SINGLE,
        V6T2,
    ),
    wide(
        "1111 1000 0011 nnnn tttt 1110 iiii iiii",
        Kind::Transfer(T::Ldrht),
        SINGLE,
        V6T2,
    ),
    wide(
        "1111 1000 0100 nnnn tttt 1110 iiii iiii",
        Kind::Transfer(T::Strt),
        SINGLE,
        V6T2,
    ),
    wide(
        "1111 1000 0101 nnnn tttt 1110 iiii iiii",
        Kind::Transfer(T::Ldrt),
        SINGLE,
        V6T2,
    ),
    wide(
        "1111 1001 0001 nnnn tttt 1110 iiii iiii",
        Kind::Transfer(T::Ldrsbt),
        SINGLE,
        V6T2,
    ),
    wide(
        "1111 1001 0011 nnnn tttt 1110 iiii iiii",
        Kind::Transfer(T::Ldrsht),
        SINGLE,
        V6T2,
    ),
    wide(
        "1111 1000 0001 nnnn 1111 1100 iiii iiii",
        Kind::Preload(PreloadOp::Pld),
        PRELOAD_DOWN,
        V6T2,
    ),
    wide(
        "1111 1000 0011 nnnn 1111 1100 iiii iiii",
        Kind::Preload(PreloadOp::Pldw),
        PRELOAD_DOWN,
        Mp,
    ),
    wide(
        "1111 1001 0001 nnnn 1111 1100 iiii iiii",
        Kind::Preload(PreloadOp::Pli),
        PRELOAD_DOWN,
        V7,
    ),
    wide(
        "1111 1001 0011 nnnn 1111 1100 iiii iiii",
        Kind::Hint(HintOp::Nop),
        Shape::Nop,
        V6T2,
    ),
    wide(
        "1111 1000 0000 nnnn tttt 1PUW iiii iiii",
        Kind::Transfer(T::Strb),
        SINGLE,
        V6T2,
    ),
    wide(
        "1111 1000 0001 nnnn tttt 1PUW iiii iiii",
        Kind::Transfer(T::Ldrb),
        SINGLE,
        V6T2,
    ),
    wide(
        "1111 1000 0010 nnnn tttt 1PUW iiii iiii",
        Kind::Transfer(T::Strh),
        SINGLE,
        V6T2,
    ),
    wide(
        "1111 1000 0011 nnnn tttt 1PUW iiii iiii",
        Kind::Transfer(T::Ldrh),
        SINGLE,
        V6T2,
    ),
    wide(
        "1111 1000 0100 nnnn tttt 1PUW iiii iiii",
        Kind::Transfer(T::Str),
        SINGLE,
        V6T2,
    ),
    wide(
        "1111 1000 0101 nnnn tttt 1PUW iiii iiii",
        Kind::Transfer(T::Ldr),
        SINGLE,
        V6T2,
    ),
    wide(
        "1111 1001 0001 nnnn tttt 1PUW iiii iiii",
        Kind::Transfer(T::Ldrsb),
        SINGLE,
        V6T2,
    ),
    wide(
        "1111 1001 0011 nnnn tttt 1PUW iiii iiii",
        Kind::Transfer(T::Ldrsh),
        SINGLE,
        V6T2,
    ),
    wide(
        "1111 1000 0001 nnnn 1111 0000 00kk mmmm",
        Kind::Preload(PreloadOp::Pld),
        PRELOAD_UP,
        V6T2,
    ),
    wide(
        "1111 1000 0011 nnnn 1111 0000 00kk mmmm",
        Kind::Preload(PreloadOp::Pldw),
        PRELOAD_UP,
        Mp,
    ),
    wide(
        "1111 1001 0001 nnnn 1111 0000 00kk mmmm",
        Kind::Preload(PreloadOp::Pli),
        PRELOAD_UP,
        V7,
    ),
    wide(
        "1111 1001 0011 nnnn 1111 0000 00kk mmmm",
        Kind::Hint(HintOp::Nop),
        Shape::Nop,
        V6T2,
    ),
    wide(
        "1111 1000 0000 nnnn tttt 0000 00kk mmmm",
        Kind::Transfer(T::Strb),
        SINGLE,
        V6T2,
    ),
    wide(
        "1111 1000 0001 nnnn tttt 0000 00kk mmmm",
        Kind::Transfer(T::Ldrb),
        SINGLE,
        V6T2,
    ),
    wide(
        "1111 1000 0010 nnnn tttt 0000 00kk mmmm",
        Kind::Transfer(T::Strh),
        SINGLE,
        V6T2,
    ),
    wide(
        "1111 1000 0011 nnnn tttt 0000 00kk mmmm",
        Kind::Transfer(T::Ldrh),
        SINGLE,
        V6T2,
    ),
    wide(
        "1111 1000 0100 nnnn tttt 0000 00kk mmmm",
        Kind::Transfer(T::Str),
        SINGLE,
        V6T2,
    ),
    wide(
        "1111 1000 0101 nnnn tttt 0000 00kk mmmm",
        Kind::Transfer(T::Ldr),
        SINGLE,
        V6T2,
    ),
    wide(
        "1111 1001 0001 nnnn tttt 0000 00kk mmmm",
        Kind::Transfer(T::Ldrsb),
        SINGLE,
        V6T2,
    ),
    wide(
        "1111 1001 0011 nnnn tttt 0000 00kk mmmm",
        Kind::Transfer(T::Ldrsh),
        SINGLE,
        V6T2,
    ),
];

/// 32-bit data processing by a register, and the multiplies and divides.
static WIDE_REGISTER: &[Row] = &[
    wide(
        "1111 1010 0yyS mmmm 1111 dddd 0000 ssss",
        Kind::Move(MoveOp::Mov),
        C,
        V6T2,
    ),
    wide(
        "1111 1010 0000 1111 1111 dddd 1-xx mmmm",
        Kind::Extend(E::Sxth),
        C,
        V6T2,
    ),
    wide(
        "1111 1010 0000 nnnn 1111 dddd 1-xx mmmm",
        Kind::Extend(E::Sxtah),
        C,
        V6T2,
    ),
    wide(
        "1111 1010 0001 1111 1111 dddd 1-xx mmmm",
        Kind::Extend(E::Uxth),
        C,
        V6T2,
    ),
    wide(
        "1111 1010 0001 nnnn 1111 dddd 1-xx mmmm",
        Kind::Extend(E::Uxtah),
        C,
        V6T2,
    ),
    wide(
        "1111 1010 0010 1111 1111 dddd 1-xx mmmm",
        Kind::Extend(E::Sxtb16),
        C,
        V6T2,
    ),
    wide(
        "1111 1010 0010 nnnn 1111 dddd 1-xx mmmm",
        Kind::Extend(E::Sxtab16),
        C,
        V6T2,
    ),
    wide(
        "1111 1010 0011 1111 1111 dddd 1-xx mmmm",
        Kind::Extend(E::Uxtb16),
        C,
        V6T2,
    ),
    wide(
        "1111 1010 0011 nnnn 1111 dddd 1-xx mmmm",
        Kind::Extend(E::Uxtab16),
        C,
        V6T2,
    ),
    wide(
        "1111 1010 0100 1111 1111 dddd 1-xx mmmm",
        Kind::Extend(E::Sxtb),
        C,
        V6T2,
    ),
    wide(
        "1111 1010 0100 nnnn 1111 dddd 1-xx mmmm",
        Kind::Extend(E::Sxtab),
        C,
        V6T2,
    ),
    wide(
        "1111 1010 0101 1111 1111 dddd 1-xx mmmm",
        Kind::Extend(E::Uxtb),
        C,
        V6T2,
    ),
    wide(
        "1111 1010 0101 nnnn 1111 dddd 1-xx mmmm",
        Kind::Extend(E::Uxtab),
        C,
        V6T2,
    ),
    wide(
        "1111 1010 1ooo nnnn 1111 dddd 0qqq mmmm",
        Kind::Parallel,
        Shape::Parallel,
        V6T2,
    ),
    wide(
        "1111 1010 1000 nnnn 1111 dddd 1000 mmmm",
        Kind::Arithmetic(A::Qadd),
        C,
        V6T2,
    ),
    wide(
        "1111 1010 1000 nnnn 1111 dddd 1001 mmmm",
        Kind::Arithmetic(A::Qdadd),
        C,
        V6T2,
    ),
    wide(
        "1111 1010 1000 nnnn 1111 dddd 1010 mmmm",
        Kind::Arithmetic(A::Qsub),
        C,
        V6T2,
    ),
    wide(
        "1111 1010 1000 nnnn 1111 dddd 1011 mmmm",
        Kind::Arithmetic(A::Qdsub),
        C,
        V6T2,
    ),
    wide(
        "1111 1010 1001 mmmm 1111 dddd 1000 mmmm",
        Kind::Unary(UnaryOp::Rev),
        Shape::Twice,
        V6T2,
    ),
    wide(
        "1111 1010 1001 mmmm 1111 dddd 1001 mmmm",
        Kind::Unary(UnaryOp::Rev16),
        Shape::Twice,
        V6T2,
    ),
    wide(
        "1111 1010 1001 mmmm 1111 dddd 1010 mmmm",
        Kind::Unary(UnaryOp::Rbit),
        Shape::Twice,
        V6T2,
    ),
    wide(
        "1111 1010 1001 mmmm 1111 dddd 1011 mmmm",
        Kind::Unary(UnaryOp::Revsh),
        Shape::Twice,
        V6T2,
    ),
    wide(
        "1111 1010 1010 nnnn 1111 dddd 1000 mmmm",
        Kind::Arithmetic(A::Sel),
        C,
        V6T2,
    ),
    wide(
        "1111 1010 1011 mmmm 1111 dddd 1000 mmmm",
        Kind::Unary(UnaryOp::Clz),
        Shape::Twice,
        V6T2,
    ),
    wide(
        "1111 1010 1100 nnnn 1111 dddd 1000 mmmm",
        Kind::Arithmetic(A::Crc32b),
        C,
        Crc32,
    )
    .outside(),
    wide(
        "1111 1010 1100 nnnn 1111 dddd 1001 mmmm",
        Kind::Arithmetic(A::Crc32h),
        C,
        Crc32,
    )
    .outside(),
    wide(
        "1111 1010 1100 nnnn 1111 dddd 1010 mmmm",
        Kind::Arithmetic(A::Crc32w),
        C,
        Crc32,
    )
    .outside(),
    wide(
        "1111 1010 1101 nnnn 1111 dddd 1000 mmmm",
        Kind::Arithmetic(A::Crc32cb),
        C,
        Crc32,
    )
    .outside(),
    wide(
        "1111 1010 1101 nnnn 1111 dddd 1001 mmmm",
        Kind::Arithmetic(A::Crc32ch),
        C,
        Crc32,
    )
    .outside(),
    wide(
        "1111 1010 1101 nnnn 1111 dddd 1010 mmmm",
        Kind::Arithmetic(A::Crc32cw),
        C,
        Crc32,
    )
    .outside(),
    wide(
        "1111 1011 0000 nnnn 1111 dddd 0000 mmmm",
        Kind::Multiply(M::Mul),
        C,
        V6T2,
    ),
    wide(
        "1111 1011 0000 nnnn aaaa dddd 0000 mmmm",
        Kind::Multiply(M::Mla),
        C,
        V6T2,
    ),
    wide(
        "1111 1011 0000 nnnn aaaa dddd 0001 mmmm",
        Kind::Multiply(M::Mls),
        C,
        V6T2,
    ),
    wide(
        "1111 1011 0001 nnnn 1111 dddd 00XY mmmm",
        Kind::Multiply(M::Smul),
        C,
        V6T2,
    ),
    wide(
        "1111 1011 0001 nnnn aaaa dddd 00XY mmmm",
        Kind::Multiply(M::Smla),
        C,
        V6T2,
    ),
    wide(
        "1111 1011 0010 nnnn 1111 dddd 000X mmmm",
        Kind::Multiply(M::Smuad),
        C,
        V6T2,
    ),
    wide(
        "1111 1011 0010 nnnn aaaa dddd 000X mmmm",
        Kind::Multiply(M::Smlad),
        C,
        V6T2,
    ),
    wide(
        "1111 1011 0011 nnnn 1111 dddd 000Y mmmm",
        Kind::Multiply(M::Smulw),
        C,
        V6T2,
    ),
    wide(
        "1111 1011 0011 nnnn aaaa dddd 000Y mmmm",
        Kind::Multiply(M::Smlaw),
        C,
        V6T2,
    ),
    wide(
        "1111 1011 0100 nnnn 1111 dddd 000X mmmm",
        Kind::Multiply(M::Smusd),
        C,
        V6T2,
    ),
    wide(
        "1111 1011 0100 nnnn aaaa dddd 000X mmmm",
        Kind::Multiply(M::Smlsd),
        C,
        V6T2,
    ),
    wide(
        "1111 1011 0101 nnnn 1111 dddd 000X mmmm",
        Kind::Multiply(M::Smmul),
        C,
        V6T2,
    ),
    wide(
        "1111 1011 0101 nnnn aaaa dddd 000X mmmm",
        Kind::Multiply(M::Smmla),
        C,
        V6T2,
    ),
    wide(
        "1111 1011 0110 nnnn aaaa dddd 000X mmmm",
        Kind::Multiply(M::Smmls),
        C,
        V6T2,
    ),
    wide(
        "1111 1011 0111 nnnn 1111 dddd 0000 mmmm",
        Kind::Multiply(M::Usad8),
        C,
        V6T2,
    ),
    wide(
        "1111 1011 0111 nnnn aaaa dddd 0000 mmmm",
        Kind::Multiply(M::Usada8),
        C,
        V6T2,
    ),
    wide(
        "1111 1011 1000 nnnn llll hhhh 0000 mmmm",
        Kind::MultiplyLong(LongOp::Smull),
        C,
        V6T2,
    ),
    wide(
        "1111 1011 1001 nnnn ++++ dddd 1111 mmmm",
        Kind::Multiply(M::Sdiv),
        C,
        Divide,
    ),
    wide(
        "1111 1011 1010 nnnn llll hhhh 0000 mmmm",
        Kind::MultiplyLong(LongOp::Umull),
        C,
        V6T2,
    ),
    wide(
        "1111 1011 1011 nnnn ++++ dddd 1111 mmmm",
        Kind::Multiply(M::Udiv),
        C,
        Divide,
    ),
    wide(
        "1111 1011 1100 nnnn llll hhhh 0000 mmmm",
        Kind::MultiplyLong(LongOp::Smlal),
        C,
        V6T2,
    ),
    wide(
        "1111 1011 1100 nnnn llll hhhh 10XY mmmm",
        Kind::MultiplyLong(LongOp::SmlalHalves),
        C,
        V6T2,
    ),
    wide(
        "1111 1011 1100 nnnn llll hhhh 110X mmmm",
        Kind::MultiplyLong(LongOp::Smlald),
        C,
        V6T2,
    ),
    wide(
        "1111 1011 1101 nnnn llll hhhh 110X mmmm",
        Kind::MultiplyLong(LongOp::Smlsld),
        C,
        V6T2,
    ),
    wide(
        "1111 1011 1110 nnnn llll hhhh 0000 mmmm",
        Kind::MultiplyLong(LongOp::Umlal),
        C,
        V6T2,
    ),
    wide(
        "1111 1011 1110 nnnn llll hhhh 0110 mmmm",
        Kind::MultiplyLong(LongOp::Umaal),
        C,
        V6T2,
    ),
];

/// The 16-bit rows, by the top three bits.
pub(super) static NARROW: [&[Row]; 8] = [
    NARROW_000, NARROW_001, NARROW_010, NARROW_011, NARROW_100, NARROW_101, NARROW_110, NARROW_111,
];

/// The rows of the coprocessor spaces, which A32's rows decode.
static WIDE_COPROCESSOR: &[Row] = &[];

/// The 32-bit rows, by [`wide_group`].
pub(super) static WIDE: [&[Row]; 8] = [
    WIDE_MULTIPLE,
    WIDE_SHIFTED,
    WIDE_COPROCESSOR,
    WIDE_IMMEDIATE,
    WIDE_BRANCH,
    WIDE_SINGLE,
    WIDE_REGISTER,
    WIDE_COPROCESSOR,
];

/// The group of a 32-bit encoding, from its first halfword's top seven bits
/// and, under `11110`, the bit dividing immediates from branches.
pub(super) const fn wide_group(word: u32) -> usize {
    let first = word >> 16;
    match (first >> 11, (first >> 9) & 3) {
        (0b11101, 0) => 0,
        (0b11101, 1) => 1,
        (0b11101, _) => 2,
        (0b11110, _) if word & 0x8000 == 0 => 3,
        (0b11110, _) => 4,
        (_, 0) => 5,
        (_, 1) => 6,
        _ => 7,
    }
}

/// The 16-bit rows `first` may match, most specific first.
pub(super) fn narrow_rows(first: u16) -> &'static [Row] {
    NARROW[usize::from(first >> 13)]
}

/// The 32-bit rows `word` may match, most specific first.
pub(super) fn wide_rows(word: u32) -> &'static [Row] {
    WIDE[wide_group(word)]
}

/// Every row, the 16-bit rows first: the encoder's order, which prefers the
/// narrow encoding.
pub(super) fn all() -> impl Iterator<Item = &'static Row> {
    NARROW.iter().chain(&WIDE).flat_map(|group| group.iter())
}
