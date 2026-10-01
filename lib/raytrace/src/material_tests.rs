//! Host tests of the microfacet, Fresnel and thin-film terms, and of relief.

use core::f64::consts::{PI, TAU};

use tairix_util::mathf;

use super::*;
use crate::sample::{mix32, unit};

/// A stream of uniform draws.
fn draws(seed: u32) -> impl FnMut() -> f64 {
    let mut state = seed;
    move || {
        state = mix32(state.wrapping_add(0x6d2b_79f5));
        unit(state)
    }
}

#[test]
fn fresnel_is_the_textbook_value_head_on_and_total_past_the_critical_angle() {
    let head_on = fresnel(1.0, 1.5);
    assert!((head_on - 0.04).abs() < 1e-12, "{head_on}");
    // Leaving glass for air, past the critical angle, all is reflected.
    let critical = mathf::asin(1.0 / 1.5);
    assert!((fresnel(mathf::cos(critical + 0.01), 1.0 / 1.5) - 1.0).abs() < 1e-12);
    assert!(fresnel(mathf::cos(critical - 0.01), 1.0 / 1.5) < 1.0);
    let mut last = 0.0;
    for step in 0..=100u32 {
        let now = fresnel(1.0 - f64::from(step) / 100.0, 1.5);
        assert!(now >= last - 1e-12 && now <= 1.0, "it rises toward grazing");
        last = now;
    }
    assert!((fresnel(0.0, 1.5) - 1.0).abs() < 1e-9);
}

#[test]
fn refraction_obeys_snells_law_and_fails_at_total_internal_reflection() {
    for step in 1..60u32 {
        let angle = f64::from(step) * (PI / 2.0) / 60.0;
        let incident = Vec3::new(mathf::sin(angle), -mathf::cos(angle), 0.0);
        let out = refract(incident, Vec3::UP, 1.5).expect("into denser glass always passes");
        assert!((out.length() - 1.0).abs() < 1e-12);
        assert!(out.y < 0.0, "it carries on through");
        let sin_out = mathf::sqrt(out.x * out.x + out.z * out.z);
        assert!(
            (mathf::sin(angle) - 1.5 * sin_out).abs() < 1e-9,
            "Snell at {angle}"
        );
    }
    let steep = Vec3::new(mathf::sin(1.2), -mathf::cos(1.2), 0.0);
    assert!(refract(steep, Vec3::UP, 1.0 / 1.5).is_none());
    let gentle = Vec3::new(mathf::sin(0.3), -mathf::cos(0.3), 0.0);
    assert!(refract(gentle, Vec3::UP, 1.0 / 1.5).is_some());
}

/// The distribution of facet normals, projected onto the surface, covers it
/// exactly once — ∫ D(h) cos θ dω = 1 — however rough, and however unevenly
/// rough along its two axes.
#[test]
fn the_facet_distribution_covers_the_surface_once() {
    for micro in [
        Microfacet::isotropic(0.15),
        Microfacet::isotropic(0.5),
        Microfacet::isotropic(1.0),
        Microfacet::anisotropic(0.3, 0.6),
        Microfacet::anisotropic(0.7, 0.35),
    ] {
        let (rings, segments) = (3000u32, 256u32);
        let (d_theta, d_phi) = ((PI / 2.0) / f64::from(rings), TAU / f64::from(segments));
        let mut total = 0.0;
        for ring in 0..rings {
            let theta = (f64::from(ring) + 0.5) * d_theta;
            let (sin, cos) = (mathf::sin(theta), mathf::cos(theta));
            for segment in 0..segments {
                let phi = (f64::from(segment) + 0.5) * d_phi;
                let h = Vec3::new(sin * mathf::cos(phi), sin * mathf::sin(phi), cos);
                total += micro.density(h) * cos * sin * d_theta * d_phi;
            }
        }
        assert!((total - 1.0).abs() < 5e-3, "{micro:?}: {total}");
    }
}

#[test]
fn masking_is_whole_head_on_and_fades_toward_grazing() {
    for micro in [
        Microfacet::isotropic(0.05),
        Microfacet::isotropic(0.4),
        Microfacet::anisotropic(0.2, 0.9),
    ] {
        assert!((micro.masking(Vec3::new(0.0, 0.0, 1.0)) - 1.0).abs() < 1e-9);
        let mut last = 1.0;
        for step in 1..=50u32 {
            let lean = f64::from(step) / 50.0 * (PI / 2.0 - 1e-3);
            let now = micro.masking(Vec3::new(mathf::sin(lean), 0.0, mathf::cos(lean)));
            assert!(now <= last + 1e-12 && now >= 0.0, "{micro:?}");
            last = now;
        }
    }
    let smooth = Microfacet::isotropic(0.0);
    assert!(smooth.is_mirror());
    assert!(!Microfacet::isotropic(0.1).is_mirror());
}

#[test]
fn a_visible_normal_faces_both_the_surface_and_the_eye() {
    let mut draw = draws(1);
    for micro in [
        Microfacet::isotropic(0.05),
        Microfacet::isotropic(0.3),
        Microfacet::anisotropic(0.1, 0.8),
    ] {
        for _ in 0..400 {
            let lean = draw() * 1.5;
            let turn = draw() * TAU;
            let view = Vec3::new(
                mathf::sin(lean) * mathf::cos(turn),
                mathf::sin(lean) * mathf::sin(turn),
                mathf::cos(lean),
            );
            let normal = micro.sample(view, (draw(), draw()));
            assert!((normal.length() - 1.0).abs() < 1e-9);
            assert!(normal.z >= 0.0);
            assert!(normal.dot(view) >= -1e-9, "a normal the eye cannot see");
            assert!(micro.reflection_density(view, normal) >= 0.0);
        }
    }
    let smooth =
        Microfacet::isotropic(0.0).sample(Vec3::new(0.3, 0.1, 0.95).normalized(), (0.7, 0.2));
    assert!(
        smooth.z > 0.999,
        "nearly smooth, the normal drawn is the surface's own"
    );
}

#[test]
fn a_brushed_surface_spreads_its_normals_across_the_brushing() {
    let micro = Microfacet::anisotropic(0.08, 0.6);
    let mut draw = draws(2);
    let (mut along, mut across) = (0.0, 0.0);
    for _ in 0..4000 {
        let normal = micro.sample(Vec3::new(0.0, 0.0, 1.0), (draw(), draw()));
        along += normal.x * normal.x;
        across += normal.y * normal.y;
    }
    assert!(across > 10.0 * along, "{along} along, {across} across");
}

#[test]
fn schlick_runs_from_its_base_head_on_to_white_at_grazing() {
    let gold = Vec3::new(1.0, 0.766, 0.336);
    assert!((schlick(gold, 1.0) - gold).length() < 1e-12);
    assert!((schlick(gold, 0.0) - Vec3::ONE).length() < 1e-12);
    assert!((schlick_scalar(COAT_F0, 1.0) - COAT_F0).abs() < 1e-12);
    assert!((schlick_scalar(COAT_F0, 0.0) - 1.0).abs() < 1e-12);
}

#[test]
fn a_film_of_no_thickness_reflects_as_its_bare_boundary_would() {
    let bare = thin_film(1.0, 0.0, 1.33, 1.5);
    for channel in [bare.x, bare.y, bare.z] {
        assert!((channel - fresnel(1.0, 1.5)).abs() < 1e-9, "{bare:?}");
    }
    let bubble = thin_film(1.0, 0.0, 1.33, 1.0);
    assert!(bubble.max_element() < 1e-12, "air to air reflects nothing");
}

#[test]
fn a_quarter_wave_film_cancels_the_light_it_is_cut_for() {
    // A coat of index √1.5 a quarter of green's wavelength thick, on glass:
    // the two reflections cancel in the green and not in the blue or red.
    let index = mathf::sqrt(1.5);
    let coat = thin_film(1.0, 545.0 / (4.0 * index), index, 1.5);
    assert!(coat.y < 0.005, "{coat:?}");
    assert!(coat.y < coat.x && coat.y < coat.z, "{coat:?}");
    let mut draw = draws(3);
    for _ in 0..500 {
        let film = thin_film(draw(), 1000.0 * draw(), 1.2 + draw(), 1.0 + draw());
        for channel in [film.x, film.y, film.z] {
            assert!((0.0..=1.0).contains(&channel), "{film:?}");
        }
    }
}

#[test]
fn dispersion_bends_blue_most_and_red_least() {
    let [red, green, blue] = SPREAD;
    assert!(red < green && green < blue);
    assert!(green.abs() < 1e-12, "the nominal index is green's");
}

#[test]
fn relief_tilts_a_normal_a_little_and_keeps_it_unit() {
    for relief in [
        Relief::ripples(0.02, 1.5, 0.6, 1),
        Relief::Grain {
            depth: 0.15,
            scale: 10.0,
            seed: 2,
        },
        Relief::Bark {
            bark: crate::bark::Bark {
                kind: crate::bark::BarkKind::Furrowed,
                light: Vec3::ONE,
                dark: Vec3::ZERO,
                accent: Vec3::ONE,
                rise: 0.0,
                snow: 0.0,
                moss: 0.0,
                seed: 4,
            },
            depth: 0.004,
        },
    ] {
        let mut moved = 0.0;
        for step in 0..500u32 {
            let p = Vec3::new(f64::from(step) * 0.17, 0.0, f64::from(step) * -0.11);
            let bump = Bump {
                p,
                uv: (p.x, p.z),
                tangent: Vec3::new(1.0, 0.0, 0.0),
                girth: 0.3,
                instance: 0,
                width: 1e-4,
            };
            let tilted = relief.tilt(Vec3::UP, &bump);
            assert!((tilted.length() - 1.0).abs() < 1e-9);
            assert!(tilted.y > 0.8, "{relief:?} tilted too far: {tilted:?}");
            moved += (tilted - Vec3::UP).length();
        }
        assert!(moved > 0.5, "{relief:?} barely tilts");
    }
    let plain = Material::new(Pigment::Solid(Vec3::ONE), Finish::Coated { roughness: 0.2 });
    assert!(plain.relief.is_none());
    assert!(plain
        .with_relief(Relief::ripples(0.01, 1.0, 0.5, 3))
        .relief
        .is_some());
}

/// Bark rising the way a limb's angle grows tilts the normal back the other
/// way, as the slope of a real ridge does: its relief is laid round the limb
/// the way its pattern is.
#[test]
fn bark_relief_leans_the_normal_away_from_where_the_bark_rises() {
    let relief = Relief::Bark {
        bark: crate::bark::Bark {
            kind: crate::bark::BarkKind::Ribbed,
            light: Vec3::ONE,
            dark: Vec3::ZERO,
            accent: Vec3::ONE,
            rise: 0.0,
            snow: 0.0,
            moss: 0.0,
            seed: 1,
        },
        depth: 0.004,
    };
    // A limb standing up the y axis, met where it faces x: its angle grows
    // toward -z, and its ribs rise that way just short of a crest.
    let (normal, axis) = (Vec3::new(1.0, 0.0, 0.0), Vec3::UP);
    let rising = axis.cross(normal);
    let bump = Bump {
        p: Vec3::ZERO,
        uv: (1.0, -core::f64::consts::PI / 36.0),
        tangent: axis,
        girth: 0.2,
        instance: 0,
        width: 1e-4,
    };
    let tilted = relief.tilt(normal, &bump);
    assert!(
        tilted.dot(rising) < -0.05,
        "{tilted:?} leans toward {rising:?}"
    );
}
