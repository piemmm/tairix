//! Host tests of the water's edge set out about the eye: each plant only
//! where the water, its bank, what the water laid and the light suit it,
//! the lattices tiling the ground about the eye without a gap, and no more
//! patches than a scene's detail allows.

extern crate std;

use alloc::vec::Vec;
use std::sync::OnceLock;

use super::*;
use crate::compose::footprint::GAP;
use crate::compose::{Composition, Setting};
use crate::detail::{Densities, Detail};

/// A place `depth` deep beneath a surface falling `fall`, on silted ground
/// as wet and as upright as `ground` holds and growing, under `lit` of the
/// sky.
fn edge(depth: f64, fall: f64, (wet, upright): (f64, f64), lit: f64) -> Edge {
    Edge {
        ground: 0.0,
        level: depth,
        depth,
        fall,
        wet,
        upright,
        way: 0.0,
        laid: SILT,
        green: 1.0,
        lit,
    }
}

/// What water lays where it slows, and the gravel its floods leave.
const SILT: f64 = 0.4;
const GRAVEL: f64 = -0.1;

const STILL: f64 = 0.0;
const WET_AND_LEVEL: (f64, f64) = (0.9, 1.0);
const OPEN: f64 = 1.0;

#[test]
fn reeds_and_reedmace_stand_in_the_shallows_and_on_wet_level_banks_alone() {
    for margin in [Margin::Reed, Margin::Reedmace] {
        let at = |depth, fall, ground| suits(margin, &edge(depth, fall, ground, OPEN), false);
        assert!(
            at(0.2, STILL, WET_AND_LEVEL) > 0.99,
            "{margin:?} in the shallows"
        );
        assert!(
            at(-0.1, STILL, WET_AND_LEVEL) > 0.0,
            "{margin:?} on a wet bank"
        );
        assert!(at(1.2, STILL, WET_AND_LEVEL) <= 0.0, "{margin:?} drowned");
        assert!(
            at(-0.1, STILL, (0.2, 1.0)) <= 0.0,
            "{margin:?} on a dry bank"
        );
        assert!(
            at(-0.1, STILL, (0.9, 0.9)) <= 0.0,
            "{margin:?} on a sloping bank"
        );
        assert!(
            at(0.2, 0.05, WET_AND_LEVEL) <= 0.0,
            "{margin:?} in a torrent"
        );
        let path = Edge {
            way: 1.0,
            ..edge(-0.1, STILL, WET_AND_LEVEL, OPEN)
        };
        assert!(suits(margin, &path, false) <= 0.0, "{margin:?} on a path");
    }
    // Reeds take to higher banks and brisker water than reedmace does.
    let bank = edge(-0.25, STILL, WET_AND_LEVEL, OPEN);
    assert!(suits(Margin::Reed, &bank, false) > 0.5);
    assert!(suits(Margin::Reedmace, &bank, false) <= 0.0);
    let brisk = edge(0.2, 0.012, WET_AND_LEVEL, OPEN);
    assert!(suits(Margin::Reed, &brisk, false) > 0.5);
    assert!(suits(Margin::Reedmace, &brisk, false) <= 0.0);
}

#[test]
fn lilies_and_pondweed_float_on_still_water_as_deep_as_they_root_in() {
    for margin in [Margin::Lily, Margin::Pondweed] {
        let at = |depth, fall| suits(margin, &edge(depth, fall, WET_AND_LEVEL, OPEN), false);
        assert!(at(1.0, STILL) > 0.99, "{margin:?}");
        assert!(at(0.1, STILL) <= 0.0, "{margin:?} too shallow");
        assert!(at(-0.2, STILL) <= 0.0, "{margin:?} on the bank");
        assert!(at(3.0, STILL) <= 0.0, "{margin:?} too deep to root in");
        assert!(at(1.0, 0.01) <= 0.0, "{margin:?} on running water");
    }
    // Pondweed takes to shallower water and a gentler flow than lilies do.
    let shallow = edge(0.3, STILL, WET_AND_LEVEL, OPEN);
    assert!(suits(Margin::Pondweed, &shallow, false) > 0.5);
    assert!(suits(Margin::Lily, &shallow, false) <= 0.0);
    let gentle = edge(1.0, 0.0025, WET_AND_LEVEL, OPEN);
    assert!(suits(Margin::Pondweed, &gentle, false) > 0.5);
    assert!(suits(Margin::Lily, &gentle, false) <= 0.0);
}

#[test]
fn none_grows_in_the_gloom_beneath_a_closed_wood() {
    for margin in KINDS {
        let depth = if matches!(margin, Margin::Lily | Margin::Pondweed) {
            1.0
        } else {
            0.2
        };
        // Crowfoot runs over gravel; the rest stand in still water's silt.
        let (fall, laid) = if margin == Margin::Crowfoot {
            (0.01, GRAVEL)
        } else {
            (STILL, SILT)
        };
        let under = |lit| {
            let place = Edge {
                laid,
                ..edge(depth, fall, WET_AND_LEVEL, lit)
            };
            suits(margin, &place, false)
        };
        assert!(under(OPEN) > 0.99, "{margin:?}");
        assert!(under(0.5) < under(0.8), "{margin:?}");
        assert!(under(0.2) <= 0.0, "{margin:?} under a closed canopy");
    }
    // Pondweed bears more shade than lilies do.
    let glade = edge(1.0, STILL, WET_AND_LEVEL, 0.45);
    assert!(suits(Margin::Pondweed, &glade, false) > 0.5);
    assert!(suits(Margin::Lily, &glade, false) <= 0.0);
}

#[test]
fn a_far_bed_wants_a_more_level_bank_than_a_near_clump() {
    let bank = edge(-0.1, STILL, (0.9, 0.98), OPEN);
    assert!(suits(Margin::Reed, &bank, false) > 0.99);
    assert!(suits(Margin::Reed, &bank, true) <= 0.0);
    // Standing in the water, the slope of the bed beneath matters to neither.
    let shallows = edge(0.2, STILL, (0.9, 0.9), OPEN);
    assert_eq!(
        suits(Margin::Reed, &shallows, false).to_bits(),
        suits(Margin::Reed, &shallows, true).to_bits()
    );
}

#[test]
fn the_floating_plants_die_back_over_winter_and_the_reeds_stand_on() {
    for season in [
        Season::Spring,
        Season::Summer,
        Season::Autumn { fallen: 80 },
    ] {
        assert!(
            KINDS.iter().all(|&margin| grows(margin, season)),
            "{season:?}"
        );
    }
    assert!(grows(Margin::Reed, Season::Winter) && grows(Margin::Reedmace, Season::Winter));
    assert!(!grows(Margin::Lily, Season::Winter) && !grows(Margin::Pondweed, Season::Winter));
    assert!(!grows(Margin::Crowfoot, Season::Winter));
}

/// Crowfoot streams only where the water runs, as shallow or deep as it
/// roots in, anchored in gravel more than silt, and not where a riffle races
/// too fast for it, on a bank, or in a still pool.
#[test]
fn crowfoot_streams_where_the_water_runs_over_gravel() {
    let at = |depth, fall, laid| {
        let place = Edge {
            laid,
            ..edge(depth, fall, WET_AND_LEVEL, OPEN)
        };
        suits(Margin::Crowfoot, &place, false)
    };
    assert!(at(0.35, 0.01, GRAVEL) > 0.99);
    assert!(at(0.35, 0.01, SILT) < 0.5 * at(0.35, 0.01, GRAVEL));
    assert!(at(0.35, STILL, GRAVEL) <= 0.0, "in a still pool");
    assert!(at(0.35, 0.08, GRAVEL) <= 0.0, "in a race");
    assert!(at(-0.1, 0.01, GRAVEL) <= 0.0, "on the bank");
    assert!(at(1.5, 0.01, GRAVEL) <= 0.0, "too deep to root in");
    assert!(at(0.35, 0.01, -1.0) < 0.15, "on bare rock");
}

/// Nothing roots on ground its floods scour bare, however wet and level,
/// and the still-water plants root thicker in silt than over gravel.
#[test]
fn reeds_keep_off_scoured_ground_and_favour_silt() {
    let scoured = Edge {
        green: 0.0,
        ..edge(-0.1, STILL, WET_AND_LEVEL, OPEN)
    };
    let grown = edge(-0.1, STILL, WET_AND_LEVEL, OPEN);
    for margin in [Margin::Reed, Margin::Reedmace] {
        assert!(suits(margin, &grown, false) > 0.0, "{margin:?}");
        assert!(suits(margin, &scoured, false) <= 0.0, "{margin:?}");
    }
    for margin in [
        Margin::Reed,
        Margin::Reedmace,
        Margin::Pondweed,
        Margin::Lily,
    ] {
        let depth = if matches!(margin, Margin::Lily | Margin::Pondweed) {
            1.0
        } else {
            0.2
        };
        let silted = edge(depth, STILL, WET_AND_LEVEL, OPEN);
        let gravelled = Edge {
            laid: GRAVEL,
            ..silted
        };
        assert!(
            suits(margin, &gravelled, false) < 0.6 * suits(margin, &silted, false),
            "{margin:?}"
        );
    }
}

#[test]
fn the_near_lattice_fills_the_far_ones_hole_on_the_lands_own_grid() {
    for eye in [(0.0, 0.0), (12.3, -40.7), (-3001.9, 777.77)] {
        let margins = Margins {
            sown: [None; KINDS.len()],
            lake: None,
            seed: 1,
            eye,
            reach: 250.0,
            most: 10,
            found: Vec::new(),
            pass: Pass::Reading { index: 0, row: 0 },
        };
        let (near, far) = (
            margins.lattice(0).expect("a near lattice"),
            margins.lattice(1).expect("a far lattice"),
        );
        assert!(margins.lattice(2).is_none());
        let on_grid = |value: f64| {
            let cells = value / FAR_CELL;
            (cells - mathf::round(cells)).abs() < 1e-9
        };
        for corner in [near.corner, far.corner] {
            assert!(
                on_grid(corner.0) && on_grid(corner.1),
                "{eye:?}: {corner:?}"
            );
        }
        let across = real(near.side) * near.cell;
        assert_eq!(
            far.hole,
            Some((
                near.corner,
                (near.corner.0 + across, near.corner.1 + across)
            )),
            "{eye:?}"
        );
        assert!(near.hole.is_none());
        // The near lattice stands about the eye, the far one out past the
        // reach.
        let (middle, span) = (0.5 * across, real(far.side) * far.cell);
        assert!(
            (near.corner.0 + middle - eye.0).abs() <= FAR_CELL
                && (near.corner.1 + middle - eye.1).abs() <= FAR_CELL
        );
        assert!(far.corner.0 <= eye.0 - 250.0 && far.corner.0 + span >= eye.0 + 250.0);
        assert!(far.corner.1 <= eye.1 - 250.0 && far.corner.1 + span >= eye.1 + 250.0);
    }
}

/// The water's-edge patches `composition` set out: each one's plant, where
/// its middle stands, and whether it is a far bed.
fn patches(composition: &Composition) -> Vec<(Margin, Vec3, bool)> {
    let stage = &composition.stage;
    stage
        .objects
        .iter()
        .filter_map(|object| match object.shape {
            Shape::Instance {
                prototype, pose, ..
            } => match stage.recipes.get(prototype as usize) {
                Some(&Recipe::Margin { margin, side, .. }) => {
                    Some((margin, pose.at, side > NEAR_CELL))
                }
                _ => None,
            },
            _ => None,
        })
        .collect()
}

/// A plant's patch is planned as a prototype only once one is set out, and
/// once for all its patches; past the most the stage plans, a patch never
/// planned is left out rather than the scene refused, and one planned still
/// stands.
#[test]
fn a_patch_is_planned_once_when_first_set_out_and_left_out_past_the_most() {
    let mut stage = Stage::new(Detail::Simple.densities()).expect("a stage");
    let mut dice = Dice::keyed(7, 0);
    margins(&mut stage, &mut dice, ((0.0, 0.0), Season::Summer, None)).expect("margins");
    assert!(
        stage.recipes.is_empty(),
        "nothing planned before a patch is set out"
    );
    let mut margins = stage.margins.take().expect("margins");
    let at = |patch: u8, x: f64| Placed {
        kind: 0,
        patch,
        base: Vec3::new(x, 0.0, 0.0),
        turn: 0.0,
        key: 0,
    };
    margins.found = alloc::vec![at(2, 3.0), at(2, 4.0)];
    margins.place(&mut stage, 0).expect("set out");
    assert_eq!(stage.recipes.len(), 1);
    assert_eq!(stage.objects.len(), 2);
    let mut filler = margins.sown[0].expect("summer reeds");
    while stage.plans_more() {
        filler.plan(&mut stage, 0).expect("planned");
    }
    margins.found = alloc::vec![at(3, 5.0), at(2, 6.0)];
    margins.place(&mut stage, 0).expect("set out");
    assert_eq!(stage.objects.len(), 3);
    let placed: Vec<u32> = stage
        .objects
        .iter()
        .filter_map(|object| match object.shape {
            Shape::Instance { prototype, .. } => Some(prototype),
            _ => None,
        })
        .collect();
    assert_eq!(placed, [0, 0, 0]);
}

/// A still lake's edge plans no crowfoot, which only grows where water
/// runs, and every plant's patch it does plan stands somewhere.
#[test]
fn a_water_plans_only_the_patches_it_sets_out() {
    for seed in 0..2 {
        let mut composition =
            Composition::new(Setting::Alpine, seed, (320, 180), Detail::Simple).expect("composes");
        composition.run_until_seen().expect("a lake lies in a land");
        let stage = &composition.stage;
        let mut used = alloc::vec![false; stage.recipes.len()];
        for object in &stage.objects {
            if let Shape::Instance { prototype, .. } = object.shape {
                used[prototype as usize] = true;
            }
        }
        for (index, recipe) in stage.recipes.iter().enumerate() {
            if let Recipe::Margin { margin, .. } = recipe {
                assert!(
                    *margin != Margin::Crowfoot,
                    "{seed}: crowfoot on a still lake"
                );
                assert!(
                    used[index],
                    "{seed}: {margin:?}'s patch {index} planned for nothing"
                );
            }
        }
    }
}

#[test]
fn a_lake_sets_reeds_in_its_shallows_and_floats_lilies_on_it_in_the_light() {
    let mut set = [0usize; KINDS.len()];
    for seed in 0..3 {
        let mut composition =
            Composition::new(Setting::Alpine, seed, (320, 180), Detail::Simple).expect("composes");
        let land = composition.run_until_seen().expect("a lake lies in a land");
        let lake = land.sea.expect("the land's sea is its lake");
        let stage = &composition.stage;
        for (margin, at, far) in patches(&composition) {
            if let Some(slot) = KINDS.iter().position(|&kind| kind == margin) {
                set[slot] += 1;
            }
            let ground = land.height(&stage.fields, at.x, at.z);
            let level = land.water_level(&stage.fields, at.x, at.z).unwrap_or(lake);
            let floating = matches!(margin, Margin::Lily | Margin::Pondweed);
            let (stands, (shallowest, deepest)) = if floating {
                (level, (0.2, 2.6))
            } else {
                (ground, (-0.45, 1.0))
            };
            assert_eq!(
                at.y.to_bits(),
                stands.to_bits(),
                "{seed}: {margin:?} at {at:?}"
            );
            let depth = level - ground;
            assert!(
                depth > shallowest && depth < deepest,
                "{seed}: {margin:?} {depth} deep, far {far}"
            );
            let hidden = stage
                .shades
                .as_ref()
                .map_or(0.0, |shades| shades.at(at.x, at.z).1);
            assert!(
                hidden < 0.75,
                "{seed}: {margin:?} beneath {hidden} of hidden sky"
            );
        }
    }
    for (&kind, &count) in KINDS.iter().zip(&set) {
        if kind == Margin::Crowfoot {
            assert_eq!(count, 0, "crowfoot, which wants running water, on a lake");
        } else {
            assert!(count > 10, "{kind:?} too seldom: {set:?}");
        }
    }
}

/// A stream's floating plants float on its water as its flow has shaped
/// it: each about the eye stands on the finer water grid's own surface.
#[test]
fn a_streams_floating_plants_float_on_its_shaped_water() {
    let mut floating = 0;
    for seed in 0..3 {
        let mut composition =
            Composition::new(Setting::Stream, seed, (320, 180), Detail::Simple).expect("composes");
        let land = composition
            .run_until_seen()
            .expect("a stream lies in a land");
        let near = land.near_water.expect("a finer water grid");
        let finer = &composition.stage.fields[near.field as usize];
        for (margin, at, _) in patches(&composition) {
            let within = (at.x - near.centre.0).abs() < near.reach
                && (at.z - near.centre.1).abs() < near.reach;
            if !within || !matches!(margin, Margin::Lily | Margin::Pondweed | Margin::Crowfoot) {
                continue;
            }
            assert_eq!(
                at.y.to_bits(),
                finer.height_at(at.x, at.z).to_bits(),
                "{seed}: {margin:?} at {at:?}"
            );
            floating += 1;
        }
    }
    assert!(floating > 0, "plants float in the streams about the eye");
}

/// The summer water's edge about `eye` on a bare stage, its pass and its
/// patches to be set by hand.
fn summer_margins(eye: (f64, f64)) -> (Stage, Margins) {
    let mut stage = Stage::new(Detail::Simple.densities()).expect("a stage");
    let mut dice = Dice::keyed(7, 0);
    margins(&mut stage, &mut dice, (eye, Season::Summer, None)).expect("margins");
    let margins = stage.margins.take().expect("margins");
    (stage, margins)
}

/// A reed patch, `patch` among its clumps and beds, standing at `(x, z)`.
fn reeds(patch: usize, x: f64, z: f64) -> Placed {
    Placed {
        kind: 0,
        patch: u8::try_from(patch).expect("a patch"),
        base: Vec3::new(x, 0.0, z),
        turn: 0.0,
        key: 0,
    }
}

/// Where each patch `margins` kept stands.
fn kept(margins: &Margins) -> Vec<(f64, f64)> {
    margins
        .found
        .iter()
        .map(|placed| (placed.base.x, placed.base.z))
        .collect()
}

#[test]
fn the_patches_kept_are_the_nearest_the_eye_in_order() {
    let (_, mut margins) = summer_margins((10.0, 10.0));
    margins.most = 3;
    margins.found = alloc::vec![
        reeds(0, 50.0, 10.0),
        reeds(0, 10.0, 12.0),
        reeds(0, -80.0, 0.0),
        reeds(0, 10.0, 8.0),
        reeds(0, 13.0, 10.0)
    ];
    margins.pass = Pass::Keeping;
    margins.keep();
    // Two equally near are kept in one order whichever was found first.
    assert_eq!(kept(&margins), [(10.0, 8.0), (10.0, 12.0), (13.0, 10.0)]);
    assert_eq!(margins.pass, Pass::Placing { next: 0 });
}

/// A patch whose square reaches a piece — a boulder, a trunk — is not kept,
/// out to its corners and as far as a bed is broad; ground kept open, a
/// pond or the eye's own, is no bar to it.
#[test]
fn a_patch_is_never_set_through_a_piece_though_open_ground_is_no_bar() {
    let (mut stage, margins) = summer_margins((0.0, 0.0));
    stage.claim((10.0, 0.0), 1.0).expect("a boulder");
    stage.keep_open((30.0, 0.0), 5.0).expect("a pond");
    let corner = 1.0 + GAP + FRAC_1_SQRT_2 * NEAR_CELL;
    let found = [
        reeds(0, 10.0 + corner - 0.01, 0.0),
        reeds(0, 10.0 + corner + 0.01, 0.0),
        reeds(0, 30.0, 0.0),
        reeds(0, 10.0, 3.5),
        reeds(CLUMPS, 10.0, -3.5),
    ];
    let clear: Vec<(f64, f64)> = found
        .iter()
        .filter(|placed| {
            let patch = (usize::from(placed.kind), usize::from(placed.patch));
            margins.clears_pieces(&stage, patch, (placed.base.x, placed.base.z))
        })
        .map(|placed| (placed.base.x, placed.base.z))
        .collect();
    assert_eq!(
        clear,
        [(10.0 + corner + 0.01, 0.0), (30.0, 0.0), (10.0, 3.5)]
    );
}

/// Every plant a scene's water's edge sets out stands clear of the pieces
/// already standing there: the boulders and drift along a stream's banks,
/// the trees, the eye's own ground aside.
#[test]
fn no_scene_sets_its_waters_edge_through_a_piece() {
    let mut checked = 0;
    for (setting, seed) in [
        (Setting::Stream, 0),
        (Setting::Stream, 1),
        (Setting::Stream, 2),
        (Setting::Winter, 1),
    ] {
        let mut composition =
            Composition::new(setting, seed, (320, 180), Detail::Simple).expect("composes");
        composition.run_until_seen();
        for (margin, at, far) in patches(&composition) {
            let side = if far { FAR_CELL } else { NEAR_CELL };
            assert!(
                composition
                    .stage
                    .clear_of_pieces((at.x, at.z), FRAC_1_SQRT_2 * side),
                "{setting:?} {seed}: {margin:?} at {at:?}"
            );
            checked += 1;
        }
    }
    assert!(checked > 500, "{checked}");
}

#[test]
fn a_scene_short_of_patches_keeps_those_nearest_the_eye() {
    static FEW: OnceLock<Densities> = OnceLock::new();
    let composed = |densities: Option<&'static Densities>| {
        let mut composition =
            Composition::new(Setting::Winter, 1, (320, 180), Detail::Simple).expect("composes");
        if let Some(densities) = densities {
            composition.stage.densities = densities;
        }
        composition.run_until_seen();
        let eye = composition.seen.as_ref().expect("seen").1.eye();
        let mut apart: Vec<f64> = patches(&composition)
            .iter()
            .map(|&(_, at, _)| mathf::hypot(at.x - eye.x, at.z - eye.z))
            .collect();
        apart.sort_by(f64::total_cmp);
        apart
    };
    let all = composed(None);
    let few = composed(Some(FEW.get_or_init(|| {
        let mut few = Detail::Simple.densities().clone();
        few.waterside.most = 25;
        few
    })));
    assert!(all.len() > 100, "{}", all.len());
    assert_eq!(few.len(), 25);
    assert_eq!(few.as_slice(), all.get(..25).expect("more than kept"));
}
