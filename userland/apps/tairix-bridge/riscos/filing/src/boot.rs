//! The boot sequence (`plans/RISCOS-EMULATOR.md` §6, decision 16): the
//! session's home made ready as RISC OS's boot application leaves a machine,
//! and the command-line half of the boot run on the user's own disc.
//!
//! On the user's own disc, `!Boot` is laid out as RISC OS 5 lays it out,
//! made where the disc has none and never written over where it has one.
//! The boot and choices variables are set as the PRM's boot application sets
//! them, then `PreDesktop` is obeyed and each object in `PreDesk` run as its
//! kind asks, the directories last, as a script that keeps
//! each failure and goes on; a `!Boot` that is a file is a boot file, run as
//! RISC OS runs one. A session with no disc of its own keeps `Choices:` on
//! its memory disc and runs no boot sequence. The desktop half — `Resources`
//! booted, `Tasks` started — is the Wimp's.

use alloc::collections::VecDeque;
use alloc::vec::Vec;

use tairix_riscos_kernel::os::{Fail, Os};
use tairix_riscos_kernel::release::RELEASE;

use crate::bootcommands::objects::{object_kind, objects, Object, DIRECTORY, FILE, IMAGE};
use crate::hostfs;
use crate::script::{self, OnError, Source};

pub use crate::script::{Failure, Report};

/// `!Boot`'s skeleton, below the disc's root, each made in turn where the
/// disc has no `!Boot`.
const SKELETON: [&[u8]; 7] = [
    b"!Boot",
    b"!Boot.Choices",
    b"!Boot.Choices.Boot",
    b"!Boot.Choices.Boot.PreDesk",
    b"!Boot.Choices.Boot.Tasks",
    b"!Boot.Resources",
    b"!Boot.Library",
];

/// The boot application's directory, below the disc's root.
const BOOT: &[u8] = b"!Boot";
/// The directories the boot runs before and with the desktop, as RISC OS 5's
/// boot application names them.
const TO_BE_LOADED: &[u8] = b"Boot:Choices.Boot.PreDesk";
const TO_BE_TASKS: &[u8] = b"Boot:Choices.Boot.Tasks";
/// The library the boot puts on the run path.
const LIBRARY: &[u8] = b",Boot:Library.";
/// `Boot$State` while the command-line half runs.
const COMMANDS: &[u8] = b"commands";
/// `Boot$Unique` for a boot application on the machine's own disc.
const LOCAL: &[u8] = b"Local";

/// The file types `PreDesk` runs as the PRM's boot application runs them.
const OBEY: u32 = 0xFEB;
const ABSOLUTE: u32 = 0xFF8;
const UTILITY: u32 = 0xFFC;
const SPRITE: u32 = 0xFF9;
const MODULE: u32 = 0xFFA;
const BASIC: u32 = 0xFFB;

/// The session's home made ready and, on the user's own disc, the
/// command-line half of the boot run: what the session's start calls once
/// its modules are up.
///
/// # Errors
///
/// An unwind that leaves the boot, as a program run from `PreDesk` that
/// becomes the application leaves it — the boot goes on once it exits, its
/// failures then for [`reported`] — or HostFS not being active.
pub fn start(os: &mut Os<'_>) -> Result<Report, Fail> {
    let (root, own) = hostfs::home(os)?;
    if !own {
        set_choices(os, &root)?;
        return Ok(Report::default());
    }
    let boot = joined(os, &[&root, b".", BOOT])?;
    let kind = object_kind(os, &boot)?;
    if kind == FILE {
        let line = joined(os, &[b"Run ", &boot])?;
        let mut lines = VecDeque::new();
        if lines.try_reserve(1).is_err() {
            return Err(os.no_memory());
        }
        lines.push_back(line);
        return run(os, Source::Made(lines));
    }
    let mut skeleton = VecDeque::new();
    if kind != DIRECTORY && kind != IMAGE {
        for part in SKELETON {
            let line = joined(os, &[b"CDir ", &root, b".", part])?;
            if skeleton.try_reserve(1).is_err() {
                return Err(os.no_memory());
            }
            skeleton.push_back(line);
        }
    }
    set_boot(os, &boot)?;
    let choices = joined(os, &[&boot, b".Choices"])?;
    set_choices(os, &choices)?;
    let pre_desktop = joined(os, &[&choices, b".Boot.PreDesktop"])?;
    run(
        os,
        Source::Boot(Sequence {
            skeleton,
            pre_desktop: Some(pre_desktop),
            pre_desk: None,
        }),
    )
}

/// What a boot that went on once a program it started exited met.
pub fn reported(os: &mut Os<'_>) -> Report {
    script::take_reported(os)
}

/// `source` run as the boot runs its lines: each failure kept.
fn run(os: &mut Os<'_>, source: Source) -> Result<Report, Fail> {
    script::run(os, source, OnError::Report)
}

/// The boot's lines: the skeleton's, `PreDesktop` obeyed where there is
/// one, then `PreDesk`'s, listed once it is reached so what came before can
/// add to it.
pub(crate) struct Sequence {
    skeleton: VecDeque<Vec<u8>>,
    pre_desktop: Option<Vec<u8>>,
    pre_desk: Option<VecDeque<Vec<u8>>>,
}

impl Sequence {
    /// The next line, none after the last.
    pub(crate) fn next(&mut self, os: &mut Os<'_>) -> Result<Option<Vec<u8>>, Fail> {
        if let Some(line) = self.skeleton.pop_front() {
            return Ok(Some(line));
        }
        if let Some(pre_desktop) = self.pre_desktop.take() {
            if object_kind(os, &pre_desktop)? == FILE {
                return joined(os, &[b"Obey -c ", &pre_desktop]).map(Some);
            }
        }
        if self.pre_desk.is_none() {
            self.pre_desk = Some(pre_desk(os)?);
        }
        Ok(self.pre_desk.as_mut().and_then(VecDeque::pop_front))
    }
}

/// `parts` end to end.
fn joined(os: &mut Os<'_>, parts: &[&[u8]]) -> Result<Vec<u8>, Fail> {
    tairix_util::fallible::joined(parts).ok_or_else(|| os.no_memory())
}

/// The boot variables for the boot application `boot`, as the PRM's boot
/// application sets them.
fn set_boot(os: &mut Os<'_>, boot: &[u8]) -> Result<(), Fail> {
    os.set_string_variable(b"Boot$Dir", boot)?;
    let path = joined(os, &[boot, b"."])?;
    os.set_string_variable(b"Boot$Path", &path)?;
    let mut version = [0; 8];
    let digits = RELEASE.iter().filter(|b| b.is_ascii_digit());
    let length = version
        .iter_mut()
        .zip(digits)
        .map(|(slot, &digit)| *slot = digit)
        .count();
    os.set_string_variable(b"Boot$OSVersion", &version[..length])?;
    os.set_string_variable(b"Boot$State", COMMANDS)?;
    os.set_string_variable(b"Boot$Unique", LOCAL)?;
    os.set_string_variable(b"Boot$ToBeLoaded", TO_BE_LOADED)?;
    os.set_string_variable(b"Boot$ToBeTasks", TO_BE_TASKS)?;
    let run = os.variable_text(b"Run$Path")?.unwrap_or_default();
    let run = joined(os, &[&run, LIBRARY])?;
    os.set_string_variable(b"Run$Path", &run)
}

/// The choices variables for the choices directory `choices`: written there,
/// and read from there first.
fn set_choices(os: &mut Os<'_>, choices: &[u8]) -> Result<(), Fail> {
    os.set_string_variable(b"Choices$Dir", choices)?;
    os.set_string_variable(b"Choices$Write", choices)?;
    let path = joined(os, &[choices, b"."])?;
    os.set_string_variable(b"Choices$Path", &path)
}

/// The line for each object in `PreDesk`, as the PRM's boot application
/// runs them, the directories after the files; none where it cannot be
/// listed.
fn pre_desk(os: &mut Os<'_>) -> Result<VecDeque<Vec<u8>>, Fail> {
    let found = match objects(os, TO_BE_LOADED) {
        Ok(found) => found,
        Err(Fail::Error(_)) => return Ok(VecDeque::new()),
        Err(unwind) => return Err(unwind),
    };
    let (directories, files): (Vec<&Object>, Vec<&Object>) =
        found.iter().partition(|object| object.kind == DIRECTORY);
    let mut lines = VecDeque::new();
    for object in files.into_iter().chain(directories) {
        let name = joined(os, &[TO_BE_LOADED, b".", &object.leaf])?;
        let Some(line) = pre_desk_line(os, object, &name)? else {
            continue;
        };
        if lines.try_reserve(1).is_err() {
            return Err(os.no_memory());
        }
        lines.push_back(line);
    }
    Ok(lines)
}

/// The command that runs `object`, named `name`, from `PreDesk`; none for a
/// directory with no `!Run`.
fn pre_desk_line(os: &mut Os<'_>, object: &Object, name: &[u8]) -> Result<Option<Vec<u8>>, Fail> {
    if object.kind == DIRECTORY {
        let run = joined(os, &[name, b".!Run"])?;
        if object_kind(os, &run)? != FILE {
            return Ok(None);
        }
        return joined(os, &[b"Run ", &run]).map(Some);
    }
    let command: &[u8] = match object.file_type() {
        Some(OBEY) => b"Obey -c ",
        Some(ABSOLUTE | UTILITY) => b"Run ",
        Some(SPRITE) => b"IconSprites ",
        Some(MODULE) => b"RMLoad ",
        Some(BASIC) => b"BASIC -quit ",
        _ => b"Load ",
    };
    joined(os, &[command, name]).map(Some)
}
