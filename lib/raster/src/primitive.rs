//! A triangle as the general pipeline sets it up: the samples it covers, the
//! depth it carries across the screen, and the weights its corners' values
//! are interpolated with.
//!
//! Its corners are what a [`Viewport`](crate::clip::Viewport) clip leaves,
//! each with its share of the triangle a draw named, in clip space and across
//! the screen. The weights a sample is given are composed back into that
//! triangle's own corners, so a value interpolated across any piece of a
//! clipped triangle is the value the whole triangle gives there.

use tairix_util::mathf;

use crate::clip::Mapped;
use crate::mesh::{area_pixels, coordinate, Footprint, Plane};
use crate::scan::UNIT;

/// How a value is interpolated across a triangle.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Interpolation {
    /// Perspective-correct: linear in clip space.
    Perspective,
    /// Linear across the screen.
    Linear,
    /// The provoking corner's value throughout: the first of the triangle the
    /// draw named.
    Flat,
}

/// A triangle set up for the general pipeline.
#[derive(Clone, Debug)]
pub struct Primitive {
    footprint: Footprint,
    /// Corner zero's position, in pixels.
    origin: (f64, f64),
    depth: Plane,
    /// The reciprocal `w`, the perspective weight.
    weight: Plane,
    /// Corners one's and two's shares across the screen, each times its
    /// perspective weight.
    first: Plane,
    second: Plane,
    /// Corners one's and two's shares across the screen.
    screen: [Plane; 2],
    /// What each corner is of the triangle the draw named, in clip space.
    weights: [[f64; 3]; 3],
    /// The same across the screen.
    linear: [[f64; 3]; 3],
}

impl Primitive {
    /// Set up the triangle whose corners are `corners`, or `None` where it
    /// covers no area, a corner lies past the converter's exact range, or a
    /// corner's depth or perspective weight is not finite or the weight not
    /// positive.
    #[must_use]
    pub fn new(corners: [Mapped; 3]) -> Option<Self> {
        if corners.iter().any(|corner| {
            !corner.depth.is_finite() || !corner.inverse_w.is_finite() || corner.inverse_w <= 0.0
        }) {
            return None;
        }
        let at = corners.map(|corner| (i64::from(corner.x), i64::from(corner.y)));
        let (footprint, area) = Footprint::new(at)?;
        let pixels = |sub: i64| coordinate(sub) / coordinate(UNIT);
        let across = [pixels(at[1].0 - at[0].0), pixels(at[2].0 - at[0].0)];
        let down = [pixels(at[1].1 - at[0].1), pixels(at[2].1 - at[0].1)];
        // From the exact area, so a sliver's determinant keeps its sign.
        let det = area_pixels(area);
        let plane = |values| Plane::through(values, across, down, det);
        let q = corners.map(|corner| corner.inverse_w);
        Some(Self {
            footprint,
            origin: (pixels(at[0].0), pixels(at[0].1)),
            depth: plane(corners.map(|corner| corner.depth)),
            weight: plane(q),
            first: plane([0.0, q[1], 0.0]),
            second: plane([0.0, 0.0, q[2]]),
            screen: [plane([0.0, 1.0, 0.0]), plane([0.0, 0.0, 1.0])],
            weights: corners.map(|corner| corner.weights),
            linear: corners.map(|corner| corner.linear),
        })
    }

    /// Which samples it covers.
    #[must_use]
    pub const fn footprint(&self) -> &Footprint {
        &self.footprint
    }

    /// Its depth at `(x, y)`, in pixels from the drawing's origin.
    #[must_use]
    pub fn depth(&self, x: f64, y: f64) -> f64 {
        self.depth.at(x - self.origin.0, y - self.origin.1)
    }

    /// The reciprocal of its clip-space `w` at `(x, y)`, in pixels: the
    /// perspective weight a value there is divided by.
    #[must_use]
    pub fn inverse_w(&self, x: f64, y: f64) -> f64 {
        self.weight.at(x - self.origin.0, y - self.origin.1)
    }

    /// The most its depth changes over a pixel, across or down: what a
    /// slope-scaled depth bias scales.
    #[must_use]
    pub fn slope(&self) -> f64 {
        mathf::fmax(mathf::fabs(self.depth.dx), mathf::fabs(self.depth.dy))
    }

    /// What each corner of the triangle the draw named is of a value at
    /// `(x, y)`, in pixels, under `interpolation`; the three sum to one.
    #[must_use]
    pub fn weights(&self, interpolation: Interpolation, x: f64, y: f64) -> [f64; 3] {
        let (across, down) = (x - self.origin.0, y - self.origin.1);
        match interpolation {
            Interpolation::Flat => [1.0, 0.0, 0.0],
            Interpolation::Linear => {
                let one = self.screen[0].at(across, down);
                let two = self.screen[1].at(across, down);
                compose([1.0 - one - two, one, two], &self.linear)
            }
            Interpolation::Perspective => {
                let inverse = 1.0 / self.weight.at(across, down);
                let one = self.first.at(across, down) * inverse;
                let two = self.second.at(across, down) * inverse;
                compose([1.0 - one - two, one, two], &self.weights)
            }
        }
    }
}

/// The shares `own` a sample takes of a piece's corners, carried to the
/// triangle they are pieces of, `corners[j]` being what corner `j` is of it.
fn compose(own: [f64; 3], corners: &[[f64; 3]; 3]) -> [f64; 3] {
    core::array::from_fn(|corner| {
        own[0] * corners[0][corner] + own[1] * corners[1][corner] + own[2] * corners[2][corner]
    })
}

#[cfg(test)]
#[path = "primitive_tests.rs"]
mod tests;
