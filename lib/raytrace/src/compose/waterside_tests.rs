//! Host tests of the water's edge set out about the eye: each plant only
//! where the water, its bank and the light suit it, the lattices tiling the
//! ground about the eye without a gap, and no more patches than a scene's
//! detail allows.

extern crate std;

use alloc::vec::Vec;
use std::sync::OnceLock;

use super::*;
use crate::compose::{Composition, Setting};
use crate::detail::{Densities, Detail};

/// A place `depth` deep beneath a surface falling `fall`, on ground as wet
/// and as upright as `ground` holds, under `lit` of the sky.
fn edge(depth: f64, fall: f64, (wet, upright): (f64, f64), lit: f64) -> Edge {
    Edge {
        ground: 0.0,
        level: depth,
        depth,
        fall,
        wet,
        upright,
        way: 0.0,
        lit,
    }
}

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
        let under = |lit| suits(margin, &edge(depth, STILL, WET_AND_LEVEL, lit), false);
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
}

#[test]
fn the_near_lattice_fills_the_far_ones_hole_on_the_lands_own_grid() {
    for eye in [(0.0, 0.0), (12.3, -40.7), (-3001.9, 777.77)] {
        let margins = Margins {
            grown: [None; 4],
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

#[test]
fn a_lake_sets_reeds_in_its_shallows_and_floats_lilies_on_it_in_the_light() {
    let mut set = [0usize; 4];
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
    assert!(
        set.iter().all(|&count| count > 10),
        "every plant somewhere: {set:?}"
    );
}

#[test]
fn the_patches_kept_are_the_nearest_the_eye_in_order() {
    let at = |x: f64, z: f64| Placed {
        prototype: 0,
        material: 0,
        base: Vec3::new(x, 0.0, z),
        turn: 0.0,
        key: 0,
    };
    let mut margins = Margins {
        grown: [None; 4],
        lake: None,
        seed: 1,
        eye: (10.0, 10.0),
        reach: 250.0,
        most: 3,
        found: alloc::vec![
            at(50.0, 10.0),
            at(10.0, 12.0),
            at(-80.0, 0.0),
            at(10.0, 8.0),
            at(13.0, 10.0)
        ],
        pass: Pass::Keeping,
    };
    margins.keep();
    let kept: Vec<(f64, f64)> = margins
        .found
        .iter()
        .map(|placed| (placed.base.x, placed.base.z))
        .collect();
    // Two equally near are kept in one order whichever was found first.
    assert_eq!(kept, [(10.0, 8.0), (10.0, 12.0), (13.0, 10.0)]);
    assert_eq!(margins.pass, Pass::Placing { next: 0 });
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
