//! The floating-point and Advanced SIMD vocabulary: VFP's operations on
//! single and double registers, Advanced SIMD's on the lanes of doubleword
//! and quadword registers, the Cryptographic Extension's, and the transfers
//! between them, memory and the core registers.
//!
//! A register number is the architecture's: a single register's `Sn`, a
//! double's `Dn`. An Advanced SIMD operand is named by its first doubleword,
//! so a quadword operand's number is even.

use super::op::BlockMode;
use crate::cond::Condition;
use crate::reg::Reg;
use crate::vfp::System;

operations! {
    /// A VFP operation on two operands: some also read the destination.
    VfpDyadicOp {
        Vmla = "VMLA",
        Vmls = "VMLS",
        Vnmla = "VNMLA",
        Vnmls = "VNMLS",
        Vmul = "VMUL",
        Vnmul = "VNMUL",
        Vadd = "VADD",
        Vsub = "VSUB",
        Vdiv = "VDIV",
        Vfma = "VFMA",
        Vfms = "VFMS",
        Vfnma = "VFNMA",
        Vfnms = "VFNMS",
        Vmaxnm = "VMAXNM",
        Vminnm = "VMINNM",
    }
}

impl VfpDyadicOp {
    /// Whether it executes as a short vector when the FPSCR sets one.
    #[must_use]
    pub const fn vector(self) -> bool {
        !matches!(self, Self::Vmaxnm | Self::Vminnm)
    }
}

operations! {
    /// A VFP operation on one operand.
    VfpMonadicOp {
        Vmov = "VMOV",
        Vabs = "VABS",
        Vneg = "VNEG",
        Vsqrt = "VSQRT",
        Vrintr = "VRINTR",
        Vrintz = "VRINTZ",
        Vrintx = "VRINTX",
        Vrinta = "VRINTA",
        Vrintn = "VRINTN",
        Vrintp = "VRINTP",
        Vrintm = "VRINTM",
    }
}

impl VfpMonadicOp {
    /// Whether it executes as a short vector when the FPSCR sets one.
    #[must_use]
    pub const fn vector(self) -> bool {
        matches!(self, Self::Vmov | Self::Vabs | Self::Vneg | Self::Vsqrt)
    }
}

operations! {
    /// How a conversion to an integer rounds: ARMv7's toward zero and as the
    /// FPSCR says, and ARMv8's four directions, whose mnemonics name them.
    ConvertRounding {
        Zero = "VCVT",
        Fpscr = "VCVTR",
        Away = "VCVTA",
        Nearest = "VCVTN",
        Plus = "VCVTP",
        Minus = "VCVTM",
    }
}

operations! {
    /// An Advanced SIMD operation on three registers of one length.
    SimdThreeOp {
        Vhadd = "VHADD",
        Vqadd = "VQADD",
        Vrhadd = "VRHADD",
        Vand = "VAND",
        Vbic = "VBIC",
        Vorr = "VORR",
        Vorn = "VORN",
        Veor = "VEOR",
        Vbsl = "VBSL",
        Vbit = "VBIT",
        Vbif = "VBIF",
        Vhsub = "VHSUB",
        Vqsub = "VQSUB",
        Vcgt = "VCGT",
        Vcge = "VCGE",
        Vshl = "VSHL",
        Vqshl = "VQSHL",
        Vrshl = "VRSHL",
        Vqrshl = "VQRSHL",
        Vmax = "VMAX",
        Vmin = "VMIN",
        Vabd = "VABD",
        Vaba = "VABA",
        Vadd = "VADD",
        Vsub = "VSUB",
        Vtst = "VTST",
        Vceq = "VCEQ",
        Vmla = "VMLA",
        Vmls = "VMLS",
        Vmul = "VMUL",
        Vmulp = "VMUL",
        Vpmax = "VPMAX",
        Vpmin = "VPMIN",
        Vqdmulh = "VQDMULH",
        Vqrdmulh = "VQRDMULH",
        Vpadd = "VPADD",
        Sha1c = "SHA1C",
        Sha1p = "SHA1P",
        Sha1m = "SHA1M",
        Sha1su0 = "SHA1SU0",
        Sha256h = "SHA256H",
        Sha256h2 = "SHA256H2",
        Sha256su1 = "SHA256SU1",
        Vfma = "VFMA",
        Vfms = "VFMS",
        VaddF = "VADD",
        VsubF = "VSUB",
        VpaddF = "VPADD",
        VabdF = "VABD",
        VmlaF = "VMLA",
        VmlsF = "VMLS",
        VmulF = "VMUL",
        VceqF = "VCEQ",
        VcgeF = "VCGE",
        VcgtF = "VCGT",
        Vacge = "VACGE",
        Vacgt = "VACGT",
        VmaxF = "VMAX",
        VminF = "VMIN",
        VpmaxF = "VPMAX",
        VpminF = "VPMIN",
        Vrecps = "VRECPS",
        Vrsqrts = "VRSQRTS",
        Vmaxnm = "VMAXNM",
        Vminnm = "VMINNM",
    }
}

operations! {
    /// An Advanced SIMD operation whose operands differ in length: a long
    /// result, a wide operand or a narrowed result.
    SimdLongOp {
        Vaddl = "VADDL",
        Vaddw = "VADDW",
        Vsubl = "VSUBL",
        Vsubw = "VSUBW",
        Vaddhn = "VADDHN",
        Vraddhn = "VRADDHN",
        Vabal = "VABAL",
        Vsubhn = "VSUBHN",
        Vrsubhn = "VRSUBHN",
        Vabdl = "VABDL",
        Vmlal = "VMLAL",
        Vqdmlal = "VQDMLAL",
        Vmlsl = "VMLSL",
        Vqdmlsl = "VQDMLSL",
        Vmull = "VMULL",
        Vqdmull = "VQDMULL",
        Vmullp = "VMULL",
        Vmullp64 = "VMULL",
    }
}

/// What lengths a [`SimdLongOp`]'s registers have.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Width {
    /// A quadword result from doubleword operands.
    Long,
    /// A quadword result from a quadword and a doubleword operand.
    Wide,
    /// A doubleword result from quadword operands.
    Narrow,
}

impl SimdLongOp {
    /// Its registers' lengths.
    #[must_use]
    pub const fn width(self) -> Width {
        match self {
            Self::Vaddw | Self::Vsubw => Width::Wide,
            Self::Vaddhn | Self::Vraddhn | Self::Vsubhn | Self::Vrsubhn => Width::Narrow,
            _ => Width::Long,
        }
    }
}

operations! {
    /// An Advanced SIMD operation by a scalar: each lane with one lane of
    /// the last operand.
    SimdScalarOp {
        Vmla = "VMLA",
        VmlaF = "VMLA",
        Vmlal = "VMLAL",
        Vqdmlal = "VQDMLAL",
        Vmls = "VMLS",
        VmlsF = "VMLS",
        Vmlsl = "VMLSL",
        Vqdmlsl = "VQDMLSL",
        Vmul = "VMUL",
        VmulF = "VMUL",
        Vmull = "VMULL",
        Vqdmull = "VQDMULL",
        Vqdmulh = "VQDMULH",
        Vqrdmulh = "VQRDMULH",
    }
}

impl SimdScalarOp {
    /// Whether the result is long: a quadword from doubleword operands.
    #[must_use]
    pub const fn long(self) -> bool {
        matches!(
            self,
            Self::Vmlal | Self::Vqdmlal | Self::Vmlsl | Self::Vqdmlsl | Self::Vmull | Self::Vqdmull
        )
    }

    /// Whether it operates on single-precision lanes.
    #[must_use]
    pub const fn float(self) -> bool {
        matches!(self, Self::VmlaF | Self::VmlsF | Self::VmulF)
    }
}

operations! {
    /// An Advanced SIMD operation by an immediate shift.
    SimdShiftOp {
        Vshr = "VSHR",
        Vsra = "VSRA",
        Vrshr = "VRSHR",
        Vrsra = "VRSRA",
        Vsri = "VSRI",
        Vshl = "VSHL",
        Vsli = "VSLI",
        Vqshlu = "VQSHLU",
        Vqshl = "VQSHL",
        Vshrn = "VSHRN",
        Vrshrn = "VRSHRN",
        Vqshrun = "VQSHRUN",
        Vqrshrun = "VQRSHRUN",
        Vqshrn = "VQSHRN",
        Vqrshrn = "VQRSHRN",
        Vshll = "VSHLL",
        Vmovl = "VMOVL",
        VcvtFromFixed = "VCVT",
        VcvtToFixed = "VCVT",
    }
}

impl SimdShiftOp {
    /// Whether the shift is to the right.
    #[must_use]
    pub const fn right(self) -> bool {
        !matches!(
            self,
            Self::Vshl | Self::Vsli | Self::Vqshlu | Self::Vqshl | Self::Vshll | Self::Vmovl
        )
    }

    /// Whether the result is narrowed to half its lanes' width.
    #[must_use]
    pub const fn narrow(self) -> bool {
        matches!(
            self,
            Self::Vshrn
                | Self::Vrshrn
                | Self::Vqshrun
                | Self::Vqrshrun
                | Self::Vqshrn
                | Self::Vqrshrn
        )
    }

    /// Whether the result is lengthened to twice its lanes' width.
    #[must_use]
    pub const fn long(self) -> bool {
        matches!(self, Self::Vshll | Self::Vmovl)
    }
}

operations! {
    /// An Advanced SIMD operation on two registers.
    SimdMiscOp {
        Vrev64 = "VREV64",
        Vrev32 = "VREV32",
        Vrev16 = "VREV16",
        Vpaddl = "VPADDL",
        Aese = "AESE",
        Aesd = "AESD",
        Aesmc = "AESMC",
        Aesimc = "AESIMC",
        Vcls = "VCLS",
        Vclz = "VCLZ",
        Vcnt = "VCNT",
        Vmvn = "VMVN",
        Vpadal = "VPADAL",
        Vqabs = "VQABS",
        Vqneg = "VQNEG",
        Vcgt0 = "VCGT",
        Vcge0 = "VCGE",
        Vceq0 = "VCEQ",
        Vcle0 = "VCLE",
        Vclt0 = "VCLT",
        Vabs = "VABS",
        Vneg = "VNEG",
        Vcgt0F = "VCGT",
        Vcge0F = "VCGE",
        Vceq0F = "VCEQ",
        Vcle0F = "VCLE",
        Vclt0F = "VCLT",
        VabsF = "VABS",
        VnegF = "VNEG",
        Sha1h = "SHA1H",
        Vswp = "VSWP",
        Vtrn = "VTRN",
        Vuzp = "VUZP",
        Vzip = "VZIP",
        Vmovn = "VMOVN",
        Vqmovun = "VQMOVUN",
        Vqmovn = "VQMOVN",
        Vshll = "VSHLL",
        Sha1su1 = "SHA1SU1",
        Sha256su0 = "SHA256SU0",
        Vrintn = "VRINTN",
        Vrintx = "VRINTX",
        Vrinta = "VRINTA",
        Vrintz = "VRINTZ",
        Vrintm = "VRINTM",
        Vrintp = "VRINTP",
        VcvtToHalf = "VCVT",
        VcvtFromHalf = "VCVT",
        Vcvta = "VCVTA",
        Vcvtn = "VCVTN",
        Vcvtp = "VCVTP",
        Vcvtm = "VCVTM",
        Vrecpe = "VRECPE",
        VrecpeF = "VRECPE",
        Vrsqrte = "VRSQRTE",
        VrsqrteF = "VRSQRTE",
        VcvtToFloat = "VCVT",
        VcvtToInteger = "VCVT",
    }
}

operations! {
    /// An Advanced SIMD operation by a modified immediate.
    SimdImmediateOp {
        Vmov = "VMOV",
        Vorr = "VORR",
        Vmvn = "VMVN",
        Vbic = "VBIC",
    }
}

impl SimdImmediateOp {
    /// The operation `cmode`, the encoding's op bit above its four cmode
    /// bits, selects; none for the one reserved value, `1 1111`.
    #[must_use]
    pub const fn of(cmode: u8) -> Option<Self> {
        let inverted = cmode & 0x10 != 0;
        let low = cmode & 0xF;
        Some(match (inverted, low) {
            (false, 0..=11) if low & 1 != 0 => Self::Vorr,
            (false, _) | (true, 14) => Self::Vmov,
            (true, 15) => return None,
            (true, 0..=11) if low & 1 != 0 => Self::Vbic,
            (true, _) => Self::Vmvn,
        })
    }
}

/// Which lanes an element or structure transfer reaches.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum StructureLanes {
    /// Every lane of every register, structures interleaved: `VLDn` and
    /// `VSTn` of multiple structures.
    Multiple,
    /// One lane of each register.
    One(u8),
    /// One structure to every lane: `VLDn`'s replicating form.
    All,
}

/// An element or structure transfer's shape.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Structure {
    /// The lanes it reaches.
    pub lanes: StructureLanes,
    /// The structure's elements, 1 to 4: `VLD1` to `VLD4`.
    pub elements: u8,
    /// Registers each element fills: more than one only for `VLD1` and
    /// `VLD2` of multiple structures, and `VLD1` to all lanes.
    pub parts: u8,
    /// The distance between one element's registers and the next's.
    pub spacing: u8,
    /// The element's size: 8 << `size` bits.
    pub size: u8,
    /// The alignment the address must have, in bytes; one for none.
    pub align: u8,
}

impl Structure {
    /// The registers the transfer names, in their list's order.
    #[must_use]
    pub const fn registers(&self) -> u32 {
        self.elements as u32 * self.parts as u32
    }

    /// The `index`th register of the list after the first.
    #[must_use]
    pub const fn register(&self, index: u32) -> u32 {
        if self.parts > 1 {
            index
        } else {
            index * self.spacing as u32
        }
    }
}

/// A floating-point or Advanced SIMD instruction.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum SimdFp {
    /// A VFP data operation on two operands.
    Dyadic {
        /// The operation.
        op: VfpDyadicOp,
        /// Double precision rather than single.
        double: bool,
        /// The destination.
        d: u8,
        /// The first operand.
        n: u8,
        /// The second operand.
        m: u8,
    },
    /// `VSEL`: `d` is `n` where the condition holds, else `m`.
    Select {
        /// `EQ`, `VS`, `GE` or `GT`.
        cond: Condition,
        /// Double precision rather than single.
        double: bool,
        /// The destination.
        d: u8,
        /// The operand chosen where the condition holds.
        n: u8,
        /// The operand chosen where it fails.
        m: u8,
    },
    /// A VFP data operation on one operand.
    Monadic {
        /// The operation.
        op: VfpMonadicOp,
        /// Double precision rather than single.
        double: bool,
        /// The destination.
        d: u8,
        /// The operand.
        m: u8,
    },
    /// `VCMP` and `VCMPE`, the flags to the FPSCR.
    Compare {
        /// `VCMPE`: a quiet NaN signals.
        signalling: bool,
        /// Double precision rather than single.
        double: bool,
        /// The first operand.
        d: u8,
        /// The second operand, or zero.
        m: Option<u8>,
    },
    /// `VCVT` between single and double precision.
    Precision {
        /// To double precision from single, rather than the reverse.
        to_double: bool,
        /// The destination.
        d: u8,
        /// The operand.
        m: u8,
    },
    /// `VCVTB` and `VCVTT`: half precision in a single register's bottom
    /// or top half, to or from single or double precision.
    Half {
        /// To half precision rather than from it.
        to_half: bool,
        /// The other format double precision rather than single.
        double: bool,
        /// The single register's top half.
        top: bool,
        /// The destination.
        d: u8,
        /// The operand.
        m: u8,
    },
    /// A conversion to a 32-bit integer in a single register.
    ToInteger {
        /// How it rounds.
        rounding: ConvertRounding,
        /// An unsigned integer rather than a signed one.
        unsigned: bool,
        /// A double operand rather than a single.
        double: bool,
        /// The destination single register.
        d: u8,
        /// The operand.
        m: u8,
    },
    /// A conversion from a 32-bit integer in a single register.
    FromInteger {
        /// An unsigned integer rather than a signed one.
        unsigned: bool,
        /// A double result rather than a single.
        double: bool,
        /// The destination.
        d: u8,
        /// The operand single register.
        m: u8,
    },
    /// A conversion between floating and fixed point in one register.
    Fixed {
        /// To fixed point rather than from it.
        to_fixed: bool,
        /// An unsigned fixed-point value rather than a signed one.
        unsigned: bool,
        /// A 32-bit fixed-point value rather than a 16-bit one.
        word: bool,
        /// A double register rather than a single.
        double: bool,
        /// The register, read and written.
        d: u8,
        /// The fraction bits.
        fraction: u8,
    },
    /// `VMOV` of an immediate: VFP's eight-bit constants.
    MoveImmediate {
        /// Double precision rather than single.
        double: bool,
        /// The destination.
        d: u8,
        /// The constant's encoding.
        imm: u8,
    },
    /// `VMOV` between a core register and a single register.
    CoreSingle {
        /// To the core register rather than from it.
        to_core: bool,
        /// The single register.
        n: u8,
        /// The core register.
        rt: Reg,
    },
    /// `VMOV` between two core registers and two consecutive single
    /// registers or one double.
    CorePair {
        /// To the core registers rather than from them.
        to_core: bool,
        /// A double register rather than two singles.
        double: bool,
        /// The first single register, or the double.
        m: u8,
        /// The core register for the low word.
        rt: Reg,
        /// The core register for the high word.
        rt2: Reg,
    },
    /// `VMOV` between a core register and a lane of a double register.
    CoreScalar {
        /// To the core register rather than from it.
        to_core: bool,
        /// The lane's size: 8 << `size` bits.
        size: u8,
        /// A narrow lane zero-extended to the core register rather than
        /// sign-extended.
        unsigned: bool,
        /// The lane.
        index: u8,
        /// The double register.
        d: u8,
        /// The core register.
        rt: Reg,
    },
    /// `VDUP` of a core register to every lane.
    CoreDuplicate {
        /// The lanes' size: 8 << `size` bits.
        size: u8,
        /// A quadword destination.
        q: bool,
        /// The destination.
        d: u8,
        /// The core register.
        rt: Reg,
    },
    /// `VMRS` and `VMSR`.
    System {
        /// `VMRS`: the system register to the core register.
        read: bool,
        /// The system register.
        register: System,
        /// The core register; `VMRS`'s R15 is APSR's flags.
        rt: Reg,
    },
    /// `VLDR` and `VSTR`.
    Transfer {
        /// A load rather than a store.
        load: bool,
        /// A double register rather than a single.
        double: bool,
        /// The register.
        d: u8,
        /// The base.
        rn: Reg,
        /// Add the offset rather than subtract it.
        up: bool,
        /// The offset in words.
        offset: u8,
    },
    /// `VLDM` and `VSTM`, `VPUSH` and `VPOP` among them, and the `FLDMX`
    /// and `FSTMX` forms.
    Multiple {
        /// A load rather than a store.
        load: bool,
        /// Double registers rather than singles.
        double: bool,
        /// The first register.
        d: u8,
        /// The base.
        rn: Reg,
        /// Increment after or decrement before.
        mode: BlockMode,
        /// Write the final address back.
        writeback: bool,
        /// The words the transfer spans: one past the registers' for `FLDMX`
        /// and `FSTMX`.
        words: u8,
    },
    /// An Advanced SIMD operation on three registers of one length.
    Three {
        /// The operation.
        op: SimdThreeOp,
        /// Unsigned lanes rather than signed, where the operation has both.
        unsigned: bool,
        /// The lanes' size: 8 << `size` bits.
        size: u8,
        /// Quadword registers.
        q: bool,
        /// The destination.
        d: u8,
        /// The first operand.
        n: u8,
        /// The second operand.
        m: u8,
    },
    /// An Advanced SIMD operation whose registers differ in length.
    Long {
        /// The operation.
        op: SimdLongOp,
        /// Unsigned lanes rather than signed, where the operation has both.
        unsigned: bool,
        /// The narrower lanes' size: 8 << `size` bits.
        size: u8,
        /// The destination.
        d: u8,
        /// The first operand.
        n: u8,
        /// The second operand.
        m: u8,
    },
    /// An Advanced SIMD operation by a scalar.
    ByScalar {
        /// The operation.
        op: SimdScalarOp,
        /// Unsigned lanes rather than signed, where the operation has both.
        unsigned: bool,
        /// The lanes' size: 8 << `size` bits.
        size: u8,
        /// Quadword registers, where the result is not long.
        q: bool,
        /// The destination.
        d: u8,
        /// The first operand.
        n: u8,
        /// The scalar's double register.
        m: u8,
        /// The scalar's lane.
        index: u8,
    },
    /// An Advanced SIMD operation by an immediate shift.
    Shift {
        /// The operation.
        op: SimdShiftOp,
        /// Unsigned lanes rather than signed, where the operation has both.
        unsigned: bool,
        /// The lanes' size, 8 << `size` bits: the operand's, but the
        /// result's for a narrowing shift.
        size: u8,
        /// Quadword registers, where the result is neither long nor
        /// narrowed.
        q: bool,
        /// The destination.
        d: u8,
        /// The operand.
        m: u8,
        /// The shift, or a fixed-point conversion's fraction bits.
        amount: u8,
    },
    /// An Advanced SIMD operation on two registers.
    Misc {
        /// The operation.
        op: SimdMiscOp,
        /// Unsigned lanes rather than signed, where the operation has both.
        unsigned: bool,
        /// The lanes' size: 8 << `size` bits.
        size: u8,
        /// Quadword registers.
        q: bool,
        /// The destination.
        d: u8,
        /// The operand.
        m: u8,
    },
    /// An Advanced SIMD operation by a modified immediate.
    Immediate {
        /// How the immediate expands and what is done with it: the
        /// encoding's op bit above its cmode bits, as
        /// [`SimdImmediateOp::of`] reads them.
        cmode: u8,
        /// A quadword destination.
        q: bool,
        /// The destination.
        d: u8,
        /// The immediate's eight bits.
        imm: u8,
    },
    /// `VEXT`: bytes from the top of `n` and the bottom of `m`.
    Extract {
        /// Quadword registers.
        q: bool,
        /// The destination.
        d: u8,
        /// The low operand.
        n: u8,
        /// The high operand.
        m: u8,
        /// The first byte taken.
        start: u8,
    },
    /// `VTBL` and `VTBX`: byte lookups in a table of registers.
    Table {
        /// `VTBX`: an index past the table keeps the destination's byte.
        extend: bool,
        /// The table's registers, 1 to 4.
        length: u8,
        /// The destination.
        d: u8,
        /// The table's first register.
        n: u8,
        /// The indices.
        m: u8,
    },
    /// `VDUP` of a lane to every lane.
    Duplicate {
        /// The lanes' size: 8 << `size` bits.
        size: u8,
        /// A quadword destination.
        q: bool,
        /// The destination.
        d: u8,
        /// The operand's double register.
        m: u8,
        /// The operand's lane.
        index: u8,
    },
    /// `VLDn` and `VSTn`.
    Structure {
        /// A load rather than a store.
        load: bool,
        /// What it transfers.
        shape: Structure,
        /// The first register.
        d: u8,
        /// The base.
        rn: Reg,
        /// The post-index: the PC for none, SP for the transfer's size, or
        /// a register to add.
        rm: Reg,
    },
}
