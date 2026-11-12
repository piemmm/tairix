//! The 8087's instructions: the decode of an ESC's eleven opcode bits, and
//! each instruction's effect on the registers, the stack and memory, with
//! the exception precedence and the masked and unmasked responses of
//! Intel's data sheet and the 80287 programmer's reference.

use core::cmp::Ordering;

use super::arith::{self, flag, Exact, Mode, Parts, Rounding};
use super::real::{Binary, Class, Real, BIAS, INTEGER};
use super::{tag, timing, trans, Registers, Store, IEM, STATE_BYTES};

/// The size and kind of a memory operand.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Format {
    /// Short real.
    Single,
    /// Long real.
    Double,
    /// Temporary real.
    Extended,
    /// Word integer.
    Int16,
    /// Short integer.
    Int32,
    /// Long integer.
    Int64,
    /// Packed decimal.
    Bcd,
    /// A control or status word.
    Word,
    /// The 14-byte environment.
    Environment,
    /// The 94-byte state.
    State,
}

impl Format {
    /// The bytes it takes.
    pub(crate) const fn bytes(self) -> usize {
        match self {
            Self::Int16 | Self::Word => 2,
            Self::Single | Self::Int32 => 4,
            Self::Double | Self::Int64 => 8,
            Self::Extended | Self::Bcd => 10,
            Self::Environment => 14,
            Self::State => STATE_BYTES,
        }
    }

    const fn integer(self) -> bool {
        matches!(self, Self::Int16 | Self::Int32 | Self::Int64)
    }
}

/// The two-operand operations.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Arith {
    /// FADD.
    Add,
    /// FMUL.
    Mul,
    /// FCOM, popping as many times as it says.
    Compare(u8),
    /// FSUB, reversed for FSUBR.
    Sub(bool),
    /// FDIV, reversed for FDIVR.
    Div(bool),
}

/// The operation an ESC's `reg` field names in the arithmetic groups;
/// `swap` reverses the sense of subtraction and division, as the opcode's
/// direction bit does.
const fn arith(reg: u8, swap: bool) -> Arith {
    match reg {
        0 => Arith::Add,
        1 => Arith::Mul,
        2 => Arith::Compare(0),
        3 => Arith::Compare(1),
        4 => Arith::Sub(swap),
        5 => Arith::Sub(!swap),
        6 => Arith::Div(swap),
        _ => Arith::Div(!swap),
    }
}

/// The constants FLD1 to FLDZ push: the 8087's truncated ones.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Constant {
    /// 1.
    One,
    /// log2 10.
    Log2Ten,
    /// log2 e.
    Log2E,
    /// π.
    Pi,
    /// log10 2.
    Log10Two,
    /// ln 2.
    LnTwo,
    /// +0.
    Zero,
}

impl Constant {
    /// The value, its significand truncated to 64 bits whatever the
    /// rounding mode, as the 8087 loads it.
    const fn value(self) -> Real {
        match self {
            Self::One => Real::ONE,
            Self::Log2Ten => Real::new(false, 0x4000, 0xD49A_784B_CD1B_8AFE),
            Self::Log2E => Real::new(false, 0x3FFF, 0xB8AA_3B29_5C17_F0BB),
            Self::Pi => Real::new(false, 0x4000, 0xC90F_DAA2_2168_C234),
            Self::Log10Two => Real::new(false, 0x3FFD, 0x9A20_9A84_FBCF_F798),
            Self::LnTwo => Real::new(false, 0x3FFE, 0xB172_17F7_D1CF_79AB),
            Self::Zero => Real::ZERO,
        }
    }
}

/// One instruction, decoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Insn {
    /// An arithmetic or compare with ST(0) and a memory operand.
    ArithMemory(Arith, Format),
    /// An arithmetic or compare with ST(0) and ST(i): into ST(i) where
    /// `into`, then popped where `pop`.
    ArithRegister {
        op: Arith,
        i: u8,
        into: bool,
        pop: bool,
    },
    /// FCOMPP.
    ComparePop2,
    /// FLD, FILD or FBLD from memory.
    Load(Format),
    /// FLD ST(i).
    LoadRegister(u8),
    /// FST, FSTP, FIST, FISTP or FBSTP to memory.
    Store(Format, bool),
    /// FST or FSTP ST(i).
    StoreRegister(u8, bool),
    /// FXCH ST(i).
    Exchange(u8),
    /// FFREE ST(i), then popped where it says.
    Free(u8, bool),
    /// FLDZ, FLD1 and the others.
    LoadConstant(Constant),
    /// FCHS.
    ChangeSign,
    /// FABS.
    Absolute,
    /// FTST.
    Test,
    /// FXAM.
    Examine,
    /// F2XM1.
    TwoPowerMinusOne,
    /// FYL2X.
    YLog2X,
    /// FPTAN.
    PartialTangent,
    /// FPATAN.
    PartialArctangent,
    /// FXTRACT.
    Extract,
    /// FDECSTP.
    DecrementTop,
    /// FINCSTP.
    IncrementTop,
    /// FPREM.
    PartialRemainder,
    /// FYL2XP1.
    YLog2XPlus1,
    /// FSQRT.
    SquareRoot,
    /// FRNDINT.
    RoundToInteger,
    /// FSCALE.
    Scale,
    /// FNOP, and every encoding the 8087 reserves.
    Nop,
    /// FLDCW.
    LoadControl,
    /// FLDENV.
    LoadEnvironment,
    /// FSTENV.
    StoreEnvironment,
    /// FRSTOR.
    Restore,
    /// FSAVE.
    Save,
    /// FENI.
    EnableInterrupts,
    /// FDISI.
    DisableInterrupts,
    /// FCLEX.
    ClearExceptions,
    /// FINIT.
    Initialise,
    /// FSTCW.
    StoreControl,
    /// FSTSW.
    StoreStatus,
}

/// The instruction an ESC's eleven opcode bits name: the ESC's low three,
/// then the `ModRM` byte; `memory` where it has a memory operand.
pub(crate) fn decode(opcode: u16, memory: bool) -> Insn {
    let group = crate::bits::hi(opcode) & 7;
    let modrm = crate::bits::lo(opcode);
    let reg = (modrm >> 3) & 7;
    let i = modrm & 7;
    if memory {
        decode_memory(group, reg)
    } else {
        decode_register(group, reg, i)
    }
}

fn decode_memory(group: u8, reg: u8) -> Insn {
    use Format::{Bcd, Double, Extended, Int16, Int32, Int64, Single};
    match (group, reg) {
        (0, _) => Insn::ArithMemory(arith(reg, false), Single),
        (2, _) => Insn::ArithMemory(arith(reg, false), Int32),
        (4, _) => Insn::ArithMemory(arith(reg, false), Double),
        (6, _) => Insn::ArithMemory(arith(reg, false), Int16),
        (1, 0) => Insn::Load(Single),
        (1, 2 | 3) => Insn::Store(Single, reg == 3),
        (1, 4) => Insn::LoadEnvironment,
        (1, 5) => Insn::LoadControl,
        (1, 6) => Insn::StoreEnvironment,
        (1, 7) => Insn::StoreControl,
        (3, 0) => Insn::Load(Int32),
        (3, 2 | 3) => Insn::Store(Int32, reg == 3),
        (3, 5) => Insn::Load(Extended),
        (3, 7) => Insn::Store(Extended, true),
        (5, 0) => Insn::Load(Double),
        (5, 2 | 3) => Insn::Store(Double, reg == 3),
        (5, 4) => Insn::Restore,
        (5, 6) => Insn::Save,
        (5, 7) => Insn::StoreStatus,
        (7, 0) => Insn::Load(Int16),
        (7, 2 | 3) => Insn::Store(Int16, reg == 3),
        (7, 4) => Insn::Load(Bcd),
        (7, 5) => Insn::Load(Int64),
        (7, 6) => Insn::Store(Bcd, true),
        (7, 7) => Insn::Store(Int64, true),
        _ => Insn::Nop,
    }
}

/// The register forms. An encoding the 8087 reserves acts as the 80387's
/// undocumented alias of it where there is one, else as FNOP.
fn decode_register(group: u8, reg: u8, i: u8) -> Insn {
    match (group, reg) {
        (0, _) => Insn::ArithRegister {
            op: arith(reg, false),
            i,
            into: false,
            pop: false,
        },
        (4 | 6, 2) => Insn::ArithRegister {
            op: Arith::Compare(u8::from(group == 6)),
            i,
            into: false,
            pop: false,
        },
        (4, 3) => Insn::ArithRegister {
            op: Arith::Compare(1),
            i,
            into: false,
            pop: false,
        },
        (6, 3) if i == 1 => Insn::ComparePop2,
        (4 | 6, _) if reg != 3 => Insn::ArithRegister {
            op: arith(reg, true),
            i,
            into: true,
            pop: group == 6,
        },
        (1, 0) => Insn::LoadRegister(i),
        (1 | 5 | 7, 1) => Insn::Exchange(i),
        (1, 2) if i == 0 => Insn::Nop,
        (1 | 5, 3) | (7, 2 | 3) => Insn::StoreRegister(i, true),
        (5, 2) => Insn::StoreRegister(i, false),
        (5, 0) => Insn::Free(i, false),
        (7, 0) => Insn::Free(i, true),
        (1, 4..=7) => decode_d9(reg, i),
        (3, 4) => match i {
            0 => Insn::EnableInterrupts,
            1 => Insn::DisableInterrupts,
            2 => Insn::ClearExceptions,
            3 => Insn::Initialise,
            _ => Insn::Nop,
        },
        _ => Insn::Nop,
    }
}

/// `D9 E0`–`D9 FF`.
fn decode_d9(reg: u8, i: u8) -> Insn {
    match (reg, i) {
        (4, 0) => Insn::ChangeSign,
        (4, 1) => Insn::Absolute,
        (4, 4) => Insn::Test,
        (4, 5) => Insn::Examine,
        (5, 0) => Insn::LoadConstant(Constant::One),
        (5, 1) => Insn::LoadConstant(Constant::Log2Ten),
        (5, 2) => Insn::LoadConstant(Constant::Log2E),
        (5, 3) => Insn::LoadConstant(Constant::Pi),
        (5, 4) => Insn::LoadConstant(Constant::Log10Two),
        (5, 5) => Insn::LoadConstant(Constant::LnTwo),
        (5, 6) => Insn::LoadConstant(Constant::Zero),
        (6, 0) => Insn::TwoPowerMinusOne,
        (6, 1) => Insn::YLog2X,
        (6, 2) => Insn::PartialTangent,
        (6, 3) => Insn::PartialArctangent,
        (6, 4) => Insn::Extract,
        (6, 6) => Insn::DecrementTop,
        (6, 7) => Insn::IncrementTop,
        (7, 0) => Insn::PartialRemainder,
        (7, 1) => Insn::YLog2XPlus1,
        (7, 2) => Insn::SquareRoot,
        (7, 4) => Insn::RoundToInteger,
        (7, 5) => Insn::Scale,
        _ => Insn::Nop,
    }
}

impl Insn {
    /// Whether the control unit runs it at once, holding no BUSY.
    pub(crate) const fn control_unit(self) -> bool {
        matches!(
            self,
            Self::EnableInterrupts
                | Self::DisableInterrupts
                | Self::ClearExceptions
                | Self::Initialise
                | Self::StoreControl
                | Self::StoreStatus
        )
    }

    /// Whether it records the instruction and operand pointers, as every
    /// instruction but the control group does.
    pub(crate) const fn numeric(self) -> bool {
        !matches!(
            self,
            Self::LoadControl
                | Self::LoadEnvironment
                | Self::StoreEnvironment
                | Self::Restore
                | Self::Save
        ) && !self.control_unit()
    }

    /// The memory operand it reads, if it reads one.
    pub(crate) const fn loads(self) -> Option<Format> {
        match self {
            Self::ArithMemory(_, format) | Self::Load(format) => Some(format),
            Self::LoadControl => Some(Format::Word),
            Self::LoadEnvironment => Some(Format::Environment),
            Self::Restore => Some(Format::State),
            _ => None,
        }
    }
}

/// An instruction's operand as an assembler writes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Operand {
    /// None.
    None,
    /// A memory operand, its size as `PTR` names it; empty for the
    /// environment and the state.
    Memory(&'static str),
    /// `ST(i)`.
    Register(u8),
    /// `ST,ST(i)`.
    ToTop(u8),
    /// `ST(i),ST`.
    FromTop(u8),
}

impl Format {
    /// Its size as an assembler's `PTR` names it.
    const fn size(self) -> &'static str {
        match self {
            Self::Int16 | Self::Word => "WORD PTR",
            Self::Single | Self::Int32 => "DWORD PTR",
            Self::Double | Self::Int64 => "QWORD PTR",
            Self::Extended | Self::Bcd => "TBYTE PTR",
            Self::Environment | Self::State => "",
        }
    }
}

const fn arith_name(op: Arith, integer: bool, pop: bool) -> &'static str {
    match (op, integer, pop) {
        (Arith::Add, false, false) => "FADD",
        (Arith::Add, false, true) => "FADDP",
        (Arith::Add, true, _) => "FIADD",
        (Arith::Mul, false, false) => "FMUL",
        (Arith::Mul, false, true) => "FMULP",
        (Arith::Mul, true, _) => "FIMUL",
        (Arith::Compare(0), false, _) => "FCOM",
        (Arith::Compare(_), false, _) => "FCOMP",
        (Arith::Compare(0), true, _) => "FICOM",
        (Arith::Compare(_), true, _) => "FICOMP",
        (Arith::Sub(false), false, false) => "FSUB",
        (Arith::Sub(false), false, true) => "FSUBP",
        (Arith::Sub(false), true, _) => "FISUB",
        (Arith::Sub(true), false, false) => "FSUBR",
        (Arith::Sub(true), false, true) => "FSUBRP",
        (Arith::Sub(true), true, _) => "FISUBR",
        (Arith::Div(false), false, false) => "FDIV",
        (Arith::Div(false), false, true) => "FDIVP",
        (Arith::Div(false), true, _) => "FIDIV",
        (Arith::Div(true), false, false) => "FDIVR",
        (Arith::Div(true), false, true) => "FDIVRP",
        (Arith::Div(true), true, _) => "FIDIVR",
    }
}

const fn constant_name(c: Constant) -> &'static str {
    match c {
        Constant::One => "FLD1",
        Constant::Log2Ten => "FLDL2T",
        Constant::Log2E => "FLDL2E",
        Constant::Pi => "FLDPI",
        Constant::Log10Two => "FLDLG2",
        Constant::LnTwo => "FLDLN2",
        Constant::Zero => "FLDZ",
    }
}

/// The mnemonic an assembler writes `insn` with, and its operand: the
/// control unit's instructions by their no-wait names, as the bytes are.
pub(crate) const fn mnemonic(insn: Insn) -> (&'static str, Operand) {
    match insn {
        Insn::ArithMemory(op, format) => (
            arith_name(op, format.integer(), false),
            Operand::Memory(format.size()),
        ),
        Insn::ArithRegister { op, i, into, pop } => {
            let operand = match (op, into) {
                (Arith::Compare(_), _) => Operand::Register(i),
                (_, false) => Operand::ToTop(i),
                (_, true) => Operand::FromTop(i),
            };
            (arith_name(op, false, pop), operand)
        }
        Insn::ComparePop2 => ("FCOMPP", Operand::None),
        Insn::Load(format) => {
            let name = match format {
                Format::Int16 | Format::Int32 | Format::Int64 => "FILD",
                Format::Bcd => "FBLD",
                _ => "FLD",
            };
            (name, Operand::Memory(format.size()))
        }
        Insn::LoadRegister(i) => ("FLD", Operand::Register(i)),
        Insn::Store(format, pop) => {
            let name = match (format, pop) {
                (Format::Int16 | Format::Int32 | Format::Int64, false) => "FIST",
                (Format::Int16 | Format::Int32 | Format::Int64, true) => "FISTP",
                (Format::Bcd, _) => "FBSTP",
                (_, false) => "FST",
                (_, true) => "FSTP",
            };
            (name, Operand::Memory(format.size()))
        }
        Insn::StoreRegister(i, pop) => (if pop { "FSTP" } else { "FST" }, Operand::Register(i)),
        Insn::Exchange(i) => ("FXCH", Operand::Register(i)),
        Insn::Free(i, pop) => (if pop { "FFREEP" } else { "FFREE" }, Operand::Register(i)),
        Insn::LoadConstant(c) => (constant_name(c), Operand::None),
        Insn::LoadControl => ("FLDCW", Operand::Memory("WORD PTR")),
        Insn::LoadEnvironment => ("FLDENV", Operand::Memory("")),
        Insn::StoreEnvironment => ("FNSTENV", Operand::Memory("")),
        Insn::Restore => ("FRSTOR", Operand::Memory("")),
        Insn::Save => ("FNSAVE", Operand::Memory("")),
        Insn::StoreControl => ("FNSTCW", Operand::Memory("WORD PTR")),
        Insn::StoreStatus => ("FNSTSW", Operand::Memory("WORD PTR")),
        other => (bare_name(other), Operand::None),
    }
}

/// The names of the instructions that take no operand.
const fn bare_name(insn: Insn) -> &'static str {
    match insn {
        Insn::ChangeSign => "FCHS",
        Insn::Absolute => "FABS",
        Insn::Test => "FTST",
        Insn::Examine => "FXAM",
        Insn::TwoPowerMinusOne => "F2XM1",
        Insn::YLog2X => "FYL2X",
        Insn::PartialTangent => "FPTAN",
        Insn::PartialArctangent => "FPATAN",
        Insn::Extract => "FXTRACT",
        Insn::DecrementTop => "FDECSTP",
        Insn::IncrementTop => "FINCSTP",
        Insn::PartialRemainder => "FPREM",
        Insn::YLog2XPlus1 => "FYL2XP1",
        Insn::SquareRoot => "FSQRT",
        Insn::RoundToInteger => "FRNDINT",
        Insn::Scale => "FSCALE",
        Insn::EnableInterrupts => "FNENI",
        Insn::DisableInterrupts => "FNDISI",
        Insn::ClearExceptions => "FNCLEX",
        Insn::Initialise => "FNINIT",
        _ => "FNOP",
    }
}

/// What an instruction did: how long it took, the flags it raised and what
/// it stores.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Done {
    /// Clocks from its issue to its end.
    pub clocks: u32,
    /// The exception flags it raised.
    pub raised: u16,
    /// What it stores, and where.
    pub store: Option<Store>,
}

/// A value as an instruction reads it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Value {
    /// An empty register.
    Empty,
    /// A true zero, or a register tagged zero.
    Zero(bool),
    /// A finite nonzero value, or a pseudo-zero.
    Finite(Parts, Kind),
    /// An infinity.
    Infinity(bool),
    /// A NaN.
    Nan(Real),
}

/// What kind of finite value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Normal,
    Unnormal,
    PseudoZero,
    /// A denormal, read as the unnormal it equals.
    Denormal,
}

impl Value {
    /// The value of `x` by its bits.
    fn of(x: Real) -> Self {
        match x.class() {
            Class::Zero => Self::Zero(x.negative()),
            Class::Normal => Self::Finite(Parts::of(x), Kind::Normal),
            Class::Unnormal => Self::Finite(Parts::of(x), Kind::Unnormal),
            Class::PseudoZero => Self::Finite(Parts::of(x), Kind::PseudoZero),
            Class::Denormal => Self::Finite(Parts::of(x), Kind::Denormal),
            Class::Infinity => Self::Infinity(x.negative()),
            Class::Nan => Self::Nan(x),
        }
    }

    const fn denormal(self) -> bool {
        matches!(self, Self::Finite(_, Kind::Denormal))
    }

    const fn negative(self) -> bool {
        match self {
            Self::Zero(negative) | Self::Infinity(negative) => negative,
            Self::Finite(p, _) => p.negative,
            Self::Nan(x) => x.negative(),
            Self::Empty => false,
        }
    }
}

/// The two-bit tag a value written to a register takes.
const fn tag_of(x: Real) -> u16 {
    match x.class() {
        Class::Zero => tag::ZERO,
        Class::Normal | Class::Unnormal | Class::PseudoZero => tag::VALID,
        Class::Denormal | Class::Infinity | Class::Nan => tag::SPECIAL,
    }
}

impl Registers {
    fn empty(&self, i: u8) -> bool {
        self.tag(self.physical(i)) == tag::EMPTY
    }

    /// ST(i) as an instruction reads it, its tag honoured over its bits.
    fn value(&self, i: u8) -> Value {
        let physical = self.physical(i);
        let x = self.data[physical];
        match self.tag(physical) {
            tag::EMPTY => Value::Empty,
            tag::ZERO => Value::Zero(x.negative()),
            _ => Value::of(x),
        }
    }

    fn put(&mut self, i: u8, x: Real) {
        let physical = self.physical(i);
        self.data[physical] = x;
        self.set_tag(physical, tag_of(x));
    }

    fn push(&mut self, x: Real) {
        self.set_top(self.top().wrapping_sub(1));
        self.put(0, x);
    }

    fn pop(&mut self) {
        let physical = self.physical(0);
        self.set_tag(physical, tag::EMPTY);
        self.set_top(self.top().wrapping_add(1));
    }

    fn masks(&self) -> u16 {
        self.control & flag::ALL
    }

    fn mode(&self) -> Mode {
        Mode::of(self.control >> 10)
    }

    /// The rounding add, subtract, multiply, divide and square root take.
    fn rounding(&self) -> Rounding {
        let bits = match (self.control >> 8) & 3 {
            0 => 24,
            2 => 53,
            _ => 64,
        };
        Rounding {
            mode: self.mode(),
            bits,
        }
    }

    fn affine(&self) -> bool {
        self.control & 0x1000 != 0
    }

    fn masked(&self, flags: u16) -> bool {
        self.masks() & flags == flags
    }

    /// Sets the condition codes `mask` covers to `codes`.
    fn set_codes(&mut self, mask: u16, codes: u16) {
        self.status = (self.status & !mask) | (codes & mask);
    }
}

/// The condition codes' bits.
mod cc {
    pub const C0: u16 = 0x0100;
    pub const C1: u16 = 0x0200;
    pub const C2: u16 = 0x0400;
    pub const C3: u16 = 0x4000;
    pub const COMPARE: u16 = C3 | C2 | C0;
    pub const ALL: u16 = C3 | C2 | C1 | C0;
}

/// A result an instruction gives its destination, or none where an
/// unmasked exception leaves it; and the flags raised.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Outcome {
    value: Option<Real>,
    flags: u16,
}

impl Outcome {
    const fn value(value: Real, flags: u16) -> Self {
        Self {
            value: Some(value),
            flags,
        }
    }

    /// Exception `flags`, which abandon the instruction where unmasked and
    /// give the indefinite where masked.
    fn invalid(regs: &Registers, flags: u16) -> Self {
        Self {
            value: regs.masked(flags).then_some(Real::INDEFINITE),
            flags,
        }
    }

    fn rounded(rounded: arith::Rounded, flags: u16) -> Self {
        Self {
            value: rounded.value,
            flags: rounded.flags | flags,
        }
    }
}

/// The NaN a masked invalid operation on `a` and `b` answers: the one with
/// the larger significand.
fn nan_of(a: Value, b: Value) -> Option<Real> {
    match (a, b) {
        (Value::Nan(x), Value::Nan(y)) => Some(if (y.sig & !INTEGER) > (x.sig & !INTEGER) {
            y
        } else {
            x
        }),
        (Value::Nan(x), _) | (_, Value::Nan(x)) => Some(x),
        _ => None,
    }
}

/// The checks a two-operand instruction makes first: an unmasked denormal
/// where `denormal` finds one by the instruction's own rule, then an empty
/// register, then a NaN. The flags so far, or the outcome they decided.
fn screen(regs: &Registers, a: Value, b: Value, denormal: bool) -> Result<u16, Outcome> {
    let denormal = if denormal { flag::DENORMAL } else { 0 };
    if denormal != 0 && !regs.masked(flag::DENORMAL) {
        return Err(Outcome {
            value: None,
            flags: denormal,
        });
    }
    if a == Value::Empty || b == Value::Empty {
        return Err(Outcome::invalid(regs, flag::INVALID | denormal));
    }
    if let Some(nan) = nan_of(a, b) {
        let flags = flag::INVALID | denormal;
        return Err(Outcome {
            value: regs.masked(flag::INVALID).then_some(nan),
            flags,
        });
    }
    Ok(denormal)
}

/// The sign a sum exactly zero takes: the operands' where they agree, else
/// plus, but minus rounding down.
fn zero_sum_sign(regs: &Registers, a: bool, b: bool) -> bool {
    if a == b {
        a
    } else {
        regs.mode() == Mode::Down
    }
}

fn add(regs: &Registers, a: Value, b: Value, subtract: bool) -> Outcome {
    let flags = match screen(regs, a, b, a.denormal() || b.denormal()) {
        Ok(flags) => flags,
        Err(outcome) => return outcome,
    };
    let b_negative = b.negative() != subtract;
    match (a, b) {
        (Value::Infinity(x), Value::Infinity(_)) => {
            if regs.affine() && x == b_negative {
                Outcome::value(Real::INFINITY.with_sign(x), flags)
            } else {
                Outcome::invalid(regs, flags | flag::INVALID)
            }
        }
        (Value::Infinity(x), _) => Outcome::value(Real::INFINITY.with_sign(x), flags),
        (_, Value::Infinity(_)) => Outcome::value(Real::INFINITY.with_sign(b_negative), flags),
        (Value::Zero(x), Value::Zero(_)) => Outcome::value(
            Real::ZERO.with_sign(zero_sum_sign(regs, x, b_negative)),
            flags,
        ),
        (Value::Zero(_), Value::Finite(p, _)) => {
            let p = Parts {
                negative: b_negative,
                ..p
            };
            Outcome::rounded(
                arith::round(Exact::of(p), regs.rounding(), regs.masks()),
                flags,
            )
        }
        (Value::Finite(p, _), Value::Zero(_)) => Outcome::rounded(
            arith::round(Exact::of(p), regs.rounding(), regs.masks()),
            flags,
        ),
        (Value::Finite(p, _), Value::Finite(q, _)) => {
            let sum = arith::add(p, q, subtract);
            if sum.is_zero() {
                let sign = zero_sum_sign(regs, p.negative, b_negative);
                return Outcome::value(Real::ZERO.with_sign(sign), flags);
            }
            Outcome::rounded(arith::round(sum, regs.rounding(), regs.masks()), flags)
        }
        _ => Outcome::invalid(regs, flags | flag::INVALID),
    }
}

fn multiply(regs: &Registers, a: Value, b: Value) -> Outcome {
    let flags = match screen(regs, a, b, a.denormal() || b.denormal()) {
        Ok(flags) => flags,
        Err(outcome) => return outcome,
    };
    let negative = a.negative() != b.negative();
    match (a, b) {
        (Value::Infinity(_), Value::Zero(_)) | (Value::Zero(_), Value::Infinity(_)) => {
            Outcome::invalid(regs, flags | flag::INVALID)
        }
        (Value::Infinity(_), _) | (_, Value::Infinity(_)) => {
            Outcome::value(Real::INFINITY.with_sign(negative), flags)
        }
        (Value::Zero(_), _) | (_, Value::Zero(_)) => {
            Outcome::value(Real::ZERO.with_sign(negative), flags)
        }
        (Value::Finite(p, _), Value::Finite(q, _)) => {
            let product = arith::multiply(p, q);
            if product.is_zero() {
                return Outcome::value(Real::ZERO.with_sign(negative), flags);
            }
            Outcome::rounded(arith::round(product, regs.rounding(), regs.masks()), flags)
        }
        _ => Outcome::invalid(regs, flags | flag::INVALID),
    }
}

fn divide(regs: &Registers, a: Value, b: Value) -> Outcome {
    // A denormal divisor is an invalid operation, not a denormal one.
    let flags = match screen(regs, a, b, a.denormal()) {
        Ok(flags) => flags,
        Err(outcome) => return outcome,
    };
    let negative = a.negative() != b.negative();
    match (a, b) {
        // Infinity over infinity, zero over zero, and anything finite over
        // a divisor that is not normal.
        (Value::Infinity(_), Value::Infinity(_))
        | (Value::Zero(_), Value::Zero(_))
        | (
            Value::Zero(_) | Value::Finite(..),
            Value::Finite(_, Kind::Unnormal | Kind::PseudoZero | Kind::Denormal),
        ) => Outcome::invalid(regs, flags | flag::INVALID),
        (Value::Infinity(_), _) => Outcome::value(Real::INFINITY.with_sign(negative), flags),
        (_, Value::Infinity(_)) | (Value::Zero(_), _) => {
            Outcome::value(Real::ZERO.with_sign(negative), flags)
        }
        (_, Value::Zero(_)) => {
            let flags = flags | flag::ZERO_DIVIDE;
            Outcome {
                value: regs
                    .masked(flag::ZERO_DIVIDE)
                    .then_some(Real::INFINITY.with_sign(negative)),
                flags,
            }
        }
        (Value::Finite(p, _), Value::Finite(q, _)) => {
            let quotient = arith::divide(p, q);
            if quotient.is_zero() {
                return Outcome::value(Real::ZERO.with_sign(negative), flags);
            }
            Outcome::rounded(arith::round(quotient, regs.rounding(), regs.masks()), flags)
        }
        _ => Outcome::invalid(regs, flags | flag::INVALID),
    }
}

/// The two-operand arithmetic `op` of `a`, the operand on the left, and `b`.
fn arithmetic(regs: &Registers, op: Arith, a: Value, b: Value) -> Outcome {
    match op {
        Arith::Add => add(regs, a, b, false),
        Arith::Sub(false) => add(regs, a, b, true),
        Arith::Sub(true) => add(regs, b, a, true),
        Arith::Mul => multiply(regs, a, b),
        Arith::Div(false) => divide(regs, a, b),
        Arith::Div(true) => divide(regs, b, a),
        Arith::Compare(_) => Outcome {
            value: None,
            flags: 0,
        },
    }
}

/// How `a` compares with `b`: the condition codes and the flags.
fn compare(regs: &Registers, a: Value, b: Value) -> (Option<u16>, u16) {
    const UNORDERED: u16 = cc::COMPARE;
    let flags = match screen(regs, a, b, a.denormal() || b.denormal()) {
        Ok(flags) => flags,
        Err(outcome) => {
            let unordered = outcome.flags & flag::INVALID != 0 && regs.masked(flag::INVALID);
            return (unordered.then_some(UNORDERED), outcome.flags);
        }
    };
    let order = match (a, b) {
        (Value::Infinity(x), Value::Infinity(y)) => {
            if regs.affine() {
                Some(y.cmp(&x))
            } else {
                Some(Ordering::Equal)
            }
        }
        (Value::Infinity(x), _) => {
            regs.affine()
                .then_some(if x { Ordering::Less } else { Ordering::Greater })
        }
        (_, Value::Infinity(y)) => {
            regs.affine()
                .then_some(if y { Ordering::Greater } else { Ordering::Less })
        }
        _ => Some(finite_parts(a).cmp_value(finite_parts(b))),
    };
    match order {
        Some(Ordering::Greater) => (Some(0), flags),
        Some(Ordering::Less) => (Some(cc::C0), flags),
        Some(Ordering::Equal) => (Some(cc::C3), flags),
        None => {
            let flags = flags | flag::INVALID;
            (regs.masked(flag::INVALID).then_some(UNORDERED), flags)
        }
    }
}

/// A zero or finite value's parts, a zero's significand zero.
fn finite_parts(v: Value) -> Parts {
    match v {
        Value::Finite(p, _) => p,
        other => Parts {
            negative: other.negative(),
            exp: 0,
            sig: 0,
        },
    }
}

/// The value memory operand `bytes` of `format` holds, and whether it was a
/// denormal of a binary format.
fn load_value(format: Format, bytes: &[u8]) -> Value {
    let mut raw = [0u8; 10];
    let n = bytes.len().min(10);
    raw[..n].copy_from_slice(&bytes[..n]);
    let word = u16::from_le_bytes([raw[0], raw[1]]);
    let int32 = u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]);
    let int64 = u64::from_le_bytes([
        raw[0], raw[1], raw[2], raw[3], raw[4], raw[5], raw[6], raw[7],
    ]);
    match format {
        Format::Single | Format::Double => {
            let (x, denormal) = if format == Format::Single {
                Real::from_single(int32)
            } else {
                Real::from_double(int64)
            };
            if denormal {
                Value::Finite(Parts::of(x), Kind::Denormal)
            } else {
                Value::of(x)
            }
        }
        Format::Int16 => Value::of(Real::from_int(i64::from(word.cast_signed()))),
        Format::Int32 => Value::of(Real::from_int(i64::from(int32.cast_signed()))),
        Format::Int64 => Value::of(Real::from_int(int64.cast_signed())),
        Format::Bcd => Value::of(Real::from_bcd(raw)),
        _ => Value::of(Real::from_bytes(raw)),
    }
}

/// Runs `insn` on `regs`, its memory operand `operand` read from `address`.
pub(crate) fn execute(
    insn: Insn,
    regs: &mut Registers,
    operand: &[u8],
    address: Option<u32>,
) -> Done {
    let mut done = Done {
        clocks: timing::clocks(insn, regs, operand),
        raised: 0,
        store: None,
    };
    let flags = match insn {
        Insn::ArithMemory(op, format) => arith_memory(regs, op, load_value(format, operand)),
        Insn::ArithRegister { op, i, into, pop } => arith_register(regs, op, i, into, pop),
        Insn::ComparePop2 => compare_into_codes(regs, 1, 2),
        Insn::Load(format) => load(regs, format, operand),
        Insn::LoadRegister(i) => load_register(regs, i),
        Insn::Store(format, pop) => {
            let (store, flags) = store_memory(regs, format, pop);
            done.store = store
                .and_then(|bytes| address.map(|addr| Store::new(addr, &bytes[..format.bytes()])));
            flags
        }
        Insn::StoreRegister(i, pop) => store_register(regs, i, pop),
        Insn::Exchange(i) => exchange(regs, i),
        Insn::Free(i, pop) => {
            let physical = regs.physical(i);
            regs.set_tag(physical, tag::EMPTY);
            if pop {
                regs.pop();
            }
            0
        }
        Insn::LoadConstant(constant) => push_checked(regs, constant.value()),
        Insn::ChangeSign | Insn::Absolute => sign(regs, insn == Insn::ChangeSign),
        Insn::Test => compare_into_codes_with(regs, Value::Zero(false), 0),
        Insn::Examine => {
            examine(regs);
            0
        }
        Insn::DecrementTop => {
            regs.set_top(regs.top().wrapping_sub(1));
            0
        }
        Insn::IncrementTop => {
            regs.set_top(regs.top().wrapping_add(1));
            0
        }
        Insn::LoadControl => {
            regs.control = u16::from_le_bytes([byte(operand, 0), byte(operand, 1)]);
            0
        }
        Insn::LoadEnvironment => {
            load_environment(regs, operand);
            0
        }
        Insn::Restore => {
            load_environment(regs, operand);
            for n in 0..8u8 {
                let at = 14 + 10 * usize::from(n);
                let mut raw = [0u8; 10];
                for (k, b) in raw.iter_mut().enumerate() {
                    *b = byte(operand, at + k);
                }
                let physical = regs.physical(n);
                regs.data[physical] = Real::from_bytes(raw);
            }
            0
        }
        Insn::StoreEnvironment | Insn::Save => {
            let mut image = [0u8; STATE_BYTES];
            store_environment(regs, &mut image);
            let len = if insn == Insn::Save {
                for n in 0..8u8 {
                    let at = 14 + 10 * usize::from(n);
                    image[at..at + 10].copy_from_slice(&regs.data[regs.physical(n)].to_bytes());
                }
                regs.initialise();
                STATE_BYTES
            } else {
                regs.control |= flag::ALL;
                14
            };
            done.store = address.map(|addr| Store::new(addr, &image[..len]));
            0
        }
        // FNOP, and the control unit's own, run before this is reached.
        Insn::Nop
        | Insn::EnableInterrupts
        | Insn::DisableInterrupts
        | Insn::ClearExceptions
        | Insn::Initialise
        | Insn::StoreControl
        | Insn::StoreStatus => 0,
        other => special(regs, other),
    };
    regs.status |= flags & flag::ALL;
    done.raised = flags & flag::ALL;
    done.clocks += timing::penalty(flags, regs.masks());
    done
}

/// The byte at `n` of an operand, or zero past its end.
fn byte(operand: &[u8], n: usize) -> u8 {
    operand.get(n).copied().unwrap_or(0)
}

/// FPTAN, FPATAN, F2XM1, FYL2X, FYL2XP1, FXTRACT, FPREM, FSQRT, FRNDINT and
/// FSCALE.
fn special(regs: &mut Registers, insn: Insn) -> u16 {
    match insn {
        Insn::SquareRoot => {
            let outcome = square_root(regs, regs.value(0));
            apply(regs, 0, outcome, false)
        }
        Insn::RoundToInteger => {
            let outcome = round_to_integer(regs, regs.value(0));
            apply(regs, 0, outcome, false)
        }
        Insn::Scale => scale(regs),
        Insn::PartialRemainder => partial_remainder(regs),
        Insn::Extract => extract(regs),
        Insn::PartialTangent => partial_tangent(regs),
        Insn::PartialArctangent => {
            let outcome = transcendental::arctangent(regs, regs.value(1), regs.value(0));
            apply(regs, 1, outcome, true)
        }
        Insn::TwoPowerMinusOne => {
            let outcome = transcendental::two_power_minus_one(regs, regs.value(0));
            apply(regs, 0, outcome, false)
        }
        Insn::YLog2X => {
            let outcome = transcendental::y_log2_x(regs, regs.value(1), regs.value(0));
            apply(regs, 1, outcome, true)
        }
        Insn::YLog2XPlus1 => {
            let outcome = transcendental::y_log2_x_plus_1(regs, regs.value(1), regs.value(0));
            apply(regs, 1, outcome, true)
        }
        _ => 0,
    }
}

/// Writes an outcome to ST(`i`), popping after where `pop`; an outcome with
/// no value leaves the stack as it was. Answers the flags.
fn apply(regs: &mut Registers, i: u8, outcome: Outcome, pop: bool) -> u16 {
    if let Some(value) = outcome.value {
        regs.put(i, value);
        if pop {
            regs.pop();
        }
    }
    outcome.flags
}

fn arith_memory(regs: &mut Registers, op: Arith, source: Value) -> u16 {
    if let Arith::Compare(pops) = op {
        return compare_into_codes_with(regs, source, pops);
    }
    let outcome = arithmetic(regs, op, regs.value(0), source);
    apply(regs, 0, outcome, false)
}

fn arith_register(regs: &mut Registers, op: Arith, i: u8, into: bool, pop: bool) -> u16 {
    if let Arith::Compare(pops) = op {
        return compare_into_codes_with(regs, regs.value(i), pops);
    }
    let (destination, a, b) = if into {
        (i, regs.value(i), regs.value(0))
    } else {
        (0, regs.value(0), regs.value(i))
    };
    let outcome = arithmetic(regs, op, a, b);
    apply(regs, destination, outcome, pop)
}

/// Compares ST(0) with ST(`i`), popping `pops` times.
fn compare_into_codes(regs: &mut Registers, i: u8, pops: u8) -> u16 {
    compare_into_codes_with(regs, regs.value(i), pops)
}

/// Compares ST(0) with `source`, sets C3, C2 and C0, and pops `pops` times
/// unless an unmasked exception abandons it.
fn compare_into_codes_with(regs: &mut Registers, source: Value, pops: u8) -> u16 {
    let (codes, flags) = compare(regs, regs.value(0), source);
    if let Some(codes) = codes {
        regs.set_codes(cc::COMPARE, codes);
        for _ in 0..pops {
            regs.pop();
        }
    }
    flags
}

/// Pushes `x` unless ST(7) holds something: then an invalid operation,
/// the indefinite pushed where it is masked.
fn push_checked(regs: &mut Registers, x: Real) -> u16 {
    if !regs.empty(7) {
        if regs.masked(flag::INVALID) {
            regs.push(Real::INDEFINITE);
        }
        return flag::INVALID;
    }
    regs.push(x);
    0
}

fn load(regs: &mut Registers, format: Format, operand: &[u8]) -> u16 {
    let value = load_value(format, operand);
    let mut raw = [0u8; 10];
    let n = operand.len().min(10);
    raw[..n].copy_from_slice(&operand[..n]);
    let x = match (format, value) {
        (Format::Extended, _) => Real::from_bytes(raw),
        (_, Value::Finite(p, _)) => {
            let field = u16::try_from(p.exp + BIAS).unwrap_or(0);
            Real::new(p.negative, field, p.sig)
        }
        (_, Value::Zero(negative)) => Real::ZERO.with_sign(negative),
        (_, Value::Infinity(negative)) => Real::INFINITY.with_sign(negative),
        (_, Value::Nan(x)) => x,
        (_, Value::Empty) => Real::INDEFINITE,
    };
    // Every load but FILD's and FBLD's checks its operand for a denormal.
    let denormal = !format.integer() && format != Format::Bcd && value.denormal();
    if denormal && !regs.masked(flag::DENORMAL) {
        return flag::DENORMAL;
    }
    let flags = if denormal { flag::DENORMAL } else { 0 };
    flags | push_checked(regs, x)
}

fn load_register(regs: &mut Registers, i: u8) -> u16 {
    if regs.empty(i) {
        if !regs.masked(flag::INVALID) {
            return flag::INVALID;
        }
        return flag::INVALID | push_checked(regs, Real::INDEFINITE);
    }
    let x = regs.data[regs.physical(i)];
    push_checked(regs, x)
}

fn store_register(regs: &mut Registers, i: u8, pop: bool) -> u16 {
    let (x, flags) = if regs.empty(0) {
        if !regs.masked(flag::INVALID) {
            return flag::INVALID;
        }
        (Real::INDEFINITE, flag::INVALID)
    } else {
        (regs.data[regs.physical(0)], 0)
    };
    regs.put(i, x);
    if pop {
        regs.pop();
    }
    flags
}

fn exchange(regs: &mut Registers, i: u8) -> u16 {
    let empties = regs.empty(0) || regs.empty(i);
    if empties && !regs.masked(flag::INVALID) {
        return flag::INVALID;
    }
    let read = |regs: &Registers, n: u8| {
        if regs.empty(n) {
            Real::INDEFINITE
        } else {
            regs.data[regs.physical(n)]
        }
    };
    let (a, b) = (read(regs, 0), read(regs, i));
    regs.put(0, b);
    regs.put(i, a);
    if empties {
        flag::INVALID
    } else {
        0
    }
}

fn sign(regs: &mut Registers, change: bool) -> u16 {
    if regs.empty(0) {
        if regs.masked(flag::INVALID) {
            regs.put(0, Real::INDEFINITE);
        }
        return flag::INVALID;
    }
    let physical = regs.physical(0);
    let x = regs.data[physical];
    let negative = change && !x.negative();
    regs.data[physical] = x.with_sign(negative);
    0
}

/// FXAM: C3 to C0 from ST(0)'s tag and bits, C1 its sign.
fn examine(regs: &mut Registers) {
    let physical = regs.physical(0);
    let x = regs.data[physical];
    let sign = if x.negative() { cc::C1 } else { 0 };
    let codes = match regs.tag(physical) {
        tag::EMPTY => cc::C3 | cc::C0,
        tag::ZERO => cc::C3,
        _ => match x.class() {
            Class::Unnormal | Class::PseudoZero => 0,
            Class::Nan => cc::C0,
            Class::Normal => cc::C2,
            Class::Infinity => cc::C2 | cc::C0,
            Class::Zero => cc::C3,
            Class::Denormal => cc::C3 | cc::C2,
        },
    };
    regs.set_codes(cc::ALL, codes | sign);
}

/// The 14-byte real-mode environment: the control, status and tag words,
/// then the instruction pointer with the opcode, and the operand pointer.
fn store_environment(regs: &Registers, image: &mut [u8; STATE_BYTES]) {
    // The status word stored shows BUSY only for an interrupt held.
    let mut status = regs.status & !0x8080;
    if regs.interrupt_request() {
        status |= 0x0080;
        if regs.control & IEM == 0 {
            status |= 0x8000;
        }
    }
    let ip_high = (crate::bits::low16(regs.ip >> 16) & 0xF) << 12;
    let dp_high = (crate::bits::low16(regs.dp >> 16) & 0xF) << 12;
    let words = [
        regs.control,
        status,
        regs.tags,
        crate::bits::low16(regs.ip),
        ip_high | (regs.opcode & 0x07FF),
        crate::bits::low16(regs.dp),
        dp_high,
    ];
    for (n, word) in words.iter().enumerate() {
        image[2 * n..2 * n + 2].copy_from_slice(&word.to_le_bytes());
    }
}

fn load_environment(regs: &mut Registers, operand: &[u8]) {
    let word = |n: usize| u16::from_le_bytes([byte(operand, 2 * n), byte(operand, 2 * n + 1)]);
    regs.control = word(0);
    regs.status = word(1) & !0x8080;
    regs.tags = word(2);
    regs.ip = u32::from(word(3)) | (u32::from(word(4) >> 12) << 16);
    regs.opcode = word(4) & 0x07FF;
    regs.dp = u32::from(word(5)) | (u32::from(word(6) >> 12) << 16);
}

/// FST, FSTP, FIST, FISTP and FBSTP: the bytes stored, or none where an
/// unmasked exception refuses the store; the flags. The stack is popped
/// where `pop` says and the store went ahead.
fn store_memory(regs: &mut Registers, format: Format, pop: bool) -> (Option<[u8; 10]>, u16) {
    let value = regs.value(0);
    let (bytes, flags) = match format {
        Format::Single | Format::Double => store_binary(regs, format, value),
        Format::Int16 | Format::Int32 | Format::Int64 => store_integer(regs, format, value),
        Format::Bcd => store_bcd(regs, value),
        _ => match value {
            Value::Empty => {
                let bytes = regs
                    .masked(flag::INVALID)
                    .then(|| Real::INDEFINITE.to_bytes());
                (bytes, flag::INVALID)
            }
            _ => (Some(regs.data[regs.physical(0)].to_bytes()), 0),
        },
    };
    if bytes.is_some() && pop {
        regs.pop();
    }
    (bytes, flags)
}

fn bytes_of(bits: u64) -> [u8; 10] {
    let mut out = [0u8; 10];
    out[..8].copy_from_slice(&bits.to_le_bytes());
    out
}

fn store_binary(regs: &Registers, format: Format, value: Value) -> (Option<[u8; 10]>, u16) {
    let binary = if format == Format::Single {
        Binary::SINGLE
    } else {
        Binary::DOUBLE
    };
    let fraction_bits = binary.precision - 1;
    let sign = |negative: bool| u64::from(negative) << (binary.width - 1);
    let special = binary.exp_mask() << fraction_bits;
    let (bits, flags) = match value {
        Value::Empty => {
            let bits = regs.masked(flag::INVALID).then_some(binary.indefinite());
            (bits, flag::INVALID)
        }
        Value::Zero(negative) => (Some(sign(negative)), 0),
        Value::Infinity(negative) => (Some(sign(negative) | special), 0),
        Value::Nan(x) => {
            // The fraction's top bits; one kept nonzero so the NaN stays
            // one.
            let mut fraction = (x.sig & !INTEGER) >> (64 - binary.precision);
            if fraction == 0 {
                fraction = 1 << (fraction_bits - 1);
            }
            (Some(sign(x.negative()) | special | fraction), 0)
        }
        Value::Finite(p, kind) => {
            let in_range = p.exp >= binary.min_exp();
            match kind {
                Kind::Unnormal | Kind::PseudoZero if in_range => {
                    let bits = regs.masked(flag::INVALID).then_some(binary.indefinite());
                    (bits, flag::INVALID)
                }
                Kind::PseudoZero => (Some(sign(p.negative)), 0),
                _ => arith::round_binary(p.normalized(), binary, regs.mode(), regs.masks()),
            }
        }
    };
    (bits.map(bytes_of), flags)
}

fn store_integer(regs: &Registers, format: Format, value: Value) -> (Option<[u8; 10]>, u16) {
    let width = match format {
        Format::Int16 => 16,
        Format::Int32 => 32,
        _ => 64,
    };
    let indefinite = 1u64 << (width - 1);
    let invalid = || {
        let bits = regs.masked(flag::INVALID).then(|| bytes_of(indefinite));
        (bits, flag::INVALID)
    };
    match value {
        Value::Zero(_) => (Some(bytes_of(0)), 0),
        Value::Finite(p, Kind::Normal) => {
            let (magnitude, inexact) = arith::to_integer(p, regs.mode());
            let limit = if p.negative {
                indefinite
            } else {
                indefinite - 1
            };
            match magnitude {
                Some(m) if m <= limit => {
                    let bits = if p.negative { m.wrapping_neg() } else { m };
                    let mask = if width == 64 {
                        u64::MAX
                    } else {
                        (1 << width) - 1
                    };
                    let flags = if inexact { flag::PRECISION } else { 0 };
                    (Some(bytes_of(bits & mask)), flags)
                }
                _ => invalid(),
            }
        }
        _ => invalid(),
    }
}

/// The BCD indefinite: sign and pad ones, then a first digit byte of ones;
/// the rest the 8087 leaves undefined, here the real indefinite's.
const BCD_INDEFINITE: [u8; 10] = [0, 0, 0, 0, 0, 0, 0, 0xC0, 0xFF, 0xFF];

fn store_bcd(regs: &Registers, value: Value) -> (Option<[u8; 10]>, u16) {
    const MAX: u64 = 999_999_999_999_999_999;
    let invalid = || {
        (
            regs.masked(flag::INVALID).then_some(BCD_INDEFINITE),
            flag::INVALID,
        )
    };
    let (negative, magnitude, flags) = match value {
        Value::Zero(negative) => (negative, 0, 0),
        Value::Finite(p, Kind::Normal) => match arith::to_integer(p, regs.mode()) {
            (Some(m), inexact) if m <= MAX => {
                (p.negative, m, if inexact { flag::PRECISION } else { 0 })
            }
            _ => return invalid(),
        },
        _ => return invalid(),
    };
    let mut bytes = [0u8; 10];
    let mut rest = magnitude;
    for byte in &mut bytes[..9] {
        let low = rest % 10;
        rest /= 10;
        let high = rest % 10;
        rest /= 10;
        *byte = crate::bits::low8(u32::try_from(high << 4 | low).unwrap_or(0));
    }
    bytes[9] = if negative { 0x80 } else { 0 };
    (Some(bytes), flags)
}

fn square_root(regs: &Registers, a: Value) -> Outcome {
    match a {
        Value::Nan(x) => Outcome {
            value: regs.masked(flag::INVALID).then_some(x),
            flags: flag::INVALID,
        },
        Value::Zero(negative) => Outcome::value(Real::ZERO.with_sign(negative), 0),
        Value::Infinity(false) if regs.affine() => Outcome::value(Real::INFINITY, 0),
        Value::Finite(p, Kind::Normal) if !p.negative => Outcome::rounded(
            arith::round(arith::square_root(p), regs.rounding(), regs.masks()),
            0,
        ),
        _ => Outcome::invalid(regs, flag::INVALID),
    }
}

fn round_to_integer(regs: &Registers, a: Value) -> Outcome {
    match a {
        Value::Empty => Outcome::invalid(regs, flag::INVALID),
        Value::Nan(x) => Outcome {
            value: regs.masked(flag::INVALID).then_some(x),
            flags: flag::INVALID,
        },
        Value::Zero(negative) => Outcome::value(Real::ZERO.with_sign(negative), 0),
        Value::Infinity(negative) => Outcome::value(Real::INFINITY.with_sign(negative), 0),
        // A pseudo-zero rounds as the zero it is worth.
        Value::Finite(p, _) => {
            let p = p.normalized();
            if let (Some(m), inexact) = arith::to_integer(p, regs.mode()) {
                let flags = if inexact { flag::PRECISION } else { 0 };
                Outcome::value(Real::from_magnitude(p.negative, m), flags)
            } else {
                // Past 2^64 every value is an integer already.
                let field = u16::try_from(p.exp + BIAS).unwrap_or(0);
                Outcome::value(Real::new(p.negative, field, p.sig), 0)
            }
        }
    }
}

fn scale(regs: &mut Registers) -> u16 {
    let (value, by) = (regs.value(0), regs.value(1));
    let outcome = match screen(regs, value, by, false) {
        Err(outcome) => outcome,
        Ok(flags) => match (value, by) {
            (Value::Infinity(_) | Value::Finite(..), Value::Infinity(_)) => {
                Outcome::invalid(regs, flags | flag::INVALID)
            }
            (Value::Infinity(negative), _) => {
                Outcome::value(Real::INFINITY.with_sign(negative), flags)
            }
            (Value::Zero(negative), _) => Outcome::value(Real::ZERO.with_sign(negative), flags),
            (Value::Finite(parts, _), _) => {
                let power = match by {
                    Value::Finite(factor, _) => scale_factor(factor),
                    _ => 0,
                };
                let x = Exact::of(Parts {
                    exp: parts.exp.saturating_add(power),
                    ..parts
                });
                let rounded = arith::round(x, Rounding::extended(regs.mode()), regs.masks());
                // FSCALE raises no precision exception.
                Outcome {
                    value: rounded.value,
                    flags: (rounded.flags & !flag::PRECISION) | flags,
                }
            }
            _ => Outcome::invalid(regs, flags | flag::INVALID),
        },
    };
    apply(regs, 0, outcome, false)
}

/// The integer FSCALE scales by: ST(1) chopped, bounded well past any
/// exponent's reach.
fn scale_factor(q: Parts) -> i32 {
    let q = q.normalized();
    if q.exp < 0 {
        return 0;
    }
    let magnitude = if q.exp >= 20 {
        1 << 20
    } else {
        i32::try_from(q.sig >> (63 - q.exp)).unwrap_or(1 << 20)
    };
    if q.negative {
        -magnitude
    } else {
        magnitude
    }
}

fn partial_remainder(regs: &mut Registers) -> u16 {
    let (a, b) = (regs.value(0), regs.value(1));
    // A denormal modulus is an invalid operation, not a denormal one.
    let flags = match screen(regs, a, b, a.denormal()) {
        Ok(flags) => flags,
        Err(outcome) => return apply(regs, 0, outcome, false),
    };
    let complete = |regs: &mut Registers, quotient: u64| {
        let mut codes = 0;
        let mut mask = cc::C2 | cc::C1;
        if quotient & 1 != 0 {
            codes |= cc::C1;
        }
        if quotient >= 2 {
            mask |= cc::C3;
            if quotient & 2 != 0 {
                codes |= cc::C3;
            }
        }
        if quotient >= 4 {
            mask |= cc::C0;
            if quotient & 4 != 0 {
                codes |= cc::C0;
            }
        }
        regs.set_codes(mask, codes);
    };
    match (a, b) {
        (Value::Infinity(_), _)
        | (
            _,
            Value::Zero(_) | Value::Finite(_, Kind::Unnormal | Kind::PseudoZero | Kind::Denormal),
        ) => {
            let outcome = Outcome::invalid(regs, flags | flag::INVALID);
            if outcome.value.is_some() {
                regs.set_codes(cc::C2, 0);
            }
            apply(regs, 0, outcome, false)
        }
        (Value::Zero(_) | Value::Finite(..), Value::Infinity(_)) | (Value::Zero(_), _) => {
            complete(regs, 0);
            flags
        }
        (Value::Finite(p, _), Value::Finite(q, _)) => {
            let (rest, quotient) = arith::remainder(p.normalized(), q);
            let rounded = arith::round(rest, Rounding::extended(regs.mode()), regs.masks());
            let outcome = Outcome {
                value: rounded.value,
                flags: (rounded.flags & !flag::PRECISION) | flags,
            };
            if outcome.value.is_some() {
                match quotient {
                    Some(quotient) => complete(regs, quotient),
                    None => regs.set_codes(cc::C2, cc::C2),
                }
            }
            apply(regs, 0, outcome, false)
        }
        _ => flags,
    }
}

fn extract(regs: &mut Registers) -> u16 {
    let a = regs.value(0);
    if a == Value::Empty || !regs.empty(7) {
        if !regs.masked(flag::INVALID) {
            return flag::INVALID;
        }
        if a == Value::Empty {
            regs.put(0, Real::INDEFINITE);
        }
        regs.push(Real::INDEFINITE);
        return flag::INVALID;
    }
    let (exponent, significand, flags) = match a {
        Value::Zero(negative) => (
            Real::ZERO.with_sign(negative),
            Real::ZERO.with_sign(negative),
            0,
        ),
        Value::Finite(_, _) => {
            let x = regs.data[regs.physical(0)];
            let exponent = Real::from_int(i64::from(x.exponent()) - i64::from(BIAS));
            let significand = Real::new(x.negative(), 0x3FFF, x.sig);
            (exponent, significand, 0)
        }
        Value::Nan(x) => {
            if !regs.masked(flag::INVALID) {
                return flag::INVALID;
            }
            (x, x, flag::INVALID)
        }
        Value::Infinity(_) | Value::Empty => {
            if !regs.masked(flag::INVALID) {
                return flag::INVALID;
            }
            (Real::INDEFINITE, Real::INDEFINITE, flag::INVALID)
        }
    };
    regs.put(0, exponent);
    regs.push(significand);
    flags
}

fn partial_tangent(regs: &mut Registers) -> u16 {
    let a = regs.value(0);
    if a == Value::Empty || !regs.empty(7) {
        // The operand stays, moved into ST(1) by the push.
        if !regs.masked(flag::INVALID) {
            return flag::INVALID;
        }
        if a == Value::Empty {
            regs.put(0, Real::INDEFINITE);
        }
        regs.push(Real::INDEFINITE);
        return flag::INVALID;
    }
    let (y, x, flags) = match a {
        Value::Zero(negative) => (Real::ZERO.with_sign(negative), Real::ONE, 0),
        Value::Finite(p, _) if p.normalized().exp >= 63 => {
            // Out of the 80387's reach: C2 set, the operand left.
            regs.set_codes(cc::C2, cc::C2);
            return 0;
        }
        Value::Finite(p, _) => {
            let rounded =
                arith::round(trans::tan(p), Rounding::extended(regs.mode()), regs.masks());
            match rounded.value {
                Some(y) => (y, Real::ONE, rounded.flags),
                None => return rounded.flags,
            }
        }
        Value::Nan(nan) => {
            if !regs.masked(flag::INVALID) {
                return flag::INVALID;
            }
            (nan, nan, flag::INVALID)
        }
        Value::Infinity(_) | Value::Empty => {
            if !regs.masked(flag::INVALID) {
                return flag::INVALID;
            }
            (Real::INDEFINITE, Real::INDEFINITE, flag::INVALID)
        }
    };
    regs.set_codes(cc::C2, 0);
    regs.put(0, y);
    regs.push(x);
    flags
}

#[path = "transcendental.rs"]
mod transcendental;

#[cfg(test)]
#[path = "exec_tests.rs"]
mod tests;
