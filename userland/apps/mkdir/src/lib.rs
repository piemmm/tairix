//! TAIRiX `mkdir` — make directories (`plans/APPS.md` §12.1 Stage C).
//!
//! `mkdir` creates each directory operand, in order. With `-p`/`--parents`
//! it creates every missing ancestor first and tolerates operands that
//! already exist as directories, the GNU semantics. `-m`/`--mode` gives each
//! operand it makes exactly the mode named, octal or symbolic, read by the
//! one grammar `chmod` reads (`tairix_util::mode`) from `a=rwx`; without it
//! an operand is made `a=rwx` less the umask. A missing ancestor `-p` makes
//! is `a=rwx` less the umask with its owner's write and search kept, so its
//! own child can always be made. `-v`/`--verbose` reports each directory
//! actually created. `-h`/`-?`/`--help` render the tool's own short help from
//! its bundled `Help/` tree through the shared `lib/help` engine
//! (plans/APPS.md §4).
//!
//! # What this crate is
//!
//! A **pure creation engine**: for one parsed [`Command`] it decides which
//! directories to create, in which order, with which modes, and what to
//! report. Everything that touches the outside world is an injected seam:
//!
//! * [`tairix_fscopy::tree::Roots`] — the shared tree seam every file tool
//!   walks through: an operand's directory is opened by path, and each
//!   directory made is held, so its mode is read and set through it, and a
//!   `-p` ancestor's child is made in it, whatever its name holds by then.
//! * [`Output`] — write `-v` reports to the terminal.
//! * [`tairix_help::HelpSource`] — the tool's own `Help/` tree, read by the
//!   short-help switches.
//!
//! The binary that ships as `mkdir` wires `tairix_rt::copy`'s seam and the
//! console; tests wire the shared in-memory tree (`tairix_fscopy::fixture`).
//!
//! The kernel stays the one path validator: a plain operand is made in the
//! directory its path names, as spelled, and only the `-p` walk reads it —
//! through the one shared grammar (`tairix_path::steps`) — with an
//! unreadable operand handed to the kernel whole so no second validator
//! exists.
//!
//! # Fail closed
//!
//! An option `getopt` refuses ([`MkdirError::Option`]), a mode that is
//! neither octal nor symbolic ([`MkdirError::BadMode`]) or a missing operand
//! ([`MkdirError::Usage`]) creates nothing; a refused creation is reported
//! with the kernel's own [`Errno`] in [`MkdirError::Create`], and a mode that
//! could not be given in [`MkdirError::Mode`], and the next operand is still
//! made, the run exiting 1; a failed terminal write is [`MkdirError::Output`].
//! No `unsafe`, and no `unwrap`/`expect`/`panic!` in production paths.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

use tairix_abi::fs::{FS_MODE_MASK, FS_NEW_DIR_MODE};
use tairix_abi::Errno;
use tairix_fscopy::tree::{entry_name, Dir, Node, Roots};
use tairix_help::{own_short_help, HelpSource};
use tairix_path::Step;
use tairix_util::getopt::{Arg, Opt, Order, Refusal, Scan, Takes};
use tairix_util::mode::{Mode, SET_ID};
use tairix_util::quote::quote;

/// The usage banner the short-help switches print when `mkdir`'s own Help
/// tree is unavailable.
pub const USAGE: &str = "\
usage: mkdir [-pv] [-m MODE] [--] directory...

  -m, --mode=MODE  give each new directory MODE (as in chmod), not
                   a=rwx less the umask
  -p, --parents    make missing parent directories; an operand that is
                   already a directory is not an error
  -v, --verbose    report each created directory
  -h, -?, --help   show this message

At least one directory operand is required. `--` ends option parsing:
every later argument is a path.
";

/// `mkdir`'s own command word: the short-help switches render its own Help
/// document through the same engine as any other command's.
const OWN_WORD: &str = "mkdir";

/// The owner's write and search bits, which a missing ancestor keeps so its
/// child can be made in it.
const OWNER_WRITE_SEARCH: u32 = 0o300;

/// Why a `mkdir` invocation did not complete.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MkdirError {
    /// An option `getopt` refuses, worded as it words it. Nothing is
    /// created.
    Option(Refusal),
    /// The `-m` operand, carried, reads as neither an octal nor a symbolic
    /// mode. Nothing is created.
    BadMode(String),
    /// The command line named no operand. Nothing is created.
    Usage,
    /// Creating `path` failed. Carries the kernel's [`Errno`] — e.g.
    /// [`Errno::AlreadyExists`] for an existing name or
    /// [`Errno::NotADirectory`] when an ancestor is a file.
    Create {
        /// The path whose creation was refused.
        path: String,
        /// The kernel's refusal.
        errno: Errno,
    },
    /// `path` was made, but the mode it was to have could not be read back
    /// or given to it.
    Mode {
        /// The directory made.
        path: String,
        /// The kernel's refusal.
        errno: Errno,
    },
    /// Writing to the terminal failed. Carries the underlying [`Errno`].
    Output(Errno),
}

impl Failure for MkdirError {
    /// Everything but a refused creation or mode, which is carried past.
    fn is_fatal(&self) -> bool {
        !matches!(self, Self::Create { .. } | Self::Mode { .. })
    }

    fn unreported(errno: Errno) -> Self {
        Self::Output(errno)
    }
}

impl fmt::Display for MkdirError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Option(refusal) => write!(f, "{refusal}"),
            Self::BadMode(spec) => write!(f, "invalid mode {}", quote(spec)),
            Self::Usage => f.write_str("invalid usage"),
            Self::Create { path, errno } => {
                write!(
                    f,
                    "cannot create directory {path}: {errno}",
                    path = quote(path)
                )
            }
            Self::Mode { path, errno } => {
                write!(
                    f,
                    "cannot set permissions of {path}: {errno}",
                    path = quote(path)
                )
            }
            Self::Output(errno) => write!(f, "terminal write failed: {errno}"),
        }
    }
}

/// The parsed switches of a creation run.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Options {
    /// `-p`/`--parents`: create missing ancestors; an operand that already
    /// exists as a directory is not an error.
    pub parents: bool,
    /// `-v`/`--verbose`: report each directory actually created.
    pub verbose: bool,
}

/// One parsed `mkdir` command line.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Command {
    /// A requested short help (`-h`, `-?`, `--help`).
    Help,
    /// Create the named directories.
    Make {
        /// The parsed switches.
        options: Options,
        /// The mode `-m` names for each operand, the last one given.
        mode: Option<Mode>,
        /// The directory operands, in command-line order.
        paths: Vec<String>,
    },
}

/// What each of `mkdir`'s options means.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Flag {
    Mode,
    Parents,
    Verbose,
    Help,
}

/// `mkdir`'s options, as GNU's `mkdir` spells them, and the reserved help
/// switches.
const OPTIONS: &[Opt<Flag>] = &[
    Opt::valued('m', &["mode"], Flag::Mode),
    Opt::flag('p', &["parents"], Flag::Parents),
    Opt::flag('v', &["verbose"], Flag::Verbose),
    Opt::flag('h', &["help"], Flag::Help),
    Opt::letter('?', Takes::Nothing, Flag::Help),
];

/// Parse an argument vector (without the program name), read as GNU's
/// `getopt_long` reads it ([`Scan`]): options anywhere among the operands,
/// clustered, a long one by any unambiguous prefix, `--` ending them. The
/// help switches (`-h`, `-?`, `--help`) answer at once. The last `-m` wins,
/// and is read only once the line is, as GNU reads it.
///
/// # Errors
///
/// * [`MkdirError::Option`] — an option `getopt` refuses, in its words.
/// * [`MkdirError::BadMode`] — the `-m` operand is no mode.
/// * [`MkdirError::Usage`] — no operand is named.
pub fn parse(args: &[&str]) -> Result<Command, MkdirError> {
    let mut options = Options::default();
    let mut spec = None;
    let mut paths = Vec::new();
    for arg in Scan::new(OPTIONS, args, Order::Permute) {
        match arg.map_err(MkdirError::Option)? {
            Arg::Operand(path) => paths.push(String::from(path)),
            Arg::Opt(Flag::Help, _) => return Ok(Command::Help),
            Arg::Opt(Flag::Mode, value) => spec = Some(value.unwrap_or_default()),
            Arg::Opt(Flag::Parents, _) => options.parents = true,
            Arg::Opt(Flag::Verbose, _) => options.verbose = true,
        }
    }
    if paths.is_empty() {
        return Err(MkdirError::Usage);
    }
    let mode = spec
        .map(|spec| Mode::parse(spec).ok_or_else(|| MkdirError::BadMode(String::from(spec))))
        .transpose()?;
    Ok(Command::Make {
        options,
        mode,
        paths,
    })
}

pub use tairix_util::output::Output;
use tairix_util::output::{Carried, Failure};

/// Run one [`Command`], creating its operands through `fs` under the
/// process's `umask`. `locale` is the user's `LANG` preference, if set;
/// `help` is the tool's own `Help/` tree, read by the short-help switches.
///
/// Operands are created in order. A refused one ([`MkdirError::Create`],
/// [`MkdirError::Mode`]) is reported on `err` in GNU's words and the next is
/// still made, as GNU's are, answering `Ok(false)` at the end; every operand
/// made answers `Ok(true)`.
///
/// # Errors
///
/// [`MkdirError::Output`] — writing the short help, a `-v` report or a
/// diagnostic failed.
pub fn run<R: Roots>(
    command: Command,
    locale: Option<&str>,
    fs: &R,
    umask: u32,
    help: &dyn HelpSource,
    out: &dyn Output,
    err: &dyn Output,
) -> Result<bool, MkdirError> {
    match command {
        Command::Help => short_help(locale, help, out).map(|()| true),
        Command::Make {
            options,
            mode,
            paths,
        } => {
            let maker = Maker {
                fs,
                out,
                verbose: options.verbose,
                mode: mode.as_ref(),
                umask,
            };
            let mut carried = Carried::new(err, OWN_WORD);
            for path in &paths {
                let made = if options.parents {
                    maker.make_parents(path)
                } else {
                    maker.make_operand(path)
                };
                carried.absorb(made)?;
            }
            Ok(carried.clean())
        }
    }
}

/// Render the tool's own short help through the shared engine, falling back
/// to the usage banner when no document can be served.
fn short_help(
    locale: Option<&str>,
    help: &dyn HelpSource,
    out: &dyn Output,
) -> Result<(), MkdirError> {
    let bytes =
        own_short_help(help, locale, OWN_WORD).unwrap_or_else(|| String::from(USAGE).into_bytes());
    out.write_all(&bytes).map_err(MkdirError::Output)
}

/// One run's making: its seams, what it reports, and the modes its
/// directories are given.
struct Maker<'a, R: Roots> {
    fs: &'a R,
    out: &'a dyn Output,
    verbose: bool,
    mode: Option<&'a Mode>,
    umask: u32,
}

impl<R: Roots> Maker<'_, R> {
    /// Create the operand `path` in the directory holding it.
    fn make_operand(&self, path: &str) -> Result<(), MkdirError> {
        if entry_name(path).is_none() {
            // A root, `.` or `..` names no entry to make: it exists, or says
            // why not.
            let refusal = self
                .fs
                .open_dir(path, true)
                .map_or_else(|errno| errno, |_| Errno::AlreadyExists);
            return Err(create_error(path, refusal));
        }
        let (parent, name) = self
            .fs
            .parent(path)
            .map_err(|errno| create_error(path, errno))?;
        self.make_operand_in(&parent, &name, path)
    }

    /// Make `name` in `at`, shown as `shown`, with `-m`'s mode when one was
    /// named.
    fn make_operand_in(&self, at: &R::Dir, name: &str, shown: &str) -> Result<(), MkdirError> {
        let Some(mode) = self.mode else {
            at.make_dir(name, FS_NEW_DIR_MODE)
                .map_err(|errno| create_error(shown, errno))?;
            return self.report(shown);
        };
        let dir = at
            .make_dir(name, mode.resolve(FS_NEW_DIR_MODE, true, self.umask))
            .map_err(|errno| create_error(shown, errno))?;
        self.report(shown)?;
        // The kernel narrowed the directory by the umask, and gave it the
        // set-group-ID bit of a parent carrying one, which a mode not naming
        // that bit keeps.
        let born = mode_of(shown, &dir)?;
        let wanted = mode.resolve(FS_NEW_DIR_MODE | (born & SET_ID), true, self.umask);
        if wanted == born {
            return Ok(());
        }
        dir.set_mode(wanted)
            .map_err(|errno| mode_error(shown, errno))
    }

    /// Make the missing ancestor `name` in `at`, shown as `shown`, keeping its
    /// owner's write and search whatever the umask holds so the next directory
    /// down can be made in it, and hold it; an ancestor already there is
    /// entered as a path walk passes through it.
    fn make_parent_in(&self, at: &R::Dir, name: &str, shown: &str) -> Result<R::Dir, MkdirError> {
        let dir = match at.make_dir(name, FS_NEW_DIR_MODE) {
            Ok(dir) => dir,
            Err(Errno::AlreadyExists) => {
                return at
                    .enter(name)
                    .map_err(|_| create_error(shown, Errno::AlreadyExists));
            }
            Err(errno) => return Err(create_error(shown, errno)),
        };
        self.report(shown)?;
        if self.umask & OWNER_WRITE_SEARCH == 0 {
            return Ok(dir);
        }
        let born = mode_of(shown, &dir)?;
        match dir.set_mode(born | OWNER_WRITE_SEARCH) {
            // A format keeping no permission bits refuses no owner either.
            Ok(()) | Err(Errno::NotSupported) => Ok(dir),
            Err(errno) => Err(mode_error(shown, errno)),
        }
    }

    /// Create `operand` and every missing ancestor, outermost first,
    /// tolerating ancestors and an operand that already exist as directories
    /// (the GNU `-p` semantics).
    ///
    /// Each level is made in, or entered from, the one before it, held: a
    /// name another process swaps for a link between two levels cannot carry
    /// the walk elsewhere, and each `..` leaves the directory the walk
    /// reached. The steps come from the one shared path grammar
    /// (`tairix_path::steps`); an operand that grammar cannot read is handed
    /// to the kernel whole, so the kernel stays the one validator.
    fn make_parents(&self, operand: &str) -> Result<(), MkdirError> {
        let Ok((_, mut read)) = tairix_path::steps(operand) else {
            return self.make_operand(operand);
        };
        // Read whole first, so an operand refused part way makes nothing.
        let mut steps = Vec::new();
        while let Some(step) = read.next() {
            let Ok(step) = step else {
                return self.make_operand(operand);
            };
            steps.push((step, read.spelled_len()));
        }
        let Some((&(last, _), parents)) = steps.split_last() else {
            // The operand names a bare root (`/`, `Home:/`, `.`), which always
            // exists; under `-p` an existing directory is not an error.
            return Ok(());
        };
        let root = match tairix_path::root_len(operand) {
            Ok(0) | Err(_) => ".",
            Ok(len) => &operand[..len],
        };
        let mut held = self
            .fs
            .open_dir(root, true)
            .map_err(|errno| create_error(operand, errno))?;
        for &(step, end) in parents {
            let shown = &operand[..end];
            held = match step {
                Step::Up => held
                    .enter("..")
                    .map_err(|errno| create_error(shown, errno))?,
                Step::Name(name) => self.make_parent_in(&held, name, shown)?,
            };
        }
        match last {
            Step::Up => held
                .enter("..")
                .map(drop)
                .map_err(|errno| create_error(operand, errno)),
            Step::Name(name) => match self.make_operand_in(&held, name, operand) {
                Err(MkdirError::Create {
                    errno: Errno::AlreadyExists,
                    ..
                }) if held.enter(name).is_ok() => Ok(()),
                made => made,
            },
        }
    }

    /// Write the GNU `-v` report line for a created directory.
    fn report(&self, path: &str) -> Result<(), MkdirError> {
        if !self.verbose {
            return Ok(());
        }
        self.out
            .write_all(format!("mkdir: created directory {path}\n", path = quote(path)).as_bytes())
            .map_err(MkdirError::Output)
    }
}

/// The mode bits of the directory just made, shown as `shown`, held as
/// `made`.
fn mode_of<D: Node>(shown: &str, made: &D) -> Result<u32, MkdirError> {
    made.stat()
        .map(|stat| stat.mode & FS_MODE_MASK)
        .map_err(|errno| mode_error(shown, errno))
}

fn create_error(path: &str, errno: Errno) -> MkdirError {
    MkdirError::Create {
        path: String::from(path),
        errno,
    }
}

fn mode_error(path: &str, errno: Errno) -> MkdirError {
    MkdirError::Mode {
        path: String::from(path),
        errno,
    }
}

#[cfg(test)]
mod tests;
