//! The map every point of the picture lies on an orbit of.
//!
//! Curve `i` steps
//!
//! ```text
//! u' = sin(i + φ + v) + sin(r·i + t + u)
//! v' = cos(i + φ + v) + cos(r·i + t + u)
//! ```
//!
//! from the origin, `r` a 235th of a turn, `t` the picture's time and `φ` its
//! slow drift: the Bubble Universe map, each step one point of the picture.
//! Every curve sets off afresh, so each is its own and none waits on another.
//! A point is the sum of two unit vectors, so none leaves the disc of radius
//! [`REACH`].
//!
//! By angle addition a step needs the sine and cosine of `u` and `v` alone,
//! turned by a curve's two phases, which are taken once a moment. Both lie
//! within [`REACH`] of nought, so a short series gives them with no range
//! reduction, and [`LANES`] curves are stepped side by side, one to a lane of
//! an array the compiler steps in vector registers where the target has
//! them.

use core::array::from_fn;
use core::f64::consts::TAU;

use tairix_util::mathf::{self, single, Turn};

/// The curves the picture is drawn from.
pub(crate) const CURVES: usize = 200;

/// The points traced along each curve.
pub(crate) const STEPS: usize = 200;

/// Curves stepped side by side, one to a lane.
pub(crate) const LANES: usize = 8;

/// The gangs of [`LANES`] curves the curves make.
pub(crate) const GANGS: usize = CURVES / LANES;

const _: () = assert!(CURVES.is_multiple_of(LANES), "every gang is whole");

/// The points one gang traces at one moment.
pub(crate) const GANG_POINTS: usize = STEPS * LANES;

/// How far each curve's second phase turns from the one before.
const CURVE_TURN: f64 = TAU / 235.0;

/// The farthest any point lies from the centre, in the map's units.
pub(crate) const REACH: f64 = 2.0;

/// A value for each curve of a gang.
pub(crate) type Lanes = [f32; LANES];

/// Sine by powers of `x²` over `x`, through `x¹³`: past it the series adds
/// under 3e-8 for `|x| <= REACH`, below single precision's own rounding.
const SINE: [f32; 7] = taylor(1.0);

/// Cosine by powers of `x²`, through `x¹⁴`.
const COSINE: [f32; 8] = taylor(0.0);

/// A moment of the picture: its time `t` and its drift `φ`.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) struct Moment {
    time: Turn,
    drift: Turn,
}

impl Moment {
    /// The moment at time `time` and drift `drift`, both in radians.
    pub(crate) fn at(time: f64, drift: f64) -> Self {
        Self {
            time: Turn::of(time),
            drift: Turn::of(drift),
        }
    }
}

/// [`LANES`] neighbouring curves: each one's own phase `i` and its turned
/// phase `r·i`.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Gang {
    own: [Turn; LANES],
    turned: [Turn; LANES],
}

impl Gang {
    /// Trace each lane's curve at `moment` from the origin, handing `point`
    /// every step's position, `u` and `v` a lane each.
    pub(crate) fn trace(&self, moment: &Moment, mut point: impl FnMut(&Lanes, &Lanes)) {
        let (sin_a, cos_a) = lanes(&self.own, moment.drift);
        let (sin_b, cos_b) = lanes(&self.turned, moment.time);
        let (mut u, mut v): (Lanes, Lanes) = ([0.0; LANES], [0.0; LANES]);
        for _ in 0..STEPS {
            let (sin_u, cos_u) = sin_cos(&u);
            let (sin_v, cos_v) = sin_cos(&v);
            u = from_fn(|l| {
                (sin_a[l] * cos_v[l] + cos_a[l] * sin_v[l])
                    + (sin_b[l] * cos_u[l] + cos_b[l] * sin_u[l])
            });
            v = from_fn(|l| {
                (cos_a[l] * cos_v[l] - sin_a[l] * sin_v[l])
                    + (cos_b[l] * cos_u[l] - sin_b[l] * sin_u[l])
            });
            point(&u, &v);
        }
    }
}

/// Every curve, gang by gang.
pub(crate) struct Curves {
    gangs: [Gang; GANGS],
}

impl Curves {
    /// The curves' fixed phases.
    pub(crate) fn new() -> Self {
        Self {
            gangs: from_fn(|gang| {
                let curve = |lane: usize| mathf::real(gang * LANES + lane);
                Gang {
                    own: from_fn(|lane| Turn::of(curve(lane))),
                    turned: from_fn(|lane| Turn::of(CURVE_TURN * curve(lane))),
                }
            }),
        }
    }

    /// The gangs, first curves first.
    pub(crate) const fn gangs(&self) -> &[Gang; GANGS] {
        &self.gangs
    }
}

/// `phases` each turned on by `by`, as their sines and cosines a lane each.
fn lanes(phases: &[Turn; LANES], by: Turn) -> (Lanes, Lanes) {
    let turned = phases.map(|phase| phase.then(by));
    (
        turned.map(|t| single(t.sin())),
        turned.map(|t| single(t.cos())),
    )
}

/// The sine and cosine of each lane, for values no farther than a little past
/// [`REACH`] from nought.
pub(crate) fn sin_cos(x: &Lanes) -> (Lanes, Lanes) {
    let square: Lanes = from_fn(|l| x[l] * x[l]);
    (
        from_fn(|l| x[l] * series(square[l], &SINE)),
        from_fn(|l| series(square[l], &COSINE)),
    )
}

/// `terms` summed by powers of `square`, by Horner's rule.
fn series<const N: usize>(square: f32, terms: &[f32; N]) -> f32 {
    terms
        .iter()
        .rev()
        .fold(0.0, |sum, &term| sum * square + term)
}

/// `(−1)ᵏ / (2k + first)!` for each `k`: the Taylor series of sine over `x`
/// when `first` is one, of cosine when it is nought, by powers of `x²`.
const fn taylor<const N: usize>(first: f64) -> [f32; N] {
    let mut terms = [0.0; N];
    let mut factorial = 1.0;
    let mut power = first;
    let mut k = 0;
    while k < N {
        let sign = if k % 2 == 0 { 1.0 } else { -1.0 };
        terms[k] = single(sign / factorial);
        factorial *= (power + 1.0) * (power + 2.0);
        power += 2.0;
        k += 1;
    }
    terms
}

#[cfg(test)]
#[path = "orbit_tests.rs"]
mod tests;
