//! Host tests of tree growth: a tree reaches the height it is grown to, its
//! leaves come and go with the seasons, and a seed grows the same tree.

use super::*;
use crate::prototype::{point, Part};

const STOCK: Stock = Stock {
    bark: 0,
    leaves: 1,
    grain: Grain {
        wood: 2,
        rot: 3,
        edge: 4,
    },
};

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
        snapped: false,
    }
}

fn grown(season: Season, seed: u64) -> Prototype {
    grown_as(&species(), season, seed)
}

fn grown_as(species: &Species, season: Season, seed: u64) -> Prototype {
    let mut growth = Growth::new(species, 12.0, (season, STOCK), seed).expect("grows");
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
            Part::Facet(_) | Part::Solid(_) => (tubes, leaves),
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
    let grain = [STOCK.grain.wood, STOCK.grain.rot, STOCK.grain.edge];
    assert!(tree.parts().iter().all(|part| match part {
        Part::Tube(tube) => tube.material == STOCK.bark,
        Part::Leaf(leaf) => leaf.material == STOCK.leaves,
        // The breaks its stubs snapped in.
        Part::Facet(facet) => facet
            .material
            .is_some_and(|material| grain.contains(&material)),
        Part::Solid(_) => false,
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
fn a_palm_grows_to_its_height() {
    let palm = palm(12.0, (STOCK, 0), 14, 4).expect("a palm").whole();
    // Its crown rises above the trunk no more than a frond is long.
    let top = palm.bounds().max.y;
    assert!(top > 10.0 && top < 12.0 * 1.38, "{top}");
    assert!(counts(&palm).1 > 1000, "fronds of leaflets");
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
        from: 0.0,
        emerges: 0.0,
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
fn a_trunk_holds_its_girth_up_its_bole_and_narrows_in_its_crown() {
    let (radius, length) = (0.4, 20.0);
    let trunk = bole(radius, length, false);
    let at = |metres: f64| radius_at(trunk, &TRUNK, metres / length);
    let breast = at(1.3);
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
        at(10.0) > 1.15 * radius_at(trunk, &cone, 0.5),
        "fuller than a cone halfway up"
    );
    // The arms of a fork carry on the trunk's girth.
    let arm = bole(radius, length, true);
    assert!((radius_at(arm, &TRUNK, 0.0) - radius).abs() < 1e-12);
}

/// The tubes each of `tree`'s roots is made of, by the key they share: every
/// limb low at the foot but the flared trunk itself.
fn roots_of(tree: &Prototype, radius: f64) -> Vec<Vec<Tube>> {
    let mut roots: Vec<Vec<Tube>> = Vec::new();
    for part in tree.parts() {
        let Part::Tube(tube) = part else { continue };
        if tube.flare.is_some() || f64::from(tube.a[1]) > 0.6 * radius {
            continue;
        }
        match roots
            .iter_mut()
            .find(|root| root.first().is_some_and(|first| first.key == tube.key))
        {
            Some(root) => root.push(*tube),
            None => roots.push(alloc::vec![*tube]),
        }
    }
    roots
}

#[test]
fn a_trees_foot_swells_out_toward_each_root_and_each_runs_out_of_its_lobe() {
    let species = species();
    let radius = 12.0 * species.girth;
    for seed in 0..4 {
        let tree = grown(Season::Winter, seed);
        let foot = tree
            .parts()
            .iter()
            .find_map(|part| match part {
                Part::Tube(tube) if tube.flare.is_some() => Some(*tube),
                _ => None,
            })
            .expect("a flared foot");
        let flare = tree.flare_of(&foot).expect("its flare");
        let ground = -f64::from(foot.a[1]);
        let roots = roots_of(&tree, radius);
        assert!(
            (5..=8).contains(&roots.len()),
            "{seed}: {} roots",
            roots.len()
        );
        for root in &roots {
            let first = root.first().expect("a root");
            let start = point(first.a);
            let out = Vec3::new(start.x, 0.0, start.z);
            let angle = foot.angle_of(out.normalized());
            // Toward its root the foot swells well beyond its swell between.
            let toward = flare.factor(ground, angle);
            let between = (0..36u32)
                .map(|step| flare.factor(ground, core::f64::consts::TAU * f64::from(step) / 36.0))
                .fold(f64::INFINITY, f64::min);
            assert!(
                toward > between + 0.25 * species.flare,
                "{seed}: {toward} against {between}"
            );
            // It leaves from within its lobe, its back above the ground there,
            let up = start.y + ground;
            let lobe = f64::from(foot.radii[0]) * flare.factor(up, angle);
            assert!(
                out.length() < lobe,
                "{seed}: starts {} out, its lobe {lobe}",
                out.length()
            );
            assert!(start.y + f64::from(first.radii[0]) > 0.2 * f64::from(first.radii[0]));
            // and every end it runs out to is buried deeper than it is thick.
            let deepest = root
                .iter()
                .map(|tube| f64::from(tube.b[1]) + f64::from(tube.radii[1]))
                .fold(f64::INFINITY, f64::min);
            assert!(deepest < 0.0, "{seed}: a root's end shows: {deepest}");
            let reach = root
                .iter()
                .map(|tube| f64::from(tube.b[0]).hypot(f64::from(tube.b[2])))
                .fold(0.0, f64::max);
            assert!(
                (1.5 * radius..5.5 * radius).contains(&reach),
                "{seed}: reaches {reach}"
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

#[test]
fn a_limb_narrows_by_its_levels_taper_and_a_forks_arm_carries_its_stems_on() {
    let limb = Stem {
        level: 1,
        crown: 1.0,
        ..bole(0.05, 3.0, false)
    };
    let level = Level {
        taper: 0.6,
        form: 0.8,
        ..TRUNK
    };
    // A limb holds no bole and no crown apart: its tip is what its level's
    // taper and form leave of its girth, not a twentyfifth of it.
    let tip = radius_at(limb, &level, 1.0);
    let wanted = 0.05 * mathf::exp(0.8 * mathf::ln(0.4));
    assert!((tip - wanted).abs() < 1e-12, "{tip} against {wanted}");
    // A trunk forked a third of the way up its bole: what grows on is its
    // own narrowing carried on, at whatever girth the fork leaves it.
    let (radius, length) = (0.4, 20.0);
    let trunk = bole(radius, length, false);
    let forked_at = 0.3;
    let rest = Stem {
        length: length * (1.0 - forked_at),
        radius: radius_at(trunk, &TRUNK, forked_at),
        forked: true,
        from: forked_at,
        ..trunk
    };
    for step in 0..=20u32 {
        let s = f64::from(step) / 20.0;
        let whole = forked_at + s * (1.0 - forked_at);
        let (arm, stem) = (radius_at(rest, &TRUNK, s), radius_at(trunk, &TRUNK, whole));
        assert!((arm - stem).abs() < 1e-12, "{s}: {arm} against {stem}");
    }
}

/// Every limb of `tree` that ends free, rather than running on into another
/// or swelling into its foot: where it ends, how thick, and whether it ends
/// open, for a break to close.
fn free_ends(tree: &Prototype) -> Vec<(Vec3, f64, bool)> {
    let tubes: Vec<&Tube> = tree
        .parts()
        .iter()
        .filter_map(|part| match part {
            Part::Tube(tube) => Some(tube),
            _ => None,
        })
        .collect();
    tubes
        .iter()
        .filter(|tube| {
            let end = point(tube.b);
            !tubes.iter().any(|other| {
                (point(other.a) - end).length() < 0.5 * f64::from(tube.radii[1]).max(1e-4)
            })
        })
        .map(|tube| (point(tube.b), f64::from(tube.radii[1]), tube.open[1]))
        .collect()
}

#[test]
fn a_bare_boles_stubs_end_torn_never_in_a_ball() {
    for seed in 0..4 {
        let tree = grown(Season::Winter, seed);
        let crown = species().base * 12.0;
        let stubs: Vec<_> = free_ends(&tree)
            .into_iter()
            .filter(|&(end, thick, _)| end.y > 0.15 * crown && end.y < crown && thick > 0.008)
            .collect();
        assert!(stubs.len() >= 3, "{seed}: {} stubs", stubs.len());
        for (end, _, open) in stubs {
            assert!(open, "{seed}: a stub at {end:?} ends rounded");
        }
        let torn = tree
            .parts()
            .iter()
            .filter(|part| matches!(part, Part::Facet(facet) if facet.material == Some(STOCK.grain.wood)))
            .count();
        assert!(torn > 30, "{seed}: {torn} faces of torn wood");
    }
}

#[test]
fn a_snag_stands_snapped_off_thick_its_limbs_broken() {
    let mut snag = Species {
        snapped: true,
        depth: 2,
        ..species()
    };
    if let Some(trunk) = snag.levels.get_mut(0) {
        trunk.taper *= 0.5;
    }
    if let Some(limbs) = snag.levels.get_mut(1) {
        limbs.length.0 *= 0.35;
        limbs.taper *= 0.35;
    }
    for seed in 0..4 {
        let tree = grown_as(&snag, Season::Winter, seed);
        let ends = free_ends(&tree);
        // Its trunk's is the thickest end it has.
        let top = ends
            .iter()
            .copied()
            .fold((Vec3::ZERO, 0.0, false), |best, end| {
                if end.1 > best.1 {
                    end
                } else {
                    best
                }
            });
        // Broken where its trunk was still a good part of its girth.
        let radius = 12.0 * snag.girth;
        assert!(
            top.1 > 0.35 * radius,
            "{seed}: snapped at {} of {radius}",
            top.1
        );
        // Nothing above the ground ends rounded; its roots end buried.
        for (end, thick, open) in ends.into_iter().filter(|&(end, thick, _)| end.y > thick) {
            assert!(
                open || thick < 0.004,
                "{seed}: a limb at {end:?} ends rounded"
            );
        }
    }
}
