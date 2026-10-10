//! Deterministic fuzz harness for the `codec-v1` wire surface.
//!
//! A codec's driver decodes every request from the digital audio interface's
//! driver, a process it did not write, and that driver decodes every reply.
//! The invariants driven here:
//!
//! * decoding any byte image as a request or a reply never panics and never
//!   reads out of bounds;
//! * anything accepted re-encodes to exactly the bytes it came from, so a
//!   codec asking the kernel whether its caller holds a quoted link asks
//!   about precisely the record the caller sent;
//! * an accepted request names a codec link and no other role's.
//!
//! A plain `cargo test` runs the fixed smoke sweep; `cargo xtask fuzz`
//! extends the loop to a wall-clock budget.

use tairix_abi::driver::audio::{GainRange, Rate, RateSet, RateSupport};
use tairix_abi::driver::codec::{
    decode_describe_reply, decode_done_reply, decode_gain_reply, encode_describe_reply,
    encode_done_reply, encode_error_reply, encode_gain_reply, ClockInversion, CodecFacts,
    CodecRequest, DaiFormat, DaiFormats, DaiLink, SampleWidths, CODEC_ENDPOINTS, CODEC_MAX_REPLY,
    CODEC_MAX_REQUEST,
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

fn link() -> LinkRequest {
    let cells = DaiLink {
        format: DaiFormat::I2s,
        codec_drives_bit_clock: false,
        codec_drives_frame_clock: false,
        inversion: ClockInversion::Normal,
        cpu_dai: 0,
        codec_dai: 0,
    }
    .to_cells();
    LinkRequest::new(CODEC_ENDPOINTS.endpoint(40), 0, &cells, b"").expect("a valid seed link")
}

fn rate(hz: u32) -> Rate {
    Rate::new(hz).expect("a rate")
}

fn request_seeds() -> Vec<Vec<u8>> {
    [
        CodecRequest::Describe(link()),
        CodecRequest::Configure {
            link: link(),
            rate: rate(48_000),
            width: 32,
        },
        CodecRequest::Gain {
            link: link(),
            millibel: -650,
            mute: false,
        },
        CodecRequest::Start(link()),
        CodecRequest::Stop(link()),
    ]
    .iter()
    .map(|request| {
        let mut out = vec![0u8; CODEC_MAX_REQUEST];
        let len = request.encode(&mut out).expect("the seed fits");
        out.truncate(len);
        out
    })
    .collect()
}

fn facts(gain: Option<GainRange>) -> CodecFacts {
    CodecFacts {
        rates: RateSupport::Discrete(RateSet::new(&[rate(44_100), rate(48_000)]).expect("rates")),
        widths: SampleWidths::EMPTY.with(16).expect("a width"),
        formats: DaiFormats::EMPTY.with(DaiFormat::I2s),
        drives_clocks: false,
        gain,
    }
}

fn reply_seeds() -> Vec<Vec<u8>> {
    let mut seeds = Vec::new();
    let mut push = |encode: &dyn Fn(&mut [u8]) -> Result<usize, Errno>| {
        let mut out = vec![0u8; CODEC_MAX_REPLY];
        let len = encode(&mut out).expect("the seed fits");
        out.truncate(len);
        seeds.push(out);
    };
    push(&|out| encode_describe_reply(out, &facts(None)));
    push(&|out| {
        encode_describe_reply(
            out,
            &facts(Some(GainRange::new(-10_300, 2_400, 50).expect("range"))),
        )
    });
    push(&|out| encode_gain_reply(out, -1_250));
    push(&encode_done_reply);
    push(&|out| encode_error_reply(out, Errno::NotSupported));
    seeds
}

fn exercise_request(bytes: &[u8]) {
    let Ok(request) = CodecRequest::decode(bytes) else {
        return;
    };
    assert_eq!(
        request.link().role(),
        LinkRole::Codec,
        "a link of another role"
    );
    let mut out = vec![0u8; CODEC_MAX_REQUEST];
    let len = request
        .encode(&mut out)
        .expect("an accepted request re-encodes");
    assert_eq!(&out[..len], bytes, "a request was normalised on decode");
}

fn exercise_replies(bytes: &[u8]) {
    let mut out = vec![0u8; CODEC_MAX_REPLY];
    if let Ok(facts) = decode_describe_reply(bytes) {
        let len = encode_describe_reply(&mut out, &facts).expect("re-encodes");
        assert_eq!(&out[..len], bytes, "a facts reply was normalised on decode");
    }
    if let Ok(millibel) = decode_gain_reply(bytes) {
        let len = encode_gain_reply(&mut out, millibel).expect("re-encodes");
        assert_eq!(&out[..len], bytes);
    }
    if decode_done_reply(bytes).is_ok() {
        let len = encode_done_reply(&mut out).expect("re-encodes");
        assert_eq!(&out[..len], bytes);
    }
}

#[test]
fn decoding_any_codec_frame_never_panics() {
    let requests = request_seeds();
    let replies = reply_seeds();
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "decoding_any_codec_frame_never_panics",
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
        let mut noise = vec![0u8; rng.at_most(CODEC_MAX_REQUEST + 8)];
        rng.fill(&mut noise);
        exercise_request(&noise);

        let seed = rng.pick(&replies);
        let mut mutated = seed.clone();
        scramble(&mut mutated, 8, &mut rng);
        exercise_replies(&mutated);
        exercise_replies(&seed[..rng.at_most(seed.len())]);
        let mut noise = vec![0u8; rng.at_most(CODEC_MAX_REPLY + 8)];
        rng.fill(&mut noise);
        exercise_replies(&noise);

        iteration += 1;
        if !tairix_fuzzseed::within_budget(deadline) && iteration >= SMOKE_ITERATIONS {
            break;
        }
    }
}
