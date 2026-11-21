//! BASIC's values: 32-bit integers, the dialect's reals and strings of up to
//! 255 bytes, and the arithmetic each dialect does on them.

use core::cmp::Ordering;

use tairix_riscos_arm::fpa::{Format, Rounding};
use tairix_riscos_kernel::os::len32;

use crate::five::Five;
use crate::six::{Arithmetic, Six};
use crate::Fault;

/// The two dialects: BASIC V's five-byte reals and BASIC VI's eight-byte
/// ones.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Dialect {
    /// BASIC V.
    V,
    /// BASIC VI, BASIC64.
    VI,
}

impl Dialect {
    /// The bytes a real takes in memory, which is also its type in `CALL`'s
    /// parameter list.
    pub(crate) const fn real_bytes(self) -> u32 {
        match self {
            Self::V => len32(crate::five::BYTES),
            Self::VI => len32(crate::six::BYTES),
        }
    }
}

/// A real: its bits, read as the machine's dialect reads them — BASIC V's
/// five bytes in the low forty, BASIC VI's double whole.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Real(u64);

impl Real {
    fn from_five(x: Five) -> Self {
        let mut bits = [0; 8];
        bits[..crate::five::BYTES].copy_from_slice(&x.to_bytes());
        Self(u64::from_le_bytes(bits))
    }

    fn five(self) -> Five {
        five_in(self.0.to_le_bytes())
    }

    const fn from_six(x: Six) -> Self {
        Self(x.to_bits())
    }

    const fn six(self) -> Six {
        Six::from_bits(self.0)
    }
}

/// The longest string BASIC holds.
pub const STRING_MOST: usize = 255;

/// A string's bytes.
#[derive(Clone, Copy)]
pub(crate) struct Text {
    len: u8,
    bytes: [u8; STRING_MOST],
}

impl Text {
    /// The empty string.
    pub(crate) const EMPTY: Self = Self {
        len: 0,
        bytes: [0; STRING_MOST],
    };

    /// `bytes`, where they fit.
    pub(crate) fn of(bytes: &[u8]) -> Option<Self> {
        let mut text = Self::EMPTY;
        text.bytes.get_mut(..bytes.len())?.copy_from_slice(bytes);
        text.len = u8::try_from(bytes.len()).ok()?;
        Some(text)
    }

    /// Its bytes.
    pub(crate) fn as_bytes(&self) -> &[u8] {
        &self.bytes[..usize::from(self.len)]
    }

    /// Its length.
    pub(crate) const fn len(&self) -> u8 {
        self.len
    }

    /// `n` more bytes at its end, written by `fill`; none past the longest
    /// string.
    pub(crate) fn extend(&mut self, n: usize, fill: impl FnOnce(&mut [u8])) -> Option<()> {
        let start = usize::from(self.len);
        let end = start.checked_add(n).filter(|&end| end <= STRING_MOST)?;
        fill(&mut self.bytes[start..end]);
        self.len = u8::try_from(end).ok()?;
        Some(())
    }

    /// `more` added at its end, none past the longest string.
    pub(crate) fn push_bytes(&mut self, more: &[u8]) -> Option<()> {
        self.extend(more.len(), |room| room.copy_from_slice(more))
    }

    /// Its bytes from `start` for `count`, within it.
    pub(crate) fn slice(&self, start: usize, count: usize) -> Self {
        let bytes = self.as_bytes();
        let start = start.min(bytes.len());
        let end = start.saturating_add(count).min(bytes.len());
        Self::of(&bytes[start..end]).unwrap_or(Self::EMPTY)
    }

    /// Its whole room: a string is copied to its block a word at a time.
    pub(crate) const fn room(&self) -> &[u8; STRING_MOST] {
        &self.bytes
    }
}

impl PartialEq for Text {
    fn eq(&self, other: &Self) -> bool {
        self.as_bytes() == other.as_bytes()
    }
}

impl Eq for Text {}

impl core::fmt::Debug for Text {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "\"{}\"", self.as_bytes().escape_ascii())
    }
}

/// A number.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Number {
    /// An integer.
    Int(i32),
    /// A real.
    Real(Real),
}

/// A value.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[expect(
    clippy::large_enum_variant,
    reason = "a string, at most 255 bytes, is carried inline so evaluating one never allocates"
)]
pub(crate) enum Value {
    /// An integer.
    Int(i32),
    /// A real.
    Real(Real),
    /// A string.
    Str(Text),
}

impl Value {
    /// The number it is, none for a string.
    pub(crate) const fn number(&self) -> Option<Number> {
        match *self {
            Self::Int(n) => Some(Number::Int(n)),
            Self::Real(r) => Some(Number::Real(r)),
            Self::Str(_) => None,
        }
    }
}

impl From<Number> for Value {
    fn from(number: Number) -> Self {
        match number {
            Number::Int(n) => Self::Int(n),
            Number::Real(r) => Self::Real(r),
        }
    }
}

/// The dialect's arithmetic: BASIC V's stateless, BASIC VI's under the
/// program's floating-point status, which the machine carries between the
/// guest's processor and it.
pub(crate) struct Arith {
    dialect: Dialect,
    six: Arithmetic,
}

impl Arith {
    /// The arithmetic of `dialect`, BASIC VI's under `fpsr`.
    pub(crate) fn new(dialect: Dialect, fpsr: u32) -> Self {
        Self {
            dialect,
            six: Arithmetic::new(fpsr),
        }
    }

    /// The dialect.
    pub(crate) const fn dialect(&self) -> Dialect {
        self.dialect
    }

    /// The floating-point status BASIC VI's arithmetic leaves.
    pub(crate) const fn fpsr(&self) -> u32 {
        self.six.fpsr()
    }

    /// The BASIC VI arithmetic, for its conversions to and from text.
    pub(crate) const fn six(&mut self) -> &mut Arithmetic {
        &mut self.six
    }

    /// `real` as BASIC V's.
    pub(crate) fn as_five(real: Real) -> Five {
        real.five()
    }

    /// `real` as BASIC VI's.
    pub(crate) const fn as_six(real: Real) -> Six {
        real.six()
    }

    /// BASIC V's `x`.
    pub(crate) fn of_five(x: Five) -> Real {
        Real::from_five(x)
    }

    /// BASIC VI's `x`.
    pub(crate) const fn of_six(x: Six) -> Real {
        Real::from_six(x)
    }

    /// `n` as a real.
    pub(crate) fn real(&mut self, n: i32) -> Result<Real, Fault> {
        match self.dialect {
            Dialect::V => Ok(Real::from_five(Five::from_int(n))),
            Dialect::VI => self.six.from_int(n).map(Real::from_six),
        }
    }

    /// `number` as a real.
    pub(crate) fn real_of(&mut self, number: Number) -> Result<Real, Fault> {
        match number {
            Number::Int(n) => self.real(n),
            Number::Real(r) => Ok(r),
        }
    }

    /// `real` truncated to an integer.
    pub(crate) fn truncated(&mut self, real: Real) -> Result<i32, Fault> {
        match self.dialect {
            Dialect::V => real.five().to_int(),
            Dialect::VI => self.six.to_int(real.six()),
        }
    }

    /// `number` as an integer, a real truncated.
    pub(crate) fn int(&mut self, number: Number) -> Result<i32, Fault> {
        match number {
            Number::Int(n) => Ok(n),
            Number::Real(r) => self.truncated(r),
        }
    }

    /// `real`'s floor, as `INT` gives it.
    pub(crate) fn floor(&mut self, real: Real) -> Result<i32, Fault> {
        match self.dialect {
            Dialect::V => real.five().floor(),
            Dialect::VI => self.six.floor(real.six()),
        }
    }

    /// `bits` / 2^32 exactly: a fraction in [0, 1).
    #[expect(
        clippy::cast_possible_truncation,
        reason = "a nonzero word has fewer than 32 leading zeros"
    )]
    pub(crate) fn unit_fraction(&self, bits: u32) -> Real {
        if bits == 0 {
            return self.zero();
        }
        let shift = bits.leading_zeros();
        let normal = bits << shift;
        match self.dialect {
            Dialect::V => {
                let mantissa = normal & !(1 << 31);
                let [a, b, c, d] = mantissa.to_le_bytes();
                let exponent = 128 - shift as u8;
                Real::from_five(Five::from_bytes([a, b, c, d, exponent]))
            }
            Dialect::VI => {
                let exponent = u64::from(1023 - 1 - shift);
                let fraction = u64::from(normal & !(1 << 31)) << 21;
                Real::from_six(Six::from_bits(exponent << 52 | fraction))
            }
        }
    }

    /// `number`'s value as a double's bits, exactly: BASIC V's 32-bit
    /// mantissa fits a double's, and BASIC VI's reals are doubles.
    pub(crate) fn double_bits(&self, number: Number) -> u64 {
        match number {
            Number::Int(n) => exact_double(n),
            Number::Real(r) => match self.dialect {
                Dialect::V => r.five().to_double_bits(),
                Dialect::VI => r.six().to_bits(),
            },
        }
    }

    /// `number` in an FPA memory `format`, words high first: BASIC V's exact
    /// value cut toward zero to the format, as BASIC V stores one; BASIC
    /// VI's rounded to nearest by the FPA under the program's traps.
    pub(crate) fn fpa_words(&mut self, number: Number, format: Format) -> Result<[u32; 3], Fault> {
        let bits = self.double_bits(number);
        match self.dialect {
            Dialect::VI => self
                .six
                .stored(Six::from_bits(bits), format, Rounding::Nearest),
            // BASIC V's own conversion leaves the program's status alone.
            Dialect::V => {
                let status = self.six.fpsr();
                let words = self
                    .six
                    .stored(Six::from_bits(bits), format, Rounding::Zero);
                self.six.set_fpsr(status);
                words
            }
        }
    }

    /// The real zero.
    pub(crate) fn zero(&self) -> Real {
        match self.dialect {
            Dialect::V => Real::from_five(Five::ZERO),
            Dialect::VI => Real::from_six(Six::ZERO),
        }
    }

    /// `PI`.
    pub(crate) fn pi(&self) -> Real {
        match self.dialect {
            Dialect::V => Real::from_five(Five::PI),
            Dialect::VI => Real::from_six(Six::PI),
        }
    }

    /// A real of the dialect from its bytes in memory, which are at least
    /// [`Dialect::real_bytes`] long.
    pub(crate) fn decode(&self, bytes: &[u8]) -> Real {
        let mut eight = [0; 8];
        let n = (self.dialect.real_bytes() as usize).min(bytes.len());
        eight[..n].copy_from_slice(&bytes[..n]);
        match self.dialect {
            Dialect::V => Real::from_five(five_in(eight)),
            Dialect::VI => Real::from_six(Six::from_bytes(eight)),
        }
    }

    /// `real`'s bytes in memory: the first [`Dialect::real_bytes`] of them.
    pub(crate) fn encode(&self, real: Real) -> [u8; 8] {
        match self.dialect {
            Dialect::V => Real::from_five(real.five()).0.to_le_bytes(),
            Dialect::VI => real.six().to_bytes(),
        }
    }

    /// `real`'s bytes in memory as the two words a record keeps them in.
    pub(crate) fn encode_words(&self, real: Real) -> [u32; 2] {
        let bits = u64::from_le_bytes(self.encode(real));
        [(bits & 0xFFFF_FFFF) as u32, (bits >> 32) as u32]
    }

    /// The real whose bytes in memory two record words hold.
    pub(crate) fn decode_words(&self, [low, high]: [u32; 2]) -> Real {
        self.decode(&(u64::from(high) << 32 | u64::from(low)).to_le_bytes())
    }

    /// Whether `real` is zero.
    pub(crate) fn is_zero(&self, real: Real) -> bool {
        match self.dialect {
            Dialect::V => real.five().is_zero(),
            Dialect::VI => real.six().is_zero(),
        }
    }

    /// Whether `real` is below zero.
    pub(crate) fn is_negative(&self, real: Real) -> bool {
        match self.dialect {
            Dialect::V => real.five().is_negative(),
            Dialect::VI => real.six().is_negative(),
        }
    }

    /// `a + b`: integers wrap, as BASIC V's do.
    pub(crate) fn add(&mut self, a: Number, b: Number) -> Result<Number, Fault> {
        if let (Number::Int(x), Number::Int(y)) = (a, b) {
            return Ok(Number::Int(x.wrapping_add(y)));
        }
        self.real_op(a, b, Five::add, Arithmetic::add)
    }

    /// `a - b`.
    pub(crate) fn sub(&mut self, a: Number, b: Number) -> Result<Number, Fault> {
        if let (Number::Int(x), Number::Int(y)) = (a, b) {
            return Ok(Number::Int(x.wrapping_sub(y)));
        }
        self.real_op(a, b, Five::sub, Arithmetic::sub)
    }

    /// `a * b`: integers exact where the product fits, a real otherwise.
    pub(crate) fn mul(&mut self, a: Number, b: Number) -> Result<Number, Fault> {
        if let (Number::Int(x), Number::Int(y)) = (a, b) {
            if let Some(product) = x.checked_mul(y) {
                return Ok(Number::Int(product));
            }
            if self.dialect == Dialect::V {
                let exact = Five::from_i64(i64::from(x) * i64::from(y));
                return Ok(Number::Real(Real::from_five(exact)));
            }
        }
        self.real_op(a, b, Five::mul, Arithmetic::mul)
    }

    /// `a / b`, always a real.
    pub(crate) fn div(&mut self, a: Number, b: Number) -> Result<Number, Fault> {
        self.real_op(a, b, Five::div, Arithmetic::div)
    }

    /// `a / b` with no check of the divisor in BASIC VI, as the statements
    /// that divide inside themselves make it.
    pub(crate) fn quotient(&mut self, a: Number, b: Number) -> Result<Number, Fault> {
        self.real_op(a, b, Five::div, Arithmetic::quotient)
    }

    /// `a ^ b`: BASIC V raises to an integer power by squaring.
    pub(crate) fn pow(&mut self, a: Number, b: Number) -> Result<Number, Fault> {
        if let (Dialect::V, Number::Int(n)) = (self.dialect, b) {
            let x = self.real_of(a)?.five();
            return x.pow_int(n).map(|r| Number::Real(Real::from_five(r)));
        }
        self.real_op(a, b, Five::pow, Arithmetic::pow)
    }

    /// `a DIV b` and `a MOD b`, each operand truncated to an integer.
    pub(crate) fn div_mod(&mut self, a: Number, b: Number) -> Result<(i32, i32), Fault> {
        let (x, y) = (self.int(a)?, self.int(b)?);
        if y == 0 {
            return Err(Fault::DivisionByZero);
        }
        Ok((x.wrapping_div(y), x.wrapping_rem(y)))
    }

    /// `-a`: an integer wraps.
    pub(crate) fn negate(&mut self, a: Number) -> Result<Number, Fault> {
        Ok(match (self.dialect, a) {
            (_, Number::Int(n)) => Number::Int(n.wrapping_neg()),
            (Dialect::V, Number::Real(r)) => Number::Real(Real::from_five(r.five().negated())),
            (Dialect::VI, Number::Real(r)) => {
                Number::Real(Real::from_six(self.six.negate(r.six())?))
            }
        })
    }

    /// `ABS a`: an integer wraps.
    pub(crate) fn abs(&mut self, a: Number) -> Result<Number, Fault> {
        Ok(match (self.dialect, a) {
            (_, Number::Int(n)) => Number::Int(n.wrapping_abs()),
            (Dialect::V, Number::Real(r)) => Number::Real(Real::from_five(r.five().magnitude())),
            (Dialect::VI, Number::Real(r)) => Number::Real(Real::from_six(self.six.abs(r.six())?)),
        })
    }

    /// How `a` compares with `b`; none where BASIC VI meets a NaN.
    pub(crate) fn compare(&mut self, a: Number, b: Number) -> Result<Option<Ordering>, Fault> {
        if let (Number::Int(x), Number::Int(y)) = (a, b) {
            return Ok(Some(x.cmp(&y)));
        }
        let (x, y) = (self.real_of(a)?, self.real_of(b)?);
        match self.dialect {
            Dialect::V => Ok(Some(x.five().cmp(&y.five()))),
            Dialect::VI => self.six.compare(x.six(), y.six()),
        }
    }

    /// A function of one real, each dialect's.
    pub(crate) fn function(
        &mut self,
        a: Number,
        five: fn(Five) -> Result<Five, Fault>,
        six: fn(&mut Arithmetic, Six) -> Result<Six, Fault>,
    ) -> Result<Real, Fault> {
        let x = self.real_of(a)?;
        match self.dialect {
            Dialect::V => five(x.five()).map(Real::from_five),
            Dialect::VI => six(&mut self.six, x.six()).map(Real::from_six),
        }
    }

    fn real_op(
        &mut self,
        a: Number,
        b: Number,
        five: fn(Five, Five) -> Result<Five, Fault>,
        six: fn(&mut Arithmetic, Six, Six) -> Result<Six, Fault>,
    ) -> Result<Number, Fault> {
        let (x, y) = (self.real_of(a)?, self.real_of(b)?);
        match self.dialect {
            Dialect::V => five(x.five(), y.five()).map(|r| Number::Real(Real::from_five(r))),
            Dialect::VI => {
                six(&mut self.six, x.six(), y.six()).map(|r| Number::Real(Real::from_six(r)))
            }
        }
    }
}

/// The five-byte real the first five of `bytes` hold.
fn five_in(bytes: [u8; 8]) -> Five {
    let mut five = [0; crate::five::BYTES];
    five.copy_from_slice(&bytes[..crate::five::BYTES]);
    Five::from_bytes(five)
}

/// `n` as a double's bits, exactly.
const fn exact_double(n: i32) -> u64 {
    if n == 0 {
        return 0;
    }
    let sign = if n < 0 { 1u64 << 63 } else { 0 };
    let magnitude = n.unsigned_abs();
    let top = magnitude.ilog2();
    let exponent = (1023 + top) as u64;
    let fraction = ((magnitude as u64) << (52 - top)) & ((1 << 52) - 1);
    sign | exponent << 52 | fraction
}
