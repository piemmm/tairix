//! The parsed shape of an `mv` command line.

use alloc::string::String;
use alloc::vec::Vec;

use tairix_util::getopt::{Arg, Opt, Order, Scan, Takes};

use crate::error::MvError;

/// What happens when a destination already exists.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Clobber {
    /// Overwrite it (the default, and `-f`, which never prompts).
    Overwrite,
    /// Ask before overwriting (`-i`).
    Prompt,
    /// Skip the source silently (`-n`).
    Skip,
}

/// How the destination operand is interpreted.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TargetMode {
    /// The last operand; moved *into* when it is an existing directory
    /// (the default).
    Inferred,
    /// An explicit target directory (`-t`); every operand is a source and
    /// the destination must be an existing directory.
    Directory,
    /// The last operand, always treated as a normal file (`-T`); exactly
    /// one source is permitted.
    NoDirectory,
}

/// The full option set of one `mv` run.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Options {
    /// The existing-destination policy (`-f` / `-i` / `-n`, last one
    /// wins).
    pub clobber: Clobber,
    /// Report each move on standard output (`-v`).
    pub verbose: bool,
    /// How the destination operand is interpreted (`-t` / `-T`).
    pub target_mode: TargetMode,
}

impl Options {
    /// The defaults of a bare `mv`.
    pub const DEFAULT: Self = Self {
        clobber: Clobber::Overwrite,
        verbose: false,
        target_mode: TargetMode::Inferred,
    };
}

/// One thing the `mv` tool can do.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Command {
    /// Move `sources` to `dest`, in operand order.
    Move {
        /// The move options.
        options: Options,
        /// The source paths to move, in order. Always at least one.
        sources: Vec<String>,
        /// The destination path. Under [`TargetMode::Inferred`], when it
        /// names an existing directory (and always when there is more than
        /// one source), each source is moved into it under the source's
        /// base name; under [`TargetMode::Directory`] it is the `-t`
        /// directory; under [`TargetMode::NoDirectory`] it is used exactly
        /// as given.
        dest: String,
    },
    /// Print the usage banner (`-h`/`--help`).
    Help,
}

/// What each of `mv`'s options means.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Flag {
    Force,
    Interactive,
    NoClobber,
    NoTargetDirectory,
    TargetDirectory,
    Verbose,
    Help,
}

/// `mv`'s options, as GNU's `mv` spells them and in its order, and the
/// reserved help switches.
const OPTIONS: &[Opt<Flag>] = &[
    Opt::flag('f', &["force"], Flag::Force),
    Opt::flag('i', &["interactive"], Flag::Interactive),
    Opt::flag('n', &["no-clobber"], Flag::NoClobber),
    Opt::flag('T', &["no-target-directory"], Flag::NoTargetDirectory),
    Opt::valued('t', &["target-directory"], Flag::TargetDirectory),
    Opt::flag('v', &["verbose"], Flag::Verbose),
    Opt::flag('h', &["help"], Flag::Help),
    Opt::letter('?', Takes::Nothing, Flag::Help),
];

/// Parse `args` (the tool's arguments, excluding the program name) into a
/// [`Command`].
///
/// The grammar is the GNU `mv` surface,
/// `mv [-finvT] [-t dir] [--] source... dest`, read as GNU's `getopt_long`
/// reads it ([`Scan`]): options anywhere among the operands, clustered, a
/// long one by any unambiguous prefix, `--` ending them. The help switches
/// (`-h`, `-?`, `--help`) answer at once. The last of `-f`, `-i` and `-n`
/// wins.
///
/// Without `-t`, the last path operand is the destination and the rest are
/// the sources; at least two path operands are required. With `-t`, every
/// path operand is a source and at least one is required.
///
/// # Errors
///
/// * [`MvError::Option`] — an option `getopt` refuses, in its words.
/// * [`MvError::Usage`] — an empty `-t` directory, `-t` combined with `-T`,
///   or too few path operands.
pub fn parse(args: &[&str]) -> Result<Command, MvError> {
    let mut options = Options::DEFAULT;
    let mut target_dir: Option<String> = None;
    let mut no_target_dir = false;
    let mut operands = Vec::new();
    for arg in Scan::new(OPTIONS, args, Order::Permute) {
        match arg.map_err(MvError::Option)? {
            Arg::Operand(path) => operands.push(String::from(path)),
            Arg::Opt(Flag::Help, _) => return Ok(Command::Help),
            Arg::Opt(Flag::TargetDirectory, dir) => {
                let dir = dir.filter(|dir| !dir.is_empty()).ok_or(MvError::Usage)?;
                target_dir = Some(String::from(dir));
            }
            Arg::Opt(Flag::NoTargetDirectory, _) => no_target_dir = true,
            Arg::Opt(Flag::Force, _) => options.clobber = Clobber::Overwrite,
            Arg::Opt(Flag::Interactive, _) => options.clobber = Clobber::Prompt,
            Arg::Opt(Flag::NoClobber, _) => options.clobber = Clobber::Skip,
            Arg::Opt(Flag::Verbose, _) => options.verbose = true,
        }
    }
    assemble(options, target_dir, no_target_dir, operands)
}

/// Assemble the parsed options and operands into a [`Command::Move`],
/// resolving the destination per the `-t` / `-T` target mode.
fn assemble(
    mut options: Options,
    target_dir: Option<String>,
    no_target_dir: bool,
    mut operands: Vec<String>,
) -> Result<Command, MvError> {
    if no_target_dir && target_dir.is_some() {
        return Err(MvError::Usage);
    }
    if no_target_dir {
        options.target_mode = TargetMode::NoDirectory;
    }
    let (sources, dest) = if let Some(dir) = target_dir {
        options.target_mode = TargetMode::Directory;
        if operands.is_empty() {
            return Err(MvError::Usage);
        }
        (operands, dir)
    } else {
        // The last operand is the destination; everything before it is a
        // source. `mv` needs at least one source and a destination.
        let Some(dest) = operands.pop() else {
            return Err(MvError::Usage);
        };
        if operands.is_empty() {
            return Err(MvError::Usage);
        }
        (operands, dest)
    };
    Ok(Command::Move {
        options,
        sources,
        dest,
    })
}

#[cfg(test)]
mod tests {
    use super::{parse, Clobber, Command, Options, TargetMode};
    use crate::error::MvError;
    use alloc::string::{String, ToString};
    use alloc::vec::Vec;

    fn mv_with(options: Options, sources: &[&str], dest: &str) -> Command {
        Command::Move {
            options,
            sources: sources.iter().map(|p| String::from(*p)).collect::<Vec<_>>(),
            dest: String::from(dest),
        }
    }

    fn mv(no_clobber: bool, sources: &[&str], dest: &str) -> Command {
        mv_with(
            Options {
                clobber: if no_clobber {
                    Clobber::Skip
                } else {
                    Clobber::Overwrite
                },
                ..Options::DEFAULT
            },
            sources,
            dest,
        )
    }

    #[test]
    fn a_single_source_and_dest_parses() {
        assert_eq!(
            parse(&["a.txt", "b.txt"]),
            Ok(mv(false, &["a.txt"], "b.txt"))
        );
    }

    #[test]
    fn fewer_than_two_operands_is_usage() {
        assert_eq!(parse(&[]), Err(MvError::Usage));
        assert_eq!(parse(&["only"]), Err(MvError::Usage));
        assert_eq!(parse(&["-f"]), Err(MvError::Usage));
        assert_eq!(parse(&["-f", "only"]), Err(MvError::Usage));
    }

    #[test]
    fn help_flags_win() {
        assert_eq!(parse(&["-h"]), Ok(Command::Help));
        assert_eq!(parse(&["-?"]), Ok(Command::Help));
        assert_eq!(parse(&["--help"]), Ok(Command::Help));
        assert_eq!(parse(&["-fh", "a", "b"]), Ok(Command::Help));
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
            let path = format!("{help_root}/{locale}/mv.md");
            let text = fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"));
            for switch in [
                "`-f, --force`",
                "`-i, --interactive`",
                "`-n, --no-clobber`",
                "`-v, --verbose`",
                "`-t dir, --target-directory=dir`",
                "`-T, --no-target-directory`",
                "`-h, -?, --help`",
            ] {
                assert!(
                    text.contains(switch),
                    "{locale}/mv.md must document {switch}"
                );
            }
        }
    }

    #[test]
    fn several_sources_precede_the_destination() {
        assert_eq!(parse(&["a", "b", "dst"]), Ok(mv(false, &["a", "b"], "dst")));
    }

    #[test]
    fn force_and_no_clobber_spellings_parse() {
        assert_eq!(parse(&["-f", "a", "b"]), Ok(mv(false, &["a"], "b")));
        assert_eq!(parse(&["--force", "a", "b"]), Ok(mv(false, &["a"], "b")));
        assert_eq!(parse(&["-n", "a", "b"]), Ok(mv(true, &["a"], "b")));
        assert_eq!(
            parse(&["--no-clobber", "a", "b"]),
            Ok(mv(true, &["a"], "b"))
        );
    }

    #[test]
    fn force_interactive_and_no_clobber_last_one_wins() {
        // As in the GNU tool, the last of `-f` / `-i` / `-n` wins.
        assert_eq!(parse(&["-n", "-f", "a", "b"]), Ok(mv(false, &["a"], "b")));
        assert_eq!(parse(&["-f", "-n", "a", "b"]), Ok(mv(true, &["a"], "b")));
        assert_eq!(
            parse(&["-f", "-i", "a", "b"]),
            Ok(mv_with(
                Options {
                    clobber: Clobber::Prompt,
                    ..Options::DEFAULT
                },
                &["a"],
                "b",
            ))
        );
        assert_eq!(parse(&["-i", "-f", "a", "b"]), Ok(mv(false, &["a"], "b")));
    }

    #[test]
    fn interactive_and_verbose_parse() {
        assert_eq!(
            parse(&["-iv", "a", "b"]),
            Ok(mv_with(
                Options {
                    clobber: Clobber::Prompt,
                    verbose: true,
                    ..Options::DEFAULT
                },
                &["a"],
                "b",
            ))
        );
        assert_eq!(
            parse(&["--interactive", "--verbose", "a", "b"]),
            Ok(mv_with(
                Options {
                    clobber: Clobber::Prompt,
                    verbose: true,
                    ..Options::DEFAULT
                },
                &["a"],
                "b",
            ))
        );
    }

    #[test]
    fn target_directory_takes_a_value() {
        let expected = mv_with(
            Options {
                target_mode: TargetMode::Directory,
                ..Options::DEFAULT
            },
            &["a", "b"],
            "dst",
        );
        assert_eq!(parse(&["-t", "dst", "a", "b"]), Ok(expected.clone()));
        assert_eq!(parse(&["-tdst", "a", "b"]), Ok(expected.clone()));
        assert_eq!(
            parse(&["--target-directory", "dst", "a", "b"]),
            Ok(expected.clone())
        );
        assert_eq!(parse(&["--target-directory=dst", "a", "b"]), Ok(expected));
        assert_eq!(refused(&["a", "-t"]), "option requires an argument -- 't'");
        assert_eq!(parse(&["--target-directory=", "a"]), Err(MvError::Usage));
        assert_eq!(parse(&["-t", "dst"]), Err(MvError::Usage));
    }

    #[test]
    fn no_target_directory_parses_and_excludes_t() {
        assert_eq!(
            parse(&["-T", "a", "b"]),
            Ok(mv_with(
                Options {
                    target_mode: TargetMode::NoDirectory,
                    ..Options::DEFAULT
                },
                &["a"],
                "b",
            ))
        );
        assert_eq!(parse(&["-T", "-t", "d", "a"]), Err(MvError::Usage));
    }

    /// GNU 9.4's words for each refusal.
    fn refused(args: &[&str]) -> String {
        parse(args).expect_err("refused").to_string()
    }

    #[test]
    fn a_refused_option_is_worded_as_getopt_words_it() {
        assert_eq!(refused(&["-x", "a", "b"]), "invalid option -- 'x'");
        assert_eq!(
            refused(&["--frobnicate", "a", "b"]),
            "unrecognized option '--frobnicate'"
        );
        // A bundle with one bad flag is refused whole.
        assert_eq!(refused(&["-fx", "a", "b"]), "invalid option -- 'x'");
        assert_eq!(
            refused(&["a", "--target-directory"]),
            "option '--target-directory' requires an argument"
        );
        assert_eq!(
            refused(&["--verbose=1", "a", "b"]),
            "option '--verbose' doesn't allow an argument"
        );
    }

    /// GNU 9.4 reads `mv --verb --no-cl a b` as `mv --verbose --no-clobber a b`.
    #[test]
    fn a_long_option_may_be_abbreviated() {
        assert_eq!(
            parse(&["--verb", "--no-cl", "a", "b"]),
            Ok(mv_with(
                Options {
                    clobber: Clobber::Skip,
                    verbose: true,
                    ..Options::DEFAULT
                },
                &["a"],
                "b",
            ))
        );
        assert_eq!(
            refused(&["--no", "a", "b"]),
            "option '--no' is ambiguous; possibilities: '--no-clobber' '--no-target-directory'"
        );
    }

    #[test]
    fn double_dash_ends_options() {
        assert_eq!(parse(&["--", "-f", "b"]), Ok(mv(false, &["-f"], "b")));
    }

    #[test]
    fn bare_dash_is_a_path() {
        assert_eq!(parse(&["-", "b"]), Ok(mv(false, &["-"], "b")));
    }
}
