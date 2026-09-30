//! A scene: what it is made of, how it is lit and seen, and the hierarchy a
//! ray finds its objects through.
//!
//! A [`Draft`] is a scene composed but not yet traceable: the grids its land,
//! sea and clouds are drawn from still to fill. It fills them a band of rows
//! at a time across whatever runner its caller holds, and
//! [`finish`](Draft::finish) builds the hierarchy. The [`Scene`] is then
//! read-only, and shared by every core tracing it.

use alloc::vec::Vec;

use tairix_parallel::JobRunner;
use tairix_util::fallible;

use crate::bvh::{Bvh, Walk};
use crate::camera::Camera;
use crate::compose::{compose, Setting};
use crate::heightfield::Heightfield;
use crate::light::Light;
use crate::material::Material;
use crate::shape::{Face, Geometry, Hit, Shape};
use crate::sky::Sky;
use crate::terrain::{Cloudscape, Sea, Terrain};
use crate::vector::{real, Pose, Ray, Vec3};

/// How near a ray may meet a surface: nearer is the surface it left.
pub(crate) const NEAR: f64 = 1e-7;

/// Whose view a ray is, which decides whether it meets a lamp kept out of
/// the picture.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Sight {
    /// Straight from the eye.
    Eye,
    /// Reflected, refracted, or looking for light.
    Bounce,
}

/// One object.
#[derive(Clone, Debug)]
pub(crate) struct Object {
    pub(crate) shape: Shape,
    pub(crate) material: usize,
    /// The frame the object's pattern and relief are fixed in, so they move
    /// and turn with it.
    pub(crate) texture: Pose,
    /// The light this object is the surface of.
    pub(crate) light: Option<usize>,
    /// What passing through a clear object leaves of light bound for another
    /// surface; `None` for an opaque one, which leaves nothing.
    pub(crate) filter: Option<Vec3>,
    /// Whether the eye sees it directly: a softbox, like a studio's, is kept
    /// out of the picture while its light and its reflections stay in it.
    pub(crate) in_view: bool,
}

/// Haze between the camera and what it sees.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Fog {
    /// How much of the light is lost per unit of distance.
    pub(crate) density: f64,
}

/// What fills a grid.
///
/// A sea's swells make it far the largest; a scene holds a handful of fills
/// at most, and boxing it would take an allocation that cannot fail
/// gracefully.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug)]
pub(crate) enum Form {
    Land(Terrain),
    Sea(Sea),
    Clouds(Cloudscape),
}

impl Form {
    fn value(&self, x: f64, z: f64) -> f64 {
        match self {
            Self::Land(land) => land.height(x, z),
            Self::Sea(sea) => sea.height(x, z),
            Self::Clouds(clouds) => clouds.density(x, z),
        }
    }
}

/// Which grid a fill fills.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Target {
    /// The scene's height grid of this index.
    Field(usize),
    /// The sky's cloud cover.
    Clouds,
}

/// A grid still being filled.
#[derive(Clone, Debug)]
pub(crate) struct Fill {
    pub(crate) target: Target,
    pub(crate) form: Form,
    /// The first row not yet filled.
    pub(crate) row: usize,
}

/// Everything a scene is, before its hierarchy is built.
#[derive(Debug)]
pub(crate) struct Parts {
    pub(crate) objects: Vec<Object>,
    pub(crate) faces: Vec<Face>,
    pub(crate) fields: Vec<Heightfield>,
    pub(crate) materials: Vec<Material>,
    pub(crate) lights: Vec<Light>,
    pub(crate) sky: Sky,
    pub(crate) fog: Option<Fog>,
    pub(crate) camera: Camera,
    pub(crate) exposure: f64,
    pub(crate) bounce: Vec3,
    pub(crate) fills: Vec<Fill>,
}

/// A scene composed, with the grids it is drawn from still to fill.
#[derive(Debug)]
pub struct Draft {
    parts: Parts,
}

impl Draft {
    /// A scene set in `setting` under `seed`, for a picture `aspect` times as
    /// wide as it is tall; `None` when the heap will not hold it.
    #[must_use]
    pub fn new(setting: Setting, seed: u64, aspect: f64) -> Option<Self> {
        compose(setting, seed, aspect).map(|parts| Self { parts })
    }

    /// How many vertices of its grids are still to fill.
    #[must_use]
    pub fn remaining(&self) -> u32 {
        let vertices: usize = self
            .parts
            .fills
            .iter()
            .map(|fill| {
                let side = self.rows_of(fill.target);
                side.saturating_sub(fill.row) * side
            })
            .sum();
        u32::try_from(vertices).unwrap_or(u32::MAX)
    }

    /// Fill about `vertices` more vertices of the scene's grids, spread over
    /// `runner`, answering how many are still to fill. Rows are filled whole,
    /// so a grid's last row may run past the count.
    pub fn prepare(&mut self, runner: &dyn JobRunner, vertices: u32) -> u32 {
        let mut budget = vertices as usize;
        while budget > 0 {
            let Some(mut fill) = self.parts.fills.pop() else {
                break;
            };
            let total = self.rows_of(fill.target);
            let take = budget
                .div_ceil(total.max(1))
                .min(total.saturating_sub(fill.row));
            self.fill(&fill, fill.row..fill.row + take, runner);
            fill.row += take;
            budget = budget.saturating_sub(take * total);
            if fill.row >= total {
                self.seal(fill.target);
            } else {
                self.parts.fills.push(fill);
            }
        }
        self.remaining()
    }

    /// The finished scene, its grids filled here first if any were left;
    /// `None` when the heap will not hold its hierarchy.
    #[must_use]
    pub fn finish(mut self) -> Option<Scene> {
        while self.prepare(&tairix_parallel::SERIAL, u32::MAX) > 0 {}
        Scene::new(self.parts)
    }

    fn rows_of(&self, target: Target) -> usize {
        match target {
            Target::Field(index) => self.parts.fields.get(index).map_or(0, Grid::rows),
            Target::Clouds => self.parts.sky.clouds.as_ref().map_or(0, Grid::rows),
        }
    }

    /// Fill `rows` of `fill`'s grid across `runner`.
    fn fill(&mut self, fill: &Fill, rows: core::ops::Range<usize>, runner: &dyn JobRunner) {
        let value = |x: f64, z: f64| fill.form.value(x, z);
        match fill.target {
            Target::Field(index) => {
                if let Some(field) = self.parts.fields.get_mut(index) {
                    fill_grid(field, rows, runner, &value);
                }
            }
            Target::Clouds => {
                if let Some(clouds) = self.parts.sky.clouds.as_mut() {
                    fill_grid(clouds, rows, runner, &value);
                }
            }
        }
    }

    fn seal(&mut self, target: Target) {
        match target {
            Target::Field(index) => {
                if let Some(field) = self.parts.fields.get_mut(index) {
                    field.seal();
                }
            }
            Target::Clouds => {
                if let Some(clouds) = self.parts.sky.clouds.as_mut() {
                    clouds.seal();
                }
            }
        }
    }
}

/// A square grid of values filled a band of rows at a time: a height grid,
/// or a cloud layer's cover.
pub(crate) trait Grid {
    /// How many rows, and so vertices a row, the grid has.
    fn rows(&self) -> usize;
    /// Where its vertices lie.
    fn layout(&self) -> Layout;
    /// Its rows `range`, as disjoint bands `rows` rows high, each with the
    /// row it starts at.
    fn bands(
        &mut self,
        range: core::ops::Range<usize>,
        rows: usize,
    ) -> impl Iterator<Item = (usize, &mut [f32])>;
}

/// Fill `rows` of `grid` with `value` at each vertex, the rows spread over
/// `runner` in bands; on the calling thread alone when the heap will not
/// hold the list of bands.
fn fill_grid<G: Grid>(
    grid: &mut G,
    rows: core::ops::Range<usize>,
    runner: &dyn JobRunner,
    value: &(dyn Fn(f64, f64) -> f64 + Sync),
) {
    let side = grid.rows().max(1);
    let layout = grid.layout();
    let pieces = tairix_parallel::bands(runner, rows.len(), FILL_GRAIN.div_ceil(side));
    let per = rows.len().div_ceil(pieces.max(1)).max(1);
    let fill_band = |(start, cells): &mut (usize, &mut [f32])| {
        for (offset, row) in cells.chunks_mut(side).enumerate() {
            for (column, cell) in row.iter_mut().enumerate() {
                let (x, z) = layout.vertex(column, *start + offset);
                // The grids hold `f32`: heights and densities need no more.
                #[allow(clippy::cast_possible_truncation)]
                let narrowed = value(x, z) as f32;
                *cell = narrowed;
            }
        }
    };
    let mut bands = Vec::new();
    if pieces > 1 && fallible::reserve(&mut bands, pieces) {
        bands.extend(grid.bands(rows, per));
        tairix_parallel::for_each(runner, &mut bands, &fill_band);
    } else {
        for mut band in grid.bands(rows, per) {
            fill_band(&mut band);
        }
    }
}

/// How few vertices a band of a grid should hold to be worth another core.
const FILL_GRAIN: usize = 2048;

/// Where a grid's vertices lie: the world x and z of vertex `(0, 0)` and the
/// distance between neighbours.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Layout {
    pub(crate) origin: (f64, f64),
    pub(crate) step: f64,
}

impl Layout {
    pub(crate) fn vertex(&self, column: usize, row: usize) -> (f64, f64) {
        (
            self.origin.0 + self.step * real(column),
            self.origin.1 + self.step * real(row),
        )
    }
}

/// A scene ready to trace.
#[derive(Debug)]
pub struct Scene {
    pub(crate) objects: Vec<Object>,
    pub(crate) faces: Vec<Face>,
    pub(crate) fields: Vec<Heightfield>,
    pub(crate) materials: Vec<Material>,
    pub(crate) lights: Vec<Light>,
    pub(crate) sky: Sky,
    pub(crate) fog: Option<Fog>,
    pub(crate) camera: Camera,
    /// What a radiance is scaled by before it is toned for the screen.
    pub(crate) exposure: f64,
    /// The light taken to reach a point from wherever the sky is hidden from
    /// it: a stand-in for the light scattered about the scene.
    pub(crate) bounce: Vec3,
    bvh: Bvh,
    /// Objects without end, which every ray is tested against.
    unbounded: Vec<usize>,
}

impl Scene {
    /// The scene of `parts`, or `None` when the heap will not hold its
    /// hierarchy.
    pub(crate) fn new(parts: Parts) -> Option<Self> {
        let mut bounded = Vec::new();
        let mut unbounded = Vec::new();
        if !(fallible::reserve(&mut bounded, parts.objects.len())
            && fallible::reserve(&mut unbounded, parts.objects.len()))
        {
            return None;
        }
        let geometry = Geometry {
            faces: &parts.faces,
            fields: &parts.fields,
        };
        for (index, object) in parts.objects.iter().enumerate() {
            match object.shape.bounds(geometry) {
                Some(bounds) => bounded.push((u32::try_from(index).ok()?, bounds)),
                None => unbounded.push(index),
            }
        }
        let bvh = Bvh::build(&bounded)?;
        Some(Self {
            objects: parts.objects,
            faces: parts.faces,
            fields: parts.fields,
            materials: parts.materials,
            lights: parts.lights,
            sky: parts.sky,
            fog: parts.fog,
            camera: parts.camera,
            exposure: parts.exposure,
            bounce: parts.bounce,
            bvh,
            unbounded,
        })
    }

    fn geometry(&self) -> Geometry<'_> {
        Geometry {
            faces: &self.faces,
            fields: &self.fields,
        }
    }

    /// The object `ray`, seen by `sight`, meets first nearer than `reach`,
    /// and where.
    pub(crate) fn closest(&self, ray: &Ray, reach: f64, sight: Sight) -> Option<(usize, Hit)> {
        let mut best: Option<(usize, Hit)> = None;
        let mut reach = reach;
        let seen = |index: usize| {
            sight == Sight::Bounce || self.objects.get(index).is_some_and(|object| object.in_view)
        };
        for &index in &self.unbounded {
            if let Some(hit) = self.test(index, ray, reach).filter(|_| seen(index)) {
                reach = hit.t;
                best = Some((index, hit));
            }
        }
        self.bvh.walk(ray, reach, |object, reach| {
            let index = object as usize;
            match self.test(index, ray, reach).filter(|_| seen(index)) {
                Some(hit) => {
                    best = Some((index, hit));
                    Walk::Within(hit.t)
                }
                None => Walk::Within(reach),
            }
        });
        best
    }

    /// How much of the light leaving `ray`'s origin arrives `reach` along
    /// it, starting within a medium absorbing `inside` if there is one:
    /// nothing past an opaque object, and filtered by each clear one.
    pub(crate) fn transmittance(&self, ray: &Ray, reach: f64, inside: Option<Vec3>) -> Vec3 {
        let mut kept = Vec3::ONE;
        // Where the ray first leaves the medium it starts in, if it does.
        let mut out = reach;
        let mut blocked = false;
        let mut through = |index: usize, hit: Hit| -> bool {
            let Some(object) = self.objects.get(index) else {
                return true;
            };
            let Some(filter) = object.filter else {
                blocked = true;
                return false;
            };
            kept = kept * filter;
            if hit.normal.dot(ray.dir) > 0.0 {
                out = out.min(hit.t);
            }
            true
        };
        for &index in &self.unbounded {
            if let Some(hit) = self.shadow_test(index, ray, reach) {
                if !through(index, hit) {
                    return Vec3::ZERO;
                }
            }
        }
        self.bvh.walk(ray, reach, |object, reach| {
            let index = object as usize;
            match self.shadow_test(index, ray, reach) {
                Some(hit) if !through(index, hit) => Walk::Stop,
                _ => Walk::Within(reach),
            }
        });
        if blocked {
            return Vec3::ZERO;
        }
        match inside {
            Some(absorb) if out.is_finite() => kept * (absorb * -out).exp(),
            _ => kept,
        }
    }

    /// Where `ray` meets object `index` nearer than `reach`.
    fn test(&self, index: usize, ray: &Ray, reach: f64) -> Option<Hit> {
        self.objects
            .get(index)?
            .shape
            .intersect(ray, NEAR, reach, self.geometry())
    }

    /// Where `ray` meets object `index` nearer than `reach`, if the object
    /// casts a shadow at all.
    fn shadow_test(&self, index: usize, ray: &Ray, reach: f64) -> Option<Hit> {
        let object = self.objects.get(index)?;
        if !object.shape.casts_shadow() {
            return None;
        }
        object.shape.intersect(ray, NEAR, reach, self.geometry())
    }
}

#[cfg(test)]
#[path = "scene_tests.rs"]
mod tests;
