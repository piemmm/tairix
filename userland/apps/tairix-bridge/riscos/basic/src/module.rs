//! The BASIC and BASIC64 modules: `*BASIC` and `*BASIC64`, each starting
//! its dialect as the application; the routines the machine is entered
//! through; the guest code they share — the routine table `CALL` hands code
//! in R14, the stub a SWI is performed from — and the environment handlers
//! BASIC installs while it runs.

use alloc::vec::Vec;

use tairix_riscos_arm::a32;
use tairix_riscos_arm::cond::Condition;
use tairix_riscos_arm::instruction::{BranchOp, CallOp, Instruction, Op};
use tairix_riscos_kernel::memory::MemoryError;
use tairix_riscos_kernel::module::{Builtin, Code, Command, CommandKind, Help};
use tairix_riscos_kernel::os::{len32, Fail, Os, Regs};
use tairix_riscos_kernel::swi::{os as swi, X};

use crate::machine::{self, flag, Leave, Machine, Mode};
use crate::value::Dialect;
use crate::workspace::{self as ws, reg};

/// BASIC V.
pub static BASIC: Builtin = Builtin {
    title: "BASIC",
    help_name: None,
    bridge: false,
    swis: None,
    services: None,
    commands: &[command("BASIC", basic_command)],
    init: None,
    finalise: None,
    start: Some(start_v),
    routines: &ROUTINES,
    vectors: &[],
    resources: &[],
    code: Some(emit_v),
};

/// BASIC VI.
pub static BASIC64: Builtin = Builtin {
    title: "BASIC64",
    help_name: None,
    bridge: false,
    swis: None,
    services: None,
    commands: &[command("BASIC64", basic64_command)],
    init: None,
    finalise: None,
    start: Some(start_vi),
    routines: &ROUTINES,
    vectors: &[],
    resources: &[],
    code: Some(emit_vi),
};

/// What `*Help` says of `*BASIC` and `*BASIC64`.
const HELP: &str = "Starts BBC BASIC, ready for commands, a program loaded, or a program run.";
/// Their syntax.
const SYNTAX: &str =
    "Syntax: *BASIC [-help] | [-chain | -load | -quit] <file> | [-chain | -quit] @<start>,<end>";

const fn command(name: &'static str, run: Code) -> Command {
    Command {
        name,
        kind: CommandKind::Command,
        min: 0,
        max: 255,
        translate: 0,
        help: Help::Text(HELP),
        syntax: Some(SYNTAX),
        run: Some(run),
    }
}

/// The routines, by their place in [`ROUTINES`].
pub(crate) mod routine {
    /// The machine's next slice.
    pub(crate) const RUN: usize = 0;
    /// A SWI the stub performed, come back.
    pub(crate) const SWI_DONE: usize = 1;
    /// Code `CALL` or `USR` entered, come back through the table's first
    /// word.
    pub(crate) const CALL_DONE: usize = 2;
    /// The error handler.
    pub(crate) const ERROR: usize = 3;
    /// The escape handler.
    pub(crate) const ESCAPE: usize = 4;
    /// The exit handler.
    pub(crate) const EXIT: usize = 5;
    /// The `UpCall` handler.
    pub(crate) const UPCALL: usize = 6;
    /// The routine table's first branch: `VARIND` and the rest in order.
    pub(crate) const TABLE: usize = 7;
}

/// The routines both modules are entered through: each reads the machine's
/// dialect from the workspace.
static ROUTINES: [Code; 24] = [
    run,
    swi_done,
    call_done,
    error_handler,
    escape_handler,
    exit_handler,
    upcall_handler,
    crate::table::varind,
    crate::table::storea,
    crate::table::ststore,
    crate::table::lvblnk,
    crate::table::create,
    crate::table::expr,
    crate::table::matcher,
    crate::table::tokenaddr,
    crate::table::fsta,
    crate::table::flda,
    crate::table::fadd,
    crate::table::fsub,
    crate::table::fmul,
    crate::table::fdiv,
    crate::table::float,
    crate::table::fix,
    crate::table::fsqrt,
];

/// The routine table's branches after its offsets: `VARIND` to `TOKENADDR`.
const BRANCHES: usize = 8;
/// Its floating-point routines after the count, BASIC V's alone.
const FLOATING: usize = 9;
/// Where the SWI stub lies after the table: its first branch, its offsets,
/// its branches, the word after them and the floating-point count, then its
/// floating-point routines.
pub(crate) const SWI_STUB: u32 =
    4 * (1 + len32(OFFSETS.len()) + len32(BRANCHES) + 2 + len32(FLOATING));

/// The offsets from ARGP the table's second to twelfth words give.
const OFFSETS: [ws::Slot; 11] = [
    ws::STRACC,
    ws::PAGE,
    ws::TOP,
    ws::LOMEM,
    ws::HIMEM,
    ws::MEMLIMIT,
    ws::FSA,
    ws::TALLY,
    ws::TRACEF,
    ws::ESCWORD,
    ws::WIDTHLOC,
];

fn encode(op: Op) -> Result<u32, MemoryError> {
    a32::encode(&Instruction {
        cond: Condition::Al,
        op,
    })
    .map_err(|_| MemoryError::HostFull)
}

/// A branch from the word at `from` to the entry of routine `index`, the
/// first entry at `first`, all relative to the code's start.
fn branch(from: usize, first: i32, index: usize) -> Result<u32, MemoryError> {
    let target = index
        .checked_mul(8)
        .and_then(|offset| i32::try_from(offset).ok())
        .and_then(|offset| first.checked_add(offset))
        .ok_or(MemoryError::HostFull)?;
    let from = 4 * i32::try_from(from).map_err(|_| MemoryError::HostFull)?;
    encode(Op::Branch {
        op: BranchOp::B,
        offset: target - (from + 8),
    })
}

/// The guest code: the routine table, its floating-point routines where
/// `floating`, then the SWI stub.
fn emit(first: i32, floating: bool) -> Result<Vec<u32>, MemoryError> {
    let mut words = Vec::new();
    words.try_reserve(SWI_STUB as usize / 4 + 2)?;
    words.push(branch(0, first, routine::CALL_DONE)?);
    for slot in OFFSETS {
        words.push(slot.0.cast_unsigned());
    }
    for i in 0..BRANCHES {
        words.push(branch(words.len(), first, routine::TABLE + i)?);
    }
    words.push(0);
    if floating {
        words.push(len32(FLOATING));
        for i in 0..FLOATING {
            words.push(branch(words.len(), first, routine::TABLE + BRANCHES + i)?);
        }
    } else {
        words.extend_from_slice(&[0; 1 + FLOATING]);
    }
    words.push(encode(Op::Call {
        op: CallOp::Swi,
        value: swi::CALL_A_SWI_R12,
    })?);
    words.push(branch(words.len(), first, routine::SWI_DONE)?);
    let table = crate::token::table::bytes()?;
    words.try_reserve(table.len().div_ceil(4))?;
    let (whole, rest) = table.as_chunks::<4>();
    words.extend(whole.iter().map(|chunk| u32::from_le_bytes(*chunk)));
    if !rest.is_empty() {
        let mut last = [0; 4];
        last[..rest.len()].copy_from_slice(rest);
        words.push(u32::from_le_bytes(last));
    }
    Ok(words)
}

/// Where the token table lies after the SWI stub.
const TOKEN_TABLE: u32 = SWI_STUB + 8;

/// The token table's address in the module of `dialect`, while it is active.
pub(crate) fn token_table_address(os: &Os<'_>, dialect: Dialect) -> Option<u32> {
    Some(os.guest_code_of(module_of(dialect))? + TOKEN_TABLE)
}

fn emit_v(first: i32) -> Result<Vec<u32>, MemoryError> {
    emit(first, true)
}

fn emit_vi(first: i32) -> Result<Vec<u32>, MemoryError> {
    emit(first, false)
}

/// The module whose machine the workspace holds.
pub(crate) fn module_of(dialect: Dialect) -> &'static Builtin {
    match dialect {
        Dialect::V => &BASIC,
        Dialect::VI => &BASIC64,
    }
}

// The commands and the start.

fn basic_command(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    enter(os, regs.r[0], &BASIC)
}

fn basic64_command(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    enter(os, regs.r[0], &BASIC64)
}

/// `OS_Module` 2: `builtin` entered as the application, `tail` its
/// parameters.
fn enter(os: &mut Os<'_>, tail: u32, builtin: &'static Builtin) -> Result<(), Fail> {
    os.with_text(builtin.title.as_bytes(), 0, |os, title| {
        let mut regs = Regs::default();
        regs.r[..3].copy_from_slice(&[MODULE_ENTER, title, tail]);
        os.perform(X | swi::MODULE, &mut regs)
    })
}

/// `OS_Module`'s reason that enters a module as the application.
const MODULE_ENTER: u32 = 2;

fn start_v(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    crate::start::start(os, regs.r[0], Dialect::V)
}

fn start_vi(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    crate::start::start(os, regs.r[0], Dialect::VI)
}

// The routines.

/// The machine's next slice, from User mode.
fn run(os: &mut Os<'_>, _regs: &mut Regs) -> Result<(), Fail> {
    slice(os, |_| Ok(machine::next::Next::Moved))
}

/// A SWI the stub performed come back: what it was for resumed.
fn swi_done(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let regs = *regs;
    slice(os, |m| m.swi_returned(&regs))
}

/// Code `CALL` or `USR` entered come back.
fn call_done(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let regs = *regs;
    slice(os, |m| m.call_returned(&regs))
}

/// The error handler: the error the kernel copied to the string
/// accumulator met as BASIC meets one.
fn error_handler(os: &mut Os<'_>, _regs: &mut Regs) -> Result<(), Fail> {
    let block = ws::STRACC.at() + 4;
    slice(os, |_| Err(machine::Trap::Os(block)))
}

/// The escape handler: the escape flag set or cleared as R11 says.
fn escape_handler(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let word = os.read_word(ws::ESCWORD.at())?;
    let word = if regs.r[11] & ESCAPE_SET == 0 {
        word & !ws::ESCAPE
    } else {
        word | ws::ESCAPE
    };
    os.write_word(ws::ESCWORD.at(), word)
}

/// R11's bit the escape handler is told an escape is set by.
const ESCAPE_SET: u32 = 1 << 6;

/// The exit handler: a program a command started has exited, back to the
/// machine.
fn exit_handler(os: &mut Os<'_>, _regs: &mut Regs) -> Result<(), Fail> {
    let regs = Regs::default();
    slice(os, |m| m.swi_returned(&regs))
}

/// The `UpCall` handler: a new application about to start in BASIC's place
/// is let start, BASIC's handlers given back first.
fn upcall_handler(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    if regs.r[0] == UPCALL_NEW_APPLICATION {
        crate::start::give_back_handlers(os)?;
    }
    Ok(())
}

/// `UpCall_NewApplication`.
const UPCALL_NEW_APPLICATION: u32 = 256;

/// A slice of the machine run after `first`, then wherever it leaves for.
pub(crate) fn slice(
    os: &mut Os<'_>,
    first: impl FnOnce(&mut Machine<'_, '_>) -> machine::Step<machine::next::Next>,
) -> Result<(), Fail> {
    let leave = {
        let mut m = match Machine::load(os) {
            Ok(m) => m,
            Err(trap) => return Err(crate::start::fatal(os, trap)),
        };
        let leave = m.resume_with(first);
        if let Err(trap) = m.save() {
            return Err(crate::start::fatal(m.os, trap));
        }
        leave
    };
    let leave = leave.map_err(Fail::Unwind)?;
    depart(os, leave)
}

/// Control on to where the machine leaves for.
pub(crate) fn depart(os: &mut Os<'_>, leave: Leave) -> Result<(), Fail> {
    let flags = os.read_word(reg::FLAGS.at())?;
    let dialect = if flags & flag::BASIC64 == 0 {
        Dialect::V
    } else {
        Dialect::VI
    };
    let builtin = module_of(dialect);
    let bits = os.caller_bits();
    let sp = os.read_word(reg::SP.at())?;
    match leave {
        Leave::Continue => {
            let run = os
                .routine_of(builtin, routine::RUN)
                .ok_or_else(|| os.no_memory())?;
            let mut registers = [0; 15];
            registers[8] = ws::ARGP;
            registers[13] = sp;
            Err(os.continue_in_user(bits, run, registers, 0))
        }
        Leave::Swi {
            number,
            regs,
            flags,
        } => {
            let code = os.guest_code_of(builtin).ok_or_else(|| os.no_memory())?;
            let mut registers = [0; 15];
            registers[..10].copy_from_slice(&regs);
            registers[12] = number;
            registers[13] = sp;
            Err(os.continue_in_user(bits, code + SWI_STUB, registers, flags))
        }
        Leave::Call { address, regs } => {
            let table = os.guest_code_of(builtin).ok_or_else(|| os.no_memory())?;
            let mut registers = [0; 15];
            registers[..14].copy_from_slice(&regs);
            registers[14] = table;
            Err(os.continue_in_user(bits, address, registers, 0))
        }
        Leave::Return { pc, regs, flags } => Err(os.continue_in_user(bits, pc, regs, flags)),
        Leave::Quit => crate::start::quit(os),
    }
}

impl Machine<'_, '_> {
    /// A slice begun with `first` followed, then run on.
    pub(crate) fn resume_with(
        &mut self,
        first: impl FnOnce(&mut Self) -> machine::Step<machine::next::Next>,
    ) -> Result<Leave, tairix_riscos_kernel::os::Unwind> {
        if self.mode == Mode::Away {
            self.mode = Mode::Statement;
        }
        let begun = first(self).and_then(|next| self.follow(next));
        match begun {
            Ok(Some(leave)) => return Ok(leave),
            Ok(None) => {}
            Err(machine::Trap::Unwind(unwind)) => return Err(unwind),
            Err(trap) => {
                if let Some(leave) = self.error(trap)? {
                    return Ok(leave);
                }
            }
        }
        self.slice()
    }
}
