//! Deterministic fuzz harness for `lib/fsprobe` recognition (a parser of
//! untrusted on-disk bytes).
//!
//! What recognition reads comes straight off removable media outside
//! TAIRiX's trust boundary: a hostile stick or disc can carry any bytes at
//! the signature, geometry, descriptor, label and identity offsets.
//! Whatever an extent holds, recognition must either name a supported
//! format or answer `None` — never panic, read out of bounds, or fabricate a
//! match. The run aborting *is* the failure.
//!
//! A per-run-seeded `Prng` mutates a valid image of every recognised format,
//! biased towards the bytes that carry structure, serves each through every
//! device block size and with and without a disc layout, truncates them,
//! and feeds pure noise. A plain `cargo test` runs the fixed
//! [`SMOKE_ITERATIONS`] sweep; `cargo xtask fuzz` extends the loop to a
//! wall-clock budget.

#[path = "support/images.rs"]
mod images;

use images::{device, iso, udf_image};
use tairix_abi::driver::block::DiscLayout;
use tairix_fsprobe::{adfs, fingerprint, iso9660, recognise, udf, Recognised};
use tairix_fuzzseed::Prng;

/// Fixed-iteration sweep run once by a plain `cargo test` (no budget set).
const SMOKE_ITERATIONS: u64 = 4_000;

/// A seed image, its block size, and the offsets that carry structure.
struct Seed {
    bytes: Vec<u8>,
    block_size: u32,
    hot: Vec<usize>,
}

impl Seed {
    fn new(bytes: Vec<u8>, block_size: u32) -> Self {
        let hot = bytes
            .iter()
            .enumerate()
            .filter(|(_, &b)| b != 0)
            .map(|(at, _)| at)
            .collect();
        Self {
            bytes,
            block_size,
            hot,
        }
    }
}

fn corpus() -> Vec<Seed> {
    let fat_root = images::fat32_with_root(
        b"NO NAME    ",
        images::FAT32_ROOT,
        &[images::dir_entry(
            b"HOLIDAYS   ",
            tairix_fsprobe::FAT_ATTR_VOLUME_ID,
        )],
    );
    let iso_plain = iso::image(&iso::Spec {
        volume_id: b"PLAIN",
        ..Default::default()
    });
    let iso_joliet = iso::image(&iso::Spec {
        volume_id: b"JOLIET",
        joliet_id: Some("Joliet disc"),
        ..Default::default()
    });
    let iso_rr = iso::image(&iso::Spec {
        volume_id: b"ROCK",
        rock_ridge: true,
        archimedes: true,
        ..Default::default()
    });
    let hsf = iso::image(&iso::Spec {
        volume_id: b"HSF",
        high_sierra: true,
        ..Default::default()
    });
    let udf = udf_image::image(&udf_image::Spec {
        block_size: 512,
        blocks: 300,
        ..Default::default()
    });
    vec![
        Seed::new(images::fat32_boot(*b"SRLN", b"HOLIDAY PIX"), 512),
        Seed::new(fat_root, 512),
        Seed::new(images::ext4_head([7u8; 16], b"backup"), 512),
        Seed::new(images::arxfs_head([9u8; 16]), 512),
        Seed::new(images::raid_member_head([0x5A; 16]), 512),
        Seed::new(iso_plain, 2048),
        Seed::new(iso_joliet, 2048),
        Seed::new(iso_rr, 2048),
        Seed::new(hsf, 2048),
        Seed::new(udf, 512),
        Seed::new(images::adfs_old_map(b"Acorn     "), 512),
    ]
}

/// A disc layout of any shape, or none.
fn layout(rng: &mut Prng) -> Option<DiscLayout> {
    match rng.below(3) {
        0 => None,
        1 => Some(DiscLayout {
            last_session_start: rng.next_u64() % 64,
            last_recorded: None,
        }),
        _ => Some(DiscLayout {
            last_session_start: rng.next_u64(),
            last_recorded: Some(rng.next_u64()),
        }),
    }
}

/// Recognise `bytes` served as `block_size` blocks under `layout`, and
/// drain what a caller would. Must never panic.
fn exercise(bytes: Vec<u8>, block_size: u32, layout: Option<DiscLayout>) {
    let mut dev = device(bytes, block_size, layout);
    if let Ok(Some(Recognised::Volume(volume))) = recognise(&mut dev) {
        let _ = volume.label();
        let _ = fingerprint(&volume.identity);
    }
    if let Ok(Some(volume)) = iso9660::identify(&mut dev, layout) {
        let _ = (volume.label(), volume.identity(), volume.span_bytes());
        let _ = volume.mounted().root(volume.flavour);
    }
    if let Ok(Some(volume)) = udf::identify(&mut dev, layout) {
        let _ = (volume.label(), volume.identity(), volume.span_bytes());
    }
    if let Ok(Some(layout)) = adfs::identify(&mut dev) {
        let _ = (layout.name(), layout.identity());
    }
}

#[test]
fn recognising_any_extent_never_panics() {
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    let corpus = corpus();
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "recognising_any_extent_never_panics",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let block_sizes = [512u32, 1024, 2048, 4096];
    let mut iteration: u64 = 0;
    loop {
        // 1. A real image with bytes flipped, mostly where it carries
        //    structure, served through any block size and layout.
        let seed = rng.pick(&corpus);
        let mut bytes = seed.bytes.clone();
        for _ in 0..rng.at_most(24) {
            let at = if seed.hot.is_empty() || rng.below(4) == 0 {
                rng.below(bytes.len())
            } else {
                *rng.pick(&seed.hot)
            };
            bytes[at] ^= rng.next_u8();
        }
        let served = if rng.below(2) == 0 {
            seed.block_size
        } else {
            *rng.pick(&block_sizes)
        };
        exercise(bytes, served, layout(&mut rng));

        // 2. A truncation of a real image, driving the bounds checks.
        let keep = rng.at_most(seed.bytes.len());
        exercise(seed.bytes[..keep].to_vec(), 512, layout(&mut rng));

        // 3. Pure noise of an arbitrary length.
        let mut noise = vec![0u8; rng.at_most(64 * 1024 + 17)];
        rng.fill(&mut noise);
        exercise(noise, *rng.pick(&block_sizes), layout(&mut rng));

        iteration += 1;
        if !tairix_fuzzseed::within_budget(deadline) && iteration >= SMOKE_ITERATIONS {
            break;
        }
    }
}
