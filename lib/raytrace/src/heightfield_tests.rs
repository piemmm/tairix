use core::f64::consts::TAU;

use super::*;
use crate::sample::{mix32, unit};

/// A grid of `cells` a side, `step` apart from `origin`, filled from
/// `height` and sealed.
fn filled(
    cells: usize,
    origin: (f64, f64),
    step: f64,
    wrap: bool,
    height: impl Fn(f64, f64) -> f64,
) -> Heightfield {
    let mut field = Heightfield::new(cells, origin, step, wrap).expect("a grid");
    let (layout, side) = (field.layout(), field.rows());
    for (start, band) in field.bands(0..side, 3) {
        for (offset, row) in band.chunks_mut(side).enumerate() {
            for (column, cell) in row.iter_mut().enumerate() {
                let (x, z) = layout.vertex(column, start + offset);
                // The grid holds `f32` heights, as the scene's grids do.
                #[allow(clippy::cast_possible_truncation)]
                let narrowed = height(x, z) as f32;
                *cell = narrowed;
            }
        }
    }
    field.seal();
    field
}

/// Rugged ground: ridges and hollows a few cells across.
fn rugged(x: f64, z: f64) -> f64 {
    3.0 * mathf::sin(0.9 * x) * mathf::cos(0.7 * z) + 1.5 * mathf::sin(2.3 * x + 1.1 * z)
}

/// A ray from above the grid at a random place, looking down across it at
/// anything from steeply to a grazing few degrees.
fn looking_down(index: u32, span: f64, height: f64) -> Ray {
    let draw = |salt: u32| unit(mix32(mix32(index) ^ salt));
    let origin = Vec3::new(span * draw(1), height, span * draw(2));
    let heading = TAU * draw(3);
    let dip = 0.03 + 1.2 * draw(4);
    let dir = Vec3::new(
        mathf::cos(dip) * mathf::cos(heading),
        -mathf::sin(dip),
        mathf::cos(dip) * mathf::sin(heading),
    );
    Ray::new(origin, dir)
}

#[test]
fn a_grid_is_a_power_of_two_a_side_and_steps_forward() {
    assert!(Heightfield::new(3, (0.0, 0.0), 1.0, false).is_none());
    assert!(Heightfield::new(0, (0.0, 0.0), 1.0, false).is_none());
    assert!(Heightfield::new(4, (0.0, 0.0), 0.0, false).is_none());
    assert!(Heightfield::new(4, (0.0, 0.0), -1.0, false).is_none());
    assert!(Heightfield::new(4, (0.0, 0.0), f64::NAN, false).is_none());
    assert!(Heightfield::new(4, (0.0, 0.0), 1.0, false).is_some());
}

#[test]
fn sealing_finds_the_extremes_and_the_pyramid_tops_out_at_the_highest() {
    let field = filled(16, (-8.0, -8.0), 1.0, false, rugged);
    let (low, high) = field
        .heights
        .iter()
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(low, high), h| {
            (low.min(f64::from(*h)), high.max(f64::from(*h)))
        });
    assert_eq!((field.low, field.high), (low, high));
    let (top, blocks) = *field.levels.last().expect("levels");
    assert_eq!(blocks, 1);
    assert!((f64::from(field.maxima[top]) - high).abs() < 1e-12);
    let bounds = field.bounds().expect("a bounded grid");
    assert_eq!((bounds.min.y, bounds.max.y), (low, high));
    assert_eq!((bounds.min.x, bounds.max.x), (-8.0, 8.0));
}

#[test]
fn a_sloping_plane_is_met_where_it_lies() {
    let plane = |x: f64, z: f64| 0.3 * x - 0.2 * z + 1.0;
    let field = filled(32, (0.0, 0.0), 0.5, false, plane);
    let normal = Vec3::new(-0.3, 1.0, 0.2).normalized();
    let mut met = 0;
    for index in 0..500 {
        let ray = looking_down(index, 16.0, 12.0);
        // Where the ray meets the plane itself: a ray nearly along it, or
        // meeting it at the grid's very edge, says little either way.
        let rate = ray.dir.y - 0.3 * ray.dir.x + 0.2 * ray.dir.z;
        if rate.abs() < 0.1 {
            continue;
        }
        let t = -(ray.origin.y - plane(ray.origin.x, ray.origin.z)) / rate;
        let at = ray.at(t);
        let within =
            |low: f64, high: f64| (low..=high).contains(&at.x) && (low..=high).contains(&at.z);
        if within(-0.01, 16.01) && !within(0.01, 15.99) {
            continue;
        }
        match field.intersect(&ray, 1e-9, f64::INFINITY) {
            Some(hit) => {
                assert!(
                    within(0.0, 16.0),
                    "a hit off the grid at {:?}",
                    ray.at(hit.t)
                );
                assert!(
                    (hit.t - t).abs() < 1e-5 * (1.0 + t),
                    "{} against {t}",
                    hit.t
                );
                assert!((hit.normal - normal).length() < 1e-5);
                assert!((hit.shading - normal).length() < 1e-5);
                met += 1;
            }
            None => assert!(!within(0.0, 16.0), "a miss at {at:?}, on the grid"),
        }
    }
    assert!(met > 100, "{met} of 500 met the plane");
}

/// Every hit on rugged ground lies on the surface the grid describes, and
/// nothing of that surface stands in the ray's way before it: no cell's
/// patch is met beyond the cell, and no nearer cell is passed over.
#[test]
fn every_hit_is_the_nearest_meeting_with_the_surface() {
    let field = filled(64, (0.0, 0.0), 0.25, false, rugged);
    let mut met = 0;
    for index in 0..2000 {
        let ray = looking_down(index, 16.0, 8.0);
        let Some(hit) = field.intersect(&ray, 1e-9, f64::INFINITY) else {
            continue;
        };
        met += 1;
        let at = ray.at(hit.t);
        let surface = field.height_at(at.x, at.z);
        assert!(
            (at.y - surface).abs() < 1e-5,
            "ray {index} met {at:?} above {surface}"
        );
        let mut t = 0.0;
        while t < hit.t - 1e-3 {
            let before = ray.at(t);
            let inside = (0.0..16.0).contains(&before.x) && (0.0..16.0).contains(&before.z);
            assert!(
                !inside || before.y >= field.height_at(before.x, before.z) - 1e-6,
                "ray {index} passed under the surface at {before:?} before meeting it at {at:?}"
            );
            t += 0.01;
        }
    }
    assert!(met > 500, "{met} of 2000 met the ground");
}

#[test]
fn a_ray_above_or_below_everything_meets_nothing() {
    let field = filled(16, (0.0, 0.0), 1.0, false, rugged);
    let over = Ray::new(Vec3::new(-5.0, 20.0, 8.0), Vec3::new(1.0, 0.0, 0.0));
    assert!(field.intersect(&over, 1e-9, f64::INFINITY).is_none());
    let under = Ray::new(Vec3::new(8.0, -20.0, 8.0), Vec3::new(0.0, -1.0, 0.0));
    assert!(field.intersect(&under, 1e-9, f64::INFINITY).is_none());
    let short = Ray::new(Vec3::new(8.0, 20.0, 8.0), Vec3::new(0.0, -1.0, 0.0));
    assert!(
        field.intersect(&short, 1e-9, 5.0).is_none(),
        "the ground lies beyond the reach"
    );
}

#[test]
fn shading_normals_are_smooth_across_cell_walls() {
    let field = filled(32, (0.0, 0.0), 0.5, false, rugged);
    for wall in 2..30 {
        let x = 0.5 * f64::from(wall);
        let down = |x: f64| {
            field.intersect(
                &Ray::new(Vec3::new(x, 20.0, 7.3), -Vec3::UP),
                1e-9,
                f64::INFINITY,
            )
        };
        let (left, right) = (down(x - 1e-7).expect("met"), down(x + 1e-7).expect("met"));
        assert!(
            (left.shading - right.shading).length() < 1e-4,
            "wall {wall}"
        );
    }
}

#[test]
fn a_wrapping_grid_repeats_across_the_plane_without_end() {
    let span = 16.0;
    let wave = |x: f64, z: f64| mathf::sin(TAU * x / span) * mathf::cos(TAU * 2.0 * z / span);
    let field = filled(64, (0.0, 0.0), span / 64.0, true, wave);
    assert!(field.bounds().is_none());
    for index in 0..200 {
        let draw = |salt: u32| unit(mix32(mix32(index) ^ salt));
        let (x, z) = (span * draw(1), span * draw(2));
        let there = field.height_at(x, z);
        assert!((field.height_at(x + 3.0 * span, z - 2.0 * span) - there).abs() < 1e-6);
        assert!((field.height_at(x - span, z + 5.0 * span) - there).abs() < 1e-6);
    }
    // Far from the first tile, and over many tiles, the surface is still
    // met where it lies.
    let mut met = 0;
    for index in 0..500 {
        let base = looking_down(index, span, 3.0);
        let ray = Ray::new(
            base.origin + Vec3::new(-250.0, 0.0, 130.0),
            Vec3::new(base.dir.x, -0.02, base.dir.z).normalized(),
        );
        let Some(hit) = field.intersect(&ray, 1e-9, f64::INFINITY) else {
            continue;
        };
        met += 1;
        let at = ray.at(hit.t);
        assert!(
            (at.y - field.height_at(at.x, at.z)).abs() < 1e-5,
            "ray {index} at {at:?}"
        );
    }
    assert!(met > 400, "{met} of 500 met the sea");
}
