use super::*;

use alloc::collections::VecDeque;
use alloc::vec;
use core::cell::RefCell;
use core::net::{Ipv4Addr, Ipv6Addr};
use core::num::NonZeroU8;

use tairix_resolver::TargetFailure;

use crate::command::parse;
use crate::net::Answer;

const TARGET: IpAddr = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 7));

fn router(hop: u8) -> IpAddr {
    IpAddr::V4(Ipv4Addr::new(10, 0, hop, 1))
}

/// What a hop on the scripted path does with a probe that runs out there.
#[derive(Clone, Copy)]
enum HopBehaviour {
    /// A router answers Time Exceeded.
    Router(IpAddr),
    /// Nothing comes back.
    Silent,
    /// A router reports an error instead, ending the path.
    Error(IpAddr, EchoOutcome),
}

/// An in-memory network: hops in front of the destination, then the
/// destination itself, answering after `rtt` each.
struct FakeIo {
    clock: u64,
    rtt: u64,
    path: Vec<HopBehaviour>,
    target: IpAddr,
    resolves: Result<IpAddr, TargetFailure>,
    connect_err: Option<Errno>,
    names: BTreeMap<IpAddr, String>,
    reversed: Vec<IpAddr>,
    pending: VecDeque<(u64, Answer)>,
    sent: Vec<(u16, u8, usize)>,
}

impl FakeIo {
    fn new(path: Vec<HopBehaviour>) -> Self {
        Self {
            clock: 0,
            rtt: 1_234_567,
            path,
            target: TARGET,
            resolves: Ok(TARGET),
            connect_err: None,
            names: BTreeMap::new(),
            reversed: Vec::new(),
            pending: VecDeque::new(),
            sent: Vec::new(),
        }
    }
}

impl TraceIo for FakeIo {
    fn resolve(
        &mut self,
        _host: &str,
        _family: Option<NetAddrFamily>,
    ) -> Result<IpAddr, TargetFailure> {
        self.resolves
    }

    fn reverse(&mut self, addr: IpAddr) -> Option<String> {
        self.reversed.push(addr);
        self.names.get(&addr).cloned()
    }

    fn connect(&mut self, _target: IpAddr) -> Result<(), Errno> {
        self.connect_err.map_or(Ok(()), Err)
    }

    fn now(&self) -> u64 {
        self.clock
    }

    fn send(&mut self, sequence: u16, hop_limit: NonZeroU8, payload: &[u8]) -> Result<(), Errno> {
        self.sent.push((sequence, hop_limit.get(), payload.len()));
        let hop = usize::from(hop_limit.get()) - 1;
        let answer = match self.path.get(hop) {
            None => Some((self.target, EchoOutcome::Reply)),
            Some(HopBehaviour::Router(from)) => Some((*from, EchoOutcome::HopLimitExceeded)),
            Some(HopBehaviour::Error(from, outcome)) => Some((*from, *outcome)),
            Some(HopBehaviour::Silent) => None,
        };
        if let Some((from, outcome)) = answer {
            self.pending.push_back((
                self.clock + self.rtt,
                Answer {
                    sequence,
                    from,
                    outcome,
                },
            ));
        }
        Ok(())
    }

    fn await_network(&mut self, deadline_ns: u64) -> bool {
        self.clock = self.clock.max(deadline_ns);
        false
    }

    fn recv(&mut self, deadline_ns: u64) -> Result<Option<Answer>, Errno> {
        match self.pending.front() {
            Some(&(at, answer)) if at <= deadline_ns => {
                self.pending.pop_front();
                self.clock = self.clock.max(at);
                Ok(Some(answer))
            }
            _ => {
                self.clock = self.clock.max(deadline_ns);
                Ok(None)
            }
        }
    }
}

/// Captures every line written.
struct Lines(RefCell<Vec<String>>);

impl Output for Lines {
    fn write_line(&self, line: &str) -> Result<(), Errno> {
        self.0.borrow_mut().push(line.to_string());
        Ok(())
    }
}

fn traced(args: &[&str], io: &mut FakeIo) -> Vec<String> {
    let Ok(Command::Run(config)) = parse(args) else {
        panic!("a run");
    };
    let out = Lines(RefCell::new(Vec::new()));
    trace(&config, io, &out).expect("trace");
    out.0.into_inner()
}

#[test]
fn a_reachable_host_prints_each_hop_and_stops_at_the_destination() {
    let mut io = FakeIo::new(vec![
        HopBehaviour::Router(router(1)),
        HopBehaviour::Router(router(2)),
    ]);
    let lines = traced(&["-n", "example.test"], &mut io);
    assert_eq!(
        lines,
        [
            "traceroute to example.test (192.0.2.7), 30 hops max, 60 byte packets",
            " 1  10.0.1.1  1.235 ms  1.235 ms  1.235 ms",
            " 2  10.0.2.1  1.235 ms  1.235 ms  1.235 ms",
            " 3  192.0.2.7  1.235 ms  1.235 ms  1.235 ms",
        ]
    );
    assert!(
        io.sent.iter().all(|&(_, _, len)| len == 32),
        "the default probe carries 32 data bytes"
    );
}

#[test]
fn a_silent_hop_prints_stars_and_the_trace_goes_on() {
    let mut io = FakeIo::new(vec![HopBehaviour::Silent, HopBehaviour::Router(router(2))]);
    let lines = traced(&["-n", "-w", "1", "-q", "2", "host"], &mut io);
    assert_eq!(lines[1], " 1  * *");
    assert_eq!(lines[2], " 2  10.0.2.1  1.235 ms  1.235 ms");
    assert_eq!(lines[3], " 3  192.0.2.7  1.235 ms  1.235 ms");
}

#[test]
fn a_router_that_reports_unreachable_ends_the_trace_with_its_annotation() {
    let mut io = FakeIo::new(vec![
        HopBehaviour::Router(router(1)),
        HopBehaviour::Error(router(2), EchoOutcome::Unreachable { code: 1 }),
        HopBehaviour::Router(router(3)),
    ]);
    let lines = traced(&["-n", "-q", "1", "host"], &mut io);
    assert_eq!(lines.len(), 3, "{lines:?}");
    assert_eq!(lines[2], " 2  10.0.2.1  1.235 ms !H");
}

#[test]
fn a_packet_too_big_carries_its_mtu() {
    let mut io = FakeIo::new(vec![HopBehaviour::Error(
        router(1),
        EchoOutcome::PacketTooBig { mtu: 1400 },
    )]);
    let lines = traced(&["-n", "-q", "1", "host", "1500"], &mut io);
    assert_eq!(
        lines[0],
        "traceroute to host (192.0.2.7), 30 hops max, 1500 byte packets"
    );
    assert_eq!(lines[1], " 1  10.0.1.1  1.235 ms !F-1400");
}

#[test]
fn hops_are_named_once_and_an_unnamed_one_shows_its_address_twice() {
    let mut io = FakeIo::new(vec![HopBehaviour::Router(router(1))]);
    io.names.insert(router(1), String::from("edge.example"));
    let lines = traced(&["-q", "2", "host"], &mut io);
    assert_eq!(lines[1], " 1  edge.example (10.0.1.1)  1.235 ms  1.235 ms");
    assert_eq!(lines[2], " 2  192.0.2.7 (192.0.2.7)  1.235 ms  1.235 ms");
    assert_eq!(
        io.reversed
            .iter()
            .filter(|&&addr| addr == router(1))
            .count(),
        1,
        "an address is looked up once however many probes it answers"
    );
}

#[test]
fn numeric_output_issues_no_reverse_lookup() {
    let mut io = FakeIo::new(vec![HopBehaviour::Router(router(1))]);
    let _ = traced(&["-n", "host"], &mut io);
    assert!(io.reversed.is_empty());
}

#[test]
fn an_unreached_host_stops_at_the_hop_bound() {
    let mut io = FakeIo::new(
        (1..=5)
            .map(|hop| HopBehaviour::Router(router(hop)))
            .collect(),
    );
    let lines = traced(&["-n", "-m", "3", "-q", "1", "host"], &mut io);
    assert_eq!(lines.len(), 4);
    assert_eq!(lines[3], " 3  10.0.3.1  1.235 ms");
}

#[test]
fn the_first_hop_option_skips_the_near_ones() {
    let mut io = FakeIo::new(vec![
        HopBehaviour::Router(router(1)),
        HopBehaviour::Router(router(2)),
    ]);
    let lines = traced(&["-n", "-f", "2", "-q", "1", "host"], &mut io);
    assert_eq!(lines[1], " 2  10.0.2.1  1.235 ms");
    assert_eq!(lines[2], " 3  192.0.2.7  1.235 ms");
}

#[test]
fn a_v6_trace_uses_the_v6_header_and_vocabulary() {
    let target = IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 7));
    let router = IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 1));
    let mut io = FakeIo::new(vec![HopBehaviour::Error(
        router,
        EchoOutcome::Unreachable { code: 0 },
    )]);
    io.target = target;
    io.resolves = Ok(target);
    let lines = traced(&["-n", "-q", "1", "host"], &mut io);
    assert_eq!(
        lines[0],
        "traceroute to host (2001:db8::7), 30 hops max, 80 byte packets"
    );
    assert_eq!(lines[1], " 1  2001:db8::1  1.235 ms !N");
}

#[test]
fn an_unresolvable_target_fails_naming_the_host() {
    let mut io = FakeIo::new(Vec::new());
    io.resolves = Err(TargetFailure::Unknown);
    let Ok(Command::Run(config)) = parse(&["nope.invalid"]) else {
        panic!("a run");
    };
    let out = Lines(RefCell::new(Vec::new()));
    assert_eq!(
        trace(&config, &mut io, &out),
        Err(TraceError::Resolve {
            host: String::from("nope.invalid"),
            reason: TargetFailure::Unknown,
        })
    );
    assert!(io.sent.is_empty());
}

#[test]
fn a_refused_socket_is_a_capability_denial() {
    let mut io = FakeIo::new(Vec::new());
    io.connect_err = Some(Errno::PermissionDenied);
    let Ok(Command::Run(config)) = parse(&["host"]) else {
        panic!("a run");
    };
    let out = Lines(RefCell::new(Vec::new()));
    assert_eq!(trace(&config, &mut io, &out), Err(TraceError::Denied));
}

#[test]
fn every_error_has_its_traceroute_annotation() {
    let v4 = NetAddrFamily::V4;
    let v6 = NetAddrFamily::V6;
    let unreachable = |code| EchoOutcome::Unreachable { code };
    assert_eq!(annotation(v4, EchoOutcome::Reply), None);
    assert_eq!(annotation(v4, EchoOutcome::HopLimitExceeded), None);
    assert_eq!(
        annotation(v4, unreachable(3)),
        None,
        "port unreachable means reached"
    );
    assert_eq!(annotation(v6, unreachable(4)), None);
    assert_eq!(annotation(v4, unreachable(0)).as_deref(), Some("!N"));
    assert_eq!(annotation(v4, unreachable(1)).as_deref(), Some("!H"));
    assert_eq!(annotation(v4, unreachable(2)).as_deref(), Some("!P"));
    assert_eq!(annotation(v4, unreachable(13)).as_deref(), Some("!X"));
    assert_eq!(annotation(v4, unreachable(5)).as_deref(), Some("!S"));
    assert_eq!(annotation(v4, unreachable(14)).as_deref(), Some("!V"));
    assert_eq!(annotation(v4, unreachable(15)).as_deref(), Some("!C"));
    assert_eq!(annotation(v4, unreachable(42)).as_deref(), Some("!<42>"));
    assert_eq!(annotation(v6, unreachable(0)).as_deref(), Some("!N"));
    assert_eq!(annotation(v6, unreachable(1)).as_deref(), Some("!X"));
    assert_eq!(annotation(v6, unreachable(3)).as_deref(), Some("!H"));
    assert_eq!(
        annotation(
            v4,
            EchoOutcome::ParameterProblem {
                code: 0,
                pointer: 1
            }
        )
        .as_deref(),
        Some("!<12-0>")
    );
    assert_eq!(
        annotation(
            v6,
            EchoOutcome::ParameterProblem {
                code: 1,
                pointer: 40
            }
        )
        .as_deref(),
        Some("!<4-1>")
    );
}

#[test]
fn the_probe_data_is_the_classic_pattern() {
    let data = probe_data(70);
    assert_eq!(&data[..3], b"@AB");
    assert_eq!(data[63], 0x7f);
    assert_eq!(data[64], 0x40, "the pattern repeats every 64 bytes");
}
