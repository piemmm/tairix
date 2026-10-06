use super::*;
use crate::vector::Vec3;

/// The cells a walk of `ray` from `t` crosses before it is `to` along it.
fn crossed(ray: &Ray, (t, to): (f64, f64)) -> alloc::vec::Vec<(u32, u32)> {
    let mut walk = Walk::from(((0.0, 0.0), 2.0), ray, t);
    let mut cells = alloc::vec![walk.cell];
    while walk.exit() < to {
        walk.step();
        cells.push(walk.cell);
    }
    cells
}

#[test]
fn a_walk_crosses_each_cell_a_ray_passes_over_in_order_one_wall_at_a_time() {
    let ray = Ray::new(
        Vec3::new(1.0, 5.0, 1.3),
        Vec3::new(3.0, -0.1, 1.0).normalized(),
    );
    let cells = crossed(&ray, (0.0, 20.0));
    assert_eq!(cells.first(), Some(&(0, 0)));
    for pair in cells.windows(2) {
        let ((ax, az), (bx, bz)) = (pair[0], pair[1]);
        assert_eq!(bx.abs_diff(ax) + bz.abs_diff(az), 1, "{pair:?}");
    }
    // Each cell holds the point midway between where the ray enters it and
    // where it leaves.
    let mut walk = Walk::from(((0.0, 0.0), 2.0), &ray, 0.0);
    let mut entered = 0.0;
    let mut checked = 0;
    while entered < 20.0 {
        let left = walk.exit();
        let middle = ray.at(f64::midpoint(entered, left));
        assert_eq!(
            walk.cell,
            (cell(middle.x / 2.0).0, cell(middle.z / 2.0).0),
            "at {middle:?}"
        );
        checked += 1;
        entered = left;
        walk.step();
    }
    assert!(checked > 10, "{checked}");
}

#[test]
fn a_walk_steps_back_across_x_and_z_as_the_ray_runs_back() {
    let ray = Ray::new(
        Vec3::new(9.0, 1.0, 9.0),
        Vec3::new(-1.0, 0.0, -2.0).normalized(),
    );
    let mut walk = Walk::from(((0.0, 0.0), 2.0), &ray, 0.0);
    assert_eq!(walk.cell, (4, 4));
    let mut steps = (0, 0);
    while walk.exit() < 8.0 {
        match walk.step() {
            Stepped::X(step) => {
                assert_eq!(step, u32::MAX);
                steps.0 += 1;
            }
            Stepped::Z(step) => {
                assert_eq!(step, u32::MAX);
                steps.1 += 1;
            }
        }
    }
    assert!(steps.1 > steps.0, "{steps:?}");
    // A ray along z alone never crosses a wall across x.
    let along = Ray::new(Vec3::new(1.0, 1.0, 1.0), Vec3::new(0.0, 0.0, 1.0));
    let walk = Walk::from(((0.0, 0.0), 2.0), &along, 0.0);
    assert!((walk.exit() - 1.0).abs() < 1e-12);
    assert_eq!(
        crossed(&along, (0.0, 6.5)),
        [(0, 0), (0, 1), (0, 2), (0, 3)]
    );
}

#[test]
fn a_walk_within_a_bounded_grid_starts_on_its_nearest_cell_and_advances_to_where_a_ray_has_come() {
    // Entering a hair past the far edge, and a hair short of the near one.
    let beyond = Ray::new(Vec3::new(8.000_000_1, 1.0, 1.0), Vec3::new(-1.0, 0.0, 0.0));
    let walk = Walk::within((((0.0, 0.0), 2.0), 4), &beyond, 0.0);
    assert_eq!(walk.cell, (3, 0));
    assert!((walk.exit() - 2.000_000_1).abs() < 1e-9, "{}", walk.exit());
    let short = Ray::new(Vec3::new(-1e-9, 1.0, 1.0), Vec3::new(1.0, 0.0, 0.0));
    let walk = Walk::within((((0.0, 0.0), 2.0), 4), &short, 0.0);
    assert_eq!(walk.cell, (0, 0));
    assert!((walk.exit() - 2.0).abs() < 1e-6);
    // On to where it has come, however many walls that crosses.
    let ray = Ray::new(
        Vec3::new(1.0, 1.0, 1.0),
        Vec3::new(1.0, 0.0, 1.0).normalized(),
    );
    let mut walk = Walk::within((((0.0, 0.0), 2.0), 8), &ray, 0.0);
    walk.advance(5.0);
    let at = ray.at(5.0);
    assert_eq!(walk.cell, (cell(at.x / 2.0).0, cell(at.z / 2.0).0));
    assert!(walk.exit() > 5.0);
}
