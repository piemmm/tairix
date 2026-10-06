//! Host tests of the buildings a scene sets out: a row of arches keeps the
//! ground clear beneath its bays as well as under its piers, every building
//! is laid unit by unit, an Ionic capital's volutes face out where they are
//! seen, a ruin's columns stand, break and fall, and an aqueduct stands on
//! the land as built and tunnels into its hills only where they cover it.

use alloc::vec::Vec;

use tairix_rng::NonCryptoRng;

use super::*;
use crate::detail::Detail;
use crate::prototype::{Building, Part, Prototype};
use crate::solid::Solid;
use crate::vector::Ray;

/// The structures `stage` laid unit by unit, their hierarchies built.
fn assembled(stage: &mut Stage) -> Vec<Prototype> {
    core::mem::take(&mut stage.assembled)
        .into_iter()
        .map(Building::whole)
        .collect()
}

/// Every solid of `built`.
fn solids(built: &Prototype) -> impl Iterator<Item = &Solid> {
    built.parts().iter().filter_map(|part| match part {
        Part::Solid(solid) => Some(solid),
        _ => None,
    })
}

fn stonework(stage: &mut Stage) -> Stonework {
    let mut dice = Dice(NonCryptoRng::seed_from_u64(5));
    let weathering = Weathering {
        damp: 0.4,
        drought: 0.3,
        foot: 0.3,
    };
    stage
        .stonework(&mut dice, Quarry::Limestone, (0.5, weathering))
        .expect("stone")
}

#[test]
fn a_row_of_arches_keeps_its_bays_clear_as_well_as_its_piers() {
    let mut stage = Stage::new(Detail::Simple.densities()).expect("a stage");
    let mut mason = Mason::new(stonework(&mut stage), 3).expect("a mason");
    let (bays, span, pier, heading) = (4, 9.0, 1.2, 0.4);
    let row = span * f64::from(bays) + 2.0 * pier;
    arcade_wall(
        &mut stage,
        &mut mason,
        (Vec3::ZERO, heading),
        (bays, span),
        (pier, 8.0),
        None,
    )
    .expect("arches");
    let along = direction(heading, FRAC_PI_2, 0.0);
    let across = Vec3::UP.cross(along);
    for step in 0..=144u32 {
        let at = along * ((f64::from(step) / 144.0 - 0.5) * row);
        // Across the masonry: its piers a pier either side, its arches less.
        for side in [-0.9, 0.0, 0.9] {
            let place = at + across * (side * pier);
            assert!(
                !stage.clear((place.x, place.z), 0.0),
                "a tree could stand beneath the arches at {place:?}"
            );
        }
    }
    let beside = across * (4.0 * pier);
    assert!(
        stage.clear((beside.x, beside.z), 0.0),
        "the ground beside the row is free"
    );
}

/// Each building is laid in solids, each its own key, none of them so
/// small its march could not resolve it. An aqueduct is laid once its land
/// stands, so an arcade may lay nothing yet.
#[test]
fn every_building_is_laid_stone_by_stone() {
    for (compose, landed) in [
        (
            colonnade as fn(&mut Stage, &mut Dice) -> Option<Composed>,
            false,
        ),
        (arcade, true),
        (rotunda, false),
        (ruins, false),
    ] {
        for seed in 0..6 {
            let mut dice = Dice(NonCryptoRng::seed_from_u64(seed));
            let mut stage = Stage::new(Detail::Simple.densities()).expect("a stage");
            compose(&mut stage, &mut dice).expect("a building");
            let built = assembled(&mut stage);
            if built.is_empty() && landed {
                continue;
            }
            assert!(!built.is_empty(), "{seed}: nothing laid");
            let mut keys = Vec::new();
            for structure in &built {
                for solid in solids(structure) {
                    let half = solid.half();
                    assert!(
                        half.x >= 1e-4 && half.y >= 1e-4 && half.z >= 1e-4,
                        "{seed}: a unit of no size, {half:?}"
                    );
                    keys.push(solid.key());
                }
            }
            assert!(keys.len() > 40, "{seed}: {} units", keys.len());
            keys.sort_unstable();
            let laid = keys.len();
            keys.dedup();
            assert_eq!(keys.len(), laid, "{seed}: two units share a key");
        }
    }
}

/// An Ionic capital's volutes face out, the first thing a ray from before
/// one meets.
#[test]
fn an_ionic_capitals_volutes_are_in_sight() {
    let mut volutes = 0;
    for seed in 0..24 {
        for compose in [colonnade, rotunda, ruins] {
            let mut dice = Dice(NonCryptoRng::seed_from_u64(seed));
            let mut stage = Stage::new(Detail::Simple.densities()).expect("a stage");
            compose(&mut stage, &mut dice).expect("a building");
            for structure in assembled(&mut stage) {
                for scroll in
                    solids(&structure).filter(|solid| matches!(solid.form(), Form::Scroll { .. }))
                {
                    volutes += 1;
                    let (centre, out) = (scroll.centre(), scroll.frame().y);
                    // Before the volute's face, off its axis to its channel.
                    let side = scroll.frame().x * (0.5 * scroll.half().x);
                    // Its inner face lies against its bolster; its outer one
                    // faces out.
                    let seen = [out, -out].into_iter().any(|way| {
                        let start = centre + way * (scroll.half().y + 0.5) + side;
                        let ray = Ray::new(start, -way);
                        structure
                            .intersect(&ray, (0.0, 1.0), None)
                            .is_some_and(|hit| hit.mark == scroll.key())
                    });
                    assert!(seen, "{seed}: a volute hidden behind its capital");
                }
            }
        }
    }
    assert!(volutes > 0, "no Ionic capital in any building");
}

/// A ruin's columns stand whole, broken off, or fallen in drums lying on
/// their sides along the way they fell.
#[test]
fn a_ruins_columns_stand_break_and_fall() {
    let (mut lying, mut upright) = (0, 0);
    for seed in 0..8 {
        let mut dice = Dice(NonCryptoRng::seed_from_u64(seed));
        let mut stage = Stage::new(Detail::Simple.densities()).expect("a stage");
        ruins(&mut stage, &mut dice).expect("a ruin");
        for structure in assembled(&mut stage) {
            for drum in solids(&structure).filter(|solid| matches!(solid.form(), Form::Drum { .. }))
            {
                if drum.frame().y.y.abs() < 0.2 {
                    lying += 1;
                } else {
                    upright += 1;
                }
            }
        }
    }
    assert!(
        lying > 0 && upright > 0,
        "{lying} drums fallen, {upright} standing"
    );
}

/// An aqueduct, its channel forty metres up, across a valley sited as
/// `sited` has the land along its line and built as `built` has it.
fn aqueduct_over(
    (sited_at, built): (&dyn Fn(f64) -> f64, &dyn Fn(f64) -> f64),
) -> (Aqueduct, Groundwork) {
    let aqueduct = Aqueduct {
        heading: 0.3,
        floor: 40.0,
        height: 80.0,
        channel: 40.0,
        span: 18.0,
    };
    let line = aqueduct.line();
    let reach = aqueduct.reach();
    let along = |x: f64, z: f64| x * line.x + z * line.z;
    let sited = Section::along(line, reach, &|x, z| sited_at(along(x, z))).expect("sited");
    let ground = |x: f64, z: f64| built(along(x, z));
    let (left, right, _) = aqueduct.crossing(&|x, z| sited.at(along(x, z)));
    let into = [
        aqueduct
            .entering(&sited, &ground, (left, -1.0))
            .expect("into the hill"),
        aqueduct
            .entering(&sited, &ground, (right, 1.0))
            .expect("into the hill"),
    ];
    let groundwork = Groundwork {
        built: Section::along(line, reach, &ground).expect("built"),
        sited,
        ends: (left, right),
        into,
    };
    (aqueduct, groundwork)
}

/// A valley rising half a metre to the metre from its middle, forty-five
/// metres below the channel.
fn sited_at(s: f64) -> f64 {
    0.5 * s.abs() - 5.0
}

/// Where the land as built lies lower than it was sited, as the coarse land
/// it is sited on smooths a hollow away, every pier still reaches down to
/// the ground it stands on. The valley is shallow enough that none stands
/// on a great arch.
#[test]
fn an_aqueducts_piers_reach_the_ground_as_built() {
    let sited = |s: f64| 0.5 * s.abs() + 25.0;
    let built = |s: f64| sited(s) - 2.0;
    let (aqueduct, groundwork) = aqueduct_over((&sited, &built));
    let mut stage = Stage::new(Detail::Simple.densities()).expect("a stage");
    let mut dice = Dice(NonCryptoRng::seed_from_u64(3));
    aqueduct
        .works(&mut stage, &mut dice, &groundwork)
        .expect("its works");
    let structures = assembled(&mut stage);
    let (left, right) = groundwork.ends;
    let arcade = Arcade::new(&aqueduct, (left, right));
    let (span_u, pier_u, ..) = arcade.upper();
    let line = aqueduct.line();
    let mut piers = 0;
    let first = mathf::round_i32(mathf::ceil(-left / span_u)) + 1;
    let last = mathf::round_i32(mathf::floor(right / span_u)) - 1;
    for k in first..=last {
        let s = f64::from(k) * span_u;
        let at = line * s;
        let lowest = structures
            .iter()
            .flat_map(solids)
            .filter(|solid| {
                let centre = solid.centre();
                solid.frame().y.y > 0.99 && mathf::hypot(centre.x - at.x, centre.z - at.z) < pier_u
            })
            .map(|solid| solid.centre().y - solid.half().y)
            .fold(f64::INFINITY, f64::min);
        if lowest.is_finite() {
            piers += 1;
            assert!(
                lowest <= built(s) - 0.5,
                "the pier at {s} stands on {lowest} over ground at {}",
                built(s)
            );
        }
    }
    assert!(piers > 4, "{piers} piers looked at");
}

/// Where the hill as built covers its conduit at a cutting's end the
/// channel tunnels through a portal whose headwall holds back the hill
/// behind it; where the hill there lies too low, the conduit runs on until
/// the hill covers it.
#[test]
fn an_aqueduct_tunnels_where_its_hill_covers_it_and_runs_on_buried_where_not() {
    let (aqueduct, groundwork) = aqueduct_over((&sited_at, &sited_at));
    let roof = aqueduct.channel + aqueduct.conduit().1 + SLAB;
    for into in &groundwork.into {
        let steps = into.portal.as_ref().expect("a portal");
        assert!((into.run - into.cut - 2.0).abs() < 1e-9, "{into:?}");
        let middle = steps
            .iter()
            .find(|step| step.from < 0.0 && step.to > 0.0)
            .expect("a stretch over the arch");
        let (left, _) = groundwork.ends;
        let behind = sited_at(left + into.cut + PAST_CUT);
        assert!(
            middle.top >= behind + 0.5 - 1e-9,
            "{middle:?} holding a hill at {behind}"
        );
        assert!(
            steps.len() >= 3,
            "its headwall steps out over the cutting's sides: {steps:?}"
        );
        assert!(
            steps.iter().all(|step| step.top >= step.foot + 0.8),
            "{steps:?}"
        );
    }
    // As built the hill lies four metres lower beyond the arches than sited.
    let lower = |s: f64| {
        if s.abs() > 70.0 {
            sited_at(s) - 4.0
        } else {
            sited_at(s)
        }
    };
    let (aqueduct, groundwork) = aqueduct_over((&sited_at, &lower));
    let (left, right) = groundwork.ends;
    for (into, end) in groundwork.into.iter().zip([left, right]) {
        assert!(
            into.portal.is_none(),
            "a portal into a hill that does not cover it: {into:?}"
        );
        assert!(into.run > into.cut + 2.0, "{into:?}");
        assert!(
            lower(end + into.run - 2.0) >= roof + COVER - 1e-9,
            "the conduit ends uncovered: {into:?}"
        );
    }
    let _ = aqueduct;
}
