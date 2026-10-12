//! Deterministic fuzz harness for the `encoder-v1` wire surface.
//!
//! An encoder's driver decodes every request from the display engine, and the
//! engine every reply from the encoder's driver: neither trusts the other's
//! bytes. The invariants driven here:
//!
//! * decoding any byte image as a request or a reply never panics and never
//!   reads out of bounds;
//! * anything accepted re-encodes to exactly the bytes it came from, so an
//!   encoder asking the kernel whether its caller holds a quoted link asks
//!   about precisely the record the caller sent;
//! * an accepted request names an encoder link, and an accepted state carries
//!   a mode only with a sink connected.
//!
//! A plain `cargo test` runs the fixed smoke sweep; `cargo xtask fuzz`
//! extends the loop to a wall-clock budget.

use tairix_abi::driver::display::{Axis, Polarity, Scan, Timing};
use tairix_abi::driver::encoder::{
    decode_done_reply, decode_state_reply, encode_done_reply, encode_error_reply,
    encode_state_reply, Connector, EncoderRequest, EncoderState, Phase, PipeLimits,
    ENCODER_ENDPOINTS, ENCODER_MAX_REPLY, ENCODER_MAX_REQUEST,
};
use tairix_abi::hwlink::{LinkRequest, LinkRole};
use tairix_abi::Errno;
use tairix_fuzzseed::Prng;

const SMOKE_ITERATIONS: u64 = 8_000;

const MODE: Timing = Timing {
    clock_khz: 594_000,
    horizontal: Axis {
        active: 3840,
        front: 176,
        sync: 88,
        back: 296,
    },
    vertical: Axis {
        active: 2160,
        front: 8,
        sync: 10,
        back: 72,
    },
    hsync: Polarity::Positive,
    vsync: Polarity::Positive,
    scan: Scan::Progressive,
    repetition: 1,
};

fn scramble(frame: &mut [u8], most: usize, rng: &mut Prng) {
    if frame.is_empty() {
        return;
    }
    for _ in 0..rng.at_most(most) {
        let pos = rng.below(frame.len());
        frame[pos] ^= rng.next_u8();
    }
}

fn request_seeds() -> Vec<Vec<u8>> {
    let link = LinkRequest::new(ENCODER_ENDPOINTS.endpoint(4), 0, &[], b"hdmi1")
        .expect("a valid seed link");
    [
        EncoderRequest::Describe(link),
        EncoderRequest::Wait { link, seen: 3 },
        EncoderRequest::Configure { link, timing: MODE },
        EncoderRequest::Prepare(link),
        EncoderRequest::Enable(link),
        EncoderRequest::Disable(link),
        EncoderRequest::PowerDown(link),
        EncoderRequest::Limit {
            link,
            limits: PipeLimits {
                max_clock_khz: 340_000,
                max_active_khz: 332_000,
            },
        },
    ]
    .iter()
    .map(|request| {
        let mut out = vec![0u8; ENCODER_MAX_REQUEST];
        let len = request.encode(&mut out).expect("the seed fits");
        out.truncate(len);
        out
    })
    .collect()
}

fn reply_seeds() -> Vec<Vec<u8>> {
    let mut seeds = Vec::new();
    let mut push = |encode: &dyn Fn(&mut [u8]) -> Result<usize, Errno>| {
        let mut out = vec![0u8; ENCODER_MAX_REPLY];
        let len = encode(&mut out).expect("the seed fits");
        out.truncate(len);
        seeds.push(out);
    };
    let enabled = EncoderState {
        moves: 2,
        connector: Connector {
            connected: true,
            mode: Some(MODE),
            link: 1,
        },
        phase: Phase::Enabled,
    };
    push(&|out| encode_state_reply(out, &enabled));
    push(&|out| {
        encode_state_reply(
            out,
            &EncoderState {
                connector: Connector {
                    connected: false,
                    mode: None,
                    link: 1,
                },
                phase: Phase::Off,
                ..enabled
            },
        )
    });
    push(&|out| encode_done_reply(out));
    push(&|out| encode_error_reply(out, Errno::Stale));
    seeds
}

fn exercise_request(bytes: &[u8]) {
    let Ok(request) = EncoderRequest::decode(bytes) else {
        return;
    };
    assert_eq!(
        request.link().role(),
        LinkRole::Encoder,
        "a link of another role"
    );
    if let EncoderRequest::Limit { limits, .. } = request {
        assert!(
            limits.max_clock_khz != 0 && limits.max_active_khz != 0,
            "a pipe that carries nothing"
        );
    }
    let mut out = vec![0u8; ENCODER_MAX_REQUEST];
    let len = request
        .encode(&mut out)
        .expect("an accepted request re-encodes");
    assert_eq!(&out[..len], bytes, "a request was normalised on decode");
}

fn exercise_replies(bytes: &[u8]) {
    let mut out = vec![0u8; ENCODER_MAX_REPLY];
    if let Ok(state) = decode_state_reply(bytes) {
        assert!(
            state.connector.mode.is_none() || state.connector.connected,
            "a mode with no sink"
        );
        let len = encode_state_reply(&mut out, &state).expect("re-encodes");
        assert_eq!(&out[..len], bytes);
    }
    if decode_done_reply(bytes).is_ok() {
        let len = encode_done_reply(&mut out).expect("re-encodes");
        assert_eq!(&out[..len], bytes);
    }
}

#[test]
fn decoding_any_encoder_frame_never_panics() {
    let requests = request_seeds();
    let replies = reply_seeds();
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "decoding_any_encoder_frame_never_panics",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let mut iteration: u64 = 0;
    loop {
        let seed = rng.pick(&requests);
        let mut mutated = seed.clone();
        scramble(&mut mutated, 8, &mut rng);
        exercise_request(&mutated);
        exercise_request(&seed[..rng.at_most(seed.len())]);
        let mut longer = seed.clone();
        longer.push(rng.next_u8());
        exercise_request(&longer);
        let mut noise = vec![0u8; rng.at_most(ENCODER_MAX_REQUEST + 8)];
        rng.fill(&mut noise);
        exercise_request(&noise);

        let seed = rng.pick(&replies);
        let mut mutated = seed.clone();
        scramble(&mut mutated, 8, &mut rng);
        exercise_replies(&mutated);
        exercise_replies(&seed[..rng.at_most(seed.len())]);
        let mut noise = vec![0u8; rng.at_most(ENCODER_MAX_REPLY + 8)];
        rng.fill(&mut noise);
        exercise_replies(&noise);

        iteration += 1;
        if !tairix_fuzzseed::within_budget(deadline) && iteration >= SMOKE_ITERATIONS {
            break;
        }
    }
}
