//! Deterministic fuzz harness for the ARC family's methods.
//!
//! ARC, Spark and `ArcFS` members reach the system inside archives a stranger
//! made, so every one of their decoders is an untrusted-input parser. Its
//! invariants:
//!
//! * no decoder panics, for any method and any input — arbitrary bytes, or a
//!   real stream corrupted or cut short — and none makes more than it is
//!   asked for; and
//! * the dynamic LZW encoder round-trips at every width, whatever pieces the
//!   input arrives in and whatever workspace bounds its dictionary.
//!
//! A plain `cargo test` runs [`SMOKE_ITERATIONS`] from a fresh, logged seed;
//! `TAIRIX_FUZZ_BUDGET_SECS` extends the loop to a wall-clock budget.

use tairix_compress::arc::{
    encoder_work, Decoder, Encoder, Hash, Method, DECODER_STACK, DECODER_TABLE, ENCODER_WORK,
    LZW_MAX_BITS, LZW_MIN_BITS,
};
use tairix_compress::{Input, Output};
use tairix_fuzzseed::Prng;

const SMOKE_ITERATIONS: u64 = 300;

const MAX_INPUT: usize = 100_000;

struct Bytes<'a>(&'a [u8]);

impl Input for Bytes<'_> {
    fn byte(&mut self) -> Option<u8> {
        let (&first, rest) = self.0.split_first()?;
        self.0 = rest;
        Some(first)
    }
}

struct Gathered(Vec<u8>);

impl Output for Gathered {
    fn put(&mut self, bytes: &[u8]) -> bool {
        self.0.extend_from_slice(bytes);
        true
    }
}

/// At most `want` bytes of `stream` decoded as `method` says, in pieces of
/// `piece`.
fn decode(stream: &[u8], method: Method, want: usize, piece: usize) -> Option<Vec<u8>> {
    let mut decoder =
        Decoder::new(vec![0u32; DECODER_TABLE], vec![0u8; DECODER_STACK], method).ok()?;
    let mut input = Bytes(stream);
    let mut out = vec![0u8; want];
    let mut made = 0;
    while made < want {
        let end = (made + piece.max(1)).min(want);
        let got = decoder.read(&mut input, &mut out[made..end]).ok()?;
        assert!(
            got <= end - made,
            "a decoder makes no more than it is asked for"
        );
        if got == 0 {
            break;
        }
        made += got;
    }
    out.truncate(made);
    Some(out)
}

fn any_method(rng: &mut Prng) -> Method {
    let bits = LZW_MIN_BITS
        + u8::try_from(rng.below(usize::from(LZW_MAX_BITS - LZW_MIN_BITS + 1))).unwrap();
    match rng.below(5) {
        0 => Method::Packed,
        1 => Method::Squeezed,
        2 => Method::Crunched {
            hash: if rng.below(2) == 0 {
                Hash::Square
            } else {
                Hash::Product
            },
            rle: rng.below(2) == 0,
        },
        _ => Method::Lzw {
            max_bits: bits,
            rle: rng.below(2) == 0,
        },
    }
}

fn plaintext(rng: &mut Prng) -> Vec<u8> {
    let len = rng.at_most(MAX_INPUT);
    let mut out = Vec::with_capacity(len);
    while out.len() < len {
        match rng.below(3) {
            0 => out.extend(std::iter::repeat_n(rng.next_u8(), 1 + rng.below(200))),
            1 => out.extend((0..=rng.below(48)).map(|_| b"spark arc"[rng.below(9)])),
            _ => out.push(rng.next_u8()),
        }
    }
    out.truncate(len);
    out
}

#[test]
fn fuzz_arc() {
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "fuzz_arc",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let mut iteration = 0u64;
    loop {
        let plain = plaintext(&mut rng);
        let bits = LZW_MIN_BITS
            + u8::try_from(rng.below(usize::from(LZW_MAX_BITS - LZW_MIN_BITS + 1))).unwrap();
        // A dictionary as large as the codes allow, or sized to some input.
        let work = if rng.below(2) == 0 {
            ENCODER_WORK
        } else {
            encoder_work(u64::try_from(rng.at_most(MAX_INPUT)).unwrap())
        };
        let mut encoder = Encoder::new(vec![0u32; work], bits).expect("an encoder");
        let mut out = Gathered(Vec::new());
        let mut rest = &plain[..];
        while !rest.is_empty() {
            let take = (1 + rng.below(5000)).min(rest.len());
            encoder.write(&rest[..take], &mut out).expect("written");
            rest = &rest[take..];
        }
        encoder.finish(&mut out).expect("finished");
        let method = Method::Lzw {
            max_bits: bits,
            rle: false,
        };
        let back = decode(&out.0, method, plain.len(), 1 + rng.below(9000));
        assert_eq!(back.as_deref(), Some(&plain[..]), "width {bits}");
        // Any method over a corrupted stream, a cut one, or noise.
        let mut broken = out.0.clone();
        if !broken.is_empty() {
            for _ in 0..=rng.below(6) {
                let at = rng.below(broken.len());
                broken[at] ^= rng.edge_biased_u8().max(1);
            }
        }
        let mut noise = vec![0u8; rng.at_most(4096)];
        rng.fill(&mut noise);
        for stream in [
            &broken[..],
            &out.0[..rng.below(out.0.len().max(1))],
            &noise[..],
        ] {
            let _ = decode(
                stream,
                any_method(&mut rng),
                1 + rng.below(1 << 18),
                1 + rng.below(4096),
            );
        }
        iteration += 1;
        if !tairix_fuzzseed::within_budget(deadline) && iteration >= SMOKE_ITERATIONS {
            break;
        }
    }
}
