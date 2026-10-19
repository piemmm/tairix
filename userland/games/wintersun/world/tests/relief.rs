//! The fine relief's character, surveyed by biome across the probe realms.
//!
//! The relief takes its grain from the conditions the classifier reads before
//! any fine relief exists, so whether a biome's ground is as rough as its
//! biome says is a property of the whole generator, measured here over whole
//! realms rather than asserted of one grain.
#![cfg(feature = "scene-tests")]

use tairix_parallel::SERIAL;
use tairix_wintersun_net::value::{ChunkCoord, Facing};
use tairix_wintersun_world::biome::{Biome, BIOME_COUNT};
use tairix_wintersun_world::blend::Kind;
use tairix_wintersun_world::chunk::ChunkBuild;
use tairix_wintersun_world::geom::{signed, Elevation, CHUNK_CELLS};
use tairix_wintersun_world::land::{Keeping, RealmLand};
use tairix_wintersun_world::params::{RealmParams, RealmSpec};
use tairix_wintersun_world::realm::RealmField;

/// Cells a broad window reaches either side of the cell it measures.
const BROAD: i32 = 4;

/// What one biome's dry, unworked cells measure.
#[derive(Clone, Debug, Default)]
struct Measured {
    /// How far each stands from its four neighbours' mean, in world units:
    /// the knobs and creases a stride across.
    fine: Vec<f64>,
    /// How far the nine-by-nine about each stands from its best-fitting
    /// plane: the swells, the breaks and the rills, with the slope taken away.
    broad: Vec<f64>,
}

/// How far the ground about `(cx, cy)` stands from the plane best fitting
/// it, as a root mean square over the window [`BROAD`] cells either side: its
/// texture, with the slope it lies on taken away.
fn texture(height: &impl Fn(i32, i32) -> f64, (cx, cy): (i32, i32)) -> f64 {
    let (mut sum, mut sum_x, mut sum_y) = (0.0, 0.0, 0.0);
    let mut count = 0.0;
    for dy in -BROAD..=BROAD {
        for dx in -BROAD..=BROAD {
            let h = height(cx + dx, cy + dy);
            sum += h;
            sum_x += h * f64::from(dx);
            sum_y += h * f64::from(dy);
            count += 1.0;
        }
    }
    // Over a square window centred on the cell the offsets are orthogonal, so
    // each slope is its own projection.
    let spread: f64 =
        (-BROAD..=BROAD).map(|d| f64::from(d * d)).sum::<f64>() * f64::from(2 * BROAD + 1);
    let (mean, gx, gy) = (sum / count, sum_x / spread, sum_y / spread);
    let mut residual = 0.0;
    for dy in -BROAD..=BROAD {
        for dx in -BROAD..=BROAD {
            let plane = mean + gx * f64::from(dx) + gy * f64::from(dy);
            let d = height(cx + dx, cy + dy) - plane;
            residual += d * d;
        }
    }
    (residual / count).sqrt()
}

fn median(values: &mut [f64]) -> f64 {
    if values.is_empty() {
        return f64::NAN;
    }
    values.sort_by(f64::total_cmp);
    values[values.len() / 2]
}

fn probes() -> [RealmSpec; 5] {
    let base = RealmSpec {
        extent_chunks: 64,
        coarse_samples: 64,
        ..RealmParams::default_realm(0).spec()
    };
    [
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
    ]
}

/// Every biome's measures over the probe realms, each on a grid of chunks as
/// far apart as the biome survey takes them: a dry continent's deserts
/// cluster in its interior basins, so it is surveyed densely.
fn survey() -> Vec<Measured> {
    let mut by_biome = vec![Measured::default(); BIOME_COUNT];
    for spec in probes() {
        let stride = if spec.ocean_permille < 200 { 4 } else { 8 };
        let params = RealmParams::new(spec).expect("legal");
        let land = RealmLand::new(RealmField::generate(params).expect("solves"), &SERIAL)
            .expect("its land");
        let mut kept = Keeping::within(64 << 20);
        let half = params.half_extent_chunks();
        for y in (-half..half).step_by(stride) {
            for x in (-half..half).step_by(stride) {
                let chunk = ChunkBuild::new(ChunkCoord { x, y })
                    .expect("fits")
                    .finish(&land, &mut kept, &SERIAL)
                    .expect("builds");
                let height = |dx: i32, dy: i32| {
                    chunk
                        .elevation_beside(dx, dy)
                        .map_or(f64::NAN, Elevation::units)
                };
                let edge = signed(CHUNK_CELLS);
                for cy in BROAD..edge - BROAD {
                    for cx in BROAD..edge - BROAD {
                        let (ux, uy) = (cx.unsigned_abs(), cy.unsigned_abs());
                        let surface = chunk.surface(ux, uy);
                        if surface.is_water() || surface.is_cleared() {
                            continue;
                        }
                        let here = height(cx, cy);
                        let around = (height(cx - 1, cy)
                            + height(cx + 1, cy)
                            + height(cx, cy - 1)
                            + height(cx, cy + 1))
                            / 4.0;
                        let biome = usize::from(chunk.biome(ux, uy).dominant().id());
                        by_biome[biome].fine.push((here - around).abs());
                        by_biome[biome].broad.push(texture(&height, (cx, cy)));
                    }
                }
            }
        }
    }
    by_biome
}

/// A biome, the band its stride-scale roughness falls in, and the band its
/// texture does.
type Band = (Biome, (f64, f64), (f64, f64));

/// Each biome's stated relief: the band its dry cells' median stride-scale
/// roughness falls in, and the band their median texture does, both in world
/// units. Wetlands and beaches lie level; grassland, woodland and scrub swell
/// smoothly; tundra heaves into frost hummocks; badlands are rilled; and rock
/// above the treeline, sea cliffs and lava fields break.
const BANDS: [Band; 26] = [
    (Biome::IceSheet, (0.2, 0.8), (1.2, 4.5)),
    (Biome::PolarDesert, (0.08, 0.4), (0.6, 2.8)),
    (Biome::Tundra, (0.4, 1.0), (1.2, 3.8)),
    (Biome::AlpineTundra, (0.3, 1.6), (2.0, 6.5)),
    (Biome::BorealForest, (0.08, 0.45), (0.5, 2.2)),
    (Biome::TemperateConiferForest, (0.12, 0.6), (0.8, 3.0)),
    (Biome::TemperateBroadleafForest, (0.25, 1.0), (1.3, 4.5)),
    (Biome::TemperateRainforest, (0.5, 2.0), (2.5, 9.0)),
    (Biome::MediterraneanWoodland, (0.06, 0.3), (0.4, 1.3)),
    (Biome::TemperateGrassland, (0.06, 0.3), (0.4, 1.3)),
    (Biome::ColdDesert, (0.05, 0.6), (0.3, 3.0)),
    (Biome::HotDesert, (0.1, 0.6), (0.4, 2.0)),
    (Biome::XericShrubland, (0.05, 0.3), (0.4, 1.3)),
    (Biome::Savanna, (0.12, 0.55), (0.9, 3.2)),
    (Biome::TropicalDryForest, (0.25, 1.0), (1.1, 4.0)),
    (Biome::TropicalRainforest, (0.25, 1.1), (1.8, 6.0)),
    (Biome::Mangrove, (0.1, 1.5), (0.5, 9.0)),
    (Biome::SwampForest, (0.0, 0.2), (0.0, 0.9)),
    (Biome::Marsh, (0.0, 0.2), (0.0, 0.9)),
    (Biome::Bog, (0.0, 0.2), (0.0, 0.9)),
    (Biome::Fen, (0.0, 0.2), (0.0, 0.9)),
    (Biome::HeathMoor, (0.15, 0.7), (1.0, 3.6)),
    (Biome::BeachDune, (0.0, 0.3), (0.0, 1.3)),
    (Biome::RockyCoast, (1.0, 4.5), (6.0, 22.0)),
    (Biome::VolcanicBarren, (0.5, 2.0), (2.6, 9.0)),
    (Biome::Badlands, (0.22, 0.8), (0.3, 1.6)),
];

#[test]
fn each_biomes_relief_falls_within_its_stated_band() {
    let mut measured = survey();
    let mut held = 0;
    for &(biome, fine_band, texture_band) in &BANDS {
        let entry = &mut measured[usize::from(biome.id())];
        if entry.fine.is_empty() {
            // A biome the probes' dry, unworked cells away from a chunk's
            // rim do not reach is not measured; the biome survey holds that
            // the probes reach every biome.
            continue;
        }
        held += 1;
        let fine = median(&mut entry.fine);
        let texture = median(&mut entry.broad);
        assert!(
            (fine_band.0..=fine_band.1).contains(&fine),
            "{biome:?}'s stride-scale roughness is {fine}, outside {fine_band:?}"
        );
        assert!(
            (texture_band.0..=texture_band.1).contains(&texture),
            "{biome:?}'s texture is {texture}, outside {texture_band:?}"
        );
    }
    assert!(held >= BANDS.len() - 2, "only {held} biomes were measured");
    // Level ground and broken ground do not overlap.
    let band = |biome: Biome| {
        BANDS
            .iter()
            .find(|(b, _, _)| *b == biome)
            .map(|(_, _, t)| *t)
    };
    for level in [Biome::SwampForest, Biome::Marsh, Biome::Bog, Biome::Fen] {
        for broken in [
            Biome::AlpineTundra,
            Biome::RockyCoast,
            Biome::VolcanicBarren,
        ] {
            let (Some(low), Some(high)) = (band(level), band(broken)) else {
                continue;
            };
            assert!(low.1 < high.0, "{level:?} and {broken:?} bands overlap");
        }
    }
}
