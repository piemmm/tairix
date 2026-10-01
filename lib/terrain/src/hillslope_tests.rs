use alloc::vec::Vec;

use super::*;

/// A level grid at `level` with one column `rise` above it at `(x, y)`.
fn spike(grid: Grid, level: f64, (x, y, rise): (u32, u32, f64)) -> Vec<f64> {
    let mut height = alloc::vec![level; grid.area()];
    height[grid.index(x, y)] += rise;
    height
}

#[test]
fn diffusion_spreads_a_spike_to_its_neighbours_and_keeps_to_the_floor() {
    let grid = Grid::new(9);
    let mut height = spike(grid, 1.0, (4, 4, 4.0));
    diffuse(
        &mut height,
        grid,
        Diffusion {
            rate: 0.2,
            floor: 0.0,
        },
    )
    .expect("diffuses");
    assert!(height[grid.index(4, 4)] < 5.0);
    assert!(height[grid.index(5, 4)] > 1.0 && height[grid.index(4, 3)] > 1.0);
    assert!((height[grid.index(0, 8)] - 1.0).abs() < 1e-12);

    let mut sunk = spike(grid, -1.0, (2, 2, 0.5));
    let law = Diffusion {
        rate: 0.2,
        floor: 0.0,
    };
    diffuse(&mut sunk, grid, law).expect("diffuses");
    assert_eq!(
        sunk,
        spike(grid, -1.0, (2, 2, 0.5)),
        "below the floor, untouched"
    );
}

#[test]
fn diffusion_in_bands_of_rows_matches_one_whole_pass() {
    let grid = Grid::new(13);
    let start: Vec<f64> = (0..grid.side() * grid.side())
        .map(|i| f64::from(i * 37 % 11))
        .collect();
    let law = Diffusion {
        rate: 0.18,
        floor: -10.0,
    };
    let mut whole = start.clone();
    diffuse(&mut whole, grid, law).expect("diffuses");
    let before = snapshot(&start).expect("copies");
    let mut bands = start.clone();
    for band in (0..13).step_by(5) {
        diffuse_rows(&before, &mut bands, grid, law, band..band + 5).expect("diffuses");
    }
    assert_eq!(bands, whole);
}

#[test]
fn a_column_too_steep_to_stand_slumps_to_its_repose_and_nothing_is_lost() {
    let grid = Grid::new(11);
    let mut height = spike(grid, 0.0, (5, 5, 20.0));
    let law = Talus {
        drop: 1.0,
        rate: 0.5,
    };
    let total = |height: &[f64]| height.iter().sum::<f64>();
    let mass = total(&height);
    for _ in 0..400 {
        slump(&mut height, grid, law).expect("slumps");
    }
    assert!(
        (total(&height) - mass).abs() < 1e-9,
        "talus moves ground, never makes it"
    );
    for y in 0..11 {
        for x in 0..11 {
            for (_, dx, dy, distance) in NEIGHBOURS {
                if let Some(next) = grid.neighbour(x, y, dx, dy) {
                    let rise = height[grid.index(x, y)] - height[next];
                    assert!(
                        rise <= law.drop * distance + 1e-3,
                        "({x}, {y}) still stands at {rise}"
                    );
                }
            }
        }
    }
}

#[test]
fn ground_at_its_repose_stays_put() {
    let grid = Grid::new(7);
    let ramp: Vec<f64> = (0..grid.area())
        .map(|index| 0.5 * f64::from(grid.position(index).0))
        .collect();
    let mut height = ramp.clone();
    slump(
        &mut height,
        grid,
        Talus {
            drop: 0.6,
            rate: 0.4,
        },
    )
    .expect("slumps");
    assert_eq!(height, ramp);
}
