//! The A32 encoding table: every encoding from ARMv2 to ARMv7-A with
//! ARMv8-A's AArch32 integer additions, as the ARMv7-A architecture manual
//! lays them out, and the FPA's on coprocessors 1 and 2 as FPEmulator claims
//! them. The VFP and Advanced SIMD encodings are in the coprocessor and
//! unconditional spaces this table leaves to them.
//!
//! The rows are grouped by bits 27 to 25, which every encoding fixes. Within
//! a group a row that refines a later one comes first; a test holds that no
//! row is shadowed by an earlier, less specific one.

use super::simdfp;
use crate::encoding::form::Kind;
use crate::encoding::layout::Layout;
use crate::instruction::{
    ArithmeticOp as A, BarrierOp, BitfieldOp, BlockOp, BranchOp, CallOp, CoRegisterOp,
    CoRegistersOp, CoTransferOp, CompareOp, CoprocessorOp, DataOp, DualOp, ExchangeOp,
    ExclusiveOp as X, ExtendOp as E, FpaCompareOp, FpaDyadicOp, FpaMonadicOp, FpaMultipleOp,
    FpaRegisterOp, FpaTransferOp, HintOp, LongOp, MoveOp, MultiplyOp as M, PackOp, PreloadOp,
    SaturateOp, SwapOp, TransferOp as T, UnaryOp, WideOp,
};
use crate::isa::Feature::{
    self, Crc32, Divide, Fpa, Jazelle, LongMultiply, Mp, Swp, V2, V3, V4, V4T, V5, V5TE, V6, V6K,
    V6T2, V7, V8,
};

/// One encoding: what it decodes to, the feature it needs and its layout.
#[derive(Debug)]
pub(crate) struct Row {
    pub kind: Kind,
    pub since: Feature,
    pub layout: Layout,
}

impl Row {
    /// Whether the row is in the unconditional space: its condition field is
    /// fixed at `1111`.
    pub const fn unconditional(&self) -> bool {
        self.layout.mask >> 28 == 0xF && self.layout.bits >> 28 == 0xF
    }
}

pub(super) const fn row(pattern: &str, kind: Kind, since: Feature) -> Row {
    Row {
        kind,
        since,
        layout: Layout::of(pattern),
    }
}

/// A data-processing row by an immediate.
macro_rules! immediate {
    ($kind:expr, $op:literal, $s:literal, $rn:literal) => {
        row(
            concat!("cccc 001", $op, $s, $rn, "dddd rrrr iiii iiii"),
            $kind,
            V2,
        )
    };
}

/// A data-processing row by a register shifted by an immediate.
macro_rules! shifted {
    ($kind:expr, $op:literal, $s:literal, $rn:literal) => {
        row(
            concat!("cccc 000", $op, $s, $rn, "dddd kkkk kyy0 mmmm"),
            $kind,
            V2,
        )
    };
}

/// A data-processing row by a register shifted by a register.
macro_rules! by_register {
    ($kind:expr, $op:literal, $s:literal, $rn:literal) => {
        row(
            concat!("cccc 000", $op, $s, $rn, "dddd ssss 0yy1 mmmm"),
            $kind,
            V2,
        )
    };
}

/// An FPA dyadic operation's row, by its opcode.
macro_rules! fpa_dyadic {
    ($op:ident, $opcode:literal) => {
        row(
            concat!("cccc 1110 ", $opcode, " vNNN 0DDD 0001 vww0 MMMM"),
            Kind::FpaDyadic(FpaDyadicOp::$op),
            Fpa,
        )
    };
}

/// An FPA monadic operation's row, by its opcode.
macro_rules! fpa_monadic {
    ($op:ident, $opcode:literal) => {
        row(
            concat!("cccc 1110 ", $opcode, " v*** 1DDD 0001 vww0 MMMM"),
            Kind::FpaMonadic(FpaMonadicOp::$op),
            Fpa,
        )
    };
}

/// An FPA comparison's row, by its opcode.
macro_rules! fpa_compare {
    ($op:ident, $opcode:literal) => {
        row(
            concat!("cccc 1110 ", $opcode, "1 vNNN tttt 0001 v**1 MMMM"),
            Kind::FpaCompare(FpaCompareOp::$op),
            Fpa,
        )
    };
}

/// Rows whose bits 27 to 25 are `000`.
static GROUP_000: &[Row] = &[
    // Data processing, which no other row in the group overlaps, first as
    // the most common.
    shifted!(Kind::Data(DataOp::And), "0000", "S", "nnnn"),
    shifted!(Kind::Data(DataOp::Eor), "0001", "S", "nnnn"),
    shifted!(Kind::Data(DataOp::Sub), "0010", "S", "nnnn"),
    shifted!(Kind::Data(DataOp::Rsb), "0011", "S", "nnnn"),
    shifted!(Kind::Data(DataOp::Add), "0100", "S", "nnnn"),
    shifted!(Kind::Data(DataOp::Adc), "0101", "S", "nnnn"),
    shifted!(Kind::Data(DataOp::Sbc), "0110", "S", "nnnn"),
    shifted!(Kind::Data(DataOp::Rsc), "0111", "S", "nnnn"),
    shifted!(Kind::Compare(CompareOp::Tst), "1000", "1", "nnnn"),
    shifted!(Kind::Compare(CompareOp::Teq), "1001", "1", "nnnn"),
    shifted!(Kind::Compare(CompareOp::Cmp), "1010", "1", "nnnn"),
    shifted!(Kind::Compare(CompareOp::Cmn), "1011", "1", "nnnn"),
    shifted!(Kind::Data(DataOp::Orr), "1100", "S", "nnnn"),
    shifted!(Kind::Move(MoveOp::Mov), "1101", "S", "----"),
    shifted!(Kind::Data(DataOp::Bic), "1110", "S", "nnnn"),
    shifted!(Kind::Move(MoveOp::Mvn), "1111", "S", "----"),
    by_register!(Kind::Data(DataOp::And), "0000", "S", "nnnn"),
    by_register!(Kind::Data(DataOp::Eor), "0001", "S", "nnnn"),
    by_register!(Kind::Data(DataOp::Sub), "0010", "S", "nnnn"),
    by_register!(Kind::Data(DataOp::Rsb), "0011", "S", "nnnn"),
    by_register!(Kind::Data(DataOp::Add), "0100", "S", "nnnn"),
    by_register!(Kind::Data(DataOp::Adc), "0101", "S", "nnnn"),
    by_register!(Kind::Data(DataOp::Sbc), "0110", "S", "nnnn"),
    by_register!(Kind::Data(DataOp::Rsc), "0111", "S", "nnnn"),
    by_register!(Kind::Compare(CompareOp::Tst), "1000", "1", "nnnn"),
    by_register!(Kind::Compare(CompareOp::Teq), "1001", "1", "nnnn"),
    by_register!(Kind::Compare(CompareOp::Cmp), "1010", "1", "nnnn"),
    by_register!(Kind::Compare(CompareOp::Cmn), "1011", "1", "nnnn"),
    by_register!(Kind::Data(DataOp::Orr), "1100", "S", "nnnn"),
    by_register!(Kind::Move(MoveOp::Mov), "1101", "S", "----"),
    by_register!(Kind::Data(DataOp::Bic), "1110", "S", "nnnn"),
    by_register!(Kind::Move(MoveOp::Mvn), "1111", "S", "----"),
    // Multiplies.
    row(
        "cccc 0000 000S dddd ---- mmmm 1001 nnnn",
        Kind::Multiply(M::Mul),
        V2,
    ),
    row(
        "cccc 0000 001S dddd aaaa mmmm 1001 nnnn",
        Kind::Multiply(M::Mla),
        V2,
    ),
    row(
        "cccc 0000 0100 hhhh llll mmmm 1001 nnnn",
        Kind::MultiplyLong(LongOp::Umaal),
        V6,
    ),
    row(
        "cccc 0000 0110 dddd aaaa mmmm 1001 nnnn",
        Kind::Multiply(M::Mls),
        V6T2,
    ),
    row(
        "cccc 0000 100S hhhh llll mmmm 1001 nnnn",
        Kind::MultiplyLong(LongOp::Umull),
        LongMultiply,
    ),
    row(
        "cccc 0000 101S hhhh llll mmmm 1001 nnnn",
        Kind::MultiplyLong(LongOp::Umlal),
        LongMultiply,
    ),
    row(
        "cccc 0000 110S hhhh llll mmmm 1001 nnnn",
        Kind::MultiplyLong(LongOp::Smull),
        LongMultiply,
    ),
    row(
        "cccc 0000 111S hhhh llll mmmm 1001 nnnn",
        Kind::MultiplyLong(LongOp::Smlal),
        LongMultiply,
    ),
    // Halfword multiplies.
    row(
        "cccc 0001 0000 dddd aaaa mmmm 1YX0 nnnn",
        Kind::Multiply(M::Smla),
        V5TE,
    ),
    row(
        "cccc 0001 0010 dddd aaaa mmmm 1Y00 nnnn",
        Kind::Multiply(M::Smlaw),
        V5TE,
    ),
    row(
        "cccc 0001 0010 dddd ---- mmmm 1Y10 nnnn",
        Kind::Multiply(M::Smulw),
        V5TE,
    ),
    row(
        "cccc 0001 0100 hhhh llll mmmm 1YX0 nnnn",
        Kind::MultiplyLong(LongOp::SmlalHalves),
        V5TE,
    ),
    row(
        "cccc 0001 0110 dddd ---- mmmm 1YX0 nnnn",
        Kind::Multiply(M::Smul),
        V5TE,
    ),
    // Saturating arithmetic.
    row(
        "cccc 0001 0000 nnnn dddd ---- 0101 mmmm",
        Kind::Arithmetic(A::Qadd),
        V5TE,
    ),
    row(
        "cccc 0001 0010 nnnn dddd ---- 0101 mmmm",
        Kind::Arithmetic(A::Qsub),
        V5TE,
    ),
    row(
        "cccc 0001 0100 nnnn dddd ---- 0101 mmmm",
        Kind::Arithmetic(A::Qdadd),
        V5TE,
    ),
    row(
        "cccc 0001 0110 nnnn dddd ---- 0101 mmmm",
        Kind::Arithmetic(A::Qdsub),
        V5TE,
    ),
    // Checksums.
    row(
        "cccc 0001 0000 nnnn dddd --0- 0100 mmmm",
        Kind::Arithmetic(A::Crc32b),
        Crc32,
    ),
    row(
        "cccc 0001 0010 nnnn dddd --0- 0100 mmmm",
        Kind::Arithmetic(A::Crc32h),
        Crc32,
    ),
    row(
        "cccc 0001 0100 nnnn dddd --0- 0100 mmmm",
        Kind::Arithmetic(A::Crc32w),
        Crc32,
    ),
    row(
        "cccc 0001 0000 nnnn dddd --1- 0100 mmmm",
        Kind::Arithmetic(A::Crc32cb),
        Crc32,
    ),
    row(
        "cccc 0001 0010 nnnn dddd --1- 0100 mmmm",
        Kind::Arithmetic(A::Crc32ch),
        Crc32,
    ),
    row(
        "cccc 0001 0100 nnnn dddd --1- 0100 mmmm",
        Kind::Arithmetic(A::Crc32cw),
        Crc32,
    ),
    // Status registers, branches and exchanges, CLZ and BKPT.
    row("cccc 0001 0Z00 ++++ dddd ---- 0000 ----", Kind::Status, V3),
    row(
        "cccc 0001 0Z10 ffff ++++ ---- 0000 mmmm",
        Kind::StatusWrite,
        V3,
    ),
    row(
        "cccc 0001 0010 ++++ ++++ ++++ 0001 mmmm",
        Kind::Exchange(ExchangeOp::Bx),
        V4T,
    ),
    row(
        "cccc 0001 0010 ++++ ++++ ++++ 0010 mmmm",
        Kind::Exchange(ExchangeOp::Bxj),
        Jazelle,
    ),
    row(
        "cccc 0001 0010 ++++ ++++ ++++ 0011 mmmm",
        Kind::Exchange(ExchangeOp::Blx),
        V5,
    ),
    row(
        "cccc 0001 0110 ++++ dddd ++++ 0001 mmmm",
        Kind::Unary(UnaryOp::Clz),
        V5,
    ),
    row(
        "1110 0001 0010 iiii iiii iiii 0111 iiii",
        Kind::Call(CallOp::Bkpt),
        V5,
    ),
    // Swaps, exclusives, load-acquires and store-releases.
    row(
        "cccc 0001 0000 nnnn tttt ---- 1001 TTTT",
        Kind::Swap(SwapOp::Swp),
        Swp,
    ),
    row(
        "cccc 0001 0100 nnnn tttt ---- 1001 TTTT",
        Kind::Swap(SwapOp::Swpb),
        Swp,
    ),
    row(
        "cccc 0001 1000 nnnn ++++ ++00 1001 tttt",
        Kind::Exclusive(X::Stl),
        V8,
    ),
    row(
        "cccc 0001 1001 nnnn tttt ++00 1001 ++++",
        Kind::Exclusive(X::Lda),
        V8,
    ),
    row(
        "cccc 0001 1100 nnnn ++++ ++00 1001 tttt",
        Kind::Exclusive(X::Stlb),
        V8,
    ),
    row(
        "cccc 0001 1101 nnnn tttt ++00 1001 ++++",
        Kind::Exclusive(X::Ldab),
        V8,
    ),
    row(
        "cccc 0001 1110 nnnn ++++ ++00 1001 tttt",
        Kind::Exclusive(X::Stlh),
        V8,
    ),
    row(
        "cccc 0001 1111 nnnn tttt ++00 1001 ++++",
        Kind::Exclusive(X::Ldah),
        V8,
    ),
    row(
        "cccc 0001 1000 nnnn dddd ++10 1001 tttt",
        Kind::Exclusive(X::Stlex),
        V8,
    ),
    row(
        "cccc 0001 1001 nnnn tttt ++10 1001 ++++",
        Kind::Exclusive(X::Ldaex),
        V8,
    ),
    row(
        "cccc 0001 1010 nnnn dddd ++10 1001 tttt",
        Kind::Exclusive(X::Stlexd),
        V8,
    ),
    row(
        "cccc 0001 1011 nnnn tttt ++10 1001 ++++",
        Kind::Exclusive(X::Ldaexd),
        V8,
    ),
    row(
        "cccc 0001 1100 nnnn dddd ++10 1001 tttt",
        Kind::Exclusive(X::Stlexb),
        V8,
    ),
    row(
        "cccc 0001 1101 nnnn tttt ++10 1001 ++++",
        Kind::Exclusive(X::Ldaexb),
        V8,
    ),
    row(
        "cccc 0001 1110 nnnn dddd ++10 1001 tttt",
        Kind::Exclusive(X::Stlexh),
        V8,
    ),
    row(
        "cccc 0001 1111 nnnn tttt ++10 1001 ++++",
        Kind::Exclusive(X::Ldaexh),
        V8,
    ),
    row(
        "cccc 0001 1000 nnnn dddd ++11 1001 tttt",
        Kind::Exclusive(X::Strex),
        V6,
    ),
    row(
        "cccc 0001 1001 nnnn tttt ++11 1001 ++++",
        Kind::Exclusive(X::Ldrex),
        V6,
    ),
    row(
        "cccc 0001 1010 nnnn dddd ++11 1001 tttt",
        Kind::Exclusive(X::Strexd),
        V6K,
    ),
    row(
        "cccc 0001 1011 nnnn tttt ++11 1001 ++++",
        Kind::Exclusive(X::Ldrexd),
        V6K,
    ),
    row(
        "cccc 0001 1100 nnnn dddd ++11 1001 tttt",
        Kind::Exclusive(X::Strexb),
        V6K,
    ),
    row(
        "cccc 0001 1101 nnnn tttt ++11 1001 ++++",
        Kind::Exclusive(X::Ldrexb),
        V6K,
    ),
    row(
        "cccc 0001 1110 nnnn dddd ++11 1001 tttt",
        Kind::Exclusive(X::Strexh),
        V6K,
    ),
    row(
        "cccc 0001 1111 nnnn tttt ++11 1001 ++++",
        Kind::Exclusive(X::Ldrexh),
        V6K,
    ),
    // The unprivileged halfword and signed transfers, before the forms
    // whose P and W they fix.
    row(
        "cccc 0000 U110 nnnn tttt iiii 1011 iiii",
        Kind::Transfer(T::Strht),
        V6T2,
    ),
    row(
        "cccc 0000 U010 nnnn tttt ---- 1011 mmmm",
        Kind::Transfer(T::Strht),
        V6T2,
    ),
    row(
        "cccc 0000 U111 nnnn tttt iiii 1011 iiii",
        Kind::Transfer(T::Ldrht),
        V6T2,
    ),
    row(
        "cccc 0000 U011 nnnn tttt ---- 1011 mmmm",
        Kind::Transfer(T::Ldrht),
        V6T2,
    ),
    row(
        "cccc 0000 U111 nnnn tttt iiii 1101 iiii",
        Kind::Transfer(T::Ldrsbt),
        V6T2,
    ),
    row(
        "cccc 0000 U011 nnnn tttt ---- 1101 mmmm",
        Kind::Transfer(T::Ldrsbt),
        V6T2,
    ),
    row(
        "cccc 0000 U111 nnnn tttt iiii 1111 iiii",
        Kind::Transfer(T::Ldrsht),
        V6T2,
    ),
    row(
        "cccc 0000 U011 nnnn tttt ---- 1111 mmmm",
        Kind::Transfer(T::Ldrsht),
        V6T2,
    ),
    // Halfword, signed and doubleword transfers.
    row(
        "cccc 000P U1W0 nnnn tttt iiii 1011 iiii",
        Kind::Transfer(T::Strh),
        V4,
    ),
    row(
        "cccc 000P U0W0 nnnn tttt ---- 1011 mmmm",
        Kind::Transfer(T::Strh),
        V4,
    ),
    row(
        "cccc 000P U1W1 nnnn tttt iiii 1011 iiii",
        Kind::Transfer(T::Ldrh),
        V4,
    ),
    row(
        "cccc 000P U0W1 nnnn tttt ---- 1011 mmmm",
        Kind::Transfer(T::Ldrh),
        V4,
    ),
    row(
        "cccc 000P U1W0 nnnn tttt iiii 1101 iiii",
        Kind::Dual(DualOp::Ldrd),
        V5TE,
    ),
    row(
        "cccc 000P U0W0 nnnn tttt ---- 1101 mmmm",
        Kind::Dual(DualOp::Ldrd),
        V5TE,
    ),
    row(
        "cccc 000P U1W1 nnnn tttt iiii 1101 iiii",
        Kind::Transfer(T::Ldrsb),
        V4,
    ),
    row(
        "cccc 000P U0W1 nnnn tttt ---- 1101 mmmm",
        Kind::Transfer(T::Ldrsb),
        V4,
    ),
    row(
        "cccc 000P U1W0 nnnn tttt iiii 1111 iiii",
        Kind::Dual(DualOp::Strd),
        V5TE,
    ),
    row(
        "cccc 000P U0W0 nnnn tttt ---- 1111 mmmm",
        Kind::Dual(DualOp::Strd),
        V5TE,
    ),
    row(
        "cccc 000P U1W1 nnnn tttt iiii 1111 iiii",
        Kind::Transfer(T::Ldrsh),
        V4,
    ),
    row(
        "cccc 000P U0W1 nnnn tttt ---- 1111 mmmm",
        Kind::Transfer(T::Ldrsh),
        V4,
    ),
    // The unconditional state changes.
    row(
        "1111 0001 0000 IIu0 ---- ---j jj0g gggg",
        Kind::ChangeState,
        V6,
    ),
    row(
        "1111 0001 0000 0001 ---- --E- 0000 ----",
        Kind::SetEndian,
        V6,
    ),
];

/// Rows whose bits 27 to 25 are `001`.
static GROUP_001: &[Row] = &[
    immediate!(Kind::Data(DataOp::And), "0000", "S", "nnnn"),
    immediate!(Kind::Data(DataOp::Eor), "0001", "S", "nnnn"),
    immediate!(Kind::Data(DataOp::Sub), "0010", "S", "nnnn"),
    immediate!(Kind::Data(DataOp::Rsb), "0011", "S", "nnnn"),
    immediate!(Kind::Data(DataOp::Add), "0100", "S", "nnnn"),
    immediate!(Kind::Data(DataOp::Adc), "0101", "S", "nnnn"),
    immediate!(Kind::Data(DataOp::Sbc), "0110", "S", "nnnn"),
    immediate!(Kind::Data(DataOp::Rsc), "0111", "S", "nnnn"),
    immediate!(Kind::Compare(CompareOp::Tst), "1000", "1", "nnnn"),
    immediate!(Kind::Compare(CompareOp::Teq), "1001", "1", "nnnn"),
    immediate!(Kind::Compare(CompareOp::Cmp), "1010", "1", "nnnn"),
    immediate!(Kind::Compare(CompareOp::Cmn), "1011", "1", "nnnn"),
    immediate!(Kind::Data(DataOp::Orr), "1100", "S", "nnnn"),
    immediate!(Kind::Move(MoveOp::Mov), "1101", "S", "----"),
    immediate!(Kind::Data(DataOp::Bic), "1110", "S", "nnnn"),
    immediate!(Kind::Move(MoveOp::Mvn), "1111", "S", "----"),
    row(
        "cccc 0011 0000 iiii dddd iiii iiii iiii",
        Kind::Wide(WideOp::Movw),
        V6T2,
    ),
    row(
        "cccc 0011 0100 iiii dddd iiii iiii iiii",
        Kind::Wide(WideOp::Movt),
        V6T2,
    ),
    row(
        "cccc 0011 0010 0000 ++++ ---- 0000 0000",
        Kind::Hint(HintOp::Nop),
        V6K,
    ),
    row(
        "cccc 0011 0010 0000 ++++ ---- 0000 0001",
        Kind::Hint(HintOp::Yield),
        V6K,
    ),
    row(
        "cccc 0011 0010 0000 ++++ ---- 0000 0010",
        Kind::Hint(HintOp::Wfe),
        V6K,
    ),
    row(
        "cccc 0011 0010 0000 ++++ ---- 0000 0011",
        Kind::Hint(HintOp::Wfi),
        V6K,
    ),
    row(
        "cccc 0011 0010 0000 ++++ ---- 0000 0100",
        Kind::Hint(HintOp::Sev),
        V6K,
    ),
    row(
        "cccc 0011 0010 0000 ++++ ---- 0000 0101",
        Kind::Hint(HintOp::Sevl),
        V8,
    ),
    row(
        "cccc 0011 0010 0000 ++++ ---- 0001 0100",
        Kind::Hint(HintOp::Csdb),
        V8,
    ),
    row(
        "cccc 0011 0010 0000 ++++ ---- 1111 iiii",
        Kind::Hint(HintOp::Dbg),
        V7,
    ),
    row(
        "cccc 0011 0010 0000 ++++ ---- iiii iiii",
        Kind::Hint(HintOp::Hint),
        V6K,
    ),
    row(
        "cccc 0011 0Z10 ffff ++++ rrrr iiii iiii",
        Kind::StatusWrite,
        V3,
    ),
];

/// Rows whose bits 27 to 25 are `010`.
static GROUP_010: &[Row] = &[
    row(
        "cccc 0100 U010 nnnn tttt iiii iiii iiii",
        Kind::Transfer(T::Strt),
        V2,
    ),
    row(
        "cccc 0100 U011 nnnn tttt iiii iiii iiii",
        Kind::Transfer(T::Ldrt),
        V2,
    ),
    row(
        "cccc 0100 U110 nnnn tttt iiii iiii iiii",
        Kind::Transfer(T::Strbt),
        V2,
    ),
    row(
        "cccc 0100 U111 nnnn tttt iiii iiii iiii",
        Kind::Transfer(T::Ldrbt),
        V2,
    ),
    row(
        "cccc 010P U0W0 nnnn tttt iiii iiii iiii",
        Kind::Transfer(T::Str),
        V2,
    ),
    row(
        "cccc 010P U0W1 nnnn tttt iiii iiii iiii",
        Kind::Transfer(T::Ldr),
        V2,
    ),
    row(
        "cccc 010P U1W0 nnnn tttt iiii iiii iiii",
        Kind::Transfer(T::Strb),
        V2,
    ),
    row(
        "cccc 010P U1W1 nnnn tttt iiii iiii iiii",
        Kind::Transfer(T::Ldrb),
        V2,
    ),
    row(
        "1111 0100 U101 nnnn ++++ iiii iiii iiii",
        Kind::Preload(PreloadOp::Pli),
        V7,
    ),
    row(
        "1111 0101 U101 nnnn ++++ iiii iiii iiii",
        Kind::Preload(PreloadOp::Pld),
        V5TE,
    ),
    row(
        "1111 0101 U001 nnnn ++++ iiii iiii iiii",
        Kind::Preload(PreloadOp::Pldw),
        Mp,
    ),
    row(
        "1111 0101 0111 ++++ ++++ ---- 0001 ++++",
        Kind::ClearExclusive,
        V6K,
    ),
    row(
        "1111 0101 0111 ++++ ++++ ---- 0100 0000",
        Kind::Barrier(BarrierOp::Ssbb),
        V8,
    ),
    row(
        "1111 0101 0111 ++++ ++++ ---- 0100 0100",
        Kind::Barrier(BarrierOp::Pssbb),
        V8,
    ),
    row(
        "1111 0101 0111 ++++ ++++ ---- 0100 iiii",
        Kind::Barrier(BarrierOp::Dsb),
        V7,
    ),
    row(
        "1111 0101 0111 ++++ ++++ ---- 0101 iiii",
        Kind::Barrier(BarrierOp::Dmb),
        V7,
    ),
    row(
        "1111 0101 0111 ++++ ++++ ---- 0110 iiii",
        Kind::Barrier(BarrierOp::Isb),
        V7,
    ),
];

/// Rows whose bits 27 to 25 are `011`.
static GROUP_011: &[Row] = &[
    // Word and byte transfers by a register.
    row(
        "cccc 0110 U010 nnnn tttt kkkk kyy0 mmmm",
        Kind::Transfer(T::Strt),
        V2,
    ),
    row(
        "cccc 0110 U011 nnnn tttt kkkk kyy0 mmmm",
        Kind::Transfer(T::Ldrt),
        V2,
    ),
    row(
        "cccc 0110 U110 nnnn tttt kkkk kyy0 mmmm",
        Kind::Transfer(T::Strbt),
        V2,
    ),
    row(
        "cccc 0110 U111 nnnn tttt kkkk kyy0 mmmm",
        Kind::Transfer(T::Ldrbt),
        V2,
    ),
    row(
        "cccc 011P U0W0 nnnn tttt kkkk kyy0 mmmm",
        Kind::Transfer(T::Str),
        V2,
    ),
    row(
        "cccc 011P U0W1 nnnn tttt kkkk kyy0 mmmm",
        Kind::Transfer(T::Ldr),
        V2,
    ),
    row(
        "cccc 011P U1W0 nnnn tttt kkkk kyy0 mmmm",
        Kind::Transfer(T::Strb),
        V2,
    ),
    row(
        "cccc 011P U1W1 nnnn tttt kkkk kyy0 mmmm",
        Kind::Transfer(T::Ldrb),
        V2,
    ),
    row(
        "1111 0110 U101 nnnn ++++ kkkk kyy0 mmmm",
        Kind::Preload(PreloadOp::Pli),
        V7,
    ),
    row(
        "1111 0111 U101 nnnn ++++ kkkk kyy0 mmmm",
        Kind::Preload(PreloadOp::Pld),
        V5TE,
    ),
    row(
        "1111 0111 U001 nnnn ++++ kkkk kyy0 mmmm",
        Kind::Preload(PreloadOp::Pldw),
        Mp,
    ),
    // Parallel additions and subtractions: the prefix in bits 22 to 20,
    // the lanes in bits 7 to 5.
    row(
        "cccc 0110 0ooo nnnn dddd ++++ qqq1 mmmm",
        Kind::Parallel,
        V6,
    ),
    // Packing, extension, selection, saturation and reversal.
    row(
        "cccc 0110 1000 nnnn dddd kkkk k001 mmmm",
        Kind::Pack(PackOp::Pkhbt),
        V6,
    ),
    row(
        "cccc 0110 1000 nnnn dddd kkkk k101 mmmm",
        Kind::Pack(PackOp::Pkhtb),
        V6,
    ),
    row(
        "cccc 0110 1000 1111 dddd xx-- 0111 mmmm",
        Kind::Extend(E::Sxtb16),
        V6,
    ),
    row(
        "cccc 0110 1000 nnnn dddd xx-- 0111 mmmm",
        Kind::Extend(E::Sxtab16),
        V6,
    ),
    row(
        "cccc 0110 1000 nnnn dddd ++++ 1011 mmmm",
        Kind::Arithmetic(A::Sel),
        V6,
    ),
    row(
        "cccc 0110 1010 iiii dddd ++++ 0011 nnnn",
        Kind::Saturate(SaturateOp::Ssat16),
        V6,
    ),
    row(
        "cccc 0110 101i iiii dddd kkkk ky01 nnnn",
        Kind::Saturate(SaturateOp::Ssat),
        V6,
    ),
    row(
        "cccc 0110 1010 1111 dddd xx-- 0111 mmmm",
        Kind::Extend(E::Sxtb),
        V6,
    ),
    row(
        "cccc 0110 1010 nnnn dddd xx-- 0111 mmmm",
        Kind::Extend(E::Sxtab),
        V6,
    ),
    row(
        "cccc 0110 1011 ++++ dddd ++++ 0011 mmmm",
        Kind::Unary(UnaryOp::Rev),
        V6,
    ),
    row(
        "cccc 0110 1011 1111 dddd xx-- 0111 mmmm",
        Kind::Extend(E::Sxth),
        V6,
    ),
    row(
        "cccc 0110 1011 nnnn dddd xx-- 0111 mmmm",
        Kind::Extend(E::Sxtah),
        V6,
    ),
    row(
        "cccc 0110 1011 ++++ dddd ++++ 1011 mmmm",
        Kind::Unary(UnaryOp::Rev16),
        V6,
    ),
    row(
        "cccc 0110 1100 1111 dddd xx-- 0111 mmmm",
        Kind::Extend(E::Uxtb16),
        V6,
    ),
    row(
        "cccc 0110 1100 nnnn dddd xx-- 0111 mmmm",
        Kind::Extend(E::Uxtab16),
        V6,
    ),
    row(
        "cccc 0110 1110 iiii dddd ++++ 0011 nnnn",
        Kind::Saturate(SaturateOp::Usat16),
        V6,
    ),
    row(
        "cccc 0110 111i iiii dddd kkkk ky01 nnnn",
        Kind::Saturate(SaturateOp::Usat),
        V6,
    ),
    row(
        "cccc 0110 1110 1111 dddd xx-- 0111 mmmm",
        Kind::Extend(E::Uxtb),
        V6,
    ),
    row(
        "cccc 0110 1110 nnnn dddd xx-- 0111 mmmm",
        Kind::Extend(E::Uxtab),
        V6,
    ),
    row(
        "cccc 0110 1111 ++++ dddd ++++ 0011 mmmm",
        Kind::Unary(UnaryOp::Rbit),
        V6T2,
    ),
    row(
        "cccc 0110 1111 1111 dddd xx-- 0111 mmmm",
        Kind::Extend(E::Uxth),
        V6,
    ),
    row(
        "cccc 0110 1111 nnnn dddd xx-- 0111 mmmm",
        Kind::Extend(E::Uxtah),
        V6,
    ),
    row(
        "cccc 0110 1111 ++++ dddd ++++ 1011 mmmm",
        Kind::Unary(UnaryOp::Revsh),
        V6,
    ),
    // Signed multiplies and the divides.
    row(
        "cccc 0111 0000 dddd 1111 mmmm 00X1 nnnn",
        Kind::Multiply(M::Smuad),
        V6,
    ),
    row(
        "cccc 0111 0000 dddd aaaa mmmm 00X1 nnnn",
        Kind::Multiply(M::Smlad),
        V6,
    ),
    row(
        "cccc 0111 0000 dddd 1111 mmmm 01X1 nnnn",
        Kind::Multiply(M::Smusd),
        V6,
    ),
    row(
        "cccc 0111 0000 dddd aaaa mmmm 01X1 nnnn",
        Kind::Multiply(M::Smlsd),
        V6,
    ),
    row(
        "cccc 0111 0001 dddd ++++ mmmm 0001 nnnn",
        Kind::Multiply(M::Sdiv),
        Divide,
    ),
    row(
        "cccc 0111 0011 dddd ++++ mmmm 0001 nnnn",
        Kind::Multiply(M::Udiv),
        Divide,
    ),
    row(
        "cccc 0111 0100 hhhh llll mmmm 00X1 nnnn",
        Kind::MultiplyLong(LongOp::Smlald),
        V6,
    ),
    row(
        "cccc 0111 0100 hhhh llll mmmm 01X1 nnnn",
        Kind::MultiplyLong(LongOp::Smlsld),
        V6,
    ),
    row(
        "cccc 0111 0101 dddd 1111 mmmm 00X1 nnnn",
        Kind::Multiply(M::Smmul),
        V6,
    ),
    row(
        "cccc 0111 0101 dddd aaaa mmmm 00X1 nnnn",
        Kind::Multiply(M::Smmla),
        V6,
    ),
    row(
        "cccc 0111 0101 dddd aaaa mmmm 11X1 nnnn",
        Kind::Multiply(M::Smmls),
        V6,
    ),
    // Sums of differences and bitfields.
    row(
        "cccc 0111 1000 dddd 1111 mmmm 0001 nnnn",
        Kind::Multiply(M::Usad8),
        V6,
    ),
    row(
        "cccc 0111 1000 dddd aaaa mmmm 0001 nnnn",
        Kind::Multiply(M::Usada8),
        V6,
    ),
    row(
        "cccc 0111 101e eeee dddd bbbb b101 nnnn",
        Kind::Bitfield(BitfieldOp::Sbfx),
        V6T2,
    ),
    row(
        "cccc 0111 110e eeee dddd bbbb b001 1111",
        Kind::Bitfield(BitfieldOp::Bfc),
        V6T2,
    ),
    row(
        "cccc 0111 110e eeee dddd bbbb b001 nnnn",
        Kind::Bitfield(BitfieldOp::Bfi),
        V6T2,
    ),
    row(
        "cccc 0111 111e eeee dddd bbbb b101 nnnn",
        Kind::Bitfield(BitfieldOp::Ubfx),
        V6T2,
    ),
    row(
        "1110 0111 1111 iiii iiii iiii 1111 iiii",
        Kind::Call(CallOp::Udf),
        V2,
    ),
];

/// Rows whose bits 27 to 25 are `100`.
static GROUP_100: &[Row] = &[
    row(
        "cccc 100P UBW0 nnnn RRRR RRRR RRRR RRRR",
        Kind::Block(BlockOp::Stm),
        V2,
    ),
    row(
        "cccc 100P UBW1 nnnn RRRR RRRR RRRR RRRR",
        Kind::Block(BlockOp::Ldm),
        V2,
    ),
    row(
        "1111 100P U1W0 ++-+ ---- -+-+ ---g gggg",
        Kind::SaveState,
        V6,
    ),
    row(
        "1111 100P U0W1 nnnn ---- +-+- ---- ----",
        Kind::ReturnFromException,
        V6,
    ),
];

/// Rows whose bits 27 to 25 are `101`.
static GROUP_101: &[Row] = &[
    row(
        "cccc 1010 iiii iiii iiii iiii iiii iiii",
        Kind::Branch(BranchOp::B),
        V2,
    ),
    row(
        "cccc 1011 iiii iiii iiii iiii iiii iiii",
        Kind::Branch(BranchOp::Bl),
        V2,
    ),
    row(
        "1111 101H iiii iiii iiii iiii iiii iiii",
        Kind::Branch(BranchOp::Blx),
        V5,
    ),
];

/// Rows whose bits 27 to 25 are `110`.
static GROUP_110: &[Row] = &[
    // The FPA claims every addressing form on its coprocessors, MCRR's and
    // MRRC's words among them.
    row(
        "cccc 110P UvW0 nnnn vDDD 0001 iiii iiii",
        Kind::FpaTransfer(FpaTransferOp::Stf),
        Fpa,
    ),
    row(
        "cccc 110P UvW1 nnnn vDDD 0001 iiii iiii",
        Kind::FpaTransfer(FpaTransferOp::Ldf),
        Fpa,
    ),
    row(
        "cccc 110P UzW0 nnnn zDDD 0010 iiii iiii",
        Kind::FpaMultiple(FpaMultipleOp::Sfm),
        Fpa,
    ),
    row(
        "cccc 110P UzW1 nnnn zDDD 0010 iiii iiii",
        Kind::FpaMultiple(FpaMultipleOp::Lfm),
        Fpa,
    ),
    row(
        "cccc 1100 0100 TTTT tttt pppp oooo MMMM",
        Kind::CoprocessorRegisters(CoRegistersOp::Mcrr),
        V5TE,
    ),
    row(
        "cccc 1100 0101 TTTT tttt pppp oooo MMMM",
        Kind::CoprocessorRegisters(CoRegistersOp::Mrrc),
        V5TE,
    ),
    row(
        "cccc 110P UBW0 nnnn DDDD pppp iiii iiii",
        Kind::CoprocessorTransfer(CoTransferOp::Stc),
        V2,
    ),
    row(
        "cccc 110P UBW1 nnnn DDDD pppp iiii iiii",
        Kind::CoprocessorTransfer(CoTransferOp::Ldc),
        V2,
    ),
    row(
        "1111 1100 0100 TTTT tttt pppp oooo MMMM",
        Kind::CoprocessorRegisters(CoRegistersOp::Mcrr2),
        V6,
    ),
    row(
        "1111 1100 0101 TTTT tttt pppp oooo MMMM",
        Kind::CoprocessorRegisters(CoRegistersOp::Mrrc2),
        V6,
    ),
    row(
        "1111 110P UBW0 nnnn DDDD pppp iiii iiii",
        Kind::CoprocessorTransfer(CoTransferOp::Stc2),
        V5,
    ),
    row(
        "1111 110P UBW1 nnnn DDDD pppp iiii iiii",
        Kind::CoprocessorTransfer(CoTransferOp::Ldc2),
        V5,
    ),
];

/// Rows whose bits 27 to 25 are `111`.
static GROUP_111: &[Row] = &[
    // SWI, which no other row in the group overlaps, first as the most common.
    row(
        "cccc 1111 iiii iiii iiii iiii iiii iiii",
        Kind::Call(CallOp::Swi),
        V2,
    ),
    fpa_dyadic!(Adf, "0000"),
    fpa_dyadic!(Muf, "0001"),
    fpa_dyadic!(Suf, "0010"),
    fpa_dyadic!(Rsf, "0011"),
    fpa_dyadic!(Dvf, "0100"),
    fpa_dyadic!(Rdf, "0101"),
    fpa_dyadic!(Pow, "0110"),
    fpa_dyadic!(Rpw, "0111"),
    fpa_dyadic!(Rmf, "1000"),
    fpa_dyadic!(Fml, "1001"),
    fpa_dyadic!(Fdv, "1010"),
    fpa_dyadic!(Frd, "1011"),
    fpa_dyadic!(Pol, "1100"),
    fpa_monadic!(Mvf, "0000"),
    fpa_monadic!(Mnf, "0001"),
    fpa_monadic!(Abs, "0010"),
    fpa_monadic!(Rnd, "0011"),
    fpa_monadic!(Sqt, "0100"),
    fpa_monadic!(Log, "0101"),
    fpa_monadic!(Lgn, "0110"),
    fpa_monadic!(Exp, "0111"),
    fpa_monadic!(Sin, "1000"),
    fpa_monadic!(Cos, "1001"),
    fpa_monadic!(Tan, "1010"),
    fpa_monadic!(Asn, "1011"),
    fpa_monadic!(Acs, "1100"),
    fpa_monadic!(Atn, "1101"),
    fpa_monadic!(Urd, "1110"),
    fpa_monadic!(Nrm, "1111"),
    row(
        "cccc 1110 0000 vNNN tttt 0001 vww1 ****",
        Kind::FpaRegister(FpaRegisterOp::Flt),
        Fpa,
    ),
    row(
        "cccc 1110 0001 v*** tttt 0001 vww1 MMMM",
        Kind::FpaRegister(FpaRegisterOp::Fix),
        Fpa,
    ),
    row(
        "cccc 1110 0010 v*** tttt 0001 v**1 ****",
        Kind::FpaRegister(FpaRegisterOp::Wfs),
        Fpa,
    ),
    row(
        "cccc 1110 0011 v*** tttt 0001 v**1 ****",
        Kind::FpaRegister(FpaRegisterOp::Rfs),
        Fpa,
    ),
    fpa_compare!(Cmf, "100"),
    fpa_compare!(Cnf, "101"),
    fpa_compare!(Cmfe, "110"),
    fpa_compare!(Cnfe, "111"),
    row(
        "cccc 1110 oooo NNNN DDDD pppp qqq0 MMMM",
        Kind::Coprocessor(CoprocessorOp::Cdp),
        V2,
    ),
    row(
        "cccc 1110 ooo0 NNNN tttt pppp qqq1 MMMM",
        Kind::CoprocessorRegister(CoRegisterOp::Mcr),
        V2,
    ),
    row(
        "cccc 1110 ooo1 NNNN tttt pppp qqq1 MMMM",
        Kind::CoprocessorRegister(CoRegisterOp::Mrc),
        V2,
    ),
    row(
        "1111 1110 oooo NNNN DDDD pppp qqq0 MMMM",
        Kind::Coprocessor(CoprocessorOp::Cdp2),
        V5,
    ),
    row(
        "1111 1110 ooo0 NNNN tttt pppp qqq1 MMMM",
        Kind::CoprocessorRegister(CoRegisterOp::Mcr2),
        V5,
    ),
    row(
        "1111 1110 ooo1 NNNN tttt pppp qqq1 MMMM",
        Kind::CoprocessorRegister(CoRegisterOp::Mrc2),
        V5,
    ),
];

/// The rows of each group, by bits 27 to 25.
pub(super) static GROUPS: [&[Row]; 8] = [
    GROUP_000, GROUP_001, GROUP_010, GROUP_011, GROUP_100, GROUP_101, GROUP_110, GROUP_111,
];

/// The floating-point and Advanced SIMD rows of each group.
pub(super) static SIMDFP: [&[Row]; 8] = [
    &[],
    simdfp::GROUP_001,
    simdfp::GROUP_010,
    &[],
    &[],
    &[],
    simdfp::GROUP_110,
    simdfp::GROUP_111,
];

/// The rows a word whose bits 27 to 25 are `group` may match, in the order
/// the first match decides.
pub(crate) fn rows(group: usize) -> impl Iterator<Item = &'static Row> {
    GROUPS[group & 7].iter().chain(SIMDFP[group & 7])
}

/// Every row, for the encoder and the table's tests.
pub(crate) fn all() -> impl Iterator<Item = &'static Row> {
    GROUPS.iter().chain(&SIMDFP).flat_map(|group| group.iter())
}

/// Whether `kind`'s rows are in the unconditional space, so its condition
/// field is not a condition.
pub(crate) fn unconditional(kind: Kind) -> bool {
    all().any(|row| row.kind == kind && row.unconditional())
}
