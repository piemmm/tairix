use alloc::vec::Vec;

use super::*;
use crate::drainage::Network;

/// A ramp falling toward its west edge, a valley line down its middle row.
fn ramp(grid: Grid) -> Vec<f64> {
    (0..grid.area())
        .map(|index| {
            let (x, y) = grid.position(index);
            let across = (f64::from(y) - f64::from(grid.side() / 2)).abs();
            2.0 + 0.5 * f64::from(x) + 0.3 * across
        })
        .collect()
}

fn west(grid: Grid) -> impl Fn(usize) -> bool {
    move |index| grid.position(index).0 == 0
}

fn network(height: &[f64], grid: Grid) -> Network {
    Network::solve(height, grid, west(grid)).expect("solves")
}

#[test]
fn explicit_incision_cuts_the_valley_but_never_below_downstream_or_the_floor() {
    let grid = Grid::new(17);
    let mut height = ramp(grid);
    let before = height.clone();
    let solved = network(&height, grid);
    let law = Explicit { k: 0.4, floor: 2.2 };
    incise(&mut height, &solved, grid, 1.0, law).expect("incises");
    let valley = grid.index(12, 8);
    assert!(height[valley] < before[valley], "the valley is cut");
    for index in 0..grid.area() {
        assert!(height[index] <= before[index]);
        assert!(height[index] >= law.floor.min(before[index]));
        if let Some(next) = crate::drainage::downstream(grid, &solved.flow, index) {
            assert!(height[index] >= before[next].min(height[index]) - 1e-12);
        }
    }
}

#[test]
fn an_implicit_step_lowers_each_sample_toward_its_receiver_and_never_past_it() {
    let grid = Grid::new(17);
    let mut height = ramp(grid);
    let before = height.clone();
    let solved = network(&height, grid);
    let law = Implicit {
        k_dt: 50.0,
        m: 0.5,
        cell_area: 1.0,
        spacing: 1.0,
        head: 0.0,
    };
    incise_implicit(
        &mut height,
        &solved,
        grid,
        law,
        (0..solved.order.len(), &|_, _| 1.0),
    )
    .expect("incises");
    for index in 0..grid.area() {
        assert!(height[index] <= before[index] + 1e-12);
        if let Some(next) = crate::drainage::downstream(grid, &solved.flow, index) {
            assert!(
                height[index] >= height[next] - 1e-12,
                "{index} fell past its receiver"
            );
        }
    }
    let valley = grid.index(14, 8);
    assert!(
        before[valley] - height[valley] > 1.0,
        "a large step cuts deep"
    );
}

#[test]
fn ground_that_does_not_erode_is_left_and_an_implicit_step_in_spans_matches_one_whole() {
    let grid = Grid::new(15);
    let solved = network(&ramp(grid), grid);
    let law = Implicit {
        k_dt: 0.8,
        m: 0.5,
        cell_area: 4.0,
        spacing: 2.0,
        head: 0.0,
    };
    let mut hard = ramp(grid);
    incise_implicit(
        &mut hard,
        &solved,
        grid,
        law,
        (0..solved.order.len(), &|_, _| 0.0),
    )
    .expect("incises");
    assert_eq!(hard, ramp(grid));
    let mut whole = ramp(grid);
    incise_implicit(
        &mut whole,
        &solved,
        grid,
        law,
        (0..solved.order.len(), &|_, _| 1.0),
    )
    .expect("incises");
    let mut pieces = ramp(grid);
    let mut start = 0;
    while start < solved.order.len() {
        let end = (start + 23).min(solved.order.len());
        incise_implicit(&mut pieces, &solved, grid, law, (start..end, &|_, _| 1.0))
            .expect("incises");
        start = end;
    }
    assert_eq!(pieces, whole);
}

#[test]
fn nothing_is_cut_above_where_a_channel_begins() {
    let grid = Grid::new(24);
    let mut height = ramp(grid);
    let network = Network::solve(&height, grid, |index| {
        let (x, y) = grid.position(index);
        grid.is_rim(x, y)
    })
    .expect("a network");
    let before = height.clone();
    let law = Implicit {
        k_dt: 0.8,
        m: 0.5,
        cell_area: 1.0,
        spacing: 1.0,
        head: 1.0e9,
    };
    incise_implicit(
        &mut height,
        &network,
        grid,
        law,
        (0..network.order.len(), &|_, _| 1.0),
    )
    .expect("incises");
    assert_eq!(height, before, "no stream drains enough to cut");
}
