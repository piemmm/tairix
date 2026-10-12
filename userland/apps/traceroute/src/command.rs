//! The `traceroute` option grammar: the familiar Linux `traceroute`'s, for the
//! one method TAIRiX probes with — ICMP echo.

use alloc::format;
use alloc::string::{String, ToString};
use core::fmt;
use core::num::NonZeroU8;

use tairix_abi::net::SOCKET_MAX_DATAGRAM;
use tairix_abi::net_ipc::NetAddrFamily;
use tairix_net::trace::{MAX_IN_FLIGHT, MAX_PROBES_PER_HOP};
use tairix_util::argv::option_value;
use tairix_util::seconds::parse_seconds;

/// Hops probed by default.
pub const DEFAULT_MAX_HOPS: u8 = 30;
/// Probes per hop by default.
pub const DEFAULT_QUERIES: u8 = 3;
/// Probes outstanding at once by default.
pub const DEFAULT_SIM_QUERIES: u8 = 16;
/// How long a probe is awaited by default.
pub const DEFAULT_WAIT_NS: u64 = 5 * tairix_abi::time::NANOS_PER_SEC as u64;

/// What the command line asked for.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Command {
    /// Render the tool's own short help.
    Help,
    /// Trace the route to a host.
    Run(Config),
}

/// Everything a trace needs, straight from the option grammar.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Config {
    /// The target operand, as typed.
    pub host: String,
    /// The family `-4`/`-6` forced, if either.
    pub family: Option<NetAddrFamily>,
    /// The hop limit the first probes carry (`-f`).
    pub first_hop: NonZeroU8,
    /// The highest hop limit probed (`-m`).
    pub max_hops: NonZeroU8,
    /// Probes per hop (`-q`).
    pub queries: NonZeroU8,
    /// Probes outstanding at once (`-N`).
    pub sim_queries: NonZeroU8,
    /// How long each probe is awaited (`-w`).
    pub wait_ns: u64,
    /// Numeric output (`-n`): no reverse lookup of any hop.
    pub numeric: bool,
    /// The total probe packet length asked for, or [`None`] for the family's
    /// default.
    pub packet_len: Option<usize>,
}

/// Why an argument vector was not a valid `traceroute` invocation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ParseError {
    /// An option that is not part of the surface.
    UnknownOption(String),
    /// A value-taking option was given without its value.
    MissingValue(String),
    /// A value did not parse, or lies outside its range.
    InvalidValue {
        /// The option, or `packetlen`.
        option: String,
        /// The rejected value.
        value: String,
    },
    /// An option selecting a probe method other than ICMP echo.
    UnsupportedMethod(String),
    /// No target was given.
    MissingTarget,
    /// More operands than a target and a packet length.
    ExtraOperand(String),
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownOption(opt) => write!(f, "unrecognized option '{opt}'"),
            Self::MissingValue(opt) => write!(f, "option '{opt}' requires a value"),
            Self::InvalidValue { option, value } => {
                write!(f, "invalid value '{value}' for '{option}'")
            }
            Self::UnsupportedMethod(opt) => write!(
                f,
                "option '{opt}' selects a probe method this traceroute does not send; \
                 it probes with ICMP echo (-I)"
            ),
            Self::MissingTarget => f.write_str("no target host given"),
            Self::ExtraOperand(word) => write!(f, "unexpected extra operand '{word}'"),
        }
    }
}

/// Accumulated option state during parsing.
struct Builder {
    family: Option<NetAddrFamily>,
    first_hop: u8,
    max_hops: u8,
    queries: u8,
    sim_queries: u8,
    wait_ns: u64,
    numeric: bool,
    host: Option<String>,
    packet_len: Option<usize>,
}

impl Default for Builder {
    fn default() -> Self {
        Self {
            family: None,
            first_hop: 1,
            max_hops: DEFAULT_MAX_HOPS,
            queries: DEFAULT_QUERIES,
            sim_queries: DEFAULT_SIM_QUERIES,
            wait_ns: DEFAULT_WAIT_NS,
            numeric: false,
            host: None,
            packet_len: None,
        }
    }
}

/// Parse an argument vector (without the program name).
///
/// # Errors
///
/// A [`ParseError`] naming the offending argument; the caller reports it with
/// the usage banner and exits `2`.
pub fn parse(args: &[&str]) -> Result<Command, ParseError> {
    let mut builder = Builder::default();
    let mut options_ended = false;
    let mut i = 0;
    while i < args.len() {
        let arg = args[i];
        i += 1;
        if options_ended || arg == "-" || !arg.starts_with('-') {
            operand(&mut builder, arg)?;
        } else if arg == "--" {
            options_ended = true;
        } else if let Some(long) = arg.strip_prefix("--") {
            if parse_long(&mut builder, long, arg, args, &mut i)? {
                return Ok(Command::Help);
            }
        } else if parse_short(&mut builder, &arg[1..], args, &mut i)? {
            return Ok(Command::Help);
        }
    }
    build(builder)
}

/// Parse one `--long` option, answering whether it asked for help.
fn parse_long(
    builder: &mut Builder,
    long: &str,
    original: &str,
    args: &[&str],
    i: &mut usize,
) -> Result<bool, ParseError> {
    let (key, inline) = match long.split_once('=') {
        Some((key, value)) => (key, Some(value)),
        None => (long, None),
    };
    let flag = match key {
        "help" => return Ok(true),
        "icmp" => return Ok(false),
        "udp" | "tcp" | "module" => {
            return Err(ParseError::UnsupportedMethod(original.to_string()))
        }
        "first" => 'f',
        "max-hops" => 'm',
        "queries" => 'q',
        "sim-queries" => 'N',
        "wait" => 'w',
        _ => return Err(ParseError::UnknownOption(original.to_string())),
    };
    let value = option_value(inline, args, i)
        .ok_or_else(|| ParseError::MissingValue(format!("--{key}")))?;
    apply_value(builder, flag, &format!("--{key}"), value)?;
    Ok(false)
}

/// Parse one short-option cluster, answering whether it asked for help.
fn parse_short(
    builder: &mut Builder,
    flags: &str,
    args: &[&str],
    i: &mut usize,
) -> Result<bool, ParseError> {
    for (pos, flag) in flags.char_indices() {
        match flag {
            '?' => return Ok(true),
            '4' => builder.family = Some(NetAddrFamily::V4),
            '6' => builder.family = Some(NetAddrFamily::V6),
            'n' => builder.numeric = true,
            'I' => {}
            'U' | 'T' | 'M' => return Err(ParseError::UnsupportedMethod(format!("-{flag}"))),
            'f' | 'm' | 'q' | 'N' | 'w' => {
                let rest = &flags[pos + flag.len_utf8()..];
                let attached = (!rest.is_empty()).then_some(rest);
                let value = option_value(attached, args, i)
                    .ok_or_else(|| ParseError::MissingValue(format!("-{flag}")))?;
                apply_value(builder, flag, &format!("-{flag}"), value)?;
                return Ok(false);
            }
            other => return Err(ParseError::UnknownOption(format!("-{other}"))),
        }
    }
    Ok(false)
}

/// Apply a value-taking option, named `option` in any refusal.
fn apply_value(
    builder: &mut Builder,
    flag: char,
    option: &str,
    value: &str,
) -> Result<(), ParseError> {
    let invalid = || ParseError::InvalidValue {
        option: option.to_string(),
        value: value.to_string(),
    };
    match flag {
        'f' => builder.first_hop = bounded(value, u8::MAX).ok_or_else(invalid)?,
        'm' => builder.max_hops = bounded(value, u8::MAX).ok_or_else(invalid)?,
        'q' => builder.queries = bounded(value, MAX_PROBES_PER_HOP).ok_or_else(invalid)?,
        'N' => builder.sim_queries = bounded(value, MAX_IN_FLIGHT).ok_or_else(invalid)?,
        'w' => {
            builder.wait_ns = parse_seconds(value)
                .filter(|&wait| wait > 0)
                .ok_or_else(invalid)?;
        }
        _ => return Err(invalid()),
    }
    Ok(())
}

/// `value` as a count in `1..=max`.
fn bounded(value: &str, max: u8) -> Option<u8> {
    value.parse::<u8>().ok().filter(|&n| (1..=max).contains(&n))
}

/// Record the target, then the packet length; refuse a third operand.
fn operand(builder: &mut Builder, word: &str) -> Result<(), ParseError> {
    if builder.host.is_none() {
        builder.host = Some(word.to_string());
    } else if builder.packet_len.is_none() {
        let len = word
            .parse::<usize>()
            .ok()
            .filter(|&len| len <= SOCKET_MAX_DATAGRAM)
            .ok_or_else(|| ParseError::InvalidValue {
                option: String::from("packetlen"),
                value: word.to_string(),
            })?;
        builder.packet_len = Some(len);
    } else {
        return Err(ParseError::ExtraOperand(word.to_string()));
    }
    Ok(())
}

fn build(builder: Builder) -> Result<Command, ParseError> {
    let host = builder.host.ok_or(ParseError::MissingTarget)?;
    let hop = |value: u8, option: &str| {
        NonZeroU8::new(value).ok_or_else(|| ParseError::InvalidValue {
            option: option.to_string(),
            value: format!("{value}"),
        })
    };
    let first_hop = hop(builder.first_hop, "-f")?;
    let max_hops = hop(builder.max_hops, "-m")?;
    if first_hop > max_hops {
        return Err(ParseError::InvalidValue {
            option: String::from("-f"),
            value: format!("{first_hop}"),
        });
    }
    Ok(Command::Run(Config {
        host,
        family: builder.family,
        first_hop,
        max_hops,
        queries: hop(builder.queries, "-q")?,
        sim_queries: hop(builder.sim_queries, "-N")?,
        wait_ns: builder.wait_ns,
        numeric: builder.numeric,
        packet_len: builder.packet_len,
    }))
}

#[cfg(test)]
#[path = "command_tests.rs"]
mod tests;
