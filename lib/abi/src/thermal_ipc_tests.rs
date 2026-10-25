use super::{
    decode_status, encode_status, FanFault, FanLines, FanMode, FanPolicy, ThermalRequest,
    ThermalStatus, THERMAL_ENDPOINT, THERMAL_MAX_REPLY, THERMAL_REQUEST_LEN,
};
use crate::driver::gpio::{HeaderLines, Polarity};
use crate::driver::thermal::Millicelsius;
use crate::ipc::{is_reserved_endpoint, is_seat_scoped_endpoint};
use crate::Errno;

fn policy() -> FanPolicy {
    FanPolicy::new(FanMode::Automatic, Some(18), Polarity::ActiveLow, 60, 8).unwrap()
}

fn lines() -> FanLines {
    FanLines::new(HeaderLines::span(2, 27), HeaderLines::span(16, 18)).unwrap()
}

fn round_trip(status: &ThermalStatus) -> Result<ThermalStatus, Errno> {
    let mut out = [0u8; THERMAL_MAX_REPLY];
    let len = encode_status(&mut out, Ok(status)).unwrap();
    decode_status(&out[..len])
}

/// A squatter holding the rendezvous would feed every reader forged
/// temperatures and fan states.
#[test]
fn the_status_rendezvous_is_reserved_but_not_seat_scoped() {
    assert!(is_reserved_endpoint(THERMAL_ENDPOINT));
    assert!(!is_seat_scoped_endpoint(THERMAL_ENDPOINT));
}

#[test]
fn the_request_round_trips_and_a_malformed_one_is_refused() {
    let mut frame = [0u8; THERMAL_REQUEST_LEN];
    assert_eq!(
        ThermalRequest::Status.encode(&mut frame),
        Ok(THERMAL_REQUEST_LEN)
    );
    assert_eq!(ThermalRequest::decode(&frame), Ok(ThermalRequest::Status));
    assert_eq!(
        ThermalRequest::decode(&frame[..7]),
        Err(Errno::LengthOutOfRange)
    );
    for (at, value, refusal) in [
        (1, 0, Errno::BadMagic),
        (4, 9, Errno::AbiVersionUnsupported),
        (6, 2, Errno::OutOfRange),
        (7, 1, Errno::BadMagic),
    ] {
        let mut bad = frame;
        bad[at] = value;
        assert_eq!(ThermalRequest::decode(&bad), Err(refusal), "byte {at}");
    }
}

#[test]
fn a_full_status_round_trips() {
    let status = ThermalStatus {
        temperature: Some(Millicelsius(61_250)),
        policy: policy(),
        running: true,
        fault: None,
        lines: Some(lines()),
    };
    assert_eq!(round_trip(&status), Ok(status));
}

#[test]
fn a_bare_status_round_trips() {
    let status = ThermalStatus {
        temperature: None,
        policy: FanPolicy::default(),
        running: false,
        fault: Some(FanFault::NoHeader),
        lines: None,
    };
    assert_eq!(round_trip(&status), Ok(status));
}

#[test]
fn a_refusal_round_trips() {
    let mut out = [0u8; THERMAL_MAX_REPLY];
    let len = encode_status(&mut out, Err(Errno::NotFound)).unwrap();
    assert_eq!(decode_status(&out[..len]), Err(Errno::NotFound));
}

#[test]
fn a_malformed_status_is_refused_whole() {
    let status = ThermalStatus {
        temperature: Some(Millicelsius(40_000)),
        policy: policy(),
        running: false,
        fault: Some(FanFault::SensorFailed),
        lines: Some(lines()),
    };
    let mut out = [0u8; THERMAL_MAX_REPLY];
    let len = encode_status(&mut out, Ok(&status)).unwrap();
    assert_eq!(decode_status(&out[..len - 1]), Err(Errno::BadMagic));
    assert_eq!(decode_status(&out[..10]), Err(Errno::BadMagic));
    for (at, value) in [
        (4, 0x80),
        (5, 3),
        (6, 64),
        (7, 2),
        (8, 39),
        (8, 86),
        (9, 1),
        (9, 21),
        (10, 6),
        (11, 1),
    ] {
        let mut frame = out;
        frame[at] = value;
        assert_eq!(
            decode_status(&frame[..len]),
            Err(Errno::BadMagic),
            "byte {at}"
        );
    }
}

#[test]
fn the_fan_s_lines_are_those_lent_and_a_free_line_must_be_one() {
    let lines = lines();
    assert_eq!(
        lines.held().mask(),
        HeaderLines::span(2, 15).mask() | HeaderLines::span(19, 27).mask()
    );
    assert_eq!(
        FanLines::new(HeaderLines::span(2, 27), HeaderLines::span(1, 2)),
        None
    );
    let status = ThermalStatus {
        temperature: None,
        policy: policy(),
        running: false,
        fault: None,
        lines: Some(lines),
    };
    let mut out = [0u8; THERMAL_MAX_REPLY];
    let len = encode_status(&mut out, Ok(&status)).unwrap();
    assert_eq!(len, THERMAL_MAX_REPLY);
    let mut stray = out;
    // A free line, 1, that the header does not lend.
    stray[len - 8] |= 1 << 1;
    assert_eq!(decode_status(&stray[..len]), Err(Errno::BadMagic));
    assert_eq!(decode_status(&out[..len - 1]), Err(Errno::BadMagic));
}

#[test]
fn absent_fields_must_be_empty() {
    let status = ThermalStatus {
        temperature: None,
        policy: policy(),
        running: false,
        fault: None,
        lines: None,
    };
    let mut out = [0u8; THERMAL_MAX_REPLY];
    let len = encode_status(&mut out, Ok(&status)).unwrap();
    let mut reading = out;
    reading[12] = 1;
    assert_eq!(decode_status(&reading[..len]), Err(Errno::BadMagic));
    assert_eq!(decode_status(&out[..len + 4]), Err(Errno::BadMagic));
}

#[test]
fn the_policy_refuses_every_field_out_of_range() {
    let fan = |line, start, hysteresis| {
        FanPolicy::new(FanMode::On, line, Polarity::ActiveHigh, start, hysteresis)
    };
    assert!(fan(Some(63), 40, 2).is_ok());
    assert!(fan(None, 85, 20).is_ok());
    assert_eq!(fan(Some(64), 65, 10), Err(Errno::OutOfRange));
    assert_eq!(fan(None, 39, 10), Err(Errno::OutOfRange));
    assert_eq!(fan(None, 86, 10), Err(Errno::OutOfRange));
    assert_eq!(fan(None, 65, 1), Err(Errno::OutOfRange));
    assert_eq!(fan(None, 65, 21), Err(Errno::OutOfRange));
}

#[test]
fn an_automatic_fan_starts_at_its_start_and_stops_only_at_its_stop() {
    let fan = policy();
    assert_eq!(fan.stop_celsius(), 52);
    let at = |celsius| Millicelsius::from_celsius(celsius);
    assert!(!fan.runs_at(Millicelsius(59_999), false));
    assert!(fan.runs_at(at(60), false));
    assert!(
        fan.runs_at(at(55), true),
        "inside the band a running fan runs"
    );
    assert!(
        !fan.runs_at(at(55), false),
        "inside the band a stopped fan stays"
    );
    assert!(fan.runs_at(Millicelsius(52_001), true));
    assert!(!fan.runs_at(at(52), true));
}

#[test]
fn only_off_leaves_the_line_undriven() {
    assert!(!FanMode::Off.drives());
    assert!(FanMode::On.drives());
    assert!(FanMode::Automatic.drives());
}
