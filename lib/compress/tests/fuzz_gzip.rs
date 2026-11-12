//! Deterministic fuzz harness for the RFC 1952 gzip member.
//!
//! A gzip stream is a stranger's file: an archive the archive driver opens, a
//! tape image the BBC Micro emulator reads. The invariants:
//!
//! * `decompress` never panics, for arbitrary bytes or a damaged real stream,
//!   and a sink that bounds the output always stops it;
//! * a real stream of several members round-trips exactly; and
//! * damage to a member's data or trailer is refused, never decoded into a
//!   silently wrong answer.
//!
//! Inputs are drawn from a per-run-seeded `Prng`; the smoke sweep runs on a
//! plain `cargo test` and `cargo xtask fuzz --soak` extends it to a
//! wall-clock budget.

use tairix_compress::deflate::{Deflate, Flush};
use tairix_compress::gzip::{decompress, Error, Header, Member, MAGIC};
use tairix_fuzzseed::Prng;

/// Fixed-iteration sweep run once by a plain `cargo test` (no budget set).
const SMOKE_ITERATIONS: u64 = 300;
/// Largest member a drawn stream holds.
const MAX_MEMBER: usize = 6000;
/// Members a drawn stream holds.
const MAX_MEMBERS: usize = 4;
/// Largest output a decode is allowed to produce.
const MAX_OUTPUT: usize = 1 << 17;

fn member(data: &[u8], name: Option<&[u8]>) -> Vec<u8> {
    let mut out = vec![
        MAGIC[0],
        MAGIC[1],
        8,
        if name.is_some() { 0x08 } else { 0 },
        0,
        0,
        0,
        0,
        0,
        3,
    ];
    if let Some(name) = name {
        out.extend_from_slice(name);
        out.push(0);
    }
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

fn decode(stream: &[u8]) -> Result<Vec<u8>, Error> {
    let mut member = Box::new(Member::new());
    let mut scratch = vec![0u8; 4096];
    let mut out = Vec::new();
    decompress(stream, &mut member, &mut scratch, |piece| {
        out.extend_from_slice(piece);
        out.len() <= MAX_OUTPUT
    })?;
    Ok(out)
}

fn draw(rng: &mut Prng, len: usize) -> Vec<u8> {
    let alphabet = 1 + rng.at_most(255);
    (0..len)
        .map(|_| u8::try_from(rng.at_most(alphabet)).unwrap_or(0))
        .collect()
}

#[test]
fn gzip_never_panics_and_round_trips_member_by_member() {
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "gzip_never_panics_and_round_trips_member_by_member",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let mut iteration: u64 = 0;
    loop {
        let mut stream = Vec::new();
        let mut whole = Vec::new();
        for _ in 0..=rng.at_most(MAX_MEMBERS - 1) {
            let len = rng.at_most(MAX_MEMBER);
            let data = draw(&mut rng, len);
            let name = rng.next_u64().is_multiple_of(2).then_some(&b"file.uef"[..]);
            stream.extend_from_slice(&member(&data, name));
            whole.extend_from_slice(&data);
        }
        assert_eq!(decode(&stream).expect("our own stream decodes"), whole);
        assert!(Header::parse(&stream).is_some());

        // Damage the stream: it may decode to nothing wrong.
        let mut damaged = stream.clone();
        for _ in 0..=rng.at_most(5) {
            let at = rng.at_most(damaged.len() - 1);
            damaged[at] ^= 1 << rng.at_most(7);
        }
        if let Ok(out) = decode(&damaged) {
            assert_eq!(out, whole, "a damaged stream decoded to something else");
        }

        // Arbitrary bytes, and a real stream cut short.
        let len = rng.at_most(2048);
        let noise = draw(&mut rng, len);
        let _ = decode(&noise);
        let cut = rng.at_most(stream.len());
        let _ = decode(&stream[..cut]);

        iteration += 1;
        if !tairix_fuzzseed::within_budget(deadline) && iteration >= SMOKE_ITERATIONS {
            break;
        }
    }
}
