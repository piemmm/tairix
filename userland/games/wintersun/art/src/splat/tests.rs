use tairix_raster::color::Pixel;
use tairix_reclaim::Unpressured;
use tairix_wintersun_net::value::WorldPoint;
use tairix_wintersun_world::blend::BLEND_SLOTS;
use tairix_wintersun_world::ground::Ground;

use super::{
    cutoff_below, fragment, mean, Fragment, Materials, QuadPlan, Warp, BLEND_DEPTH, HEIGHT_SHIFT,
    MAX_BLEND_TOTAL, MAX_HEIGHT_SCORE, QUAD_ONE,
};
use crate::cache::{MaterialCache, TileKey};
use crate::material::{self, MaterialTile, Mip, Quality};
use crate::weight::{WeightField, TOTAL};

const SEED: u64 = 0x5748_4954_4553_554E;

/// A fragment at `sample`, `across` and `down` its quad, read at the finest
/// mip a fine pixel picks.
fn at(sample: WorldPoint, across: u16, down: u16) -> Fragment {
    Fragment {
        sample,
        across,
        down,
        footprint: 1,
        dither: 0,
    }
}

const ORIGIN: WorldPoint = WorldPoint { x: 0, y: 0 };

fn solid(ground: Ground) -> QuadPlan {
    let field = WeightField::solid(ground);
    QuadPlan::new([&field; 4])
}

fn tile(ground: Ground, level: u32) -> MaterialTile {
    MaterialTile::synthesise(
        ground,
        Mip::new(level).expect("in the chain"),
        Quality::FULL,
    )
    .expect("a tile fits in test memory")
}

#[test]
fn a_plan_over_one_material_is_that_material_at_full_weight() {
    let plan = solid(Ground::Granite);
    assert_eq!(plan.len(), 1);
    assert!(!plan.is_empty());
    assert_eq!(plan.grounds().next(), Some(Ground::Granite));
    assert_eq!(plan.weights_at(QUAD_ONE / 2, QUAD_ONE / 3)[0], TOTAL);
}

#[test]
fn a_plan_unions_its_corners() {
    let sand = WeightField::solid(Ground::GoldenSand);
    let water = WeightField::solid(Ground::Water);
    let plan = QuadPlan::new([&sand, &water, &sand, &water]);
    let grounds: alloc::vec::Vec<_> = plan.grounds().collect();
    assert_eq!(grounds.len(), 2);
    assert!(grounds.contains(&Ground::GoldenSand) && grounds.contains(&Ground::Water));
}

#[test]
fn a_plan_never_exceeds_the_slot_count() {
    let mut a = WeightField::solid(Ground::Granite);
    a.cover(Ground::Gravel, 180);
    a.cover(Ground::GoldenSand, 100);
    let mut b = WeightField::solid(Ground::Ice);
    b.cover(Ground::Snow, 180);
    b.cover(Ground::Lichen, 100);
    let c = WeightField::solid(Ground::Peat);
    let d = WeightField::solid(Ground::Water);
    let plan = QuadPlan::new([&a, &b, &c, &d]);
    assert_eq!(plan.len(), BLEND_SLOTS);
    assert_eq!(plan.grounds().count(), BLEND_SLOTS);
}

#[test]
fn every_corner_stays_normalised_and_keeps_to_its_own_grounds() {
    let snow = WeightField::solid(Ground::Snow);
    let mut shore = WeightField::solid(Ground::Water);
    shore.cover(Ground::Mud, 100);
    shore.cover(Ground::Peat, 60);
    shore.cover(Ground::GoldenSand, 40);
    let plan = QuadPlan::new([&snow, &shore, &shore, &shore]);
    let corners = [(0, 0), (QUAD_ONE, 0), (0, QUAD_ONE), (QUAD_ONE, QUAD_ONE)];
    for (index, (across, down)) in corners.into_iter().enumerate() {
        let weights = plan.weights_at(across, down);
        assert_eq!(weights.iter().sum::<u16>(), TOTAL, "corner {index}");
        let snowy = plan
            .grounds()
            .position(|g| g == Ground::Snow)
            .map_or(0, |slot| weights[slot]);
        assert_eq!(snowy == 0, index != 0, "snow is the first corner's alone");
    }
}

#[test]
fn a_fragment_with_no_tiles_draws_the_flat_tones() {
    let flat = material::params(Ground::Peat).flat();
    for across in [0, 77, QUAD_ONE] {
        let pixel = fragment(
            &solid(Ground::Peat),
            &Materials::flat(),
            at(ORIGIN, across, 9),
        );
        assert_eq!(pixel.a, u8::MAX, "ground is opaque");
        assert_eq!((pixel.r, pixel.g, pixel.b), (flat.r, flat.g, flat.b));
    }
}

#[test]
fn a_fragment_is_always_opaque() {
    let mut litter = WeightField::solid(Ground::NeedleLitter);
    litter.cover(Ground::Granite, 120);
    let snow = WeightField::solid(Ground::Snow);
    let plan = QuadPlan::new([&litter, &snow, &snow, &litter]);
    let granite = tile(Ground::Granite, 0);
    let mut materials = Materials::flat();
    materials.hold(&granite);
    for step in 0..64u16 {
        let sample = WorldPoint {
            x: i32::from(step) * 37,
            y: i32::from(step) * -91,
        };
        let pixel = fragment(&plan, &materials, at(sample, step * 4, 255 - step * 3));
        assert_eq!(pixel.a, u8::MAX);
    }
}

#[test]
fn a_quad_walks_from_one_ground_to_the_other() {
    // The property a terrain boundary is: one side reads as one ground, the
    // other as the other, with no jump between.
    let snow = WeightField::solid(Ground::Snow);
    let ash = WeightField::solid(Ground::Ash);
    let plan = QuadPlan::new([&snow, &ash, &snow, &ash]);
    let row: alloc::vec::Vec<Pixel> = (0..=QUAD_ONE)
        .map(|across| fragment(&plan, &Materials::flat(), at(ORIGIN, across, 128)))
        .collect();
    let (snow, ash) = (
        material::params(Ground::Snow).flat(),
        material::params(Ground::Ash).flat(),
    );
    assert_eq!((row[0].r, row[0].g, row[0].b), (snow.r, snow.g, snow.b));
    let last = row[row.len() - 1];
    assert_eq!((last.r, last.g, last.b), (ash.r, ash.g, ash.b));
    let biggest = row
        .windows(2)
        .map(|pair| (i32::from(pair[1].r) - i32::from(pair[0].r)).abs())
        .max()
        .unwrap_or(0);
    assert!(biggest < 120, "the boundary jumps by {biggest}");
}

#[test]
fn the_height_offset_makes_a_standing_material_win_early() {
    // Against a linear blend, the middle of a snow/peat quad sits toward
    // snow, because snow stands far higher than peat.
    let snow = WeightField::solid(Ground::Snow);
    let peat = WeightField::solid(Ground::Peat);
    let plan = QuadPlan::new([&snow, &peat, &snow, &peat]);
    let middle = fragment(&plan, &Materials::flat(), at(ORIGIN, QUAD_ONE / 2, 0));
    let (snow, peat) = (
        material::params(Ground::Snow).flat(),
        material::params(Ground::Peat).flat(),
    );
    let to_snow = i32::from(middle.r) - i32::from(peat.r);
    let to_peat = i32::from(snow.r) - i32::from(middle.r);
    assert!(
        to_snow > to_peat,
        "snow {to_peat} / peat {to_snow} is a linear blend"
    );
}

/// A coarser mip is the same ground sampled further apart: its tile repeats
/// at the full tile's world period, and one of its texels covers as much
/// ground as the mip's spacing. Addressing a mip at the finest spacing
/// instead repeated a mip-3 tile eight times too often.
#[test]
fn a_mip_keeps_the_grounds_world_scale() {
    let ground = Ground::Gravel;
    let shift = material::params(ground).grain_shift;
    let level = 3;
    let coarse = tile(ground, level);
    let mut materials = Materials::flat();
    materials.hold(&coarse);
    let plan = solid(ground);
    let footprint = 1u32 << (shift + level);
    let read = |x: i32| {
        fragment(
            &plan,
            &materials,
            Fragment {
                sample: WorldPoint { x, y: 1 << 13 },
                across: 0,
                down: 0,
                footprint,
                dither: 0,
            },
        )
    };
    let period = i32::try_from(material::TILE_SIDE << shift).expect("a period fits");
    let short = period >> level;
    let mut repeats_short = 0;
    for step in 0..64 {
        let x = step * (1 << (shift + level));
        assert_eq!(
            read(x),
            read(x + period),
            "the mip repeats at the tile's world period"
        );
        assert_eq!(
            read(x),
            read(x + (1 << (shift + level)) - 1),
            "one texel spans the mip's spacing"
        );
        if read(x) == read(x + short) {
            repeats_short += 1;
        }
    }
    assert!(
        repeats_short < 48,
        "the mip repeats at its own side in finest texels"
    );
}

#[test]
fn materials_fall_back_to_a_coarser_tile_then_a_finer_one_and_only_then_to_flat() {
    let mut cache = MaterialCache::new("splat-tests", 64 << 20, &Unpressured, &Quiet);
    let key = |level| TileKey {
        ground: Ground::Granite,
        mip: Mip::new(level).expect("in the chain"),
    };
    assert!(cache.ensure(Quality::FULL, key(4)));
    let materials = Materials::resolve(&cache, Quality::FULL, [Ground::Granite].into_iter());
    for level in 0..=4 {
        let held = materials.tile(Ground::Granite, Mip::new(level).expect("in the chain"));
        assert_eq!(
            held.map(|t| t.mip().level()),
            Some(4),
            "level {level} reads the coarser tile"
        );
    }
    assert_eq!(
        materials
            .tile(Ground::Granite, Mip::coarsest())
            .map(|t| t.mip().level()),
        Some(4),
        "with nothing coarser held, the coarsest level reads the finer tile"
    );
    assert!(
        materials.tile(Ground::Snow, Mip::BASE).is_none(),
        "an unasked ground is flat"
    );
}

struct Quiet;

impl tairix_log::Sink for Quiet {
    fn write_event(&self, _: &tairix_log::Event<'_>) {}
}

/// One ground blends nothing, so its quad draws exactly the texel it reads,
/// wherever the fragment lies in the quad.
#[test]
fn a_quad_of_one_ground_draws_its_texel() {
    let ground = Ground::Heath;
    let heath = tile(ground, 0);
    let mut materials = Materials::flat();
    materials.hold(&heath);
    let plan = solid(ground);
    let shift = material::params(ground).grain_shift;
    for step in 0u16..64 {
        let sample = WorldPoint {
            x: i32::from(step) * 97,
            y: i32::from(step) * 53,
        };
        let drawn = fragment(&plan, &materials, at(sample, step * 4, QUAD_ONE - step));
        let read = heath.texel(
            u32::try_from(sample.x >> shift).expect("east of the origin"),
            u32::try_from(sample.y >> shift).expect("south of the origin"),
        );
        assert_eq!(
            (drawn.r, drawn.g, drawn.b, drawn.a),
            (read.r, read.g, read.b, u8::MAX)
        );
    }
}

#[test]
fn negative_world_coordinates_draw() {
    // Half the world is west and north of the origin, and a tile read there
    // must wrap rather than fault or clamp.
    let sand = tile(Ground::GoldenSand, 0);
    let mut materials = Materials::flat();
    materials.hold(&sand);
    let plan = solid(Ground::GoldenSand);
    let row: alloc::vec::Vec<Pixel> = (0..48)
        .map(|step| {
            let sample = WorldPoint {
                x: -1_000_000 + step * 32,
                y: -2_000_000,
            };
            fragment(&plan, &materials, at(sample, 0, 0))
        })
        .collect();
    assert!(row.iter().all(|p| p.a == u8::MAX));
    assert!(row.iter().any(|p| *p != row[0]), "the row is flat");
}

#[test]
fn the_warp_breaks_a_tile_period() {
    // Without the warp, two points exactly one tile period apart read the
    // same texel. With it, they must not.
    let ground = Ground::Gravel;
    let gravel = tile(ground, 0);
    let mut materials = Materials::flat();
    materials.hold(&gravel);
    let plan = solid(ground);
    let shift = material::params(ground).grain_shift;
    let period = i32::try_from(gravel.side() << shift).expect("a tile's world extent fits");
    let warp = Warp::new(SEED);
    let draw = |x: i32| -> alloc::vec::Vec<Pixel> {
        (0..16)
            .map(|step| {
                let world = WorldPoint {
                    x: x + step * (1 << shift),
                    y: 4_096,
                };
                let (dx, dy) = warp.at(world);
                let sample = WorldPoint {
                    x: world.x + dx,
                    y: world.y + dy,
                };
                fragment(&plan, &materials, at(sample, 0, 0))
            })
            .collect()
    };
    assert_ne!(
        draw(0),
        draw(period),
        "one tile period apart draws identically"
    );
}

#[test]
fn a_fragment_is_reproducible() {
    let granite = tile(Ground::Granite, 0);
    let mut materials = Materials::flat();
    materials.hold(&granite);
    let plan = solid(Ground::Granite);
    let fragment_at = at(WorldPoint { x: 999, y: -777 }, 40, 200);
    assert_eq!(
        fragment(&plan, &materials, fragment_at),
        fragment(&plan, &materials, fragment_at)
    );
}

#[test]
fn the_warp_displaces_and_stays_inside_its_amplitude() {
    let warp = Warp::new(SEED);
    let mut moved = 0;
    for step in 0..64 {
        let at = WorldPoint {
            x: step * 4096,
            y: step * 2048,
        };
        let (dx, dy) = warp.at(at);
        assert!(dx.abs() <= super::WARP_AMPLITUDE);
        assert!(dy.abs() <= super::WARP_AMPLITUDE);
        if dx != 0 || dy != 0 {
            moved += 1;
        }
    }
    assert!(moved > 32, "the warp barely displaces anything");
}

#[test]
fn the_warp_is_smooth() {
    // A warp that jumped would put a visible crease across the ground.
    let warp = Warp::new(SEED);
    let step = 256;
    let mut previous = warp.at(WorldPoint { x: 0, y: 0 });
    for i in 1..200 {
        let next = warp.at(WorldPoint { x: i * step, y: 0 });
        let jump = (next.0 - previous.0).abs().max((next.1 - previous.1).abs());
        assert!(jump < super::WARP_AMPLITUDE / 8, "warp jumps {jump} at {i}");
        previous = next;
    }
}

#[test]
fn two_realms_warp_differently() {
    let a = Warp::new(SEED);
    let b = Warp::new(SEED ^ 0xFFFF);
    let at = WorldPoint {
        x: 12_345,
        y: -6_789,
    };
    assert_ne!(a.at(at), b.at(at));
}

/// The reciprocal table floors exactly as the division it replaced, over
/// every divisor a blend can produce and every channel sum that divisor
/// admits.
///
/// Exhaustive rather than sampled: the domain is small, and the whole
/// value of replacing a division with a multiply is that the two agree
/// everywhere — a table that is right for most inputs is a wrong pixel
/// nobody would find by looking.
#[test]
fn reciprocals_are_exact() {
    for total in 1..=u32::try_from(MAX_BLEND_TOTAL).expect("the bound fits") {
        // A channel sum is a sum of `u8` texels weighted by the shares
        // that make up `total`, so it cannot exceed `255 * total`.
        for sum in 0..=255 * total {
            assert_eq!(
                mean(sum, total),
                u8::try_from(sum / total).unwrap_or(u8::MAX)
            );
        }
    }
}

/// A divisor outside the table's domain is clamped into it rather than
/// read past the end, so a later change to the blend's depth or slot
/// count cannot turn into an out-of-bounds read.
#[test]
fn a_divisor_past_the_domain_is_clamped() {
    let last = u32::try_from(MAX_BLEND_TOTAL).expect("the bound fits");
    assert_eq!(mean(255, 0), mean(255, 1), "zero takes the unit divisor");
    assert_eq!(mean(255, last + 1), mean(255, last));
}

/// The bound the table rests on: no slot can contribute more than the
/// blend depth, so the divisor stays inside the domain.
#[test]
fn the_blend_cannot_produce_a_divisor_past_the_domain() {
    assert_eq!(MAX_BLEND_TOTAL, BLEND_SLOTS * usize::from(BLEND_DEPTH));
}

/// A slot the blend skips could never have contributed anything.
///
/// The skip is the one place the kernel decides *not* to read a texel, so
/// it is checked against the definition it claims to follow rather than
/// against a rendered picture: over every weight pair and every relief a
/// texel can carry, a skipped slot's share must be zero.
#[test]
fn a_skipped_slot_could_not_have_reached_the_floor() {
    for heaviest in 0..=TOTAL {
        let cutoff = cutoff_below(heaviest);
        for weight in 0..cutoff {
            for height in 0..=u8::MAX {
                // The winning score is at least the heaviest weight, so
                // the floor is at least this.
                let floor = heaviest.saturating_sub(BLEND_DEPTH);
                let score = weight.saturating_add(u16::from(height) >> HEIGHT_SHIFT);
                assert_eq!(
                    score.saturating_sub(floor),
                    0,
                    "weight {weight} with relief {height} was skipped under {heaviest}"
                );
            }
        }
    }
}

/// The relief bound the skip rests on is the whole height range, so no
/// texel can lift a slot further than the cutoff assumed.
#[test]
fn relief_cannot_exceed_its_bound() {
    assert_eq!(MAX_HEIGHT_SCORE, u16::from(u8::MAX) >> HEIGHT_SHIFT);
    for height in 0..=u8::MAX {
        assert!(u16::from(height) >> HEIGHT_SHIFT <= MAX_HEIGHT_SCORE);
    }
}

/// Nothing is skipped when no slot is far enough ahead, so a genuinely
/// blended pixel still reads every texel.
#[test]
fn a_blended_pixel_skips_nothing() {
    assert_eq!(cutoff_below(0), 0);
    assert_eq!(cutoff_below(BLEND_DEPTH + MAX_HEIGHT_SCORE), 0);
    assert!(cutoff_below(TOTAL) > 0, "a dominant material does skip");
}
