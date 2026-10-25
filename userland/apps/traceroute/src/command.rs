//! The `traceroute` option grammar: the familiar Linux `traceroute`'s, for the
//! one method TAIRiX probes with — ICMP echo.

use alloc::format;
use alloc::string::{String, ToString};
use core::fmt;
use core::num::NonZeroU8;

use tairix_abi::net::SOCKET_MAX_DATAGRAM;
use tairix_abi::net_ipc::NetAddrFamily;
use tairix_net::trace::{MAX_IN_FLIGHT, MAX_PROBES_PER_HOP};
use tairix_util::getopt::{Arg, Opt, Order, Refusal, Scan, Takes};
use tairix_util::quote::quote;
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
    /// An option `getopt` refuses, worded as it words it.
    Option(Refusal),
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
            Self::Option(refusal) => write!(f, "{refusal}"),
            Self::InvalidValue { option, value } => {
                write!(
                    f,
                    "invalid value {value} for {option}",
                    value = quote(value),
                    option = quote(option)
                )
            }
            Self::UnsupportedMethod(opt) => write!(
                f,
                "option {opt} selects a probe method this traceroute does not send; \
                 it probes with ICMP echo (-I)",
                opt = quote(opt)
            ),
            Self::MissingTarget => f.write_str("no target host given"),
            Self::ExtraOperand(word) => {
                write!(f, "unexpected extra operand {word}", word = quote(word))
            }
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

/// What each of `traceroute`'s options means.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Flag {
    Ipv4,
    Ipv6,
    Icmp,
    /// A probe method this traceroute does not send, by the letter it is
    /// refused under.
    OtherMethod(&'static str),
    First,
    MaxHops,
    Queries,
    SimQueries,
    Wait,
    Numeric,
    Help,
}

/// `traceroute`'s options, as Linux `traceroute` spells them, and the reserved
/// help switches.
const OPTIONS: &[Opt<Flag>] = &[
    Opt::letter('4', Takes::Nothing, Flag::Ipv4),
    Opt::letter('6', Takes::Nothing, Flag::Ipv6),
    Opt::flag('I', &["icmp"], Flag::Icmp),
    Opt::flag('U', &["udp"], Flag::OtherMethod("-U")),
    Opt::flag('T', &["tcp"], Flag::OtherMethod("-T")),
    Opt {
        short: Some('M'),
        long: &["module"],
        takes: Takes::Attached,
        means: Flag::OtherMethod("-M"),
    },
    Opt::valued('f', &["first"], Flag::First),
    Opt::valued('m', &["max-hops"], Flag::MaxHops),
    Opt::valued('q', &["queries"], Flag::Queries),
    Opt::valued('N', &["sim-queries"], Flag::SimQueries),
    Opt::valued('w', &["wait"], Flag::Wait),
    Opt::letter('n', Takes::Nothing, Flag::Numeric),
    Opt::flag('?', &["help"], Flag::Help),
];

/// Parse an argument vector (without the program name).
///
/// Read as GNU's `getopt_long` reads it ([`Scan`]): options anywhere among the
/// operands, clustered, a value attached or the next argument, a long option by
/// any unambiguous prefix, `--` ending them. The help switches (`-?`,
/// `--help`) answer at once. The operands are the target, then the packet
/// length.
///
/// # Errors
///
/// * [`ParseError::Option`] — an option `getopt` refuses, in its words.
/// * [`ParseError::UnsupportedMethod`] — `-U`, `-T` or `-M`: a probe method
///   other than ICMP echo.
/// * [`ParseError::InvalidValue`] — a value or packet length that does not
///   parse or lies outside its range, the option named by its letter, or a
///   first hop beyond the last.
/// * [`ParseError::MissingTarget`] — no target.
/// * [`ParseError::ExtraOperand`] — a third operand.
///
/// The caller reports each, then where the help is, and exits `1`.
pub fn parse(args: &[&str]) -> Result<Command, ParseError> {
    let mut builder = Builder::default();
    for arg in Scan::new(OPTIONS, args, Order::Permute) {
        match arg.map_err(ParseError::Option)? {
            Arg::Operand(word) => operand(&mut builder, word)?,
            Arg::Opt(Flag::Help, _) => return Ok(Command::Help),
            Arg::Opt(Flag::OtherMethod(option), _) => {
                return Err(ParseError::UnsupportedMethod(String::from(option)))
            }
            Arg::Opt(flag, value) => builder.set(flag, value.unwrap_or_default())?,
        }
    }
    build(builder)
}

impl Builder {
    /// Apply the option `flag` names, with the value it took, naming the option
    /// by its letter in any refusal.
    fn set(&mut self, flag: Flag, value: &str) -> Result<(), ParseError> {
        let invalid = |option: &str| ParseError::InvalidValue {
            option: option.to_string(),
            value: value.to_string(),
        };
        match flag {
            Flag::Ipv4 => self.family = Some(NetAddrFamily::V4),
            Flag::Ipv6 => self.family = Some(NetAddrFamily::V6),
            Flag::Numeric => self.numeric = true,
            Flag::First => self.first_hop = bounded(value, u8::MAX).ok_or_else(|| invalid("-f"))?,
            Flag::MaxHops => {
                self.max_hops = bounded(value, u8::MAX).ok_or_else(|| invalid("-m"))?;
            }
            Flag::Queries => {
                self.queries = bounded(value, MAX_PROBES_PER_HOP).ok_or_else(|| invalid("-q"))?;
            }
            Flag::SimQueries => {
                self.sim_queries = bounded(value, MAX_IN_FLIGHT).ok_or_else(|| invalid("-N"))?;
            }
            Flag::Wait => {
                self.wait_ns = parse_seconds(value)
                    .filter(|&wait| wait > 0)
                    .ok_or_else(|| invalid("-w"))?;
            }
            Flag::Icmp | Flag::OtherMethod(_) | Flag::Help => {}
        }
        Ok(())
    }
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
