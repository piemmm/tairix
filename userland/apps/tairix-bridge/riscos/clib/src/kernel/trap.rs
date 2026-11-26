//! Traps, and the escapes and events searched for as traps are, taken as
//! the PRM's library kernel takes them: the registers dumped to the
//! client's stack, a language whose code holds the PC, R14 or a frame's
//! link found, its procedure entered, and its answer taken — the dumped
//! registers resumed if it handled the cause, the search gone on if not,
//! the second pass's procedures after the first's. Past them all a trap is
//! an error for the program's parent, and an escape or event is done with.
//! A real event's search ends at the first procedure found, willing or not.
//!
//! A procedure is entered by leaving with its registers and the library's
//! continuation as its return, never from a nested call, so one that does
//! not return — a signal handler's longjmp — leaves nothing of the library
//! behind.

use tairix_riscos_kernel::os::{Fail, Os, Regs};
use tairix_riscos_kernel::{hex8, Registers};

use super::environment;
use super::exit;
use super::frame::{code_address, readable, SAVED_FP, SAVED_LR};
use super::language::{self, word, Language};
use super::os::{last_error_text, note_error};
use super::statics::{at, Statics};
use super::{context, Routine};
use crate::module::{error, message, raise as raise_error};

/// The traps the library raises itself: a divisor of nought, a stack that
/// cannot be extended, and a checked read or write outside the application
/// space.
pub(crate) const DIVIDE_BY_ZERO: u32 = 0x8000_0020;
pub(crate) const STACK_OVERFLOW: u32 = 0x8000_0021;
pub(crate) const READ_FAIL: u32 = 0x8080_0EA0;
pub(crate) const WRITE_FAIL: u32 = 0x8080_0EA1;
/// The code an escape is searched for by.
pub(crate) const ESCAPE: u32 = u32::MAX;

/// The text each trap the library raises is shown by, which it notes as
/// the last error.
const TEXTS: [(u32, &str); 4] = [
    (DIVIDE_BY_ZERO, "DivideByZero"),
    (STACK_OVERFLOW, "StackOverflow"),
    (READ_FAIL, "ReadFail"),
    (WRITE_FAIL, "WriteFail"),
];

/// What a procedure answers: it did not handle the cause, or did.
pub(crate) const DECLINED: u32 = 0;
pub(crate) const HANDLED: u32 = 1;
/// The register set lies this far below the interrupted SP, as RISC OS
/// 5.30 puts it.
const SET_BELOW: u32 = 68;
/// An escape's or event's procedure runs this far below the set.
const EVENT_STACK_BELOW: u32 = 100;
/// A register set's words.
const SET_WORDS: u32 = 16;
/// The most frames a search follows.
const FRAMES_MOST: u32 = 1024;

/// A search's words, from its block in the statics: the cause's code, the
/// set and the flags with it, where the search stands — the pass, the
/// candidate PC within it and the frame the next one's link lies in — the
/// languages marked failed, the one whose procedure was last entered,
/// whether one is under way, and whether the next procedure is entered
/// from a privileged handler.
pub(crate) mod state {
    pub(crate) const CODE: u32 = 0;
    pub(crate) const SET: u32 = 4;
    pub(crate) const FLAGS: u32 = 8;
    pub(crate) const PASS: u32 = 12;
    pub(crate) const CANDIDATE: u32 = 16;
    pub(crate) const FRAME: u32 = 20;
    pub(crate) const FAILED: u32 = 24;
    pub(crate) const LANGUAGE: u32 = 28;
    pub(crate) const ACTIVE: u32 = 32;
    pub(crate) const ENTERING: u32 = 36;
    /// The block's size.
    pub(crate) const BYTES: u32 = 40;
}

/// What a search is for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Cause {
    Trap,
    Escape,
    Event,
}

impl Cause {
    /// The procedures asked, pass by pass.
    const fn passes(self) -> [u32; 2] {
        match self {
            Self::Trap => [word::TRAP, word::UNCAUGHT_TRAP],
            Self::Escape | Self::Event => [word::EVENT, word::UNHANDLED_EVENT],
        }
    }

    /// Its search's block in the statics.
    const fn block(self) -> u32 {
        match self {
            Self::Trap => at::TRAP_SEARCH,
            Self::Escape | Self::Event => at::EVENT_SEARCH,
        }
    }

    /// Where its procedures return.
    const fn continuation(self) -> Routine {
        match self {
            Self::Trap => Routine::TrapReturned,
            Self::Escape | Self::Event => Routine::EventReturned,
        }
    }

    /// The escape or event an event search's `code` says it is for.
    const fn of_event_code(code: u32) -> Self {
        if code == ESCAPE {
            Self::Escape
        } else {
            Self::Event
        }
    }
}

/// How a search's first procedure is entered: from User mode by leaving
/// with its registers, or from a privileged handler in User mode, the
/// privileged stacks emptied.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Entering {
    User,
    Privileged,
}

/// A cause's search in a client's statics.
#[derive(Clone, Copy)]
struct Search {
    statics: Statics,
    cause: Cause,
}

impl Search {
    fn read(self, os: &mut Os<'_>, word: u32) -> Result<u32, Fail> {
        self.statics.read(os, self.cause.block() + word)
    }

    fn write(self, os: &mut Os<'_>, word: u32, value: u32) -> Result<(), Fail> {
        self.statics.write(os, self.cause.block() + word, value)
    }
}

/// Trap `code` at the call that asked for what failed: the registers as the
/// caller had them, the PC wound back to its call, so resuming retries it.
pub(crate) fn at_call(os: &mut Os<'_>, code: u32) -> Result<(), Fail> {
    let mut dumped = os.registers();
    dumped.r[15] = dumped.r[15].wrapping_sub(4);
    let statics = context::statics_at(os, dumped.r[10])?;
    note_own(os, statics, code)?;
    raise(os, Cause::Trap, code, dumped, Entering::User)
}

/// The library's own trap `code` noted as the last error, as an exception's
/// or an OS error's is, with the text it is shown by.
fn note_own(os: &mut Os<'_>, statics: Statics, code: u32) -> Result<(), Fail> {
    let text = match TEXTS.iter().find(|(known, _)| *known == code) {
        Some((_, token)) => message(os, token)?,
        None => hex8(code).to_vec(),
    };
    note_error(os, statics, code, &text)
}

/// Trap `code` at the call, uncaught at once: for a privileged caller,
/// whose mode no language's procedure runs in.
pub(crate) fn fault(os: &mut Os<'_>, code: u32) -> Result<(), Fail> {
    let mut dumped = os.registers();
    dumped.r[15] = dumped.r[15].wrapping_sub(4);
    let statics = context::statics_at(os, dumped.r[10])?;
    note_own(os, statics, code)?;
    let search = Search {
        statics,
        cause: Cause::Trap,
    };
    let set = dumped.r[13].wrapping_sub(SET_BELOW);
    for (n, &value) in (0..).zip(&dumped.r) {
        os.as_caller().write_word(set.wrapping_add(4 * n), value)?;
    }
    search.write(os, state::CODE, code)?;
    search.write(os, state::SET, set)?;
    uncaught(os, search)
}

/// Whether `statics`' search for `cause` is under way.
pub(crate) fn searching(os: &mut Os<'_>, statics: Statics, cause: Cause) -> Result<bool, Fail> {
    Ok(Search { statics, cause }.read(os, state::ACTIVE)? != 0)
}

/// `statics`' search for `cause` taken as over, its procedure having gone
/// on by itself.
pub(crate) fn abandon(os: &mut Os<'_>, statics: Statics, cause: Cause) -> Result<(), Fail> {
    Search { statics, cause }.write(os, state::ACTIVE, 0)
}

/// A search for `cause`, coded `code`, from the registers `dumped`: the set
/// written below their SP and the first willing procedure entered. A trap
/// while one is being handled is uncaught at once.
pub(crate) fn raise(
    os: &mut Os<'_>,
    cause: Cause,
    code: u32,
    dumped: Registers,
    entering: Entering,
) -> Result<(), Fail> {
    let statics = context::statics_at(os, dumped.r[10])?;
    let search = Search { statics, cause };
    let set = dumped.r[13].wrapping_sub(SET_BELOW);
    for (n, &value) in (0..).zip(&dumped.r) {
        os.as_caller().write_word(set.wrapping_add(4 * n), value)?;
    }
    let again = search.read(os, state::ACTIVE)? != 0;
    for (word, value) in [
        (state::CODE, code),
        (state::SET, set),
        (state::FLAGS, dumped.flags),
        (state::PASS, 0),
        (state::CANDIDATE, 0),
        (state::FRAME, 0),
        (state::FAILED, 0),
        (state::ACTIVE, 1),
        (state::ENTERING, u32::from(entering == Entering::Privileged)),
    ] {
        search.write(os, word, value)?;
    }
    if again {
        return uncaught(os, search);
    }
    search_on(os, search)
}

/// A trap procedure has returned: R0 says whether it handled the trap.
pub(crate) fn trap_returned(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let statics = context::statics_at(os, regs.r[10])?;
    returned(
        os,
        Search {
            statics,
            cause: Cause::Trap,
        },
        regs.r[0],
    )
}

/// An escape's or event's procedure has returned: R0 says whether it
/// handled it.
pub(crate) fn event_returned(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let statics = context::statics_at(os, regs.r[10])?;
    let code = statics.read(os, Cause::Event.block() + state::CODE)?;
    let search = Search {
        statics,
        cause: Cause::of_event_code(code),
    };
    returned(os, search, regs.r[0])
}

/// Its procedure's answer `answer` taken: the set resumed where it handled
/// the cause or the search ends at the first found, the search gone on
/// past the procedure's language otherwise.
fn returned(os: &mut Os<'_>, search: Search, answer: u32) -> Result<(), Fail> {
    if answer == HANDLED || search.cause == Cause::Event {
        return resume(os, search);
    }
    let failed = search.read(os, state::FAILED)?;
    let language = search.read(os, state::LANGUAGE)?;
    search.write(os, state::FAILED, failed | 1 << (language % 32))?;
    search_on(os, search)
}

/// The search gone on from where its state says it stands: the next
/// willing language's procedure entered, or the cause past every language.
fn search_on(os: &mut Os<'_>, search: Search) -> Result<(), Fail> {
    let set = search.read(os, state::SET)?;
    let mut pass = search.read(os, state::PASS)?;
    let mut candidate = search.read(os, state::CANDIDATE)?;
    let mut frame = search.read(os, state::FRAME)?;
    let mut failed = search.read(os, state::FAILED)?;
    let passes = search.cause.passes();
    while let Some(&word) = passes.get(pass as usize) {
        while let Some((pc, next)) = candidate_pc(os, set, candidate, frame)? {
            if let Some((language, procedure)) = handler(os, search.statics, pc, word, failed)? {
                for (field, value) in [
                    (state::PASS, pass),
                    (state::CANDIDATE, candidate),
                    (state::FRAME, frame),
                    (state::FAILED, failed),
                    (state::LANGUAGE, language.index),
                ] {
                    search.write(os, field, value)?;
                }
                return enter(os, search, set, procedure);
            }
            candidate += 1;
            frame = next;
            os.yield_point()?;
        }
        pass += 1;
        candidate = 0;
        frame = 0;
        failed = 0;
    }
    match search.cause {
        Cause::Trap => uncaught(os, search),
        Cause::Escape | Cause::Event => resume(os, search),
    }
}

/// Candidate `candidate`'s PC — the dumped PC, R14, then each frame's link
/// — and the frame the next candidate's link lies in; none past the last
/// frame, or where the frames are found corrupt.
fn candidate_pc(
    os: &mut Os<'_>,
    set: u32,
    candidate: u32,
    frame: u32,
) -> Result<Option<(u32, u32)>, Fail> {
    let dumped = |os: &mut Os<'_>, n: u32| os.as_caller().read_word(set.wrapping_add(4 * n));
    let found = match candidate {
        0 => Some((dumped(os, 15)?, 0)),
        1 => Some((dumped(os, 14)?, dumped(os, 11)?)),
        n if n < FRAMES_MOST + 2 && frame != 0 && frame.is_multiple_of(4) => {
            let link = readable(os, frame.wrapping_sub(SAVED_LR));
            let caller = readable(os, frame.wrapping_sub(SAVED_FP));
            link.zip(caller)
        }
        _ => None,
    };
    let bits = os.caller_bits();
    Ok(found.map(|(pc, next)| (code_address(bits, pc), next)))
}

/// The first language not marked failed whose code holds `pc` and whose
/// procedure at `word` is its own.
fn handler(
    os: &mut Os<'_>,
    statics: Statics,
    pc: u32,
    word: u32,
    failed: u32,
) -> Result<Option<(Language, u32)>, Fail> {
    let mut walk = language::languages(os, statics)?;
    while let Some(language) = walk.next(os)? {
        if failed & 1 << language.index == 0 && language.holds(pc) {
            if let Some(procedure) = language.procedure(os, word)? {
                return Ok(Some((language, procedure)));
            }
        }
    }
    Ok(None)
}

/// The set's registers, its flags with them.
fn dumped(os: &mut Os<'_>, search: Search, set: u32) -> Result<Registers, Fail> {
    let mut registers = Registers {
        flags: search.read(os, state::FLAGS)?,
        ..Registers::default()
    };
    for (n, slot) in (0..SET_WORDS).zip(registers.r.iter_mut()) {
        *slot = os.as_caller().read_word(set.wrapping_add(4 * n))?;
    }
    Ok(registers)
}

/// `procedure` entered with the cause's code and set: a trap's below the
/// set without a frame, an escape's or event's further below with the
/// interrupted frame; returning to the library's continuation.
fn enter(os: &mut Os<'_>, search: Search, set: u32, procedure: u32) -> Result<(), Fail> {
    let mut registers = dumped(os, search, set)?;
    registers.r[0] = search.read(os, state::CODE)?;
    registers.r[1] = set;
    match search.cause {
        Cause::Trap => {
            registers.r[11] = 0;
            registers.r[13] = set;
        }
        Cause::Escape | Cause::Event => registers.r[13] = set.wrapping_sub(EVENT_STACK_BELOW),
    }
    registers.r[14] = context::routine(os, search.cause.continuation())?;
    registers.r[15] = procedure;
    leave(os, search, registers)
}

/// The set resumed, as its procedure left it, the search over.
fn resume(os: &mut Os<'_>, search: Search) -> Result<(), Fail> {
    let set = search.read(os, state::SET)?;
    let registers = dumped(os, search, set)?;
    search.write(os, state::ACTIVE, 0)?;
    leave(os, search, registers)
}

/// Control carried on with `registers`: by leaving with them from User
/// mode, or from the privileged handler that started the search.
fn leave(os: &mut Os<'_>, search: Search, registers: Registers) -> Result<(), Fail> {
    if search.read(os, state::ENTERING)? == 0 {
        os.leave_with(registers);
        return Ok(());
    }
    search.write(os, state::ENTERING, 0)?;
    let mut user = [0; 15];
    user.copy_from_slice(&registers.r[..15]);
    let bits = os.caller_bits();
    Err(os.resume_in_user(bits, registers.r[15], user, registers.flags))
}

/// A trap no language took: the handlers in force before `_kernel_init`
/// back, and an error naming it for the program's parent.
fn uncaught(os: &mut Os<'_>, search: Search) -> Result<(), Fail> {
    search.write(os, state::ACTIVE, 0)?;
    let set = search.read(os, state::SET)?;
    let pc = os.as_caller().read_word(set.wrapping_add(4 * 15))?;
    let text = last_error_text(os, search.statics)?;
    environment::restore(os, search.statics)?;
    let failed = raise_error(os, error::UNCAUGHT_TRAP, &[&text, &hex8(pc), &hex8(set)]);
    exit::generate(os, failed)
}
