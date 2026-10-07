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
use crate::vector::{single, Ray, Vec3};

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
        let deepest = *depth
            * (self.scale / bark.measure(self.scale))
            * smoothstep(CUT_GIRTH.0, CUT_GIRTH.1, girth * self.scale);
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

/// The girth, in metres, at which a bark rising the faster the thinner its
/// limb, as a cactus's ribs do, rises fastest once its cut has faded with
/// its girth: where `smoothstep(CUT_GIRTH)` over the girth peaks.
const FASTEST_FADED: f64 = 0.11;

/// How far `p` lies from the segment from `a` to `b`.
fn segment_distance(p: Vec3, (a, b): (Vec3, Vec3)) -> f64 {
    let span = b - a;
    let along = (p - a).dot(span) / span.dot(span).max(1e-30);
    (p - (a + span * along.clamp(0.0, 1.0))).length()
}

/// A tube laid out for its march: its first end, the unit way to the
/// second, its length, its radius at the first and how fast it narrows, the
/// sine and cosine of the angle its side leans in at, the two directions its
/// angle is measured between, the bark cut in it if any is near enough to
/// show and what takes the prototype's units to that bark's, and the flare
/// its foot swells in if it does.
///
/// Its side is the cone tangent to the spheres rounding its ends, the tube's
/// own surface, and past where the side meets each it is that sphere, its
/// bark run on over it down its meridians: so a bending limb's joints stay
/// closed, a limb's neighbours meet it without a crease in the sphere they
/// share rather than one's side showing through the other's, and a free end
/// rounds, its bark crowding over it as a cactus's ribs crowd over its apex.
/// A flared foot's side swells by its flare from the cone between its ends'
/// rounds instead.
struct Limb<'a> {
    a: Vec3,
    axis: Vec3,
    length: f64,
    radius: f64,
    taper: f64,
    lean: (f64, f64),
    /// Each end's radius, the sphere that rounds it.
    ends: (f64, f64),
    round: (Vec3, Vec3),
    tube: &'a Tube,
    bark: Option<&'a Bark>,
    measure: f64,
    depth: f64,
    /// The most moss on its bark stands proud of it, in the prototype's own
    /// units, where its bark is cut.
    moss: f64,
    flare: Option<&'a Flare>,
}

/// Where a point lies on a limb: how far along it, as a share of its
/// length, its way out from the axis, the limb's radius there, its angle
/// round it, how far from the eye in metres, the cut's depth there in the
/// prototype's own units, how far it stands out from the uncut limb, and,
/// past its side, where over the sphere rounding the end it lies past.
struct Place {
    along: f64,
    out: Vec3,
    radius: f64,
    angle: f64,
    distance: f64,
    cut: f64,
    over: f64,
    end: Option<End>,
}

/// Where a point past an end a limb rounds lies over the sphere rounding it:
/// that sphere's radius, how far from its middle the point is and the way
/// out to it, how far along the stem carried on over the sphere down its
/// meridian, the sphere's girth there about the limb's axis, and the cut's
/// depth there in the prototype's own units.
struct End {
    radius: f64,
    reach: f64,
    out: Vec3,
    stem: f64,
    girth: f64,
    cut: f64,
}

impl<'a> Limb<'a> {
    /// `tube` laid out for its march, its bark `cut` as `cutting` places the
    /// tube, if any of it is, and its foot swelling in `flare`, if it does;
    /// `None` for a tube of no length.
    fn new(
        tube: &'a Tube,
        (cutting, cut): (Option<&Cutting<'_>>, Option<(&'a Bark, f64)>),
        flare: Option<&'a Flare>,
    ) -> Option<Self> {
        let (a, b) = (point(tube.a), point(tube.b));
        let length = (b - a).length();
        if length <= 1e-12 {
            return None;
        }
        let axis = (b - a) * (1.0 / length);
        let (ra, rb) = (f64::from(tube.radii[0]), f64::from(tube.radii[1]));
        let bark = cut.map(|(bark, _)| bark);
        let sine = ((ra - rb) / length).clamp(-0.999, 0.999);
        Some(Self {
            a,
            axis,
            length,
            radius: ra,
            taper: (rb - ra) / length,
            lean: (sine, mathf::sqrt(1.0 - sine * sine)),
            ends: (ra, rb),
            round: round(axis),
            tube,
            bark,
            measure: match (bark, cutting) {
                (Some(bark), Some(cutting)) => bark.measure(cutting.scale),
                _ => 1.0,
            },
            depth: cut.map_or(0.0, |(_, deepest)| deepest),
            moss: match (bark, cutting) {
                (Some(bark), Some(cutting)) => bark.moss_reach() / cutting.scale,
                _ => 0.0,
            },
            flare,
        })
    }

    /// How much of its full relief the bark shows where it is cut `cut`
    /// deep: the share a moss cushion stands proud by there too.
    fn shown(&self, cut: f64) -> f64 {
        if self.depth > 0.0 {
            (cut / self.depth).clamp(0.0, 1.0)
        } else {
            0.0
        }
    }

    /// How far the bark at `at`, facing `out`, stands out of the surface
    /// the limb is cut to `cut` deep there, its moss raised by as much of
    /// its relief as shows; and how much of it the moss covers.
    fn relief(
        &self,
        (bark, at): (&Bark, &OnLimb),
        out: Vec3,
        cut: f64,
        cutting: &Cutting<'_>,
    ) -> (f64, f64) {
        let height = bark.height(at);
        let (covered, rise) = if self.moss > 0.0 {
            bark.moss_cushion(at, out, height)
        } else {
            (0.0, 0.0)
        };
        (
            cut * (1.0 - height) - rise * self.shown(cut) / cutting.scale,
            covered,
        )
    }

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
        let distance = cutting.map_or(f64::INFINITY, |cutting| {
            (p - cutting.eye).length() * cutting.scale
        });
        let cut = self.cut_at(radius, distance, cutting);
        let along = up / self.length;
        // Where along the side's own slant the point lies: before its first
        // end's tangent round, or past its second's.
        let (sine, cosine) = self.lean;
        let (slant, past) = match self.flare {
            Some(_) => (up, self.length),
            None => (up * cosine - reach * sine, self.length * cosine),
        };
        let end = match slant {
            _ if slant < 0.0 && !self.tube.open[0] => Some((0, self.a, self.ends.0, -self.axis)),
            _ if slant > past && !self.tube.open[1] => {
                Some((1, self.a + self.axis * self.length, self.ends.1, self.axis))
            }
            _ => None,
        }
        .map(|(index, middle, radius, outward)| {
            let from = p - middle;
            let reach = from.length();
            let out = if reach > 1e-12 {
                from * (1.0 / reach)
            } else {
                outward
            };
            let (stem, girth) = self.tube.over_end(index, out);
            End {
                radius,
                reach,
                out,
                stem,
                girth,
                cut: self.cut_at(girth, distance, cutting),
            }
        });
        let over = match (&end, self.flare) {
            (Some(end), _) => end.reach - end.radius,
            (None, Some(_)) => reach - radius,
            (None, None) => up * sine + reach * cosine - self.ends.0,
        };
        Place {
            along,
            out,
            radius,
            angle,
            distance,
            cut,
            over,
            end,
        }
    }

    /// The cut's depth, in the prototype's own units, on a limb `girth`
    /// thick in those units `distance` metres from the eye.
    fn cut_at(&self, girth: f64, distance: f64, cutting: Option<&Cutting<'_>>) -> f64 {
        match (self.bark, cutting) {
            (Some(_), Some(cutting)) => {
                cutting.depth(self.depth, (girth * cutting.scale, distance)) / cutting.scale
            }
            _ => 0.0,
        }
    }

    /// Where `place` lies on the limb's bark, `stem` along its stem and
    /// `girth` thick there in the prototype's own units, seen from the eye.
    fn bark_at(&self, (stem, girth): (f64, f64), place: &Place, cutting: &Cutting<'_>) -> OnLimb {
        OnLimb::new(
            stem * self.measure,
            place.angle,
            girth * self.measure,
            (
                cutting.key,
                place.distance * cutting.pixel * self.measure / cutting.scale,
            ),
        )
    }

    /// How far along its stem the place `along` of the limb's length lies.
    fn stem(&self, along: f64) -> f64 {
        let (from, to) = (f64::from(self.tube.stem[0]), f64::from(self.tube.stem[1]));
        from + (to - from) * along
    }

    /// How far `place` stands out from the limb: negative within it.
    fn above(&self, place: &Place, cutting: Option<&Cutting<'_>>) -> f64 {
        match &place.end {
            Some(end) => self.beyond(end, place, cutting),
            None => self.beside(place, cutting),
        }
    }

    /// How far `place` stands out from the limb's side, its ends aside.
    /// Beyond the bark's outer surface, or deeper than its cut, the bark
    /// cannot change which side of the cut surface a place stands, so it is
    /// read only between.
    fn beside(&self, place: &Place, cutting: Option<&Cutting<'_>>) -> f64 {
        let over = place.over;
        match (self.bark, cutting) {
            (Some(bark), Some(cutting))
                if (-place.cut..=self.moss * self.shown(place.cut)).contains(&over)
                    && place.cut > 0.0 =>
            {
                let at = self.bark_at((self.stem(place.along), place.radius), place, cutting);
                over + self.relief((bark, &at), place.out, place.cut, cutting).0
            }
            _ => over,
        }
    }

    /// The most the limb's flare swells it anywhere.
    fn most(&self) -> f64 {
        self.flare.map_or(1.0, Flare::most)
    }

    /// How far `place`, past `end`, stands outside the sphere rounding it,
    /// cut by the bark run on over it; read only within the cut, as the
    /// side's is.
    fn beyond(&self, end: &End, place: &Place, cutting: Option<&Cutting<'_>>) -> f64 {
        let over = place.over;
        match (self.bark, cutting) {
            (Some(bark), Some(cutting))
                if (-end.cut..=self.moss * self.shown(end.cut)).contains(&over)
                    && end.cut > 0.0 =>
            {
                let at = self.bark_at((end.stem, end.girth), place, cutting);
                over + self.relief((bark, &at), end.out, end.cut, cutting).0
            }
            _ => over,
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
    let limb = Limb::new(tube, (cutting, cut), flare)?;
    let (enter, leave) = body_span(&limb, ray)?;
    let (enter, leave) = (enter.max(near), leave.min(far));
    (enter < leave)
        .then(|| march(&limb, ray, ((enter, leave), seeking), cutting))
        .flatten()
}

/// The stretch of `ray` within the limb's bounds, if it crosses any: the
/// cone the limb and its flare lie in between its ends, and the sphere
/// rounding each end it rounds, which a narrowing limb's cone run on would
/// not hold; each as far out as its moss stands, so a march never begins
/// within a cushion.
fn body_span(limb: &Limb<'_>, ray: &Ray) -> Option<(f64, f64)> {
    let ends = [
        (limb.a, limb.ends.0, limb.tube.open[0]),
        (
            limb.a + limb.axis * limb.length,
            limb.ends.1,
            limb.tube.open[1],
        ),
    ];
    ends.into_iter()
        .filter(|&(_, _, open)| !open)
        .filter_map(|(middle, radius, _)| sphere_span(ray, middle, radius + limb.moss))
        .chain(cone_span(limb, ray))
        .reduce(|(enter, leave), (from, to)| (enter.min(from), leave.max(to)))
}

/// The stretch of `ray` within the sphere about `middle` `radius` across, if
/// it crosses it.
fn sphere_span(ray: &Ray, middle: Vec3, radius: f64) -> Option<(f64, f64)> {
    let to = ray.origin - middle;
    let half_b = to.dot(ray.dir);
    let reach = half_b * half_b - to.dot(to) + radius * radius;
    (reach > 0.0).then(|| {
        let root = mathf::sqrt(reach);
        (-half_b - root, -half_b + root)
    })
}

/// The stretch of `ray` within the cone the limb and its flare lie in
/// between its ends, if it crosses any.
fn cone_span(limb: &Limb<'_>, ray: &Ray) -> Option<(f64, f64)> {
    let offset = ray.origin - limb.a;
    let (up, rate) = (offset.dot(limb.axis), ray.dir.dot(limb.axis));
    // The side tangent to the ends' spheres stands outside the cone between
    // their rounds by its lean, and meets them a little past each end.
    let (low, high) = (
        -limb.ends.0 - limb.moss,
        limb.length + limb.ends.1 + limb.moss,
    );
    let slab = if rate.abs() > 1e-12 {
        let (from, to) = ((low - up) / rate, (high - up) / rate);
        (from.min(to), from.max(to))
    } else if (low..=high).contains(&up) {
        (f64::NEG_INFINITY, f64::INFINITY)
    } else {
        return None;
    };
    let most = limb.most() / limb.lean.1;
    let taper = limb.taper * most;
    let radius = limb.radius * most + limb.moss + taper * up;
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
    let barked = match (limb.bark, cutting) {
        (Some(bark), Some(cutting)) => {
            let side = limb.ends.0.min(limb.ends.1);
            // Over an end's sphere the girth falls away to nothing, but its
            // cut fades out as it thins.
            let rounded = !(limb.tube.open[0] && limb.tube.open[1]);
            let thinnest = if rounded {
                side.min(FASTEST_FADED / cutting.scale)
            } else {
                side
            };
            limb.depth * (limb.measure / cutting.scale) * bark.steepest(thinnest * limb.measure)
                + bark.moss_steepest() * f64::from(u8::from(limb.moss > 0.0))
        }
        _ => 0.0,
    };
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
        // Moss may stand proud of the bark's outer surface, so the limb's
        // own gentle rise bounds a step only beyond the moss's reach.
        let over = place.over - limb.moss;
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
    let p = ray.at(t);
    let place = limb.place(p, cutting);
    let spacing = 4.0 * tolerance;
    let covered = match (limb.bark, cutting) {
        (Some(bark), Some(cutting)) if limb.moss > 0.0 => {
            let (stem, girth, out, cut) = match &place.end {
                Some(end) => (end.stem, end.girth, end.out, end.cut),
                None => (limb.stem(place.along), place.radius, place.out, place.cut),
            };
            (cut > 0.0).then(|| {
                let at = limb.bark_at((stem, girth), &place, cutting);
                limb.relief((bark, &at), out, cut, cutting).1
            })
        }
        _ => None,
    };
    let ((normal, relieved), (stem, girth)) = if let Some(end) = &place.end {
        let normal = if end.cut > 0.0 {
            gradient(p, spacing, |q| limb.above(&limb.place(q, cutting), cutting))
        } else {
            end.out
        };
        ((normal, end.cut > 0.0), (end.stem, end.girth))
    } else {
        let normal = if place.cut > 0.0 || limb.flare.is_some() {
            gradient(p, spacing, |q| {
                limb.beside(&limb.place(q, cutting), cutting)
            })
        } else {
            place.out * limb.lean.1 + limb.axis * limb.lean.0
        };
        (
            (normal, place.cut > 0.0),
            (limb.stem(place.along), place.radius),
        )
    };
    Hit {
        t,
        normal,
        shading: normal,
        mark: limb.tube.key,
        along: place.along.clamp(0.0, 1.0),
        uv: (stem, place.angle),
        girth,
        material: Some(u32::from(limb.tube.material)),
        tangent: limb.axis,
        relieved,
        member: None,
        cover: covered.map(single),
    }
}

/// The way out of a surface at `p`: how fast `standing` — how far a point
/// stands out from it — grows, taken `step` either side along each axis. It
/// is the surface the march crossed, so it faces whatever ray crossed into
/// it, however steep the bark's walls or the flare's lobes; an end, rounded
/// or open, is not read by the side's, so a hit at its rim faces out of the
/// side it is on.
pub(crate) fn gradient(p: Vec3, step: f64, standing: impl Fn(Vec3) -> f64) -> Vec3 {
    differences(p, step, standing).normalized()
}

/// How fast `field` rises at `p`, and which way, taken `step` about it.
pub(crate) fn slope(p: Vec3, step: f64, field: impl Fn(Vec3) -> f64) -> Vec3 {
    differences(p, step, field) * (0.25 / step)
}

/// Four times `step` times the gradient of `field` at `p`.
fn differences(p: Vec3, step: f64, field: impl Fn(Vec3) -> f64) -> Vec3 {
    // Four samples at a tetrahedron's corners find as much as six along the
    // axes would.
    TETRAHEDRON.iter().fold(Vec3::ZERO, |sum, &corner| {
        sum + corner * field(p + corner * step)
    })
}

/// The corners of a tetrahedron about the origin, each a step along every
/// axis.
const TETRAHEDRON: [Vec3; 4] = [
    Vec3::new(1.0, -1.0, -1.0),
    Vec3::new(-1.0, -1.0, 1.0),
    Vec3::new(-1.0, 1.0, -1.0),
    Vec3::new(1.0, 1.0, 1.0),
];

#[cfg(test)]
#[path = "cut_tests.rs"]
mod tests;
