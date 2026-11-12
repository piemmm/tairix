//! How long each 8087 instruction takes: the typical clocks of Intel's
//! instruction table (the 80287 programmer's reference gives them; the 8087
//! data sheet only the ranges they sit in), from the instruction's issue.

use super::exec::{Arith, Constant, Format, Insn};
use super::{arith::flag, tag, Registers};

/// Whether a significand is "short": 40 trailing zero bits, as a value
/// loaded from a short real has, which FMUL multiplies by faster.
const fn short(sig: u64) -> bool {
    sig & ((1 << 40) - 1) == 0
}

/// ST(i)'s significand is short, or the register holds no finite value.
fn short_register(regs: &Registers, i: u8) -> bool {
    let physical = regs.physical(i);
    regs.tag(physical) != tag::VALID || short(regs.data[physical].sig)
}

fn arith_memory(op: Arith, format: Format, operand: &[u8]) -> u32 {
    let double_short = || {
        let mut raw = [0u8; 8];
        let n = operand.len().min(8);
        raw[..n].copy_from_slice(&operand[..n]);
        // A long real's fraction moves up 11 places into the significand.
        short(u64::from_le_bytes(raw) << 11)
    };
    match (format, op) {
        (Format::Single, Arith::Add | Arith::Sub(_)) => 105,
        (Format::Single, Arith::Mul) => 118,
        (Format::Single, Arith::Div(false)) => 220,
        (Format::Single, Arith::Div(true)) => 221,
        (Format::Single, Arith::Compare(0)) => 65,
        (Format::Single, Arith::Compare(_)) => 68,
        (Format::Double, Arith::Add | Arith::Sub(_)) => 110,
        (Format::Double, Arith::Mul) if double_short() => 120,
        (Format::Double, Arith::Mul) => 161,
        (Format::Double, Arith::Div(false)) => 225,
        (Format::Double, Arith::Div(true)) => 226,
        (Format::Double, Arith::Compare(0)) => 70,
        (Format::Double, Arith::Compare(_)) => 72,
        (Format::Int16, Arith::Add | Arith::Sub(_)) => 120,
        (Format::Int16, Arith::Mul) => 130,
        (Format::Int16, Arith::Div(_)) => 230,
        (Format::Int16, Arith::Compare(0)) => 80,
        (Format::Int16, Arith::Compare(_)) => 82,
        (_, Arith::Add | Arith::Sub(_)) => 125,
        (_, Arith::Mul) => 136,
        (_, Arith::Div(false)) => 236,
        (_, Arith::Div(true)) => 237,
        (_, Arith::Compare(0)) => 85,
        (_, Arith::Compare(_)) => 87,
    }
}

fn arith_register(regs: &Registers, op: Arith, i: u8, pop: bool) -> u32 {
    let short = short_register(regs, i) || short_register(regs, 0);
    match (op, pop) {
        (Arith::Add | Arith::Sub(false), false) => 85,
        (Arith::Sub(true), false) => 87,
        (Arith::Add | Arith::Sub(_), true) => 90,
        (Arith::Mul, false) if short => 97,
        (Arith::Mul, false) => 138,
        (Arith::Mul, true) if short => 100,
        (Arith::Mul, true) => 142,
        (Arith::Div(false), false) => 198,
        (Arith::Div(true), false) => 199,
        (Arith::Div(false), true) => 202,
        (Arith::Div(true), true) => 203,
        (Arith::Compare(0), _) => 45,
        (Arith::Compare(_), _) => 47,
    }
}

const fn load(format: Format) -> u32 {
    match format {
        Format::Single => 43,
        Format::Double => 46,
        Format::Extended => 57,
        Format::Int16 => 50,
        Format::Int32 => 56,
        Format::Int64 => 64,
        Format::Bcd => 300,
        Format::Word | Format::Environment | Format::State => 0,
    }
}

const fn store(format: Format, pop: bool) -> u32 {
    match (format, pop) {
        (Format::Single, false) => 87,
        (Format::Single, true) => 89,
        (Format::Double, false) | (Format::Int64, _) => 100,
        (Format::Double, true) => 102,
        (Format::Extended, _) => 55,
        (Format::Int16, false) => 86,
        (Format::Int16, true) | (Format::Int32, false) => 88,
        (Format::Int32, true) => 90,
        (Format::Bcd, _) => 530,
        (Format::Word | Format::Environment | Format::State, _) => 0,
    }
}

const fn constant(c: Constant) -> u32 {
    match c {
        Constant::Zero => 14,
        Constant::One | Constant::Log2E => 18,
        Constant::Pi | Constant::Log2Ten => 19,
        Constant::LnTwo => 20,
        Constant::Log10Two => 21,
    }
}

/// The clocks `insn` takes from its issue, `regs` the state it starts
/// from and `operand` the memory operand it read.
pub(super) fn clocks(insn: Insn, regs: &Registers, operand: &[u8]) -> u32 {
    match insn {
        Insn::ArithMemory(op, format) => arith_memory(op, format, operand),
        Insn::ArithRegister { op, i, pop, .. } => arith_register(regs, op, i, pop),
        Insn::Load(format) => load(format),
        Insn::Store(format, pop) => store(format, pop),
        Insn::LoadConstant(c) => constant(c),
        Insn::EnableInterrupts
        | Insn::DisableInterrupts
        | Insn::ClearExceptions
        | Insn::Initialise => 5,
        Insn::DecrementTop | Insn::IncrementTop => 9,
        Insn::LoadControl => 10,
        Insn::Free(..) => 11,
        Insn::Exchange(_) => 12,
        Insn::Nop => 13,
        Insn::Absolute => 14,
        Insn::ChangeSign | Insn::StoreControl | Insn::StoreStatus => 15,
        Insn::Examine => 17,
        Insn::StoreRegister(_, false) => 18,
        Insn::LoadRegister(_) | Insn::StoreRegister(_, true) => 20,
        Insn::Scale => 35,
        Insn::LoadEnvironment => 40,
        Insn::Test => 42,
        Insn::StoreEnvironment | Insn::RoundToInteger => 45,
        Insn::ComparePop2 | Insn::Extract => 50,
        Insn::PartialRemainder => 125,
        Insn::SquareRoot => 183,
        // The data sheet's range is 197 to 207.
        Insn::Restore | Insn::Save => 202,
        Insn::PartialTangent => 450,
        Insn::TwoPowerMinusOne => 500,
        Insn::PartialArctangent => 650,
        Insn::YLog2XPlus1 => 850,
        Insn::YLog2X => 950,
    }
}

/// The clocks the exceptions `flags` raised add, under `masks`: a masked
/// denormal's normalisation, and an unmasked overflow's or underflow's
/// rewrapped exponent.
pub(super) const fn penalty(flags: u16, masks: u16) -> u32 {
    let mut clocks = 0;
    if flags & flag::DENORMAL != 0 && masks & flag::DENORMAL != 0 {
        clocks += 33;
    }
    if flags & flag::OVERFLOW != 0 && masks & flag::OVERFLOW == 0 {
        clocks += 14;
    }
    if flags & flag::UNDERFLOW != 0 && masks & flag::UNDERFLOW == 0 {
        clocks += 16;
    }
    clocks
}
