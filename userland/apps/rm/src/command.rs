//! The parsed shape of an `rm` command line.

use alloc::string::String;
use alloc::vec::Vec;

use tairix_util::getopt::{Arg, Opt, Order, Scan, Takes};

use crate::error::RmError;

/// When the tool asks before removing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Interactive {
    /// Never prompt (the default, and `-f`).
    Never,
    /// Prompt once before removing more than three operands, or before a
    /// recursive removal (`-I`).
    Once,
    /// Prompt before every removal (`-i` / `--interactive`).
    Always,
}

/// The full option set of one `rm` run.
///
/// The flags are the GNU tool's independent boolean switches; a
/// two-variant enum per flag would only restate `bool` with extra noise,
/// so the field count mirrors the command surface deliberately.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Options {
    /// Descend into and remove directories (`-r`/`-R`). Without it, a
    /// directory operand is a [`RmError::IsDirectory`] (unless `dir`
    /// permits an empty one).
    pub recursive: bool,
    /// Ignore operands that do not exist, and remove nothing-to-report
    /// silently (`-f`).
    pub force: bool,
    /// Remove empty directories without `-r` (`-d`).
    pub dir: bool,
    /// Report each removal on standard output (`-v`).
    pub verbose: bool,
    /// The prompting policy (`-i` / `-I` / `-f`, last one wins).
    pub interactive: Interactive,
    /// Refuse to remove `/` (the default; cleared by
    /// `--no-preserve-root`).
    pub preserve_root: bool,
}

impl Options {
    /// The defaults of a bare `rm`.
    pub const DEFAULT: Self = Self {
        recursive: false,
        force: false,
        dir: false,
        verbose: false,
        interactive: Interactive::Never,
        preserve_root: true,
    };
}

/// One thing the `rm` tool can do.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Command {
    /// Remove `paths`, in operand order.
    Remove {
        /// The removal options.
        options: Options,
        /// The paths to remove, in order. May be empty only when `-f` is
        /// set (an empty `rm -f` removes nothing and succeeds).
        paths: Vec<String>,
    },
    /// Print the usage banner (`-h`/`--help`).
    Help,
}

/// What each of `rm`'s options means.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Flag {
    Recursive,
    Force,
    Dir,
    Verbose,
    Interactive,
    /// `-I`.
    InteractiveOnce,
    PreserveRoot,
    NoPreserveRoot,
    Help,
}

/// `rm`'s options, as GNU's `rm` spells them, and the reserved help
/// switches.
const OPTIONS: &[Opt<Flag>] = &[
    Opt::flag('r', &["recursive"], Flag::Recursive),
    Opt::letter('R', Takes::Nothing, Flag::Recursive),
    Opt::flag('f', &["force"], Flag::Force),
    Opt::flag('d', &["dir"], Flag::Dir),
    Opt::flag('v', &["verbose"], Flag::Verbose),
    Opt::flag('i', &["interactive"], Flag::Interactive),
    Opt::letter('I', Takes::Nothing, Flag::InteractiveOnce),
    Opt::long(&["preserve-root"], Takes::Nothing, Flag::PreserveRoot),
    Opt::long(&["no-preserve-root"], Takes::Nothing, Flag::NoPreserveRoot),
    Opt::flag('h', &["help"], Flag::Help),
    Opt::letter('?', Takes::Nothing, Flag::Help),
];

/// The only spelling `--no-preserve-root` is accepted in.
const NO_PRESERVE_ROOT: &str = "--no-preserve-root";

/// Parse `args` (the tool's arguments, excluding the program name) into a
/// [`Command`].
///
/// The grammar is the GNU `rm` surface, `rm [-dfiIrRv] [--] file...`, read
/// as GNU's `getopt_long` reads it ([`Scan`]): options anywhere among the
/// operands, clustered, a long one by any unambiguous prefix but
/// `--no-preserve-root`, which GNU accepts only in full, `--` ending them.
/// The help switches (`-h`, `-?`, `--help`) answer at once. As in the GNU
/// tool, the later of `-f` / `-i` / `-I` wins: `-f` cancels prompting and a
/// prompt flag cancels `-f`.
///
/// # Errors
///
/// * [`RmError::Option`] — an option `getopt` refuses, in its words.
/// * [`RmError::AbbreviatedNoPreserveRoot`] — `--no-preserve-root`
///   abbreviated.
/// * [`RmError::Usage`] — no path operand without `-f`.
pub fn parse(args: &[&str]) -> Result<Command, RmError> {
    let mut options = Options::DEFAULT;
    let mut paths = Vec::new();
    // GNU accepts `--no-preserve-root` only in full. `rm` takes no option
    // value, so the arguments before `--` naming it are the ones the scan
    // reads, in its order.
    let mut no_preserve_root = args
        .iter()
        .take_while(|arg| **arg != "--")
        .filter(|arg| arg.len() > 2 && NO_PRESERVE_ROOT.starts_with(**arg));
    for arg in Scan::new(OPTIONS, args, Order::Permute) {
        match arg.map_err(RmError::Option)? {
            Arg::Operand(path) => paths.push(String::from(path)),
            Arg::Opt(Flag::Help, _) => return Ok(Command::Help),
            Arg::Opt(Flag::NoPreserveRoot, _) => {
                if no_preserve_root.next() != Some(&NO_PRESERVE_ROOT) {
                    return Err(RmError::AbbreviatedNoPreserveRoot);
                }
                options.preserve_root = false;
            }
            Arg::Opt(flag, _) => options.set(flag),
        }
    }
    if paths.is_empty() && !options.force {
        return Err(RmError::Usage);
    }
    Ok(Command::Remove { options, paths })
}

impl Options {
    /// Apply the flag `flag` names.
    fn set(&mut self, flag: Flag) {
        match flag {
            Flag::Recursive => self.recursive = true,
            Flag::Force => {
                self.force = true;
                self.interactive = Interactive::Never;
            }
            Flag::Dir => self.dir = true,
            Flag::Verbose => self.verbose = true,
            Flag::Interactive => {
                self.interactive = Interactive::Always;
                self.force = false;
            }
            Flag::InteractiveOnce => {
                self.interactive = Interactive::Once;
                self.force = false;
            }
            Flag::PreserveRoot => self.preserve_root = true,
            Flag::NoPreserveRoot | Flag::Help => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{parse, Command, Interactive, Options};
    use crate::error::RmError;
    use alloc::string::{String, ToString};
    use alloc::vec::Vec;

    fn remove_with(options: Options, paths: &[&str]) -> Command {
        Command::Remove {
            options,
            paths: paths.iter().map(|p| String::from(*p)).collect::<Vec<_>>(),
        }
    }

    fn remove(recursive: bool, force: bool, paths: &[&str]) -> Command {
        remove_with(
            Options {
                recursive,
                force,
                ..Options::DEFAULT
            },
            paths,
        )
    }

    #[test]
    fn a_single_file_operand_parses() {
        assert_eq!(parse(&["a.txt"]), Ok(remove(false, false, &["a.txt"])));
    }

    #[test]
    fn no_operand_without_force_is_usage() {
        assert_eq!(parse(&[]), Err(RmError::Usage));
        assert_eq!(parse(&["-r"]), Err(RmError::Usage));
    }

    #[test]
    fn no_operand_with_force_succeeds_empty() {
        assert_eq!(parse(&["-f"]), Ok(remove(false, true, &[])));
    }

    #[test]
    fn recursive_spellings_parse() {
        for args in [["-r", "d"], ["-R", "d"], ["--recursive", "d"]] {
            assert_eq!(parse(&args), Ok(remove(true, false, &["d"])));
        }
    }

    #[test]
    fn clustered_short_options_parse() {
        assert_eq!(parse(&["-rf", "d"]), Ok(remove(true, true, &["d"])));
        assert_eq!(parse(&["-fR", "d"]), Ok(remove(true, true, &["d"])));
    }

    #[test]
    fn dir_and_verbose_parse() {
        assert_eq!(
            parse(&["-d", "-v", "e"]),
            Ok(remove_with(
                Options {
                    dir: true,
                    verbose: true,
                    ..Options::DEFAULT
                },
                &["e"],
            ))
        );
        assert_eq!(
            parse(&["--dir", "--verbose", "e"]),
            Ok(remove_with(
                Options {
                    dir: true,
                    verbose: true,
                    ..Options::DEFAULT
                },
                &["e"],
            ))
        );
    }

    #[test]
    fn interactive_flags_parse() {
        assert_eq!(
            parse(&["-i", "a"]),
            Ok(remove_with(
                Options {
                    interactive: Interactive::Always,
                    ..Options::DEFAULT
                },
                &["a"],
            ))
        );
        assert_eq!(
            parse(&["--interactive", "a"]),
            Ok(remove_with(
                Options {
                    interactive: Interactive::Always,
                    ..Options::DEFAULT
                },
                &["a"],
            ))
        );
        assert_eq!(
            parse(&["-I", "a"]),
            Ok(remove_with(
                Options {
                    interactive: Interactive::Once,
                    ..Options::DEFAULT
                },
                &["a"],
            ))
        );
    }

    #[test]
    fn force_and_interactive_last_one_wins() {
        // As in the GNU tool: `-f` cancels prompting, a prompt flag
        // cancels `-f`.
        assert_eq!(parse(&["-i", "-f", "a"]), Ok(remove(false, true, &["a"])));
        assert_eq!(
            parse(&["-f", "-i", "a"]),
            Ok(remove_with(
                Options {
                    interactive: Interactive::Always,
                    ..Options::DEFAULT
                },
                &["a"],
            ))
        );
    }

    #[test]
    fn preserve_root_toggles() {
        assert_eq!(
            parse(&["--no-preserve-root", "/"]),
            Ok(remove_with(
                Options {
                    preserve_root: false,
                    ..Options::DEFAULT
                },
                &["/"],
            ))
        );
        assert_eq!(
            parse(&["--no-preserve-root", "--preserve-root", "/"]),
            Ok(remove_with(Options::DEFAULT, &["/"]))
        );
    }

    #[test]
    fn help_flags_win() {
        assert_eq!(parse(&["-h"]), Ok(Command::Help));
        assert_eq!(parse(&["-?"]), Ok(Command::Help));
        assert_eq!(parse(&["--help"]), Ok(Command::Help));
        assert_eq!(parse(&["-rh"]), Ok(Command::Help));
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
            let path = format!("{help_root}/{locale}/rm.md");
            let text = fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"));
            for switch in [
                "`-r, -R, --recursive`",
                "`-f, --force`",
                "`-d, --dir`",
                "`-i, --interactive`",
                "`-I`",
                "`-v, --verbose`",
                "`--preserve-root`",
                "`--no-preserve-root`",
                "`-h, -?, --help`",
            ] {
                assert!(
                    text.contains(switch),
                    "{locale}/rm.md must document {switch}"
                );
            }
        }
    }

    /// GNU 9.4's words for each refusal.
    fn refused(args: &[&str]) -> String {
        parse(args).expect_err("refused").to_string()
    }

    #[test]
    fn a_refused_option_is_worded_as_getopt_words_it() {
        assert_eq!(refused(&["-x", "a"]), "invalid option -- 'x'");
        assert_eq!(
            refused(&["--frobnicate", "a"]),
            "unrecognized option '--frobnicate'"
        );
        // A bundle with one bad flag is refused whole.
        assert_eq!(refused(&["-rx", "a"]), "invalid option -- 'x'");
        assert_eq!(
            refused(&["--force=1", "a"]),
            "option '--force' doesn't allow an argument"
        );
    }

    /// GNU 9.4 reads `rm --rec --verb d` as `rm --recursive --verbose d`, and
    /// refuses `--no-preserve-root` abbreviated.
    #[test]
    fn a_long_option_may_be_abbreviated() {
        assert_eq!(
            parse(&["--rec", "--verb", "d"]),
            Ok(remove_with(
                Options {
                    recursive: true,
                    verbose: true,
                    ..Options::DEFAULT
                },
                &["d"],
            ))
        );
        for args in [
            ["--no", "/"],
            ["--no-preserve-roo", "/"],
            ["--no-preserve-root", "--no-pres"],
        ] {
            assert_eq!(
                refused(&args),
                "you may not abbreviate the --no-preserve-root option",
                "{args:?}"
            );
        }
        // An earlier refusal is still the one reported, as getopt reads in
        // order; after `--` the spelling is an operand.
        assert_eq!(refused(&["-x", "--no", "a"]), "invalid option -- 'x'");
        assert_eq!(
            parse(&["--no-preserve-root", "--", "--no"]),
            Ok(remove_with(
                Options {
                    preserve_root: false,
                    ..Options::DEFAULT
                },
                &["--no"],
            ))
        );
    }

    #[test]
    fn double_dash_ends_options() {
        assert_eq!(parse(&["--", "-r"]), Ok(remove(false, false, &["-r"])));
    }

    #[test]
    fn bare_dash_is_a_path() {
        assert_eq!(parse(&["-"]), Ok(remove(false, false, &["-"])));
    }

    #[test]
    fn paths_preserve_order() {
        assert_eq!(
            parse(&["a", "b", "c"]),
            Ok(remove(false, false, &["a", "b", "c"]))
        );
    }
}
