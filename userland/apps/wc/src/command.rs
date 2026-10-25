//! The shapes of a parsed `wc` command line, and their parser.
//!
//! The grammar is the GNU `wc` surface: the count selectors `-c`/`--bytes`,
//! `-m`/`--chars`, `-l`/`--lines`, `-w`/`--words`, `-L`/`--max-line-length`
//! (none selected means the newline/word/byte default), `--total=WHEN`
//! (an unambiguous prefix of `auto`, `always`,
//! `only`, `never`), `--files0-from=F` (NUL-separated operand list, which
//! excludes file operands), the reserved `-h`/`-?`/`--help` short-help
//! switches, and `--` end-of-options — exactly the forms the GNU tool
//! accepts, so a user or script that knows GNU `wc` finds this one
//! familiar.

use alloc::string::String;
use alloc::vec::Vec;

use tairix_util::argv::choose;
use tairix_util::getopt::{Arg, Opt, Order, Scan, Takes};

use crate::error::WcError;

/// Which counts a `wc` run prints, in the fixed GNU output order:
/// lines, words, chars, bytes, max line length.
///
/// The flags are the GNU tool's independent boolean selectors; a
/// two-variant enum per flag would only restate `bool` with extra noise,
/// so the field count mirrors the command surface deliberately.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Selection {
    /// `-l`: newline count.
    pub lines: bool,
    /// `-w`: word count.
    pub words: bool,
    /// `-m`: character count.
    pub chars: bool,
    /// `-c`: byte count.
    pub bytes: bool,
    /// `-L`: maximum display width of a line.
    pub max_line: bool,
}

impl Selection {
    /// The GNU default when no selector is given: lines, words, bytes.
    pub const DEFAULT: Self = Self {
        lines: true,
        words: true,
        chars: false,
        bytes: true,
        max_line: false,
    };

    /// No count selected yet (the state before defaulting).
    const NONE: Self = Self {
        lines: false,
        words: false,
        chars: false,
        bytes: false,
        max_line: false,
    };

    /// How many counts this selection prints per row.
    #[must_use]
    pub fn selected(&self) -> usize {
        usize::from(self.lines)
            + usize::from(self.words)
            + usize::from(self.chars)
            + usize::from(self.bytes)
            + usize::from(self.max_line)
    }
}

/// When the `total` row is printed (`--total`, GNU coreutils).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum TotalMode {
    /// The default: a total row only when more than one operand is named.
    #[default]
    Auto,
    /// Always print the total row.
    Always,
    /// Print *only* the total row (no per-file rows, no `total` label).
    Only,
    /// Never print the total row.
    Never,
}

/// The `--total` modes by the words naming them, in GNU's order.
pub(crate) const MODES: [(&str, TotalMode); 4] = [
    ("auto", TotalMode::Auto),
    ("always", TotalMode::Always),
    ("only", TotalMode::Only),
    ("never", TotalMode::Never),
];

impl TotalMode {
    /// The mode `text` names: its word, or the only one it is a prefix of
    /// (`au` names `auto`; `a` prefixes `auto` and `always` and is refused).
    fn parse(text: &str) -> Result<Self, WcError> {
        choose(&MODES, text).map_err(|miss| WcError::BadTotal(miss, String::from(text)))
    }
}

/// One named input of a `wc` run.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Operand {
    /// The explicit `-` operand: standard input, labelled `-` in the row.
    Dash,
    /// A named file, read through the filesystem seam.
    Path(String),
}

/// A parsed, runnable `wc` invocation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Job {
    /// The counts to print.
    pub select: Selection,
    /// When the total row appears.
    pub total: TotalMode,
    /// `--files0-from`: read the operand list, NUL-separated, from this
    /// file (`-` means standard input). Excludes `operands`.
    pub files0_from: Option<String>,
    /// The command-line operands. Empty means "count standard input,
    /// unlabelled" (unless `files0_from` supplies the list).
    pub operands: Vec<Operand>,
}

/// One thing the `wc` tool can do.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Command {
    /// Count the selected quantities of each input.
    Count(Job),
    /// Render `wc`'s own short help (`-h`/`-?`/`--help`) through the same
    /// engine as any other command's short help (plans/APPS.md §4).
    Help,
}

/// What each of `wc`'s options means.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Flag {
    Bytes,
    Chars,
    Lines,
    Words,
    MaxLine,
    Total,
    Files0From,
    Help,
}

/// `wc`'s options, as GNU's `wc` spells them, and the reserved help
/// switches.
const OPTIONS: &[Opt<Flag>] = &[
    Opt::flag('c', &["bytes"], Flag::Bytes),
    Opt::flag('m', &["chars"], Flag::Chars),
    Opt::flag('l', &["lines"], Flag::Lines),
    Opt::flag('w', &["words"], Flag::Words),
    Opt::flag('L', &["max-line-length"], Flag::MaxLine),
    Opt::long(&["total"], Takes::Value, Flag::Total),
    Opt::long(&["files0-from"], Takes::Value, Flag::Files0From),
    Opt::flag('h', &["help"], Flag::Help),
    Opt::letter('?', Takes::Nothing, Flag::Help),
];

/// Parse `args` (the tool's arguments, excluding the program name) into a
/// [`Command`].
///
/// Read as GNU's `getopt_long` reads it ([`Scan`]): options anywhere among
/// the operands, clustered, a long one by any unambiguous prefix, `--` ending
/// them, and a lone `-` the labelled standard-input operand. The help
/// switches (`-h`, `-?`, `--help`) answer at once; the selectors accumulate.
///
/// # Errors
///
/// * [`WcError::Option`] — an option `getopt` refuses, in its words.
/// * [`WcError::BadTotal`] — a `--total` value naming no one mode.
/// * [`WcError::Files0Conflict`] — file operands with `--files0-from`, which
///   GNU refuses.
pub fn parse(args: &[&str]) -> Result<Command, WcError> {
    let mut select = Selection::NONE;
    let mut total = TotalMode::default();
    let mut files0_from: Option<String> = None;
    let mut operands: Vec<Operand> = Vec::new();

    for arg in Scan::new(OPTIONS, args, Order::Permute) {
        match arg.map_err(WcError::Option)? {
            Arg::Operand("-") => operands.push(Operand::Dash),
            Arg::Operand(path) => operands.push(Operand::Path(String::from(path))),
            Arg::Opt(Flag::Help, _) => return Ok(Command::Help),
            Arg::Opt(Flag::Bytes, _) => select.bytes = true,
            Arg::Opt(Flag::Chars, _) => select.chars = true,
            Arg::Opt(Flag::Lines, _) => select.lines = true,
            Arg::Opt(Flag::Words, _) => select.words = true,
            Arg::Opt(Flag::MaxLine, _) => select.max_line = true,
            Arg::Opt(Flag::Total, value) => total = TotalMode::parse(value.unwrap_or_default())?,
            Arg::Opt(Flag::Files0From, value) => files0_from = value.map(String::from),
        }
    }

    if files0_from.is_some() && !operands.is_empty() {
        return Err(WcError::Files0Conflict);
    }
    if select == Selection::NONE {
        select = Selection::DEFAULT;
    }
    Ok(Command::Count(Job {
        select,
        total,
        files0_from,
        operands,
    }))
}

#[cfg(test)]
mod tests {
    use alloc::string::{String, ToString};
    use alloc::vec;
    use alloc::vec::Vec;

    use super::{parse, Command, Job, Operand, Selection, TotalMode};
    use crate::error::WcError;
    use tairix_util::argv::WordMiss;

    fn job(
        select: Selection,
        total: TotalMode,
        files0_from: Option<&str>,
        operands: Vec<Operand>,
    ) -> Command {
        Command::Count(Job {
            select,
            total,
            files0_from: files0_from.map(String::from),
            operands,
        })
    }

    fn path(name: &str) -> Operand {
        Operand::Path(String::from(name))
    }

    #[test]
    fn defaults_to_lines_words_bytes_of_stdin() {
        assert_eq!(
            parse(&[]),
            Ok(job(Selection::DEFAULT, TotalMode::Auto, None, vec![]))
        );
    }

    #[test]
    fn selectors_accumulate() {
        let expected = Selection {
            lines: true,
            words: false,
            chars: true,
            bytes: false,
            max_line: true,
        };
        assert_eq!(
            parse(&["-l", "-m", "-L", "f"]),
            Ok(job(expected, TotalMode::Auto, None, vec![path("f")]))
        );
        assert_eq!(
            parse(&["-lmL", "f"]),
            Ok(job(expected, TotalMode::Auto, None, vec![path("f")]))
        );
        assert_eq!(
            parse(&["--lines", "--chars", "--max-line-length", "f"]),
            Ok(job(expected, TotalMode::Auto, None, vec![path("f")]))
        );
    }

    #[test]
    fn repeated_selectors_are_idempotent() {
        assert_eq!(parse(&["-ll", "-l"]), parse(&["-l"]));
    }

    #[test]
    fn dash_is_the_labelled_stdin_operand() {
        assert_eq!(
            parse(&["-", "f"]),
            Ok(job(
                Selection::DEFAULT,
                TotalMode::Auto,
                None,
                vec![Operand::Dash, path("f")]
            ))
        );
    }

    #[test]
    fn total_modes_take_a_word_or_the_one_prefix_naming_it() {
        assert_eq!(
            parse(&["--total=never"]),
            Ok(job(Selection::DEFAULT, TotalMode::Never, None, vec![]))
        );
        assert_eq!(
            parse(&["--total", "only"]),
            Ok(job(Selection::DEFAULT, TotalMode::Only, None, vec![]))
        );
        // An unambiguous prefix is accepted; an ambiguous one is refused.
        assert_eq!(
            parse(&["--total=au"]),
            Ok(job(Selection::DEFAULT, TotalMode::Auto, None, vec![]))
        );
        assert_eq!(
            parse(&["--total=a"]),
            Err(WcError::BadTotal(WordMiss::Ambiguous, String::from("a")))
        );
        assert_eq!(
            parse(&["--total=sometimes"]),
            Err(WcError::BadTotal(
                WordMiss::Unknown,
                String::from("sometimes")
            ))
        );
        assert_eq!(
            refused(&["--total"]),
            "option '--total' requires an argument"
        );
    }

    #[test]
    fn a_refused_mode_is_worded_as_gnu_words_it() {
        assert_eq!(
            alloc::format!("{}", WcError::BadTotal(WordMiss::Unknown, String::from("bogus"))),
            "invalid argument 'bogus' for '--total'\nValid arguments are:\n  - 'auto'\n  - 'always'\n  - 'only'\n  - 'never'"
        );
        assert_eq!(
            parse(&["--total="]),
            Err(WcError::BadTotal(WordMiss::Ambiguous, String::new()))
        );
    }

    #[test]
    fn files0_from_excludes_operands() {
        assert_eq!(
            parse(&["--files0-from=list"]),
            Ok(job(
                Selection::DEFAULT,
                TotalMode::Auto,
                Some("list"),
                vec![]
            ))
        );
        assert_eq!(
            parse(&["--files0-from", "-"]),
            Ok(job(Selection::DEFAULT, TotalMode::Auto, Some("-"), vec![]))
        );
        assert_eq!(
            parse(&["--files0-from=list", "f"]),
            Err(WcError::Files0Conflict)
        );
        assert_eq!(
            refused(&["--files0-from"]),
            "option '--files0-from' requires an argument"
        );
    }

    #[test]
    fn double_dash_ends_options() {
        assert_eq!(
            parse(&["--", "-l"]),
            Ok(job(
                Selection::DEFAULT,
                TotalMode::Auto,
                None,
                vec![path("-l")]
            ))
        );
    }

    #[test]
    fn help_flags_win() {
        assert_eq!(parse(&["-h"]), Ok(Command::Help));
        assert_eq!(parse(&["-?"]), Ok(Command::Help));
        assert_eq!(parse(&["--help"]), Ok(Command::Help));
        assert_eq!(parse(&["-l?"]), Ok(Command::Help));
    }

    /// GNU 9.4's words for each refusal.
    fn refused(args: &[&str]) -> String {
        parse(args).expect_err("refused").to_string()
    }

    #[test]
    fn a_refused_option_is_worded_as_getopt_words_it() {
        assert_eq!(refused(&["--frob"]), "unrecognized option '--frob'");
        assert_eq!(refused(&["-x"]), "invalid option -- 'x'");
        assert_eq!(refused(&["-lx"]), "invalid option -- 'x'");
        assert_eq!(
            refused(&["--total"]),
            "option '--total' requires an argument"
        );
        assert_eq!(
            refused(&["--lines=x"]),
            "option '--lines' doesn't allow an argument"
        );
    }

    /// GNU 9.4 reads `wc --li --max --tot=only f` as
    /// `wc --lines --max-line-length --total=only f`.
    #[test]
    fn a_long_option_may_be_abbreviated() {
        assert_eq!(
            parse(&["--li", "--max", "--tot=only", "f"]),
            Ok(job(
                Selection {
                    lines: true,
                    max_line: true,
                    ..Selection::NONE
                },
                TotalMode::Only,
                None,
                vec![path("f")]
            ))
        );
        assert_eq!(parse(&["--he"]), Ok(Command::Help));
    }

    #[test]
    fn options_and_operands_interleave() {
        assert_eq!(
            parse(&["f", "-l"]),
            Ok(job(
                Selection {
                    lines: true,
                    ..Selection::NONE
                },
                TotalMode::Auto,
                None,
                vec![path("f")]
            ))
        );
    }
}
