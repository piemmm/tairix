//! The world's biomes, surveyed across whole realms.
//!
//! The classifier's own tests prove it total and each climate's biome where
//! the climatologists put it. These prove the whole generator: that a realm
//! solved from its parameters actually reaches every biome its latitude span
//! can hold, and that no one biome swallows the land.

use tairix_wintersun_net::value::{ChunkCoord, Facing};
use tairix_wintersun_world::biome::{Biome, BIOME_COUNT};
use tairix_wintersun_world::blend::Kind;
use tairix_wintersun_world::chunk::ChunkBuild;
use tairix_wintersun_world::geom::CHUNK_CELLS;
use tairix_wintersun_world::params::{RealmParams, RealmSpec};
use tairix_wintersun_world::realm::RealmField;

/// The most of a realm's land any one biome may hold, in percent.
const LARGEST_SHARE_PERCENT: u64 = 20;

/// How many cells each biome is the dominant biome of, over the chunks on a
/// grid `stride` chunks apart across the realm.
fn survey(params: RealmParams, stride: usize) -> [u64; BIOME_COUNT] {
    let field = RealmField::generate(params).expect("the realm solves");
    let half = params.half_extent_chunks();
    let mut counts = [0_u64; BIOME_COUNT];
    for y in (-half..half).step_by(stride) {
        for x in (-half..half).step_by(stride) {
            let chunk = ChunkBuild::new(ChunkCoord { x, y })
                .expect("fits")
                .finish(&field)
                .expect("builds");
            for cy in 0..CHUNK_CELLS {
                for cx in 0..CHUNK_CELLS {
                    counts[usize::from(chunk.biome(cx, cy).dominant().id())] += 1;
                }
            }
        }
    }
    counts
}

fn missing(counts: &[u64; BIOME_COUNT]) -> Vec<Biome> {
    Biome::ALL
        .iter()
        .copied()
        .filter(|biome| counts[usize::from(biome.id())] == 0)
        .collect()
}

#[test]
fn the_default_realm_holds_every_biome_and_none_holds_the_land() {
    // The default realm runs from the ice sheet to past the equator, so
    // every biome in the table is within its reach.
    let counts = survey(RealmParams::default_realm(1), 16);
    assert!(
        missing(&counts).is_empty(),
        "absent: {:?}",
        missing(&counts)
    );

    let land: u64 = counts.iter().sum::<u64>() - counts[usize::from(Biome::OpenWater.id())];
    for &biome in Biome::ALL {
        if biome == Biome::OpenWater {
            continue;
        }
        let held = counts[usize::from(biome.id())];
        assert!(
            held * 100 <= land * LARGEST_SHARE_PERCENT,
            "{biome:?} holds {held} of {land} land cells"
        );
    }
}

#[test]
fn every_biome_in_the_table_occurs_across_the_probe_realms() {
    // A polar realm, an equatorial one, a dry continent across the middle
    // latitudes, a many-plated one with rifts and basalt everywhere, and one
    // whose westerlies veer south of east rather than north.
    let base = RealmSpec {
        extent_chunks: 64,
        coarse_samples: 64,
        ..RealmParams::default_realm(0).spec()
    };
    // Each probe with the stride it is surveyed at: a dry continent's
    // deserts cluster in its interior basins, as real ones do, so it is
    // surveyed densely.
    let probes = [
        RealmSpec {
            seed: 0x9013,
            north_latitude: 90,
            south_latitude: 48,
            ..base
        },
        RealmSpec {
            seed: 0xE0A7,
            north_latitude: 22,
            south_latitude: -22,
            ocean_permille: 450,
            ..base
        },
        RealmSpec {
            seed: 0xC01D,
            north_latitude: 55,
            south_latitude: 28,
            ocean_permille: 150,
            ..base
        },
        RealmSpec {
            seed: 0x71F7,
            plates: 64,
            relief_units: 2600,
            ..base
        },
        RealmSpec {
            seed: 0xD3FA,
            westerlies: Facing(0x0800),
            ..base
        },
    ];
    let mut union = [0_u64; BIOME_COUNT];
    for spec in probes {
        let stride = if spec.ocean_permille < 200 { 4 } else { 8 };
        let counts = survey(RealmParams::new(spec).expect("legal"), stride);
        for (total, count) in union.iter_mut().zip(counts) {
            *total += count;
        }
    }
    assert!(missing(&union).is_empty(), "absent: {:?}", missing(&union));
}
