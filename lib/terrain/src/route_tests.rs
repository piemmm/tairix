use alloc::vec::Vec;

use super::*;

const LEAST: u32 = 10;

/// Straight steps cost `LEAST`, diagonal ones half again, wherever `open`
/// lets a route step at all.
fn flat(open: &dyn Fn(usize) -> bool) -> impl Fn(usize, usize, bool) -> Option<u32> + '_ {
    move |_, to, diagonal| open(to).then_some(if diagonal { LEAST * 3 / 2 } else { LEAST })
}

fn cost(path: &[usize], grid: Grid) -> u32 {
    path.windows(2)
        .map(|pair| {
            let ((ax, ay), (bx, by)) = (grid.position(pair[0]), grid.position(pair[1]));
            assert!(ax.abs_diff(bx) <= 1 && ay.abs_diff(by) <= 1, "a step jumps");
            if ax != bx && ay != by {
                LEAST * 3 / 2
            } else {
                LEAST
            }
        })
        .sum()
}

#[test]
fn over_even_ground_the_path_costs_exactly_the_octile_distance() {
    let grid = Grid::new(20);
    let mut router = Router::new(grid.area()).expect("a router");
    let (start, goal) = (grid.index(2, 3), grid.index(15, 9));
    let path = router
        .route(grid, (start, goal), (4, LEAST), &flat(&|_| true))
        .expect("routes")
        .expect("a path");
    assert_eq!(path.first(), Some(&start));
    assert_eq!(path.last(), Some(&goal));
    assert_eq!(cost(&path, grid), heuristic(grid, (start, goal), LEAST));
}

#[test]
fn a_route_goes_round_a_wall_and_none_crosses_a_closed_one() {
    let grid = Grid::new(16);
    let mut router = Router::new(grid.area()).expect("a router");
    let wall = |index: usize| {
        let (x, y) = grid.position(index);
        !(x == 8 && y < 13)
    };
    let (start, goal) = (grid.index(2, 2), grid.index(14, 2));
    let path = router
        .route(grid, (start, goal), (16, LEAST), &flat(&wall))
        .expect("routes")
        .expect("a path round the wall");
    assert!(path.iter().all(|&index| wall(index)));
    assert!(path.iter().any(|&index| grid.position(index).1 >= 13));
    let closed = |index: usize| grid.position(index).0 != 8;
    assert_eq!(
        router
            .route(grid, (start, goal), (16, LEAST), &flat(&closed))
            .expect("routes"),
        None
    );
}

#[test]
fn a_search_advanced_a_little_at_a_time_finds_the_path_one_search_does() {
    let grid = Grid::new(24);
    let price = |from: usize, to: usize, diagonal: bool| {
        let rise = (grid.position(to).1).abs_diff(grid.position(from).1) * 7
            + (grid.position(to).0 % 5) * 3;
        let straight = LEAST + rise;
        Some(if diagonal {
            straight + straight / 2
        } else {
            straight
        })
    };
    let ends = (grid.index(1, 20), grid.index(22, 2));
    let mut router = Router::new(grid.area()).expect("a router");
    let whole = router
        .route(grid, ends, (6, LEAST), &price)
        .expect("routes")
        .expect("a path");
    router.begin(grid, ends, 6, LEAST).expect("begins");
    let mut calls = 0;
    let found = loop {
        calls += 1;
        match router.advance(9, &price).expect("advances") {
            Routed::Pending => {}
            Routed::Found(path) => break path,
            Routed::Unreachable => panic!("no path"),
        }
    };
    assert!(calls > 3);
    assert_eq!(found, whole);
}

#[test]
fn a_guided_search_finds_as_cheap_a_path_and_settles_far_less() {
    let grid = Grid::new(24);
    // Every step costs four times the least, so octile distance bounds the
    // rest of the way loosely and four times it exactly.
    let price =
        |_: usize, _: usize, diagonal: bool| Some(if diagonal { 6 * LEAST } else { 4 * LEAST });
    let ends = (grid.index(1, 20), grid.index(22, 2));
    let exact = |index: usize| 4 * heuristic(grid, (index, ends.1), LEAST);
    let mut router = Router::new(grid.area()).expect("a router");
    let mut search = |guided: bool| {
        router.begin(grid, ends, 6, LEAST).expect("begins");
        let mut settles = 0;
        loop {
            settles += 1;
            let outcome = if guided {
                router.advance_guided(1, &price, &exact)
            } else {
                router.advance(1, &price)
            };
            match outcome.expect("advances") {
                Routed::Pending => {}
                Routed::Found(path) => break (path, settles),
                Routed::Unreachable => panic!("no path"),
            }
        }
    };
    let (loose, loosely) = search(false);
    let (tight, tightly) = search(true);
    assert_eq!(cost(&tight, grid), cost(&loose, grid));
    assert!(
        4 * tightly < loosely,
        "{tightly} settles guided, {loosely} not"
    );
}

#[test]
fn a_route_to_where_it_starts_is_that_one_sample() {
    let grid = Grid::new(5);
    let mut router = Router::new(grid.area()).expect("a router");
    let path: Option<Vec<usize>> = router
        .route(grid, (7, 7), (2, LEAST), &flat(&|_| true))
        .expect("routes");
    assert_eq!(path, Some(alloc::vec![7]));
    assert_eq!(
        router.begin(grid, (7, 99), 2, LEAST),
        Err(TerrainError::Shape)
    );
}
