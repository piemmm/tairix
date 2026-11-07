//! The elliptical arc: SVG's endpoint form, its centre form, and its cubics.

use core::f64::consts::{FRAC_PI_2, TAU};

use tairix_util::mathf::{acos, ceil, cos, fmax, hypot, sin, sqrt, tan};

use crate::cubic::CubicBez;
use crate::point::Point;

/// The most cubics a single arc is drawn with: a whole turn takes four, and
/// a sweep is held to a whole turn, so this is never reached by an arc that
/// draws anything a smaller sweep would not.
const MAX_CUBICS: u32 = 4;

/// An arc of an ellipse in centre form: the ellipse's centre, its radii, the
/// turn of its x axis, and the parameter angle the arc starts at and sweeps
/// through, positive clockwise as drawn.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct EllipseArc {
    /// The ellipse's centre.
    pub centre: Point,
    /// The radii along the ellipse's own axes.
    pub radii: Point,
    /// The turn of the ellipse's x axis from the plane's, in radians.
    pub x_rotation: f64,
    /// The parameter angle the arc starts at, in radians.
    pub start: f64,
    /// The parameter angle it sweeps through, in radians.
    pub sweep: f64,
}

impl EllipseArc {
    /// SVG's endpoint arc from `from` to `to` on radii `radii` with its x
    /// axis turned `x_rotation` radians, in centre form.
    ///
    /// `None` for the cases the specification defines as a straight line:
    /// coincident endpoints, or a zero radius. Radii too small to reach both
    /// endpoints are grown by the specification's correction factor rather
    /// than refused.
    #[must_use]
    pub fn from_svg(
        from: Point,
        to: Point,
        radii: Point,
        x_rotation: f64,
        large: bool,
        sweep: bool,
    ) -> Option<Self> {
        if from == to {
            return None;
        }
        let (mut rx, mut ry) = (radii.x.abs(), radii.y.abs());
        if rx == 0.0 || ry == 0.0 {
            return None;
        }
        let (cos_r, sin_r) = (cos(x_rotation), sin(x_rotation));
        let (dx, dy) = ((from.x - to.x) / 2.0, (from.y - to.y) / 2.0);
        let x1 = cos_r * dx + sin_r * dy;
        let y1 = -sin_r * dx + cos_r * dy;

        let lambda = (x1 * x1) / (rx * rx) + (y1 * y1) / (ry * ry);
        if lambda > 1.0 {
            let grow = sqrt(lambda);
            rx *= grow;
            ry *= grow;
        }

        let numerator = fmax(
            rx * rx * ry * ry - rx * rx * y1 * y1 - ry * ry * x1 * x1,
            0.0,
        );
        let denominator = rx * rx * y1 * y1 + ry * ry * x1 * x1;
        if denominator <= 0.0 {
            return None;
        }
        let sign = if large == sweep { -1.0 } else { 1.0 };
        let coefficient = sign * sqrt(numerator / denominator);
        let cx1 = coefficient * rx * y1 / ry;
        let cy1 = -coefficient * ry * x1 / rx;
        let centre = Point::new(
            cos_r * cx1 - sin_r * cy1 + f64::midpoint(from.x, to.x),
            sin_r * cx1 + cos_r * cy1 + f64::midpoint(from.y, to.y),
        );

        let u = Point::new((x1 - cx1) / rx, (y1 - cy1) / ry);
        let v = Point::new((-x1 - cx1) / rx, (-y1 - cy1) / ry);
        let start = angle_between(Point::new(1.0, 0.0), u);
        let mut delta = angle_between(u, v) % TAU;
        if !sweep && delta > 0.0 {
            delta -= TAU;
        } else if sweep && delta < 0.0 {
            delta += TAU;
        }
        Some(Self {
            centre,
            radii: Point::new(rx, ry),
            x_rotation,
            start,
            sweep: delta,
        })
    }

    /// The point at parameter angle `angle`.
    #[must_use]
    pub fn point_at(self, angle: f64) -> Point {
        let (cos_r, sin_r) = (cos(self.x_rotation), sin(self.x_rotation));
        let (x, y) = (self.radii.x * cos(angle), self.radii.y * sin(angle));
        Point::new(
            self.centre.x + cos_r * x - sin_r * y,
            self.centre.y + sin_r * x + cos_r * y,
        )
    }

    /// Where it starts.
    #[must_use]
    pub fn start_point(self) -> Point {
        self.point_at(self.start)
    }

    /// Where it ends.
    #[must_use]
    pub fn end_point(self) -> Point {
        self.point_at(self.start + self.sweep)
    }

    /// The arc's directions at its two ends, from the same parameterisation
    /// it is drawn by, so they are exact rather than chord approximations.
    #[must_use]
    pub fn tangents(self) -> (Point, Point) {
        let (cos_r, sin_r) = (cos(self.x_rotation), sin(self.x_rotation));
        let facing = |angle: f64| {
            let (dx, dy) = (-self.radii.x * sin(angle), self.radii.y * cos(angle));
            let turned = Point::new(cos_r * dx - sin_r * dy, sin_r * dx + cos_r * dy);
            if self.sweep < 0.0 {
                -turned
            } else {
                turned
            }
        };
        (facing(self.start), facing(self.start + self.sweep))
    }

    /// Hand `emit` the cubics that draw the arc, in order, each sweeping at
    /// most a quarter turn: the standard `4/3·tan(θ/4)` control length, which
    /// keeps a circle within 0.03% of its radius. A sweep beyond a whole turn
    /// retraces it, so it is drawn as one turn.
    pub fn to_cubics(self, mut emit: impl FnMut(CubicBez)) {
        let sweep = self.sweep.clamp(-TAU, TAU);
        if sweep == 0.0 || !sweep.is_finite() {
            return;
        }
        let quarters = ceil(sweep.abs() / FRAC_PI_2 - 1e-9);
        let pieces = if quarters >= 1.0 {
            // The quotient is at most four by the clamp above.
            pieces_of(quarters)
        } else {
            1
        };
        let step = sweep / f64::from(pieces);
        let handle = 4.0 / 3.0 * tan(step / 4.0);
        let (cos_r, sin_r) = (cos(self.x_rotation), sin(self.x_rotation));
        let place = |unit: Point| {
            let (x, y) = (self.radii.x * unit.x, self.radii.y * unit.y);
            Point::new(
                self.centre.x + cos_r * x - sin_r * y,
                self.centre.y + sin_r * x + cos_r * y,
            )
        };
        for piece in 0..pieces {
            let a0 = self.start + step * f64::from(piece);
            let a1 = a0 + step;
            let (u0, u1) = (Point::from_angle(a0), Point::from_angle(a1));
            emit(CubicBez::new(
                place(u0),
                place(u0 + u0.perpendicular() * handle),
                place(u1 - u1.perpendicular() * handle),
                place(u1),
            ));
        }
    }
}

/// `quarters`, a whole number from one to [`MAX_CUBICS`], as a count.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the caller holds the sweep to a whole turn, so the value is a whole \
              number from one to four, which the cast carries exactly"
)]
fn pieces_of(quarters: f64) -> u32 {
    (quarters as u32).clamp(1, MAX_CUBICS)
}

/// The signed angle from `u` to `v`, in `-PI..=PI`.
#[must_use]
pub fn angle_between(u: Point, v: Point) -> f64 {
    let lengths = hypot(u.x, u.y) * hypot(v.x, v.y);
    if lengths == 0.0 {
        return 0.0;
    }
    let cosine = (u.x * v.x + u.y * v.y) / lengths;
    let angle = acos(cosine);
    if u.x * v.y - u.y * v.x < 0.0 {
        -angle
    } else {
        angle
    }
}

#[cfg(test)]
mod tests {
    use core::f64::consts::{FRAC_PI_2, PI, TAU};

    use super::EllipseArc;
    use crate::point::Point;

    #[test]
    fn an_svg_arc_lands_on_both_endpoints() {
        let from = Point::new(10.0, 0.0);
        let to = Point::new(0.0, 10.0);
        let arc = EllipseArc::from_svg(from, to, Point::new(10.0, 10.0), 0.0, false, true)
            .expect("a real arc");
        assert!(arc.start_point().distance(from) < 1e-9);
        assert!(arc.end_point().distance(to) < 1e-9);
        assert!((arc.sweep - FRAC_PI_2).abs() < 1e-9);
    }

    #[test]
    fn coincident_ends_or_a_zero_radius_are_no_arc() {
        let p = Point::new(1.0, 1.0);
        assert_eq!(
            EllipseArc::from_svg(p, p, Point::new(5.0, 5.0), 0.0, false, false),
            None
        );
        let q = Point::new(4.0, 1.0);
        assert_eq!(
            EllipseArc::from_svg(p, q, Point::new(0.0, 5.0), 0.0, false, false),
            None
        );
    }

    #[test]
    fn a_whole_turn_is_four_cubics_hugging_the_circle() {
        let arc = EllipseArc {
            centre: Point::new(5.0, -5.0),
            radii: Point::new(3.0, 3.0),
            x_rotation: 0.3,
            start: 0.0,
            sweep: TAU,
        };
        let mut cubics = alloc::vec::Vec::new();
        arc.to_cubics(|c| cubics.push(c));
        assert_eq!(cubics.len(), 4);
        for cubic in &cubics {
            for step in 0..=16 {
                let r = cubic.eval(f64::from(step) / 16.0).distance(arc.centre);
                assert!((r - 3.0).abs() < 3.0 * 3e-4, "radius {r}");
            }
        }
        assert!(cubics[3].p3.distance(cubics[0].p0) < 1e-9);
    }

    #[test]
    fn a_small_sweep_is_one_cubic_and_a_backwards_one_runs_back() {
        let arc = EllipseArc {
            centre: Point::ZERO,
            radii: Point::new(1.0, 2.0),
            x_rotation: 0.0,
            start: PI,
            sweep: -0.5,
        };
        let mut count = 0;
        arc.to_cubics(|cubic| {
            count += 1;
            assert!(cubic.p0.distance(arc.start_point()) < 1e-12);
            assert!(cubic.p3.distance(arc.end_point()) < 1e-12);
        });
        assert_eq!(count, 1);
    }
}
