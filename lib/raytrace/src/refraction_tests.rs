//! Host tests of the bending air: the standard atmosphere's density, the
//! dispersion of its index, and the refraction a ray takes leaving it.

use super::*;
use crate::atmosphere::{GROUND, TOP};

fn clear(_: f64) -> Vec3 {
    Vec3::ZERO
}

fn green() -> Refraction {
    Refraction::new(refractivity(0.550))
}

/// How far `refraction` lifts a ray leaving `height` kilometres up at
/// `elevation` degrees above the level, in minutes of arc.
fn lift(refraction: &Refraction, height: f64, elevation: f64) -> f64 {
    let zenith = (90.0 - elevation).to_radians();
    let path = refraction
        .trace((GROUND + height, mathf::cos(zenith)), (GROUND, TOP), &clear)
        .expect("the ray leaves the air");
    (path.zenith - zenith).to_degrees() * 60.0
}

#[test]
fn standard_air_bends_blue_more_than_red() {
    let (red, green, blue) = (
        refractivity(0.680),
        refractivity(0.550),
        refractivity(0.440),
    );
    assert!((green - 2.7784e-4).abs() < 1e-8, "Ciddor's green: {green}");
    assert!(blue > green && green > red, "{red} {green} {blue}");
}

#[test]
fn the_density_follows_the_standard_atmospheres_tables() {
    let air = green();
    // The 1976 standard's own densities, over its sea level's 1.2250 kg/m³.
    for (height, density) in [
        (0.0, 1.2250),
        (5.0, 0.7364),
        (10.0, 0.4135),
        (15.0, 0.19476),
        (20.0, 0.08891),
        (32.0, 0.013_555),
        (50.0, 0.001_026_9),
    ] {
        let ratio = air.density(height);
        let expected = density / 1.2250;
        assert!(
            (ratio / expected - 1.0).abs() < 1e-3,
            "{height} km: {ratio} against {expected}"
        );
    }
}

#[test]
fn a_ray_bends_most_at_the_horizon_and_hardly_at_all_overhead() {
    let air = green();
    let horizon = lift(&air, 0.0, 0.0);
    // About half a degree at the horizon, as every almanac's table has it.
    assert!((32.0..34.0).contains(&horizon), "{horizon}′");
    let mut last = horizon;
    for elevation in [0.5, 1.0, 2.0, 5.0, 10.0, 20.0, 45.0, 80.0] {
        let now = lift(&air, 0.0, elevation);
        assert!(now < last, "{elevation}°: {now}′ after {last}′");
        last = now;
    }
    // High up, refraction is the refractivity times the zenith's tangent.
    let half = lift(&air, 0.0, 45.0);
    let expected = refractivity(0.550).to_degrees() * 60.0;
    assert!((half / expected - 1.0).abs() < 5e-3, "{half}′ {expected}′");
    let overhead = lift(&air, 0.0, 90.0);
    assert!(overhead.abs() < 1e-6, "{overhead}′");
}

/// A ray rising a hair above the level passes within a hair of where it
/// would run level, where a march from its start alone cannot resolve the
/// bending; and it is just there that the bending changes fastest.
#[test]
fn just_above_the_horizon_a_sources_true_zenith_turns_a_fifth_again_as_fast() {
    let air = green();
    let true_zenith = |elevation: f64| {
        let zenith = (90.0 - elevation).to_radians();
        air.trace((GROUND, mathf::cos(zenith)), (GROUND, TOP), &clear)
            .expect("the ray leaves the air")
            .zenith
    };
    let (mut last_bend, mut last_turning) = (f64::INFINITY, f64::INFINITY);
    for elevation in [0.0, 1e-3, 3e-3, 0.01, 0.03, 0.1, 0.2, 0.5] {
        let step = 1e-3_f64;
        let turning = (true_zenith(elevation) - true_zenith(elevation + step)) / step.to_radians();
        // The refraction falls by a fifth of the change in elevation at the
        // horizon (Bennett's tables), a sixth half a degree up, and the
        // faster the lower.
        assert!(
            (1.15..1.25).contains(&turning) && turning <= last_turning,
            "{elevation}°: {turning} after {last_turning}"
        );
        let bend = lift(&air, 0.0, elevation);
        assert!(bend < last_bend, "{elevation}°: {bend}′ after {last_bend}′");
        (last_bend, last_turning) = (bend, turning);
    }
}

#[test]
fn thinner_air_higher_up_bends_less() {
    let air = green();
    for elevation in [0.0, 1.0, 5.0] {
        let (low, high) = (lift(&air, 0.0, elevation), lift(&air, 2.0, elevation));
        assert!(
            high < low,
            "{elevation}°: {high}′ from 2 km, {low}′ at the sea"
        );
    }
}

#[test]
fn a_ray_heading_down_turns_above_the_ground_or_meets_it() {
    let air = green();
    let height = 2.0;
    // From 2 km the sea's horizon dips some 1.44°: a ray a degree below the
    // level turns above it and leaves the air, bent the more for the dense
    // air it skims.
    let below = lift(&air, height, -1.0);
    assert!(below > lift(&air, height, 0.0), "{below}′");
    let zenith = (90.0 + 2.0_f64).to_radians();
    assert!(
        air.trace((GROUND + height, mathf::cos(zenith)), (GROUND, TOP), &clear)
            .is_none(),
        "two degrees down meets the sea"
    );
}

#[test]
fn without_bending_a_ray_runs_straight_and_crosses_its_chord() {
    let air = Refraction::new(0.0);
    let even = |_: f64| Vec3::splat(0.01);
    for (height, elevation) in [(0.0, 0.0), (0.0, 3.0), (1.5, 30.0), (4.0, -1.5_f64)] {
        let zenith = (90.0 - elevation).to_radians();
        let (r, mu) = (GROUND + height, mathf::cos(zenith));
        let path = air.trace((r, mu), (GROUND, TOP), &even).expect("clear");
        assert!(
            (path.zenith - zenith).abs() < 1e-9,
            "{height} {elevation}: {}",
            path.zenith
        );
        let chord = -r * mu + mathf::sqrt(r * r * (mu * mu - 1.0) + TOP * TOP);
        assert!(
            (path.depth.x / (0.01 * chord) - 1.0).abs() < 1e-6,
            "{height} {elevation}: {} against {}",
            path.depth.x,
            0.01 * chord
        );
    }
}
