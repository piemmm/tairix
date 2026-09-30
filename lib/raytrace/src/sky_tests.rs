//! Host tests of the sky.

use tairix_util::mathf;

use super::{CloudLook, Clouds, Glow, Sky};
use crate::scene::Grid;
use crate::vector::Vec3;

fn plain() -> Sky {
    Sky {
        zenith: Vec3::new(0.1, 0.2, 0.6),
        horizon: Vec3::new(0.7, 0.75, 0.8),
        ground: Vec3::splat(0.2),
        glow: None,
        stars: 0.0,
        clouds: None,
    }
}

#[test]
fn the_sky_runs_from_the_horizon_up_to_the_zenith_and_down_to_the_ground() {
    let sky = plain();
    assert!((sky.radiance(Vec3::ZERO, Vec3::UP, true) - sky.zenith).length() < 1e-12);
    assert!(
        (sky.radiance(Vec3::ZERO, Vec3::new(1.0, 0.0, 0.0), true) - sky.horizon).length() < 1e-12
    );
    assert!((sky.radiance(Vec3::ZERO, -Vec3::UP, true) - sky.ground).length() < 1e-12);
    // Blue deepens steadily with height.
    let mut last = sky.radiance(Vec3::ZERO, Vec3::new(1.0, 0.0, 0.0), true).x;
    for step in 1..=20u32 {
        let up = f64::from(step) / 20.0;
        let dir = Vec3::new(mathf::sqrt(1.0 - up * up), up, 0.0);
        let red = sky.radiance(Vec3::ZERO, dir, true).x;
        assert!(red <= last + 1e-12);
        last = red;
    }
}

#[test]
fn the_glow_is_brightest_toward_the_sun() {
    let toward = Vec3::new(0.0, 0.2, 1.0).normalized();
    let sky = Sky {
        glow: Some(Glow {
            toward,
            colour: Vec3::new(1.0, 0.6, 0.3),
            horizon: 1.0,
        }),
        ..plain()
    };
    let at_sun = sky.radiance(Vec3::ZERO, toward, true).x;
    let aside = sky
        .radiance(Vec3::ZERO, Vec3::new(1.0, 0.2, 0.0).normalized(), true)
        .x;
    let behind = sky
        .radiance(Vec3::ZERO, Vec3::new(0.0, 0.2, -1.0).normalized(), true)
        .x;
    assert!(
        at_sun > aside && aside >= behind,
        "{at_sun} {aside} {behind}"
    );
}

fn starry() -> Sky {
    Sky {
        zenith: Vec3::ZERO,
        horizon: Vec3::ZERO,
        ground: Vec3::ZERO,
        glow: None,
        stars: 1.0,
        clouds: None,
    }
}

/// Directions over the upper sky at a spacing finer than a star.
fn upper_sky(steps: u32) -> impl Iterator<Item = Vec3> {
    (0..steps).flat_map(move |i| {
        (0..steps).map(move |j| {
            let a = f64::from(i) / f64::from(steps);
            let b = f64::from(j) / f64::from(steps);
            // A patch of sky around one direction, some 0.25 radians across.
            Vec3::new(0.3 + 0.25 * a, 0.8, 0.1 + 0.25 * b).normalized()
        })
    })
}

#[test]
fn stars_are_scattered_small_and_never_below_the_horizon() {
    let sky = starry();
    let lit = upper_sky(600)
        .filter(|dir| sky.radiance(Vec3::ZERO, *dir, true).x > 0.0)
        .count();
    let total = 600 * 600;
    // A few percent of the sky shows a star's light: points, not a haze.
    assert!(lit > total / 2000 && lit < total / 20, "{lit} of {total}");
    for dir in upper_sky(60) {
        let below = Vec3::new(dir.x, -dir.y, dir.z);
        assert!(sky.radiance(Vec3::ZERO, below, true).max_element() <= 0.0);
    }
}

/// Every star's light fades smoothly all the way round it, so none is cut
/// off by the grid its cell belongs to.
#[test]
fn no_star_is_cut_by_the_edge_of_its_cell() {
    let sky = starry();
    let mut stars = 0;
    for dir in upper_sky(700) {
        let here = sky.radiance(Vec3::ZERO, dir, true).x;
        if here < 0.05 {
            continue;
        }
        stars += 1;
        // A step of a tenth of a star's radius is far too small to cross
        // from full light to none.
        let step = 0.000_11;
        for (dx, dz) in [(step, 0.0), (-step, 0.0), (0.0, step), (0.0, -step)] {
            let beside = sky
                .radiance(
                    Vec3::ZERO,
                    (dir + Vec3::new(dx, 0.0, dz)).normalized(),
                    true,
                )
                .x;
            assert!(
                (beside - here).abs() < 0.5 * here,
                "a cut star at {dir:?}: {here} beside {beside}"
            );
        }
    }
    assert!(stars > 10, "only {stars} bright star samples");
}

/// A cloud layer whose cover is `cover` everywhere.
fn overcast(cover: f32) -> Clouds {
    let look = CloudLook {
        altitude: 1000.0,
        threshold: 0.0,
        softness: 0.2,
        depth: 3.0,
        shade: Vec3::splat(0.6),
        sunlight: Vec3::splat(1.0),
        toward: Vec3::new(0.0, 0.6, 0.8),
        detail: 200.0,
        seed: 1,
    };
    let mut clouds = Clouds::new(16, 20_000.0, look).expect("a layer");
    let side = clouds.rows();
    for (_, band) in clouds.bands(0..side, side) {
        band.fill(cover);
    }
    clouds.seal();
    clouds
}

#[test]
fn a_cloud_hides_the_sky_above_and_shades_the_ground_below() {
    let clear = plain();
    let cloudy = Sky {
        clouds: Some(overcast(1.0)),
        ..plain()
    };
    let up = Vec3::new(0.0, 0.8, 0.6).normalized();
    let seen = cloudy.radiance(Vec3::ZERO, up, false);
    assert!(
        (seen - clear.radiance(Vec3::ZERO, up, false)).length() > 0.1,
        "the cloud is seen"
    );
    let toward = Vec3::new(0.0, 0.6, 0.8);
    let shaded = cloudy.cloud_shadow(Vec3::ZERO, toward);
    assert!(shaded < 0.2, "{shaded}");
    assert!((clear.cloud_shadow(Vec3::ZERO, toward) - 1.0).abs() < 1e-12);
    // Above the layer, and looking down, there is no cloud in the way.
    assert!((cloudy.cloud_shadow(Vec3::new(0.0, 2000.0, 0.0), toward) - 1.0).abs() < 1e-12);
    let down = Vec3::new(0.0, -0.5, 0.8).normalized();
    assert_eq!(
        cloudy.radiance(Vec3::ZERO, down, false),
        clear.radiance(Vec3::ZERO, down, false)
    );
    // The scene's scattered light greys as the cloud covers the sky.
    assert!(
        (cloudy.ambient() - Vec3::splat(0.6)).length()
            < (clear.ambient() - Vec3::splat(0.6)).length()
    );
}

#[test]
fn a_clear_patch_lets_the_sun_through_and_shows_the_sky() {
    let broken = Sky {
        clouds: Some(overcast(-1.0)),
        ..plain()
    };
    let toward = Vec3::new(0.0, 0.6, 0.8);
    assert!((broken.cloud_shadow(Vec3::ZERO, toward) - 1.0).abs() < 1e-12);
    let up = Vec3::new(0.3, 0.8, 0.5).normalized();
    assert_eq!(
        broken.radiance(Vec3::ZERO, up, false),
        plain().radiance(Vec3::ZERO, up, false)
    );
}

#[test]
fn haze_takes_the_horizon_and_a_share_of_the_glow() {
    let toward = Vec3::new(0.0, 0.05, 1.0).normalized();
    let sky = Sky {
        glow: Some(Glow {
            toward,
            colour: Vec3::new(1.0, 0.6, 0.3),
            horizon: 1.0,
        }),
        ..plain()
    };
    let level = Vec3::new(0.0, 0.0, 1.0);
    let haze = sky.haze(level);
    let horizon = sky.radiance(Vec3::ZERO, level, false);
    assert!(
        haze.x > sky.horizon.x && haze.x < horizon.x,
        "{haze:?} under {horizon:?}"
    );
    assert_eq!(plain().haze(level), plain().horizon);
}
