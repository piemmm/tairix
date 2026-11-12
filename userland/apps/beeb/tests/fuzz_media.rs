//! Deterministic fuzz harness for the disc and tape image decoders.
//!
//! A disc or tape image is a stranger's file. The invariants:
//!
//! * decoding never panics, for arbitrary bytes or a damaged real image,
//!   plain or gzipped, and CSW pulses stored or zlib-compressed;
//! * a disc image decodes to one that encodes back to the same bytes, with
//!   nothing lost; and
//! * a decoded tape encodes as a UEF that decodes again, to the same tape
//!   when the tape is all framed bytes and carrier, its markers and base
//!   frequencies with it.
//!
//! Inputs are drawn from a per-run-seeded `Prng`; the smoke sweep runs on a
//! plain `cargo test` and `cargo xtask fuzz --soak` extends it to a
//! wall-clock budget.

use tairix_beeb::media::disc::{Disc, ImageFormat, TRACK_BYTES};
use tairix_beeb::media::tape::Tape;
use tairix_compress::deflate::{Deflate, Flush};
use tairix_compress::zlib;
use tairix_fuzzseed::Prng;

/// Fixed-iteration sweep run once by a plain `cargo test` (no budget set).
const SMOKE_ITERATIONS: u64 = 200;
/// Largest UEF a drawn tape is built from, in chunks.
const MAX_CHUNKS: usize = 12;
/// Base frequencies a drawn UEF moves between, in hertz, each exact as a
/// single.
const BASES: [f32; 3] = [1200.0, 1250.0, 1187.5];

fn uef_chunk(out: &mut Vec<u8>, id: u16, body: &[u8]) {
    out.extend_from_slice(&id.to_le_bytes());
    out.extend_from_slice(&u32::try_from(body.len()).expect("small").to_le_bytes());
    out.extend_from_slice(body);
}

/// A UEF of carrier, data, markers and base frequencies, at either rate.
fn framed_uef(rng: &mut Prng) -> Vec<u8> {
    let mut out = b"UEF File!\0\x0A\x00".to_vec();
    for _ in 0..=rng.at_most(MAX_CHUNKS) {
        match rng.below(6) {
            0 => uef_chunk(&mut out, 0x0110, &(1 + rng.next_u16() % 2400).to_le_bytes()),
            1 => uef_chunk(
                &mut out,
                0x0117,
                &[300u16, 1200][rng.below(2)].to_le_bytes(),
            ),
            2 => uef_chunk(&mut out, 0x0120, b"label\0"),
            3 => uef_chunk(&mut out, 0x0113, &rng.pick(&BASES).to_bits().to_le_bytes()),
            _ => {
                let len = 1 + rng.at_most(300);
                uef_chunk(&mut out, 0x0100, &rng.noise(len));
            }
        }
    }
    out
}

/// A UEF of any chunk the decoder reads, its bodies drawn at random.
fn any_uef(rng: &mut Prng) -> Vec<u8> {
    let ids = [
        0x0100, 0x0102, 0x0104, 0x0110, 0x0111, 0x0112, 0x0113, 0x0114, 0x0116, 0x0117, 0x0120,
    ];
    let mut out = b"UEF File!\0\x0A\x00".to_vec();
    for _ in 0..=rng.at_most(MAX_CHUNKS) {
        let len = rng.at_most(64);
        uef_chunk(&mut out, *rng.pick(&ids), &rng.noise(len));
    }
    out
}

/// `data` deflated into a zlib stream.
fn zlib_stream(data: &[u8]) -> Vec<u8> {
    let mut encoder = Box::new(zlib::Encoder::new());
    let mut out = vec![0; encoder.bound(data.len())];
    let len = encoder
        .compress(data, &mut out, Flush::Finish)
        .expect("a bound-sized destination fits");
    out.truncate(len);
    out
}

/// A CSW of either version, its pulses drawn at random: a version 2 image at
/// a rate the decoder takes, its pulses stored or zlib-compressed.
fn any_csw(rng: &mut Prng) -> Vec<u8> {
    let mut out = b"Compressed Square Wave\x1A".to_vec();
    let len = rng.at_most(512);
    let pulses = rng.noise(len);
    if rng.next_u64().is_multiple_of(2) {
        out.extend_from_slice(&[1, 1]);
        out.extend_from_slice(&rng.next_u16().to_le_bytes());
        out.extend_from_slice(&[1, rng.next_u8() & 1, 0, 0, 0]);
        out.extend_from_slice(&pulses);
    } else {
        let rate = 1_000 + u32::try_from(rng.at_most(999_000)).expect("small");
        let compressed = rng.next_u64().is_multiple_of(2);
        out.extend_from_slice(&[2, 0]);
        out.extend_from_slice(&rate.to_le_bytes());
        out.extend_from_slice(&rng.next_u32().to_le_bytes());
        out.extend_from_slice(&[if compressed { 2 } else { 1 }, rng.next_u8() & 1, 0]);
        out.extend_from_slice(&[0; 16]);
        if compressed {
            out.extend_from_slice(&zlib_stream(&pulses));
        } else {
            out.extend_from_slice(&pulses);
        }
    }
    out
}

/// `data` as one gzip member.
fn gzipped(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 0, 3];
    let mut deflate = Box::new(Deflate::new());
    let mut body = vec![0; deflate.bound(data.len())];
    let len = deflate
        .deflate(data, &mut body, Flush::Finish)
        .expect("a bound-sized destination fits");
    out.extend_from_slice(&body[..len]);
    out.extend_from_slice(&tairix_crc32::checksum(data).to_le_bytes());
    out.extend_from_slice(&u32::try_from(data.len()).expect("small").to_le_bytes());
    out
}

fn check_tape(bytes: &[u8]) {
    if let Ok(tape) = Tape::decode(bytes) {
        assert!(
            Tape::decode(&tape.encode_uef()).is_ok(),
            "a tape's own UEF decodes"
        );
    }
}

#[test]
fn disc_and_tape_images_never_panic_and_round_trip() {
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "disc_and_tape_images_never_panic_and_round_trip",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let mut iteration: u64 = 0;
    loop {
        let format = *rng.pick(&[ImageFormat::Ssd, ImageFormat::Dsd]);
        let len = rng.at_most(2 * 90 * TRACK_BYTES);
        let image = rng.noise(len);
        if let Ok(disc) = Disc::decode(&image, format, false) {
            let encoded = disc.encode();
            assert_eq!(encoded.image, image, "a disc saves as it came");
            assert_eq!(
                encoded.lost, None,
                "a sector dump holds all a dump decodes to"
            );
        }

        let framed = framed_uef(&mut rng);
        let tape = Tape::decode(&framed).expect("a well-formed UEF decodes");
        assert_eq!(
            Tape::decode(&tape.encode_uef()).as_ref(),
            Ok(&tape),
            "framed bytes, carrier, markers and bases round-trip exactly"
        );
        assert_eq!(Tape::decode(&gzipped(&framed)), Ok(tape), "gzipped alike");
        for bytes in [framed, any_uef(&mut rng), any_csw(&mut rng)] {
            check_tape(&bytes);
            check_tape(&rng.mutated(&bytes));
            let wrapped = gzipped(&bytes);
            check_tape(&wrapped);
            check_tape(&rng.mutated(&wrapped));
        }
        let len = rng.at_most(256);
        check_tape(&rng.noise(len));

        iteration += 1;
        match deadline {
            None if iteration >= SMOKE_ITERATIONS => break,
            Some(_) if !tairix_fuzzseed::within_budget(deadline) => break,
            _ => {}
        }
    }
}
