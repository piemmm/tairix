//! Deterministic fuzz harness for the active reachability probe engine.
//!
//! The engine drives an administrator's liveness sweep from answers it is fed
//! about probes it sent. Those answers come off the network — a less trusted
//! source — so an arbitrary stream of them must never make the engine misbehave.
//! The invariants driven here:
//!
//! * `poll`/`answer_echo`/`answer_connect`/`fail_*` never panic on any stream of
//!   sequences, tokens, outcomes and hop limits, in or out of the set the
//!   engine issued;
//! * the outstanding-echo and outstanding-connect counts never exceed the
//!   profile's fixed bounds, whatever the answers;
//! * every campaign terminates in a bounded number of steps.
//!
//! A plain `cargo test` runs the fixed smoke sweep; `cargo xtask fuzz` extends
//! the loop to a wall-clock budget.

use core::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use tairix_abi::net::EchoOutcome;
use tairix_fuzzseed::Prng;
use tairix_netmap::{ConnectOutcome, Probe, ProbeAction, ProbeCampaign, ProbeProfile};

const SMOKE_ITERATIONS: u64 = 4_000;

/// A random address, mostly IPv4 so the sweep paths see real work.
fn address(rng: &mut Prng) -> IpAddr {
    if rng.below(4) == 0 {
        IpAddr::V6(Ipv6Addr::from(u128::from(rng.next_u64())))
    } else {
        IpAddr::V4(Ipv4Addr::from(rng.next_u32()))
    }
}

/// A random profile whose bounds are small but non-trivial, so the in-flight
/// limits are exercised and campaigns stay short.
fn profile(rng: &mut Prng) -> ProbeProfile {
    match rng.below(4) {
        0 => ProbeProfile::PASSIVE,
        1 => ProbeProfile::POLITE,
        2 => ProbeProfile::THOROUGH,
        _ => ProbeProfile {
            probe: true,
            echo_rate: 1 + rng.next_u32() % 200,
            echo_burst: 1 + rng.next_u32() % 32,
            echo_in_flight: 1 + rng.next_u8() % 12,
            connect_in_flight: 1 + rng.next_u8() % 12,
            probe_wait_ns: 1 + u64::from(rng.next_u32()),
            connect_deadline_ns: 1 + u64::from(rng.next_u32()),
            sweep_host_cap: rng.next_u32() % 512,
        },
    }
}

fn outcome(rng: &mut Prng) -> EchoOutcome {
    match rng.below(5) {
        0 => EchoOutcome::Reply,
        1 => EchoOutcome::HopLimitExceeded,
        2 => EchoOutcome::Unreachable {
            code: rng.next_u8(),
        },
        3 => EchoOutcome::PacketTooBig {
            mtu: rng.next_u32(),
        },
        _ => EchoOutcome::ParameterProblem {
            code: rng.next_u8(),
            pointer: rng.next_u32(),
        },
    }
}

fn connect_outcome(rng: &mut Prng) -> ConnectOutcome {
    match rng.below(3) {
        0 => ConnectOutcome::Open,
        1 => ConnectOutcome::Refused,
        _ => ConnectOutcome::Inconclusive,
    }
}

/// Run one campaign, feeding each action an arbitrary answer, and check the
/// engine's bounds and termination hold throughout.
fn exercise(rng: &mut Prng) {
    let count = rng.below(40);
    let targets: Vec<IpAddr> = (0..count).map(|_| address(rng)).collect();
    let profile = profile(rng);
    let echo_bound = usize::from(profile.echo_in_flight);
    let connect_bound = usize::from(profile.connect_in_flight);
    let mut probe = Probe::new(ProbeCampaign { targets, profile }, rng.next_u16());
    let mut now = u64::from(rng.next_u32());
    let mut steps = 0u64;
    loop {
        steps += 1;
        assert!(steps < 1_000_000, "a campaign must terminate");
        assert!(probe.echoes_in_flight() <= echo_bound);
        assert!(probe.connects_in_flight() <= connect_bound);
        // Occasionally feed a stray answer for a probe the engine never issued.
        if rng.below(8) == 0 {
            probe.answer_echo(
                rng.next_u16(),
                address(rng),
                outcome(rng),
                Some(rng.next_u8()),
                now,
            );
        }
        if rng.below(8) == 0 {
            probe.answer_connect(rng.next_u64(), connect_outcome(rng), now);
        }
        match probe.poll(now) {
            ProbeAction::SendEcho { dest, sequence } => match rng.below(3) {
                0 => {
                    let hop = (rng.below(2) == 0).then(|| rng.next_u8());
                    let source = if rng.below(2) == 0 {
                        dest
                    } else {
                        address(rng)
                    };
                    probe.answer_echo(sequence, source, outcome(rng), hop, now);
                }
                1 => probe.fail_echo(sequence),
                _ => {}
            },
            ProbeAction::OpenConnect { token, .. } => match rng.below(3) {
                0 => {
                    probe.answer_connect(token, connect_outcome(rng), now);
                }
                1 => probe.fail_connect(token),
                _ => {}
            },
            ProbeAction::CloseConnect { .. } => {}
            ProbeAction::Wait { until_ns } => {
                // Advance the clock to at or beyond the deadline, so expiry runs.
                now = until_ns.saturating_add(u64::from(rng.next_u32() % 4));
            }
            ProbeAction::Done => break,
        }
    }
    let reach = probe.into_reachability();
    assert!(reach.live_count() <= reach.probed_count());
}

#[test]
fn an_arbitrary_answer_stream_never_breaks_the_probe_engine() {
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "an_arbitrary_answer_stream_never_breaks_the_probe_engine",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let mut iteration: u64 = 0;
    loop {
        exercise(&mut rng);
        iteration += 1;
        if !tairix_fuzzseed::within_budget(deadline) && iteration >= SMOKE_ITERATIONS {
            break;
        }
    }
}
