//! The parsed shape of a `chmod` command line.

use alloc::string::String;
use alloc::vec::Vec;

use tairix_util::getopt::{Arg, Opt, Order, Scan, Takes};
use tairix_util::mode::Mode;

use crate::error::ChmodError;

/// Which mode changes are reported on standard output.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Verbosity {
    /// Report nothing (the default).
    None,
    /// Report only files whose mode actually changed (`-c`).
    Changes,
    /// Report every file processed (`-v`).
    All,
}

/// The full option set of one `chmod` run.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Options {
    /// Descend into directories and apply the mode to their contents
    /// (`-R`/`--recursive`).
    pub recursive: bool,
    /// The reporting policy (`-c` / `-v`, last one wins).
    pub verbosity: Verbosity,
    /// Suppress per-operand error diagnostics (`-f`); a suppressed failure
    /// still fails the run.
    pub quiet: bool,
    /// Refuse a recursive change of the view root (`--preserve-root`); off
    /// by default, as GNU's is.
    pub preserve_root: bool,
}

impl Options {
    /// The defaults of a bare `chmod`.
    pub const DEFAULT: Self = Self {
        recursive: false,
        verbosity: Verbosity::None,
        quiet: false,
        preserve_root: false,
    };
}

/// One thing the `chmod` tool can do.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Command {
    /// Apply `mode` to each of `files`, in operand order.
    Change {
        /// The change options.
        options: Options,
        /// The mode to apply, octal or symbolic.
        mode: Mode,
        /// The files to change, in order. Always at least one.
        files: Vec<String>,
    },
    /// Print the usage banner (`-h`/`--help`).
    Help,
}

/// What each of `chmod`'s options means.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Flag {
    Changes,
    Recursive,
    Quiet,
    Verbose,
    NoPreserveRoot,
    PreserveRoot,
    Help,
}

/// `chmod`'s options, as GNU's `chmod` spells them, and the reserved help
/// switches.
const OPTIONS: &[Opt<Flag>] = &[
    Opt::flag('c', &["changes"], Flag::Changes),
    Opt::flag('R', &["recursive"], Flag::Recursive),
    Opt::flag('f', &["silent", "quiet"], Flag::Quiet),
    Opt::flag('v', &["verbose"], Flag::Verbose),
    Opt::long(&["no-preserve-root"], Takes::Nothing, Flag::NoPreserveRoot),
    Opt::long(&["preserve-root"], Takes::Nothing, Flag::PreserveRoot),
    Opt::flag('h', &["help"], Flag::Help),
    Opt::letter('?', Takes::Nothing, Flag::Help),
];

/// Parse `args` (the tool's arguments, excluding the program name) into a
/// [`Command`].
///
/// The grammar is the GNU `chmod` surface,
/// `chmod [-cfRv] [--] MODE file...`, read as GNU's `getopt_long` reads it
/// ([`Scan`]): options anywhere among the operands, clustered, a long one by
/// any unambiguous prefix, `--` ending them. The help switches (`-h`, `-?`,
/// `--help`) answer at once. The later of `-c` and `-v` wins.
///
/// The first operand is the mode, read by the one shared grammar
/// ([`Mode::parse`]); the rest are files. To set a mode that begins with `-`
/// (for example, "remove write for all"), write it without a leading dash
/// (`a-w`) or end option parsing first (`chmod -- -w file`).
///
/// # Errors
///
/// * [`ChmodError::Option`] — an option `getopt` refuses, in its words.
/// * [`ChmodError::Usage`] — fewer than two operands (a mode and at least
///   one file).
/// * [`ChmodError::BadMode`] — the mode operand reads as neither an octal
///   nor a symbolic mode.
pub fn parse(args: &[&str]) -> Result<Command, ChmodError> {
    let mut options = Options::DEFAULT;
    let mut operands = Vec::new();
    for arg in Scan::new(OPTIONS, args, Order::Permute) {
        match arg.map_err(ChmodError::Option)? {
            Arg::Operand(operand) => operands.push(String::from(operand)),
            Arg::Opt(Flag::Help, _) => return Ok(Command::Help),
            Arg::Opt(Flag::Changes, _) => options.verbosity = Verbosity::Changes,
            Arg::Opt(Flag::Recursive, _) => options.recursive = true,
            Arg::Opt(Flag::Quiet, _) => options.quiet = true,
            Arg::Opt(Flag::Verbose, _) => options.verbosity = Verbosity::All,
            Arg::Opt(Flag::NoPreserveRoot, _) => options.preserve_root = false,
            Arg::Opt(Flag::PreserveRoot, _) => options.preserve_root = true,
        }
    }
    if operands.is_empty() {
        return Err(ChmodError::Usage);
    }
    let mode_spec = operands.remove(0);
    if operands.is_empty() {
        return Err(ChmodError::Usage);
    }
    let mode = Mode::parse(&mode_spec).ok_or(ChmodError::BadMode(mode_spec))?;
    Ok(Command::Change {
        options,
        mode,
        files: operands,
    })
}

#[cfg(test)]
mod tests {
    use super::{parse, Command, Mode, Options, Verbosity};
    use crate::error::ChmodError;
    use alloc::string::{String, ToString};
    use alloc::vec;
    use alloc::vec::Vec;
    use tairix_util::mode::{Change, Op, Perms, PERM_WRITE};

    fn change(recursive: bool, mode: Mode, files: &[&str]) -> Command {
        Command::Change {
            options: Options {
                recursive,
                ..Options::DEFAULT
            },
            mode,
            files: files.iter().map(|p| String::from(*p)).collect::<Vec<_>>(),
        }
    }

    fn octal(bits: u32) -> Mode {
        Mode::Octal {
            bits,
            names_set_id: false,
        }
    }

    #[test]
    fn changes_verbose_and_quiet_parse() {
        assert_eq!(
            parse(&["-c", "644", "f"]),
            Ok(Command::Change {
                options: Options {
                    verbosity: Verbosity::Changes,
                    ..Options::DEFAULT
                },
                mode: octal(0o644),
                files: vec![String::from("f")],
            })
        );
        // The later of `-c` / `-v` wins, as in the GNU tool.
        assert_eq!(
            parse(&["-c", "-v", "644", "f"]),
            Ok(Command::Change {
                options: Options {
                    verbosity: Verbosity::All,
                    ..Options::DEFAULT
                },
                mode: octal(0o644),
                files: vec![String::from("f")],
            })
        );
        for args in [
            ["-f", "644", "f"],
            ["--silent", "644", "f"],
            ["--quiet", "644", "f"],
        ] {
            assert_eq!(
                parse(&args),
                Ok(Command::Change {
                    options: Options {
                        quiet: true,
                        ..Options::DEFAULT
                    },
                    mode: octal(0o644),
                    files: vec![String::from("f")],
                })
            );
        }
    }

    #[test]
    fn an_octal_mode_and_one_file_parses() {
        assert_eq!(
            parse(&["644", "a.txt"]),
            Ok(change(false, octal(0o644), &["a.txt"]))
        );
    }

    #[test]
    fn a_leading_zero_octal_mode_parses() {
        assert_eq!(
            parse(&["0755", "f"]),
            Ok(change(false, octal(0o755), &["f"]))
        );
    }

    #[test]
    fn fewer_than_two_operands_is_usage() {
        assert_eq!(parse(&[]), Err(ChmodError::Usage));
        assert_eq!(parse(&["644"]), Err(ChmodError::Usage));
        assert_eq!(parse(&["-R"]), Err(ChmodError::Usage));
        assert_eq!(parse(&["-R", "644"]), Err(ChmodError::Usage));
    }

    #[test]
    fn help_flags_win() {
        assert_eq!(parse(&["-h"]), Ok(Command::Help));
        assert_eq!(parse(&["-?"]), Ok(Command::Help));
        assert_eq!(parse(&["--help"]), Ok(Command::Help));
        assert_eq!(parse(&["-Rh", "644", "f"]), Ok(Command::Help));
    }

    #[test]
    fn recursive_flag_sets_its_field() {
        assert_eq!(
            parse(&["-R", "600", "d"]),
            Ok(change(true, octal(0o600), &["d"]))
        );
        assert_eq!(
            parse(&["--recursive", "600", "d"]),
            Ok(change(true, octal(0o600), &["d"]))
        );
    }

    #[test]
    fn several_files_after_the_mode_are_all_collected() {
        assert_eq!(
            parse(&["640", "a", "b", "c"]),
            Ok(change(false, octal(0o640), &["a", "b", "c"]))
        );
    }

    #[test]
    fn lowercase_r_is_not_recursive_and_is_usage() {
        // POSIX `chmod` spells recursive `-R`; a bare `-r` is not an option.
        assert_eq!(refused(&["-r", "644", "f"]), "invalid option -- 'r'");
    }

    /// GNU 9.4's words for each refusal.
    fn refused(args: &[&str]) -> String {
        parse(args).expect_err("refused").to_string()
    }

    #[test]
    fn a_refused_option_is_worded_as_getopt_words_it() {
        assert_eq!(refused(&["-x", "644", "f"]), "invalid option -- 'x'");
        assert_eq!(
            refused(&["--frob", "644", "f"]),
            "unrecognized option '--frob'"
        );
        // A bundle with one bad flag is refused whole.
        assert_eq!(refused(&["-Rx", "644", "f"]), "invalid option -- 'x'");
        assert_eq!(
            refused(&["--changes=1", "644", "f"]),
            "option '--changes' doesn't allow an argument"
        );
    }

    /// GNU 9.4 reads `chmod --rec --ch 644 d` as
    /// `chmod --recursive --changes 644 d`.
    #[test]
    fn a_long_option_may_be_abbreviated() {
        assert_eq!(
            parse(&["--rec", "--ch", "644", "d"]),
            Ok(Command::Change {
                options: Options {
                    recursive: true,
                    verbosity: Verbosity::Changes,
                    ..Options::DEFAULT
                },
                mode: octal(0o644),
                files: vec![String::from("d")],
            })
        );
        assert_eq!(parse(&["--he"]), Ok(Command::Help));
    }

    #[test]
    fn double_dash_ends_options_so_a_dash_mode_is_an_operand() {
        // `chmod -- -w file` applies the symbolic clause `-w` to `file`.
        assert_eq!(
            parse(&["--", "-w", "file"]),
            Ok(change(
                false,
                Mode::Symbolic(vec![Change {
                    who: 0,
                    op: Op::Remove,
                    perms: Perms::Letters(PERM_WRITE),
                }]),
                &["file"],
            ))
        );
    }

    /// GNU 9.4 names the operand it could not read: `invalid mode: 'zzz'`.
    #[test]
    fn an_unparseable_mode_is_refused_naming_it() {
        for spec in ["zzz", "8", "010000", "g", "a+rw,", "a+q"] {
            assert_eq!(
                parse(&[spec, "f"]),
                Err(ChmodError::BadMode(String::from(spec)))
            );
        }
        assert_eq!(refused(&["zzz", "f"]), "invalid mode: 'zzz'");
        assert_eq!(refused(&["--", "-q", "f"]), "invalid mode: '-q'");
    }
}
