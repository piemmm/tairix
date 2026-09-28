use super::{
    lerp, Ramp, ASH, BASALT, BLACK_SAND, CHALK, CLAY_CRUST, COOLED_LAVA, DRY_GRASS, DUNE_SAND,
    DUST, EMBER, FOREST_LOAM, GOLDEN_SAND, GRANITE, GRAVEL, HEATH, ICE, LATERITE, LEAF,
    LEAF_LITTER, LICHEN, LIMESTONE, LUSH_GRASS, MEADOW, MOSS, MUD, NEEDLE_LITTER, PEAT, RAIN,
    RAINFOREST_FLOOR, RED_SAND, RIFT_GROUND, SALT_PAN, SANDSTONE, SCHIST, SCREE, SHALE,
    SHIELD_ROCK, SHINGLE, SHORT_GRASS, SMOKE, SNOW, SNOWFALL, SPLASH, TALL_GRASS, WATER,
    WHITE_SAND,
};
use tairix_raster::color::Color;

/// Every ground ramp the palette publishes.
const GROUNDS: &[Ramp] = &[
    WATER,
    ICE,
    SNOW,
    LICHEN,
    MOSS,
    NEEDLE_LITTER,
    LEAF_LITTER,
    FOREST_LOAM,
    RAINFOREST_FLOOR,
    SHORT_GRASS,
    LUSH_GRASS,
    DRY_GRASS,
    TALL_GRASS,
    MEADOW,
    HEATH,
    PEAT,
    MUD,
    WHITE_SAND,
    GOLDEN_SAND,
    RED_SAND,
    BLACK_SAND,
    DUNE_SAND,
    GRAVEL,
    SHINGLE,
    SCREE,
    CLAY_CRUST,
    SALT_PAN,
    LATERITE,
    ASH,
    COOLED_LAVA,
    SHIELD_ROCK,
    GRANITE,
    BASALT,
    LIMESTONE,
    SANDSTONE,
    SHALE,
    CHALK,
    SCHIST,
    RIFT_GROUND,
];

/// Every particle ramp.
const PARTICLES: &[Ramp] = &[RAIN, SNOWFALL, EMBER, SMOKE, DUST, SPLASH, LEAF];

fn every_ramp() -> impl Iterator<Item = &'static Ramp> {
    GROUNDS.iter().chain(PARTICLES.iter())
}

/// Perceived brightness, weighted as the eye weighs the channels.
fn luma(c: Color) -> u32 {
    u32::from(c.r) * 2 + u32::from(c.g) * 5 + u32::from(c.b)
}

/// How far apart two tones read: channel distance, weighted as the eye
/// weighs the channels.
fn distance(a: Color, b: Color) -> u32 {
    let d = |x: u8, y: u8| u32::from(x.abs_diff(y));
    d(a.r, b.r) * 2 + d(a.g, b.g) * 3 + d(a.b, b.b)
}

#[test]
fn there_is_a_ramp_for_every_ground() {
    use tairix_wintersun_world::blend::Kind;
    use tairix_wintersun_world::ground::Ground;
    assert_eq!(GROUNDS.len(), Ground::ALL.len());
    for (index, &ground) in Ground::ALL.iter().enumerate() {
        let ramp = crate::material::params(ground).ramp;
        assert!(
            GROUNDS.contains(&ramp),
            "{ground:?} is drawn from no ground ramp"
        );
        for &other in &Ground::ALL[..index] {
            assert_ne!(
                crate::material::params(other).ramp,
                ramp,
                "{ground:?} shares {other:?}'s ramp"
            );
        }
    }
}

#[test]
fn every_ramp_is_ordered_dark_to_light() {
    for ramp in every_ramp() {
        assert!(
            luma(ramp.shadow) < luma(ramp.mid),
            "shadow is not darker than mid in {ramp:?}"
        );
        assert!(
            luma(ramp.mid) < luma(ramp.light),
            "mid is not darker than light in {ramp:?}"
        );
    }
}

#[test]
fn every_ramp_is_opaque() {
    for ramp in every_ramp() {
        assert_eq!((ramp.shadow.a, ramp.mid.a, ramp.light.a), (255, 255, 255));
    }
}

#[test]
fn sample_hits_all_three_stops_exactly() {
    for ramp in every_ramp() {
        assert_eq!(ramp.sample(0), ramp.shadow);
        assert_eq!(ramp.sample(128), ramp.mid);
        assert_eq!(ramp.sample(255), ramp.light);
    }
}

#[test]
fn sample_is_monotone_across_the_whole_axis() {
    for ramp in every_ramp() {
        let mut previous = luma(ramp.sample(0));
        for t in 1..=u8::MAX {
            let next = luma(ramp.sample(t));
            assert!(next >= previous, "{ramp:?} dips at t={t}");
            previous = next;
        }
    }
}

#[test]
fn lerp_hits_both_ends_and_rounds_the_middle() {
    let a = Color::rgb(0, 10, 200);
    let b = Color::rgb(255, 20, 100);
    assert_eq!(lerp(a, b, 0), a);
    assert_eq!(lerp(a, b, 255), b);
    // 128/255 of the way, rounded to nearest: 128, 15, 150.
    assert_eq!(lerp(a, b, 128), Color::rgb(128, 15, 150));
}

#[test]
fn lerp_carries_alpha() {
    let a = Color::rgba(0, 0, 0, 0);
    let b = Color::rgba(0, 0, 0, 255);
    assert_eq!(lerp(a, b, 255).a, 255);
    assert_eq!(lerp(a, b, 0).a, 0);
}

#[test]
fn the_ground_set_spans_the_climate_range() {
    // From glacier ice to red desert: warm grounds and cold ones both, and a
    // brightness range from fresh lava to a salt pan.
    let warm = GROUNDS
        .iter()
        .filter(|r| u32::from(r.mid.r) > u32::from(r.mid.b) + 60)
        .count();
    let cold = GROUNDS.iter().filter(|r| r.mid.b >= r.mid.r).count();
    assert!(warm >= 6, "only {warm} warm grounds");
    assert!(cold >= 6, "only {cold} cold grounds");
    let darkest = GROUNDS.iter().map(|r| luma(r.mid)).min().unwrap_or(0);
    let brightest = GROUNDS.iter().map(|r| luma(r.mid)).max().unwrap_or(0);
    assert!(darkest < 400 && brightest > 1700, "{darkest}..{brightest}");
}

/// How far apart two grounds that meet must read in their mid tones alone,
/// before grain or relief tell them apart: the difference between two
/// neighbouring grass greens, the finest distinction the set asks the eye to
/// make.
const APART: u32 = 60;

#[test]
fn grounds_that_meet_in_nature_stay_distinguishable() {
    // Pairs that share a boundary somewhere in nature, whether or not a
    // realm the world makes today lays them side by side.
    let pairs = [
        ("water, mud", WATER, MUD),
        ("water, sand", WATER, GOLDEN_SAND),
        ("water, shingle", WATER, SHINGLE),
        ("snow, ice", SNOW, ICE),
        ("snow, scree", SNOW, SCREE),
        ("ice, scree", ICE, SCREE),
        ("lichen, moss", LICHEN, MOSS),
        ("lichen, short grass", LICHEN, SHORT_GRASS),
        ("lichen, gravel", LICHEN, GRAVEL),
        ("moss, needles", MOSS, NEEDLE_LITTER),
        ("moss, peat", MOSS, PEAT),
        ("needles, loam", NEEDLE_LITTER, FOREST_LOAM),
        ("leaves, loam", LEAF_LITTER, FOREST_LOAM),
        ("leaves, lush grass", LEAF_LITTER, LUSH_GRASS),
        ("short, lush grass", SHORT_GRASS, LUSH_GRASS),
        ("short, dry grass", SHORT_GRASS, DRY_GRASS),
        ("short, tall grass", SHORT_GRASS, TALL_GRASS),
        ("short grass, meadow", SHORT_GRASS, MEADOW),
        ("dry, tall grass", DRY_GRASS, TALL_GRASS),
        ("dry grass, laterite", DRY_GRASS, LATERITE),
        ("dry grass, clay", DRY_GRASS, CLAY_CRUST),
        ("heath, peat", HEATH, PEAT),
        ("heath, short grass", HEATH, SHORT_GRASS),
        ("mud, tall grass", MUD, TALL_GRASS),
        ("mud, lush grass", MUD, LUSH_GRASS),
        ("sand, dune", GOLDEN_SAND, DUNE_SAND),
        ("sand, gravel", GOLDEN_SAND, GRAVEL),
        ("clay, salt", CLAY_CRUST, SALT_PAN),
        ("clay, sand", CLAY_CRUST, GOLDEN_SAND),
        ("red sand, laterite", RED_SAND, LATERITE),
        ("red sand, sandstone", RED_SAND, SANDSTONE),
        ("lava, ash", COOLED_LAVA, ASH),
        ("lava, black sand", COOLED_LAVA, BLACK_SAND),
        ("gravel, shingle", GRAVEL, SHINGLE),
        ("granite, scree", GRANITE, SCREE),
        ("limestone, short grass", LIMESTONE, SHORT_GRASS),
        ("chalk, short grass", CHALK, SHORT_GRASS),
        ("shale, clay", SHALE, CLAY_CRUST),
        ("schist, scree", SCHIST, SCREE),
        ("basalt, black sand", BASALT, BLACK_SAND),
        ("shield, lichen", SHIELD_ROCK, LICHEN),
        ("rainforest floor, leaves", RAINFOREST_FLOOR, LEAF_LITTER),
        ("rift, rock", RIFT_GROUND, GRANITE),
    ];
    for (name, a, b) in pairs {
        let apart = distance(a.mid, b.mid);
        assert!(apart >= APART, "{name} read as one ground: {apart}");
    }
}

#[test]
fn grounds_the_world_lays_together_stay_distinguishable() {
    // Every pair of grounds that share a cell, or stand in neighbouring
    // cells, across the default realm and a polar, an equatorial, a dry and a
    // many-plated one.
    use alloc::collections::BTreeSet;
    use alloc::format;
    use alloc::vec::Vec;
    use tairix_wintersun_net::value::ChunkCoord;
    use tairix_wintersun_world::blend::Kind;
    use tairix_wintersun_world::chunk::ChunkBuild;
    use tairix_wintersun_world::geom::CHUNK_CELLS;
    use tairix_wintersun_world::ground::Ground;
    use tairix_wintersun_world::params::{RealmParams, RealmSpec};
    use tairix_wintersun_world::realm::RealmField;

    /// A share of a cell a player sees as a ground of its own.
    const VISIBLE: u8 = 32;

    let base = RealmSpec {
        extent_chunks: 64,
        coarse_samples: 64,
        ..RealmParams::default_realm(1).spec()
    };
    let realms = [
        base,
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
    ];
    let mut pairs = BTreeSet::new();
    let mut meet = |a: Ground, b: Ground| {
        if a != b {
            pairs.insert((a.id().min(b.id()), a.id().max(b.id())));
        }
    };
    for spec in realms {
        let params = RealmParams::new(spec).expect("legal");
        let field = RealmField::generate(params).expect("solves");
        let half = params.half_extent_chunks();
        for y in (-half..half).step_by(8) {
            for x in (-half..half).step_by(8) {
                let chunk = ChunkBuild::new(ChunkCoord { x, y })
                    .and_then(|build| build.finish(&field))
                    .expect("builds");
                for cy in 0..CHUNK_CELLS {
                    for cx in 0..CHUNK_CELLS {
                        let here = chunk.ground(cx, cy);
                        let seen: Vec<Ground> = here
                            .slots()
                            .filter(|&(_, weight)| weight >= VISIBLE)
                            .map(|(ground, _)| ground)
                            .collect();
                        for (i, &a) in seen.iter().enumerate() {
                            for &b in &seen[i + 1..] {
                                meet(a, b);
                            }
                        }
                        if cx + 1 < CHUNK_CELLS {
                            meet(here.dominant(), chunk.ground(cx + 1, cy).dominant());
                        }
                        if cy + 1 < CHUNK_CELLS {
                            meet(here.dominant(), chunk.ground(cx, cy + 1).dominant());
                        }
                    }
                }
            }
        }
    }
    let ramp = |id: u8| crate::material::params(Ground::ALL[usize::from(id)]).ramp;
    let close: Vec<_> = pairs
        .iter()
        .filter(|&&(a, b)| distance(ramp(a).mid, ramp(b).mid) < APART)
        .map(|&(a, b)| {
            format!(
                "{:?}/{:?}",
                Ground::ALL[usize::from(a)],
                Ground::ALL[usize::from(b)]
            )
        })
        .collect();
    assert!(pairs.len() > 200, "only {} pairs met", pairs.len());
    assert!(close.is_empty(), "read as one ground: {close:?}");
}
