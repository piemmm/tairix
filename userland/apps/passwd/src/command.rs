//! The parsed shape of a `passwd` command line.

use alloc::string::String;
use core::fmt;

use tairix_util::getopt::{Arg, Opt, Order, Refusal, Scan, Takes};

/// Why the command line was not understood. Nothing is changed.
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

/// Where the new credential comes from.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Secret {
    /// Prompt the terminal twice, echo off, and hash what is typed.
    Prompt,
    /// Use the ready salted PBKDF2 record the caller built, for a caller
    /// with no terminal to prompt at.
    Record(String),
}

/// One thing the `passwd` tool can do.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Command {
    /// Replace the named account's password.
    Set {
        /// The account whose password is replaced.
        name: String,
        /// Where the new credential comes from.
        secret: Secret,
    },
    /// Render this command's own short help (`-h`/`-?`/`--help`).
    Help,
}

/// What each of `passwd`'s options means.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Flag {
    Record,
    Help,
}

/// `passwd`'s options and the reserved help switches. `--record` is
/// TAIRiX-native, so it is long-only and cannot collide with a shadow-utils
/// switch.
const OPTIONS: &[Opt<Flag>] = &[
    Opt::long(&["record"], Takes::Value, Flag::Record),
    Opt::flag('h', &["help"], Flag::Help),
    Opt::letter('?', Takes::Nothing, Flag::Help),
];

/// Parse `args` (the tool's arguments, excluding the program name).
///
/// The grammar is `passwd [--record RECORD] [--] NAME`, read as GNU's
/// `getopt_long` reads it ([`Scan`]): options anywhere among the operands, a
/// value attached (`--record=RECORD`) or following, a long one by any
/// unambiguous prefix, `--` ending them. `--record` is a ready salted PBKDF2
/// record, for a caller with no terminal to prompt at; the help switches
/// (`-h`, `-?`, `--help`) answer at once.
///
/// Exactly one operand — the account name — is required; see the crate
/// docs for why there is no operand-less self-service form.
///
/// # Errors
///
/// [`UsageError::Option`] for an option `getopt` refuses, and
/// [`UsageError::Usage`] for an operand count other than one.
pub fn parse(args: &[&str]) -> Result<Command, UsageError> {
    let mut record: Option<String> = None;
    let mut operand: Option<&str> = None;
    let mut extra = false;
    for arg in Scan::new(OPTIONS, args, Order::Permute) {
        match arg.map_err(UsageError::Option)? {
            Arg::Operand(name) => extra |= operand.replace(name).is_some(),
            Arg::Opt(Flag::Help, _) => return Ok(Command::Help),
            Arg::Opt(Flag::Record, value) => record = value.map(String::from),
        }
    }
    let (Some(name), false) = (operand, extra) else {
        return Err(UsageError::Usage);
    };
    Ok(Command::Set {
        name: String::from(name),
        secret: record.map_or(Secret::Prompt, Secret::Record),
    })
}

#[cfg(test)]
mod tests {
    use super::{parse, Command, Secret, UsageError};
    use alloc::string::{String, ToString};

    #[test]
    fn one_operand_prompts_for_the_new_password() {
        assert_eq!(
            parse(&["ada"]),
            Ok(Command::Set {
                name: String::from("ada"),
                secret: Secret::Prompt,
            })
        );
    }

    #[test]
    fn a_ready_record_is_taken_attached_or_detached() {
        let want = Ok(Command::Set {
            name: String::from("ada"),
            secret: Secret::Record(String::from("pbkdf2-sha256$1000$AAAA$BBBB")),
        });
        assert_eq!(
            parse(&["--record", "pbkdf2-sha256$1000$AAAA$BBBB", "ada"]),
            want
        );
        assert_eq!(
            parse(&["--record=pbkdf2-sha256$1000$AAAA$BBBB", "ada"]),
            want
        );
    }

    #[test]
    fn the_help_switches_win_immediately() {
        for flag in ["-h", "-?", "--help"] {
            assert_eq!(parse(&[flag]), Ok(Command::Help));
            assert_eq!(parse(&[flag, "ada"]), Ok(Command::Help));
        }
    }

    #[test]
    fn end_of_options_lets_a_leading_dash_be_a_name() {
        assert_eq!(
            parse(&["--", "-odd"]),
            Ok(Command::Set {
                name: String::from("-odd"),
                secret: Secret::Prompt,
            })
        );
    }

    #[test]
    fn a_wrong_operand_count_fails_closed() {
        assert_eq!(parse(&[]), Err(UsageError::Usage));
        assert_eq!(parse(&["ada", "bob"]), Err(UsageError::Usage));
    }

    /// `getopt`'s words for each refusal.
    fn refused(args: &[&str]) -> String {
        parse(args).expect_err("refused").to_string()
    }

    #[test]
    fn a_refused_option_is_worded_as_getopt_words_it() {
        assert_eq!(refused(&["-x", "ada"]), "invalid option -- 'x'");
        assert_eq!(refused(&["--frob", "ada"]), "unrecognized option '--frob'");
        assert_eq!(
            refused(&["--record"]),
            "option '--record' requires an argument"
        );
        assert_eq!(
            refused(&["--help=x", "ada"]),
            "option '--help' doesn't allow an argument"
        );
    }

    /// `getopt_long` reads `--rec` as `--record`.
    #[test]
    fn a_long_option_may_be_abbreviated() {
        assert_eq!(
            parse(&["--rec=pbkdf2-sha256$1000$AAAA$BBBB", "ada"]),
            Ok(Command::Set {
                name: String::from("ada"),
                secret: Secret::Record(String::from("pbkdf2-sha256$1000$AAAA$BBBB")),
            })
        );
        assert_eq!(parse(&["--he"]), Ok(Command::Help));
    }
}
