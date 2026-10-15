use super::*;
use alloc::vec;
use core::net::{Ipv4Addr, Ipv6Addr};

fn v4(a: u8, b: u8, c: u8, d: u8) -> IpAddr {
    IpAddr::V4(Ipv4Addr::new(a, b, c, d))
}

fn set(addrs: &[IpAddr]) -> BTreeSet<IpAddr> {
    addrs.iter().copied().collect()
}

/// A profile whose timings make deadlines land on round, testable instants.
fn test_profile() -> ProbeProfile {
    ProbeProfile {
        probe: true,
        echo_rate: 1000,
        echo_burst: 100,
        echo_in_flight: 4,
        connect_in_flight: 4,
        probe_wait_ns: 1_000,
        connect_deadline_ns: 1_000,
        sweep_host_cap: 256,
    }
}

fn campaign(targets: &[IpAddr], profile: ProbeProfile) -> ProbeCampaign {
    ProbeCampaign {
        targets: targets.to_vec(),
        profile,
    }
}

#[test]
fn targets_puts_known_hosts_first_dedups_and_skips_local() {
    let local = set(&[v4(192, 168, 1, 10)]);
    let known = set(&[
        v4(192, 168, 1, 1),
        v4(192, 168, 1, 10),
        IpAddr::V6(Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 1)),
    ]);
    let out = targets(&local, &known, &[], &ProbeProfile::POLITE);
    assert!(out.contains(&v4(192, 168, 1, 1)));
    assert!(out.contains(&IpAddr::V6(Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 1))));
    assert!(
        !out.contains(&v4(192, 168, 1, 10)),
        "this machine is not probed"
    );
    // Deduplicated: the local address appears once in `known` and is dropped,
    // so no address repeats.
    let mut sorted = out.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(sorted.len(), out.len());
}

#[test]
fn a_small_subnet_is_swept_without_its_network_or_broadcast() {
    let out = targets(
        &BTreeSet::new(),
        &BTreeSet::new(),
        &[(Ipv4Addr::new(192, 168, 1, 0), 29)],
        &ProbeProfile::POLITE,
    );
    assert!(
        !out.contains(&v4(192, 168, 1, 0)),
        "network address skipped"
    );
    assert!(!out.contains(&v4(192, 168, 1, 7)), "broadcast skipped");
    assert!(out.contains(&v4(192, 168, 1, 1)));
    assert!(out.contains(&v4(192, 168, 1, 6)));
    assert_eq!(out.len(), 6, "a /29 has six unicast hosts");
}

#[test]
fn a_31_sweeps_both_hosts_and_a_32_just_one() {
    let thirty_one = targets(
        &BTreeSet::new(),
        &BTreeSet::new(),
        &[(Ipv4Addr::new(10, 0, 0, 0), 31)],
        &ProbeProfile::POLITE,
    );
    assert_eq!(thirty_one, vec![v4(10, 0, 0, 0), v4(10, 0, 0, 1)]);
    let thirty_two = targets(
        &BTreeSet::new(),
        &BTreeSet::new(),
        &[(Ipv4Addr::new(10, 0, 0, 5), 32)],
        &ProbeProfile::POLITE,
    );
    assert_eq!(thirty_two, vec![v4(10, 0, 0, 5)]);
}

#[test]
fn a_subnet_larger_than_the_cap_is_not_swept() {
    let out = targets(
        &BTreeSet::new(),
        &BTreeSet::new(),
        &[(Ipv4Addr::new(10, 0, 0, 0), 8)],
        &ProbeProfile::POLITE,
    );
    assert!(out.is_empty(), "a /8 dwarfs the polite sweep cap");
}

#[test]
fn passive_probes_nothing_and_ipv6_is_never_swept() {
    assert!(targets(
        &BTreeSet::new(),
        &set(&[v4(192, 168, 1, 1)]),
        &[(Ipv4Addr::new(192, 168, 1, 0), 29)],
        &ProbeProfile::PASSIVE,
    )
    .is_empty());
    // Only known IPv6 hosts are probed; there is no IPv6 subnet argument at all,
    // so a blind IPv6 sweep is structurally impossible.
    let known_v6 = IpAddr::V6(Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 2));
    let out = targets(
        &BTreeSet::new(),
        &set(&[known_v6]),
        &[],
        &ProbeProfile::POLITE,
    );
    assert_eq!(out, vec![known_v6]);
}

#[test]
fn the_target_set_is_capped() {
    let mut subnets = Vec::new();
    // Many /24s, each 254 hosts, dwarf the global cap together.
    for third in 0..=40u8 {
        subnets.push((Ipv4Addr::new(10, 0, third, 0), 24));
    }
    let profile = ProbeProfile {
        sweep_host_cap: 1024,
        ..ProbeProfile::POLITE
    };
    let out = targets(&BTreeSet::new(), &BTreeSet::new(), &subnets, &profile);
    assert_eq!(out.len(), MAX_PROBE_TARGETS);
}

#[test]
fn echo_concurrency_is_bounded() {
    let mut probe = Probe::new(
        campaign(
            &[
                v4(10, 0, 0, 1),
                v4(10, 0, 0, 2),
                v4(10, 0, 0, 3),
                v4(10, 0, 0, 4),
            ],
            test_profile(),
        ),
        0,
    );
    let mut in_flight = 0;
    loop {
        match probe.poll(0) {
            ProbeAction::SendEcho { .. } => in_flight += 1,
            ProbeAction::Wait { .. } => break,
            other => panic!("unexpected {other:?}"),
        }
    }
    assert_eq!(in_flight, usize::from(test_profile().echo_in_flight));
}

#[test]
fn pacing_waits_for_the_next_token() {
    let profile = ProbeProfile {
        echo_burst: 1,
        echo_rate: 1,
        echo_in_flight: 10,
        // A long per-probe wait so the bucket, not the in-flight deadline, is
        // what the second poll waits on.
        probe_wait_ns: 5_000_000_000,
        ..test_profile()
    };
    let mut probe = Probe::new(campaign(&[v4(10, 0, 0, 1), v4(10, 0, 0, 2)], profile), 0);
    assert!(matches!(probe.poll(0), ProbeAction::SendEcho { .. }));
    // The one burst token is spent; the next refills one second out, sooner
    // than the outstanding probe's five-second wait.
    assert_eq!(
        probe.poll(0),
        ProbeAction::Wait {
            until_ns: 1_000_000_000
        }
    );
}

#[test]
fn a_reply_marks_live_records_the_hop_limit_and_queues_the_port_check() {
    let dest = v4(10, 0, 0, 1);
    let mut probe = Probe::new(campaign(&[dest], test_profile()), 0);
    let ProbeAction::SendEcho {
        dest: sent,
        sequence,
    } = probe.poll(0)
    else {
        panic!("expected an echo");
    };
    assert_eq!(sent, dest);
    assert!(probe.answer_echo(sequence, dest, EchoOutcome::Reply, Some(64), 1));
    let reach = probe.reachability().get(dest).expect("probed");
    assert!(reach.live);
    assert_eq!(reach.reply_hop_limit, Some(64));
    assert!(matches!(
        probe.poll(1),
        ProbeAction::OpenConnect { target, .. } if target == dest
    ));
}

#[test]
fn a_reply_from_the_wrong_source_does_not_mark_live() {
    let dest = v4(10, 0, 0, 1);
    let mut probe = Probe::new(campaign(&[dest], test_profile()), 0);
    let ProbeAction::SendEcho { sequence, .. } = probe.poll(0) else {
        panic!("expected an echo");
    };
    assert!(probe.answer_echo(sequence, v4(10, 0, 0, 9), EchoOutcome::Reply, Some(64), 1));
    assert!(!probe.reachability().get(dest).expect("probed").live);
}

#[test]
fn an_unknown_sequence_or_token_is_not_this_campaigns() {
    let mut probe = Probe::new(campaign(&[v4(10, 0, 0, 1)], test_profile()), 0);
    assert!(!probe.answer_echo(999, v4(10, 0, 0, 1), EchoOutcome::Reply, None, 0));
    assert!(!probe.answer_connect(999, ConnectOutcome::Open, 0));
}

#[test]
fn an_unanswered_echo_expires_as_no_answer() {
    let dest = v4(10, 0, 0, 1);
    let profile = ProbeProfile {
        probe_wait_ns: 100,
        ..test_profile()
    };
    let mut probe = Probe::new(campaign(&[dest], profile), 0);
    assert!(matches!(probe.poll(0), ProbeAction::SendEcho { .. }));
    assert_eq!(probe.poll(200), ProbeAction::Done);
    assert!(!probe.reachability().get(dest).expect("probed").live);
}

#[test]
fn a_connect_verdict_records_the_port_and_liveness() {
    let dest = v4(10, 0, 0, 1);
    let mut probe = Probe::new(campaign(&[dest], test_profile()), 0);
    let ProbeAction::SendEcho { sequence, .. } = probe.poll(0) else {
        panic!("expected an echo");
    };
    probe.answer_echo(sequence, dest, EchoOutcome::Reply, None, 1);
    let mut opened = None;
    let mut refused = None;
    loop {
        match probe.poll(1) {
            ProbeAction::OpenConnect { port, token, .. } => {
                if opened.is_none() {
                    opened = Some((port, token));
                } else if refused.is_none() {
                    refused = Some((port, token));
                }
            }
            ProbeAction::Wait { .. } | ProbeAction::Done => break,
            other => panic!("unexpected {other:?}"),
        }
        if opened.is_some() && refused.is_some() {
            break;
        }
    }
    let (open_port, open_token) = opened.expect("a connect was opened");
    assert!(probe.answer_connect(open_token, ConnectOutcome::Open, 2));
    let reach = probe.reachability().get(dest).expect("probed");
    assert!(reach.open_ports.contains(&open_port));
    assert!(reach.live);
    if let Some((refused_port, refused_token)) = refused {
        assert!(probe.answer_connect(refused_token, ConnectOutcome::Refused, 2));
        assert!(!probe
            .reachability()
            .get(dest)
            .expect("probed")
            .open_ports
            .contains(&refused_port));
    }
}

#[test]
fn a_connect_past_its_deadline_is_closed() {
    let dest = v4(10, 0, 0, 1);
    let profile = ProbeProfile {
        connect_deadline_ns: 100,
        ..test_profile()
    };
    let mut probe = Probe::new(campaign(&[dest], profile), 0);
    let ProbeAction::SendEcho { sequence, .. } = probe.poll(0) else {
        panic!("expected an echo");
    };
    probe.answer_echo(sequence, dest, EchoOutcome::Reply, None, 1);
    let ProbeAction::OpenConnect { token, .. } = probe.poll(1) else {
        panic!("expected a connect");
    };
    assert_eq!(probe.poll(500), ProbeAction::CloseConnect { token });
}

#[test]
fn a_campaign_where_every_host_answers_terminates() {
    let hosts = [v4(10, 0, 0, 1), v4(10, 0, 0, 2), v4(10, 0, 0, 3)];
    let mut probe = Probe::new(campaign(&hosts, test_profile()), 0);
    let mut now = 0u64;
    let mut guard = 0;
    loop {
        guard += 1;
        assert!(guard < 10_000, "the campaign must terminate");
        assert!(probe.echoes_in_flight() <= usize::from(test_profile().echo_in_flight));
        assert!(probe.connects_in_flight() <= usize::from(test_profile().connect_in_flight));
        match probe.poll(now) {
            ProbeAction::SendEcho { dest, sequence } => {
                probe.answer_echo(sequence, dest, EchoOutcome::Reply, Some(64), now);
            }
            ProbeAction::OpenConnect { token, .. } => {
                probe.answer_connect(token, ConnectOutcome::Refused, now);
            }
            ProbeAction::CloseConnect { .. } => {}
            ProbeAction::Wait { until_ns } => now = until_ns,
            ProbeAction::Done => break,
        }
    }
    let reach = probe.into_reachability();
    assert_eq!(reach.live_count(), hosts.len());
    assert_eq!(reach.probed_count(), hosts.len());
}
