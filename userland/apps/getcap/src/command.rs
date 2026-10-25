//! The parsed shape of a `getcap` command line.

use alloc::string::String;
use alloc::vec::Vec;

use tairix_util::getopt::{Arg, Opt, Order, Scan};

use crate::error::GetcapError;

/// One thing the `getcap` tool can do.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Command {
    /// Report the capability gate of each of `files`, in operand order.
    Report {
        /// Descend into directories and report their contents
        /// (`-R`/`--recursive`).
        recursive: bool,
        /// The files to report on, in order. Always at least one.
        files: Vec<String>,
    },
    /// Print the usage banner (`-h`/`--help`).
    Help,
}

/// What each of `getcap`'s options means.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Flag {
    Recursive,
    Help,
}

/// `getcap`'s options and its help switches (`-h` and `--help`).
const OPTIONS: &[Opt<Flag>] = &[
    Opt::flag('R', &["recursive"], Flag::Recursive),
    Opt::flag('h', &["help"], Flag::Help),
];

/// Parse `args` (the tool's arguments, excluding the program name) into a
/// [`Command`].
///
/// The grammar is `getcap [-R] [--] file...`, read as GNU's `getopt_long`
/// reads it ([`Scan`]): options anywhere among the operands, clustered, a
/// long one by any unambiguous prefix, `--` ending them. The help switches
/// (`-h`, `--help`) answer at once.
///
/// # Errors
///
/// * [`GetcapError::Option`] — an option `getopt` refuses, in its words.
/// * [`GetcapError::Usage`] — no file operand is given.
pub fn parse(args: &[&str]) -> Result<Command, GetcapError> {
    let mut recursive = false;
    let mut files = Vec::new();
    for arg in Scan::new(OPTIONS, args, Order::Permute) {
        match arg.map_err(GetcapError::Option)? {
            Arg::Operand(file) => files.push(String::from(file)),
            Arg::Opt(Flag::Help, _) => return Ok(Command::Help),
            Arg::Opt(Flag::Recursive, _) => recursive = true,
        }
    }
    if files.is_empty() {
        return Err(GetcapError::Usage);
    }
    Ok(Command::Report { recursive, files })
}

#[cfg(test)]
mod tests {
    use super::{parse, Command};
    use crate::error::GetcapError;
    use alloc::string::{String, ToString};
    use alloc::vec::Vec;

    fn report(recursive: bool, files: &[&str]) -> Command {
        Command::Report {
            recursive,
            files: files.iter().map(|p| String::from(*p)).collect::<Vec<_>>(),
        }
    }

    #[test]
    fn one_file_parses() {
        assert_eq!(parse(&["/f"]), Ok(report(false, &["/f"])));
    }

    #[test]
    fn several_files_are_all_collected() {
        assert_eq!(
            parse(&["/a", "/b", "/c"]),
            Ok(report(false, &["/a", "/b", "/c"]))
        );
    }

    #[test]
    fn no_operand_is_usage() {
        assert_eq!(parse(&[]), Err(GetcapError::Usage));
        assert_eq!(parse(&["-R"]), Err(GetcapError::Usage));
    }

    #[test]
    fn help_flags_win() {
        assert_eq!(parse(&["-h"]), Ok(Command::Help));
        assert_eq!(parse(&["--help"]), Ok(Command::Help));
        assert_eq!(parse(&["-Rh", "/f"]), Ok(Command::Help));
    }

    #[test]
    fn recursive_flag_sets_its_field() {
        assert_eq!(parse(&["-R", "/d"]), Ok(report(true, &["/d"])));
        assert_eq!(parse(&["--recursive", "/d"]), Ok(report(true, &["/d"])));
    }

    #[test]
    fn lowercase_r_is_not_recursive_and_is_usage() {
        // `getcap` spells recursive `-R`; a bare `-r` is not an option.
        assert_eq!(refused(&["-r", "/f"]), "invalid option -- 'r'");
    }

    /// GNU 9.4's words for each refusal.
    fn refused(args: &[&str]) -> String {
        parse(args).expect_err("refused").to_string()
    }

    #[test]
    fn a_refused_option_is_worded_as_getopt_words_it() {
        assert_eq!(refused(&["-x", "/f"]), "invalid option -- 'x'");
        assert_eq!(refused(&["--frob", "/f"]), "unrecognized option '--frob'");
        // A bundle with one bad flag is refused whole.
        assert_eq!(refused(&["-Rx", "/f"]), "invalid option -- 'x'");
        assert_eq!(
            refused(&["--recursive=1", "/f"]),
            "option '--recursive' doesn't allow an argument"
        );
    }

    #[test]
    fn a_long_option_may_be_abbreviated() {
        assert_eq!(parse(&["--rec", "/d"]), Ok(report(true, &["/d"])));
        assert_eq!(parse(&["--he"]), Ok(Command::Help));
    }

    #[test]
    fn double_dash_ends_options_so_a_dash_named_file_is_an_operand() {
        assert_eq!(parse(&["--", "-weird"]), Ok(report(false, &["-weird"])));
    }
}
