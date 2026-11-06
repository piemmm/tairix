//! The parsed shape of a `label` command line.

use alloc::string::String;
use alloc::vec::Vec;

use tairix_util::getopt::{Arg, Opt, Order, Scan, Takes};

use crate::error::LabelError;

/// One thing the `label` tool can do.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Command {
    /// Print the title of the volume `name` names.
    Show {
        /// The volume: its catalog name, a directory it is mounted on, the
        /// file name of the disk image it was mounted from, or its durable
        /// root.
        name: String,
    },
    /// Title the volume `name` names `title`; an empty one clears it.
    Set {
        /// The volume, as for [`Command::Show`].
        name: String,
        /// The title asked for.
        title: String,
    },
    /// Render the tool's own short help (`-h`/`-?`/`--help`).
    Help,
    /// Print the tool's name and version (`--version`).
    Version,
}

/// What each of `label`'s options means.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Flag {
    Help,
    Version,
}

/// `label`'s options: the reserved help and version switches.
const OPTIONS: &[Opt<Flag>] = &[
    Opt::flag('h', &["help"], Flag::Help),
    Opt::letter('?', Takes::Nothing, Flag::Help),
    Opt::long(&["version"], Takes::Nothing, Flag::Version),
];

/// Parse `args` (the tool's arguments, excluding the program name) into a
/// [`Command`].
///
/// The grammar is `label [option...] [--] NAME [TITLE]`, read as GNU's
/// `getopt_long` reads it ([`Scan`]): options anywhere among the operands, a
/// long one by any unambiguous prefix, `--` ending them, so a title beginning
/// `-` follows `--`. The help and version switches win wherever they stand.
///
/// # Errors
///
/// * [`LabelError::Option`] — an option `getopt` refuses, in its words.
/// * [`LabelError::Usage`] — no operand, an empty `NAME`, which names no
///   volume, or more than two operands.
pub fn parse(args: &[&str]) -> Result<Command, LabelError> {
    let mut operands: Vec<&str> = Vec::new();
    for arg in Scan::new(OPTIONS, args, Order::Permute) {
        match arg.map_err(LabelError::Option)? {
            Arg::Operand(operand) => operands.push(operand),
            Arg::Opt(Flag::Help, _) => return Ok(Command::Help),
            Arg::Opt(Flag::Version, _) => return Ok(Command::Version),
        }
    }
    match operands.as_slice() {
        [name] if !name.is_empty() => Ok(Command::Show {
            name: String::from(*name),
        }),
        [name, title] if !name.is_empty() => Ok(Command::Set {
            name: String::from(*name),
            title: String::from(*title),
        }),
        _ => Err(LabelError::Usage),
    }
}

#[cfg(test)]
mod tests {
    use super::{parse, Command};
    use crate::error::LabelError;
    use alloc::string::{String, ToString};

    fn show(name: &str) -> Command {
        Command::Show {
            name: String::from(name),
        }
    }

    fn set(name: &str, title: &str) -> Command {
        Command::Set {
            name: String::from(name),
            title: String::from(title),
        }
    }

    #[test]
    fn one_operand_shows_and_two_set() {
        assert_eq!(parse(&["usb1"]), Ok(show("usb1")));
        assert_eq!(parse(&["usb1", "HOLIDAY"]), Ok(set("usb1", "HOLIDAY")));
        assert_eq!(
            parse(&["usb1", ""]),
            Ok(set("usb1", "")),
            "an empty title clears it"
        );
    }

    #[test]
    fn a_title_beginning_with_a_dash_follows_the_double_dash() {
        assert_eq!(parse(&["--", "usb1", "-x-"]), Ok(set("usb1", "-x-")));
        assert_eq!(parse(&["usb1", "--", "-x-"]), Ok(set("usb1", "-x-")));
    }

    #[test]
    fn help_and_version_win_wherever_they_stand() {
        for args in [
            &["-h"][..],
            &["-?"],
            &["--help"],
            &["usb1", "HOLIDAY", "--he"],
        ] {
            assert_eq!(parse(args), Ok(Command::Help), "{args:?}");
        }
        assert_eq!(parse(&["--version"]), Ok(Command::Version));
        assert_eq!(parse(&["a", "b", "c", "--vers"]), Ok(Command::Version));
    }

    /// An unset variable expanded to nothing names no volume.
    #[test]
    fn wrong_operand_counts_and_an_empty_name_are_usage() {
        assert_eq!(parse(&[]), Err(LabelError::Usage));
        assert_eq!(parse(&["a", "b", "c"]), Err(LabelError::Usage));
        assert_eq!(parse(&[""]), Err(LabelError::Usage));
        assert_eq!(parse(&["", "HOLIDAY"]), Err(LabelError::Usage));
    }

    /// GNU 9.4's words for each refusal.
    #[test]
    fn a_refused_option_is_worded_as_getopt_words_it() {
        assert_eq!(
            parse(&["-x", "usb1"]).expect_err("refused").to_string(),
            "invalid option -- 'x'"
        );
        assert_eq!(
            parse(&["--frob", "usb1"]).expect_err("refused").to_string(),
            "unrecognized option '--frob'"
        );
    }
}
