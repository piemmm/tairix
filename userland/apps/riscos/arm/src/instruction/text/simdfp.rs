//! The text of a floating-point or Advanced SIMD instruction: UAL's, the
//! condition before the data type (`VADDEQ.F32`), as BASIC VI's assembler and
//! RISC OS's Debugger spell it.

use core::fmt::{self, Display, Formatter, Write as _};

use super::{Buffer, Number, Signed};
use crate::instruction::{
    BlockMode, ConvertRounding, SimdFp, SimdImmediateOp, SimdLongOp, SimdMiscOp, SimdScalarOp,
    SimdShiftOp, SimdThreeOp, StructureLanes, Width,
};
use crate::reg::Reg;

/// A VFP register: a single, or a double.
struct Fp {
    double: bool,
    n: u8,
}

impl Display for Fp {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let bank = if self.double { 'D' } else { 'S' };
        write!(f, "{bank}{}", self.n)
    }
}

/// An Advanced SIMD register: a double, or the quad whose first double it is.
struct Vector {
    q: bool,
    d: u8,
}

impl Display for Vector {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        if self.q {
            write!(f, "Q{}", self.d >> 1)
        } else {
            write!(f, "D{}", self.d)
        }
    }
}

const fn dq(d: u8) -> Vector {
    Vector { q: false, d }
}

const fn qq(d: u8) -> Vector {
    Vector { q: true, d }
}

/// A floating-point precision's data type.
const fn float(double: bool) -> &'static str {
    if double {
        ".F64"
    } else {
        ".F32"
    }
}

/// How an operation's lanes are typed in its mnemonic.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Typing {
    /// No type: the bitwise operations.
    Untyped,
    /// A size alone: `.8`.
    Size,
    /// Integers of either sign: `.I8`.
    Integer,
    /// Signed or unsigned as the instruction says: `.S8`, `.U8`.
    Signed,
    /// Signed always: `.S16`.
    SignedOnly,
    /// Polynomials: `.P8`.
    Polynomial,
    /// Single precision: `.F32`.
    Float,
}

fn data_type(out: &mut Buffer, typing: Typing, unsigned: bool, bits: u32) -> fmt::Result {
    match typing {
        Typing::Untyped => Ok(()),
        Typing::Size => write!(out, ".{bits}"),
        Typing::Integer => write!(out, ".I{bits}"),
        Typing::Signed if unsigned => write!(out, ".U{bits}"),
        Typing::Signed | Typing::SignedOnly => write!(out, ".S{bits}"),
        Typing::Polynomial => write!(out, ".P{bits}"),
        Typing::Float => out.push(".F32"),
    }
}

const fn three_typing(op: SimdThreeOp) -> Typing {
    use SimdThreeOp as O;
    match op {
        O::Vhadd
        | O::Vqadd
        | O::Vrhadd
        | O::Vhsub
        | O::Vqsub
        | O::Vcgt
        | O::Vcge
        | O::Vshl
        | O::Vqshl
        | O::Vrshl
        | O::Vqrshl
        | O::Vmax
        | O::Vmin
        | O::Vabd
        | O::Vaba
        | O::Vpmax
        | O::Vpmin => Typing::Signed,
        O::Vand | O::Vbic | O::Vorr | O::Vorn | O::Veor | O::Vbsl | O::Vbit | O::Vbif => {
            Typing::Untyped
        }
        O::Vadd | O::Vsub | O::Vceq | O::Vmla | O::Vmls | O::Vmul | O::Vpadd => Typing::Integer,
        O::Vtst
        | O::Sha1c
        | O::Sha1p
        | O::Sha1m
        | O::Sha1su0
        | O::Sha256h
        | O::Sha256h2
        | O::Sha256su1 => Typing::Size,
        O::Vmulp => Typing::Polynomial,
        O::Vqdmulh | O::Vqrdmulh => Typing::SignedOnly,
        _ => Typing::Float,
    }
}

const fn long_typing(op: SimdLongOp) -> Typing {
    use SimdLongOp as O;
    match op {
        O::Vaddhn | O::Vraddhn | O::Vsubhn | O::Vrsubhn => Typing::Integer,
        O::Vqdmlal | O::Vqdmlsl | O::Vqdmull => Typing::SignedOnly,
        O::Vmullp | O::Vmullp64 => Typing::Polynomial,
        _ => Typing::Signed,
    }
}

const fn scalar_typing(op: SimdScalarOp) -> Typing {
    use SimdScalarOp as O;
    match op {
        O::Vmla | O::Vmls | O::Vmul => Typing::Integer,
        O::VmlaF | O::VmlsF | O::VmulF => Typing::Float,
        O::Vmlal | O::Vmlsl | O::Vmull => Typing::Signed,
        _ => Typing::SignedOnly,
    }
}

const fn shift_typing(op: SimdShiftOp) -> Typing {
    use SimdShiftOp as O;
    match op {
        O::Vsri | O::Vsli => Typing::Size,
        O::Vshl | O::Vshrn | O::Vrshrn => Typing::Integer,
        O::Vqshlu | O::Vqshrun | O::Vqrshrun => Typing::SignedOnly,
        _ => Typing::Signed,
    }
}

const fn misc_typing(op: SimdMiscOp) -> Typing {
    use SimdMiscOp as O;
    match op {
        O::Vmvn | O::Vswp => Typing::Untyped,
        O::Vrev64
        | O::Vrev32
        | O::Vrev16
        | O::Vcnt
        | O::Vtrn
        | O::Vuzp
        | O::Vzip
        | O::Aese
        | O::Aesd
        | O::Aesmc
        | O::Aesimc
        | O::Sha1h
        | O::Sha1su1
        | O::Sha256su0 => Typing::Size,
        O::Vpaddl | O::Vpadal | O::Vqmovn => Typing::Signed,
        O::Vcls
        | O::Vqabs
        | O::Vqneg
        | O::Vcgt0
        | O::Vcge0
        | O::Vcle0
        | O::Vclt0
        | O::Vabs
        | O::Vneg
        | O::Vqmovun => Typing::SignedOnly,
        O::Vclz | O::Vceq0 | O::Vmovn | O::Vshll => Typing::Integer,
        _ => Typing::Float,
    }
}

/// A register list: `{S0}`, `{D0-D3}`.
/// `base` bits widened `size` times over: a lane's width. A size past any
/// lane's gives nothing.
const fn bits(base: u32, size: u8) -> u32 {
    match base.checked_shl(size as u32) {
        Some(bits) => bits,
        None => 0,
    }
}

fn list(out: &mut Buffer, double: bool, first: u8, count: u8) -> fmt::Result {
    let last = first.saturating_add(count.saturating_sub(1));
    let bank = if double { 'D' } else { 'S' };
    if count <= 1 {
        write!(out, "{{{bank}{first}}}")
    } else {
        write!(out, "{{{bank}{first}-{bank}{last}}}")
    }
}

/// VFP's eight-bit constant as the decimal it is exactly: ±(16 + efgh)
/// over a power of two from 1 to 128, whose decimal terminates.
fn constant(out: &mut Buffer, imm: u8) -> fmt::Result {
    let cd = i32::from((imm >> 4) & 3);
    let exponent = if imm & 0x40 == 0 { 1 + cd } else { cd - 3 };
    let shift = (4 - exponent).cast_unsigned();
    let numerator = 16 + u32::from(imm & 15);
    out.push(if imm & 0x80 == 0 { "#" } else { "#-" })?;
    let mask = (1u32 << shift) - 1;
    write!(out, "{}.", numerator >> shift)?;
    let mut rest = numerator & mask;
    loop {
        rest *= 10;
        write!(out, "{}", rest >> shift)?;
        rest &= mask;
        if rest == 0 {
            return Ok(());
        }
    }
}

/// A modified immediate's data type and value, as the expansion gives it.
fn modified_immediate(out: &mut Buffer, operands: &mut Buffer, cmode: u8, imm: u8) -> fmt::Result {
    let value = u32::from(imm);
    let (typing, shown) = match cmode & 0xF {
        0 | 1 => (".I32", value),
        2 | 3 => (".I32", value << 8),
        4 | 5 => (".I32", value << 16),
        6 | 7 => (".I32", value << 24),
        8 | 9 => (".I16", value),
        10 | 11 => (".I16", value << 8),
        12 => (".I32", value << 8 | 0xFF),
        13 => (".I32", value << 16 | 0xFFFF),
        14 if cmode & 0x10 == 0 => (".I8", value),
        14 => {
            out.push(".I64")?;
            let mut mask = 0u64;
            for bit in 0..8 {
                if imm >> bit & 1 != 0 {
                    mask |= 0xFF << (8 * bit);
                }
            }
            return write!(operands, "#&{mask:X}");
        }
        _ => {
            out.push(".F32")?;
            return constant(operands, imm);
        }
    };
    out.push(typing)?;
    write!(operands, "#{}", Number(shown))
}

/// A structure transfer's register list: whole registers, as a range where
/// they follow one another; a lane of each; or each to all lanes.
fn structure_list(out: &mut Buffer, d: u8, shape: crate::instruction::Structure) -> fmt::Result {
    let registers = shape.registers();
    let whole = shape.lanes == StructureLanes::Multiple;
    if whole && (shape.parts > 1 || shape.spacing == 1) {
        return list(out, true, d, u8::try_from(registers).unwrap_or(u8::MAX));
    }
    out.push("{")?;
    for index in 0..registers {
        if index > 0 {
            out.push(",")?;
        }
        write!(out, "D{}", u32::from(d) + shape.register(index))?;
        match shape.lanes {
            StructureLanes::Multiple => {}
            StructureLanes::All => out.push("[]")?,
            StructureLanes::One(lane) => write!(out, "[{lane}]")?,
        }
    }
    out.push("}")
}

/// The mnemonic and operands of `op` under the condition suffix `c`.
#[allow(clippy::too_many_lines)] // One arm per form, each its own syntax.
pub(super) fn write(op: &SimdFp, c: &str, m: &mut Buffer, operands: &mut Buffer) -> fmt::Result {
    match *op {
        SimdFp::Dyadic {
            op,
            double,
            d,
            n,
            m: rm,
        } => {
            m.push(op.name())?;
            m.push(c)?;
            m.push(float(double))?;
            let reg = |n| Fp { double, n };
            write!(operands, "{},{},{}", reg(d), reg(n), reg(rm))?;
        }
        SimdFp::Select {
            cond,
            double,
            d,
            n,
            m: rm,
        } => {
            write!(m, "VSEL{}{}", cond.suffix(), float(double))?;
            let reg = |n| Fp { double, n };
            write!(operands, "{},{},{}", reg(d), reg(n), reg(rm))?;
        }
        SimdFp::Monadic {
            op,
            double,
            d,
            m: rm,
        } => {
            m.push(op.name())?;
            m.push(c)?;
            m.push(float(double))?;
            write!(operands, "{},{}", Fp { double, n: d }, Fp { double, n: rm })?;
        }
        SimdFp::Compare {
            signalling,
            double,
            d,
            m: rm,
        } => {
            m.push(if signalling { "VCMPE" } else { "VCMP" })?;
            m.push(c)?;
            m.push(float(double))?;
            match rm {
                Some(rm) => write!(operands, "{},{}", Fp { double, n: d }, Fp { double, n: rm })?,
                None => write!(operands, "{},#0", Fp { double, n: d })?,
            }
        }
        SimdFp::Precision {
            to_double,
            d,
            m: rm,
        } => {
            m.push("VCVT")?;
            m.push(c)?;
            m.push(if to_double { ".F64.F32" } else { ".F32.F64" })?;
            write!(
                operands,
                "{},{}",
                Fp {
                    double: to_double,
                    n: d
                },
                Fp {
                    double: !to_double,
                    n: rm
                }
            )?;
        }
        SimdFp::Half {
            to_half,
            double,
            top,
            d,
            m: rm,
        } => {
            m.push(if top { "VCVTT" } else { "VCVTB" })?;
            m.push(c)?;
            let other = float(double);
            if to_half {
                write!(m, ".F16{other}")?;
            } else {
                write!(m, "{other}.F16")?;
            }
            let d = Fp {
                double: double && !to_half,
                n: d,
            };
            let rm = Fp {
                double: double && to_half,
                n: rm,
            };
            write!(operands, "{d},{rm}")?;
        }
        SimdFp::ToInteger {
            rounding,
            unsigned,
            double,
            d,
            m: rm,
        } => {
            m.push(rounding.name())?;
            if matches!(rounding, ConvertRounding::Zero | ConvertRounding::Fpscr) {
                m.push(c)?;
            }
            m.push(if unsigned { ".U32" } else { ".S32" })?;
            m.push(float(double))?;
            write!(
                operands,
                "{},{}",
                Fp {
                    double: false,
                    n: d
                },
                Fp { double, n: rm }
            )?;
        }
        SimdFp::FromInteger {
            unsigned,
            double,
            d,
            m: rm,
        } => {
            m.push("VCVT")?;
            m.push(c)?;
            m.push(float(double))?;
            m.push(if unsigned { ".U32" } else { ".S32" })?;
            write!(
                operands,
                "{},{}",
                Fp { double, n: d },
                Fp {
                    double: false,
                    n: rm
                }
            )?;
        }
        SimdFp::Fixed {
            to_fixed,
            unsigned,
            word,
            double,
            d,
            fraction,
        } => {
            m.push("VCVT")?;
            m.push(c)?;
            let sign = if unsigned { 'U' } else { 'S' };
            let bits = if word { 32 } else { 16 };
            let fixed = |m: &mut Buffer| write!(m, ".{sign}{bits}");
            if to_fixed {
                fixed(m)?;
                m.push(float(double))?;
            } else {
                m.push(float(double))?;
                fixed(m)?;
            }
            let reg = Fp { double, n: d };
            write!(operands, "{reg},{reg},#{fraction}")?;
        }
        SimdFp::MoveImmediate { double, d, imm } => {
            m.push("VMOV")?;
            m.push(c)?;
            m.push(float(double))?;
            write!(operands, "{},", Fp { double, n: d })?;
            constant(operands, imm)?;
        }
        SimdFp::CoreSingle { to_core, n, rt } => {
            m.push("VMOV")?;
            m.push(c)?;
            let reg = Fp { double: false, n };
            if to_core {
                write!(operands, "{rt},{reg}")?;
            } else {
                write!(operands, "{reg},{rt}")?;
            }
        }
        SimdFp::CorePair {
            to_core,
            double,
            m: rm,
            rt,
            rt2,
        } => {
            m.push("VMOV")?;
            m.push(c)?;
            let mut registers = Buffer::new();
            if double {
                write!(registers, "D{rm}")?;
            } else {
                write!(registers, "S{rm},S{}", rm.wrapping_add(1))?;
            }
            let registers = registers.as_str();
            if to_core {
                write!(operands, "{rt},{rt2},{registers}")?;
            } else {
                write!(operands, "{registers},{rt},{rt2}")?;
            }
        }
        SimdFp::CoreScalar {
            to_core,
            size,
            unsigned,
            index,
            d,
            rt,
        } => {
            m.push("VMOV")?;
            m.push(c)?;
            let bits = bits(8, size);
            if to_core && size < 2 {
                data_type(m, Typing::Signed, unsigned, bits)?;
            } else {
                write!(m, ".{bits}")?;
            }
            if to_core {
                write!(operands, "{rt},D{d}[{index}]")?;
            } else {
                write!(operands, "D{d}[{index}],{rt}")?;
            }
        }
        SimdFp::CoreDuplicate { size, q, d, rt } => {
            m.push("VDUP")?;
            m.push(c)?;
            write!(m, ".{}", bits(8, size))?;
            write!(operands, "{},{rt}", Vector { q, d })?;
        }
        SimdFp::System { read, register, rt } => {
            m.push(if read { "VMRS" } else { "VMSR" })?;
            m.push(c)?;
            let name = register.name();
            match (read, rt == Reg::PC) {
                (true, true) => write!(operands, "APSR_nzcv,{name}")?,
                (true, false) => write!(operands, "{rt},{name}")?,
                (false, _) => write!(operands, "{name},{rt}")?,
            }
        }
        SimdFp::Transfer {
            load,
            double,
            d,
            rn,
            up,
            offset,
        } => {
            m.push(if load { "VLDR" } else { "VSTR" })?;
            m.push(c)?;
            let bytes = Signed {
                up,
                value: 4 * u32::from(offset),
            };
            write!(operands, "{},[{rn},#{bytes}]", Fp { double, n: d })?;
        }
        SimdFp::Multiple {
            load,
            double,
            d,
            rn,
            mode,
            writeback,
            words,
        } => {
            let registers = if double { words / 2 } else { words };
            let unknown = double && words & 1 != 0;
            let stack = rn == Reg::SP
                && writeback
                && !unknown
                && if load {
                    mode == BlockMode::IncrementAfter
                } else {
                    mode == BlockMode::DecrementBefore
                };
            if stack {
                m.push(if load { "VPOP" } else { "VPUSH" })?;
                m.push(c)?;
                return list(operands, double, d, registers);
            }
            let base = match (load, unknown) {
                (true, false) => "VLDM",
                (false, false) => "VSTM",
                (true, true) => "FLDM",
                (false, true) => "FSTM",
            };
            m.push(base)?;
            m.push(if mode == BlockMode::IncrementAfter {
                "IA"
            } else {
                "DB"
            })?;
            if unknown {
                m.push("X")?;
            }
            m.push(c)?;
            write!(operands, "{rn}{},", if writeback { "!" } else { "" })?;
            list(operands, double, d, registers)?;
        }
        SimdFp::Three {
            op,
            unsigned,
            size,
            q,
            d,
            n,
            m: rm,
        } => {
            let copy = op == SimdThreeOp::Vorr && n == rm;
            m.push(if copy { "VMOV" } else { op.name() })?;
            m.push(c)?;
            data_type(m, three_typing(op), unsigned, bits(8, size))?;
            let reg = |d| Vector { q, d };
            // A shift by register names the value before the shifts.
            let by_register = matches!(
                op,
                SimdThreeOp::Vshl | SimdThreeOp::Vqshl | SimdThreeOp::Vrshl | SimdThreeOp::Vqrshl
            );
            if copy {
                write!(operands, "{},{}", reg(d), reg(n))?;
            } else if by_register {
                write!(operands, "{},{},{}", reg(d), reg(rm), reg(n))?;
            } else {
                write!(operands, "{},{},{}", reg(d), reg(n), reg(rm))?;
            }
        }
        SimdFp::Long {
            op,
            unsigned,
            size,
            d,
            n,
            m: rm,
        } => {
            m.push(op.name())?;
            m.push(c)?;
            let bits = match op {
                SimdLongOp::Vmullp64 => 64,
                _ if op.width() == Width::Narrow => bits(16, size),
                _ => bits(8, size),
            };
            data_type(m, long_typing(op), unsigned, bits)?;
            match op.width() {
                Width::Long => write!(operands, "{},{},{}", qq(d), dq(n), dq(rm))?,
                Width::Wide => write!(operands, "{},{},{}", qq(d), qq(n), dq(rm))?,
                Width::Narrow => write!(operands, "{},{},{}", dq(d), qq(n), qq(rm))?,
            }
        }
        SimdFp::ByScalar {
            op,
            unsigned,
            size,
            q,
            d,
            n,
            m: rm,
            index,
        } => {
            m.push(op.name())?;
            m.push(c)?;
            data_type(m, scalar_typing(op), unsigned, bits(8, size))?;
            let (dest, first) = if op.long() {
                (qq(d), dq(n))
            } else {
                (Vector { q, d }, Vector { q, d: n })
            };
            write!(operands, "{dest},{first},D{rm}[{index}]")?;
        }
        SimdFp::Shift {
            op,
            unsigned,
            size,
            q,
            d,
            m: rm,
            amount,
        } => {
            m.push(op.name())?;
            m.push(c)?;
            match op {
                SimdShiftOp::VcvtFromFixed => {
                    m.push(".F32")?;
                    m.push(if unsigned { ".U32" } else { ".S32" })?;
                }
                SimdShiftOp::VcvtToFixed => {
                    m.push(if unsigned { ".U32" } else { ".S32" })?;
                    m.push(".F32")?;
                }
                _ => {
                    let bits = if op.narrow() {
                        bits(16, size)
                    } else {
                        bits(8, size)
                    };
                    data_type(m, shift_typing(op), unsigned, bits)?;
                }
            }
            if op.narrow() {
                write!(operands, "{},{},#{amount}", dq(d), qq(rm))?;
            } else if op == SimdShiftOp::Vmovl {
                write!(operands, "{},{}", qq(d), dq(rm))?;
            } else if op.long() {
                write!(operands, "{},{},#{amount}", qq(d), dq(rm))?;
            } else {
                write!(
                    operands,
                    "{},{},#{amount}",
                    Vector { q, d },
                    Vector { q, d: rm }
                )?;
            }
        }
        SimdFp::Misc {
            op,
            unsigned,
            size,
            q,
            d,
            m: rm,
        } => misc(m, operands, c, op, unsigned, size, q, d, rm)?,
        SimdFp::Immediate { cmode, q, d, imm } => {
            let op = SimdImmediateOp::of(cmode).unwrap_or(SimdImmediateOp::Vmov);
            m.push(op.name())?;
            m.push(c)?;
            write!(operands, "{},", Vector { q, d })?;
            modified_immediate(m, operands, cmode, imm)?;
        }
        SimdFp::Extract {
            q,
            d,
            n,
            m: rm,
            start,
        } => {
            m.push("VEXT")?;
            m.push(c)?;
            m.push(".8")?;
            let reg = |d| Vector { q, d };
            write!(operands, "{},{},{},#{start}", reg(d), reg(n), reg(rm))?;
        }
        SimdFp::Table {
            extend,
            length,
            d,
            n,
            m: rm,
        } => {
            m.push(if extend { "VTBX" } else { "VTBL" })?;
            m.push(c)?;
            m.push(".8")?;
            write!(operands, "D{d},")?;
            list(operands, true, n, length)?;
            write!(operands, ",D{rm}")?;
        }
        SimdFp::Duplicate {
            size,
            q,
            d,
            m: rm,
            index,
        } => {
            m.push("VDUP")?;
            m.push(c)?;
            write!(m, ".{}", bits(8, size))?;
            write!(operands, "{},D{rm}[{index}]", Vector { q, d })?;
        }
        SimdFp::Structure {
            load,
            shape,
            d,
            rn,
            rm,
        } => {
            write!(m, "{}{}", if load { "VLD" } else { "VST" }, shape.elements)?;
            m.push(c)?;
            write!(m, ".{}", bits(8, shape.size))?;
            structure_list(operands, d, shape)?;
            write!(operands, ",[{rn}")?;
            if shape.align > 1 {
                write!(operands, ":{}", 8 * u32::from(shape.align))?;
            }
            operands.push("]")?;
            match rm.number() {
                15 => {}
                13 => operands.push("!")?,
                _ => write!(operands, ",{rm}")?,
            }
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)] // The two-register form's fields, unpacked.
fn misc(
    m: &mut Buffer,
    operands: &mut Buffer,
    c: &str,
    op: SimdMiscOp,
    unsigned: bool,
    size: u8,
    q: bool,
    d: u8,
    rm: u8,
) -> fmt::Result {
    use SimdMiscOp as O;
    m.push(op.name())?;
    m.push(c)?;
    let sign = if unsigned { ".U32" } else { ".S32" };
    match op {
        O::VcvtToHalf => m.push(".F16.F32")?,
        O::VcvtFromHalf => m.push(".F32.F16")?,
        O::Vcvta | O::Vcvtn | O::Vcvtp | O::Vcvtm | O::VcvtToInteger => {
            m.push(sign)?;
            m.push(".F32")?;
        }
        O::VcvtToFloat => {
            m.push(".F32")?;
            m.push(sign)?;
        }
        O::Vrecpe | O::Vrsqrte => m.push(".U32")?,
        O::Vmovn | O::Vqmovun | O::Vqmovn => {
            data_type(m, misc_typing(op), unsigned, bits(16, size))?;
        }
        _ => data_type(m, misc_typing(op), unsigned, bits(8, size))?,
    }
    let reg = |d| Vector { q, d };
    match op {
        O::Vmovn | O::Vqmovun | O::Vqmovn | O::VcvtToHalf => {
            write!(operands, "{},{}", dq(d), qq(rm))
        }
        O::VcvtFromHalf => write!(operands, "{},{}", qq(d), dq(rm)),
        O::Vshll => write!(operands, "{},{},#{}", qq(d), dq(rm), bits(8, size)),
        O::Vcgt0
        | O::Vcge0
        | O::Vceq0
        | O::Vcle0
        | O::Vclt0
        | O::Vcgt0F
        | O::Vcge0F
        | O::Vceq0F
        | O::Vcle0F
        | O::Vclt0F => write!(operands, "{},{},#0", reg(d), reg(rm)),
        _ => write!(operands, "{},{}", reg(d), reg(rm)),
    }
}
