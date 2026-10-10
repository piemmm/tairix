//! Deterministic fuzz harness for the `clock-v1` wire surface.
//!
//! A clock controller's driver holds the one register page that sets every
//! clock on the chip, so every frame it decodes arrives from a less trusted
//! consumer, and every reply a consumer decodes from a process it did not
//! write. The invariants driven here:
//!
//! * decoding any byte image as a request or a reply never panics and never
//!   reads out of bounds;
//! * anything accepted re-encodes to exactly the bytes it came from, so a
//!   controller asking the kernel whether its caller holds a quoted clock link
//!   asks about precisely the record the caller sent;
//! * an accepted request names a clock link and no other role's.
//!
//! A plain `cargo test` runs the fixed smoke sweep; `cargo xtask fuzz`
//! extends the loop to a wall-clock budget.

use tairix_abi::driver::clock::{
    decode_describe_reply, decode_release_reply, decode_run_reply, encode_describe_reply,
    encode_error_reply, encode_release_reply, encode_run_reply, ClockRequest, ClockState,
    CLOCK_CONTROLLER_ENDPOINTS, CLOCK_MAX_REPLY, CLOCK_MAX_REQUEST,
};
use tairix_abi::hwlink::{LinkRequest, LinkRole};
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

fn link(index: u8, specifier: &[u32], name: &[u8]) -> LinkRequest {
    LinkRequest::new(
        CLOCK_CONTROLLER_ENDPOINTS.endpoint(8),
        index,
        specifier,
        name,
    )
    .expect("a valid seed link")
}

fn request_seeds() -> Vec<Vec<u8>> {
    [
        ClockRequest::Describe(link(0, &[0x1E], b"pwm")),
        ClockRequest::Run {
            link: link(1, &[0x1F], b"pcm"),
            hz: 3_072_000,
        },
        ClockRequest::Release(link(2, &[], b"")),
    ]
    .iter()
    .map(|request| {
        let mut out = vec![0u8; CLOCK_MAX_REQUEST];
        let len = request.encode(&mut out).expect("the seed fits");
        out.truncate(len);
        out
    })
    .collect()
}

fn reply_seeds() -> Vec<Vec<u8>> {
    let mut seeds = Vec::new();
    let mut push = |encode: &dyn Fn(&mut [u8]) -> Result<usize, Errno>| {
        let mut out = vec![0u8; CLOCK_MAX_REPLY];
        let len = encode(&mut out).expect("the seed fits");
        out.truncate(len);
        seeds.push(out);
    };
    push(&|out| {
        encode_describe_reply(
            out,
            ClockState {
                hz: 100_000_000,
                held_elsewhere: true,
            },
        )
    });
    push(&|out| encode_run_reply(out, 99_999_744));
    push(&|out| encode_release_reply(out));
    push(&|out| encode_error_reply(out, Errno::Busy));
    seeds
}

fn exercise_request(bytes: &[u8]) {
    let Ok(request) = ClockRequest::decode(bytes) else {
        return;
    };
    assert_eq!(
        request.link().role(),
        LinkRole::Clock,
        "a link of another role"
    );
    let mut out = vec![0u8; CLOCK_MAX_REQUEST];
    let len = request
        .encode(&mut out)
        .expect("an accepted request re-encodes");
    assert_eq!(&out[..len], bytes, "a request was normalised on decode");
}

fn exercise_replies(bytes: &[u8]) {
    let mut out = vec![0u8; CLOCK_MAX_REPLY];
    if let Ok(state) = decode_describe_reply(bytes) {
        let len = encode_describe_reply(&mut out, state).expect("re-encodes");
        assert_eq!(&out[..len], bytes);
    }
    if let Ok(hz) = decode_run_reply(bytes) {
        let len = encode_run_reply(&mut out, hz).expect("re-encodes");
        assert_eq!(&out[..len], bytes);
    }
    if decode_release_reply(bytes).is_ok() {
        let len = encode_release_reply(&mut out).expect("re-encodes");
        assert_eq!(&out[..len], bytes);
    }
}

#[test]
fn decoding_any_clock_frame_never_panics() {
    let requests = request_seeds();
    let replies = reply_seeds();
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "decoding_any_clock_frame_never_panics",
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
        let mut noise = vec![0u8; rng.at_most(CLOCK_MAX_REQUEST + 8)];
        rng.fill(&mut noise);
        exercise_request(&noise);

        let seed = rng.pick(&replies);
        let mut mutated = seed.clone();
        scramble(&mut mutated, 8, &mut rng);
        exercise_replies(&mutated);
        exercise_replies(&seed[..rng.at_most(seed.len())]);
        let mut noise = vec![0u8; rng.at_most(CLOCK_MAX_REPLY + 8)];
        rng.fill(&mut noise);
        exercise_replies(&noise);

        iteration += 1;
        if !tairix_fuzzseed::within_budget(deadline) && iteration >= SMOKE_ITERATIONS {
            break;
        }
    }
}
