//! TAIRiX `clear` — clear the terminal screen (`plans/APPS.md`).
//!
//! The TAIRiX counterpart of the ncurses `clear` tool: it writes the byte
//! sequence that moves the cursor home and erases the display, and nothing
//! else. Which bytes those are is decided by the inherited `TERM` through
//! the compiled-in capability database (`lib/termcap`), and the sequence is
//! encoded through the one shared escape vocabulary (`lib/vt`) — never a
//! hand-rolled escape string.
//!
//! # What this crate is
//!
//! The pure, host-testable core of the tool:
//!
//! * [`parse`] — the [`Command`] a command line names.
//! * [`clear_bytes`] — the encoded clear sequence for a capability profile,
//!   or `None` when the terminal cannot clear (the dumb baseline), so the
//!   caller fails honestly instead of emitting sequences the terminal does
//!   not understand.
//!
//! # Divergence from ncurses `clear`
//!
//! GNU/ncurses `clear -x` skips the scrollback-erase (`E3`) extension. A
//! TAIRiX console keeps no scrollback, so there is nothing to skip: `-x` is
//! accepted for script compatibility and the output is identical with and
//! without it. Documented in the tool's `Help/` documents.
//!
//! # Layering & safety
//!
//! `no_std` (with `alloc`). It links only `lib/*` crates — `lib/termcap`
//! and `lib/vt` — never a kernel or driver crate. No `unsafe`, and no
//! `unwrap`/`expect`/`panic!` in production paths; nothing writes to fd 3
//! (`stdinfo`).

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

use tairix_termcap::Capabilities;
use tairix_util::getopt::{Arg, Opt, Order, Refusal, Scan, Takes};
use tairix_util::quote::quote;
use tairix_vt::{encode_all_into, EraseMode, Op};

/// The usage banner the short-help switches print when `clear`'s own Help
/// tree is unavailable.
pub const USAGE: &str = "usage: clear [-x] [-h | -?]";

/// One thing the `clear` tool can do.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Command {
    /// Clear the screen.
    Run,
    /// Render `clear`'s own short help (`-h`/`-?`/`--help`): the `NAME`,
    /// `SYNOPSIS`, and compact `OPTIONS` of its Help document, through the
    /// same engine as any other command's short help (plans/APPS.md).
    Help,
}

/// Why [`parse`] refused a command line.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ClearError {
    /// An option `getopt` refuses, worded as it words it.
    Option(Refusal),
    /// An operand; the tool takes none (GNU `extra operand`).
    ExtraOperand(String),
}

impl fmt::Display for ClearError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Option(refusal) => write!(f, "{refusal}"),
            Self::ExtraOperand(operand) => {
                write!(f, "extra operand {operand}", operand = quote(operand))
            }
        }
    }
}

/// What each of `clear`'s options means.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Flag {
    KeepScrollback,
    Help,
}

/// `clear`'s option, and the reserved help switches.
const OPTIONS: &[Opt<Flag>] = &[
    Opt::letter('x', Takes::Nothing, Flag::KeepScrollback),
    Opt::flag('h', &["help"], Flag::Help),
    Opt::letter('?', Takes::Nothing, Flag::Help),
];

/// Parse `args` (the tool's arguments, excluding the program name) into a
/// [`Command`].
///
/// The grammar is `clear [-x] [-h | -?]`, read as GNU's `getopt_long` reads it
/// ([`Scan`]). The help switches (`-h`, `-?`, `--help`) answer at once. `-x`
/// is accepted, as often as it is given, for GNU/ncurses compatibility ("do
/// not clear the scrollback"); a TAIRiX console keeps no scrollback, so the
/// behaviour is identical with and without it. The tool takes no operands.
///
/// # Errors
///
/// * [`ClearError::Option`] — an option `getopt` refuses, in its words.
/// * [`ClearError::ExtraOperand`] — an operand.
pub fn parse(args: &[&str]) -> Result<Command, ClearError> {
    for arg in Scan::new(OPTIONS, args, Order::Permute) {
        match arg.map_err(ClearError::Option)? {
            Arg::Opt(Flag::Help, _) => return Ok(Command::Help),
            Arg::Opt(Flag::KeepScrollback, _) => {}
            Arg::Operand(operand) => return Err(ClearError::ExtraOperand(String::from(operand))),
        }
    }
    Ok(Command::Run)
}

/// The encoded clear sequence for the capability profile `caps`: move the
/// cursor home, then erase the whole display — the shared `lib/vt`
/// operations the profile's terminal accepts.
///
/// Returns `None` when the terminal can neither address the cursor nor
/// erase (the fail-closed dumb baseline an unknown `TERM` degrades to):
/// there is no sequence such a terminal understands, so the caller reports
/// the failure honestly rather than writing bytes that would print as
/// garbage.
#[must_use]
pub fn clear_bytes(caps: &Capabilities) -> Option<Vec<u8>> {
    if !(caps.cursor_addressing && caps.erase) {
        return None;
    }
    let mut bytes = Vec::new();
    encode_all_into(
        &[
            Op::CursorPosition { row: 1, col: 1 },
            Op::EraseInDisplay(EraseMode::All),
        ],
        &mut bytes,
    );
    Some(bytes)
}

#[cfg(test)]
mod tests {
    extern crate std;

    use alloc::string::{String, ToString};

    use tairix_termcap::TermType;

    use super::{clear_bytes, parse, ClearError, Command};

    #[test]
    fn no_arguments_clears() {
        assert_eq!(parse(&[]), Ok(Command::Run));
    }

    #[test]
    fn dash_x_is_accepted() {
        // `-x` is the GNU "keep the scrollback" switch; a TAIRiX console
        // keeps none, so it parses to the same command.
        assert_eq!(parse(&["-x"]), Ok(Command::Run));
        // ncurses `clear` takes a repeated `-x` as readily as a single one.
        assert_eq!(parse(&["-x", "-x"]), Ok(Command::Run));
    }

    #[test]
    fn help_flags_win() {
        assert_eq!(parse(&["-h"]), Ok(Command::Help));
        assert_eq!(parse(&["-?"]), Ok(Command::Help));
        assert_eq!(parse(&["--help"]), Ok(Command::Help));
        assert_eq!(parse(&["-x", "-h"]), Ok(Command::Help));
    }

    #[test]
    fn an_operand_is_an_extra_operand() {
        let err = parse(&["operand"]).expect_err("an operand is refused");
        assert_eq!(err, ClearError::ExtraOperand(String::from("operand")));
        assert_eq!(err.to_string(), "extra operand 'operand'");
    }

    /// GNU's words for each refusal.
    fn refused(args: &[&str]) -> String {
        match parse(args) {
            Err(ClearError::Option(refusal)) => refusal.to_string(),
            other => panic!("expected a refused option, got {other:?}"),
        }
    }

    #[test]
    fn a_refused_option_is_worded_as_getopt_words_it() {
        assert_eq!(refused(&["-y"]), "invalid option -- 'y'");
        assert_eq!(refused(&["--frob"]), "unrecognized option '--frob'");
        assert_eq!(
            refused(&["--help=1"]),
            "option '--help' doesn't allow an argument"
        );
    }

    #[test]
    fn a_long_option_may_be_abbreviated() {
        assert_eq!(parse(&["--he"]), Ok(Command::Help));
    }

    #[test]
    fn clear_bytes_emits_home_then_erase_on_a_capable_terminal() {
        for term in [TermType::Xterm256Color, TermType::Vt100] {
            let bytes = clear_bytes(&term.capabilities()).expect("terminal can clear");
            // `CUP` home then `ED` all: the ncurses `clear` sequence,
            // through the shared vocabulary.
            assert_eq!(bytes, b"\x1b[1;1H\x1b[2J");
        }
    }

    #[test]
    fn clear_bytes_refuses_the_dumb_baseline() {
        // An unknown `TERM` degrades to `Dumb`, which cannot clear: the
        // tool must fail honestly, never print escape garbage.
        assert_eq!(clear_bytes(&TermType::Dumb.capabilities()), None);
    }

    /// Every locale's `OPTIONS` section documents exactly the switches this
    /// parser accepts (`plans/APPS.md`): the flag tokens are
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
            let path = format!("{help_root}/{locale}/clear.md");
            let text = fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"));
            for switch in ["`-x`", "`-h, -?`"] {
                assert!(
                    text.contains(switch),
                    "{locale}/clear.md must document {switch}"
                );
            }
        }
    }
}
