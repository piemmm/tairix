//! Deterministic fuzz harness for the FAT format's decoders, which read
//! volumes from any device a user attaches. For any boot sector, directory
//! slot and table bytes:
//!
//! 1. Nothing panics.
//! 2. A boot sector that parses lays every region and cluster inside the
//!    volume it describes, counts no more clusters than its table and kind
//!    can, and writes back as itself.
//! 3. A short entry encodes back to the bytes it decoded from, and a long
//!    name gathered from any slots decodes within its buffer.
//! 4. Every table entry set reads back, and its neighbours keep theirs.
//!
//! Half the boot sectors are well-formed volumes of every kind with fields
//! mutated, so the parse's deep checks are reached by construction. Runs the
//! fixed smoke sweep under plain `cargo test`; keeps drawing from the same
//! seeded stream until `TAIRIX_FUZZ_BUDGET_SECS` elapses under
//! `cargo xtask fuzz`.

use tairix_fat::boot::{Bpb, Kind};
use tairix_fat::dir::{self, LongName, ShortEntry, ENTRY};
use tairix_fat::format::{self, Medium, Shape};
use tairix_fat::table;
use tairix_fuzzseed::Prng;

const SMOKE_ITERATIONS: u64 = 4_000;

/// A well-formed boot sector of some kind and size.
fn well_formed(rng: &mut Prng) -> [u8; 512] {
    let (sectors, kind, spc, root) = *rng.pick(&[
        (2880, Some(Kind::Fat12), Some(1), 224),
        (720, Some(Kind::Fat12), Some(2), 112),
        (20_000, None, None, 0),
        (63_934, None, None, 0),
        (1_000_000, Some(Kind::Fat16), None, 0),
        (140_000, Some(Kind::Fat32), None, 0),
        (5_000_000, None, None, 0),
    ]);
    let medium = Medium {
        sectors,
        bytes_per_sector: *rng.pick(&[512, 512, 1024, 4096]),
        sectors_per_track: 17,
        heads: 4,
        hidden_sectors: rng.next_u32() % 64,
        media: *rng.pick(&[0xF0, 0xF8, 0xF9, 0xFD]),
    };
    let shape = Shape {
        kind,
        sectors_per_cluster: spc,
        root_entries: root,
        serial: rng.next_u32(),
        label: None,
    };
    match format::plan(&medium, &shape) {
        Ok(bpb) => format::boot_sector(&bpb, b"FUZZFAT ", &rng.noise(64)),
        Err(_) => rng.noise(512).try_into().unwrap(),
    }
}

fn check_boot(boot: &[u8; 512]) {
    let dos: [u8; tairix_fat::boot::DOS_BPB] = boot[11..36].try_into().unwrap();
    if let Some(from_dos) = Bpb::from_dos(&dos) {
        assert_ne!(
            from_dos.kind,
            Kind::Fat32,
            "a DOS parameter block is FAT12 or FAT16"
        );
    }
    let Some(bpb) = Bpb::parse(boot) else {
        return;
    };
    let volume = u64::from(bpb.total_sectors) * bpb.sector_bytes();
    assert!(bpb.clusters() >= 1);
    assert!(bpb.clusters() <= bpb.kind.max_clusters());
    let entries = bpb.fat_bytes() * 8 / bpb.kind.entry_bits();
    assert!(
        u64::from(bpb.max_cluster()) < entries,
        "every cluster has an entry"
    );
    assert!(bpb.fat_start() < bpb.data_start() || bpb.fats == 0);
    assert_eq!(bpb.root_start() + bpb.root_bytes(), bpb.data_start());
    let last = bpb
        .cluster_start(bpb.max_cluster())
        .expect("its last cluster");
    assert!(last + bpb.cluster_bytes() <= volume, "inside the volume");
    assert_eq!(bpb.cluster_start(bpb.max_cluster() + 1), None);
    if bpb.kind == Kind::Fat32 {
        assert!((2..=bpb.max_cluster()).contains(&bpb.root_cluster));
    }
    let rewritten = format::boot_sector(&bpb, b"FUZZFAT ", &[]);
    let reread = Bpb::parse(&rewritten).expect("its own boot sector");
    assert_eq!(reread.clusters(), bpb.clusters());
    assert_eq!(reread.kind, bpb.kind);
}

fn check_directory(rng: &mut Prng) {
    let mut name = LongName::new();
    let mut out = [0u8; 255 * 3];
    for _ in 0..rng.below(24) {
        let mut raw: [u8; ENTRY] = rng.noise(ENTRY).try_into().unwrap();
        if rng.below(3) == 0 {
            raw[11] = dir::attr::LONG_NAME;
            raw[0] = u8::try_from(rng.below(22)).unwrap()
                | if rng.below(4) == 0 { dir::LAST } else { 0 };
        }
        let entry = ShortEntry::decode(&raw);
        assert_eq!(entry.encode(), raw, "a short entry round trips");
        match dir::slot(&raw) {
            dir::Slot::Long => name.push(&raw),
            dir::Slot::Short => {
                if let Some(len) = name.finish(&entry.name, &mut out) {
                    assert!(len > 0 && len <= out.len());
                }
                name.reset();
            }
            _ => {}
        }
    }
    let units: Vec<u16> = (0..rng.below(300)).map(|_| rng.next_u16()).collect();
    let mut small = [0u8; 40];
    if let Some(len) = dir::decode_utf16(&units, &mut small) {
        assert!(len <= small.len());
    }
}

fn check_table(rng: &mut Prng) {
    let kind = *rng.pick(&[Kind::Fat12, Kind::Fat16, Kind::Fat32]);
    let mut fat = rng.noise(64);
    let cluster = u32::try_from(rng.below(48)).unwrap();
    let neighbour = if cluster > 0 {
        cluster - 1
    } else {
        cluster + 1
    };
    let before = table::get(kind, &fat, neighbour);
    let value = rng.next_u32() & table::mask(kind);
    if table::set(kind, &mut fat, cluster, value) {
        assert_eq!(table::get(kind, &fat, cluster), Some(value));
        assert_eq!(
            table::get(kind, &fat, neighbour),
            before,
            "the neighbour kept"
        );
    }
    let _ = table::classify(kind, rng.next_u32(), rng.next_u32());
}

#[test]
fn hostile_volumes_never_panic_or_lay_a_region_outside_themselves() {
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "hostile_volumes_never_panic_or_lay_a_region_outside_themselves",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    let mut parsed = 0u64;
    let mut runs = 0u64;
    loop {
        for _ in 0..SMOKE_ITERATIONS {
            let pristine = well_formed(&mut rng);
            let boot = if rng.below(2) == 0 {
                let mut bytes = rng.mutated(&pristine);
                bytes.resize(512, 0);
                bytes[..512].try_into().unwrap()
            } else {
                pristine
            };
            if Bpb::parse(&boot).is_some() {
                parsed += 1;
            }
            check_boot(&boot);
            check_directory(&mut rng);
            check_table(&mut rng);
            runs += 1;
        }
        if !tairix_fuzzseed::within_budget(deadline) {
            break;
        }
    }
    assert!(
        parsed * 4 >= runs,
        "the well-formed half parses: {parsed} of {runs}"
    );
}
