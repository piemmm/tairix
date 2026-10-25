//! The `df` command shape and its parser.
//!
//! The option surface follows GNU coreutils `df`: `-a` includes the
//! pseudo/duplicate filesystems the default hides, `-T` adds the type
//! column, `-t`/`-x` filter by filesystem type, `-i` reports inodes,
//! `-P` selects the POSIX portable format, `--total` appends a summary
//! row, and the unit options (`-k`, `-h`, `-H`/`--si`, `-B <size>`)
//! select the reporting scale, the later selection winning as in the
//! GNU tool. `-l` restricts the report to local filesystems (every
//! TAIRiX mount today — the option filters nothing away, exactly as on
//! an all-local Linux machine). Short help is the reserved `-?`/`--help`
//! pair (plans/APPS.md §4); `-h` keeps its GNU meaning
//! (human-readable), exactly as `ls` and `du`.

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt;

use tairix_util::getopt::{Arg, Opt, Order, Refusal, Scan, Takes};
use tairix_util::quote::quote;
use tairix_util::size::{parse_block_size, SizeScale};

/// A parsed `df` invocation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Command {
    /// Render `df`'s own short help (`-?`/`--help`): the `NAME`,
    /// `SYNOPSIS`, and compact `OPTIONS` of its bundled help document,
    /// through the same engine as any other command's short help
    /// (plans/APPS.md §4).
    Help,
    /// Report the mounted filesystems' usage.
    Report(Options),
}

/// Everything a `df` run needs to know, straight from the option grammar.
// The bools mirror GNU `df`'s independent switches one-to-one (the `ls`/`rm`
// Options precedent); folding them into state enums would obscure the
// grammar they transcribe.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Options {
    /// The `file` operands, in command-line order; empty reports every
    /// mounted filesystem.
    pub paths: Vec<String>,
    /// Include the pseudo and duplicate filesystems the default hides
    /// (`-a`).
    pub all: bool,
    /// Add the filesystem-type column (`-T`).
    pub print_type: bool,
    /// Report only filesystems of these types (`-t`, repeatable).
    pub types: Vec<String>,
    /// Exclude filesystems of these types (`-x`, repeatable).
    pub exclude_types: Vec<String>,
    /// Report inode counts instead of block usage (`-i`).
    pub inodes: bool,
    /// POSIX portable format (`-P`): the `1024-blocks`/`Capacity`
    /// header wording.
    pub portability: bool,
    /// Append a summary row labelled `total` (`--total`).
    pub grand_total: bool,
    /// The reporting scale; the default is 1024-byte blocks.
    pub scale: SizeScale,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            paths: Vec::new(),
            all: false,
            print_type: false,
            types: Vec::new(),
            exclude_types: Vec::new(),
            inodes: false,
            portability: false,
            grand_total: false,
            scale: SizeScale::Blocks(1024),
        }
    }
}

/// Why an argument vector was not a valid `df` invocation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ParseError {
    /// An option `getopt` refuses, worded as it words it.
    Option(Refusal),
    /// A `--block-size`/`-B` value that is not a valid GNU size.
    BadBlockSize(String),
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
        }
    }
}

/// What each of `df`'s options means.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Flag {
    All,
    BlockSize,
    Inodes,
    HumanReadable,
    Si,
    Kibibytes,
    Local,
    Portability,
    PrintType,
    Total,
    Type,
    ExcludeType,
    Help,
}

/// `df`'s options, as GNU's `df` spells them, and the reserved help switches.
const OPTIONS: &[Opt<Flag>] = &[
    Opt::flag('a', &["all"], Flag::All),
    Opt::valued('B', &["block-size"], Flag::BlockSize),
    Opt::flag('i', &["inodes"], Flag::Inodes),
    Opt::flag('h', &["human-readable"], Flag::HumanReadable),
    Opt::flag('H', &["si"], Flag::Si),
    Opt::letter('k', Takes::Nothing, Flag::Kibibytes),
    Opt::flag('l', &["local"], Flag::Local),
    Opt::flag('P', &["portability"], Flag::Portability),
    Opt::flag('T', &["print-type"], Flag::PrintType),
    Opt::long(&["total"], Takes::Nothing, Flag::Total),
    Opt::valued('t', &["type"], Flag::Type),
    Opt::valued('x', &["exclude-type"], Flag::ExcludeType),
    Opt::flag('?', &["help"], Flag::Help),
];

/// Parse an argument vector (without the program name).
///
/// Read as GNU's `getopt_long` reads it ([`Scan`]): options anywhere among
/// the operands, clustered, a value attached (`-tarxfs`, `--type=arxfs`) or
/// the next argument, a long option by any unambiguous prefix, `--` ending
/// them. The help switches (`-?`, `--help`) answer at once. The later unit
/// selection wins, exactly as in the GNU tool; `-t` and `-x` accumulate.
///
/// # Errors
///
/// * [`ParseError::Option`] — an option `getopt` refuses, in its words.
/// * [`ParseError::BadBlockSize`] — a `-B` value that is not a GNU size.
pub fn parse(args: &[&str]) -> Result<Command, ParseError> {
    let mut options = Options::default();
    for arg in Scan::new(OPTIONS, args, Order::Permute) {
        match arg.map_err(ParseError::Option)? {
            Arg::Operand(path) => options.paths.push(path.to_string()),
            Arg::Opt(Flag::Help, _) => return Ok(Command::Help),
            Arg::Opt(flag, value) => options.set(flag, value.unwrap_or_default())?,
        }
    }
    Ok(Command::Report(options))
}

impl Options {
    /// Apply the option `flag` names, with the value it took.
    fn set(&mut self, flag: Flag, value: &str) -> Result<(), ParseError> {
        match flag {
            Flag::All => self.all = true,
            Flag::BlockSize => {
                self.scale = parse_block_size(value)
                    .ok_or_else(|| ParseError::BadBlockSize(value.to_string()))?;
            }
            Flag::Inodes => self.inodes = true,
            Flag::HumanReadable => self.scale = SizeScale::HumanBinary,
            Flag::Si => self.scale = SizeScale::HumanDecimal,
            Flag::Kibibytes => self.scale = SizeScale::Blocks(1024),
            Flag::Local | Flag::Help => {}
            Flag::Portability => self.portability = true,
            Flag::PrintType => self.print_type = true,
            Flag::Total => self.grand_total = true,
            Flag::Type => self.types.push(value.to_string()),
            Flag::ExcludeType => self.exclude_types.push(value.to_string()),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{parse, Command, Options, ParseError, SizeScale};
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
        assert_eq!(refused(&["-t"]), "option requires an argument -- 't'");
        assert_eq!(
            refused(&["--block-size"]),
            "option '--block-size' requires an argument"
        );
        assert_eq!(
            refused(&["--all=x"]),
            "option '--all' doesn't allow an argument"
        );
    }

    /// GNU 9.4 reads `df --inod --print /` as `df --inodes --print-type /`.
    #[test]
    fn a_long_option_may_be_abbreviated() {
        let options = report(&["--inod", "--print", "/"]);
        assert!(options.inodes && options.print_type);
        assert_eq!(
            refused(&["--p"]),
            "option '--p' is ambiguous; possibilities: '--portability' '--print-type'"
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
    }

    #[test]
    fn h_is_human_readable_not_help() {
        // As in the GNU tool (and `ls`/`du`), `-h` scales sizes; short
        // help is `-?`/`--help`.
        assert_eq!(report(&["-h"]).scale, SizeScale::HumanBinary);
        assert_eq!(report(&["-H"]).scale, SizeScale::HumanDecimal);
        assert_eq!(report(&["--si"]).scale, SizeScale::HumanDecimal);
        assert_eq!(report(&["-h", "-k"]).scale, SizeScale::Blocks(1024));
    }

    #[test]
    fn flags_cluster_and_operands_interleave() {
        let options = report(&["-aTP", "/Users", "-i", "/Storage"]);
        assert!(options.all);
        assert!(options.print_type);
        assert!(options.portability);
        assert!(options.inodes);
        assert_eq!(
            options.paths,
            vec!["/Users".to_string(), "/Storage".to_string()]
        );
    }

    #[test]
    fn type_filters_accumulate() {
        let options = report(&[
            "-t",
            "arxfs",
            "-tmemfs",
            "--exclude-type=fat32",
            "-x",
            "ext4",
        ]);
        assert_eq!(
            options.types,
            vec!["arxfs".to_string(), "memfs".to_string()]
        );
        assert_eq!(
            options.exclude_types,
            vec!["fat32".to_string(), "ext4".to_string()]
        );
    }

    #[test]
    fn block_size_parses_the_gnu_grammar() {
        assert_eq!(report(&["-B", "4K"]).scale, SizeScale::Blocks(4096));
        assert_eq!(report(&["-B4K"]).scale, SizeScale::Blocks(4096));
        assert_eq!(
            report(&["--block-size=1MiB"]).scale,
            SizeScale::Blocks(1024 * 1024)
        );
    }

    #[test]
    fn local_is_accepted_and_filters_nothing() {
        // Every TAIRiX mount is local, so `-l`/`--local` is the identity
        // filter — accepted for GNU script compatibility.
        assert_eq!(report(&["-l"]), Options::default());
        assert_eq!(report(&["--local"]), Options::default());
    }

    #[test]
    fn malformed_values_fail_closed() {
        assert_eq!(
            parse(&["-B", "banana"]),
            Err(ParseError::BadBlockSize("banana".to_string()))
        );
        assert_eq!(refused(&["-B"]), "option requires an argument -- 'B'");
        assert_eq!(refused(&["-t"]), "option requires an argument -- 't'");
        assert_eq!(refused(&["--nonsense"]), "unrecognized option '--nonsense'");
        assert_eq!(refused(&["-z"]), "invalid option -- 'z'");
    }

    #[test]
    fn double_dash_ends_options() {
        let options = report(&["--", "-a"]);
        assert!(!options.all);
        assert_eq!(options.paths, vec!["-a".to_string()]);
    }
}
