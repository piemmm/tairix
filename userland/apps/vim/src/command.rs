//! The `vim` command line: switches, startup positioning, and file
//! operands.
//!
//! The accepted grammar is the vim core:
//!
//! ```text
//! vim [-R] [+num | + | +/pattern] [--] [file ...]
//! vim -h | -?
//! ```
//!
//! `-R` opens readonly; `+num` starts on line `num`, a bare `+` on the
//! last line, and `+/pattern` on the first match. `--` ends option
//! parsing. The reserved `-h`/`-?` short-help switches render the tool's
//! own Help document (plans/APPS.md). Anything else is a usage error.

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

use tairix_util::getopt::{Arg, Opt, Order, Refusal, Scan, Takes};
use tairix_util::quote::quote_if_needed;

/// The usage banner the `-h` fallback prints when no help document is
/// available.
pub const USAGE: &str = "usage: vim [-R] [+num | + | +/pattern] [--] [file ...]";

/// Where the editor starts after loading the first file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Start {
    /// `+num` — a 1-based line.
    Line(usize),
    /// `+` — the last line.
    LastLine,
    /// `+/pattern` — the first match of `pattern`.
    Pattern(String),
}

/// A parsed command line.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Command {
    /// Serve the short help (`-h` / `-?`).
    Help,
    /// Run the editor.
    Run {
        /// `-R` — refuse writes.
        readonly: bool,
        /// The startup position, if any.
        start: Option<Start>,
        /// The file operands (the argument list).
        files: Vec<String>,
    },
}

/// A command line `vim` refuses, spelled out on stderr by the caller
/// alongside [`USAGE`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum UsageError {
    /// An option `getopt` refuses, worded as it words it.
    Option(Refusal),
    /// A `+…` startup position naming no line or pattern; carries it.
    Start(String),
}

impl fmt::Display for UsageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Option(refusal) => write!(f, "{refusal}"),
            Self::Start(argument) => write!(f, "unknown argument: {}", quote_if_needed(argument)),
        }
    }
}

/// What each of `vim`'s options means.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Flag {
    Readonly,
    Help,
}

/// `vim`'s option, and the reserved help switches.
const OPTIONS: &[Opt<Flag>] = &[
    Opt::letter('R', Takes::Nothing, Flag::Readonly),
    Opt::flag('h', &["help"], Flag::Help),
    Opt::letter('?', Takes::Nothing, Flag::Help),
];

/// Parse the arguments after the command word.
///
/// Options are read as GNU's `getopt_long` reads them ([`Scan`]): anywhere
/// among the operands, clustered, `--` ending them. The help switches (`-h`,
/// `-?`, `--help`) answer at once. Before `--`, an operand beginning with `+`
/// is the startup position, the last one winning; every other operand is a
/// file.
///
/// # Errors
///
/// * [`UsageError::Option`] — an option `getopt` refuses, in its words.
/// * [`UsageError::Start`] — a `+…` operand naming no line or pattern.
pub fn parse(arguments: &[&str]) -> Result<Command, UsageError> {
    // Past `--` a `+…` word is a file, and the scan reports no `--`: split there.
    let end = arguments.iter().position(|&argument| argument == "--");
    let (head, tail) = arguments.split_at(end.unwrap_or(arguments.len()));
    let mut readonly = false;
    let mut start: Option<Start> = None;
    let mut files: Vec<String> = Vec::new();
    for arg in Scan::new(OPTIONS, head, Order::Permute) {
        match arg.map_err(UsageError::Option)? {
            Arg::Opt(Flag::Help, _) => return Ok(Command::Help),
            Arg::Opt(Flag::Readonly, _) => readonly = true,
            Arg::Operand(word) if word.starts_with('+') => {
                start = start_at(word);
                if start.is_none() {
                    return Err(UsageError::Start(String::from(word)));
                }
            }
            Arg::Operand(file) => files.push(String::from(file)),
        }
    }
    files.extend(tail.iter().skip(1).map(|&file| String::from(file)));
    Ok(Command::Run {
        readonly,
        start,
        files,
    })
}

/// The startup position a `+…` word names: `+` the last line, `+/pattern`
/// the first match of `pattern`, `+num` the 1-based line `num`.
fn start_at(word: &str) -> Option<Start> {
    match word.strip_prefix('+')? {
        "" => Some(Start::LastLine),
        position => match position.strip_prefix('/') {
            Some("") => None,
            Some(pattern) => Some(Start::Pattern(String::from(pattern))),
            None => match position.parse::<usize>() {
                Ok(line) if line > 0 => Some(Start::Line(line)),
                _ => None,
            },
        },
    }
}
