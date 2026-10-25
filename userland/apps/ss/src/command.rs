//! The `ss` command shape and its parser.
//!
//! The option surface follows iproute2 `ss`: `-t`/`-u` select the TCP and
//! UDP protocol families (both when neither is given), `-a` shows both
//! listening and connected sockets, `-l` shows only listening sockets
//! (the default hides them), `-r` resolves addresses to host names over
//! DNS, `-n` keeps *service* names numeric (TAIRiX has no service-name
//! database, so ports are always numeric and the switch is accepted for
//! familiarity), `-p` adds the owning-process column, `-4`/`-6` restrict
//! the family, `-H` suppresses the header line, and `-s` prints the
//! stack-wide connection-defence summary instead of the socket table. Short
//! help is the reserved `-?`/`--help` pair (plans/APPS.md §4).
//!
//! `-r` and `-n` are independent, exactly as in iproute2: `-n` governs
//! service names and `-r` governs host names, so `ss -rn` resolves hosts
//! and leaves ports numeric.

use alloc::string::{String, ToString};
use core::fmt;
use tairix_util::getopt::{Arg, Opt, Order, Refusal, Scan};
use tairix_util::quote::quote;

/// A parsed `ss` invocation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Command {
    /// Render `ss`'s own short help (`-?`/`--help`).
    Help,
    /// List the open sockets the filters select.
    Report(Options),
    /// Print the stack-wide TCP connection-defence summary (`-s`): the
    /// SYN-backlog, SYN-cookie, accept-queue, and reset totals a flood in
    /// progress shows up in.
    Summary,
}

/// Everything an `ss` run needs, straight from the option grammar.
// The bools mirror iproute2 `ss`'s independent switches one-to-one (the
// `df`/`ls` Options precedent); folding them into state enums would obscure
// the grammar they transcribe.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Options {
    /// Show TCP sockets (`-t`). With neither `-t` nor `-u`, both show.
    pub tcp: bool,
    /// Show UDP sockets (`-u`). With neither `-t` nor `-u`, both show.
    pub udp: bool,
    /// Show both listening and connected sockets (`-a`).
    pub all: bool,
    /// Show only listening sockets (`-l`).
    pub listening: bool,
    /// Numeric *service* names (`-n`). Always in force on TAIRiX (there is
    /// no service-name database); accepted for familiarity. Host-name
    /// resolution is `resolve`'s job, not this switch's.
    pub numeric: bool,
    /// Resolve addresses to host names over DNS (`-r`). Off by default, so
    /// the listing puts no query on the wire unless asked.
    pub resolve: bool,
    /// Add the owning-process column (`-p`).
    pub processes: bool,
    /// Restrict to IPv4 sockets (`-4`). With neither, both families show.
    pub ipv4: bool,
    /// Restrict to IPv6 sockets (`-6`). With neither, both families show.
    pub ipv6: bool,
    /// Suppress the header line (`-H`).
    pub no_header: bool,
}

/// Why an argument vector was not a valid `ss` invocation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ParseError {
    /// An option `getopt` refuses, worded as it words it.
    Option(Refusal),
    /// A positional operand: `ss` takes options only (its filter-expression
    /// grammar is not implemented), so a bare word is a usage error rather
    /// than a silently ignored argument.
    UnexpectedOperand(String),
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Option(refusal) => write!(f, "{refusal}"),
            Self::UnexpectedOperand(word) => {
                write!(f, "unexpected operand {word}", word = quote(word))
            }
        }
    }
}

/// What each of `ss`'s options means.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Flag {
    Tcp,
    Udp,
    All,
    Listening,
    Numeric,
    Resolve,
    Processes,
    Ipv4,
    Ipv6,
    NoHeader,
    Summary,
    Help,
}

/// `ss`'s options, as iproute2 `ss` spells them, and the reserved help
/// switches.
const OPTIONS: &[Opt<Flag>] = &[
    Opt::flag('t', &["tcp"], Flag::Tcp),
    Opt::flag('u', &["udp"], Flag::Udp),
    Opt::flag('a', &["all"], Flag::All),
    Opt::flag('l', &["listening"], Flag::Listening),
    Opt::flag('n', &["numeric"], Flag::Numeric),
    Opt::flag('r', &["resolve"], Flag::Resolve),
    Opt::flag('p', &["processes"], Flag::Processes),
    Opt::flag('4', &["ipv4"], Flag::Ipv4),
    Opt::flag('6', &["ipv6"], Flag::Ipv6),
    Opt::flag('H', &["no-header"], Flag::NoHeader),
    Opt::flag('s', &["summary"], Flag::Summary),
    Opt::flag('?', &["help"], Flag::Help),
];

/// Parse an argument vector (without the program name).
///
/// Read as GNU's `getopt_long` reads it ([`Scan`]): options clustered, a long
/// one by any unambiguous prefix, `--` ending them. The help switches (`-?`,
/// `--help`) and `-s` answer at once.
///
/// # Errors
///
/// * [`ParseError::Option`] — an option `getopt` refuses, in its words.
/// * [`ParseError::UnexpectedOperand`] — any operand, before or after `--`:
///   `ss`'s filter-expression grammar is not implemented.
///
/// The caller reports each, then where the help is, and exits `1`.
pub fn parse(args: &[&str]) -> Result<Command, ParseError> {
    let mut options = Options::default();
    for arg in Scan::new(OPTIONS, args, Order::Permute) {
        match arg.map_err(ParseError::Option)? {
            Arg::Operand(word) => return Err(ParseError::UnexpectedOperand(word.to_string())),
            Arg::Opt(Flag::Help, _) => return Ok(Command::Help),
            Arg::Opt(Flag::Summary, _) => return Ok(Command::Summary),
            Arg::Opt(flag, _) => options.set(flag),
        }
    }
    Ok(Command::Report(options))
}

impl Options {
    /// Apply the switch `flag` names.
    fn set(&mut self, flag: Flag) {
        match flag {
            Flag::Tcp => self.tcp = true,
            Flag::Udp => self.udp = true,
            Flag::All => self.all = true,
            Flag::Listening => self.listening = true,
            Flag::Numeric => self.numeric = true,
            Flag::Resolve => self.resolve = true,
            Flag::Processes => self.processes = true,
            Flag::Ipv4 => self.ipv4 = true,
            Flag::Ipv6 => self.ipv6 = true,
            Flag::NoHeader => self.no_header = true,
            Flag::Summary | Flag::Help => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use alloc::string::{String, ToString};

    use tairix_util::getopt::Refusal;

    use super::{parse, Command, Options, ParseError};

    /// GNU's words for each refusal.
    fn refused(args: &[&str]) -> String {
        parse(args).expect_err("refused").to_string()
    }

    #[test]
    fn a_refused_option_is_worded_as_getopt_words_it() {
        assert_eq!(refused(&["-Q"]), "invalid option -- 'Q'");
        assert_eq!(refused(&["-tQ"]), "invalid option -- 'Q'");
        assert_eq!(refused(&["--frob"]), "unrecognized option '--frob'");
        assert_eq!(
            refused(&["--tcp=1"]),
            "option '--tcp' doesn't allow an argument"
        );
    }

    #[test]
    fn a_long_option_may_be_abbreviated() {
        let long = parse(&["--tc", "--list", "--proc"]).unwrap();
        assert_eq!(long, parse(&["-t", "-l", "-p"]).unwrap());
        assert_eq!(parse(&["--sum"]), Ok(Command::Summary));
        assert_eq!(parse(&["--he"]), Ok(Command::Help));
        assert_eq!(
            refused(&["--ipv"]),
            "option '--ipv' is ambiguous; possibilities: '--ipv4' '--ipv6'"
        );
    }

    #[test]
    fn no_arguments_reports_with_defaults() {
        assert_eq!(parse(&[]), Ok(Command::Report(Options::default())));
    }

    #[test]
    fn long_and_short_forms_agree() {
        let long = parse(&["--tcp", "--listening", "--processes"]).unwrap();
        let short = parse(&["-t", "-l", "-p"]).unwrap();
        assert_eq!(long, short);
        let Command::Report(options) = short else {
            panic!("report");
        };
        assert!(options.tcp && options.listening && options.processes);
        assert!(!options.udp && !options.all);
    }

    #[test]
    fn a_short_cluster_sets_every_flag() {
        let Command::Report(options) = parse(&["-tuan4"]).unwrap() else {
            panic!("report");
        };
        assert!(options.tcp && options.udp && options.all && options.numeric && options.ipv4);
    }

    #[test]
    fn help_wins_from_either_form() {
        assert_eq!(parse(&["--help"]), Ok(Command::Help));
        assert_eq!(parse(&["-a", "-?"]), Ok(Command::Help));
    }

    #[test]
    fn an_unknown_option_fails_closed() {
        assert_eq!(
            parse(&["-z"]),
            Err(ParseError::Option(Refusal::UnknownShort('z')))
        );
        assert_eq!(
            parse(&["--bogus"]),
            Err(ParseError::Option(Refusal::UnknownLong("--bogus".into())))
        );
    }

    #[test]
    fn an_operand_is_a_usage_error() {
        assert_eq!(
            parse(&["dst", ":80"]),
            Err(ParseError::UnexpectedOperand("dst".into()))
        );
        // `--` does not turn the next word into a valid operand.
        assert_eq!(
            parse(&["--", "state"]),
            Err(ParseError::UnexpectedOperand("state".into()))
        );
    }

    #[test]
    fn every_option_key_is_covered() {
        // Guards against a flag drifting out of the parser without its
        // Help OPTIONS row (and vice versa): the switch keys the help
        // document pins must all parse here.
        for arg in ["-t", "-u", "-a", "-l", "-n", "-p", "-4", "-6", "-H"] {
            assert!(matches!(parse(&[arg]), Ok(Command::Report(_))), "{arg}");
        }
        for arg in [
            "--tcp",
            "--udp",
            "--all",
            "--listening",
            "--numeric",
            "--processes",
            "--ipv4",
            "--ipv6",
            "--no-header",
        ] {
            assert!(matches!(parse(&[arg]), Ok(Command::Report(_))), "{arg}");
        }
        // `-s` selects a different report, not a filter, so it parses to
        // its own command rather than to `Report`.
        for arg in ["-s", "--summary"] {
            assert_eq!(parse(&[arg]), Ok(Command::Summary), "{arg}");
        }
    }
}
