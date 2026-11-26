//! Deterministic fuzz harness for Squash streams.
//!
//! A Squash stream reaches the system as a stranger's file, and a stream's
//! whole state lives in a workspace a guest program owns and may scribble
//! over between calls, so both are untrusted. The invariants:
//!
//! * no call panics or fails to return, for any input, any split of it into
//!   calls, and any workspace — one a coder left and then corrupted, or
//!   bytes no coder wrote — none takes or makes more than it is given room
//!   for, and each place it tells lies, in order, in what it took and made;
//! * both compressors' streams decompress back to their input, however the
//!   calls split the input and the output; and
//! * a decompression tells the places its compression told, and a stream
//!   read from any one of them reads the rest of the input.
//!
//! A plain `cargo test` runs [`SMOKE_ITERATIONS`] from a fresh, logged seed;
//! `TAIRIX_FUZZ_BUDGET_SECS` extends the loop to a wall-clock budget.

use tairix_compress::squash::{
    compress, decompress, start_compression, start_decompression, start_decompression_at_place,
    Clearing, Place, Progress, Status, COMPRESS_WORK, DECOMPRESS_WORK,
};
use tairix_fuzzseed::Prng;

const SMOKE_ITERATIONS: u64 = 300;

const MAX_INPUT: usize = 120_000;

/// A call's progress held to what it was given, and the places it `told`,
/// in order, to what it did: a compressor's in the input it took, a
/// decompressor's in the input it took and the output it made.
fn checked(
    progress: Progress,
    told: &[Place],
    given: usize,
    room: usize,
    compressing: bool,
) -> Progress {
    assert!(progress.consumed <= given, "takes no more than given");
    assert!(progress.produced <= room, "makes no more than room for");
    for place in told {
        if compressing {
            assert!(place.plain < progress.consumed);
        } else {
            assert!(place.plain <= progress.produced && place.packed <= progress.consumed);
        }
    }
    assert!(
        told.windows(2)
            .all(|pair| pair[0].plain <= pair[1].plain && pair[0].packed < pair[1].packed),
        "places told in order"
    );
    progress
}

/// One call's places, counted from the stream's start.
fn placed(told: &[Place], plain_at: usize, packed_at: usize) -> impl Iterator<Item = Place> + '_ {
    told.iter().map(move |place| Place {
        plain: plain_at + place.plain,
        packed: packed_at + place.packed,
    })
}

fn plaintext(rng: &mut Prng) -> Vec<u8> {
    let len = rng.at_most(MAX_INPUT);
    let mut out = Vec::with_capacity(len);
    while out.len() < len {
        match rng.below(3) {
            0 => out.extend(std::iter::repeat_n(rng.next_u8(), 1 + rng.below(300))),
            1 => out.extend((0..=rng.below(48)).map(|_| b"squash risc"[rng.below(11)])),
            _ => out.push(rng.next_u8()),
        }
    }
    out.truncate(len);
    out
}

/// `plain` compressed in calls of random pieces into random rooms, with the
/// places its table was cleared.
fn compressed(rng: &mut Prng, plain: &[u8], clearing: Clearing) -> (Vec<u8>, Vec<Place>) {
    let mut work = vec![0u8; COMPRESS_WORK];
    start_compression(&mut work, clearing).expect("a workspace");
    let (mut taken, mut stream, mut places) = (0, Vec::new(), Vec::new());
    loop {
        let give = rng.at_most(4000).min(plain.len() - taken);
        let more = taken + give < plain.len();
        let mut out = vec![0u8; rng.at_most(2000)];
        let mut told = Vec::new();
        let progress = compress(
            &mut work,
            &plain[taken..taken + give],
            more,
            &mut out,
            &mut |place| {
                told.push(place);
            },
        )
        .expect("compressed");
        let progress = checked(progress, &told, give, out.len(), true);
        places.extend(placed(&told, taken, stream.len()));
        taken += progress.consumed;
        stream.extend_from_slice(&out[..progress.produced]);
        if progress.status == Status::Done {
            return (stream, places);
        }
        // A room too small for any group only waits on more room.
        assert!(
            progress.status != Status::Output || progress.produced > 0 || out.len() < 12,
            "a group of at most twelve bytes fits twelve bytes of room"
        );
    }
}

/// `stream` decompressed in calls of random pieces into random rooms, up to
/// `most` bytes, with the places its table was cleared: `None` where it is
/// refused.
fn decompressed(
    rng: &mut Prng,
    work: &mut [u8],
    stream: &[u8],
    most: usize,
) -> Option<(Vec<u8>, Vec<Place>)> {
    let (mut taken, mut back, mut places) = (0, Vec::new(), Vec::new());
    // Each call takes or makes something, or is the last.
    for _ in 0..stream.len() + most + 8 {
        let give = rng.at_most(3000).min(stream.len() - taken);
        let more = taken + give < stream.len();
        let mut out = vec![0u8; 1 + rng.below(5000)];
        let mut told = Vec::new();
        let progress = decompress(
            work,
            &stream[taken..taken + give],
            more,
            &mut out,
            &mut |place| {
                told.push(place);
            },
        )
        .ok()?;
        let progress = checked(progress, &told, give, out.len(), false);
        places.extend(placed(&told, back.len(), taken));
        taken += progress.consumed;
        back.extend_from_slice(&out[..progress.produced]);
        if progress.status == Status::Done || back.len() >= most {
            return Some((back, places));
        }
    }
    panic!("a decompression that never ends");
}

#[test]
fn fuzz_squash() {
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "fuzz_squash",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let mut iteration = 0u64;
    loop {
        // The first input is empty, which a random length all but never is.
        let plain = if iteration == 0 {
            Vec::new()
        } else {
            plaintext(&mut rng)
        };
        let clearing = if rng.below(2) == 0 {
            Clearing::AtOnce
        } else {
            Clearing::Periodically
        };
        let (stream, places) = compressed(&mut rng, &plain, clearing);
        let mut work = vec![0u8; DECOMPRESS_WORK];
        start_decompression(&mut work).expect("a workspace");
        let (back, read_places) =
            decompressed(&mut rng, &mut work, &stream, plain.len() + 1).expect("decompressed");
        assert_eq!(back, plain, "{clearing:?}");
        assert_eq!(read_places, places, "{clearing:?}");
        if !places.is_empty() {
            let place = places[rng.below(places.len())];
            start_decompression_at_place(&mut work).expect("a workspace");
            let (rest, _) = decompressed(&mut rng, &mut work, &stream[place.packed..], plain.len())
                .expect("decompressed from a place");
            assert_eq!(rest, plain[place.plain..], "{clearing:?} {place:?}");
        }

        // A stream corrupted, cut short, or noise, decompressed.
        let mut broken = stream.clone();
        if broken.len() > 3 {
            for _ in 0..=rng.below(6) {
                let at = 3 + rng.below(broken.len() - 3);
                broken[at] ^= rng.edge_biased_u8().max(1);
            }
        }
        let mut noise = vec![0u8; 3 + rng.at_most(4096)];
        rng.fill(&mut noise);
        noise[..3].copy_from_slice(&[0x1F, 0x9D, 0x8C]);
        let cut = rng.below(stream.len().max(1));
        for input in [&broken[..], &stream[..cut], &noise[..]] {
            start_decompression(&mut work).expect("a workspace");
            let _ = decompressed(&mut rng, &mut work, input, 1 << 18);
        }

        scribbled(&mut rng, &mut work, &plain, &stream, clearing);

        iteration += 1;
        if !tairix_fuzzseed::within_budget(deadline) && iteration >= SMOKE_ITERATIONS {
            break;
        }
    }
}

/// Workspaces scribbled over part way through, their state or their tables,
/// continued; and ones no coder wrote.
fn scribbled(rng: &mut Prng, work: &mut [u8], plain: &[u8], stream: &[u8], clearing: Clearing) {
    start_decompression(work).expect("a workspace");
    let half = stream.len() / 2;
    let mut out = vec![0u8; 4096];
    let ignore = &mut |_: Place| {};
    let _ = decompress(work, &stream[..half], true, &mut out, ignore);
    for _ in 0..=rng.below(8) {
        let at = if rng.below(2) == 0 {
            rng.below(64)
        } else {
            rng.below(work.len())
        };
        work[at] = rng.next_u8();
    }
    // A word of the state set whole to its most, as a counter no stream
    // reaches would be, which a byte's scribble never makes.
    let at = rng.below(64 - 8);
    work[at..at + 8].fill(0xFF);
    let _ = decompressed(rng, work, &stream[half..], 1 << 18);
    let mut cwork = vec![0u8; COMPRESS_WORK];
    start_compression(&mut cwork, clearing).expect("a workspace");
    let _ = compress(
        &mut cwork,
        &plain[..plain.len() / 2],
        true,
        &mut out,
        ignore,
    );
    for _ in 0..=rng.below(8) {
        let at = if rng.below(2) == 0 {
            rng.below(160)
        } else {
            rng.below(cwork.len())
        };
        cwork[at] = rng.next_u8();
    }
    let at = rng.below(160 - 8);
    cwork[at..at + 8].fill(0xFF);
    for _ in 0..4 {
        let continued = compress(
            &mut cwork,
            &plain[plain.len() / 2..],
            false,
            &mut out,
            ignore,
        );
        if !matches!(continued, Ok(progress) if progress.status == Status::Output) {
            break;
        }
    }
    rng.fill(&mut cwork);
    let _ = compress(&mut cwork, plain, false, &mut out, ignore);
    rng.fill(work);
    let _ = decompress(work, stream, false, &mut out, ignore);
}
