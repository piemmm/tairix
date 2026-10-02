//! Host tests of tree growth: a tree reaches the height it is grown to, its
//! leaves come and go with the seasons, and a seed grows the same tree.

use super::*;
use crate::prototype::{point, Part};

const STOCK: Stock = Stock { bark: 0, leaves: 1 };

fn species() -> Species {
    let level = |branches: f64, length: f64, down: f64, segments: u32| Level {
        branches,
        length: (length, 0.1),
        taper: 0.9,
        form: 1.0,
        down: (down, 10.0),
        rotate: (140.0, 20.0),
        curve: (-10.0, 10.0, 30.0),
        segments,
        fork: (0.1, 25.0),
    };
    Species {
        envelope: Envelope::Spherical,
        height: (10.0, 14.0),
        base: 0.25,
        girth: 0.025,
        flare: 0.5,
        stubs: 1.5,
        ratio_power: 1.4,
        trunks: 1,
        levels: [
            level(0.0, 1.0, 0.0, 8),
            level(12.0, 0.5, 55.0, 6),
            level(10.0, 0.45, 45.0, 4),
            level(8.0, 0.35, 45.0, 2),
        ],
        depth: 4,
        attraction: (0.1, 0.0),
        leafing: Leafing {
            outline: Outline::Ovate { teeth: 6 },
            per_twig: 10,
            length: 0.1,
            breadth: 0.6,
            fold: 0.2,
            angle: 50.0,
            toward_light: 0.7,
        },
        evergreen: false,
    }
}

fn grown(season: Season, seed: u64) -> Prototype {
    let mut growth = Growth::new(&species(), 12.0, (season, STOCK), seed).expect("grows");
    let mut steps = 0;
    while !growth.step().expect("grows") {
        steps += 1;
        assert!(steps < 10_000, "growth ends");
    }
    growth.finish().expect("a tree")
}

fn counts(tree: &Prototype) -> (usize, usize) {
    tree.parts()
        .iter()
        .fold((0, 0), |(tubes, leaves), part| match part {
            Part::Tube(_) => (tubes + 1, leaves),
            Part::Leaf(_) => (tubes, leaves + 1),
            Part::Facet(_) => (tubes, leaves),
        })
}

#[test]
fn a_tree_grows_to_its_height_with_limbs_and_leaves_within_its_budget() {
    let tree = grown(Season::Summer, 3);
    let bounds = tree.bounds();
    assert!(bounds.max.y > 8.0 && bounds.max.y < 16.0, "{bounds:?}");
    assert!(
        bounds.max.x - bounds.min.x > 3.0,
        "a crown spreads: {bounds:?}"
    );
    let (tubes, leaves) = counts(&tree);
    assert!(
        tubes > 300 && leaves > 2000,
        "{tubes} limbs, {leaves} leaves"
    );
    assert!(tree.parts().len() <= MOST_PARTS);
    assert!(tree.parts().iter().all(|part| match part {
        Part::Tube(tube) => tube.material == STOCK.bark,
        Part::Leaf(leaf) => leaf.material == STOCK.leaves,
        Part::Facet(_) => false,
    }));
}

#[test]
fn a_deciduous_tree_loses_its_leaves_through_autumn_and_winter() {
    let (_, summer) = counts(&grown(Season::Summer, 5));
    let (_, autumn) = counts(&grown(Season::Autumn { fallen: 50 }, 5));
    let (tubes, winter) = counts(&grown(Season::Winter, 5));
    assert!(
        autumn < summer * 3 / 4 && autumn > summer / 4,
        "{summer} then {autumn}"
    );
    assert_eq!(winter, 0, "bare in winter");
    assert!(tubes > 300, "its limbs stay");
}

#[test]
fn a_seed_grows_the_same_tree_and_another_seed_another() {
    let describe = |tree: &Prototype| alloc::format!("{:?}", tree.parts());
    assert_eq!(
        describe(&grown(Season::Summer, 11)),
        describe(&grown(Season::Summer, 11))
    );
    assert_ne!(
        describe(&grown(Season::Summer, 11)),
        describe(&grown(Season::Summer, 12))
    );
}

#[test]
fn a_palm_and_a_saguaro_grow_to_their_heights() {
    let palm = palm(12.0, STOCK, 14, 4).expect("a palm").whole();
    // Its crown rises above the trunk no more than a frond is long.
    let top = palm.bounds().max.y;
    assert!(top > 10.0 && top < 12.0 * 1.38, "{top}");
    assert!(counts(&palm).1 > 1000, "fronds of leaflets");
    let cactus = saguaro(5.0, STOCK, 9).expect("a saguaro").whole();
    assert!(cactus.bounds().max.y > 4.0 && cactus.bounds().max.y < 6.0);
}

#[test]
fn a_fern_arches_its_fronds_out_from_the_ground_as_wide_as_it_is_tall() {
    for seed in 0..4u64 {
        let fern = fern(0.8, STOCK, 12, seed).expect("a fern").whole();
        let bounds = fern.bounds();
        assert!(
            bounds.min.y > -0.05,
            "{seed}: rooted at the ground, {:?}",
            bounds.min
        );
        assert!(
            (0.3..1.2).contains(&bounds.max.y),
            "{seed}: {} tall",
            bounds.max.y
        );
        let breadth = f64::midpoint(bounds.max.x - bounds.min.x, bounds.max.z - bounds.min.z);
        assert!((0.8..3.0).contains(&breadth), "{seed}: {breadth} across");
        let (rachises, pinnae) = counts(&fern);
        assert_eq!(
            rachises, 120,
            "{seed}: ten stretches of rachis to each of twelve fronds"
        );
        assert!(
            pinnae > 12 * 70,
            "{seed}: pinnae along all but the fronds' feet: {pinnae}"
        );
    }
}

/// A trunk grown as a trunk is: `radius` at its foot, `length` long.
fn bole(radius: f64, length: f64, forked: bool) -> Stem {
    Stem {
        level: 0,
        base: Vec3::ZERO,
        frame: Frame::WORLD,
        length,
        radius,
        travelled: 0.0,
        forked,
        crown: 0.5,
    }
}

const TRUNK: Level = Level {
    branches: 0.0,
    length: (1.0, 0.0),
    taper: 0.97,
    form: 0.6,
    down: (0.0, 0.0),
    rotate: (0.0, 0.0),
    curve: (0.0, 0.0, 10.0),
    segments: 10,
    fork: (0.0, 0.0),
};

#[test]
fn a_trunk_holds_its_girth_up_its_bole_swells_at_its_foot_and_narrows_in_its_crown() {
    let (radius, length) = (0.4, 20.0);
    let trunk = bole(radius, length, false);
    let at = |metres: f64| radius_at(trunk, &TRUNK, metres / length, 0.6);
    let breast = at(1.3);
    assert!(
        at(0.0) > 1.45 * breast,
        "a flare at the ground: {} against {breast}",
        at(0.0)
    );
    assert!(
        at(1.0) < 1.12 * breast,
        "gone within a metre or so: {}",
        at(1.0)
    );
    assert!(
        at(10.0) > 0.62 * breast,
        "a bole, not a spike: {} halfway up",
        at(10.0)
    );
    assert!(
        at(19.0) < 0.3 * breast,
        "narrowing in its crown: {}",
        at(19.0)
    );
    // Inside its crown the stem gives its girth to its limbs, narrowing
    // straight to its tip rather than holding it as its bole does.
    assert!(
        (at(15.0) / at(10.0) - 0.5).abs() < 0.02,
        "half its girth halfway from its crown's foot to its tip: {}",
        at(15.0) / at(10.0)
    );
    let cone = Level { form: 1.0, ..TRUNK };
    assert!(
        at(10.0) > 1.15 * radius_at(trunk, &cone, 0.5, 0.0),
        "fuller than a cone halfway up"
    );
    // The arms of a fork carry on the trunk's girth: they swell at no foot.
    let arm = bole(radius, length, true);
    assert!(
        (radius_at(arm, &TRUNK, 0.0, 0.6) - radius).abs() < 1e-12,
        "a fork's arm does not flare"
    );
}

#[test]
fn a_trees_foot_is_gripped_by_roots_spreading_along_the_ground() {
    let species = species();
    for seed in 0..4 {
        let tree = grown(Season::Winter, seed);
        let radius = 12.0 * species.girth;
        let roots: Vec<&Tube> = tree
            .parts()
            .iter()
            .filter_map(|part| match part {
                Part::Tube(tube)
                    if tube.b[1] < 0.0
                        && f64::from(tube.b[0]).hypot(f64::from(tube.b[2])) > 2.0 * radius =>
                {
                    Some(tube)
                }
                _ => None,
            })
            .collect();
        assert!(
            (5..=8).contains(&roots.len()),
            "{seed}: {} roots",
            roots.len()
        );
        for root in roots {
            let top = f64::from(root.a[1]) + f64::from(root.radii[0]);
            assert!(
                top > 0.2 * radius,
                "{seed}: a root shows above the ground: {root:?}"
            );
        }
    }
}

#[test]
fn a_bare_bole_carries_the_stubs_of_its_dead_branches() {
    let species = species();
    let (height, radius) = (12.0, 12.0 * species.girth);
    let crown = species.base * height;
    for seed in 0..4 {
        let tree = grown(Season::Winter, seed);
        let stubs = tree
            .parts()
            .iter()
            .filter(|part| match part {
                Part::Tube(tube) => {
                    let (a, b) = (point(tube.a), point(tube.b));
                    let out = Vec3::new(b.x - a.x, 0.0, b.z - a.z);
                    a.y > 0.15 * crown
                        && a.y < crown
                        && (b - a).length() < 1.2
                        && out.length() > 0.8 * (b - a).length()
                        && f64::from(tube.radii[0]) < 0.3 * radius
                }
                _ => false,
            })
            .count();
        assert!(
            stubs >= 3,
            "{seed}: {stubs} stubs up the bare bole below {crown}"
        );
    }
}
