//! FPEmulator 4.39's interface, as RISC OS 5.30's multiple-context FPEmulator
//! answers: its version, the floating point contexts it switches between, and
//! the exception dump an enabled trap leaves the FPA's registers in. The FPA
//! is the processor's own (`tairix_riscos_arm::fpa`).
//!
//! A context is an opaque block laid out as FPEmulator lays it: each
//! register's three words in FPEmulator's internal format and a nought, then
//! the FPSR and a nought. While a context is current its state is the
//! processor's FPA; with none current, the FPA's instructions are undefined.
//! Each of the session's contexts has its own current context and its own
//! default one, FPEmulator's.

use tairix_riscos_arm::fpa::{fpsr, Exception, Format, Fpa};

use crate::module::{Builtin, Swis, SWI_OUT_OF_RANGE};
use crate::os::{Fail, Os, Regs};
use crate::swi::chunk::FP_EMULATOR as CHUNK;

/// FPEmulator's version, in hundredths, as `FPEmulator_Version` answers.
const VERSION: u32 = crate::module::inventory::FP_EMULATOR_ROM.version as u32;

/// The context pointer naming none.
const NO_CONTEXT: u32 = 0;
/// The default context, FPEmulator's own.
const DEFAULT: u32 = u32::MAX;
/// The current context, which the calls that save and load one accept.
const CURRENT: u32 = u32::MAX - 1;

/// A context's bytes, as `FPEmulator_ContextLength` answers.
const CONTEXT_BYTES: u32 = 136;
const CONTEXT_WORDS: usize = CONTEXT_BYTES as usize / 4;
const REGISTERS: usize = 8;
/// A register's words in a context, and the FPSR's place.
const REGISTER_WORDS: usize = 4;
const FPSR_WORD: usize = REGISTERS * REGISTER_WORDS;

/// The words a register set is saved and loaded in: the FPSR, then each
/// register as `STFE` stores it.
const SAVED_WORDS: u32 = 25;
const SAVED: usize = SAVED_WORDS as usize;

/// A fresh context's FPSR, which a program starts with: FPEmulator's system
/// ID, its invalid operation, division by zero and overflow traps enabled.
pub const FRESH_FPSR: u32 = fpsr::SYSTEM_ID
    | Exception::InvalidOperation.enable()
    | Exception::DivisionByZero.enable()
    | Exception::Overflow.enable();

/// The workspace: the exception dump.
const DUMP_AT: u32 = 0;
const WORKSPACE: u32 = DUMP_AT + 4 * SAVED_WORDS;

/// FPEmulator's state: its workspace, and the current context's own.
pub(crate) struct State {
    workspace: Option<u32>,
    pub(crate) own: Own,
}

impl State {
    pub(crate) const fn new() -> Self {
        Self {
            workspace: None,
            own: Own::NONE,
        }
    }
}

/// What each context has of FPEmulator's: the floating point context
/// current, and a default context of its own, which no other context's
/// work can reach.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Own {
    current: u32,
    default: Context,
}

impl Own {
    /// No context current: the FPA's instructions are undefined.
    pub(crate) const NONE: Self = Self {
        current: NO_CONTEXT,
        default: Context::FRESH,
    };

    /// Whether a context is current, so the FPA has one to work in.
    pub(crate) const fn active(&self) -> bool {
        self.current != NO_CONTEXT
    }
}

/// FPEmulator.
pub(crate) static FP_EMULATOR: Builtin = Builtin {
    title: "FPEmulator",
    help_name: None,
    help_suffix: Some(" (1.13M)"),
    bridge: false,
    swis: Some(Swis {
        chunk: CHUNK,
        prefix: "FPEmulator",
        names: &[
            "Version",
            "DeactivateContext",
            "ActivateContext",
            "ChangeContext",
            "ContextLength",
            "InitContext",
            "ExceptionDump",
            "Abort",
            "LoadContext",
            "SaveContext",
        ],
        handler: swi,
    }),
    services: None,
    commands: &[],
    init: Some(init),
    finalise: Some(finalise),
    start: None,
    routines: &[],
    vectors: &[],
    resources: &[],
    code: None,
};

/// A context's state: each register in FPEmulator's internal format, and
/// the FPSR.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Context {
    registers: [[u32; 3]; REGISTERS],
    fpsr: u32,
}

impl Context {
    /// As `FPEmulator_InitContext` leaves one: every register nought.
    const FRESH: Self = Self {
        registers: [[0; 3]; REGISTERS],
        fpsr: FRESH_FPSR,
    };

    fn of(fpa: &Fpa) -> Self {
        let mut registers = [[0; 3]; REGISTERS];
        for (n, register) in (0..).zip(&mut registers) {
            *register = fpa.store_multiple(n);
        }
        Self {
            registers,
            fpsr: fpa.fpsr(),
        }
    }

    fn put_into(&self, fpa: &mut Fpa) {
        for (n, &register) in (0..).zip(&self.registers) {
            fpa.load_multiple(n, register);
        }
        fpa.write_fpsr(self.fpsr);
    }

    fn read(os: &mut Os<'_>, at: u32) -> Result<Self, Fail> {
        let mut words = [0; CONTEXT_WORDS];
        os.read_words(at, &mut words)?;
        let mut registers = [[0; 3]; REGISTERS];
        for (register, kept) in registers
            .iter_mut()
            .zip(words.as_chunks::<REGISTER_WORDS>().0)
        {
            *register = [kept[0], kept[1], kept[2]];
        }
        Ok(Self {
            registers,
            fpsr: words[FPSR_WORD],
        })
    }

    fn write(&self, os: &mut Os<'_>, at: u32) -> Result<(), Fail> {
        let mut words = [0; CONTEXT_WORDS];
        for (kept, register) in words
            .as_chunks_mut::<REGISTER_WORDS>()
            .0
            .iter_mut()
            .zip(&self.registers)
        {
            kept[..3].copy_from_slice(register);
        }
        words[FPSR_WORD] = self.fpsr;
        os.write_words(at, &words)
    }

    /// As the calls that save a register set write it.
    fn saved(&self) -> [u32; SAVED] {
        let mut fpa = Fpa::new();
        self.put_into(&mut fpa);
        let mut words = [0; SAVED];
        words[0] = self.fpsr;
        for (n, register) in (0..).zip(words[1..].as_chunks_mut::<3>().0) {
            *register = fpa.extended(n);
        }
        words
    }

    /// From a register set as the save calls write one, the FPSR's system
    /// ID kept as `WFS` keeps it.
    fn from_saved(words: &[u32; SAVED]) -> Self {
        let mut fpa = Fpa::new();
        fpa.write_fpsr(words[0]);
        for (n, register) in (0..).zip(words[1..].as_chunks::<3>().0) {
            // Loading an extended value raises nothing.
            let _ = fpa.load(
                Format::Extended,
                n,
                [register[0], register[1], register[2], 0],
            );
        }
        Self::of(&fpa)
    }
}

fn workspace(os: &mut Os<'_>) -> Result<u32, Fail> {
    os.world
        .fp_emulator
        .workspace
        .ok_or_else(|| os.error_with(SWI_OUT_OF_RANGE, &[FP_EMULATOR.title.as_bytes()]))
}

/// The context `pointer` names as its block keeps it, the default context
/// the current kernel context's own.
fn kept(os: &mut Os<'_>, pointer: u32) -> Result<Context, Fail> {
    if pointer == DEFAULT {
        workspace(os)?;
        return Ok(os.world.fp_emulator.own.default);
    }
    Context::read(os, pointer)
}

/// `context` kept in the block `pointer` names.
fn keep(os: &mut Os<'_>, pointer: u32, context: Context) -> Result<(), Fail> {
    if pointer == DEFAULT {
        workspace(os)?;
        os.world.fp_emulator.own.default = context;
        return Ok(());
    }
    context.write(os, pointer)
}

/// The current context's state kept in its block: the context, nought for
/// none.
fn deactivate(os: &mut Os<'_>) -> Result<u32, Fail> {
    let current = os.world.fp_emulator.own.current;
    if current != NO_CONTEXT {
        let state = Context::of(os.world.processor.cpu().fpa());
        keep(os, current, state)?;
    }
    Ok(current)
}

/// The context `pointer` made current, its state the FPA's; with none, the
/// FPA's instructions undefined.
fn activate(os: &mut Os<'_>, pointer: u32) -> Result<(), Fail> {
    if pointer != NO_CONTEXT {
        let context = kept(os, pointer)?;
        context.put_into(os.world.processor.cpu().fpa_mut());
    }
    os.world
        .processor
        .cpu()
        .set_fpa_active(pointer != NO_CONTEXT);
    os.world.fp_emulator.own.current = pointer;
    Ok(())
}

/// A new context's floating point, as FPEmulator's start leaves it: its own
/// default context fresh and current, or none while FPEmulator is not
/// running.
pub(crate) fn start_context(os: &mut Os<'_>) -> Result<(), Fail> {
    os.world.fp_emulator.own = Own::NONE;
    if os.world.fp_emulator.workspace.is_none() {
        os.world.processor.cpu().set_fpa_active(false);
        return Ok(());
    }
    activate(os, DEFAULT)
}

/// The context `pointer` names, the current one for [`CURRENT`]: whether it
/// is current.
fn resolved(os: &Os<'_>, pointer: u32) -> (u32, bool) {
    let current = os.world.fp_emulator.own.current;
    let pointer = if pointer == CURRENT { current } else { pointer };
    (pointer, pointer == current)
}

/// `FPEmulator_SaveContext`'s work: the context's register set at `into`.
/// Nothing is ever pending, so the answer is nought.
fn save(os: &mut Os<'_>, pointer: u32, into: u32) -> Result<u32, Fail> {
    let (pointer, current) = resolved(os, pointer);
    let context = match (pointer, current) {
        (NO_CONTEXT, _) => return Ok(0),
        (_, true) => Context::of(os.world.processor.cpu().fpa()),
        (_, false) => kept(os, pointer)?,
    };
    os.write_words(into, &context.saved())?;
    Ok(0)
}

/// `FPEmulator_LoadContext`'s work: the context given the register set at
/// `from`.
fn load(os: &mut Os<'_>, pointer: u32, from: u32) -> Result<(), Fail> {
    let (pointer, current) = resolved(os, pointer);
    if pointer == NO_CONTEXT {
        return Ok(());
    }
    let mut words = [0; SAVED];
    os.read_words(from, &mut words)?;
    let context = Context::from_saved(&words);
    if current {
        context.put_into(os.world.processor.cpu().fpa_mut());
        return Ok(());
    }
    keep(os, pointer, context)
}

fn swi(os: &mut Os<'_>, offset: u32, regs: &mut Regs) -> Result<(), Fail> {
    match offset {
        0 => regs.r[0] = VERSION,
        1 => regs.r[0] = deactivate(os)?,
        2 => activate(os, regs.r[0])?,
        3 => {
            let old = deactivate(os)?;
            activate(os, regs.r[0])?;
            regs.r[0] = old;
        }
        4 => regs.r[0] = CONTEXT_BYTES,
        5 => keep(os, regs.r[0], Context::FRESH)?,
        6 => regs.r[0] = workspace(os)? + DUMP_AT,
        // An FPA instruction is never left partly done, so there is nothing
        // to abandon and no frame to answer.
        7 => regs.r[0] = 0,
        8 => load(os, regs.r[0], regs.r[1])?,
        9 => regs.r[0] = save(os, regs.r[0], regs.r[1])?,
        _ => return Err(os.error_with(SWI_OUT_OF_RANGE, &[FP_EMULATOR.title.as_bytes()])),
    }
    Ok(())
}

/// The FPA's register set kept in the exception dump, as a trap keeps it.
pub(crate) fn dump(os: &mut Os<'_>) -> Result<(), Fail> {
    let Some(workspace) = os.world.fp_emulator.workspace else {
        return Ok(());
    };
    let saved = Context::of(os.world.processor.cpu().fpa()).saved();
    os.write_words(workspace + DUMP_AT, &saved)
}

fn init(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let workspace = os.rma_claim(WORKSPACE)?;
    os.write_word(regs.r[12], workspace)?;
    os.write_words(workspace + DUMP_AT, &Context::FRESH.saved())?;
    os.world.fp_emulator = State {
        workspace: Some(workspace),
        own: Own::NONE,
    };
    activate(os, DEFAULT)
}

/// Its finalisation: every context's floating point context let go of, a
/// context set aside meanwhile included, as their state went with the
/// FPEmulator that held it.
#[expect(
    clippy::unnecessary_wraps,
    reason = "a module's finalisation's signature"
)]
fn finalise(os: &mut Os<'_>, _regs: &mut Regs) -> Result<(), Fail> {
    os.world.processor.cpu().set_fpa_active(false);
    os.world.fp_emulator = State::new();
    for kept in os.world.contexts.parked() {
        kept.fp = Own::NONE;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
