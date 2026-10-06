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
    let overhead = atmosphere.sunlight(0.0, 1.0);
    assert!(overhead.min(Vec3::splat(1.0)).z > 0.6, "{overhead:?}");
    let low = atmosphere.sunlight(0.0, mathf::sin(0.05));
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
    let cosine = dusk.air().sun.y;
    let ground = dusk.sunlight(0.0, cosine);
    assert!(
        ground.max_element() < 1e-3,
        "the ground is in the Earth's shadow: {ground:?}"
    );
    let high = dusk.sunlight(7000.0, cosine);
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

/// The table's first 32 slices lie within 60 km, where the land is, and the
/// rest carry it on toward the horizon's cloud, still dimming and glowing.
#[test]
fn the_aerial_table_keeps_its_near_slices_and_reaches_the_horizons_cloud() {
    assert!((place_of(60_000.0) - 31.0).abs() < 1e-9);
    assert!((place_of(AERIAL_REACH * 1000.0) - real(AERIAL.2 - 1)).abs() < 1e-9);
    let atmosphere = built(air(20.0), &tairix_parallel::SERIAL);
    let dir = Vec3::new(0.7, 0.004, 0.7).normalized();
    let mut last = atmosphere.between(dir, 60_000.0);
    for distance in [120_000.0, 220_000.0, 330_000.0, 450_000.0] {
        let between = atmosphere.between(dir, distance);
        assert!(between.kept.y < last.kept.y, "{distance}: dims on");
        assert!(between.light().y > last.light().y, "{distance}: glows on");
        last = between;
    }
}

/// Drawn along a stretch, a point falls where the share it was drawn with
/// of the stretch's sunlit air's light has been gathered, from its near end
/// to its far.
#[test]
fn a_point_drawn_along_the_air_falls_as_its_sunlight_is_gathered() {
    let atmosphere = built(air(6.0), &tairix_parallel::SERIAL);
    for (dir, (from, to)) in [
        (Vec3::new(0.0, 0.06, 1.0).normalized(), (0.0, 140_000.0)),
        (Vec3::new(1.0, 0.3, -0.2).normalized(), (2_500.0, 9_000.0)),
        (Vec3::new(-0.4, 0.01, -1.0).normalized(), (40.0, 400_000.0)),
    ] {
        let sight = atmosphere.sight(dir);
        let green = |distance: f64| sight.between(distance).sun.y;
        let (low, high) = (green(from), green(to));
        assert!(high > low, "{dir:?}");
        let mut last = from;
        for step in 0..=40u32 {
            let u = f64::from(step) / 40.0;
            let at = sight.drawn((from, to), u);
            assert!((from..=to).contains(&at) && at >= last, "{dir:?} {u}: {at}");
            let share = (green(at) - low) / (high - low);
            assert!((share - u).abs() < 1e-6, "{dir:?} {u}: {share} at {at}");
            last = at;
        }
    }
    let sight = atmosphere.sight(Vec3::UP);
    assert_eq!(
        sight.drawn((500.0, 500.0), 0.3).to_bits(),
        500.0f64.to_bits(),
        "an empty stretch"
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
    let open = atmosphere.aerial(toward, 300.0, ground, Lit::OPEN);
    let shadowed = atmosphere.aerial(
        toward,
        300.0,
        ground,
        Lit {
            sun: Vec3::ZERO,
            ..Lit::OPEN
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

/// Low sunlight reaches the air bent toward the vertical, and scatters
/// about the way it arrives rather than the way it set out.
#[test]
fn bent_sunlight_arrives_nearer_the_zenith_and_scatters_about_its_way_in() {
    let atmosphere = built(air(0.5), &tairix_parallel::SERIAL);
    let r = GROUND + 0.01;
    for mu in [0.002, 0.02, 0.2, 0.7] {
        let bent = atmosphere.paths.bent(r, mu).expect("above the shadow");
        let sun = Vec3::new(mathf::sqrt(1.0 - mu * mu), mu, 0.0);
        let (cos_seen, sin_seen) = bent.seen();
        let apparent = Vec3::new(sin_seen, cos_seen, 0.0);
        assert!(cos_seen > mu, "{mu}: lifted to {cos_seen}");
        assert!((apparent.length() - 1.0).abs() < 1e-12);
        assert!((apparent.dot(sun) - mathf::cos(bent.by)).abs() < 1e-12);
        for step in 0..24u32 {
            let around = f64::from(step) * 0.83;
            let dir = Vec3::new(
                mathf::cos(around),
                f64::from(step) / 12.0 - 1.0,
                mathf::sin(around),
            )
            .normalized();
            let toward = bent.toward(dir.dot(sun), dir.y);
            assert!((toward - dir.dot(apparent)).abs() < 1e-12, "{mu} {dir:?}");
        }
    }
}

#[test]
fn the_ambient_is_the_skys_mean_over_the_hemisphere_weighed_by_its_cosine() {
    let mut atmosphere = Atmosphere::new(air(40.0)).expect("tables fit");
    let mut sky = |radiance: &(dyn Fn(f64) -> f64 + Sync)| {
        fill_rows(
            &mut atmosphere.view,
            0..VIEW.1,
            &tairix_parallel::SERIAL,
            &|_, v| Vec3::splat(radiance(elevation_of(v).max(0.0))),
        );
        atmosphere.hemisphere().x
    };
    let even = sky(&|_| 3.0);
    assert!(
        (even - 3.0).abs() < 1e-6,
        "an even sky is its own mean: {even}"
    );
    // Bright as the sine of its elevation, the sky's cosine-weighted mean is
    // the integral of sin²e cos e over that of sin e cos e: two thirds. A
    // texel weighed by its cosine alone gives the zenith more, about 0.72.
    let rising = sky(&mathf::sin);
    assert!((rising - 2.0 / 3.0).abs() < 2e-3, "{rising}");
    // Bright toward the horizon instead, as the cosine of its elevation: two
    // thirds again, where too little weight there gives about 0.57.
    let falling = sky(&mathf::cos);
    assert!((falling - 2.0 / 3.0).abs() < 2e-3, "{falling}");
}
