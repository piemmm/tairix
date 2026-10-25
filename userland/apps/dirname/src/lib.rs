//! TAIRiX `dirname` — strip the last component from names
//! (`plans/APPS.md` §12.1 Stage C).
//!
//! The GNU coreutils `dirname`: print each path spelling with its last
//! component removed. The surgery is purely **lexical** — no path is
//! resolved, normalised, or touched on disk — exactly as under POSIX.
//!
//! # Divergence from GNU `dirname`
//!
//! TAIRiX paths may be anchored to a named alias root (`Home:/tools`,
//! `plans/DRIVES.md`) as well as to the view root `/`. An alias root is
//! treated the way POSIX treats `/`: it is never stripped into, so
//! `dirname Home:/tools` is `Home:/` exactly as `dirname /tools` is `/`.
//! Where the root prefix ends is decided by the shared path grammar's own
//! rule (`tairix_path::dir_name`, over `tairix_path::alias_root_len`) — this
//! tool carries no second path parser. Documented in the tool's `Help/` documents.
//!
//! # What this crate is
//!
//! The pure, host-testable core of the tool:
//!
//! * [`parse`] — the [`Command`] a command line names, with the GNU
//!   option surface (`-z`/`--zero`, `NAME...`).
//! * [`output`] — the rendered result block for a parsed command.
//!
//! # Layering & safety
//!
//! `no_std` (with `alloc`). It links only `lib/*` crates — `lib/path`,
//! `lib/util`'s command-line scanner, and the `lib/help` engine used by the
//! `Run` binary — never a kernel or driver crate. No `unsafe`, and no
//! `unwrap`/`expect`/`panic!` in production paths; nothing writes to fd 3
//! (`stdinfo`).

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

use tairix_path::dir_name;
use tairix_util::getopt::{Arg, Opt, Order, Refusal, Scan, Takes};

/// The usage banner the short-help switches print when `dirname`'s own
/// Help tree is unavailable.
pub const USAGE: &str = "usage: dirname [-z] name...";

/// One thing the `dirname` tool can do.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Command {
    /// Print the directory name of each spelling in `names`.
    Emit {
        /// The path spellings to take the directory name of, in
        /// command-line order. Never empty.
        names: Vec<String>,
        /// Terminate each result with NUL instead of newline (`-z`).
        zero: bool,
    },
    /// Render `dirname`'s own short help (`-h`/`-?`/`--help`): the
    /// `NAME`, `SYNOPSIS`, and compact `OPTIONS` of its Help document,
    /// through the same engine as any other command's short help
    /// (plans/APPS.md §4).
    Help,
}

/// The failures [`parse`] reports, mirroring the GNU tool's diagnostics.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DirnameError {
    /// An option `getopt` refuses, worded as it words it.
    Option(Refusal),
    /// No `name` operand was given.
    MissingOperand,
}

impl fmt::Display for DirnameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DirnameError::Option(refusal) => write!(f, "{refusal}"),
            DirnameError::MissingOperand => f.write_str("missing operand"),
        }
    }
}

/// What each of `dirname`'s options means.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Flag {
    Zero,
    Help,
}

/// `dirname`'s options, as GNU's `dirname` spells them, and the reserved
/// help switches.
const OPTIONS: &[Opt<Flag>] = &[
    Opt::flag('z', &["zero"], Flag::Zero),
    Opt::flag('h', &["help"], Flag::Help),
    Opt::letter('?', Takes::Nothing, Flag::Help),
];

/// Parse `args` (the tool's arguments, excluding the program name) into a
/// [`Command`].
///
/// The grammar is the GNU `dirname` surface, `dirname [-z] name...`, read as
/// GNU's `getopt_long` reads it ([`Scan`]): options anywhere among the
/// operands, clustered, a long one by any unambiguous prefix, `--` ending
/// them, and a lone `-` an operand.
///
/// * `-z` / `--zero` — terminate each result with NUL instead of newline.
/// * `-h` / `-?` / `--help` — the reserved short-help switches
///   (plans/APPS.md §4; they win immediately).
/// * at least one `name` operand is required; any number is accepted.
///
/// # Errors
///
/// * [`DirnameError::Option`] — an option `getopt` refuses, in its words.
/// * [`DirnameError::MissingOperand`] — no `name` was given.
pub fn parse(args: &[&str]) -> Result<Command, DirnameError> {
    let mut zero = false;
    let mut names: Vec<String> = Vec::new();

    for arg in Scan::new(OPTIONS, args, Order::Permute) {
        match arg.map_err(DirnameError::Option)? {
            Arg::Operand(name) => names.push(String::from(name)),
            Arg::Opt(Flag::Help, _) => return Ok(Command::Help),
            Arg::Opt(Flag::Zero, _) => zero = true,
        }
    }

    if names.is_empty() {
        return Err(DirnameError::MissingOperand);
    }
    Ok(Command::Emit { names, zero })
}

/// The rendered output for a [`Command::Emit`]: each name's directory
/// name followed by the terminator `-z` selects. The block is bounded by
/// the argument vector that produced it.
#[must_use]
pub fn output(names: &[String], zero: bool) -> Vec<u8> {
    let terminator = if zero { b'\0' } else { b'\n' };
    let mut out = Vec::new();
    for name in names {
        out.extend_from_slice(dir_name(name).as_bytes());
        out.push(terminator);
    }
    out
}

#[cfg(test)]
mod tests {
    extern crate std;

    use alloc::string::{String, ToString};
    use alloc::vec;

    use super::{output, parse, Command, DirnameError};

    fn emit(names: &[&str], zero: bool) -> Command {
        Command::Emit {
            names: names.iter().map(|&s| String::from(s)).collect(),
            zero,
        }
    }

    #[test]
    fn operands_are_collected() {
        assert_eq!(parse(&["/a/b"]), Ok(emit(&["/a/b"], false)));
        assert_eq!(parse(&["a", "b", "c"]), Ok(emit(&["a", "b", "c"], false)));
        // A lone `-` is an operand.
        assert_eq!(parse(&["-"]), Ok(emit(&["-"], false)));
    }

    #[test]
    fn missing_operand() {
        assert_eq!(parse(&[]), Err(DirnameError::MissingOperand));
        assert_eq!(parse(&["-z"]), Err(DirnameError::MissingOperand));
    }

    #[test]
    fn zero_terminates_with_nul() {
        assert_eq!(parse(&["-z", "a"]), Ok(emit(&["a"], true)));
        assert_eq!(parse(&["--zero", "a"]), Ok(emit(&["a"], true)));
        // The GNU permutation: options are recognised after operands too.
        assert_eq!(parse(&["a", "-z"]), Ok(emit(&["a"], true)));
        // A bundle of known flags is accepted.
        assert_eq!(parse(&["-zz", "a"]), Ok(emit(&["a"], true)));
    }

    #[test]
    fn double_dash_ends_options() {
        assert_eq!(parse(&["--", "-z"]), Ok(emit(&["-z"], false)));
    }

    #[test]
    fn help_flags_win() {
        assert_eq!(parse(&["-h"]), Ok(Command::Help));
        assert_eq!(parse(&["-?"]), Ok(Command::Help));
        assert_eq!(parse(&["--help"]), Ok(Command::Help));
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
        assert_eq!(refused(&["-zx", "a"]), "invalid option -- 'x'");
        assert_eq!(
            refused(&["--zero=x", "a"]),
            "option '--zero' doesn't allow an argument"
        );
    }

    /// GNU 9.4 reads `dirname --z a` as `dirname --zero a`.
    #[test]
    fn a_long_option_may_be_abbreviated() {
        assert_eq!(parse(&["--z", "a"]), Ok(emit(&["a"], true)));
        assert_eq!(parse(&["--he"]), Ok(Command::Help));
    }

    #[test]
    fn output_renders_each_name_with_its_terminator() {
        let names = vec![String::from("/a/x"), String::from("b/y")];
        assert_eq!(output(&names, false), b"/a\nb\n");
        assert_eq!(output(&names, true), b"/a\0b\0");
    }

    /// Every locale's `OPTIONS` section documents exactly the switches this
    /// parser accepts (`plans/APPS.md` §3.1): the flag tokens are
    /// language-neutral, so each translated document must carry the same
    /// keys as the canonical one. The documents are read from the bundle's
    /// own on-disk `Help/` tree — the single source the image builder
    /// plants — never a copy embedded in this crate.
    #[test]
    fn help_documents_the_parser_switches() {
        use alloc::format;
        use std::fs;

        let help_root = format!("{}/Help", env!("CARGO_MANIFEST_DIR"));
        let locales = tairix_help::REQUIRED_LOCALES;
        for locale in locales {
            let path = format!("{help_root}/{locale}/dirname.md");
            let text = fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"));
            for switch in ["`-z, --zero`", "`-h, -?`"] {
                assert!(
                    text.contains(switch),
                    "{locale}/dirname.md must document {switch}"
                );
            }
        }
    }
}
