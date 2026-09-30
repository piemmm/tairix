//! Foliage: a crown of leaves, each its own small ellipse, at most one to a
//! cell of a grid through the crown.
//!
//! A ray walks the cells it crosses in order (Amanatides and Woo, "A Fast
//! Voxel Traversal Algorithm for Ray Tracing", 1987) and meets a cell's leaf
//! where it cuts the leaf's plane within its outline. Every leaf lies wholly
//! within its cell, so the first one met is the nearest, and a shadow ray
//! passes through the gaps the light comes through.
//!
//! Nothing is stored: whether a cell holds a leaf, and where the leaf lies
//! and faces, are hashed from the cell itself.

use tairix_util::mathf;

use crate::grass::FLOWER;
use crate::noise::{cell, hash3, smoothstep};
use crate::sample::{mix32, unit};
use crate::shape::{Aabb, Hit};
use crate::vector::{Frame, Ray, Vec3};

/// The most cells a ray walks through one crown.
const MAX_CELLS: u32 = 768;

/// A crown of leaves.
#[derive(Clone, Debug)]
pub(crate) struct Crown {
    /// The crown's middle, and its radii along the world's axes.
    pub(crate) centre: Vec3,
    pub(crate) radii: Vec3,
    /// The side of a leaf's cell.
    pub(crate) cell: f64,
    /// A leaf's half-length, less than half the cell.
    pub(crate) leaf: f64,
    /// A leaf's breadth over its length.
    pub(crate) breadth: f64,
    /// The share of cells holding a leaf at the crown's surface, and at its
    /// heart.
    pub(crate) surface: f64,
    pub(crate) heart: f64,
    /// How far the leaves turn to face up and out, from `0.0`, any way at
    /// all, to `1.0`.
    pub(crate) lift: f64,
    pub(crate) seed: u32,
}

impl Crown {
    pub(crate) fn bounds(&self) -> Aabb {
        Aabb {
            min: self.centre - self.radii,
            max: self.centre + self.radii,
        }
    }

    /// The nearest leaf `ray` meets within `(near, far)`.
    pub(crate) fn intersect(&self, ray: &Ray, near: f64, far: f64) -> Option<Hit> {
        let (enter, leave) = self.span(ray)?;
        let (start, end) = (enter.max(near), leave.min(far));
        if start >= end {
            return None;
        }
        let base = self.centre - self.radii;
        let first = ray.at(start);
        let lattice = |at: f64, from: f64| cell((at - from) / self.cell);
        let (mut index, mut next, mut delta, mut step) =
            ([0u32; 3], [0.0f64; 3], [0.0f64; 3], [0u32; 3]);
        for axis in 0..3 {
            let (whole, fraction) = lattice(first.along(axis), base.along(axis));
            let dir = ray.dir.along(axis);
            index[axis] = whole;
            if dir.abs() < 1e-12 {
                next[axis] = f64::INFINITY;
                delta[axis] = f64::INFINITY;
            } else {
                let to_wall = if dir > 0.0 { 1.0 - fraction } else { fraction };
                next[axis] = start + to_wall * self.cell / dir.abs();
                delta[axis] = self.cell / dir.abs();
                step[axis] = if dir > 0.0 { 1 } else { u32::MAX };
            }
        }
        let mut t = start;
        for _ in 0..MAX_CELLS {
            let exit = next[0].min(next[1]).min(next[2]);
            if let Some(hit) = self.leaf_in(ray, index, (t, exit.min(end))) {
                return Some(hit);
            }
            if exit >= end {
                return None;
            }
            let axis = if next[0] <= next[1] && next[0] <= next[2] {
                0
            } else if next[1] <= next[2] {
                1
            } else {
                2
            };
            index[axis] = index[axis].wrapping_add(step[axis]);
            next[axis] += delta[axis];
            t = exit;
        }
        None
    }

    /// Where the ray enters and leaves the crown's ellipsoid.
    fn span(&self, ray: &Ray) -> Option<(f64, f64)> {
        let scale = |v: Vec3| Vec3::new(v.x / self.radii.x, v.y / self.radii.y, v.z / self.radii.z);
        let (origin, dir) = (scale(ray.origin - self.centre), scale(ray.dir));
        let a = dir.dot(dir);
        let half_b = origin.dot(dir);
        let c = origin.dot(origin) - 1.0;
        let disc = half_b * half_b - a * c;
        if disc < 0.0 || a <= 0.0 {
            return None;
        }
        let root = mathf::sqrt(disc);
        Some(((-half_b - root) / a, (-half_b + root) / a))
    }

    /// The leaf of cell `index`, if it has one the ray meets within
    /// `(from, to)`.
    fn leaf_in(&self, ray: &Ray, [i, j, k]: [u32; 3], (from, to): (f64, f64)) -> Option<Hit> {
        let key = hash3(i, j, k, self.seed);
        let margin = (0.5 * self.cell - self.leaf).max(0.0);
        let corner = self.centre - self.radii;
        let place = |at: u32, from: f64, salt: u32| {
            from + (f64::from(at) + 0.5) * self.cell
                + margin * (2.0 * unit(mix32(key ^ salt)) - 1.0)
        };
        let middle = Vec3::new(
            place(i, corner.x, 1),
            place(j, corner.y, 2),
            place(k, corner.z, 3),
        );
        let offset = middle - self.centre;
        let scaled = Vec3::new(
            offset.x / self.radii.x,
            offset.y / self.radii.y,
            offset.z / self.radii.z,
        );
        let depth = scaled.length();
        if depth > 1.0 {
            return None;
        }
        let fill = self.heart + (self.surface - self.heart) * smoothstep(0.35, 0.95, depth);
        if unit(key) >= fill {
            return None;
        }
        let outward = Vec3::new(
            scaled.x / self.radii.x,
            scaled.y / self.radii.y,
            scaled.z / self.radii.z,
        )
        .normalized();
        let rise = 2.0 * unit(mix32(key ^ 4)) - 1.0;
        let around = core::f64::consts::TAU * unit(mix32(key ^ 5));
        let level = mathf::sqrt((1.0 - rise * rise).max(0.0));
        let any = Vec3::new(level * mathf::cos(around), rise, level * mathf::sin(around));
        let normal = (any + (outward * 0.6 + Vec3::UP * 0.4) * (2.0 * self.lift)).normalized();
        let facing = normal.dot(ray.dir);
        if facing.abs() < 1e-9 {
            return None;
        }
        let t = normal.dot(middle - ray.origin) / facing;
        if !(t >= from && t <= to) {
            return None;
        }
        let frame = Frame::around(normal);
        let turn = core::f64::consts::TAU * unit(mix32(key ^ 6));
        let length = frame.x * mathf::cos(turn) + frame.y * mathf::sin(turn);
        let width = normal.cross(length);
        let from_middle = ray.at(t) - middle;
        let (lengthwise, crosswise) = (
            from_middle.dot(length) / self.leaf,
            from_middle.dot(width) / (self.leaf * self.breadth),
        );
        (lengthwise * lengthwise + crosswise * crosswise <= 1.0).then_some(Hit {
            t,
            normal,
            shading: normal,
            mark: key & !FLOWER,
            along: depth,
        })
    }
}

#[cfg(test)]
#[path = "foliage_tests.rs"]
mod tests;
