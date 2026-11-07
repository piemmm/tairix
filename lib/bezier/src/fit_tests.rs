//! Unit tests for curve fitting.

use alloc::vec::Vec;

use core::f64::consts::TAU;

use tairix_fuzzseed::Prng;

use super::{fit, FitError, Joint, MAX_FIT_POINTS};
use crate::cubic::CubicBez;
use crate::point::Point;

fn fitted(points: &[Point], tolerance: f64, corner: f64) -> Vec<(CubicBez, Joint)> {
    let mut out = Vec::new();
    fit(points, tolerance, corner, |curve, joint| {
        out.push((curve, joint));
    })
    .expect("fits");
    out
}

/// The greatest distance from any of `points` to the nearest of `curves`.
fn worst_distance(points: &[Point], curves: &[(CubicBez, Joint)]) -> f64 {
    points
        .iter()
        .map(|&point| {
            curves
                .iter()
                .map(|(curve, _)| curve.nearest(point).1)
                .fold(f64::INFINITY, |a, b| if b < a { b } else { a })
        })
        .fold(0.0, |a: f64, b| if b > a { b } else { a })
}

#[test]
fn points_from_one_cubic_fit_as_one_within_tolerance() {
    let source = CubicBez::new(
        Point::new(0.0, 0.0),
        Point::new(30.0, 80.0),
        Point::new(90.0, 80.0),
        Point::new(120.0, 0.0),
    );
    let points: Vec<Point> = (0..=100)
        .map(|i| source.eval(f64::from(i) / 100.0))
        .collect();
    let curves = fitted(&points, 0.25, 70.0);
    assert!(curves.len() <= 2, "{} cubics", curves.len());
    assert!(tairix_util::mathf::sqrt(worst_distance(&points, &curves)) <= 0.25 + 1e-6);
    assert_eq!(curves.last().map(|c| c.1), Some(Joint::End));
}

#[test]
fn a_circle_fits_smoothly_within_tolerance() {
    let points: Vec<Point> = (0..=360)
        .map(|i| Point::from_angle(f64::from(i) / 360.0 * TAU) * 50.0)
        .collect();
    let curves = fitted(&points, 0.1, 70.0);
    assert!(
        curves.len() >= 4 && curves.len() <= 16,
        "{} cubics",
        curves.len()
    );
    assert!(tairix_util::mathf::sqrt(worst_distance(&points, &curves)) <= 0.1 + 1e-6);
    for window in curves.windows(2) {
        assert_eq!(window[0].1, Joint::Smooth);
        assert!(window[0].0.p3 == window[1].0.p0, "the chain is continuous");
        let (_, arriving) = window[0].0.tangents();
        let (leaving, _) = window[1].0.tangents();
        let (a, b) = (
            arriving.normalised().unwrap(),
            leaving.normalised().unwrap(),
        );
        assert!(a.dot(b) > 0.999_999, "smooth joints share a tangent");
    }
}

#[test]
fn a_square_corner_stays_a_corner() {
    let mut points = Vec::new();
    for i in 0..=20 {
        points.push(Point::new(f64::from(i), 0.0));
    }
    for i in 1..=20 {
        points.push(Point::new(20.0, f64::from(i)));
    }
    let curves = fitted(&points, 0.05, 60.0);
    assert_eq!(curves.len(), 2, "one cubic each side of the corner");
    assert_eq!(curves[0].1, Joint::Corner);
    assert_eq!(curves[0].0.p3, Point::new(20.0, 0.0));
    assert!(curves.iter().all(|(curve, _)| curve.is_line(1e-6)));
}

#[test]
fn degenerate_input_fits_nothing_and_too_much_is_refused() {
    assert!(fitted(&[], 1.0, 60.0).is_empty());
    assert!(fitted(&[Point::new(3.0, 3.0); 50], 1.0, 60.0).is_empty());
    let huge = alloc::vec![Point::ZERO; MAX_FIT_POINTS + 1];
    assert_eq!(fit(&huge, 1.0, 60.0, |_, _| {}), Err(FitError::TooComplex));
}

#[test]
fn noisy_strokes_fit_within_tolerance_with_few_cubics() {
    let mut rng = Prng::new(0x0f17_0f17);
    for _ in 0..50 {
        let mut points = Vec::new();
        let mut at = Point::ZERO;
        let mut heading = 0.0_f64;
        for _ in 0..400 {
            heading += (f64::from(rng.next_u32() % 1000) / 1000.0 - 0.5) * 0.2;
            at += Point::from_angle(heading) * 1.5;
            let jitter = Point::new(
                (f64::from(rng.next_u32() % 100) / 100.0 - 0.5) * 0.2,
                (f64::from(rng.next_u32() % 100) / 100.0 - 0.5) * 0.2,
            );
            points.push(at + jitter);
        }
        let curves = fitted(&points, 1.0, 75.0);
        assert!(
            !curves.is_empty() && curves.len() < 100,
            "{} cubics",
            curves.len()
        );
        // Thinning may drop points within half the tolerance of a kept one.
        assert!(tairix_util::mathf::sqrt(worst_distance(&points, &curves)) <= 1.5 + 1e-6);
    }
}
