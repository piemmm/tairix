//! The parsed shape of a `cat` command line.

use alloc::string::String;
use alloc::vec::Vec;

use tairix_util::getopt::{Arg, Opt, Order, Scan, Takes};

use crate::error::CatError;

/// One source `cat` reads from, in command-line order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Source {
    /// Standard input — the `-` operand, and the default when no operand is
    /// given.
    Stdin,
    /// A named file.
    Path(String),
}

/// Which lines receive a number prefix.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Numbering {
    /// No line numbers (the default).
    None,
    /// Number every output line (`-n`).
    All,
    /// Number only non-empty output lines (`-b`; overrides `-n`, as in the
    /// GNU tool).
    NonBlank,
}

/// The output-shaping options of the GNU `cat` surface.
///
/// The flags are the GNU tool's independent boolean switches; a
/// two-variant enum per flag would only restate `bool` with extra noise,
/// so the field count mirrors the command surface deliberately.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Render {
    /// Line-number policy (`-n` / `-b`).
    pub numbering: Numbering,
    /// Suppress repeated adjacent blank lines (`-s`).
    pub squeeze_blank: bool,
    /// Print `$` at the end of every line (`-E`).
    pub show_ends: bool,
    /// Print TAB characters as `^I` (`-T`).
    pub show_tabs: bool,
    /// Print other control and non-ASCII bytes in `^`/`M-` notation (`-v`).
    pub show_nonprinting: bool,
}

impl Render {
    /// The plain pass-through rendering: no numbering, no transformation.
    pub const PLAIN: Self = Self {
        numbering: Numbering::None,
        squeeze_blank: false,
        show_ends: false,
        show_tabs: false,
        show_nonprinting: false,
    };
}

/// One thing the `cat` tool can do.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Command {
    /// Concatenate `sources` to the terminal, shaped by `render`.
    Concat {
        /// The output-shaping options.
        render: Render,
        /// The sources to read, in order. Never empty: an empty command line
        /// yields a single [`Source::Stdin`].
        sources: Vec<Source>,
    },
    /// Render `cat`'s own short help (`-h`/`-?`/`--help`): the `NAME`,
    /// `SYNOPSIS`, and compact `OPTIONS` of its Help document, through the
    /// same engine as any other command's short help (plans/APPS.md §4).
    Help,
}

/// What each of `cat`'s options means.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Flag {
    NumberNonblank,
    Number,
    SqueezeBlank,
    ShowNonprinting,
    ShowEnds,
    ShowTabs,
    ShowAll,
    /// `-e`, which is `-vE`.
    NonprintingEnds,
    /// `-t`, which is `-vT`.
    NonprintingTabs,
    Unbuffered,
    Help,
}

/// `cat`'s options, as GNU's `cat` spells them and in its order, and the
/// reserved help switches.
const OPTIONS: &[Opt<Flag>] = &[
    Opt::flag('b', &["number-nonblank"], Flag::NumberNonblank),
    Opt::flag('n', &["number"], Flag::Number),
    Opt::flag('s', &["squeeze-blank"], Flag::SqueezeBlank),
    Opt::flag('v', &["show-nonprinting"], Flag::ShowNonprinting),
    Opt::flag('E', &["show-ends"], Flag::ShowEnds),
    Opt::flag('T', &["show-tabs"], Flag::ShowTabs),
    Opt::flag('A', &["show-all"], Flag::ShowAll),
    Opt::letter('e', Takes::Nothing, Flag::NonprintingEnds),
    Opt::letter('t', Takes::Nothing, Flag::NonprintingTabs),
    Opt::letter('u', Takes::Nothing, Flag::Unbuffered),
    Opt::flag('h', &["help"], Flag::Help),
    Opt::letter('?', Takes::Nothing, Flag::Help),
];

/// Parse `args` (the tool's arguments, excluding the program name) into a
/// [`Command`].
///
/// The grammar is the GNU `cat` surface, `cat [-AbeEnstTuv] [--] [file...]`,
/// read as GNU's `getopt_long` reads it ([`Scan`]): options anywhere among
/// the operands, clustered, a long one by any unambiguous prefix, `--`
/// ending them. The help switches (`-h`, `-?`, `--help`) answer at once.
/// `-b` overrides `-n` in either order. The `-` operand is standard input,
/// and with no operand at all the single source is standard input.
///
/// # Errors
///
/// [`CatError::Option`] — an option `getopt` refuses, in its words.
pub fn parse(args: &[&str]) -> Result<Command, CatError> {
    let mut render = Render::PLAIN;
    let mut number_all = false;
    let mut number_nonblank = false;
    let mut sources = Vec::new();
    for arg in Scan::new(OPTIONS, args, Order::Permute) {
        match arg.map_err(CatError::Option)? {
            Arg::Operand("-") => sources.push(Source::Stdin),
            Arg::Operand(path) => sources.push(Source::Path(String::from(path))),
            Arg::Opt(Flag::Help, _) => return Ok(Command::Help),
            Arg::Opt(Flag::NumberNonblank, _) => number_nonblank = true,
            Arg::Opt(Flag::Number, _) => number_all = true,
            Arg::Opt(Flag::SqueezeBlank, _) => render.squeeze_blank = true,
            Arg::Opt(Flag::ShowNonprinting, _) => render.show_nonprinting = true,
            Arg::Opt(Flag::ShowEnds, _) => render.show_ends = true,
            Arg::Opt(Flag::ShowTabs, _) => render.show_tabs = true,
            Arg::Opt(Flag::ShowAll, _) => {
                render.show_nonprinting = true;
                render.show_ends = true;
                render.show_tabs = true;
            }
            Arg::Opt(Flag::NonprintingEnds, _) => {
                render.show_nonprinting = true;
                render.show_ends = true;
            }
            Arg::Opt(Flag::NonprintingTabs, _) => {
                render.show_nonprinting = true;
                render.show_tabs = true;
            }
            Arg::Opt(Flag::Unbuffered, _) => {}
        }
    }
    render.numbering = if number_nonblank {
        Numbering::NonBlank
    } else if number_all {
        Numbering::All
    } else {
        Numbering::None
    };
    if sources.is_empty() {
        sources.push(Source::Stdin);
    }
    Ok(Command::Concat { render, sources })
}

#[cfg(test)]
mod tests {
    use super::{parse, Command, Numbering, Render, Source};
    use alloc::string::{String, ToString};
    use alloc::vec;

    fn path(name: &str) -> Source {
        Source::Path(String::from(name))
    }

    fn numbered(numbering: Numbering) -> Render {
        Render {
            numbering,
            ..Render::PLAIN
        }
    }

    #[test]
    fn no_arguments_reads_stdin() {
        assert_eq!(
            parse(&[]),
            Ok(Command::Concat {
                render: Render::PLAIN,
                sources: vec![Source::Stdin],
            })
        );
    }

    #[test]
    fn help_flags_win() {
        assert_eq!(parse(&["-h"]), Ok(Command::Help));
        assert_eq!(parse(&["-?"]), Ok(Command::Help));
        assert_eq!(parse(&["--help"]), Ok(Command::Help));
        // `--help` is recognised even alongside other arguments.
        assert_eq!(parse(&["-n", "file", "--help"]), Ok(Command::Help));
        // A short-help flag wins from inside a bundle too.
        assert_eq!(parse(&["-nh"]), Ok(Command::Help));
    }

    #[test]
    fn number_flag_sets_numbering() {
        assert_eq!(
            parse(&["-n", "a.txt"]),
            Ok(Command::Concat {
                render: numbered(Numbering::All),
                sources: vec![path("a.txt")],
            })
        );
        assert_eq!(
            parse(&["--number"]),
            Ok(Command::Concat {
                render: numbered(Numbering::All),
                sources: vec![Source::Stdin],
            })
        );
    }

    #[test]
    fn number_nonblank_overrides_number() {
        // As in the GNU tool, `-b` wins over `-n` in either order.
        for args in [["-n", "-b"], ["-b", "-n"]] {
            assert_eq!(
                parse(&args),
                Ok(Command::Concat {
                    render: numbered(Numbering::NonBlank),
                    sources: vec![Source::Stdin],
                })
            );
        }
    }

    #[test]
    fn show_all_is_vet() {
        let expected = Render {
            numbering: Numbering::None,
            squeeze_blank: false,
            show_ends: true,
            show_tabs: true,
            show_nonprinting: true,
        };
        for args in [
            vec!["-A"],
            vec!["--show-all"],
            vec!["-v", "-E", "-T"],
            vec!["-vET"],
        ] {
            assert_eq!(
                parse(&args),
                Ok(Command::Concat {
                    render: expected,
                    sources: vec![Source::Stdin],
                })
            );
        }
    }

    #[test]
    fn e_and_t_imply_nonprinting() {
        assert_eq!(
            parse(&["-e"]),
            Ok(Command::Concat {
                render: Render {
                    show_ends: true,
                    show_nonprinting: true,
                    ..Render::PLAIN
                },
                sources: vec![Source::Stdin],
            })
        );
        assert_eq!(
            parse(&["-t"]),
            Ok(Command::Concat {
                render: Render {
                    show_tabs: true,
                    show_nonprinting: true,
                    ..Render::PLAIN
                },
                sources: vec![Source::Stdin],
            })
        );
    }

    #[test]
    fn squeeze_and_marker_long_options_parse() {
        assert_eq!(
            parse(&["--squeeze-blank", "--show-ends", "--show-tabs"]),
            Ok(Command::Concat {
                render: Render {
                    squeeze_blank: true,
                    show_ends: true,
                    show_tabs: true,
                    ..Render::PLAIN
                },
                sources: vec![Source::Stdin],
            })
        );
        assert_eq!(
            parse(&["--show-nonprinting", "--number-nonblank"]),
            Ok(Command::Concat {
                render: Render {
                    numbering: Numbering::NonBlank,
                    show_nonprinting: true,
                    ..Render::PLAIN
                },
                sources: vec![Source::Stdin],
            })
        );
    }

    #[test]
    fn unbuffered_flag_is_accepted_and_ignored() {
        assert_eq!(
            parse(&["-u", "a"]),
            Ok(Command::Concat {
                render: Render::PLAIN,
                sources: vec![path("a")],
            })
        );
    }

    #[test]
    fn bundled_short_flags_expand() {
        assert_eq!(
            parse(&["-bsE", "a"]),
            Ok(Command::Concat {
                render: Render {
                    numbering: Numbering::NonBlank,
                    squeeze_blank: true,
                    show_ends: true,
                    ..Render::PLAIN
                },
                sources: vec![path("a")],
            })
        );
    }

    #[test]
    fn paths_preserve_order() {
        assert_eq!(
            parse(&["a", "b", "c"]),
            Ok(Command::Concat {
                render: Render::PLAIN,
                sources: vec![path("a"), path("b"), path("c")],
            })
        );
    }

    #[test]
    fn dash_is_stdin_mixed_with_files() {
        assert_eq!(
            parse(&["a", "-", "b"]),
            Ok(Command::Concat {
                render: Render::PLAIN,
                sources: vec![path("a"), Source::Stdin, path("b")],
            })
        );
    }

    /// GNU 9.4's words for each refusal.
    fn refused(args: &[&str]) -> String {
        parse(args).expect_err("refused").to_string()
    }

    #[test]
    fn a_refused_option_is_worded_as_getopt_words_it() {
        assert_eq!(refused(&["-x"]), "invalid option -- 'x'");
        assert_eq!(
            refused(&["--frobnicate"]),
            "unrecognized option '--frobnicate'"
        );
        // A bundle with one bad flag is refused whole.
        assert_eq!(refused(&["-nx"]), "invalid option -- 'x'");
        assert_eq!(
            refused(&["--number=3"]),
            "option '--number' doesn't allow an argument"
        );
    }

    /// GNU 9.4 reads `cat --squ --show-e` as
    /// `cat --squeeze-blank --show-ends`.
    #[test]
    fn a_long_option_may_be_abbreviated() {
        assert_eq!(
            parse(&["--squ", "--show-e"]),
            Ok(Command::Concat {
                render: Render {
                    squeeze_blank: true,
                    show_ends: true,
                    ..Render::PLAIN
                },
                sources: vec![Source::Stdin],
            })
        );
        assert_eq!(
            refused(&["--num"]),
            "option '--num' is ambiguous; possibilities: '--number-nonblank' '--number'"
        );
    }

    #[test]
    fn double_dash_ends_options() {
        // After `--`, a leading-dash argument is a path, not an option.
        assert_eq!(
            parse(&["--", "-n"]),
            Ok(Command::Concat {
                render: Render::PLAIN,
                sources: vec![path("-n")],
            })
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
        let locales = tairix_help::REQUIRED_LOCALES;
        for locale in locales {
            let path = format!("{help_root}/{locale}/cat.md");
            let text = fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"));
            for switch in [
                "`-A, --show-all`",
                "`-b, --number-nonblank`",
                "`-e`",
                "`-E, --show-ends`",
                "`-n, --number`",
                "`-s, --squeeze-blank`",
                "`-t`",
                "`-T, --show-tabs`",
                "`-u`",
                "`-v, --show-nonprinting`",
                "`-h, -?`",
            ] {
                assert!(
                    text.contains(switch),
                    "{locale}/cat.md must document {switch}"
                );
            }
        }
    }
}
