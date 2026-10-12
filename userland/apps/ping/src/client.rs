//! The `ping` engine: send echo requests, collect what becomes of each, print
//! each result and the final statistics.
//!
//! Pure and host-testable: it drives the injected [`PingIo`] seam (clock,
//! echo socket, wait/park) and the [`Output`] seams, never a syscall.

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;

use tairix_abi::net::EchoOutcome;
use tairix_abi::net_ipc::NetAddrFamily;
use tairix_abi::Errno;
use tairix_help::{own_short_help, HelpSource};

use crate::command::{Command, Config, PayloadKind, Target};
use crate::error::PingError;
use crate::net::{EchoEvent, PingIo};
use crate::report::{self, RttStats};
use crate::Output;

/// The one-line usage banner, printed on a usage error and as the fallback
/// when the bundled help document is unavailable.
pub const USAGE: &str = "usage: ping [-c count] [-i interval] [-s size] [-W timeout] \
                         [-w deadline] [-p pattern] [-46nq] <host>";

/// The command word this bundle is named by, for the own-help lookup.
const OWN_WORD: &str = "ping";

/// Most reporting routers one run remembers the names of; past it, each
/// further one is looked up whenever it reports.
const MAX_REMEMBERED_NAMES: usize = 64;

/// The outcome of a completed ping run: enough to choose the exit code.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RunSummary {
    /// Requests sent.
    pub transmitted: u32,
    /// Replies received and verified.
    pub received: u32,
    /// Requests a router or the host reported an error about.
    pub errors: u32,
}

impl RunSummary {
    /// Whether at least one reply came back (the exit-`0` condition).
    #[must_use]
    pub fn any_received(&self) -> bool {
        self.received > 0
    }
}

/// Run a parsed `ping` command against the injected seams.
///
/// # Errors
///
/// A [`PingError`] when the socket cannot be opened/connected, a receive
/// fails fatally, or a line cannot be written. A run in which every request
/// went unanswered is **not** an error — it completes and returns a
/// [`RunSummary`] with `received == 0` (the caller maps that to exit `1`).
pub fn run(
    command: Command,
    locale: Option<&str>,
    io: &mut dyn PingIo,
    help: &dyn HelpSource,
    out: &dyn Output,
    err: &dyn Output,
) -> Result<RunSummary, PingError> {
    let config = match command {
        Command::Help => {
            let bytes = own_short_help(help, locale, OWN_WORD)
                .unwrap_or_else(|| format!("{USAGE}\n").into_bytes());
            out.write_all(&bytes).map_err(PingError::Output)?;
            return Ok(RunSummary::default());
        }
        Command::Run(config) => config,
    };
    ping(&config, io, out, err)
}

/// Resolve and connect, then run the requests.
fn ping(
    config: &Config,
    io: &mut dyn PingIo,
    out: &dyn Output,
    err: &dyn Output,
) -> Result<RunSummary, PingError> {
    let target = resolve(config, io)?;
    io.connect(target.family, target.addr)
        .map_err(map_connect_error)?;
    Run {
        config,
        io,
        out,
        err,
        target,
        summary: RunSummary::default(),
        rtt: RttStats::default(),
        names: BTreeMap::new(),
    }
    .requests()
}

/// One run in progress.
struct Run<'a> {
    config: &'a Config,
    io: &'a mut dyn PingIo,
    out: &'a dyn Output,
    err: &'a dyn Output,
    target: Target,
    summary: RunSummary,
    rtt: RttStats,
    /// The names reporting routers resolved to, each looked up once.
    names: BTreeMap<[u8; 16], Option<String>>,
}

impl Run<'_> {
    /// Send each request, await what becomes of it, and print the closing
    /// statistics.
    fn requests(mut self) -> Result<RunSummary, PingError> {
        let mut payload = vec![0u8; self.config.size];
        let start = self.io.now();
        let overall_deadline = self.config.deadline_ns.map(|d| start.saturating_add(d));
        self.print(&header(self.config, &self.target))?;
        let mut seq: u16 = 0;
        while self.config.count.is_none_or(|c| u32::from(seq) < c) {
            if overall_deadline.is_some_and(|deadline| self.io.now() >= deadline) {
                break;
            }
            seq = seq.wrapping_add(1);
            // Fresh bytes for every request: an identical payload would let a
            // de-duplicating link answer from a cache, and the echoed copy is
            // then a per-packet integrity check as well.
            fill_payload(self.config, self.io, &mut payload);
            let sent_at = self.io.now();
            let answer_by = sent_at
                .saturating_add(self.config.timeout_ns)
                .min(overall_deadline.unwrap_or(u64::MAX));
            self.summary.transmitted += 1;
            match self.send(seq, &payload, answer_by) {
                Ok(()) => self.await_answer(seq, &payload, sent_at, answer_by)?,
                Err(errno) => self
                    .err
                    .write_all(format!("ping: sendmsg: {errno}\n").as_bytes())
                    .map_err(PingError::Output)?,
            }
            let last = self.config.count.is_some_and(|c| u32::from(seq) >= c);
            if !last {
                self.io
                    .sleep_until(sent_at.saturating_add(self.config.interval_ns));
            }
        }
        let elapsed = self.io.now().saturating_sub(start);
        let closing = report::statistics(&self.target.display, self.summary, &self.rtt, elapsed);
        self.print(&closing)?;
        Ok(self.summary)
    }

    /// Send `seq`. While no interface can carry it, wait for the network to
    /// change — never on a timer — until `answer_by`.
    fn send(&mut self, seq: u16, payload: &[u8], answer_by: u64) -> Result<(), Errno> {
        loop {
            match self.io.send(seq, payload) {
                Err(Errno::NetworkUnreachable) if self.io.await_network(answer_by) => {}
                sent => return sent,
            }
        }
    }

    /// Await what becomes of `seq` until `answer_by`, discarding anything
    /// about another request, and record it. Nothing at all is a silent loss.
    fn await_answer(
        &mut self,
        seq: u16,
        payload: &[u8],
        sent_at: u64,
        answer_by: u64,
    ) -> Result<(), PingError> {
        while let Some(event) = self.io.recv(answer_by).map_err(PingError::Receive)? {
            if event.seq != seq || event.family != self.target.family {
                continue;
            }
            if event.outcome != EchoOutcome::Reply {
                self.summary.errors += 1;
                let line = self.error_line(&event);
                return self.print_unless_quiet(&line);
            }
            if reply_matches(&self.target, payload, &event) {
                let rtt_ns = self.io.now().saturating_sub(sent_at);
                self.rtt.record(rtt_ns);
                self.summary.received += 1;
                let line = reply_line(&event, rtt_ns, self.target.name.as_deref());
                return self.print_unless_quiet(&line);
            }
        }
        Ok(())
    }

    /// iputils' `From <reporter> icmp_seq=<seq> <reason>` line.
    fn error_line(&mut self, event: &EchoEvent) -> String {
        let reporter = self.reporter(event.family, event.addr);
        let reason = report::reason(self.target.family, event.outcome).unwrap_or_default();
        format!("From {reporter} icmp_seq={} {reason}\n", event.seq)
    }

    /// A reporter as iputils names it: `name (address)` when it has a name
    /// and output is not numeric, else the bare address.
    fn reporter(&mut self, family: NetAddrFamily, addr: [u8; 16]) -> String {
        let shown = render_addr(family, &addr);
        if self.config.numeric {
            return shown;
        }
        let name = if addr == self.target.addr {
            self.target.name.clone()
        } else if let Some(known) = self.names.get(&addr) {
            known.clone()
        } else {
            let found = self.io.reverse(family, addr);
            if self.names.len() < MAX_REMEMBERED_NAMES {
                self.names.insert(addr, found.clone());
            }
            found
        };
        match name {
            Some(name) => format!("{name} ({shown})"),
            None => shown,
        }
    }

    fn print(&self, text: &str) -> Result<(), PingError> {
        self.out
            .write_all(text.as_bytes())
            .map_err(PingError::Output)
    }

    fn print_unless_quiet(&self, text: &str) -> Result<(), PingError> {
        if self.config.quiet {
            Ok(())
        } else {
            self.print(text)
        }
    }
}

/// Resolve the target operand, or fail naming the host as typed.
fn resolve(config: &Config, io: &mut dyn PingIo) -> Result<Target, PingError> {
    let host = config.target.host.as_str();
    let (family, addr) =
        io.resolve(host, config.target.family)
            .map_err(|reason| PingError::Resolve {
                host: host.to_string(),
                reason,
            })?;
    // A reverse lookup only when the peer will actually be named: `-n`
    // means numeric output *and* no query.
    let name = if config.numeric {
        None
    } else {
        io.reverse(family, addr)
    };
    Ok(Target {
        display: host.to_string(),
        family,
        addr,
        name,
    })
}

/// Map a connect refusal onto the tool's error type, keeping a capability
/// denial distinguishable from every other cause.
fn map_connect_error(errno: Errno) -> PingError {
    match errno {
        Errno::PermissionDenied => PingError::Denied,
        other => PingError::Socket(other),
    }
}

/// Whether a reply's source and payload match what we sent (defence in
/// depth: the stack already filters by identifier and connected peer).
///
/// With the default random payload this is also a per-packet integrity
/// check: a link that corrupted a byte cannot echo the bytes we drew.
fn reply_matches(target: &Target, payload: &[u8], reply: &EchoEvent) -> bool {
    reply.addr == target.addr && reply.payload == payload
}

/// Fill `payload` for the next request.
///
/// The random default goes through the injected seam, so the engine stays
/// pure; a `-p` pattern is repeated to fill the buffer and needs no entropy.
fn fill_payload(config: &Config, io: &mut dyn PingIo, payload: &mut [u8]) {
    match &config.payload {
        PayloadKind::Random => io.fill_payload(payload),
        PayloadKind::Pattern(pattern) => {
            for (slot, byte) in payload.iter_mut().zip(pattern.iter().cycle()) {
                *slot = *byte;
            }
        }
    }
}

/// The opening `PING …` header line.
fn header(config: &Config, target: &Target) -> String {
    let addr = render_addr(target.family, &target.addr);
    match target.family {
        NetAddrFamily::V4 => {
            // v4 total on the wire = payload + 8 (ICMP) + 20 (IPv4).
            let total = config.size + 28;
            format!(
                "PING {} ({}) {}({}) bytes of data.\n",
                target.display, addr, config.size, total
            )
        }
        NetAddrFamily::V6 => format!(
            "PING {} ({}) {} data bytes\n",
            target.display, addr, config.size
        ),
    }
}

/// One `<n> bytes from <peer>: icmp_seq=<seq> time=<ms> ms` line, where
/// `<peer>` is `<name> (<addr>)` when the address reverse-resolved and the
/// bare address otherwise (iputils' shape, and what `-n` forces).
///
/// The IP TTL is not surfaced through the echo-socket abstraction, so —
/// unlike iputils — the line carries no `ttl=` field (a documented
/// TAIRiX-specific divergence).
fn reply_line(reply: &EchoEvent, rtt_ns: u64, name: Option<&str>) -> String {
    let addr = render_addr(reply.family, &reply.addr);
    let peer = match name {
        Some(name) => format!("{name} ({addr})"),
        None => addr,
    };
    // "bytes from" counts the ICMP message: the 8-byte header plus data.
    let bytes = reply.payload.len() + 8;
    format!(
        "{} bytes from {}: icmp_seq={} time={} ms\n",
        bytes,
        peer,
        u32::from(reply.seq),
        report::reply_time(rtt_ns),
    )
}

/// Render an address for display.
fn render_addr(family: NetAddrFamily, addr: &[u8; 16]) -> String {
    tairix_abi::net_ipc::ip_from_parts(family, *addr).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::HostTarget;
    use alloc::collections::VecDeque;
    use alloc::vec::Vec;
    use core::cell::RefCell;
    use tairix_resolver::TargetFailure;

    /// An in-memory `PingIo`: it echoes every sent request unless its `seq`
    /// is dropped or answered with a scripted error, advancing a synthetic
    /// monotonic clock by `rtt_step` on each delivered answer.
    struct FakeIo {
        clock: u64,
        rtt_step: u64,
        family: NetAddrFamily,
        addr: [u8; 16],
        drop: Vec<u16>,
        /// Requests a router answers with an error instead: the outcome and
        /// the reporter's address.
        errors: BTreeMap<u16, (EchoOutcome, [u8; 16])>,
        /// Send refusals still to come, before sends succeed.
        send_errs: VecDeque<Errno>,
        /// Whether the network moves while a send waits for it.
        network_moves: bool,
        /// How many times a send waited for the network.
        network_waits: u32,
        answers: VecDeque<EchoEvent>,
        /// Scripted resolution outcome; `None` means "does not resolve".
        resolves: Option<(NetAddrFamily, [u8; 16])>,
        /// The failure a `None` resolution reports.
        resolve_failure: TargetFailure,
        /// Hosts `resolve` was asked about, in order.
        asked: Vec<String>,
        /// The name every reverse lookup answers with, or `None`.
        reverse: Option<String>,
        /// Addresses `reverse` was asked about, in order.
        reversed: Vec<[u8; 16]>,
        /// A scripted connect refusal.
        connect_err: Option<Errno>,
        /// Counter driving the fake entropy, so successive payloads differ.
        entropy: u8,
        /// Every payload handed to `send`, in order.
        sent: Vec<Vec<u8>>,
    }

    fn v4_addr(octets: [u8; 4]) -> [u8; 16] {
        let mut addr = [0u8; 16];
        addr[..4].copy_from_slice(&octets);
        addr
    }

    impl FakeIo {
        fn v4() -> Self {
            let addr = v4_addr([10, 0, 2, 2]);
            Self {
                clock: 1_000,
                rtt_step: 250_000, // 0.250 ms
                family: NetAddrFamily::V4,
                addr,
                drop: Vec::new(),
                errors: BTreeMap::new(),
                send_errs: VecDeque::new(),
                network_moves: false,
                network_waits: 0,
                answers: VecDeque::new(),
                resolves: Some((NetAddrFamily::V4, addr)),
                resolve_failure: TargetFailure::Unknown,
                asked: Vec::new(),
                reverse: None,
                reversed: Vec::new(),
                connect_err: None,
                entropy: 0,
                sent: Vec::new(),
            }
        }
    }

    impl PingIo for FakeIo {
        fn resolve(
            &mut self,
            host: &str,
            _family: Option<NetAddrFamily>,
        ) -> Result<(NetAddrFamily, [u8; 16]), TargetFailure> {
            self.asked.push(host.to_string());
            self.resolves.ok_or(self.resolve_failure)
        }

        fn reverse(&mut self, _family: NetAddrFamily, addr: [u8; 16]) -> Option<String> {
            self.reversed.push(addr);
            self.reverse.clone()
        }

        fn connect(&mut self, _family: NetAddrFamily, _addr: [u8; 16]) -> Result<(), Errno> {
            match self.connect_err {
                Some(errno) => Err(errno),
                None => Ok(()),
            }
        }

        fn fill_payload(&mut self, out: &mut [u8]) {
            // Not random, but distinct per call, which is the property the
            // engine must preserve.
            self.entropy = self.entropy.wrapping_add(1);
            for (offset, byte) in out.iter_mut().enumerate() {
                *byte = self
                    .entropy
                    .wrapping_add(u8::try_from(offset & 0xff).unwrap_or(0));
            }
        }

        fn now(&self) -> u64 {
            self.clock
        }

        fn send(&mut self, seq: u16, payload: &[u8]) -> Result<(), Errno> {
            if let Some(err) = self.send_errs.pop_front() {
                return Err(err);
            }
            self.sent.push(payload.to_vec());
            if let Some(&(outcome, reporter)) = self.errors.get(&seq) {
                self.answers.push_back(EchoEvent {
                    seq,
                    outcome,
                    family: self.family,
                    addr: reporter,
                    payload: Vec::new(),
                });
            } else if !self.drop.contains(&seq) {
                self.answers.push_back(EchoEvent {
                    seq,
                    outcome: EchoOutcome::Reply,
                    family: self.family,
                    addr: self.addr,
                    payload: payload.to_vec(),
                });
            }
            Ok(())
        }

        fn await_network(&mut self, deadline_ns: u64) -> bool {
            self.network_waits += 1;
            if !self.network_moves {
                self.clock = self.clock.max(deadline_ns);
            }
            self.network_moves
        }

        fn recv(&mut self, deadline_ns: u64) -> Result<Option<EchoEvent>, Errno> {
            if let Some(answer) = self.answers.pop_front() {
                self.clock += self.rtt_step;
                Ok(Some(answer))
            } else {
                // Nothing came back: the wait runs out at the deadline.
                self.clock = self.clock.max(deadline_ns);
                Ok(None)
            }
        }

        fn sleep_until(&mut self, deadline_ns: u64) {
            self.clock = self.clock.max(deadline_ns);
        }
    }

    /// A buffer `Output` capturing everything written.
    struct BufOut(RefCell<Vec<u8>>);

    impl BufOut {
        fn new() -> Self {
            Self(RefCell::new(Vec::new()))
        }
        fn text(&self) -> String {
            String::from_utf8(self.0.borrow().clone()).expect("utf8")
        }
    }

    impl Output for BufOut {
        fn write_all(&self, bytes: &[u8]) -> Result<(), Errno> {
            self.0.borrow_mut().extend_from_slice(bytes);
            Ok(())
        }
    }

    fn config(count: Option<u32>, quiet: bool) -> Config {
        Config {
            target: HostTarget {
                host: "10.0.2.2".to_string(),
                family: None,
            },
            payload: PayloadKind::Random,
            count,
            interval_ns: 0,
            timeout_ns: 1_000_000_000,
            deadline_ns: None,
            size: 4,
            quiet,
            numeric: true,
        }
    }

    /// The same configuration with the default (name-resolving) output.
    fn resolving_config(count: Option<u32>) -> Config {
        Config {
            numeric: false,
            ..config(count, false)
        }
    }

    /// Run `config` against `io`, returning the summary and both streams.
    fn ping_with(config: &Config, io: &mut FakeIo) -> (RunSummary, String, String) {
        let out = BufOut::new();
        let err = BufOut::new();
        let summary = ping(config, io, &out, &err).expect("run");
        (summary, out.text(), err.text())
    }

    #[test]
    fn a_reply_from_a_named_peer_shows_the_name_and_the_address() {
        let mut io = FakeIo::v4();
        io.reverse = Some("gateway.example".to_string());
        let (_, text, _) = ping_with(&resolving_config(Some(1)), &mut io);
        assert!(
            text.contains("bytes from gateway.example (10.0.2.2): icmp_seq=1"),
            "{text}"
        );
        assert_eq!(
            io.reversed.len(),
            1,
            "the peer is looked up once, not once per reply"
        );
    }

    #[test]
    fn a_peer_with_no_pointer_record_stays_numeric() {
        let mut io = FakeIo::v4();
        io.reverse = None;
        let (_, text, _) = ping_with(&resolving_config(Some(1)), &mut io);
        assert!(text.contains("bytes from 10.0.2.2: icmp_seq=1"));
    }

    #[test]
    fn numeric_output_issues_no_reverse_lookup() {
        let mut io = FakeIo::v4();
        io.reverse = Some("gateway.example".to_string());
        let (_, text, _) = ping_with(&config(Some(1), false), &mut io);
        assert!(text.contains("bytes from 10.0.2.2: icmp_seq=1"), "{text}");
        assert!(
            io.reversed.is_empty(),
            "-n must not put a PTR query on the wire"
        );
    }

    #[test]
    fn all_replies_are_received_and_reported() {
        let mut io = FakeIo::v4();
        let (summary, text, _) = ping_with(&config(Some(3), false), &mut io);
        assert_eq!(summary.transmitted, 3);
        assert_eq!(summary.received, 3);
        assert!(summary.any_received());
        assert!(text.starts_with("PING 10.0.2.2 (10.0.2.2) 4(32) bytes of data."));
        assert_eq!(text.matches("bytes from 10.0.2.2").count(), 3);
        assert!(text.contains("icmp_seq=1"));
        assert!(text.contains("time=0.250 ms"));
        assert!(text.contains("3 packets transmitted, 3 received, 0% packet loss"));
        assert!(text.contains("rtt min/avg/max/mdev = 0.250/0.250/0.250/0.000 ms"));
    }

    #[test]
    fn an_unanswered_request_prints_nothing_and_counts_as_loss() {
        let mut io = FakeIo::v4();
        io.drop = vec![2];
        let (summary, text, _) = ping_with(&config(Some(3), false), &mut io);
        assert_eq!(summary.transmitted, 3);
        assert_eq!(summary.received, 2);
        assert!(!text.contains("icmp_seq=2"), "{text}");
        assert!(text.contains("3 packets transmitted, 2 received, 33.3333% packet loss"));
    }

    #[test]
    fn quiet_suppresses_per_reply_lines_but_keeps_statistics() {
        let mut io = FakeIo::v4();
        let (_, text, _) = ping_with(&config(Some(2), true), &mut io);
        assert!(!text.contains("bytes from"));
        assert!(text.contains("2 packets transmitted, 2 received, 0% packet loss"));
    }

    #[test]
    fn a_router_error_prints_a_from_line_and_counts_as_an_error() {
        let mut io = FakeIo::v4();
        let gateway = v4_addr([10, 0, 2, 1]);
        io.errors
            .insert(1, (EchoOutcome::Unreachable { code: 1 }, gateway));
        io.errors
            .insert(2, (EchoOutcome::HopLimitExceeded, gateway));
        let (summary, text, _) = ping_with(&config(Some(3), false), &mut io);
        assert_eq!(summary.errors, 2);
        assert_eq!(summary.received, 1);
        assert!(
            text.contains("From 10.0.2.1 icmp_seq=1 Destination Host Unreachable\n"),
            "{text}"
        );
        assert!(text.contains("From 10.0.2.1 icmp_seq=2 Time to live exceeded\n"));
        assert!(text.contains("3 packets transmitted, 1 received, +2 errors, 66.6667% packet loss"));
    }

    #[test]
    fn a_v6_error_is_reported_in_the_v6_vocabulary() {
        let mut io = FakeIo::v4();
        let target = [0xfe, 0x80, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2];
        let router = [0xfe, 0x80, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1];
        io.family = NetAddrFamily::V6;
        io.addr = target;
        io.resolves = Some((NetAddrFamily::V6, target));
        io.errors
            .insert(1, (EchoOutcome::PacketTooBig { mtu: 1280 }, router));
        let (_, text, _) = ping_with(&config(Some(1), false), &mut io);
        assert!(
            text.contains("From fe80::1 icmp_seq=1 Packet too big: mtu=1280\n"),
            "{text}"
        );
    }

    #[test]
    fn a_quiet_run_prints_no_from_line_but_still_counts_the_error() {
        let mut io = FakeIo::v4();
        io.errors.insert(
            1,
            (EchoOutcome::Unreachable { code: 0 }, v4_addr([10, 0, 2, 1])),
        );
        let (summary, text, _) = ping_with(&config(Some(1), true), &mut io);
        assert_eq!(summary.errors, 1);
        assert!(!text.contains("From"));
        assert!(text.contains("+1 errors"));
    }

    #[test]
    fn a_reporting_router_is_named_once_however_often_it_reports() {
        let mut io = FakeIo::v4();
        io.reverse = Some("edge.example".to_string());
        let router = v4_addr([10, 0, 2, 1]);
        for seq in 1..=3 {
            io.errors
                .insert(seq, (EchoOutcome::HopLimitExceeded, router));
        }
        let (_, text, _) = ping_with(&resolving_config(Some(3)), &mut io);
        assert_eq!(
            text.matches("From edge.example (10.0.2.1) icmp_seq=")
                .count(),
            3,
            "{text}"
        );
        assert_eq!(
            io.reversed.iter().filter(|&&addr| addr == router).count(),
            1,
            "a router that reports again is not looked up again"
        );
    }

    #[test]
    fn a_send_the_network_cannot_carry_yet_waits_for_it_to_come_up() {
        let mut io = FakeIo::v4();
        io.send_errs = VecDeque::from([Errno::NetworkUnreachable]);
        io.network_moves = true;
        let (summary, text, errors) = ping_with(&config(Some(1), false), &mut io);
        assert_eq!(io.network_waits, 1);
        assert_eq!(
            summary.received, 1,
            "the request went out once the network moved"
        );
        assert!(text.contains("icmp_seq=1"));
        assert!(errors.is_empty());
    }

    #[test]
    fn a_network_that_never_comes_up_is_reported_on_standard_error() {
        let mut io = FakeIo::v4();
        io.send_errs = VecDeque::from([Errno::NetworkUnreachable, Errno::NetworkUnreachable]);
        let (summary, text, errors) = ping_with(&config(Some(2), false), &mut io);
        assert_eq!(summary.transmitted, 2);
        assert_eq!(summary.received, 0);
        assert!(!summary.any_received());
        assert_eq!(errors.matches("ping: sendmsg:").count(), 2, "{errors}");
        assert!(text.contains("2 packets transmitted, 0 received, 100% packet loss"));
        assert!(!text.contains("rtt min/avg/max"));
    }

    #[test]
    fn the_run_deadline_cuts_a_reply_wait_short() {
        let mut io = FakeIo::v4();
        io.drop = vec![1];
        let mut config = config(None, false);
        config.timeout_ns = 10_000_000_000;
        config.deadline_ns = Some(2_000_000_000);
        let (summary, text, _) = ping_with(&config, &mut io);
        assert_eq!(
            summary.transmitted, 1,
            "the deadline passed during the first wait"
        );
        assert!(text.contains("time 2000ms"), "{text}");
    }

    #[test]
    fn a_reply_from_a_wrong_source_is_not_counted() {
        let mut io = FakeIo::v4();
        io.drop = vec![1];
        io.answers.push_back(EchoEvent {
            seq: 1,
            outcome: EchoOutcome::Reply,
            family: NetAddrFamily::V4,
            addr: v4_addr([10, 0, 2, 99]),
            payload: vec![0, 1, 2, 3],
        });
        let (summary, _, _) = ping_with(&config(Some(1), false), &mut io);
        assert_eq!(summary.received, 0);
    }

    #[test]
    fn an_answer_about_another_request_is_ignored() {
        let mut io = FakeIo::v4();
        io.drop = vec![1];
        io.answers.push_back(EchoEvent {
            seq: 7,
            outcome: EchoOutcome::HopLimitExceeded,
            family: NetAddrFamily::V4,
            addr: v4_addr([10, 0, 2, 1]),
            payload: Vec::new(),
        });
        let (summary, text, _) = ping_with(&config(Some(1), false), &mut io);
        assert_eq!(summary.errors, 0);
        assert!(!text.contains("From"));
    }

    #[test]
    fn every_request_carries_fresh_payload_bytes() {
        let mut io = FakeIo::v4();
        let _ = ping_with(&config(Some(4), true), &mut io);
        assert_eq!(io.sent.len(), 4);
        for (index, payload) in io.sent.iter().enumerate() {
            assert_eq!(payload.len(), 4, "each payload is the requested size");
            for other in &io.sent[index + 1..] {
                assert_ne!(
                    payload, other,
                    "an identical payload would let a de-duplicating link answer from cache"
                );
            }
        }
    }

    #[test]
    fn a_pattern_payload_repeats_to_fill_the_size() {
        let mut io = FakeIo::v4();
        let mut config = config(Some(2), true);
        config.payload = PayloadKind::Pattern(vec![0xab, 0xcd]);
        config.size = 5;
        let _ = ping_with(&config, &mut io);
        for payload in &io.sent {
            assert_eq!(
                payload.as_slice(),
                &[0xab, 0xcd, 0xab, 0xcd, 0xab],
                "the pattern cycles and every request is identical"
            );
        }
    }

    #[test]
    fn a_zero_size_payload_is_empty_and_still_pings() {
        let mut io = FakeIo::v4();
        let mut config = config(Some(1), true);
        config.size = 0;
        let (summary, _, _) = ping_with(&config, &mut io);
        assert_eq!(summary.received, 1);
        assert_eq!(io.sent, vec![Vec::new()]);
    }

    #[test]
    fn a_name_is_resolved_and_the_header_shows_both() {
        let mut io = FakeIo::v4();
        let mut config = config(Some(1), false);
        config.target.host = "gateway.example".to_string();
        let (_, text, _) = ping_with(&config, &mut io);
        assert_eq!(io.asked, vec!["gateway.example".to_string()]);
        assert!(
            text.starts_with("PING gateway.example (10.0.2.2) 4(32) bytes of data."),
            "the header names the host as typed and the address it resolved to: {text}"
        );
        assert!(text.contains("--- gateway.example ping statistics ---"));
    }

    #[test]
    fn an_unresolvable_target_fails_loud_naming_the_host() {
        let mut io = FakeIo::v4();
        io.resolves = None;
        let mut config = config(Some(1), false);
        config.target.host = "nope.invalid".to_string();
        let out = BufOut::new();
        let err = BufOut::new();
        let error = ping(&config, &mut io, &out, &err).expect_err("must not silently succeed");
        assert_eq!(
            error,
            PingError::Resolve {
                host: "nope.invalid".to_string(),
                reason: TargetFailure::Unknown,
            }
        );
        assert!(io.sent.is_empty(), "nothing is sent when nothing resolved");
        assert!(
            out.text().is_empty(),
            "no header for a run that never began"
        );
    }

    #[test]
    fn a_literal_of_the_excluded_family_is_reported_as_such() {
        let mut io = FakeIo::v4();
        io.resolves = None;
        io.resolve_failure = TargetFailure::FamilyMismatch;
        let mut config = config(Some(1), false);
        config.target.host = "fe80::1".to_string();
        config.target.family = Some(NetAddrFamily::V4);
        let out = BufOut::new();
        let err = BufOut::new();
        let error = ping(&config, &mut io, &out, &err).expect_err("must not silently succeed");
        assert_eq!(
            error,
            PingError::Resolve {
                host: "fe80::1".to_string(),
                reason: TargetFailure::FamilyMismatch,
            },
            "a wrong-family literal is a distinct diagnosis from an unknown name"
        );
    }

    #[test]
    fn a_refused_socket_is_reported_as_a_capability_denial() {
        let mut io = FakeIo::v4();
        io.connect_err = Some(Errno::PermissionDenied);
        let out = BufOut::new();
        let err = BufOut::new();
        let error = ping(&config(Some(1), false), &mut io, &out, &err).expect_err("denied");
        assert_eq!(error, PingError::Denied);
        assert!(io.sent.is_empty());
    }
}
