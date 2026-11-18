//! Deterministic fuzz harness for the DFS format's decoders, which read the
//! catalogue of any disc image a user opens. For any catalogue bytes:
//!
//! 1. Nothing panics.
//! 2. A catalogue believed lays every file after the catalogue and inside its
//!    side, and writes back as itself.
//! 3. Any entry's sixteen bytes decode and encode back to themselves.
//! 4. Each layout places a side's bytes at distinct places of its image, each
//!    one the image holds when `side_len` says so, and an image of
//!    `image_end` bytes holds the side's bytes it was asked to.
//! 5. An address field widens to the OS address and narrows back to itself.
//!
//! Half the catalogues are well-formed ones with bytes mutated, so the deep
//! checks are reached by construction. Runs the fixed smoke sweep under plain
//! `cargo test`; keeps drawing from the same seeded stream until
//! `TAIRIX_FUZZ_BUDGET_SECS` elapses under `cargo xtask fuzz`.

use tairix_dfs::catalogue::{
    address, field, Catalogue, Entry, CATALOGUE_LEN, CATALOGUE_SECTORS, MAX_FIELD, MAX_FILES,
};
use tairix_dfs::image::{Layout, TRACK_LEN};
use tairix_fuzzseed::Prng;

const SMOKE_ITERATIONS: u64 = 4_000;

/// A well-formed catalogue of some files.
fn well_formed(rng: &mut Prng) -> [u8; CATALOGUE_LEN] {
    let any = 2 + rng.next_u16() % 1022;
    let sectors = *rng.pick(&[400u16, 800, any]);
    let mut catalogue = Catalogue::blank(sectors);
    catalogue.title = *b"FUZZED DISC ";
    catalogue.cycle = rng.next_u8();
    catalogue.boot = rng.next_u8() & 3;
    let mut files = [Entry::NONE; MAX_FILES as usize];
    let count = rng.at_most(usize::from(MAX_FILES));
    let mut next = CATALOGUE_SECTORS;
    let mut kept = 0;
    for _ in 0..count {
        let length = rng.next_u32() % 0x4000;
        let entry = Entry {
            name: *b"FILE   ",
            dir: *rng.pick(b"$ABZ!"),
            locked: rng.below(2) == 0,
            load: rng.next_u32() & 0x3_FFFF,
            exec: rng.next_u32() & 0x3_FFFF,
            length,
            start: next,
        };
        if entry.end() > u32::from(sectors) {
            break;
        }
        next = u16::try_from(entry.end()).unwrap();
        files[kept] = entry;
        kept += 1;
    }
    catalogue.set_files(&files[..kept]).unwrap();
    let mut bytes = [0; CATALOGUE_LEN];
    catalogue.write(&mut bytes);
    bytes
}

fn check_catalogue(bytes: &[u8; CATALOGUE_LEN]) -> bool {
    let Some(catalogue) = Catalogue::read(bytes) else {
        return false;
    };
    assert!(catalogue.sectors >= CATALOGUE_SECTORS);
    for entry in catalogue.files() {
        assert!(entry.start >= CATALOGUE_SECTORS, "{entry:?}");
        assert!(entry.end() <= u32::from(catalogue.sectors), "{entry:?}");
    }
    let mut written = [0; CATALOGUE_LEN];
    catalogue.write(&mut written);
    assert_eq!(Catalogue::read(&written).as_ref(), Some(&catalogue));
    true
}

fn check_entry(rng: &mut Prng) {
    let names: [u8; 8] = rng.noise(8).try_into().unwrap();
    let info: [u8; 8] = rng.noise(8).try_into().unwrap();
    let entry = Entry::decode(&names, &info);
    assert_eq!(entry.encode(), (names, info));
    for value in [entry.load, entry.exec] {
        assert_eq!(field(address(value)), Some(value));
    }
    let any = rng.next_u32();
    if let Some(value) = field(any) {
        assert!(value <= MAX_FIELD);
        assert_eq!(address(value), any);
    }
}

fn check_layout(rng: &mut Prng) {
    let layout = *rng.pick(&[Layout::Single, Layout::Double]);
    let image_len = rng.at_most(4 * TRACK_LEN * 2);
    let side = rng.below(layout.sides());
    let held = layout.side_len(side, image_len);
    let a = rng.at_most(held.saturating_sub(1));
    let b = rng.at_most(held.saturating_sub(1));
    if held > 0 {
        assert!(layout.image_offset(side, a) < image_len);
        assert!(layout.image_offset(side, held - 1) < image_len);
        assert!(layout.image_end(side, a + 1) <= layout.image_end(side, held));
    }
    let len = rng.at_most(4 * TRACK_LEN);
    assert!(layout.side_len(side, layout.image_end(side, len)) >= len);
    if a != b {
        assert_ne!(layout.image_offset(side, a), layout.image_offset(side, b));
    }
    let other = (side + 1) % layout.sides();
    if other != side {
        assert_ne!(layout.image_offset(side, a), layout.image_offset(other, a));
    }
}

#[test]
fn hostile_catalogues_never_panic_or_lay_a_file_outside_their_side() {
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "hostile_catalogues_never_panic_or_lay_a_file_outside_their_side",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    let mut believed = 0u64;
    let mut runs = 0u64;
    loop {
        for _ in 0..SMOKE_ITERATIONS {
            let pristine = well_formed(&mut rng);
            let bytes = if rng.below(2) == 0 {
                let mut bytes = rng.mutated(&pristine);
                bytes.resize(CATALOGUE_LEN, 0);
                bytes[..CATALOGUE_LEN].try_into().unwrap()
            } else {
                pristine
            };
            if check_catalogue(&bytes) {
                believed += 1;
            }
            check_entry(&mut rng);
            check_layout(&mut rng);
            runs += 1;
        }
        if !tairix_fuzzseed::within_budget(deadline) {
            break;
        }
    }
    assert!(
        believed * 4 >= runs,
        "the well-formed half is believed: {believed} of {runs}"
    );
}
