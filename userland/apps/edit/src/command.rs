//! The parsed shape of an `edit` command line.

use alloc::string::String;

use tairix_util::getopt::{Arg, Opt, Order, Scan, Takes};

use crate::error::EditError;

/// The usage banner the short-help switches print when `edit`'s own Help
/// tree is unavailable.
pub const USAGE: &str = "usage: edit [file] [-h | -?]";

/// One thing the `edit` tool can do.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Command {
    /// Run the full-screen editor, on `path` when one was named (a file
    /// that does not exist yet starts as an empty buffer to be created on
    /// the first save) or on an unnamed new buffer otherwise.
    Run {
        /// The file operand, when the command line named one.
        path: Option<String>,
    },
    /// Render `edit`'s own short help (`-h`/`-?`/`--help`): the `NAME`,
    /// `SYNOPSIS`, and compact `OPTIONS` of its Help document, through the
    /// same engine as any other command's short help (plans/APPS.md §4).
    Help,
}

/// What each of `edit`'s options means.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Flag {
    Help,
}

/// `edit`'s options: the reserved help switches.
const OPTIONS: &[Opt<Flag>] = &[
    Opt::flag('h', &["help"], Flag::Help),
    Opt::letter('?', Takes::Nothing, Flag::Help),
];

/// Parse `args` (the tool's arguments, excluding the program name) into a
/// [`Command`].
///
/// The grammar is `edit [file] [-h | -?]`, read as GNU's `getopt_long` reads
/// it ([`Scan`]): options anywhere among the operands, `--` ending them. The
/// help switches (`-h`, `-?`, `--help`) answer at once. The one optional
/// operand is the file to edit; everything the editor does beyond that is
/// keys pressed inside the session.
///
/// # Errors
///
/// * [`EditError::Option`] — an option `getopt` refuses, in its words.
/// * [`EditError::Usage`] — a second operand.
pub fn parse(args: &[&str]) -> Result<Command, EditError> {
    let mut path: Option<String> = None;
    for arg in Scan::new(OPTIONS, args, Order::Permute) {
        match arg.map_err(EditError::Option)? {
            Arg::Opt(Flag::Help, _) => return Ok(Command::Help),
            // A second operand: the editor holds exactly one buffer.
            Arg::Operand(_) if path.is_some() => return Err(EditError::Usage),
            Arg::Operand(file) => path = Some(String::from(file)),
        }
    }
    Ok(Command::Run { path })
}
