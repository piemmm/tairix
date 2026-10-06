//! Light within a plane-parallel slab of scattering medium — a cloud's
//! column, lit by the sun from above, the sky above and the ground below —
//! by the δ-Eddington two-stream approximation (Joseph, Wiscombe and
//! Weinman, "The Delta-Eddington Approximation for Radiative Flux Transfer",
//! 1976): the radiance written as its mean and its first moment in the
//! vertical, the phase function's forward peak folded into the unscattered
//! beam (`f = g²`), the two coupled moments solved exactly for the sun's beam
//! falling into the slab and the diffuse light entering it from above and
//! below.
//!
//! The medium is taken as almost wholly scattering, so the solution never
//! meets the conservative limit's degeneracy, and the homogeneous solution is
//! written decaying from either face, so no exponential grows however thick
//! the slab.

use core::f64::consts::PI;

use tairix_util::mathf;

use crate::vector::Vec3;

/// A medium's scattering: the share of what it meets it scatters, and the
/// mean cosine it scatters through.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) struct Medium {
    pub(crate) albedo: f64,
    pub(crate) asymmetry: f64,
}

/// Where in a slab a point lies: its optical depth below the slab's top
/// face, and the slab's whole optical depth, each as the medium's extinction
/// leaves it before δ-scaling.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) struct Depth {
    pub(crate) below_top: f64,
    pub(crate) whole: f64,
}

/// The diffuse radiance at a point: its mean over every way, and its first
/// moment along the upward vertical.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) struct Diffuse {
    pub(crate) mean: Vec3,
    pub(crate) rising: Vec3,
}

impl Diffuse {
    /// The light the medium scatters toward a way rising at cosine `rise`
    /// from the diffuse light at the point, per unit of its extinction: its
    /// mean, and its first moment through the full phase function's
    /// asymmetry, its forward peak returned to the way it came from.
    pub(crate) fn scattered(&self, medium: Medium, rise: f64) -> Vec3 {
        (self.mean + self.rising * (medium.asymmetry * rise)) * medium.albedo
    }
}

/// The diffuse radiance at `depth` in a slab of `medium`, from the sun's
/// light `sun` falling on its top face at cosine `cosine` from the vertical;
/// none where the sun stands at or below the horizon.
pub(crate) fn sunlit(medium: Medium, depth: Depth, (sun, cosine): (Vec3, f64)) -> Diffuse {
    if cosine <= 1e-4 {
        return Diffuse {
            mean: Vec3::ZERO,
            rising: Vec3::ZERO,
        };
    }
    let solved = Solution::new(medium, depth.whole);
    let (mean, rising) = solved.beam(depth.below_top, cosine);
    Diffuse {
        mean: sun * mean,
        rising: sun * rising,
    }
}

/// The diffuse radiance at `depth` in a slab of `medium` from the diffuse
/// radiance `sky` entering its top face and `ground` entering its base.
pub(crate) fn skylit(medium: Medium, depth: Depth, (sky, ground): (Vec3, Vec3)) -> Diffuse {
    let solved = Solution::new(medium, depth.whole);
    let ((top_mean, top_rising), (base_mean, base_rising)) = solved.faces(depth.below_top);
    Diffuse {
        mean: sky * top_mean + ground * base_mean,
        rising: sky * top_rising + ground * base_rising,
    }
}

/// The two-stream solution's terms for one slab: its scaled medium and
/// thickness, the homogeneous solution's rate of decay and the ratio of its
/// first moment to its mean, and the faces' coefficients.
struct Solution {
    albedo: f64,
    asymmetry: f64,
    whole: f64,
    /// How much the δ-scaling thins the medium.
    thinned: f64,
    decay: f64,
    moment: f64,
}

impl Solution {
    fn new(medium: Medium, whole: f64) -> Self {
        let albedo = medium.albedo.clamp(0.0, 1.0 - 1e-7);
        let g = medium.asymmetry.clamp(-0.99, 0.99);
        let peak = g * g;
        let thinned = 1.0 - peak * albedo;
        let scaled_albedo = (1.0 - peak) * albedo / thinned;
        let scaled_g = g / (1.0 + g);
        let decay = mathf::sqrt(3.0 * (1.0 - scaled_albedo) * (1.0 - scaled_albedo * scaled_g));
        Self {
            albedo: scaled_albedo,
            asymmetry: scaled_g,
            whole: thinned * whole.max(0.0),
            thinned,
            decay,
            moment: decay / (1.0 - scaled_albedo * scaled_g),
        }
    }

    /// The coefficients of the homogeneous solution meeting the faces' own
    /// conditions: the downward diffuse light at the top `top` and the upward
    /// at the base `base`, in radiance, less the particular solution's.
    fn coefficients(&self, (top, base): (f64, f64)) -> (f64, f64) {
        let fading = mathf::exp(-self.decay * self.whole);
        let (a, b) = (1.0 + 2.0 / 3.0 * self.moment, 1.0 - 2.0 / 3.0 * self.moment);
        let determinant = (fading * b) * (fading * b) - a * a;
        (
            (top * fading * b - a * base) / determinant,
            (base * fading * b - a * top) / determinant,
        )
    }

    /// The mean radiance and its first moment at scaled `depth` from the
    /// homogeneous solution with `coefficients`, the first decaying from the
    /// base and the second from the top.
    fn homogeneous(&self, (rising, falling): (f64, f64), depth: f64) -> (f64, f64) {
        let (from_base, from_top) = (
            mathf::exp(self.decay * (depth - self.whole)),
            mathf::exp(-self.decay * depth),
        );
        (
            rising * from_base + falling * from_top,
            self.moment * (rising * from_base - falling * from_top),
        )
    }

    /// The mean diffuse radiance and its first moment at `below_top`, per
    /// unit of the sun's light falling at `cosine`.
    fn beam(&self, below_top: f64, cosine: f64) -> (f64, f64) {
        let (w, g) = (self.albedo, self.asymmetry);
        let k2 = self.decay * self.decay;
        let squared = cosine * cosine;
        let source = 3.0 * w / (4.0 * PI);
        // The resonance where the beam fades as fast as the homogeneous
        // solution does is met only by a medium absorbing far more than any
        // cloud; nudged clear of it all the same.
        let off = (k2 * squared - 1.0)
            .abs()
            .max(1e-9)
            .copysign(k2 * squared - 1.0);
        let mean = source * squared * (1.0 + g * (1.0 - w)) / off;
        let first = (-mean / cosine - source * g * cosine) / (1.0 - w * g);
        let at_base = mathf::exp(-self.whole / cosine);
        let coefficients = self.coefficients((
            -mean + 2.0 / 3.0 * first,
            -(mean + 2.0 / 3.0 * first) * at_base,
        ));
        let depth = self.thinned * below_top.clamp(0.0, self.whole / self.thinned.max(1e-12));
        let (homogeneous, moment) = self.homogeneous(coefficients, depth);
        let falling = mathf::exp(-depth / cosine);
        (homogeneous + mean * falling, moment + first * falling)
    }

    /// The mean diffuse radiance and its first moment at `below_top`, per
    /// unit of the radiance entering the top face, and per unit of that
    /// entering the base.
    fn faces(&self, below_top: f64) -> ((f64, f64), (f64, f64)) {
        let depth = self.thinned * below_top.clamp(0.0, self.whole / self.thinned.max(1e-12));
        (
            self.homogeneous(self.coefficients((1.0, 0.0)), depth),
            self.homogeneous(self.coefficients((0.0, 1.0)), depth),
        )
    }
}

#[cfg(test)]
#[path = "slab_tests.rs"]
mod tests;
