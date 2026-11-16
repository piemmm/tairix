//! The A32 floating-point and Advanced SIMD encodings: VFP's on coprocessors
//! 10 and 11, ARMv8's unconditional additions beside them, and Advanced
//! SIMD's in the unconditional space, as Arm's AArch32 encoding index lays
//! them out.
//!
//! Arm's index resolves some overlaps by priority, an encoding naming the
//! one that takes its words, so these rows may overlap without either being
//! more specific; the earlier decides, and a later row's build refuses the
//! words it gave away. A test holds that no row is wholly covered by an
//! earlier one.

use super::table::{row, Row};
use crate::encoding::form::Kind;
use crate::encoding::simdfp::SimdFpKind as K;
use crate::instruction::{
    ConvertRounding as R, SimdLongOp as L, SimdMiscOp as Mi, SimdScalarOp as Sc, SimdShiftOp as Sh,
    SimdThreeOp as T, VfpDyadicOp as Dy, VfpMonadicOp as Mo,
};
use crate::isa::Feature::{Crypto, FpV8, Simd, Vfp};

const fn simd(pattern: &str, kind: K) -> Row {
    row(pattern, Kind::SimdFp(kind), Simd)
}

const fn vfp(pattern: &str, kind: K) -> Row {
    row(pattern, Kind::SimdFp(kind), Vfp)
}

const fn v8(pattern: &str, kind: K) -> Row {
    row(pattern, Kind::SimdFp(kind), FpV8)
}

const fn crypto(pattern: &str, kind: K) -> Row {
    row(pattern, Kind::SimdFp(kind), Crypto)
}

/// A three-register row: its U bit, size bits, opcode and bit 4.
macro_rules! three {
    ($maker:ident, $op:ident, $u:literal, $size:literal, $opcode:literal, $o:literal) => {
        $maker(
            concat!(
                "1111 001",
                $u,
                " 0D",
                $size,
                " NNNN DDDD ",
                $opcode,
                " NQM",
                $o,
                " MMMM"
            ),
            K::Three(T::$op),
        )
    };
}

/// A row of three registers of different lengths.
macro_rules! long {
    ($maker:ident, $op:ident, $u:literal, $size:literal, $opcode:literal) => {
        $maker(
            concat!(
                "1111 001",
                $u,
                " 1D",
                $size,
                " NNNN DDDD ",
                $opcode,
                " N0M0 MMMM"
            ),
            K::Long(L::$op),
        )
    };
}

/// A row by a scalar; bit 24 is Q, or U for a long result.
macro_rules! scalar {
    ($op:ident, $bit24:literal, $opcode:literal) => {
        simd(
            concat!(
                "1111 001",
                $bit24,
                " 1Dvv NNNN DDDD ",
                $opcode,
                " N1M0 MMMM"
            ),
            K::ByScalar(Sc::$op),
        )
    };
}

/// A row by an immediate shift.
macro_rules! shift {
    ($op:ident, $u:literal, $opcode:literal, $low:literal) => {
        simd(
            concat!(
                "1111 001",
                $u,
                " 1Dii iiii DDDD ",
                $opcode,
                " ",
                $low,
                " MMMM"
            ),
            K::Shift(Sh::$op),
        )
    };
}

/// A two-register row: bits 17 and 16, then bits 11 to 4.
macro_rules! misc {
    ($maker:ident, $op:ident, $a:literal, $b:literal) => {
        $maker(
            concat!("1111 0011 1D11 vv", $a, " DDDD ", $b, " MMMM"),
            K::Misc(Mi::$op),
        )
    };
}

/// A VFP data-processing row on two operands.
macro_rules! dyadic {
    ($maker:ident, $cond:literal, $op:ident, $opc1:literal, $opc3:literal) => {
        $maker(
            concat!(
                $cond,
                " 1110 ",
                $opc1,
                " NNNN DDDD 101v N",
                $opc3,
                "M0 MMMM"
            ),
            K::Dyadic(Dy::$op),
        )
    };
}

/// A VFP data-processing row on one operand: its `opc2` and bit 7.
macro_rules! monadic {
    ($maker:ident, $cond:literal, $op:ident, $opc2:literal, $bit7:literal) => {
        $maker(
            concat!(
                $cond,
                " 1110 1D11 ",
                $opc2,
                " DDDD 101v ",
                $bit7,
                "1M0 MMMM"
            ),
            K::Monadic(Mo::$op),
        )
    };
}

/// Rows whose bits 27 to 25 are `001`: Advanced SIMD data processing.
pub(super) const GROUP_001: &[Row] = &[
    simd("1111 001i 1D00 0iii DDDD CCCC 0QX1 iiii", K::Immediate),
    shift!(Vshr, "U", "0000", "LQM1"),
    shift!(Vsra, "U", "0001", "LQM1"),
    shift!(Vrshr, "U", "0010", "LQM1"),
    shift!(Vrsra, "U", "0011", "LQM1"),
    shift!(Vsri, "1", "0100", "LQM1"),
    shift!(Vshl, "0", "0101", "LQM1"),
    shift!(Vsli, "1", "0101", "LQM1"),
    shift!(Vqshlu, "1", "0110", "LQM1"),
    shift!(Vqshl, "U", "0111", "LQM1"),
    shift!(Vshrn, "0", "1000", "00M1"),
    shift!(Vrshrn, "0", "1000", "01M1"),
    shift!(Vqshrun, "1", "1000", "00M1"),
    shift!(Vqrshrun, "1", "1000", "01M1"),
    shift!(Vqshrn, "U", "1001", "00M1"),
    shift!(Vqrshrn, "U", "1001", "01M1"),
    simd(
        "1111 001U 1Dii i000 DDDD 1010 00M1 MMMM",
        K::Shift(Sh::Vmovl),
    ),
    shift!(Vshll, "U", "1010", "00M1"),
    shift!(VcvtFromFixed, "U", "1110", "0QM1"),
    shift!(VcvtToFixed, "U", "1111", "0QM1"),
    misc!(simd, Vrev64, "00", "0000 0QM0"),
    misc!(simd, Vrev32, "00", "0000 1QM0"),
    misc!(simd, Vrev16, "00", "0001 0QM0"),
    misc!(simd, Vpaddl, "00", "0010 UQM0"),
    misc!(crypto, Aese, "00", "0011 00M0"),
    misc!(crypto, Aesd, "00", "0011 01M0"),
    misc!(crypto, Aesmc, "00", "0011 10M0"),
    misc!(crypto, Aesimc, "00", "0011 11M0"),
    misc!(simd, Vcls, "00", "0100 0QM0"),
    misc!(simd, Vclz, "00", "0100 1QM0"),
    misc!(simd, Vcnt, "00", "0101 0QM0"),
    misc!(simd, Vmvn, "00", "0101 1QM0"),
    misc!(simd, Vpadal, "00", "0110 UQM0"),
    misc!(simd, Vqabs, "00", "0111 0QM0"),
    misc!(simd, Vqneg, "00", "0111 1QM0"),
    misc!(simd, Vcgt0, "01", "0000 0QM0"),
    misc!(simd, Vcge0, "01", "0000 1QM0"),
    misc!(simd, Vceq0, "01", "0001 0QM0"),
    misc!(simd, Vcle0, "01", "0001 1QM0"),
    misc!(simd, Vclt0, "01", "0010 0QM0"),
    misc!(crypto, Sha1h, "01", "0010 11M0"),
    misc!(simd, Vabs, "01", "0011 0QM0"),
    misc!(simd, Vneg, "01", "0011 1QM0"),
    misc!(simd, Vcgt0F, "01", "0100 0QM0"),
    misc!(simd, Vcge0F, "01", "0100 1QM0"),
    misc!(simd, Vceq0F, "01", "0101 0QM0"),
    misc!(simd, Vcle0F, "01", "0101 1QM0"),
    misc!(simd, Vclt0F, "01", "0110 0QM0"),
    misc!(simd, VabsF, "01", "0111 0QM0"),
    misc!(simd, VnegF, "01", "0111 1QM0"),
    misc!(simd, Vswp, "10", "0000 0QM0"),
    misc!(simd, Vtrn, "10", "0000 1QM0"),
    misc!(simd, Vuzp, "10", "0001 0QM0"),
    misc!(simd, Vzip, "10", "0001 1QM0"),
    misc!(simd, Vmovn, "10", "0010 00M0"),
    misc!(simd, Vqmovun, "10", "0010 01M0"),
    misc!(simd, Vqmovn, "10", "0010 1UM0"),
    misc!(simd, Vshll, "10", "0011 00M0"),
    misc!(crypto, Sha1su1, "10", "0011 10M0"),
    misc!(crypto, Sha256su0, "10", "0011 11M0"),
    misc!(v8, Vrintn, "10", "0100 0QM0"),
    misc!(v8, Vrintx, "10", "0100 1QM0"),
    misc!(v8, Vrinta, "10", "0101 0QM0"),
    misc!(v8, Vrintz, "10", "0101 1QM0"),
    misc!(simd, VcvtToHalf, "10", "0110 00M0"),
    misc!(v8, Vrintm, "10", "0110 1QM0"),
    misc!(simd, VcvtFromHalf, "10", "0111 00M0"),
    misc!(v8, Vrintp, "10", "0111 1QM0"),
    misc!(v8, Vcvta, "11", "0000 UQM0"),
    misc!(v8, Vcvtn, "11", "0001 UQM0"),
    misc!(v8, Vcvtp, "11", "0010 UQM0"),
    misc!(v8, Vcvtm, "11", "0011 UQM0"),
    misc!(simd, Vrecpe, "11", "0100 0QM0"),
    misc!(simd, Vrsqrte, "11", "0100 1QM0"),
    misc!(simd, VrecpeF, "11", "0101 0QM0"),
    misc!(simd, VrsqrteF, "11", "0101 1QM0"),
    misc!(simd, VcvtToFloat, "11", "0110 UQM0"),
    misc!(simd, VcvtToInteger, "11", "0111 UQM0"),
    simd("1111 0011 1D11 NNNN DDDD 10zz NXM0 MMMM", K::Table),
    simd("1111 0011 1D11 iiii DDDD 1100 0QM0 MMMM", K::Duplicate),
    simd("1111 0010 1D11 NNNN DDDD iiii NQM0 MMMM", K::Extract),
    long!(simd, Vaddl, "U", "vv", "0000"),
    long!(simd, Vaddw, "U", "vv", "0001"),
    long!(simd, Vsubl, "U", "vv", "0010"),
    long!(simd, Vsubw, "U", "vv", "0011"),
    long!(simd, Vaddhn, "0", "vv", "0100"),
    long!(simd, Vraddhn, "1", "vv", "0100"),
    long!(simd, Vabal, "U", "vv", "0101"),
    long!(simd, Vsubhn, "0", "vv", "0110"),
    long!(simd, Vrsubhn, "1", "vv", "0110"),
    long!(simd, Vabdl, "U", "vv", "0111"),
    long!(simd, Vmlal, "U", "vv", "1000"),
    long!(simd, Vqdmlal, "0", "vv", "1001"),
    long!(simd, Vmlsl, "U", "vv", "1010"),
    long!(simd, Vqdmlsl, "0", "vv", "1011"),
    long!(simd, Vmull, "U", "vv", "1100"),
    long!(simd, Vqdmull, "0", "vv", "1101"),
    long!(simd, Vmullp, "0", "00", "1110"),
    long!(crypto, Vmullp64, "0", "10", "1110"),
    scalar!(Vmla, "Q", "0000"),
    scalar!(VmlaF, "Q", "0001"),
    scalar!(Vmlal, "U", "0010"),
    scalar!(Vqdmlal, "0", "0011"),
    scalar!(Vmls, "Q", "0100"),
    scalar!(VmlsF, "Q", "0101"),
    scalar!(Vmlsl, "U", "0110"),
    scalar!(Vqdmlsl, "0", "0111"),
    scalar!(Vmul, "Q", "1000"),
    scalar!(VmulF, "Q", "1001"),
    scalar!(Vmull, "U", "1010"),
    scalar!(Vqdmull, "0", "1011"),
    scalar!(Vqdmulh, "Q", "1100"),
    scalar!(Vqrdmulh, "Q", "1101"),
    three!(simd, Vhadd, "U", "vv", "0000", "0"),
    three!(simd, Vqadd, "U", "vv", "0000", "1"),
    three!(simd, Vrhadd, "U", "vv", "0001", "0"),
    three!(simd, Vand, "0", "00", "0001", "1"),
    three!(simd, Vbic, "0", "01", "0001", "1"),
    three!(simd, Vorr, "0", "10", "0001", "1"),
    three!(simd, Vorn, "0", "11", "0001", "1"),
    three!(simd, Veor, "1", "00", "0001", "1"),
    three!(simd, Vbsl, "1", "01", "0001", "1"),
    three!(simd, Vbit, "1", "10", "0001", "1"),
    three!(simd, Vbif, "1", "11", "0001", "1"),
    three!(simd, Vhsub, "U", "vv", "0010", "0"),
    three!(simd, Vqsub, "U", "vv", "0010", "1"),
    three!(simd, Vcgt, "U", "vv", "0011", "0"),
    three!(simd, Vcge, "U", "vv", "0011", "1"),
    three!(simd, Vshl, "U", "vv", "0100", "0"),
    three!(simd, Vqshl, "U", "vv", "0100", "1"),
    three!(simd, Vrshl, "U", "vv", "0101", "0"),
    three!(simd, Vqrshl, "U", "vv", "0101", "1"),
    three!(simd, Vmax, "U", "vv", "0110", "0"),
    three!(simd, Vmin, "U", "vv", "0110", "1"),
    three!(simd, Vabd, "U", "vv", "0111", "0"),
    three!(simd, Vaba, "U", "vv", "0111", "1"),
    three!(simd, Vadd, "0", "vv", "1000", "0"),
    three!(simd, Vsub, "1", "vv", "1000", "0"),
    three!(simd, Vtst, "0", "vv", "1000", "1"),
    three!(simd, Vceq, "1", "vv", "1000", "1"),
    three!(simd, Vmla, "0", "vv", "1001", "0"),
    three!(simd, Vmls, "1", "vv", "1001", "0"),
    three!(simd, Vmul, "0", "vv", "1001", "1"),
    three!(simd, Vmulp, "1", "vv", "1001", "1"),
    three!(simd, Vpmax, "U", "vv", "1010", "0"),
    three!(simd, Vpmin, "U", "vv", "1010", "1"),
    three!(simd, Vqdmulh, "0", "vv", "1011", "0"),
    three!(simd, Vqrdmulh, "1", "vv", "1011", "0"),
    three!(simd, Vpadd, "0", "vv", "1011", "1"),
    three!(crypto, Sha1c, "0", "00", "1100", "0"),
    three!(crypto, Sha1p, "0", "01", "1100", "0"),
    three!(crypto, Sha1m, "0", "10", "1100", "0"),
    three!(crypto, Sha1su0, "0", "11", "1100", "0"),
    three!(crypto, Sha256h, "1", "00", "1100", "0"),
    three!(crypto, Sha256h2, "1", "01", "1100", "0"),
    three!(crypto, Sha256su1, "1", "10", "1100", "0"),
    three!(simd, Vfma, "0", "00", "1100", "1"),
    three!(simd, Vfms, "0", "10", "1100", "1"),
    three!(simd, VaddF, "0", "00", "1101", "0"),
    three!(simd, VsubF, "0", "10", "1101", "0"),
    three!(simd, VpaddF, "1", "00", "1101", "0"),
    three!(simd, VabdF, "1", "10", "1101", "0"),
    three!(simd, VmlaF, "0", "00", "1101", "1"),
    three!(simd, VmlsF, "0", "10", "1101", "1"),
    three!(simd, VmulF, "1", "00", "1101", "1"),
    three!(simd, VceqF, "0", "00", "1110", "0"),
    three!(simd, VcgeF, "1", "00", "1110", "0"),
    three!(simd, VcgtF, "1", "10", "1110", "0"),
    three!(simd, Vacge, "1", "00", "1110", "1"),
    three!(simd, Vacgt, "1", "10", "1110", "1"),
    three!(simd, VmaxF, "0", "00", "1111", "0"),
    three!(simd, VminF, "0", "10", "1111", "0"),
    three!(simd, VpmaxF, "1", "00", "1111", "0"),
    three!(simd, VpminF, "1", "10", "1111", "0"),
    three!(simd, Vrecps, "0", "00", "1111", "1"),
    three!(simd, Vrsqrts, "0", "10", "1111", "1"),
    three!(v8, Vmaxnm, "1", "00", "1111", "1"),
    three!(v8, Vminnm, "1", "10", "1111", "1"),
];

/// Rows whose bits 27 to 25 are `010`: Advanced SIMD's element and structure
/// transfers.
pub(super) const GROUP_010: &[Row] = &[
    simd("1111 0100 1DL0 nnnn DDDD 11zz vvXA mmmm", K::StructureAll),
    simd("1111 0100 1DL0 nnnn DDDD vvzz AAAA mmmm", K::StructureLane),
    simd(
        "1111 0100 0DL0 nnnn DDDD oooo vvAA mmmm",
        K::StructureMultiple,
    ),
];

/// Rows whose bits 27 to 25 are `110`: the 64-bit transfers, loads and
/// stores.
pub(super) const GROUP_110: &[Row] = &[
    vfp("cccc 1100 010L TTTT tttt 101v 00M1 MMMM", K::CorePair),
    vfp("cccc 1101 UD0L nnnn DDDD 101v iiii iiii", K::Transfer),
    vfp("cccc 110P UDWL nnnn DDDD 101v iiii iiii", K::Multiple),
];

/// Rows whose bits 27 to 25 are `111`: VFP data processing, ARMv8's
/// unconditional additions and the transfers to and from core registers.
pub(super) const GROUP_111: &[Row] = &[
    dyadic!(vfp, "cccc", Vmla, "0D00", "0"),
    dyadic!(vfp, "cccc", Vmls, "0D00", "1"),
    dyadic!(vfp, "cccc", Vnmls, "0D01", "0"),
    dyadic!(vfp, "cccc", Vnmla, "0D01", "1"),
    dyadic!(vfp, "cccc", Vmul, "0D10", "0"),
    dyadic!(vfp, "cccc", Vnmul, "0D10", "1"),
    dyadic!(vfp, "cccc", Vadd, "0D11", "0"),
    dyadic!(vfp, "cccc", Vsub, "0D11", "1"),
    dyadic!(vfp, "cccc", Vdiv, "1D00", "0"),
    dyadic!(vfp, "cccc", Vfnms, "1D01", "0"),
    dyadic!(vfp, "cccc", Vfnma, "1D01", "1"),
    dyadic!(vfp, "cccc", Vfma, "1D10", "0"),
    dyadic!(vfp, "cccc", Vfms, "1D10", "1"),
    vfp("cccc 1110 1D11 iiii DDDD 101v -0-0 iiii", K::MoveImmediate),
    monadic!(vfp, "cccc", Vmov, "0000", "0"),
    monadic!(vfp, "cccc", Vabs, "0000", "1"),
    monadic!(vfp, "cccc", Vneg, "0001", "0"),
    monadic!(vfp, "cccc", Vsqrt, "0001", "1"),
    vfp(
        "cccc 1110 1D11 001o DDDD 1010 X1M0 MMMM",
        K::Half { double: false },
    ),
    v8(
        "cccc 1110 1D11 001o DDDD 1011 X1M0 MMMM",
        K::Half { double: true },
    ),
    vfp("cccc 1110 1D11 0100 DDDD 101v X1M0 MMMM", K::Compare),
    vfp("cccc 1110 1D11 0101 DDDD 101v X1-0 ----", K::CompareZero),
    monadic!(v8, "cccc", Vrintr, "0110", "0"),
    monadic!(v8, "cccc", Vrintz, "0110", "1"),
    monadic!(v8, "cccc", Vrintx, "0111", "0"),
    vfp("cccc 1110 1D11 0111 DDDD 101v 11M0 MMMM", K::Precision),
    vfp("cccc 1110 1D11 1000 DDDD 101v X1M0 MMMM", K::FromInteger),
    vfp("cccc 1110 1D11 1o1U DDDD 101v X1Y0 iiii", K::Fixed),
    vfp(
        "cccc 1110 1D11 110Y DDDD 101v 01M0 MMMM",
        K::ToInteger(R::Fpscr),
    ),
    vfp(
        "cccc 1110 1D11 110Y DDDD 101v 11M0 MMMM",
        K::ToInteger(R::Zero),
    ),
    v8("1111 1110 0Doo NNNN DDDD 101v N0M0 MMMM", K::Select),
    dyadic!(v8, "1111", Vmaxnm, "1D00", "0"),
    dyadic!(v8, "1111", Vminnm, "1D00", "1"),
    monadic!(v8, "1111", Vrinta, "1000", "0"),
    monadic!(v8, "1111", Vrintn, "1001", "0"),
    monadic!(v8, "1111", Vrintp, "1010", "0"),
    monadic!(v8, "1111", Vrintm, "1011", "0"),
    v8(
        "1111 1110 1D11 1100 DDDD 101v Y1M0 MMMM",
        K::ToInteger(R::Away),
    ),
    v8(
        "1111 1110 1D11 1101 DDDD 101v Y1M0 MMMM",
        K::ToInteger(R::Nearest),
    ),
    v8(
        "1111 1110 1D11 1110 DDDD 101v Y1M0 MMMM",
        K::ToInteger(R::Plus),
    ),
    v8(
        "1111 1110 1D11 1111 DDDD 101v Y1M0 MMMM",
        K::ToInteger(R::Minus),
    ),
    vfp("cccc 1110 000L NNNN tttt 1010 N--1 ----", K::CoreSingle),
    vfp("cccc 1110 111L NNNN tttt 1010 ---1 ----", K::System),
    vfp(
        "cccc 1110 00o0 NNNN tttt 1011 N001 ----",
        K::CoreScalar { to_core: false },
    ),
    simd(
        "cccc 1110 0oo0 NNNN tttt 1011 Nqq1 ----",
        K::CoreScalar { to_core: false },
    ),
    vfp(
        "cccc 1110 00o1 NNNN tttt 1011 N001 ----",
        K::CoreScalar { to_core: true },
    ),
    simd(
        "cccc 1110 Uoo1 NNNN tttt 1011 Nqq1 ----",
        K::CoreScalar { to_core: true },
    ),
    simd("cccc 1110 1BQ0 NNNN tttt 1011 N0E1 ----", K::CoreDuplicate),
];
