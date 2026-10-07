use alloc::vec::Vec;

use super::*;
use crate::compose::{Composition, Setting};
use crate::detail::Detail;
use crate::prototype::Part;

const MATERIALS: Materials = Materials {
    leaf: 1,
    stalk: 2,
    sheath: 3,
    tassel: 4,
    husk: 5,
    silk: 6,
    root: 7,
};

/// The leaves of a plant built of `building`, lowest first: each leaf's key
/// and the sum of its points, whose level part runs the way it heads out
/// from the stalk.
fn leaves(building: &Building) -> Vec<(u32, Vec3)> {
    let mut leaves: Vec<(u32, Vec3, u32)> = Vec::new();
    for part in building.parts() {
        let Part::Facet(facet) = part else {
            continue;
        };
        if facet.material != Some(MATERIALS.leaf) {
            continue;
        }
        let at = building.vertex(facet.corners[2]).expect("a corner");
        match leaves.iter_mut().find(|(key, ..)| *key == facet.key) {
            Some((_, sum, count)) => {
                *sum += at;
                *count += 1;
            }
            None => leaves.push((facet.key, at, 1)),
        }
    }
    let low = |&(_, sum, count): &(u32, Vec3, u32)| sum.y / f64::from(count);
    leaves.sort_by(|a, b| low(a).total_cmp(&low(b)));
    leaves
        .into_iter()
        .map(|(key, sum, _)| (key, Vec3::new(sum.x, 0.0, sum.z)))
        .collect()
}

/// A green plant stands as tall as maize does, its leaves alternating either
/// side of its stalk and reaching no further than a stand allows for.
#[test]
fn a_maize_plant_stands_tall_its_leaves_alternating() {
    for seed in 0..4 {
        let mut dice = Dice::keyed(seed, 0);
        let (building, top, reach) = plant(&mut dice, &GREEN, &MATERIALS).expect("a plant");
        assert!(top > 1.9 && top < 2.6, "{seed}: it stands {top} tall");
        assert!(
            reach > 0.4 && reach < 1.2,
            "{seed}: its leaves reach {reach}"
        );
        let bounds = building.bounds();
        assert!(
            bounds.max.y <= top + 1e-3 && bounds.min.y > -0.06,
            "{seed}: {bounds:?}"
        );
        let leaves = leaves(&building);
        let count = u32::try_from(leaves.len()).expect("a count");
        assert!(
            (GREEN.leaves.0..=GREEN.leaves.1).contains(&count),
            "{seed}: {count} leaves"
        );
        // Leaves alternate: one leaf's way out runs against the next's.
        let alternating = leaves
            .windows(2)
            .filter(|pair| pair[0].1.dot(pair[1].1) < 0.0)
            .count();
        assert!(
            alternating + 2 >= leaves.len(),
            "{seed}: {alternating} of {} alternate",
            leaves.len()
        );
    }
}

/// A ripe plant's leaves have all dried back from their tips; a green one's
/// upper leaves are green to theirs.
#[test]
fn a_ripe_plants_leaves_have_dried() {
    let leaf = |form: &Form, seed: u64| {
        let mut dice = Dice::keyed(seed, 0);
        let (building, ..) = plant(&mut dice, form, &MATERIALS).expect("a plant");
        let keys: Vec<u32> = leaves(&building).iter().map(|&(key, _)| key).collect();
        keys
    };
    let colours = Maize {
        greens: [Vec3::new(0.1, 0.3, 0.05); 2],
        midrib: Vec3::new(0.2, 0.35, 0.1),
        straw: Vec3::new(0.5, 0.42, 0.25),
        dead: Vec3::new(0.3, 0.2, 0.1),
    };
    let green = |key: u32| {
        let colour = colours.colour(&crate::pigment::Spot {
            mark: key,
            uv: (0.64, 0.4),
            girth: 0.8,
            width: 1e-3,
            front: true,
            ..crate::pigment::Spot::default()
        });
        colour.y / colour.x
    };
    for seed in 0..3 {
        let ripe = leaf(&RIPE, seed);
        assert!(
            ripe.iter().all(|&key| green(key) < 1.5),
            "{seed}: a ripe plant's leaf is green"
        );
        let summer = leaf(&GREEN, seed);
        assert!(
            summer.iter().rev().take(4).all(|&key| green(key) > 2.0),
            "{seed}: a green plant's top leaves are dry"
        );
    }
}

/// Summer maize about the eye stands as plants, and the eye is never put
/// among them, nor among a vineyard's vines.
#[test]
fn summer_maize_stands_about_the_eye_never_among_it() {
    let mut stood = 0;
    for seed in [6, 15, 26, 33, 47, 134] {
        let mut composition = Composition::new(Setting::Farmland, seed, (320, 180), Detail::Simple)
            .expect("composes");
        let land = composition.run_until_seen().expect("a land");
        let seen = composition.seen.as_ref().expect("seen").1.eye();
        let fields = &composition.stage.fields;
        for (dx, dz) in [(0.0, 0.0), (3.0, 0.0), (-3.0, 0.0), (0.0, 3.0), (0.0, -3.0)] {
            let grown = Grown::of(land.grids.grows(fields, seen.x + dx, seen.z + dz).0);
            assert!(
                !super::super::tall(grown),
                "{seed}: the eye stands among {grown:?}"
            );
        }
        stood += composition.stage.stands.len();
    }
    assert!(stood > 0, "no maize stood about the eye");
}
