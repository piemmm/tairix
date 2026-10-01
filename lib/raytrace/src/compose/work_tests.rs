use alloc::vec::Vec;

use super::*;

fn mark(index: usize) -> f32 {
    u16::try_from(index).map(f32::from).expect("small")
}

/// Grids each marked with its own index.
fn fields(count: usize) -> Vec<Heightfield> {
    (0..count)
        .map(|index| {
            let mut field = Heightfield::new(1, (0.0, 0.0), 1.0, false).expect("a grid");
            field.heights_mut().fill(mark(index));
            field
        })
        .collect()
}

#[test]
fn apart_hands_out_the_two_grids_named_whichever_comes_first() {
    let mut grids = fields(3);
    for (filled, read) in [(0, 2), (2, 0), (1, 2)] {
        let (field, ground) = apart(&mut grids, filled, read).expect("both there");
        assert_eq!(field.heights()[0].to_bits(), mark(filled).to_bits());
        assert_eq!(ground.heights()[0].to_bits(), mark(read).to_bits());
    }
}

#[test]
fn apart_refuses_one_grid_twice_or_one_that_is_not_there() {
    let mut grids = fields(2);
    assert!(apart(&mut grids, 1, 1).is_none());
    for (filled, read) in [(0, 2), (2, 0), (5, 9)] {
        assert!(apart(&mut grids, filled, read).is_none());
    }
}
