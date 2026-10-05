//! Host tests of a limb's flared foot: it swells most at the ground toward
//! each lobe, fades smoothly to round before its reach, never passes its
//! most, rises no faster than its bound, and refuses what is not a flare.

use core::f64::consts::{PI, TAU};

use super::*;

const GROUND: f64 = 0.06;
const REACH: f64 = 1.2;

fn lobe(angle: f64, out: f64) -> Lobe {
    Lobe {
        angle: crate::vector::single(angle),
        out: crate::vector::single(out),
        width: 0.45,
        climb: 0.25,
    }
}

fn flare() -> Flare {
    Flare::new(
        GROUND,
        REACH,
        (0.3, 0.35),
        &[lobe(0.4, 0.6), lobe(2.1, 0.45), lobe(-2.3, 0.7)],
    )
    .expect("a flare")
}

#[test]
fn the_foot_swells_most_toward_a_lobe_at_the_ground() {
    let flare = flare();
    let toward = flare.factor(GROUND, 0.4);
    let between = flare.factor(GROUND, 0.4 + 0.9);
    assert!((toward - 1.9).abs() < 0.02, "{toward}");
    assert!((between - 1.3).abs() < 0.02, "{between}");
    // Higher up it is rounder, and above its reach round.
    assert!(flare.factor(GROUND + 0.3, 0.4) < toward - 0.3);
    assert!((flare.factor(REACH, 0.4) - 1.0).abs() < 1e-12);
    assert!((flare.factor(REACH + 0.5, -2.3) - 1.0).abs() < 1e-12);
}

#[test]
fn the_flare_fades_smoothly_to_round_and_never_passes_its_most() {
    let flare = flare();
    let most = flare.most();
    let mut last = f64::INFINITY;
    for step in 0..=600u32 {
        let up = REACH * f64::from(step) / 600.0;
        let ridge = flare.factor(up, -2.3);
        assert!(ridge <= most + 1e-12, "{up}: {ridge} past {most}");
        if up >= GROUND {
            assert!(ridge <= last + 1e-12, "{up}: it narrows up the limb");
        }
        last = ridge;
        for spoke in 0..64u32 {
            let angle = TAU * f64::from(spoke) / 64.0 - PI;
            assert!(flare.factor(up, angle) >= 1.0 - 1e-12);
        }
    }
}

#[test]
fn the_flare_rises_no_faster_than_its_bound() {
    let (flare, radius) = (flare(), 0.4);
    let bound = flare.steepest(radius);
    let mut steepest = 0.0f64;
    let step = 1e-4;
    for row in 0..400u32 {
        let up = REACH * f64::from(row) / 400.0;
        for spoke in 0..720u32 {
            let angle = TAU * f64::from(spoke) / 720.0;
            let here = radius * flare.factor(up, angle);
            let along = (radius * flare.factor(up + step, angle) - here) / step;
            // Round the limb a metre is `1/radius` radians where it is
            // round, and fewer out on a lobe.
            let round = (radius * flare.factor(up, angle + step / radius) - here) / step;
            steepest = steepest.max(along.abs() + round.abs());
        }
    }
    assert!(steepest < bound, "rises {steepest} against {bound}");
}

#[test]
fn what_is_not_a_flare_is_refused() {
    let lobes = [lobe(0.0, 0.5); MOST_LOBES + 1];
    assert!(Flare::new(GROUND, REACH, (0.3, 0.35), &lobes).is_none());
    assert!(Flare::new(GROUND, REACH, (0.3, 0.35), &lobes[..MOST_LOBES]).is_some());
    assert!(Flare::new(GROUND, 0.0, (0.3, 0.35), &[]).is_none());
    assert!(Flare::new(-0.1, REACH, (0.3, 0.35), &[]).is_none());
    assert!(Flare::new(GROUND, REACH, (f64::NAN, 0.35), &[]).is_none());
    let mut thin = lobe(0.0, 0.5);
    thin.width = 0.0;
    assert!(Flare::new(GROUND, REACH, (0.3, 0.35), &[thin]).is_none());
}
