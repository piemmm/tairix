//! Deterministic fuzz harness for the cooling wire surface: `thermal-sensor-v1`
//! (a sensor driver's endpoint), `gpio-header-v1` (a header's lines) and
//! `thermal-v1` (the cooling service's status).
//!
//! A sensor and a header decode every request from a caller they did not
//! write, and the cooling service and the Settings application decode every
//! reply from a process they did not write. The invariants driven here:
//!
//! * decoding any byte image as a request or a reply never panics and never
//!   reads out of bounds;
//! * anything accepted re-encodes to exactly the bytes it came from, so no
//!   two frames mean the same thing and a refused field cannot be smuggled
//!   through a lenient decode.
//!
//! A plain `cargo test` runs the fixed smoke sweep; `cargo xtask fuzz`
//! extends the loop to a wall-clock budget.

use tairix_abi::driver::gpio::{
    decode_lines, decode_status as decode_header_status, encode_lines,
    encode_status as encode_header_status, HeaderLines, HeaderRequest, Holder, LineEntry,
    LineState, LineTable, Polarity, HEADER_MAX_REPLY, HEADER_REQUEST_LEN,
};
use tairix_abi::driver::thermal::{
    decode_reading, decode_request, encode_read, encode_reading, Millicelsius, SENSOR_REPLY_LEN,
    SENSOR_REQUEST_LEN,
};
use tairix_abi::thermal_ipc::{
    decode_status, encode_status, FanFault, FanLines, FanMode, FanPolicy, ThermalRequest,
    ThermalStatus, THERMAL_MAX_REPLY, THERMAL_REQUEST_LEN,
};
use tairix_abi::Errno;
use tairix_fuzzseed::Prng;

const SMOKE_ITERATIONS: u64 = 8_000;

fn scramble(frame: &mut [u8], most: usize, rng: &mut Prng) {
    if frame.is_empty() {
        return;
    }
    for _ in 0..rng.at_most(most) {
        let pos = rng.below(frame.len());
        frame[pos] ^= rng.next_u8();
    }
}

fn framed(len: usize, encode: impl FnOnce(&mut [u8]) -> Result<usize, Errno>) -> Vec<u8> {
    let mut out = vec![0u8; len];
    let used = encode(&mut out).expect("a seed fits");
    out.truncate(used);
    out
}

fn lines() -> LineTable {
    let mut table = LineTable::default();
    for entry in [
        LineEntry {
            line: 14,
            state: LineState::Peripheral { function: 0 },
        },
        LineEntry {
            line: 17,
            state: LineState::Interrupt,
        },
        LineEntry {
            line: 18,
            state: LineState::Output {
                holder: Holder::Caller,
                asserted: true,
            },
        },
        LineEntry {
            line: 22,
            state: LineState::Free,
        },
    ] {
        table.push(entry).expect("ordered");
    }
    table
}

fn sensor_requests() -> Vec<Vec<u8>> {
    vec![framed(SENSOR_REQUEST_LEN, encode_read)]
}

fn sensor_replies() -> Vec<Vec<u8>> {
    [
        Ok(Millicelsius(48_686)),
        Ok(Millicelsius(-5)),
        Err(Errno::NoData),
    ]
    .into_iter()
    .map(|outcome| framed(SENSOR_REPLY_LEN, |out| encode_reading(out, outcome)))
    .collect()
}

fn header_requests() -> Vec<Vec<u8>> {
    [
        HeaderRequest::Lines,
        HeaderRequest::Claim {
            line: 18,
            polarity: Polarity::ActiveLow,
            asserted: false,
        },
        HeaderRequest::Drive {
            line: 18,
            asserted: true,
        },
        HeaderRequest::Release { line: 27 },
    ]
    .into_iter()
    .map(|request| framed(HEADER_REQUEST_LEN, |out| request.encode(out)))
    .collect()
}

fn header_replies() -> Vec<Vec<u8>> {
    let table = lines();
    vec![
        framed(HEADER_MAX_REPLY, |out| encode_lines(out, Ok(&table))),
        framed(HEADER_MAX_REPLY, |out| encode_lines(out, Err(Errno::Busy))),
        framed(HEADER_MAX_REPLY, |out| encode_header_status(out, Ok(()))),
    ]
}

fn thermal_replies() -> Vec<Vec<u8>> {
    let policy = FanPolicy::new(FanMode::Automatic, Some(18), Polarity::ActiveHigh, 65, 10)
        .expect("in range");
    [
        ThermalStatus {
            temperature: Some(Millicelsius(66_100)),
            policy,
            running: true,
            fault: None,
            lines: FanLines::new(HeaderLines::span(2, 27), HeaderLines::span(17, 18)),
        },
        ThermalStatus {
            temperature: None,
            policy: FanPolicy::default(),
            running: false,
            fault: Some(FanFault::NoSensor),
            lines: None,
        },
    ]
    .into_iter()
    .map(|status| framed(THERMAL_MAX_REPLY, |out| encode_status(out, Ok(&status))))
    .collect()
}

fn exercise_sensor_request(bytes: &[u8]) {
    if decode_request(bytes).is_ok() {
        let mut out = [0u8; SENSOR_REQUEST_LEN];
        encode_read(&mut out).expect("fits");
        assert_eq!(&out[..], bytes, "a sensor request was normalised");
    }
}

fn exercise_sensor_reply(bytes: &[u8]) {
    if let Ok(reading) = decode_reading(bytes) {
        let mut out = [0u8; SENSOR_REPLY_LEN];
        encode_reading(&mut out, Ok(reading)).expect("fits");
        assert_eq!(&out[..], bytes, "a sensor reply was normalised");
    }
}

fn exercise_header_request(bytes: &[u8]) {
    if let Ok(request) = HeaderRequest::decode(bytes) {
        let mut out = [0u8; HEADER_REQUEST_LEN];
        request.encode(&mut out).expect("fits");
        assert_eq!(&out[..], bytes, "a header request was normalised");
    }
}

fn exercise_header_reply(bytes: &[u8]) {
    if let Ok(table) = decode_lines(bytes) {
        let mut out = [0u8; HEADER_MAX_REPLY];
        let len = encode_lines(&mut out, Ok(&table)).expect("fits");
        assert_eq!(&out[..len], bytes, "a line table was normalised");
    }
    if decode_header_status(bytes).is_ok() {
        let mut out = [0u8; HEADER_MAX_REPLY];
        let len = encode_header_status(&mut out, Ok(())).expect("fits");
        assert_eq!(&out[..len], bytes, "a header status was normalised");
    }
}

fn exercise_thermal_request(bytes: &[u8]) {
    if let Ok(request) = ThermalRequest::decode(bytes) {
        let mut out = [0u8; THERMAL_REQUEST_LEN];
        request.encode(&mut out).expect("fits");
        assert_eq!(&out[..], bytes, "a status request was normalised");
    }
}

fn exercise_thermal_reply(bytes: &[u8]) {
    if let Ok(status) = decode_status(bytes) {
        let mut out = [0u8; THERMAL_MAX_REPLY];
        let len = encode_status(&mut out, Ok(&status)).expect("fits");
        assert_eq!(&out[..len], bytes, "a status was normalised");
    }
}

fn round(rng: &mut Prng, seeds: &[Vec<u8>], longest: usize, exercise: fn(&[u8])) {
    let seed = rng.pick(seeds);
    let mut mutated = seed.clone();
    scramble(&mut mutated, 6, rng);
    exercise(&mutated);
    exercise(&seed[..rng.at_most(seed.len())]);
    let mut noise = vec![0u8; rng.at_most(longest + 8)];
    rng.fill(&mut noise);
    exercise(&noise);
}

#[test]
fn decoding_any_cooling_frame_never_panics() {
    let sensor_requests = sensor_requests();
    let sensor_replies = sensor_replies();
    let header_requests = header_requests();
    let header_replies = header_replies();
    let thermal_requests = vec![framed(THERMAL_REQUEST_LEN, |out| {
        ThermalRequest::Status.encode(out)
    })];
    let thermal_replies = thermal_replies();
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "decoding_any_cooling_frame_never_panics",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let mut iteration: u64 = 0;
    loop {
        round(
            &mut rng,
            &sensor_requests,
            SENSOR_REQUEST_LEN,
            exercise_sensor_request,
        );
        round(
            &mut rng,
            &sensor_replies,
            SENSOR_REPLY_LEN,
            exercise_sensor_reply,
        );
        round(
            &mut rng,
            &header_requests,
            HEADER_REQUEST_LEN,
            exercise_header_request,
        );
        round(
            &mut rng,
            &header_replies,
            HEADER_MAX_REPLY,
            exercise_header_reply,
        );
        round(
            &mut rng,
            &thermal_requests,
            THERMAL_REQUEST_LEN,
            exercise_thermal_request,
        );
        round(
            &mut rng,
            &thermal_replies,
            THERMAL_MAX_REPLY,
            exercise_thermal_reply,
        );
        iteration += 1;
        if !tairix_fuzzseed::within_budget(deadline) && iteration >= SMOKE_ITERATIONS {
            break;
        }
    }
}
