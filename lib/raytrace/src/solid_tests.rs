//! Host tests of a structure's units: each met where its dressed form is,
//! its wear only taking stone away and only where it shows, and what covers
//! it standing proud near the eye and coloured as it grows either way.

use alloc::vec::Vec;

use super::*;
use crate::cover::{Lodging, Substrate};
use crate::material::Finish;
use crate::pigment::Pigment;
use crate::sample::{mix32, unit};

const BARE_WEAR: Wear = Wear {
    arris: 0.0,
    chips: 0,
    lumps: 0.0,
    pits: 0.0,
    crack: 0.0,
};

fn solid(form: Form, half: Vec3, wear: &Wear) -> Solid {
    Solid::new(
        (Pose::new(Vec3::ZERO, Frame::WORLD), half),
        (form, wear),
        (0, None, 0.5),
        0x51d,
    )
}

fn block(half: Vec3) -> Solid {
    solid(Form::Block { fan: 0 }, half, &BARE_WEAR)
}

/// A view from `eye`, a pixel a thousandth of a radian across.
fn view(materials: &[Material], eye: Vec3) -> Cutting<'_> {
    Cutting {
        materials,
        key: 0,
        scale: 1.0,
        eye,
        pixel: 1e-3,
    }
}

fn nearest(solid: &Solid, ray: &Ray, cutting: Option<&Cutting<'_>>) -> Option<Hit> {
    solid.meet(ray, ((1e-9, 10.0), Seeking::Nearest), cutting)
}

#[test]
fn a_frame_survives_its_quaternion() {
    for index in 0..200u32 {
        let draw = |salt: u32| unit(mix32(index ^ salt));
        let axis = Vec3::new(draw(1) - 0.5, draw(2) - 0.5, draw(3) - 0.5).normalized();
        let frame =
            Frame::about(axis, 6.3 * draw(4)).rotated_by(Frame::turned(6.3 * draw(5), draw(6)));
        let back = frame_of(turn_of(frame));
        for (got, want) in [(back.x, frame.x), (back.y, frame.y), (back.z, frame.z)] {
            assert!(
                (got - want).length() < 2e-4,
                "{index}: {got:?} against {want:?}"
            );
        }
    }
}

#[test]
fn a_block_is_met_at_its_faces() {
    let half = Vec3::new(0.3, 0.2, 0.1);
    let stone = block(half);
    for (way, reach) in [
        (Vec3::new(1.0, 0.0, 0.0), half.x),
        (Vec3::UP, half.y),
        (Vec3::new(0.0, 0.0, 1.0), half.z),
    ] {
        for side in [1.0, -1.0] {
            let out = way * side;
            let hit = nearest(&stone, &Ray::new(out, -out), None).expect("a face");
            assert!(
                (hit.t - (1.0 - reach)).abs() < 1e-3,
                "{out:?}: met at {}",
                hit.t
            );
            assert!(
                hit.normal.dot(out) > 0.99,
                "{out:?}: faces {:?}",
                hit.normal
            );
        }
    }
    assert!(nearest(
        &stone,
        &Ray::new(Vec3::new(0.0, 0.5, 1.0), Vec3::new(0.0, 0.0, -1.0)),
        None
    )
    .is_none());
}

#[test]
fn worn_arrises_round_its_corners() {
    let half = Vec3::splat(0.2);
    let sharp = nearest(
        &block(half),
        &Ray::new(Vec3::splat(1.0), -Vec3::splat(1.0).normalized()),
        None,
    )
    .expect("a corner");
    let worn = Wear {
        arris: 0.03,
        ..BARE_WEAR
    };
    let round = solid(Form::Block { fan: 0 }, half, &worn);
    let rounded = nearest(
        &round,
        &Ray::new(Vec3::splat(1.0), -Vec3::splat(1.0).normalized()),
        None,
    )
    .expect("a corner");
    let lost = rounded.t - sharp.t;
    let expected = 0.03 * (mathf::sqrt(3.0) - 1.0);
    assert!(
        (lost - expected).abs() < 2e-3,
        "a corner worn by {lost}, not {expected}"
    );
}

#[test]
fn a_voussoirs_ends_lean() {
    let half = Vec3::new(0.15, 0.25, 0.3);
    let voussoir = solid(Form::Block { fan: 20 }, half, &BARE_WEAR);
    let across = |y: f64| {
        let hit = nearest(
            &voussoir,
            &Ray::new(Vec3::new(1.0, y, 0.0), Vec3::new(-1.0, 0.0, 0.0)),
            None,
        )
        .expect("an end");
        1.0 - hit.t
    };
    let (foot, head) = (across(-0.2), across(0.2));
    assert!(
        head > foot + 0.02,
        "its ends lean: {foot} at its foot, {head} at its head"
    );
    let middle = across(0.0);
    assert!(
        (middle - half.x).abs() < 2e-3,
        "its middle is its half length: {middle}"
    );
}

#[test]
fn a_drum_tapers_and_is_fluted() {
    let half = Vec3::new(0.3, 0.4, 0.3);
    let plain = solid(
        Form::Drum {
            taper: 51,
            swell: 0,
            flutes: 0,
        },
        half,
        &BARE_WEAR,
    );
    let radius = |drum: &Solid, y: f64, angle: f64| {
        let out = Vec3::new(mathf::cos(angle), 0.0, mathf::sin(angle));
        let hit = nearest(drum, &Ray::new(out * 2.0 + Vec3::UP * y, -out), None).expect("its side");
        2.0 - hit.t
    };
    let (foot, head) = (radius(&plain, -0.35, 0.3), radius(&plain, 0.35, 0.3));
    assert!(
        foot > head + 0.04,
        "it tapers: {foot} at its foot, {head} at its head"
    );
    let fluted = solid(
        Form::Drum {
            taper: 0,
            swell: 0,
            flutes: 20,
        },
        half,
        &BARE_WEAR,
    );
    let step = core::f64::consts::TAU / 20.0;
    let (arris, flute) = (radius(&fluted, 0.0, 0.0), radius(&fluted, 0.0, 0.5 * step));
    assert!(
        (arris - half.x).abs() < 2e-3,
        "an arris stands at its radius: {arris}"
    );
    assert!(
        arris - flute > 0.6 * FLUTE_DEPTH * half.x,
        "a flute is cut: {arris} and {flute}"
    );
}

#[test]
fn a_turned_moulding_follows_its_profile() {
    let half = Vec3::new(0.2, 0.1, 0.3);
    let moulding = solid(Form::Turned { bow: 2, bulge: 0 }, half, &BARE_WEAR);
    let radius = |y: f64| {
        let hit = nearest(
            &moulding,
            &Ray::new(Vec3::new(2.0, y, 0.0), Vec3::new(-1.0, 0.0, 0.0)),
            None,
        )
        .expect("its side");
        2.0 - hit.t
    };
    let (foot, head) = (radius(-0.095), radius(0.095));
    assert!(
        (foot - 0.2).abs() < 0.01 && (head - 0.3).abs() < 0.01,
        "{foot} to {head}"
    );
    // It bows out fast at its foot: past halfway out by its middle.
    assert!(radius(0.0) > 0.27, "{}", radius(0.0));
}

#[test]
fn a_volute_is_channelled_between_its_turns() {
    let half = Vec3::new(0.2, 0.03, 0.2);
    let volute = solid(Form::Scroll { turns: 3 }, half, &BARE_WEAR);
    let face = |x: f64, z: f64| {
        let hit =
            nearest(&volute, &Ray::new(Vec3::new(x, 1.0, z), -Vec3::UP), None).expect("its face");
        1.0 - hit.t
    };
    let eye = face(0.0, 0.0);
    assert!((eye - half.y).abs() < 1e-3, "the eye stands full: {eye}");
    let (lowest, highest) = (0..400u32)
        .map(|index| {
            let r = 0.05 + 0.14 * f64::from(index) / 400.0;
            face(r, 0.0)
        })
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(low, high), at| {
            (low.min(at), high.max(at))
        });
    let pitch = Shaped::spiral_pitch(half.x, 3);
    assert!(
        highest - half.y > -1e-3,
        "the fillet stands full: {highest}"
    );
    assert!(
        half.y - lowest > 0.8 * SCROLL_DEPTH * pitch,
        "a channel is cut: {lowest}"
    );
}

/// A stone whose wear shows: chips, pits and a crack.
fn worn() -> Solid {
    let wear = Wear {
        arris: 0.008,
        chips: 8,
        lumps: 0.0,
        pits: 0.003,
        crack: 0.002,
    };
    solid(Form::Block { fan: 0 }, Vec3::new(0.35, 0.18, 0.2), &wear)
}

#[test]
fn wear_only_takes_stone_away_and_only_where_it_shows() {
    let (stone, dressed) = (
        worn(),
        solid(
            Form::Block { fan: 0 },
            Vec3::new(0.35, 0.18, 0.2),
            &Wear {
                arris: 0.008,
                ..BARE_WEAR
            },
        ),
    );
    let materials: [Material; 0] = [];
    let (near, far) = (
        view(&materials, Vec3::new(0.0, 0.0, 0.6)),
        view(&materials, Vec3::new(0.0, 0.0, 400.0)),
    );
    let mut differs = 0;
    for index in 0..400u32 {
        let draw = |salt: u32| unit(mix32(index ^ salt));
        let start = Vec3::new(0.7 * (draw(1) - 0.5), 0.36 * (draw(2) - 0.5), 1.0);
        let ray = Ray::new(start, Vec3::new(0.0, 0.0, -1.0));
        let plain = nearest(&dressed, &ray, Some(&far));
        let seen_far = nearest(&stone, &ray, Some(&far));
        if let (Some(plain), Some(seen)) = (plain, seen_far) {
            assert!(
                (plain.t - seen.t).abs() < 1e-3,
                "{index}: far off the stone is its dressed form"
            );
        }
        if let (Some(plain), Some(seen)) = (plain, nearest(&stone, &ray, Some(&near))) {
            assert!(
                seen.t > plain.t - 1e-4,
                "{index}: wear stood proud of the dressed face"
            );
            if seen.t > plain.t + 5e-4 {
                differs += 1;
            }
        }
    }
    assert!(
        differs > 10,
        "near the eye its wear shows: {differs} of 400"
    );
}

#[test]
fn its_bounds_hold_every_hit_and_a_shadow_sees_what_the_eye_does() {
    let materials = [cover_material(Substrate::Siliceous)];
    let stone = Solid::new(
        (
            Pose::new(Vec3::new(0.1, 0.2, -0.1), Frame::turned(0.4, 0.3)),
            Vec3::new(0.3, 0.15, 0.22),
        ),
        (
            Form::Block { fan: 12 },
            &Wear {
                arris: 0.01,
                chips: 6,
                lumps: 0.01,
                pits: 0.002,
                crack: 0.001,
            },
        ),
        (0, Some(0), 0.8),
        0x77,
    );
    let bounds = stone.bounds();
    let cutting = view(&materials, Vec3::new(0.0, 1.0, 1.5));
    for index in 0..600u32 {
        let draw = |salt: u32| unit(mix32(index ^ salt));
        let start = Vec3::new(2.0 * (draw(1) - 0.5), 1.0 + draw(2), 1.5);
        let target = Vec3::new(
            0.6 * (draw(3) - 0.5),
            0.2 + 0.4 * (draw(4) - 0.5),
            0.5 * (draw(5) - 0.5) - 0.1,
        );
        let ray = Ray::new(start, (target - start).normalized());
        let hit = stone.meet(&ray, ((1e-9, 10.0), Seeking::Nearest), Some(&cutting));
        let shadow = stone.meet(&ray, ((1e-9, 10.0), Seeking::Any), Some(&cutting));
        assert_eq!(
            hit.is_some(),
            shadow.is_some(),
            "{index}: a shadow and the eye disagree"
        );
        if let Some(hit) = hit {
            let at = ray.at(hit.t);
            assert!(
                at.x >= bounds.min.x
                    && at.x <= bounds.max.x
                    && at.y >= bounds.min.y
                    && at.y <= bounds.max.y
                    && at.z >= bounds.min.z
                    && at.z <= bounds.max.z,
                "{index}: met at {at:?} outside {bounds:?}"
            );
        }
    }
}

fn cover_material(substrate: Substrate) -> Material {
    Material::new(
        Pigment::Cover(Cover {
            moss: 1.0,
            lichen: 1.0,
            damp: 1.0,
            drought: 0.0,
            foot: -10.0,
            shade: Vec3::new(1.0, 0.0, 0.0),
            substrate,
            seed: 4,
        }),
        Finish::Matte,
    )
}

#[test]
fn moss_stands_proud_near_the_eye_and_colours_as_it_grows_far_off() {
    let materials = [
        Material::new(Pigment::Solid(Vec3::splat(0.5)), Finish::Matte),
        cover_material(Substrate::Calcareous),
    ];
    let Pigment::Cover(cover) = &materials[1].pigment else {
        unreachable!("the cover's material")
    };
    let slab = Solid::new(
        (
            Pose::new(Vec3::ZERO, Frame::WORLD),
            Vec3::new(0.6, 0.1, 0.6),
        ),
        (Form::Block { fan: 0 }, &BARE_WEAR),
        (0, Some(1), 1.0),
        9,
    );
    // Where moss grows on the slab's top.
    let mossed: Vec<(f64, f64)> = (0..2000u32)
        .map(|index| {
            (
                1.1 * (unit(mix32(index)) - 0.5),
                1.1 * (unit(mix32(index ^ 7)) - 0.5),
            )
        })
        .filter(|&(x, z)| {
            let at = Lodging {
                p: Vec3::new(x, 0.1, z),
                normal: Vec3::UP,
                affinity: 1.0,
                joint: (0.6 - x.abs()).min(0.6 - z.abs()),
            };
            matches!(cover.growth(&at), Growth::Moss(height) if height > 0.6)
        })
        .take(20)
        .collect();
    assert!(mossed.len() >= 10, "a mossed top grows moss");
    for (x, z) in mossed {
        let ray = Ray::new(Vec3::new(x, 1.0, z), -Vec3::UP);
        let near =
            nearest(&slab, &ray, Some(&view(&materials, Vec3::new(x, 0.6, z)))).expect("the top");
        let far =
            nearest(&slab, &ray, Some(&view(&materials, Vec3::new(x, 900.0, z)))).expect("the top");
        assert!(
            near.t < 0.9 - 0.003,
            "near the eye its cushion stands proud: met at {}",
            near.t
        );
        assert!(
            (far.t - 0.9).abs() < 1e-3,
            "far off the slab is its own face: met at {}",
            far.t
        );
        for hit in [near, far] {
            assert_eq!(hit.material, Some(1), "moss is coloured where it grows");
        }
    }
}

/// A block's dressed face is read off its rounded box as the gradient of
/// the distance it measures would find it, fan and lean and all.
#[test]
fn a_blocks_face_is_read_off_its_box() {
    for (index, fan) in [0i8, 18, -12, 40].into_iter().enumerate() {
        let wear = Wear {
            arris: 0.004 + 0.01 * crate::vector::real(index),
            ..BARE_WEAR
        };
        let block = Solid::new(
            (
                Pose::new(Vec3::ZERO, Frame::WORLD),
                Vec3::new(0.3, 0.2, 0.25),
            ),
            (Form::Block { fan }, &wear),
            (0, None, 0.5),
            7,
        );
        let shaped = Shaped::new(&block, None);
        let mut compared = 0;
        for sample in 0..4000u32 {
            let draw = |salt: u32| 2.0 * unit(mix32(sample ^ salt)) - 1.0;
            let q = Vec3::new(0.4 * draw(1), 0.3 * draw(2), 0.35 * draw(3));
            // Near its surface, clear of the creases where which face is
            // nearest changes and a gradient has no single value.
            if shaped.dressed(q).abs() > 0.05 {
                continue;
            }
            let found = gradient(q, 1e-6, |at| shaped.dressed(at));
            let nudged = gradient(q + Vec3::splat(2e-4), 1e-6, |at| shaped.dressed(at));
            if found.dot(nudged) < 0.999 {
                continue;
            }
            let read = shaped.dressed_normal(q);
            assert!(
                read.dot(found) > 0.999,
                "fan {fan} at {q:?}: read {read:?}, found {found:?}"
            );
            compared += 1;
        }
        assert!(compared > 200, "fan {fan}: {compared} compared");
    }
}

fn field_stone(key: u32, half: Vec3, round: u8, facets: u8) -> Solid {
    Solid::new(
        (Pose::new(Vec3::ZERO, Frame::WORLD), half),
        (Form::Rock { round, facets }, &BARE_WEAR),
        (0, None, 0.5),
        key,
    )
}

/// A wall's stone, longer than it is deep and deeper than it is tall.
const WALLING: Vec3 = Vec3::new(0.2, 0.09, 0.15);

/// However its faces broke, a field stone's distance never rises faster
/// than a true distance would, so a march can never step through it; and
/// none of it lies outside its box.
#[test]
fn a_field_stone_measures_a_true_distance_within_its_box() {
    for key in 0..40u32 {
        let stone = field_stone(key, WALLING, 60 + 4 * u8::try_from(key).unwrap_or(0), 5);
        let shaped = Shaped::new(&stone, None);
        assert_eq!(shaped.faceted, 5, "{key}: broken");
        for sample in 0..400u32 {
            let draw = |salt: u32| 2.0 * unit(mix32(key ^ mix32(sample ^ salt))) - 1.0;
            let a = Vec3::new(0.3 * draw(1), 0.15 * draw(2), 0.25 * draw(3));
            let b = a + Vec3::new(draw(4), draw(5), draw(6)) * 0.04;
            let (at_a, at_b) = (shaped.dressed(a), shaped.dressed(b));
            assert!(
                (at_a - at_b).abs() <= (a - b).length() + 1e-12,
                "{key}: rises {} over {}",
                (at_a - at_b).abs(),
                (a - b).length()
            );
            if at_a < 0.0 {
                let reach = WALLING + Vec3::splat(1e-9);
                assert!(
                    a.x.abs() <= reach.x && a.y.abs() <= reach.y && a.z.abs() <= reach.z,
                    "{key}: {a:?} within it but outside its box"
                );
            }
        }
    }
}

/// Its shape: how far in from a metre out a ray toward its middle meets it,
/// from every way about it.
fn profile(stone: &Solid) -> Vec<f64> {
    let golden = core::f64::consts::PI * (3.0 - mathf::sqrt(5.0));
    let bounds = stone.bounds();
    (0..96u32)
        .map(|index| {
            let rise = 1.0 - 2.0 * (f64::from(index) + 0.5) / 96.0;
            let (round, angle) = (mathf::sqrt(1.0 - rise * rise), golden * f64::from(index));
            let start = Vec3::new(round * mathf::cos(angle), rise, round * mathf::sin(angle));
            let ray = Ray::new(start, -start);
            let hit = nearest(stone, &ray, None).expect("met from every way about it");
            let at = ray.at(hit.t);
            assert!(
                at.x >= bounds.min.x
                    && at.x <= bounds.max.x
                    && at.y >= bounds.min.y
                    && at.y <= bounds.max.y
                    && at.z >= bounds.min.z
                    && at.z <= bounds.max.z,
                "met at {at:?} outside {bounds:?}"
            );
            hit.t
        })
        .collect()
}

#[test]
fn field_stones_break_each_its_own_way() {
    let profiles: Vec<Vec<f64>> = (0..12u32).map(|key| profile(&field_stone(key, WALLING, 20, 7))).collect();
    for (index, a) in profiles.iter().enumerate() {
        for b in profiles.iter().skip(index + 1) {
            assert!(
                a.iter().zip(b).any(|(a, b)| (a - b).abs() > 0.01),
                "two stones broke alike"
            );
        }
    }
    // Broken, a stone is only ever cut back from the body it would be.
    let whole = profile(&field_stone(3, WALLING, 20, 0));
    let broken = &profiles[3];
    assert!(whole.iter().zip(broken).all(|(whole, broken)| broken >= &(whole - 1e-3)));
    assert!(whole.iter().zip(broken).any(|(whole, broken)| broken > &(whole + 0.01)));
}

/// Worn, the arrises its fractures leave only lose stone, and lose it
/// somewhere.
#[test]
fn worn_arrises_take_stone_from_a_broken_stones_edges() {
    let sharp = profile(&field_stone(9, WALLING, 5, 7));
    let worn = profile(&field_stone(9, WALLING, 230, 7));
    assert!(sharp.iter().zip(&worn).all(|(sharp, worn)| worn >= &(sharp - 1e-3)));
    assert!(sharp.iter().zip(&worn).any(|(sharp, worn)| worn > &(sharp + 0.002)));
}

/// Broken along planes, a stone is faceted, not boxed: its face is square to
/// its own `z`, its fractures round its sides lie at angles no box's faces
/// do, and most of what it shows a wall's face is flat, its breaks or its
/// body's own faces.
#[test]
fn a_broken_stone_is_faceted_along_planes_no_box_has() {
    let golden = core::f64::consts::PI * (3.0 - mathf::sqrt(5.0));
    let (mut broken, mut skewed, mut fractures) = (Vec::new(), 0, 0);
    for key in 0..24u32 {
        let stone = field_stone(key, WALLING, 10, 8);
        let shaped = Shaped::new(&stone, None);
        let planes: Vec<Vec3> = shaped.facets.iter().take(shaped.faceted).map(|facet| facet.normal).collect();
        assert!(planes[1].z > 0.98, "{key}: its face is set square: {:?}", planes[1]);
        // Ten degrees or more off every axis a box's faces lie square to.
        let off = mathf::cos(10.0f64.to_radians());
        fractures += planes.len() - 3;
        skewed += planes
            .iter()
            .skip(3)
            .filter(|normal| normal.x.abs().max(normal.y.abs()).max(normal.z.abs()) < off)
            .count();
        let (mut flat, mut seen) = (0u32, 0u32);
        for index in 0..96u32 {
            let rise = 1.0 - 2.0 * (f64::from(index) + 0.5) / 96.0;
            let (round, angle) = (mathf::sqrt(1.0 - rise * rise), golden * f64::from(index));
            let start = Vec3::new(round * mathf::cos(angle), rise, round * mathf::sin(angle));
            if start.z < 0.2 {
                continue;
            }
            seen += 1;
            let ray = Ray::new(start, -start);
            let hit = nearest(&stone, &ray, None).expect("met from every way about it");
            let normal = gradient(ray.at(hit.t), 1e-6, |at| shaped.dressed(at));
            let square = normal.x.abs().max(normal.y.abs()).max(normal.z.abs()) > 0.999;
            if square || planes.iter().any(|plane| plane.dot(normal) > 0.999) {
                flat += 1;
            }
        }
        broken.push((flat, seen));
    }
    assert!(5 * skewed > 3 * fractures, "{skewed} of {fractures} fractures at angles");
    // Two fifths of every stone's front and over half of them all.
    let (flat, seen) = broken.iter().fold((0, 0), |(flat, seen), &(f, s)| (flat + f, seen + s));
    assert!(
        broken.iter().all(|&(flat, seen)| 5 * flat >= 2 * seen) && 2 * flat > seen,
        "on broken faces, of those seen: {broken:?}"
    );
}

/// Nothing broke away under a stone laid on its bed, so it sits on the work
/// below it however its faces broke.
#[test]
fn a_field_stone_keeps_its_bed() {
    for key in 0..60u32 {
        let stone = field_stone(key, WALLING, 60, 8);
        let shaped = Shaped::new(&stone, None);
        let hit = nearest(&stone, &Ray::new(Vec3::new(0.0, -1.0, 0.0), Vec3::UP), None).expect("its bed");
        let bed = -1.0 + hit.t;
        assert!(bed < -0.9 * WALLING.y, "{key}: bedded at {bed}");
        let flat = gradient(Vec3::new(0.0, bed, 0.0), 1e-5, |at| shaped.dressed(at));
        assert!(flat.y < -0.97, "{key}: its bed faces {flat:?}");
    }
}

/// Every scar a field stone is struck is struck where its broken faces are,
/// never on a corner of its body its fractures took away before, and takes
/// stone from it.
#[test]
fn a_field_stones_chips_spall_its_broken_faces() {
    let wear = Wear {
        chips: 8,
        ..BARE_WEAR
    };
    let mut scarred = 0;
    for key in 0..40u32 {
        let struck = Solid::new(
            (Pose::new(Vec3::ZERO, Frame::WORLD), WALLING),
            (Form::Rock { round: 8, facets: 8 }, &wear),
            (0, None, 0.5),
            key,
        );
        let shaped = Shaped::new(&struck, None);
        assert_eq!(shaped.chipped, 8, "{key}: struck");
        let half = shaped.half;
        let broken = |p: Vec3| {
            let boxed = (p.x.abs() - half.x).max(p.y.abs() - half.y).max(p.z.abs() - half.z);
            shaped
                .facets
                .iter()
                .take(shaped.faceted)
                .fold(boxed, |d, facet| d.max(facet.normal.dot(p) - facet.offset))
        };
        for chip in shaped.chips.iter().take(shaped.chipped) {
            let struck_at = chip.centre - chip.out * (chip.radius - chip.bite);
            assert!(
                broken(struck_at).abs() < 1e-9,
                "{key}: struck at {struck_at:?}, off its broken faces by {}",
                broken(struck_at)
            );
        }
        let whole = profile(&field_stone(key, WALLING, 8, 8));
        let chipped = profile(&struck);
        assert!(whole.iter().zip(&chipped).all(|(whole, chipped)| chipped >= &(whole - 1e-3)));
        if whole.iter().zip(&chipped).any(|(whole, chipped)| chipped > &(whole + 0.002)) {
            scarred += 1;
        }
    }
    assert!(scarred > 30, "{scarred} of 40 stones scarred where a ray sees");
}

/// Lumps too shallow to stand out of a face far off still shade it, so a
/// rough face never turns to a plane while they are broad enough to see;
/// near the eye the light follows the relief they stand out in.
#[test]
fn lumps_too_shallow_to_stand_out_still_turn_the_light() {
    let half = Vec3::new(0.35, 0.18, 0.2);
    let wear = Wear {
        lumps: 0.012,
        ..BARE_WEAR
    };
    let stone = solid(Form::Block { fan: 0 }, half, &wear);
    let materials: [Material; 0] = [];
    // Lumps a centimetre deep and a sixth of a metre across: lost in relief
    // tens of metres off, lost to the light only thousands of metres off.
    let (near, far, distant) = (
        view(&materials, Vec3::new(0.0, 0.0, 0.6)),
        view(&materials, Vec3::new(0.0, 0.0, 30.0)),
        view(&materials, Vec3::new(0.0, 0.0, 3000.0)),
    );
    let mut turned = 0;
    for index in 0..200u32 {
        let draw = |salt: u32| unit(mix32(index ^ salt));
        let start = Vec3::new(0.6 * (draw(1) - 0.5), 0.3 * (draw(2) - 0.5), 1.0);
        let ray = Ray::new(start, Vec3::new(0.0, 0.0, -1.0));
        let seen = nearest(&stone, &ray, Some(&far)).expect("its face");
        assert!(
            (seen.t - (1.0 - half.z)).abs() < 1e-3 && seen.normal.z > 0.9999,
            "{index}: far off its face stands flat, met at {} facing {:?}",
            seen.t,
            seen.normal
        );
        if seen.shading.z < 0.9995 {
            turned += 1;
        }
        let gone = nearest(&stone, &ray, Some(&distant)).expect("its face");
        assert!(
            gone.shading.z > 0.9999,
            "{index}: too far off to see, they shade nothing: {:?}",
            gone.shading
        );
        let close = nearest(&stone, &ray, Some(&near)).expect("its face");
        assert_eq!(close.shading, close.normal, "{index}: near, the relief is the light's");
    }
    assert!(turned > 140, "far off, its lumps shade {turned} of 200");
}

#[test]
fn a_smooth_max_is_the_max_but_where_the_two_near_and_never_more_than_a_quarter_over() {
    let k = 0.02;
    for step in -100..=100 {
        let a = f64::from(step) / 1000.0;
        let b = 0.013;
        let blended = smooth_max(a, b, k);
        if (a - b).abs() >= k {
            assert_eq!(blended, a.max(b), "{a}");
        } else {
            assert!(blended >= a.max(b) && blended <= a.max(b) + 0.25 * k + 1e-15, "{a}: {blended}");
        }
    }
    assert_eq!(smooth_max(0.1, -0.2, 0.0), 0.1);
}
