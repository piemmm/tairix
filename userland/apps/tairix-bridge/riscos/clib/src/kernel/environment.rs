//! The handlers a client's library sets in place of its parent's, as RISC
//! OS 5.30's `_kernel_init` sets them, and their parent's put back.

use tairix_riscos_kernel::os::{Entry, Fail, Os, Regs, Target};
use tairix_riscos_kernel::swi::os as kos;

use super::statics::{at, Statics};

/// `OS_ChangeEnvironment`'s numbers the library sets: its handlers, and
/// for a sub-program the memory limit and the current active object.
pub(crate) use tairix_riscos_kernel::swi::os::environment as handler;

/// The exception handlers, the first of [`SET`], each a trampoline.
pub(crate) const EXCEPTION_HANDLERS: u32 = 4;

/// The handlers the library sets, in the order it sets and saves them.
pub(crate) const SET: [u32; HANDLERS as usize] = [
    handler::UNDEFINED,
    handler::PREFETCH_ABORT,
    handler::DATA_ABORT,
    handler::ADDRESS_EXCEPTION,
    handler::ERROR,
    handler::CALLBACK,
    handler::ESCAPE,
    handler::EVENT,
    handler::EXIT,
    handler::UPCALL,
];

/// How many it sets, and each saved one's bytes: its code, its R12 and its
/// buffer, a word each.
const HANDLERS: u32 = 10;
const HANDLER_BYTES: u32 = 12;
/// The saved handlers' bytes.
pub(crate) const SAVED_BYTES: u32 = HANDLER_BYTES * HANDLERS;

/// `handler` set to `code` with `workspace` and `buffer`, the one it
/// replaces saved in its place among the statics' saved handlers.
pub(crate) fn set(
    os: &mut Os<'_>,
    statics: Statics,
    place: u32,
    handler: u32,
    words: [u32; 3],
) -> Result<(), Fail> {
    let replaced = change(os, handler, words)?;
    let saved = at::SAVED_HANDLERS + HANDLER_BYTES * place;
    for (offset, word) in (0..).zip(replaced) {
        statics.write(os, saved + 4 * offset, word)?;
    }
    Ok(())
}

/// The words — code, R12, buffer — of the handler in force before the
/// library set `handler`; nought for one it does not set.
pub(crate) fn saved(os: &mut Os<'_>, statics: Statics, handler: u32) -> Result<[u32; 3], Fail> {
    let mut words = [0; 3];
    if let Some(place) = (0..)
        .zip(&SET)
        .find_map(|(place, &set)| (set == handler).then_some(place))
    {
        let saved = at::SAVED_HANDLERS + HANDLER_BYTES * place;
        for (offset, slot) in (0..).zip(words.iter_mut()) {
            *slot = statics.read(os, saved + 4 * offset)?;
        }
    }
    Ok(words)
}

/// `number` given `words` — code, R12, buffer — as they stand, nothing
/// saved; the words it replaces answered.
pub(crate) fn change(os: &mut Os<'_>, number: u32, words: [u32; 3]) -> Result<[u32; 3], Fail> {
    let mut regs = Regs::default();
    regs.r[0] = number;
    regs.r[1..4].copy_from_slice(&words);
    os.perform(
        kos::CHANGE_ENVIRONMENT | tairix_riscos_kernel::swi::X,
        &mut regs,
    )?;
    Ok([regs.r[1], regs.r[2], regs.r[3]])
}

/// The upcall in `regs` passed on to the upcall handler in force before
/// the library set its own, its answer taken back.
pub(crate) fn pass_up_call(os: &mut Os<'_>, statics: Statics, regs: &mut Regs) -> Result<(), Fail> {
    let [code, workspace, _] = saved(os, statics, handler::UPCALL)?;
    if code == 0 {
        return Ok(());
    }
    let mut call = *regs;
    call.r[12] = workspace;
    let bits = os.caller_bits();
    os.call(
        Target {
            address: code,
            bits,
        },
        &mut call,
        Entry::Supervisor,
    )?;
    regs.r[..10].copy_from_slice(&call.r[..10]);
    regs.flags = call.flags;
    Ok(())
}

/// The handlers the library set put back as they were before it set them.
pub(crate) fn restore(os: &mut Os<'_>, statics: Statics) -> Result<(), Fail> {
    for &handler in &SET {
        let words = saved(os, statics, handler)?;
        change(os, handler, words)?;
    }
    Ok(())
}
