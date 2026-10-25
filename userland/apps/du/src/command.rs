//! The `du` command shape and its parser.
//!
//! The option surface follows GNU coreutils `du`: `-a` reports every
//! file, `-s` only the operands, `-c` appends a grand total, `-d`
//! bounds the reported depth, `-S` excludes subdirectories from a
//! directory's own row, `-l` counts a multiply-named file once per name
//! instead of once, `--apparent-size`/`-b` measure apparent bytes
//! instead of allocated storage, and the unit options (`-k`, `-m`,
//! `-h`, `--si`, `-B <size>`, `-b`) select the reporting scale, the
//! later selection winning as in the GNU tool. `-0` NUL-terminates
//! rows. Short help is the reserved `-?`/`--help` pair (plans/APPS.md
//! §4); `-h` keeps its GNU meaning (human-readable), exactly as `ls`.

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt;

use tairix_util::getopt::{Arg, Opt, Order, Refusal, Scan, Takes};
use tairix_util::quote::quote;
use tairix_util::size::{parse_block_size, SizeScale};

/// A parsed `du` invocation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Command {
    /// Render `du`'s own short help (`-?`/`--help`): the `NAME`,
    /// `SYNOPSIS`, and compact `OPTIONS` of its bundled help document,
    /// through the same engine as any other command's short help
    /// (plans/APPS.md §4).
    Help,
    /// Walk the operands and report their usage.
    Report(Options),
}

/// Everything a `du` run needs to know, straight from the option grammar.
// The bools mirror GNU `du`'s independent switches one-to-one (the `ls`/`rm`
// Options precedent); folding them into state enums would obscure the
// grammar they transcribe.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Options {
    /// The path operands, in command-line order; empty means the current
    /// directory (`.`), applied by the client.
    pub paths: Vec<String>,
    /// Report an entry for every file, not just directories (`-a`).
    pub all: bool,
    /// Report only each operand's total (`-s`).
    pub summarize: bool,
    /// Append a grand-total row (`-c`).
    pub grand_total: bool,
    /// Measure apparent byte lengths instead of allocated on-disk
    /// storage (`--apparent-size`, `-b`).
    pub apparent_size: bool,
    /// The reporting scale; the default is 1024-byte blocks.
    pub scale: SizeScale,
    /// Report directories at most this many levels below an operand
    /// (`-d`); `0` reports the operands only.
    pub max_depth: Option<u64>,
    /// A directory's row excludes its subdirectories (`-S`).
    pub separate_dirs: bool,
    /// Count a multiply-named file once per name rather than once
    /// (`-l`/`--count-links`), the GNU opt-out of the deduplication.
    pub count_links: bool,
    /// End each row with NUL instead of newline (`-0`).
    pub null_terminated: bool,
    /// Which symbolic links the walk follows (`-P`, `-H`/`-D`, `-L`).
    pub dereference: Dereference,
}

/// Which symbolic links `du` follows; the last option given wins.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Dereference {
    /// None: a link counts as itself (`-P`, `--no-dereference`, the default).
    Never,
    /// Those named on the command line (`-H`, `-D`, `--dereference-args`).
    Operands,
    /// Every one met (`-L`, `--dereference`).
    Always,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            paths: Vec::new(),
            all: false,
            summarize: false,
            grand_total: false,
            apparent_size: false,
            scale: SizeScale::Blocks(1024),
            max_depth: None,
            separate_dirs: false,
            count_links: false,
            null_terminated: false,
            dereference: Dereference::Never,
        }
    }
}

/// Why an argument vector was not a valid `du` invocation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ParseError {
    /// An option `getopt` refuses, worded as it words it.
    Option(Refusal),
    /// A `--block-size`/`-B` value that is not a valid GNU size.
    BadBlockSize(String),
    /// A `--max-depth`/`-d` value that is not a non-negative integer.
    BadMaxDepth(String),
    /// `-s` combined with `-a`: the two select contradictory row sets.
    SummarizeConflictsWithAll,
    /// `-s` combined with `-d`: `-s` already means `--max-depth=0`.
    SummarizeConflictsWithDepth,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Option(refusal) => write!(f, "{refusal}"),
            Self::BadBlockSize(value) => write!(
                f,
                "invalid --block-size argument {value}",
                value = quote(value)
            ),
            Self::BadMaxDepth(value) => {
                write!(f, "invalid maximum depth {value}", value = quote(value))
            }
            Self::SummarizeConflictsWithAll => {
                write!(f, "cannot both summarize and show all entries")
            }
            Self::SummarizeConflictsWithDepth => {
                write!(f, "summarizing conflicts with --max-depth")
            }
        }
    }
}

/// What each of `du`'s options means.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Flag {
    All,
    ApparentSize,
    BlockSize,
    Bytes,
    CountLinks,
    Dereference,
    DereferenceArgs,
    HumanReadable,
    Si,
    MaxDepth,
    Null,
    NoDereference,
    SeparateDirs,
    Summarize,
    Total,
    Kibibytes,
    Mebibytes,
    Help,
}

/// `du`'s options, as GNU's `du` spells them, and the reserved help switches.
const OPTIONS: &[Opt<Flag>] = &[
    Opt::flag('a', &["all"], Flag::All),
    Opt::long(&["apparent-size"], Takes::Nothing, Flag::ApparentSize),
    Opt::valued('B', &["block-size"], Flag::BlockSize),
    Opt::flag('b', &["bytes"], Flag::Bytes),
    Opt::flag('l', &["count-links"], Flag::CountLinks),
    Opt::flag('L', &["dereference"], Flag::Dereference),
    Opt::flag('D', &["dereference-args"], Flag::DereferenceArgs),
    Opt::letter('H', Takes::Nothing, Flag::DereferenceArgs),
    Opt::flag('h', &["human-readable"], Flag::HumanReadable),
    Opt::long(&["si"], Takes::Nothing, Flag::Si),
    Opt::valued('d', &["max-depth"], Flag::MaxDepth),
    Opt::flag('0', &["null"], Flag::Null),
    Opt::flag('P', &["no-dereference"], Flag::NoDereference),
    Opt::flag('S', &["separate-dirs"], Flag::SeparateDirs),
    Opt::flag('s', &["summarize"], Flag::Summarize),
    Opt::flag('c', &["total"], Flag::Total),
    Opt::letter('k', Takes::Nothing, Flag::Kibibytes),
    Opt::letter('m', Takes::Nothing, Flag::Mebibytes),
    Opt::flag('?', &["help"], Flag::Help),
];

/// Parse an argument vector (without the program name).
///
/// Read as GNU's `getopt_long` reads it ([`Scan`]): options anywhere among
/// the operands, clustered, a value attached (`-B1M`, `--max-depth=2`) or
/// the next argument, a long option by any unambiguous prefix, `--` ending
/// them. The help switches (`-?`, `--help`) answer at once. The later unit
/// selection, and the later link option, wins, exactly as in the GNU tool.
///
/// # Errors
///
/// * [`ParseError::Option`] — an option `getopt` refuses, in its words.
/// * [`ParseError::BadBlockSize`] — a `-B` value that is not a GNU size.
/// * [`ParseError::BadMaxDepth`] — a `-d` value that is not a non-negative
///   integer.
/// * [`ParseError::SummarizeConflictsWithAll`] — `-s` with `-a`.
/// * [`ParseError::SummarizeConflictsWithDepth`] — `-s` with `-d`.
pub fn parse(args: &[&str]) -> Result<Command, ParseError> {
    let mut options = Options::default();
    for arg in Scan::new(OPTIONS, args, Order::Permute) {
        match arg.map_err(ParseError::Option)? {
            Arg::Operand(path) => options.paths.push(path.to_string()),
            Arg::Opt(Flag::Help, _) => return Ok(Command::Help),
            Arg::Opt(flag, value) => options.set(flag, value.unwrap_or_default())?,
        }
    }
    if options.summarize && options.all {
        return Err(ParseError::SummarizeConflictsWithAll);
    }
    // `-d` is the only option that sets a depth during the scan, so a depth
    // already present is exactly the `-s`/`-d` conflict.
    if options.summarize && options.max_depth.is_some() {
        return Err(ParseError::SummarizeConflictsWithDepth);
    }
    if options.summarize {
        options.max_depth = Some(0);
    }
    Ok(Command::Report(options))
}

impl Options {
    /// Apply the option `flag` names, with the value it took.
    fn set(&mut self, flag: Flag, value: &str) -> Result<(), ParseError> {
        match flag {
            Flag::All => self.all = true,
            Flag::ApparentSize => self.apparent_size = true,
            Flag::BlockSize => {
                self.scale = parse_block_size(value)
                    .ok_or_else(|| ParseError::BadBlockSize(value.to_string()))?;
            }
            Flag::Bytes => {
                self.apparent_size = true;
                self.scale = SizeScale::Blocks(1);
            }
            Flag::CountLinks => self.count_links = true,
            Flag::Dereference => self.dereference = Dereference::Always,
            Flag::DereferenceArgs => self.dereference = Dereference::Operands,
            Flag::HumanReadable => self.scale = SizeScale::HumanBinary,
            Flag::Si => self.scale = SizeScale::HumanDecimal,
            Flag::MaxDepth => {
                self.max_depth = Some(
                    value
                        .parse()
                        .map_err(|_| ParseError::BadMaxDepth(value.to_string()))?,
                );
            }
            Flag::Null => self.null_terminated = true,
            Flag::NoDereference => self.dereference = Dereference::Never,
            Flag::SeparateDirs => self.separate_dirs = true,
            Flag::Summarize => self.summarize = true,
            Flag::Total => self.grand_total = true,
            Flag::Kibibytes => self.scale = SizeScale::Blocks(1024),
            Flag::Mebibytes => self.scale = SizeScale::Blocks(1024 * 1024),
            Flag::Help => {}
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{parse, Command, Dereference, Options, ParseError, SizeScale};
    use alloc::string::{String, ToString};
    use alloc::vec;

    /// GNU 9.4's words for each refusal.
    fn refused(args: &[&str]) -> String {
        parse(args).expect_err("refused").to_string()
    }

    #[test]
    fn a_refused_option_is_worded_as_getopt_words_it() {
        assert_eq!(refused(&["-Q"]), "invalid option -- 'Q'");
        assert_eq!(refused(&["--frob"]), "unrecognized option '--frob'");
        assert_eq!(refused(&["-d"]), "option requires an argument -- 'd'");
        assert_eq!(
            refused(&["--max-depth"]),
            "option '--max-depth' requires an argument"
        );
        assert_eq!(
            refused(&["--summarize=x"]),
            "option '--summarize' doesn't allow an argument"
        );
    }

    /// GNU 9.4 reads `du --summ --apparent` as
    /// `du --summarize --apparent-size`.
    #[test]
    fn a_long_option_may_be_abbreviated() {
        let options = report(&["--summ", "--apparent", "docs"]);
        assert!(options.summarize && options.apparent_size);
        assert_eq!(
            refused(&["--deref"]),
            "option '--deref' is ambiguous; possibilities: '--dereference' '--dereference-args'"
        );
    }

    fn report(args: &[&str]) -> Options {
        match parse(args) {
            Ok(Command::Report(options)) => options,
            other => panic!("expected a report command, got {other:?}"),
        }
    }

    #[test]
    fn defaults_are_the_gnu_defaults() {
        let options = report(&[]);
        assert_eq!(options, Options::default());
        assert_eq!(options.scale, SizeScale::Blocks(1024));
    }

    #[test]
    fn help_switches_win() {
        assert_eq!(parse(&["-?"]), Ok(Command::Help));
        assert_eq!(parse(&["--help"]), Ok(Command::Help));
        assert_eq!(parse(&["-a?", "x"]), Ok(Command::Help));
    }

    #[test]
    fn h_is_human_readable_not_help() {
        // As in the GNU tool (and `ls`), `-h` scales sizes; short help is
        // `-?`/`--help`.
        assert_eq!(report(&["-h"]).scale, SizeScale::HumanBinary);
        assert_eq!(report(&["--human-readable"]).scale, SizeScale::HumanBinary);
        assert_eq!(report(&["--si"]).scale, SizeScale::HumanDecimal);
    }

    #[test]
    fn flags_cluster_and_operands_interleave() {
        let options = report(&["-acS0", "docs", "-k", "notes"]);
        assert!(options.all);
        assert!(options.grand_total);
        assert!(options.separate_dirs);
        assert!(options.null_terminated);
        assert_eq!(options.scale, SizeScale::Blocks(1024));
        assert_eq!(options.paths, vec!["docs".to_string(), "notes".to_string()]);
    }

    #[test]
    fn count_links_opts_out_of_the_deduplication() {
        assert!(!report(&[]).count_links, "deduplication is the default");
        assert!(report(&["-l"]).count_links);
        assert!(report(&["--count-links"]).count_links);
        assert!(report(&["-al"]).count_links);
    }

    #[test]
    fn link_options_choose_the_links_followed_and_the_last_wins() {
        assert_eq!(report(&[]).dereference, Dereference::Never);
        for (args, wanted) in [
            (&["-L"][..], Dereference::Always),
            (&["--dereference"][..], Dereference::Always),
            (&["-H"][..], Dereference::Operands),
            (&["-D"][..], Dereference::Operands),
            (&["--dereference-args"][..], Dereference::Operands),
            (&["-L", "-P"][..], Dereference::Never),
            (&["-P", "--no-dereference", "-H"][..], Dereference::Operands),
            (&["-aL"][..], Dereference::Always),
        ] {
            assert_eq!(report(args).dereference, wanted, "{args:?}");
        }
    }

    #[test]
    fn unit_selections_apply_the_last_one() {
        assert_eq!(report(&["-k", "-m"]).scale, SizeScale::Blocks(1024 * 1024));
        assert_eq!(report(&["-h", "-k"]).scale, SizeScale::Blocks(1024));
        assert_eq!(report(&["-B", "4K"]).scale, SizeScale::Blocks(4096));
        assert_eq!(report(&["-B4K"]).scale, SizeScale::Blocks(4096));
        assert_eq!(
            report(&["--block-size=1MiB"]).scale,
            SizeScale::Blocks(1024 * 1024)
        );
    }

    #[test]
    fn bytes_selects_apparent_size_in_single_bytes() {
        let options = report(&["-b"]);
        assert!(options.apparent_size);
        assert_eq!(options.scale, SizeScale::Blocks(1));
        // `--apparent-size` alone keeps the block scale.
        let options = report(&["--apparent-size"]);
        assert!(options.apparent_size);
        assert_eq!(options.scale, SizeScale::Blocks(1024));
    }

    #[test]
    fn depth_and_summarize_follow_the_gnu_rules() {
        assert_eq!(report(&["-d", "2"]).max_depth, Some(2));
        assert_eq!(report(&["-d2"]).max_depth, Some(2));
        assert_eq!(report(&["--max-depth=0"]).max_depth, Some(0));
        // `-s` is `--max-depth=0`.
        assert_eq!(report(&["-s"]).max_depth, Some(0));
        assert_eq!(
            parse(&["-s", "-a"]),
            Err(ParseError::SummarizeConflictsWithAll)
        );
        assert_eq!(
            parse(&["-s", "-d1"]),
            Err(ParseError::SummarizeConflictsWithDepth)
        );
    }

    #[test]
    fn malformed_values_fail_closed() {
        assert_eq!(
            parse(&["-B", "banana"]),
            Err(ParseError::BadBlockSize("banana".to_string()))
        );
        assert_eq!(refused(&["-B"]), "option requires an argument -- 'B'");
        assert_eq!(
            refused(&["--block-size"]),
            "option '--block-size' requires an argument"
        );
        assert_eq!(
            parse(&["-d", "-1"]),
            Err(ParseError::BadMaxDepth("-1".to_string()))
        );
        assert_eq!(refused(&["--nonsense"]), "unrecognized option '--nonsense'");
        assert_eq!(refused(&["-x"]), "invalid option -- 'x'");
    }

    #[test]
    fn double_dash_ends_options_and_dash_is_an_operand() {
        let options = report(&["--", "-a"]);
        assert!(!options.all);
        assert_eq!(options.paths, vec!["-a".to_string()]);
        let options = report(&["-"]);
        assert_eq!(options.paths, vec!["-".to_string()]);
    }
}
