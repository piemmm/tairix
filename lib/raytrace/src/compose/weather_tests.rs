//! Host tests of the weather's moon: by night the sun lies set below it as
//! far as its phase has it, and by day or at dusk a moon stands above the
//! horizon at an elongation its hour allows, lighting nothing.

use alloc::vec::Vec;

use super::*;
use crate::light::Light;

/// The unit direction `elevation` degrees up at compass angle `heading`.
fn raised(heading: f64, elevation: f64) -> Vec3 {
    direction(heading.to_radians(), 0.0, elevation.to_radians())
}

#[test]
fn by_night_the_sun_lies_set_below_the_moon_as_far_as_its_phase_has_it() {
    let mut phases = Vec::new();
    for seed in 0..400u64 {
        let mut dice = Dice::keyed(seed, 0);
        let moon = raised(dice.range(0.0, 360.0), dice.range(20.0, 50.0));
        let sun = set_beneath(&mut dice, moon);
        assert!((sun.length() - 1.0).abs() < 1e-9, "{seed}");
        assert!(
            sun.y < -mathf::sin(14.9f64.to_radians()),
            "{seed}: the sun at {}",
            sun.y
        );
        phases.push(180.0 - mathf::acos(moon.dot(sun).clamp(-1.0, 1.0)).to_degrees());
    }
    let full = phases.iter().filter(|&&phase| phase < 60.0).count();
    let crescent = phases.iter().filter(|&&phase| phase > 110.0).count();
    assert!(
        full > crescent && crescent > 10,
        "{full} near full, {crescent} crescents"
    );
}

#[test]
fn a_moon_by_day_or_at_dusk_stands_above_the_horizon_and_lights_nothing() {
    let sun = raised(30.0, 35.0);
    let (mut risen_by_day, mut at_dusk) = (0, 0);
    for seed in 0..300u64 {
        let mut dice = Dice::keyed(seed, 1);
        if let Some(Light::Sun { toward, .. }) = risen(&mut dice, Hour::Day, sun) {
            risen_by_day += 1;
            let apart = mathf::acos(toward.dot(sun)).to_degrees();
            assert!((44.9..150.1).contains(&apart), "{seed}: {apart}");
            assert!(toward.y >= mathf::sin(7.9f64.to_radians()), "{seed}");
        }
        let set = raised(200.0, -3.0);
        if let Some(Light::Sun { toward, .. }) = risen(&mut dice, Hour::Dusk, set) {
            at_dusk += 1;
            let apart = mathf::acos(toward.dot(set)).to_degrees();
            assert!((11.9..55.1).contains(&apart), "a crescent: {apart}");
        }
        assert!(risen(&mut dice, Hour::Night, sun).is_none());
    }
    assert!((40..140).contains(&risen_by_day), "{risen_by_day} by day");
    assert!(at_dusk > 60, "{at_dusk} at dusk");
}
