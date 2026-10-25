//! The parsed shape of a `groupdel` command line.

use alloc::string::String;
use core::fmt;

use tairix_util::getopt::{Arg, Opt, Order, Refusal, Scan, Takes};

/// One thing the `groupdel` tool can do.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Command {
    /// Delete the named group.
    Delete(String),
    /// Render this command's own short help (`-h`/`-?`/`--help`).
    Help,
}

/// The command line was not understood. Nothing is deleted.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum UsageError {
    /// An option `getopt` refuses, worded as it words it.
    Option(Refusal),
    /// No operand, or more than one.
    Usage,
}

impl fmt::Display for UsageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Option(refusal) => write!(f, "{refusal}"),
            Self::Usage => f.write_str("invalid usage"),
        }
    }
}

/// What each of `groupdel`'s options means.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Flag {
    Help,
}

/// The reserved help switches, `groupdel`'s only options.
const OPTIONS: &[Opt<Flag>] = &[
    Opt::flag('h', &["help"], Flag::Help),
    Opt::letter('?', Takes::Nothing, Flag::Help),
];

/// Parse `args` (the tool's arguments, excluding the program name).
///
/// The grammar is `groupdel [--] NAME`, read as GNU's `getopt_long` reads it
/// ([`Scan`]): the help switches (`-h`, `-?`, `--help`) answer at once
/// wherever they stand, and `--` ends the options. Exactly one operand is
/// required. The group name is not validated here: the registry is the
/// authority on what names exist, and a name it does not hold is simply not
/// found.
///
/// # Errors
///
/// [`UsageError::Option`] for an option `getopt` refuses, and
/// [`UsageError::Usage`] for an operand count other than one.
pub fn parse(args: &[&str]) -> Result<Command, UsageError> {
    let mut operand: Option<&str> = None;
    let mut extra = false;
    for arg in Scan::new(OPTIONS, args, Order::Permute) {
        match arg.map_err(UsageError::Option)? {
            Arg::Operand(name) => extra |= operand.replace(name).is_some(),
            Arg::Opt(Flag::Help, _) => return Ok(Command::Help),
        }
    }
    match (operand, extra) {
        (Some(name), false) => Ok(Command::Delete(String::from(name))),
        _ => Err(UsageError::Usage),
    }
}

#[cfg(test)]
mod tests {
    use super::{parse, Command, UsageError};
    use alloc::string::{String, ToString};

    #[test]
    fn one_operand_is_the_deletion() {
        assert_eq!(
            parse(&["staff"]),
            Ok(Command::Delete(String::from("staff")))
        );
    }

    #[test]
    fn the_help_switches_win_immediately() {
        for flag in ["-h", "-?", "--help"] {
            assert_eq!(parse(&[flag]), Ok(Command::Help));
            assert_eq!(parse(&[flag, "staff"]), Ok(Command::Help));
        }
    }

    #[test]
    fn end_of_options_lets_a_leading_dash_be_a_name() {
        assert_eq!(
            parse(&["--", "-odd"]),
            Ok(Command::Delete(String::from("-odd")))
        );
    }

    #[test]
    fn no_operand_and_too_many_operands_fail_closed() {
        assert_eq!(parse(&[]), Err(UsageError::Usage));
        assert_eq!(parse(&["a", "b"]), Err(UsageError::Usage));
    }

    /// `getopt`'s words for each refusal.
    fn refused(args: &[&str]) -> String {
        parse(args).expect_err("refused").to_string()
    }

    #[test]
    fn a_refused_option_is_worded_as_getopt_words_it() {
        assert_eq!(refused(&["-x", "a"]), "invalid option -- 'x'");
        assert_eq!(refused(&["--frob", "a"]), "unrecognized option '--frob'");
        assert_eq!(
            refused(&["--help=x", "a"]),
            "option '--help' doesn't allow an argument"
        );
    }

    /// `getopt_long` reads `--he` as `--help`.
    #[test]
    fn a_long_option_may_be_abbreviated() {
        assert_eq!(parse(&["--he", "staff"]), Ok(Command::Help));
        assert_eq!(parse(&["staff", "--h"]), Ok(Command::Help));
    }
}
