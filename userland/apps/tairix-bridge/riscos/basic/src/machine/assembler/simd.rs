//! The floating-point and Advanced SIMD instructions, in UAL as RISC OS 5's
//! BASIC assembler reads it: a VFP instruction's condition before its data
//! types, an Advanced SIMD one taking none, every register named rather than
//! numbered by an expression. Mnemonics are the vocabulary's own names.
//!
//! Where BASIC departs from UAL it is followed: `VRINT` names its type
//! twice, a VFP fixed-point `VCVT` names its register once, `VRSHL` takes
//! its operands as `VADD` does, and half-precision Advanced SIMD conversions,
//! `VMULL.P64` and the Cryptographic Extension are refused.

use tairix_riscos_arm::a32;
use tairix_riscos_arm::cond::Condition;
use tairix_riscos_arm::fpa::Format;
use tairix_riscos_arm::instruction::{
    BlockMode, ConvertRounding, Instruction, Op, SimdFp, SimdImmediateOp, SimdLongOp, SimdMiscOp,
    SimdScalarOp, SimdShiftOp, SimdThreeOp, Structure, StructureLanes, Typing, VfpDyadicOp,
    VfpMonadicOp, Width,
};
use tairix_riscos_arm::reg::Reg;
use tairix_riscos_arm::vfp::{expand_immediate, expand_modified_immediate, System};

use super::core::start;
use super::mnemonic::Word;
use super::parse::{Parse, Parser, Want};
use crate::machine::in_name;
use crate::messages::Error;
use crate::value::Number;

/// The mnemonics a condition may follow at once, as VFP's take one: tried
/// ahead of any longer mnemonic they begin.
const CONDITIONAL: [&str; 33] = [
    "VMLA", "VMLS", "VNMLA", "VNMLS", "VMUL", "VNMUL", "VADD", "VSUB", "VDIV", "VFMA", "VFMS",
    "VFNMA", "VFNMS", "VMOV", "VABS", "VNEG", "VSQRT", "VRINTR", "VRINTZ", "VRINTX", "VCMP",
    "VCMPE", "VCVT", "VCVTR", "VCVTB", "VCVTT", "VMRS", "VMSR", "VLDR", "VSTR", "VPUSH", "VPOP",
    "VDUP",
];

/// The mnemonics no operation of the vocabulary names on its own.
const OTHERS: [&str; 24] = [
    "VSEL", "VCMP", "VCMPE", "VCVTB", "VCVTT", "VDUP", "VMRS", "VMSR", "VLDR", "VSTR", "VLDM",
    "VSTM", "VPUSH", "VPOP", "VEXT", "VTBL", "VTBX", "VLD1", "VLD2", "VLD3", "VLD4", "VST1",
    "VST2", "VST3",
];

/// `VST4`, past [`OTHERS`]'s room.
const LAST: &str = "VST4";

/// Every floating-point and Advanced SIMD mnemonic.
fn names() -> impl Iterator<Item = &'static str> {
    VfpDyadicOp::ALL
        .iter()
        .map(|op| op.name())
        .chain(VfpMonadicOp::ALL.iter().map(|op| op.name()))
        .chain(ConvertRounding::ALL.iter().map(|op| op.name()))
        .chain(SimdThreeOp::ALL.iter().map(|op| op.name()))
        .chain(SimdLongOp::ALL.iter().map(|op| op.name()))
        .chain(SimdScalarOp::ALL.iter().map(|op| op.name()))
        .chain(SimdShiftOp::ALL.iter().map(|op| op.name()))
        .chain(SimdMiscOp::ALL.iter().map(|op| op.name()))
        .chain(SimdImmediateOp::ALL.iter().map(|op| op.name()))
        .chain(OTHERS)
        .chain([LAST])
        .filter(|name| name.starts_with('V'))
}

/// The mnemonic `word` begins with and the condition after it.
pub(super) fn matched(word: &mut Word) -> Option<(&'static str, Condition)> {
    let conditioned = CONDITIONAL
        .iter()
        .filter(|&&name| word.followed_by_condition(name))
        .max_by_key(|name| name.len());
    if let Some(&name) = conditioned {
        word.take(name);
        let cond = word.condition().unwrap_or(Condition::Al);
        return Some((name, cond));
    }
    let name = names()
        .filter(|&name| word.starts_with(name))
        .max_by_key(|name| name.len())?;
    word.take(name);
    Some((name, Condition::Al))
}

/// A data type: its kind's letter, none for a size alone, and its size.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Dt {
    kind: Option<u8>,
    bits: u32,
}

impl Dt {
    const F32: Self = Self {
        kind: Some(b'F'),
        bits: 32,
    };
    const F64: Self = Self {
        kind: Some(b'F'),
        bits: 64,
    };
    const F16: Self = Self {
        kind: Some(b'F'),
        bits: 16,
    };

    /// Whether it is a floating-point type.
    const fn float(self) -> bool {
        matches!(self.kind, Some(b'F'))
    }

    /// The lane size it names: 8 << size bits.
    fn size(self) -> Parse<u8> {
        match self.bits {
            8 => Ok(0),
            16 => Ok(1),
            32 => Ok(2),
            64 => Ok(3),
            _ => Err(Error::Syntax.into()),
        }
    }

    /// The signedness and size an operation typed by `typing` takes from it.
    fn lanes(self, typing: Typing) -> Option<(bool, u8)> {
        let size = self.size().ok()?;
        let unsigned = self.kind == Some(b'U');
        let fits = match typing {
            Typing::Untyped => true,
            Typing::Size => matches!(self.kind, None | Some(b'I' | b'S' | b'U')),
            Typing::Integer => matches!(self.kind, Some(b'I' | b'S' | b'U')),
            Typing::Signed => matches!(self.kind, Some(b'S' | b'U')),
            Typing::SignedOnly => self.kind == Some(b'S'),
            Typing::Polynomial => self.kind == Some(b'P'),
            Typing::Float => self == Self::F32,
        };
        fits.then_some((typing == Typing::Signed && unsigned, size))
    }
}

/// The data types after a mnemonic, at most two.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
struct Types([Option<Dt>; 2]);

impl Types {
    const fn one(self) -> Option<Dt> {
        match self.0 {
            [Some(dt), None] => Some(dt),
            _ => None,
        }
    }

    const fn two(self) -> Option<(Dt, Dt)> {
        match self.0 {
            [Some(a), Some(b)] => Some((a, b)),
            _ => None,
        }
    }

    const fn none(self) -> bool {
        matches!(self.0, [None, None])
    }
}

/// The data types `word` ends with; anything else after them is a syntax
/// error.
fn data_types(word: &mut Word) -> Parse<Types> {
    let mut types = Types::default();
    for slot in &mut types.0 {
        if !word.take(".") {
            break;
        }
        let kind = match word.peek() {
            Some(k @ (b'I' | b'S' | b'U' | b'F' | b'P')) => {
                word.bump();
                Some(k)
            }
            _ => None,
        };
        let mut bits = 0u32;
        let mut any = false;
        while let Some(d @ b'0'..=b'9') = word.peek() {
            word.bump();
            bits = bits.saturating_mul(10).saturating_add(u32::from(d - b'0'));
            any = true;
        }
        if !any {
            return Err(Error::Syntax.into());
        }
        *slot = Some(Dt { kind, bits });
    }
    if word.peek().is_some() {
        return Err(Error::Syntax.into());
    }
    Ok(types)
}

/// An operand.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Arg {
    S(u8),
    D(u8),
    /// A quadword register, by its number.
    Q(u8),
    /// A doubleword register's lane; none for all of them.
    Scalar {
        d: u8,
        index: Option<u8>,
    },
    Core(Reg),
    Imm(Number),
    Apsr,
    System(System),
}

/// The system registers `VMRS` and `VMSR` name.
const SYSTEMS: [System; 6] = [
    System::Fpsid,
    System::Fpscr,
    System::Mvfr2,
    System::Mvfr1,
    System::Mvfr0,
    System::Fpexc,
];

/// An operand: a register by name, a lane of one, a core register, `#` and
/// an expression, or a system register.
fn arg(p: &mut Parser<'_>) -> Parse<Arg> {
    arg_in(p, Error::Syntax)
}

/// An operand, as [`arg`] reads one, `run_on` the error for a single's name
/// running on past the two digits BASIC reads of its number.
fn arg_in(p: &mut Parser<'_>, run_on: Error) -> Parse<Arg> {
    p.spaces();
    if p.take(b'#') {
        return Ok(Arg::Imm(p.number(Want::Expression)?));
    }
    let bank = p.peek().to_ascii_uppercase();
    if matches!(bank, b'S' | b'D' | b'Q') && p.byte(1).is_ascii_digit() {
        p.advance(1);
        let mut n = 0u32;
        let mut digits = 0;
        while p.peek().is_ascii_digit() {
            if bank == b'S' && digits == 2 {
                return Err(run_on.into());
            }
            n = n
                .saturating_mul(10)
                .saturating_add(u32::from(p.peek() - b'0'));
            p.advance(1);
            digits += 1;
        }
        if in_name(p.peek()) {
            return Err(Error::NoSuchVariable.into());
        }
        let most = if bank == b'Q' { 16 } else { 32 };
        let n = u8::try_from(n)
            .ok()
            .filter(|&n| u32::from(n) < most)
            .ok_or(Error::BadRegister)?;
        return Ok(match bank {
            b'S' => Arg::S(n),
            b'Q' => Arg::Q(n),
            _ if p.peek() == b'[' => {
                p.advance(1);
                let index = if p.take(b']') {
                    None
                } else {
                    let i = lane(p)?;
                    p.expect(b']', Error::MissingSquare)?;
                    Some(i)
                };
                Arg::Scalar { d: n, index }
            }
            _ => Arg::D(n),
        });
    }
    if let Some(reg) = p.register_name(true) {
        return Ok(Arg::Core(reg));
    }
    if p.whole_word("APSR_NZCV") {
        return Ok(Arg::Apsr);
    }
    for system in SYSTEMS {
        if p.whole_word(system.name()) {
            return Ok(Arg::System(system));
        }
    }
    Err(Error::NoSuchVariable.into())
}

/// A lane's index: its digits.
fn lane(p: &mut Parser<'_>) -> Parse<u8> {
    p.spaces();
    if !p.peek().is_ascii_digit() {
        return Err(Error::Syntax.into());
    }
    let mut n = 0u32;
    while p.peek().is_ascii_digit() {
        n = n
            .saturating_mul(10)
            .saturating_add(u32::from(p.peek() - b'0'));
        p.advance(1);
    }
    u8::try_from(n).map_err(|_| Error::BadRegister.into())
}

/// The operands to the statement's end, at most four, each read as
/// [`arg_in`] reads one.
fn args(p: &mut Parser<'_>, run_on: Error) -> Parse<([Option<Arg>; 4], usize)> {
    let mut args = [None; 4];
    let mut count = 0;
    loop {
        let a = arg_in(p, run_on)?;
        let slot = args.get_mut(count).ok_or(Error::Syntax)?;
        *slot = Some(a);
        count += 1;
        if !p.take(b',') {
            return Ok((args, count));
        }
    }
}

/// A register's shape, as operands are matched.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Shape {
    S,
    D,
    Q,
    Scalar,
    Core,
    Imm,
    Other,
}

const fn shape(arg: &Arg) -> Shape {
    match arg {
        Arg::S(_) => Shape::S,
        Arg::D(_) => Shape::D,
        Arg::Q(_) => Shape::Q,
        Arg::Scalar { .. } => Shape::Scalar,
        Arg::Core(_) => Shape::Core,
        Arg::Imm(_) => Shape::Imm,
        Arg::Apsr | Arg::System(_) => Shape::Other,
    }
}

/// A register's number as an encoding names it: a quadword's first double.
const fn number(arg: &Arg) -> u8 {
    match *arg {
        Arg::S(n) | Arg::D(n) | Arg::Scalar { d: n, .. } => n,
        Arg::Q(n) => 2 * n,
        _ => 0,
    }
}

/// The word of the floating-point or Advanced SIMD instruction `name`, under
/// `cond`, whose word `word` has been read to past it.
pub(super) fn instruction(
    p: &mut Parser<'_>,
    word: &mut Word,
    name: &str,
    cond: Condition,
) -> Parse<u32> {
    let (cond, mode) = if matches!(name, "VLDM" | "VSTM") {
        let mode = if word.take("IA") {
            BlockMode::IncrementAfter
        } else if word.take("DB") {
            BlockMode::DecrementBefore
        } else {
            return Err(Error::Syntax.into());
        };
        (word.condition().unwrap_or(Condition::Al), Some(mode))
    } else {
        (cond, None)
    };
    let select = if name == "VSEL" {
        let sel = word.condition().ok_or(Error::Syntax)?;
        if !matches!(
            sel,
            Condition::Eq | Condition::Ge | Condition::Gt | Condition::Vs
        ) {
            return Err(Error::Syntax.into());
        }
        Some(sel)
    } else {
        None
    };
    let types = data_types(word)?;
    start(p, word)?;
    let op = form(p, name, types, mode, select)?;
    let unconditional = a32::unconditional_op(&Op::SimdFp(op));
    let cond = match (unconditional, cond) {
        (true, Condition::Al) => Condition::Nv,
        (true, _) => return Err(Error::Syntax.into()),
        (false, cond) => cond,
    };
    a32::encode(&Instruction {
        cond,
        op: Op::SimdFp(op),
    })
    .map_err(|_| Error::Syntax.into())
}

/// The operation a mnemonic, its types and its operands spell.
fn form(
    p: &mut Parser<'_>,
    name: &str,
    types: Types,
    mode: Option<BlockMode>,
    select: Option<Condition>,
) -> Parse<SimdFp> {
    match name {
        "VLDR" | "VSTR" => return transfer(p, name == "VLDR", types),
        "VLDM" | "VSTM" => return multiple(p, name == "VLDM", types, mode),
        "VPUSH" | "VPOP" => return stack(p, name == "VPOP", types),
        "VLD1" | "VLD2" | "VLD3" | "VLD4" | "VST1" | "VST2" | "VST3" | "VST4" => {
            return structure(p, name, types);
        }
        "VTBL" | "VTBX" => return table(p, name == "VTBX", types),
        _ => {}
    }
    // BASIC's VMOV reads a single's name run on as a variable's.
    let run_on = if name == "VMOV" {
        Error::NoSuchVariable
    } else {
        Error::Syntax
    };
    let (args, count) = args(p, run_on)?;
    let args = &args[..count];
    let shapes = {
        let mut shapes = [Shape::Other; 4];
        for (s, a) in shapes.iter_mut().zip(args.iter().flatten()) {
            *s = shape(a);
        }
        shapes
    };
    let shapes = &shapes[..count];
    let regs: [u8; 4] =
        core::array::from_fn(|i| args.get(i).copied().flatten().map_or(0, |a| number(&a)));
    let arg_at = |i: usize| args.get(i).copied().flatten();
    match name {
        "VSEL" => {
            let double = vfp_float(types)?;
            vfp_shapes(shapes, double, 3)?;
            let cond = select.ok_or(Error::Syntax)?;
            return Ok(SimdFp::Select {
                cond,
                double,
                d: regs[0],
                n: regs[1],
                m: regs[2],
            });
        }
        "VCMP" | "VCMPE" => return compare(name == "VCMPE", types, shapes, regs, arg_at(1), p),
        "VMRS" | "VMSR" => return system(name == "VMRS", types, args),
        "VMOV" => return vmov(p, types, shapes, regs, args),
        "VDUP" => return vdup(types, shapes, regs, args),
        "VEXT" => return vext(p, types, shapes, regs, arg_at(3)),
        "VCVT" | "VCVTR" | "VCVTA" | "VCVTN" | "VCVTP" | "VCVTM" | "VCVTB" | "VCVTT" => {
            return convert(
                p,
                name,
                types,
                shapes,
                regs,
                arg_at(count.saturating_sub(1)),
            );
        }
        _ => {}
    }
    if let Some(op) = VfpDyadicOp::ALL.iter().find(|op| op.name() == name) {
        if let Some(double) = vfp_types(types, shapes) {
            vfp_shapes(shapes, double, 3)?;
            return Ok(SimdFp::Dyadic {
                op: *op,
                double,
                d: regs[0],
                n: regs[1],
                m: regs[2],
            });
        }
    }
    if let Some(op) = VfpMonadicOp::ALL.iter().find(|op| op.name() == name) {
        let read_types = if name.starts_with("VRINT") {
            match types.two() {
                Some((a, b)) if a == b && a.float() => Types([Some(a), None]),
                _ => return Err(Error::Syntax.into()),
            }
        } else {
            types
        };
        if let Some(double) = vfp_types(read_types, shapes) {
            vfp_shapes(shapes, double, 2)?;
            return Ok(SimdFp::Monadic {
                op: *op,
                double,
                d: regs[0],
                m: regs[1],
            });
        }
        if name.starts_with("VRINT") {
            return misc(name, read_types, shapes, regs, args);
        }
    }
    neon(p, name, types, shapes, regs, args)
}

/// The precision a VFP data operation's one type names: none where the type
/// is not a VFP one or the registers are Advanced SIMD's.
fn vfp_types(types: Types, shapes: &[Shape]) -> Option<bool> {
    match (types.one()?, shapes.first()?) {
        (Dt::F64, Shape::D) => Some(true),
        (Dt::F32, Shape::S) => Some(false),
        _ => None,
    }
}

/// The precision a VFP-only operation's type names.
fn vfp_float(types: Types) -> Parse<bool> {
    match types.one() {
        Some(Dt::F64) => Ok(true),
        Some(Dt::F32) => Ok(false),
        _ => Err(Error::Syntax.into()),
    }
}

/// `count` registers, each single or double as `double` says.
fn vfp_shapes(shapes: &[Shape], double: bool, count: usize) -> Parse<()> {
    let bank = if double { Shape::D } else { Shape::S };
    if shapes.len() == count && shapes.iter().all(|&s| s == bank) {
        Ok(())
    } else {
        Err(Error::Syntax.into())
    }
}

/// `VCMP` and `VCMPE`: with a register or zero.
fn compare(
    signalling: bool,
    types: Types,
    shapes: &[Shape],
    regs: [u8; 4],
    second: Option<Arg>,
    p: &mut Parser<'_>,
) -> Parse<SimdFp> {
    let double = vfp_float(types)?;
    let bank = if double { Shape::D } else { Shape::S };
    match shapes {
        [first, second_shape] if *first == bank && *second_shape == bank => Ok(SimdFp::Compare {
            signalling,
            double,
            d: regs[0],
            m: Some(regs[1]),
        }),
        [first, Shape::Imm] if *first == bank => {
            let Some(Arg::Imm(zero)) = second else {
                return Err(Error::Syntax.into());
            };
            if p.arith.double_bits(zero) & !(1 << 63) != 0 {
                return Err(Error::Syntax.into());
            }
            Ok(SimdFp::Compare {
                signalling,
                double,
                d: regs[0],
                m: None,
            })
        }
        _ => Err(Error::Syntax.into()),
    }
}

/// `VMRS` and `VMSR`: APSR's flags or a core register, and a system
/// register.
fn system(read: bool, types: Types, args: &[Option<Arg>]) -> Parse<SimdFp> {
    if !types.none() {
        return Err(Error::Syntax.into());
    }
    match (read, args) {
        (true, [Some(Arg::Apsr), Some(Arg::System(register))]) => Ok(SimdFp::System {
            read,
            register: *register,
            rt: Reg::PC,
        }),
        (true, [Some(Arg::Core(rt)), Some(Arg::System(register))])
        | (false, [Some(Arg::System(register)), Some(Arg::Core(rt))]) => Ok(SimdFp::System {
            read,
            register: *register,
            rt: *rt,
        }),
        _ => Err(Error::Syntax.into()),
    }
}

/// `VMOV`: between registers of either kind, of an immediate, or between a
/// core register and a lane.
#[expect(clippy::too_many_lines, reason = "one arm per form")]
fn vmov(
    p: &mut Parser<'_>,
    types: Types,
    shapes: &[Shape],
    regs: [u8; 4],
    args: &[Option<Arg>],
) -> Parse<SimdFp> {
    use Shape as S;
    if let Some(dt) = types.one() {
        // A single's type on doubles or quadwords is Advanced SIMD's.
        let simd =
            dt == Dt::F32 && matches!(shapes, [S::D | S::Q, S::Imm] | [S::D, S::D] | [S::Q, S::Q]);
        if dt.float() && !simd {
            // BASIC reads the type's register first, and a core register
            // after a single as an unknown variable.
            let bank = if dt == Dt::F64 { S::D } else { S::S };
            if shapes.first() != Some(&bank) {
                return Err(Error::Syntax.into());
            }
            if shapes.contains(&S::Core) {
                return Err(if bank == S::S {
                    Error::NoSuchVariable
                } else {
                    Error::Syntax
                }
                .into());
            }
            if matches!(
                (dt, shapes),
                (Dt::F32, [S::S, S::Imm]) | (Dt::F64, [S::D, S::Imm])
            ) {
                let Some(Arg::Imm(value)) = args[1] else {
                    return Err(Error::Syntax.into());
                };
                let imm = vfp_constant(p, value)?;
                return Ok(SimdFp::MoveImmediate {
                    double: dt == Dt::F64,
                    d: regs[0],
                    imm,
                });
            }
            if let Some(double) = vfp_types(types, shapes) {
                vfp_shapes(shapes, double, 2)?;
                return Ok(SimdFp::Monadic {
                    op: VfpMonadicOp::Vmov,
                    double,
                    d: regs[0],
                    m: regs[1],
                });
            }
        }
        if let [S::D | S::Q, S::Imm] = shapes {
            return immediate(
                p,
                SimdImmediateOp::Vmov,
                dt,
                shapes[0] == S::Q,
                regs[0],
                args[1],
            );
        }
        if matches!(shapes.first(), Some(S::S | S::D)) && shapes.contains(&S::Core) {
            return Err(Error::NoSuchVariable.into());
        }
    }
    if let [S::S, S::Core, S::Core] = shapes {
        return Err(Error::NoSuchVariable.into());
    }
    match shapes {
        [S::Core, S::S] | [S::S, S::Core] if types.none() => {
            let to_core = shapes[0] == S::Core;
            let (n, rt) = if to_core {
                (regs[1], core(args[0])?)
            } else {
                (regs[0], core(args[1])?)
            };
            Ok(SimdFp::CoreSingle { to_core, n, rt })
        }
        [S::Core, S::Core, S::D] if types.none() => Ok(SimdFp::CorePair {
            to_core: true,
            double: true,
            m: regs[2],
            rt: core(args[0])?,
            rt2: core(args[1])?,
        }),
        [S::D, S::Core, S::Core] if types.none() => Ok(SimdFp::CorePair {
            to_core: false,
            double: true,
            m: regs[0],
            rt: core(args[1])?,
            rt2: core(args[2])?,
        }),
        [S::Core, S::Core, S::S, S::S] if types.none() && regs[3] == regs[2].wrapping_add(1) => {
            Ok(SimdFp::CorePair {
                to_core: true,
                double: false,
                m: regs[2],
                rt: core(args[0])?,
                rt2: core(args[1])?,
            })
        }
        [S::S, S::S, S::Core, S::Core] if types.none() && regs[1] == regs[0].wrapping_add(1) => {
            Ok(SimdFp::CorePair {
                to_core: false,
                double: false,
                m: regs[0],
                rt: core(args[2])?,
                rt2: core(args[3])?,
            })
        }
        [S::D, S::D] | [S::Q, S::Q] if types.none() || types.one() == Some(Dt::F32) => {
            Ok(SimdFp::Three {
                op: SimdThreeOp::Vorr,
                unsigned: false,
                size: 0,
                q: shapes[0] == S::Q,
                d: regs[0],
                n: regs[1],
                m: regs[1],
            })
        }
        [S::Core, S::Scalar] | [S::Scalar, S::Core] => {
            let to_core = shapes[0] == S::Core;
            let (scalar, rt) = if to_core {
                (args[1], core(args[0])?)
            } else {
                (args[0], core(args[1])?)
            };
            let Some(Arg::Scalar {
                d,
                index: Some(index),
            }) = scalar
            else {
                return Err(Error::Syntax.into());
            };
            let (size, unsigned) = match types.one() {
                None => (2, false),
                Some(Dt { kind: None, bits }) => (Dt { kind: None, bits }.size()?, false),
                Some(Dt {
                    kind: Some(k @ (b'S' | b'U')),
                    bits: bits @ (8 | 16),
                }) if to_core => (Dt { kind: None, bits }.size()?, k == b'U'),
                _ => return Err(Error::Syntax.into()),
            };
            if index >= 8 >> size {
                return Err(Error::BadRegister.into());
            }
            Ok(SimdFp::CoreScalar {
                to_core,
                size,
                unsigned,
                index,
                d,
                rt,
            })
        }
        _ => Err(Error::Syntax.into()),
    }
}

/// A core register operand.
fn core(arg: Option<Arg>) -> Parse<Reg> {
    match arg {
        Some(Arg::Core(reg)) => Ok(reg),
        _ => Err(Error::BadRegister.into()),
    }
}

/// VFP's eight-bit constant for `value`, which must be one exactly: each is
/// a single's value as well as a double's.
fn vfp_constant(p: &mut Parser<'_>, value: Number) -> Parse<u8> {
    let bits = p.arith.double_bits(value);
    (0..=255u8)
        .find(|&imm| expand_immediate(imm, true) == bits)
        .ok_or_else(|| Error::BadImmediate.into())
}

/// An Advanced SIMD modified immediate: the first encoding of `op` whose
/// expansion is `value` at `dt`'s width, the type's own encodings tried
/// before narrower types'.
fn immediate(
    p: &mut Parser<'_>,
    op: SimdImmediateOp,
    dt: Dt,
    q: bool,
    d: u8,
    value: Option<Arg>,
) -> Parse<SimdFp> {
    let Some(Arg::Imm(number)) = value else {
        return Err(Error::Syntax.into());
    };
    let pattern = if dt.float() {
        // Only VMOV takes a floating-point type with an immediate.
        if dt != Dt::F32 || op != SimdImmediateOp::Vmov {
            return Err(Error::Syntax.into());
        }
        let [bits, ..] = p.arith.fpa_words(number, Format::Single)?;
        u64::from(bits) << 32 | u64::from(bits)
    } else {
        let n = u64::from(p.arith.int(number)?.cast_unsigned());
        match dt.bits {
            8 => (n & 0xFF) * 0x0101_0101_0101_0101,
            16 => (n & 0xFFFF) * 0x0001_0001_0001_0001,
            32 => (n & 0xFFFF_FFFF) << 32 | (n & 0xFFFF_FFFF),
            64 => n,
            _ => return Err(Error::Syntax.into()),
        }
    };
    let widths: &[u32] = match dt.bits {
        64 => &[64],
        32 => &[32, 16, 8],
        16 => &[16, 8],
        _ => &[8],
    };
    for &width in widths {
        for cmode in 0..32u8 {
            if SimdImmediateOp::of(cmode) != Some(op) || cmode_width(cmode) != Some(width) {
                continue;
            }
            if let Some(imm) =
                (0..=255u8).find(|&imm| expand_modified_immediate(cmode, imm) == pattern)
            {
                return Ok(SimdFp::Immediate { cmode, q, d, imm });
            }
        }
    }
    // A single's bits no integer encoding spells are the floating-point
    // encoding's, as BASIC tries them last.
    if dt.float() {
        if let Some(imm) =
            (0..=255u8).find(|&imm| expand_modified_immediate(FLOAT_CMODE, imm) == pattern)
        {
            return Ok(SimdFp::Immediate {
                cmode: FLOAT_CMODE,
                q,
                d,
                imm,
            });
        }
    }
    Err(Error::BadImmediate.into())
}

/// The modified immediate's floating-point encoding.
const FLOAT_CMODE: u8 = 15;

/// The lane width a modified immediate's encoding is for.
const fn cmode_width(cmode: u8) -> Option<u32> {
    Some(match cmode & 0xF {
        0..=7 | 12 | 13 => 32,
        8..=11 => 16,
        14 if cmode & 0x10 == 0 => 8,
        14 => 64,
        _ => return None,
    })
}

/// `VDUP`: of a core register, or of a lane.
fn vdup(types: Types, shapes: &[Shape], regs: [u8; 4], args: &[Option<Arg>]) -> Parse<SimdFp> {
    let dt = types.one().ok_or(Error::Syntax)?;
    if dt.kind.is_some_and(|k| k == b'F' || k == b'P') {
        return Err(Error::Syntax.into());
    }
    let size = dt.size()?;
    match shapes {
        [Shape::D | Shape::Q, Shape::Core] if size < 3 => Ok(SimdFp::CoreDuplicate {
            size,
            q: shapes[0] == Shape::Q,
            d: regs[0],
            rt: core(args[1])?,
        }),
        [Shape::D | Shape::Q, Shape::Scalar] => {
            let Some(Arg::Scalar {
                d: m,
                index: Some(index),
            }) = args[1]
            else {
                return Err(Error::Syntax.into());
            };
            Ok(SimdFp::Duplicate {
                size,
                q: shapes[0] == Shape::Q,
                d: regs[0],
                m,
                index,
            })
        }
        _ => Err(Error::Syntax.into()),
    }
}

/// `VEXT`: the start in lanes of the type's size.
fn vext(
    p: &mut Parser<'_>,
    types: Types,
    shapes: &[Shape],
    regs: [u8; 4],
    start: Option<Arg>,
) -> Parse<SimdFp> {
    let dt = types.one().ok_or(Error::Syntax)?;
    let lanes = dt.size()?;
    let q = match shapes {
        [Shape::D, Shape::D, Shape::D, Shape::Imm] => false,
        [Shape::Q, Shape::Q, Shape::Q, Shape::Imm] => true,
        _ => return Err(Error::Syntax.into()),
    };
    let Some(Arg::Imm(n)) = start else {
        return Err(Error::Syntax.into());
    };
    let index = p.arith.int(n)?;
    let start = u8::try_from(index)
        .ok()
        .and_then(|i| i.checked_shl(u32::from(lanes)))
        .filter(|&s| s < if q { 16 } else { 8 })
        .ok_or(Error::Syntax)?;
    Ok(SimdFp::Extract {
        q,
        d: regs[0],
        n: regs[1],
        m: regs[2],
        start,
    })
}

/// The conversions: precision, integer, fixed point and half precision, in
/// VFP and Advanced SIMD.
fn convert(
    p: &mut Parser<'_>,
    name: &str,
    types: Types,
    shapes: &[Shape],
    regs: [u8; 4],
    last: Option<Arg>,
) -> Parse<SimdFp> {
    use Shape as S;
    let (to, from) = types.two().ok_or(Error::Syntax)?;
    let integer = |dt: Dt| matches!(dt.kind, Some(b'S' | b'U')) && dt.bits == 32;
    let rounding = ConvertRounding::ALL
        .iter()
        .find(|r| r.name() == name)
        .copied();
    if matches!(name, "VCVTB" | "VCVTT") {
        return half(name == "VCVTT", (to, from), shapes, regs);
    }
    let rounding = rounding.ok_or(Error::Syntax)?;
    if let ((Dt::F64, Dt::F32), [S::D, S::S]) | ((Dt::F32, Dt::F64), [S::S, S::D]) =
        ((to, from), shapes)
    {
        if rounding == ConvertRounding::Zero {
            return Ok(SimdFp::Precision {
                to_double: to == Dt::F64,
                d: regs[0],
                m: regs[1],
            });
        }
    }
    if integer(to) && from.float() {
        let unsigned = to.kind == Some(b'U');
        if let Some(conversion) = to_integer(p, rounding, unsigned, from, shapes, regs, last)? {
            return Ok(conversion);
        }
    }
    if to.float() && integer(from) && rounding == ConvertRounding::Zero {
        let unsigned = from.kind == Some(b'U');
        if let Some(conversion) = from_integer(p, unsigned, to, shapes, regs, last)? {
            return Ok(conversion);
        }
    }
    let fixed = |dt: Dt| matches!(dt.kind, Some(b'S' | b'U')) && matches!(dt.bits, 16 | 32);
    if rounding == ConvertRounding::Zero
        && ((to.float() && fixed(from)) || (fixed(to) && from.float()))
    {
        let to_fixed = fixed(to);
        let (float, point) = if to_fixed { (from, to) } else { (to, from) };
        let double = match (float, shapes) {
            (Dt::F64, [S::D, S::Imm]) => true,
            (Dt::F32, [S::S, S::Imm]) => false,
            _ => return Err(Error::Syntax.into()),
        };
        let word = point.bits == 32;
        let (most, fewest) = if word { (32, 1) } else { (16, 0) };
        let fraction = fraction(p, last, most, fewest)?;
        return Ok(SimdFp::Fixed {
            to_fixed,
            unsigned: point.kind == Some(b'U'),
            word,
            double,
            d: regs[0],
            fraction,
        });
    }
    Err(Error::Syntax.into())
}

/// A conversion from floating point to a 32-bit integer, VFP's or Advanced
/// SIMD's; none where `from` and `shapes` name neither.
fn to_integer(
    p: &mut Parser<'_>,
    rounding: ConvertRounding,
    unsigned: bool,
    from: Dt,
    shapes: &[Shape],
    regs: [u8; 4],
    last: Option<Arg>,
) -> Parse<Option<SimdFp>> {
    use Shape as S;
    let (d, m) = (regs[0], regs[1]);
    let q = shapes.first() == Some(&S::Q);
    Ok(Some(match (from, shapes) {
        (Dt::F32, [S::S, S::S]) | (Dt::F64, [S::S, S::D]) => SimdFp::ToInteger {
            rounding,
            unsigned,
            double: from == Dt::F64,
            d,
            m,
        },
        (Dt::F32, [S::D, S::D] | [S::Q, S::Q]) => {
            let op = match rounding {
                ConvertRounding::Zero => SimdMiscOp::VcvtToInteger,
                ConvertRounding::Away => SimdMiscOp::Vcvta,
                ConvertRounding::Nearest => SimdMiscOp::Vcvtn,
                ConvertRounding::Plus => SimdMiscOp::Vcvtp,
                ConvertRounding::Minus => SimdMiscOp::Vcvtm,
                ConvertRounding::Fpscr => return Err(Error::Syntax.into()),
            };
            SimdFp::Misc {
                op,
                unsigned,
                size: 2,
                q,
                d,
                m,
            }
        }
        (Dt::F32, [S::D, S::D, S::Imm] | [S::Q, S::Q, S::Imm])
            if rounding == ConvertRounding::Zero =>
        {
            let amount = fraction(p, last, 32, 1)?;
            SimdFp::Shift {
                op: SimdShiftOp::VcvtToFixed,
                unsigned,
                size: 2,
                q,
                d,
                m,
                amount,
            }
        }
        _ => return Ok(None),
    }))
}

/// A conversion from a 32-bit integer to floating point, VFP's or Advanced
/// SIMD's; none where `to` and `shapes` name neither.
fn from_integer(
    p: &mut Parser<'_>,
    unsigned: bool,
    to: Dt,
    shapes: &[Shape],
    regs: [u8; 4],
    last: Option<Arg>,
) -> Parse<Option<SimdFp>> {
    use Shape as S;
    let (d, m) = (regs[0], regs[1]);
    let q = shapes.first() == Some(&S::Q);
    // BASIC lays Advanced SIMD's conversions from unsigned integers down as
    // from signed ones, but for doubleword ones to floating point.
    Ok(Some(match (to, shapes) {
        (Dt::F32, [S::S, S::S]) | (Dt::F64, [S::D, S::S]) => SimdFp::FromInteger {
            unsigned,
            double: to == Dt::F64,
            d,
            m,
        },
        (Dt::F32, [S::D, S::D] | [S::Q, S::Q]) => SimdFp::Misc {
            op: SimdMiscOp::VcvtToFloat,
            unsigned: unsigned && !q,
            size: 2,
            q,
            d,
            m,
        },
        (Dt::F32, [S::D, S::D, S::Imm] | [S::Q, S::Q, S::Imm]) => {
            let amount = fraction(p, last, 32, 1)?;
            SimdFp::Shift {
                op: SimdShiftOp::VcvtFromFixed,
                unsigned: false,
                size: 2,
                q,
                d,
                m,
                amount,
            }
        }
        _ => return Ok(None),
    }))
}

/// `VCVTB` or, `top`, `VCVTT`: between half precision, in a single's lower or
/// upper half, and single or double precision.
fn half(top: bool, types: (Dt, Dt), shapes: &[Shape], regs: [u8; 4]) -> Parse<SimdFp> {
    use Shape as S;
    let (to_half, double) = match (types, shapes) {
        ((Dt::F32, Dt::F16), [S::S, S::S]) => (false, false),
        ((Dt::F16, Dt::F32), [S::S, S::S]) => (true, false),
        ((Dt::F64, Dt::F16), [S::D, S::S]) => (false, true),
        ((Dt::F16, Dt::F64), [S::S, S::D]) => (true, true),
        _ => return Err(Error::Syntax.into()),
    };
    Ok(SimdFp::Half {
        to_half,
        double,
        top,
        d: regs[0],
        m: regs[1],
    })
}

/// A fixed-point conversion's fraction bits, `fewest` to `most`.
fn fraction(p: &mut Parser<'_>, arg: Option<Arg>, most: u8, fewest: u8) -> Parse<u8> {
    let Some(Arg::Imm(n)) = arg else {
        return Err(Error::Syntax.into());
    };
    let n = p.arith.int(n)?;
    u8::try_from(n)
        .ok()
        .filter(|n| (fewest..=most).contains(n))
        .ok_or_else(|| Error::Syntax.into())
}

/// The Advanced SIMD operations: three registers alike, long, wide or
/// narrow, by a scalar, by an immediate shift, on two registers, or of an
/// immediate.
#[expect(clippy::too_many_lines, reason = "one arm per form")]
fn neon(
    p: &mut Parser<'_>,
    name: &str,
    types: Types,
    shapes: &[Shape],
    regs: [u8; 4],
    args: &[Option<Arg>],
) -> Parse<SimdFp> {
    use Shape as S;
    if let [S::D | S::Q, S::Imm] = shapes {
        let op = SimdImmediateOp::ALL
            .iter()
            .find(|op| op.name() == name)
            .copied()
            .ok_or(Error::Syntax)?;
        let dt = types.one().ok_or(Error::Syntax)?;
        return immediate(p, op, dt, shapes[0] == S::Q, regs[0], args[1]);
    }
    let dt = types.one();
    let alike = |a: Shape, b: Shape| a == b && matches!(a, S::D | S::Q);
    match shapes {
        [a, b, c] if alike(*a, *b) && alike(*b, *c) => {
            let op = typed(
                SimdThreeOp::ALL.iter().copied(),
                name,
                dt,
                SimdThreeOp::typing,
            )
            .ok_or(Error::Syntax)?;
            let (unsigned, size) = op.1;
            // BASIC reads VRSHL's operands as VADD's, where UAL names the
            // value before the shifts.
            let (rn, rm) = if matches!(
                op.0,
                SimdThreeOp::Vshl | SimdThreeOp::Vqshl | SimdThreeOp::Vqrshl
            ) {
                (regs[2], regs[1])
            } else {
                (regs[1], regs[2])
            };
            Ok(SimdFp::Three {
                op: op.0,
                unsigned,
                size,
                q: *a == S::Q,
                d: regs[0],
                n: rn,
                m: rm,
            })
        }
        [S::Q, S::D | S::Q, S::D] | [S::D, S::Q, S::Q] => {
            let width = match shapes {
                [S::Q, S::D, S::D] => Width::Long,
                [S::Q, S::Q, S::D] => Width::Wide,
                _ => Width::Narrow,
            };
            let candidates = SimdLongOp::ALL
                .iter()
                .copied()
                .filter(|op| op.width() == width && *op != SimdLongOp::Vmullp64);
            // A narrowing operation's type names its operands' wide lanes.
            let dt = dt.map(|dt| {
                if width == Width::Narrow {
                    halved(dt)
                } else {
                    dt
                }
            });
            let (op, (unsigned, size)) =
                typed(candidates, name, dt, SimdLongOp::typing).ok_or(Error::Syntax)?;
            Ok(SimdFp::Long {
                op,
                unsigned,
                size,
                d: regs[0],
                n: regs[1],
                m: regs[2],
            })
        }
        [a, b, S::Scalar] if matches!(a, S::D | S::Q) && matches!(b, S::D | S::Q) => {
            let long = *a == S::Q && *b == S::D;
            if !long && a != b {
                return Err(Error::Syntax.into());
            }
            let candidates = SimdScalarOp::ALL
                .iter()
                .filter(|op| op.long() == long)
                .copied();
            let (op, (unsigned, size)) =
                typed(candidates, name, dt, SimdScalarOp::typing).ok_or(Error::Syntax)?;
            let Some(Arg::Scalar {
                d: m,
                index: Some(index),
            }) = args[2]
            else {
                return Err(Error::Syntax.into());
            };
            let size = if op.float() { 2 } else { size };
            Ok(SimdFp::ByScalar {
                op,
                unsigned,
                size,
                q: !long && *a == S::Q,
                d: regs[0],
                n: regs[1],
                m,
                index,
            })
        }
        [a, b, S::Imm] if matches!(a, S::D | S::Q) && matches!(b, S::D | S::Q) => {
            shift(p, name, dt, shapes, regs, args[2])
        }
        [a, b] if matches!(a, S::D | S::Q) && matches!(b, S::D | S::Q) => {
            if name == "VMOVL" {
                let (unsigned, size) = dt
                    .and_then(|dt| dt.lanes(Typing::Signed))
                    .ok_or(Error::Syntax)?;
                if *a != S::Q || *b != S::D {
                    return Err(Error::Syntax.into());
                }
                return Ok(SimdFp::Shift {
                    op: SimdShiftOp::Vmovl,
                    unsigned,
                    size,
                    q: false,
                    d: regs[0],
                    m: regs[1],
                    amount: 0,
                });
            }
            misc(name, types, shapes, regs, args)
        }
        _ => Err(Error::Syntax.into()),
    }
}

/// The operation among `ops` named `name` whose typing takes `dt`, with the
/// signedness and size it takes; an untyped one takes no type too.
fn typed<T: Copy + Named>(
    ops: impl IntoIterator<Item = T>,
    name: &str,
    dt: Option<Dt>,
    typing: fn(T) -> Typing,
) -> Option<(T, (bool, u8))> {
    ops.into_iter()
        .filter(|op| op.name_of() == name)
        .find_map(|op| {
            let typing = typing(op);
            match dt {
                None if typing == Typing::Untyped => Some((op, (false, 0))),
                None => None,
                Some(dt) => dt.lanes(typing).map(|lanes| (op, lanes)),
            }
        })
}

/// An operation with a mnemonic.
trait Named {
    fn name_of(&self) -> &'static str;
}

macro_rules! named {
    ($($t:ty),*) => {$(
        impl Named for $t {
            fn name_of(&self) -> &'static str {
                self.name()
            }
        }
    )*};
}

named!(
    SimdThreeOp,
    SimdLongOp,
    SimdScalarOp,
    SimdShiftOp,
    SimdMiscOp
);

/// The shifts by an immediate, and the comparisons with zero.
fn shift(
    p: &mut Parser<'_>,
    name: &str,
    dt: Option<Dt>,
    shapes: &[Shape],
    regs: [u8; 4],
    amount: Option<Arg>,
) -> Parse<SimdFp> {
    use Shape as S;
    let Some(Arg::Imm(number)) = amount else {
        return Err(Error::Syntax.into());
    };
    let n = p.arith.int(number)?;
    let dt = dt.ok_or(Error::Syntax)?;
    let (a, b) = (shapes[0], shapes[1]);
    if n == 0 && a == b {
        let compares = SimdMiscOp::ALL
            .iter()
            .copied()
            .filter(|op| zero_compare(*op));
        if let Some((op, (unsigned, size))) = typed(compares, name, Some(dt), SimdMiscOp::typing) {
            return Ok(SimdFp::Misc {
                op,
                unsigned,
                size,
                q: a == S::Q,
                d: regs[0],
                m: regs[1],
            });
        }
    }
    if name == "VSHLL" && a == S::Q && b == S::D && n == i32::try_from(dt.bits).unwrap_or(0) {
        let (unsigned, size) = dt.lanes(Typing::Integer).ok_or(Error::Syntax)?;
        return Ok(SimdFp::Misc {
            op: SimdMiscOp::Vshll,
            unsigned,
            size,
            q: false,
            d: regs[0],
            m: regs[1],
        });
    }
    let candidates = SimdShiftOp::ALL.iter().copied().filter(|op| {
        !matches!(
            op,
            SimdShiftOp::VcvtFromFixed | SimdShiftOp::VcvtToFixed | SimdShiftOp::Vmovl
        )
    });
    let narrowed = |op: SimdShiftOp| if op.narrow() { halved(dt) } else { dt };
    let found = candidates
        .filter(|op| op.name() == name)
        .find_map(|op| narrowed(op).lanes(op.typing()).map(|lanes| (op, lanes)));
    let (op, (unsigned, size)) = found.ok_or(Error::Syntax)?;
    let fits = if op.narrow() {
        a == S::D && b == S::Q
    } else if op.long() {
        a == S::Q && b == S::D
    } else {
        a == b
    };
    if !fits {
        return Err(Error::Syntax.into());
    }
    let amount = u8::try_from(n).map_err(|_| Error::Syntax)?;
    Ok(SimdFp::Shift {
        op,
        unsigned,
        size,
        q: !op.narrow() && !op.long() && a == S::Q,
        d: regs[0],
        m: regs[1],
        amount,
    })
}

/// A narrowing operation's type, which names its operands' wide lanes, as
/// the lanes of its result.
const fn halved(dt: Dt) -> Dt {
    Dt {
        bits: dt.bits / 2,
        ..dt
    }
}

/// Whether `op` compares with zero.
const fn zero_compare(op: SimdMiscOp) -> bool {
    use SimdMiscOp as O;
    matches!(
        op,
        O::Vcgt0
            | O::Vcge0
            | O::Vceq0
            | O::Vcle0
            | O::Vclt0
            | O::Vcgt0F
            | O::Vcge0F
            | O::Vceq0F
            | O::Vcle0F
            | O::Vclt0F
    )
}

/// The operations on two registers.
fn misc(
    name: &str,
    types: Types,
    shapes: &[Shape],
    regs: [u8; 4],
    _args: &[Option<Arg>],
) -> Parse<SimdFp> {
    use Shape as S;
    use SimdMiscOp as O;
    let dt = types.one();
    let candidates = O::ALL.iter().copied().filter(|op| {
        !zero_compare(*op)
            && !matches!(
                op,
                O::VcvtToHalf
                    | O::VcvtFromHalf
                    | O::Vcvta
                    | O::Vcvtn
                    | O::Vcvtp
                    | O::Vcvtm
                    | O::VcvtToInteger
                    | O::VcvtToFloat
                    | O::Vshll
            )
    });
    let narrow = |op: O| matches!(op, O::Vmovn | O::Vqmovun | O::Vqmovn);
    let found = candidates.filter(|op| op.name() == name).find_map(|op| {
        let typing = match op {
            O::Vrecpe | O::Vrsqrte => Typing::SignedOnly,
            _ => op.typing(),
        };
        let lanes = match (dt, typing) {
            (None, Typing::Untyped) => Some((false, 0)),
            (None, _) => None,
            (Some(dt), _) if matches!(op, O::Vrecpe | O::Vrsqrte) => (dt
                == Dt {
                    kind: Some(b'U'),
                    bits: 32,
                })
            .then_some((false, 2)),
            (Some(dt), _) if narrow(op) => halved(dt).lanes(typing),
            (Some(dt), _) => dt.lanes(typing),
        };
        lanes.map(|lanes| (op, lanes))
    });
    let (op, (unsigned, size)) = found.ok_or(Error::Syntax)?;
    let fits = match shapes {
        [S::D, S::Q] => narrow(op),
        [a, b] => a == b && !narrow(op),
        _ => false,
    };
    if !fits {
        return Err(Error::Syntax.into());
    }
    let size = if op.typing() == Typing::Float {
        2
    } else {
        size
    };
    Ok(SimdFp::Misc {
        op,
        unsigned,
        size,
        q: !narrow(op) && shapes[0] == S::Q,
        d: regs[0],
        m: regs[1],
    })
}

/// A floating-point register list: consecutive singles or doubles, as a
/// range or one by one; its first register, whether double, and its length.
/// BASIC refuses a range that runs down as a bad register before it looks
/// at what follows, and anything out of place, a single's third digit
/// among it, as a syntax error.
fn fp_list(p: &mut Parser<'_>) -> Parse<(u8, bool, u8)> {
    p.expect(b'{', Error::MissingBrace)?;
    if p.spaces() == b'}' {
        return Err(Error::Syntax.into());
    }
    let mut first: Option<(Shape, u8)> = None;
    let mut next = 0u8;
    loop {
        let (bank, n) = list_register(p)?;
        let last = if p.take(b'-') {
            let (to_bank, m) = list_register(p)?;
            if to_bank != bank {
                return Err(Error::Syntax.into());
            }
            if m < n {
                return Err(Error::BadRegister.into());
            }
            m
        } else {
            n
        };
        match first {
            None => first = Some((bank, n)),
            Some((b, _)) if b == bank && n == next => {}
            _ => return Err(Error::Syntax.into()),
        }
        next = last.wrapping_add(1);
        if !p.take(b',') {
            break;
        }
    }
    p.expect(b'}', Error::Syntax)?;
    let (bank, start) = first.ok_or(Error::Syntax)?;
    let count = next.wrapping_sub(start);
    if bank == Shape::D && count > 16 {
        return Err(Error::BadRegister.into());
    }
    Ok((start, bank == Shape::D, count))
}

/// A register of a floating-point list: a single, its number read to two
/// digits at most, or a double; what follows is the list's to judge.
fn list_register(p: &mut Parser<'_>) -> Parse<(Shape, u8)> {
    p.spaces();
    let (bank, most_digits) = match p.peek().to_ascii_uppercase() {
        b'S' => (Shape::S, 2),
        b'D' => (Shape::D, usize::MAX),
        _ => return Err(Error::Syntax.into()),
    };
    if !p.byte(1).is_ascii_digit() {
        return Err(Error::Syntax.into());
    }
    p.advance(1);
    let mut n = 0u32;
    let mut digits = 0;
    while digits < most_digits && p.peek().is_ascii_digit() {
        n = n
            .saturating_mul(10)
            .saturating_add(u32::from(p.peek() - b'0'));
        p.advance(1);
        digits += 1;
    }
    let n = u8::try_from(n)
        .ok()
        .filter(|&n| n < 32)
        .ok_or(Error::BadRegister)?;
    Ok((bank, n))
}

/// `VLDR` and `VSTR`: BASIC takes a bracketed offset of whole words from
/// -256 to 255 of them, the magnitude's low eight bits laid down, and a
/// misaligned one as "Bad address offset".
fn transfer(p: &mut Parser<'_>, load: bool, types: Types) -> Parse<SimdFp> {
    if !types.none() {
        return Err(Error::Syntax.into());
    }
    let (double, d) = match arg(p)? {
        Arg::S(n) => (false, n),
        Arg::D(n) => (true, n),
        _ => return Err(Error::Syntax.into()),
    };
    p.comma()?;
    if !p.take(b'[') {
        let target = p.int(Want::Expression)?;
        let distance = target
            .cast_unsigned()
            .wrapping_sub(p.context.address.wrapping_add(8))
            .cast_signed();
        let offset = Some(distance.unsigned_abs())
            .filter(|magnitude| magnitude.is_multiple_of(4))
            .and_then(|magnitude| u8::try_from(magnitude / 4).ok())
            .ok_or(Error::BadOffset)?;
        return Ok(SimdFp::Transfer {
            load,
            double,
            d,
            rn: Reg::PC,
            up: distance >= 0,
            offset,
        });
    }
    let rn = p.register()?;
    // BASIC lays `[Rn]` alone down subtracting its offset of nothing.
    let value = if p.take(b',') {
        Some(p.hash_int()?)
    } else {
        None
    };
    p.expect(b']', Error::MissingSquare)?;
    let Some(value) = value else {
        return Ok(SimdFp::Transfer {
            load,
            double,
            d,
            rn,
            up: false,
            offset: 0,
        });
    };
    if value % 4 != 0 {
        return Err(Error::BadOffset.into());
    }
    let words = value / 4;
    if !(-256..=255).contains(&words) {
        return Err(Error::NoSuchVariable.into());
    }
    let offset = (words.unsigned_abs() & 0xFF) as u8;
    Ok(SimdFp::Transfer {
        load,
        double,
        d,
        rn,
        up: words >= 0,
        offset,
    })
}

/// `VLDM` and `VSTM`: the base, its write back and the list.
fn multiple(
    p: &mut Parser<'_>,
    load: bool,
    types: Types,
    mode: Option<BlockMode>,
) -> Parse<SimdFp> {
    if !types.none() {
        return Err(Error::Syntax.into());
    }
    let mode = mode.ok_or(Error::Syntax)?;
    let rn = p.register()?;
    let writeback = p.writeback();
    p.comma()?;
    let (d, double, count) = fp_list(p)?;
    let words = if double { count.wrapping_mul(2) } else { count };
    Ok(SimdFp::Multiple {
        load,
        double,
        d,
        rn,
        mode,
        writeback,
        words,
    })
}

/// `VPUSH` and `VPOP`.
fn stack(p: &mut Parser<'_>, load: bool, types: Types) -> Parse<SimdFp> {
    if !types.none() {
        return Err(Error::Syntax.into());
    }
    let (d, double, count) = fp_list(p)?;
    let words = if double { count.wrapping_mul(2) } else { count };
    let mode = if load {
        BlockMode::IncrementAfter
    } else {
        BlockMode::DecrementBefore
    };
    Ok(SimdFp::Multiple {
        load,
        double,
        d,
        rn: Reg::SP,
        mode,
        writeback: true,
        words,
    })
}

/// `VTBL` and `VTBX`: the table's consecutive doubles in a list.
fn table(p: &mut Parser<'_>, extend: bool, types: Types) -> Parse<SimdFp> {
    if types.one()
        != Some(Dt {
            kind: None,
            bits: 8,
        })
    {
        return Err(Error::Syntax.into());
    }
    let Arg::D(d) = arg(p)? else {
        return Err(Error::Syntax.into());
    };
    p.comma()?;
    let (n, double, length) = fp_list(p)?;
    if !double || !(1..=4).contains(&length) {
        return Err(Error::Syntax.into());
    }
    p.comma()?;
    let Arg::D(m) = arg(p)? else {
        return Err(Error::Syntax.into());
    };
    Ok(SimdFp::Table {
        extend,
        length,
        d,
        n,
        m,
    })
}

/// `VLDn` and `VSTn`: a list of whole registers, of a lane of each or of all
/// their lanes, then the base with any alignment and its post-index.
fn structure(p: &mut Parser<'_>, name: &str, types: Types) -> Parse<SimdFp> {
    let load = name.starts_with("VLD");
    let elements = name.as_bytes().get(3).map_or(0, |&b| b - b'0');
    let size = types.one().ok_or(Error::Syntax)?.size()?;
    p.expect(b'{', Error::MissingBrace)?;
    let mut registers = [0u8; 4];
    let mut count = 0usize;
    let mut lanes: Option<StructureLanes> = None;
    loop {
        let (first, index) = match arg(p)? {
            Arg::D(n) => (n, None),
            Arg::Scalar { d, index } => (d, Some(index)),
            _ => return Err(Error::Syntax.into()),
        };
        let kind = match index {
            None => StructureLanes::Multiple,
            Some(None) => StructureLanes::All,
            Some(Some(i)) => StructureLanes::One(i),
        };
        if lanes.is_some_and(|l| l != kind) {
            return Err(Error::Syntax.into());
        }
        lanes = Some(kind);
        let last = if index.is_none() && p.take(b'-') {
            match arg(p)? {
                Arg::D(m) if m >= first => m,
                _ => return Err(Error::Syntax.into()),
            }
        } else {
            first
        };
        for n in first..=last {
            *registers.get_mut(count).ok_or(Error::Syntax)? = n;
            count += 1;
        }
        if !p.take(b',') {
            break;
        }
    }
    p.expect(b'}', Error::MissingCloseBrace)?;
    let lanes = lanes.ok_or(Error::Syntax)?;
    let registers = &registers[..count];
    let spacing = match registers {
        [_] => 1,
        [a, b, ..] => b.wrapping_sub(*a),
        [] => return Err(Error::Syntax.into()),
    };
    if !matches!(spacing, 1 | 2)
        || registers
            .windows(2)
            .any(|w| w[1].wrapping_sub(w[0]) != spacing)
    {
        return Err(Error::Syntax.into());
    }
    let count = u8::try_from(count).map_err(|_| Error::Syntax)?;
    let (parts, spacing) = match lanes {
        StructureLanes::Multiple | StructureLanes::All if elements == 1 => (count, 1),
        StructureLanes::Multiple if elements == 2 && count == 4 && spacing == 1 => (2, 1),
        _ if count == elements => (1, spacing),
        _ => return Err(Error::Syntax.into()),
    };
    p.comma()?;
    p.expect(b'[', Error::Syntax)?;
    let rn = p.register()?;
    let align = if p.take(b'@') {
        let bits = p.int(Want::Expression)?;
        u8::try_from(bits / 8)
            .ok()
            .filter(|_| bits % 8 == 0)
            .ok_or(Error::Syntax)?
    } else {
        1
    };
    p.expect(b']', Error::MissingSquare)?;
    let rm = if p.writeback() {
        Reg::SP
    } else if p.take(b',') {
        let rm = p.register()?;
        if matches!(rm.number(), 13 | 15) {
            return Err(Error::BadRegister.into());
        }
        rm
    } else {
        Reg::PC
    };
    let shape = Structure {
        lanes,
        elements,
        parts,
        spacing,
        size,
        align,
    };
    Ok(SimdFp::Structure {
        load,
        shape,
        d: registers[0],
        rn,
        rm,
    })
}
