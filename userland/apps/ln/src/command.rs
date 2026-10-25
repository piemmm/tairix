//! The parsed shape of an `ln` command line.

use alloc::string::String;
use alloc::vec::Vec;

use tairix_util::getopt::{Arg, Opt, Order, Scan, Takes};

use crate::error::LnError;

/// What to do with a link name that is already taken.
///
/// GNU keeps `-f` and `-i` as two flags that clear each other, so the last
/// one on the command line wins; one enum expresses that precedence directly.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Clobber {
    /// Refuse the link and report the taken name (the default).
    Refuse,
    /// Put the new link in the existing name's place (`-f`).
    Replace,
    /// Ask, and replace only on an affirmative reply (`-i`).
    Ask,
}

/// How the destination operand is read.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TargetMode {
    /// The default: one destination operand is a link *name* unless the
    /// filesystem says it is a directory (or a link to one), and three or
    /// more operands make the last a directory.
    Inferred,
    /// `-t dir`: every operand is a target and the links go in `dir`, which
    /// must already be a directory.
    Directory,
    /// `-T`: the destination is a link name, never a directory to fill, so
    /// exactly two operands are accepted.
    NoDirectory,
}

/// The full option set of one `ln` invocation.
///
/// The flags are the GNU tool's independent boolean switches; a two-variant
/// enum per flag would only restate `bool` with extra noise, so the field
/// count mirrors the command surface deliberately.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Options {
    /// Create symbolic links (`-s`). Without it a hard link is made: a
    /// second directory entry for the target's own inode.
    pub symbolic: bool,
    /// Store each symbolic link's target *relative to the link's own
    /// directory* (`-r`). Only meaningful with `-s`, since a hard link
    /// stores no target; `-r` without `-s` is a usage error rather than a
    /// silently ignored switch.
    pub relative: bool,
    /// Hard-link what a symbolic-link target *names* rather than the link
    /// itself (`-L`; `-P` is the default and clears it). Meaningless for
    /// `-s`, which stores the target verbatim and resolves nothing.
    pub dereference_target: bool,
    /// Permit a directory operand (`-d` / `-F`). The attempt still fails:
    /// no principal may give a directory a second name, because the tree
    /// staying a tree is what makes physical `..` resolution well-defined.
    /// The switch only stops `ln` refusing the *command line*, matching what
    /// the GNU tool does on a system whose kernel refuses the operation.
    pub allow_directory: bool,
    /// What to do with a link name that is already taken (`-f` / `-i`).
    pub clobber: Clobber,
    /// Treat a destination that is a symbolic link to a directory as the
    /// plain name it also is, rather than a directory to create links in
    /// (`-n`).
    pub no_dereference: bool,
    /// Report each created link on standard output (`-v`).
    pub verbose: bool,
    /// How the destination operand is read (`-t` / `-T`).
    pub target_mode: TargetMode,
}

impl Options {
    /// The defaults of a bare `ln`.
    pub const DEFAULT: Self = Self {
        symbolic: false,
        relative: false,
        dereference_target: false,
        allow_directory: false,
        clobber: Clobber::Refuse,
        no_dereference: false,
        verbose: false,
        target_mode: TargetMode::Inferred,
    };
}

/// One thing the `ln` tool can do.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Command {
    /// Create a link per target.
    Link {
        /// The link options.
        options: Options,
        /// The link targets, in operand order. Never empty.
        targets: Vec<String>,
        /// The destination operand: the directory `-t` named, the last
        /// operand otherwise, and `None` for the single-operand form (whose
        /// link takes the target's own leaf name in the working directory).
        destination: Option<String>,
    },
    /// Render `ln`'s own short help (`-h`/`-?`/`--help`): the `NAME`,
    /// `SYNOPSIS`, and compact `OPTIONS` of its Help document, through the
    /// same engine as any other command's short help (plans/APPS.md §4).
    Help,
}

/// What each of `ln`'s options means.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Flag {
    Symbolic,
    Relative,
    Logical,
    Physical,
    Directory,
    Force,
    Interactive,
    NoDereference,
    Verbose,
    TargetDirectory,
    NoTargetDirectory,
    Help,
}

/// `ln`'s options, as GNU's `ln` spells them, and the reserved help switches.
const OPTIONS: &[Opt<Flag>] = &[
    Opt::flag('s', &["symbolic"], Flag::Symbolic),
    Opt::flag('r', &["relative"], Flag::Relative),
    Opt::flag('L', &["logical"], Flag::Logical),
    Opt::flag('P', &["physical"], Flag::Physical),
    Opt::flag('d', &["directory"], Flag::Directory),
    Opt::letter('F', Takes::Nothing, Flag::Directory),
    Opt::flag('f', &["force"], Flag::Force),
    Opt::flag('i', &["interactive"], Flag::Interactive),
    Opt::flag('n', &["no-dereference"], Flag::NoDereference),
    Opt::flag('v', &["verbose"], Flag::Verbose),
    Opt::valued('t', &["target-directory"], Flag::TargetDirectory),
    Opt::flag('T', &["no-target-directory"], Flag::NoTargetDirectory),
    Opt::flag('h', &["help"], Flag::Help),
    Opt::letter('?', Takes::Nothing, Flag::Help),
];

/// Parse `args` (the tool's arguments, excluding the program name) into a
/// [`Command`].
///
/// The grammar is the GNU `ln` surface,
/// `ln [-srLPdFfinvT] [-t dir] [--] target... [link_name]`, read as GNU's
/// `getopt_long` reads it ([`Scan`]): options anywhere among the operands,
/// clustered, a long one by any unambiguous prefix, `--` ending them. The
/// help switches (`-h`, `-?`, `--help`) answer at once. The later of `-f` and
/// `-i`, and of `-L` and `-P`, wins.
///
/// Operand shapes, as the GNU tool reads them:
///
/// * one operand — the link is created in the working directory under the
///   target's own leaf name.
/// * two operands — the second is a directory to fill when it is one (or a
///   link to one, unless `-n`), and the link's name otherwise. `-T` forces
///   the name reading; `-t` makes both operands targets.
/// * three or more — the last must already be a directory (unless `-t`
///   named one, in which case they are all targets).
///
/// # Errors
///
/// * [`LnError::Option`] — an option `getopt` refuses, in its words.
/// * [`LnError::TargetConflict`] — `-t` together with `-T`.
/// * [`LnError::RelativeNeedsSymbolic`] — `-r` without `-s` (a hard link
///   stores no target to make relative).
/// * [`LnError::MissingOperand`] — no operand at all.
/// * [`LnError::MissingDestination`] — `-T` with a single operand.
/// * [`LnError::ExtraOperand`] — `-T` with three or more operands.
pub fn parse(args: &[&str]) -> Result<Command, LnError> {
    let mut options = Options::DEFAULT;
    let mut directory: Option<String> = None;
    let mut operands: Vec<String> = Vec::new();
    for arg in Scan::new(OPTIONS, args, Order::Permute) {
        match arg.map_err(LnError::Option)? {
            Arg::Operand(operand) => operands.push(String::from(operand)),
            Arg::Opt(Flag::Help, _) => return Ok(Command::Help),
            Arg::Opt(Flag::TargetDirectory, value) => directory = value.map(String::from),
            Arg::Opt(flag, _) => options.set(flag),
        }
    }

    // The two destination readings contradict, so a command line asking for
    // both is refused rather than one silently winning, as GNU refuses it.
    if directory.is_some() && options.target_mode == TargetMode::NoDirectory {
        return Err(LnError::TargetConflict);
    }
    // A hard link stores no target, so there is nothing for `-r` to make
    // relative: refused rather than ignored, as GNU does.
    if options.relative && !options.symbolic {
        return Err(LnError::RelativeNeedsSymbolic);
    }
    if directory.is_some() {
        options.target_mode = TargetMode::Directory;
    }

    if operands.is_empty() {
        return Err(LnError::MissingOperand);
    }

    let (targets, destination) = match options.target_mode {
        // `-t dir`: every operand is a target.
        TargetMode::Directory => (operands, directory),
        TargetMode::NoDirectory => match operands.len() {
            1 => return Err(LnError::MissingDestination(operands.swap_remove(0))),
            2 => {
                let destination = operands.pop().unwrap_or_default();
                (operands, Some(destination))
            }
            _ => return Err(LnError::ExtraOperand(operands.swap_remove(2))),
        },
        TargetMode::Inferred => {
            if operands.len() == 1 {
                (operands, None)
            } else {
                let destination = operands.pop().unwrap_or_default();
                (operands, Some(destination))
            }
        }
    };

    Ok(Command::Link {
        options,
        targets,
        destination,
    })
}

impl Options {
    /// Apply the flag `flag` names.
    fn set(&mut self, flag: Flag) {
        match flag {
            Flag::Symbolic => self.symbolic = true,
            Flag::Relative => self.relative = true,
            Flag::Logical => self.dereference_target = true,
            Flag::Physical => self.dereference_target = false,
            Flag::Directory => self.allow_directory = true,
            Flag::Force => self.clobber = Clobber::Replace,
            Flag::Interactive => self.clobber = Clobber::Ask,
            Flag::NoDereference => self.no_dereference = true,
            Flag::Verbose => self.verbose = true,
            Flag::NoTargetDirectory => self.target_mode = TargetMode::NoDirectory,
            Flag::TargetDirectory | Flag::Help => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use alloc::string::{String, ToString};
    use alloc::vec;
    use alloc::vec::Vec;

    use super::{parse, Clobber, Command, Options, TargetMode};
    use crate::error::LnError;

    fn link(options: Options, targets: &[&str], destination: Option<&str>) -> Command {
        Command::Link {
            options,
            targets: targets.iter().map(|&s| String::from(s)).collect(),
            destination: destination.map(String::from),
        }
    }

    fn symbolic() -> Options {
        Options {
            symbolic: true,
            ..Options::DEFAULT
        }
    }

    #[test]
    fn one_operand_takes_the_targets_own_name() {
        assert_eq!(
            parse(&["-s", "/a/b"]),
            Ok(link(symbolic(), &["/a/b"], None))
        );
    }

    #[test]
    fn two_operands_name_the_link() {
        assert_eq!(
            parse(&["-s", "/a/b", "c"]),
            Ok(link(symbolic(), &["/a/b"], Some("c")))
        );
    }

    #[test]
    fn three_operands_make_the_last_the_destination() {
        assert_eq!(
            parse(&["-s", "a", "b", "dir"]),
            Ok(link(symbolic(), &["a", "b"], Some("dir")))
        );
    }

    #[test]
    fn target_directory_makes_every_operand_a_target() {
        let expected = Options {
            target_mode: TargetMode::Directory,
            ..symbolic()
        };
        assert_eq!(
            parse(&["-s", "-t", "dir", "a", "b"]),
            Ok(link(expected, &["a", "b"], Some("dir")))
        );
        // The value may ride in the cluster or in a long option.
        assert_eq!(
            parse(&["-s", "-tdir", "a"]),
            Ok(link(expected, &["a"], Some("dir")))
        );
        assert_eq!(
            parse(&["-s", "--target-directory=dir", "a"]),
            Ok(link(expected, &["a"], Some("dir")))
        );
        assert_eq!(
            parse(&["-s", "--target-directory", "dir", "a"]),
            Ok(link(expected, &["a"], Some("dir")))
        );
    }

    #[test]
    fn no_target_directory_takes_exactly_two_operands() {
        let expected = Options {
            target_mode: TargetMode::NoDirectory,
            ..symbolic()
        };
        assert_eq!(
            parse(&["-sT", "a", "b"]),
            Ok(link(expected, &["a"], Some("b")))
        );
        assert_eq!(
            parse(&["-sT", "a"]),
            Err(LnError::MissingDestination(String::from("a")))
        );
        assert_eq!(
            parse(&["-sT", "a", "b", "c"]),
            Err(LnError::ExtraOperand(String::from("c")))
        );
    }

    #[test]
    fn the_two_destination_readings_cannot_both_hold() {
        let conflict = parse(&["-s", "-t", "dir", "-T", "a"]);
        assert_eq!(conflict, Err(LnError::TargetConflict));
        assert_eq!(
            LnError::TargetConflict.to_string(),
            "cannot combine --target-directory and --no-target-directory"
        );
    }

    #[test]
    fn the_later_of_force_and_interactive_wins() {
        let parsed = |args: &[&str]| match parse(args) {
            Ok(Command::Link { options, .. }) => options.clobber,
            other => panic!("expected a link command, got {other:?}"),
        };
        assert_eq!(parsed(&["-sfi", "a", "b"]), Clobber::Ask);
        assert_eq!(parsed(&["-sif", "a", "b"]), Clobber::Replace);
        assert_eq!(
            parsed(&["-s", "--force", "--interactive", "a"]),
            Clobber::Ask
        );
        assert_eq!(parsed(&["-s", "a"]), Clobber::Refuse);
    }

    #[test]
    fn flags_bundle_and_set_each_axis() {
        let expected = Options {
            symbolic: true,
            clobber: Clobber::Replace,
            no_dereference: true,
            verbose: true,
            ..Options::DEFAULT
        };
        assert_eq!(
            parse(&["-sfnv", "a", "b"]),
            Ok(link(expected, &["a"], Some("b")))
        );
        assert_eq!(
            parse(&[
                "--symbolic",
                "--force",
                "--no-dereference",
                "--verbose",
                "a",
                "b"
            ]),
            Ok(link(expected, &["a"], Some("b")))
        );
    }

    #[test]
    fn double_dash_ends_options() {
        assert_eq!(
            parse(&["-s", "--", "-v", "-n"]),
            Ok(link(symbolic(), &["-v"], Some("-n")))
        );
    }

    #[test]
    fn help_flags_win() {
        assert_eq!(parse(&["-h"]), Ok(Command::Help));
        assert_eq!(parse(&["-?"]), Ok(Command::Help));
        assert_eq!(parse(&["--help"]), Ok(Command::Help));
        assert_eq!(parse(&["-s", "a", "-h"]), Ok(Command::Help));
    }

    #[test]
    fn missing_operand_is_reported() {
        assert_eq!(parse(&[]), Err(LnError::MissingOperand));
        assert_eq!(parse(&["-s"]), Err(LnError::MissingOperand));
    }

    /// GNU 9.4's words for each refusal.
    fn refused(args: &[&str]) -> String {
        parse(args).expect_err("refused").to_string()
    }

    #[test]
    fn a_refused_option_is_worded_as_getopt_words_it() {
        assert_eq!(refused(&["-x", "a"]), "invalid option -- 'x'");
        assert_eq!(refused(&["--frob", "a"]), "unrecognized option '--frob'");
        // A bundle with one bad flag is refused whole.
        assert_eq!(refused(&["-sx", "a"]), "invalid option -- 'x'");
        assert_eq!(refused(&["-s", "-t"]), "option requires an argument -- 't'");
        assert_eq!(
            refused(&["--target-directory"]),
            "option '--target-directory' requires an argument"
        );
        // A value on a switch that takes none is a mistake, not a token to
        // ignore.
        assert_eq!(
            refused(&["--verbose=1", "a"]),
            "option '--verbose' doesn't allow an argument"
        );
        assert_eq!(
            refused(&["--n", "a"]),
            "option '--n' is ambiguous; possibilities: '--no-dereference' '--no-target-directory'"
        );
    }

    /// GNU 9.4 reads `ln --verb --sym a b` as `ln --verbose --symbolic a b`.
    #[test]
    fn a_long_option_may_be_abbreviated() {
        let Ok(Command::Link { options, .. }) = parse(&["--verb", "--sym", "--target=d", "a"])
        else {
            panic!("parsed");
        };
        assert!(options.verbose && options.symbolic);
        assert_eq!(options.target_mode, TargetMode::Directory);
        assert_eq!(parse(&["--he"]), Ok(Command::Help));
    }

    #[test]
    fn a_lone_dash_is_an_operand() {
        assert_eq!(
            parse(&["-s", "-", "l"]),
            Ok(link(symbolic(), &["-"], Some("l")))
        );
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
        for locale in tairix_help::REQUIRED_LOCALES {
            let path = format!("{help_root}/{locale}/ln.md");
            let text = fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"));
            for switch in [
                "`-s, --symbolic`",
                "`-r, --relative`",
                "`-f, --force`",
                "`-i, --interactive`",
                "`-n, --no-dereference`",
                "`-v, --verbose`",
                "`-t dir, --target-directory=dir`",
                "`-T, --no-target-directory`",
                "`-h, -?, --help`",
            ] {
                assert!(
                    text.contains(switch),
                    "{locale}/ln.md must document {switch}"
                );
            }
        }
    }

    #[test]
    fn without_s_the_options_still_parse() {
        // Whether a hard link can be made is the client's refusal, not the
        // parser's: the command line itself is well formed.
        let expected: Vec<String> = vec![String::from("a")];
        assert_eq!(
            parse(&["a", "b"]),
            Ok(Command::Link {
                options: Options::DEFAULT,
                targets: expected,
                destination: Some(String::from("b")),
            })
        );
    }
}
