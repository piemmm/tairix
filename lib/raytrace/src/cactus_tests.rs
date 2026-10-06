use core::f64::consts::{FRAC_PI_2, TAU};

use super::*;
use crate::prototype::point;

const RIBS: Ribs = Ribs {
    count: 19,
    seed: 0x5a9a,
};

const FLESH: Flesh = Flesh {
    trunk: (1, RIBS),
    arms: (
        2,
        Ribs {
            count: 14,
            seed: 0x0a12,
        },
    ),
    spines: 3,
};

const SPINES: Spines = Spines {
    centrals: 4,
    radials: 9,
};

/// A saguaro grown whole from `seed`, `height` tall.
fn grown(height: f64, seed: u64) -> Prototype {
    let mut bristling = Bristling::new(height, (0.25, 0.72), FLESH, SPINES, seed).expect("stems");
    while !bristling.step().expect("spines") {}
    bristling.finish().expect("parts").whole()
}

fn tubes(saguaro: &Prototype, material: u16) -> impl Iterator<Item = &Tube> {
    saguaro.parts().iter().filter_map(move |part| match part {
        Part::Tube(tube) if tube.material == material => Some(tube),
        _ => None,
    })
}

#[test]
fn every_point_lies_nearest_the_crest_it_is_given() {
    for step in 0..20_000u32 {
        let angle = f64::from(step) * 0.0137 - 40.0;
        let stem = f64::from(step % 997) * 0.0091;
        let lie = RIBS.lie(angle, stem);
        assert!((0.0..=1.0).contains(&lie.toward), "{step}: {}", lie.toward);
        let crest = RIBS.crest_angle(lie.rib, stem);
        // Round the stem the short way, a crest is at most half a pitch away.
        let apart = wrapped(angle - crest);
        assert!(
            (apart - lie.off * RIBS.pitch()).abs() < 1e-9,
            "{step}: {apart} against {}",
            lie.off * RIBS.pitch()
        );
        for other in 0..u32::from(RIBS.count) {
            let theirs = wrapped(angle - RIBS.crest_angle(other, stem)).abs();
            assert!(
                theirs + 1e-12 >= apart.abs(),
                "{step}: rib {other} is nearer"
            );
        }
    }
}

#[test]
fn ribs_close_round_the_stem_with_no_seam() {
    for step in 0..2000u32 {
        let (angle, stem) = (f64::from(step) * 0.031, f64::from(step) * 0.0043);
        let (here, round) = (RIBS.lie(angle, stem), RIBS.lie(angle + TAU, stem));
        assert_eq!(here.rib, round.rib, "{step}");
        assert!((here.off - round.off).abs() < 1e-9, "{step}");
        assert!((here.toward - round.toward).abs() < 1e-9, "{step}");
    }
}

#[test]
fn a_crest_stands_at_its_profile_s_top_and_a_groove_at_its_foot() {
    let crest = RIBS.lie(RIBS.crest_angle(5, 1.3), 1.3);
    assert_eq!(crest.rib, 5);
    assert!(crest.toward < 1e-9 && (profile(crest.toward) - 1.0).abs() < 1e-9);
    let between = f64::midpoint(RIBS.crest_angle(5, 1.3), RIBS.crest_angle(6, 1.3));
    let groove = RIBS.lie(between, 1.3);
    assert!(groove.toward > 0.999 && profile(groove.toward) < 1e-3);
}

#[test]
fn a_rib_s_profile_falls_steadily_no_steeper_than_its_bound_and_holds_its_mean() {
    const STEPS: u32 = 100_000;
    let (mut last, mut steepest, mut sum) = (profile(0.0), 0.0f64, 0.0);
    for step in 1..=STEPS {
        let toward = f64::from(step) / f64::from(STEPS);
        let here = profile(toward);
        assert!(here < last, "{toward}: falls all the way to its groove");
        steepest = steepest.max((last - here) * f64::from(STEPS));
        sum += here / f64::from(STEPS);
        last = here;
    }
    assert!(steepest <= PROFILE_STEEPEST, "{steepest}");
    assert!(
        steepest > 0.99 * PROFILE_STEEPEST,
        "the bound is tight: {steepest}"
    );
    assert!((sum - MEAN_RIB).abs() < 1e-3, "{sum}");
}

#[test]
fn areoles_follow_one_another_up_every_crest_about_two_centimetres_apart() {
    for rib in 0..u32::from(RIBS.count) {
        let first = RIBS.first_areole(rib, 0.0);
        let mut last = RIBS.areole(rib, first).0;
        assert!(last <= 0.0, "{rib}: counting begins at or above the apex");
        for index in first + 1..first + 400 {
            let (at, _) = RIBS.areole(rib, index);
            let apart = at - last;
            assert!(
                apart > 0.65 * AREOLE_SPACING && apart < 1.35 * AREOLE_SPACING,
                "{rib}, {index}: {apart} apart"
            );
            last = at;
        }
        for step in 0..500u32 {
            let stem = f64::from(step) * 0.0133;
            let (off, _) = RIBS.nearest_areole(rib, stem);
            let first = RIBS.first_areole(rib, stem);
            let nearest = (first - 2..first + 5)
                .map(|index| (stem - RIBS.areole(rib, index).0).abs())
                .fold(f64::INFINITY, f64::min);
            assert!((off.abs() - nearest).abs() < 1e-12, "{rib}, {stem}");
        }
    }
}

#[test]
fn a_cushion_stands_full_at_its_middle_and_falls_to_nothing_at_its_rim() {
    for wool in [-1.0, -0.3, 0.4, 1.0] {
        let top = cushion(0.0, 0.0, wool);
        assert!((0.55..=1.0 + 1e-12).contains(&top), "{wool}: {top}");
        assert!(cushion(CUSHION.0, 0.0, wool).abs() < 1e-12);
        assert!(cushion(0.0, CUSHION.1, wool).abs() < 1e-12);
        assert!(cushion(0.5 * CUSHION.0, 0.5 * CUSHION.1, wool) > 0.0);
        assert!(cushion(CUSHION.0, CUSHION.1, wool).abs() < 1e-12);
    }
    assert!(
        (cushion(0.0, 0.0, 1.0) - 1.0).abs() < 1e-12,
        "its wool's tallest tufts"
    );
}

#[test]
fn only_the_oldest_skin_corks_over() {
    for step in 0..500u32 {
        let angle = f64::from(step) * 0.11;
        assert!(
            RIBS.corked(angle, 4.0 + f64::from(step) * 0.0014) == 0.0,
            "{step}"
        );
        assert!(
            RIBS.corked(angle, 11.2 + f64::from(step) * 0.002) > 0.95,
            "{step}"
        );
    }
    let middling = (0..500u32)
        .map(|step| RIBS.corked(f64::from(step) * 0.11, 8.0))
        .sum::<f64>()
        / 500.0;
    assert!(middling > 0.1 && middling < 0.9, "in tongues: {middling}");
}

#[test]
fn a_saguaro_grows_to_its_height_its_arms_rising_beside_its_trunk() {
    let mut armed = 0;
    for seed in 0..6u64 {
        let saguaro = grown(7.0, seed);
        let bounds = saguaro.bounds();
        assert!(
            bounds.max.y > 6.4 && bounds.max.y < 7.2,
            "{seed}: {} tall",
            bounds.max.y
        );
        assert!(
            bounds.min.y < -0.2,
            "{seed}: its foot runs on into the ground"
        );
        assert!(saguaro.parts().len() <= MOST_PARTS, "{seed}");
        let arms: Vec<&Tube> = tubes(&saguaro, FLESH.arms.0).collect();
        if !arms.is_empty() {
            armed += 1;
        }
        for arm in &arms {
            // No arm grows down into the ground, nor above the trunk's apex.
            let (a, b) = (point(arm.a), point(arm.b));
            assert!(
                a.y.min(b.y) > 0.5 && a.y.max(b.y) < 7.2,
                "{seed}: {a:?} {b:?}"
            );
        }
        // Each arm turns up through its elbow and ends in its apex, upright.
        for top in arms.iter().copied().filter(|&arm| apex(arm)) {
            let up = (point(top.b) - point(top.a)).normalized();
            assert!(up.y > 0.95, "{seed}: an arm's apex points up");
        }
        let trunk_tops = tubes(&saguaro, FLESH.trunk.0)
            .filter(|&tube| apex(tube))
            .count();
        assert_eq!(trunk_tops, 1, "{seed}");
    }
    assert!(armed >= 3, "most grown saguaros bear arms: {armed} of 6");
}

#[test]
fn a_young_saguaro_is_a_lone_column() {
    for seed in 0..4u64 {
        let saguaro = grown(2.8, seed);
        assert_eq!(tubes(&saguaro, FLESH.arms.0).count(), 0, "{seed}");
    }
}

#[test]
fn an_arm_leaves_its_trunk_narrow_and_swells_to_its_girth() {
    for seed in 0..8u64 {
        let saguaro = grown(7.5, seed);
        let arms: Vec<&Tube> = tubes(&saguaro, FLESH.arms.0).collect();
        // Each arm's limbs run on from one to the next; its first leaves the
        // trunk's axis.
        let mut index = 0;
        while index < arms.len() {
            let mut end = index + 1;
            while end < arms.len() && !apex(arms[end - 1]) {
                end += 1;
            }
            let arm = &arms[index..end];
            let neck = f64::from(arm[0].radii[0]);
            let girth = arm
                .iter()
                .map(|tube| f64::from(tube.radii[1]))
                .fold(0.0, f64::max);
            assert!(neck < 0.6 * girth, "{seed}: {neck} against {girth}");
            index = end;
        }
    }
}

#[test]
fn a_saguaro_s_stem_is_reckoned_from_its_apex_down() {
    let saguaro = grown(6.0, 3);
    let (mut tops, mut stems) = (0, 0);
    for tube in tubes(&saguaro, FLESH.trunk.0).chain(tubes(&saguaro, FLESH.arms.0)) {
        assert!(tube.stem[0] > tube.stem[1], "it runs down from the apex");
        tops += usize::from(apex(tube));
        // Over the sphere rounding its end, its stem runs on to nought at the
        // apex.
        let (a, b) = (point(tube.a), point(tube.b));
        let (at_pole, _) = tube.over_end(1, (b - a).normalized());
        if apex(tube) {
            assert!(at_pole.abs() < 1e-4, "{at_pole}");
        }
    }
    for _ in tubes(&saguaro, FLESH.arms.0).filter(|&tube| apex(tube)) {
        stems += 1;
    }
    assert_eq!(tops, stems + 1, "one apex a stem");
}

/// Whether `tube` is its stem's last, its round a quarter meridian below the
/// apex its end rounds into.
fn apex(tube: &Tube) -> bool {
    (f64::from(tube.stem[1]) - FRAC_PI_2 * f64::from(tube.radii[1])).abs() < 1e-4
}

#[test]
fn every_spine_springs_from_an_areole_on_a_crest() {
    let saguaro = grown(6.5, 5);
    let limbs: Vec<(&Tube, Ribs)> = tubes(&saguaro, FLESH.trunk.0)
        .map(|tube| (tube, FLESH.trunk.1))
        .chain(tubes(&saguaro, FLESH.arms.0).map(|tube| (tube, FLESH.arms.1)))
        .collect();
    let mut spines = 0;
    for spine in tubes(&saguaro, FLESH.spines) {
        spines += 1;
        // Its foot sinks a millimetre and a half into its cushion.
        let foot = point(spine.a);
        let near = limbs
            .iter()
            .filter_map(|&(limb, ribs)| on_crest(limb, ribs, foot))
            .fold(f64::INFINITY, f64::min);
        assert!(near < 0.004, "a spine stands {near} from any areole");
    }
    assert!(spines > 20_000, "{spines}");
}

/// How far `point` lies from the nearest areole of `limb`, folded into
/// `ribs`, if it lies by the limb's skin at all: on the nearest crest's or,
/// where the ribs crowd over a dome, a neighbour's.
fn on_crest(limb: &Tube, ribs: Ribs, point: Vec3) -> Option<f64> {
    let (a, b) = (
        crate::prototype::point(limb.a),
        crate::prototype::point(limb.b),
    );
    let length = (b - a).length();
    let axis = (b - a) * (1.0 / length);
    let up = (point - a).dot(axis);
    let (ra, rb) = (f64::from(limb.radii[0]), f64::from(limb.radii[1]));
    // Over the sphere rounding its end, or along its side, which a bending
    // stem's limbs share a little of at their joints.
    let over = (up > length).then(|| {
        let normal = (point - b).normalized();
        let (stem, girth) = limb.over_end(1, normal);
        (((point - b).length() - rb).abs(), (stem, girth, normal))
    });
    let side = (-0.01..=length + 0.01).contains(&up).then(|| {
        let up = up.clamp(0.0, length);
        let radial = point - a - axis * up;
        let radius = ra + (rb - ra) * up / length;
        let (from, to) = (f64::from(limb.stem[0]), f64::from(limb.stem[1]));
        (
            (radial.length() - radius).abs(),
            (
                from + (to - from) * up / length,
                radius,
                radial.normalized(),
            ),
        )
    });
    over.into_iter()
        .chain(side)
        .filter(|&(off, _)| off < 0.004)
        .filter_map(|(_, (stem, girth, normal))| {
            let angle = limb.angle_of(normal);
            let nearest = ribs.lie(angle, stem).rib + u32::from(ribs.count);
            (nearest - 1..=nearest + 1)
                .map(|rib| {
                    let rib = rib % u32::from(ribs.count);
                    let across = wrapped(angle - ribs.crest_angle(rib, stem)) * girth;
                    let (along, _) = ribs.nearest_areole(rib, stem);
                    mathf::hypot(across, along)
                })
                .reduce(f64::min)
        })
        .reduce(f64::min)
}

#[test]
fn old_spines_are_fewer_and_snapped() {
    let mut bristling = Bristling::new(7.8, (0.28, 0.75), FLESH, SPINES, 11).expect("stems");
    while !bristling.step().expect("spines") {}
    // The areoles whose age falls between two, over every stem.
    let areoles = |(from, to): (f64, f64)| -> f64 {
        bristling
            .stems
            .iter()
            .map(|stem| {
                let held = stem.length().min(to) - from.min(stem.length());
                held.max(0.0) * f64::from(stem.ribs.count) / AREOLE_SPACING
            })
            .sum()
    };
    let bands = [(0.1, 1.5), (4.0, 5.0)];
    let mut tallies = [(0u32, 0u32); 2];
    for part in &bristling.parts {
        let Part::Tube(spine) = part else {
            continue;
        };
        if spine.material != FLESH.spines {
            continue;
        }
        let age = f64::from(spine.stem[0]);
        assert_eq!(
            spine.stem[0].to_bits(),
            spine.stem[1].to_bits(),
            "a spine's age is one along it"
        );
        for (tally, &(from, to)) in tallies.iter_mut().zip(&bands) {
            if (from..to).contains(&age) {
                tally.0 += 1;
                tally.1 += u32::from(spine.radii[1] > 0.2 * spine.radii[0]);
            }
        }
    }
    let each = |(spines, _): (u32, u32), band| f64::from(spines) / areoles(band);
    let snapped = |(spines, blunt): (u32, u32)| f64::from(blunt) / f64::from(spines.max(1));
    let [young, old] = tallies;
    let (young_each, old_each) = (each(young, bands[0]), each(old, bands[1]));
    assert!(young_each > 10.0, "{young_each} an areole about the apex");
    assert!(
        old_each < 0.7 * young_each,
        "{old_each} against {young_each}"
    );
    assert!(
        snapped(young) < 0.02 && snapped(old) > 0.1,
        "{young:?} {old:?}"
    );
}

#[test]
fn a_spine_carries_the_age_of_its_areole() {
    let saguaro = grown(6.5, 5);
    let limbs: Vec<&Tube> = tubes(&saguaro, FLESH.trunk.0)
        .chain(tubes(&saguaro, FLESH.arms.0))
        .collect();
    for spine in tubes(&saguaro, FLESH.spines) {
        let foot = point(spine.a);
        // The stem of the skin it springs from: along a limb's side, which a
        // bending stem's limbs share a little of at their joints, or over the
        // sphere rounding its end.
        let skin = limbs
            .iter()
            .flat_map(|limb| {
                let (a, b) = (point(limb.a), point(limb.b));
                let length = (b - a).length();
                let up = (foot - a).dot((b - a) * (1.0 / length));
                let (from, to) = (f64::from(limb.stem[0]), f64::from(limb.stem[1]));
                let side = (-0.01..=length + 0.01).contains(&up).then(|| {
                    let up = up.clamp(0.0, length);
                    let radial = (foot - a - (b - a) * (up / length)).length();
                    let radius = f64::from(limb.radii[0])
                        + f64::from(limb.radii[1] - limb.radii[0]) * up / length;
                    ((radial - radius).abs() < 0.004).then(|| from + (to - from) * up / length)
                });
                let over = (up > length).then(|| {
                    let (stem, _) = limb.over_end(1, (foot - b).normalized());
                    (((foot - b).length() - f64::from(limb.radii[1])).abs() < 0.004).then_some(stem)
                });
                side.flatten().into_iter().chain(over.flatten())
            })
            .fold(f64::INFINITY, |best, stem| {
                if (stem - f64::from(spine.stem[0])).abs() < (best - f64::from(spine.stem[0])).abs()
                {
                    stem
                } else {
                    best
                }
            });
        assert!(
            (skin - f64::from(spine.stem[0])).abs() < 0.006,
            "a spine {} old on skin {skin} old",
            spine.stem[0]
        );
    }
}

#[test]
fn a_seed_grows_the_same_saguaro_and_another_seed_another() {
    let describe = |seed| alloc::format!("{:?}", grown(5.5, seed).parts());
    assert_eq!(describe(4), describe(4));
    assert_ne!(describe(4), describe(5));
}

#[test]
fn a_saguaro_s_spines_are_set_a_bounded_stretch_at_a_time() {
    let mut bristling = Bristling::new(7.5, (0.28, 0.75), FLESH, SPINES, 2).expect("stems");
    let mut steps = 0;
    let mut last = bristling.parts.len();
    while !bristling.step().expect("spines") {
        steps += 1;
        let laid = bristling.parts.len() - last;
        // A step sets a bounded number of areoles' spines: within one limb's
        // worth of the budget.
        assert!(laid < 2 * AREOLES_PER_STEP * 14, "{laid}");
        last = bristling.parts.len();
    }
    assert!(steps > 3, "{steps}");
}

#[test]
fn a_crowded_saguaro_thins_its_spines_evenly_within_its_parts() {
    let thick = Spines {
        centrals: 40,
        radials: 200,
    };
    let mut bristling = Bristling::new(8.0, (0.3, 0.8), FLESH, thick, 6).expect("stems");
    while !bristling.step().expect("spines") {}
    assert!(bristling.share < 0.2, "{}", bristling.share);
    let parts = bristling.parts.len();
    assert!(parts <= MOST_PARTS + MOST_PARTS / 10, "{parts}");
    assert!(parts > MOST_PARTS / 3, "{parts}");
}
