use alloc::vec::Vec;

use tairix_util::mathf;

use super::*;

/// A bowl `side` across with a notch cut in its east rim at `notch`, on a
/// plane falling gently east.
fn bowl(side: u32, notch: f64) -> Vec<f64> {
    let middle = f64::from(side - 1) / 2.0;
    let mut height = Vec::new();
    for y in 0..side {
        for x in 0..side {
            let (dx, dy) = (f64::from(x) - middle, f64::from(y) - middle);
            let rim = mathf::sqrt(dx * dx + dy * dy).min(middle);
            let mut h = 10.0 + rim * 2.0 - 0.01 * f64::from(x);
            if y == side / 2 && x > side / 2 {
                h = h.min(notch);
            }
            height.push(h);
        }
    }
    height
}

fn rim(grid: Grid) -> impl Fn(usize) -> bool {
    move |index| {
        let (x, y) = grid.position(index);
        grid.is_rim(x, y)
    }
}

fn solved(height: &[f64], grid: Grid) -> Network {
    Network::solve(height, grid, rim(grid)).expect("solves")
}

#[test]
fn a_pit_fills_to_the_height_of_its_outflow() {
    let grid = Grid::new(21);
    let height = bowl(21, 12.0);
    let network = solved(&height, grid);
    let middle = grid.index(10, 10);
    assert!(height[middle] < 12.0);
    assert!((network.filled[middle] - 12.0).abs() < 1e-9);
    for (filled, ground) in network.filled.iter().zip(&height) {
        assert!(filled >= ground);
    }
}

#[test]
fn every_sample_drains_to_a_sink_without_a_cycle() {
    let grid = Grid::new(33);
    let height: Vec<f64> = (0..grid.area())
        .map(|index| {
            let (x, y) = grid.position(index);
            mathf::sin(f64::from(x) * 0.7) * 3.0 + mathf::cos(f64::from(y) * 0.45) * 2.0
        })
        .collect();
    let network = solved(&height, grid);
    for start in 0..grid.area() {
        let mut here = start;
        let mut steps = 0;
        while let Some(next) = downstream(grid, &network.flow, here) {
            assert!(network.filled[next] <= network.filled[here]);
            here = next;
            steps += 1;
            assert!(steps <= grid.area(), "a cycle through {start}");
        }
        let (x, y) = grid.position(here);
        assert!(grid.is_rim(x, y), "{start} ends inland");
    }
    let sunk: u64 = (0..grid.area())
        .filter(|&index| network.flow[index] == FlowDir::Sink)
        .map(|index| u64::from(network.discharge[index]))
        .sum();
    assert_eq!(sunk, grid.area() as u64, "every sample drains exactly once");
}

#[test]
fn a_flood_advanced_in_any_budget_matches_one_run_to_its_end() {
    let grid = Grid::new(25);
    let height = bowl(25, 13.0);
    let whole = {
        let mut flood = Flood::new(&height, grid, rim(grid)).expect("floods");
        assert_eq!(flood.advance(&height, usize::MAX), Ok(true));
        flood.into_parts()
    };
    for budget in [1, 7, 300] {
        let mut flood = Flood::new(&height, grid, rim(grid)).expect("floods");
        while !flood.advance(&height, budget).expect("one grid") {}
        assert_eq!(flood.into_parts(), whole, "budget {budget}");
    }
}

#[test]
fn routing_and_accumulating_in_pieces_matches_doing_either_whole() {
    let grid = Grid::new(19);
    let height = bowl(19, 11.5);
    let network = solved(&height, grid);
    let mut flood = Flood::new(&height, grid, rim(grid)).expect("floods");
    flood.advance(&height, usize::MAX).expect("one grid");
    let mut flow = alloc::vec![FlowDir::Sink; grid.area()];
    for band in (0..19).step_by(4) {
        route(
            (flood.filled(), flood.rank()),
            grid,
            rim(grid),
            &mut flow,
            band..band + 4,
        )
        .expect("routes");
    }
    assert_eq!(flow, network.flow);
    let mut discharge = alloc::vec![1_u32; grid.area()];
    let order = flood.order();
    let mut end = order.len();
    while end > 0 {
        let start = end.saturating_sub(37);
        accumulate((order, &flow), grid, &mut discharge, start..end).expect("accumulates");
        end = start;
    }
    assert_eq!(discharge, network.discharge);
}

#[test]
fn a_buffer_of_the_wrong_size_is_refused() {
    let grid = Grid::new(8);
    assert_eq!(
        Flood::new(&[0.0; 10], grid, |_| true).map(|_| ()),
        Err(TerrainError::Shape)
    );
    // Advanced over another grid, a flood refuses rather than never finishing.
    let mut flood = Flood::new(&[0.0; 64], grid, |_| true).expect("floods");
    assert_eq!(
        flood.advance(&[0.0; 10], usize::MAX),
        Err(TerrainError::Shape)
    );
    let mut flow = alloc::vec![FlowDir::Sink; 3];
    assert_eq!(
        route((&[0.0; 64], &[0; 64]), grid, |_| true, &mut flow, 0..8),
        Err(TerrainError::Shape)
    );
}
