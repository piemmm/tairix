//! Host tests of the chained grid: an item is found from every cell its
//! square was linked into and from none other, a square past the grid says
//! so, and a grid kept within its most cells grows its cells instead.

use alloc::vec::Vec;

use super::*;

#[test]
fn an_item_is_found_from_every_cell_it_was_linked_into_and_no_other() {
    let mut chains = Chains::new(((0.0, 0.0), 10.0), 1.0, 64).expect("a grid");
    let (cells, past) = chains.span((2.5, -3.5), 0.75);
    assert!(!past);
    assert_eq!(cells, ((11, 13), (5, 7)));
    chains.link(7, cells).expect("linked");
    let (own, _) = chains.span((0.2, 0.2), 0.0);
    chains.link(9, own).expect("linked");
    let found = |at: (f64, f64)| -> Vec<u32> {
        let (cells, _) = chains.span(at, 0.0);
        chains.within(cells).collect()
    };
    assert_eq!(found((1.6, -4.4)), [7]);
    assert_eq!(found((3.9, -2.1)), [7]);
    assert!(found((4.1, -3.5)).is_empty());
    assert_eq!(found((0.9, 0.9)), [9]);
    let (both, _) = chains.span((1.0, -1.5), 3.0);
    let mut near: Vec<u32> = chains.within(both).collect();
    near.sort_unstable();
    near.dedup();
    assert_eq!(near, [7, 9]);
}

#[test]
fn a_square_past_the_grid_is_clamped_to_it_and_says_so() {
    let chains = Chains::new(((5.0, 5.0), 5.0), 1.0, 64).expect("a grid");
    let (cells, past) = chains.span((-2.0, 4.0), 0.5);
    assert!(past);
    assert_eq!(cells, ((0, 0), (3, 4)));
    let (inside, past) = chains.span((5.0, 5.0), 4.9);
    assert!(!past);
    assert_eq!(inside, ((0, 9), (0, 9)));
}

#[test]
fn a_grid_kept_within_its_most_cells_grows_its_cells_instead() {
    let chains = Chains::new(((0.0, 0.0), 100.0), 0.5, 50).expect("a grid");
    assert!((chains.cell() - 4.0).abs() < 1e-12);
    let fine = Chains::new(((0.0, 0.0), 100.0), 8.0, 50).expect("a grid");
    assert!((fine.cell() - 8.0).abs() < 1e-12);
}
