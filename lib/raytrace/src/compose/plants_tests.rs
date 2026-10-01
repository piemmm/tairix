extern crate std;

use super::*;
use crate::prototype::Prototype;
use crate::tree::Growth;

const KINDS: [Kind; 13] = [
    Kind::Oak,
    Kind::Maple,
    Kind::Birch,
    Kind::Beech,
    Kind::Willow,
    Kind::Poplar,
    Kind::Pine,
    Kind::Spruce,
    Kind::Olive,
    Kind::Cherry,
    Kind::Hazel,
    Kind::Box,
    Kind::Heather,
];

/// `kind` grown as `stand` has it from `seed`, and the height it was asked
/// to grow to.
fn grown(kind: Kind, stand: Stand, seed: u64) -> (Prototype, f64) {
    let species = stood(kind, stand);
    let height = f64::midpoint(species.height.0, species.height.1);
    let stock = Stock { bark: 0, leaves: 1 };
    let mut growth = Growth::new(&species, height, (Season::Summer, stock), seed).expect("grows");
    while !growth.step().expect("grows") {}
    (growth.finish().expect("a tree"), height)
}

/// How far a grown tree's crown spreads from its trunk: half its mean
/// breadth.
fn spread(tree: &Prototype) -> f64 {
    let bounds = tree.bounds();
    0.25 * ((bounds.max.x - bounds.min.x) + (bounds.max.z - bounds.min.z))
}

/// How far a grown tree's crown spreads from its trunk, its roots left out:
/// half the mean breadth of what stands above the ground about its foot.
fn crown_spread(tree: &Prototype, height: f64) -> f64 {
    let above = 0.05 * height;
    let (mut least, mut most) = ([f64::INFINITY; 2], [f64::NEG_INFINITY; 2]);
    let mut reach = |[x, y, z]: [f32; 3], pad: f32| {
        if f64::from(y) > above {
            let (x, z, pad) = (f64::from(x), f64::from(z), f64::from(pad));
            least = [least[0].min(x - pad), least[1].min(z - pad)];
            most = [most[0].max(x + pad), most[1].max(z + pad)];
        }
    };
    for part in tree.parts() {
        match part {
            crate::prototype::Part::Tube(tube) => {
                reach(tube.a, tube.radii[0]);
                reach(tube.b, tube.radii[1]);
            }
            crate::prototype::Part::Leaf(leaf) => reach(leaf.base, leaf.length),
            crate::prototype::Part::Facet(_) => {}
        }
    }
    0.25 * ((most[0] - least[0]) + (most[1] - least[1]))
}

/// Every kind grows to about the height asked of it and no broader than a
/// tree of that height spreads, whether it stood in the open, close among
/// others, or young beneath them: a tree grown from a frame gone awry would
/// reach past the sky, and every ray would pass through it.
#[test]
fn every_kind_grows_a_sound_tree_however_it_stood() {
    for kind in KINDS {
        for stand in [Stand::Open, Stand::Close, Stand::Young, Stand::Dead] {
            for seed in 0..3u64 {
                let (tree, height) = grown(kind, stand, seed);
                let bounds = tree.bounds();
                assert!(
                    bounds.min.is_finite() && bounds.max.is_finite(),
                    "{kind:?} {stand:?} {seed}"
                );
                assert!(
                    (0.85 * height..1.35 * height).contains(&bounds.max.y),
                    "{kind:?} {stand:?} {seed}: {} tall of {height}",
                    bounds.max.y
                );
                assert!(
                    spread(&tree) < 0.8 * height,
                    "{kind:?} {stand:?} {seed}: spreads {}",
                    spread(&tree)
                );
            }
        }
    }
}

/// A kind's crown reach, which its trees are spaced by and their shade cast
/// with, is what its grown trees spread.
#[test]
fn a_kinds_crown_reaches_as_far_as_its_grown_trees_spread() {
    for kind in KINDS {
        for (stand, within) in [
            (Stand::Open, 0.2),
            (Stand::Close, 0.2),
            (Stand::Young, 0.35),
            (Stand::Dead, 0.25),
        ] {
            let seeds = 4u64;
            let measured = (0..seeds)
                .map(|seed| {
                    let (tree, height) = grown(kind, stand, seed);
                    crown_spread(&tree, height) / height
                })
                .sum::<f64>()
                / 4.0;
            let table = kind.crown(stand);
            assert!(
                (measured / table - 1.0).abs() < within,
                "{kind:?} {stand:?}: {measured} spread, {table} reckoned"
            );
        }
    }
}
