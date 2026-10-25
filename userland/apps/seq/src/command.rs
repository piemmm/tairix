//! The `seq` command line and its parser.

use alloc::string::String;
use alloc::vec::Vec;

use tairix_util::getopt::{Arg, Opt, Order, Scan, Takes};

use crate::error::SeqError;
use crate::format::Format;

/// One parsed `seq` job: what to print and how.
#[derive(Clone, Debug, PartialEq)]
pub struct Job {
    /// The validated `-f` format, if one was given.
    pub format: Option<Format>,
    /// The `-s` separator between numbers (default `\n`).
    pub separator: String,
    /// `-w` — pad every number to equal width with leading zeros.
    pub equal_width: bool,
    /// The 1–3 raw operands (FIRST, INCREMENT, LAST spellings), kept as
    /// typed as the user spelled them: the fast integer path and the
    /// width/precision inference both read the original text.
    pub operands: Vec<String>,
}

/// One thing the `seq` tool can do.
#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    /// Print the sequence the job describes.
    Print(Job),
    /// Render `seq`'s own short help (`-h`/`-?`/`--help`) through the same
    /// engine as any other command's short help (plans/APPS.md §4).
    Help,
}

/// What each of `seq`'s options means.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Flag {
    Format,
    Separator,
    EqualWidth,
    Help,
}

/// `seq`'s options, as GNU's `seq` spells them, and the reserved help
/// switches.
const OPTIONS: &[Opt<Flag>] = &[
    Opt::valued('f', &["format"], Flag::Format),
    Opt::valued('s', &["separator"], Flag::Separator),
    Opt::flag('w', &["equal-width"], Flag::EqualWidth),
    Opt::flag('h', &["help"], Flag::Help),
    Opt::letter('?', Takes::Nothing, Flag::Help),
];

/// Parse `args` (the tool's arguments, excluding the program name) into a
/// [`Command`].
///
/// Read as GNU's `getopt_long` reads `seq`'s line ([`Scan`]): clustered, a
/// long option by any unambiguous prefix, a value attached or following, but
/// not permuted — the first operand ends the options, as does `--` — and an
/// argument that looks like a negative number (`-5`, `-.3`) is an operand.
/// The help switches (`-h`, `-?`, `--help`) answer at once. After the scan:
/// one to three operands are required, the `-f` format must validate, and
/// `-f` may not be combined with `-w` — diagnosed in exactly that order, as
/// in the GNU tool.
///
/// # Errors
///
/// * [`SeqError::Option`] — an option `getopt` refuses, in its words.
/// * [`SeqError::MissingOperand`] / [`SeqError::ExtraOperand`] — no operand,
///   or more than three.
/// * [`SeqError::FormatNoDirective`], [`SeqError::FormatEndsInPercent`],
///   [`SeqError::FormatUnknownDirective`],
///   [`SeqError::FormatTooManyDirectives`] — a `-f` format that does not
///   validate.
/// * [`SeqError::FormatWithEqualWidth`] — `-f` with `-w`.
pub fn parse(args: &[&str]) -> Result<Command, SeqError> {
    let mut format_str: Option<&str> = None;
    let mut separator: Option<&str> = None;
    let mut equal_width = false;
    let mut operands: Vec<String> = Vec::new();

    for arg in Scan::new(OPTIONS, args, Order::OperandsEnd).numbers_are_operands() {
        match arg.map_err(SeqError::Option)? {
            Arg::Operand(operand) => operands.push(String::from(operand)),
            Arg::Opt(Flag::Help, _) => return Ok(Command::Help),
            Arg::Opt(Flag::Format, value) => format_str = value,
            Arg::Opt(Flag::Separator, value) => separator = value,
            Arg::Opt(Flag::EqualWidth, _) => equal_width = true,
        }
    }

    if operands.is_empty() {
        return Err(SeqError::MissingOperand);
    }
    if operands.len() > 3 {
        return Err(SeqError::ExtraOperand(operands[3].clone()));
    }

    let format = match format_str {
        Some(text) => Some(Format::parse(text)?),
        None => None,
    };
    if format.is_some() && equal_width {
        return Err(SeqError::FormatWithEqualWidth);
    }

    Ok(Command::Print(Job {
        format,
        separator: String::from(separator.unwrap_or("\n")),
        equal_width,
        operands,
    }))
}

#[cfg(test)]
mod tests {
    use alloc::string::{String, ToString};
    use alloc::vec;

    use super::{parse, Command, Job};
    use crate::error::SeqError;
    use crate::format::Format;

    fn print_job(command: Command) -> Job {
        match command {
            Command::Print(job) => job,
            Command::Help => panic!("expected a print job"),
        }
    }

    #[test]
    fn bare_operands_parse() {
        let job = print_job(parse(&["10"]).expect("parses"));
        assert_eq!(job.operands, vec![String::from("10")]);
        assert_eq!(job.separator, "\n");
        assert!(!job.equal_width);
        assert!(job.format.is_none());

        let job2 = print_job(parse(&["1", "2", "10"]).expect("parses"));
        assert_eq!(job2.operands.len(), 3);
    }

    #[test]
    fn negative_numbers_are_operands() {
        let job = print_job(parse(&["-5", "5"]).expect("parses"));
        assert_eq!(job.operands, vec![String::from("-5"), String::from("5")]);
        let job2 = print_job(parse(&["-.5"]).expect("parses"));
        assert_eq!(job2.operands, vec![String::from("-.5")]);
    }

    #[test]
    fn first_operand_ends_option_scanning() {
        // GNU seq does not permute: options after an operand are operands
        // (and fail the number scan later, not the option parser).
        let job = print_job(parse(&["1", "-w", "3"]).expect("parses"));
        assert!(!job.equal_width);
        assert_eq!(job.operands.len(), 3);
    }

    #[test]
    fn double_dash_ends_options() {
        let job = print_job(parse(&["--", "5"]).expect("parses"));
        assert_eq!(job.operands, vec![String::from("5")]);
    }

    #[test]
    fn help_flags_win() {
        assert_eq!(parse(&["-h"]), Ok(Command::Help));
        assert_eq!(parse(&["-?"]), Ok(Command::Help));
        assert_eq!(parse(&["--help"]), Ok(Command::Help));
        assert_eq!(parse(&["-wh", "5"]), Ok(Command::Help));
    }

    #[test]
    fn separator_forms() {
        for args in [
            ["-s", ",", "5"].as_slice(),
            ["-s,", "5"].as_slice(),
            ["--separator", ",", "5"].as_slice(),
            ["--separator=,", "5"].as_slice(),
        ] {
            assert_eq!(
                print_job(parse(args).expect("parses")).separator,
                ",",
                "{args:?}"
            );
        }
        // An empty separator is legal.
        assert_eq!(
            print_job(parse(&["--separator=", "5"]).expect("parses")).separator,
            ""
        );
    }

    #[test]
    fn format_forms() {
        let expected = Format::parse("%.2f").expect("validates");
        for args in [
            ["-f", "%.2f", "5"].as_slice(),
            ["-f%.2f", "5"].as_slice(),
            ["--format", "%.2f", "5"].as_slice(),
            ["--format=%.2f", "5"].as_slice(),
        ] {
            assert_eq!(
                print_job(parse(args).expect("parses")).format.as_ref(),
                Some(&expected),
                "{args:?}"
            );
        }
    }

    #[test]
    fn cluster_flags() {
        let job = print_job(parse(&["-ws,", "5"]).expect("parses"));
        assert!(job.equal_width);
        assert_eq!(job.separator, ",");
    }

    #[test]
    fn operand_count_is_validated() {
        assert_eq!(parse(&[]), Err(SeqError::MissingOperand));
        assert_eq!(parse(&["-w"]), Err(SeqError::MissingOperand));
        assert_eq!(
            parse(&["1", "2", "3", "4"]),
            Err(SeqError::ExtraOperand(String::from("4")))
        );
    }

    /// GNU 9.4's words for each refusal.
    fn refused(args: &[&str]) -> String {
        parse(args).expect_err("refused").to_string()
    }

    #[test]
    fn a_refused_option_is_worded_as_getopt_words_it() {
        assert_eq!(refused(&["--frob", "5"]), "unrecognized option '--frob'");
        assert_eq!(refused(&["-x", "5"]), "invalid option -- 'x'");
        assert_eq!(refused(&["-w5"]), "invalid option -- '5'");
        assert_eq!(refused(&["-f"]), "option requires an argument -- 'f'");
        assert_eq!(refused(&["-s"]), "option requires an argument -- 's'");
        assert_eq!(
            refused(&["--format"]),
            "option '--format' requires an argument"
        );
        assert_eq!(
            refused(&["--separator"]),
            "option '--separator' requires an argument"
        );
        assert_eq!(
            refused(&["--equal-width=x", "5"]),
            "option '--equal-width' doesn't allow an argument"
        );
    }

    /// GNU 9.4 reads `seq --eq --sep=, 5` as `seq --equal-width --separator=, 5`.
    #[test]
    fn a_long_option_may_be_abbreviated() {
        let job = print_job(parse(&["--eq", "--sep=,", "5"]).expect("parses"));
        assert!(job.equal_width);
        assert_eq!(job.separator, ",");
        assert_eq!(parse(&["--he"]), Ok(Command::Help));
    }

    #[test]
    fn diagnostics_follow_the_gnu_order() {
        // Missing operands are diagnosed before the format is validated…
        assert_eq!(parse(&["-f", "%d"]), Err(SeqError::MissingOperand));
        // …the format is validated before the -w conflict…
        assert_eq!(
            parse(&["-w", "-f", "%d", "5"]),
            Err(SeqError::FormatUnknownDirective(String::from("%d"), 'd'))
        );
        // …and a valid format with -w is the conflict diagnostic.
        assert_eq!(
            parse(&["-w", "-f", "%f", "5"]),
            Err(SeqError::FormatWithEqualWidth)
        );
    }
}
