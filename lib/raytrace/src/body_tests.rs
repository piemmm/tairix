//! Host tests of the sun and the moon: their sizes, their light, the sun's
//! darkening limb, and the moon's phases and earthshine.

use super::*;
use crate::vector::Frame;

fn disc(light: &Light) -> (f64, Vec3, Limb) {
    let Light::Sun {
        cos_radius,
        radiance,
        limb,
        ..
    } = *light
    else {
        panic!("a disc: {light:?}");
    };
    (mathf::acos(cos_radius), radiance, limb)
}

#[test]
fn the_sun_is_half_a_degree_across_and_brings_the_sunlight_above_the_air() {
    let sun = sun(Vec3::UP);
    let (radius, _, limb) = disc(&sun);
    // 959 seconds of arc: the solar radius over the astronomical unit.
    assert!(
        (radius.to_degrees() * 3600.0 - 959.2).abs() < 0.5,
        "{radius}"
    );
    let irradiance = sun.irradiance();
    assert!(
        ((irradiance - sunlight()).max_element()).abs() < 1e-9 * SOLAR,
        "{irradiance:?}"
    );
    assert!(matches!(limb, Limb::Darkening(_)));
}

/// The full moon, the sun opposite it.
fn full_moon(toward: Vec3) -> Light {
    moon(toward, -toward)
}

/// The moon high in the south, `elongation` degrees from the sun.
fn moon_at(elongation: f64) -> Light {
    let toward = Vec3::new(0.0, 0.8, -0.6);
    let side = Vec3::new(1.0, 0.0, 0.0);
    let apart = elongation.to_radians();
    moon(
        toward,
        (toward * mathf::cos(apart) + side * mathf::sin(apart)).normalized(),
    )
}

/// The moon's radiance at `(u, v)` across its disc, in its radii along its
/// frame's x and y, as a share of its mean.
fn radiance_at(light: &Light, (u, v): (f64, f64)) -> Vec3 {
    let Light::Sun {
        toward,
        cos_radius,
        limb,
        ..
    } = *light
    else {
        panic!("a disc");
    };
    let radius = mathf::sqrt(1.0 - cos_radius * cos_radius);
    let frame = Frame::around(toward);
    let source = (toward + (frame.x * u + frame.y * v) * radius).normalized();
    limb.profile((toward, radius), [source; 3])
}

#[test]
fn the_full_moon_is_its_magnitude_bright_and_nearer_and_larger_overhead() {
    let level = full_moon(Vec3::new(1.0, 0.0, 0.0));
    let high = full_moon(Vec3::UP);
    let ((low_radius, _, _), (high_radius, ..)) = (disc(&level), disc(&high));
    // Lit square on, evenly bright.
    for place in [(0.0, 0.0), (0.5, 0.3), (-0.2, -0.8), (0.9, 0.0)] {
        let shine = radiance_at(&level, place);
        assert!((shine.y - 1.0).abs() < 0.01, "{place:?}: {shine:?}");
    }
    // Its mean semi-diameter, 15′ 32.6″, from the Earth's centre; toward the
    // horizon the eye is about as far from it.
    assert!(
        (low_radius.to_degrees() * 60.0 - 15.54).abs() < 0.02,
        "{low_radius}"
    );
    assert!(high_radius > low_radius * 1.015, "a radius nearer overhead");
    let share = level.irradiance().y / sunlight().y;
    // Fourteen magnitudes fainter than the sun: ten to the minus 5.6.
    let expected = 2.511_886_431_509_58e-6;
    assert!((share / expected - 1.0).abs() < 2e-3, "{share} {expected}");
    assert!(high.irradiance().y > 1.03 * level.irradiance().y);
}

/// The light of `light`'s sunlit part alone: its whole light less the
/// Earth's, which lights the disc evenly, as its dark part shows.
fn sunlit_part(light: &Light) -> f64 {
    light.irradiance().y * (1.0 - radiance_at(light, (-0.95, 0.0)).y)
}

#[test]
fn the_moon_wanes_by_its_phase_its_lit_part_toward_the_sun() {
    let full = moon_at(180.0).irradiance().y;
    // Allen's law: a quarter 2.60 magnitudes below full, a crescent 30° from
    // the sun 5.93, and one 12° from it 7.55, whose sunlit part brings hardly
    // ten times the Earth's light on it.
    for (elongation, magnitudes) in [(90.0, 2.6024), (30.0, 5.925), (12.0, 7.5544)] {
        let share = sunlit_part(&moon_at(elongation)) / full;
        assert!(
            (share / fainter(magnitudes) - 1.0).abs() < 1e-3,
            "{elongation}: {share}"
        );
    }
    // The sun stands toward the frame's x of a moon lit from its side: that
    // half bright, the other dark but for the Earth's light.
    let lit = radiance_at(&moon_at(90.0), (0.6, 0.0)).y;
    let dark = radiance_at(&moon_at(90.0), (-0.6, 0.0)).y;
    assert!(lit > 1.0 && dark < 1e-2 * lit, "{lit} lit, {dark} dark");
}

#[test]
fn a_crescents_dark_part_glows_with_the_earths_light() {
    let shown = |elongation: f64| {
        let light = moon_at(elongation);
        let dark = radiance_at(&light, (-0.7, 0.1));
        let lit = (0..40)
            .map(|step| radiance_at(&light, (0.99 - f64::from(step) * 0.002, 0.0)).y)
            .fold(0.0, f64::max);
        (dark, lit)
    };
    let (dark, lit) = shown(25.0);
    let ratio = dark.y / lit;
    // A few thousandths of the crescent's brightness, as the eye sees the old
    // moon in the new moon's arms.
    assert!((1e-4..1e-2).contains(&ratio), "{ratio}");
    assert!(
        dark.z > dark.y && dark.y > dark.x,
        "bluer than moonlight: {dark:?}"
    );
    // The thinner the crescent, the fuller the Earth the moon sees.
    let (quarter_dark, quarter_lit) = shown(90.0);
    assert!(ratio > 2.0 * quarter_dark.y / quarter_lit, "{ratio}");
}

#[test]
fn at_new_moon_only_the_earths_light_remains() {
    let full = moon_at(180.0).irradiance().y;
    for elongation in [0.0, 0.2] {
        let light = moon_at(elongation);
        // The full Earth's light on the moon: a ten-thousandth of the sun's.
        let share = light.irradiance().y / full;
        assert!((0.9e-4..1.3e-4).contains(&share), "{elongation}: {share}");
        let middle = radiance_at(&light, (0.0, 0.0));
        assert!(
            middle.is_finite() && middle.y > 0.5,
            "{elongation}: {middle:?}"
        );
    }
}

#[test]
fn the_moons_disc_brings_its_whole_light_at_any_phase() {
    // Down to the thinnest crescent the grid below resolves, and new moon.
    for elongation in [180.0, 120.0, 90.0, 40.0, 15.0, 0.0] {
        let light = moon_at(elongation);
        let (mut sum, mut inside) = (Vec3::ZERO, 0u32);
        for row in 0..600u32 {
            for column in 0..600u32 {
                let (u, v) = (
                    (f64::from(column) + 0.5) / 300.0 - 1.0,
                    (f64::from(row) + 0.5) / 300.0 - 1.0,
                );
                if u * u + v * v <= 1.0 {
                    sum += radiance_at(&light, (u, v));
                    inside += 1;
                }
            }
        }
        let mean = sum * (1.0 / f64::from(inside));
        assert!((mean.y - 1.0).abs() < 0.02, "{elongation}: {mean:?}");
    }
}

#[test]
fn moonlight_is_warmer_than_the_sunlight_it_reflects() {
    let (moon, above) = (full_moon(Vec3::UP).irradiance(), sunlight());
    assert!(moon.x / moon.y > above.x / above.y, "{moon:?}");
    assert!(moon.z / moon.y < above.z / above.y, "{moon:?}");
}

#[test]
fn the_limb_darkens_most_in_the_blue_as_the_measured_profiles_say() {
    assert!((limb_exponent(579.88) - 0.477).abs() < 1e-12);
    assert!((limb_exponent(416.319) - 0.724).abs() < 1e-12);
    let exponents = solar_limb();
    assert!(
        exponents.z > exponents.y && exponents.y > exponents.x,
        "{exponents:?}"
    );
    // Between the profiles either side of 550 nm.
    assert!((0.496..0.514).contains(&exponents.y), "{exponents:?}");
}

#[test]
fn a_lux_is_the_sunlights_share_of_its_illuminance() {
    let lux = sunlight().luminance() / per_lux();
    assert!((lux - SOLAR_ILLUMINANCE).abs() < 1e-6, "{lux}");
    assert!((fainter(5.0) - 0.01).abs() < 1e-15);
}
