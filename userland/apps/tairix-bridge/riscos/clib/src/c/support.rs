//! The C compiler's support entries in chunk 2 that are not the library
//! kernel's own: the division test, multiplication, the access checks of
//! code compiled to check its memory accesses, profiling's counts, and the
//! packed decimal conversions.

use tairix_riscos_arm::fpa::{fpsr, Format};
use tairix_riscos_kernel::memory::layout::APPLICATION;
use tairix_riscos_kernel::os::{Fail, Os, Regs};
use tairix_softfloat::from_fpa_words;

use crate::kernel::frame::returning;
use crate::kernel::trap::{self, READ_FAIL, WRITE_FAIL};

/// `x$divtest(divisor)`: a divisor of nought a trap.
pub(super) fn divtest(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    if regs.r[0] == 0 {
        return trap::at_call(os, trap::DIVIDE_BY_ZERO);
    }
    Ok(())
}

/// `x$multiply(a, b)`: the product's low word.
#[expect(clippy::unnecessary_wraps, reason = "an entry's signature")]
pub(super) fn multiply(_os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    regs.r[0] = regs.r[0].wrapping_mul(regs.r[1]);
    Ok(())
}

/// The access an address check is for: a read or a write of a byte, a half
/// word or a word.
#[derive(Clone, Copy)]
struct Access {
    error: u32,
    size: u32,
}

/// An address checked for `access`: in the application space as far as the
/// session could ever reach it, and aligned to the access, else a trap with
/// the access's error at the call.
fn check(os: &mut Os<'_>, regs: &Regs, access: Access) -> Result<(), Fail> {
    let address = regs.r[0];
    let inside = (APPLICATION..os.application_ceiling()).contains(&address);
    if inside && address.is_multiple_of(access.size) {
        return Ok(());
    }
    trap::at_call(os, access.error)
}

/// `_rd1chk(address)`.
pub(super) fn read_byte_check(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    check(
        os,
        regs,
        Access {
            error: READ_FAIL,
            size: 1,
        },
    )
}

/// `_rd2chk(address)`.
pub(super) fn read_half_check(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    check(
        os,
        regs,
        Access {
            error: READ_FAIL,
            size: 2,
        },
    )
}

/// `_rd4chk(address)`.
pub(super) fn read_word_check(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    check(
        os,
        regs,
        Access {
            error: READ_FAIL,
            size: 4,
        },
    )
}

/// `_wr1chk(address)`.
pub(super) fn write_byte_check(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    check(
        os,
        regs,
        Access {
            error: WRITE_FAIL,
            size: 1,
        },
    )
}

/// `_wr2chk(address)`.
pub(super) fn write_half_check(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    check(
        os,
        regs,
        Access {
            error: WRITE_FAIL,
            size: 2,
        },
    )
}

/// `_wr4chk(address)`.
pub(super) fn write_word_check(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    check(
        os,
        regs,
        Access {
            error: WRITE_FAIL,
            size: 4,
        },
    )
}

/// The word after the call counted, and the call returned past it and the
/// `after` words that follow it.
fn count(os: &mut Os<'_>, after: u32) -> Result<(), Fail> {
    let mut registers = os.registers();
    let (counter, flags) = returning(os.caller_bits(), registers.r[14], registers.flags);
    let tally = os.as_caller().read_word(counter)?.wrapping_add(1);
    os.as_caller().write_word(counter, tally)?;
    registers.r[15] = counter.wrapping_add(4 * (1 + after));
    registers.flags = flags;
    os.leave_with(registers);
    Ok(())
}

/// `_count`: the word after the call counted, the call returned past it.
pub(super) fn count_next(os: &mut Os<'_>, _regs: &mut Regs) -> Result<(), Fail> {
    count(os, 0)
}

/// `_count1`: as `_count`, the source position's word after the counted one
/// skipped too.
pub(super) fn count_with_position(os: &mut Os<'_>, _regs: &mut Regs) -> Result<(), Fail> {
    count(os, 1)
}

/// The words a packed decimal takes: four where the FPSR's EP bit says so.
fn packed_words(os: &mut Os<'_>) -> usize {
    Format::Packed.words(os.fpsr() & fpsr::EP != 0)
}

/// `_stfp(d, x)`: `d` stored at `x` as the guest's FPA stores it with
/// `STFP`, by way of F0; an enabled trap raised as FPEmulator raises it.
pub(super) fn stfp(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    os.set_fpa_double(0, from_fpa_words([regs.r[0], regs.r[1]]));
    let words = match os.fpa_packed(0) {
        Ok(words) => words,
        Err(exception) => return Ok(os.floating_point(exception)?),
    };
    let count = packed_words(os);
    os.as_caller().write_words(regs.r[2], &words[..count])
}

/// `_ldfp(x)`: the packed decimal at `x` loaded as the guest's FPA loads it
/// with `LDFP`, then rounded to a double, in F0; an enabled trap raised as
/// FPEmulator raises it.
pub(super) fn ldfp(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let mut words = [0; 4];
    let count = packed_words(os);
    os.as_caller().read_words(regs.r[0], &mut words[..count])?;
    let loaded = os.set_fpa_packed(0, words).and_then(|()| os.fpa_double(0));
    match loaded {
        Ok(bits) => os.set_fpa_double(0, bits),
        Err(exception) => os.floating_point(exception)?,
    }
    Ok(())
}

#[cfg(test)]
mod tests;
