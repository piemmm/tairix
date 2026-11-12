//! Deterministic fuzz harness for the ImageDisk parser, which reads archives
//! a user hands the machine. For any bytes:
//!
//! 1. [`Image::parse`] never panics and refuses what it cannot represent.
//! 2. An archive it accepts answers every track and sector query, reading
//!    or failing as a drive would, without panicking.
//!
//! Runs the fixed smoke sweep under plain `cargo test`; keeps drawing from
//! the same seeded stream until `TAIRIX_FUZZ_BUDGET_SECS` elapses under
//! `cargo xtask fuzz`.

use tairix_fuzzseed::Prng;
use tairix_pcemu::storage::imd::Image;

/// Fixed-iteration sweep run once by a plain `cargo test` (no budget set).
const SMOKE_ITERATIONS: u64 = 400;

/// A byte drawn from 0 to `most`.
fn pick(rng: &mut Prng, most: u8) -> u8 {
    u8::try_from(rng.at_most(usize::from(most))).unwrap()
}

/// A well-formed archive of a few random tracks, the seed mutations start
/// from.
fn pristine(rng: &mut Prng) -> Vec<u8> {
    let mut out = b"IMD 1.18: fuzz\x1A".to_vec();
    for cylinder in 0..=pick(rng, 3) {
        let count = 1 + pick(rng, 9);
        let size = pick(rng, 3);
        out.extend([pick(rng, 5), cylinder, pick(rng, 1), count, size]);
        out.extend(1..=count);
        for _ in 0..count {
            let kind = pick(rng, 8);
            out.push(kind);
            match kind {
                0 => {}
                k if k % 2 == 0 => out.push(rng.next_u8()),
                _ => out.extend(rng.noise(128 << size)),
            }
        }
    }
    out
}

fn exercise(bytes: &[u8]) {
    let Ok(image) = Image::parse(bytes) else {
        return;
    };
    let mut out = [0u8; 8192];
    for cylinder in 0..=image.cylinders() {
        for head in 0..2 {
            let Some(track) = image.track(cylinder, head) else {
                continue;
            };
            for slot in 0..=track.sectors().len() {
                let _ = image.read(cylinder, head, slot, &mut out);
            }
        }
    }
}

#[test]
fn hostile_archives_never_panic() {
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "hostile_archives_never_panic",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    loop {
        for _ in 0..SMOKE_ITERATIONS {
            let base = pristine(&mut rng);
            exercise(&base);
            exercise(&rng.mutated(&base));
            let mut noise = b"IMD \x1A".to_vec();
            let len = rng.at_most(600);
            noise.extend(rng.noise(len));
            exercise(&noise);
        }
        if !tairix_fuzzseed::within_budget(deadline) {
            break;
        }
    }
}
