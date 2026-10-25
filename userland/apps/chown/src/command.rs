//! The parsed shape of a `chown` command line.

use alloc::string::String;
use alloc::vec::Vec;

use tairix_util::getopt::{Arg, Opt, Order, Refusal, Scan, Takes};

use crate::error::ChownError;

/// Which nodes are reported on standard output.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Verbosity {
    /// None (the default).
    None,
    /// Only a node whose ownership changed (`-c`).
    Changes,
    /// Every node processed (`-v`).
    All,
}

/// What a symbolic link stands for where a change meets one.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Links {
    /// What it names (`--dereference`).
    Referent,
    /// The link itself (`-h`, `--no-dereference`).
    Itself,
}

/// Which link operands a recursive change follows into a directory.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Traverse {
    /// None (`-P`, the default).
    Never,
    /// One that names a directory (`-H`).
    Operands,
}

/// The options of one `chown` run.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Options {
    /// Change a directory's contents too (`-R`).
    pub recursive: bool,
    /// The reporting policy (`-c`, `-v`; the later wins).
    pub verbosity: Verbosity,
    /// Report no node's failure (`-f`); the run still fails.
    pub quiet: bool,
    /// `--dereference` or `-h`, the later winning; [`None`] leaves GNU's
    /// default, which [`Options::links`] answers.
    pub dereference: Option<Links>,
    /// `-H` or `-P`, the later winning.
    pub traverse: Traverse,
    /// Refuse a recursive change of the view root (`--preserve-root`); off
    /// by default, as GNU's is.
    pub preserve_root: bool,
}

impl Options {
    /// The options of a bare `chown`.
    pub const DEFAULT: Self = Self {
        recursive: false,
        verbosity: Verbosity::None,
        quiet: false,
        dereference: None,
        traverse: Traverse::Never,
        preserve_root: false,
    };

    /// What a link counts as in this run: as asked, else GNU's default — what
    /// it names, or the link itself when the change is recursive.
    #[must_use]
    pub const fn links(self) -> Links {
        match self.dereference {
            Some(links) => links,
            None if self.recursive => Links::Itself,
            None => Links::Referent,
        }
    }
}

/// What a change gives its files.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Target {
    /// An owner operand as written, read against the directories before any
    /// file is touched.
    Spec(String),
    /// The owner and group of what `--reference=RFILE` names.
    Reference(String),
}

/// One thing the `chown` tool can do.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Command {
    /// Give each of `files`, in order, what `target` names.
    Change {
        /// The options.
        options: Options,
        /// What the files are given.
        target: Target,
        /// `--from`'s operand as written: only a node it matches is changed.
        from: Option<String>,
        /// The files, at least one.
        files: Vec<String>,
    },
    /// Print the short help (`-?`, `--help`).
    Help,
    /// Print the tool's name and version (`--version`).
    Version,
}

/// What each option means.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Flag {
    Changes,
    Quiet,
    From,
    Dereference,
    NoDereference,
    NoPreserveRoot,
    PreserveRoot,
    Reference,
    Verbose,
    Recursive,
    FollowOperands,
    FollowEvery,
    FollowNone,
    Help,
    Version,
}

/// `chown`'s options in the order GNU's manual lists them, then the help and
/// version switches. `-h` is GNU's `--no-dereference`, so help is `-?`.
const OPTIONS: &[Opt<Flag>] = &[
    Opt::flag('c', &["changes"], Flag::Changes),
    Opt::flag('f', &["silent", "quiet"], Flag::Quiet),
    Opt::long(&["from"], Takes::Value, Flag::From),
    Opt::long(&["dereference"], Takes::Nothing, Flag::Dereference),
    Opt::flag('h', &["no-dereference"], Flag::NoDereference),
    Opt::long(&["no-preserve-root"], Takes::Nothing, Flag::NoPreserveRoot),
    Opt::long(&["preserve-root"], Takes::Nothing, Flag::PreserveRoot),
    Opt::long(&["reference"], Takes::Value, Flag::Reference),
    Opt::flag('v', &["verbose"], Flag::Verbose),
    Opt::flag('R', &["recursive"], Flag::Recursive),
    Opt::letter('H', Takes::Nothing, Flag::FollowOperands),
    Opt::letter('L', Takes::Nothing, Flag::FollowEvery),
    Opt::letter('P', Takes::Nothing, Flag::FollowNone),
    Opt::long(&["help"], Takes::Nothing, Flag::Help),
    Opt::letter('?', Takes::Nothing, Flag::Help),
    Opt::long(&["version"], Takes::Nothing, Flag::Version),
];

/// Parse `args` (the arguments, the program's word excluded) as GNU's
/// `getopt_long` reads `chown`'s: options anywhere among the operands, a long
/// one by any unambiguous prefix, `--` ending them, and the help and version
/// switches answering at once. The first operand is the owner unless
/// `--reference` names one; the rest are files.
///
/// # Errors
///
/// [`ChownError::Option`] for an option `getopt` refuses,
/// [`ChownError::MissingOperand`] or [`ChownError::MissingFile`] for too few
/// operands, and [`ChownError::EveryLink`] for `-L` under `-R`.
pub fn parse(args: &[&str]) -> Result<Command, ChownError> {
    let mut options = Options::DEFAULT;
    let mut every_link = false;
    let mut from = None;
    let mut reference = None;
    let mut operands = Vec::new();
    for arg in Scan::new(OPTIONS, args, Order::Permute) {
        match arg.map_err(ChownError::Option)? {
            Arg::Operand(operand) => operands.push(String::from(operand)),
            Arg::Opt(Flag::Help, _) => return Ok(Command::Help),
            Arg::Opt(Flag::Version, _) => return Ok(Command::Version),
            Arg::Opt(Flag::Changes, _) => options.verbosity = Verbosity::Changes,
            Arg::Opt(Flag::Verbose, _) => options.verbosity = Verbosity::All,
            Arg::Opt(Flag::Quiet, _) => options.quiet = true,
            Arg::Opt(Flag::Recursive, _) => options.recursive = true,
            Arg::Opt(Flag::Dereference, _) => options.dereference = Some(Links::Referent),
            Arg::Opt(Flag::NoDereference, _) => options.dereference = Some(Links::Itself),
            Arg::Opt(Flag::NoPreserveRoot, _) => options.preserve_root = false,
            Arg::Opt(Flag::PreserveRoot, _) => options.preserve_root = true,
            Arg::Opt(Flag::FollowOperands, _) => {
                options.traverse = Traverse::Operands;
                every_link = false;
            }
            Arg::Opt(Flag::FollowNone, _) => {
                options.traverse = Traverse::Never;
                every_link = false;
            }
            Arg::Opt(Flag::FollowEvery, _) => every_link = true,
            Arg::Opt(Flag::From, value) => from = Some(value_of(value, "from")?),
            Arg::Opt(Flag::Reference, value) => reference = Some(value_of(value, "reference")?),
        }
    }
    if every_link && options.recursive {
        return Err(ChownError::EveryLink);
    }
    let mut operands = operands.into_iter();
    let target = match reference {
        Some(path) => Target::Reference(path),
        None => Target::Spec(operands.next().ok_or(ChownError::MissingOperand)?),
    };
    let files: Vec<String> = operands.collect();
    if files.is_empty() {
        return Err(match target {
            Target::Spec(spec) => ChownError::MissingFile(spec),
            Target::Reference(_) => ChownError::MissingOperand,
        });
    }
    Ok(Command::Change {
        options,
        target,
        from,
        files,
    })
}

/// The value [`Scan`] hands a valued option, refused as `getopt` refuses a
/// missing one should it ever hand none.
fn value_of(value: Option<&str>, name: &'static str) -> Result<String, ChownError> {
    value
        .map(String::from)
        .ok_or(ChownError::Option(Refusal::MissingLongValue(name)))
}

#[cfg(test)]
mod tests {
    use super::{parse, Command, Links, Options, Target, Traverse, Verbosity};
    use crate::error::ChownError;
    use alloc::string::{String, ToString};
    use alloc::vec::Vec;

    /// What `args` asks to change, or a panic naming what it parsed to.
    fn change(args: &[&str]) -> (Options, Target, Option<String>, Vec<String>) {
        match parse(args) {
            Ok(Command::Change {
                options,
                target,
                from,
                files,
            }) => (options, target, from, files),
            other => panic!("{args:?} parsed to {other:?}"),
        }
    }

    fn options(args: &[&str]) -> Options {
        change(args).0
    }

    /// The refusal `args` meets, as GNU's `chown` words it.
    fn refused(args: &[&str]) -> String {
        parse(args).expect_err("refused").to_string()
    }

    #[test]
    fn each_option_sets_what_gnu_s_manual_says() {
        assert_eq!(options(&["0", "f"]), Options::DEFAULT);
        assert_eq!(change(&["0", "f"]).1, Target::Spec(String::from("0")));
        for flag in ["-c", "--changes"] {
            assert_eq!(
                options(&[flag, "0", "f"]).verbosity,
                Verbosity::Changes,
                "{flag}"
            );
        }
        for flag in ["-v", "--verbose"] {
            assert_eq!(
                options(&[flag, "0", "f"]).verbosity,
                Verbosity::All,
                "{flag}"
            );
        }
        for flag in ["-f", "--silent", "--quiet"] {
            assert!(options(&[flag, "0", "f"]).quiet, "{flag}");
        }
        for flag in ["-R", "--recursive"] {
            assert!(options(&[flag, "0", "f"]).recursive, "{flag}");
        }
        assert_eq!(
            options(&["--dereference", "0", "f"]).dereference,
            Some(Links::Referent)
        );
        for flag in ["-h", "--no-dereference"] {
            assert_eq!(
                options(&[flag, "0", "f"]).dereference,
                Some(Links::Itself),
                "{flag}"
            );
        }
        assert_eq!(options(&["-H", "0", "f"]).traverse, Traverse::Operands);
        assert_eq!(options(&["-P", "0", "f"]).traverse, Traverse::Never);
    }

    /// GNU's manual: of `-H`, `-L` and `-P` only the final one takes effect;
    /// the later of `-c` and `-v`, and of `--dereference` and `-h`, wins too.
    #[test]
    fn the_later_of_each_pair_wins() {
        assert_eq!(
            options(&["-v", "-c", "0", "f"]).verbosity,
            Verbosity::Changes
        );
        assert_eq!(options(&["-c", "-v", "0", "f"]).verbosity, Verbosity::All);
        assert_eq!(
            options(&["-h", "--dereference", "0", "f"]).links(),
            Links::Referent
        );
        assert_eq!(
            options(&["--dereference", "-h", "0", "f"]).links(),
            Links::Itself
        );
        assert_eq!(
            options(&["-R", "-H", "-P", "0", "f"]).traverse,
            Traverse::Never
        );
        assert_eq!(
            options(&["-R", "-L", "-H", "0", "f"]).traverse,
            Traverse::Operands
        );
        assert_eq!(
            options(&["-R", "-L", "-P", "0", "f"]).traverse,
            Traverse::Never
        );
    }

    /// GNU's manual: `--dereference` is the default when not operating
    /// recursively, and `-R` changes a link it meets itself.
    #[test]
    fn a_link_is_what_it_names_unless_the_change_is_recursive() {
        assert_eq!(Options::DEFAULT.links(), Links::Referent);
        assert_eq!(options(&["-R", "0", "f"]).links(), Links::Itself);
        assert_eq!(
            options(&["-R", "--dereference", "0", "f"]).links(),
            Links::Referent
        );
    }

    /// A walk never follows a link it meets, so `-L` could only be honoured
    /// in part; without `-R` it has no effect, as GNU's manual says.
    #[test]
    fn l_is_refused_under_recursive_and_ignored_without() {
        for args in [
            &["-R", "-L", "0", "d"][..],
            &["-L", "-R", "0", "d"],
            &["-RH", "-L", "0", "d"],
        ] {
            assert_eq!(parse(args), Err(ChownError::EveryLink), "{args:?}");
        }
        assert_eq!(options(&["-L", "0", "f"]), Options::DEFAULT);
        assert!(refused(&["-RL", "0", "d"]).starts_with("-L is not available: "));
    }

    #[test]
    fn from_and_reference_take_a_value_attached_or_following() {
        assert_eq!(
            change(&["--from=alice:staff", "0", "f"]).2.as_deref(),
            Some("alice:staff")
        );
        assert_eq!(
            change(&["--from", ":staff", "0", "f"]).2.as_deref(),
            Some(":staff")
        );
        let (_, target, from, files) = change(&["--reference=r", "f", "g"]);
        assert_eq!(target, Target::Reference(String::from("r")));
        assert_eq!(from, None);
        assert_eq!(files, ["f", "g"]);
        assert_eq!(
            change(&["--reference", "r", "f"]).1,
            Target::Reference(String::from("r"))
        );
        assert_eq!(
            refused(&["0", "f", "--from"]),
            "option '--from' requires an argument"
        );
    }

    #[test]
    fn help_and_version_answer_at_once_and_h_is_no_help() {
        for args in [&["-?"][..], &["--help"], &["--he"], &["-R?", "0", "f"]] {
            assert_eq!(parse(args), Ok(Command::Help), "{args:?}");
        }
        assert_eq!(parse(&["--version"]), Ok(Command::Version));
        assert_eq!(parse(&["--vers"]), Ok(Command::Version));
        assert_eq!(parse(&["-h"]), Err(ChownError::MissingOperand));
    }

    /// GNU 9.4's words for too few operands.
    #[test]
    fn missing_operands_are_named_as_gnu_names_them() {
        assert_eq!(refused(&[]), "missing operand");
        assert_eq!(refused(&["-R"]), "missing operand");
        assert_eq!(refused(&["alice"]), "missing operand after 'alice'");
        assert_eq!(refused(&["-R", "alice"]), "missing operand after 'alice'");
        assert_eq!(refused(&["--reference=r"]), "missing operand");
    }

    /// GNU 9.4's words for each refusal.
    #[test]
    fn a_refused_option_is_worded_as_getopt_words_it() {
        assert_eq!(refused(&["-x", "0", "f"]), "invalid option -- 'x'");
        assert_eq!(refused(&["-Rx", "0", "f"]), "invalid option -- 'x'");
        assert_eq!(refused(&["-r", "0", "f"]), "invalid option -- 'r'");
        assert_eq!(
            refused(&["--frob", "0", "f"]),
            "unrecognized option '--frob'"
        );
        assert_eq!(
            refused(&["--verbose=1", "0", "f"]),
            "option '--verbose' doesn't allow an argument"
        );
        assert_eq!(
            refused(&["--re", "0", "f"]),
            "option '--re' is ambiguous; possibilities: '--reference' '--recursive'"
        );
        assert_eq!(
            refused(&["--ver"]),
            "option '--ver' is ambiguous; possibilities: '--verbose' '--version'"
        );
    }

    #[test]
    fn a_long_option_may_be_abbreviated() {
        assert_eq!(
            options(&["--rec", "--sil", "--deref", "0", "d"]),
            Options {
                recursive: true,
                quiet: true,
                dereference: Some(Links::Referent),
                ..Options::DEFAULT
            }
        );
        assert_eq!(options(&["--no-d", "0", "d"]).links(), Links::Itself);
        assert_eq!(
            refused(&["--no", "0", "d"]),
            "option '--no' is ambiguous; possibilities: '--no-dereference' '--no-preserve-root'"
        );
        assert_eq!(
            options(&["--pres", "--no-pres", "0", "d"]),
            Options::DEFAULT
        );
        assert!(options(&["--preserve-root", "0", "d"]).preserve_root);
    }

    #[test]
    fn double_dash_ends_options_so_a_dash_named_file_is_an_operand() {
        let (_, target, _, files) = change(&["--", "1000", "-weird"]);
        assert_eq!(target, Target::Spec(String::from("1000")));
        assert_eq!(files, ["-weird"]);
    }

    /// Every locale's `OPTIONS` section documents exactly the switches this
    /// parser accepts: the flag tokens are language-neutral, so each
    /// translated document carries the canonical document's keys. The
    /// documents are read from the bundle's own on-disk `Help/` tree.
    #[test]
    fn help_documents_the_parser_switches() {
        extern crate std;
        use alloc::format;
        use std::fs;

        let help_root = format!("{}/Help", env!("CARGO_MANIFEST_DIR"));
        for locale in tairix_help::REQUIRED_LOCALES {
            let path = format!("{help_root}/{locale}/chown.md");
            let text = fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"));
            for switch in [
                "`-c, --changes`",
                "`-f, --silent, --quiet`",
                "`-v, --verbose`",
                "`--dereference`",
                "`-h, --no-dereference`",
                "`--from=CURRENT_OWNER:CURRENT_GROUP`",
                "`--reference=RFILE`",
                "`-R, --recursive`",
                "`-H`",
                "`-L`",
                "`-P`",
                "`-?, --help`",
                "`--version`",
            ] {
                assert!(
                    text.contains(switch),
                    "{locale}/chown.md must document {switch}"
                );
            }
        }
    }
}
