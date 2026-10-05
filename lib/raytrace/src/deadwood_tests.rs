use super::*;
use alloc::vec::Vec;

use crate::prototype::{point, Blade, Facet, Prototype};
use crate::vector::Ray;

/// Bark, sound wood and rotten wood as materials 0, 1 and 2, the bark's cut
/// edge 9 and soil 10.
const WOODS: Woods = Woods {
    bark: 0,
    wood: 1,
    rot: 2,
    edge: 9,
    soil: 10,
};

/// Dead wood freshly fallen or cut.
const FRESH: Decay = Decay {
    age: 0.0,
    fungus: None,
};

/// Where a ray straight down from high over `(x, z)` meets `prototype`, and
/// the height it meets it at.
fn top_at(prototype: &Prototype, (x, z): (f64, f64)) -> Option<f64> {
    let down = Ray::new(Vec3::new(x, 50.0, z), -Vec3::UP);
    prototype
        .intersect(&down, (1e-9, 100.0), None)
        .map(|hit| 50.0 - hit.t)
}

#[test]
fn a_fallen_trunk_lies_along_the_ground_narrowing_toward_its_crown() {
    for seed in 0..6u64 {
        let (length, radius) = (9.0, 0.35);
        let log = log(length, radius, (WOODS, seed % 2 == 0), FRESH, seed)
            .expect("a log")
            .whole();
        let bounds = log.bounds();
        assert!(
            (0.9 * length..1.3 * length).contains(&bounds.max.z),
            "{seed}: {:?}",
            bounds.max
        );
        // Lying, not standing: its top about its thickness above the ground,
        // and lower toward where its crown was.
        let foot = top_at(&log, (0.0, 0.1 * length)).expect("met near its foot");
        let far = top_at(&log, (0.0, 0.7 * length));
        assert!(
            (1.2 * radius..2.2 * radius).contains(&foot),
            "{seed}: {foot}"
        );
        if let Some(far) = far {
            assert!(far < foot, "{seed}: narrows, {far} beyond {foot}");
        }
        assert!(
            bounds.max.y < 2.0 * length * 0.3 + 3.0 * radius,
            "{seed}: {:?}",
            bounds.max
        );
    }
}

#[test]
fn a_windthrown_trunk_carries_its_roots_and_a_snapped_one_ends_in_splinters() {
    let radius = 0.4;
    let thrown = log(8.0, radius, (WOODS, true), FRESH, 3)
        .expect("a log")
        .whole()
        .bounds();
    let snapped = log(8.0, radius, (WOODS, false), FRESH, 3)
        .expect("a log")
        .whole()
        .bounds();
    // The plate of roots spreads far wider than the trunk's own girth.
    assert!(thrown.max.x - thrown.min.x > 3.0 * radius, "{thrown:?}");
    assert!(
        thrown.max.y > 2.5 * radius,
        "the plate stands on edge: {thrown:?}"
    );
    // Splinters point back past its foot, and no further out than the trunk.
    assert!(snapped.min.z < -0.1 * radius, "{snapped:?}");
    assert!(
        snapped.max.y < 2.2 * radius + 8.0 * 0.4 * 2.0,
        "{snapped:?}"
    );
}

/// A trunk breaks torn, not rounded: looking back along it from beyond
/// either broken end a ray meets the torn wood of the break or the bark's
/// torn edge, never a rounded cap of bark, and its laths stand out past the
/// end the trunk itself reaches.
#[test]
fn a_broken_end_is_torn_wood_bristling_with_laths() {
    let (length, radius) = (6.0, 0.25);
    for seed in 0..4u64 {
        let log = log(length, radius, (WOODS, false), FRESH, seed)
            .expect("a log")
            .whole();
        // The trunk's segments come first, foot to crown.
        let segment = |index: usize| match log.parts().get(index) {
            Some(Part::Tube(tube)) => (point(tube.a), point(tube.b)),
            _ => panic!("{seed}: segment {index} is no tube"),
        };
        let ((foot, second), (before, tip)) = (segment(0), segment(LOG_SEGMENTS as usize - 1));
        for (end, out) in [
            (foot, (foot - second).normalized()),
            (tip, (tip - before).normalized()),
        ] {
            let hit = log
                .intersect(&Ray::new(end + out * 3.0, -out), (1e-9, 10.0), None)
                .expect("the break is met");
            assert!(
                matches!(hit.material, Some(1 | 9)),
                "{seed}: torn wood or bark's edge, not bark: {:?}",
                hit.material
            );
        }
        assert!(
            log.bounds().min.z < foot.z - 0.2 * radius,
            "{seed}: laths past its foot"
        );
    }
}

#[test]
fn every_limb_stub_and_torn_root_ends_in_a_break_not_a_ball() {
    for seed in 0..6u64 {
        let log = log(7.0, 0.3, (WOODS, seed % 2 == 0), FRESH, seed)
            .expect("a log")
            .whole();
        // Every limb of bark is open where it ends but those joined on along a
        // root or the trunk, and the hanging fine roots too thin to break.
        let limbs: Vec<&Tube> = log
            .parts()
            .iter()
            .filter_map(|part| match part {
                Part::Tube(tube) if tube.material == WOODS.bark => Some(tube),
                _ => None,
            })
            .collect();
        let ends = |tube: &Tube| point(tube.b);
        for tube in &limbs {
            let joined = limbs
                .iter()
                .any(|other| (point(other.a) - ends(tube)).length() < 1e-6);
            let fine = f64::from(tube.radii[1]) < 0.01;
            assert!(
                joined || fine || tube.open[1],
                "{seed}: a limb ends rounded at {:?}",
                ends(tube)
            );
        }
    }
}

#[test]
fn a_thrown_trunks_plate_is_soil_its_roots_torn_off_past_its_rim() {
    for seed in 0..4u64 {
        let radius = 0.35;
        let log = log(8.0, radius, (WOODS, true), FRESH, seed)
            .expect("a log")
            .whole();
        let soil = log
            .parts()
            .iter()
            .filter(|part| {
                matches!(
                    part,
                    Part::Facet(Facet {
                        material: Some(10),
                        ..
                    })
                )
            })
            .count();
        assert!(soil > 200, "{seed}: {soil} facets of soil");
        // Torn wood out round the plate, well beyond the trunk.
        let torn = log
            .parts()
            .iter()
            .filter_map(|part| match part {
                Part::Facet(facet) if facet.material == Some(1) => {
                    let corner = facet.corners.first().copied()?;
                    log.vertex(corner)
                }
                _ => None,
            })
            .filter(|point| point.x.hypot(point.y - 0.82 * radius) > 2.0 * radius && point.z < 0.5)
            .count();
        assert!(
            torn > 50,
            "{seed}: {torn} facets of roots torn off about the plate"
        );
    }
}

#[test]
fn a_stump_stands_on_its_roots_sawn_flat_or_snapped_in_splinters() {
    let (height, radius) = (0.8, 0.3);
    let sawn = stump(height, radius, (Top::Sawn, WOODS), (FRESH, None), 5)
        .expect("a stump")
        .whole();
    let face = top_at(&sawn, (0.0, 0.0)).expect("met at its middle");
    assert!((face - height).abs() < 0.08, "the sawn face: {face}");
    let snapped = stump(height, radius, (Top::Snapped, WOODS), (FRESH, None), 5)
        .expect("a stump")
        .whole();
    assert!(
        snapped.bounds().max.y > height + 0.2 * radius,
        "splinters above the break"
    );
    for stump in [&sawn, &snapped] {
        let bounds = stump.bounds();
        assert!(bounds.min.y < 0.0, "its roots reach into the ground");
        assert!(
            bounds.max.x - bounds.min.x > 3.0 * radius,
            "its roots flare"
        );
    }
}

/// The leafing of a broadleaf, its stumps' shoots bearing such leaves.
fn leafing() -> Leafing {
    Leafing {
        outline: crate::leaf::Outline::Ovate { teeth: 12 },
        per_twig: 8,
        length: 0.07,
        breadth: 0.35,
        fold: 0.2,
        angle: 45.0,
        toward_light: 0.6,
    }
}

/// The heights a stump's top is met at straight down, `ring` of its radius
/// out from its middle all the way round.
fn round_the_top(stump: &Prototype, (radius, ring): (f64, f64)) -> Vec<f64> {
    (0..720u32)
        .filter_map(|step| {
            let angle = TAU * f64::from(step) / 720.0;
            top_at(
                stump,
                (
                    ring * radius * mathf::cos(angle),
                    ring * radius * mathf::sin(angle),
                ),
            )
        })
        .collect()
}

#[test]
fn a_sawn_face_is_ringed_in_the_bark_it_cut_through() {
    let (height, radius) = (0.7, 0.3);
    for seed in 0..6u64 {
        let stump = stump(height, radius, (Top::Sawn, WOODS), (FRESH, None), seed)
            .expect("a stump")
            .whole();
        // Coming in from beyond it, the first of its level face met at each
        // angle, however it leans, is its rim.
        let rim = |angle: f64| {
            (0..300u32).find_map(|step| {
                let reach = 1.2 * radius - f64::from(step) * 0.002;
                let down = Ray::new(
                    Vec3::new(reach * mathf::cos(angle), 5.0, reach * mathf::sin(angle)),
                    -Vec3::UP,
                );
                let hit = stump.intersect(&down, (1e-9, 10.0), None)?;
                (5.0 - hit.t > height - 0.35 * radius && hit.normal.y > 0.7).then_some(hit.material)
            })
        };
        let edged = (0..36u32)
            .filter(|&step| rim(TAU * f64::from(step) / 36.0) == Some(Some(9)))
            .count();
        // Broken only where a fresh face's check or two, or the notch's edges,
        // run through it.
        assert!(
            edged >= 27,
            "{seed}: {edged} of 36 rims are the bark it cut through"
        );
        let down = Ray::new(Vec3::new(0.0, 5.0, 0.0), -Vec3::UP);
        let middle = stump
            .intersect(&down, (1e-9, 10.0), None)
            .expect("its middle");
        assert_eq!(middle.material, Some(1), "{seed}: sound wood within");
    }
}

#[test]
fn a_snapped_stump_is_torn_jagged_never_capped_in_bark() {
    let (height, radius) = (0.6, 0.3);
    for seed in 0..6u64 {
        let stump = stump(height, radius, (Top::Snapped, WOODS), (FRESH, None), seed)
            .expect("a stump")
            .whole();
        let mut heights = Vec::new();
        for step in 0..90u32 {
            let angle = TAU * f64::from(step) / 90.0;
            let reach = 0.5 * radius;
            let down = Ray::new(
                Vec3::new(reach * mathf::cos(angle), 5.0, reach * mathf::sin(angle)),
                -Vec3::UP,
            );
            let hit = stump
                .intersect(&down, (1e-9, 10.0), None)
                .expect("its top is met");
            assert_ne!(
                hit.material,
                Some(u32::from(WOODS.bark)),
                "{seed}: bark over the break"
            );
            heights.push(5.0 - hit.t);
        }
        let spread = heights.iter().copied().fold(f64::NEG_INFINITY, f64::max)
            - heights.iter().copied().fold(f64::INFINITY, f64::min);
        assert!(spread > 0.15 * radius, "{seed}: jagged across by {spread}");
    }
}

#[test]
fn a_sawn_face_dries_into_checks_running_in_from_its_rim() {
    let (height, radius) = (0.6, 0.3);
    let old = Decay {
        age: 0.5,
        fungus: None,
    };
    let (mut checked, mut sound_middles) = (0, 0);
    for seed in 0..8u64 {
        let stump = stump(height, radius, (Top::Sawn, WOODS), (old, None), seed)
            .expect("a stump")
            .whole();
        let rim = round_the_top(&stump, (radius, 0.9));
        let highest = rim.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        // A check cuts deep and narrow: well below the face beside it.
        if rim
            .iter()
            .any(|&met| met < highest - 0.04 * radius - 0.25 * radius)
        {
            checked += 1;
        }
        let middle = round_the_top(&stump, (radius, 0.05));
        let spread = middle.iter().copied().fold(f64::NEG_INFINITY, f64::max)
            - middle.iter().copied().fold(f64::INFINITY, f64::min);
        if spread < 0.3 * radius {
            sound_middles += 1;
        }
    }
    assert!(checked >= 6, "{checked} of eight checked at the rim");
    assert_eq!(sound_middles, 8, "checks never reach the pith");
}

#[test]
fn a_felled_stump_keeps_the_step_from_its_notch_to_its_back_cut() {
    let (height, radius) = (0.6, 0.3);
    let mut stepped = 0;
    for seed in 0..10u64 {
        let stump = stump(height, radius, (Top::Sawn, WOODS), (FRESH, None), seed)
            .expect("a stump")
            .whole();
        let across = round_the_top(&stump, (radius, 0.6));
        // The back cut stands highest, tilted as far as the stump leans; a
        // check, narrow, takes few of the samples down with it.
        let high = across.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        let level = across
            .iter()
            .filter(|&&met| met > high - 0.09 * radius)
            .count();
        let notch = across
            .iter()
            .filter(|&&met| met < high - 0.1 * radius && met > high - 0.34 * radius)
            .count();
        if notch > 40 {
            assert!(level > 40, "{seed}: the back cut beside the notch");
            stepped += 1;
        } else {
            assert!(
                10 * level > 9 * across.len(),
                "{seed}: level but for its checks"
            );
        }
    }
    assert!((3..=9).contains(&stepped), "{stepped} of ten felled");
}

#[test]
fn an_old_stumps_heart_rots_hollow() {
    let (height, radius) = (0.6, 0.3);
    let rotten = Decay {
        age: 0.95,
        fungus: None,
    };
    for seed in 0..4u64 {
        let stump = stump(height, radius, (Top::Sawn, WOODS), (rotten, None), seed)
            .expect("a stump")
            .whole();
        let down = Ray::new(Vec3::new(0.0, 5.0, 0.0), -Vec3::UP);
        let heart = stump
            .intersect(&down, (1e-9, 10.0), None)
            .expect("its hollow is met");
        let sound = round_the_top(&stump, (radius, 0.95));
        let rim = sound.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        assert!(5.0 - heart.t < rim - 0.2 * radius, "{seed}: hollowed");
        assert_eq!(heart.material, Some(2), "{seed}: rotten wood");
    }
}

#[test]
fn rot_fruits_in_brackets_shelving_level_out_from_the_wood_their_pores_beneath() {
    let fungus = Fungus {
        habit: Habit::Thick,
        zones: [3, 4],
        margin: 5,
        pores: 6,
    };
    let decayed = Decay {
        age: 0.6,
        fungus: Some(fungus),
    };
    for seed in 0..4u64 {
        let (height, radius) = (0.8, 0.3);
        let stump = stump(height, radius, (Top::Snapped, WOODS), (decayed, None), seed)
            .expect("a stump")
            .whole();
        let shelves = stump
            .parts()
            .iter()
            .filter(|part| {
                matches!(
                    part,
                    Part::Facet(Facet {
                        material: Some(3..=6),
                        ..
                    })
                )
            })
            .count();
        assert!(shelves > 100, "{seed}: {shelves} facets of bracket");
        // From below, beside the stump however its foot swells, a ray rising
        // meets a bracket's pores before anything else.
        let mut pores = 0;
        for step in 0..360u32 {
            let angle = TAU * f64::from(step) / 360.0;
            for out in [0.04, 0.1, 0.16, 0.22].map(|beyond| radius + beyond) {
                let up = Ray::new(
                    Vec3::new(out * mathf::cos(angle), 0.05, out * mathf::sin(angle)),
                    Vec3::UP,
                );
                if let Some(hit) = stump.intersect(&up, (1e-9, height), None) {
                    if hit.material == Some(6) {
                        pores += 1;
                    }
                }
            }
        }
        assert!(pores > 0, "{seed}: pores face the ground");
    }
    let log = log(6.0, 0.3, (WOODS, false), decayed, 2)
        .expect("a log")
        .whole();
    assert!(
        log.parts().iter().any(|part| matches!(
            part,
            Part::Facet(Facet {
                material: Some(6),
                ..
            })
        )),
        "brackets on a log's flanks"
    );
}

#[test]
fn a_broadleaf_stump_sends_up_shoots_leafy_but_in_winter() {
    let (height, radius) = (0.5, 0.25);
    let leafy = Sprouting {
        bark: 7,
        leaves: Some(8),
        leafing: leafing(),
    };
    let stump = stump(height, radius, (Top::Sawn, WOODS), (FRESH, Some(leafy)), 4)
        .expect("a stump")
        .whole();
    let shoots: Vec<&Tube> = stump
        .parts()
        .iter()
        .filter_map(|part| match part {
            Part::Tube(tube) if tube.material == 7 => Some(tube),
            _ => None,
        })
        .collect();
    assert!(shoots.len() >= 4 * 6, "{} shoot segments", shoots.len());
    let tallest = shoots
        .iter()
        .map(|tube| f64::from(tube.b[1]))
        .fold(f64::NEG_INFINITY, f64::max);
    assert!(tallest > height + 0.2, "rising over the stump: {tallest}");
    let leaves = stump
        .parts()
        .iter()
        .filter(|part| matches!(part, Part::Leaf(Blade { material: 8, .. })))
        .count();
    assert!(leaves > 10, "{leaves} leaves");
    let bare = Sprouting {
        leaves: None,
        ..leafy
    };
    let winter = super::stump(height, radius, (Top::Sawn, WOODS), (FRESH, Some(bare)), 4)
        .expect("a stump")
        .whole();
    assert!(!winter
        .parts()
        .iter()
        .any(|part| matches!(part, Part::Leaf(_))));
}
