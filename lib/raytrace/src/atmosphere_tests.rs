//! Host tests of the atmosphere: its light kept and scattered as physics
//! has it, and its tables the same however they are built.

use super::*;

fn air(elevation: f64) -> Air {
    let radians = elevation.to_radians();
    Air {
        sun: Vec3::new(0.0, mathf::sin(radians), mathf::cos(radians)),
        solar: Vec3::splat(20.0),
        base: 100.0,
        haze: 1.0,
        albedo: Vec3::splat(0.2),
        eye: Vec3::new(0.0, 2.0, 0.0),
    }
}

fn built(air: Air, runner: &dyn JobRunner) -> Atmosphere {
    let mut atmosphere = Atmosphere::new(air).expect("tables fit");
    while !atmosphere.step(runner) {}
    atmosphere
}

#[test]
fn the_air_keeps_most_light_overhead_and_reddens_it_toward_the_horizon() {
    let atmosphere = built(air(40.0), &tairix_parallel::SERIAL);
    let overhead = atmosphere.sunlight(0.0, Vec3::UP);
    assert!(overhead.min(Vec3::splat(1.0)).z > 0.6, "{overhead:?}");
    let low = atmosphere.sunlight(0.0, Vec3::new(0.0, mathf::sin(0.05), mathf::cos(0.05)));
    assert!(low.x > low.y && low.y > low.z, "reddened: {low:?}");
    assert!(
        low.z < 0.5 * overhead.z,
        "dimmed: {low:?} against {overhead:?}"
    );
}

#[test]
fn the_sky_is_blue_overhead_by_day_and_red_by_the_setting_sun() {
    let noon = built(air(55.0), &tairix_parallel::SERIAL);
    let zenith = noon.sky(Vec3::UP);
    assert!(
        zenith.z > zenith.y && zenith.y > zenith.x,
        "blue: {zenith:?}"
    );
    let setting = built(air(1.0), &tairix_parallel::SERIAL);
    let near = setting.sky(Vec3::new(0.0, 0.02, 1.0).normalized());
    assert!(near.x > near.z, "red toward a setting sun: {near:?}");
    assert!(setting.ambient().max_element() > 0.0);
}

#[test]
fn after_sunset_the_air_above_is_still_lit_while_the_ground_lies_in_shadow() {
    let dusk = built(air(-2.0), &tairix_parallel::SERIAL);
    let toward = dusk.air().sun;
    let ground = dusk.sunlight(0.0, toward);
    assert!(
        ground.max_element() < 1e-3,
        "the ground is in the Earth's shadow: {ground:?}"
    );
    let high = dusk.sunlight(7000.0, toward);
    assert!(
        high.max_element() > 1e-3,
        "a high cloud still sees the sun: {high:?}"
    );
    assert!(high.x > high.z, "and sees it red: {high:?}");
}

#[test]
fn the_air_between_dims_what_lies_beyond_and_glows_the_more_the_further() {
    let atmosphere = built(air(30.0), &tairix_parallel::SERIAL);
    let dir = Vec3::new(1.0, 0.02, 0.3).normalized();
    let near = atmosphere.between(dir, 0.0);
    assert!(
        near.light().max_element() < 1e-9 && (near.kept - Vec3::ONE).max_element().abs() < 1e-9
    );
    let mut last = (Vec3::ZERO, Vec3::ONE);
    for distance in [100.0, 1_000.0, 5_000.0, 20_000.0, 50_000.0] {
        let between = atmosphere.between(dir, distance);
        let (glow, kept) = (between.light(), between.kept);
        assert!(
            kept.max_element() <= last.1.max_element() + 1e-6,
            "{distance}"
        );
        let risen = glow - last.0;
        assert!(risen.x.min(risen.y).min(risen.z) >= -1e-6, "{distance}");
        last = (glow, kept);
    }
    assert!(
        last.1.z < 0.9,
        "fifty kilometres of air dims blue: {:?}",
        last.1
    );
}

#[test]
fn toward_a_low_sun_the_air_glows_with_the_suns_light_which_a_shadow_takes_away() {
    let atmosphere = built(air(8.0), &tairix_parallel::SERIAL);
    let (toward, away) = (
        air(8.0).sun,
        Vec3::new(0.0, air(8.0).sun.y, -air(8.0).sun.z),
    );
    let (sunward, backward) = (
        atmosphere.between(toward, 300.0),
        atmosphere.between(away, 300.0),
    );
    assert!(
        sunward.sun.max_element() > 3.0 * sunward.sky.max_element(),
        "the sun's own light glows toward it: {sunward:?}"
    );
    assert!(
        sunward.sun.max_element() > 3.0 * backward.sun.max_element(),
        "and far less away from it: {backward:?}"
    );
    let ground = Vec3::splat(0.1);
    let open = atmosphere.aerial(
        toward,
        300.0,
        ground,
        Lit {
            sun: Vec3::ONE,
            sky: 1.0,
        },
    );
    let shadowed = atmosphere.aerial(
        toward,
        300.0,
        ground,
        Lit {
            sun: Vec3::ZERO,
            sky: 1.0,
        },
    );
    let expected = ground * sunward.kept + sunward.sky;
    assert!(
        (shadowed - expected).max_element().abs() < 1e-12,
        "{shadowed:?}"
    );
    assert!(
        ((open - shadowed) - sunward.sun).max_element().abs() < 1e-12,
        "a shadow takes the sun's glow and nothing else: {open:?} {shadowed:?}"
    );
}

#[test]
fn tables_built_across_workers_match_one_built_alone() {
    let alone = built(air(12.0), &tairix_parallel::SERIAL);
    let spread = built(air(12.0), &tairix_parallel::Threaded::new(3));
    for step in 0..40u32 {
        let angle = f64::from(step) * 0.37;
        let dir = Vec3::new(
            mathf::cos(angle),
            f64::from(step) / 40.0 - 0.1,
            mathf::sin(angle),
        )
        .normalized();
        assert_eq!(alone.sky(dir), spread.sky(dir));
        assert_eq!(alone.between(dir, 3_000.0), spread.between(dir, 3_000.0));
    }
}
