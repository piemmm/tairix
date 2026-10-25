//! The parsed shape of a `users` command line.

use alloc::string::String;
use core::fmt;

use tairix_util::getopt::{Arg, Opt, Order, Refusal, Scan, Takes};
use tairix_util::quote::quote;

/// The usage banner the short-help switches print when `users`'s own Help
/// tree is unavailable.
pub const USAGE: &str = "usage: users [-h | -? | -l]";

/// A command line `users` refuses. The session never starts.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum UsageError {
    /// An option `getopt` refuses, worded as it words it.
    Option(Refusal),
    /// An operand; the tool takes none (GNU `extra operand`).
    ExtraOperand(String),
}

impl fmt::Display for UsageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Option(refusal) => write!(f, "{refusal}"),
            Self::ExtraOperand(operand) => {
                write!(f, "extra operand {operand}", operand = quote(operand))
            }
        }
    }
}

/// One thing the `users` tool can do.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Command {
    /// Run the interactive account-administration session.
    Session,
    /// Print the whole account and group listing in the shared relayable
    /// line form and exit, without a session.
    ///
    /// The non-interactive form a caller with no terminal needs: the
    /// desktop's Users pane cannot type into a session — its standard
    /// input is closed when this tool runs under the supervisor's
    /// elevated-read seam — so it asks an authenticated account to run
    /// this and reads what was printed.
    List,
    /// Render `users`'s own short help (`-h`/`-?`/`--help`): the `NAME`,
    /// `SYNOPSIS`, and compact `OPTIONS` of its Help document, through the
    /// same engine as any other command's short help (plans/APPS.md §4).
    Help,
}

/// What each of `users`'s options means.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Flag {
    List,
    Help,
}

/// `users`'s option, and the reserved help switches.
const OPTIONS: &[Opt<Flag>] = &[
    Opt::flag('l', &["list"], Flag::List),
    Opt::flag('h', &["help"], Flag::Help),
    Opt::letter('?', Takes::Nothing, Flag::Help),
];

/// Parse `args` (the tool's arguments, excluding the program name) into a
/// [`Command`].
///
/// The grammar is `users [-h | -? | -l]`, read as GNU's `getopt_long` reads
/// it ([`Scan`]); the first argument decides. The help switches (`-h`, `-?`,
/// `--help`) answer at once, as does `-l` / `--list`, which prints the account
/// and group listing and exits. The tool takes no operands and no other
/// options: accounts are *administered* with commands typed inside the
/// interactive session, not command-line switches.
///
/// # Errors
///
/// * [`UsageError::Option`] — an option `getopt` refuses, in its words.
/// * [`UsageError::ExtraOperand`] — an operand.
pub fn parse(args: &[&str]) -> Result<Command, UsageError> {
    let first = Scan::new(OPTIONS, args, Order::Permute).next().transpose();
    match first.map_err(UsageError::Option)? {
        None => Ok(Command::Session),
        Some(Arg::Opt(Flag::Help, _)) => Ok(Command::Help),
        Some(Arg::Opt(Flag::List, _)) => Ok(Command::List),
        Some(Arg::Operand(operand)) => Err(UsageError::ExtraOperand(String::from(operand))),
    }
}

#[cfg(test)]
mod tests {
    use alloc::string::{String, ToString};

    use super::{parse, Command, UsageError};

    #[test]
    fn no_arguments_runs_the_session() {
        assert_eq!(parse(&[]), Ok(Command::Session));
    }

    #[test]
    fn help_flags_win() {
        assert_eq!(parse(&["-h"]), Ok(Command::Help));
        assert_eq!(parse(&["-?"]), Ok(Command::Help));
        assert_eq!(parse(&["--help"]), Ok(Command::Help));
        // The first token decides; trailing noise never reaches the session.
        assert_eq!(parse(&["-h", "extra"]), Ok(Command::Help));
    }

    #[test]
    fn the_list_switch_prints_without_a_session() {
        assert_eq!(parse(&["-l"]), Ok(Command::List));
        assert_eq!(parse(&["--list"]), Ok(Command::List));
        // The first token decides, as it does for the help switches.
        assert_eq!(parse(&["-l", "extra"]), Ok(Command::List));
        // Help still wins where both are named first.
        assert_eq!(parse(&["-h", "-l"]), Ok(Command::Help));
    }

    #[test]
    fn an_operand_is_an_extra_operand() {
        let err = parse(&["operand"]).expect_err("an operand is refused");
        assert_eq!(err, UsageError::ExtraOperand(String::from("operand")));
        assert_eq!(err.to_string(), "extra operand 'operand'");
    }

    /// GNU's words for each refusal.
    fn refused(args: &[&str]) -> String {
        match parse(args) {
            Err(UsageError::Option(refusal)) => refusal.to_string(),
            other => panic!("expected a refused option, got {other:?}"),
        }
    }

    #[test]
    fn a_refused_option_is_worded_as_getopt_words_it() {
        assert_eq!(refused(&["-x"]), "invalid option -- 'x'");
        assert_eq!(refused(&["--frob"]), "unrecognized option '--frob'");
        assert_eq!(
            refused(&["--list=all"]),
            "option '--list' doesn't allow an argument"
        );
    }

    #[test]
    fn a_long_option_may_be_abbreviated() {
        assert_eq!(parse(&["--li"]), Ok(Command::List));
        assert_eq!(parse(&["--he"]), Ok(Command::Help));
    }

    /// Every locale's `OPTIONS` section documents exactly the switches this
    /// parser accepts (`plans/APPS.md` §3.1): the flag tokens are
    /// language-neutral, so each translated document must carry the same
    /// keys as the canonical one. The documents are read from the bundle's
    /// own on-disk `Help/` tree — the single source the image builder plants
    /// — never a copy embedded in this crate.
    #[test]
    fn help_documents_the_parser_switches() {
        extern crate std;
        use alloc::format;
        use std::fs;

        let help_root = format!("{}/Help", env!("CARGO_MANIFEST_DIR"));
        let locales = tairix_help::REQUIRED_LOCALES;
        for locale in locales {
            let path = format!("{help_root}/{locale}/users.md");
            let text = fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"));
            for switch in ["`-h, -?`", "`-l, --list`"] {
                assert!(
                    text.contains(switch),
                    "{locale}/users.md must document {switch}"
                );
            }
        }
    }
}
