//! What `ping` prints, in iputils' wording and number shapes: the reply and
//! `From` lines, the ICMP error reasons, and the closing statistics.
//!
//! A user or script that knows iputils `ping` reads these exactly as it would
//! there — the `time=` precision that narrows as the round trip grows, the
//! `%g` loss percentage, the rounded total time, and the `mdev` deviation.

use alloc::format;
use alloc::string::String;
use core::fmt::Write as _;

use tairix_abi::net::EchoOutcome;
use tairix_abi::net_ipc::NetAddrFamily;
use tairix_abi::time::NANOS_PER_MILLI;
use tairix_util::cfloat::{FloatConversion, FloatDirective};

use crate::client::RunSummary;

/// Round-trip times in whole microseconds, as iputils accumulates them.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct RttStats {
    count: u64,
    min_us: u64,
    max_us: u64,
    sum_us: u128,
    sum_sq_us: u128,
}

impl RttStats {
    /// Record one reply's round trip.
    pub(crate) fn record(&mut self, rtt_ns: u64) {
        let us = rtt_ns / 1_000;
        if self.count == 0 || us < self.min_us {
            self.min_us = us;
        }
        self.max_us = self.max_us.max(us);
        self.sum_us += u128::from(us);
        self.sum_sq_us += u128::from(us) * u128::from(us);
        self.count += 1;
    }

    fn avg_us(&self) -> u64 {
        u64::try_from(self.sum_us / u128::from(self.count.max(1))).unwrap_or(u64::MAX)
    }

    /// The mean deviation, iputils' way: the square root of the variance,
    /// whose subtraction order keeps small round trips exact.
    fn mdev_us(&self) -> u64 {
        let total = u128::from(self.count.max(1));
        let variance = if self.sum_us < u128::from(i32::MAX.unsigned_abs()) {
            self.sum_sq_us
                .saturating_sub(self.sum_us * self.sum_us / total)
                / total
        } else {
            let avg = u128::from(self.avg_us());
            (self.sum_sq_us / total).saturating_sub(avg * avg)
        };
        isqrt(variance)
    }
}

/// The `time=` value of a reply line: three decimals under a millisecond's
/// thousandth-scale, narrowing to none past 100 ms, each tier rounded.
pub(crate) fn reply_time(rtt_ns: u64) -> String {
    let us = rtt_ns / 1_000;
    if us >= 100_000 - 50 {
        format!("{}", (us + 500) / 1_000)
    } else if us >= 10_000 - 5 {
        format!("{}.{}", (us + 50) / 1_000, ((us + 50) % 1_000) / 100)
    } else if us >= 1_000 {
        format!("{}.{:02}", (us + 5) / 1_000, ((us + 5) % 1_000) / 10)
    } else {
        format!("{}.{:03}", us / 1_000, us % 1_000)
    }
}

/// What a router or the host said about a request, for its `From` line, in
/// the probe family's own vocabulary; [`None`] for a reply.
pub(crate) fn reason(family: NetAddrFamily, outcome: EchoOutcome) -> Option<String> {
    let text = match (family, outcome) {
        (_, EchoOutcome::Reply) => return None,
        (NetAddrFamily::V4, EchoOutcome::HopLimitExceeded) => String::from("Time to live exceeded"),
        (NetAddrFamily::V6, EchoOutcome::HopLimitExceeded) => {
            String::from("Time exceeded: Hop limit")
        }
        (NetAddrFamily::V4, EchoOutcome::Unreachable { code }) => v4_unreachable(code),
        (NetAddrFamily::V6, EchoOutcome::Unreachable { code }) => {
            format!("Destination unreachable: {}", v6_unreachable(code))
        }
        (NetAddrFamily::V4, EchoOutcome::PacketTooBig { mtu }) => {
            format!("Frag needed and DF set (mtu = {mtu})")
        }
        (NetAddrFamily::V6, EchoOutcome::PacketTooBig { mtu }) => {
            format!("Packet too big: mtu={mtu}")
        }
        (NetAddrFamily::V4, EchoOutcome::ParameterProblem { pointer, .. }) => {
            format!("Parameter problem: pointer = {pointer}")
        }
        (NetAddrFamily::V6, EchoOutcome::ParameterProblem { code, pointer }) => {
            let field = match code {
                0 => String::from("Wrong header field "),
                1 => String::from("Unknown header "),
                2 => String::from("Unknown option "),
                other => format!("code {other} "),
            };
            format!("Parameter problem: {field}at {pointer}")
        }
    };
    Some(text)
}

fn v4_unreachable(code: u8) -> String {
    let named = match code {
        0 => "Destination Net Unreachable",
        1 => "Destination Host Unreachable",
        2 => "Destination Protocol Unreachable",
        3 => "Destination Port Unreachable",
        5 => "Source Route Failed",
        6 => "Destination Net Unknown",
        7 => "Destination Host Unknown",
        8 => "Source Host Isolated",
        9 => "Destination Net Prohibited",
        10 => "Destination Host Prohibited",
        11 => "Destination Net Unreachable for Type of Service",
        12 => "Destination Host Unreachable for Type of Service",
        13 => "Packet filtered",
        14 => "Precedence Violation",
        15 => "Precedence Cutoff",
        other => return format!("Dest Unreachable, Bad Code: {other}"),
    };
    String::from(named)
}

fn v6_unreachable(code: u8) -> String {
    let named = match code {
        0 => "No route",
        1 => "Administratively prohibited",
        2 => "Beyond scope of source address",
        3 => "Address unreachable",
        4 => "Port unreachable",
        other => return format!("Unknown code {other}"),
    };
    String::from(named)
}

/// The closing statistics block.
pub(crate) fn statistics(
    display: &str,
    summary: RunSummary,
    rtt: &RttStats,
    elapsed_ns: u64,
) -> String {
    let mut text = String::new();
    let _ = writeln!(text, "\n--- {display} ping statistics ---");
    let _ = write!(
        text,
        "{} packets transmitted, {} received",
        summary.transmitted, summary.received
    );
    if summary.errors > 0 {
        let _ = write!(text, ", +{} errors", summary.errors);
    }
    if summary.transmitted > 0 {
        let lost = summary.transmitted.saturating_sub(summary.received);
        let _ = write!(
            text,
            ", {}% packet loss, time {}ms",
            loss_percent(lost, summary.transmitted),
            (elapsed_ns + NANOS_PER_MILLI / 2) / NANOS_PER_MILLI,
        );
    }
    text.push('\n');
    if rtt.count > 0 {
        let _ = writeln!(
            text,
            "rtt min/avg/max/mdev = {}/{}/{}/{} ms",
            millis(rtt.min_us),
            millis(rtt.avg_us()),
            millis(rtt.max_us),
            millis(rtt.mdev_us()),
        );
    }
    text
}

/// Microseconds as `%ld.%03ld` milliseconds.
fn millis(us: u64) -> String {
    format!("{}.{:03}", us / 1_000, us % 1_000)
}

/// The loss percentage as iputils prints it: computed in `double`, narrowed
/// to `float`, and rendered `%g`.
#[allow(
    clippy::cast_possible_truncation,
    reason = "iputils narrows the percentage to float before printing it"
)]
fn loss_percent(lost: u32, transmitted: u32) -> String {
    let percent = f64::from(lost) * 100.0 / f64::from(transmitted);
    FloatDirective::plain(FloatConversion::Shortest).render(f64::from(percent as f32))
}

/// The integer square root, rounded down.
fn isqrt(value: u128) -> u64 {
    if value == 0 {
        return 0;
    }
    // Newton's iteration from above converges monotonically to the floor.
    let mut root = value;
    // ceil(value / 2), without the overflow `(value + 1) / 2` has at the top.
    let mut next = value / 2 + (value & 1);
    while next < root {
        root = next;
        next = root.midpoint(value / root);
    }
    u64::try_from(root).unwrap_or(u64::MAX)
}

#[cfg(test)]
#[path = "report_tests.rs"]
mod tests;
