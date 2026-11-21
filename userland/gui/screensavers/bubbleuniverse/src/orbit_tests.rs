//! Host tests of the map: its series, its reach, and a gang traced against
//! the map stepped straight from its definition.

extern crate std;

use alloc::vec::Vec;
use core::f64::consts::TAU;

use tairix_util::mathf;

use super::{sin_cos, Curves, Moment, COSINE, CURVES, GANGS, LANES, REACH, SINE, STEPS};

/// Every point each gang traces at `moment`, gang by gang, step by step.
fn traced(moment: &Moment) -> Vec<Vec<(f32, f32)>> {
    let curves = Curves::new();
    let mut all = Vec::new();
    for gang in curves.gangs() {
        let mut lanes = alloc::vec![Vec::new(); LANES];
        gang.trace(moment, |u, v| {
            for (lane, points) in lanes.iter_mut().enumerate() {
                points.push((u[lane], v[lane]));
            }
        });
        all.extend(lanes);
    }
    all
}

#[test]
fn the_series_are_the_reciprocal_factorials() {
    let mut factorial = 1.0f64;
    for (k, &term) in SINE.iter().enumerate() {
        let sign = if k % 2 == 0 { 1.0 } else { -1.0 };
        assert!(
            (f64::from(term) - sign / factorial).abs() <= 1e-7 / factorial,
            "sine term {k}"
        );
        factorial *= mathf::real(2 * k + 2) * mathf::real(2 * k + 3);
    }
    factorial = 1.0;
    for (k, &term) in COSINE.iter().enumerate() {
        let sign = if k % 2 == 0 { 1.0 } else { -1.0 };
        assert!(
            (f64::from(term) - sign / factorial).abs() <= 1e-7 / factorial,
            "cosine term {k}"
        );
        factorial *= mathf::real(2 * k + 1) * mathf::real(2 * k + 2);
    }
}

/// The short series hold the sine and cosine to single precision's rounding
/// across the whole reach a step can take, and a little past it.
#[test]
fn the_series_track_the_sine_and_cosine_across_the_reach() {
    let mut worst = 0.0f64;
    let samples = 41_000;
    for chunk in 0..samples / LANES {
        let x: [f32; LANES] = core::array::from_fn(|lane| {
            let at = mathf::real(chunk * LANES + lane) / mathf::real(samples);
            mathf::single(-2.05 + 4.1 * at)
        });
        let (sin, cos) = sin_cos(&x);
        for lane in 0..LANES {
            let exact = f64::from(x[lane]);
            worst = worst
                .max((f64::from(sin[lane]) - mathf::sin(exact)).abs())
                .max((f64::from(cos[lane]) - mathf::cos(exact)).abs());
        }
    }
    assert!(worst < 4e-7, "{worst}");
}

/// A point is two unit vectors summed, so none leaves the disc of radius two,
/// whatever the moment.
#[test]
fn every_point_stays_within_the_reach() {
    for (time, drift) in [(0.0, 0.0), (1.3, 0.4), (5.3, 2.2), (40_000.0, -7.0)] {
        for curve in traced(&Moment::at(time, drift)) {
            for (u, v) in curve {
                let distance = mathf::hypot(f64::from(u), f64::from(v));
                assert!(distance <= REACH * (1.0 + 1e-5), "{distance} at {time}");
            }
        }
    }
}

/// The map stepped from its definition, in double precision with the exact
/// sine and cosine of each whole angle: curve `curve`'s first `steps` points.
fn stepped(curve: usize, steps: usize, (time, drift): (f64, f64)) -> Vec<(f64, f64)> {
    let own = mathf::real(curve);
    let turned = TAU / 235.0 * own;
    let (mut u, mut v) = (0.0, 0.0);
    (0..steps)
        .map(|_| {
            let (first, second) = (own + drift + v, turned + time + u);
            (u, v) = (
                mathf::sin(first) + mathf::sin(second),
                mathf::cos(first) + mathf::cos(second),
            );
            (u, v)
        })
        .collect()
}

/// Each lane of each gang is its own curve of the map: its first points,
/// before the map's own sensitivity can amplify single precision's rounding,
/// are the map's to within a few millionths of the bubble.
#[test]
fn a_gang_traces_each_lane_s_curve_of_the_map() {
    let at = (5.3, 0.8);
    let all = traced(&Moment::at(at.0, at.1));
    assert_eq!(all.len(), CURVES);
    for (curve, points) in all.iter().enumerate() {
        assert_eq!(points.len(), STEPS);
        for (step, (&(u, v), (eu, ev))) in points.iter().zip(stepped(curve, 4, at)).enumerate() {
            let off = mathf::hypot(f64::from(u) - eu, f64::from(v) - ev);
            assert!(off < 2e-5, "curve {curve} step {step}: {off}");
        }
    }
    assert_eq!(GANGS * LANES, CURVES);
}

#[test]
fn the_same_moment_traces_the_same_points_and_another_moves_them() {
    let moment = Moment::at(2.0, 1.0);
    assert_eq!(traced(&moment), traced(&moment));
    assert_ne!(
        traced(&moment),
        traced(&Moment::at(2.05, 1.0)),
        "time moves it"
    );
    assert_ne!(
        traced(&moment),
        traced(&Moment::at(2.0, 1.05)),
        "drift moves it"
    );
}
