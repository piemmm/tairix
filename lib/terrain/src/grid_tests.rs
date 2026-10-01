use super::*;

#[test]
fn an_index_and_its_position_name_the_same_sample() {
    let grid = Grid::new(7);
    assert_eq!(grid.area(), 49);
    for y in 0..7 {
        for x in 0..7 {
            assert_eq!(grid.position(grid.index(x, y)), (x, y));
        }
    }
}

#[test]
fn a_neighbour_off_the_grid_is_none_and_the_rim_is_its_edge() {
    let grid = Grid::new(4);
    assert_eq!(grid.neighbour(0, 0, -1, 0), None);
    assert_eq!(grid.neighbour(3, 3, 1, 1), None);
    assert_eq!(grid.neighbour(1, 1, 1, 1), Some(grid.index(2, 2)));
    assert!(grid.is_rim(0, 2) && grid.is_rim(3, 1) && grid.is_rim(2, 3));
    assert!(!grid.is_rim(1, 2));
}

#[test]
fn every_direction_offsets_by_one_sample_and_runs_its_length() {
    for (dir, dx, dy, distance) in NEIGHBOURS {
        assert_eq!(dir.offset(), Some((dx, dy)));
        assert!(dx.abs() <= 1 && dy.abs() <= 1);
        let expected = if dx == 0 || dy == 0 {
            1.0
        } else {
            core::f64::consts::SQRT_2
        };
        assert!((distance - expected).abs() < f64::EPSILON);
        assert!((dir.length() - expected).abs() < f64::EPSILON);
    }
    assert_eq!(FlowDir::Sink.offset(), None);
    assert!(FlowDir::Sink.length().abs() < f64::EPSILON);
}
