//! The library kernel's arithmetic: division and remainder, unsigned and
//! signed, by a divisor or by ten, as RISC OS 5.30's give them — quotients
//! toward nought, remainders the dividend's sign, a quotient past a word
//! wrapping, and a divisor of nought a trap.

use tairix_riscos_kernel::os::{Fail, Os, Regs};

use super::trap;

/// `_kernel_udiv(divisor, dividend)`: the quotient, the remainder in R1.
pub(crate) fn udiv(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (divisor, dividend) = (regs.r[0], regs.r[1]);
    let Some(quotient) = dividend.checked_div(divisor) else {
        return trap::at_call(os, trap::DIVIDE_BY_ZERO);
    };
    regs.r[0] = quotient;
    regs.r[1] = dividend % divisor;
    Ok(())
}

/// `_kernel_urem(divisor, dividend)`: the remainder, in R0 and R1.
pub(crate) fn urem(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (divisor, dividend) = (regs.r[0], regs.r[1]);
    let Some(remainder) = dividend.checked_rem(divisor) else {
        return trap::at_call(os, trap::DIVIDE_BY_ZERO);
    };
    regs.r[0] = remainder;
    regs.r[1] = remainder;
    Ok(())
}

/// `_kernel_udiv10(dividend)`: the quotient, the remainder in R1.
#[expect(clippy::unnecessary_wraps, reason = "an entry's signature")]
pub(super) fn udiv10(_os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let dividend = regs.r[0];
    regs.r[0] = dividend / 10;
    regs.r[1] = dividend % 10;
    Ok(())
}

/// `_kernel_sdiv(divisor, dividend)`: the quotient toward nought, the
/// remainder in R1 with the dividend's sign.
pub(crate) fn sdiv(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (divisor, dividend) = (regs.r[0].cast_signed(), regs.r[1].cast_signed());
    if divisor == 0 {
        return trap::at_call(os, trap::DIVIDE_BY_ZERO);
    }
    regs.r[0] = dividend.wrapping_div(divisor).cast_unsigned();
    regs.r[1] = dividend.wrapping_rem(divisor).cast_unsigned();
    Ok(())
}

/// `_kernel_srem(divisor, dividend)`: the remainder, in R0 and R1.
pub(crate) fn srem(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (divisor, dividend) = (regs.r[0].cast_signed(), regs.r[1].cast_signed());
    if divisor == 0 {
        return trap::at_call(os, trap::DIVIDE_BY_ZERO);
    }
    let remainder = dividend.wrapping_rem(divisor).cast_unsigned();
    regs.r[0] = remainder;
    regs.r[1] = remainder;
    Ok(())
}

/// `_kernel_sdiv10(dividend)`: the quotient toward nought, the remainder in
/// R1.
#[expect(clippy::unnecessary_wraps, reason = "an entry's signature")]
pub(super) fn sdiv10(_os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let dividend = regs.r[0].cast_signed();
    regs.r[0] = (dividend / 10).cast_unsigned();
    regs.r[1] = (dividend % 10).cast_unsigned();
    Ok(())
}
