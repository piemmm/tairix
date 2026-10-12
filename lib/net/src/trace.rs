//! Hop-by-hop path tracing — the traceroute method — over hop-limited echo
//! probes, as a pure state machine its caller drives with its own clock and
//! echo socket.
//!
//! Probes go out in hop order, a bounded number in flight at once, each with a
//! sequence number the trace assigns and a deadline the caller's clock
//! enforces. What becomes of each is fed back as the echo socket reports it.
//! The trace covers every hop up to the one where the destination answered or
//! a terminal error stopped the path, or up to its hop bound; probes beyond
//! that hop are abandoned. Nothing here sends or waits: [`PathTrace::poll`]
//! says what to do next.

use alloc::vec::Vec;
use core::net::IpAddr;
use core::num::NonZeroU8;

use tairix_abi::net::EchoOutcome;

/// Most probes one hop carries.
pub const MAX_PROBES_PER_HOP: u8 = 10;
/// Most probes outstanding at once.
pub const MAX_IN_FLIGHT: u8 = 64;

/// How a trace probes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TraceConfig {
    /// The hop limit the first probes carry.
    pub first_hop: NonZeroU8,
    /// The highest hop limit probed.
    pub max_hops: NonZeroU8,
    /// Probes sent at each hop limit.
    pub probes_per_hop: NonZeroU8,
    /// Probes outstanding at once.
    pub in_flight: NonZeroU8,
    /// How long a probe is awaited before it counts as lost.
    pub wait_ns: u64,
}

/// Why a [`TraceConfig`] cannot be traced.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TraceConfigError {
    /// The first hop lies beyond the last.
    FirstHopBeyondMax,
    /// More than [`MAX_PROBES_PER_HOP`] probes per hop.
    TooManyProbesPerHop,
    /// More than [`MAX_IN_FLIGHT`] probes outstanding.
    TooManyInFlight,
    /// A zero wait would lose every probe.
    NoWait,
}

impl core::fmt::Display for TraceConfigError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::FirstHopBeyondMax => f.write_str("the first hop lies beyond the last"),
            Self::TooManyProbesPerHop => {
                write!(f, "more than {MAX_PROBES_PER_HOP} probes per hop")
            }
            Self::TooManyInFlight => write!(f, "more than {MAX_IN_FLIGHT} probes outstanding"),
            Self::NoWait => f.write_str("a probe must be awaited for some time"),
        }
    }
}

impl TraceConfig {
    /// Check that the configuration can be traced.
    ///
    /// # Errors
    ///
    /// The first bound it breaks.
    pub fn validate(self) -> Result<Self, TraceConfigError> {
        if self.first_hop > self.max_hops {
            Err(TraceConfigError::FirstHopBeyondMax)
        } else if self.probes_per_hop.get() > MAX_PROBES_PER_HOP {
            Err(TraceConfigError::TooManyProbesPerHop)
        } else if self.in_flight.get() > MAX_IN_FLIGHT {
            Err(TraceConfigError::TooManyInFlight)
        } else if self.wait_ns == 0 {
            Err(TraceConfigError::NoWait)
        } else {
            Ok(self)
        }
    }
}

/// What became of one probe.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Probe {
    /// Sent, and nothing back yet.
    Pending,
    /// The destination or a router answered.
    Answered {
        /// Who answered.
        from: IpAddr,
        /// The round trip, in nanoseconds.
        rtt_ns: u64,
        /// What the answer said.
        outcome: EchoOutcome,
    },
    /// Nothing came back in time.
    Lost,
}

impl Probe {
    /// Whether the probe's fate is known.
    #[must_use]
    pub fn is_settled(&self) -> bool {
        !matches!(self, Self::Pending)
    }
}

/// One hop limit's probes, in the order they were sent.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Hop {
    /// The hop limit its probes carried.
    pub hop_limit: NonZeroU8,
    /// Its probes so far.
    pub probes: Vec<Probe>,
}

/// What the caller does next.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Action {
    /// Send probe `sequence` carrying `hop_limit` now.
    Send {
        /// The hop limit to send it with.
        hop_limit: NonZeroU8,
        /// Its sequence number.
        sequence: u16,
    },
    /// Wait for an answer, but no later than `until_ns`.
    Wait {
        /// When the earliest outstanding probe runs out of time.
        until_ns: u64,
    },
    /// The trace is complete.
    Done,
}

/// A probe awaiting its answer.
#[derive(Clone, Copy, Debug)]
struct InFlight {
    sequence: u16,
    hop: usize,
    probe: usize,
    sent_ns: u64,
}

/// One trace in progress.
#[derive(Clone, Debug)]
pub struct PathTrace {
    config: TraceConfig,
    first_sequence: u16,
    sent: u16,
    hops: Vec<Hop>,
    in_flight: Vec<InFlight>,
    /// The index of the last hop the trace covers, once an answer fixed it.
    end: Option<usize>,
}

impl PathTrace {
    /// A trace with `config`, numbering its probes from `first_sequence`.
    ///
    /// # Errors
    ///
    /// The bound `config` breaks.
    pub fn new(config: TraceConfig, first_sequence: u16) -> Result<Self, TraceConfigError> {
        let config = config.validate()?;
        Ok(Self {
            config,
            first_sequence,
            sent: 0,
            hops: Vec::new(),
            in_flight: Vec::with_capacity(usize::from(config.in_flight.get())),
            end: None,
        })
    }

    /// What to do next at `now_ns`. Probes whose wait ran out by then are
    /// settled as lost first.
    pub fn poll(&mut self, now_ns: u64) -> Action {
        self.expire(now_ns);
        if self.in_flight.len() < usize::from(self.config.in_flight.get()) {
            if let Some((hop, probe, hop_limit)) = self.next_probe() {
                return self.send(hop, probe, hop_limit, now_ns);
            }
        }
        let wait = self.config.wait_ns;
        self.in_flight
            .iter()
            .map(|probe| probe.sent_ns.saturating_add(wait))
            .min()
            .map_or(Action::Done, |until_ns| Action::Wait { until_ns })
    }

    /// Record what became of probe `sequence`, answered by `from` at
    /// `now_ns`. Answers whether it was one of this trace's outstanding
    /// probes; anything else — a late answer, a duplicate, another socket's
    /// sequence — changes nothing.
    pub fn answer(
        &mut self,
        sequence: u16,
        from: IpAddr,
        outcome: EchoOutcome,
        now_ns: u64,
    ) -> bool {
        let Some(position) = self
            .in_flight
            .iter()
            .position(|probe| probe.sequence == sequence)
        else {
            return false;
        };
        let probe = self.in_flight.swap_remove(position);
        if let Some(slot) = slot(&mut self.hops, &probe) {
            *slot = Probe::Answered {
                from,
                rtt_ns: now_ns.saturating_sub(probe.sent_ns),
                outcome,
            };
        }
        if outcome != EchoOutcome::HopLimitExceeded {
            self.end_at(probe.hop);
        }
        true
    }

    /// The hops probed so far, nearest first, never past the hop that ended
    /// the trace.
    #[must_use]
    pub fn hops(&self) -> &[Hop] {
        &self.hops
    }

    /// How many leading hops have every probe settled — the ones a caller
    /// printing in order may show.
    #[must_use]
    pub fn settled_hops(&self) -> usize {
        let per_hop = usize::from(self.config.probes_per_hop.get());
        self.hops
            .iter()
            .take_while(|hop| {
                hop.probes.len() == per_hop && hop.probes.iter().all(Probe::is_settled)
            })
            .count()
    }

    /// Whether the destination itself answered.
    #[must_use]
    pub fn reached(&self) -> bool {
        self.end
            .and_then(|end| self.hops.get(end))
            .is_some_and(|hop| {
                hop.probes.iter().any(|probe| {
                    matches!(
                        probe,
                        Probe::Answered {
                            outcome: EchoOutcome::Reply,
                            ..
                        }
                    )
                })
            })
    }

    /// The next probe to send, as (hop index, probe index, hop limit), or
    /// [`None`] once every probe the trace will send has gone.
    fn next_probe(&self) -> Option<(usize, usize, NonZeroU8)> {
        let per_hop = usize::from(self.config.probes_per_hop.get());
        let index = usize::from(self.sent);
        let (hop, probe) = (index / per_hop, index % per_hop);
        if self.end.is_some_and(|end| hop > end) {
            return None;
        }
        let limit = usize::from(self.config.first_hop.get()) + hop;
        if limit > usize::from(self.config.max_hops.get()) {
            return None;
        }
        let hop_limit = NonZeroU8::new(u8::try_from(limit).ok()?)?;
        Some((hop, probe, hop_limit))
    }

    fn send(&mut self, hop: usize, probe: usize, hop_limit: NonZeroU8, now_ns: u64) -> Action {
        if hop == self.hops.len() {
            self.hops.push(Hop {
                hop_limit,
                probes: Vec::with_capacity(usize::from(self.config.probes_per_hop.get())),
            });
        }
        if let Some(row) = self.hops.get_mut(hop) {
            row.probes.push(Probe::Pending);
        }
        let sequence = self.first_sequence.wrapping_add(self.sent);
        self.sent += 1;
        self.in_flight.push(InFlight {
            sequence,
            hop,
            probe,
            sent_ns: now_ns,
        });
        Action::Send {
            hop_limit,
            sequence,
        }
    }

    fn expire(&mut self, now_ns: u64) {
        let wait = self.config.wait_ns;
        let hops = &mut self.hops;
        self.in_flight.retain(|probe| {
            let due = probe.sent_ns.saturating_add(wait) <= now_ns;
            if let Some(slot) = slot(hops, probe).filter(|_| due) {
                *slot = Probe::Lost;
            }
            !due
        });
    }

    /// The path ends at `hop`: nothing past it is probed or kept.
    fn end_at(&mut self, hop: usize) {
        let end = self.end.map_or(hop, |end| end.min(hop));
        self.end = Some(end);
        self.hops.truncate(end + 1);
        self.in_flight.retain(|probe| probe.hop <= end);
    }
}

/// The record `probe` settles into.
fn slot<'a>(hops: &'a mut [Hop], probe: &InFlight) -> Option<&'a mut Probe> {
    hops.get_mut(probe.hop)?.probes.get_mut(probe.probe)
}

#[cfg(test)]
#[path = "trace_tests.rs"]
mod tests;
