//! The Obey module (`plans/RISCOS-EMULATOR.md` §5): `*Obey`, a file of
//! commands each run as `OS_CLI` runs a line, as RISC OS 5's Obey runs them.
//!
//! The file is read through FileSwitch, a line at a time from the open file,
//! or whole before its first line runs where `-c` asks. Each line has the
//! parameters put in as `lib/obey` puts them, `%0` the first after the file's
//! name; `Obey$Dir` is the name as given up to its last full stop. The file
//! runs as a script: the first line that fails ends it with
//! its error, and a program a line starts hands it back its turn when it
//! exits.

use alloc::vec::Vec;

use tairix_obey::args::substitute;
use tairix_obey::command::MOST_LINE;
use tairix_obey::is_terminator;
use tairix_riscos_kernel::error::obey_error;
use tairix_riscos_kernel::module::{Builtin, Command, CommandKind, Help};
use tairix_riscos_kernel::os::{len32, Fail, Os, Regs};
use tairix_riscos_kernel::swi::os::{self as swi, find};
use tairix_riscos_kernel::swi::X;

use crate::script::{self, OnError, Source};

/// What `Obey$Dir` is for a name with no directory in it.
const CURRENT: &[u8] = b"@";

/// The Obey module.
pub static OBEY: Builtin = Builtin {
    title: "Obey",
    help_name: None,
    bridge: false,
    swis: None,
    services: None,
    commands: &[Command {
        name: "Obey",
        kind: CommandKind::Command,
        min: 0,
        max: 255,
        translate: 0,
        help: Help::Text(
            "*Obey runs each line of a file as a command, the parameters given put in for %0 to %9 and %*0 to %*9. -c reads the whole file before running it, -v shows each line first.",
        ),
        syntax: Some("Syntax: *Obey [-c] [-v] [<filename> [<parameters>]]"),
        run: Some(obey),
    }],
    init: None,
    finalise: None,
    start: None,
    routines: &[],
    vectors: &[],
    resources: &[],
    code: None,
};

/// What `*Obey`'s options ask.
#[derive(Clone, Copy, Default)]
struct Options {
    /// The whole file read before its first line runs.
    cache: bool,
    /// Each line shown before it runs.
    verbose: bool,
}

/// `*Obey`: its options, then the file and its parameters; nothing named is
/// nothing to do.
fn obey(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let mut options = Options::default();
    let mut at = regs.r[0];
    let name = loop {
        let (_, start) = os.skip_spaces(at)?;
        let (word, _) = os.read_until(start, MOST_LINE, |b| b <= b' ')?;
        match word.as_slice() {
            [b'-', letter] if letter.eq_ignore_ascii_case(&b'c') => options.cache = true,
            [b'-', letter] if letter.eq_ignore_ascii_case(&b'v') => options.verbose = true,
            _ => break (word, start),
        }
        at = start.wrapping_add(len32(word.len()));
    };
    let (name, start) = name;
    if name.is_empty() {
        return Ok(());
    }
    let (_, tail) = os.skip_spaces(start.wrapping_add(len32(name.len())))?;
    let (arguments, _) = os.read_until(tail, MOST_LINE, |b| b < b' ')?;
    let handle = open(os, start)?;
    let lines = if options.cache {
        let file = read_all(os, handle);
        let closed = close(os, handle);
        let file = file?;
        closed?;
        Lines::Cached { file, place: 0 }
    } else {
        Lines::Open {
            handle,
            ended: false,
        }
    };
    let file = File {
        lines,
        arguments,
        verbose: options.verbose,
    };
    if let Err(fail) = os.set_string_variable(b"Obey$Dir", directory_of(&name)) {
        let _ = file.close(os);
        return Err(fail);
    }
    script::run(os, Source::Obey(file), OnError::Stop).map(drop)
}

/// An Obey file being obeyed: where its lines come from, the parameters put
/// in each, and whether each is shown before it runs.
pub(crate) struct File {
    lines: Lines,
    arguments: Vec<u8>,
    verbose: bool,
}

impl File {
    /// The next line, its parameters put in; none after the last.
    pub(crate) fn next(&mut self, os: &mut Os<'_>) -> Result<Option<Vec<u8>>, Fail> {
        let Some(line) = self.lines.next(os)? else {
            return Ok(None);
        };
        let command = substitute(&self.arguments, &line, false, MOST_LINE)
            .map_err(|error| os.error(obey_error(error)))?;
        if self.verbose {
            os.write_line(&command)?;
        }
        Ok(Some(command))
    }

    /// The file let go: closed where it is still open.
    pub(crate) fn close(self, os: &mut Os<'_>) -> Result<(), Fail> {
        match self.lines {
            Lines::Open { handle, .. } => close(os, handle),
            Lines::Cached { .. } => Ok(()),
        }
    }
}

/// `Obey$Dir` for a file named `name`: the name up to its last full stop,
/// or the current directory where it has none, a prefix and all.
fn directory_of(name: &[u8]) -> &[u8] {
    name.iter()
        .rposition(|&b| b == b'.')
        .map_or(CURRENT, |at| &name[..at])
}

/// The file named at `name` opened to read, refused where it is missing or a
/// directory.
fn open(os: &mut Os<'_>, name: u32) -> Result<u32, Fail> {
    let mut call = Regs::default();
    call.r[..2].copy_from_slice(&[find::INPUT | find::MUST_OPEN, name]);
    os.perform(X | swi::FIND, &mut call)?;
    Ok(call.r[0])
}

/// `handle` closed.
fn close(os: &mut Os<'_>, handle: u32) -> Result<(), Fail> {
    let mut call = Regs::default();
    call.r[1] = handle;
    os.perform(X | swi::FIND, &mut call)
}

/// Where an Obey file's lines come from.
enum Lines {
    /// The open file on `handle`, read a byte at a time so what the lines
    /// before one do to the file is what it finds; `ended` once its end has
    /// been read, which a read after would be refused.
    Open { handle: u32, ended: bool },
    /// The whole file, read before its first line, from `place` on.
    Cached { file: Vec<u8>, place: usize },
}

impl Lines {
    /// The next line, none after the last.
    fn next(&mut self, os: &mut Os<'_>) -> Result<Option<Vec<u8>>, Fail> {
        match self {
            Self::Cached { file, place } => {
                if *place >= file.len() {
                    return Ok(None);
                }
                let rest = &file[*place..];
                let length = rest
                    .iter()
                    .position(|&b| is_terminator(b))
                    .unwrap_or(rest.len());
                let line =
                    tairix_util::fallible::copied(&rest[..length]).ok_or_else(|| os.no_memory())?;
                *place += length + 1;
                Ok(Some(line))
            }
            Self::Open { handle, ended } => {
                if *ended {
                    return Ok(None);
                }
                let mut line = Vec::new();
                loop {
                    let mut call = Regs::default();
                    call.r[1] = *handle;
                    os.perform(X | swi::BGET, &mut call)?;
                    if call.flag(tairix_riscos_arm::psr::C) {
                        *ended = true;
                        return Ok((!line.is_empty()).then_some(line));
                    }
                    let byte = call.byte(0);
                    if is_terminator(byte) {
                        return Ok(Some(line));
                    }
                    if line.len() >= MOST_LINE {
                        return Err(os.error(tairix_riscos_kernel::error::LINE_TOO_LONG));
                    }
                    if line.try_reserve(1).is_err() {
                        return Err(os.no_memory());
                    }
                    line.push(byte);
                }
            }
        }
    }
}

/// The whole file on `handle`, from its start.
fn read_all(os: &mut Os<'_>, handle: u32) -> Result<Vec<u8>, Fail> {
    let mut extent = Regs::default();
    extent.r[..2].copy_from_slice(&[swi::args::READ_EXTENT, handle]);
    os.perform(X | swi::ARGS, &mut extent)?;
    let length = extent.r[2];
    let mut file = Vec::new();
    if file.try_reserve_exact(length as usize).is_err() {
        return Err(os.no_memory());
    }
    file.resize(length as usize, 0);
    let buffer = os.rma_claim(length.max(4))?;
    let mut read = Regs::default();
    read.r[..5].copy_from_slice(&[swi::gbpb::READ_AT, handle, buffer, length, 0]);
    let got = os
        .perform(X | swi::GBPB, &mut read)
        .and_then(|()| os.read_bytes(buffer, &mut file));
    os.rma_free(buffer)?;
    got.map(|()| file)
}
