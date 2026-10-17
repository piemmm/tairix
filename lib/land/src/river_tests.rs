//! Host tests of what a land's rivers make of it: a hard cap wearing slowly,
//! a low river's bed and banks, a level course wandering, and a delta's fan.

use alloc::vec::Vec;
use core::f64::consts::TAU;

use tairix_noise::noise2;
use tairix_util::mathf::{self, real};

use super::*;
use crate::channel::{Banked, Form, Station};
use crate::course::{Mark, Nearest};
use crate::plan::Rivers;

/// A bed's hard cap wears its hardness times as slowly as the softer rock
/// beneath it, so a canyon's caps stand as cliffs over its benches; ground
/// with no strata wears as ordinary ground does.
#[test]
fn a_hard_cap_wears_its_hardness_times_as_slowly() {
    let (spacing, hardness) = (6.0, 4.0);
    let strata = Some((spacing, hardness));
    for bed in 0..5 {
        let floor = spacing * f64::from(bed);
        let cap = erodibility(strata, floor + 0.95 * spacing);
        let soft = erodibility(strata, floor + 0.5 * spacing);
        assert!((cap * hardness - 1.0).abs() < 1e-12, "bed {bed}: cap {cap}");
        assert!((soft - 1.0).abs() < 1e-12, "bed {bed}: soft {soft}");
        assert!(cap < soft);
    }
    assert!((erodibility(None, 17.0) - 1.0).abs() < 1e-12);
}

/// A low river's bed is soaked where it is bared, never under standing
/// water above the river's level, and nothing roots in it; the plants on its
/// banks thin toward it over a fringe, not at one grid step; and what its
/// channel laid there has the say within its banks' faces, and none beyond.
#[test]
fn a_low_rivers_bare_margin_is_soaked_and_its_banks_thin_toward_it() {
    let station = Station {
        along: 0.0,
        brim: 10.0,
        width: 6.0,
        depth: 1.0,
        phase: 0.6,
        turn: 0.0,
        fall: 0.01,
    };
    let form = Form {
        flowing: 0.4,
        ledges: 0.0,
        outcrops: 0.0,
        seed: 3,
    };
    let banked = Banked::new(&station, &form);
    let section = &banked.section;
    // On the bar, across from where the deep water swings.
    let side = if section.thalweg >= 0.0 { -1.0 } else { 1.0 };
    let near = |distance: f64| Nearest {
        course: 0,
        along: 0.0,
        distance,
        side,
        level: 10.0,
        width: 6.0,
        depth: 1.0,
        run: 0.0,
        phase: 0.6,
        turn: 0.0,
        fall: 0.01,
        toward: (0.0, 1.0),
        past: 0.0,
    };
    let step = 0.05;
    let at = |distance: f64| river_bed(&banked, &near(distance), 12.0, (step, 0.0));
    let deepest = river_bed(
        &banked,
        &Nearest {
            side: section.thalweg.signum(),
            ..near(section.thalweg.abs())
        },
        12.0,
        (step, 0.0),
    );
    assert!(deepest.wet > 0.99 && deepest.scoured > 0.99);
    assert!((deepest.height - section.deepest).abs() < 1e-9);
    // Bared: the bed stands above the water up the bar.
    let edge = side * section.edge(side);
    let bared = at(edge + 0.6 * (section.half - edge));
    assert!(bared.height > section.water + BARE, "{bared:?}");
    assert!(bared.wet < 0.82 && bared.wet > 0.6, "{bared:?}");
    // Pioneer plants take at most their share of a bar's top.
    assert!(bared.scoured >= 1.0 - PIONEERS - 1e-9, "{bared:?}");
    let fringe = (FRINGE * 6.0).clamp(0.4, 2.0);
    let from = section.half - 0.25 * fringe;
    let scours: Vec<f64> = (0..200)
        .map(|index| at(from + 0.01 * f64::from(index)).scoured)
        .collect();
    assert!(scours[0] >= 1.0 - PIONEERS - 1e-9 && scours[199] < 1e-12);
    let thinning = scours
        .iter()
        .filter(|&&share| share > 0.02 && share < 0.98)
        .count();
    assert!(
        0.01 * f64::from(u32::try_from(thinning).expect("a count")) > 0.5 * fringe,
        "the fringe thins over {thinning} hundredths"
    );
    assert!(at(section.half + 1.5 * fringe).scoured < 1e-12);
    assert!((at(1.0).say - 1.0).abs() < 1e-12);
    let faces = section.half + banked.bank_reach(side, step);
    assert!(at(faces + 3.0 * step).say < 1e-12);
    // The bank rises from the brim to the land beyond.
    assert!((at(section.half).height - section.brim).abs() < 1e-9);
    assert!(at(faces + 20.0).height > 11.9);
}

#[test]
fn a_straight_level_river_wanders_only_sideways_by_its_own_length_run() {
    let rivers = Rivers {
        catchment: 1.0e5,
        width: 4.0,
        meander: 1.0,
        flowing: 1.0,
        ledges: 0.0,
        outcrops: 0.0,
    };
    let course: Vec<Mark> = (0..64)
        .map(|index| Mark {
            x: 4.0 * real(index),
            z: 10.0,
            level: 2.0,
            width: 4.0,
            depth: 1.0,
            ..Mark::default()
        })
        .collect();
    let seed = 0x5eed;
    let wandering = meandering(&course, rivers, seed).expect("a course");
    assert_eq!(wandering.len(), course.len());
    let (first, last) = (wandering[0], wandering[course.len() - 1]);
    assert_eq!((first.x, first.z), (0.0, 10.0), "the ends stay put");
    assert_eq!((last.x, last.z), (252.0, 10.0), "the ends stay put");
    let wavelength = 11.0 * 4.0;
    let mut swayed = 0.0_f64;
    for (index, (moved, at)) in wandering.iter().zip(&course).enumerate().skip(1).take(62) {
        assert_eq!(
            moved.x.to_bits(),
            at.x.to_bits(),
            "a course along x moves across it alone"
        );
        // Measured along the course as it lay, not as earlier points left it.
        let travelled = 4.0 * real(index);
        let sway = 4.0
            * mathf::sin(
                TAU * travelled / wavelength + noise2(travelled / (4.0 * wavelength), 0.3, seed),
            );
        assert!(
            (moved.z - (10.0 + sway)).abs() < 1e-9,
            "{index}: {} against {sway}",
            moved.z - 10.0
        );
        swayed = mathf::fmax(swayed, sway.abs());
    }
    assert!(
        swayed > 3.0,
        "it wanders as far as its width either way: {swayed}"
    );
}

/// A river running out into standing water fans its silt out before it:
/// built a little above the water at its mouth, shelving under it toward
/// its fringe, cut by its distributaries, and leaving the bed beyond its
/// spread as it was.
#[test]
fn a_delta_fans_its_silt_out_into_the_water() {
    let course: Vec<Mark> = (0..8u32)
        .map(|index| Mark {
            x: 100.0 + 10.0 * f64::from(index),
            z: -40.0,
            width: 6.0,
            level: 3.0,
            ..Mark::default()
        })
        .collect();
    let fan = delta(&course, 2.0).expect("a delta at the mouth");
    assert_eq!(fan.apex, (170.0, -40.0));
    assert!((fan.toward.0 - 1.0).abs() < 1e-12 && fan.toward.1.abs() < 1e-12);
    assert!((fan.length - (40.0 + 7.0 * 6.0)).abs() < 1e-12);
    assert!(delta(&course[..3], 2.0).is_none(), "too short a river");
    assert!(delta(&course, f64::NAN).is_none(), "no water to fan into");
    let bed = -1.5;
    let probe = |along: f64, across: f64| delta_bed(&fan, (170.0 + along, -40.0 + across), bed);
    let (mouth, silt) = probe(1.0, 0.0);
    assert!(mouth > fan.level - 0.8, "a mouth at {mouth}");
    assert!(silt > 0.5, "fresh silt at the mouth: {silt}");
    // Its fan's crest shelves from above the water at the mouth to below it.
    let crest = |along: f64| {
        (0..=400u32)
            .map(|step| probe(along, -60.0 + 0.3 * f64::from(step)).0)
            .fold(f64::NEG_INFINITY, mathf::fmax)
    };
    assert!(crest(2.0) > fan.level, "built above the water at its mouth");
    assert!(
        crest(0.75 * fan.length) < fan.level,
        "under the water at its fringe"
    );
    assert_eq!(
        probe(1.0, 3.0 * fan.width + 10.0),
        (bed, 0.0),
        "beyond its spread"
    );
    assert_eq!(probe(fan.length + 1.0, 0.0), (bed, 0.0), "past its fringe");
    assert_eq!(
        probe(-2.0 * fan.width, 0.0),
        (bed, 0.0),
        "upstream of its mouth"
    );
    let across: Vec<f64> = (0..=200u32)
        .map(|step| probe(30.0, -20.0 + 0.2 * f64::from(step)).0)
        .collect();
    let (low, high) = across.iter().fold(
        (f64::INFINITY, f64::NEG_INFINITY),
        |(low, high), &height| (mathf::fmin(low, height), mathf::fmax(high, height)),
    );
    assert!(
        high - low > 0.5,
        "distributaries cut its fan: {low} to {high}"
    );
}
