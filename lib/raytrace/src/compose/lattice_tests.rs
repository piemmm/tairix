use super::*;

/// A lattice about the eye spans its reach whatever the eye, and its cells
/// lie on the land's own grid: moved a little, the eye keeps the same cells.
#[test]
fn a_lattice_about_the_eye_spans_its_reach_on_the_lands_own_grid() {
    for eye in [(0.0, 0.0), (3.7, -12.2), (-0.01, 1e3)] {
        let lattice = Lattice::about(eye, 10.0, 2.0).expect("a lattice");
        let far = lattice.corner_of((lattice.side, lattice.side));
        assert!(lattice.corner.0 <= eye.0 - 10.0 && lattice.corner.1 <= eye.1 - 10.0);
        assert!(far.0 >= eye.0 + 10.0 && far.1 >= eye.1 + 10.0, "{eye:?}");
        for corner in [lattice.corner.0, lattice.corner.1] {
            assert!((corner / 2.0 - mathf::round(corner / 2.0)).abs() < 1e-9);
        }
    }
}

/// A cell in the square a finer lattice reads has no middle of its own here,
/// and a cell's place is the land's own grid cell its middle lies in.
#[test]
fn a_cell_under_a_finer_lattice_is_left_to_it() {
    let lattice = Lattice::new((0.0, 0.0), 4, 1.0).without(((1.0, 1.0), (3.0, 3.0)));
    assert_eq!(lattice.middle((0, 0)), Some((0.5, 0.5)));
    assert_eq!(lattice.middle((1, 2)), None);
    assert_eq!(lattice.middle((3, 1)), Some((3.5, 1.5)));
    assert_eq!(lattice.place(3.5), 3);
    assert_eq!(lattice.place(-0.5), u32::MAX);
}

/// Reading rows of a lattice visits each of their cells once, by its own
/// column and row, whichever runner shares them out.
#[test]
fn reading_rows_finds_each_cell_once_by_its_place() {
    let lattice = Lattice::new((0.0, 0.0), 5, 1.0);
    let runners: [&dyn JobRunner; 2] =
        [&tairix_parallel::SERIAL, &tairix_parallel::Reversed::new(4)];
    for runner in runners {
        let cells = lattice
            .read(1..4, runner, &|(column, row)| Some(10 * row + column))
            .expect("read");
        assert_eq!(cells.len(), 15);
        for (index, cell) in cells.iter().enumerate() {
            assert_eq!(*cell, Some(10 * (1 + index / 5) + index % 5));
        }
    }
}

/// A lattice no cell or reach could lay out is refused rather than laid
/// with a side that overflowed.
#[test]
fn a_lattice_of_no_cell_or_endless_reach_is_refused() {
    assert!(Lattice::about((0.0, 0.0), 10.0, 0.0).is_none());
    assert!(Lattice::about((0.0, 0.0), 10.0, -1.0).is_none());
    assert!(Lattice::about((0.0, 0.0), f64::INFINITY, 1.0).is_none());
    assert!(Lattice::about((0.0, 0.0), 10.0, f64::NAN).is_none());
}
