//! The active reachability probe engine: a polite, bounded liveness sweep and
//! a well-known-port check, as a pure state machine its caller drives with its
//! own clock and sockets.
//!
//! An administrator runs this against a network they administer, so it is a
//! friendly host-discovery sweep — never an attack tool. Every campaign is
//! token-bucket paced, bounded in concurrency and in target set, and skips a
//! subnet's own network and broadcast addresses. Nothing here sends or waits:
//! [`Probe::poll`] says what to do next, and the caller feeds answers back.
//!
//! Probes are attributed by the sequence number (echo) or token (connect) the
//! engine assigned, because an echo reply carries no sense of which destination
//! it was for and a dead host produces no event at all — the engine's own
//! per-probe deadline is what declares "no answer".

use alloc::collections::{BTreeMap, BTreeSet, VecDeque};
use alloc::vec::Vec;
use core::net::{IpAddr, Ipv4Addr};

use tairix_abi::net::EchoOutcome;
use tairix_abi::time::Duration64;
use tairix_net::rate::TokenBucket;

/// Most addresses one campaign probes, a fixed containment bound: a segment of
/// attacker-chosen size cannot make the engine hold an unbounded target set.
pub const MAX_PROBE_TARGETS: usize = 4096;

/// The TCP ports a reachable host is checked for, a small curated set of
/// well-known services that discriminate a host's kind. Bounded by definition.
pub const WELL_KNOWN_PORTS: &[u16] = &[
    22,   // ssh
    80,   // http
    139,  // netbios session
    443,  // https
    445,  // smb
    515,  // lpd
    548,  // afp
    631,  // ipp
    3389, // rdp
    9100, // jetdirect raw print
];

/// How a campaign probes — the preset a posture resolves to. Every limit is a
/// fixed containment bound (choosing our own CPU/wire cost is a denial-of-service
/// channel), never a capacity a machine scales.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ProbeProfile {
    /// Whether to probe at all. A `passive` posture sets this `false`.
    pub probe: bool,
    /// Echo sends allowed per second.
    pub echo_rate: u32,
    /// Echo sends allowed in a burst.
    pub echo_burst: u32,
    /// Echo probes outstanding at once.
    pub echo_in_flight: u8,
    /// TCP connects outstanding at once.
    pub connect_in_flight: u8,
    /// How long an echo is awaited before it counts as no answer.
    pub probe_wait_ns: u64,
    /// How long a connect is awaited before it is closed as no answer.
    pub connect_deadline_ns: u64,
    /// The largest attached subnet that is swept whole, in host addresses.
    pub sweep_host_cap: u32,
}

impl ProbeProfile {
    /// No active probing: the map keeps the liveness it already has.
    pub const PASSIVE: Self = Self {
        probe: false,
        echo_rate: 0,
        echo_burst: 0,
        echo_in_flight: 0,
        connect_in_flight: 0,
        probe_wait_ns: 0,
        connect_deadline_ns: 0,
        sweep_host_cap: 0,
    };

    /// The default: a gentle sweep that stays well inside the segment's ARP and
    /// per-process socket budgets.
    pub const POLITE: Self = Self {
        probe: true,
        echo_rate: 20,
        echo_burst: 10,
        echo_in_flight: 8,
        connect_in_flight: 8,
        probe_wait_ns: 1_000_000_000,
        connect_deadline_ns: 1_000_000_000,
        sweep_host_cap: 256,
    };

    /// A faster, wider sweep for a network the administrator wants mapped
    /// thoroughly — still bounded, still polite by any scanner's standard.
    pub const THOROUGH: Self = Self {
        probe: true,
        echo_rate: 100,
        echo_burst: 20,
        echo_in_flight: 12,
        connect_in_flight: 16,
        probe_wait_ns: 2_000_000_000,
        connect_deadline_ns: 2_000_000_000,
        sweep_host_cap: 1024,
    };

    /// A short name for the audit trail: the preset this profile is, or
    /// `custom` for any other combination of knobs.
    #[must_use]
    pub fn label(&self) -> &'static str {
        match *self {
            Self::PASSIVE => "passive",
            Self::POLITE => "polite",
            Self::THOROUGH => "thorough",
            _ => "custom",
        }
    }
}

/// What a reachability campaign found about one host.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HostReach {
    /// Whether it answered any probe.
    pub live: bool,
    /// The hop limit its echo reply carried, if it replied to echo.
    pub reply_hop_limit: Option<u8>,
    /// The well-known ports found open on it, in ascending order.
    pub open_ports: Vec<u16>,
}

/// What a whole campaign found, keyed by host address.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Reachability {
    hosts: BTreeMap<IpAddr, HostReach>,
}

impl Reachability {
    /// An empty result.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// What was found about `addr`, if it was probed.
    #[must_use]
    pub fn get(&self, addr: IpAddr) -> Option<&HostReach> {
        self.hosts.get(&addr)
    }

    /// How many probed hosts answered.
    #[must_use]
    pub fn live_count(&self) -> usize {
        self.hosts.values().filter(|reach| reach.live).count()
    }

    /// How many hosts were probed.
    #[must_use]
    pub fn probed_count(&self) -> usize {
        self.hosts.len()
    }

    fn entry(&mut self, addr: IpAddr) -> &mut HostReach {
        self.hosts.entry(addr).or_default()
    }
}

/// A whole campaign to run: the targets and how to probe them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProbeCampaign {
    /// The host addresses to probe, deduplicated and capped by [`targets`].
    pub targets: Vec<IpAddr>,
    /// The preset this campaign probes at.
    pub profile: ProbeProfile,
}

/// What the caller does next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProbeAction {
    /// Send an echo request to `dest`, numbered `sequence`, now.
    SendEcho {
        /// The host to probe.
        dest: IpAddr,
        /// The sequence number the reply is attributed by.
        sequence: u16,
    },
    /// Open a TCP connection to `target` port `port`, tracked as `token`.
    OpenConnect {
        /// The host to check.
        target: IpAddr,
        /// The port to check.
        port: u16,
        /// The handle the verdict and close are attributed by.
        token: u64,
    },
    /// Close the connection tracked as `token`: it answered, or ran out of time.
    CloseConnect {
        /// The connection to close.
        token: u64,
    },
    /// Wait for an answer, but no later than `until_ns`.
    Wait {
        /// When the earliest timer the campaign is waiting on comes due.
        until_ns: u64,
    },
    /// The campaign is complete.
    Done,
}

/// What a TCP connect answered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectOutcome {
    /// The connection opened: the port is open.
    Open,
    /// The connection was refused (a reset answered the SYN): the host is alive,
    /// the port closed.
    Refused,
    /// The connection ended for any other reason (reset mid-stream, peer
    /// unreachable): nothing conclusive about the port.
    Inconclusive,
}

/// An echo probe awaiting its answer.
#[derive(Clone, Copy, Debug)]
struct EchoProbe {
    sequence: u16,
    addr: IpAddr,
    sent_ns: u64,
}

/// A TCP connect awaiting its answer.
#[derive(Clone, Copy, Debug)]
struct ConnectProbe {
    token: u64,
    addr: IpAddr,
    port: u16,
    sent_ns: u64,
}

/// One campaign in progress.
#[derive(Clone, Debug)]
pub struct Probe {
    profile: ProbeProfile,
    bucket: TokenBucket,
    echo_targets: Vec<IpAddr>,
    echo_next: usize,
    echo_in_flight: Vec<EchoProbe>,
    next_sequence: u16,
    connect_queue: VecDeque<(IpAddr, u16)>,
    connect_in_flight: Vec<ConnectProbe>,
    next_token: u64,
    result: Reachability,
}

impl Probe {
    /// A campaign numbering its echo probes from `first_sequence`.
    #[must_use]
    pub fn new(campaign: ProbeCampaign, first_sequence: u16) -> Self {
        let profile = campaign.profile;
        Self {
            profile,
            bucket: TokenBucket::new(profile.echo_burst, profile.echo_rate),
            echo_targets: campaign.targets,
            echo_next: 0,
            echo_in_flight: Vec::new(),
            next_sequence: first_sequence,
            connect_queue: VecDeque::new(),
            connect_in_flight: Vec::new(),
            next_token: 0,
            result: Reachability::new(),
        }
    }

    /// What to do next at `now_ns`. Probes whose wait ran out by then are
    /// settled first — an echo as no answer, a connect by being closed.
    pub fn poll(&mut self, now_ns: u64) -> ProbeAction {
        self.expire_echo(now_ns);
        if let Some(token) = self.take_expired_connect(now_ns) {
            return ProbeAction::CloseConnect { token };
        }
        if self.connect_in_flight.len() < usize::from(self.profile.connect_in_flight) {
            if let Some((addr, port)) = self.connect_queue.pop_front() {
                let token = self.next_token;
                self.next_token += 1;
                self.connect_in_flight.push(ConnectProbe {
                    token,
                    addr,
                    port,
                    sent_ns: now_ns,
                });
                return ProbeAction::OpenConnect {
                    target: addr,
                    port,
                    token,
                };
            }
        }
        if self.echo_in_flight.len() < usize::from(self.profile.echo_in_flight)
            && self.echo_next < self.echo_targets.len()
            && self.bucket.allow(Duration64::from_nanos(now_ns))
        {
            let addr = self.echo_targets[self.echo_next];
            self.echo_next += 1;
            let sequence = self.next_sequence;
            self.next_sequence = self.next_sequence.wrapping_add(1);
            self.echo_in_flight.push(EchoProbe {
                sequence,
                addr,
                sent_ns: now_ns,
            });
            self.result.entry(addr);
            return ProbeAction::SendEcho {
                dest: addr,
                sequence,
            };
        }
        match self.wait_until() {
            Some(until_ns) => ProbeAction::Wait { until_ns },
            None => ProbeAction::Done,
        }
    }

    /// Record that echo `sequence` was answered by `source` with `outcome` at
    /// `now_ns`, its reply carrying `reply_hop_limit`. A reply counts as
    /// liveness only from the host it was sent to — a mismatched source is a
    /// forgery and is ignored (the sweep socket is unconnected, so the stack
    /// cannot filter the peer for us). Answers whether it was one of this
    /// campaign's outstanding probes.
    pub fn answer_echo(
        &mut self,
        sequence: u16,
        source: IpAddr,
        outcome: EchoOutcome,
        reply_hop_limit: Option<u8>,
        _now_ns: u64,
    ) -> bool {
        let Some(position) = self
            .echo_in_flight
            .iter()
            .position(|probe| probe.sequence == sequence)
        else {
            return false;
        };
        let probe = self.echo_in_flight.swap_remove(position);
        if outcome == EchoOutcome::Reply && source == probe.addr {
            let reach = self.result.entry(probe.addr);
            reach.live = true;
            if reply_hop_limit.is_some() {
                reach.reply_hop_limit = reply_hop_limit;
            }
            for &port in WELL_KNOWN_PORTS {
                self.connect_queue.push_back((probe.addr, port));
            }
        }
        true
    }

    /// Drop echo `sequence` because its send was refused: no answer will come,
    /// so it stops being awaited and queues no port check.
    pub fn fail_echo(&mut self, sequence: u16) {
        self.echo_in_flight
            .retain(|probe| probe.sequence != sequence);
    }

    /// Record that connect `token` answered with `outcome` at `now_ns`. Answers
    /// whether it was one of this campaign's outstanding connects.
    pub fn answer_connect(&mut self, token: u64, outcome: ConnectOutcome, _now_ns: u64) -> bool {
        let Some(position) = self
            .connect_in_flight
            .iter()
            .position(|probe| probe.token == token)
        else {
            return false;
        };
        let probe = self.connect_in_flight.swap_remove(position);
        let reach = self.result.entry(probe.addr);
        match outcome {
            ConnectOutcome::Open => {
                reach.live = true;
                if !reach.open_ports.contains(&probe.port) {
                    reach.open_ports.push(probe.port);
                    reach.open_ports.sort_unstable();
                }
            }
            ConnectOutcome::Refused => reach.live = true,
            ConnectOutcome::Inconclusive => {}
        }
        true
    }

    /// Drop connect `token` because its open was refused before it even began.
    pub fn fail_connect(&mut self, token: u64) {
        self.connect_in_flight.retain(|probe| probe.token != token);
    }

    /// Echo probes outstanding, never above the profile's `echo_in_flight`.
    #[must_use]
    pub fn echoes_in_flight(&self) -> usize {
        self.echo_in_flight.len()
    }

    /// Connects outstanding, never above the profile's `connect_in_flight`.
    #[must_use]
    pub fn connects_in_flight(&self) -> usize {
        self.connect_in_flight.len()
    }

    /// The reachability found so far.
    #[must_use]
    pub fn reachability(&self) -> &Reachability {
        &self.result
    }

    /// Take the reachability out once the campaign is [`ProbeAction::Done`].
    #[must_use]
    pub fn into_reachability(self) -> Reachability {
        self.result
    }

    fn expire_echo(&mut self, now_ns: u64) {
        let wait = self.profile.probe_wait_ns;
        self.echo_in_flight
            .retain(|probe| probe.sent_ns.saturating_add(wait) > now_ns);
    }

    fn take_expired_connect(&mut self, now_ns: u64) -> Option<u64> {
        let deadline = self.profile.connect_deadline_ns;
        let position = self
            .connect_in_flight
            .iter()
            .position(|probe| probe.sent_ns.saturating_add(deadline) <= now_ns)?;
        Some(self.connect_in_flight.swap_remove(position).token)
    }

    /// The earliest instant a timer the campaign is waiting on comes due, or
    /// [`None`] when nothing is outstanding and nothing more can be sent.
    fn wait_until(&self) -> Option<u64> {
        let echo = self
            .echo_in_flight
            .iter()
            .map(|probe| probe.sent_ns.saturating_add(self.profile.probe_wait_ns));
        let connect = self.connect_in_flight.iter().map(|probe| {
            probe
                .sent_ns
                .saturating_add(self.profile.connect_deadline_ns)
        });
        let echoes_waiting = self.echo_next < self.echo_targets.len()
            && self.echo_in_flight.len() < usize::from(self.profile.echo_in_flight);
        let bucket = echoes_waiting
            .then(|| self.bucket.ready_at(0))
            .flatten()
            .map(|ready| ready.saturating_total_nanos());
        echo.chain(connect).chain(bucket).min()
    }
}

/// The hosts a campaign at `profile` probes: the known host addresses that are
/// not this machine's, plus every unicast host of each attached IPv4 subnet
/// small enough to sweep whole (its network and broadcast addresses skipped).
/// IPv6 is never swept — only its already-known hosts are probed. The result is
/// deduplicated, known hosts first, and capped at [`MAX_PROBE_TARGETS`].
#[must_use]
pub fn targets(
    local_addrs: &BTreeSet<IpAddr>,
    known_hosts: &BTreeSet<IpAddr>,
    ipv4_subnets: &[(Ipv4Addr, u8)],
    profile: &ProbeProfile,
) -> Vec<IpAddr> {
    let mut out = Vec::new();
    if !profile.probe {
        return out;
    }
    let mut seen = BTreeSet::new();
    let mut push = |addr: IpAddr| {
        if !local_addrs.contains(&addr) && seen.insert(addr) && out.len() < MAX_PROBE_TARGETS {
            out.push(addr);
        }
    };
    for &addr in known_hosts {
        push(addr);
    }
    for &(network, prefix) in ipv4_subnets {
        if u64::from(profile.sweep_host_cap) < ipv4_host_count(prefix) {
            continue;
        }
        for host in ipv4_hosts(network, prefix) {
            push(IpAddr::V4(host));
        }
    }
    out
}

/// How many host addresses an IPv4 subnet of `prefix` has to sweep — every
/// address but its network and directed-broadcast, which the sweep never
/// targets (a `/31` has neither, a `/32` is a single host).
#[must_use]
fn ipv4_host_count(prefix: u8) -> u64 {
    match prefix {
        0..=30 => (1u64 << (32 - u32::from(prefix))).saturating_sub(2),
        31 => 2,
        _ => 1,
    }
}

/// The unicast host addresses of the IPv4 subnet `network/prefix`, network and
/// broadcast excluded. Only called once the subnet is known small enough to
/// sweep, so the range never overflows.
fn ipv4_hosts(network: Ipv4Addr, prefix: u8) -> impl Iterator<Item = Ipv4Addr> {
    let base = u64::from(u32::from(network));
    let size = 1u64 << (32 - u32::from(prefix.min(32)));
    let (first, last) = match prefix {
        32 => (base, base),
        31 => (base, base + 1),
        _ => (base + 1, base + size - 2),
    };
    (first..=last).map(|bits| Ipv4Addr::from(u32::try_from(bits).unwrap_or(u32::MAX)))
}

#[cfg(test)]
#[path = "probe_tests.rs"]
mod tests;
