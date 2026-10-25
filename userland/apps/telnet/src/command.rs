//! The `telnet` argument grammar.
//!
//! The option set and spellings track inetutils `telnet` so a user who knows
//! that tool finds these already familiar. The switches inetutils defines for
//! facilities TAIRiX does not have — `-n tracefile` (no trace file), `-r`
//! (no rlogin), `-c` (no `.telnetrc`, since TAIRiX has no `/etc`) — are
//! deliberately absent rather than accepted and ignored: a switch that silently
//! does nothing is worse than one that is honestly unknown.

use alloc::string::{String, ToString};
use core::fmt;

use tairix_abi::net_ipc::NetAddrFamily;
use tairix_util::getopt::{Arg, Opt, Order, Refusal, Scan, Takes};
use tairix_util::quote::quote;

/// The default port when the command line names none: the assigned telnet port.
pub const DEFAULT_PORT: u16 = 23;

/// The default escape character, `^]`, as BSD telnet uses.
pub const DEFAULT_ESCAPE: u8 = 0x1D;

/// The remote endpoint an invocation or an `open` command names.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Target {
    /// The host as the user spelled it — a literal address or a name to
    /// resolve. Kept verbatim so every diagnostic names what was asked for.
    pub host: String,
    /// The TCP port.
    pub port: u16,
}

/// The operating parameters an invocation sets up.
// The bools transcribe inetutils `telnet`'s independent switches one-to-one
// (the `ss`/`df` Options precedent); folding them into state enums would
// obscure the grammar they mirror.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Config {
    /// Restrict resolution to one address family (`-4` / `-6`).
    pub family: Option<NetAddrFamily>,
    /// Request BINARY on the receive side (`-8`).
    pub binary_in: bool,
    /// Request BINARY on the transmit side (`-8` / `-L`).
    pub binary_out: bool,
    /// The escape character, or [`None`] when `-E` disabled it.
    pub escape: Option<u8>,
    /// Disclose the login name over NEW-ENVIRON (`-a` / `-l`).
    pub auto_login: bool,
    /// The login name to disclose (`-l`); with `-a` alone it comes from the
    /// session's `USER`.
    pub user: Option<String>,
    /// A local address to bind before connecting (`-b`).
    pub bind: Option<String>,
    /// Trace option negotiation on standard error (`-d`).
    pub debug: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            family: None,
            binary_in: false,
            binary_out: false,
            escape: Some(DEFAULT_ESCAPE),
            auto_login: false,
            user: None,
            bind: None,
            debug: false,
        }
    }
}

/// A parsed `telnet` invocation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Command {
    /// Render `telnet`'s own short help (`-?`/`-h`/`--help`).
    Help,
    /// Start a session. With no [`Target`] the tool opens in command mode, as
    /// a bare `telnet` does, and the operator connects with `open`.
    Session {
        /// The operating parameters.
        config: Config,
        /// The host to connect to at once, if one was named.
        target: Option<Target>,
    },
}

/// Why an argument vector was not understood.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ParseError {
    /// An option `getopt` refuses, worded as it words it.
    Option(Refusal),
    /// The port operand is not a number in `1..=65535`. TAIRiX has no services
    /// database, so a service *name* cannot be looked up.
    BadPort(String),
    /// More operands than `host [port]`.
    TooManyOperands,
    /// The `-e` value is not a single character or `^X` control spelling.
    BadEscape(String),
    /// `-4` and `-6` were both given.
    FamilyConflict,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Option(refusal) => write!(f, "{refusal}"),
            Self::BadPort(port) => write!(
                f,
                "{port} is not a port number (a service name cannot be resolved)",
                port = quote(port)
            ),
            Self::TooManyOperands => f.write_str("only 'host [port]' may be given"),
            Self::BadEscape(value) => write!(
                f,
                "{value} is not an escape character (give one character, or '^X')",
                value = quote(value)
            ),
            Self::FamilyConflict => f.write_str("'-4' and '-6' cannot both be given"),
        }
    }
}

/// Decode an escape-character spelling.
///
/// An empty value means "no escape character" (the `-E` spelling in value
/// form). `^X` is the control character `X` names, `^?` is Delete, and a single
/// byte is itself. Returns [`None`] for anything else, which the caller reports
/// rather than guessing at.
#[must_use]
pub fn parse_escape(value: &str) -> Option<Option<u8>> {
    if value.is_empty() {
        return Some(None);
    }
    let bytes = value.as_bytes();
    match *bytes {
        [b'^', b'?'] => Some(Some(0x7F)),
        [b'^', letter] if letter.is_ascii() && letter >= 0x40 => Some(Some(letter & 0x1F)),
        [single] => Some(Some(single)),
        _ => None,
    }
}

/// What each of `telnet`'s options means.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Flag {
    Help,
    Ipv4,
    Ipv6,
    Binary,
    EightBitOutput,
    NoEscape,
    Login,
    Debug,
    Escape,
    User,
    Bind,
}

/// `telnet`'s options, as inetutils `telnet` spells them, and the reserved
/// help switches.
const OPTIONS: &[Opt<Flag>] = &[
    Opt::flag('h', &["help"], Flag::Help),
    Opt::letter('?', Takes::Nothing, Flag::Help),
    Opt::flag('4', &["ipv4"], Flag::Ipv4),
    Opt::flag('6', &["ipv6"], Flag::Ipv6),
    Opt::flag('8', &["binary"], Flag::Binary),
    Opt::flag('L', &["eight-bit-output"], Flag::EightBitOutput),
    Opt::flag('E', &["no-escape"], Flag::NoEscape),
    Opt::flag('a', &["login"], Flag::Login),
    Opt::flag('d', &["debug"], Flag::Debug),
    Opt::valued('e', &["escape"], Flag::Escape),
    Opt::valued('l', &["user"], Flag::User),
    Opt::valued('b', &["bind"], Flag::Bind),
];

/// Parse a `telnet` argument vector.
///
/// Read as GNU's `getopt_long` reads it ([`Scan`]): options anywhere among the
/// operands, clustered, a value attached (`-e^]`, `--escape=^]`) or the next
/// argument, a long option by any unambiguous prefix, `--` ending them. `-?`,
/// `-h` and `--help` ask for the short help, answered once the whole line is
/// read.
///
/// # Errors
///
/// * [`ParseError::Option`] — an option `getopt` refuses, in its words.
/// * [`ParseError::BadEscape`] — an `-e` value that names no escape character.
/// * [`ParseError::BadPort`] — a port operand that is not a port number.
/// * [`ParseError::TooManyOperands`] — more operands than `host [port]`.
/// * [`ParseError::FamilyConflict`] — `-4` with `-6`, and no help asked for.
pub fn parse(args: &[&str]) -> Result<Command, ParseError> {
    let mut config = Config::default();
    let mut host: Option<String> = None;
    let mut port: Option<u16> = None;
    let mut want_help = false;
    let mut saw_v4 = false;
    let mut saw_v6 = false;

    for arg in Scan::new(OPTIONS, args, Order::Permute) {
        match arg.map_err(ParseError::Option)? {
            Arg::Operand(word) => {
                if host.is_none() {
                    host = Some(word.to_string());
                } else if port.is_none() {
                    port = Some(parse_port(word)?);
                } else {
                    return Err(ParseError::TooManyOperands);
                }
            }
            Arg::Opt(Flag::Help, _) => want_help = true,
            Arg::Opt(Flag::Ipv4, _) => saw_v4 = true,
            Arg::Opt(Flag::Ipv6, _) => saw_v6 = true,
            Arg::Opt(flag, value) => config.set(flag, value.unwrap_or_default())?,
        }
    }

    if want_help {
        return Ok(Command::Help);
    }
    if saw_v4 && saw_v6 {
        return Err(ParseError::FamilyConflict);
    }
    if saw_v4 {
        config.family = Some(NetAddrFamily::V4);
    } else if saw_v6 {
        config.family = Some(NetAddrFamily::V6);
    }
    // A port is only ever taken as the *second* operand, so it cannot exist
    // without a host to attach it to.
    let target = host.map(|host| Target {
        host,
        port: port.unwrap_or(DEFAULT_PORT),
    });
    Ok(Command::Session { config, target })
}

/// Parse a port operand: a decimal number in `1..=65535`.
///
/// # Errors
///
/// [`ParseError::BadPort`] for anything else, including a service name — there
/// is no services database to look one up in, and silently defaulting to 23
/// would connect somewhere the user did not ask for.
pub fn parse_port(text: &str) -> Result<u16, ParseError> {
    match text.parse::<u16>() {
        Ok(port) if port > 0 => Ok(port),
        _ => Err(ParseError::BadPort(text.to_string())),
    }
}

impl Config {
    /// Apply the option `flag` names, with the value it took.
    fn set(&mut self, flag: Flag, value: &str) -> Result<(), ParseError> {
        match flag {
            Flag::Binary => {
                self.binary_in = true;
                self.binary_out = true;
            }
            Flag::EightBitOutput => self.binary_out = true,
            Flag::NoEscape => self.escape = None,
            Flag::Login => self.auto_login = true,
            Flag::Debug => self.debug = true,
            Flag::Escape => {
                self.escape =
                    parse_escape(value).ok_or_else(|| ParseError::BadEscape(value.to_string()))?;
            }
            Flag::User => {
                self.user = Some(value.to_string());
                self.auto_login = true;
            }
            Flag::Bind => self.bind = Some(value.to_string()),
            Flag::Help | Flag::Ipv4 | Flag::Ipv6 => {}
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
