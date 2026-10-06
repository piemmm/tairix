//! Host tests of bark cut in true relief: a limb far off or too thin is
//! its tube; near the eye it is never met outside its tube and is sunk up to
//! its bark's depth, its outline ridged so some grazing rays pass through its
//! fissures; what is met lies on the cut surface, faces out and carries its
//! relief; a shadow is cast wherever a ray meets it; and a bending limb's
//! joint stays closed and its free end rounded.

use alloc::vec::Vec;

use super::*;
use crate::bark::BarkKind;
use crate::material::Finish;
use crate::pigment::Pigment;
use crate::prototype::{Part, Prototype};

/// A furrowed bark cut up to `DEPTH` deep.
const DEPTH: f64 = 0.024;

fn material() -> Material {
    let bark = Bark {
        kind: BarkKind::Furrowed,
        light: Vec3::splat(0.4),
        dark: Vec3::splat(0.05),
        accent: Vec3::splat(0.3),
        rise: 4.0,
        snow: 0.0,
        moss: 0.0,
        bare: 0.0,
        seed: 9,
    };
    Material::new(Pigment::Bark(bark.clone()), Finish::Matte)
        .with_relief(Relief::Bark { bark, depth: DEPTH })
}

/// An upright limb `radius` thick from the ground to 2 m, in material 0.
fn limb(radius: f64) -> Tube {
    Tube::new(
        (Vec3::ZERO, Vec3::new(0.0, 2.0, 0.0)),
        ((radius, 0.95 * radius), (0.0, 2.0)),
        (0, 5),
        Vec3::new(1.0, 0.0, 0.0),
    )
}

/// The limb seen from `eye`, unscaled, through a pixel a thousandth of a
/// radian across.
fn cutting(materials: &[Material], eye: Vec3) -> Cutting<'_> {
    Cutting {
        materials,
        key: 3,
        scale: 1.0,
        eye,
        pixel: 1e-3,
    }
}

/// Where `ray` meets `tube` as `cutting` sees it, cut or not.
fn met(tube: &Tube, ray: &Ray, cutting: &Cutting<'_>) -> Option<Hit> {
    match cutting.of(tube) {
        Some(cut) => meet_relieved(
            tube,
            ray,
            ((1e-9, f64::INFINITY), Seeking::Nearest),
            (Some(cutting), Some(cut)),
            None,
        ),
        None => tube_alone(tube).intersect(ray, (1e-9, f64::INFINITY), None),
    }
}

/// `tube` as a prototype of its own, never cut.
fn tube_alone(tube: &Tube) -> Prototype {
    Prototype::new(alloc::vec![Part::Tube(*tube)], Vec::new(), Vec::new()).expect("a limb")
}

/// A level ray at `height` aimed `offset` to the side of the limb's axis
/// from 3 m away, round it by `around`.
fn level_ray(height: f64, around: f64, offset: f64) -> Ray {
    let toward = Vec3::new(-mathf::cos(around), 0.0, -mathf::sin(around));
    let side = Vec3::new(-toward.z, 0.0, toward.x);
    Ray::new(
        Vec3::new(0.0, height, 0.0) - toward * 3.0 + side * offset,
        toward,
    )
}

#[test]
fn a_limb_far_off_or_too_thin_to_have_fissured_is_its_tube() {
    let materials = [material()];
    let far = cutting(&materials, Vec3::new(40.0, 1.0, 0.0));
    let near = cutting(&materials, Vec3::new(3.0, 1.0, 0.0));
    assert!(far.of(&limb(0.3)).is_none(), "far off");
    assert!(near.of(&limb(0.02)).is_none(), "too thin");
    assert!(near.of(&limb(0.3)).is_some(), "near and fissured");
}

#[test]
fn a_cut_limb_is_never_met_outside_its_tube_and_is_sunk_to_its_barks_depth() {
    let materials = [material()];
    let near = cutting(&materials, Vec3::new(3.0, 1.0, 0.0));
    let tube = limb(0.3);
    let alone = tube_alone(&tube);
    let (mut shallowest, mut deepest) = (f64::INFINITY, 0.0f64);
    for step in 0..2000u32 {
        let height = 0.3 + 1.4 * f64::from(step % 97) / 97.0;
        let around = core::f64::consts::TAU * f64::from(step) / 2000.0;
        let ray = level_ray(height, around, 0.0);
        let plain = alone
            .intersect(&ray, (1e-9, f64::INFINITY), None)
            .expect("the tube");
        let cut = met(&tube, &ray, &near).expect("the cut limb, face on");
        let sunk = cut.t - plain.t;
        assert!(sunk > -1e-6, "{step}: met {sunk} outside its tube");
        assert!(sunk < 1.05 * DEPTH, "{step}: sunk {sunk}");
        shallowest = shallowest.min(sunk);
        deepest = deepest.max(sunk);
    }
    assert!(
        shallowest < 0.1 * DEPTH,
        "its ridges at its radius: {shallowest}"
    );
    assert!(deepest > 0.6 * DEPTH, "its fissures sunk: {deepest}");
}

#[test]
fn a_cut_limbs_outline_is_ridged() {
    let materials = [material()];
    let near = cutting(&materials, Vec3::new(3.0, 1.0, 0.0));
    let tube = limb(0.3);
    // Grazing it a third of its bark's depth within its radius: a ridge
    // stops a ray there, a fissure lets it through.
    let (mut stopped, mut through) = (0, 0);
    for step in 0..600u32 {
        let height = 0.3 + 1.4 * f64::from(step) / 600.0;
        let ray = level_ray(height, 0.3, 0.3 * 0.99 - DEPTH / 3.0);
        match met(&tube, &ray, &near) {
            Some(_) => stopped += 1,
            None => through += 1,
        }
    }
    assert!(
        stopped > 30 && through > 30,
        "{stopped} stopped, {through} through"
    );
}

#[test]
fn what_a_ray_meets_lies_on_the_cut_faces_out_and_carries_its_relief() {
    let materials = [material()];
    let near = cutting(&materials, Vec3::new(3.0, 1.0, 0.0));
    let tube = limb(0.3);
    let cut = near.of(&tube).expect("cut");
    let limb = Limb::new(&tube, (Some(&near), Some(cut)), None).expect("a limb");
    for step in 0..400u32 {
        let ray = level_ray(0.4 + f64::from(step) * 0.003, f64::from(step) * 0.37, 0.05);
        let hit = met(&tube, &ray, &near).expect("met");
        let place = limb.place(ray.at(hit.t), Some(&near));
        // Found to a hundredth of the march's shortest step.
        let off = limb.above(&place, Some(&near));
        assert!(off.abs() < 1e-4, "{step}: {off} off the surface");
        assert!(hit.normal.dot(ray.dir) < 0.0, "{step}: faces the ray");
        assert!(hit.relieved, "{step}: its normal is its relief's");
    }
}

#[test]
fn a_cut_limb_shadows_wherever_a_ray_meets_it() {
    let materials = [material()];
    let near = cutting(&materials, Vec3::new(3.0, 1.0, 0.0));
    let tube = limb(0.3);
    let alone =
        Prototype::new(alloc::vec![Part::Tube(tube)], Vec::new(), Vec::new()).expect("a limb");
    for step in 0..500u32 {
        let ray = level_ray(
            0.4 + f64::from(step) * 0.002,
            f64::from(step) * 0.71,
            0.3 - 0.04 * f64::from(step % 9) / 8.0,
        );
        let hit = alone.intersect(&ray, (1e-9, f64::INFINITY), Some(&near));
        let shadowed = alone.occludes(&ray, (1e-9, f64::INFINITY), Some(&near));
        assert_eq!(hit.is_some(), shadowed, "{step}");
    }
}

#[test]
fn a_bending_limbs_joint_stays_closed_and_its_free_end_rounded() {
    let materials = [material()];
    let eye = Vec3::new(3.0, 1.0, 0.0);
    let near = cutting(&materials, eye);
    let joint = Vec3::new(0.0, 1.0, 0.0);
    let bent = Vec3::new(0.12, 2.0, 0.0);
    let lower = Tube::new(
        (Vec3::ZERO, joint),
        ((0.3, 0.29), (0.0, 1.0)),
        (0, 1),
        Vec3::new(1.0, 0.0, 0.0),
    );
    let upper = Tube::new(
        (joint, bent),
        ((0.29, 0.28), (1.0, 2.0)),
        (0, 2),
        Vec3::new(1.0, 0.0, 0.0),
    );
    let chain = Prototype::new(
        alloc::vec![Part::Tube(lower), Part::Tube(upper)],
        Vec::new(),
        Vec::new(),
    )
    .expect("a chain");
    // Toward the joint from the outside of the bend, every ray is stopped no
    // deeper than the bark is cut.
    for step in 0..120u32 {
        let around = core::f64::consts::PI * (0.75 + 0.5 * f64::from(step) / 120.0);
        let toward = Vec3::new(mathf::cos(around), 0.0, mathf::sin(around));
        let from = joint - toward * 3.0;
        let ray = Ray::new(from, toward);
        let hit = chain
            .intersect(&ray, (1e-9, f64::INFINITY), Some(&near))
            .expect("the joint is closed");
        assert!(
            hit.t < 3.0 - 0.29 + 1.1 * DEPTH,
            "{step}: sunk to {}",
            3.0 - hit.t
        );
    }
    // Along its axis past its free end, nothing stands beyond the sphere that
    // rounds it.
    let top = Ray::new(Vec3::new(0.12, 4.0, 0.0), -Vec3::UP);
    let hit = chain
        .intersect(&top, (1e-9, f64::INFINITY), Some(&near))
        .expect("its end");
    assert!(4.0 - hit.t <= 2.0 + 0.28 + 1e-4, "{}", 4.0 - hit.t);
}

/// An upright limb 0.3 thick whose foot, its first end 6 cm below the
/// ground, flares toward three lobes; and its prototype, alone.
fn flared() -> (Tube, Flare, Prototype) {
    let lobe = |angle: f64| crate::flare::Lobe {
        angle: crate::vector::single(angle),
        out: 0.6,
        width: 0.5,
        climb: 0.25,
    };
    let flare =
        Flare::new(0.06, 1.4, (0.2, 0.3), &[lobe(0.0), lobe(2.1), lobe(4.2)]).expect("a flare");
    let tube = Tube::new(
        (Vec3::new(0.0, -0.06, 0.0), Vec3::new(0.0, 1.6, 0.0)),
        ((0.3, 0.28), (0.0, 1.66)),
        (0, 5),
        Vec3::new(1.0, 0.0, 0.0),
    )
    .flared(0);
    let alone = Prototype::flared(
        alloc::vec![Part::Tube(tube)],
        (Vec::new(), Vec::new()),
        alloc::vec![flare],
    )
    .expect("a flared limb")
    .whole();
    (tube, flare, alone)
}

#[test]
fn a_flared_foot_is_met_where_it_swells_from_near_and_far_and_unseen() {
    let materials = [material()];
    let far = cutting(&materials, Vec3::new(400.0, 1.0, 0.0));
    let (tube, flare, alone) = flared();
    // Unseen, as a scene is set out, it is met to a tenth of a millimetre;
    // seen from far off, to a hundredth of the pixel there.
    for (cutting, within) in [(None, 1e-4), (Some(&far), 0.01 * 400.0 * far.pixel)] {
        for (angle, height) in [(0.0, 0.1), (1.05, 0.1), (2.1, 0.3), (4.2, 0.02), (3.0, 0.9)] {
            let way = tube.way(angle);
            let ray = Ray::new(way * 3.0 + Vec3::UP * height, -way);
            let up = height + 0.06;
            let reach = (0.3 - 0.02 * up / 1.66) * flare.factor(up, angle);
            let hit = alone
                .intersect(&ray, (1e-9, f64::INFINITY), cutting)
                .expect("met");
            assert!(
                (hit.t - (3.0 - reach)).abs() < within,
                "{angle} at {height}: met {} out, swollen to {reach}",
                3.0 - hit.t
            );
            assert!(
                hit.normal.dot(ray.dir) < 0.0,
                "{angle} at {height}: faces the ray"
            );
            assert!(alone.occludes(&ray, (1e-9, f64::INFINITY), cutting));
        }
    }
    // Between its lobes, where a lobe would have stopped it, a ray passes.
    let way = tube.way(1.05);
    let skimming = Ray::new(
        way * 0.45 + way.cross(Vec3::UP) * -2.0 + Vec3::UP * 0.1,
        way.cross(Vec3::UP),
    );
    let passes = alone.intersect(&skimming, (1e-9, f64::INFINITY), None);
    let reach = 0.3 * flare.factor(0.16, 1.05);
    assert!(
        reach < 0.45 && passes.is_none_or(|hit| hit.t > 2.0),
        "{reach}"
    );
}

/// A ray falling just past an open end's rim meets the limb's side there,
/// and the side faces out, not up the limb as the end it lacks would: so a
/// low stump's cut, its flare still swelling it there, has no lit rim.
#[test]
fn a_hit_at_an_open_ends_rim_faces_out_of_the_side() {
    let flare = Flare::new(0.06, 2.0, (0.3, 0.3), &[]).expect("a flare");
    let top = 0.5;
    let open = Tube::new(
        (Vec3::new(0.0, -0.06, 0.0), Vec3::new(0.0, top, 0.0)),
        ((0.3, 0.29), (0.0, top + 0.06)),
        (0, 5),
        Vec3::new(1.0, 0.0, 0.0),
    )
    .opened([false, true])
    .flared(0);
    let alone = Prototype::flared(
        alloc::vec![Part::Tube(open)],
        (Vec::new(), Vec::new()),
        alloc::vec![flare],
    )
    .expect("a flared limb")
    .whole();
    let up = top + 0.06;
    let rim = open.round_radius(up) * flare.factor(up, 0.0);
    let mut met = 0;
    for step in 0..72u32 {
        let angle = core::f64::consts::TAU * f64::from(step) / 72.0;
        // The limb swells as it falls, so a ray a hair's breadth past its
        // top's rim meets its side right below it.
        let ray = Ray::new(
            open.way(angle) * (rim + 1e-6) + Vec3::UP * (top + 0.5),
            -Vec3::UP,
        );
        if let Some(hit) = alone.intersect(&ray, (1e-9, f64::INFINITY), None) {
            met += 1;
            assert!(hit.normal.y.abs() < 0.5, "{angle}: faces {:?}", hit.normal);
        }
    }
    assert!(met > 36, "{met} of 72 met its side below the rim");
}
