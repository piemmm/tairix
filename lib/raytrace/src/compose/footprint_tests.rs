use super::*;

use crate::sample::{mix32, unit};

/// A spread of circles across a square `reach` either way of the middle,
/// some reaching past it.
fn scattered(count: u32, reach: f64) -> Vec<(f64, f64, f64)> {
    (0..count)
        .map(|i| {
            let draw = |salt: u32| unit(mix32(i.wrapping_mul(0x9e37_79b9) ^ salt));
            (
                (draw(1) * 2.2 - 1.1) * reach,
                (draw(2) * 2.2 - 1.1) * reach,
                0.2 + 12.0 * draw(3) * draw(3),
            )
        })
        .collect()
}

/// Whether `(at, radius)` keeps clear of `circles`, asked of every one.
fn clear_of(circles: &[(f64, f64, f64)], at: (f64, f64), radius: f64) -> bool {
    circles.iter().all(|&(x, z, taken)| {
        let least = radius + taken + GAP;
        (at.0 - x).powi(2) + (at.1 - z).powi(2) > least * least
    })
}

#[test]
fn the_index_answers_as_asking_every_circle_would() {
    let reach = 600.0;
    let circles = scattered(3000, reach);
    let mut indexed = Footprints::default();
    // Half claimed before the grid is laid, half after, so both ways in are
    // proved.
    for &(x, z, radius) in &circles[..1500] {
        indexed.claim((x, z), radius).expect("claimed");
    }
    indexed.index((0.0, 0.0), reach).expect("indexed");
    for &(x, z, radius) in &circles[1500..] {
        indexed.claim((x, z), radius).expect("claimed");
    }
    let (mut clear, mut taken) = (0, 0);
    for probe in 0..20_000u32 {
        let draw = |salt: u32| unit(mix32(probe.wrapping_mul(0x85eb_ca6b) ^ salt));
        let at = ((draw(5) * 2.4 - 1.2) * reach, (draw(6) * 2.4 - 1.2) * reach);
        let radius = 0.1 + 6.0 * draw(7);
        let expected = clear_of(&circles, at, radius);
        assert_eq!(indexed.clear(at, radius), expected, "{at:?} {radius}");
        if expected {
            clear += 1;
        } else {
            taken += 1;
        }
    }
    assert!(
        clear > 1000 && taken > 1000,
        "probes land both ways: {clear} clear, {taken} taken"
    );
}

#[test]
fn circles_just_apart_are_clear_and_just_touching_are_not() {
    let mut footprints = Footprints::default();
    footprints.index((0.0, 0.0), 100.0).expect("indexed");
    footprints.claim((10.0, 10.0), 2.0).expect("claimed");
    let edge = 10.0 + 2.0 + 1.0 + GAP;
    assert!(footprints.clear((edge + 1e-6, 10.0), 1.0));
    assert!(!footprints.clear((edge - 1e-6, 10.0), 1.0));
    // Across a cell's wall, at x = -4, as readily as within one.
    footprints.claim((-4.5, 0.0), 0.3).expect("claimed");
    assert!(!footprints.clear((-3.9, 0.0), 0.3));
    assert!(footprints.clear((-3.8, 0.0), 0.3));
}

#[test]
fn a_circle_past_the_grid_is_still_kept_clear_of() {
    let mut footprints = Footprints::default();
    footprints.index((0.0, 0.0), 50.0).expect("indexed");
    footprints.claim((70.0, 0.0), 30.0).expect("claimed");
    assert!(
        !footprints.clear((45.0, 0.0), 1.0),
        "within the grid, under a circle reaching in from past it"
    );
    assert!(!footprints.clear((90.0, 0.0), 1.0), "past the grid");
    assert!(footprints.clear((-40.0, 0.0), 1.0));
}

/// A circle given a negative radius takes no room of its own, and the index
/// answers for it exactly as asking every circle would.
#[test]
fn a_negative_radius_is_answered_as_the_whole_list_would() {
    let reach = 200.0;
    let circles = scattered(400, reach);
    let mut indexed = Footprints::default();
    for &(x, z, radius) in &circles {
        indexed.claim((x, z), radius).expect("claimed");
    }
    indexed.index((0.0, 0.0), reach).expect("indexed");
    for probe in 0..2_000u32 {
        let draw = |salt: u32| unit(mix32(probe.wrapping_mul(0x85eb_ca6b) ^ salt));
        let at = ((draw(5) * 2.4 - 1.2) * reach, (draw(6) * 2.4 - 1.2) * reach);
        let radius = -4.0 * draw(7);
        assert_eq!(
            indexed.clear(at, radius),
            clear_of(&circles, at, 0.0),
            "{at:?} {radius}"
        );
    }
    indexed
        .claim((10.0, 10.0), -50.0)
        .expect("a negative radius chains into the cells about its point");
}
