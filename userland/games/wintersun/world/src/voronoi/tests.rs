use alloc::vec::Vec;

use super::{nearest, site, square_distance, wrap, SITE_JITTER};

/// Every site within `reach` cells of `point`, nearest first, the ties in the
/// order the search meets them.
fn brute_force(
    point: (f64, f64),
    reach: i32,
    site_of: impl Fn(i32, i32) -> (f64, f64),
) -> Vec<(f64, (i32, i32))> {
    let (cx, cy) = (
        tairix_util::mathf::round_i32(tairix_util::mathf::floor(point.0)),
        tairix_util::mathf::round_i32(tairix_util::mathf::floor(point.1)),
    );
    let mut all: Vec<(f64, i32, (i32, i32))> = Vec::new();
    for dy in -reach..=reach {
        for dx in -reach..=reach {
            let cell = (cx + dx, cy + dy);
            let ring = dx.abs().max(dy.abs());
            all.push((square_distance(site_of(cell.0, cell.1), point), ring, cell));
        }
    }
    all.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
    all.into_iter().map(|(d2, _, cell)| (d2, cell)).collect()
}

#[test]
fn the_search_finds_a_nearest_site_the_nine_cells_around_it_miss() {
    // Every site of the nine cells around the query pushed as far from it as
    // the jitter allows, and one site two cells off pulled toward it: the
    // nearest is outside the block.
    let point = (0.99, 0.99);
    let site_of = |cx: i32, cy: i32| {
        let in_block = cx.abs() <= 1 && cy.abs() <= 1;
        let place = |c: i32, q: f64| {
            let centre = f64::from(c) + 0.5;
            let away = if centre < q {
                -SITE_JITTER
            } else {
                SITE_JITTER
            };
            centre + if in_block { away } else { -away }
        };
        (place(cx, point.0), place(cy, point.1))
    };
    let [(d2, cell)] = nearest::<1>(point, site_of);
    let truth = brute_force(point, 3, site_of);
    assert_eq!(cell, truth[0].1);
    assert!(
        cell.0.abs() > 1 || cell.1.abs() > 1,
        "{cell:?} is inside the block"
    );
    assert!((d2 - truth[0].0).abs() < 1.0e-12);
}

#[test]
fn the_nearest_two_are_the_nearest_two() {
    // A pseudo-random jittered grid, queried everywhere across a few cells.
    let site_of = |cx: i32, cy: i32| {
        let hash = |salt: i64| {
            let mixed = (i64::from(cx) * 73_856_093) ^ (i64::from(cy) * 19_349_663) ^ salt;
            let bits = u64::from_le_bytes(mixed.to_le_bytes()).wrapping_mul(0x9E37_79B9_7F4A_7C15);
            let top = u32::try_from(bits >> 40).expect("24 bits");
            f64::from(top) / f64::from(1_u32 << 24) * 2.0 - 1.0
        };
        site(cx, cy, (hash(1), hash(2)))
    };
    for step in 0..400 {
        let t = f64::from(step) / 37.0;
        let point = (t - 3.0, 2.0 - t * 0.61);
        let found = nearest::<2>(point, site_of);
        let truth = brute_force(point, 4, site_of);
        assert_eq!(found[0].1, truth[0].1, "nearest at {point:?}");
        assert_eq!(found[1].1, truth[1].1, "second at {point:?}");
        assert!(found[0].0 <= found[1].0);
    }
}

#[test]
fn a_point_off_the_plane_ends_the_search() {
    let site_of = |cx: i32, cy: i32| site(cx, cy, (0.0, 0.0));
    for point in [
        (f64::NAN, 0.5),
        (0.5, f64::NAN),
        (f64::INFINITY, 0.5),
        (0.5, f64::NEG_INFINITY),
    ] {
        let found = nearest::<2>(point, site_of);
        assert!(
            found
                .iter()
                .all(|&(d2, _)| d2.to_bits() == f64::MAX.to_bits()),
            "{point:?}"
        );
    }
}

#[test]
fn a_site_is_its_cells_centre_moved_by_the_scaled_jitter() {
    assert_eq!(site(3, -2, (0.0, 0.0)), (3.5, -1.5));
    assert_eq!(
        site(0, 0, (1.0, -1.0)),
        (0.5 + SITE_JITTER, 0.5 - SITE_JITTER)
    );
    // Bit for bit the construction the plates and provinces drew before.
    let (jx, jy) = (0.123_456_789, -0.987_654_321);
    assert_eq!(
        site(7, 9, (jx, jy)),
        (
            f64::from(7) + 0.5 + jx * SITE_JITTER,
            f64::from(9) + 0.5 + jy * SITE_JITTER
        )
    );
}

#[test]
fn a_grid_index_wraps_by_flooring() {
    assert_eq!(wrap(-1, 4), 3);
    assert_eq!(wrap(-4, 4), 0);
    assert_eq!(wrap(4, 4), 0);
    assert_eq!(wrap(9, 4), 1);
    assert_eq!(wrap(7, 0), 0, "an empty grid is one cell");
}
