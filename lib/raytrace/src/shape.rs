//! The shapes a scene is built from, and where a ray first meets each.

use core::f64::consts::TAU;

use tairix_util::mathf;

use crate::foliage::Crown;
use crate::grass::Lawn;
use crate::heightfield::Heightfield;
use crate::vector::{Pose, Ray, Vec3};

/// Where a ray met a shape.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Hit {
    /// Distance along the ray.
    pub(crate) t: f64,
    /// The surface's unit normal there, pointing out of the shape.
    pub(crate) normal: Vec3,
    /// The normal it is shaded by: the geometric one, or a smoother surface's
    /// the geometry approximates.
    pub(crate) shading: Vec3,
    /// Which instance of a crowd the ray met, for one leaf or blade of many
    /// to differ from the rest; `0` for a shape that is one thing.
    pub(crate) mark: u32,
    /// How far along its instance, root to tip, the ray met it.
    pub(crate) along: f64,
}

impl Hit {
    /// A hit on a surface shaded by its own geometry.
    pub(crate) const fn plain(t: f64, normal: Vec3) -> Self {
        Self {
            t,
            normal,
            shading: normal,
            mark: 0,
            along: 0.0,
        }
    }
}

/// What the shapes of a scene share: the faces its hulls are cut by and the
/// grids its land and sea are traced over.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Geometry<'a> {
    pub(crate) faces: &'a [Face],
    pub(crate) fields: &'a [Heightfield],
}

/// One face of a convex hull in the hull's own frame: the points `p` with
/// `normal · p <= offset` are on its inner side.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) struct Face {
    pub(crate) normal: Vec3,
    pub(crate) offset: f64,
}

/// An axis-aligned box.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) struct Aabb {
    pub(crate) min: Vec3,
    pub(crate) max: Vec3,
}

impl Aabb {
    pub(crate) const EMPTY: Self = Self {
        min: Vec3::splat(f64::INFINITY),
        max: Vec3::splat(f64::NEG_INFINITY),
    };

    pub(crate) fn around(centre: Vec3, reach: f64) -> Self {
        Self {
            min: centre - Vec3::splat(reach),
            max: centre + Vec3::splat(reach),
        }
    }

    /// This box grown by a sliver of its own size, so a ray grazing a face
    /// exactly, or one rounding nudges outside it, is still let in.
    pub(crate) fn padded(self) -> Self {
        let size = self
            .min
            .x
            .abs()
            .max(self.min.y.abs())
            .max(self.min.z.abs())
            .max(self.max.x.abs().max(self.max.y.abs()).max(self.max.z.abs()));
        let pad = Vec3::splat(1e-9 * (1.0 + size));
        Self {
            min: self.min - pad,
            max: self.max + pad,
        }
    }

    pub(crate) fn union(self, other: Self) -> Self {
        Self {
            min: self.min.min(other.min),
            max: self.max.max(other.max),
        }
    }

    pub(crate) fn including(self, point: Vec3) -> Self {
        Self {
            min: self.min.min(point),
            max: self.max.max(point),
        }
    }

    pub(crate) fn centre(&self) -> Vec3 {
        (self.min + self.max) * 0.5
    }

    /// Half the surface area: what the chance a ray crosses the box goes as.
    pub(crate) fn half_area(&self) -> f64 {
        let size = (self.max - self.min).max(Vec3::ZERO);
        size.x * size.y + size.y * size.z + size.z * size.x
    }

    /// Whether the ray with [`reciprocal`] direction `inverse` crosses the
    /// box nearer than `reach`, and if so where it enters.
    pub(crate) fn entry(&self, ray: &Ray, inverse: Vec3, reach: f64) -> Option<f64> {
        self.span(ray, inverse, reach).map(|(enter, _)| enter)
    }

    /// Where the ray with [`reciprocal`] direction `inverse` enters the box
    /// and leaves it, or reaches `reach` first, if it crosses it at all
    /// nearer than that.
    pub(crate) fn span(&self, ray: &Ray, inverse: Vec3, reach: f64) -> Option<(f64, f64)> {
        let (x0, x1) = slab(self.min.x, self.max.x, ray.origin.x, inverse.x);
        let (y0, y1) = slab(self.min.y, self.max.y, ray.origin.y, inverse.y);
        let (z0, z1) = slab(self.min.z, self.max.z, ray.origin.z, inverse.z);
        let enter = x0.max(y0).max(z0).max(0.0);
        let leave = x1.min(y1).min(z1).min(reach);
        (enter <= leave).then_some((enter, leave))
    }
}

/// Where a ray enters and leaves one pair of a box's parallel faces.
fn slab(min: f64, max: f64, origin: f64, inverse: f64) -> (f64, f64) {
    let (a, b) = ((min - origin) * inverse, (max - origin) * inverse);
    (a.min(b), a.max(b))
}

/// The reciprocal of each component of `dir`, a component too near nought to
/// invert taken as a sliver of its sign: the result is always finite, so a
/// slab test never meets the `0 · ∞` a ray parallel to a face would make.
pub(crate) fn reciprocal(dir: Vec3) -> Vec3 {
    let invert = |d: f64| {
        1.0 / if d.abs() < 1e-12 {
            1e-12f64.copysign(d)
        } else {
            d
        }
    };
    Vec3::new(invert(dir.x), invert(dir.y), invert(dir.z))
}

/// The shapes a scene can hold.
#[derive(Clone, Debug)]
pub(crate) enum Shape {
    Sphere {
        centre: Vec3,
        radius: f64,
    },
    /// The upper half of a sphere's shell, open beneath: a dome, seen from
    /// within as from without.
    Dome {
        centre: Vec3,
        radius: f64,
    },
    /// The points `p` with `normal · p <= offset`: the ground, or the sea.
    Plane {
        normal: Vec3,
        offset: f64,
    },
    /// A rectangle `corner + a * edge_u + b * edge_v` for `a, b` in
    /// `0.0..=1.0`, the two edges at right angles, facing `edge_u × edge_v`.
    Quad {
        corner: Vec3,
        edge_u: Vec3,
        edge_v: Vec3,
    },
    /// A convex solid bounded by the scene's faces `first..first + count`,
    /// lying within `extent` in its own frame.
    Hull {
        pose: Pose,
        first: u32,
        count: u32,
        extent: Aabb,
    },
    /// A capped frustum of a cone about the local y axis, from `0` up to
    /// `height`; a cylinder when both radii agree.
    Frustum {
        pose: Pose,
        bottom: f64,
        top: f64,
        height: f64,
    },
    /// A ring about the local y axis: a tube of radius `minor` swept round a
    /// circle of radius `major`, larger than it; only the arc within the
    /// angle whose cosine is `arc` of the local −z axis, so `-1.0` for the
    /// whole ring and `0.0` for an arch.
    Torus {
        pose: Pose,
        major: f64,
        minor: f64,
        arc: f64,
    },
    /// The scene's height grid `field`: land, or the sea.
    Land {
        field: u32,
    },
    /// A crown of leaves.
    Crown(Crown),
    /// A lawn of grass.
    Lawn(Lawn),
}

impl Shape {
    /// The box this shape lies within, or `None` for one without end.
    pub(crate) fn bounds(&self, geometry: Geometry<'_>) -> Option<Aabb> {
        match *self {
            Self::Sphere { centre, radius } => Some(Aabb::around(centre, radius)),
            Self::Dome { centre, radius } => Some(Aabb {
                min: centre - Vec3::new(radius, 0.0, radius),
                max: centre + Vec3::splat(radius),
            }),
            Self::Plane { .. } => None,
            Self::Quad {
                corner,
                edge_u,
                edge_v,
            } => {
                let corners = [
                    corner,
                    corner + edge_u,
                    corner + edge_v,
                    corner + edge_u + edge_v,
                ];
                let bounds = corners.into_iter().fold(Aabb::EMPTY, Aabb::including);
                // Flat: pad it so the slab test never sees a box of no depth.
                Some(Aabb {
                    min: bounds.min - Vec3::splat(1e-6),
                    max: bounds.max + Vec3::splat(1e-6),
                })
            }
            Self::Hull { pose, extent, .. } => Some(posed_box(&pose, extent.min, extent.max)),
            Self::Frustum {
                pose,
                bottom,
                top,
                height,
            } => {
                let radius = bottom.max(top);
                Some(posed_box(
                    &pose,
                    Vec3::new(-radius, 0.0, -radius),
                    Vec3::new(radius, height, radius),
                ))
            }
            Self::Torus {
                pose, major, minor, ..
            } => {
                let reach = major + minor;
                Some(posed_box(
                    &pose,
                    Vec3::new(-reach, -minor, -reach),
                    Vec3::new(reach, minor, reach),
                ))
            }
            Self::Land { field } => geometry
                .fields
                .get(usize::try_from(field).ok()?)
                .and_then(Heightfield::bounds),
            Self::Crown(ref crown) => Some(crown.bounds()),
            Self::Lawn(ref lawn) => Some(lawn.bounds()),
        }
    }

    /// The nearest place in `(near, far)` along `ray` where it meets this
    /// shape.
    pub(crate) fn intersect(
        &self,
        ray: &Ray,
        near: f64,
        far: f64,
        geometry: Geometry<'_>,
    ) -> Option<Hit> {
        match *self {
            Self::Sphere { centre, radius } => sphere(ray, centre, radius, near, far),
            Self::Dome { centre, radius } => dome(ray, centre, radius, near, far),
            Self::Plane { normal, offset } => plane(ray, normal, offset, near, far),
            Self::Quad {
                corner,
                edge_u,
                edge_v,
            } => quad(ray, corner, edge_u, edge_v, near, far),
            Self::Hull {
                pose, first, count, ..
            } => {
                let start = usize::try_from(first).ok()?;
                let end = start.checked_add(usize::try_from(count).ok()?)?;
                hull(ray, &pose, geometry.faces.get(start..end)?, near, far)
            }
            Self::Frustum {
                pose,
                bottom,
                top,
                height,
            } => frustum(ray, &pose, (bottom, top, height), near, far),
            Self::Torus {
                pose,
                major,
                minor,
                arc,
            } => torus(ray, &pose, (major, minor, arc), near, far),
            Self::Land { field } => geometry
                .fields
                .get(usize::try_from(field).ok()?)?
                .intersect(ray, near, far),
            Self::Crown(ref crown) => crown.intersect(ray, near, far),
            Self::Lawn(ref lawn) => lawn.intersect(ray, near, far, geometry),
        }
    }

    /// Whether this shape stands in the way of light bound for another
    /// surface: a lawn's blades are too fine to shadow one another, and are
    /// shaded as though they did.
    pub(crate) const fn casts_shadow(&self) -> bool {
        !matches!(self, Self::Lawn(_))
    }
}

/// The world box around the local box `min..max` placed at `pose`.
fn posed_box(pose: &Pose, min: Vec3, max: Vec3) -> Aabb {
    let mut bounds = Aabb::EMPTY;
    for corner in 0..8u8 {
        let local = Vec3::new(
            if corner & 1 == 0 { min.x } else { max.x },
            if corner & 2 == 0 { min.y } else { max.y },
            if corner & 4 == 0 { min.z } else { max.z },
        );
        bounds = bounds.including(pose.at + pose.frame.to_world(local));
    }
    bounds
}

/// The roots of `a t² + 2 half_b t + c`, nearer first, computed so neither
/// suffers the cancellation of the schoolbook formula.
pub(crate) fn quadratic(a: f64, half_b: f64, c: f64) -> Option<(f64, f64)> {
    if a.abs() < 1e-14 {
        if half_b.abs() < 1e-300 {
            return None;
        }
        let only = -c / (2.0 * half_b);
        return Some((only, only));
    }
    let disc = half_b * half_b - a * c;
    if disc < 0.0 {
        return None;
    }
    let q = -(half_b + mathf::sqrt(disc).copysign(half_b));
    let (first, second) = (q / a, if q == 0.0 { 0.0 } else { c / q });
    Some((first.min(second), first.max(second)))
}

/// Where the unit-direction `ray`'s line crosses the sphere of `radius`
/// about `centre`, the nearer first.
fn sphere_roots(ray: &Ray, centre: Vec3, radius: f64) -> Option<[f64; 2]> {
    let to = ray.origin - centre;
    let b = to.dot(ray.dir);
    // The squared distance from the centre to the ray's line, taken from the
    // perpendicular itself so a distant sphere keeps its precision.
    let perpendicular = to - ray.dir * b;
    let disc = radius * radius - perpendicular.dot(perpendicular);
    if disc < 0.0 {
        return None;
    }
    let c = to.dot(to) - radius * radius;
    let q = -(b + mathf::sqrt(disc).copysign(b));
    let (first, second) = (q, if q == 0.0 { 0.0 } else { c / q });
    Some([first.min(second), first.max(second)])
}

fn sphere(ray: &Ray, centre: Vec3, radius: f64, near: f64, far: f64) -> Option<Hit> {
    let t = sphere_roots(ray, centre, radius)?
        .into_iter()
        .find(|t| *t > near && *t < far)?;
    Some(Hit::plain(t, (ray.at(t) - centre) / radius))
}

fn dome(ray: &Ray, centre: Vec3, radius: f64, near: f64, far: f64) -> Option<Hit> {
    // The nearer root on the upper half; failing that, the farther, the
    // dome's inside seen through its open base or over its rim.
    sphere_roots(ray, centre, radius)?
        .into_iter()
        .filter(|t| *t > near && *t < far)
        .find(|t| ray.at(*t).y >= centre.y)
        .map(|t| Hit::plain(t, (ray.at(t) - centre) / radius))
}

fn plane(ray: &Ray, normal: Vec3, offset: f64, near: f64, far: f64) -> Option<Hit> {
    let facing = normal.dot(ray.dir);
    if facing.abs() < 1e-12 {
        return None;
    }
    let t = (offset - normal.dot(ray.origin)) / facing;
    (t > near && t < far).then_some(Hit::plain(t, normal))
}

fn quad(ray: &Ray, corner: Vec3, edge_u: Vec3, edge_v: Vec3, near: f64, far: f64) -> Option<Hit> {
    let normal = edge_u.cross(edge_v).normalized();
    let facing = normal.dot(ray.dir);
    if facing.abs() < 1e-12 {
        return None;
    }
    let t = normal.dot(corner - ray.origin) / facing;
    if !(t > near && t < far) {
        return None;
    }
    let offset = ray.at(t) - corner;
    let a = offset.dot(edge_u) / edge_u.dot(edge_u);
    let b = offset.dot(edge_v) / edge_v.dot(edge_v);
    ((0.0..=1.0).contains(&a) && (0.0..=1.0).contains(&b)).then_some(Hit::plain(t, normal))
}

fn hull(ray: &Ray, pose: &Pose, faces: &[Face], near: f64, far: f64) -> Option<Hit> {
    let local = ray.to_local(pose);
    let (mut enter, mut leave) = (f64::NEG_INFINITY, f64::INFINITY);
    let (mut entering, mut leaving) = (Vec3::ZERO, Vec3::ZERO);
    for face in faces {
        let facing = face.normal.dot(local.dir);
        let depth = face.offset - face.normal.dot(local.origin);
        if facing.abs() < 1e-14 {
            if depth < 0.0 {
                return None;
            }
            continue;
        }
        let t = depth / facing;
        if facing < 0.0 {
            if t > enter {
                enter = t;
                entering = face.normal;
            }
        } else if t < leave {
            leave = t;
            leaving = face.normal;
        }
        if enter > leave {
            return None;
        }
    }
    let (t, normal) = if enter > near {
        (enter, entering)
    } else {
        (leave, leaving)
    };
    (t > near && t < far).then(|| Hit::plain(t, pose.frame.to_world(normal)))
}

fn frustum(
    ray: &Ray,
    pose: &Pose,
    (bottom, top, height): (f64, f64, f64),
    near: f64,
    far: f64,
) -> Option<Hit> {
    let Ray { origin, dir } = ray.to_local(pose);
    let slope = (top - bottom) / height;
    let radius_at = |up: f64| bottom + slope * up;
    let mut best: Option<(f64, Vec3)> = None;
    let mut consider = |t: f64, normal: Vec3| {
        if t > near && t < far && best.is_none_or(|(held, _)| t < held) {
            best = Some((t, normal));
        }
    };
    let origin_radius = radius_at(origin.y);
    let a = dir.x * dir.x + dir.z * dir.z - slope * slope * dir.y * dir.y;
    let half_b = origin.x * dir.x + origin.z * dir.z - slope * dir.y * origin_radius;
    let c = origin.x * origin.x + origin.z * origin.z - origin_radius * origin_radius;
    if let Some((first, second)) = quadratic(a, half_b, c) {
        for t in [first, second] {
            let point = origin + dir * t;
            if (0.0..=height).contains(&point.y) {
                consider(t, Vec3::new(point.x, -slope * radius_at(point.y), point.z));
            }
        }
    }
    if dir.y.abs() > 1e-14 {
        for (level, radius, normal) in [(0.0, bottom, -Vec3::UP), (height, top, Vec3::UP)] {
            let t = (level - origin.y) / dir.y;
            let point = origin + dir * t;
            if point.x * point.x + point.z * point.z <= radius * radius {
                consider(t, normal);
            }
        }
    }
    let (t, normal) = best?;
    Some(Hit::plain(t, pose.frame.to_world(normal).normalized()))
}

fn torus(
    ray: &Ray,
    pose: &Pose,
    (major, minor, arc): (f64, f64, f64),
    near: f64,
    far: f64,
) -> Option<Hit> {
    let Ray { origin, dir } = ray.to_local(pose);
    // Only the stretch of the ray within the torus's bounding sphere can meet
    // it; starting the quartic there keeps its coefficients the torus's size.
    // The sphere is padded, for the torus touches it all round its outer
    // equator, and a root at the very start of the stretch would be passed.
    let reach = (major + minor) * (1.0 + 1e-6);
    let along = origin.dot(dir);
    let perpendicular = origin - dir * along;
    let disc = reach * reach - perpendicular.dot(perpendicular);
    if disc < 0.0 {
        return None;
    }
    let half = mathf::sqrt(disc);
    let mut start = (-along - half).max(near);
    let mut end = (-along + half).min(far);
    // The tube lies within the slab `|y| <= minor`: most rays near a slender
    // ring cross that for far less of their length, or not at all.
    if dir.y.abs() > 1e-12 {
        let (low, high) = ((-minor - origin.y) / dir.y, (minor - origin.y) / dir.y);
        start = start.max(low.min(high) - 1e-9);
        end = end.min(low.max(high) + 1e-9);
    } else if origin.y.abs() > minor {
        return None;
    }
    if start >= end {
        return None;
    }
    let from = origin + dir * start;
    let (squared, facing) = (from.dot(from), from.dot(dir));
    let (rr, big) = (major * major + minor * minor, major * major);
    let small = minor * minor;
    let quartic = [
        4.0 * facing,
        4.0 * facing * facing + 2.0 * squared - 2.0 * rr + 4.0 * big * dir.y * dir.y,
        4.0 * facing * squared - 4.0 * rr * facing + 8.0 * big * from.y * dir.y,
        squared * squared - 2.0 * rr * squared
            + (big - small) * (big - small)
            + 4.0 * big * from.y * from.y,
    ];
    // The quartic has four roots at most, so an arc needs at most four
    // tries to find the first that falls on it.
    let mut after = 0.0;
    for _ in 0..4 {
        let u = first_root(quartic, after, end - start)?;
        let t = start + u;
        if t <= near || t >= far {
            return None;
        }
        let point = origin + dir * t;
        let around = Vec3::new(point.x, 0.0, point.z).normalized();
        if -around.z >= arc {
            return Some(Hit::plain(
                t,
                pose.frame.to_world((point - around * major).normalized()),
            ));
        }
        after = u + 1e-9 * (1.0 + u);
    }
    None
}

/// The first root of the monic quartic `u⁴ + c₃u³ + c₂u² + c₁u + c₀` in
/// `(low, high]`.
///
/// The quartic's turning points, the roots of its derivative, cut the
/// interval into pieces on which it is monotone, so each piece holds at most
/// one root and a change of sign between a piece's ends proves one. The first
/// such piece is refined by Newton's method held within its bracket. A root
/// it finds is always a root; a tangent graze, whose two roots coincide, is
/// the one kind it may pass by.
pub(crate) fn first_root([c3, c2, c1, c0]: [f64; 4], low: f64, high: f64) -> Option<f64> {
    let value = |u: f64| (((u + c3) * u + c2) * u + c1) * u + c0;
    let (turns, count) = cubic_roots(0.75 * c3, 0.5 * c2, 0.25 * c1);
    let mut left = low;
    let mut at_left = value(low);
    for right in turns
        .into_iter()
        .take(count)
        .filter(|turn| *turn > low && *turn < high)
        .chain([high])
    {
        let at_right = value(right);
        if at_right == 0.0 {
            return Some(right);
        }
        if at_left * at_right < 0.0 {
            return Some(refine([c3, c2, c1, c0], (left, at_left), right));
        }
        left = right;
        at_left = at_right;
    }
    None
}

/// The quartic's root in the bracket from `(low, at_low)` to `high`, whose
/// ends differ in sign.
fn refine([c3, c2, c1, c0]: [f64; 4], (low, at_low): (f64, f64), high: f64) -> f64 {
    let (mut negative, mut positive) = if at_low < 0.0 {
        (low, high)
    } else {
        (high, low)
    };
    let mut u = low.midpoint(high);
    for _ in 0..64 {
        let value = (((u + c3) * u + c2) * u + c1) * u + c0;
        let slope = ((4.0 * u + 3.0 * c3) * u + 2.0 * c2) * u + c1;
        if value < 0.0 {
            negative = u;
        } else {
            positive = u;
        }
        let newton = u - value / slope;
        let within = newton > negative.min(positive) && newton < negative.max(positive);
        let next = if slope != 0.0 && within {
            newton
        } else {
            negative.midpoint(positive)
        };
        if (next - u).abs() <= 1e-13 * (1.0 + u.abs()) {
            return next;
        }
        u = next;
    }
    u
}

/// The real roots of the monic cubic `x³ + c₂x² + c₁x + c₀` in ascending
/// order, and how many there are.
fn cubic_roots(c2: f64, c1: f64, c0: f64) -> ([f64; 3], usize) {
    let third = c2 / 3.0;
    // Depressed, `y³ + p y + q` in `y = x + c₂ / 3`.
    let p = c1 - c2 * third;
    let q = c0 + third * (2.0 * third * third - c1);
    let (half_q, third_p) = (0.5 * q, p / 3.0);
    let disc = half_q * half_q + third_p * third_p * third_p;
    if disc > 0.0 {
        // One real root; the larger of Cardano's terms first, so its partner
        // comes from a division rather than a cancelling subtraction.
        let big = cbrt(-half_q - mathf::sqrt(disc).copysign(half_q));
        let y = if big == 0.0 { 0.0 } else { big - third_p / big };
        return ([y - third, 0.0, 0.0], 1);
    }
    let radius = mathf::sqrt(-third_p);
    if radius == 0.0 {
        return ([-third, 0.0, 0.0], 1);
    }
    let angle = mathf::acos(-half_q / (radius * radius * radius)) / 3.0;
    // With `angle` in `0..=π/3`, the third turn is the least and the first the
    // greatest.
    let roots = [2.0, 1.0, 0.0].map(|k| 2.0 * radius * mathf::cos(angle - TAU * k / 3.0) - third);
    (roots, 3)
}

/// The real cube root.
fn cbrt(x: f64) -> f64 {
    if x == 0.0 {
        return 0.0;
    }
    let magnitude = x.abs();
    let guess = mathf::exp(mathf::ln(magnitude) / 3.0);
    let root = guess - (guess * guess * guess - magnitude) / (3.0 * guess * guess);
    root.copysign(x)
}

#[cfg(test)]
#[path = "shape_tests.rs"]
mod tests;
