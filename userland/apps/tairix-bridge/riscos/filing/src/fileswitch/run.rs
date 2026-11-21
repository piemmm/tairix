//! `OS_FSControl` 4, `*Run`'s call: the object a name finds through
//! `Run$Path` run as RISC OS 5 runs it — an application directory by its
//! `!Run`, an Absolute or untyped file as the application, a Utility from the
//! RMA, and any other through its type's `@RunType` alias, handed the file's
//! canonical name and the parameters after it.

use alloc::vec::Vec;

use tairix_obey::command::MOST_LINE;
use tairix_riscos_kernel::error::Error;
use tairix_riscos_kernel::filing::filetype::{ABSOLUTE, UTILITY};
use tairix_riscos_kernel::filing::fs::stamp;
use tairix_riscos_kernel::filing::NOT_FOUND;
use tairix_riscos_kernel::memory::layout::APPLICATION;
use tairix_riscos_kernel::os::{len32, Entry, Fail, Os, Regs};
use tairix_riscos_kernel::swi::os::{self as swi, fs_control as reason};
use tairix_riscos_kernel::swi::X;

use super::call::{Info, Need, DIRECTORY, FILE, IMAGE};
use super::name::{self, Search};
use super::{args, error};

/// The path a name to run is looked for along.
const RUN_PATH: &[u8] = b"Run$Path";
/// The workspace a Utility is given after its code, its stack's.
const UTILITY_WORKSPACE: u32 = 1024;

/// "No run action is set for files of type &%0".
const NO_RUN_TYPE: Error = Error {
    number: 0x409,
    token: "NoRunnerForType",
};
/// A file to run as the application that does not fit where it must be
/// loaded, a condition RISC OS 5 was never seen to report.
const NO_ROOM: Error = Error {
    number: 0x400,
    token: "NoRoomToRun",
};

/// `OS_FSControl` 4: the name at R1 run, the parameters after it its own.
pub(crate) fn run(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (given, start) = name::read_from(os, regs.r[1])?;
    let (_, tail) = os.skip_spaces(start.wrapping_add(len32(given.len())))?;
    let (resolved, found) = name::find_named(os, &given, Search::Variable(RUN_PATH), Need::Length)?;
    let canonical = args::canonical(os, resolved.calls.number, resolved.whole())?;
    match found.kind {
        FILE | IMAGE => run_file(os, &found, &canonical, start, tail),
        DIRECTORY if name::leaf(&resolved.path).starts_with(b"!") => {
            let mut line = canonical;
            if line.try_reserve(6).is_err() {
                return Err(os.no_memory());
            }
            line.extend_from_slice(b".!Run");
            run_line(os, Through::Run, &line, tail)
        }
        _ => Err(name::named_error(os, NOT_FOUND, &given)),
    }
}

/// The file `found`, named `canonical`, run by its type: the name as given at
/// `start`, its parameters at `tail`.
fn run_file(
    os: &mut Os<'_>,
    found: &Info,
    canonical: &[u8],
    start: u32,
    tail: u32,
) -> Result<(), Fail> {
    let Some(kind) = stamp::type_of(found.load) else {
        return application(
            os,
            canonical,
            start,
            tail,
            (found.load, found.exec, found.length),
        );
    };
    match kind {
        ABSOLUTE => application(
            os,
            canonical,
            start,
            tail,
            (APPLICATION, APPLICATION, found.length),
        ),
        UTILITY => utility(os, canonical, start, tail, found.length),
        kind => {
            let alias = alloc::format!("@RunType_{kind:03X}").into_bytes();
            let mut variable = b"Alias$".to_vec();
            variable.extend_from_slice(&alias);
            if os.variable_text(&variable)?.is_none() {
                return Err(error::fail(os, NO_RUN_TYPE, &[&alias[9..]]));
            }
            let mut line = alias;
            if line.try_reserve(1 + canonical.len()).is_err() {
                return Err(os.no_memory());
            }
            line.push(b' ');
            line.extend_from_slice(canonical);
            run_line(os, Through::Command, &line, tail)
        }
    }
}

/// What a line [`run_line`] builds is handed to.
#[derive(Clone, Copy)]
enum Through {
    /// `OS_FSControl` 4, as a name to run.
    Run,
    /// `OS_CLI`, as a command.
    Command,
}

/// `text`, the parameters at `tail` after it, handed on `through`.
fn run_line(os: &mut Os<'_>, through: Through, text: &[u8], tail: u32) -> Result<(), Fail> {
    let (parameters, _) = os.read_until(tail, MOST_LINE, |b| b < b' ')?;
    let mut line = Vec::new();
    if line.try_reserve(text.len() + 1 + parameters.len()).is_err() {
        return Err(os.no_memory());
    }
    line.extend_from_slice(text);
    if !parameters.is_empty() {
        line.push(b' ');
        line.extend_from_slice(&parameters);
    }
    match through {
        Through::Command => os.perform_line(&line),
        Through::Run => os.with_text(&line, b'\r', |os, at| {
            let mut call = Regs::default();
            call.r[..2].copy_from_slice(&[reason::RUN, at]);
            os.perform(X | swi::FS_CONTROL, &mut call)
        }),
    }
}

/// The file `canonical` loaded at `load` for `length` bytes as the
/// application, entered at `exec`: the application space must hold it.
fn application(
    os: &mut Os<'_>,
    canonical: &[u8],
    start: u32,
    tail: u32,
    (load, exec, length): (u32, u32, u32),
) -> Result<(), Fail> {
    let mut env = Regs::default();
    os.perform(X | swi::GET_ENV, &mut env)?;
    let limit = u64::from(env.r[1]);
    if load < APPLICATION || u64::from(load) + u64::from(length) > limit {
        return Err(error::fail(os, NO_ROOM, &[canonical]));
    }
    os.start_application(tail, load, start)?;
    load_at(os, canonical, load)?;
    let bits = os.caller_bits();
    Err(os.enter_application(bits, exec, |_| {}))
}

/// The Utility `canonical` loaded into the RMA and called in User mode, as
/// RISC OS 5 calls one: R0 the name as given at `start`, R1 the parameters at
/// `tail`, R3 its code, R12 the workspace after it and R13 that workspace's
/// end; its block let go once it returns, whatever it returns.
fn utility(
    os: &mut Os<'_>,
    canonical: &[u8],
    start: u32,
    tail: u32,
    length: u32,
) -> Result<(), Fail> {
    let Some(size) = length.checked_add(UTILITY_WORKSPACE) else {
        return Err(error::fail(os, NO_ROOM, &[canonical]));
    };
    let code = os.rma_claim(size)?;
    let called = load_at(os, canonical, code).and_then(|()| {
        let workspace = code.wrapping_add(length);
        let mut call = Regs::default();
        call.r[0] = start;
        call.r[1] = tail;
        call.r[3] = code;
        call.r[12] = workspace;
        let bits = os.caller_bits();
        os.call_routine(
            code,
            bits,
            Entry::User(workspace.wrapping_add(UTILITY_WORKSPACE)),
            &mut call,
        )
    });
    os.rma_free(code)?;
    called
}

/// The file `canonical` loaded at `address` by `OS_File` 255.
fn load_at(os: &mut Os<'_>, canonical: &[u8], address: u32) -> Result<(), Fail> {
    os.with_text(canonical, 0, |os, at| {
        let mut load = Regs::default();
        load.r[..4].copy_from_slice(&[swi::file::LOAD, at, address, 0]);
        os.perform(X | swi::FILE, &mut load)
    })
}
