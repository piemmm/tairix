//! `BootCommands` (`plans/RISCOS-EMULATOR.md` §5): the commands boot
//! sequences and applications' `!Boot` and `!Run` files rely on, which RISC
//! OS 5 keeps in a ROM module of that name. Each answers as RISC OS 5 was
//! recorded answering; the parameters the kernel passes through `GSTrans`
//! for each are the ones RISC OS 5's command table names.
//!
//! - `addapp`: `*AddApp`, applications added to `Resources:$.Apps`.
//! - `repeat`: `*Repeat`, a command run on each object of a directory.
//! - `memory`: the RMA, application space and the free pool.
//! - `cmos`: the CMOS RAM saved and loaded.
//! - `objects`: objects found as these commands and the boot find them.

mod addapp;
mod cmos;
mod memory;
pub(crate) mod objects;
mod repeat;

use alloc::collections::{TryReserveError, VecDeque};
use alloc::vec::Vec;

use tairix_obey::command::MOST_LINE;
use tairix_riscos_kernel::module::{Builtin, Command};
use tairix_riscos_kernel::os::{Fail, Os, Regs};
use tairix_riscos_kernel::swi::os::{self as swi};
use tairix_riscos_kernel::swi::X;

use objects::object_kind;

/// The module's title, by which `*Repeat` enters it.
const TITLE: &str = "BootCommands";

/// The variable `*X` leaves the error it swallowed in.
const X_ERROR: &[u8] = b"X$Error";

const IF_THERE_SYNTAX: &str = "Syntax: *IfThere <object> Then <command> [Else <command>]";

/// `BootCommands`.
pub static BOOT_COMMANDS: Builtin = Builtin {
    title: TITLE,
    help_name: Some("Boot Commands"),
    help_suffix: None,
    bridge: false,
    swis: None,
    services: None,
    commands: &COMMANDS,
    init: None,
    finalise: None,
    start: Some(repeat::start),
    routines: &[],
    vectors: &[],
    resources: &[],
    code: None,
};

/// A command of `BootCommands`: `translate` the parameters the kernel
/// passes through `GSTrans` for it.
const fn command(
    name: &'static str,
    (min, max): (u8, u8),
    translate: u8,
    help: &'static str,
    syntax: &'static str,
    run: tairix_riscos_kernel::module::Code,
) -> Command {
    Command {
        translate,
        ..Command::new(name, (min, max), help, Some(syntax), run)
    }
}

/// Its commands, in RISC OS 5's table's order.
static COMMANDS: [Command; 13] = [
    command("AddApp", (1, 1), 1, "*AddApp adds the applications that match to Resources:$.Apps, where they can be found and run.", "Syntax: *AddApp [<directory>.]<pattern>", addapp::add_app),
    command("AppSize", (1, 1), 0, "*AppSize gives back the RMA's free pages at its top.", "Syntax: *AppSize <size>[K]", memory::app_size),
    command("Do", (0, 255), 0, "*Do runs a command after GSTransing it.", "Syntax: *Do <command>", run_translated),
    command("IfThere", (1, 255), 1, "*IfThere runs the command after Then if the object exists, else the one after Else.", IF_THERE_SYNTAX, if_there_command),
    command("LoadCMOS", (1, 1), 1, "*LoadCMOS reads a file of CMOS RAM SaveCMOS made.", "Syntax: *LoadCMOS <filename>", cmos::load),
    command("SaveCMOS", (1, 1), 1, "*SaveCMOS saves the CMOS RAM to a file.", "Syntax: *SaveCMOS <filename>", cmos::save),
    command("Repeat", (0, 255), 2, "*Repeat runs a command on each object in a directory the switches choose.", repeat::SYNTAX, repeat::repeat),
    command("SafeLogon", (1, 255), 0, "*SafeLogon logs on to a file server unless already logged on as that user.", "Syntax: *SafeLogon [[:]<file server number>|:<file server name>] <user name> [[:<CR>]<password>]", safe_logon),
    command("FreePool", (0, 0), 0, "*FreePool gives application space to the free pool.", "Syntax: *FreePool", memory::free_pool),
    command("ShrinkRMA", (0, 0), 0, "*ShrinkRMA gives back the RMA's free pages at its top.", "Syntax: *ShrinkRMA", memory::shrink),
    command("AddToRMA", (1, 1), 0, "*AddToRMA grows the RMA by the size given.", "Syntax: *AddToRMA <size>[K|M]", memory::add_to_rma),
    command("AppSlot", (1, 1), 0, "*AppSlot makes application space the size given.", "Syntax: *AppSlot <size>[K|M]", memory::app_slot),
    command("X", (0, 255), 0, "*X runs a command and ignores its error, keeping it in X$Error.", "Syntax: *X <command>", x),
];

/// What the module holds for the session: each object added, which stays in
/// ResourceFS through the module's restart as on RISC OS 5, and the walk
/// `*Repeat` hands the application it becomes.
#[derive(Default)]
struct State {
    /// Each added leaf and the block of the RMA its files are in.
    added: Vec<(Vec<u8>, u32)>,
    walk: Option<VecDeque<Vec<u8>>>,
}

impl State {
    /// The leaf `leaf` held in `block`: the block it was held in before, if
    /// it was; an error where there is no room to keep it.
    fn added(&mut self, leaf: &[u8], block: u32) -> Result<Option<u32>, TryReserveError> {
        if let Some(held) = self
            .added
            .iter_mut()
            .find(|(held, _)| held.eq_ignore_ascii_case(leaf))
        {
            return Ok(Some(core::mem::replace(&mut held.1, block)));
        }
        let mut kept = Vec::new();
        kept.try_reserve_exact(leaf.len())?;
        kept.extend_from_slice(leaf);
        self.added.try_reserve(1)?;
        self.added.push((kept, block));
        Ok(None)
    }
}

/// `work` on the module's state, held from the first use on.
fn with<T>(os: &mut Os<'_>, work: impl FnOnce(&mut State) -> T) -> Result<T, Fail> {
    if os.extension::<State>().is_none() {
        os.hold_extension(State::default())?;
    }
    match os.extension::<State>() {
        Some(state) => Ok(work(state)),
        None => Err(os.no_memory()),
    }
}

/// The words of `tail`, each its start and end; a word that begins with a
/// quotation mark runs to the next.
fn words(tail: &[u8]) -> impl Iterator<Item = (usize, usize)> + '_ {
    let mut at = 0;
    core::iter::from_fn(move || {
        let start = at + tail.get(at..)?.iter().position(|&b| b != b' ')?;
        let rest = &tail[start..];
        let length = match rest.split_first() {
            Some((b'"', after)) => after
                .iter()
                .position(|&b| b == b'"')
                .map_or(rest.len(), |end| end + 2),
            _ => rest.iter().position(|&b| b == b' ').unwrap_or(rest.len()),
        };
        at = start + length;
        Some((start, start + length))
    })
}

/// The parameters of `tail`, a quoted one without its quotation marks.
fn parameters(tail: &[u8]) -> impl Iterator<Item = &[u8]> + '_ {
    words(tail).map(|(start, end)| {
        let word = &tail[start..end];
        word.strip_prefix(b"\"")
            .map_or(word, |inner| inner.strip_suffix(b"\"").unwrap_or(inner))
    })
}

/// `*X`: the command run, an error it gives swallowed and kept in `X$Error`
/// unless that is already set.
fn x(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (line, _) = os.read_until(regs.r[0], MOST_LINE, |b| b < b' ')?;
    if line.is_empty() {
        return Ok(());
    }
    match os.perform_line(&line) {
        Err(Fail::Error(block)) => {
            if os.variable_text(X_ERROR)?.is_none() {
                let (_, text) = os.read_error(block)?;
                os.set_string_variable(X_ERROR, &text)?;
            }
            Ok(())
        }
        done => done,
    }
}

/// `*Do`: the command GSTrans'd once, then run.
fn run_translated(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let tail = regs.r[0];
    let room = tairix_riscos_kernel::os::len32(MOST_LINE);
    os.below_supervisor_stack(room, |os, at| {
        let mut translate = Regs::default();
        translate.r[..3].copy_from_slice(&[tail, at, room - 1]);
        os.perform(X | swi::GS_TRANS, &mut translate)?;
        if translate.flag(tairix_riscos_arm::psr::C) {
            return Err(os.error(tairix_riscos_kernel::error::LINE_TOO_LONG));
        }
        os.write_byte(at.wrapping_add(translate.r[2]), b'\r')?;
        let mut cli = Regs::default();
        cli.r[0] = at;
        os.perform(X | swi::CLI, &mut cli)
    })
}

/// What `*IfThere` was given: the object, the command for when it is there,
/// and the one for when it is not.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct IfThere<'a> {
    object: &'a [u8],
    then: &'a [u8],
    otherwise: &'a [u8],
}

/// `tail` read as `*IfThere` reads it: the object, then `Then` and the rest,
/// split at the first word that is `Else`, each keyword in either case; none
/// where `Then` does not follow the object.
fn if_there(tail: &[u8]) -> Option<IfThere<'_>> {
    let mut words = words(tail);
    let (start, end) = words.next()?;
    let (keyword, then_end) = words.next()?;
    if !tail[keyword..then_end].eq_ignore_ascii_case(b"Then") {
        return None;
    }
    let mut then = (then_end, tail.len());
    let mut otherwise = (tail.len(), tail.len());
    for (word, word_end) in words {
        if tail[word..word_end].eq_ignore_ascii_case(b"Else") {
            then.1 = word;
            otherwise = (word_end, tail.len());
            break;
        }
    }
    let trimmed = |(from, to): (usize, usize)| tail[from..to].trim_ascii();
    Some(IfThere {
        object: &tail[start..end],
        then: trimmed(then),
        otherwise: trimmed(otherwise),
    })
}

/// `*IfThere`: the command for whether the object exists, as `OS_File` 17
/// finds it; a name it refuses is not there.
fn if_there_command(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (tail, _) = os.read_until(regs.r[0], MOST_LINE, |b| b < b' ')?;
    let Some(given) = if_there(&tail) else {
        return Err(os.syntax_failure(IF_THERE_SYNTAX));
    };
    let chosen = if object_kind(os, given.object)? != 0 {
        given.then
    } else {
        given.otherwise
    };
    if chosen.is_empty() {
        return Ok(());
    }
    os.perform_line(chosen)
}

/// `*SafeLogon`: the logon handed to the filing system's own `*Logon`,
/// never its alias, `NetFS` — which alone could already have the user logged
/// on — not being one the emulator has.
fn safe_logon(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (tail, _) = os.read_until(regs.r[0], MOST_LINE, |b| b < b' ')?;
    let line = tairix_util::fallible::joined(&[b"%Logon ", &tail]).ok_or_else(|| os.no_memory())?;
    os.perform_line(&line)
}

#[cfg(test)]
mod tests;
