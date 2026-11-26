//! The heap above the extension stack, kept as RISC OS 5.30's library keeps
//! it: the stack chunks it claims for itself and `_kernel_alloc`'s blocks
//! bumped from one pointer and never given back; and the allocators and
//! slot-extension procedure a client registers.
//!
//! Outside the desktop the heap ends where `_kernel_init` was told the
//! workspace does, and the registered procedure is not asked: a request past
//! the end gets what is left.

use tairix_riscos_kernel::os::{Fail, Os, Regs};

use super::context;
use super::statics::{at, Statics};

/// `_kernel_alloc`'s least block, in words.
const ALLOC_LEAST_WORDS: u32 = 2048;

/// `bytes` from the heap, none where it has not got them.
pub(crate) fn claim(os: &mut Os<'_>, statics: Statics, bytes: u32) -> Result<Option<u32>, Fail> {
    let next = statics.read(os, at::HEAP)?;
    let limit = statics.read(os, at::HEAP_LIMIT)?;
    let Some(end) = next.checked_add(bytes).filter(|&end| end <= limit) else {
        return Ok(None);
    };
    statics.write(os, at::HEAP, end)?;
    Ok(Some(next))
}

/// `words` words from the heap, at least 2048, or what it has left, which
/// may be none: the block and its size in words.
pub(crate) fn take(os: &mut Os<'_>, statics: Statics, words: u32) -> Result<(u32, u32), Fail> {
    let next = statics.read(os, at::HEAP)?;
    let left = statics.read(os, at::HEAP_LIMIT)?.saturating_sub(next) / 4;
    let words = words.max(ALLOC_LEAST_WORDS).min(left);
    statics.write(os, at::HEAP, next.wrapping_add(4 * words))?;
    Ok((next, words))
}

/// `_kernel_alloc(words, block)`: as [`take`] gives them, the size answered
/// in words and the block's address put at `block`, nought for none.
pub(super) fn alloc(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let statics = context::statics_at(os, regs.r[10])?;
    let (block, words) = take(os, statics, regs.r[0])?;
    os.as_caller()
        .write_word(regs.r[1], if words == 0 { 0 } else { block })?;
    regs.r[0] = words;
    Ok(())
}

/// `_kernel_register_allocs(alloc, free)`: the procedures stack chunks are
/// claimed and freed through from now on.
pub(super) fn register_allocs(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let statics = context::statics_at(os, regs.r[10])?;
    statics.write(os, at::ALLOC, regs.r[0])?;
    statics.write(os, at::FREE, regs.r[1])
}

/// `_kernel_register_slotextend(proc)`: the procedure asked for the space
/// above the heap, the one it replaces answered.
pub(super) fn register_slotextend(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let statics = context::statics_at(os, regs.r[10])?;
    let previous = statics.read(os, at::SLOT_EXTEND)?;
    statics.write(os, at::SLOT_EXTEND, regs.r[0])?;
    regs.r[0] = previous;
    Ok(())
}
