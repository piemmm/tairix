//! Bark in true relief: a limb near the eye cut by its own bark, its ridges
//! standing at the limb's radius and its fissures sunk the bark's depth into
//! it, so its outline is ridged and its furrows shadow one another. Farther
//! off, where the cut would span too few pixels to show, the limb is its tube
//! again and its bark only tilts the light; a thin limb, whose bark has not
//! yet fissured, is never cut.
//!
//! A limb whose foot flares toward its roots is met the same way however far
//! off it stands, the round tube it would otherwise be swollen by its
//! flare's lobes, and its bark cut in them too where the eye is near.
//!
//! The cut surface stands over the limb's core by the bark's height at each
//! point's place along and round the limb, and is met by sphere tracing
//! (Hart, "Sphere Tracing", 1996): each step the height above the surface
//! over how fast the surface can rise, never shorter than a sliver of the
//! pixel there, and a crossing then found by bisection. It depends only on
//! where a point lies and where the eye stands, so every ray — the eye's, a
//! shadow's, a bounce's — meets the one surface.

use tairix_util::mathf;

use crate::bark::{Bark, OnLimb};
use crate::flare::Flare;
use crate::material::{Material, Relief};
use crate::noise::smoothstep;
use crate::prototype::{point, round, Tube};
use crate::shape::{quadratic, Hit};
use crate::vector::{Ray, Vec3};

/// Where a scene is seen from: the eye, and the angle a pixel spans.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) struct Viewpoint {
    pub(crate) eye: Vec3,
    pub(crate) pixel: f64,
}

/// A placing of a prototype, as cutting its limbs needs it: the scene's
/// materials, the key it was placed under, how many times its own size it
/// stands, the eye in its own frame and units, and the angle a pixel spans.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Cutting<'a> {
    pub(crate) materials: &'a [Material],
    pub(crate) key: u32,
    pub(crate) scale: f64,
    pub(crate) eye: Vec3,
    pub(crate) pixel: f64,
}

/// How many pixels a cut must span to show at all, and from how many it is
/// cut in full.
const SPANS: (f64, f64) = (1.5, 3.0);

/// The girths, in metres, over which a limb's bark comes to be cut: a young
/// branch's is smooth, a trunk's fissured.
const CUT_GIRTH: (f64, f64) = (0.03, 0.12);

/// The shortest step a march takes, as a share of the pixel where it stands
/// and in metres at the least; the most steps it takes over its stretch, its
/// shortest lengthened where a long stretch would want more; and how closely
/// bisection finds a crossing, as a share of that shortest step.
const LEAST_STEP: (f64, f64) = (0.75, 4e-4);
const MOST_STEPS: f64 = 256.0;
const FOUND: f64 = 0.01;

/// How much faster than the bark's own steepest the cut surface may rise
/// along a ray, for its fading with distance and its taper.
const MARGIN: f64 = 1.25;

impl Cutting<'_> {
    /// The bark `tube` is cut in and the deepest its cut could be, in
    /// metres, if any of the tube stands near enough the eye to show it.
    pub(crate) fn of(&self, tube: &Tube) -> Option<(&Bark, f64)> {
        // Most limbs a ray passes are twigs: the girth is tested first.
        let girth = f64::from(tube.radii[0].max(tube.radii[1]));
        if girth * self.scale <= CUT_GIRTH.0 {
            return None;
        }
        let material = self.materials.get(usize::from(tube.material))?;
        let Some(Relief::Bark { bark, depth }) = material.relief.as_ref() else {
            return None;
        };
        let deepest = *depth * smoothstep(CUT_GIRTH.0, CUT_GIRTH.1, girth * self.scale);
        if deepest <= 0.0 {
            return None;
        }
        let nearest = (segment_distance(self.eye, (point(tube.a), point(tube.b))) - girth).max(0.0)
            * self.scale;
        let spans = deepest / (nearest * self.pixel).max(1e-12);
        (spans > SPANS.0).then_some((bark, deepest))
    }

    /// How deep the cut is, in metres, at `distance` metres from the eye on
    /// a limb `girth` metres in radius, its bark cut `depth` deep in full:
    /// as deep as its bark wherever the cut spans enough pixels, fading to
    /// none where it would span too few.
    fn depth(&self, depth: f64, (girth, distance): (f64, f64)) -> f64 {
        let deep = depth * smoothstep(CUT_GIRTH.0, CUT_GIRTH.1, girth);
        let spans = deep / (distance * self.pixel).max(1e-12);
        deep * smoothstep(SPANS.0, SPANS.1, spans)
    }
}

/// How far `p` lies from the segment from `a` to `b`.
fn segment_distance(p: Vec3, (a, b): (Vec3, Vec3)) -> f64 {
    let span = b - a;
    let along = (p - a).dot(span) / span.dot(span).max(1e-30);
    (p - (a + span * along.clamp(0.0, 1.0))).length()
}

/// A tube laid out for its march: its first end, the unit way to the
/// second, its length, its radius at the first and how fast it narrows, the
/// two directions its angle is measured between, the bark cut in it if any
/// is near enough to show, and the flare its foot swells in if it does. Past
/// each end it rounds, the limb runs on within that end's sphere, so
/// neighbours along a bending limb overlap rather than open a notch on its
/// outer side, and a free end is rounded as the tube's is.
struct Limb<'a> {
    a: Vec3,
    axis: Vec3,
    length: f64,
    radius: f64,
    taper: f64,
    /// Each end's radius, the sphere that rounds it.
    ends: (f64, f64),
    round: (Vec3, Vec3),
    tube: &'a Tube,
    bark: Option<&'a Bark>,
    depth: f64,
    flare: Option<&'a Flare>,
}

/// Where a point lies on a limb: how far along it, as a share of its
/// length, its way out from the axis and how far out, the limb's radius
/// there, its angle round it, how far from the eye in metres, and the cut's
/// depth there in the prototype's own units.
struct Place {
    along: f64,
    out: Vec3,
    reach: f64,
    radius: f64,
    angle: f64,
    distance: f64,
    cut: f64,
}

impl Limb<'_> {
    /// Where `p` lies on the limb, seen as `cutting` sees it, if anything
    /// does yet.
    fn place(&self, p: Vec3, cutting: Option<&Cutting<'_>>) -> Place {
        let offset = p - self.a;
        let up = offset.dot(self.axis);
        let radial = offset - self.axis * up;
        let reach = radial.length();
        let out = if reach > 1e-12 {
            radial * (1.0 / reach)
        } else {
            self.round.0
        };
        let angle =
            mathf::atan2(out.dot(self.round.1), out.dot(self.round.0)) - f64::from(self.tube.turn);
        let round_radius = self.radius + self.taper * up;
        let radius = round_radius * self.flare.map_or(1.0, |flare| flare.factor(up, angle));
        let (distance, cut) = match cutting {
            Some(cutting) => {
                let distance = (p - cutting.eye).length() * cutting.scale;
                let cut = if self.bark.is_some() {
                    cutting.depth(self.depth, (radius * cutting.scale, distance)) / cutting.scale
                } else {
                    0.0
                };
                (distance, cut)
            }
            None => (f64::INFINITY, 0.0),
        };
        Place {
            along: up / self.length,
            out,
            reach,
            radius,
            angle,
            distance,
            cut,
        }
    }

    /// Where `place` lies on the limb's bark, seen from the eye.
    fn bark_at(&self, place: &Place, cutting: &Cutting<'_>) -> OnLimb {
        let stem = self.stem(place.along) * cutting.scale;
        OnLimb::new(
            stem,
            place.angle,
            place.radius * cutting.scale,
            (cutting.key, place.distance * cutting.pixel),
        )
    }

    /// How far along its stem the place `along` of the limb's length lies.
    fn stem(&self, along: f64) -> f64 {
        let (from, to) = (f64::from(self.tube.stem[0]), f64::from(self.tube.stem[1]));
        from + (to - from) * along
    }

    /// How far `place` stands out from the limb: negative within it.
    fn above(&self, place: &Place, cutting: Option<&Cutting<'_>>) -> f64 {
        self.beside(place, cutting).max(self.beyond(place))
    }

    /// How far `place` stands out from the limb's side, its ends aside.
    /// Beyond the bark's outer surface, or deeper than its cut, the bark
    /// cannot change which side of the cut surface a place stands, so it is
    /// read only between.
    fn beside(&self, place: &Place, cutting: Option<&Cutting<'_>>) -> f64 {
        let over = place.reach - place.radius;
        match (self.bark, cutting) {
            (Some(bark), Some(cutting))
                if (-place.cut..=0.0).contains(&over) && place.cut > 0.0 =>
            {
                over + place.cut * (1.0 - bark.height(&self.bark_at(place, cutting)))
            }
            _ => over,
        }
    }

    /// The most the limb's flare swells it anywhere.
    fn most(&self) -> f64 {
        self.flare.map_or(1.0, Flare::most)
    }

    /// How far `place` stands outside the sphere rounding the end it lies
    /// past, if it lies past either end.
    fn beyond(&self, place: &Place) -> f64 {
        if place.along < 0.0 {
            mathf::hypot(place.along * self.length, place.reach) - self.ends.0
        } else if place.along > 1.0 {
            mathf::hypot((place.along - 1.0) * self.length, place.reach) - self.ends.1
        } else {
            f64::NEG_INFINITY
        }
    }
}

/// What a march looks for: where a ray first meets the cut limb, or only
/// whether it does, as a shadow asks.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Seeking {
    Nearest,
    Any,
}

/// Where `ray` meets `tube` within `(near, far)`, its bark `cut` as
/// `cutting` places the tube, if any of it is cut, and its foot swelling in
/// `flare`, if it does; for a shadow, which seeks only whether it does, the
/// place it was found crossing in.
pub(crate) fn meet_relieved(
    tube: &Tube,
    ray: &Ray,
    ((near, far), seeking): ((f64, f64), Seeking),
    (cutting, cut): (Option<&Cutting<'_>>, Option<(&Bark, f64)>),
    flare: Option<&Flare>,
) -> Option<Hit> {
    let (a, b) = (point(tube.a), point(tube.b));
    let length = (b - a).length();
    if length <= 1e-12 {
        return None;
    }
    let axis = (b - a) * (1.0 / length);
    let (ra, rb) = (f64::from(tube.radii[0]), f64::from(tube.radii[1]));
    let limb = Limb {
        a,
        axis,
        length,
        radius: ra,
        taper: (rb - ra) / length,
        ends: (ra, rb),
        round: round(axis),
        tube,
        bark: cut.map(|(bark, _)| bark),
        depth: cut.map_or(0.0, |(_, deepest)| deepest),
        flare,
    };
    let overlap = (
        if tube.open[0] { 0.0 } else { ra },
        if tube.open[1] { 0.0 } else { rb },
    );
    let (enter, leave) = body_span(&limb, ray, overlap)?;
    let (enter, leave) = (enter.max(near), leave.min(far));
    (enter < leave)
        .then(|| march(&limb, ray, ((enter, leave), seeking), cutting))
        .flatten()
}

/// The stretch of `ray` within the cone the limb and its flare lie in, run
/// on past its first end and its second by `overlap`, if it crosses any.
fn body_span(limb: &Limb<'_>, ray: &Ray, overlap: (f64, f64)) -> Option<(f64, f64)> {
    let offset = ray.origin - limb.a;
    let (up, rate) = (offset.dot(limb.axis), ray.dir.dot(limb.axis));
    let (low, high) = (-overlap.0, limb.length + overlap.1);
    let slab = if rate.abs() > 1e-12 {
        let (from, to) = ((low - up) / rate, (high - up) / rate);
        (from.min(to), from.max(to))
    } else if (low..=high).contains(&up) {
        (f64::NEG_INFINITY, f64::INFINITY)
    } else {
        return None;
    };
    let most = limb.most();
    let taper = limb.taper * most;
    let radius = limb.radius * most + taper * up;
    let a = 1.0 - rate * rate * (1.0 + taper * taper);
    let half_b = offset.dot(ray.dir) - up * rate - taper * rate * radius;
    let c = offset.dot(offset) - up * up - radius * radius;
    let cone = match quadratic(a, half_b, c) {
        Some((first, second)) if a > 0.0 => (first, second),
        // Along a cone's own lines its inside lies outside the roots: the
        // piece the slab holds.
        Some((first, second)) => {
            if slab.0 < first {
                (slab.0, first)
            } else {
                (second, slab.1)
            }
        }
        None if c <= 0.0 => (f64::NEG_INFINITY, f64::INFINITY),
        None => return None,
    };
    let span = (cone.0.max(slab.0), cone.1.min(slab.1));
    (span.0 < span.1).then_some(span)
}

/// The surface `ray` first meets within `(enter, leave)` along the limb.
fn march(
    limb: &Limb<'_>,
    ray: &Ray,
    ((enter, leave), seeking): ((f64, f64), Seeking),
    cutting: Option<&Cutting<'_>>,
) -> Option<Hit> {
    let barked = limb.bark.map_or(0.0, |bark| limb.depth * bark.steepest());
    let flared = limb
        .flare
        .map_or(0.0, |flare| flare.steepest(limb.ends.0.max(limb.ends.1)));
    // Outside the bark the surface lies no nearer than the limb it is cut
    // into, which rises far less steeply than the bark's walls.
    let envelope = MARGIN * (1.0 + limb.taper.abs() * limb.most() + flared);
    let steepest = envelope + MARGIN * barked;
    let shortest = |place: &Place| {
        let least = match cutting {
            Some(cutting) => {
                let pixel = place.distance * cutting.pixel / cutting.scale;
                (LEAST_STEP.0 * pixel).max(LEAST_STEP.1 / cutting.scale)
            }
            None => LEAST_STEP.1,
        };
        least.max((leave - enter) / MOST_STEPS)
    };
    let (mut before, mut t) = (enter, enter);
    let mut place = limb.place(ray.at(t), cutting);
    let mut above = limb.above(&place, cutting);
    // A ray leaving the bark from within a hollow it rounds off is let out
    // before it looks for the surface again.
    let mut inside = above < 0.0;
    let found = |(before, t): (f64, f64), place: &Place| {
        let tolerance = FOUND * shortest(place);
        match seeking {
            Seeking::Nearest => crossing(limb, ray, ((before, t), tolerance), cutting),
            Seeking::Any => Hit::plain(t, place.out),
        }
    };
    while t < leave {
        if above < 0.0 && !inside {
            return Some(found((before, t), &place));
        }
        if above >= 0.0 {
            inside = false;
        }
        let over = place.reach - place.radius;
        let step = if over > 0.0 {
            over / envelope
        } else {
            above.abs() / steepest
        }
        .max(shortest(&place));
        before = t;
        t = (t + step).min(leave);
        place = limb.place(ray.at(t), cutting);
        above = limb.above(&place, cutting);
        if t >= leave {
            break;
        }
    }
    (above < 0.0 && !inside).then(|| found((before, t), &place))
}

/// The hit where `ray` crosses into the limb's surface between `outside`
/// and `within`, found to within `tolerance`.
fn crossing(
    limb: &Limb<'_>,
    ray: &Ray,
    ((mut outside, mut within), tolerance): ((f64, f64), f64),
    cutting: Option<&Cutting<'_>>,
) -> Hit {
    while within - outside > tolerance {
        let middle = outside.midpoint(within);
        if limb.above(&limb.place(ray.at(middle), cutting), cutting) < 0.0 {
            within = middle;
        } else {
            outside = middle;
        }
    }
    let t = outside.midpoint(within);
    let place = limb.place(ray.at(t), cutting);
    let rounded = limb.beyond(&place) >= place.reach - place.radius;
    let normal = if rounded {
        let end = if place.along < 0.0 {
            limb.a
        } else {
            limb.a + limb.axis * limb.length
        };
        (ray.at(t) - end).normalized()
    } else if place.cut > 0.0 || limb.flare.is_some() {
        outward(limb, ray.at(t), 4.0 * tolerance, cutting)
    } else {
        (place.out - limb.axis * limb.taper).normalized()
    };
    let along = place.along.clamp(0.0, 1.0);
    Hit {
        t,
        normal,
        shading: normal,
        mark: limb.tube.key,
        along,
        uv: (limb.stem(place.along), place.angle),
        girth: place.radius,
        material: Some(u32::from(limb.tube.material)),
        tangent: limb.axis,
        relieved: place.cut > 0.0 && !rounded,
    }
}

/// The way out of the limb's side at `p`: how fast a point's standing
/// beside it grows, taken `step` either side along each axis. It is the
/// surface the march crossed, so it faces whatever ray crossed into it,
/// however steep the bark's walls or the flare's lobes; an end, rounded or
/// open, is not read, so a hit at its rim faces out of the side it is on.
fn outward(limb: &Limb<'_>, p: Vec3, step: f64, cutting: Option<&Cutting<'_>>) -> Vec3 {
    let above = |q: Vec3| limb.beside(&limb.place(q, cutting), cutting);
    let rise = |way: Vec3| above(p + way * step) - above(p - way * step);
    Vec3::new(
        rise(Vec3::new(1.0, 0.0, 0.0)),
        rise(Vec3::UP),
        rise(Vec3::new(0.0, 0.0, 1.0)),
    )
    .normalized()
}

#[cfg(test)]
#[path = "cut_tests.rs"]
mod tests;
