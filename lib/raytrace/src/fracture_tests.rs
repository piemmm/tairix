//! Host tests of torn wood: a break keeps within its outline but for the
//! laths and tatters it frays into, climbs toward the side its fibres pulled
//! out on, faces out everywhere, and rots hollow and loses its splinters as
//! it ages.

use super::*;

const GRAIN: Grain = Grain {
    wood: 1,
    rot: 2,
    edge: 3,
};

/// A break 0.3 across, upright, pulled out toward +x, `age` old, from
/// `seed`.
fn torn(age: f64, seed: u64) -> Mesh {
    let outline = |_: f64| 0.3;
    let brk = Break {
        centre: Vec3::ZERO,
        frame: Frame::WORLD,
        outline: &outline,
        tension: 0.0,
        age,
        barked: true,
    };
    tear(&brk, GRAIN, &mut NonCryptoRng::seed_from_u64(seed)).expect("torn")
}

#[test]
fn a_break_frays_no_further_than_its_laths_and_tatters_reach() {
    for seed in 0..16 {
        let torn = torn(0.1, seed);
        // Its crest climbs up to its radius, its fibres a third of that
        // again, and its laths a radius and a third beyond.
        for point in &torn.points {
            assert!(point.x.hypot(point.z) < 0.3 * 1.8, "{seed}: {point:?}");
            assert!(
                point.y > -0.3 * 0.9 && point.y < 0.3 * 2.7,
                "{seed}: {point:?}"
            );
        }
        for &(corners, material) in &torn.faces {
            assert!(corners
                .iter()
                .all(|&corner| (corner as usize) < torn.points.len()));
            assert!([GRAIN.wood, GRAIN.rot, GRAIN.edge].contains(&material));
        }
    }
}

#[test]
fn a_break_climbs_toward_where_its_fibres_pulled_out() {
    let mut higher = 0;
    for seed in 0..16 {
        let torn = torn(0.1, seed);
        let side = |toward: f64| {
            let (sum, count) = torn
                .points
                .iter()
                .filter(|point| point.x * toward > 0.12 && point.z.abs() < 0.1)
                .fold((0.0, 0u32), |(sum, count), point| {
                    (sum + point.y, count + 1)
                });
            sum / f64::from(count.max(1))
        };
        if side(1.0) > side(-1.0) + 0.03 {
            higher += 1;
        }
    }
    assert!(
        higher >= 14,
        "{higher} of 16 climb toward their pulled side"
    );
}

#[test]
fn every_face_of_a_break_faces_out_of_the_wood() {
    let torn = torn(0.2, 3);
    // Out of the wood is up the face and out round its rim: a face's own
    // normal leans away from the limb's axis below it.
    let below = Vec3::new(0.0, -1.0, 0.0);
    let mut away = 0usize;
    for &([a, b, c], _) in &torn.faces {
        let (a, b, c) = (
            torn.points[a as usize],
            torn.points[b as usize],
            torn.points[c as usize],
        );
        let normal = (b - a).cross(c - a);
        let middle = (a + b + c) * (1.0 / 3.0);
        if normal.dot(middle - below) > 0.0 {
            away += 1;
        }
    }
    assert!(
        away * 10 > torn.faces.len() * 9,
        "{away} of {} face out",
        torn.faces.len()
    );
}

#[test]
fn an_old_break_is_hollow_and_has_lost_its_splinters() {
    let (fresh, old) = (torn(0.05, 7), torn(0.95, 7));
    let rotten = |torn: &Mesh| {
        torn.faces
            .iter()
            .filter(|(_, material)| *material == GRAIN.rot)
            .count()
    };
    assert_eq!(rotten(&fresh), 0);
    assert!(rotten(&old) > 0);
    let deepest = old
        .points
        .iter()
        .map(|point| point.y)
        .fold(f64::INFINITY, f64::min);
    assert!(deepest < -0.05, "a hollow heart: {deepest}");
    let tallest = |torn: &Mesh| torn.points.iter().map(|point| point.y).fold(0.0, f64::max);
    assert!(
        tallest(&fresh) > tallest(&old),
        "{} against {}",
        tallest(&fresh),
        tallest(&old)
    );
    assert!(fresh.faces.len() > old.faces.len());
}
