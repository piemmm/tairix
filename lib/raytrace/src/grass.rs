//! Grass: a lawn of blades standing on the ground, a few to every cell of a
//! grid over it, and here and there a flower.
//!
//! A ray walks the cells it crosses in order, only over the stretch where it
//! is low enough to reach a blade, and tests each blade of each cell as the
//! two tapering ribbons it bends through. A blade stands wholly within its
//! cell, so the nearest blade of the first cell met is the nearest of all.
//! Nothing is stored: every blade is hashed from its cell.

use core::f64::consts::TAU;

use tairix_util::mathf;

use crate::heightfield::Heightfield;
use crate::noise::{cell, hash2};
use crate::sample::{mix32, mix64, unit};
use crate::shape::{reciprocal, Aabb, Geometry, Hit};
use crate::vector::{Ray, Vec3};

/// The most cells a ray walks across one lawn.
const MAX_CELLS: u32 = 1024;

/// The bit of a hit's mark set when it is a flower, not a blade.
pub(crate) const FLOWER: u32 = 1 << 31;

/// The widest a flower's head spreads from its blade's tip.
const FLOWER_ROOM: f64 = 0.03;

/// A lawn.
#[derive(Clone, Debug)]
pub(crate) struct Lawn {
    /// The scene's height grid it grows on.
    pub(crate) field: u32,
    /// The corners of the rectangle it covers, in x and z.
    pub(crate) from: (f64, f64),
    pub(crate) to: (f64, f64),
    /// The lowest and highest the ground lies under it.
    pub(crate) floor: f64,
    pub(crate) ceiling: f64,
    /// The side of a cell, and how many blades each holds.
    pub(crate) cell: f64,
    pub(crate) blades: u32,
    /// The shortest and tallest a blade grows.
    pub(crate) height: (f64, f64),
    /// A blade's width at its root.
    pub(crate) width: f64,
    /// How far a blade's tip leans out, over its height.
    pub(crate) lean: f64,
    /// The share of blades ending in a flower.
    pub(crate) flowers: f64,
    pub(crate) seed: u32,
}

/// Where one blade of a cell stands, from the cell's first corner: its root,
/// the way it leans and how far, how tall it grows, and the radius of the
/// flower it bears, if it bears one.
#[derive(Copy, Clone, Debug)]
struct Blade {
    root: (f64, f64),
    toward: (f64, f64),
    lean: f64,
    height: f64,
    flower: Option<f64>,
    key: u32,
}

impl Lawn {
    pub(crate) fn bounds(&self) -> Aabb {
        Aabb {
            min: Vec3::new(self.from.0, self.floor - 0.01, self.from.1),
            max: Vec3::new(self.to.0, self.ceiling + self.height.1, self.to.1),
        }
    }

    /// The highest anything of the lawn stands above the ground: its tallest
    /// blade, and a flower's head over it.
    fn reach(&self) -> f64 {
        self.height.1 + FLOWER_ROOM
    }

    /// Blade `index` of the cell hashed to `cell_key`, leaning `toward`.
    ///
    /// It roots anywhere in its cell and leans no further than the cell's
    /// walls allow, so it stays within the cell the walk tests it in and no
    /// grid shows through the lawn; a flower is borne only where its head
    /// clears the walls too.
    fn blade(&self, cell_key: u32, index: u32, toward: (f64, f64)) -> Blade {
        let key = mix32(cell_key ^ index.wrapping_mul(0x9e37_79b9)) & !FLOWER;
        // Sixteen-bit draws, finer than any blade can show, four to a mix.
        let (first, second) = (
            mix64(u64::from(key)),
            mix64(u64::from(key) ^ 0x9e37_79b9_7f4a_7c15),
        );
        let draw = |bits: u64, at: u32| {
            u32::try_from((bits >> (16 * at)) & 0xffff).map_or(0.0, f64::from) * (1.0 / 65_536.0)
        };
        let (inner, outer) = (self.width, self.cell - self.width);
        let root = (
            inner + (outer - inner) * draw(first, 0),
            inner + (outer - inner) * draw(first, 1),
        );
        // The taller of two draws: more tall blades than short, as a square
        // root would spread them.
        let height =
            self.height.0 + (self.height.1 - self.height.0) * draw(first, 2).max(draw(first, 3));
        let clearance = |at: f64, d: f64| {
            if d > 1e-9 {
                (outer - at) / d
            } else if d < -1e-9 {
                (inner - at) / d
            } else {
                f64::INFINITY
            }
        };
        let lean = (self.lean * height * (0.3 + 0.7 * draw(second, 0)))
            .min(clearance(root.0, toward.0))
            .min(clearance(root.1, toward.1))
            .max(0.0);
        let tip = (root.0 + lean * toward.0, root.1 + lean * toward.1);
        let clear = |at: f64| at >= FLOWER_ROOM && self.cell - at >= FLOWER_ROOM;
        let flower = (draw(second, 1) < self.flowers && clear(tip.0) && clear(tip.1))
            .then(|| (0.012 + 0.014 * draw(second, 2)).min(FLOWER_ROOM));
        Blade {
            root,
            toward,
            lean,
            height,
            flower,
            key,
        }
    }

    /// The nearest blade or flower `ray` meets within `(near, far)`.
    pub(crate) fn intersect(
        &self,
        ray: &Ray,
        near: f64,
        far: f64,
        geometry: Geometry<'_>,
    ) -> Option<Hit> {
        let field = geometry.fields.get(self.field as usize)?;
        let (enter, leave) = self.bounds().span(ray, reciprocal(ray.dir), far)?;
        // Straight to where the ray first comes down within the blades'
        // reach, before which is all air above them, and no further than
        // where it meets the ground, past which every blade is hidden.
        let enter = field.approach(ray, self.reach(), (enter.max(near), leave))?;
        let leave = field
            .intersect(ray, enter, leave)
            .map_or(leave, |ground| ground.t);
        if enter >= leave {
            return None;
        }
        let first = ray.at(enter);
        let ((mut cx, fx), (mut cz, fz)) = (
            cell((first.x - self.from.0) / self.cell),
            cell((first.z - self.from.1) / self.cell),
        );
        let wall = |fraction: f64, dir: f64| {
            if dir.abs() < 1e-12 {
                f64::INFINITY
            } else {
                let to = if dir > 0.0 { 1.0 - fraction } else { fraction };
                enter + to * self.cell / dir.abs()
            }
        };
        let (mut next_x, mut next_z) = (wall(fx, ray.dir.x), wall(fz, ray.dir.z));
        let delta = |dir: f64| {
            if dir.abs() < 1e-12 {
                f64::INFINITY
            } else {
                self.cell / dir.abs()
            }
        };
        let (delta_x, delta_z) = (delta(ray.dir.x), delta(ray.dir.z));
        let step = |dir: f64| if dir > 0.0 { 1 } else { u32::MAX };
        let (step_x, step_z) = (step(ray.dir.x), step(ray.dir.z));
        let mut t = enter;
        for _ in 0..MAX_CELLS {
            let exit = next_x.min(next_z).min(leave);
            if let Some(hit) = self.cell_hit(ray, (cx, cz), (t, exit), field) {
                return Some(hit);
            }
            if exit >= leave {
                return None;
            }
            if next_x <= next_z {
                cx = cx.wrapping_add(step_x);
                next_x += delta_x;
            } else {
                cz = cz.wrapping_add(step_z);
                next_z += delta_z;
            }
            t = exit;
        }
        None
    }

    /// The nearest blade of cell `(cx, cz)` the ray meets within `(from, to)`.
    fn cell_hit(
        &self,
        ray: &Ray,
        (cx, cz): (u32, u32),
        (from, to): (f64, f64),
        field: &Heightfield,
    ) -> Option<Hit> {
        let x0 = self.from.0 + f64::from(cx) * self.cell;
        let z0 = self.from.1 + f64::from(cz) * self.cell;
        // Above every blade of this cell the whole way across it, the ground
        // at its highest: nothing here.
        let lowest = (ray.origin.y + ray.dir.y * from).min(ray.origin.y + ray.dir.y * to);
        let highest = field.highest_over((x0, z0), (x0 + self.cell, z0 + self.cell));
        if lowest > highest + self.reach() {
            return None;
        }
        // The roots lie on the plane of the ground across the cell, three
        // looks at it apart, and never above its highest.
        let (mx, mz) = (x0 + 0.5 * self.cell, z0 + 0.5 * self.cell);
        let half = 0.5 * self.cell;
        let ground = field.height_at(mx, mz);
        let slope_x = (field.height_at(mx + half, mz) - ground) / half;
        let slope_z = (field.height_at(mx, mz + half) - ground) / half;
        let cell_key = hash2(cx, cz, self.seed);
        let mut best: Option<Hit> = None;
        let mut reach = to;
        let flat = ray.dir.x * ray.dir.x + ray.dir.z * ray.dir.z;
        let start = TAU * unit(mix32(cell_key ^ 0x51));
        let mut toward = (mathf::cos(start), mathf::sin(start));
        for index in 0..self.blades {
            let Blade {
                root: (rx, rz),
                toward: (lx, lz),
                lean,
                height,
                flower,
                key,
            } = self.blade(cell_key, index, toward);
            toward = turn_golden(toward);
            let (rx, rz) = (x0 + rx, z0 + rz);
            // From above, a blade and its flower lie within a disc about the
            // middle of its lean; a ray passing wide of that meets neither.
            let (centre_x, centre_z) = (rx + 0.5 * lean * lx, rz + 0.5 * lean * lz);
            let spread = 0.5 * lean + self.width + flower.unwrap_or(0.0);
            let across =
                ray.dir.x * (centre_z - ray.origin.z) - ray.dir.z * (centre_x - ray.origin.x);
            if across * across > spread * spread * flat {
                continue;
            }
            // Rooted a little into the ground it stands on.
            let plane = ground + slope_x * (rx - mx) + slope_z * (rz - mz);
            let root = Vec3::new(rx, plane.min(highest) - 0.01, rz);
            let side = Vec3::new(-lz, 0.0, lx);
            let middle = root + Vec3::new(0.3 * lean * lx, 0.55 * height, 0.3 * lean * lz);
            let tip = root + Vec3::new(lean * lx, height, lean * lz);
            let ribbons = [
                (root, middle, self.width, 0.65 * self.width, 0.0, 0.55),
                (middle, tip, 0.65 * self.width, 0.0, 0.55, 1.0),
            ];
            for (start, end, wide, narrow, low, high) in ribbons {
                if let Some((t, normal, along)) =
                    ribbon(ray, (start, end, side), (wide, narrow), (from, reach))
                {
                    reach = t;
                    best = Some(Hit {
                        t,
                        normal,
                        shading: normal,
                        mark: key,
                        along: low + (high - low) * along,
                    });
                }
            }
            if let Some(radius) = flower {
                let head = tip + Vec3::new(0.0, 0.008, 0.0);
                let facing = Vec3::new(0.35 * lx, 1.0, 0.35 * lz).normalized();
                if let Some(t) = disc(ray, head, facing, radius, (from, reach)) {
                    reach = t;
                    best = Some(Hit {
                        t,
                        normal: facing,
                        shading: facing,
                        mark: key | FLOWER,
                        along: 1.0,
                    });
                }
            }
        }
        best
    }
}

/// The golden angle's cosine and sine: each blade of a cell turned this far
/// from the last spreads their headings evenly round the circle.
const GOLDEN: (f64, f64) = (-0.737_368_878_078_319_7, 0.675_490_294_261_523_9);

/// `toward` turned by the golden angle.
fn turn_golden((x, z): (f64, f64)) -> (f64, f64) {
    (x * GOLDEN.0 - z * GOLDEN.1, x * GOLDEN.1 + z * GOLDEN.0)
}

/// Where `ray` meets, within `(from, to)`, the flat ribbon from `start` to
/// `end` spread along the unit `side` — `wide` across at its start, `narrow`
/// at its end — its normal there, and how far along it.
fn ribbon(
    ray: &Ray,
    (start, end, side): (Vec3, Vec3, Vec3),
    (wide, narrow): (f64, f64),
    (from, to): (f64, f64),
) -> Option<(f64, Vec3, f64)> {
    let axis = end - start;
    let normal = side.cross(axis).normalized();
    let facing = normal.dot(ray.dir);
    if facing.abs() < 1e-12 {
        return None;
    }
    let t = normal.dot(start - ray.origin) / facing;
    if !(t >= from && t < to) {
        return None;
    }
    let offset = ray.at(t) - start;
    let along = offset.dot(axis) / axis.dot(axis);
    if !(0.0..=1.0).contains(&along) {
        return None;
    }
    let breadth = wide + (narrow - wide) * along;
    let half = 0.5 * breadth;
    (offset.dot(side).abs() <= half).then_some((t, normal, along))
}

/// Where `ray` meets, within `(from, to)`, the disc of `radius` about
/// `centre` facing `normal`.
fn disc(ray: &Ray, centre: Vec3, normal: Vec3, radius: f64, (from, to): (f64, f64)) -> Option<f64> {
    let facing = normal.dot(ray.dir);
    if facing.abs() < 1e-12 {
        return None;
    }
    let t = normal.dot(centre - ray.origin) / facing;
    let offset = ray.at(t) - centre;
    (t >= from && t < to && offset.dot(offset) <= radius * radius).then_some(t)
}

#[cfg(test)]
#[path = "grass_tests.rs"]
mod tests;
