use core::f64::consts::TAU;

use super::*;

fn crown(surface: f64, heart: f64) -> Crown {
    Crown {
        centre: Vec3::new(1.0, 5.0, -2.0),
        radii: Vec3::new(2.0, 1.5, 2.0),
        cell: 0.2,
        leaf: 0.085,
        breadth: 0.55,
        surface,
        heart,
        lift: 0.45,
        seed: 7,
    }
}

/// A ray from well outside the crown toward a point near its middle.
fn inward(index: u32, crown: &Crown) -> Ray {
    let draw = |salt: u32| unit(mix32(mix32(index ^ 0x3c3c) ^ salt));
    let rise = 2.0 * draw(1) - 1.0;
    let around = TAU * draw(2);
    let level = mathf::sqrt(1.0 - rise * rise);
    let from = crown.centre
        + Vec3::new(level * mathf::cos(around), rise, level * mathf::sin(around)) * 6.0;
    let aim = crown.centre + Vec3::new(draw(3) - 0.5, draw(4) - 0.5, draw(5) - 0.5);
    Ray::new(from, (aim - from).normalized())
}

/// How far out of the crown's ellipsoid `p` lies: `1.0` on its surface.
fn depth(crown: &Crown, p: Vec3) -> f64 {
    let d = p - crown.centre;
    Vec3::new(
        d.x / crown.radii.x,
        d.y / crown.radii.y,
        d.z / crown.radii.z,
    )
    .length()
}

#[test]
fn a_full_crown_is_met_on_its_leaves_within_its_bounds() {
    let crown = crown(1.0, 1.0);
    let bounds = crown.bounds();
    let mut stopped = 0;
    for index in 0..1000 {
        let ray = inward(index, &crown);
        let Some(hit) = crown.intersect(&ray, 1e-9, f64::INFINITY) else {
            continue;
        };
        stopped += 1;
        let at = ray.at(hit.t);
        // A leaf is kept only if its middle lies within the crown, and it
        // reaches a cell's diagonal at most beyond that.
        let slack = crown.cell * 0.9 / crown.radii.y;
        assert!(depth(&crown, at) <= 1.0 + slack, "ray {index} met {at:?}");
        assert!(at.x >= bounds.min.x - crown.cell && at.x <= bounds.max.x + crown.cell);
        assert!((hit.normal.length() - 1.0).abs() < 1e-9);
        assert!((0.0..=1.0).contains(&hit.along));
        assert_eq!(hit.mark & FLOWER, 0, "a leaf is never taken for a flower");
    }
    // A leaf fills only part of its cell, so even a crown with a leaf in
    // every cell lets the odd ray slip between them.
    assert!(stopped > 900, "{stopped} of 1000 stopped");
}

#[test]
fn a_bare_crown_and_a_ray_past_it_meet_nothing() {
    let bare = crown(0.0, 0.0);
    for index in 0..200 {
        assert!(bare
            .intersect(&inward(index, &bare), 1e-9, f64::INFINITY)
            .is_none());
    }
    let full = crown(1.0, 1.0);
    let past = Ray::new(
        full.centre + Vec3::new(-6.0, 3.0, 0.0),
        Vec3::new(1.0, 0.0, 0.0),
    );
    assert!(full.intersect(&past, 1e-9, f64::INFINITY).is_none());
}

#[test]
fn a_sparse_crown_lets_light_through_its_gaps() {
    let crown = crown(0.35, 0.05);
    let met = (0..1000)
        .filter(|index| {
            crown
                .intersect(&inward(*index, &crown), 1e-9, f64::INFINITY)
                .is_some()
        })
        .count();
    assert!(met > 200 && met < 950, "{met} of 1000 stopped");
}

#[test]
fn the_leaf_met_is_the_nearest_and_the_same_every_time() {
    let crown = crown(0.6, 0.15);
    for index in 0..1000 {
        let ray = inward(index, &crown);
        let Some(hit) = crown.intersect(&ray, 1e-9, f64::INFINITY) else {
            continue;
        };
        assert!(
            crown.intersect(&ray, 1e-9, hit.t * (1.0 - 1e-9)).is_none(),
            "ray {index}"
        );
        let again = crown
            .intersect(&ray, 1e-9, f64::INFINITY)
            .expect("met again");
        assert_eq!((again.t, again.mark), (hit.t, hit.mark));
    }
}

#[test]
fn another_seed_grows_other_leaves() {
    let (one, other) = (
        crown(0.6, 0.15),
        Crown {
            seed: 8,
            ..crown(0.6, 0.15)
        },
    );
    let (mut met, mut differ) = (0, 0);
    for index in 0..300 {
        let ray = inward(index, &one);
        let (a, b) = (
            one.intersect(&ray, 1e-9, f64::INFINITY),
            other.intersect(&ray, 1e-9, f64::INFINITY),
        );
        if a.is_some() || b.is_some() {
            met += 1;
            if a.map(|hit| hit.t) != b.map(|hit| hit.t) {
                differ += 1;
            }
        }
    }
    assert!(
        met > 150 && differ * 10 > met * 9,
        "{differ} of {met} differ"
    );
}
