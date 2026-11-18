//! Floating-point and Advanced SIMD execution: the access CPACR and FPEXC
//! give, VFP's operations with VFPv2's short vectors, Advanced SIMD's lanes,
//! and the transfers to and from the core registers and memory.
//!
//! A short vector runs element by element, each a scalar operation in turn,
//! as the architecture defines it; a load gathers every word before it
//! writes a register, so an aborted one changes nothing.

use tairix_softfloat::{Binary, Mode};

use super::memory::{block_range, Aborted, Access};
use super::{Cpu, Flow};
use crate::bus::{Bus, Width};
use crate::instruction::{
    BlockMode, ConvertRounding, SimdFp, SimdImmediateOp, SimdMiscOp, SimdShiftOp, SimdThreeOp,
    Structure, StructureLanes, VfpDyadicOp, VfpMonadicOp, Width as Lengths,
};
use crate::psr;
use crate::reg::Reg;
use crate::vfp::crypto::{self, Sha1};
use crate::vfp::simd::{self, lane, lanes, replicate, with_lane};
use crate::vfp::{expand_immediate, fpscr, System, Unit};

/// A short vector's length and each operand's step per element.
#[derive(Clone, Copy)]
struct Vector {
    length: u8,
    step_d: u8,
    step_m: u8,
}

const SCALAR: Vector = Vector {
    length: 1,
    step_d: 0,
    step_m: 0,
};

/// The floating-point format a precision names.
const fn format(double: bool) -> Binary {
    if double {
        Binary::DOUBLE
    } else {
        Binary::SINGLE
    }
}

/// `bits` negated: its sign flipped, whatever it is.
const fn negate(double: bool, bits: u64) -> u64 {
    bits ^ format(double).sign_bit()
}

/// Whether register `n` is in its file's first bank, which a short vector
/// does not step through: S0 to S7, or D0 to D3 and D16 to D19.
const fn scalar_bank(double: bool, n: u8) -> bool {
    if double {
        n & 0x0C == 0
    } else {
        n & 0x18 == 0
    }
}

/// Register `n` stepped by `step` within its bank.
const fn advance(double: bool, n: u8, step: u8) -> u8 {
    let within = if double { 3 } else { 7 };
    (n & !within) | (n.wrapping_add(step) & within)
}

/// The rounding a conversion to an integer names.
const fn conversion_mode(rounding: ConvertRounding, fpscr: Mode) -> Mode {
    match rounding {
        ConvertRounding::Zero => Mode::Zero,
        ConvertRounding::Fpscr => fpscr,
        ConvertRounding::Away => Mode::NearestAway,
        ConvertRounding::Nearest => Mode::Nearest,
        ConvertRounding::Plus => Mode::Up,
        ConvertRounding::Minus => Mode::Down,
    }
}

impl Cpu {
    /// Executes a floating-point or Advanced SIMD instruction, or takes the
    /// undefined instruction exception where CPACR or FPEXC deny the unit.
    #[allow(clippy::too_many_lines)] // One arm per form, each handing its operands on.
    pub(super) fn simdfp<B: Bus>(&mut self, bus: &mut B, op: &SimdFp) -> Flow {
        if !self.simdfp_enabled(op) {
            self.undefined();
            return Flow::Next;
        }
        match *op {
            SimdFp::Dyadic {
                op,
                double,
                d,
                n,
                m,
            } => self.vfp_dyadic(op, double, d, n, m),
            SimdFp::Select {
                cond,
                double,
                d,
                n,
                m,
            } => {
                let source = if cond.passes(self.cpsr) { n } else { m };
                let value = self.fp_read(double, source);
                self.fp_write(double, d, value);
                Flow::Next
            }
            SimdFp::Monadic { op, double, d, m } => self.vfp_monadic(op, double, d, m),
            SimdFp::Compare {
                signalling,
                double,
                d,
                m,
            } => {
                let mut unit = Unit::new(self.vfp.fpscr());
                let second = m.map_or(0, |m| self.fp_read(double, m));
                let nzcv =
                    unit.compare(format(double), self.fp_read(double, d), second, signalling);
                self.vfp.set_nzcv(nzcv);
                self.vfp.raise(unit.flags);
                Flow::Next
            }
            SimdFp::Precision { to_double, d, m } => {
                let mut unit = Unit::new(self.vfp.fpscr());
                let value = unit.convert(
                    format(!to_double),
                    format(to_double),
                    self.fp_read(!to_double, m),
                );
                self.fp_write(to_double, d, value);
                self.vfp.raise(unit.flags);
                Flow::Next
            }
            SimdFp::Half {
                to_half,
                double,
                top,
                d,
                m,
            } => {
                self.vfp_half(to_half, double, top, d, m);
                Flow::Next
            }
            SimdFp::ToInteger {
                rounding,
                unsigned,
                double,
                d,
                m,
            } => {
                let mut unit = Unit::new(self.vfp.fpscr());
                let mode = conversion_mode(rounding, unit.mode());
                let value = unit.to_fixed(
                    format(double),
                    self.fp_read(double, m),
                    0,
                    unsigned,
                    32,
                    mode,
                );
                self.vfp.set_s(d, value);
                self.vfp.raise(unit.flags);
                Flow::Next
            }
            SimdFp::FromInteger {
                unsigned,
                double,
                d,
                m,
            } => {
                let mut unit = Unit::new(self.vfp.fpscr());
                let mode = unit.mode();
                let value = unit.from_fixed(format(double), self.vfp.s(m), 32, 0, unsigned, mode);
                self.fp_write(double, d, value);
                self.vfp.raise(unit.flags);
                Flow::Next
            }
            SimdFp::Fixed {
                to_fixed,
                unsigned,
                word,
                double,
                d,
                fraction,
            } => {
                self.vfp_fixed(to_fixed, unsigned, word, double, d, fraction);
                Flow::Next
            }
            SimdFp::MoveImmediate { double, d, imm } => self.vfp_move_immediate(double, d, imm),
            SimdFp::CoreSingle { to_core, n, rt } => {
                if to_core {
                    let value = self.vfp.s(n);
                    self.write(rt, value);
                } else {
                    let value = self.read(rt);
                    self.vfp.set_s(n, value);
                }
                Flow::Next
            }
            SimdFp::CorePair {
                to_core,
                double,
                m,
                rt,
                rt2,
            } => {
                self.vfp_core_pair(to_core, double, m, rt, rt2);
                Flow::Next
            }
            SimdFp::CoreScalar {
                to_core,
                size,
                unsigned,
                index,
                d,
                rt,
            } => {
                let register = u128::from(self.vfp.d(d));
                if to_core {
                    let value = lane(register, size, u32::from(index));
                    let extended = if unsigned || size == 2 {
                        value
                    } else {
                        let unused = 64 - (8 << size);
                        ((value << unused).cast_signed() >> unused).cast_unsigned()
                    };
                    self.write(rt, crate::word::low(extended));
                } else {
                    let value = u64::from(self.read(rt));
                    let updated = with_lane(register, size, u32::from(index), value);
                    self.vfp.set_d(d, crate::word::low64(updated));
                }
                Flow::Next
            }
            SimdFp::CoreDuplicate { size, q, d, rt } => {
                let value = replicate(u64::from(self.read(rt)), size);
                self.vector_write(d, q, value);
                Flow::Next
            }
            SimdFp::System { read, register, rt } => {
                self.vfp_system(read, register, rt);
                Flow::Next
            }
            SimdFp::Transfer {
                load,
                double,
                d,
                rn,
                up,
                offset,
            } => self.vfp_transfer(bus, load, double, d, rn, up, offset),
            SimdFp::Multiple {
                load,
                double,
                d,
                rn,
                mode,
                writeback,
                words,
            } => self.vfp_multiple(bus, load, double, d, rn, mode, writeback, words),
            SimdFp::Three {
                op,
                unsigned,
                size,
                q,
                d,
                n,
                m,
            } => {
                self.simd_three(op, unsigned, size, q, d, n, m);
                Flow::Next
            }
            SimdFp::Long {
                op,
                unsigned,
                size,
                d,
                n,
                m,
            } => {
                let width = op.width();
                let old = self.vector_read(d, width != Lengths::Narrow);
                let n_value = self.vector_read(n, width != Lengths::Long);
                let m_value = self.vector_read(m, width == Lengths::Narrow);
                let (result, saturated) = simd::long(op, unsigned, size, old, n_value, m_value);
                self.vector_write(d, width != Lengths::Narrow, result);
                self.saturated_lanes(saturated);
                Flow::Next
            }
            SimdFp::ByScalar {
                op,
                unsigned,
                size,
                q,
                d,
                n,
                m,
                index,
            } => {
                let long = op.long();
                let mut unit = Unit::standard(self.vfp.fpscr());
                let scalar = lane(u128::from(self.vfp.d(m)), size, u32::from(index));
                let old = self.vector_read(d, q || long);
                let first = self.vector_read(n, q);
                let (result, saturated) =
                    simd::by_scalar(op, unsigned, size, q, &mut unit, old, first, scalar);
                self.vector_write(d, q || long, result);
                self.saturated_lanes(saturated);
                self.vfp.raise(unit.flags);
                Flow::Next
            }
            SimdFp::Shift {
                op,
                unsigned,
                size,
                q,
                d,
                m,
                amount,
            } => {
                let (wide_d, wide_m) = if op.narrow() {
                    (false, true)
                } else if op.long() {
                    (true, false)
                } else {
                    (q, q)
                };
                let mut unit = Unit::standard(self.vfp.fpscr());
                let old = self.vector_read(d, wide_d);
                let operand = self.vector_read(m, wide_m);
                let (result, saturated) =
                    simd::shift(op, unsigned, size, q, amount, &mut unit, old, operand);
                self.vector_write(d, wide_d, result);
                self.saturated_lanes(saturated);
                if matches!(op, SimdShiftOp::VcvtFromFixed | SimdShiftOp::VcvtToFixed) {
                    self.vfp.raise(unit.flags);
                }
                Flow::Next
            }
            SimdFp::Misc {
                op,
                unsigned,
                size,
                q,
                d,
                m,
            } => {
                self.simd_misc(op, unsigned, size, q, d, m);
                Flow::Next
            }
            SimdFp::Immediate { cmode, q, d, imm } => {
                let value = replicate(simd::expand_immediate(cmode, imm), 3);
                let old = self.vector_read(d, q);
                let result = match SimdImmediateOp::of(cmode) {
                    Some(SimdImmediateOp::Vorr) => old | value,
                    Some(SimdImmediateOp::Vmvn) => !value,
                    Some(SimdImmediateOp::Vbic) => old & !value,
                    _ => value,
                };
                self.vector_write(d, q, result);
                Flow::Next
            }
            SimdFp::Extract { q, d, n, m, start } => {
                let result =
                    simd::extract(q, self.vector_read(n, q), self.vector_read(m, q), start);
                self.vector_write(d, q, result);
                Flow::Next
            }
            SimdFp::Table {
                extend,
                length,
                d,
                n,
                m,
            } => {
                let mut table = [0u64; 4];
                for (offset, slot) in (0u8..).zip(&mut table) {
                    *slot = self.vfp.d(n.wrapping_add(offset));
                }
                let result = simd::table(&table, length, extend, self.vfp.d(d), self.vfp.d(m));
                self.vfp.set_d(d, result);
                Flow::Next
            }
            SimdFp::Duplicate {
                size,
                q,
                d,
                m,
                index,
            } => {
                let value = lane(u128::from(self.vfp.d(m)), size, u32::from(index));
                self.vector_write(d, q, replicate(value, size));
                Flow::Next
            }
            SimdFp::Structure {
                load,
                shape,
                d,
                rn,
                rm,
            } => self.simd_structure(bus, load, shape, d, rn, rm),
        }
    }

    /// Whether CPACR and FPEXC let the unit execute `op`: both of its
    /// coprocessors granted to the current mode, and the unit enabled but
    /// for the identification registers and FPEXC, which privileged code
    /// reaches either way.
    fn simdfp_enabled(&self, op: &SimdFp) -> bool {
        let privileged = self.mode.privileged();
        let granted = |cp| match self.cp15.coprocessor_access(cp) {
            0b01 => privileged,
            0b11 => true,
            _ => false,
        };
        if !(granted(10) && granted(11)) {
            return false;
        }
        match *op {
            SimdFp::System { register, .. } if register != System::Fpscr => privileged,
            _ => self.vfp.enabled(),
        }
    }

    /// Sets FPSCR.QC where a lane saturated.
    fn saturated_lanes(&mut self, saturated: bool) {
        if saturated {
            self.vfp.raise(fpscr::QC);
        }
    }

    fn fp_read(&self, double: bool, n: u8) -> u64 {
        if double {
            self.vfp.d(n)
        } else {
            u64::from(self.vfp.s(n))
        }
    }

    fn fp_write(&mut self, double: bool, n: u8, value: u64) {
        if double {
            self.vfp.set_d(n, value);
        } else {
            self.vfp.set_s(n, crate::word::low(value));
        }
    }

    /// A doubleword register, or the quadword `d` begins.
    fn vector_read(&self, d: u8, q: bool) -> u128 {
        if q {
            self.vfp.q(d >> 1)
        } else {
            u128::from(self.vfp.d(d))
        }
    }

    fn vector_write(&mut self, d: u8, q: bool, value: u128) {
        if q {
            self.vfp.set_q(d >> 1, value);
        } else {
            self.vfp.set_d(d, crate::word::low64(value));
        }
    }

    /// The short vector the FPSCR makes of an operation writing `d` from
    /// `m`, or `None` where the architecture leaves it UNPREDICTABLE: a
    /// reserved stride, or a vector longer than its bank.
    fn short_vector(&self, double: bool, d: u8, m: u8) -> Option<Vector> {
        let (length, stride) = self.vfp.vector()?;
        let bank = if double { 4 } else { 8 };
        if length * stride > bank {
            return None;
        }
        if length == 1 || scalar_bank(double, d) {
            return Some(SCALAR);
        }
        Some(Vector {
            length,
            step_d: stride,
            step_m: if scalar_bank(double, m) { 0 } else { stride },
        })
    }

    /// The vector an operation runs as, its FPSCR's or a scalar; `None`
    /// where it is refused as UNPREDICTABLE, the exception taken.
    fn vector_for(&mut self, vector: bool, double: bool, d: u8, m: u8) -> Option<Vector> {
        if !vector {
            return Some(SCALAR);
        }
        match self.short_vector(double, d, m) {
            Some(shape) => Some(shape),
            None if self.refuse() => None,
            None => Some(SCALAR),
        }
    }

    fn vfp_dyadic(&mut self, op: VfpDyadicOp, double: bool, d: u8, n: u8, m: u8) -> Flow {
        use VfpDyadicOp as O;
        let fused = matches!(op, O::Vfma | O::Vfms | O::Vfnma | O::Vfnms);
        // A fused operation with short vectors set is UNPREDICTABLE.
        if fused && self.vfp.fpscr() & fpscr::VECTOR != 0 && self.refuse() {
            return Flow::Next;
        }
        let Some(shape) = self.vector_for(op.vector() && !fused, double, d, m) else {
            return Flow::Next;
        };
        let format = format(double);
        let mut unit = Unit::new(self.vfp.fpscr());
        let (mut d, mut n, mut m) = (d, n, m);
        for _ in 0..shape.length {
            let (left, right, old) = (
                self.fp_read(double, n),
                self.fp_read(double, m),
                self.fp_read(double, d),
            );
            let result = match op {
                O::Vmla => {
                    let product = unit.mul(format, left, right);
                    unit.add(format, old, product)
                }
                O::Vmls => {
                    let product = unit.mul(format, left, right);
                    unit.add(format, old, negate(double, product))
                }
                O::Vnmla => {
                    let product = unit.mul(format, left, right);
                    unit.add(format, negate(double, old), negate(double, product))
                }
                O::Vnmls => {
                    let product = unit.mul(format, left, right);
                    unit.add(format, negate(double, old), product)
                }
                O::Vmul => unit.mul(format, left, right),
                O::Vnmul => negate(double, unit.mul(format, left, right)),
                O::Vadd => unit.add(format, left, right),
                O::Vsub => unit.sub(format, left, right),
                O::Vdiv => unit.div(format, left, right),
                O::Vfma => unit.mul_add(format, old, left, right),
                O::Vfms => unit.mul_add(format, old, negate(double, left), right),
                O::Vfnma => unit.mul_add(format, negate(double, old), negate(double, left), right),
                O::Vfnms => unit.mul_add(format, negate(double, old), left, right),
                O::Vmaxnm => unit.max_number(format, left, right),
                O::Vminnm => unit.min_number(format, left, right),
            };
            self.fp_write(double, d, result);
            d = advance(double, d, shape.step_d);
            n = advance(double, n, shape.step_d);
            m = advance(double, m, shape.step_m);
        }
        self.vfp.raise(unit.flags);
        Flow::Next
    }

    fn vfp_monadic(&mut self, op: VfpMonadicOp, double: bool, d: u8, m: u8) -> Flow {
        use VfpMonadicOp as O;
        let Some(shape) = self.vector_for(op.vector(), double, d, m) else {
            return Flow::Next;
        };
        let format = format(double);
        let mut unit = Unit::new(self.vfp.fpscr());
        let fpscr_mode = unit.mode();
        let (mut d, mut m) = (d, m);
        for _ in 0..shape.length {
            let value = self.fp_read(double, m);
            let round =
                |unit: &mut Unit, mode, exact| unit.round_integral(format, value, mode, exact);
            let result = match op {
                O::Vmov => value,
                O::Vabs => value & !format.sign_bit(),
                O::Vneg => negate(double, value),
                O::Vsqrt => unit.sqrt(format, value),
                O::Vrintr => round(&mut unit, fpscr_mode, false),
                O::Vrintz => round(&mut unit, Mode::Zero, false),
                O::Vrintx => round(&mut unit, fpscr_mode, true),
                O::Vrinta => round(&mut unit, Mode::NearestAway, false),
                O::Vrintn => round(&mut unit, Mode::Nearest, false),
                O::Vrintp => round(&mut unit, Mode::Up, false),
                O::Vrintm => round(&mut unit, Mode::Down, false),
            };
            self.fp_write(double, d, result);
            d = advance(double, d, shape.step_d);
            m = advance(double, m, shape.step_m);
        }
        self.vfp.raise(unit.flags);
        Flow::Next
    }

    fn vfp_move_immediate(&mut self, double: bool, d: u8, imm: u8) -> Flow {
        let Some(shape) = self.vector_for(true, double, d, 0) else {
            return Flow::Next;
        };
        let value = expand_immediate(imm, double);
        let mut d = d;
        for _ in 0..shape.length {
            self.fp_write(double, d, value);
            d = advance(double, d, shape.step_d);
        }
        Flow::Next
    }

    /// `VCVTB` and `VCVTT`: half precision in a single register's half.
    fn vfp_half(&mut self, to_half: bool, double: bool, top: bool, d: u8, m: u8) {
        let shift = if top { 16 } else { 0 };
        let other = format(double);
        let mut unit = Unit::new(self.vfp.fpscr());
        if to_half {
            let half = unit.convert(other, Binary::HALF, self.fp_read(double, m));
            let old = self.vfp.s(d);
            let updated = (old & !(0xFFFF << shift)) | (crate::word::low(half) & 0xFFFF) << shift;
            self.vfp.set_s(d, updated);
        } else {
            let half = u64::from((self.vfp.s(m) >> shift) & 0xFFFF);
            let value = unit.convert(Binary::HALF, other, half);
            self.fp_write(double, d, value);
        }
        self.vfp.raise(unit.flags);
    }

    /// A conversion between floating and fixed point in register `d`.
    #[allow(clippy::fn_params_excessive_bools)] // The encoding's four independent choices.
    fn vfp_fixed(
        &mut self,
        to_fixed: bool,
        unsigned: bool,
        word: bool,
        double: bool,
        d: u8,
        fraction: u8,
    ) {
        let width = if word { 32 } else { 16 };
        let format = format(double);
        let mut unit = Unit::new(self.vfp.fpscr());
        let fraction = u32::from(fraction);
        if to_fixed {
            let value = unit.to_fixed(
                format,
                self.fp_read(double, d),
                fraction,
                unsigned,
                width,
                Mode::Zero,
            );
            let extended = if unsigned {
                u64::from(value)
            } else {
                i64::from(value.cast_signed()).cast_unsigned()
            };
            self.fp_write(double, d, extended);
        } else {
            // Fixed point converts to nearest, whatever the FPSCR's mode.
            let source = crate::word::low(self.fp_read(double, d));
            let value = unit.from_fixed(format, source, width, fraction, unsigned, Mode::Nearest);
            self.fp_write(double, d, value);
        }
        self.vfp.raise(unit.flags);
    }

    fn vfp_core_pair(&mut self, to_core: bool, double: bool, m: u8, rt: Reg, rt2: Reg) {
        if to_core {
            let (low, high) = if double {
                let value = self.vfp.d(m);
                (crate::word::low(value), crate::word::high(value))
            } else {
                (self.vfp.s(m), self.vfp.s(m.wrapping_add(1)))
            };
            self.write(rt, low);
            self.write(rt2, high);
        } else {
            let (low, high) = (self.read(rt), self.read(rt2));
            if double {
                self.vfp.set_d(m, u64::from(high) << 32 | u64::from(low));
            } else {
                self.vfp.set_s(m, low);
                self.vfp.set_s(m.wrapping_add(1), high);
            }
        }
    }

    fn vfp_system(&mut self, read: bool, register: System, rt: Reg) {
        if read {
            let value = match register {
                System::Fpscr => self.vfp.fpscr(),
                System::Fpexc => self.vfp.fpexc(),
                identity => identity.identity(self.profile.isa),
            };
            if rt == Reg::PC {
                self.set_nzcv(value);
            } else {
                self.write(rt, value);
            }
            return;
        }
        let value = self.read(rt);
        match register {
            System::Fpscr => self.vfp.set_fpscr(value),
            System::Fpexc => self.vfp.set_fpexc(value),
            // Writes to FPSID are ignored, and the decoder admits no others.
            _ => {}
        }
    }

    /// A data access the instruction requires aligned to `align` bytes.
    fn require_alignment(address: u32, align: u32, write: bool) -> Access<()> {
        if address & (align - 1) == 0 {
            Ok(())
        } else {
            Err(Aborted::misaligned(address, write))
        }
    }

    /// The doubleword at `address`, two words in the order the data
    /// endianness gives them.
    fn load_double<B: Bus>(&self, bus: &mut B, address: u32, privileged: bool) -> Access<u64> {
        let first = self.load(bus, address, Width::Word, privileged)?;
        let second = self.load(bus, address.wrapping_add(4), Width::Word, privileged)?;
        Ok(self.join(first, second))
    }

    fn store_double<B: Bus>(
        &self,
        bus: &mut B,
        address: u32,
        value: u64,
        privileged: bool,
    ) -> Access<()> {
        let (first, second) = self.split(value);
        self.store(bus, address, Width::Word, first, privileged)?;
        self.store(
            bus,
            address.wrapping_add(4),
            Width::Word,
            second,
            privileged,
        )
    }

    /// A doubleword from the words at its lower and upper addresses.
    const fn join(&self, first: u32, second: u32) -> u64 {
        if self.cpsr & psr::E != 0 {
            (first as u64) << 32 | second as u64
        } else {
            (second as u64) << 32 | first as u64
        }
    }

    /// A doubleword's words at its lower and upper addresses.
    const fn split(&self, value: u64) -> (u32, u32) {
        let (low, high) = (crate::word::low(value), crate::word::high(value));
        if self.cpsr & psr::E != 0 {
            (high, low)
        } else {
            (low, high)
        }
    }

    #[allow(clippy::too_many_arguments)] // The form's fields, unpacked.
    fn vfp_transfer<B: Bus>(
        &mut self,
        bus: &mut B,
        load: bool,
        double: bool,
        d: u8,
        rn: Reg,
        up: bool,
        offset: u8,
    ) -> Flow {
        let base = if rn == Reg::PC {
            self.pc_operand() & !3
        } else {
            self.read(rn)
        };
        let bytes = 4 * u32::from(offset);
        let address = if up {
            base.wrapping_add(bytes)
        } else {
            base.wrapping_sub(bytes)
        };
        let outcome = if load {
            self.load_fp(bus, double, address)
                .map(|value| self.fp_write(double, d, value))
        } else {
            self.store_fp(bus, double, address, self.fp_read(double, d))
        };
        match outcome {
            Ok(()) => Flow::Next,
            Err(aborted) => self.aborted(&aborted),
        }
    }

    /// A single or double register's value from the aligned `address`.
    fn load_fp<B: Bus>(&self, bus: &mut B, double: bool, address: u32) -> Access<u64> {
        Self::require_alignment(address, 4, false)?;
        let privileged = self.mode.privileged();
        if double {
            self.load_double(bus, address, privileged)
        } else {
            Ok(u64::from(self.load(
                bus,
                address,
                Width::Word,
                privileged,
            )?))
        }
    }

    fn store_fp<B: Bus>(&self, bus: &mut B, double: bool, address: u32, value: u64) -> Access<()> {
        Self::require_alignment(address, 4, true)?;
        let privileged = self.mode.privileged();
        if double {
            self.store_double(bus, address, value, privileged)
        } else {
            self.store(
                bus,
                address,
                Width::Word,
                crate::word::low(value),
                privileged,
            )
        }
    }

    #[allow(clippy::too_many_arguments)] // The form's fields, unpacked.
    fn vfp_multiple<B: Bus>(
        &mut self,
        bus: &mut B,
        load: bool,
        double: bool,
        d: u8,
        rn: Reg,
        mode: BlockMode,
        writeback: bool,
        words: u8,
    ) -> Flow {
        let registers = if double { words / 2 } else { words };
        let base = self.read(rn);
        let (start, end) = block_range(mode, base, u32::from(words));
        let step = if double { 8 } else { 4 };
        let mut gathered = [0u64; 32];
        for (index, slot) in (0..registers).zip(&mut gathered) {
            let address = start.wrapping_add(step * u32::from(index));
            let outcome = if load {
                self.load_fp(bus, double, address)
                    .map(|value| *slot = value)
            } else {
                self.store_fp(
                    bus,
                    double,
                    address,
                    self.fp_read(double, d.wrapping_add(index)),
                )
            };
            if let Err(aborted) = outcome {
                return self.aborted(&aborted);
            }
        }
        if load {
            for (index, &value) in (0..registers).zip(&gathered) {
                self.fp_write(double, d.wrapping_add(index), value);
            }
        }
        if writeback {
            self.write(rn, end);
        }
        Flow::Next
    }

    #[allow(clippy::too_many_arguments)] // The form's fields, unpacked.
    fn simd_three(
        &mut self,
        op: SimdThreeOp,
        unsigned: bool,
        size: u8,
        q: bool,
        d: u8,
        n: u8,
        m: u8,
    ) {
        use SimdThreeOp as O;
        let (old, first, second) = (
            self.vector_read(d, q),
            self.vector_read(n, q),
            self.vector_read(m, q),
        );
        let mut unit = Unit::standard(self.vfp.fpscr());
        let (result, saturated) = match op {
            O::Vpmax | O::Vpmin | O::Vpadd => {
                let result = simd::pairwise_integer(
                    op,
                    unsigned,
                    size,
                    crate::word::low64(first),
                    crate::word::low64(second),
                );
                (u128::from(result), false)
            }
            O::VpaddF | O::VpmaxF | O::VpminF => {
                let result = simd::pairwise_float(
                    op,
                    &mut unit,
                    crate::word::low64(first),
                    crate::word::low64(second),
                );
                (u128::from(result), false)
            }
            O::Sha1c => (crypto::sha1_hash(Sha1::Choose, old, first, second), false),
            O::Sha1p => (crypto::sha1_hash(Sha1::Parity, old, first, second), false),
            O::Sha1m => (crypto::sha1_hash(Sha1::Majority, old, first, second), false),
            O::Sha1su0 => (crypto::sha1su0(old, first, second), false),
            O::Sha256h => (crypto::sha256h(old, first, second), false),
            O::Sha256h2 => (crypto::sha256h2(old, first, second), false),
            O::Sha256su1 => (crypto::sha256su1(old, first, second), false),
            O::Vfma
            | O::Vfms
            | O::VaddF
            | O::VsubF
            | O::VabdF
            | O::VmlaF
            | O::VmlsF
            | O::VmulF
            | O::VceqF
            | O::VcgeF
            | O::VcgtF
            | O::Vacge
            | O::Vacgt
            | O::VmaxF
            | O::VminF
            | O::Vrecps
            | O::Vrsqrts
            | O::Vmaxnm
            | O::Vminnm => (
                simd::three_float(op, &mut unit, q, old, first, second),
                false,
            ),
            _ => simd::three_integer(op, unsigned, size, q, old, first, second),
        };
        self.vector_write(d, q, result);
        self.saturated_lanes(saturated);
        self.vfp.raise(unit.flags);
    }

    fn simd_misc(&mut self, op: SimdMiscOp, unsigned: bool, size: u8, q: bool, d: u8, m: u8) {
        use SimdMiscOp as O;
        match op {
            O::Vswp | O::Vtrn | O::Vuzp | O::Vzip => {
                let (new_d, new_m) =
                    simd::permute(op, size, q, self.vector_read(d, q), self.vector_read(m, q));
                // Each register is written once, the destination last, as a
                // register named twice ends with the destination's value.
                self.vector_write(m, q, new_m);
                self.vector_write(d, q, new_d);
                return;
            }
            O::Aese | O::Aesd | O::Aesmc | O::Aesimc | O::Sha1h | O::Sha1su1 | O::Sha256su0 => {
                let (old, operand) = (self.vfp.q(d >> 1), self.vfp.q(m >> 1));
                let result = match op {
                    O::Aese => crypto::aese(old, operand),
                    O::Aesd => crypto::aesd(old, operand),
                    O::Aesmc => crypto::aesmc(operand),
                    O::Aesimc => crypto::aesimc(operand),
                    O::Sha1h => crypto::sha1h(operand),
                    O::Sha1su1 => crypto::sha1su1(old, operand),
                    _ => crypto::sha256su0(old, operand),
                };
                self.vfp.set_q(d >> 1, result);
                return;
            }
            _ => {}
        }
        let (wide_d, wide_m) = match op {
            O::Vmovn | O::Vqmovun | O::Vqmovn | O::VcvtToHalf => (false, true),
            O::Vshll | O::VcvtFromHalf => (true, false),
            _ => (q, q),
        };
        let mut unit = Unit::standard(self.vfp.fpscr());
        let old = self.vector_read(d, wide_d);
        let operand = self.vector_read(m, wide_m);
        let (result, saturated) = simd::misc(op, unsigned, size, q, &mut unit, old, operand);
        self.vector_write(d, wide_d, result);
        self.saturated_lanes(saturated);
        self.vfp.raise(unit.flags);
    }

    /// One element of `bytes` at `address`, the profile's alignment rule
    /// applied to it.
    fn load_element<B: Bus>(
        &self,
        bus: &mut B,
        address: u32,
        bytes: u32,
        privileged: bool,
    ) -> Access<u64> {
        Ok(match bytes {
            1 => u64::from(self.load(bus, address, Width::Byte, privileged)?),
            2 => u64::from(self.load(bus, address, Width::Half, privileged)?),
            4 => u64::from(self.load(bus, address, Width::Word, privileged)?),
            _ => {
                if self.profile.alignment_faults {
                    Self::require_alignment(address, 8, false)?;
                }
                self.load_double(bus, address, privileged)?
            }
        })
    }

    fn store_element<B: Bus>(
        &self,
        bus: &mut B,
        address: u32,
        bytes: u32,
        value: u64,
        privileged: bool,
    ) -> Access<()> {
        let word = crate::word::low(value);
        match bytes {
            1 => self.store(bus, address, Width::Byte, word, privileged),
            2 => self.store(bus, address, Width::Half, word, privileged),
            4 => self.store(bus, address, Width::Word, word, privileged),
            _ => {
                if self.profile.alignment_faults {
                    Self::require_alignment(address, 8, true)?;
                }
                self.store_double(bus, address, value, privileged)
            }
        }
    }

    /// `VLDn` and `VSTn`.
    fn simd_structure<B: Bus>(
        &mut self,
        bus: &mut B,
        load: bool,
        shape: Structure,
        d: u8,
        rn: Reg,
        rm: Reg,
    ) -> Flow {
        let address = self.read(rn);
        let mut written = Written::default();
        let outcome = Self::require_alignment(address, u32::from(shape.align), !load)
            .and_then(|()| self.structure_transfer(bus, load, shape, d, address, &mut written));
        let transferred = match outcome {
            Ok(bytes) => bytes,
            Err(aborted) => return self.aborted(&aborted),
        };
        for &(target, value) in written.registers() {
            self.vfp.set_d(target, value);
        }
        match rm.number() {
            15 => {}
            13 => self.write(rn, address.wrapping_add(transferred)),
            _ => {
                let step = self.read(rm);
                self.write(rn, address.wrapping_add(step));
            }
        }
        Flow::Next
    }

    /// A structure transfer's memory accesses from `address`, a load's
    /// registers gathered into `written`: the bytes it spans.
    fn structure_transfer<B: Bus>(
        &self,
        bus: &mut B,
        load: bool,
        shape: Structure,
        d: u8,
        address: u32,
        written: &mut Written,
    ) -> Access<u32> {
        let size = shape.size;
        let ebytes = 1u32 << size;
        let privileged = self.mode.privileged();
        let register = |element: u8, part: u8| d.wrapping_add(element * shape.spacing + part);
        let mut cursor = address;
        let next = |cursor: &mut u32| {
            let at = *cursor;
            *cursor = at.wrapping_add(ebytes);
            at
        };
        match shape.lanes {
            StructureLanes::Multiple => {
                let mut values = [[0u128; 4]; 4];
                for (element, row) in (0u8..).zip(values.iter_mut()).take(shape.elements.into()) {
                    for (part, slot) in (0u8..).zip(row.iter_mut()).take(shape.parts.into()) {
                        *slot = u128::from(self.vfp.d(register(element, part)));
                    }
                }
                for part in 0..shape.parts {
                    for index in 0..lanes(size, false) {
                        for element in 0..shape.elements {
                            let slot = &mut values[usize::from(element)][usize::from(part)];
                            let at = next(&mut cursor);
                            if load {
                                let value = self.load_element(bus, at, ebytes, privileged)?;
                                *slot = with_lane(*slot, size, index, value);
                            } else {
                                self.store_element(
                                    bus,
                                    at,
                                    ebytes,
                                    lane(*slot, size, index),
                                    privileged,
                                )?;
                            }
                        }
                    }
                }
                if load {
                    for element in 0..shape.elements {
                        for part in 0..shape.parts {
                            let value = values[usize::from(element)][usize::from(part)];
                            written.push(register(element, part), crate::word::low64(value));
                        }
                    }
                }
                Ok(8 * shape.registers())
            }
            StructureLanes::One(index) => {
                for element in 0..shape.elements {
                    let target = register(element, 0);
                    let current = u128::from(self.vfp.d(target));
                    let at = next(&mut cursor);
                    if load {
                        let value = self.load_element(bus, at, ebytes, privileged)?;
                        let updated = with_lane(current, size, u32::from(index), value);
                        written.push(target, crate::word::low64(updated));
                    } else {
                        let value = lane(current, size, u32::from(index));
                        self.store_element(bus, at, ebytes, value, privileged)?;
                    }
                }
                Ok(ebytes * u32::from(shape.elements))
            }
            StructureLanes::All => {
                for element in 0..shape.elements {
                    let value = self.load_element(bus, next(&mut cursor), ebytes, privileged)?;
                    let replicated = crate::word::low64(replicate(value, size));
                    for part in 0..shape.parts {
                        written.push(register(element, part), replicated);
                    }
                }
                Ok(ebytes * u32::from(shape.elements))
            }
        }
    }
}

/// The doublewords a structure load writes, gathered before any is.
#[derive(Default)]
struct Written {
    entries: [(u8, u64); 4],
    count: usize,
}

impl Written {
    fn push(&mut self, register: u8, value: u64) {
        if let Some(entry) = self.entries.get_mut(self.count) {
            *entry = (register, value);
            self.count += 1;
        }
    }

    fn registers(&self) -> &[(u8, u64)] {
        &self.entries[..self.count]
    }
}

#[cfg(test)]
mod tests;
