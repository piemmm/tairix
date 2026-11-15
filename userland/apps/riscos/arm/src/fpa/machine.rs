//! The FPA FPEmulator runs its own routines on: the transcendental functions
//! and the packed decimal conversions are sequences of FPA instructions it
//! executes on itself, with traps disabled.

use super::arith::Unit;
use super::internal::Internal;
use super::transfer::load_internal;
use super::{fpsr, Exception, Precision, Rounding};
use crate::instruction::{FpaCompareOp, FpaDyadicOp, FpaMonadicOp};

/// The FPA FPEmulator's routines run on: eight registers, and an FPSR
/// holding the caller's control bits with every trap disabled and every flag
/// clear.
pub(super) struct Machine {
    pub f: [Internal; 8],
    pub fpsr: u32,
}

/// An operand: a register, or a value loaded for the instruction.
#[derive(Clone, Copy)]
pub(super) enum Src {
    F(usize),
    K(Internal),
}

/// One of the FPA's own constants, as `#0.5` names it.
pub(super) const fn constant(n: u8) -> Src {
    Src::K(Internal::constant(n))
}

pub(super) const HALF: Src = constant(6);
pub(super) const ONE: Src = constant(1);
pub(super) const TWO: Src = constant(2);
pub(super) const ZERO: Src = constant(0);

/// A round word: what lies below a mantissa is under half its last place.
pub(super) const ROUND_DOWN: u32 = 0x4000_0000;
/// A round word: what lies below a mantissa is over half its last place.
pub(super) const ROUND_UP: u32 = 0xC000_0000;

/// A head and mantissa as `LFM` would load their words.
pub(super) const fn loaded(head: u32, mantissa: u64) -> Internal {
    load_internal(Internal { head, mantissa }.words())
}

/// A constant from three words, loaded as `LFM` would.
pub(super) const fn load(words: [u32; 3]) -> Src {
    Src::K(load_internal(words))
}

/// A comparison's outcome, as the ARM conditions after `CMF` read it.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Order {
    Less,
    Equal,
    Greater,
    Unordered,
}

impl Order {
    const fn of(flags: u32) -> Self {
        match flags >> 28 {
            0b0110 => Self::Equal,
            0b1000 => Self::Less,
            0b0010 => Self::Greater,
            _ => Self::Unordered,
        }
    }

    pub(super) fn lt(self) -> bool {
        self == Self::Less
    }

    pub(super) fn gt(self) -> bool {
        self == Self::Greater
    }

    pub(super) fn le(self) -> bool {
        matches!(self, Self::Less | Self::Equal)
    }

    pub(super) fn eq(self) -> bool {
        self == Self::Equal
    }

    /// The carry flag `CMF` sets: greater or equal, and unordered only with
    /// the AC bit.
    pub(super) fn carry(self, ac: bool) -> bool {
        match self {
            Self::Greater | Self::Equal => true,
            Self::Less => false,
            Self::Unordered => ac,
        }
    }
}

impl Machine {
    pub(super) fn new(caller: u32) -> Self {
        Self {
            f: [Internal::ZERO; 8],
            fpsr: caller & !(fpsr::ENABLES | fpsr::FLAGS),
        }
    }

    pub(super) fn value(&self, src: Src) -> Internal {
        match src {
            Src::F(n) => self.f[n],
            Src::K(value) => value,
        }
    }

    /// One unit's result: traps are disabled, so none stops it.
    pub(super) fn run(
        &mut self,
        precision: Precision,
        rounding: Rounding,
        op: impl FnOnce(&mut Unit) -> Result<Internal, Exception>,
    ) -> Internal {
        let mut unit = Unit::new(self.fpsr, precision, rounding);
        let result = op(&mut unit);
        self.fpsr = unit.fpsr;
        result.unwrap_or(Internal::ZERO)
    }

    pub(super) fn dyadic(
        &mut self,
        op: FpaDyadicOp,
        rounding: Rounding,
        fd: usize,
        fn_: Src,
        fm: Src,
    ) {
        let (a, b) = (self.value(fn_), self.value(fm));
        self.f[fd] = self.run(Precision::Extended, rounding, |unit| unit.dyadic(op, a, b));
    }

    pub(super) fn monadic(&mut self, op: FpaMonadicOp, d: usize, m: Src) {
        let a = self.value(m);
        self.f[d] = self.run(Precision::Extended, Rounding::Nearest, |unit| {
            unit.monadic(op, a)
        });
    }

    pub(super) fn add(&mut self, d: usize, n: Src, m: Src) {
        self.dyadic(FpaDyadicOp::Adf, Rounding::Nearest, d, n, m);
    }

    pub(super) fn sub(&mut self, d: usize, n: Src, m: Src) {
        self.dyadic(FpaDyadicOp::Suf, Rounding::Nearest, d, n, m);
    }

    pub(super) fn mul(&mut self, d: usize, n: Src, m: Src) {
        self.dyadic(FpaDyadicOp::Muf, Rounding::Nearest, d, n, m);
    }

    pub(super) fn div(&mut self, d: usize, n: Src, m: Src) {
        self.dyadic(FpaDyadicOp::Dvf, Rounding::Nearest, d, n, m);
    }

    pub(super) fn mov(&mut self, d: usize, m: Src) {
        self.monadic(FpaMonadicOp::Mvf, d, m);
    }

    pub(super) fn neg(&mut self, d: usize, m: Src) {
        self.monadic(FpaMonadicOp::Mnf, d, m);
    }

    pub(super) fn abs(&mut self, d: usize, m: Src) {
        self.monadic(FpaMonadicOp::Abs, d, m);
    }

    pub(super) fn compare(&mut self, op: FpaCompareOp, n: usize, m: Src) -> Order {
        let (a, b) = (self.f[n], self.value(m));
        let mut unit = Unit::new(self.fpsr, Precision::Extended, Rounding::Nearest);
        let flags = unit.compare(op, a, b);
        self.fpsr = unit.fpsr;
        Order::of(flags.unwrap_or(0x1000_0000))
    }

    pub(super) fn cmf(&mut self, n: usize, m: Src) -> Order {
        self.compare(FpaCompareOp::Cmf, n, m)
    }

    pub(super) fn fix(&mut self, m: usize) -> u32 {
        let a = self.f[m];
        let mut unit = Unit::new(self.fpsr, Precision::Extended, Rounding::Nearest);
        let value = unit.fix(a);
        self.fpsr = unit.fpsr;
        value.unwrap_or(0)
    }

    pub(super) fn float(&mut self, precision: Precision, d: usize, value: u32) {
        self.f[d] = self.run(precision, Rounding::Nearest, |unit| unit.float(value));
    }

    pub(super) fn flags(&self) -> u32 {
        self.fpsr
    }

    /// `PolynomialOfF1InF2`: `F2 = ((c0·F1 + c1)·F1 + … + cn)·F1`, each
    /// coefficient passing through F3.
    pub(super) fn polynomial(&mut self, coefficients: &[[u32; 3]]) {
        let mut rest = coefficients.iter().map(|&c| load_internal(c));
        self.f[2] = rest.next().unwrap_or(Internal::ZERO);
        for c in rest {
            self.f[3] = c;
            self.mul(2, Src::F(2), Src::F(1));
            self.add(2, Src::F(2), Src::F(3));
        }
        self.mul(2, Src::F(2), Src::F(1));
    }

    /// `PolynomialOfF1InF4`: `F4 = ((F1 + c0)·F1 + c1)·F1 + … + cn`, monic.
    pub(super) fn monic(&mut self, coefficients: &[[u32; 3]]) {
        let mut rest = coefficients.iter().map(|&c| load_internal(c));
        self.f[3] = rest.next().unwrap_or(Internal::ZERO);
        self.add(4, Src::F(1), Src::F(3));
        for c in rest {
            self.f[3] = c;
            self.mul(4, Src::F(4), Src::F(1));
            self.add(4, Src::F(4), Src::F(3));
        }
    }

    /// `RangeRedByMod`: `F0` reduced by a modulus held as `R ≈ 1/M`, `M1`
    /// and `M2` with `M = M1 + M2`, `M1` short enough that its multiples are
    /// exact. The quotient; past an integer's range, what `FIX` gave.
    pub(super) fn reduce(&mut self, params: &[[u32; 3]; 3]) -> Result<u32, u32> {
        let [r, m1, m2] = params.map(load_internal);
        self.f[2] = r;
        self.f[3] = m1;
        self.f[4] = m2;
        self.mul(1, Src::F(0), Src::F(2));
        let n = self.fix(1);
        let failed = Exception::InvalidOperation.flag() | Exception::Overflow.flag();
        if self.flags() & failed != 0 {
            return Err(n);
        }
        self.float(Precision::Extended, 1, n);
        self.mul(2, Src::F(1), Src::F(3));
        self.sub(0, Src::F(0), Src::F(2));
        self.mul(2, Src::F(1), Src::F(4));
        self.sub(0, Src::F(0), Src::F(2));
        Ok(n)
    }

    /// The round word from the last step done both toward zero (`truncated`)
    /// and to nearest (`nearest`): equal, the value lies below half way.
    pub(super) fn below(&self, truncated: usize, nearest: usize) -> u32 {
        if self.f[truncated] == self.f[nearest] {
            ROUND_DOWN
        } else {
            ROUND_UP
        }
    }
}
