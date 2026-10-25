//! TAIRiX `flock` — run a command holding an advisory file lock
//! (`plans/FILELOCK.md`).
//!
//! The canonical way a shell script gets mutual exclusion: `flock` takes a
//! whole-file advisory lock, runs the command while holding it, and releases
//! it when the command finishes. Two runs of the same script naming the same
//! lock file therefore never overlap, and neither has to invent a lockfile
//! protocol with a stale-lock problem — the kernel releases the lock when the
//! holding process ends, however it ends.
//!
//! The option surface follows util-linux `flock(1)` (`-s`, `-x`, `-n`,
//! `-w`, `-E`, `-v`), so a script written for it works here. Three of its
//! options are deliberately absent rather than faked, because TAIRiX has no
//! honest implementation of them:
//!
//! * `-u`/`--unlock` and `-o`/`--close` exist for util-linux's
//!   *file-descriptor* form (`flock -u 9`), where a shell has already opened
//!   the descriptor with `exec 9>file`. TAIRiX locks are owned by the open
//!   file description and released when it closes, so with no descriptor
//!   form there is nothing for either option to act on.
//! * `-c` runs its argument through a shell. It needs a shell-invocation
//!   seam this tool does not have; `flock <file> elsh -c '…'` is the
//!   explicit spelling and does not hide which shell runs.
//!
//! # What this crate is
//!
//! A **parse-and-run engine**. Everything touching the outside world is an
//! injected seam, so the whole decision surface — including every exit code
//! — is host-testable without a kernel:
//!
//! * [`Session`] — take the lock, run the command, release the lock.
//! * [`Output`] — the short help (standard output) and the `-v` notes
//!   (standard error).
//! * [`tairix_help::HelpSource`] — the tool's own `Help/` tree.
//!
//! # Fail closed
//!
//! An option `getopt` refuses ([`FlockError::Option`]), a missing operand,
//! or a malformed `-w`/`-E` value ([`FlockError::Usage`]) means **nothing is
//! locked and nothing is run**. A lock that cannot be taken under `-n` or
//! `-w` exits with the conflict code rather than running the command, so a
//! script can never mistake "I could not get the lock" for "the command
//! succeeded".

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

use tairix_abi::Errno;
use tairix_help::{own_short_help, HelpSource};
use tairix_util::getopt::{Arg, Opt, Order, Refusal, Scan, Takes};
use tairix_util::quote::quote_subject;

/// The usage banner the short-help switches print when `flock`'s own Help
/// tree is unavailable.
pub const USAGE: &str = "\
usage: flock [options] file command [argument...]

  -s, --shared             take a shared (read) lock
  -x, --exclusive          take an exclusive (write) lock (default)
  -n, --nonblock           fail rather than wait for the lock
  -w, --timeout <seconds>  wait at most <seconds> for the lock
  -E, --conflict-code <n>  exit with <n> when -n or -w gives up (default 1)
  -v, --verbose            report what was locked, on standard error
  -?, --help               show this message

The lock covers the whole file and is held for as long as the command runs.
`--` ends option parsing. Exit status is the command's own, or the conflict
code when the lock could not be taken.
";

/// Exit status for a command word that resolved to nothing, matching the
/// shell's own spelling for it.
pub const EXIT_NOT_FOUND: i32 = 127;

/// Exit status for a command that resolved but could not be executed,
/// matching the shell's own spelling for it.
pub const EXIT_NOT_EXECUTABLE: i32 = 126;

/// The conflict status `-E` defaults to.
pub const DEFAULT_CONFLICT_CODE: i32 = 1;

/// Nanoseconds in one second, for the `-w` operand's conversion.
const NANOS_PER_SECOND: u64 = tairix_abi::time::NANOS_PER_SEC as u64;

/// The largest `-w` operand accepted, in whole seconds.
///
/// A wait is expressed to the kernel in nanoseconds, so a longer span would
/// not survive the conversion. A century is past any legitimate script's
/// patience, and refusing rather than saturating keeps a typo from reading
/// as "wait forever".
const MAX_TIMEOUT_SECONDS: u64 = u64::MAX / NANOS_PER_SECOND;

/// Which lock a run asks for.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Mode {
    /// Shared: coexists with other shared holders, excludes every writer.
    Shared,
    /// Exclusive: excludes every other holder. The default.
    Exclusive,
}

/// How long a run will wait for the lock.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Wait {
    /// Wait indefinitely (the default).
    Forever,
    /// Do not wait at all (`-n`).
    Never,
    /// Wait at most this many nanoseconds (`-w`).
    Until(u64),
}

/// One parsed command line.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Command<'a> {
    /// Render the short help and do nothing else.
    pub help: bool,
    /// The lock to take.
    pub mode: Mode,
    /// How long to wait for it.
    pub wait: Wait,
    /// The status to exit with when the lock could not be taken.
    pub conflict_code: i32,
    /// Report the acquisition on standard error.
    pub verbose: bool,
    /// The file whose lock serialises the runs.
    pub file: &'a str,
    /// The command word to run, and its arguments.
    pub argv: Vec<&'a str>,
}

/// Why a run could not proceed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FlockError {
    /// An option `getopt` refuses, worded as it words it; nothing was
    /// locked and nothing ran.
    Option(Refusal),
    /// The command line was malformed; nothing was locked and nothing ran.
    Usage,
    /// The lock could not be taken, for a reason waiting would not fix.
    Lock(Errno),
    /// The command word resolved to nothing.
    NotFound(String),
    /// The command resolved but could not be executed.
    NotExecutable(String),
    /// The command could not be run, or its status could not be collected.
    Run(Errno),
    /// A stream stopped accepting bytes.
    Output,
}

impl fmt::Display for FlockError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Option(refusal) => write!(f, "{refusal}"),
            Self::Usage => f.write_str("bad usage"),
            Self::Lock(err) => write!(f, "cannot lock: {err}"),
            Self::NotFound(word) => {
                write!(f, "{word}: command not found", word = quote_subject(word))
            }
            Self::NotExecutable(word) => write!(
                f,
                "{word}: command not executable",
                word = quote_subject(word)
            ),
            Self::Run(err) => write!(f, "cannot run: {err}"),
            Self::Output => f.write_str("cannot write output"),
        }
    }
}

impl FlockError {
    /// The exit status this failure reports.
    #[must_use]
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::Option(_) | Self::Usage => tairix_help::MISUSE,
            Self::NotFound(_) => EXIT_NOT_FOUND,
            Self::NotExecutable(_) => EXIT_NOT_EXECUTABLE,
            Self::Lock(_) | Self::Run(_) | Self::Output => 1,
        }
    }
}

/// The outcome of running the command word.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Ran {
    /// The command ran and left this status.
    Exited(i32),
    /// The command word resolved to nothing.
    NotFound,
    /// The command resolved but could not be executed.
    NotExecutable,
}

/// Taking the lock, running the command, and releasing the lock.
///
/// One seam rather than three, because the three are one transaction: the
/// lock must be held for exactly the span the command runs in, and splitting
/// them across seams would let a caller wire an order that does not hold it.
pub trait Session {
    /// Open `path` (creating it if absent) and take `mode` over the whole of
    /// it, waiting as `wait` says.
    ///
    /// # Errors
    ///
    /// [`Errno::WouldBlock`] when a `Never` wait found the lock held,
    /// [`Errno::TimedOut`] when an `Until` wait ran out, or whatever refusal
    /// the open or the lock produced.
    fn lock(&self, path: &str, mode: Mode, wait: Wait) -> Result<(), Errno>;

    /// Run `word` with `args`, waiting for it to finish.
    ///
    /// # Errors
    ///
    /// Whatever refusal the launch or the status collection produced.
    fn run(&self, word: &str, args: &[&str]) -> Result<Ran, Errno>;
}

pub use tairix_util::output::Output;

/// What each of `flock`'s options means.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Flag {
    Shared,
    Exclusive,
    Nonblock,
    Timeout,
    ConflictCode,
    Verbose,
    Help,
}

/// `flock`'s options, and the reserved help switches.
const OPTIONS: &[Opt<Flag>] = &[
    Opt::flag('s', &["shared"], Flag::Shared),
    Opt::flag('x', &["exclusive"], Flag::Exclusive),
    Opt::flag('n', &["nonblock", "nb"], Flag::Nonblock),
    Opt::valued('w', &["timeout", "wait"], Flag::Timeout),
    Opt::valued('E', &["conflict-code"], Flag::ConflictCode),
    Opt::flag('v', &["verbose"], Flag::Verbose),
    Opt::flag('h', &["help"], Flag::Help),
    Opt::letter('?', Takes::Nothing, Flag::Help),
];

/// Parse the arguments after the command word into a [`Command`].
///
/// The line is read as GNU's `getopt_long` reads it ([`Scan`]): clustered
/// short flags (`-sn`), a value attached (`-w5`, `--timeout=5`) or following
/// (`-w 5`), a long option by any unambiguous prefix, and `--` ending the
/// options so a file or command beginning with a dash stays reachable. The
/// first operand ends them too: the command's own arguments reach it
/// verbatim. A help switch is honoured once the line has been read.
///
/// # Errors
///
/// * [`FlockError::Option`] — an option `getopt` refuses, in its words.
/// * [`FlockError::Usage`] — a malformed value, a missing file, or a
///   missing command word.
pub fn parse<'a>(argv: &'a [&'a str]) -> Result<Command<'a>, FlockError> {
    let mut command = Command {
        help: false,
        mode: Mode::Exclusive,
        wait: Wait::Forever,
        conflict_code: DEFAULT_CONFLICT_CODE,
        verbose: false,
        file: "",
        argv: Vec::new(),
    };
    let mut operands: Vec<&'a str> = Vec::new();
    // `flock lock ls -l` passes `-l` to `ls`: it means nothing to `flock`.
    for arg in Scan::new(OPTIONS, argv, Order::OperandsEnd) {
        match arg.map_err(FlockError::Option)? {
            Arg::Operand(operand) => operands.push(operand),
            Arg::Opt(Flag::Help, _) => command.help = true,
            Arg::Opt(Flag::Shared, _) => command.mode = Mode::Shared,
            Arg::Opt(Flag::Exclusive, _) => command.mode = Mode::Exclusive,
            Arg::Opt(Flag::Nonblock, _) => command.wait = Wait::Never,
            Arg::Opt(Flag::Verbose, _) => command.verbose = true,
            Arg::Opt(Flag::Timeout, value) => {
                command.wait = Wait::Until(parse_seconds(value.ok_or(FlockError::Usage)?)?);
            }
            Arg::Opt(Flag::ConflictCode, value) => {
                command.conflict_code = parse_code(value.ok_or(FlockError::Usage)?)?;
            }
        }
    }

    if command.help {
        return Ok(command);
    }
    let mut operands = operands.into_iter();
    command.file = operands.next().ok_or(FlockError::Usage)?;
    command.argv = operands.collect();
    if command.argv.is_empty() {
        return Err(FlockError::Usage);
    }
    Ok(command)
}

/// Parse a `-w` operand: whole seconds, converted to the nanoseconds the
/// kernel waits in.
fn parse_seconds(value: &str) -> Result<u64, FlockError> {
    let seconds = parse_u64(value)?;
    if seconds > MAX_TIMEOUT_SECONDS {
        return Err(FlockError::Usage);
    }
    Ok(seconds * NANOS_PER_SECOND)
}

/// Parse an `-E` operand: an exit status, which the process ABI carries in
/// the low byte like every other.
fn parse_code(value: &str) -> Result<i32, FlockError> {
    let raw = parse_u64(value)?;
    i32::try_from(raw).map_err(|_| FlockError::Usage)
}

/// Parse an unsigned decimal operand, refusing an empty string, a sign, a
/// non-digit, or a value too large — never saturating one into a different
/// instruction.
fn parse_u64(value: &str) -> Result<u64, FlockError> {
    if value.is_empty() {
        return Err(FlockError::Usage);
    }
    let mut out: u64 = 0;
    for byte in value.bytes() {
        let digit = match byte {
            b'0'..=b'9' => u64::from(byte - b'0'),
            _ => return Err(FlockError::Usage),
        };
        out = out
            .checked_mul(10)
            .and_then(|scaled| scaled.checked_add(digit))
            .ok_or(FlockError::Usage)?;
    }
    Ok(out)
}

/// Take the lock, run the command, and report the status to exit with.
///
/// The lock is released by the process ending, so there is no unlock step to
/// forget on an error path: the kernel owns that guarantee.
///
/// # Errors
///
/// [`FlockError`] for a refusal the caller must report. A *conflict* — the
/// lock held when `-n` or `-w` gave up — is not an error here: it returns
/// the configured conflict code, because failing to get the lock is an
/// ordinary outcome a script branches on.
pub fn run<S: Session, O: Output, N: Output>(
    command: &Command<'_>,
    locale: Option<&str>,
    session: &S,
    help: &dyn HelpSource,
    out: &O,
    notices: &N,
) -> Result<i32, FlockError> {
    if command.help {
        let text =
            own_short_help(help, locale, "flock").unwrap_or_else(|| Vec::from(USAGE.as_bytes()));
        out.write_all(&text).map_err(|_| FlockError::Output)?;
        return Ok(0);
    }

    match session.lock(command.file, command.mode, command.wait) {
        Ok(()) => {}
        // Giving up on a contended lock is an answer, not a failure: the
        // script asked to be told rather than to wait.
        Err(Errno::WouldBlock | Errno::TimedOut) => {
            if command.verbose {
                let _ = notices.write_all(b"flock: lock already held\n");
            }
            return Ok(command.conflict_code);
        }
        Err(err) => return Err(FlockError::Lock(err)),
    }
    if command.verbose {
        let _ = notices.write_all(b"flock: lock held\n");
    }

    let (word, args) = command.argv.split_first().ok_or(FlockError::Usage)?;
    match session.run(word, args) {
        Ok(Ran::Exited(status)) => Ok(status),
        Ok(Ran::NotFound) => Err(FlockError::NotFound(String::from(*word))),
        Ok(Ran::NotExecutable) => Err(FlockError::NotExecutable(String::from(*word))),
        Err(err) => Err(FlockError::Run(err)),
    }
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
