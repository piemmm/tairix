//! Host tests of the sun and the full moon: their sizes, their light, and
//! the sun's darkening limb.

use super::*;

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

#[test]
fn the_full_moon_is_its_magnitude_bright_and_nearer_and_larger_overhead() {
    let level = full_moon(Vec3::new(1.0, 0.0, 0.0));
    let high = full_moon(Vec3::UP);
    let ((low_radius, _, limb), (high_radius, ..)) = (disc(&level), disc(&high));
    assert_eq!(limb, Limb::Even, "lit square on, evenly bright");
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
