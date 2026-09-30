use alloc::vec::Vec;

use super::*;
use crate::heightfield::Heightfield;
use crate::scene::Grid;
use crate::shape::Shape;

/// A level field at height nought over `-4..4` each way.
fn level() -> Heightfield {
    let mut field = Heightfield::new(16, (-4.0, -4.0), 0.5, false).expect("a grid");
    let side = field.rows();
    for (_, band) in field.bands(0..side, side) {
        band.fill(0.0);
    }
    field.seal();
    field
}

fn lawn(flowers: f64) -> Lawn {
    Lawn {
        field: 0,
        from: (-1.0, -1.0),
        to: (1.0, 1.0),
        floor: -0.05,
        ceiling: 0.05,
        cell: 0.15,
        blades: 7,
        height: (0.1, 0.3),
        width: 0.01,
        lean: 0.45,
        flowers,
        seed: 3,
    }
}

/// A ray from `height` above a random place over the lawn, looking down
/// across it at anything from steeply to a few degrees.
fn looking_down(index: u32, height: f64) -> Ray {
    let draw = |salt: u32| unit(mix32(mix32(index ^ 0xabc) ^ salt));
    let origin = Vec3::new(2.0 * draw(1) - 1.0, height, 2.0 * draw(2) - 1.0);
    let heading = TAU * draw(3);
    let dip = 0.1 + 1.4 * draw(4);
    let dir = Vec3::new(
        mathf::cos(dip) * mathf::cos(heading),
        -mathf::sin(dip),
        mathf::cos(dip) * mathf::sin(heading),
    );
    Ray::new(origin, dir)
}

#[test]
fn blades_root_all_over_their_cells_and_never_cross_the_walls() {
    let lawn = lawn(1.0);
    let (mut edge, mut total, mut flowered) = (0u32, 0u32, 0u32);
    for cell in 0..400u32 {
        let cell_key = mix32(cell);
        let mut toward = (1.0, 0.0);
        for index in 0..lawn.blades {
            let blade = lawn.blade(cell_key, index, toward);
            toward = turn_golden(toward);
            let tip = (
                blade.root.0 + blade.lean * blade.toward.0,
                blade.root.1 + blade.lean * blade.toward.1,
            );
            for at in [blade.root.0, blade.root.1, tip.0, tip.1] {
                assert!(
                    at >= lawn.width - 1e-12 && at <= lawn.cell - lawn.width + 1e-12,
                    "{blade:?}"
                );
            }
            if let Some(radius) = blade.flower {
                flowered += 1;
                for at in [tip.0, tip.1] {
                    assert!(at - radius >= 0.0 && at + radius <= lawn.cell, "{blade:?}");
                }
            }
            assert_eq!(blade.key & FLOWER, 0);
            let near_wall = |at: f64| at < 0.2 * lawn.cell || at > 0.8 * lawn.cell;
            if near_wall(blade.root.0) || near_wall(blade.root.1) {
                edge += 1;
            }
            total += 1;
        }
    }
    // Rooted evenly, about three in five fall within a fifth of the walls:
    // none do if the blades crowd the middle, and the lawn shows its grid.
    assert!(edge * 5 > total * 2, "{edge} of {total} near the walls");
    assert!(flowered > 0);
}

#[test]
fn blades_stand_on_the_ground_within_the_lawn() {
    let fields = [level()];
    let geometry = Geometry {
        faces: &[],
        fields: &fields,
    };
    let lawn = lawn(0.05);
    let mut met = 0;
    for index in 0..3000 {
        let ray = looking_down(index, 0.6);
        let Some(hit) = lawn.intersect(&ray, 1e-9, f64::INFINITY, geometry) else {
            continue;
        };
        met += 1;
        let at = ray.at(hit.t);
        assert!(hit.t > 0.0);
        assert!(
            (-1.0..=1.0).contains(&at.x) && (-1.0..=1.0).contains(&at.z),
            "{at:?}"
        );
        assert!(at.y >= -0.011 && at.y <= 0.3 + 0.009, "{at:?}");
        assert!((hit.normal.length() - 1.0).abs() < 1e-9);
        assert!((0.0..=1.0).contains(&hit.along));
    }
    assert!(met > 250, "{met} of 3000 met the grass");
}

#[test]
fn the_blade_met_is_the_nearest() {
    let fields = [level()];
    let geometry = Geometry {
        faces: &[],
        fields: &fields,
    };
    let lawn = lawn(0.1);
    for index in 0..1500 {
        let ray = looking_down(index, 0.6);
        let Some(hit) = lawn.intersect(&ray, 1e-9, f64::INFINITY, geometry) else {
            continue;
        };
        assert!(
            lawn.intersect(&ray, 1e-9, hit.t * (1.0 - 1e-9), geometry)
                .is_none(),
            "ray {index}: something nearer than {}",
            hit.t
        );
        let again = lawn
            .intersect(&ray, 1e-9, f64::INFINITY, geometry)
            .expect("met again");
        assert_eq!((again.t, again.mark), (hit.t, hit.mark));
    }
}

#[test]
fn flowers_are_met_at_the_tips_of_their_blades() {
    let fields = [level()];
    let geometry = Geometry {
        faces: &[],
        fields: &fields,
    };
    let lawn = lawn(1.0);
    let mut flowers = 0;
    for index in 0..3000 {
        let ray = looking_down(index, 0.6);
        let Some(hit) = lawn.intersect(&ray, 1e-9, f64::INFINITY, geometry) else {
            continue;
        };
        if hit.mark & FLOWER != 0 {
            flowers += 1;
            assert!(ray.at(hit.t).y >= 0.08, "a flower heads its blade");
            assert!((hit.along - 1.0).abs() < 1e-12);
        }
    }
    assert!(flowers > 100, "{flowers} flowers met");
}

#[test]
fn nothing_is_met_above_the_blades_or_beside_the_lawn() {
    let fields = [level()];
    let geometry = Geometry {
        faces: &[],
        fields: &fields,
    };
    let lawn = lawn(0.1);
    let over = Ray::new(Vec3::new(-3.0, 0.5, 0.0), Vec3::new(1.0, 0.0, 0.0));
    assert!(lawn
        .intersect(&over, 1e-9, f64::INFINITY, geometry)
        .is_none());
    let beside = Ray::new(Vec3::new(2.5, 1.0, 0.0), -Vec3::UP);
    assert!(lawn
        .intersect(&beside, 1e-9, f64::INFINITY, geometry)
        .is_none());
    assert!(!Shape::Lawn(lawn).casts_shadow());
}

#[test]
fn a_golden_turn_keeps_a_heading_unit_and_spreads_a_cell_evenly() {
    let mut toward = (1.0, 0.0);
    let mut headings = Vec::new();
    for _ in 0..8 {
        headings.push(mathf::atan2(toward.1, toward.0));
        toward = turn_golden(toward);
        assert!((toward.0 * toward.0 + toward.1 * toward.1 - 1.0).abs() < 1e-12);
    }
    headings.sort_by(f64::total_cmp);
    // Eight headings, none crowding another: every gap within twice the
    // even share of the circle.
    let gaps = headings.windows(2).map(|pair| pair[1] - pair[0]);
    let wrap = TAU - (headings[7] - headings[0]);
    for gap in gaps.chain([wrap]) {
        assert!(gap < 2.0 * TAU / 8.0, "{headings:?}");
    }
}
