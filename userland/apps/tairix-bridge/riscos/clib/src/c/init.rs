//! The C library's start and end: `_clib_initialise` laying the C statics as
//! RISC OS 5.30's starts them, `_main` entering a program's `main`, `exit`
//! and `atexit`, the finalisation the C language's description names, a
//! module's own finalisation, and the library's version.

use ::alloc::vec::Vec;

use tairix_riscos_kernel::os::{Fail, Os, Regs};
use tairix_softfloat::fpa_words;

use super::args::{self, Malformed};
use super::statics::{at, EXIT_LIST_HERE};
use super::stdio::{open, stream};
use super::{alloc, locale, math, report, signal, stdlib, time};
use crate::chunk;
use crate::client;
use crate::kernel::exit::{self, Then};
use crate::kernel::init::COMMAND_MOST;
use crate::kernel::statics::{at as kernel_at, Statics};
use crate::kernel::{context, template, Routine};
use crate::module::message_with;

/// The seed the random number generator starts with.
const FIRST_SEED: u32 = 1;

/// `_clib_initialise()`: `HUGE_VAL` laid, the program's start noted for
/// `clock`, the C locale, every signal's handler the default, the random
/// number generator seeded with one, the heap's first arena, which the
/// library kernel's stack chunks are claimed from from now on, and the
/// streams.
pub(super) fn clib_initialise(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let statics = context::statics_at(os, regs.r[10])?;
    let lists = [AT_EXIT, AT_QUICK_EXIT].map(|list| [list.count, list.more]);
    for field in [at::ERRNO, at::TOKEN_NEXT, at::GETENV]
        .into_iter()
        .chain(lists.into_iter().flatten())
    {
        statics.write(os, field, 0)?;
    }
    os.as_caller().write_words(
        statics.field(at::HUGE_VAL),
        &fpa_words(math::HUGE_VAL.to_bits()),
    )?;
    let started = time::monotonic_time(os)?;
    statics.write(os, at::CLOCK_START, started)?;
    locale::start_in_c(os, statics)?;
    signal::start(os, statics)?;
    stdlib::seed(os, statics, FIRST_SEED)?;
    if alloc::begin(os, statics)? {
        let claim = context::routine(os, Routine::HeapAlloc)?;
        let free = context::routine(os, Routine::HeapFree)?;
        statics.write(os, kernel_at::ALLOC, claim)?;
        statics.write(os, kernel_at::FREE, free)?;
    }
    stream::start(os, statics)?;
    regs.r[0] = 0;
    Ok(())
}

/// `_main(command line, main)`: the line read into `argc` and `argv`, its
/// redirections made in turn, and `main` entered with them, its return
/// going on into `exit`; a line that cannot be read reported on `stderr`
/// and the program ended failing.
pub(super) fn main(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let statics = context::statics_at(os, regs.r[10])?;
    let main = regs.r[1];
    let line = os
        .as_caller()
        .read_until(regs.r[0], COMMAND_MOST, |b| b < b' ')?
        .0;
    let parsed = match args::parse(&line) {
        Ok(parsed) => parsed,
        Err(malformed) => {
            let token = match malformed {
                Malformed::MissingQuote => "MissingQuote",
                Malformed::IllegalRedirection => "BadRedirection",
                Malformed::NoMemory => "NoArgumentsRoom",
            };
            return refused(os, statics, token, &[]);
        }
    };
    for redirection in &parsed.redirections {
        if !open::redirect(os, statics, redirection)? {
            let name = match redirection {
                args::Redirection::File { name, .. } => name.as_slice(),
                args::Redirection::ErrorToOutput => &[],
            };
            return refused(os, statics, "RedirectionFailed", &[name]);
        }
    }
    let Some(argv) = arguments(os, statics, &parsed.words)? else {
        return refused(os, statics, "NoArgumentsRoom", &[]);
    };
    let mut registers = os.registers();
    registers.r[0] = u32::try_from(parsed.words.len()).unwrap_or(u32::MAX);
    registers.r[1] = argv;
    registers.r[14] = context::routine(os, Routine::MainReturned)?;
    registers.r[15] = main;
    os.leave_with(registers);
    Ok(())
}

/// `words` laid in the heap as `argv`: a pointer to each, a null pointer,
/// then the words; none where the heap has no room for them.
fn arguments(os: &mut Os<'_>, statics: Statics, words: &[Vec<u8>]) -> Result<Option<u32>, Fail> {
    let pointers = words.len().saturating_add(1).saturating_mul(4);
    let bytes = words.iter().fold(pointers, |total, word| {
        total.saturating_add(word.len()).saturating_add(1)
    });
    let Ok(length) = u32::try_from(bytes) else {
        return Ok(None);
    };
    let block = alloc::allocated(os, statics, length)?;
    if block == 0 {
        return Ok(None);
    }
    let mut image = Vec::new();
    image.try_reserve_exact(bytes).map_err(|_| os.no_memory())?;
    let mut offset = pointers;
    for word in words {
        #[expect(
            clippy::cast_possible_truncation,
            reason = "an offset within the block, whose length fits a word"
        )]
        let text = block.wrapping_add(offset as u32);
        image.extend_from_slice(&text.to_le_bytes());
        offset += word.len() + 1;
    }
    image.extend_from_slice(&[0; 4]);
    for word in words {
        image.extend_from_slice(word);
        image.push(0);
    }
    os.as_caller().write_bytes(block, &image)?;
    Ok(Some(block))
}

/// The library's message `token` with `arguments` reported on `stderr` on a
/// line, and the program ended failing.
fn refused(
    os: &mut Os<'_>,
    statics: Statics,
    token: &str,
    arguments: &[&[u8]],
) -> Result<(), Fail> {
    let text = message_with(os, token, arguments)?;
    report::to_stderr(os, statics, &[&text[..], b"\n"].concat())?;
    report::end_failing(os, statics)
}

/// `exit(status)` and `_exit(status)`, and where `main` returns: the
/// program finalised — the functions `atexit` registered called and the
/// streams closed by the C language's finalisation — and ended with
/// `status` as its return code.
pub(crate) fn exit(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let statics = context::statics_at(os, regs.r[10])?;
    statics.write(os, kernel_at::RETURN_CODE, regs.r[0])?;
    exit::finalise(os, statics, Then::Exit, 0)
}

/// A list of functions an exit calls, the last registered first: how many,
/// where the first of them lie in the statics, and the statics' word
/// holding the heap block the rest lie in, which is replaced by one twice
/// its size as it fills. Only the heap's room bounds it, as RISC OS 5.30's
/// lists. The count and the block are the client's to overwrite, so the
/// list holds no more than the block the heap says it is.
#[derive(Clone, Copy)]
struct ExitList {
    count: u32,
    here: u32,
    more: u32,
}

const AT_EXIT: ExitList = ExitList {
    count: at::ATEXIT_COUNT,
    here: at::ATEXIT,
    more: at::ATEXIT_MORE,
};
const AT_QUICK_EXIT: ExitList = ExitList {
    count: at::QUICK_EXIT_COUNT,
    here: at::QUICK_EXIT,
    more: at::QUICK_EXIT_MORE,
};

impl ExitList {
    /// The heap block holding the functions past the statics' own and how
    /// many it holds: none where the statics' word names no block in use.
    fn more(self, os: &mut Os<'_>, statics: Statics) -> Result<(u32, u32), Fail> {
        let block = statics.read(os, self.more)?;
        if block == 0 {
            return Ok((0, 0));
        }
        let size = alloc::size_of(os, statics, block)?;
        Ok(size.map_or((0, 0), |size| (block, size / 4)))
    }

    /// The most functions the list holds now.
    fn holds(self, os: &mut Os<'_>, statics: Statics) -> Result<u32, Fail> {
        let (_, capacity) = self.more(os, statics)?;
        Ok(EXIT_LIST_HERE + capacity)
    }

    /// Where function `n` lies, below [`Self::holds`]; or the next past
    /// them, the heap block grown to hold it, where `grow` asks.
    fn place(
        self,
        os: &mut Os<'_>,
        statics: Statics,
        n: u32,
        grow: bool,
    ) -> Result<Option<u32>, Fail> {
        if n < EXIT_LIST_HERE {
            return Ok(Some(statics.field(self.here + 4 * n)));
        }
        let beyond = n - EXIT_LIST_HERE;
        let (block, capacity) = self.more(os, statics)?;
        if beyond < capacity {
            return Ok(Some(block + 4 * beyond));
        }
        if !grow || beyond != capacity {
            return Ok(None);
        }
        let wider = capacity.saturating_mul(2).max(EXIT_LIST_HERE);
        let Some(bytes) = wider.checked_mul(4) else {
            return Ok(None);
        };
        let grown = alloc::allocated(os, statics, bytes)?;
        if grown == 0 {
            return Ok(None);
        }
        if block != 0 {
            os.as_caller().copy_bytes(block, grown, 4 * capacity)?;
            alloc::freed(os, statics, block)?;
        }
        statics.write(os, self.more, grown)?;
        Ok(Some(grown + 4 * beyond))
    }

    /// `function` registered: whether there was room.
    fn register(self, os: &mut Os<'_>, statics: Statics, function: u32) -> Result<bool, Fail> {
        let count = statics.read(os, self.count)?;
        let Some(at) = self.place(os, statics, count, true)? else {
            return Ok(false);
        };
        os.as_caller().write_word(at, function)?;
        statics.write(os, self.count, count + 1)?;
        Ok(true)
    }

    /// Every function called, the last first, each taken off before it is
    /// called so an exit inside one does not call it again; each entered
    /// with its own address in R0 and R2, as RISC OS 5.30 enters them. A
    /// count past what the list holds counts only those it holds.
    fn call_all(self, os: &mut Os<'_>, statics: Statics, sl: u32) -> Result<(), Fail> {
        let caller = os.registers();
        loop {
            let count = statics.read(os, self.count)?;
            let Some(last) = count.min(self.holds(os, statics)?).checked_sub(1) else {
                return Ok(());
            };
            statics.write(os, self.count, last)?;
            let Some(at) = self.place(os, statics, last, false)? else {
                return Ok(());
            };
            let function = os.as_caller().read_word(at)?;
            let mut call = Regs::default();
            call.r[0] = function;
            call.r[2] = function;
            call.r[10] = sl;
            call.r[11] = caller.r[11];
            client::call(os, function, &mut call, caller.r[13])?;
        }
    }
}

/// `atexit(func)`, and chunk 9's entry that registers as it does: nought
/// once registered; non-nought where the heap has no room for it.
pub(crate) fn atexit(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let statics = context::statics_at(os, regs.r[10])?;
    regs.r[0] = u32::from(!AT_EXIT.register(os, statics, regs.r[0])?);
    Ok(())
}

/// `at_quick_exit(func)`: as `atexit`, for `quick_exit`.
pub(crate) fn at_quick_exit(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let statics = context::statics_at(os, regs.r[10])?;
    regs.r[0] = u32::from(!AT_QUICK_EXIT.register(os, statics, regs.r[0])?);
    Ok(())
}

/// `quick_exit(status)`: the functions `at_quick_exit` registered called,
/// then `_Exit(status)`.
pub(crate) fn quick_exit(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let statics = context::statics_at(os, regs.r[10])?;
    AT_QUICK_EXIT.call_all(os, statics, regs.r[10])?;
    exit_immediately(os, regs)
}

/// `_Exit(status)`: the program ended with `status` as its return code, no
/// function `atexit` registered called and no stream closed, only the
/// handlers it took put back.
pub(crate) fn exit_immediately(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let statics = context::statics_at(os, regs.r[10])?;
    statics.write(os, kernel_at::RETURN_CODE, regs.r[0])?;
    exit::end_unfinalised(os, statics)
}

/// The functions `atexit` registered called.
fn call_atexit(os: &mut Os<'_>, statics: Statics, regs: &Regs) -> Result<(), Fail> {
    AT_EXIT.call_all(os, statics, regs.r[10])
}

/// `Finalise`, the C language's finalisation procedure: the functions
/// `atexit` registered called, then every stream closed.
pub(super) fn finalise(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let statics = context::statics_at(os, regs.r[10])?;
    call_atexit(os, statics, regs)?;
    stream::close_all(os, statics)
}

/// `_clib_finalisemodule(pw)`: a module's library finalised — the functions
/// `atexit` registered called, its streams closed, its heap given back to
/// the RMA — and the workspace its private word points to freed, the word
/// cleared so the kernel, which frees what a dead module's private word
/// points to, frees nothing twice.
pub(super) fn clib_finalisemodule(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let statics = context::statics_at(os, regs.r[10])?;
    call_atexit(os, statics, regs)?;
    stream::close_all(os, statics)?;
    alloc::release(os, statics)?;
    let private = regs.r[0];
    let workspace = os.as_caller().read_word(private)?;
    if workspace != 0 {
        os.rma_free(workspace)?;
        os.as_caller().write_word(private, 0)?;
    }
    Ok(())
}

/// `_clib_version()`: the library's version string.
pub(super) fn clib_version(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    regs.r[0] = template(os)?.wrapping_add(chunk::VERSION_AT);
    Ok(())
}

#[cfg(test)]
mod tests;
