//! Host tests of the two-stream slab: it conserves the light a lossless
//! slab takes in, glows evenly under even light, is brightest toward the
//! face the sun falls on, scatters little more than once when thin, and
//! stays finite however deep.

use super::*;

const CLOUD: Medium = Medium {
    albedo: 0.999_99,
    asymmetry: 0.84,
};

const SUN: Vec3 = Vec3::new(1.0, 1.0, 1.0);

/// The upward diffuse flux at the slab's top and the downward at its base,
/// from `diffuse` read at each.
fn fluxes(top: Diffuse, base: Diffuse) -> (f64, f64) {
    (
        PI * (top.mean.x + 2.0 / 3.0 * top.rising.x),
        PI * (base.mean.x - 2.0 / 3.0 * base.rising.x),
    )
}

#[test]
fn a_lossless_slab_sends_back_or_through_all_the_sun_it_takes_in() {
    for whole in [0.5, 2.0, 10.0, 60.0] {
        for cosine in [0.2, 0.5, 0.9] {
            let at = |below_top| sunlit(CLOUD, Depth { below_top, whole }, (SUN, cosine));
            let (up, down) = fluxes(at(0.0), at(whole));
            let thinned = 1.0 - CLOUD.asymmetry * CLOUD.asymmetry * CLOUD.albedo;
            let direct = cosine * mathf::exp(-thinned * whole / cosine);
            let total = up + down + direct;
            assert!(
                (total - cosine).abs() < 0.01 * cosine,
                "{whole} {cosine}: {up} + {down} + {direct} = {total}"
            );
            assert!(up >= -1e-9 && down >= -1e-9);
        }
    }
}

#[test]
fn a_thick_cloud_reflects_most_of_the_sun_and_lets_little_through() {
    let at = |below_top| {
        sunlit(
            CLOUD,
            Depth {
                below_top,
                whole: 80.0,
            },
            (SUN, 0.7),
        )
    };
    let (up, down) = fluxes(at(0.0), at(80.0));
    assert!(up > 0.55 * 0.7 && down < 0.35 * 0.7, "{up} {down}");
    // Brightest toward the face the sun falls on, dimming deeper in.
    assert!(at(5.0).mean.x > at(40.0).mean.x && at(40.0).mean.x > at(78.0).mean.x);
    // And diffusing back upward near its top, downward near its base.
    assert!(at(0.5).rising.x > 0.0 && at(79.5).rising.x < 0.0);
}

#[test]
fn a_thin_cloud_scatters_little_of_the_sun_more_than_once() {
    let mean = |whole: f64| {
        sunlit(
            CLOUD,
            Depth {
                below_top: 0.5 * whole,
                whole,
            },
            (SUN, 0.8),
        )
        .mean
        .x
    };
    assert!(
        mean(0.02) < 0.05 * mean(20.0),
        "{} {}",
        mean(0.02),
        mean(20.0)
    );
    assert!(mean(0.02) >= 0.0);
}

#[test]
fn an_evenly_lit_slab_glows_evenly() {
    let sky = Vec3::new(0.3, 0.4, 0.6);
    for whole in [0.1, 5.0, 50.0] {
        for share in [0.0, 0.3, 0.7, 1.0] {
            let diffuse = skylit(
                CLOUD,
                Depth {
                    below_top: share * whole,
                    whole,
                },
                (sky, sky),
            );
            assert!(
                (diffuse.mean - sky).length() < 0.02 * sky.length(),
                "{whole} {share}: {:?}",
                diffuse.mean
            );
            assert!(diffuse.rising.length() < 0.02 * sky.length());
        }
    }
    // Lit from above alone, it is brightest near its top.
    let at = |below_top| {
        skylit(
            CLOUD,
            Depth {
                below_top,
                whole: 30.0,
            },
            (sky, Vec3::ZERO),
        )
    };
    assert!(at(1.0).mean.z > at(29.0).mean.z);
}

#[test]
fn the_sun_below_the_horizon_lights_nothing_and_no_depth_overflows() {
    let dark = sunlit(
        CLOUD,
        Depth {
            below_top: 3.0,
            whole: 10.0,
        },
        (SUN, -0.2),
    );
    assert_eq!(dark.mean, Vec3::ZERO);
    for whole in [1e-6, 1.0, 1e3, 1e6] {
        for share in [0.0, 0.5, 1.0] {
            let depth = Depth {
                below_top: share * whole,
                whole,
            };
            let lit = sunlit(CLOUD, depth, (SUN, 0.6));
            let sky = skylit(CLOUD, depth, (SUN, SUN));
            for value in [lit.mean, lit.rising, sky.mean, sky.rising] {
                assert!(value.is_finite(), "{whole} {share}: {value:?}");
            }
        }
    }
}

#[test]
fn what_it_scatters_toward_the_eye_leans_with_the_diffuse_flow() {
    let diffuse = Diffuse {
        mean: Vec3::splat(1.0),
        rising: Vec3::splat(0.5),
    };
    let (up, down) = (
        diffuse.scattered(CLOUD, 1.0),
        diffuse.scattered(CLOUD, -1.0),
    );
    assert!(up.x > down.x);
    assert!((diffuse.scattered(CLOUD, 0.0).x - CLOUD.albedo).abs() < 1e-12);
}
