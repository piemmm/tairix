//! Fuzz harness for the VP8 stream decoder: untrusted frame bytes.
//!
//! Invariants, for every stream:
//!
//! 1. Decoding a frame never panics, and a frame refused — malformed, or
//!    refused its memory — leaves the stream as it was: every later frame
//!    decodes exactly as it does in a twin stream that never saw it.
//! 2. A decoded frame is as large as its stream's keyframe declared, and hands
//!    over every row, refusing a row it does not have or a buffer too short.
//! 3. A decode maps no more than the forecast of the stream it decodes into.
//!
//! Inputs are arbitrary bytes, the same behind a keyframe's tag and start
//! code, and streams of the fixture's drawn keyframes and inter frames, then
//! mutated.

#![allow(
    clippy::cast_possible_truncation,
    reason = "the generators narrow sides and counts they keep small"
)]

use tairix_fuzzseed::meter::{metered, refusing_above, refusing_after, Metered};
use tairix_fuzzseed::Prng;
use tairix_video::vp8::fixture::{drawn_inter_frame, drawn_keyframe, Filter, InterFrame, Update};
use tairix_video::vp8::{stream_peak_bytes, Decoded, Decoder};

#[global_allocator]
static ALLOC: Metered = Metered;

/// Fixed-iteration sweep run when no budget is set.
const SMOKE_ITERATIONS: u64 = 40;

/// The largest side a drawn stream has.
const MAX_SIDE: u32 = 72;

/// The most frames a drawn stream holds.
const MAX_FRAMES: usize = 10;

/// The largest single request an arbitrary input's decode is granted, as the
/// sandbox's budget refuses a picture no recording on the desktop needs.
const LARGEST_GRANT: usize = 8 << 20;

/// What a decode came to, kept past the stream it borrows from.
#[derive(Debug, PartialEq)]
struct Picture {
    size: (u32, u32),
    keyframe: bool,
    shown: bool,
    rgba: Vec<u8>,
}

fn picture(decoded: &Decoded<'_>) -> Picture {
    let (width, height) = (decoded.width() as usize, decoded.height() as usize);
    let stride = width * 4;
    let mut rgba = vec![0; stride * height];
    decoded
        .rgba_rows(0..height, &mut rgba, stride)
        .expect("a decoded frame hands over every row");
    assert!(
        decoded
            .rgba_rows(0..height + 1, &mut vec![0; stride * (height + 1)], stride)
            .is_err(),
        "a row past the picture was handed over"
    );
    if height > 0 {
        assert!(
            decoded
                .rgba_rows(0..height, &mut rgba[..stride * height - 1], stride)
                .is_err(),
            "a buffer too short was filled"
        );
    }
    Picture {
        size: (decoded.width(), decoded.height()),
        keyframe: decoded.keyframe(),
        shown: decoded.shown(),
        rgba,
    }
}

/// The size a keyframe's header declares, read as the format lays it out.
fn declared(frame: &[u8]) -> Option<(u32, u32)> {
    let head = frame.get(..10)?;
    let side = |at: usize| u32::from(u16::from_le_bytes([head[at], head[at + 1]])) & 0x3FFF;
    (head[0] & 1 == 0 && head[3..6] == [0x9D, 0x01, 0x2A]).then(|| (side(6), side(8)))
}

/// Decode `frames` in a stream refused its memory at random, beside a twin
/// fed only the frames the first accepted.
fn exercise(frames: &[Vec<u8>], rng: &mut Prng) {
    let mut fuzzed = Decoder::new();
    let mut twin = Decoder::new();
    let mut size = None;
    for frame in frames {
        let into = declared(frame).or(size);
        let grants = (rng.below(3) == 0).then(|| rng.below(24));
        let decoder = &mut fuzzed;
        // Each closure moves the stream's borrow on, so the decode it answers
        // outlives them to be handed over with the heap whole again.
        let (outcome, metering) = metered(move || {
            refusing_above(LARGEST_GRANT, move || {
                let decoder = decoder;
                match grants {
                    Some(granted) => refusing_after(granted, move || {
                        let decoder = decoder;
                        decoder.decode(frame)
                    }),
                    None => decoder.decode(frame),
                }
            })
        });
        if let Some(forecast) = into.and_then(|(width, height)| stream_peak_bytes(width, height)) {
            let mapped = u64::try_from(metering.peak).unwrap_or(u64::MAX);
            assert!(
                mapped <= forecast,
                "a decode mapped {mapped} bytes into a stream forecast at {forecast}"
            );
        }
        let Ok(out) = outcome.map(|decoded| picture(&decoded)) else {
            continue;
        };
        if out.keyframe {
            size = Some(out.size);
        }
        assert_eq!(Some(out.size), size, "a frame unlike its stream's keyframe");
        let again = twin
            .decode(frame)
            .map(|decoded| picture(&decoded))
            .expect("the twin refused a frame the stream accepted");
        assert_eq!(out, again, "a refused frame left the stream changed");
    }
}

/// A drawn filter.
fn filter(rng: &mut Prng) -> Filter {
    Filter {
        simple: rng.below(2) == 0,
        level: rng.below(64) as u32,
        sharpness: rng.below(8) as u32,
    }
}

/// A drawn reference update.
fn update(rng: &mut Prng) -> Update {
    *rng.pick(&[
        Update::Kept,
        Update::Refreshed,
        Update::CopiedFromLast,
        Update::CopiedFromOther,
    ])
}

/// A drawn side of a stream's picture.
fn side(rng: &mut Prng) -> u32 {
    1 + rng.below(MAX_SIDE as usize) as u32
}

/// A stream of drawn frames: a keyframe, inter frames set at random, and now
/// and then a keyframe of another size.
fn drawn(rng: &mut Prng) -> Vec<Vec<u8>> {
    let mut frames = Vec::new();
    let (mut width, mut height) = (side(rng), side(rng));
    for at in 0..=rng.below(MAX_FRAMES) {
        let partitions = *rng.pick(&[1usize, 2, 4, 8]);
        let mut word = {
            let seed = rng.next_u64();
            let mut words = Prng::new(seed);
            move || words.next_u64()
        };
        if at == 0 || rng.below(6) == 0 {
            if at > 0 && rng.below(2) == 0 {
                (width, height) = (side(rng), side(rng));
            }
            let filter = filter(rng);
            frames.push(drawn_keyframe(width, height, filter, partitions, &mut word));
        } else {
            let frame = InterFrame {
                filter: filter(rng),
                quantiser: rng.below(128) as u32,
                golden: update(rng),
                alternate: update(rng),
                sign_bias: [rng.below(2) == 0, rng.below(2) == 0],
                refresh_last: rng.below(4) != 0,
                shown: rng.below(4) != 0,
            };
            frames.push(drawn_inter_frame(
                width, height, &frame, partitions, &mut word,
            ));
        }
    }
    frames
}

/// A keyframe's tag, start code and a small size, then `rest`.
fn behind_a_keyframe_header(rng: &mut Prng, rest: &[u8]) -> Vec<u8> {
    let first_part = rest.len() as u32;
    let tag = (first_part << 5) | (1 << 4);
    let mut frame = tag.to_le_bytes()[..3].to_vec();
    frame.extend_from_slice(&[0x9D, 0x01, 0x2A]);
    for _ in 0..2 {
        frame.extend_from_slice(&(side(rng) as u16).to_le_bytes());
    }
    frame.extend_from_slice(rest);
    frame
}

fn sweep(name: &str, mut one: impl FnMut(&mut Prng)) {
    let mut rng = Prng::new(tairix_fuzzseed::start(name, tairix_fuzzseed::FUZZ_SEED_ENV));
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    loop {
        for _ in 0..SMOKE_ITERATIONS {
            one(&mut rng);
        }
        if !tairix_fuzzseed::within_budget(deadline) {
            break;
        }
    }
}

#[test]
fn arbitrary_frames_never_panic() {
    sweep("arbitrary_frames_never_panic", |rng| {
        let frames: Vec<Vec<u8>> = (0..=rng.below(4))
            .map(|_| {
                let len = rng.below(256);
                rng.noise(len)
            })
            .collect();
        exercise(&frames, rng);
    });
}

#[test]
fn garbage_behind_a_keyframe_header_never_panics() {
    // Random bytes essentially never carry the start code, so without this
    // the walk past the frame tag would hardly ever be entered.
    sweep("garbage_behind_a_keyframe_header_never_panics", |rng| {
        let mut frames = Vec::new();
        for _ in 0..=rng.below(4) {
            let len = rng.below(512);
            let rest = rng.noise(len);
            frames.push(behind_a_keyframe_header(rng, &rest));
        }
        exercise(&frames, rng);
    });
}

#[test]
fn drawn_streams_decode_alike_however_their_memory_is_refused() {
    sweep(
        "drawn_streams_decode_alike_however_their_memory_is_refused",
        |rng| {
            let frames = drawn(rng);
            let mut whole = Decoder::new();
            for frame in &frames {
                whole.decode(frame).expect("a drawn frame decodes");
            }
            exercise(&frames, rng);
        },
    );
}

#[test]
fn mutated_drawn_streams_never_panic() {
    sweep("mutated_drawn_streams_never_panic", |rng| {
        let mut frames = drawn(rng);
        for _ in 0..=rng.below(3) {
            let at = rng.below(frames.len());
            frames[at] = rng.mutated(&frames[at]);
        }
        exercise(&frames, rng);
    });
}
