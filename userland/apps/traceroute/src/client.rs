//! The `traceroute` engine: drive the shared path trace, then print each hop
//! as soon as its probes have settled, in Linux `traceroute`'s shape.
//!
//! Pure and host-testable: it drives the injected [`TraceIo`] and [`Output`]
//! seams, never a syscall.

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt::Write as _;
use core::net::IpAddr;

use tairix_abi::net::EchoOutcome;
use tairix_abi::net_ipc::NetAddrFamily;
use tairix_abi::Errno;
use tairix_help::{own_short_help, HelpSource};
use tairix_net::trace::{Action, Hop, PathTrace, Probe, TraceConfig};
use tairix_procinfo::Output;

use crate::command::{Command, Config};
use crate::error::TraceError;
use crate::net::TraceIo;

/// The one-line usage banner.
pub const USAGE: &str = "usage: traceroute [-46In] [-f first_ttl] [-m max_ttl] [-q nqueries] \
                         [-N squeries] [-w waittime] host [packetlen]";

/// The command word this bundle is named by, for the own-help lookup.
const OWN_WORD: &str = "traceroute";

/// IPv4 header bytes a probe carries before its ICMP message.
const IPV4_HEADER_LEN: usize = 20;
/// IPv6 header bytes a probe carries before its ICMP message.
const IPV6_HEADER_LEN: usize = 40;
/// The ICMP echo header.
const ICMP_ECHO_HEADER_LEN: usize = 8;
/// Probe data bytes by default, after the headers.
const DEFAULT_DATA_LEN: usize = 32;
/// The first probe's sequence number.
const FIRST_SEQUENCE: u16 = 1;
/// Most hop addresses one run remembers the names of; past it, each further
/// address is looked up whenever it is printed.
const MAX_REMEMBERED_NAMES: usize = 256;

/// Run a parsed `traceroute` command against the injected seams.
///
/// # Errors
///
/// A [`TraceError`] when the target does not resolve, the socket is refused,
/// a probe cannot be sent or received, or a line cannot be written. A trace
/// that never reaches its destination is **not** an error: it ends at the hop
/// bound.
pub fn run(
    command: Command,
    locale: Option<&str>,
    io: &mut dyn TraceIo,
    help: &dyn HelpSource,
    out: &dyn Output,
) -> Result<(), TraceError> {
    match command {
        Command::Help => {
            let bytes = own_short_help(help, locale, OWN_WORD)
                .unwrap_or_else(|| format!("{USAGE}\n").into_bytes());
            let text = String::from_utf8_lossy(&bytes);
            for line in text.lines() {
                out.write_line(line).map_err(TraceError::Output)?;
            }
            Ok(())
        }
        Command::Run(config) => trace(&config, io, out),
    }
}

/// Resolve and connect, then probe hop by hop until the trace is done.
fn trace(config: &Config, io: &mut dyn TraceIo, out: &dyn Output) -> Result<(), TraceError> {
    let target = io
        .resolve(&config.host, config.family)
        .map_err(|reason| TraceError::Resolve {
            host: config.host.clone(),
            reason,
        })?;
    io.connect(target).map_err(|errno| match errno {
        Errno::PermissionDenied => TraceError::Denied,
        other => TraceError::Socket(other),
    })?;
    let family = NetAddrFamily::of(target);
    let headers = header_len(family);
    let packet_len = config
        .packet_len
        .unwrap_or(headers + DEFAULT_DATA_LEN)
        .max(headers);
    let payload = probe_data(packet_len - headers);
    out.write_line(&format!(
        "traceroute to {} ({target}), {} hops max, {packet_len} byte packets",
        config.host, config.max_hops
    ))
    .map_err(TraceError::Output)?;

    let mut path = PathTrace::new(
        TraceConfig {
            first_hop: config.first_hop,
            max_hops: config.max_hops,
            probes_per_hop: config.queries,
            in_flight: config.sim_queries,
            wait_ns: config.wait_ns,
        },
        FIRST_SEQUENCE,
    )
    .map_err(TraceError::Config)?;
    let mut printer = HopPrinter {
        family,
        numeric: config.numeric,
        printed: 0,
        names: BTreeMap::new(),
    };
    loop {
        match path.poll(io.now()) {
            Action::Send {
                hop_limit,
                sequence,
            } => {
                let answer_by = io.now().saturating_add(config.wait_ns);
                send(io, sequence, hop_limit, &payload, answer_by)?;
            }
            Action::Wait { until_ns } => {
                if let Some(answer) = io.recv(until_ns).map_err(TraceError::Receive)? {
                    let now = io.now();
                    path.answer(answer.sequence, answer.from, answer.outcome, now);
                }
            }
            Action::Done => break,
        }
        printer.print_settled(&path, io, out)?;
    }
    printer.print_settled(&path, io, out)
}

/// Send one probe. While no interface can carry it, wait for the network to
/// change — never on a timer — until `answer_by`.
fn send(
    io: &mut dyn TraceIo,
    sequence: u16,
    hop_limit: core::num::NonZeroU8,
    payload: &[u8],
    answer_by: u64,
) -> Result<(), TraceError> {
    loop {
        match io.send(sequence, hop_limit, payload) {
            Err(Errno::NetworkUnreachable) if io.await_network(answer_by) => {}
            sent => return sent.map_err(TraceError::Send),
        }
    }
}

/// The header bytes in front of a probe's data.
fn header_len(family: NetAddrFamily) -> usize {
    ICMP_ECHO_HEADER_LEN
        + match family {
            NetAddrFamily::V4 => IPV4_HEADER_LEN,
            NetAddrFamily::V6 => IPV6_HEADER_LEN,
        }
}

/// `len` bytes of the classic traceroute probe data.
fn probe_data(len: usize) -> Vec<u8> {
    (0..len)
        .map(|offset| 0x40 + u8::try_from(offset & 0x3f).unwrap_or(0))
        .collect()
}

/// Prints the settled hops in order, naming each address once.
struct HopPrinter {
    family: NetAddrFamily,
    numeric: bool,
    printed: usize,
    names: BTreeMap<IpAddr, Option<String>>,
}

impl HopPrinter {
    fn print_settled(
        &mut self,
        path: &PathTrace,
        io: &mut dyn TraceIo,
        out: &dyn Output,
    ) -> Result<(), TraceError> {
        while self.printed < path.settled_hops() {
            let Some(hop) = path.hops().get(self.printed) else {
                break;
            };
            let line = self.hop_line(hop, io);
            out.write_line(&line).map_err(TraceError::Output)?;
            self.printed += 1;
        }
        Ok(())
    }

    /// One hop's line: its number, then each probe — the address whenever it
    /// differs from the last that answered, the round trip, any annotation,
    /// and `*` for a probe that drew nothing.
    fn hop_line(&mut self, hop: &Hop, io: &mut dyn TraceIo) -> String {
        let mut line = format!("{:2} ", hop.hop_limit);
        let mut last = None;
        for probe in &hop.probes {
            let Probe::Answered {
                from,
                rtt_ns,
                outcome,
            } = *probe
            else {
                line.push_str(" *");
                continue;
            };
            if last != Some(from) {
                line.push(' ');
                line.push_str(&self.name(from, io));
                last = Some(from);
            }
            let micros = rtt_ns.saturating_add(500) / 1_000;
            let _ = write!(line, "  {}.{:03} ms", micros / 1_000, micros % 1_000);
            if let Some(note) = annotation(self.family, outcome) {
                line.push(' ');
                line.push_str(&note);
            }
        }
        line
    }

    /// `addr` as traceroute names it: `name (address)`, the address standing
    /// in for a name it lacks, or the bare address under `-n`.
    fn name(&mut self, addr: IpAddr, io: &mut dyn TraceIo) -> String {
        let shown = addr.to_string();
        if self.numeric {
            return shown;
        }
        let name = if let Some(known) = self.names.get(&addr) {
            known.clone()
        } else {
            let found = io.reverse(addr);
            if self.names.len() < MAX_REMEMBERED_NAMES {
                self.names.insert(addr, found.clone());
            }
            found
        };
        format!("{} ({shown})", name.as_deref().unwrap_or(&shown))
    }
}

/// The `!` annotation traceroute prints after a probe's round trip, in the
/// probe family's own vocabulary; [`None`] for a probe that went on, or that
/// reached the destination.
fn annotation(family: NetAddrFamily, outcome: EchoOutcome) -> Option<String> {
    let note = match (family, outcome) {
        (_, EchoOutcome::Reply | EchoOutcome::HopLimitExceeded)
        | (NetAddrFamily::V4, EchoOutcome::Unreachable { code: 3 })
        | (NetAddrFamily::V6, EchoOutcome::Unreachable { code: 4 }) => return None,
        (NetAddrFamily::V4, EchoOutcome::Unreachable { code }) => match code {
            0 | 6 | 8 | 11 => String::from("!N"),
            1 | 7 | 12 => String::from("!H"),
            9 | 10 | 13 => String::from("!X"),
            2 => String::from("!P"),
            5 => String::from("!S"),
            14 => String::from("!V"),
            15 => String::from("!C"),
            other => format!("!<{other}>"),
        },
        (NetAddrFamily::V6, EchoOutcome::Unreachable { code }) => match code {
            0 => String::from("!N"),
            2 | 3 => String::from("!H"),
            1 => String::from("!X"),
            other => format!("!<{other}>"),
        },
        (_, EchoOutcome::PacketTooBig { mtu }) => format!("!F-{mtu}"),
        (NetAddrFamily::V4, EchoOutcome::ParameterProblem { code, .. }) => {
            format!("!<12-{code}>")
        }
        (NetAddrFamily::V6, EchoOutcome::ParameterProblem { code, .. }) => {
            format!("!<4-{code}>")
        }
    };
    Some(note)
}

#[cfg(test)]
#[path = "client_tests.rs"]
mod tests;
