//! Host tests of the microfacet, Fresnel and thin-film terms, and of relief.

use alloc::vec::Vec;
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

/// A breeze over open water, gusting.
const BREEZE: Wind = Wind {
    slope_variance: 0.02,
    lengths: (2.0, 0.03),
    spread: 0.6,
    gusts: (0.3, 40.0),
};

/// `wind` with no lulls: every place under the full wind.
const fn steady(wind: Wind) -> Wind {
    Wind {
        gusts: (1.0, wind.gusts.1),
        ..wind
    }
}

/// Where a relief is read over a footprint `width` wide, seen head on.
fn bump_at(p: Vec3, width: f64) -> Bump {
    Bump {
        p,
        uv: (p.x, p.z),
        tangent: Vec3::new(1.0, 0.0, 0.0),
        girth: 0.3,
        instance: 0,
        width,
        stretch: width,
    }
}

#[test]
fn relief_tilts_a_normal_a_little_and_keeps_it_unit() {
    for relief in [
        Relief::waves(BREEZE, 1).expect("waves"),
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
                bare: 0.0,
                seed: 4,
            },
            depth: 0.004,
        },
    ] {
        let mut moved = 0.0;
        for step in 0..500u32 {
            let p = Vec3::new(f64::from(step) * 0.17, 0.0, f64::from(step) * -0.11);
            let tilted = relief.tilt(Vec3::UP, &bump_at(p, 1e-4)).normal;
            assert!((tilted.length() - 1.0).abs() < 1e-9);
            assert!(tilted.y > 0.8, "{relief:?} tilted too far: {tilted:?}");
            moved += (tilted - Vec3::UP).length();
        }
        assert!(moved > 0.5, "{relief:?} barely tilts");
    }
    let plain = Material::new(Pigment::Solid(Vec3::ONE), Finish::Coated { roughness: 0.2 });
    assert!(plain.relief.is_none());
    assert!(plain
        .with_relief(Relief::waves(BREEZE, 3).expect("waves"))
        .relief
        .is_some());
}

/// The slope a wave's sum leaves, at `p`, of a normal tilted from straight up.
fn slope_of(normal: Vec3) -> (f64, f64) {
    (-normal.x / normal.y, -normal.z / normal.y)
}

/// Read close, the waves hold the slope variance asked of them and lend no
/// roughness; read over a footprint coarser than all of them, they tilt
/// nothing and lend it all; and between, what they tilt and what they lend
/// still add up to it.
#[test]
fn waves_hold_their_slope_variance_whether_they_tilt_or_roughen() {
    let relief = Relief::waves(steady(BREEZE), 5).expect("waves");
    let points = 6000u32;
    let mut draw = draws(11);
    for width in [1e-5, 0.01, 0.1] {
        let (mut squares, mut lent) = (0.0, 0.0);
        for _ in 0..points {
            let p = Vec3::new(400.0 * draw(), 0.0, 400.0 * draw());
            let tilt = relief.tilt(Vec3::UP, &bump_at(p, width));
            let (sx, sz) = slope_of(tilt.normal);
            squares += sx * sx + sz * sz;
            lent += tilt.unresolved;
        }
        let total = (squares + lent) / f64::from(points);
        assert!(
            (total - BREEZE.slope_variance).abs() < 0.15 * BREEZE.slope_variance,
            "over {width} m: {total}"
        );
        if width < 1e-4 {
            assert!(lent < 1e-12, "a close look lends nothing: {lent}");
        }
    }
    let tilt = relief.tilt(Vec3::UP, &bump_at(Vec3::new(3.0, 0.0, 7.0), 10.0));
    assert!((tilt.normal - Vec3::UP).length() < 1e-12);
    assert!((tilt.unresolved - BREEZE.slope_variance).abs() < 1e-12);
}

/// The waves' slope at one place says almost nothing of their slope a step
/// away along any direction, however long the step: no stretch of the water
/// repeats another, where a handful of swells would beat into a lattice.
#[test]
fn waves_never_repeat_across_the_water() {
    let relief = Relief::waves(steady(BREEZE), 9).expect("waves");
    let slope = |x: f64, z: f64| {
        slope_of(
            relief
                .tilt(Vec3::UP, &bump_at(Vec3::new(x, 0.0, z), 1e-5))
                .normal,
        )
        .0
    };
    let mut draw = draws(23);
    let places: Vec<(f64, f64)> = (0..3000)
        .map(|_| (500.0 * draw(), 500.0 * draw()))
        .collect();
    let mut worst: f64 = 0.0;
    for lag in [7.3, 38.4, 96.0, 384.0, 1024.0, 2000.0] {
        for turn in 0..8 {
            let angle = TAU * f64::from(turn) / 8.0;
            let (dx, dz) = (lag * mathf::cos(angle), lag * mathf::sin(angle));
            let (mut both, mut here2, mut there2) = (0.0, 0.0, 0.0);
            for &(x, z) in &places {
                let (here, there) = (slope(x, z), slope(x + dx, z + dz));
                both += here * there;
                here2 += here * here;
                there2 += there * there;
            }
            let correlation = both / mathf::sqrt(here2 * there2);
            worst = worst.max(correlation.abs());
        }
    }
    assert!(worst < 0.25, "the water repeats itself: {worst}");
}

/// Gusts raise the waves in patches and lulls lay them down: across the
/// water the waves' height runs from the calm's share to all of it, and from
/// one metre to the next it changes only a little.
#[test]
fn gusts_raise_the_waves_in_patches() {
    let Relief::Waves(waves) = Relief::waves(BREEZE, 13).expect("waves") else {
        panic!("waves");
    };
    let mut draw = draws(29);
    let (mut lowest, mut highest, mut stepped) = (1.0f64, 0.0f64, 0.0f64);
    for _ in 0..4000 {
        let (x, z) = (2000.0 * draw(), 2000.0 * draw());
        let here = waves.gust(x, z);
        assert!(
            (BREEZE.gusts.0 - 1e-9..=1.0 + 1e-9).contains(&here),
            "{here}"
        );
        lowest = lowest.min(here);
        highest = highest.max(here);
        stepped = stepped.max((waves.gust(x + 1.0, z) - here).abs());
    }
    assert!(lowest < BREEZE.gusts.0 + 0.05, "no lull: {lowest}");
    assert!(highest > 0.95, "no gust: {highest}");
    assert!(stepped < 0.15, "a gust's edge is abrupt: {stepped}");
}

/// Read close, the waves curve the surface as much as their slopes swing,
/// wave by wave, says; read over a footprint coarser than all of them,
/// they curve it not at all, and what they leave unresolved is the rest of
/// their slope variance.
#[test]
fn waves_curve_the_surface_as_their_slopes_change() {
    let Relief::Waves(waves) = Relief::waves(steady(BREEZE), 17).expect("waves") else {
        panic!("waves");
    };
    // The curvature's variance read off the surface: each axis's change in
    // slope across a small step, about as far as a hundredth of the
    // shortest wave, either way.
    let step = 1e-4;
    let slope = |x: f64, z: f64| {
        let normal = waves.tilt(Vec3::UP, Vec3::new(x, 0.0, z), 1e-6).normal;
        (-normal.x / normal.y, -normal.z / normal.y)
    };
    let mut draw = draws(41);
    let (mut squares, points) = (0.0, 4000u32);
    for _ in 0..points {
        let (x, z) = (300.0 * draw(), 300.0 * draw());
        let (sx, sz) = slope(x, z);
        let (sxx, _) = slope(x + step, z);
        let (_, szz) = slope(x, z + step);
        let (sxz, _) = slope(x, z + step);
        let (dxx, dzz, dxz) = ((sxx - sx) / step, (szz - sz) / step, (sxz - sx) / step);
        squares += dxx * dxx + dzz * dzz + 2.0 * dxz * dxz;
    }
    let measured = mathf::sqrt(squares / f64::from(points));
    let reckoned = waves.curvature(1e-6);
    assert!(
        (measured - reckoned).abs() < 0.1 * reckoned,
        "measured {measured} against {reckoned}"
    );
    assert!(
        waves.curvature(0.02) < reckoned,
        "a coarser look sees less curvature"
    );
    assert!(waves.curvature(10.0) < 1e-12, "far too coarse, none at all");
    for footprint in [1e-6, 0.004, 0.03, 0.3, 10.0] {
        let lent = waves.unresolved(footprint);
        let total = waves.slope_variance();
        assert!(
            (0.0..=total + 1e-15).contains(&lent),
            "{footprint}: {lent} of {total}"
        );
    }
    assert!(waves.unresolved(1e-6) < 1e-15);
    assert!((waves.unresolved(10.0) - waves.slope_variance()).abs() < 1e-15);
}

/// Swept along a row far from the origin, the waves tilt every point as
/// reading them afresh there does, to within the rounding of the turns.
#[test]
fn a_sweep_along_the_waves_tilts_each_point_as_reading_it_afresh_does() {
    let Relief::Waves(waves) = Relief::waves(BREEZE, 23).expect("waves") else {
        panic!("waves");
    };
    let (start, step) = (Vec3::new(150.3, 0.4, -42.7), Vec3::new(0.0039, 0.0, 0.0011));
    let mut sweep = Sweep::new();
    for footprint in [0.004, 0.05, 0.6, 50.0] {
        waves.begin(&mut sweep, start, step, footprint);
        let mut worst = 0.0f64;
        for index in 0..1000u32 {
            let p = start + step * f64::from(index);
            let swept = waves.tilted(Vec3::UP, p, &sweep);
            let afresh = waves.tilt(Vec3::UP, p, footprint).normal;
            worst = worst.max((swept - afresh).length());
            sweep.advance();
        }
        assert!(worst < 1e-9, "{footprint}: {worst}");
    }
}

#[test]
fn widening_adds_the_unresolved_slope_variance_to_the_roughness() {
    assert!((widened(0.3, 0.0) - 0.3).abs() < 1e-12);
    assert!((widened(0.0, 0.01) - mathf::sqrt(mathf::sqrt(0.01))).abs() < 1e-12);
    assert!(widened(0.2, 0.01) > widened(0.2, 0.001));
    assert!(widened(0.2, 0.01) > widened(0.1, 0.01));
    assert!(
        (widened(0.0, -1.0)).abs() < 1e-12,
        "a negative variance lends nothing"
    );
}

/// Bark rising the way a limb's angle grows tilts the normal back the other
/// way, as the slope of a real ridge does: its relief is laid round the limb
/// the way its pattern is.
#[test]
fn bark_relief_leans_the_normal_away_from_where_the_bark_rises() {
    let ribs = crate::cactus::Ribs { count: 18, seed: 1 };
    let relief = Relief::Bark {
        bark: crate::bark::Bark {
            kind: crate::bark::BarkKind::Ribbed { ribs: ribs.count },
            light: Vec3::ONE,
            dark: Vec3::ZERO,
            accent: Vec3::ONE,
            rise: 0.0,
            snow: 0.0,
            moss: 0.0,
            bare: 0.0,
            seed: 1,
        },
        depth: 0.004,
    };
    // A limb standing up the y axis, met where it faces x: its angle grows
    // toward -z, and its ribs rise that way short of a crest.
    let (normal, axis) = (Vec3::new(1.0, 0.0, 0.0), Vec3::UP);
    let rising = axis.cross(normal);
    let bump = Bump {
        p: Vec3::ZERO,
        uv: (1.0, ribs.crest_angle(0, 1.0) - 0.25 * ribs.pitch()),
        tangent: axis,
        girth: 0.2,
        instance: 0,
        width: 1e-4,
        stretch: 1e-4,
    };
    let tilted = relief.tilt(normal, &bump).normal;
    assert!(
        tilted.dot(rising) < -0.05,
        "{tilted:?} leans toward {rising:?}"
    );
}

/// A grain tilts a normal finely up close and settles to its mean as its
/// footprint widens: the slope of what the footprint cannot resolve goes to
/// the roughness instead, growing as the tilt it would have given fades, and
/// the whole of it once nothing is resolved.
#[test]
fn a_grain_settles_to_its_mean_in_relief_as_its_footprint_widens() {
    let grain = Relief::Grain {
        depth: 0.12,
        scale: 14.0,
        seed: 5,
    };
    let at = |width: f64| grain.tilt(Vec3::UP, &bump_at(Vec3::new(0.37, 0.0, -1.21), width));
    let close = at(1e-5);
    assert!(close.normal.y < 1.0 - 1e-6, "{close:?}");
    let mut last = close.unresolved;
    for step in 1..60 {
        let width = 1e-5 * mathf::exp(0.25 * f64::from(step));
        let tilt = at(width);
        assert!(tilt.unresolved >= last - 1e-12, "{width}: {tilt:?}");
        last = tilt.unresolved;
    }
    let far = at(10.0);
    assert!((far.normal.y - 1.0).abs() < 1e-12);
    let whole: f64 = (0..GRAIN_OCTAVES)
        .map(|octave| {
            let steep = 0.12 / GRAIN_SUM * mathf::exp(f64::from(octave) * mathf::ln(GRAIN_GAIN));
            steep * steep / 3.0
        })
        .sum();
    assert!(
        (far.unresolved - whole).abs() < 1e-12,
        "{} against {whole}",
        far.unresolved
    );
}

/// A grain made for its coarsest relief stands that steep at its coarsest
/// octave, whatever its finer octaves add: resolved by nothing, it lends the
/// roughness that octave's slope and each finer one's in turn.
#[test]
fn a_grain_made_for_its_coarse_relief_keeps_it() {
    let coarse = 0.12;
    let grain = Relief::grain(coarse, 14.0, 5);
    let far = grain.tilt(Vec3::UP, &bump_at(Vec3::new(0.37, 0.0, -1.21), 10.0));
    let whole: f64 = (0..GRAIN_OCTAVES)
        .map(|octave| {
            let steep = coarse * mathf::exp(f64::from(octave) * mathf::ln(GRAIN_GAIN));
            steep * steep / 3.0
        })
        .sum();
    assert!(
        (far.unresolved - whole).abs() < 1e-12,
        "{} against {whole}",
        far.unresolved
    );
}

/// A grain's octaves share its depth between them: too fine for a pixel to
/// resolve, all of it lends the roughness no more slope variance than one
/// octave as steep as its depth would hold, and resolved, it tilts a normal
/// no further than that octave would.
#[test]
fn a_grains_octaves_share_its_depth() {
    let depth = 0.2;
    let grain = Relief::Grain {
        depth,
        scale: 10.0,
        seed: 3,
    };
    let up = Vec3::UP;
    let blurred = grain.tilt(up, &bump_at(Vec3::new(0.3, 0.0, 0.7), 1e3));
    assert!(blurred.unresolved > 0.0);
    assert!(
        blurred.unresolved <= depth * depth / 3.0,
        "{} lent the roughness",
        blurred.unresolved
    );
    assert!((blurred.normal.dot(up) - 1.0).abs() < 1e-12);
    for index in 0..2000u32 {
        let p = Vec3::new(0.013 * f64::from(index), 0.0, 0.031 * f64::from(index % 53));
        let sharp = grain.tilt(up, &bump_at(p, 1e-6));
        let lean = mathf::sqrt(1.0 - sharp.normal.dot(up).powi(2)) / sharp.normal.dot(up);
        assert!(lean <= depth * mathf::sqrt(3.0) + 1e-9, "{lean} at {p:?}");
    }
}
