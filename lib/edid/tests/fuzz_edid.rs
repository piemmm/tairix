//! Deterministic fuzz harness for the sink descriptions a display driver and
//! an HD Audio driver read from devices.
//!
//! An EDID arrives over a display's DDC bus and an ELD from a codec's memory,
//! both written by whatever is plugged in. The invariants driven here:
//!
//! * reading any byte image as an EDID, a block count or an ELD never panics
//!   and never reads out of bounds;
//! * what is read holds to its bounds: at most [`MAX_MODES`] timings, each
//!   with something shown and a sync pulse on both axes, at most
//!   [`MAX_AUDIO_DESCRIPTORS`] descriptors each of one to eight channels, and
//!   a block count of one to [`MAX_BLOCKS`];
//! * reading is a function of the bytes: the same image reads the same.
//!
//! Half the mutated images have their checksums repaired, so the mutations
//! reach the decoding behind each block's checksum rather than stopping at
//! it. A plain `cargo test` runs the fixed smoke sweep; `cargo xtask fuzz`
//! extends the loop to a wall-clock budget.

use tairix_edid::synth::{self, checksum, dtd_1080p, dtd_720p, Base, Cta, BLOCK};
use tairix_edid::{block_count, Edid, Eld, MAX_AUDIO_DESCRIPTORS, MAX_BLOCKS, MAX_MODES};
use tairix_fuzzseed::Prng;

const SMOKE_ITERATIONS: u64 = 6_000;

fn scramble(image: &mut [u8], most: usize, rng: &mut Prng) {
    if image.is_empty() {
        return;
    }
    for _ in 0..rng.at_most(most) {
        let pos = rng.below(image.len());
        image[pos] ^= rng.next_u8();
    }
}

fn repair(image: &mut [u8]) {
    let (blocks, _) = image.as_chunks_mut::<BLOCK>();
    for block in blocks {
        checksum(block);
    }
}

fn edid_seeds() -> Vec<Vec<u8>> {
    let hdmi = [
        0x03, 0x0C, 0x00, 0x10, 0x00, 0x00, 60, 0xE0, 1, 2, 3, 4, 0x80, 0x40, 1, 2,
    ];
    let forum = [0xD8, 0x5D, 0xC4, 1, 120, 0xC8, 0];
    let television = Cta::new()
        .flags(0x40)
        .block(2, &[16 | 0x80, 4, 97, 1, 2, 3])
        .block(1, &[0x09, 0x07, 0x07, 0x15, 0x07, 0x50])
        .block(4, &[0x0F, 0, 0])
        .block(3, &hdmi)
        .block(3, &forum)
        .block(7, &[0x00, 0x40])
        .block(7, &[0x79, 0, 0, 1, 120, 0xC8, 0])
        .detailed(dtd_720p())
        .build();
    let overriding = Cta::new().block(7, &[0x78, 2]).block(2, &[16]).build();
    let monitor = Base::new()
        .descriptor(0, dtd_1080p())
        .display(1, 0xFC, b"TAIRIX TEST\n ")
        .display(2, 0xFD, &[50, 75, 30, 83, 17])
        .display(3, 0xF7, &[0x0A, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xF0])
        .established([0xFF, 0xFF, 0x80])
        .standard(&[[129, 0x80], [140, 0xC0], [209, 0x40]]);
    let timing = synth::displayid_timing(24_150, [2560, 48, 32, 80], [1440, 3, 5, 33], 0x80);
    vec![
        monitor.build().to_vec(),
        [
            Base::new().descriptor(0, dtd_1080p()).extensions(2).build(),
            television,
            synth::displayid(&[(0x03, &timing), (0x22, &timing)]),
        ]
        .concat(),
        [
            Base::new().extensions(1).build(),
            overriding,
            Cta::new().block(2, &[4]).build(),
        ]
        .concat(),
    ]
}

fn eld_seeds() -> Vec<Vec<u8>> {
    vec![
        synth::eld(
            0,
            b"MONITOR",
            &[[0x09, 0x07, 0x07], [0x15, 0x07, 0x50]],
            0x0F,
        ),
        synth::eld(1, b"", &[], 0),
    ]
}

fn exercise_edid(image: &[u8]) {
    if let Ok(count) = block_count(image) {
        assert!((1..=MAX_BLOCKS).contains(&count));
    }
    let Ok(edid) = Edid::parse(image) else {
        return;
    };
    assert!(edid.modes.len() <= MAX_MODES);
    for mode in &edid.modes {
        let timing = mode.timing;
        assert!(timing.horizontal.active > 0 && timing.vertical.active > 0);
        assert!(timing.horizontal.sync > 0 && timing.vertical.sync > 0);
        let _ = timing.refresh_mhz();
    }
    assert!(edid.audio.descriptors.len() <= MAX_AUDIO_DESCRIPTORS);
    for descriptor in &edid.audio.descriptors {
        assert!((1..=8).contains(&descriptor.channels));
    }
    assert_eq!(
        Edid::parse(image),
        Ok(edid),
        "reading is a function of the bytes"
    );
}

fn exercise_eld(image: &[u8]) {
    let Ok(eld) = Eld::parse(image) else {
        return;
    };
    assert!(eld.audio.descriptors.len() <= 15);
    assert_eq!(Eld::parse(image), Ok(eld));
}

#[test]
fn reading_any_sink_description_never_panics() {
    let edids = edid_seeds();
    let elds = eld_seeds();
    for seed in &edids {
        assert!(Edid::parse(seed).is_ok(), "every seed reads");
    }
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "reading_any_sink_description_never_panics",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let mut iteration: u64 = 0;
    loop {
        let seed = rng.pick(&edids);
        let mut mutated = seed.clone();
        scramble(&mut mutated, 12, &mut rng);
        if rng.below(2) == 0 {
            repair(&mut mutated);
        }
        exercise_edid(&mutated);
        exercise_edid(&seed[..rng.at_most(seed.len())]);
        let mut noise = vec![0u8; BLOCK * (1 + rng.below(3))];
        rng.fill(&mut noise);
        noise[..8].copy_from_slice(&[0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00]);
        noise[18] = 1;
        repair(&mut noise);
        exercise_edid(&noise);

        let seed = rng.pick(&elds);
        let mut mutated = seed.clone();
        scramble(&mut mutated, 6, &mut rng);
        exercise_eld(&mutated);
        exercise_eld(&seed[..rng.at_most(seed.len())]);
        let mut noise = vec![0u8; rng.at_most(64)];
        rng.fill(&mut noise);
        exercise_eld(&noise);

        iteration += 1;
        if !tairix_fuzzseed::within_budget(deadline) && iteration >= SMOKE_ITERATIONS {
            break;
        }
    }
}
