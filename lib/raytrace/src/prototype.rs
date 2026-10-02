//! Prototypes: a tree, a rock or a bush built once in its own frame and
//! placed as many times as a scene wants it.
//!
//! A prototype is a list of parts — tapering limbs with rounded ends, flat
//! leaves cut to an outline, and the triangles of a mesh — and a hierarchy
//! over them, so a ray entering an instance tests the few parts along its
//! path. Its parts are stored in single precision, since a forest's worth of
//! leaves is what a scene holds most of; they are met in double.

use alloc::vec::Vec;

use tairix_util::mathf;

use crate::bvh::{Builder, Bvh, Walk};
use crate::leaf::Outline;
use crate::shape::{Aabb, Hit};
use crate::vector::{Ray, Vec3};

/// A limb: a tube tapering from radius `radii[0]` at `a` to `radii[1]` at
/// `b`, each end rounded by a sphere of its radius, so a chain of them bends
/// without a seam.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Tube {
    pub(crate) a: [f32; 3],
    pub(crate) b: [f32; 3],
    pub(crate) radii: [f32; 2],
    /// How far along its stem each end lies, for the bark's pattern.
    pub(crate) stem: [f32; 2],
    pub(crate) material: u16,
    pub(crate) key: u32,
}

/// A leaf: the part of the plane through `base` facing `normal` that its
/// outline covers, its midrib running `length` along the unit `axis` and its
/// greatest half-width `width`; `fold` tilts its halves up from the midrib.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Blade {
    pub(crate) base: [f32; 3],
    pub(crate) normal: [f32; 3],
    pub(crate) axis: [f32; 3],
    pub(crate) length: f32,
    pub(crate) width: f32,
    pub(crate) outline: Outline,
    pub(crate) fold: f32,
    pub(crate) material: u16,
    pub(crate) key: u32,
}

/// A triangle of the prototype's vertices, shaded by their normals blended.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Facet {
    pub(crate) corners: [u32; 3],
    pub(crate) material: u16,
}

/// One part of a prototype.
#[derive(Copy, Clone, Debug)]
pub(crate) enum Part {
    Tube(Tube),
    Leaf(Blade),
    Facet(Facet),
}

/// A shape built once, in its own frame.
#[derive(Clone, Debug)]
pub(crate) struct Prototype {
    parts: Vec<Part>,
    vertices: Vec<[f32; 3]>,
    normals: Vec<[f32; 3]>,
    bvh: Bvh,
    bounds: Aabb,
}

/// A point held in single precision, back in double.
pub(crate) fn point(at: [f32; 3]) -> Vec3 {
    Vec3::new(f64::from(at[0]), f64::from(at[1]), f64::from(at[2]))
}

/// A point in the single precision a part holds.
pub(crate) fn stored(at: Vec3) -> [f32; 3] {
    [single(at.x), single(at.y), single(at.z)]
}

/// A value in single precision.
pub(crate) fn single(value: f64) -> f32 {
    #[allow(
        clippy::cast_possible_truncation,
        reason = "a prototype's parts are millimetres across metres, well within single precision"
    )]
    {
        value as f32
    }
}

/// A prototype whose hierarchy is still being built.
#[derive(Debug)]
pub(crate) struct Building {
    parts: Vec<Part>,
    vertices: Vec<[f32; 3]>,
    normals: Vec<[f32; 3]>,
    builder: Builder,
    bounds: Aabb,
}

/// Parts' worth of a prototype's hierarchy built in one step.
pub(crate) const BUILD_UNIT: usize = 24_000;

impl Building {
    /// Build about `budget` parts' worth more of the hierarchy; whether it
    /// is whole.
    pub(crate) fn step(&mut self, budget: usize) -> bool {
        self.builder.step(budget)
    }

    /// The prototype, its hierarchy built whole at once.
    #[cfg(test)]
    pub(crate) fn whole(mut self) -> Prototype {
        self.step(usize::MAX);
        self.finish()
    }

    /// The prototype, once its hierarchy is whole.
    pub(crate) fn finish(self) -> Prototype {
        Prototype {
            parts: self.parts,
            vertices: self.vertices,
            normals: self.normals,
            bvh: self.builder.finish(),
            bounds: self.bounds,
        }
    }
}

impl Prototype {
    /// The prototype of `parts`, whose facets index `vertices` and their
    /// `normals`, built at once; `None` when the heap will not hold its
    /// hierarchy, or a facet names a vertex it lacks.
    #[cfg(test)]
    pub(crate) fn new(
        parts: Vec<Part>,
        vertices: Vec<[f32; 3]>,
        normals: Vec<[f32; 3]>,
    ) -> Option<Self> {
        Some(Self::building(parts, vertices, normals)?.whole())
    }

    /// The prototype of `parts`, whose facets index `vertices` and their
    /// `normals`, its hierarchy to build step by step; `None` when the heap
    /// will not hold it, or a facet names a vertex it lacks.
    pub(crate) fn building(
        parts: Vec<Part>,
        vertices: Vec<[f32; 3]>,
        normals: Vec<[f32; 3]>,
    ) -> Option<Building> {
        if normals.len() != vertices.len() {
            return None;
        }
        let mut boxes = Vec::new();
        if boxes.try_reserve_exact(parts.len()).is_err() {
            return None;
        }
        let mut bounds = Aabb::EMPTY;
        for (index, part) in parts.iter().enumerate() {
            let extent = part_bounds(part, &vertices)?;
            bounds = bounds.union(extent);
            boxes.push((u32::try_from(index).ok()?, extent));
        }
        let builder = Builder::new(&boxes)?;
        // A prototype of no parts is a point, so every box built on it is one.
        if bounds.min.x > bounds.max.x {
            bounds = Aabb::around(Vec3::ZERO, 1e-3);
        }
        Some(Building {
            parts,
            vertices,
            normals,
            builder,
            bounds,
        })
    }

    /// The box the prototype lies in, in its own frame.
    pub(crate) const fn bounds(&self) -> Aabb {
        self.bounds
    }

    /// The parts it is made of.
    #[cfg(test)]
    pub(crate) fn parts(&self) -> &[Part] {
        &self.parts
    }

    /// The nearest part `ray`, given in the prototype's frame, meets within
    /// `(near, far)`.
    pub(crate) fn intersect(&self, ray: &Ray, near: f64, far: f64) -> Option<Hit> {
        let mut best: Option<Hit> = None;
        self.bvh.walk(ray, far, |index, reach| {
            match self
                .parts
                .get(index as usize)
                .and_then(|part| self.meet(part, ray, (near, reach)))
            {
                Some(hit) => {
                    let t = hit.t;
                    best = Some(hit);
                    Walk::Within(t)
                }
                None => Walk::Within(reach),
            }
        });
        best
    }

    /// Whether `ray` meets any part within `(near, far)`.
    pub(crate) fn occludes(&self, ray: &Ray, near: f64, far: f64) -> bool {
        let mut blocked = false;
        self.bvh.walk(ray, far, |index, reach| {
            let met = self
                .parts
                .get(index as usize)
                .and_then(|part| self.meet(part, ray, (near, reach)))
                .is_some();
            if met {
                blocked = true;
                Walk::Stop
            } else {
                Walk::Within(reach)
            }
        });
        blocked
    }

    fn meet(&self, part: &Part, ray: &Ray, span: (f64, f64)) -> Option<Hit> {
        match part {
            Part::Tube(tube) => meet_tube(tube, ray, span),
            Part::Leaf(blade) => meet_blade(blade, ray, span),
            Part::Facet(facet) => self.meet_facet(facet, ray, span),
        }
    }

    /// Where `ray` meets `facet` within `(near, far)` (Möller and Trumbore,
    /// "Fast, minimum storage ray/triangle intersection", 1997).
    #[allow(
        clippy::many_single_char_names,
        reason = "the paper's symbols, as it writes them"
    )]
    fn meet_facet(&self, facet: &Facet, ray: &Ray, (near, far): (f64, f64)) -> Option<Hit> {
        let corner = |at: usize| {
            let index = *facet.corners.get(at)? as usize;
            Some((
                point(*self.vertices.get(index)?),
                point(*self.normals.get(index)?),
            ))
        };
        let ((v0, n0), (v1, n1), (v2, n2)) = (corner(0)?, corner(1)?, corner(2)?);
        let (e1, e2) = (v1 - v0, v2 - v0);
        let p = ray.dir.cross(e2);
        let det = e1.dot(p);
        if det.abs() < 1e-18 {
            return None;
        }
        let inverse = 1.0 / det;
        let s = ray.origin - v0;
        let u = s.dot(p) * inverse;
        if !(0.0..=1.0).contains(&u) {
            return None;
        }
        let q = s.cross(e1);
        let v = ray.dir.dot(q) * inverse;
        if v < 0.0 || u + v > 1.0 {
            return None;
        }
        let t = e2.dot(q) * inverse;
        if !(t > near && t < far) {
            return None;
        }
        // Out is the way the corners' normals face, however the corners wind.
        let blended = (n0 * (1.0 - u - v) + n1 * u + n2 * v).normalized();
        let face = e1.cross(e2).normalized();
        let normal = if face.dot(blended) < 0.0 { -face } else { face };
        Some(Hit {
            t,
            normal,
            shading: blended,
            mark: 0,
            along: 0.0,
            uv: (u, v),
            girth: 0.0,
            material: Some(u32::from(facet.material)),
            tangent: Vec3::ZERO,
        })
    }
}

/// The box a part lies in.
fn part_bounds(part: &Part, vertices: &[[f32; 3]]) -> Option<Aabb> {
    Some(match part {
        Part::Tube(tube) => {
            let (a, b) = (point(tube.a), point(tube.b));
            let (ra, rb) = (f64::from(tube.radii[0]), f64::from(tube.radii[1]));
            Aabb::around(a, ra).union(Aabb::around(b, rb))
        }
        Part::Leaf(blade) => {
            let (base, axis, normal) = (point(blade.base), point(blade.axis), point(blade.normal));
            let across = normal.cross(axis) * f64::from(blade.width);
            let tip = base + axis * f64::from(blade.length);
            let lift = normal * (0.25 * f64::from(blade.fold) * f64::from(blade.width));
            [base + across, base - across, tip + across, tip - across]
                .into_iter()
                .fold(Aabb::EMPTY, Aabb::including)
                .union(Aabb::around(base + lift, 1e-4))
                .padded()
        }
        Part::Facet(facet) => {
            let mut bounds = Aabb::EMPTY;
            for &corner in &facet.corners {
                bounds = bounds.including(point(*vertices.get(corner as usize)?));
            }
            bounds.padded()
        }
    })
}

/// Where `ray` meets a limb within `(near, far)`: the tapering body between
/// its two spheres, or the sphere capping either end (Quílez, "Rounded cone
/// – intersection", 2019).
#[allow(
    clippy::many_single_char_names,
    reason = "the derivation's symbols, as it writes them"
)]
fn meet_tube(tube: &Tube, ray: &Ray, (near, far): (f64, f64)) -> Option<Hit> {
    let (a, b) = (point(tube.a), point(tube.b));
    let (ra, rb) = (f64::from(tube.radii[0]), f64::from(tube.radii[1]));
    let ba = b - a;
    let (oa, ob) = (ray.origin - a, ray.origin - b);
    let rr = ra - rb;
    let m0 = ba.dot(ba);
    let (m1, m2, m3) = (ba.dot(oa), ba.dot(ray.dir), ray.dir.dot(oa));
    let m5 = oa.dot(oa);
    let d2 = m0 - rr * rr;
    let k2 = d2 - m2 * m2;
    let k1 = d2 * m3 - m1 * m2 + m2 * rr * ra;
    let k0 = d2 * m5 - m1 * m1 + m1 * rr * ra * 2.0 - m0 * ra * ra;
    let h = k1 * k1 - k0 * k2;
    if h < 0.0 {
        return None;
    }
    let stem = |along: f64| {
        f64::from(tube.stem[0]) + (f64::from(tube.stem[1]) - f64::from(tube.stem[0])) * along
    };
    if k2.abs() > 1e-18 {
        let t = (-mathf::sqrt(h) - k1) / k2;
        let y = m1 - ra * rr + t * m2;
        if y > 0.0 && y < d2 {
            if !(t > near && t < far) {
                return None;
            }
            let normal = ((oa + ray.dir * t) * d2 - ba * y).normalized();
            let along = y / d2;
            return Some(limb_hit(t, normal, (stem(along), along), tube));
        }
    }
    let mut best: Option<Hit> = None;
    for (centre, radius, to, at) in [(a, ra, oa, 0.0), (b, rb, ob, 1.0)] {
        let dot = ray.dir.dot(to);
        let reach = dot * dot - to.dot(to) + radius * radius;
        if reach <= 0.0 || radius <= 0.0 {
            continue;
        }
        let t = -dot - mathf::sqrt(reach);
        if t > near && t < far && best.is_none_or(|held| t < held.t) {
            let normal = (ray.at(t) - centre) / radius;
            best = Some(limb_hit(t, normal, (stem(at), at), tube));
        }
    }
    best
}

/// A hit on `tube` at `t` with outward `normal`, `stem` along its stem and
/// `along` of the way from its first end to its second: its surface
/// coordinates are the distance along the stem and the distance round it.
fn limb_hit(t: f64, normal: Vec3, (stem, along): (f64, f64), tube: &Tube) -> Hit {
    let axis = (point(tube.b) - point(tube.a)).normalized();
    // Round the limb from a side the world fixes, so a stem's segments agree
    // on where its bark's pattern starts.
    let reference = if axis.x.abs() < 0.9 {
        Vec3::new(1.0, 0.0, 0.0)
    } else {
        Vec3::new(0.0, 0.0, 1.0)
    };
    let first = (reference - axis * reference.dot(axis)).normalized();
    let second = axis.cross(first);
    let angle = mathf::atan2(normal.dot(second), normal.dot(first));
    let radius =
        f64::from(tube.radii[0]) + (f64::from(tube.radii[1]) - f64::from(tube.radii[0])) * along;
    Hit {
        t,
        normal,
        shading: normal,
        mark: tube.key,
        along,
        uv: (stem, angle),
        girth: radius,
        material: Some(u32::from(tube.material)),
        tangent: axis,
    }
}

/// Where `ray` meets a leaf within `(near, far)`.
fn meet_blade(blade: &Blade, ray: &Ray, (near, far): (f64, f64)) -> Option<Hit> {
    let (base, normal, axis) = (point(blade.base), point(blade.normal), point(blade.axis));
    let facing = normal.dot(ray.dir);
    if facing.abs() < 1e-12 {
        return None;
    }
    let t = normal.dot(base - ray.origin) / facing;
    if !(t > near && t < far) {
        return None;
    }
    let offset = ray.at(t) - base;
    let across = normal.cross(axis);
    let (length, width) = (f64::from(blade.length), f64::from(blade.width));
    let u = offset.dot(axis) / length;
    let v = offset.dot(across) / width;
    if !blade.outline.covers(u, v) {
        return None;
    }
    // Each half tilts up from the midrib, so the blade catches the light as
    // a shallow trough does.
    let from = blade.outline.off_midrib(u, v);
    let tilt = f64::from(blade.fold) * from * v.signum();
    let shading =
        (normal - across * tilt + axis * (0.15 * (u - 0.5) * f64::from(blade.fold))).normalized();
    Some(Hit {
        t,
        normal,
        shading,
        mark: blade.key,
        along: u,
        uv: (u, v),
        girth: 0.0,
        material: Some(u32::from(blade.material)),
        tangent: axis,
    })
}

#[cfg(test)]
#[path = "prototype_tests.rs"]
mod tests;
