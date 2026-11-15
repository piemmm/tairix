//! The instruction vocabulary: an [`Op`] for each form, carrying the
//! operation its encodings select and the operands their fields hold.

use super::SimdFp;
use crate::cond::Condition;
use crate::fpa::{Format, Precision, Rounding};
use crate::reg::{Reg, RegList};

// Every decoded page slot holds one; past 16 bytes the interpreter measurably
// slows.
const _: () = assert!(core::mem::size_of::<Instruction>() <= 16);

/// A decoded instruction.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Instruction {
    /// The condition: an A32 instruction's field, [`Condition::Nv`] for an
    /// unconditional one; a T32 instruction's from its `IT` block, or a
    /// conditional branch's own.
    pub cond: Condition,
    /// The operation and its operands.
    pub op: Op,
}

operations! {
    /// A data-processing operation that writes Rd from Rn and an operand.
    DataOp {
        And = "AND",
        Eor = "EOR",
        Sub = "SUB",
        Rsb = "RSB",
        Add = "ADD",
        Adc = "ADC",
        Sbc = "SBC",
        Rsc = "RSC",
        Orr = "ORR",
        Bic = "BIC",
        Orn = "ORN",
    }
}

operations! {
    /// A comparison: a data-processing operation that only sets the flags.
    CompareOp {
        Tst = "TST",
        Teq = "TEQ",
        Cmp = "CMP",
        Cmn = "CMN",
    }
}

operations! {
    /// A move of an operand, or of its complement.
    MoveOp {
        Mov = "MOV",
        Mvn = "MVN",
    }
}

operations! {
    /// A 16-bit immediate move (ARMv6T2).
    WideOp {
        Movw = "MOVW",
        Movt = "MOVT",
    }
}

operations! {
    /// A multiply, divide or sum of differences with a 32-bit result.
    MultiplyOp {
        Mul = "MUL",
        Mla = "MLA",
        Mls = "MLS",
        Smla = "SMLA",
        Smlaw = "SMLAW",
        Smulw = "SMULW",
        Smul = "SMUL",
        Smlad = "SMLAD",
        Smuad = "SMUAD",
        Smlsd = "SMLSD",
        Smusd = "SMUSD",
        Smmla = "SMMLA",
        Smmul = "SMMUL",
        Smmls = "SMMLS",
        Usad8 = "USAD8",
        Usada8 = "USADA8",
        Sdiv = "SDIV",
        Udiv = "UDIV",
    }
}

operations! {
    /// A multiply with a 64-bit result in `RdLo` and `RdHi`.
    LongOp {
        Umull = "UMULL",
        Umlal = "UMLAL",
        Smull = "SMULL",
        Smlal = "SMLAL",
        Umaal = "UMAAL",
        SmlalHalves = "SMLAL",
        Smlald = "SMLALD",
        Smlsld = "SMLSLD",
    }
}

operations! {
    /// A three-register operation: saturating, `SEL` and the checksums.
    ArithmeticOp {
        Qadd = "QADD",
        Qsub = "QSUB",
        Qdadd = "QDADD",
        Qdsub = "QDSUB",
        Sel = "SEL",
        Crc32b = "CRC32B",
        Crc32h = "CRC32H",
        Crc32w = "CRC32W",
        Crc32cb = "CRC32CB",
        Crc32ch = "CRC32CH",
        Crc32cw = "CRC32CW",
    }
}

operations! {
    /// How a parallel operation treats each lane's result (ARMv6).
    Prefix {
        Signed = "S",
        SignedSaturating = "Q",
        SignedHalving = "SH",
        Unsigned = "U",
        UnsignedSaturating = "UQ",
        UnsignedHalving = "UH",
    }
}

operations! {
    /// Which lanes a parallel operation combines, and how (ARMv6).
    Lanes {
        Add16 = "ADD16",
        Asx = "ASX",
        Sax = "SAX",
        Sub16 = "SUB16",
        Add8 = "ADD8",
        Sub8 = "SUB8",
    }
}

operations! {
    /// A one-source operation.
    UnaryOp {
        Clz = "CLZ",
        Rev = "REV",
        Rev16 = "REV16",
        Revsh = "REVSH",
        Rbit = "RBIT",
    }
}

operations! {
    /// A saturation to a bit position (ARMv6).
    SaturateOp {
        Ssat = "SSAT",
        Usat = "USAT",
        Ssat16 = "SSAT16",
        Usat16 = "USAT16",
    }
}

operations! {
    /// A sign or zero extension, with or without an addend (ARMv6).
    ExtendOp {
        Sxtab16 = "SXTAB16",
        Sxtb16 = "SXTB16",
        Sxtab = "SXTAB",
        Sxtb = "SXTB",
        Sxtah = "SXTAH",
        Sxth = "SXTH",
        Uxtab16 = "UXTAB16",
        Uxtb16 = "UXTB16",
        Uxtab = "UXTAB",
        Uxtb = "UXTB",
        Uxtah = "UXTAH",
        Uxth = "UXTH",
    }
}

operations! {
    /// A halfword pack (ARMv6).
    PackOp {
        Pkhbt = "PKHBT",
        Pkhtb = "PKHTB",
    }
}

operations! {
    /// A bitfield operation (ARMv6T2).
    BitfieldOp {
        Sbfx = "SBFX",
        Ubfx = "UBFX",
        Bfi = "BFI",
        Bfc = "BFC",
    }
}

operations! {
    /// A branch by an immediate offset.
    BranchOp {
        B = "B",
        Bl = "BL",
        Blx = "BLX",
    }
}

operations! {
    /// A branch to a register's address.
    ExchangeOp {
        Bx = "BX",
        Bxj = "BXJ",
        Blx = "BLX",
    }
}

operations! {
    /// An instruction whose immediate a handler reads: a software interrupt,
    /// a breakpoint, or the permanently undefined instruction.
    CallOp {
        Swi = "SWI",
        Bkpt = "BKPT",
        Udf = "UDF",
    }
}

operations! {
    /// A hint (ARMv6K).
    HintOp {
        Nop = "NOP",
        Yield = "YIELD",
        Wfe = "WFE",
        Wfi = "WFI",
        Sev = "SEV",
        Sevl = "SEVL",
        Csdb = "CSDB",
        Dbg = "DBG",
        Hint = "HINT",
    }
}

operations! {
    /// A memory barrier (ARMv7), or a speculation barrier (ARMv8).
    BarrierOp {
        Dsb = "DSB",
        Dmb = "DMB",
        Isb = "ISB",
        Ssbb = "SSBB",
        Pssbb = "PSSBB",
    }
}

operations! {
    /// A single load or store.
    TransferOp {
        Ldr = "LDR",
        Str = "STR",
        Ldrb = "LDRB",
        Strb = "STRB",
        Ldrt = "LDRT",
        Strt = "STRT",
        Ldrbt = "LDRBT",
        Strbt = "STRBT",
        Ldrh = "LDRH",
        Strh = "STRH",
        Ldrsb = "LDRSB",
        Ldrsh = "LDRSH",
        Ldrht = "LDRHT",
        Strht = "STRHT",
        Ldrsbt = "LDRSBT",
        Ldrsht = "LDRSHT",
    }
}

impl TransferOp {
    /// Whether the transfer reaches memory as User mode would: the `T`
    /// forms.
    #[must_use]
    pub const fn unprivileged(self) -> bool {
        matches!(
            self,
            Self::Ldrt
                | Self::Strt
                | Self::Ldrbt
                | Self::Strbt
                | Self::Ldrht
                | Self::Strht
                | Self::Ldrsbt
                | Self::Ldrsht
        )
    }

    /// Whether the transfer stores.
    #[must_use]
    pub const fn stores(self) -> bool {
        matches!(
            self,
            Self::Str | Self::Strb | Self::Strh | Self::Strt | Self::Strbt | Self::Strht
        )
    }
}

operations! {
    /// A transfer of a pair of words (ARMv5TE).
    DualOp {
        Ldrd = "LDRD",
        Strd = "STRD",
    }
}

operations! {
    /// A preload hint.
    PreloadOp {
        Pld = "PLD",
        Pldw = "PLDW",
        Pli = "PLI",
    }
}

operations! {
    /// A block transfer.
    BlockOp {
        Ldm = "LDM",
        Stm = "STM",
    }
}

operations! {
    /// An atomic swap (ARMv2a to ARMv7).
    SwapOp {
        Swp = "SWP",
        Swpb = "SWPB",
    }
}

operations! {
    /// An exclusive, load-acquire or store-release transfer.
    ExclusiveOp {
        Ldrex = "LDREX",
        Strex = "STREX",
        Ldrexb = "LDREXB",
        Strexb = "STREXB",
        Ldrexh = "LDREXH",
        Strexh = "STREXH",
        Ldrexd = "LDREXD",
        Strexd = "STREXD",
        Lda = "LDA",
        Ldab = "LDAB",
        Ldah = "LDAH",
        Stl = "STL",
        Stlb = "STLB",
        Stlh = "STLH",
        Ldaex = "LDAEX",
        Ldaexb = "LDAEXB",
        Ldaexh = "LDAEXH",
        Ldaexd = "LDAEXD",
        Stlex = "STLEX",
        Stlexb = "STLEXB",
        Stlexh = "STLEXH",
        Stlexd = "STLEXD",
    }
}

operations! {
    /// A coprocessor data operation.
    CoprocessorOp {
        Cdp = "CDP",
        Cdp2 = "CDP2",
    }
}

operations! {
    /// A transfer between a core register and a coprocessor.
    CoRegisterOp {
        Mcr = "MCR",
        Mrc = "MRC",
        Mcr2 = "MCR2",
        Mrc2 = "MRC2",
    }
}

operations! {
    /// A transfer between two core registers and a coprocessor (ARMv5TE).
    CoRegistersOp {
        Mcrr = "MCRR",
        Mrrc = "MRRC",
        Mcrr2 = "MCRR2",
        Mrrc2 = "MRRC2",
    }
}

operations! {
    /// A transfer between memory and a coprocessor.
    CoTransferOp {
        Ldc = "LDC",
        Stc = "STC",
        Ldc2 = "LDC2",
        Stc2 = "STC2",
    }
}

operations! {
    /// An FPA dyadic data operation.
    FpaDyadicOp {
        Adf = "ADF",
        Muf = "MUF",
        Suf = "SUF",
        Rsf = "RSF",
        Dvf = "DVF",
        Rdf = "RDF",
        Pow = "POW",
        Rpw = "RPW",
        Rmf = "RMF",
        Fml = "FML",
        Fdv = "FDV",
        Frd = "FRD",
        Pol = "POL",
    }
}

operations! {
    /// An FPA monadic data operation.
    FpaMonadicOp {
        Mvf = "MVF",
        Mnf = "MNF",
        Abs = "ABS",
        Rnd = "RND",
        Sqt = "SQT",
        Log = "LOG",
        Lgn = "LGN",
        Exp = "EXP",
        Sin = "SIN",
        Cos = "COS",
        Tan = "TAN",
        Asn = "ASN",
        Acs = "ACS",
        Atn = "ATN",
        Urd = "URD",
        Nrm = "NRM",
    }
}

operations! {
    /// A transfer between a core register and the FPA.
    FpaRegisterOp {
        Flt = "FLT",
        Fix = "FIX",
        Wfs = "WFS",
        Rfs = "RFS",
    }
}

operations! {
    /// An FPA comparison.
    FpaCompareOp {
        Cmf = "CMF",
        Cnf = "CNF",
        Cmfe = "CMFE",
        Cnfe = "CNFE",
    }
}

operations! {
    /// An FPA load or store.
    FpaTransferOp {
        Ldf = "LDF",
        Stf = "STF",
    }
}

operations! {
    /// An FPA multiple load or store.
    FpaMultipleOp {
        Lfm = "LFM",
        Sfm = "SFM",
    }
}

/// An FPA data operation's last operand: a register, or one of the eight
/// constants 0, 1, 2, 3, 4, 5, 0.5 and 10 by index.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum FpaOperand {
    /// F0 to F7.
    Register(u8),
    /// A constant's index.
    Constant(u8),
}

/// An immediate shift, as a data-processing operand or a transfer's offset
/// applies it.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Shift {
    /// Left by 0 to 31; by 0 is no shift.
    Lsl(u8),
    /// Logical right by 1 to 32.
    Lsr(u8),
    /// Arithmetic right by 1 to 32.
    Asr(u8),
    /// Rotate right by 1 to 31.
    Ror(u8),
    /// Rotate right by one through the carry.
    Rrx,
}

impl Shift {
    /// No shift.
    pub const NONE: Self = Self::Lsl(0);

    /// The shift a two-bit type and five-bit amount encode.
    #[must_use]
    pub const fn decode(kind: u32, amount: u32) -> Self {
        let amount = (amount & 31).to_le_bytes()[0];
        match kind & 3 {
            0 => Self::Lsl(amount),
            1 => Self::Lsr(if amount == 0 { 32 } else { amount }),
            2 => Self::Asr(if amount == 0 { 32 } else { amount }),
            _ if amount == 0 => Self::Rrx,
            _ => Self::Ror(amount),
        }
    }

    /// The type and amount fields, or `None` for an amount the shift cannot
    /// encode.
    #[must_use]
    pub const fn encode(self) -> Option<(u32, u32)> {
        match self {
            Self::Lsl(n @ 0..=31) => Some((0, n as u32)),
            Self::Lsr(n @ 1..=32) => Some((1, n as u32 & 31)),
            Self::Asr(n @ 1..=32) => Some((2, n as u32 & 31)),
            Self::Ror(n @ 1..=31) => Some((3, n as u32)),
            Self::Rrx => Some((3, 0)),
            _ => None,
        }
    }
}

/// A shift by the bottom byte of a register.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum ShiftType {
    /// Left.
    Lsl,
    /// Logical right.
    Lsr,
    /// Arithmetic right.
    Asr,
    /// Rotate right.
    Ror,
}

impl ShiftType {
    /// The type a two-bit field holds.
    #[must_use]
    pub const fn field(bits: u32) -> Self {
        match bits & 3 {
            0 => Self::Lsl,
            1 => Self::Lsr,
            2 => Self::Asr,
            _ => Self::Ror,
        }
    }

    /// The field value.
    #[must_use]
    pub const fn bits(self) -> u32 {
        self as u32
    }

    /// The assembler's name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Lsl => "LSL",
            Self::Lsr => "LSR",
            Self::Asr => "ASR",
            Self::Ror => "ROR",
        }
    }
}

/// An 8-bit value rotated right by twice a 4-bit rotation: a
/// data-processing or `MSR` immediate.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Immediate {
    /// The 8-bit value.
    pub value: u8,
    /// Half the rotation, 0 to 15.
    pub rotation: u8,
}

impl Immediate {
    /// The value the immediate stands for.
    #[must_use]
    pub const fn value(self) -> u32 {
        (self.value as u32).rotate_right(2 * (self.rotation as u32 & 15))
    }

    /// The encoding of `value` with the least rotation, as assemblers choose
    /// it, or `None` when no rotation of an 8-bit value gives it.
    #[must_use]
    pub const fn encode(value: u32) -> Option<Self> {
        let mut rotation = 0;
        while rotation < 16 {
            let unrotated = value.rotate_left(2 * rotation);
            if unrotated <= 0xFF {
                return Some(Self {
                    value: unrotated.to_le_bytes()[0],
                    rotation: rotation.to_le_bytes()[0],
                });
            }
            rotation += 1;
        }
        None
    }
}

/// A T32 modified immediate: the 12-bit field that expands to a byte
/// repeated in a pattern, or a rotated byte with its top bit set.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Expanded(pub u16);

impl Expanded {
    /// The value the field expands to.
    #[must_use]
    pub const fn value(self) -> u32 {
        let field = self.0 as u32 & 0xFFF;
        let byte = field & 0xFF;
        if field >> 10 != 0 {
            return (0x80 | (field & 0x7F)).rotate_right(field >> 7);
        }
        match (field >> 8) & 3 {
            0 => byte,
            1 => (byte << 16) | byte,
            2 => (byte << 24) | (byte << 8),
            _ => byte * 0x0101_0101,
        }
    }

    /// The carry out, given the carry in: unchanged by a repeated byte, the
    /// value's top bit for a rotated one.
    #[must_use]
    pub const fn carry(self, carry: bool) -> bool {
        if self.0 & 0xC00 == 0 {
            carry
        } else {
            self.value() >> 31 != 0
        }
    }
}

/// A data-processing instruction's second operand.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Operand {
    /// A rotated immediate (A32).
    Immediate(Immediate),
    /// A modified immediate (T32).
    Expanded(Expanded),
    /// An immediate as written, unrotated (T32), which leaves the carry:
    /// twelve bits at most.
    Literal(u16),
    /// Rm shifted by an immediate.
    Register {
        /// The register.
        rm: Reg,
        /// The shift.
        shift: Shift,
    },
    /// Rm shifted by Rs's bottom byte.
    Shifted {
        /// The register shifted.
        rm: Reg,
        /// The shift.
        kind: ShiftType,
        /// The register holding the amount.
        rs: Reg,
    },
}

/// A single transfer's or preload's offset from its base.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Offset {
    /// An immediate: 12 bits for a word or byte, 8 for the rest.
    Immediate(u16),
    /// A register, shifted by an immediate for a word or byte.
    Register {
        /// The register.
        rm: Reg,
        /// The shift; [`Shift::NONE`] for the transfers that have none.
        shift: Shift,
    },
}

/// When a single transfer applies its offset.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Index {
    /// At the base plus the offset, which is not written back.
    Offset,
    /// At the base plus the offset, which is written back.
    PreIndexed,
    /// At the base, then the base plus the offset is written back.
    PostIndexed,
}

/// Where a block transfer's registers go relative to its base.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum BlockMode {
    /// From the base up.
    IncrementAfter,
    /// From the word above the base up.
    IncrementBefore,
    /// Down to the base.
    DecrementAfter,
    /// Down to the word below the base.
    DecrementBefore,
}

impl BlockMode {
    /// The mode the P and U bits select.
    #[must_use]
    pub const fn of(before: bool, up: bool) -> Self {
        match (before, up) {
            (false, true) => Self::IncrementAfter,
            (true, true) => Self::IncrementBefore,
            (false, false) => Self::DecrementAfter,
            (true, false) => Self::DecrementBefore,
        }
    }

    /// Whether the transfer begins one word past the base.
    #[must_use]
    pub const fn before(self) -> bool {
        matches!(self, Self::IncrementBefore | Self::DecrementBefore)
    }

    /// Whether the transfer runs upward from the base.
    #[must_use]
    pub const fn up(self) -> bool {
        matches!(self, Self::IncrementAfter | Self::IncrementBefore)
    }
}

/// How a coprocessor transfer addresses memory.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum CoIndex {
    /// At the base plus the offset, which is not written back.
    Offset,
    /// At the base plus the offset, which is written back.
    PreIndexed,
    /// At the base, then the base plus the offset is written back.
    PostIndexed,
    /// At the base; the immediate is an option for the coprocessor.
    Unindexed,
}

/// `MSR`'s source.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Source {
    /// A rotated immediate.
    Immediate(Immediate),
    /// A register.
    Register(Reg),
}

/// An operation and its operands.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Op {
    /// `AND` to `BIC`: Rd = Rn op operand.
    Data {
        /// The operation.
        op: DataOp,
        /// Set the flags.
        s: bool,
        /// The destination.
        rd: Reg,
        /// The first operand.
        rn: Reg,
        /// The second operand.
        operand: Operand,
    },
    /// `TST` to `CMN`.
    Compare {
        /// The operation.
        op: CompareOp,
        /// The field a 26-bit core's `P` forms set to R15, which a 32-bit
        /// mode wants zero.
        rd: Reg,
        /// The first operand.
        rn: Reg,
        /// The second operand.
        operand: Operand,
    },
    /// `MOV` and `MVN`.
    Move {
        /// The operation.
        op: MoveOp,
        /// Set the flags.
        s: bool,
        /// The destination.
        rd: Reg,
        /// The operand.
        operand: Operand,
    },
    /// `MOVW` and `MOVT`.
    Wide {
        /// The operation.
        op: WideOp,
        /// The destination.
        rd: Reg,
        /// The 16-bit value.
        value: u16,
    },
    /// The multiplies with a 32-bit result, the divides and the sums of
    /// differences.
    Multiply {
        /// The operation.
        op: MultiplyOp,
        /// Set the flags (`MUL` and `MLA`).
        s: bool,
        /// Rn's top half (`SMLA`, `SMUL`), the exchange (`SMLAD` to
        /// `SMUSD`) or the rounding (`SMMLA` to `SMMLS`).
        x: bool,
        /// Rm's top half (`SMLA`, `SMLAW`, `SMULW`, `SMUL`).
        y: bool,
        /// The destination.
        rd: Reg,
        /// The first operand.
        rn: Reg,
        /// The second operand.
        rm: Reg,
        /// The accumulator, for the operations that have one.
        ra: Option<Reg>,
    },
    /// The multiplies with a 64-bit result.
    MultiplyLong {
        /// The operation.
        op: LongOp,
        /// Set the flags (`UMULL` to `SMLAL`).
        s: bool,
        /// Rn's top half (the halfword `SMLAL`) or the exchange (`SMLALD`,
        /// `SMLSLD`).
        x: bool,
        /// Rm's top half (the halfword `SMLAL`).
        y: bool,
        /// The low word.
        lo: Reg,
        /// The high word.
        hi: Reg,
        /// The first operand.
        rn: Reg,
        /// The second operand.
        rm: Reg,
    },
    /// The parallel additions and subtractions.
    Parallel {
        /// How each lane's result is treated.
        prefix: Prefix,
        /// The lanes and their operations.
        lanes: Lanes,
        /// The destination.
        rd: Reg,
        /// The first operand.
        rn: Reg,
        /// The second operand.
        rm: Reg,
    },
    /// The three-register operations.
    Arithmetic {
        /// The operation.
        op: ArithmeticOp,
        /// The destination.
        rd: Reg,
        /// The first operand (the second for `QADD` to `QDSUB`).
        rn: Reg,
        /// The second operand (the first for `QADD` to `QDSUB`).
        rm: Reg,
    },
    /// The one-source operations.
    Unary {
        /// The operation.
        op: UnaryOp,
        /// The destination.
        rd: Reg,
        /// The source.
        rm: Reg,
    },
    /// The saturations.
    Saturate {
        /// The operation.
        op: SaturateOp,
        /// The destination.
        rd: Reg,
        /// The saturation field as encoded: the bit position less one for
        /// the signed forms, the position for the unsigned.
        saturate: u8,
        /// The source.
        rn: Reg,
        /// `LSL` or `ASR` for `SSAT` and `USAT`; none for the 16-bit forms.
        shift: Shift,
    },
    /// The extensions.
    Extend {
        /// The operation.
        op: ExtendOp,
        /// The destination.
        rd: Reg,
        /// The addend, for the forms that have one.
        rn: Option<Reg>,
        /// The source.
        rm: Reg,
        /// The source's rotation right, in bytes.
        rotation: u8,
    },
    /// The halfword packs.
    Pack {
        /// The operation.
        op: PackOp,
        /// The destination.
        rd: Reg,
        /// The first operand.
        rn: Reg,
        /// The second operand.
        rm: Reg,
        /// `LSL` for `PKHBT`, `ASR` for `PKHTB`.
        shift: Shift,
    },
    /// The bitfield operations.
    Bitfield {
        /// The operation.
        op: BitfieldOp,
        /// The destination.
        rd: Reg,
        /// The source, for the forms that have one.
        rn: Option<Reg>,
        /// The field's least significant bit.
        lsb: u8,
        /// For `BFI` and `BFC` the field's most significant bit; for `SBFX`
        /// and `UBFX` its width less one.
        top: u8,
    },
    /// `MRS`.
    Status {
        /// Read the SPSR rather than the CPSR.
        spsr: bool,
        /// The destination.
        rd: Reg,
    },
    /// `MSR`.
    StatusWrite {
        /// Write the SPSR rather than the CPSR.
        spsr: bool,
        /// The fields written: bit 0 control, 1 extension, 2 status, 3 flags.
        fields: u8,
        /// The value.
        source: Source,
    },
    /// `B`, `BL` and `BLX` by an offset.
    Branch {
        /// The operation.
        op: BranchOp,
        /// The target less the PC as the instruction reads it, which a
        /// `BLX` from T32 reads word-aligned.
        offset: i32,
    },
    /// `BX`, `BXJ` and `BLX` to a register.
    Exchange {
        /// The operation.
        op: ExchangeOp,
        /// The register holding the target.
        rm: Reg,
    },
    /// `SWI`, `BKPT` and `UDF`.
    Call {
        /// The operation.
        op: CallOp,
        /// The immediate: 24 bits for `SWI`, 16 for the others.
        value: u32,
    },
    /// The hints.
    Hint {
        /// The hint.
        op: HintOp,
        /// `DBG`'s option, or an unallocated hint's number.
        value: u8,
    },
    /// The barriers.
    Barrier {
        /// The barrier.
        op: BarrierOp,
        /// The option: the domain and access types it orders.
        option: u8,
    },
    /// `CLREX`.
    ClearExclusive,
    /// The single loads and stores.
    Transfer {
        /// The operation.
        op: TransferOp,
        /// The register transferred.
        rt: Reg,
        /// The base.
        rn: Reg,
        /// Add the offset rather than subtract it.
        up: bool,
        /// The offset.
        offset: Offset,
        /// When the offset applies: A32's unprivileged forms are
        /// post-indexed, T32's at an offset.
        index: Index,
    },
    /// `LDRD` and `STRD`.
    Dual {
        /// The operation.
        op: DualOp,
        /// The first register, at the lower address.
        rt: Reg,
        /// The second register: the next one in A32, any in T32.
        rt2: Reg,
        /// The base.
        rn: Reg,
        /// Add the offset rather than subtract it.
        up: bool,
        /// The offset.
        offset: Offset,
        /// When the offset applies.
        index: Index,
    },
    /// The preloads.
    Preload {
        /// The operation.
        op: PreloadOp,
        /// The base.
        rn: Reg,
        /// Add the offset rather than subtract it.
        up: bool,
        /// The offset.
        offset: Offset,
    },
    /// `LDM` and `STM`.
    Block {
        /// The operation.
        op: BlockOp,
        /// The base.
        rn: Reg,
        /// Where the registers go.
        mode: BlockMode,
        /// Write the final address back.
        writeback: bool,
        /// The S bit: the user bank, or with R15 loaded the SPSR restored.
        user: bool,
        /// The registers.
        list: RegList,
    },
    /// `SWP` and `SWPB`.
    Swap {
        /// The operation.
        op: SwapOp,
        /// The register loaded.
        rt: Reg,
        /// The register stored.
        rt2: Reg,
        /// The address.
        rn: Reg,
    },
    /// The exclusive, load-acquire and store-release transfers.
    Exclusive {
        /// The operation.
        op: ExclusiveOp,
        /// The status register, for the exclusive stores.
        rd: Option<Reg>,
        /// The register transferred.
        rt: Reg,
        /// The doubleword forms' second register: the next one in A32, any
        /// in T32.
        rt2: Option<Reg>,
        /// The base.
        rn: Reg,
        /// T32's word-scaled offset from the base, in bytes; zero in A32.
        offset: u16,
    },
    /// `CBZ` and `CBNZ` (T32).
    CompareBranch {
        /// Branch when the register is not zero.
        nonzero: bool,
        /// The register tested.
        rn: Reg,
        /// The forward distance from the PC.
        offset: u32,
    },
    /// `IT` (T32): up to four following instructions made conditional.
    IfThen {
        /// The first instruction's condition.
        firstcond: Condition,
        /// The block's length and its then-or-else pattern.
        mask: u8,
    },
    /// `TBB` and `TBH` (T32).
    TableBranch {
        /// Halfword entries rather than bytes.
        half: bool,
        /// The table's base.
        rn: Reg,
        /// The index.
        rm: Reg,
    },
    /// `CPS`.
    ChangeState {
        /// `0b10` enables and `0b11` disables the interrupts named; `0b00`
        /// leaves them.
        imod: u8,
        /// Change to `mode`.
        change_mode: bool,
        /// A, I and F, from bit 2 down.
        interrupts: u8,
        /// The mode changed to.
        mode: u8,
    },
    /// `SETEND`.
    SetEndian {
        /// Big-endian data.
        big: bool,
    },
    /// `SRS`.
    SaveState {
        /// Where the two words go relative to the mode's R13.
        mode: BlockMode,
        /// Write the final address back.
        writeback: bool,
        /// The mode whose R13 is the base.
        bank: u8,
    },
    /// `RFE`.
    ReturnFromException {
        /// The base.
        rn: Reg,
        /// Where the two words come from relative to the base.
        mode: BlockMode,
        /// Write the final address back.
        writeback: bool,
    },
    /// `CDP` and `CDP2`.
    Coprocessor {
        /// The operation.
        op: CoprocessorOp,
        /// The coprocessor.
        cp: u8,
        /// The first opcode.
        opc1: u8,
        /// The destination coprocessor register.
        crd: u8,
        /// The first source coprocessor register.
        crn: u8,
        /// The second source coprocessor register.
        crm: u8,
        /// The second opcode.
        opc2: u8,
    },
    /// `MCR`, `MRC`, `MCR2` and `MRC2`.
    CoprocessorRegister {
        /// The operation.
        op: CoRegisterOp,
        /// The coprocessor.
        cp: u8,
        /// The first opcode.
        opc1: u8,
        /// The core register.
        rt: Reg,
        /// The first coprocessor register.
        crn: u8,
        /// The second coprocessor register.
        crm: u8,
        /// The second opcode.
        opc2: u8,
    },
    /// `MCRR`, `MRRC`, `MCRR2` and `MRRC2`.
    CoprocessorRegisters {
        /// The operation.
        op: CoRegistersOp,
        /// The coprocessor.
        cp: u8,
        /// The opcode.
        opc1: u8,
        /// The first core register.
        rt: Reg,
        /// The second core register.
        rt2: Reg,
        /// The coprocessor register.
        crm: u8,
    },
    /// An FPA dyadic operation: `fd = fn op fm`.
    FpaDyadic {
        /// The operation.
        op: FpaDyadicOp,
        /// The result's precision.
        precision: Precision,
        /// The result's rounding.
        rounding: Rounding,
        /// The destination.
        fd: u8,
        /// The first operand.
        fn_: u8,
        /// The second operand.
        fm: FpaOperand,
    },
    /// An FPA monadic operation: `fd = op fm`.
    FpaMonadic {
        /// The operation.
        op: FpaMonadicOp,
        /// The result's precision.
        precision: Precision,
        /// The result's rounding.
        rounding: Rounding,
        /// The destination.
        fd: u8,
        /// The operand.
        fm: FpaOperand,
    },
    /// `FLT`, `FIX`, `WFS` and `RFS`.
    FpaRegister {
        /// The operation.
        op: FpaRegisterOp,
        /// The precision field: `FLT`'s result precision, which the others
        /// do not read.
        precision: Precision,
        /// `FLT`'s and `FIX`'s rounding.
        rounding: Rounding,
        /// `FLT`'s destination.
        fn_: u8,
        /// `FIX`'s operand.
        fm: FpaOperand,
        /// The core register.
        rd: Reg,
    },
    /// `CMF`, `CNF`, `CMFE` and `CNFE`: the flags to the PSR, or, where `rd`
    /// is not the PC, to `rd`.
    FpaCompare {
        /// The operation.
        op: FpaCompareOp,
        /// The precision field, which a comparison does not read.
        precision: Precision,
        /// The first operand.
        fn_: u8,
        /// The second operand.
        fm: FpaOperand,
        /// Where the flags go.
        rd: Reg,
    },
    /// `LDF` and `STF`.
    FpaTransfer {
        /// The operation.
        op: FpaTransferOp,
        /// The memory format.
        format: Format,
        /// The register.
        fd: u8,
        /// The base.
        rn: Reg,
        /// Add the offset rather than subtract it.
        up: bool,
        /// How memory is addressed; unindexed, at the base without
        /// writeback.
        index: CoIndex,
        /// The offset in words.
        offset: u8,
    },
    /// `LFM` and `SFM`.
    FpaMultiple {
        /// The operation.
        op: FpaMultipleOp,
        /// The first register; the rest follow it, wrapping past F7.
        fd: u8,
        /// How many, from 1 to 4.
        count: u8,
        /// The base.
        rn: Reg,
        /// Add the offset rather than subtract it.
        up: bool,
        /// How memory is addressed; unindexed, at the base without
        /// writeback.
        index: CoIndex,
        /// The offset in words.
        offset: u8,
    },
    /// `LDC`, `STC`, `LDC2` and `STC2`.
    CoprocessorTransfer {
        /// The operation.
        op: CoTransferOp,
        /// The coprocessor.
        cp: u8,
        /// The coprocessor register.
        crd: u8,
        /// The base.
        rn: Reg,
        /// The long form.
        long: bool,
        /// Add the offset rather than subtract it.
        up: bool,
        /// How memory is addressed.
        index: CoIndex,
        /// The offset in words, or the unindexed form's option.
        offset: u8,
    },
    /// A floating-point or Advanced SIMD instruction.
    SimdFp(SimdFp),
}
