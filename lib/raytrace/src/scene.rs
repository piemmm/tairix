//! A scene: what it is made of, how it is lit and seen, and the hierarchy a
//! ray finds its objects through.
//!
//! A [`Draft`] is a scene composed but not yet traceable: the work that makes
//! it so — its grids filled, its land shaped, its plants grown, its radiosity
//! gathered and its exposure measured — still to do. It does that work a
//! bounded unit at a time across whatever runner its caller holds, and
//! [`finish`](Draft::finish) completes it. The [`Scene`] is then read-only,
//! and shared by every core tracing it.

use alloc::vec::Vec;

use tairix_parallel::JobRunner;
use tairix_util::{fallible, mathf};

use crate::bvh::{Builder, Bvh, Cursor, Walk};
use crate::camera::Camera;
use crate::compose::{Composition, Setting};
use crate::grass::{Canopy, Cover, Lawn};
use crate::heightfield::Heightfield;
use crate::light::Light;
use crate::material::Material;
use crate::prototype::Prototype;
use crate::radiosity::{Gathering, Radiosity};
use crate::shade::Shades;
use crate::shape::{Aabb, Face, Geometry, Hit, Shape};
use crate::sky::Sky;
use crate::trace::Meter;
use crate::vector::{real, Members, Pose, Ray, Vec3};

/// How near a ray may meet a surface: nearer is the surface it left.
pub(crate) const NEAR: f64 = 1e-7;

/// Whose view a ray is, which decides whether it meets a lamp kept out of
/// the picture, and a lawn's blades, too fine to stand in the way of light.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Sight {
    /// Straight from the eye.
    Eye,
    /// Reflected, refracted, or looking for light.
    Bounce,
    /// Scattered off a diffuse surface: only what casts a shadow, as the
    /// light the surface gathers came.
    Scattered,
    /// The eye's view of what casts a shadow: where the light a diffuse
    /// surface gathers is recorded, beneath any lawn.
    Recorded,
}

impl Sight {
    fn sees(self, object: &Object) -> bool {
        match self {
            Self::Eye => object.in_view,
            Self::Bounce => true,
            Self::Scattered => object.shape.casts_shadow(),
            Self::Recorded => object.in_view && object.shape.casts_shadow(),
        }
    }
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

/// How a scene's light is scaled before it is toned for the screen.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) enum Exposure {
    /// By this factor, chosen with the scene: a studio's lamps are set by
    /// hand, as a photographer sets them.
    Fixed(f64),
    /// So that the scene's mean luminance, measured once it is built, reads
    /// as `key` of white: a sky with the sun in it and a field in its shade
    /// both come out as the eye would have them.
    Metered { key: f64 },
}

/// The sun as the lens sees it: which way it lies, and how much of its light
/// reaches the eye past what stands in front of it, which a lens spreads
/// about the sun's image as glare.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Glare {
    pub(crate) toward: Vec3,
    pub(crate) irradiance: Vec3,
}

/// Haze between the camera and what it sees.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Fog {
    /// How much of the light is lost per unit of distance.
    pub(crate) density: f64,
}

/// Everything a scene is, before its hierarchy is built.
#[derive(Debug)]
pub(crate) struct Parts {
    pub(crate) objects: Vec<Object>,
    pub(crate) faces: Vec<Face>,
    pub(crate) fields: Vec<Heightfield>,
    pub(crate) prototypes: Vec<Prototype>,
    pub(crate) lawns: Vec<Lawn>,
    pub(crate) materials: Vec<Material>,
    pub(crate) lights: Vec<Light>,
    pub(crate) sky: Sky,
    pub(crate) fog: Option<Fog>,
    /// The shade a land's woods cast, which roofs the air beneath them.
    pub(crate) shades: Option<Shades>,
    pub(crate) camera: Camera,
    pub(crate) exposure: Exposure,
    pub(crate) daylight: f64,
}

/// A scene composed, with the work that makes it traceable still to do.
#[derive(Debug)]
pub struct Draft {
    state: State,
    /// The picture the scene is drawn for.
    size: (u32, u32),
    /// Whether the scene stands on a land, whose building and planting then
    /// take much of the work.
    landed: bool,
    /// How far the work has come, in thousandths, as last measured.
    progress: u16,
}

/// Where each stage of a draft's work ends, as a share of the whole, for a
/// scene on a land and for one without: measured over the settings on a
/// desktop-class machine preparing across eight threads.
const LANDED_ENDS: Ends = Ends {
    composed: 0.11,
    built: 0.113,
    gathered: 0.999,
};
const UNLANDED_ENDS: Ends = Ends {
    composed: 0.03,
    built: 0.032,
    gathered: 0.996,
};

/// Where a draft's composing, building and gathering end, as shares of its
/// work; metering takes the rest.
#[derive(Copy, Clone, Debug)]
struct Ends {
    composed: f64,
    built: f64,
    gathered: f64,
}

#[allow(
    clippy::large_enum_variant,
    reason = "a draft holds one state, and a box could not fail gracefully"
)]
#[derive(Debug)]
enum State {
    Composing(Composition),
    /// Composed, the hierarchy its rays find its objects through being built.
    Building(Building, Exposure),
    /// Built, its radiosity records being laid down, and then exposed so.
    Gathering(Scene, Gathering, Exposure),
    /// Built, its exposure being measured.
    Metering(Scene, Meter),
    Ready(Scene),
    /// Taken by `finish`, or refused by the heap.
    Gone,
}

impl Draft {
    /// A scene set in `setting` under `seed`, for a picture of `size`; `None`
    /// when the heap will not hold it or the picture has no pixels.
    #[must_use]
    pub fn new(setting: Setting, seed: u64, size: (u32, u32)) -> Option<Self> {
        Composition::new(setting, seed, size).map(|composition| Self {
            landed: composition.landed(),
            state: State::Composing(composition),
            size,
            progress: 0,
        })
    }

    /// How far the draft's work has come, in thousandths: never less than it
    /// last answered, and a thousand only once the scene is ready.
    #[must_use]
    pub const fn progress(&self) -> u16 {
        self.progress
    }

    /// How far the work stands now, in thousandths.
    fn measure(&self) -> u16 {
        let ends = if self.landed {
            LANDED_ENDS
        } else {
            UNLANDED_ENDS
        };
        let within = |from: f64, to: f64, done: f64| from + (to - from) * done.clamp(0.0, 1.0);
        let done = match &self.state {
            State::Composing(composition) => within(0.0, ends.composed, composition.done()),
            State::Building(building, _) => within(ends.composed, ends.built, building.done()),
            State::Gathering(_, gathering, _) => {
                within(ends.built, ends.gathered, gathering.done())
            }
            State::Metering(_, meter) => within(ends.gathered, 1.0, meter.done()),
            State::Ready(_) => return PROGRESS_WHOLE,
            State::Gone => return self.progress,
        };
        let thousandths = mathf::round_i32(done * f64::from(PROGRESS_WHOLE));
        u16::try_from(thousandths).map_or(0, |at| at.min(PROGRESS_WHOLE - 1))
    }

    /// Do the draft's work a unit at a time across `runner` until `spent`
    /// answers that the caller's time is used — at least one unit, however
    /// soon it answers — and answer whether the scene is ready; `None` when
    /// the heap refused it. The scene comes out the same however the work
    /// was divided.
    pub fn prepare(
        &mut self,
        runner: &dyn JobRunner,
        spent: &mut dyn FnMut() -> bool,
    ) -> Option<bool> {
        let state = core::mem::replace(&mut self.state, State::Gone);
        self.state = match state {
            State::Composing(mut composition) => {
                if composition.advance(runner, spent)? {
                    let parts = composition.finish()?;
                    let exposure = parts.exposure;
                    State::Building(Scene::building(parts)?, exposure)
                } else {
                    State::Composing(composition)
                }
            }
            State::Building(mut building, exposure) => loop {
                if building.step() {
                    break State::Gathering(
                        building.finish(),
                        Gathering::new(self.size)?,
                        exposure,
                    );
                }
                if spent() {
                    break State::Building(building, exposure);
                }
            },
            State::Gathering(mut scene, mut gathering, exposure) => loop {
                if gathering.step(&scene, runner)? {
                    scene.radiosity = Some(gathering.finish()?);
                    break match exposure {
                        Exposure::Fixed(_) => State::Ready(scene),
                        Exposure::Metered { key } => State::Metering(scene, Meter::new(key)?),
                    };
                }
                if spent() {
                    break State::Gathering(scene, gathering, exposure);
                }
            },
            State::Metering(mut scene, mut meter) => loop {
                if meter.step(&scene, runner) {
                    meter.settle(&mut scene);
                    break State::Ready(scene);
                }
                if spent() {
                    break State::Metering(scene, meter);
                }
            },
            ready @ State::Ready(_) => ready,
            State::Gone => return None,
        };
        self.progress = self.progress.max(self.measure());
        Some(matches!(self.state, State::Ready(_)))
    }

    /// The finished scene, what work was left done here first; `None` when
    /// the heap will not hold it.
    #[must_use]
    pub fn finish(mut self) -> Option<Scene> {
        while !self.prepare(&tairix_parallel::SERIAL, &mut || false)? {}
        match self.state {
            State::Ready(scene) => Some(scene),
            State::Composing(_)
            | State::Building(..)
            | State::Gathering(..)
            | State::Metering(..)
            | State::Gone => None,
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

/// What an object does to a shadow ray.
enum Shadow {
    Missed,
    Opaque,
    /// A clear object met where the hit says.
    Clear(Hit),
}

/// A scene ready to trace.
#[derive(Debug)]
pub struct Scene {
    pub(crate) objects: Vec<Object>,
    pub(crate) faces: Vec<Face>,
    pub(crate) fields: Vec<Heightfield>,
    pub(crate) prototypes: Vec<Prototype>,
    pub(crate) lawns: Vec<Lawn>,
    pub(crate) materials: Vec<Material>,
    pub(crate) lights: Vec<Light>,
    pub(crate) sky: Sky,
    pub(crate) fog: Option<Fog>,
    pub(crate) shades: Option<Shades>,
    pub(crate) camera: Camera,
    /// What a radiance is scaled by before it is toned for the screen.
    pub(crate) exposure: f64,
    /// The sun the lens spreads glare about, once measured.
    pub(crate) glare: Option<Glare>,
    /// The light the scene's diffuse surfaces gather from one another and
    /// the sky, once laid down.
    pub(crate) radiosity: Option<Radiosity>,
    /// Roughly how much light falls on the scene, as a share of a clear
    /// day's: what the glow within water is scaled by.
    pub(crate) daylight: f64,
    bvh: Bvh,
    /// Objects without end, which every ray is tested against.
    unbounded: Vec<usize>,
    /// The lawns of grass, whose blades thin the light within them, among
    /// the scene's lawns.
    swards: Vec<usize>,
}

/// A scene whose hierarchy is being built, a bounded share at a time.
#[derive(Debug)]
pub(crate) struct Building {
    parts: Parts,
    builder: Builder,
    unbounded: Vec<usize>,
    swards: Vec<usize>,
}

/// Objects' worth of a scene's hierarchy built in one step.
const BUILD_UNIT: usize = 16_384;

/// A draft's progress once its scene is ready, in thousandths.
const PROGRESS_WHOLE: u16 = 1000;

impl Building {
    /// The share of the hierarchy built.
    pub(crate) fn done(&self) -> f64 {
        self.builder.done()
    }

    /// Build a step more of the hierarchy; whether it is whole.
    pub(crate) fn step(&mut self) -> bool {
        self.builder.step(BUILD_UNIT)
    }

    /// The scene, once its hierarchy is whole.
    pub(crate) fn finish(self) -> Scene {
        let Self {
            parts,
            builder,
            unbounded,
            swards,
        } = self;
        Scene {
            objects: parts.objects,
            faces: parts.faces,
            fields: parts.fields,
            prototypes: parts.prototypes,
            lawns: parts.lawns,
            materials: parts.materials,
            lights: parts.lights,
            sky: parts.sky,
            fog: parts.fog,
            shades: parts.shades,
            camera: parts.camera,
            exposure: match parts.exposure {
                Exposure::Fixed(exposure) => exposure,
                Exposure::Metered { .. } => 1.0,
            },
            glare: None,
            radiosity: None,
            daylight: parts.daylight,
            bvh: builder.finish(),
            unbounded,
            swards,
        }
    }
}

impl Scene {
    /// The scene of `parts`, its hierarchy built at once, as a test sets a
    /// scene out; `None` when the heap will not hold it.
    #[cfg(test)]
    pub(crate) fn new(parts: Parts) -> Option<Self> {
        let mut building = Self::building(parts)?;
        while !building.step() {}
        Some(building.finish())
    }

    /// The scene of `parts`, its hierarchy to build step by step; `None` when
    /// the heap will not hold it.
    pub(crate) fn building(parts: Parts) -> Option<Building> {
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
            prototypes: &parts.prototypes,
            lawns: &parts.lawns,
        };
        // An object whose box is not finite is tested by every ray rather
        // than trusted to the slab test, which a non-finite box defeats.
        for (index, object) in parts.objects.iter().enumerate() {
            match object.shape.bounds(geometry).filter(Aabb::is_finite) {
                Some(bounds) => bounded.push((u32::try_from(index).ok()?, bounds)),
                None => unbounded.push(index),
            }
        }
        let grassed = parts
            .lawns
            .iter()
            .enumerate()
            .filter(|(_, lawn)| matches!(lawn.cover, Cover::Grass(_)));
        let swards = fallible::collected(parts.lawns.len(), grassed.map(|(index, _)| index))?;
        Some(Building {
            builder: Builder::new(&bounded)?,
            parts,
            unbounded,
            swards,
        })
    }

    fn geometry(&self) -> Geometry<'_> {
        Geometry {
            faces: &self.faces,
            fields: &self.fields,
            prototypes: &self.prototypes,
            lawns: &self.lawns,
        }
    }

    /// The object `ray`, seen by `sight`, meets first nearer than `reach`,
    /// and where.
    pub(crate) fn closest(&self, ray: &Ray, reach: f64, sight: Sight) -> Option<(usize, Hit)> {
        let mut found = [None];
        self.closest_of(&[*ray], reach, sight, &mut found);
        found[0]
    }

    /// For each of `rays`, all seen by `sight`, the object it meets first
    /// nearer than `reach`, and where, into `found`: what [`Scene::closest`]
    /// finds of each alone. Each ray walks the hierarchy in its own order,
    /// testing each object with its own reach as it stands; rays that come
    /// to the same lawn wait there for one another and cross its cells
    /// together.
    pub(crate) fn closest_of<const N: usize>(
        &self,
        rays: &[Ray; N],
        reach: f64,
        sight: Sight,
        found: &mut [Option<(usize, Hit)>; N],
    ) {
        const { assert!(N <= Members::LANES) };
        let seen = |index: usize| self.objects.get(index).filter(|object| sight.sees(object));
        let mut reaches = [reach; N];
        for (lane, ray) in rays.iter().enumerate() {
            found[lane] = None;
            for &index in &self.unbounded {
                let Some(object) = seen(index) else {
                    continue;
                };
                let met = object
                    .shape
                    .intersect(ray, NEAR, reaches[lane], self.geometry());
                if let Some(hit) = met {
                    reaches[lane] = hit.t;
                    found[lane] = Some((index, hit));
                }
            }
        }
        let mut cursors: [Cursor; N] =
            core::array::from_fn(|lane| self.bvh.cursor(&rays[lane], reaches[lane]));
        let mut walking = (0..N).fold(Members::NONE, Members::with);
        let mut waiting = Members::NONE;
        let mut waiting_at = [0usize; N];
        loop {
            for lane in walking.lanes() {
                let ray = &rays[lane];
                while let Some(index) = self.bvh.next(&mut cursors[lane], ray, reaches[lane]) {
                    let index = index as usize;
                    let Some(object) = seen(index) else {
                        continue;
                    };
                    if let Shape::Lawn { .. } = object.shape {
                        waiting_at[lane] = index;
                        waiting = waiting.with(lane);
                        break;
                    }
                    let met = object
                        .shape
                        .intersect(ray, NEAR, reaches[lane], self.geometry());
                    if let Some(hit) = met {
                        reaches[lane] = hit.t;
                        found[lane] = Some((index, hit));
                    }
                }
            }
            let Some(first) = waiting.first() else {
                return;
            };
            let object = waiting_at[first];
            let at_lawn = waiting
                .lanes()
                .filter(|&lane| waiting_at[lane] == object)
                .fold(Members::NONE, Members::with);
            let mut hits = [None; N];
            if let Some(lawn) = self.lawn_of(object) {
                lawn.intersect_rays(rays, at_lawn, (NEAR, &reaches), self.geometry(), &mut hits);
            }
            for lane in at_lawn.lanes() {
                if let Some(hit) = hits[lane] {
                    reaches[lane] = hit.t;
                    found[lane] = Some((object, hit));
                }
                waiting = waiting.without(lane);
            }
            walking = at_lawn;
        }
    }

    /// The lawn object `index` is, if it is one.
    fn lawn_of(&self, index: usize) -> Option<&Lawn> {
        match self.objects.get(index)?.shape {
            Shape::Lawn { lawn } => self.lawns.get(lawn as usize),
            _ => None,
        }
    }

    /// How much of the light leaving `ray`'s origin arrives `reach` along
    /// it, starting within a medium absorbing `inside` if there is one:
    /// nothing past an opaque object, and filtered by each clear one.
    pub(crate) fn transmittance(&self, ray: &Ray, reach: f64, inside: Option<Vec3>) -> Vec3 {
        let mut kept = Vec3::ONE;
        // Where the ray first leaves the medium it starts in, if it does.
        let mut out = reach;
        let mut filter = |index: usize, hit: &Hit| {
            if let Some(passed) = self.objects.get(index).and_then(|object| object.filter) {
                kept = kept * passed;
            }
            if hit.normal.dot(ray.dir) > 0.0 {
                out = out.min(hit.t);
            }
        };
        for &index in &self.unbounded {
            match self.shadow_test(index, ray, reach) {
                Shadow::Opaque => return Vec3::ZERO,
                Shadow::Clear(hit) => filter(index, &hit),
                Shadow::Missed => {}
            }
        }
        let mut blocked = false;
        self.bvh.walk(ray, reach, |object, reach| {
            let index = object as usize;
            match self.shadow_test(index, ray, reach) {
                Shadow::Opaque => {
                    blocked = true;
                    Walk::Stop
                }
                Shadow::Clear(hit) => {
                    filter(index, &hit);
                    Walk::Within(reach)
                }
                Shadow::Missed => Walk::Within(reach),
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

    /// The first sun lighting the scene: which way it lies, the cosine of its
    /// disc's angular radius, and its radiance.
    pub(crate) fn sun(&self) -> Option<(Vec3, f64, Vec3)> {
        self.lights.iter().find_map(|light| match *light {
            Light::Sun {
                toward,
                cos_radius,
                radiance,
            } => Some((toward, cos_radius, radiance)),
            _ => None,
        })
    }

    /// The blades of the sward `point` lies within, if it lies within one.
    pub(crate) fn canopy(&self, point: Vec3) -> Option<Canopy> {
        self.swards
            .iter()
            .find_map(|&index| self.lawns.get(index)?.canopy(point, &self.fields))
    }

    /// What object `index` does to light crossing it along `ray` nearer than
    /// `reach`: an opaque object is asked only whether it is in the way, which
    /// a forest answers at the first leaf, and a clear one where it is met.
    fn shadow_test(&self, index: usize, ray: &Ray, reach: f64) -> Shadow {
        let Some(object) = self.objects.get(index) else {
            return Shadow::Missed;
        };
        if !object.shape.casts_shadow() {
            return Shadow::Missed;
        }
        if object.filter.is_none() {
            return if object.shape.occludes(ray, NEAR, reach, self.geometry()) {
                Shadow::Opaque
            } else {
                Shadow::Missed
            };
        }
        object
            .shape
            .intersect(ray, NEAR, reach, self.geometry())
            .map_or(Shadow::Missed, Shadow::Clear)
    }
}

#[cfg(test)]
#[path = "scene_tests.rs"]
mod tests;
