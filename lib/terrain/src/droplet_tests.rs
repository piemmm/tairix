use alloc::vec::Vec;

use tairix_parallel::{Reversed, Threaded, SERIAL};
use tairix_util::mathf;

use super::*;
use tairix_parallel::JobRunner;

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

/// Run every droplet of `erosion` over `height`, and `flux` if given, across
/// `runner`, returning how many calls it took.
fn run_all(
    erosion: &mut Erosion,
    height: &mut [f32],
    mut flux: Option<&mut [f32]>,
    runner: &dyn JobRunner,
) -> u32 {
    let mut calls = 1;
    while !erosion
        .run(height, flux.as_deref_mut(), runner)
        .expect("runs")
    {
        calls += 1;
    }
    calls
}

/// Ground worn from the steep upper slope is laid down on the gentle foot.
#[test]
fn droplets_carry_the_upper_slope_down_to_its_foot() {
    let grid = Grid::new(64);
    let mut height = hillside(grid);
    let before = height.clone();
    let mut erosion = Erosion::new(grid, LAW, 0.9, 7).expect("an erosion");
    assert!(
        (2800..3100).contains(&erosion.total()),
        "{}",
        erosion.total()
    );
    run_all(&mut erosion, &mut height, None, &SERIAL);
    assert_eq!(erosion.left(), 0);
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
    let mut erosion = Erosion::new(grid, LAW, 0.8, 1).expect("an erosion");
    run_all(&mut erosion, &mut height, None, &SERIAL);
    assert!(height.iter().all(|h| (h - 3.0).abs() < 1e-6));
}

/// A run's tiles shared among any number of cores, in whatever order they
/// take them, leave the ground and the water's tracks exactly as one core
/// running them in turn leaves them; and the run is many calls, never one.
#[test]
fn a_run_shared_among_cores_leaves_the_ground_one_core_leaves() {
    let grid = Grid::new(600);
    let ground = |runner: &dyn JobRunner| {
        let mut height = hillside(grid);
        let mut flux = alloc::vec![0.0_f32; grid.area()];
        let mut erosion = Erosion::new(grid, LAW, 0.15, 99).expect("an erosion");
        assert!(erosion.tiles > STRIDE, "{} tiles a side", erosion.tiles);
        let calls = run_all(&mut erosion, &mut height, Some(&mut flux), runner);
        let turns = erosion.total().div_ceil(TURN) as usize;
        assert!(calls as usize * runner.width() >= turns, "{calls} calls");
        (height, flux)
    };
    let alone = ground(&SERIAL);
    assert_ne!(alone.0, hillside(grid), "the droplets wore nothing");
    for runner in [&Reversed::new(3) as &dyn JobRunner, &Threaded::new(4)] {
        assert_eq!(ground(runner), alone);
    }
}

/// No two tiles of one phase can reach the same sample: the windows a phase
/// runs at once lie apart, and each holds its tile's cells and a droplet's
/// whole reach about them, wherever the grid's edges allow.
#[test]
fn the_tiles_of_a_phase_reach_no_sample_in_common() {
    let grid = Grid::new(1025);
    let erosion = Erosion::new(grid, LAW, 0.01, 5).expect("an erosion");
    let side = grid.side();
    let mut seen = 0;
    for phase in 0..PHASES {
        let windows: Vec<Window> = phase_tiles(erosion.tiles, phase)
            .map(|tile| erosion.window(tile))
            .collect();
        for (index, a) in windows.iter().enumerate() {
            for b in &windows[index + 1..] {
                let apart = a.columns.1 <= b.columns.0
                    || b.columns.1 <= a.columns.0
                    || a.rows.1 <= b.rows.0
                    || b.rows.1 <= a.rows.0;
                assert!(apart, "phase {phase}: {a:?} and {b:?} overlap");
            }
        }
        for (column, row) in phase_tiles(erosion.tiles, phase) {
            let window = erosion.window((column, row));
            let holds = |(start, end): (u32, u32), at: u32| {
                let low = (at * erosion.tile).saturating_sub(erosion.reach);
                let high = ((at + 1) * erosion.tile + erosion.reach + 1).min(side);
                start <= low && end >= high
            };
            assert!(holds(window.columns, column) && holds(window.rows, row));
            seen += 1;
        }
    }
    assert_eq!(seen, (erosion.tiles * erosion.tiles) as usize);
}

#[test]
fn water_gathers_in_the_hollows_the_droplets_run_through() {
    let grid = Grid::new(64);
    let mut height = hillside(grid);
    let mut flux = alloc::vec![0.0_f32; grid.area()];
    let mut erosion = Erosion::new(grid, LAW, 0.45, 3).expect("an erosion");
    run_all(&mut erosion, &mut height, Some(&mut flux), &SERIAL);
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
        erosion.run(&mut height[..10], None, &SERIAL),
        Err(TerrainError::Shape)
    );
}
