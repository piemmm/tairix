//! `BootCommands`' memory commands, each a change of a dynamic area as RISC
//! OS 5's makes it: `*AddToRMA`, `*ShrinkRMA`, `*AppSize`, `*AppSlot` and
//! `*FreePool`.

use tairix_riscos_kernel::memory::{AreaError, FREE_POOL, RMA};
use tairix_riscos_kernel::os::{Fail, Os, Regs};
use tairix_riscos_kernel::swi::os::read_unsigned::DECIMAL;
use tairix_riscos_kernel::swi::os::{self as swi};
use tairix_riscos_kernel::swi::X;

/// The most a shrink asks for: every page an area can give.
const ALL: i32 = -0x7FFF_F000;

/// The bytes a size parameter at `at` gives, as RISC OS 5 reads one: a
/// decimal count of bytes, or of kilobytes or megabytes where `K` or `M`
/// follows it, in 32 bits; whatever else follows the digits is let be.
pub(super) fn size(os: &mut Os<'_>, at: u32) -> Result<u32, Fail> {
    let (count, end) = os.number_at(at, DECIMAL, 0)?;
    let scale: u32 = match os.read_byte(end)? {
        b'K' | b'k' => 1 << 10,
        b'M' | b'm' => 1 << 20,
        _ => 1,
    };
    Ok(count.wrapping_mul(scale))
}

/// `area` changed by `bytes`: the bytes moved.
fn change(os: &mut Os<'_>, area: u32, bytes: i32) -> Result<u32, Fail> {
    let mut regs = Regs::default();
    regs.r[..2].copy_from_slice(&[area, bytes.cast_unsigned()]);
    os.perform(X | swi::CHANGE_DYNAMIC_AREA, &mut regs)?;
    Ok(regs.r[1])
}

/// The RMA given back every free page at its top: a shrink cut short where
/// they end is the shrink asked for.
fn shrink_rma(os: &mut Os<'_>) -> Result<(), Fail> {
    match change(os, RMA, ALL) {
        Ok(_) => Ok(()),
        Err(Fail::Error(block)) if is_not_all_moved(os, block)? => Ok(()),
        Err(fail) => Err(fail),
    }
}

/// Whether the error at `block` is the area manager's "not all moved".
fn is_not_all_moved(os: &mut Os<'_>, block: u32) -> Result<bool, Fail> {
    let not_all_moved = AreaError::NotAllMoved
        .number_and_token()
        .map(|(number, _)| number);
    Ok(Some(os.read_word(block)?) == not_all_moved)
}

/// `*AddToRMA <size>[K|M]`: the RMA changed by the size, in whole pages; as
/// on RISC OS 5, a size past half the address space asks for a shrink.
pub(super) fn add_to_rma(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let bytes = size(os, regs.r[0])?;
    change(os, RMA, bytes.cast_signed()).map(drop)
}

/// `*ShrinkRMA`.
pub(super) fn shrink(os: &mut Os<'_>, _regs: &mut Regs) -> Result<(), Fail> {
    shrink_rma(os)
}

/// `*AppSize <size>[K]`: the size read, and the RMA shrunk as far as it
/// goes, as RISC OS 5's is whatever the size.
pub(super) fn app_size(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    size(os, regs.r[0])?;
    shrink_rma(os)
}

/// The size of application space, from `&8000`.
fn application_size(os: &mut Os<'_>) -> Result<u32, Fail> {
    let mut regs = Regs::default();
    regs.r[0] = u32::MAX;
    os.perform(X | swi::READ_DYNAMIC_AREA, &mut regs)?;
    Ok(regs.r[1])
}

/// `*AppSlot <size>[K|M]`: application space made the size, the free pool
/// giving or taking the difference.
pub(super) fn app_slot(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let wanted = size(os, regs.r[0])?;
    let growth = wanted.wrapping_sub(application_size(os)?).cast_signed();
    if growth == 0 {
        return Ok(());
    }
    change(os, FREE_POOL, growth.wrapping_neg()).map(drop)
}

/// `*FreePool`: every page of application space given to the free pool.
pub(super) fn free_pool(os: &mut Os<'_>, _regs: &mut Regs) -> Result<(), Fail> {
    let now = application_size(os)?;
    change(os, FREE_POOL, now.cast_signed()).map(drop)
}
