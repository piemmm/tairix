//! Unit tests for the polynomial root solvers.

use tairix_fuzzseed::Prng;
use tairix_util::mathf::fmax;

use super::{cubic, cubic_in_unit, quadratic, quadratic_in_unit};

#[track_caller]
fn assert_roots(found: &[f64], want: &[f64]) {
    assert_eq!(found.len(), want.len(), "found {found:?}, want {want:?}");
    for (got, expected) in found.iter().zip(want) {
        assert!(
            (got - expected).abs() <= 1e-9 * fmax(expected.abs(), 1.0),
            "found {found:?}, want {want:?}"
        );
    }
}

#[test]
fn a_quadratic_with_two_roots_finds_both_in_order() {
    // (t - 1)(t + 3) = t² + 2t - 3
    assert_roots(&quadratic(1.0, 2.0, -3.0), &[-3.0, 1.0]);
}

#[test]
fn a_quadratic_without_real_roots_finds_none() {
    assert!(quadratic(1.0, 0.0, 1.0).is_empty());
}

#[test]
fn a_negligible_leading_coefficient_falls_back_to_the_line() {
    assert_roots(&quadratic(1e-20, 2.0, -1.0), &[0.5]);
    assert!(quadratic(0.0, 0.0, 5.0).is_empty());
}

#[test]
fn the_quadratic_keeps_a_tiny_root_beside_a_huge_one() {
    // The textbook form loses the small root to cancellation here.
    let roots = quadratic(1.0, -1e9, 1.0);
    assert_roots(&roots, &[1e-9, 1e9]);
}

#[test]
fn a_cubic_with_three_roots_finds_all_three() {
    // (t - 1)(t - 2)(t - 3) = t³ - 6t² + 11t - 6
    assert_roots(&cubic(1.0, -6.0, 11.0, -6.0), &[1.0, 2.0, 3.0]);
}

#[test]
fn a_cubic_with_one_real_root_finds_it() {
    // (t - 2)(t² + 1) = t³ - 2t² + t - 2
    assert_roots(&cubic(1.0, -2.0, 1.0, -2.0), &[2.0]);
}

#[test]
fn a_cubic_with_a_double_root_reports_it_once() {
    // (t - 1)²(t + 2) = t³ - 3t + 2
    let roots = cubic(1.0, 0.0, -3.0, 2.0);
    assert_roots(&roots, &[-2.0, 1.0]);
}

#[test]
fn a_degenerate_cubic_is_solved_as_a_quadratic() {
    assert_roots(&cubic(0.0, 1.0, 2.0, -3.0), &[-3.0, 1.0]);
    assert!(cubic(0.0, 0.0, 0.0, 0.0).is_empty());
    assert!(cubic(f64::NAN, 1.0, 1.0, 1.0).is_empty());
}

#[test]
fn unit_roots_hold_a_rounded_endpoint_and_drop_the_rest() {
    // Roots at 0, 0.5 and 2: the last is outside.
    let roots = cubic_in_unit(1.0, -2.5, 1.0, 0.0);
    assert_roots(&roots, &[0.0, 0.5]);
    assert_roots(&quadratic_in_unit(1.0, -1.0 - 1e-12, 1e-12), &[1e-12, 1.0]);
}

#[test]
fn every_root_of_random_cubics_is_a_root() {
    let mut rng = Prng::new(0x00b0_0c1e);
    for _ in 0..2000 {
        let pick = |rng: &mut Prng| f64::from(rng.next_u32()) / f64::from(u32::MAX) * 20.0 - 10.0;
        let (r0, r1, r2) = (pick(&mut rng), pick(&mut rng), pick(&mut rng));
        // (t - r0)(t - r1)(t - r2)
        let b = -(r0 + r1 + r2);
        let c = r0 * r1 + r0 * r2 + r1 * r2;
        let d = -r0 * r1 * r2;
        let roots = cubic(1.0, b, c, d);
        assert!(!roots.is_empty());
        for root in roots {
            let residual = ((root + b) * root + c) * root + d;
            assert!(residual.abs() <= 1e-6, "residual {residual} at {root}");
        }
    }
}
