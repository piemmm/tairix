//! `complex.h`, chunk 8, as RISC OS 5.30's library computes it, on an FPA
//! working as the client's, so an overflow or an invalid operation traps as
//! the client's FPA would.
//!
//! A finite argument takes its function's formula, which gives an answer's
//! noughts their signs; any other takes the special value C99's Annex G gives
//! it, raising only the invalid operation the annex names. A float function
//! works its double's form and narrows each part.
//!
//! A double complex argument comes in R0 to R3, its real part first, a float
//! complex one as two raw singles in R0 and R1; the answer's real part goes
//! back in F0, its imaginary part in F1. `cpow`'s second argument is on the
//! stack, `cpowf`'s in R2 and R3. The helpers the compiler calls for
//! multiplication and division take their operands in F0 to F3.

use core::f64::consts::{FRAC_PI_2, FRAC_PI_4, PI};

use tairix_riscos_arm::fpa::{Exception, Precision, Rounding};
use tairix_riscos_arm::instruction::{FpaDyadicOp, FpaMonadicOp};
use tairix_riscos_kernel::os::{Fail, Os, Regs};
use tairix_softfloat::from_fpa_words;

use super::{overflows_a_float, precision, FRAC_3_PI_4};
use crate::c::fpa::{as_client, double, quietly, widened, Extended, Unit};
use crate::c::math::sinh_of;
use crate::client::stacked;

/// A complex number's parts.
#[derive(Clone, Copy)]
struct Complex {
    re: f64,
    im: f64,
}

const fn complex(re: f64, im: f64) -> Complex {
    Complex { re, im }
}

impl Complex {
    /// `iz`.
    fn times_i(self) -> Self {
        complex(-self.im, self.re)
    }

    /// `−iz`.
    fn over_i(self) -> Self {
        complex(self.im, -self.re)
    }

    fn conj(self) -> Self {
        complex(self.re, -self.im)
    }

    fn negated(self) -> Self {
        complex(-self.re, -self.im)
    }

    fn is_finite(self) -> bool {
        self.re.is_finite() && self.im.is_finite()
    }

    fn is_zero(self) -> bool {
        self.re == 0.0 && self.im == 0.0
    }
}

/// `value` negated where `like`'s sign bit is set.
fn signed_as(value: f64, like: f64) -> f64 {
    if like.is_sign_negative() {
        -value
    } else {
        value
    }
}

/// A nought negative where exactly one of `a` and `b` is.
fn nought_of(a: f64, b: f64) -> f64 {
    signed_as(signed_as(0.0, a), b)
}

/// Annex G's `r cis y` for an `r` of nought or infinity, its circular
/// functions' invalid operation trapped as the client's FPA traps it but no
/// flag of theirs left, and its products the FPA's, as RISC OS 5.30 works
/// it: a NaN from either alike on every host.
fn cis(unit: &mut Unit, r: f64, y: f64) -> Result<Complex, Exception> {
    let before = unit.fpsr();
    let cos = unit.double_op(FpaMonadicOp::Cos, y)?;
    let sin = unit.double_op(FpaMonadicOp::Sin, y)?;
    unit.set_fpsr(before);
    Ok(complex(unit.mul(r, cos)?, unit.mul(r, sin)?))
}

/// Past `asinh(Ω)/4`, Kahan's `CTANH` answers ±1, where `βs²` would overflow.
const TANH_UNIT_BEYOND: f64 = 177.618_965_018_485_97;

/// `e^|x|` halved in F3 and its reciprocal halved in F4, at extended
/// precision, which holds `e^710`; an overflow traps before anything is
/// inexact, as RISC OS 5.30's complex functions overflow.
fn halves(e: &mut Extended<'_>, x: f64) -> Result<(), Exception> {
    e.constant(1, x.abs());
    e.mono(FpaMonadicOp::Exp, 1, 1)?;
    e.constant(2, 0.5);
    e.op(FpaDyadicOp::Muf, 3, 1, 2)?;
    e.op(FpaDyadicOp::Dvf, 4, 2, 1)
}

/// `sinh x` past ±1, from the halves [`halves`] leaves.
fn sinh_beyond_one(e: &mut Extended<'_>, x: f64) -> Result<f64, Exception> {
    e.op(FpaDyadicOp::Suf, 5, 3, 4)?;
    Ok(e.answer(5)?.copysign(x))
}

/// `sinh x` as the complex functions work it: within ±1 chunk 2's.
fn sinh(unit: &mut Unit, x: f64) -> Result<f64, Exception> {
    if x.abs() <= 1.0 {
        return sinh_of(unit, x);
    }
    let e = &mut Extended(unit);
    halves(e, x)?;
    sinh_beyond_one(e, x)
}

/// `cosh x` and `sinh x` as the complex functions work them.
fn hyperbolic(unit: &mut Unit, x: f64) -> Result<(f64, f64), Exception> {
    let e = &mut Extended(unit);
    halves(e, x)?;
    e.op(FpaDyadicOp::Adf, 5, 3, 4)?;
    let cosh = e.answer(5)?;
    let sinh = if x.abs() <= 1.0 {
        sinh_of(unit, x)?
    } else {
        sinh_beyond_one(e, x)?
    };
    Ok((cosh, sinh))
}

/// `|z|` in F3 at extended precision, which holds the squares: an infinite
/// part's infinity whatever the other, and a NaN part's NaN squaring nothing.
fn magnitude(e: &mut Extended<'_>, z: Complex) -> Result<(), Exception> {
    if z.re.is_infinite() || z.im.is_infinite() {
        e.constant(3, f64::INFINITY);
        return Ok(());
    }
    if z.re.is_nan() || z.im.is_nan() {
        e.constant(3, if z.re.is_nan() { z.re } else { z.im });
        return Ok(());
    }
    e.constant(1, z.re);
    e.constant(2, z.im);
    e.op(FpaDyadicOp::Muf, 1, 1, 1)?;
    e.op(FpaDyadicOp::Muf, 2, 2, 2)?;
    e.op(FpaDyadicOp::Adf, 3, 1, 2)?;
    e.mono(FpaMonadicOp::Sqt, 3, 3)
}

/// Annex G's `csqrt` of a non-finite argument.
fn sqrt_special(z: Complex) -> Complex {
    let (x, y) = (z.re, z.im);
    if y.is_infinite() {
        complex(f64::INFINITY, y)
    } else if x == f64::NEG_INFINITY {
        if y.is_nan() {
            complex(y, f64::INFINITY)
        } else {
            complex(0.0, f64::INFINITY.copysign(y))
        }
    } else if x == f64::INFINITY {
        complex(x, if y.is_nan() { y } else { 0.0_f64.copysign(y) })
    } else {
        let nan = if x.is_nan() { x } else { y };
        complex(nan, nan)
    }
}

/// `a` in F1 and F2, `b` in F3 and F4.
fn load(e: &mut Extended<'_>, a: Complex, b: Complex) {
    for (n, part) in (1..).zip([a.re, a.im, b.re, b.im]) {
        e.constant(n, part);
    }
}

/// The unit's arithmetic on complex numbers.
struct Arithmetic<'a>(&'a mut Unit);

impl Arithmetic<'_> {
    fn op(&mut self, op: FpaMonadicOp, x: f64) -> Result<f64, Exception> {
        self.0.double_op(op, x)
    }

    /// `cos y` and `sin y`.
    fn circular(&mut self, y: f64) -> Result<(f64, f64), Exception> {
        Ok((
            self.op(FpaMonadicOp::Cos, y)?,
            self.op(FpaMonadicOp::Sin, y)?,
        ))
    }

    fn add(&mut self, a: Complex, b: Complex) -> Result<Complex, Exception> {
        Ok(complex(self.0.add(a.re, b.re)?, self.0.add(a.im, b.im)?))
    }

    /// `z + k` for a real `k`, which leaves the imaginary part as it is.
    fn shifted(&mut self, z: Complex, k: f64) -> Result<Complex, Exception> {
        Ok(complex(self.0.add(z.re, k)?, z.im))
    }

    fn scaled(&mut self, z: Complex, k: f64) -> Result<Complex, Exception> {
        Ok(complex(self.0.mul(z.re, k)?, self.0.mul(z.im, k)?))
    }

    /// `√((z + k)/2)` for a real `k`, the square roots Kahan's inverse cosines
    /// are written in.
    fn root_of_half(&mut self, z: Complex, k: f64) -> Result<Complex, Exception> {
        let shifted = self.shifted(z, k)?;
        let half = self.scaled(shifted, 0.5)?;
        self.sqrt(half)
    }

    /// `ab`, the textbook form worked at extended precision, each part
    /// rounded to `precision`.
    fn product(
        &mut self,
        a: Complex,
        b: Complex,
        precision: Precision,
    ) -> Result<Complex, Exception> {
        let e = &mut Extended(self.0);
        load(e, a, b);
        e.op(FpaDyadicOp::Muf, 5, 1, 3)?;
        e.op(FpaDyadicOp::Muf, 6, 2, 4)?;
        e.op(FpaDyadicOp::Suf, 5, 5, 6)?;
        e.op(FpaDyadicOp::Muf, 6, 1, 4)?;
        e.op(FpaDyadicOp::Muf, 7, 2, 3)?;
        e.op(FpaDyadicOp::Adf, 6, 6, 7)?;
        Ok(complex(
            e.answer_in(5, precision)?,
            e.answer_in(6, precision)?,
        ))
    }

    /// `a/b`, the textbook form worked at extended precision, each part
    /// rounded to `precision`.
    fn quotient(
        &mut self,
        a: Complex,
        b: Complex,
        precision: Precision,
    ) -> Result<Complex, Exception> {
        let e = &mut Extended(self.0);
        load(e, a, b);
        e.op(FpaDyadicOp::Muf, 5, 3, 3)?;
        e.op(FpaDyadicOp::Muf, 6, 4, 4)?;
        e.op(FpaDyadicOp::Adf, 5, 5, 6)?;
        e.op(FpaDyadicOp::Muf, 6, 1, 3)?;
        e.op(FpaDyadicOp::Muf, 7, 2, 4)?;
        e.op(FpaDyadicOp::Adf, 6, 6, 7)?;
        e.op(FpaDyadicOp::Muf, 7, 2, 3)?;
        e.op(FpaDyadicOp::Muf, 1, 1, 4)?;
        e.op(FpaDyadicOp::Suf, 7, 7, 1)?;
        e.op(FpaDyadicOp::Dvf, 6, 6, 5)?;
        e.op(FpaDyadicOp::Dvf, 7, 7, 5)?;
        Ok(complex(
            e.answer_in(6, precision)?,
            e.answer_in(7, precision)?,
        ))
    }

    fn abs(&mut self, z: Complex) -> Result<f64, Exception> {
        let e = &mut Extended(self.0);
        magnitude(e, z)?;
        e.answer(3)
    }

    /// `arg z`: the FPA's polar angle, of the unit vector where both parts
    /// are noughts or both infinities, as C's `atan2` takes them.
    fn arg(&mut self, z: Complex) -> Result<f64, Exception> {
        let (x, y) = if z.is_zero() {
            (1.0_f64.copysign(z.re), z.im)
        } else if z.re.is_infinite() && z.im.is_infinite() {
            (1.0_f64.copysign(z.re), 1.0_f64.copysign(z.im))
        } else {
            (z.re, z.im)
        };
        self.0
            .dyadic(FpaDyadicOp::Pol, Precision::Double, Rounding::Nearest, x, y)
    }

    /// `log z`: the angle first, then the logarithm of `|z|` at extended
    /// precision.
    fn log(&mut self, z: Complex) -> Result<Complex, Exception> {
        let angle = self.arg(z)?;
        let e = &mut Extended(self.0);
        magnitude(e, z)?;
        e.mono(FpaMonadicOp::Lgn, 3, 3)?;
        Ok(complex(e.answer(3)?, angle))
    }

    /// The principal square root, at extended precision: `t = √((|z| + |x|)/2)`,
    /// then `t + iy/2t`, or `|y|/2t ± it` where `x` is negative.
    fn sqrt(&mut self, z: Complex) -> Result<Complex, Exception> {
        if !z.is_finite() {
            return Ok(sqrt_special(z));
        }
        if z.is_zero() {
            return Ok(complex(0.0, z.im));
        }
        let upper = z.re >= 0.0;
        let e = &mut Extended(self.0);
        magnitude(e, z)?;
        e.constant(1, z.re.abs());
        e.op(FpaDyadicOp::Adf, 3, 3, 1)?;
        e.constant(2, 0.5);
        e.op(FpaDyadicOp::Muf, 3, 3, 2)?;
        e.mono(FpaMonadicOp::Sqt, 4, 3)?;
        e.op(FpaDyadicOp::Adf, 5, 4, 4)?;
        e.constant(1, if upper { z.im } else { z.im.abs() });
        e.op(FpaDyadicOp::Dvf, 6, 1, 5)?;
        let (t, ratio) = (e.answer(4)?, e.answer(6)?);
        Ok(if upper {
            complex(t, ratio)
        } else {
            complex(ratio, t.copysign(z.im))
        })
    }

    fn exp(&mut self, z: Complex) -> Result<Complex, Exception> {
        if !z.is_finite() {
            return self.exp_special(z);
        }
        let magnitude = self.op(FpaMonadicOp::Exp, z.re)?;
        let (cos, sin) = self.circular(z.im)?;
        Ok(complex(
            self.0.mul(magnitude, cos)?,
            self.0.mul(magnitude, sin)?,
        ))
    }

    /// Annex G's `cexp` of a non-finite argument.
    fn exp_special(&mut self, z: Complex) -> Result<Complex, Exception> {
        let (x, y) = (z.re, z.im);
        if y == 0.0 {
            let limit = if x == f64::NEG_INFINITY { 0.0 } else { x };
            return Ok(complex(limit, y));
        }
        if x.is_nan() {
            return Ok(complex(x, x));
        }
        if x == f64::NEG_INFINITY {
            return if y.is_finite() {
                cis(self.0, 0.0, y)
            } else {
                Ok(complex(0.0, 0.0_f64.copysign(y)))
            };
        }
        if y.is_finite() {
            return cis(self.0, f64::INFINITY, y);
        }
        if y.is_infinite() {
            self.0.raise(Exception::InvalidOperation)?;
        }
        let nan = if y.is_nan() { y } else { f64::NAN };
        Ok(complex(if x.is_infinite() { x } else { nan }, nan))
    }

    fn cosh(&mut self, z: Complex) -> Result<Complex, Exception> {
        if !z.is_finite() {
            return self.cosh_special(z);
        }
        let (cosh, sinh) = hyperbolic(self.0, z.re)?;
        let (cos, sin) = self.circular(z.im)?;
        Ok(complex(self.0.mul(cosh, cos)?, self.0.mul(sinh, sin)?))
    }

    /// Annex G's `ccosh` of a non-finite argument.
    fn cosh_special(&mut self, z: Complex) -> Result<Complex, Exception> {
        let (x, y) = (z.re, z.im);
        if y == 0.0 {
            return Ok(complex(x.abs(), nought_of(x, y)));
        }
        if x.is_nan() {
            return Ok(complex(x.abs(), x));
        }
        if y.is_finite() {
            let w = cis(self.0, f64::INFINITY, y)?;
            return Ok(complex(w.re, signed_as(w.im, x)));
        }
        if y.is_infinite() {
            self.0.raise(Exception::InvalidOperation)?;
        }
        let nan = if y.is_nan() { y } else { f64::NAN };
        Ok(if x == 0.0 {
            complex(nan, nought_of(x, y))
        } else if x.is_infinite() {
            complex(f64::INFINITY, nan)
        } else {
            complex(nan, nan)
        })
    }

    fn sinh(&mut self, z: Complex) -> Result<Complex, Exception> {
        if !z.is_finite() {
            return self.sinh_special(z);
        }
        let (cosh, sinh) = hyperbolic(self.0, z.re)?;
        let (cos, sin) = self.circular(z.im)?;
        Ok(complex(self.0.mul(sinh, cos)?, self.0.mul(cosh, sin)?))
    }

    /// Annex G's `csinh` of a non-finite argument.
    fn sinh_special(&mut self, z: Complex) -> Result<Complex, Exception> {
        let (x, y) = (z.re, z.im);
        if y == 0.0 {
            return Ok(z);
        }
        if x.is_nan() {
            return Ok(complex(x, x.abs()));
        }
        if y.is_finite() {
            let w = cis(self.0, f64::INFINITY, y)?;
            return Ok(complex(signed_as(w.re, x), w.im));
        }
        if y.is_infinite() {
            self.0.raise(Exception::InvalidOperation)?;
        }
        let nan = if y.is_nan() { y } else { f64::NAN };
        Ok(complex(
            if x == 0.0 || x.is_infinite() { x } else { nan },
            nan,
        ))
    }

    /// `cos z`, worked directly from the real part's circular functions and
    /// the imaginary part's hyperbolic ones.
    fn cos(&mut self, z: Complex) -> Result<Complex, Exception> {
        if !z.is_finite() {
            return self.cosh(z.times_i());
        }
        let (cos, sin) = self.circular(z.re)?;
        let (cosh, sinh) = hyperbolic(self.0, z.im)?;
        let re = self.0.mul(cos, cosh)?;
        let im = self.0.mul(sin, sinh)?;
        Ok(complex(re, -im))
    }

    /// `sin z`, worked as [`Self::cos`] is.
    fn sin(&mut self, z: Complex) -> Result<Complex, Exception> {
        if !z.is_finite() {
            return Ok(self.sinh(z.times_i())?.over_i());
        }
        let (cos, sin) = self.circular(z.re)?;
        let (cosh, sinh) = hyperbolic(self.0, z.im)?;
        Ok(complex(self.0.mul(sin, cosh)?, self.0.mul(cos, sinh)?))
    }

    /// `tanh z` by Kahan's `CTANH` (W. Kahan, "Branch Cuts for Complex
    /// Elementary Functions", 1987): `t = tan y`, `β = 1 + t²`, `s = sinh x`,
    /// `ρ = √(1 + s²)`, then `(βρs + it)/(1 + βs²)`. Annex G's special values
    /// come first, an infinite `x`'s nought taking the sign of `sin 2y`,
    /// worked quietly.
    fn tanh(&mut self, z: Complex) -> Result<Complex, Exception> {
        let (x, y) = (z.re, z.im);
        if x.is_nan() {
            return Ok(if y == 0.0 { z } else { complex(x, x.abs()) });
        }
        let unit = 1.0_f64.copysign(x);
        if x.is_infinite() {
            let nought = if y.is_finite() {
                quietly(|own| {
                    Ok(nought_of(
                        own.double_op(FpaMonadicOp::Cos, y)?,
                        own.double_op(FpaMonadicOp::Sin, y)?,
                    ))
                })
            } else {
                0.0_f64.copysign(y)
            };
            return Ok(complex(unit, nought));
        }
        if !y.is_finite() {
            if y.is_infinite() {
                self.0.raise(Exception::InvalidOperation)?;
            }
            let nan = if y.is_nan() { y } else { f64::NAN };
            return Ok(complex(nan, nan));
        }
        if x.abs() > TANH_UNIT_BEYOND {
            self.0.raise(Exception::Inexact)?;
            return Ok(complex(unit, 0.0_f64.copysign(y)));
        }
        let tangent = self.op(FpaMonadicOp::Tan, y)?;
        let tangent_squared = self.0.mul(tangent, tangent)?;
        let beta = self.0.add(1.0, tangent_squared)?;
        let sinh_x = sinh(self.0, x)?;
        let sinh_x_squared = self.0.mul(sinh_x, sinh_x)?;
        let rho_squared = self.0.add(1.0, sinh_x_squared)?;
        let rho = self.op(FpaMonadicOp::Sqt, rho_squared)?;
        let beta_sinh_x = self.0.mul(beta, sinh_x)?;
        let beta_sinh_x_squared = self.0.mul(beta_sinh_x, sinh_x)?;
        let denominator = self.0.add(1.0, beta_sinh_x_squared)?;
        let beta_rho = self.0.mul(beta, rho)?;
        let beta_rho_sinh_x = self.0.mul(beta_rho, sinh_x)?;
        Ok(complex(
            self.0.div(beta_rho_sinh_x, denominator)?,
            self.0.div(tangent, denominator)?,
        ))
    }

    /// `asinh z = log(z + √(z² + 1))`, worked on `|x| + i|y|` and the signs
    /// put back.
    fn asinh(&mut self, z: Complex) -> Result<Complex, Exception> {
        let a = complex(z.re.abs(), z.im.abs());
        let w = if let Some(w) = asinh_special(a) {
            w
        } else {
            let square = self.product(a, a, Precision::Double)?;
            let shifted = self.shifted(square, 1.0)?;
            let root = self.sqrt(shifted)?;
            let sum = self.add(a, root)?;
            self.log(sum)?
        };
        Ok(complex(signed_as(w.re, z.re), signed_as(w.im, z.im)))
    }

    /// `acosh z`, Kahan's `2 log(√((z + 1)/2) + √((z − 1)/2))`. An infinite
    /// imaginary part over a NaN answers that infinity where Annex G gives a
    /// NaN, as RISC OS 5.30's does; a nought answers C99's `+0 ± iπ/2`, where
    /// genuine's real part is a rounding's 1.37·10⁻¹⁶, inexact as its working
    /// raises it.
    fn acosh(&mut self, z: Complex) -> Result<Complex, Exception> {
        if z.im.is_infinite() {
            if z.re.is_nan() {
                return Ok(complex(f64::INFINITY, z.im));
            }
            if z.re.is_infinite() {
                let angle = if z.re > 0.0 { FRAC_PI_4 } else { FRAC_3_PI_4 };
                return Ok(complex(f64::INFINITY, angle.copysign(z.im)));
            }
        }
        if z.is_zero() {
            self.0.raise(Exception::Inexact)?;
            return Ok(complex(0.0, FRAC_PI_2.copysign(z.im)));
        }
        let above = self.root_of_half(z, 1.0)?;
        let below = self.root_of_half(z, -1.0)?;
        let sum = self.add(above, below)?;
        let log = self.log(sum)?;
        self.scaled(log, 2.0)
    }

    /// `acos z`, Kahan's `−2i log(√((1 + z)/2) + i√((1 − z)/2))`, worked on
    /// the upper half plane and conjugated back.
    fn acos(&mut self, z: Complex) -> Result<Complex, Exception> {
        if let Some(w) = acos_special(z) {
            return Ok(w);
        }
        let lower = z.im.is_sign_negative();
        let upper = if lower { z.conj() } else { z };
        let above = self.root_of_half(upper, 1.0)?;
        let below = self.root_of_half(upper.negated(), 1.0)?;
        let sum = self.add(above, below.times_i())?;
        let log = self.log(sum)?;
        let angle = self.scaled(log.over_i(), 2.0)?;
        Ok(if lower { angle.conj() } else { angle })
    }

    /// `atanh z = (log(1 + z) − log(1 − z))/2`, worked on `x + i|y|` and the
    /// imaginary part's sign put back. RISC OS 5.30 answers `casinh z` for a
    /// real part whose sign bit is set, and so does this.
    fn atanh(&mut self, z: Complex) -> Result<Complex, Exception> {
        if z.re.is_sign_negative() {
            return self.asinh(z);
        }
        let u = complex(z.re, z.im.abs());
        let w = if let Some(w) = atanh_special(u) {
            w
        } else {
            let below = self.shifted(u.negated(), 1.0)?;
            let lower = self.log(below)?;
            let above = self.shifted(u, 1.0)?;
            let upper = self.log(above)?;
            let difference = complex(
                self.0.sub(upper.re, lower.re)?,
                self.0.sub(upper.im, lower.im)?,
            );
            self.scaled(difference, 0.5)?
        };
        Ok(complex(w.re, signed_as(w.im, z.im)))
    }

    /// `z^w = exp(w log z)`.
    fn pow(&mut self, z: Complex, w: Complex) -> Result<Complex, Exception> {
        let log = self.log(z)?;
        let product = self.product(w, log, Precision::Double)?;
        self.exp(product)
    }
}

/// Annex G's `casinh` of `a`, both parts' sign bits clear, where a part is
/// infinite or a NaN.
fn asinh_special(a: Complex) -> Option<Complex> {
    let (x, y) = (a.re, a.im);
    Some(if x.is_nan() {
        if y.is_infinite() {
            complex(y, x)
        } else if y == 0.0 {
            a
        } else {
            complex(x, x)
        }
    } else if y.is_nan() {
        if x.is_infinite() {
            a
        } else {
            complex(y, y)
        }
    } else if x.is_infinite() {
        complex(x, if y.is_infinite() { FRAC_PI_4 } else { 0.0 })
    } else if y.is_infinite() {
        complex(y, FRAC_PI_2)
    } else {
        return None;
    })
}

/// Annex G's `cacos` where RISC OS 5.30's gives it rather than its formula's
/// working: a nought or NaN over a zero real part, an infinite part, an
/// infinity against a NaN. A NaN against a number, and an infinite imaginary
/// part over a zero real one, take the formula.
fn acos_special(z: Complex) -> Option<Complex> {
    let (x, y) = (z.re, z.im);
    Some(if x == 0.0 && (y == 0.0 || y.is_nan()) {
        complex(FRAC_PI_2, -y)
    } else if x.is_nan() {
        if y.is_infinite() {
            complex(x, -y)
        } else {
            return None;
        }
    } else if y.is_nan() {
        if x.is_infinite() {
            complex(y, f64::NEG_INFINITY)
        } else {
            return None;
        }
    } else if x.is_infinite() {
        let angle = match (x > 0.0, y.is_infinite()) {
            (true, true) => FRAC_PI_4,
            (false, true) => FRAC_3_PI_4,
            (true, false) => 0.0,
            (false, false) => PI,
        };
        complex(angle, -f64::INFINITY.copysign(y))
    } else if y.is_infinite() && x != 0.0 {
        complex(FRAC_PI_2, -y)
    } else {
        return None;
    })
}

/// Annex G's `catanh` of `u`, whose parts' sign bits are clear, where a part
/// is infinite or a NaN.
fn atanh_special(u: Complex) -> Option<Complex> {
    let (x, y) = (u.re, u.im);
    Some(if y.is_nan() {
        if x == 0.0 {
            u
        } else if x.is_infinite() {
            complex(0.0, y)
        } else {
            complex(y, y)
        }
    } else if x.is_nan() {
        if y.is_infinite() {
            complex(0.0, FRAC_PI_2)
        } else {
            complex(x, x)
        }
    } else if x.is_infinite() || y.is_infinite() {
        complex(0.0, FRAC_PI_2)
    } else {
        return None;
    })
}

/// The complex number R0 to R3 hold as two doubles.
fn double_argument(regs: &Regs) -> Complex {
    complex(double(regs, 0), double(regs, 2))
}

/// The complex number R`n` and R`n + 1` hold as two raw singles.
fn float_argument(regs: &Regs, n: usize) -> Complex {
    complex(widened(regs.r[n]), widened(regs.r[n + 1]))
}

/// The argument of a double or a float function.
fn argument<const FLOAT: bool>(regs: &Regs) -> Complex {
    if FLOAT {
        float_argument(regs, 0)
    } else {
        double_argument(regs)
    }
}

/// The double function's `parts` as its float function gives them: an
/// exception the work traps, or a part beyond a float's range, raised with the
/// client's flags as they were on `entry`; each part then narrowed.
fn as_float<const N: usize>(
    unit: &mut Unit,
    entry: u32,
    work: impl FnOnce(&mut Unit) -> Result<[f64; N], Exception>,
) -> Result<[f64; N], Exception> {
    let parts = match work(unit) {
        Ok(parts) => parts,
        Err(exception) => {
            unit.set_fpsr(entry);
            return Err(exception);
        }
    };
    if parts.into_iter().any(overflows_a_float) {
        unit.set_fpsr(entry);
        unit.raise(Exception::Overflow)?;
    }
    let mut singles = parts;
    for part in &mut singles {
        *part = unit.single(*part)?;
    }
    Ok(singles)
}

/// `work`'s complex answer in F0 and F1, a float function's where `FLOAT`.
fn give_complex<const FLOAT: bool>(
    os: &mut Os<'_>,
    work: impl FnOnce(&mut Arithmetic<'_>) -> Result<Complex, Exception>,
) -> Result<(), Fail> {
    let entry = os.fpsr();
    let answer = as_client(os, |unit| {
        if !FLOAT {
            return work(&mut Arithmetic(unit));
        }
        let [re, im] = as_float(unit, entry, |unit| {
            let z = work(&mut Arithmetic(unit))?;
            Ok([z.re, z.im])
        })?;
        Ok(complex(re, im))
    })?;
    if let Some(z) = answer {
        os.set_fpa_double(0, z.re.to_bits());
        os.set_fpa_double(1, z.im.to_bits());
    }
    Ok(())
}

/// `work`'s real answer in F0, a float function's where `FLOAT`.
fn give_real<const FLOAT: bool>(
    os: &mut Os<'_>,
    work: impl FnOnce(&mut Arithmetic<'_>) -> Result<f64, Exception>,
) -> Result<(), Fail> {
    let entry = os.fpsr();
    let answer = as_client(os, |unit| {
        if !FLOAT {
            return work(&mut Arithmetic(unit));
        }
        let [x] = as_float(unit, entry, |unit| Ok([work(&mut Arithmetic(unit))?]))?;
        Ok(x)
    })?;
    if let Some(x) = answer {
        os.set_fpa_double(0, x.to_bits());
    }
    Ok(())
}

/// The functions of one complex argument with a complex answer.
pub(super) const ACOS: u8 = 0;
pub(super) const ASIN: u8 = 1;
pub(super) const ATAN: u8 = 2;
pub(super) const COS: u8 = 3;
pub(super) const SIN: u8 = 4;
pub(super) const TAN: u8 = 5;
pub(super) const ACOSH: u8 = 6;
pub(super) const ASINH: u8 = 7;
pub(super) const ATANH: u8 = 8;
pub(super) const COSH: u8 = 9;
pub(super) const SINH: u8 = 10;
pub(super) const TANH: u8 = 11;
pub(super) const EXP: u8 = 12;
pub(super) const LOG: u8 = 13;
pub(super) const SQRT: u8 = 14;

/// `cacos(z)` to `csqrt`, and their float functions; `casin`, `catan` and
/// `ctan` are `−i` times the hyperbolic function of `iz`.
pub(super) fn function<const WHICH: u8, const FLOAT: bool>(
    os: &mut Os<'_>,
    regs: &mut Regs,
) -> Result<(), Fail> {
    let z = argument::<FLOAT>(regs);
    give_complex::<FLOAT>(os, |a| match WHICH {
        ACOS => a.acos(z),
        ASIN => Ok(a.asinh(z.times_i())?.over_i()),
        ATAN => Ok(a.atanh(z.times_i())?.over_i()),
        COS => a.cos(z),
        SIN => a.sin(z),
        TAN => Ok(a.tanh(z.times_i())?.over_i()),
        ACOSH => a.acosh(z),
        ASINH => a.asinh(z),
        ATANH => a.atanh(z),
        COSH => a.cosh(z),
        SINH => a.sinh(z),
        TANH => a.tanh(z),
        EXP => a.exp(z),
        LOG => a.log(z),
        _ => a.sqrt(z),
    })
}

/// `cabs(z)` and `cabsf`.
pub(super) fn cabs<const FLOAT: bool>(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let z = argument::<FLOAT>(regs);
    give_real::<FLOAT>(os, |a| a.abs(z))
}

/// `carg(z)` and `cargf`.
pub(super) fn carg<const FLOAT: bool>(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let z = argument::<FLOAT>(regs);
    give_real::<FLOAT>(os, |a| a.arg(z))
}

/// `creal(z)`, `cimag` and their float functions.
pub(super) fn part<const IMAGINARY: bool, const FLOAT: bool>(
    os: &mut Os<'_>,
    regs: &mut Regs,
) -> Result<(), Fail> {
    let z = argument::<FLOAT>(regs);
    give_real::<FLOAT>(os, |_| Ok(if IMAGINARY { z.im } else { z.re }))
}

/// `conj(z)` and `conjf`.
pub(super) fn conj<const FLOAT: bool>(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let z = argument::<FLOAT>(regs);
    give_complex::<FLOAT>(os, |_| Ok(z.conj()))
}

/// `cproj(z)` and `cprojf`: an infinity projected to the positive real one,
/// with the imaginary part's sign on a nought.
pub(super) fn cproj<const FLOAT: bool>(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let z = argument::<FLOAT>(regs);
    give_complex::<FLOAT>(os, |_| {
        Ok(if z.re.is_infinite() || z.im.is_infinite() {
            complex(f64::INFINITY, 0.0_f64.copysign(z.im))
        } else {
            z
        })
    })
}

/// `cpow(z, w)`, `w` on the stack.
pub(super) fn cpow(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let z = double_argument(regs);
    let [re_high, re_low, im_high, im_low] = stacked(os)?;
    let w = complex(
        f64::from_bits(from_fpa_words([re_high, re_low])),
        f64::from_bits(from_fpa_words([im_high, im_low])),
    );
    give_complex::<false>(os, |a| a.pow(z, w))
}

/// `cpowf(z, w)`, `w` in R2 and R3.
pub(super) fn cpowf(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (z, w) = (float_argument(regs, 0), float_argument(regs, 2));
    give_complex::<true>(os, |a| a.pow(z, w))
}

/// The compiler's helpers.
pub(super) const MULTIPLY: u8 = 0;
pub(super) const DIVIDE: u8 = 1;
pub(super) const DIVIDE_REVERSED: u8 = 2;

/// The compiler's complex multiply, divide and reverse divide: `a + bi` in F0
/// and F1, `c + di` in F2 and F3, the answer rounded once to its precision.
pub(super) fn helper<const WHICH: u8, const FLOAT: bool>(
    os: &mut Os<'_>,
    _regs: &mut Regs,
) -> Result<(), Fail> {
    let mut operands = [0.0; 4];
    for (n, operand) in (0u8..).zip(operands.iter_mut()) {
        match os.fpa_double(n) {
            Ok(bits) => *operand = f64::from_bits(bits),
            Err(exception) => return Ok(os.floating_point(exception)?),
        }
    }
    let [a, b, c, d] = operands;
    let (first, second) = (complex(a, b), complex(c, d));
    let precision = precision(FLOAT);
    give_complex::<false>(os, |arithmetic| match WHICH {
        MULTIPLY => arithmetic.product(first, second, precision),
        DIVIDE => arithmetic.quotient(first, second, precision),
        _ => arithmetic.quotient(second, first, precision),
    })
}

#[cfg(test)]
mod tests {
    use super::{cis, Unit};

    #[test]
    fn r_cis_y_works_its_products_on_the_fpa() {
        let got = cis(&mut Unit::own(), f64::INFINITY, 0.0).expect("no trap");
        let want = Unit::own().mul(f64::INFINITY, 0.0).expect("no trap");
        assert_eq!(got.re.to_bits(), f64::INFINITY.to_bits());
        assert_eq!(
            got.im.to_bits(),
            want.to_bits(),
            "the FPA's NaN, not the host's"
        );
    }
}
