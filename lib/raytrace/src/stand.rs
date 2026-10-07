//! A field's crop as it stands near the eye, as plants on its drilled rows:
//! each plant hashed from a lattice laid along the rows rather than stood
//! one by one, so a field stands as thickly as it was drilled for no more
//! than the walk a ray takes across it.
//!
//! A cell of the lattice is a plant's spacing along its row by the rows'
//! spacing across, and holds one place a plant might stand, jittered about
//! its row as its planting has it; where a trellis holds the plants, a post
//! stands true in place of every so many. A place stands a plant where its
//! seed came up, no tramline runs,
//! the eye is near enough — further off the plants thin away to the sward
//! that carries the crop on — and the land's nearest vertex carries the
//! stand's growth and rows. Which of the stand's plants it is, how it is
//! turned and how tall it stands are drawn from the place's own key, so the
//! lattice reads the same however a ray comes to it. A ray walks the cells it
//! crosses over the field while low enough to meet a plant, and meets each
//! plant whose leaves could reach them once, as they come within reach: none
//! reaches further than the walk looks about the cell it is in, so a hit
//! before the ray leaves that cell is the nearest.

use alloc::vec::Vec;
use core::f64::consts::{PI, TAU};

use tairix_countryside::plane::Convex;
use tairix_countryside::Point;
use tairix_util::mathf;

use crate::farmed::Drill;
use crate::heightfield::Heightfield;
use crate::land::Grids;
use crate::noise::{hash2, smoothstep};
use crate::sample::{mix32, unit};
use crate::shape::{meet_placed, occluded_by_placed, reciprocal, Aabb, Geometry, Hit};
use crate::vector::{Frame, Pose, Ray, Vec3};
use crate::walk::{self, Stepped, Walk};

/// The most kinds of plant a stand draws from.
pub(crate) const PLANTS: usize = 8;

/// The most cells a ray walks across a stand: past any it holds along a ray,
/// so only a walk gone wrong ever meets it.
const MOST_CELLS: u32 = 1 << 16;

/// How far into the ground a plant's foot is set, so no gap shows beneath
/// it on a slope.
const ROOTED: f64 = 0.02;

/// How a stand's plants are set out: how far off its place one stands,
/// along its row as a share of the plants' spacing and across it in metres;
/// the most it leans off upright, in radians; for plants trained along their
/// row, as a vine is along its wire, how far either side of it one is turned
/// at most, rather than any way; the share of the places that stand one; and
/// whether it was drilled with the tramlines a sprayer's wheels keep clear.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) struct Planting {
    pub(crate) jitter: (f64, f64),
    pub(crate) lean: f64,
    pub(crate) trained: Option<f64>,
    pub(crate) come_up: f64,
    pub(crate) tramlines: bool,
}

/// A crop drilled in its rows: each seed falls a little off its place, the
/// plant leaning a little and turned any way, most coming up, its tramlines
/// kept clear.
pub(crate) const DRILLED: Planting = Planting {
    jitter: (0.35, 0.025),
    lean: 0.06,
    trained: None,
    come_up: 0.94,
    tramlines: true,
};

/// A kind of plant a stand stands: the prototype it is placed from, the
/// material its parts are made in where they set none, and how tall it
/// stands and how far its leaves reach about its foot at its natural size.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) struct Plant {
    pub(crate) prototype: u32,
    pub(crate) material: u32,
    pub(crate) height: f64,
    pub(crate) reach: f64,
}

/// How a stand's plants are sown: what its land's nearest vertex carries
/// where its crop grows, and how the field was drilled; how far apart its
/// plants stand along a row and how they are set out there; the kinds it
/// draws from, the first `kinds` of `plants`, and how much of its natural
/// size one stands at, least and most; the post that stands in a row in
/// place of every so many plants, set true at its natural size and turned to
/// its row, if a trellis holds them; where it is seen from, and how far off
/// from there its plants begin to thin away and are gone; and the seed its
/// places are drawn under.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Sowing {
    pub(crate) grows: (u8, u8),
    pub(crate) drill: Drill,
    pub(crate) apart: f64,
    pub(crate) planting: Planting,
    pub(crate) plants: [Plant; PLANTS],
    pub(crate) kinds: usize,
    pub(crate) sizes: (f64, f64),
    pub(crate) posts: Option<(u32, Plant)>,
    pub(crate) eye: (f64, f64),
    pub(crate) thinning: (f64, f64),
    pub(crate) seed: u32,
}

/// How far about the eye no plant of a stand stands, as the eye is never put
/// among them.
const CLEAR: f64 = 1.5;

/// A field's crop standing as plants near the eye.
#[derive(Debug)]
pub(crate) struct Stand {
    grids: Grids,
    sowing: Sowing,
    /// The field it stands in.
    field: Convex,
    /// Which of all the cells its drill lays, along the rows and across
    /// them, its lattice counts from.
    first: (i32, i32),
    /// How many cells about its own a plant's leaves may reach, along the
    /// rows and across them.
    spread: (u32, u32),
    /// How tall its tallest plant stands, and how far its widest reaches.
    tallest: f64,
    reach: f64,
    bounds: Aabb,
    /// The heading its rows run along.
    heading: f64,
}

/// A place a plant may stand: where on the ground, its key, and whether a
/// trellis's post stands there in its stead.
#[derive(Copy, Clone, Debug)]
struct Place {
    at: (f64, f64),
    key: u32,
    post: bool,
}

/// A plant as it stands at its place: the prototype it is placed from and
/// the material its parts are made in where they set none, its pose and
/// scale, and how far over its pose it stands.
#[derive(Copy, Clone, Debug)]
struct Stood {
    prototype: u32,
    material: u32,
    pose: Pose,
    scale: f64,
    height: f64,
}

impl Stand {
    /// `sowing`'s plants standing in `field` on the land `grids` trace in
    /// `fields`; `None` where none could stand near enough the eye, or the
    /// heap will not hold the field.
    pub(crate) fn new(
        (grids, fields): (Grids, &[Heightfield]),
        field: &Convex,
        sowing: &Sowing,
    ) -> Option<Self> {
        let plants = sowing
            .plants
            .get(..sowing.kinds)
            .filter(|plants| !plants.is_empty())?;
        let most = sowing.sizes.1;
        let post = sowing.posts.map(|(_, post)| post);
        let tallest = plants
            .iter()
            .map(|plant| plant.height * most)
            .chain(post.map(|post| post.height))
            .fold(0.0, f64::max);
        let reach = plants
            .iter()
            .map(|plant| plant.reach * most)
            .chain(post.map(|post| post.reach))
            .fold(0.0, f64::max);
        let (drill, apart) = (sowing.drill, sowing.apart);
        let usable = |length: f64| length.is_finite() && length > 0.0;
        if !usable(tallest) || !usable(reach) || !usable(apart) || !usable(drill.spacing()) {
            return None;
        }
        let bounds = field.bounds()?;
        let gone = sowing.thinning.1 + reach;
        let (eye, low, high) = (
            sowing.eye,
            (bounds.low.x, bounds.low.y),
            (bounds.high.x, bounds.high.y),
        );
        let from = (low.0.max(eye.0 - gone), low.1.max(eye.1 - gone));
        let to = (high.0.min(eye.0 + gone), high.1.min(eye.1 + gone));
        if from.0 >= to.0 || from.1 >= to.1 {
            return None;
        }
        // The lattice counts from a cell clear of every corner of the square,
        // so each cell the walk visits has a whole place to look at.
        let corners = [from, (to.0, from.1), (from.0, to.1), to];
        let (mut least, mut lowest) = (f64::INFINITY, f64::INFINITY);
        for corner in corners {
            least = least.min(drill.along(corner) / apart);
            lowest = lowest.min(drill.rows_from(corner) + 0.5);
        }
        let first = |at: f64| mathf::round_i32(mathf::floor(at)).checked_sub(1);
        let first = (first(least)?, first(lowest)?);
        let spread =
            |reach: f64, cell: f64| u32::try_from(mathf::round_i32(mathf::ceil(reach / cell))).ok();
        let (surface, top) = grids.extremes(fields, (from, to));
        let ground = surface - grids.deepest_snow(fields, (from, to));
        if !ground.is_finite() || !top.is_finite() {
            return None;
        }
        let (along_x, along_z) = drill.place((0.0, 1.0));
        Some(Self {
            grids,
            sowing: *sowing,
            field: field.copied()?,
            first,
            spread: (spread(reach, apart)?, spread(reach, drill.spacing())?),
            tallest,
            reach,
            bounds: Aabb {
                min: Vec3::new(from.0 - reach, ground - ROOTED, from.1 - reach),
                max: Vec3::new(to.0 + reach, top + tallest, to.1 + reach),
            },
            heading: mathf::atan2(-along_z, along_x),
        })
    }

    /// The box it stands within.
    pub(crate) const fn bounds(&self) -> Aabb {
        self.bounds
    }

    /// The nearest place in `(near, far)` where `ray` meets one of its
    /// plants.
    pub(crate) fn intersect(
        &self,
        ray: &Ray,
        span: (f64, f64),
        geometry: Geometry<'_>,
    ) -> Option<Hit> {
        self.walk(ray, span, geometry, false)
    }

    /// Whether `ray` meets any of its plants in `(near, far)`.
    pub(crate) fn occludes(&self, ray: &Ray, span: (f64, f64), geometry: Geometry<'_>) -> bool {
        self.walk(ray, span, geometry, true).is_some()
    }

    /// The posts its trellis stands, each its row among those its drill lays
    /// and where its foot stands on the land in `fields`, in order along each
    /// row; none where no trellis holds its plants, and `None` when the heap
    /// will not hold them.
    pub(crate) fn posts(&self, fields: &[Heightfield]) -> Option<Vec<(i32, Vec3)>> {
        let mut posts = Vec::new();
        // As `place` has it, a trellis setting no plant apart stands no post.
        let every = self
            .sowing
            .posts
            .and_then(|(every, _)| i32::try_from(every).ok());
        let Some(every) = every.filter(|&every| every > 0) else {
            return Some(posts);
        };
        let corner = |x: f64, z: f64| {
            self.on_lattice(&Ray::new(Vec3::new(x, 0.0, z), Vec3::ZERO))
                .origin
        };
        let (min, max) = (self.bounds.min, self.bounds.max);
        let corners = [
            corner(min.x, min.z),
            corner(max.x, min.z),
            corner(min.x, max.z),
            corner(max.x, max.z),
        ];
        let most = |along: fn(&Vec3) -> f64| corners.iter().map(along).fold(0.0, f64::max);
        let columns = u32::try_from(mathf::round_i32(mathf::ceil(most(|at| at.x)))).ok()?;
        let rows = u32::try_from(mathf::round_i32(mathf::ceil(most(|at| at.z)))).ok()?;
        let offset = self.first.0.rem_euclid(every).cast_unsigned();
        let every = every.cast_unsigned();
        for row in 0..=rows {
            // The first column whose global index is a post's.
            let mut column = (every - offset) % every;
            while column <= columns {
                if let Some(place) = self
                    .place((column, row))
                    .filter(|&place| place.post && self.stands(place, fields))
                {
                    let foot = self.plant(place, fields)?.pose.at;
                    posts.try_reserve(1).ok()?;
                    posts.push((self.first.1.checked_add(i32::try_from(row).ok()?)?, foot));
                }
                column += every;
            }
        }
        Some(posts)
    }

    /// Where the plant a hit on cell `cell` met stands, and its key: what its
    /// pattern is fixed in and set apart by.
    pub(crate) fn placing(&self, cell: (u32, u32), fields: &[Heightfield]) -> Option<(Pose, u32)> {
        let place = self.place(cell)?;
        Some((self.plant(place, fields)?.pose, place.key))
    }

    /// The place cell `cell` of its lattice holds; `None` past any cell the
    /// drill lays.
    fn place(&self, (column, row): (u32, u32)) -> Option<Place> {
        let along = self.first.0.checked_add(i32::try_from(column).ok()?)?;
        let across = self.first.1.checked_add(i32::try_from(row).ok()?)?;
        let key = hash2(
            along.cast_unsigned(),
            across.cast_unsigned(),
            self.sowing.seed,
        );
        let post = self.sowing.posts.is_some_and(|(every, _)| {
            i32::try_from(every).is_ok_and(|every| every > 0 && along.rem_euclid(every) == 0)
        });
        let drill = &self.sowing.drill;
        // A post is set true on its place.
        let jitter = if post {
            (0.0, 0.0)
        } else {
            self.sowing.planting.jitter
        };
        let off = |salt: u32| 2.0 * unit(mix32(key ^ salt)) - 1.0;
        let along = (f64::from(along) + 0.5 + jitter.0 * off(1)) * self.sowing.apart;
        let across = drill.row(f64::from(across)) + jitter.1 * off(2);
        Some(Place {
            at: drill.place((across, along)),
            key,
            post,
        })
    }

    /// Whether `place` stands a plant: its seed came up, no tramline its
    /// drilling kept clear runs over it, the eye is near enough but not among
    /// them, and the land there is its own field's, growing the crop. A post
    /// always stood.
    fn stands(&self, place: Place, fields: &[Heightfield]) -> bool {
        let Place { at, key, post } = place;
        let sowing = &self.sowing;
        let off = mathf::hypot(at.0 - sowing.eye.0, at.1 - sowing.eye.1);
        (post || unit(mix32(key ^ 3)) < sowing.planting.come_up)
            && off > CLEAR
            && unit(mix32(key ^ 4)) >= smoothstep(sowing.thinning.0, sowing.thinning.1, off)
            && !(sowing.planting.tramlines && sowing.drill.tracked(at, 0.0) >= 0.5)
            && self.field.contains(Point::new(at.0, at.1))
            && self.grids.grows(fields, at.0, at.1) == sowing.grows
    }

    /// The plant `place` stands, on the land in `fields`: a trained plant or
    /// a post its local x along its row, either way round.
    fn plant(&self, place: Place, fields: &[Heightfield]) -> Option<Stood> {
        let Place { at, key, post } = place;
        let (sowing, along) = (&self.sowing, self.heading);
        let draw = |salt: u32| unit(mix32(key ^ salt));
        let (kind, frame, scale) = if let Some((_, post)) = sowing.posts.filter(|_| post) {
            (post, Frame::turned(along, 0.0), 1.0)
        } else {
            let kinds = u32::try_from(sowing.kinds)
                .ok()
                .filter(|&kinds| kinds > 0)?;
            let kind = *sowing
                .plants
                .get(usize::try_from(mix32(key ^ 5) % kinds).ok()?)?;
            let yaw = sowing.planting.trained.map_or_else(
                || TAU * draw(6),
                |turned| {
                    let round = if draw(9) < 0.5 { 0.0 } else { PI };
                    along + round + turned * (2.0 * draw(6) - 1.0)
                },
            );
            let scale = sowing.sizes.0 + (sowing.sizes.1 - sowing.sizes.0) * draw(8);
            (
                kind,
                Frame::turned(yaw, sowing.planting.lean * draw(7)),
                scale,
            )
        };
        let foot = Vec3::new(
            at.0,
            self.grids.beneath_snow(fields, at.0, at.1) - ROOTED,
            at.1,
        );
        Some(Stood {
            prototype: kind.prototype,
            material: kind.material,
            pose: Pose::new(foot, frame),
            scale,
            height: (kind.height + ROOTED) * scale,
        })
    }

    /// `ray` as its lattice reads it: in plants along the rows and rows
    /// across them, from its first cell, with its height and its pace kept,
    /// so a place along it lies as far along the one as the other.
    fn on_lattice(&self, ray: &Ray) -> Ray {
        let (drill, apart) = (&self.sowing.drill, self.sowing.apart);
        let (along, rows) = (f64::from(self.first.0), f64::from(self.first.1));
        let (origin, dir) = (ray.origin, ray.dir);
        let at = (origin.x, origin.z);
        let (x, z) = drill.normal();
        Ray::new(
            Vec3::new(
                drill.along(at) / apart - along,
                origin.y,
                drill.rows_from(at) + 0.5 - rows,
            ),
            Vec3::new(
                (dir.z * x - dir.x * z) / apart,
                dir.y,
                (dir.x * x + dir.z * z) / drill.spacing(),
            ),
        )
    }

    /// Where along `ray`, within `(enter, leave)`, its track across the
    /// ground lies within reach of a plant's leaves over the field; `None`
    /// where it never does.
    fn over_field(&self, ray: &Ray, (mut enter, mut leave): (f64, f64)) -> Option<(f64, f64)> {
        for (a, b) in self.field.edges() {
            // Its corners run anticlockwise, so the field lies left of each
            // edge; each edge is pushed out by the leaves' reach.
            let edge = b - a;
            let inward = Point::new(-edge.y, edge.x);
            let start = (ray.origin.x - a.x) * inward.x
                + (ray.origin.z - a.y) * inward.y
                + self.reach * edge.length();
            let rate = ray.dir.x * inward.x + ray.dir.z * inward.y;
            if rate.abs() < 1e-12 {
                if start < 0.0 {
                    return None;
                }
                continue;
            }
            let crossing = -start / rate;
            if rate > 0.0 {
                enter = enter.max(crossing);
            } else {
                leave = leave.min(crossing);
            }
            if enter > leave {
                return None;
            }
        }
        Some((enter, leave))
    }

    /// Walk `ray` across the cells over the field within `(near, far)` for
    /// the nearest plant it meets, or for any where `any`: from where it
    /// comes down within reach of a plant, until it rises clear of them all,
    /// goes beneath the ground or leaves the field.
    fn walk(
        &self,
        ray: &Ray,
        (near, far): (f64, f64),
        geometry: Geometry<'_>,
        any: bool,
    ) -> Option<Hit> {
        let (enter, leave) = self.bounds.span(ray, reciprocal(ray.dir), far)?;
        let (enter, leave) = self.over_field(ray, (enter.max(near), leave))?;
        let fields = geometry.fields;
        // A plant's leaves may reach over ground rising toward it.
        let t = self
            .grids
            .approach(fields, ray, self.tallest + self.reach, (enter, leave))?;
        let lattice = self.on_lattice(ray);
        let mut walk = Walk::from(((0.0, 0.0), 1.0), &lattice, t);
        let mut meeting = Meeting {
            stand: self,
            ray,
            geometry,
            near,
            reach: far,
            best: None,
            any,
        };
        meeting.around(walk.cell);
        let ceiling = self.bounds.max.y;
        for _ in 0..MOST_CELLS {
            let exit = walk.exit().min(leave);
            if meeting.best.is_some_and(|hit| any || hit.t <= exit)
                || exit >= leave.min(meeting.reach)
            {
                return meeting.best;
            }
            if ray.dir.y >= 0.0 && ray.at(exit).y > ceiling {
                return meeting.best;
            }
            let stepped = walk.step();
            meeting.entering(walk.cell, stepped);
        }
        meeting.best
    }
}

/// One ray's meetings with a stand's plants, nearest kept.
struct Meeting<'a> {
    stand: &'a Stand,
    ray: &'a Ray,
    geometry: Geometry<'a>,
    near: f64,
    reach: f64,
    best: Option<Hit>,
    any: bool,
}

impl Meeting<'_> {
    /// Meet every plant whose leaves could reach `cell`.
    fn around(&mut self, cell: (u32, u32)) {
        for about in walk::about(cell, self.stand.spread) {
            self.meet(about);
        }
    }

    /// Meet the plants that come within reach as the walk steps into `cell`
    /// as `stepped` has it: a column of cells along the rows, or a row of
    /// them across.
    fn entering(&mut self, cell: (u32, u32), stepped: Stepped) {
        for ahead in walk::ahead(cell, stepped, self.stand.spread) {
            self.meet(ahead);
        }
    }

    /// Meet the plant `cell` holds, if one stands there and the ray passes
    /// within its reach low enough to meet it: the land there read only once
    /// the ray passes near.
    fn meet(&mut self, cell: (u32, u32)) {
        if self.any && self.best.is_some() {
            return;
        }
        let stand = self.stand;
        let Some(place) = stand.place(cell) else {
            return;
        };
        let Some((first, last)) =
            walk::passing(self.ray, place.at, stand.reach, (self.near, self.reach))
        else {
            return;
        };
        let ray = self.ray;
        let lowest = (ray.origin.y + ray.dir.y * first).min(ray.origin.y + ray.dir.y * last);
        let fields = self.geometry.fields;
        if lowest > stand.bounds.max.y || !stand.stands(place, fields) {
            return;
        }
        let Some(stood) = stand.plant(place, fields) else {
            return;
        };
        if lowest > stood.pose.at.y + stood.height {
            return;
        }
        let (placing, span) = (
            (stood.prototype, &stood.pose, stood.scale, place.key),
            (self.near, self.reach),
        );
        if self.any {
            if occluded_by_placed(placing, ray, span, self.geometry) {
                self.best = Some(Hit::plain(self.near, Vec3::UP));
            }
            return;
        }
        let Some(mut hit) = meet_placed(placing, ray, span, self.geometry) else {
            return;
        };
        if hit.material.is_none() {
            hit.material = Some(stood.material);
        }
        hit.member = Some(cell);
        self.reach = hit.t;
        self.best = Some(hit);
    }
}

#[cfg(test)]
#[path = "stand_tests.rs"]
mod tests;
