//! The parsed shape of a `top` command line.

use tairix_util::getopt::{Arg, Opt, Order, Scan, Takes};

use crate::error::TopError;

/// The usage banner the short-help switches print when `top`'s own Help tree
/// is unavailable.
pub const USAGE: &str = "usage: top [-d secs.tenths] [-h | -?]";

/// The default refresh delay in tenths of a second: GNU `top`'s 3.0 s.
pub const DEFAULT_DELAY_TENTHS: u32 = 30;

/// The smallest accepted refresh delay, in tenths of a second.
///
/// GNU `top` accepts a zero delay and spins as fast as it can; TAIRiX never
/// busy-loops, so a requested delay below one tenth of a second is clamped
/// to it — a deliberate, documented divergence in the tool's Help. The
/// grammar (and this clamp) is the shared full-screen-viewer delay grammar
/// in `lib/curses`, so `top` and `sysmon` can never drift apart.
pub use tairix_curses::MIN_DELAY_TENTHS;

/// One thing the `top` tool can do.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Command {
    /// Run the live process viewer, auto-refreshing every `delay_tenths`
    /// tenths of a second (always at least [`MIN_DELAY_TENTHS`]).
    Run {
        /// The refresh interval in tenths of a second.
        delay_tenths: u32,
    },
    /// Render `top`'s own short help (`-h`/`-?`/`--help`): the `NAME`,
    /// `SYNOPSIS`, and compact `OPTIONS` of its Help document, through the
    /// same engine as any other command's short help (plans/APPS.md §4).
    Help,
}

/// What each of `top`'s options means.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Flag {
    Delay,
    Help,
}

/// `top`'s option, as GNU `top` spells it, and the reserved help switches.
const OPTIONS: &[Opt<Flag>] = &[
    Opt::valued('d', &["delay"], Flag::Delay),
    Opt::flag('h', &["help"], Flag::Help),
    Opt::letter('?', Takes::Nothing, Flag::Help),
];

/// Parse `args` (the tool's arguments, excluding the program name) into a
/// [`Command`].
///
/// The grammar is `top [-d secs.tenths] [-h | -?]`, read as GNU's
/// `getopt_long` reads it ([`Scan`]): options clustered, a value attached
/// (`-d5`, `--delay=5`) or the next argument, a long option by any
/// unambiguous prefix, `--` ending them.
///
/// * `-h` / `-?` / `--help` — the reserved short-help switches
///   (plans/APPS.md §4; they win immediately).
/// * `-d secs.tenths` / `--delay secs.tenths` — the refresh interval between
///   automatic redisplays, exactly GNU `top`'s `-d, --delay` spellings.
///   Seconds with an optional fraction; only the first fractional digit
///   (tenths) is kept, as in GNU `top`. A zero delay is clamped to the 0.1 s
///   minimum rather than busy-looping. The last one given wins.
/// * an operand — a [`TopError::Usage`] error. The viewer takes none; its
///   scope and navigation are keys pressed inside the session.
///
/// # Errors
///
/// * [`TopError::Option`] — an option `getopt` refuses, in its words.
/// * [`TopError::Usage`] — an operand, or a malformed delay value.
pub fn parse(args: &[&str]) -> Result<Command, TopError> {
    let mut delay_tenths = DEFAULT_DELAY_TENTHS;
    for arg in Scan::new(OPTIONS, args, Order::Permute) {
        match arg.map_err(TopError::Option)? {
            Arg::Operand(_) => return Err(TopError::Usage),
            Arg::Opt(Flag::Delay, value) => {
                delay_tenths = parse_delay_tenths(value.unwrap_or_default())?;
            }
            Arg::Opt(Flag::Help, _) => return Ok(Command::Help),
        }
    }
    Ok(Command::Run { delay_tenths })
}

/// Parse a `secs[.tenths…]` delay into whole tenths of a second through the
/// shared full-screen-viewer grammar
/// ([`tairix_curses::parse_delay_tenths`]), mapping its fail-closed `None`
/// onto this tool's usage error.
///
/// # Errors
///
/// [`TopError::Usage`] when the value is empty, carries a non-digit, has
/// more than one decimal point, or overflows the tenths counter.
fn parse_delay_tenths(value: &str) -> Result<u32, TopError> {
    tairix_curses::parse_delay_tenths(value).ok_or(TopError::Usage)
}

#[cfg(test)]
mod tests {
    use super::{parse, Command, DEFAULT_DELAY_TENTHS, MIN_DELAY_TENTHS};
    use crate::error::TopError;
    use alloc::string::{String, ToString};

    fn run(delay_tenths: u32) -> Command {
        Command::Run { delay_tenths }
    }

    #[test]
    fn no_arguments_runs_the_viewer_at_the_default_delay() {
        assert_eq!(parse(&[]), Ok(run(DEFAULT_DELAY_TENTHS)));
    }

    #[test]
    fn help_flags_win() {
        assert_eq!(parse(&["-h"]), Ok(Command::Help));
        assert_eq!(parse(&["-?"]), Ok(Command::Help));
        assert_eq!(parse(&["--help"]), Ok(Command::Help));
        // Help wins wherever it appears, exactly as GNU tools treat it.
        assert_eq!(parse(&["-d", "1", "-h"]), Ok(Command::Help));
    }

    #[test]
    fn delay_accepts_the_gnu_spellings() {
        assert_eq!(parse(&["-d", "5"]), Ok(run(50)));
        assert_eq!(parse(&["-d5"]), Ok(run(50)));
        assert_eq!(parse(&["--delay", "5"]), Ok(run(50)));
        assert_eq!(parse(&["--delay=5"]), Ok(run(50)));
    }

    #[test]
    fn delay_keeps_only_the_first_fractional_digit() {
        assert_eq!(parse(&["-d", "1.5"]), Ok(run(15)));
        assert_eq!(parse(&["-d", "1.59"]), Ok(run(15)));
        assert_eq!(parse(&["-d", ".5"]), Ok(run(5)));
        assert_eq!(parse(&["-d", "2."]), Ok(run(20)));
    }

    #[test]
    fn a_zero_delay_is_clamped_never_a_busy_loop() {
        assert_eq!(parse(&["-d", "0"]), Ok(run(MIN_DELAY_TENTHS)));
        assert_eq!(parse(&["-d", "0.0"]), Ok(run(MIN_DELAY_TENTHS)));
    }

    #[test]
    fn the_last_delay_wins() {
        assert_eq!(parse(&["-d", "1", "-d", "2"]), Ok(run(20)));
    }

    #[test]
    fn malformed_delays_are_usage_errors() {
        assert_eq!(refused(&["-d"]), "option requires an argument -- 'd'");
        assert_eq!(parse(&["-d", ""]), Err(TopError::Usage));
        assert_eq!(parse(&["-d", "."]), Err(TopError::Usage));
        assert_eq!(parse(&["-d", "abc"]), Err(TopError::Usage));
        assert_eq!(parse(&["-d", "1.2.3"]), Err(TopError::Usage));
        assert_eq!(parse(&["-d", "-1"]), Err(TopError::Usage));
        assert_eq!(parse(&["-d", "99999999999"]), Err(TopError::Usage));
    }

    #[test]
    fn an_operand_is_usage() {
        assert_eq!(parse(&["operand"]), Err(TopError::Usage));
    }

    /// GNU 9.4's words for each refusal.
    fn refused(args: &[&str]) -> String {
        parse(args).expect_err("refused").to_string()
    }

    #[test]
    fn a_refused_option_is_worded_as_getopt_words_it() {
        assert_eq!(refused(&["-x"]), "invalid option -- 'x'");
        assert_eq!(refused(&["--frob"]), "unrecognized option '--frob'");
        assert_eq!(
            refused(&["--delay"]),
            "option '--delay' requires an argument"
        );
        assert_eq!(
            refused(&["--help=x"]),
            "option '--help' doesn't allow an argument"
        );
    }

    /// `getopt_long` reads `top --del=5` as `top --delay=5`.
    #[test]
    fn a_long_option_may_be_abbreviated() {
        assert_eq!(parse(&["--del=5"]), Ok(run(50)));
        assert_eq!(parse(&["--he"]), Ok(Command::Help));
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
            let path = format!("{help_root}/{locale}/top.md");
            let text = fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"));
            for switch in ["`-d, --delay <seconds>`", "`-h, -?`"] {
                assert!(
                    text.contains(switch),
                    "{locale}/top.md must document {switch}"
                );
            }
        }
    }
}
