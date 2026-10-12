use super::*;

use core::net::Ipv4Addr;

use tairix_fuzzseed::Prng;

const WAIT: u64 = 1_000;

fn hop(value: u8) -> NonZeroU8 {
    NonZeroU8::new(value).expect("a non-zero hop")
}

fn config(max_hops: u8, probes_per_hop: u8, in_flight: u8) -> TraceConfig {
    TraceConfig {
        first_hop: hop(1),
        max_hops: hop(max_hops),
        probes_per_hop: hop(probes_per_hop),
        in_flight: hop(in_flight),
        wait_ns: WAIT,
    }
}

fn router(n: u8) -> IpAddr {
    IpAddr::V4(Ipv4Addr::new(10, 0, n, 1))
}

const TARGET: IpAddr = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 7));

/// Drive `trace` to completion against a path whose destination sits at hop
/// `distance`, every router answering, and return the sends in order.
fn walk(trace: &mut PathTrace, distance: u8) -> Vec<(u8, u16)> {
    let mut sends = Vec::new();
    let mut now = 0u64;
    loop {
        match trace.poll(now) {
            Action::Send {
                hop_limit,
                sequence,
            } => {
                sends.push((hop_limit.get(), sequence));
                let answer = if hop_limit.get() >= distance {
                    (TARGET, EchoOutcome::Reply)
                } else {
                    (router(hop_limit.get()), EchoOutcome::HopLimitExceeded)
                };
                now += 10;
                assert!(trace.answer(sequence, answer.0, answer.1, now));
            }
            Action::Wait { until_ns } => now = until_ns,
            Action::Done => return sends,
        }
    }
}

#[test]
fn a_reachable_destination_ends_the_trace_at_its_own_hop() {
    let mut trace = PathTrace::new(config(30, 3, 1), 1).expect("trace");
    let sends = walk(&mut trace, 3);
    assert_eq!(sends.len(), 9, "three hops of three probes");
    assert_eq!(
        sends.iter().map(|&(limit, _)| limit).collect::<Vec<_>>(),
        [1, 1, 1, 2, 2, 2, 3, 3, 3]
    );
    assert_eq!(
        sends.iter().map(|&(_, seq)| seq).collect::<Vec<_>>(),
        (1..=9).collect::<Vec<u16>>(),
        "probes are numbered consecutively from the first sequence"
    );
    assert!(trace.reached());
    assert_eq!(trace.hops().len(), 3);
    assert_eq!(trace.settled_hops(), 3);
    match trace.hops()[1].probes[0] {
        Probe::Answered { from, outcome, .. } => {
            assert_eq!(from, router(2));
            assert_eq!(outcome, EchoOutcome::HopLimitExceeded);
        }
        other => panic!("hop 2 was answered: {other:?}"),
    }
}

#[test]
fn a_silent_hop_is_lost_at_its_deadline_and_the_trace_goes_on() {
    let mut trace = PathTrace::new(config(30, 1, 1), 1).expect("trace");
    let Action::Send { sequence, .. } = trace.poll(0) else {
        panic!("the first probe goes out");
    };
    assert_eq!(trace.poll(5), Action::Wait { until_ns: WAIT });
    let Action::Send {
        hop_limit,
        sequence: next,
    } = trace.poll(WAIT)
    else {
        panic!("the next hop is probed once the first is lost");
    };
    assert_eq!(hop_limit.get(), 2);
    assert_ne!(next, sequence);
    assert_eq!(trace.hops()[0].probes, [Probe::Lost]);
    assert!(
        !trace.answer(sequence, router(1), EchoOutcome::HopLimitExceeded, WAIT + 1),
        "an answer after its deadline is not taken"
    );
    assert_eq!(trace.hops()[0].probes, [Probe::Lost]);
}

#[test]
fn concurrent_probes_beyond_the_destination_are_abandoned() {
    let mut trace = PathTrace::new(config(30, 2, 16), 100).expect("trace");
    let mut sends = Vec::new();
    while let Action::Send {
        hop_limit,
        sequence,
    } = trace.poll(0)
    {
        sends.push((hop_limit.get(), sequence));
    }
    assert_eq!(sends.len(), 16, "the in-flight bound caps the burst");
    // Hop 3's first probe reaches the destination.
    let (_, at_three) = sends[4];
    assert!(trace.answer(at_three, TARGET, EchoOutcome::Reply, 5));
    assert_eq!(
        trace.hops().len(),
        3,
        "nothing past the destination is kept"
    );
    let (_, beyond) = sends[10];
    assert!(
        !trace.answer(beyond, TARGET, EchoOutcome::Reply, 6),
        "a probe past the end is no longer outstanding"
    );
    // The hops up to the destination still settle in full.
    for &(limit, seq) in &sends[..6] {
        if seq != at_three {
            let from = if limit == 3 { TARGET } else { router(limit) };
            let outcome = if limit == 3 {
                EchoOutcome::Reply
            } else {
                EchoOutcome::HopLimitExceeded
            };
            assert!(trace.answer(seq, from, outcome, 7));
        }
    }
    assert_eq!(trace.poll(8), Action::Done);
    assert!(trace.reached());
    assert_eq!(trace.settled_hops(), 3);
}

#[test]
fn a_terminal_error_from_a_router_ends_the_path_there() {
    let mut trace = PathTrace::new(config(30, 1, 1), 1).expect("trace");
    let Action::Send { sequence, .. } = trace.poll(0) else {
        panic!("probe");
    };
    assert!(trace.answer(sequence, router(1), EchoOutcome::HopLimitExceeded, 1));
    let Action::Send { sequence, .. } = trace.poll(1) else {
        panic!("probe");
    };
    assert!(trace.answer(sequence, router(2), EchoOutcome::Unreachable { code: 0 }, 2));
    assert_eq!(trace.poll(2), Action::Done);
    assert!(!trace.reached(), "the destination itself never answered");
    assert_eq!(trace.hops().len(), 2);
}

#[test]
fn an_unknown_sequence_changes_nothing() {
    let mut trace = PathTrace::new(config(30, 1, 1), 1).expect("trace");
    let Action::Send { sequence, .. } = trace.poll(0) else {
        panic!("probe");
    };
    assert!(!trace.answer(sequence.wrapping_add(1), TARGET, EchoOutcome::Reply, 1));
    assert_eq!(trace.hops()[0].probes, [Probe::Pending]);
    assert!(trace.answer(sequence, TARGET, EchoOutcome::Reply, 1));
    assert!(
        !trace.answer(sequence, TARGET, EchoOutcome::Reply, 2),
        "a duplicate answer is not taken twice"
    );
}

#[test]
fn an_unreached_destination_stops_at_the_hop_bound() {
    let mut trace = PathTrace::new(config(4, 2, 3), 1).expect("trace");
    let sends = walk(&mut trace, u8::MAX);
    assert_eq!(sends.len(), 8);
    assert_eq!(trace.hops().len(), 4);
    assert!(!trace.reached());
    assert_eq!(trace.settled_hops(), 4);
}

#[test]
fn the_first_hop_need_not_be_one() {
    let mut trace = PathTrace::new(
        TraceConfig {
            first_hop: hop(5),
            ..config(7, 1, 1)
        },
        1,
    )
    .expect("trace");
    let sends = walk(&mut trace, u8::MAX);
    assert_eq!(
        sends.iter().map(|&(limit, _)| limit).collect::<Vec<_>>(),
        [5, 6, 7]
    );
}

#[test]
fn an_untraceable_configuration_is_refused() {
    assert_eq!(
        PathTrace::new(
            TraceConfig {
                first_hop: hop(9),
                ..config(8, 1, 1)
            },
            1
        )
        .err(),
        Some(TraceConfigError::FirstHopBeyondMax)
    );
    assert_eq!(
        config(8, MAX_PROBES_PER_HOP + 1, 1).validate(),
        Err(TraceConfigError::TooManyProbesPerHop)
    );
    assert_eq!(
        config(8, 1, MAX_IN_FLIGHT + 1).validate(),
        Err(TraceConfigError::TooManyInFlight)
    );
    assert_eq!(
        TraceConfig {
            wait_ns: 0,
            ..config(8, 1, 1)
        }
        .validate(),
        Err(TraceConfigError::NoWait)
    );
}

/// Against a path that answers, drops and errs at random, a trace keeps
/// every bound and always finishes.
#[test]
fn random_paths_keep_the_bounds_and_always_finish() {
    let mut rng = Prng::new(0x7ACE_0001);
    for _ in 0..500 {
        let max_hops = u8::try_from(1 + rng.below(40)).expect("small");
        let per_hop = u8::try_from(1 + rng.below(4)).expect("small");
        let in_flight = u8::try_from(1 + rng.below(20)).expect("small");
        let mut trace = PathTrace::new(config(max_hops, per_hop, in_flight), 7).expect("trace");
        let mut outstanding: Vec<(u16, u8)> = Vec::new();
        let mut seen = alloc::collections::BTreeSet::new();
        let mut now = 0u64;
        for _ in 0..10_000 {
            match trace.poll(now) {
                Action::Send {
                    hop_limit,
                    sequence,
                } => {
                    assert!(hop_limit.get() <= max_hops);
                    assert!(seen.insert(sequence), "a sequence is never reused");
                    outstanding.push((sequence, hop_limit.get()));
                    assert!(trace.in_flight.len() <= usize::from(in_flight));
                }
                Action::Wait { until_ns } => {
                    assert!(until_ns > now, "a wait always ends in the future");
                    if outstanding.is_empty() || rng.below(4) == 0 {
                        now = until_ns;
                    } else {
                        let pick = rng.below(outstanding.len());
                        let (sequence, limit) = outstanding.swap_remove(pick);
                        let outcome = match rng.below(10) {
                            0 => EchoOutcome::Reply,
                            1 => EchoOutcome::Unreachable { code: 1 },
                            _ => EchoOutcome::HopLimitExceeded,
                        };
                        now += 1;
                        let _ = trace.answer(sequence, router(limit), outcome, now);
                    }
                }
                Action::Done => break,
            }
        }
        assert_eq!(trace.poll(now), Action::Done, "every trace finishes");
        assert_eq!(trace.settled_hops(), trace.hops().len());
        for (index, row) in trace.hops().iter().enumerate() {
            assert_eq!(usize::from(row.hop_limit.get()), index + 1);
            assert!(row.probes.iter().all(Probe::is_settled));
        }
    }
}
