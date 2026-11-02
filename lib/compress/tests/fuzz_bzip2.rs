//! Deterministic fuzz harness for the bzip2 codec.
//!
//! bzip2 reaches the system inside archives a stranger made, so its decoder
//! is an untrusted-input parser. Its invariants:
//!
//! * the decoder never panics, for any input — arbitrary bytes, or a real
//!   stream corrupted, cut short or resumed at any bit;
//! * the codec round-trips at every level, whatever pieces the input and
//!   output arrive in; and
//! * a decoder resumed at a block the encoder recorded decodes the rest of
//!   the stream exactly.
//!
//! A plain `cargo test` runs [`SMOKE_ITERATIONS`] from a fresh, logged seed;
//! `TAIRIX_FUZZ_BUDGET_SECS` extends the loop to a wall-clock budget.

use tairix_compress::bzip2::{
    encoder_block, encoder_work, BlockIndex, Decoder, Encoder, Next, MAX_BLOCK,
};
use tairix_compress::{Input, Output};
use tairix_fuzzseed::Prng;

const SMOKE_ITERATIONS: u64 = 48;

/// Largest plaintext a round trip draws: past one block at level 1.
const MAX_INPUT: usize = 160_000;

/// Largest output a decode of arbitrary bytes may make.
const MAX_OUTPUT: usize = 1 << 21;

struct Bytes<'a> {
    data: &'a [u8],
    at: usize,
}

impl Input for Bytes<'_> {
    fn byte(&mut self) -> Option<u8> {
        let byte = *self.data.get(self.at)?;
        self.at += 1;
        Some(byte)
    }
}

struct Gathered(Vec<u8>);

impl Output for Gathered {
    fn put(&mut self, bytes: &[u8]) -> bool {
        self.0.extend_from_slice(bytes);
        true
    }
}

#[derive(Default)]
struct Starts(Vec<(u64, u64)>);

impl BlockIndex for Starts {
    fn block(&mut self, plain: u64, bit: u64) {
        self.0.push((plain, bit));
    }
}

/// Decode `stream` from its start, in output pieces of at most `piece`,
/// bounded by [`MAX_OUTPUT`].
fn decode(stream: &[u8], piece: usize) -> Option<Vec<u8>> {
    let mut decoder = Box::new(Decoder::new(vec![0u32; MAX_BLOCK]));
    let mut input = Bytes {
        data: stream,
        at: 0,
    };
    let mut out = Vec::new();
    let mut buf = vec![0u8; piece.max(1)];
    loop {
        if input.at >= stream.len() {
            return Some(out);
        }
        decoder.begin(&mut input).ok()?;
        loop {
            match decoder.next_block(&mut input).ok()? {
                Next::End => break,
                Next::Block { .. } => loop {
                    let got = decoder.read(&mut buf).ok()?;
                    if got == 0 {
                        break;
                    }
                    out.extend_from_slice(&buf[..got]);
                    if out.len() > MAX_OUTPUT {
                        return None;
                    }
                },
            }
        }
    }
}

fn encode(plain: &[u8], level: u8, rng: &mut Prng) -> (Vec<u8>, Starts) {
    let mut encoder = Box::new(
        Encoder::new(
            vec![0u32; encoder_work(level)],
            vec![0u8; encoder_block(level)],
            level,
        )
        .expect("an encoder"),
    );
    let mut out = Gathered(Vec::new());
    let mut starts = Starts::default();
    let mut rest = plain;
    while !rest.is_empty() {
        let take = (1 + rng.below(9000)).min(rest.len());
        encoder
            .write(&rest[..take], &mut out, &mut starts)
            .expect("written");
        rest = &rest[take..];
    }
    encoder.finish(&mut out, &mut starts).expect("finished");
    (out.0, starts)
}

/// Text with runs and repeats, which every stage of the format reaches.
fn plaintext(rng: &mut Prng) -> Vec<u8> {
    let len = rng.at_most(MAX_INPUT);
    let mut out = Vec::with_capacity(len);
    while out.len() < len {
        match rng.below(3) {
            0 => out.extend(std::iter::repeat_n(rng.next_u8(), 1 + rng.below(300))),
            1 => out.extend((0..=rng.below(64)).map(|_| b"abcdefgh"[rng.below(8)])),
            _ => out.push(rng.next_u8()),
        }
    }
    out.truncate(len);
    out
}

#[test]
fn fuzz_bzip2() {
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "fuzz_bzip2",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let mut iteration = 0u64;
    loop {
        let plain = plaintext(&mut rng);
        let level = 1 + u8::try_from(rng.below(9)).unwrap();
        let (stream, starts) = encode(&plain, level, &mut rng);
        assert_eq!(
            decode(&stream, 1 + rng.below(70_000)).as_deref(),
            Some(&plain[..]),
            "level {level}"
        );
        // Resumed at a recorded block, the rest of the stream decodes.
        if let Some(&(plain_at, bit)) = starts
            .0
            .get(rng.below(starts.0.len().max(1)))
            .filter(|start| start.1 > 0)
        {
            let mut decoder = Box::new(Decoder::new(vec![0u32; MAX_BLOCK]));
            let skip = u8::try_from(bit % 8).unwrap();
            decoder.resume(level, skip).expect("resumed");
            let at = usize::try_from(bit / 8).unwrap();
            let mut input = Bytes {
                data: &stream[at..],
                at: 0,
            };
            let mut tail = Vec::new();
            let mut buf = vec![0u8; 4096];
            while let Ok(Next::Block { .. }) = decoder.next_block(&mut input) {
                while let Ok(got @ 1..) = decoder.read(&mut buf) {
                    tail.extend_from_slice(&buf[..got]);
                }
            }
            assert_eq!(tail, plain[usize::try_from(plain_at).unwrap()..]);
        }
        // Corrupted, cut short, or arbitrary: refused, never a panic.
        let mut broken = stream.clone();
        if !broken.is_empty() {
            for _ in 0..=rng.below(6) {
                let at = rng.below(broken.len());
                broken[at] ^= rng.edge_biased_u8().max(1);
            }
            let _ = decode(&broken, 1 + rng.below(4096));
            let _ = decode(&stream[..rng.below(stream.len())], 4096);
        }
        let mut noise = vec![0u8; rng.at_most(4096)];
        rng.fill(&mut noise);
        noise.splice(0..0, *b"BZh9");
        let _ = decode(&noise, 4096);
        iteration += 1;
        if !tairix_fuzzseed::within_budget(deadline) && iteration >= SMOKE_ITERATIONS {
            break;
        }
    }
}
