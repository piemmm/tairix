use alloc::vec::Vec;

use tairix_util::mathf;

use super::*;

const LAW: Droplets = Droplets {
    lifetime: 80,
    inertia: 0.1,
    capacity: 4.0,
    min_slope: 0.01,
    erosion: 0.1,
    deposition: 0.1,
    evaporation: 0.02,
    gravity: 4.0,
    friction: 0.1,
    radius: 2,
};

/// A hillside falling toward its low-x edge onto a basin's level floor, the
/// basin's far lip rising again at the edge itself, and the whole rippled
/// across so water gathers in its troughs.
fn hillside(grid: Grid) -> Vec<f32> {
    let run = f64::from(grid.side() - 17);
    (0..grid.area())
        .map(|index| {
            let (x, y) = grid.position(index);
            let (x, y) = (f64::from(x), f64::from(y));
            let slope = ((x - 16.0) / run).max(0.0);
            let lip = ((6.0 - x) / 6.0).max(0.0);
            let ripple = mathf::sin(y * 0.4);
            #[allow(clippy::cast_possible_truncation, reason = "a test fixture's heights")]
            let height = (14.0 * slope * slope + 3.0 * lip * lip + ripple) as f32;
            height
        })
        .collect()
}

/// Ground worn from the steep upper slope is laid down on the gentle foot.
#[test]
fn droplets_carry_the_upper_slope_down_to_its_foot() {
    let grid = Grid::new(64);
    let mut height = hillside(grid);
    let before = height.clone();
    let mut erosion = Erosion::new(grid, LAW, 7).expect("an erosion");
    erosion.run(&mut height, 3000, None).expect("runs");
    assert!(height.iter().all(|h| h.is_finite()));
    let change = |columns: core::ops::Range<u32>| {
        let mut total = 0.0;
        for y in 4..60 {
            for x in columns.clone() {
                let index = grid.index(x, y);
                total += f64::from(height[index] - before[index]);
            }
        }
        total
    };
    let (foot, upper) = (change(6..16), change(32..60));
    assert!(upper < -1.0, "the upper slope lost {upper}");
    assert!(foot > 0.0, "the foot gained {foot}");
}

#[test]
fn level_ground_stays_level() {
    let grid = Grid::new(32);
    let mut height = alloc::vec![3.0_f32; grid.area()];
    let mut erosion = Erosion::new(grid, LAW, 1).expect("an erosion");
    erosion.run(&mut height, 500, None).expect("runs");
    assert!(height.iter().all(|h| (h - 3.0).abs() < 1e-6));
}

#[test]
fn a_run_split_across_calls_leaves_the_ground_one_run_leaves() {
    let grid = Grid::new(48);
    let mut whole = hillside(grid);
    Erosion::new(grid, LAW, 99)
        .expect("an erosion")
        .run(&mut whole, 900, None)
        .expect("runs");
    let mut pieces = hillside(grid);
    let mut erosion = Erosion::new(grid, LAW, 99).expect("an erosion");
    for count in [1, 250, 49, 600] {
        erosion.run(&mut pieces, count, None).expect("runs");
    }
    assert_eq!(pieces, whole);
}

#[test]
fn water_gathers_in_the_hollows_the_droplets_run_through() {
    let grid = Grid::new(64);
    let mut height = hillside(grid);
    let mut flux = alloc::vec![0.0_f32; grid.area()];
    let mut erosion = Erosion::new(grid, LAW, 3).expect("an erosion");
    erosion
        .run(&mut height, 1500, Some(&mut flux))
        .expect("runs");
    // The ripple's troughs lie where `sin(0.4 y)` is least, its crests where
    // it is most.
    let row_flux = |y: u32| {
        (8..56)
            .map(|x| f64::from(flux[grid.index(x, y)]))
            .sum::<f64>()
    };
    let trough = row_flux(12) + row_flux(27);
    let crest = row_flux(4) + row_flux(20);
    assert!(trough > 2.0 * crest, "trough {trough} crest {crest}");
    assert_eq!(
        erosion.run(&mut height[..10], 1, None),
        Err(TerrainError::Shape)
    );
}
