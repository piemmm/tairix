use super::*;

#[test]
fn reply_times_narrow_their_precision_as_the_round_trip_grows() {
    assert_eq!(reply_time(45_000), "0.045");
    assert_eq!(reply_time(999_999), "0.999");
    assert_eq!(reply_time(1_000_000), "1.00");
    assert_eq!(reply_time(1_234_000), "1.23");
    assert_eq!(reply_time(1_235_000), "1.24", "rounded, not truncated");
    assert_eq!(reply_time(9_994_000), "9.99");
    assert_eq!(reply_time(9_995_000), "10.0", "a tier boundary rounds up");
    assert_eq!(reply_time(12_340_000), "12.3");
    assert_eq!(reply_time(99_949_000), "99.9");
    assert_eq!(reply_time(99_950_000), "100");
    assert_eq!(reply_time(250_400_000), "250");
}

#[test]
fn the_loss_percentage_is_printed_as_iputils_prints_it() {
    assert_eq!(loss_percent(0, 3), "0");
    assert_eq!(loss_percent(3, 3), "100");
    assert_eq!(loss_percent(1, 3), "33.3333");
    assert_eq!(loss_percent(2, 3), "66.6667");
    assert_eq!(loss_percent(1, 7), "14.2857");
    assert_eq!(loss_percent(1, 2), "50");
    assert_eq!(loss_percent(1, 8), "12.5");
    assert_eq!(loss_percent(1, 1_000), "0.1");
    assert_eq!(loss_percent(1, 10_000_000), "1e-05");
}

#[test]
fn the_deviation_of_identical_round_trips_is_zero() {
    let mut rtt = RttStats::default();
    for _ in 0..4 {
        rtt.record(250_000);
    }
    assert_eq!(rtt.mdev_us(), 0);
    assert_eq!(rtt.avg_us(), 250);
}

#[test]
fn the_deviation_is_the_population_standard_deviation_in_microseconds() {
    let mut rtt = RttStats::default();
    for us in [100u64, 200, 300] {
        rtt.record(us * 1_000);
    }
    // sqrt(((100-200)^2 + 0 + (300-200)^2) / 3) = sqrt(6666.67) = 81.6
    assert_eq!(rtt.mdev_us(), 81);
}

#[test]
fn isqrt_floors() {
    assert_eq!(isqrt(0), 0);
    assert_eq!(isqrt(1), 1);
    assert_eq!(isqrt(2), 1);
    assert_eq!(isqrt(15), 3);
    assert_eq!(isqrt(16), 4);
    assert_eq!(isqrt(u128::MAX), u64::MAX);
}

#[test]
fn statistics_carry_errors_mdev_and_a_rounded_total_time() {
    let mut rtt = RttStats::default();
    rtt.record(45_000);
    let summary = RunSummary {
        transmitted: 3,
        received: 1,
        errors: 2,
    };
    let text = statistics("10.0.2.2", summary, &rtt, 2_003_500_000);
    assert_eq!(
        text,
        "\n--- 10.0.2.2 ping statistics ---\n\
         3 packets transmitted, 1 received, +2 errors, 66.6667% packet loss, time 2004ms\n\
         rtt min/avg/max/mdev = 0.045/0.045/0.045/0.000 ms\n"
    );
}

#[test]
fn statistics_without_replies_carry_no_rtt_line_and_no_errors_field() {
    let summary = RunSummary {
        transmitted: 2,
        received: 0,
        errors: 0,
    };
    let text = statistics("host", summary, &RttStats::default(), 1_000_000_000);
    assert!(text.contains("2 packets transmitted, 0 received, 100% packet loss, time 1000ms"));
    assert!(!text.contains("errors"));
    assert!(!text.contains("rtt"));
}

#[test]
fn every_error_has_its_iputils_reason_in_each_family() {
    use EchoOutcome::{HopLimitExceeded, PacketTooBig, ParameterProblem, Reply, Unreachable};
    let v4 = NetAddrFamily::V4;
    let v6 = NetAddrFamily::V6;
    assert_eq!(reason(v4, Reply), None);
    assert_eq!(
        reason(v4, HopLimitExceeded).as_deref(),
        Some("Time to live exceeded")
    );
    assert_eq!(
        reason(v6, HopLimitExceeded).as_deref(),
        Some("Time exceeded: Hop limit")
    );
    assert_eq!(
        reason(v4, Unreachable { code: 1 }).as_deref(),
        Some("Destination Host Unreachable")
    );
    assert_eq!(
        reason(v4, Unreachable { code: 99 }).as_deref(),
        Some("Dest Unreachable, Bad Code: 99")
    );
    assert_eq!(
        reason(v6, Unreachable { code: 3 }).as_deref(),
        Some("Destination unreachable: Address unreachable")
    );
    assert_eq!(
        reason(v6, Unreachable { code: 7 }).as_deref(),
        Some("Destination unreachable: Unknown code 7")
    );
    assert_eq!(
        reason(v4, PacketTooBig { mtu: 1400 }).as_deref(),
        Some("Frag needed and DF set (mtu = 1400)")
    );
    assert_eq!(
        reason(v6, PacketTooBig { mtu: 1280 }).as_deref(),
        Some("Packet too big: mtu=1280")
    );
    assert_eq!(
        reason(
            v4,
            ParameterProblem {
                code: 0,
                pointer: 9
            }
        )
        .as_deref(),
        Some("Parameter problem: pointer = 9")
    );
    assert_eq!(
        reason(
            v6,
            ParameterProblem {
                code: 1,
                pointer: 40
            }
        )
        .as_deref(),
        Some("Parameter problem: Unknown header at 40")
    );
}
