//! Scenes composed at random: a ground, the pieces standing on it, what each
//! is made of, the lamps and the sky, and a camera that frames them.
//!
//! Every scene is set in one of the [`Setting`]s, each with its own ground,
//! sky, lighting and cast of pieces, and most with structural variants of
//! their own. Within a setting everything is drawn from the scene's seed —
//! which pieces, where, in what, lit from where, at what time of day, under
//! what weather, and seen from where — and each draw is bounded so every
//! scene is lit and framed to read well.

mod architecture;
mod footprint;
mod landscape;
mod plants;
mod still;
mod stones;
mod weather;
mod woodland;
mod work;

use alloc::collections::VecDeque;
use alloc::vec::Vec;
use core::f64::consts::{FRAC_PI_2, TAU};

use tairix_parallel::JobRunner;
use tairix_rng::{NonCryptoRng, RandU64};
use tairix_theme::color::srgb_to_linear;
use tairix_util::mathf;

use crate::camera::Camera;
use crate::deadwood::{log, stump, Top};
use crate::detail::{Densities, Detail};
use crate::grass::{Lawn, Tops};
use crate::heightfield::Heightfield;
use crate::land::{Build, Land};
use crate::light::Light;
use crate::material::{Finish, Foam, Material, Relief};
use crate::pigment::Pigment;
use crate::prototype::{Building, Prototype, BUILD_UNIT};
use crate::rock::{rock, Habit};
use crate::sample::mix64;
use crate::scene::{Exposure, Fog, Grid, Object, Parts};
use crate::shade::{Crown, Shades};
use crate::shape::{Aabb, Face, Geometry, Shape};
use crate::sky::{Dome, Sky};
use crate::terrain::{Cloudscape, Sea};
use crate::tree::{fern, palm, saguaro, Growth, Season, Species, Stock};
use crate::vector::{real, share, Frame, Pose, Ray, Vec3};
use footprint::Footprints;
use landscape::Lawning;
use landscape::{Scheme, Vantage};
use woodland::{Growing, Wood};

/// The settings a scene can be set in.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum Setting {
    /// Spheres and solids on a lacquered checkerboard under an open sky: the
    /// picture ray tracing began with.
    Classic,
    /// A few pieces under softboxes: a pale sweep, or black glass.
    Studio,
    /// Clusters of coloured crystal on black glass against the dusk.
    Crystals,
    /// Glass and chrome among glowing lamps on polished stone, at night.
    Nocturne,
    /// Soap bubbles drifting over a lawn or a still life.
    Bubbles,
    /// A colonnade, in marble or stone, under the day's sun.
    Colonnade,
    /// An arcade of arches on piers, or an aqueduct striding over a valley.
    Arcade,
    /// A domed rotunda of columns on a stepped base.
    Rotunda,
    /// Broken columns and fallen stones overgrown with grass.
    Ruins,
    /// Rolling hills of grass and flowers, trees and cloud.
    Meadow,
    /// A glade among trees, their crowns lit through.
    Forest,
    /// Mountains, snow on their peaks, over a still lake.
    Alpine,
    /// A sea's swells running in on a rocky shore.
    Coast,
    /// Dunes under a hard sun or a low one.
    Desert,
    /// Snow on hills and pines, under a pale sky or a clear night.
    Winter,
    /// Stone pillars standing in a shallow sea at sunset.
    Lagoon,
    /// Terraced mesas and the canyons between them.
    Canyon,
    /// A river valley, a road crossing it on a stone bridge.
    Valley,
    /// A monumental abstract sculpture standing out in a natural landscape.
    Sculpture,
}

impl Setting {
    /// Every setting.
    pub const ALL: [Self; 19] = [
        Self::Classic,
        Self::Studio,
        Self::Crystals,
        Self::Nocturne,
        Self::Bubbles,
        Self::Colonnade,
        Self::Arcade,
        Self::Rotunda,
        Self::Ruins,
        Self::Meadow,
        Self::Forest,
        Self::Alpine,
        Self::Coast,
        Self::Desert,
        Self::Winter,
        Self::Lagoon,
        Self::Canyon,
        Self::Valley,
        Self::Sculpture,
    ];

    /// The setting's name as a person reads it, which a picture of it is
    /// called after.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Classic => "Classic",
            Self::Studio => "Studio",
            Self::Crystals => "Crystals",
            Self::Nocturne => "Nocturne",
            Self::Bubbles => "Bubbles",
            Self::Colonnade => "Colonnade",
            Self::Arcade => "Arcade",
            Self::Rotunda => "Rotunda",
            Self::Ruins => "Ruins",
            Self::Meadow => "Meadow",
            Self::Forest => "Forest",
            Self::Alpine => "Alpine",
            Self::Coast => "Coast",
            Self::Desert => "Desert",
            Self::Winter => "Winter",
            Self::Lagoon => "Lagoon",
            Self::Canyon => "Canyon",
            Self::Valley => "Valley",
            Self::Sculpture => "Sculpture",
        }
    }
}

use work::{Fill, Form, Target};

/// A prototype a scene plans before it is traced: grown or built in the
/// work behind the composition.
#[allow(
    clippy::large_enum_variant,
    reason = "a scene plans a few dozen prototypes, and a box could not fail gracefully"
)]
#[derive(Copy, Clone, Debug)]
pub(super) enum Recipe {
    Tree {
        species: Species,
        height: f64,
        season: Season,
        stock: Stock,
        seed: u64,
    },
    Palm {
        height: f64,
        stock: Stock,
        fronds: u16,
        seed: u64,
    },
    Saguaro {
        height: f64,
        stock: Stock,
        seed: u64,
    },
    Fern {
        height: f64,
        stock: Stock,
        fronds: u16,
        seed: u64,
    },
    Rock {
        habit: Habit,
        stock: u16,
        seed: u64,
    },
    Log {
        length: f64,
        radius: f64,
        bark: u16,
        thrown: bool,
        seed: u64,
    },
    Stump {
        height: f64,
        radius: f64,
        top: Top,
        bark: u16,
        wood: u16,
        seed: u64,
    },
}

/// The prototypes a composition's recipes are growing: as many at once as
/// the runner runs, each a bounded step a unit on a core of its own.
#[derive(Debug)]
struct Grow {
    /// The next recipe not yet begun.
    next: usize,
    active: Vec<(usize, Work)>,
    grown: Vec<Option<Prototype>>,
}

/// A recipe being grown.
#[allow(
    clippy::large_enum_variant,
    reason = "a runner's width of these at once, and a box could not fail gracefully"
)]
#[derive(Debug)]
enum Work {
    /// Not yet begun.
    Planned(Recipe),
    /// A tree, growing its stems and then its hierarchy.
    Growing(Growth),
    /// Its parts made, its hierarchy being built.
    Indexing(Building),
    Grown(Prototype),
    /// The heap refused it.
    Refused,
}

impl Work {
    /// The next bounded step of the recipe: a tree's next stems or its
    /// hierarchy's next slice, or another kind's parts made whole and then
    /// its hierarchy a slice at a time.
    fn step(&mut self) {
        *self = match core::mem::replace(self, Self::Refused) {
            Self::Planned(recipe) => begun(&recipe).unwrap_or(Self::Refused),
            Self::Growing(mut growth) => match growth.step() {
                Some(true) => growth.finish().map_or(Self::Refused, Self::Grown),
                Some(false) => Self::Growing(growth),
                None => Self::Refused,
            },
            Self::Indexing(mut building) => {
                if building.step(BUILD_UNIT) {
                    Self::Grown(building.finish())
                } else {
                    Self::Indexing(building)
                }
            }
            done @ (Self::Grown(_) | Self::Refused) => done,
        };
    }
}

/// `recipe` begun: a tree's first stems, or another kind's parts made.
fn begun(recipe: &Recipe) -> Option<Work> {
    Some(match *recipe {
        Recipe::Tree {
            species,
            height,
            season,
            stock,
            seed,
        } => Work::Growing(Growth::new(&species, height, (season, stock), seed)?),
        Recipe::Palm {
            height,
            stock,
            fronds,
            seed,
        } => Work::Indexing(palm(height, stock, fronds, seed)?),
        Recipe::Saguaro {
            height,
            stock,
            seed,
        } => Work::Indexing(saguaro(height, stock, seed)?),
        Recipe::Fern {
            height,
            stock,
            fronds,
            seed,
        } => Work::Indexing(fern(height, stock, fronds, seed)?),
        Recipe::Rock { habit, stock, seed } => Work::Indexing(rock(habit, stock, seed)?),
        Recipe::Log {
            length,
            radius,
            bark,
            thrown,
            seed,
        } => Work::Indexing(log(length, radius, (bark, thrown), seed)?),
        Recipe::Stump {
            height,
            radius,
            top,
            bark,
            wood,
            seed,
        } => Work::Indexing(stump(height, radius, (top, bark, wood), seed)?),
    })
}

impl Grow {
    fn new(recipes: usize) -> Option<Self> {
        let mut grown = Vec::new();
        grown.try_reserve_exact(recipes).ok()?;
        grown.resize_with(recipes, || None);
        Some(Self {
            next: 0,
            active: Vec::new(),
            grown,
        })
    }

    /// The share of the recipes grown so far.
    fn done(&self) -> f64 {
        share(
            self.grown.iter().filter(|grown| grown.is_some()).count(),
            self.grown.len(),
        )
    }

    /// Grow the next step of as many recipes as `runner` runs at once;
    /// whether every recipe is grown, or `None` when the heap refused one.
    fn step(&mut self, recipes: &[Recipe], runner: &dyn JobRunner) -> Option<bool> {
        let width = runner.width().max(1);
        while self.active.len() < width {
            let Some(&recipe) = recipes.get(self.next) else {
                break;
            };
            self.active.try_reserve(1).ok()?;
            self.active.push((self.next, Work::Planned(recipe)));
            self.next += 1;
        }
        tairix_parallel::for_each(runner, &mut self.active, &|(_, work)| work.step());
        let (grown, mut refused) = (&mut self.grown, false);
        self.active.retain_mut(|(index, work)| match work {
            Work::Grown(_) => {
                if let (Work::Grown(prototype), Some(slot)) = (
                    core::mem::replace(work, Work::Refused),
                    grown.get_mut(*index),
                ) {
                    *slot = Some(prototype);
                }
                false
            }
            Work::Refused => {
                refused = true;
                false
            }
            Work::Planned(_) | Work::Growing(_) | Work::Indexing(_) => true,
        });
        if refused {
            return None;
        }
        Some(self.active.is_empty() && self.next >= recipes.len())
    }
}

/// A scene set out, with the work that makes it traceable queued behind it.
#[derive(Debug)]
pub(crate) struct Composition {
    stage: Stage,
    dice: Dice,
    aspect: f64,
    /// The picture's height in pixels, which how finely its lawns need drawing
    /// follows.
    height: u32,
    /// How the scene is seen and lit, and the camera it is seen through:
    /// known once its pieces stand, which on a land waits for the land.
    seen: Option<(Look, Camera)>,
    jobs: VecDeque<Job>,
    /// Whether the scene stands on a land, which most of its work then is.
    landed: bool,
    /// How many jobs its look queued once its pieces stood.
    settled: usize,
}

/// One piece of a composition's remaining work.
#[allow(
    clippy::large_enum_variant,
    reason = "a composition queues a handful of jobs, and a box could not fail gracefully"
)]
#[derive(Debug)]
enum Job {
    /// Fill a grid, a band of rows at a time.
    Fill(Fill),
    /// Build a land, site the eye on it, and set the scene out there.
    Land(Landing),
    /// Grow the woods and the sward set out on a land, then take the look.
    Plant(Planting),
    /// Grow the planned prototypes.
    Grow(Grow),
    /// Build the atmosphere's tables and the cloud's, then light the clouds
    /// by the air.
    Sky,
}

/// How far a unit of a job has brought it: done, or to be run again —
/// itself unfinished, or the job its end has led on to.
#[allow(
    clippy::large_enum_variant,
    reason = "passed back once a unit, and a box could not fail gracefully"
)]
enum Progress {
    Done,
    Again(Job),
}

/// A scene as its setting first sets it out.
#[allow(
    clippy::large_enum_variant,
    reason = "held once, while the scene is set out"
)]
enum Composed {
    /// Every piece standing, and seen as its look has it.
    Seen(Look),
    /// Standing on a land still to be built.
    Landed(Landing),
}

/// A land's woods and sward being grown, and how the scene is seen once
/// they stand.
#[derive(Debug)]
struct Planting {
    land: Land,
    growing: Growing,
    look: Look,
}

/// A land being built, and what the scene sets out on it once it is.
#[derive(Debug)]
struct Landing {
    build: Build,
    scheme: Scheme,
    /// Where the eye stands, once the scheme has sited it on the far land,
    /// and how far above the ground or water it stood there.
    vantage: Option<(Vantage, f64)>,
}

impl Composition {
    /// A scene set in `setting` under `seed`, for a picture of `size`, at
    /// `detail`; `None` when the heap will not hold it or the picture has no
    /// pixels.
    pub(crate) fn new(
        setting: Setting,
        seed: u64,
        size: (u32, u32),
        detail: Detail,
    ) -> Option<Self> {
        if size.0 == 0 || size.1 == 0 {
            return None;
        }
        let mut dice = Dice(NonCryptoRng::seed_from_u64(seed));
        let mut stage = Stage::new(detail.densities())?;
        stage.pixel = MIDDLING_FOV / f64::from(size.1);
        let composed = compose(setting, &mut stage, &mut dice)?;
        let mut jobs = VecDeque::new();
        jobs.try_reserve(MAX_FIELDS + 4).ok()?;
        let mut composition = Self {
            stage,
            dice,
            aspect: f64::from(size.0) / f64::from(size.1),
            height: size.1,
            seen: None,
            jobs,
            landed: matches!(composed, Composed::Landed(_)),
            settled: 0,
        };
        match composed {
            Composed::Seen(look) => composition.settle(look)?,
            Composed::Landed(landing) => composition.jobs.push_back(Job::Land(landing)),
        }
        Some(composition)
    }

    /// Take the scene's look once its pieces stand: queue the grids and the
    /// growing they still need, place the camera, and queue the sky seen
    /// from it.
    fn settle(&mut self, mut look: Look) -> Option<()> {
        let stage = &mut self.stage;
        self.jobs.try_reserve(stage.fills.len() + 2).ok()?;
        self.jobs.extend(stage.fills.drain(..).map(Job::Fill));
        if !stage.recipes.is_empty() {
            self.jobs
                .push_back(Job::Grow(Grow::new(stage.recipes.len())?));
        }
        let camera = stage.camera(&look.view, self.aspect);
        let eye = camera.eye();
        let pixel = camera.pixel_angle(self.height);
        for lawn in &mut stage.lawns {
            lawn.seen.pixel = pixel;
        }
        if let Some(bank) = look.sky.bank.as_mut() {
            bank.centre_on((eye.x, eye.z));
        }
        if let Dome::Air(atmosphere) = &mut look.sky.dome {
            atmosphere.place_eye(eye);
            self.jobs.push_back(Job::Sky);
        }
        self.seen = Some((look, camera));
        self.settled = self.jobs.len();
        Some(())
    }

    /// Whether the scene stands on a land.
    pub(crate) const fn landed(&self) -> bool {
        self.landed
    }

    /// How far the queued work has come, as a share: on a land, its building
    /// and then its planting take most of it, and the grids, growing and sky
    /// its look queued the rest.
    pub(crate) fn done(&self) -> f64 {
        let (from, span) = if self.landed {
            (0.78, 0.22)
        } else {
            (0.0, 1.0)
        };
        match self.jobs.front() {
            Some(Job::Land(landing)) => 0.44 * landing.build.done(),
            Some(Job::Plant(planting)) => 0.44 + 0.34 * planting.growing.done(),
            Some(front) => {
                let finished = self.settled.saturating_sub(self.jobs.len());
                let within = (real(finished) + self.job_done(front)) / real(self.settled.max(1));
                from + span * within.min(1.0)
            }
            None => 1.0,
        }
    }

    /// How far one of the look's queued jobs has come.
    fn job_done(&self, job: &Job) -> f64 {
        match job {
            Job::Fill(fill) => match fill.target {
                Target::Field(index) => self
                    .stage
                    .fields
                    .get(index)
                    .map_or(1.0, |field| fill.done(field.rows(), Some(field))),
                Target::Clouds => self
                    .seen
                    .as_ref()
                    .and_then(|(look, _)| look.sky.clouds.as_ref())
                    .map_or(1.0, |clouds| fill.done(clouds.rows(), None)),
            },
            Job::Grow(grow) => grow.done(),
            Job::Land(_) | Job::Plant(_) | Job::Sky => 0.0,
        }
    }

    /// Do the queued work a unit at a time across `runner` until `spent`
    /// answers — at least one unit — and answer whether all of it is done;
    /// `None` when the heap refused it.
    pub(crate) fn advance(
        &mut self,
        runner: &dyn JobRunner,
        spent: &mut dyn FnMut() -> bool,
    ) -> Option<bool> {
        while let Some(job) = self.jobs.pop_front() {
            if let Progress::Again(unfinished) = self.run(job, runner)? {
                // Put back where it was taken from, into the room its taking
                // left: no allocation.
                self.jobs.push_front(unfinished);
            }
            if self.jobs.is_empty() {
                break;
            }
            if spent() {
                return Some(false);
            }
        }
        Some(true)
    }

    /// A unit of `job`: how far it has come, or `None` when the heap refused
    /// it.
    fn run(&mut self, job: Job, runner: &dyn JobRunner) -> Option<Progress> {
        let again = |unfinished: bool, job: Job| {
            if unfinished {
                Progress::Again(job)
            } else {
                Progress::Done
            }
        };
        Some(match job {
            Job::Fill(mut fill) => {
                let clouds = self
                    .seen
                    .as_mut()
                    .and_then(|(look, _)| look.sky.clouds.as_mut());
                let grids = work::Grids {
                    fields: &mut self.stage.fields,
                    clouds,
                };
                let whole = fill.step(grids, runner)?;
                again(!whole, Job::Fill(fill))
            }
            Job::Land(landing) => self.land(landing, runner)?,
            Job::Plant(mut planting) => {
                if planting
                    .growing
                    .step(&mut self.stage, (&planting.land, runner))?
                {
                    self.settle(planting.look)?;
                    Progress::Done
                } else {
                    Progress::Again(Job::Plant(planting))
                }
            }
            Job::Grow(mut grow) => {
                if grow.step(&self.stage.recipes, runner)? {
                    let grown = core::mem::take(&mut grow.grown);
                    self.stage.prototypes.try_reserve_exact(grown.len()).ok()?;
                    for prototype in grown {
                        self.stage.prototypes.push(prototype?);
                    }
                    Progress::Done
                } else {
                    Progress::Again(Job::Grow(grow))
                }
            }
            Job::Sky => {
                let (look, _) = self.seen.as_mut()?;
                let built = look.sky.build(runner)?;
                again(!built, Job::Sky)
            }
        })
    }

    /// A unit of building `landing`'s land: siting the eye once the far land
    /// stands, and setting the scene out once all of it does. The landing
    /// again while it is unfinished, then its woods to grow, if it has any.
    fn land(&mut self, mut landing: Landing, runner: &dyn JobRunner) -> Option<Progress> {
        if !landing.build.step(&mut self.stage.fields, runner)? {
            return Some(Progress::Again(Job::Land(landing)));
        }
        if landing.build.waiting() {
            let survey = landing.build.survey(&self.stage.fields)?;
            let (vantage, siting) = landing.scheme.site(&mut self.dice, &survey)?;
            landing.vantage = vantage.map(|vantage| {
                (
                    vantage,
                    vantage.eye.y - survey.surface(vantage.eye.x, vantage.eye.z),
                )
            });
            landing
                .build
                .site(siting.focus, siting.lead, siting.path.as_deref())?;
            return Some(Progress::Again(Job::Land(landing)));
        }
        let mut land = landing.build.finish()?;
        self.stage.footprints.index(land.centre, land.reach)?;
        // The finer land laid about the eye stands higher or lower than the
        // far land it was sited on: the eye keeps its rise above what is
        // really there, rather than ending up beneath it.
        let fields = &self.stage.fields;
        let vantage = landing.vantage.map(|(mut vantage, rise)| {
            vantage.eye.y = land.surface(fields, vantage.eye.x, vantage.eye.z) + rise;
            vantage
        });
        let look = landing
            .scheme
            .finish(&mut self.stage, &mut self.dice, &mut land, vantage)?;
        if let Some(growing) = Growing::from(&mut self.stage, &mut self.dice) {
            return Some(Progress::Again(Job::Plant(Planting {
                land,
                growing,
                look,
            })));
        }
        self.settle(look)?;
        Some(Progress::Done)
    }

    /// Everything the scene is, once its work is done.
    pub(crate) fn finish(self) -> Option<Parts> {
        if !self.jobs.is_empty() {
            return None;
        }
        let (look, camera) = self.seen?;
        Some(self.stage.finish(look, camera))
    }
}

/// The scene `setting` sets out on `stage` from `dice`; `None` when the heap
/// will not hold it.
fn compose(setting: Setting, stage: &mut Stage, dice: &mut Dice) -> Option<Composed> {
    Some(match setting {
        Setting::Classic => Composed::Seen(still::classic(stage, dice)?),
        Setting::Studio => Composed::Seen(still::studio(stage, dice)?),
        Setting::Crystals => Composed::Seen(still::crystals(stage, dice)?),
        Setting::Nocturne => Composed::Seen(still::nocturne(stage, dice)?),
        Setting::Bubbles => still::bubbles(stage, dice)?,
        Setting::Colonnade => architecture::colonnade(stage, dice)?,
        Setting::Arcade => architecture::arcade(stage, dice)?,
        Setting::Rotunda => architecture::rotunda(stage, dice)?,
        Setting::Ruins => architecture::ruins(stage, dice)?,
        Setting::Meadow => landscape::meadow(stage, dice)?,
        Setting::Forest => landscape::forest(stage, dice)?,
        Setting::Alpine => landscape::alpine(stage, dice)?,
        Setting::Coast => landscape::coast(stage, dice)?,
        Setting::Desert => landscape::desert(stage, dice)?,
        Setting::Winter => landscape::winter(stage, dice)?,
        Setting::Lagoon => landscape::lagoon(stage, dice)?,
        Setting::Canyon => landscape::canyon(stage, dice)?,
        Setting::Valley => landscape::valley(stage, dice)?,
        Setting::Sculpture => landscape::sculpture(stage, dice)?,
    })
}

/// The scene's draws.
struct Dice(NonCryptoRng);

impl core::fmt::Debug for Dice {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Dice").finish_non_exhaustive()
    }
}

impl Dice {
    fn unit(&mut self) -> f64 {
        self.0.next_f64()
    }

    fn range(&mut self, low: f64, high: f64) -> f64 {
        low + (high - low) * self.unit()
    }

    /// An angle between `low` and `high` degrees, in radians.
    fn angle(&mut self, low: f64, high: f64) -> f64 {
        self.range(low, high).to_radians()
    }

    fn chance(&mut self, odds: f64) -> bool {
        self.unit() < odds
    }

    /// A whole number in `low..=high`.
    fn count(&mut self, low: u32, high: u32) -> u32 {
        let span = u64::from(high.saturating_sub(low)) + 1;
        low + u32::try_from(self.0.next_below(span)).unwrap_or(0)
    }

    fn pick<T: Copy>(&mut self, items: &[T]) -> Option<T> {
        let at = self.0.next_below(u64::try_from(items.len()).unwrap_or(0));
        items.get(usize::try_from(at).ok()?).copied()
    }

    fn seed(&mut self) -> u32 {
        self.0.next_u32()
    }

    /// A seed wide enough for a generator of its own.
    fn wide(&mut self) -> u64 {
        self.0.next_u64()
    }

    /// The draws of the `index`th of the streams keyed from `seed`.
    fn keyed(seed: u64, index: usize) -> Self {
        let index = u64::try_from(index).unwrap_or(u64::MAX);
        Self(NonCryptoRng::seed_from_u64(mix64(
            seed ^ mix64(index.wrapping_add(1)),
        )))
    }

    /// Either way round.
    fn sign(&mut self) -> f64 {
        if self.chance(0.5) {
            1.0
        } else {
            -1.0
        }
    }
}

/// How a scene is seen and lit overall, once its pieces stand.
#[derive(Debug)]
struct Look {
    sky: Sky,
    fog: Option<Fog>,
    exposure: Exposure,
    /// Roughly how much light falls on the scene, as a share of a clear
    /// day's: what the glow within water is scaled by.
    daylight: f64,
    view: View,
}

/// Where the camera stands.
#[derive(Debug)]
enum View {
    /// Looking at the pieces from the side `yaw` names, `elevation` above
    /// them, through `fov`, drawn back until they fill `fill` of the
    /// picture; the lens `aperture` of the distance.
    Framed {
        yaw: f64,
        elevation: f64,
        fov: f64,
        fill: f64,
        aperture: f64,
    },
    /// At `eye` looking at `target` through `fov`, focused there through a
    /// lens `aperture` across.
    Placed {
        eye: Vec3,
        target: Vec3,
        fov: f64,
        aperture: f64,
    },
}

/// The pieces of a scene as they are set out.
#[derive(Debug)]
struct Stage {
    objects: Vec<Object>,
    faces: Vec<Face>,
    fields: Vec<Heightfield>,
    prototypes: Vec<Prototype>,
    lawns: Vec<Lawn>,
    /// The prototypes planned, grown in order into `prototypes` before the
    /// scene is traced.
    recipes: Vec<Recipe>,
    fills: Vec<Fill>,
    materials: Vec<Material>,
    lights: Vec<Light>,
    /// What stands on the ground, as circles a new piece keeps clear of.
    footprints: Footprints,
    /// The crowns of the trees standing, as where each trunk stands and how
    /// far its crown reaches: the shade the ground beneath them is in.
    canopies: Vec<Crown>,
    /// The woods a scene sets out, and the sward under them, grown once the
    /// land stands.
    woods: Vec<Wood>,
    sward: Option<Lawning>,
    /// The shade the woods cast once they stand.
    shades: Option<Shades>,
    /// Around every piece the camera frames.
    subject: Aabb,
    /// About the angle a pixel spans, the picture seen through a middling
    /// lens: how small a piece may be and still be seen.
    pixel: f64,
    /// How much the scene sets out.
    densities: &'static Densities,
}

/// About how tall a picture a landscape is seen through, in radians.
const MIDDLING_FOV: f64 = 1.0;

/// The most cells a side of a lawn's canopy grid holds: a block of its
/// cells to a vertex, as many cells a block as keeps it within this.
const CANOPY_SIDE: usize = 2048;

/// The most of each a stage holds: bounds on what one scene may cost, not
/// capacities a larger machine would want more of. Its objects are its
/// detail's to bound.
const MAX_FACES: usize = 4096;
const MAX_FIELDS: usize = 12;
const MAX_MATERIALS: usize = 256;
const MAX_LIGHTS: usize = 12;
const MAX_PROTOTYPES: usize = 96;
const MAX_WOODS: usize = 8;
const MAX_LAWNS: usize = 16;

/// How much of the light crossing a soap bubble its skin lets through, the
/// rest reflected by its two faces.
const BUBBLE_SHADOW: f64 = 0.85;

/// How much light a clear solid's shadow keeps once its refraction has bent
/// the rest aside, the caustic it would gather that light into not being
/// traced: enough that the shadow still reads.
const GLASS_SHADOW: f64 = 0.62;

impl Stage {
    fn new(densities: &'static Densities) -> Option<Self> {
        let mut stage = Self {
            objects: Vec::new(),
            faces: Vec::new(),
            fields: Vec::new(),
            prototypes: Vec::new(),
            lawns: Vec::new(),
            recipes: Vec::new(),
            fills: Vec::new(),
            materials: Vec::new(),
            lights: Vec::new(),
            footprints: Footprints::default(),
            canopies: Vec::new(),
            woods: Vec::new(),
            sward: None,
            shades: None,
            subject: Aabb::EMPTY,
            pixel: MIDDLING_FOV / 1080.0,
            densities,
        };
        let reserved = stage.objects.try_reserve(256).is_ok()
            && stage.faces.try_reserve(256).is_ok()
            && stage.fields.try_reserve_exact(MAX_FIELDS).is_ok()
            && stage.fills.try_reserve_exact(MAX_FIELDS + 1).is_ok()
            && stage.materials.try_reserve(64).is_ok()
            && stage.lights.try_reserve_exact(MAX_LIGHTS).is_ok();
        reserved.then_some(stage)
    }

    fn geometry(&self) -> Geometry<'_> {
        Geometry {
            faces: &self.faces,
            fields: &self.fields,
            prototypes: &self.prototypes,
            lawns: &self.lawns,
        }
    }

    fn material(&mut self, material: Material) -> Option<usize> {
        push(&mut self.materials, MAX_MATERIALS, material)
    }

    /// Plan a prototype of `recipe`: the index it will have among the
    /// scene's once it is grown.
    fn plan(&mut self, recipe: &Recipe) -> Option<u32> {
        u32::try_from(push(&mut self.recipes, MAX_PROTOTYPES, *recipe)?).ok()
    }

    /// Add `shape` in `material`, its pattern fixed in `texture`; `framed`
    /// when the camera must keep it in view.
    fn add(&mut self, shape: Shape, material: usize, texture: Pose, framed: bool) -> Option<usize> {
        let bounds = shape.bounds(self.geometry());
        let filter = self
            .materials
            .get(material)
            .and_then(|made| clear_filter(&made.finish, &shape, self.geometry()));
        if framed {
            if let Some(bounds) = bounds {
                self.subject = self.subject.union(bounds);
            }
        }
        push(
            &mut self.objects,
            self.densities.objects,
            Object {
                shape,
                material,
                texture,
                light: None,
                filter,
                in_view: true,
            },
        )
    }

    /// Add the bounded convex solid of `faces`, given in its own frame,
    /// placed at `pose`.
    fn hull(&mut self, pose: Pose, faces: &[Face], material: usize) -> Option<usize> {
        let extent = hull_extent(faces)?;
        let first = u32::try_from(self.faces.len()).ok()?;
        if self.faces.len() + faces.len() > MAX_FACES
            || self.faces.try_reserve(faces.len()).is_err()
        {
            return None;
        }
        self.faces.extend_from_slice(faces);
        let count = u32::try_from(faces.len()).ok()?;
        self.add(
            Shape::Hull {
                pose,
                first,
                count,
                extent,
            },
            material,
            pose,
            true,
        )
    }

    fn light(&mut self, light: Light) -> Option<usize> {
        push(&mut self.lights, MAX_LIGHTS, light)
    }

    /// A glowing sphere, which is both a piece and a lamp.
    fn orb(&mut self, centre: Vec3, radius: f64, radiance: Vec3) -> Option<()> {
        let glow = self.material(Material::new(
            Pigment::Solid(Vec3::ZERO),
            Finish::Glow { radiance },
        ))?;
        let object = self.add(
            Shape::Sphere { centre, radius },
            glow,
            Pose::new(centre, Frame::WORLD),
            true,
        )?;
        let light = self.light(Light::Orb {
            object: u32::try_from(object).ok()?,
            centre,
            radius,
            radiance,
        })?;
        self.link(object, light)
    }

    /// A glowing rectangle facing `edge_u × edge_v`: a softbox, kept out of
    /// the picture while its light and reflections stay in it.
    fn panel(&mut self, corner: Vec3, edge_u: Vec3, edge_v: Vec3, radiance: Vec3) -> Option<()> {
        let glow = self.material(Material::new(
            Pigment::Solid(Vec3::ZERO),
            Finish::Glow { radiance },
        ))?;
        let object = self.add(
            Shape::Quad {
                corner,
                edge_u,
                edge_v,
            },
            glow,
            Pose::new(corner, Frame::WORLD),
            false,
        )?;
        let light = self.light(Light::Panel {
            object: u32::try_from(object).ok()?,
            corner,
            edge_u,
            edge_v,
            radiance,
        })?;
        self.objects.get_mut(object)?.in_view = false;
        self.link(object, light)
    }

    fn link(&mut self, object: usize, light: usize) -> Option<()> {
        self.objects.get_mut(object)?.light = Some(light);
        Some(())
    }

    /// Level ground at `height`, in `material`.
    fn ground(&mut self, height: f64, material: usize) -> Option<usize> {
        self.add(
            Shape::Plane {
                normal: Vec3::UP,
                offset: height,
            },
            material,
            Pose::new(Vec3::ZERO, Frame::WORLD),
            false,
        )
    }

    /// The open sea of `sea`, its grid `cells` a side repeating every
    /// `period`, in `material`.
    fn sea(&mut self, sea: Sea, (period, cells): (f64, usize), material: usize) -> Option<u32> {
        let step = period / f64::from(u32::try_from(cells).ok()?);
        let field = self.grid(
            Heightfield::new(cells, (0.0, 0.0), step, true)?,
            Form::Sea(sea),
        )?;
        self.add(
            Shape::Land { field },
            material,
            Pose::new(Vec3::ZERO, Frame::WORLD),
            false,
        )?;
        Some(field)
    }

    /// Take `field` among the scene's grids, to be filled from `form`.
    fn grid(&mut self, field: Heightfield, form: Form) -> Option<u32> {
        let index = self.field(field)?;
        push(
            &mut self.fills,
            MAX_FIELDS + 1,
            Fill::new(Target::Field(index as usize), form),
        )?;
        Some(index)
    }

    /// The grid of how high `lawn`'s shoots stand, a vertex at the middle of
    /// each block of its cells and one more beyond each end — blocks no more
    /// than a canopy grid's side across it, and where a block is a cell, how
    /// the cell grows — taken among the scene's grids to be filled.
    fn tops(&mut self, lawn: &Lawn) -> Option<Tops> {
        let cells = |low: f64, high: f64| mathf::ceil((high - low) / lawn.cell).max(1.0);
        let across = cells(lawn.from.0, lawn.to.0).max(cells(lawn.from.1, lawn.to.1));
        let block = mathf::ceil(across / real(CANOPY_SIDE - 2)).max(1.0);
        let blocks = usize::try_from(mathf::round_i32(mathf::ceil(across / block))).ok()?;
        let spacing = block * lawn.cell;
        let origin = (lawn.from.0 - 0.5 * spacing, lawn.from.1 - 0.5 * spacing);
        let mut grid = Heightfield::new(blocks + 1, origin, spacing, false)?;
        if !grid.carry_attributes() {
            return None;
        }
        let block = u32::try_from(mathf::round_i32(block)).ok()?;
        let field = self.grid(
            grid,
            Form::Canopy {
                lawn: lawn.copied()?,
                block,
            },
        )?;
        Some(Tops { field, block })
    }

    /// Take `field` among the scene's grids, to be built as its land is.
    fn field(&mut self, field: Heightfield) -> Option<u32> {
        u32::try_from(push(&mut self.fields, MAX_FIELDS, field)?).ok()
    }

    /// Fill the sky's cloud layer from `form`, once the layer is set.
    fn clouds(&mut self, form: Cloudscape) -> Option<()> {
        push(
            &mut self.fills,
            MAX_FIELDS + 1,
            Fill::new(Target::Clouds, Form::Clouds(form)),
        )
        .map(|_| ())
    }

    /// A place on the ground within `spread` of `centre` a piece `radius`
    /// across leaves the others clear of, claimed for it; `None` when none
    /// was found in a fair number of tries.
    fn place(
        &mut self,
        dice: &mut Dice,
        (centre, spread): ((f64, f64), f64),
        radius: f64,
    ) -> Option<(f64, f64)> {
        for _ in 0..48 {
            let angle = dice.range(0.0, TAU);
            let distance = spread * mathf::sqrt(dice.unit());
            let (x, z) = (
                centre.0 + distance * mathf::cos(angle),
                centre.1 + distance * mathf::sin(angle),
            );
            if self.footprints.clear((x, z), radius) {
                self.footprints.claim((x, z), radius)?;
                return Some((x, z));
            }
        }
        None
    }

    /// Whether a piece `radius` across at `at` stays clear of the rest.
    fn clear(&self, at: (f64, f64), radius: f64) -> bool {
        self.footprints.clear(at, radius)
    }

    /// Record a tree's crown reaching `reach` from its trunk at `(x, z)`.
    fn canopy(&mut self, (x, z): (f64, f64), reach: f64) -> Option<()> {
        push(&mut self.canopies, usize::MAX, ((x, z), reach)).map(|_| ())
    }

    /// Ask for `wood` to be grown once the land stands.
    fn sow(&mut self, wood: Wood) -> Option<()> {
        push(&mut self.woods, MAX_WOODS, wood).map(|_| ())
    }

    /// How many more objects the stage will hold.
    const fn room(&self) -> usize {
        self.densities.objects.saturating_sub(self.objects.len())
    }

    /// Mark `at` taken by a piece `radius` across placed there by hand.
    fn claim(&mut self, at: (f64, f64), radius: f64) -> Option<()> {
        self.footprints.claim(at, radius)
    }

    /// Mark taken the strip `half` either side of the line from `from` to
    /// `to`, as circles close enough along it to leave no gap at its edges:
    /// what a bridge's deck or a row of arches keeps clear beneath it.
    fn claim_along(&mut self, from: (f64, f64), to: (f64, f64), half: f64) -> Option<()> {
        let (dx, dz) = (to.0 - from.0, to.1 - from.1);
        let steps = mathf::ceil(mathf::hypot(dx, dz) / half.max(1e-3)).max(1.0);
        let radius = mathf::hypot(half, 0.5 * mathf::hypot(dx, dz) / steps);
        for step in 0..=u32::try_from(mathf::round_i32(steps)).ok()? {
            let t = f64::from(step) / steps;
            self.claim((from.0 + dx * t, from.1 + dz * t), radius)?;
        }
        Some(())
    }

    /// The camera `view` places, for a picture `aspect` times as wide as it
    /// is tall.
    fn camera(&self, view: &View, aspect: f64) -> Camera {
        match *view {
            View::Framed {
                yaw,
                elevation,
                fov,
                fill,
                aperture,
            } => frame(
                self.subject,
                (yaw, elevation),
                (fov, fill, aperture),
                aspect,
            ),
            View::Placed {
                eye,
                target,
                fov,
                aperture,
            } => Camera::looking(
                eye,
                target,
                fov,
                aspect,
                (aperture, (target - eye).length()),
            ),
        }
    }

    fn finish(self, look: Look, camera: Camera) -> Parts {
        Parts {
            objects: self.objects,
            faces: self.faces,
            fields: self.fields,
            prototypes: self.prototypes,
            lawns: self.lawns,
            materials: self.materials,
            lights: self.lights,
            sky: look.sky,
            fog: look.fog,
            shades: self.shades,
            camera,
            exposure: look.exposure,
            daylight: look.daylight,
        }
    }
}

/// The box the bounded convex solid of `faces` lies within: around its
/// corners, each where three faces meet within all the rest.
fn hull_extent(faces: &[Face]) -> Option<Aabb> {
    let mut extent = Aabb::EMPTY;
    for (i, a) in faces.iter().enumerate() {
        for (j, b) in faces.iter().enumerate().skip(i + 1) {
            for c in faces.iter().skip(j + 1) {
                let (bc, ca, ab) = (
                    b.normal.cross(c.normal),
                    c.normal.cross(a.normal),
                    a.normal.cross(b.normal),
                );
                let det = a.normal.dot(bc);
                if det.abs() < 1e-9 {
                    continue;
                }
                let corner = (bc * a.offset + ca * b.offset + ab * c.offset) / det;
                let inside = faces.iter().all(|face| {
                    face.normal.dot(corner) <= face.offset + 1e-7 * (1.0 + face.offset.abs())
                });
                if inside {
                    extent = extent.including(corner);
                }
            }
        }
    }
    (extent.min.x <= extent.max.x).then(|| extent.padded())
}

/// `item` pushed onto `list` unless it already holds `most`, or the heap
/// will not grow it; its index.
fn push<T>(list: &mut Vec<T>, most: usize, item: T) -> Option<usize> {
    if list.len() >= most || list.try_reserve(1).is_err() {
        return None;
    }
    list.push(item);
    Some(list.len() - 1)
}

/// What light passing through a clear `shape` of `finish` keeps.
fn clear_filter(finish: &Finish, shape: &Shape, geometry: Geometry<'_>) -> Option<Vec3> {
    let (ior, absorb) = match *finish {
        Finish::Glass { ior, absorb, .. } => (ior, absorb),
        // A bubble's two faces each reflect a little and bend nothing.
        Finish::Film { shell: true, .. } => return Some(Vec3::splat(BUBBLE_SHADOW)),
        _ => return None,
    };
    let reflected = ((ior - 1.0) / (ior + 1.0)) * ((ior - 1.0) / (ior + 1.0));
    // Open water's own depth is absorbed on the way out of it; a solid is
    // crossed twice, through about two thirds of its breadth.
    let (surfaces, depth) = match shape {
        Shape::Plane { .. } | Shape::Quad { .. } | Shape::Land { .. } => (1.0, 0.0),
        other => (
            2.0,
            other.bounds(geometry).map_or(0.0, |bounds| {
                let size = bounds.max - bounds.min;
                0.66 * size.x.min(size.y).min(size.z)
            }),
        ),
    };
    let kept = mathf::exp(surfaces * mathf::ln(1.0 - reflected));
    let bent = if surfaces > 1.0 { GLASS_SHADOW } else { 1.0 };
    Some((absorb * -depth).exp() * (kept * bent))
}

/// The camera looking at `subject` from `(yaw, elevation)` through `fov`,
/// drawn back just far enough that every corner of it lies within `fill` of
/// the picture's frame, focused on its middle through a lens `aperture` of
/// the distance.
fn frame(
    subject: Aabb,
    (yaw, elevation): (f64, f64),
    (fov, fill, aperture): (f64, f64, f64),
    aspect: f64,
) -> Camera {
    let centre = subject.centre();
    let back = direction(yaw, 0.0, elevation);
    let right = (-back).cross(Vec3::UP).normalized();
    let up = right.cross(-back);
    let tan = mathf::tan(0.5 * fov) * fill;
    let mut distance: f64 = 1.0;
    for corner in 0..8u8 {
        let point = Vec3::new(
            if corner & 1 == 0 {
                subject.min.x
            } else {
                subject.max.x
            },
            if corner & 2 == 0 {
                subject.min.y
            } else {
                subject.max.y
            },
            if corner & 4 == 0 {
                subject.min.z
            } else {
                subject.max.z
            },
        );
        let offset = point - centre;
        let (across, rise, toward) = (offset.dot(right), offset.dot(up), offset.dot(back));
        distance = distance
            .max(toward + across.abs() / (tan * aspect))
            .max(toward + rise.abs() / tan);
    }
    let eye = centre + back * distance;
    Camera::looking(eye, centre, fov, aspect, (aperture * distance, distance))
}

/// The unit direction at compass angle `yaw + turn` and `elevation` above
/// the horizon.
fn direction(yaw: f64, turn: f64, elevation: f64) -> Vec3 {
    let heading = yaw + turn;
    let level = mathf::cos(elevation);
    Vec3::new(
        mathf::sin(heading) * level,
        mathf::sin(elevation),
        mathf::cos(heading) * level,
    )
}

/// A colour authored as sRGB `0xRRGGBB`, as linear light.
fn rgb(hex: u32) -> Vec3 {
    let channel = |shift: u32| srgb_to_linear(f64::from((hex >> shift) & 0xff) / 255.0);
    Vec3::new(channel(16), channel(8), channel(0))
}

/// A sun of `irradiance` toward `toward`, its disc `radius` degrees across:
/// the disc's radiance is what spreads that much light over its solid angle.
fn sun(toward: Vec3, radius: f64, colour: Vec3, irradiance: f64) -> Light {
    let cos_radius = mathf::cos(radius.to_radians());
    let solid = TAU * (1.0 - cos_radius);
    Light::Sun {
        toward,
        cos_radius,
        radiance: colour * (irradiance / solid.max(1e-12)),
    }
}

/// A turn about a random axis, for a pattern no two pieces share.
fn tumble(dice: &mut Dice) -> Frame {
    Frame::turned(dice.range(0.0, TAU), dice.range(-FRAC_PI_2, FRAC_PI_2))
}

/// The solids a gem can be cut as.
#[derive(Copy, Clone, Debug)]
enum Cut {
    Octahedron,
    Icosahedron,
    Dodecahedron,
}

const PHI: f64 = crate::sample::GOLDEN_RATIO;
const PHI_INV: f64 = PHI - 1.0;

/// The directions a cut's faces face, before they are made unit.
const OCTAHEDRON: [[f64; 3]; 8] = [
    [1.0, 1.0, 1.0],
    [1.0, 1.0, -1.0],
    [1.0, -1.0, 1.0],
    [1.0, -1.0, -1.0],
    [-1.0, 1.0, 1.0],
    [-1.0, 1.0, -1.0],
    [-1.0, -1.0, 1.0],
    [-1.0, -1.0, -1.0],
];
const ICOSAHEDRON: [[f64; 3]; 20] = [
    [1.0, 1.0, 1.0],
    [1.0, 1.0, -1.0],
    [1.0, -1.0, 1.0],
    [1.0, -1.0, -1.0],
    [-1.0, 1.0, 1.0],
    [-1.0, 1.0, -1.0],
    [-1.0, -1.0, 1.0],
    [-1.0, -1.0, -1.0],
    [0.0, PHI_INV, PHI],
    [0.0, PHI_INV, -PHI],
    [0.0, -PHI_INV, PHI],
    [0.0, -PHI_INV, -PHI],
    [PHI_INV, PHI, 0.0],
    [PHI_INV, -PHI, 0.0],
    [-PHI_INV, PHI, 0.0],
    [-PHI_INV, -PHI, 0.0],
    [PHI, 0.0, PHI_INV],
    [PHI, 0.0, -PHI_INV],
    [-PHI, 0.0, PHI_INV],
    [-PHI, 0.0, -PHI_INV],
];
const DODECAHEDRON: [[f64; 3]; 12] = [
    [0.0, 1.0, PHI],
    [0.0, 1.0, -PHI],
    [0.0, -1.0, PHI],
    [0.0, -1.0, -PHI],
    [1.0, PHI, 0.0],
    [1.0, -PHI, 0.0],
    [-1.0, PHI, 0.0],
    [-1.0, -PHI, 0.0],
    [PHI, 0.0, 1.0],
    [PHI, 0.0, -1.0],
    [-PHI, 0.0, 1.0],
    [-PHI, 0.0, -1.0],
];

/// The most faces any hull a stage builds has.
const MOST_FACES: usize = 20;

impl Cut {
    const ALL: [Self; 3] = [Self::Octahedron, Self::Icosahedron, Self::Dodecahedron];

    fn normals(self) -> &'static [[f64; 3]] {
        match self {
            Self::Octahedron => &OCTAHEDRON,
            Self::Icosahedron => &ICOSAHEDRON,
            Self::Dodecahedron => &DODECAHEDRON,
        }
    }

    /// How far the faces are from the centre, for a solid whose corners are
    /// a unit away.
    fn inradius(self) -> f64 {
        match self {
            Self::Octahedron => 0.577_350_269_189_625_8,
            Self::Icosahedron | Self::Dodecahedron => 0.794_654_472_291_766_1,
        }
    }
}

/// Pieces a stage sets out.
impl Stage {
    /// A sphere of `radius` resting at `base`.
    fn ball(&mut self, base: Vec3, radius: f64, material: usize, dice: &mut Dice) -> Option<usize> {
        let centre = base + Vec3::UP * radius;
        self.add(
            Shape::Sphere { centre, radius },
            material,
            Pose::new(centre, tumble(dice)),
            true,
        )
    }

    /// A box `half` its size each way, standing on `base`, turned `yaw`.
    fn block(&mut self, base: Vec3, half: Vec3, yaw: f64, material: usize) -> Option<usize> {
        self.slab(
            Pose::new(base + Vec3::UP * half.y, Frame::turned(yaw, 0.0)),
            half,
            material,
        )
    }

    /// A box `half` its size each way, centred and turned at `pose`.
    fn slab(&mut self, pose: Pose, half: Vec3, material: usize) -> Option<usize> {
        let faces = [
            Face {
                normal: Vec3::new(1.0, 0.0, 0.0),
                offset: half.x,
            },
            Face {
                normal: Vec3::new(-1.0, 0.0, 0.0),
                offset: half.x,
            },
            Face {
                normal: Vec3::UP,
                offset: half.y,
            },
            Face {
                normal: -Vec3::UP,
                offset: half.y,
            },
            Face {
                normal: Vec3::new(0.0, 0.0, 1.0),
                offset: half.z,
            },
            Face {
                normal: Vec3::new(0.0, 0.0, -1.0),
                offset: half.z,
            },
        ];
        self.hull(pose, &faces, material)
    }

    /// An upright frustum standing on `base`, `bottom` and `top` in radius.
    fn post(
        &mut self,
        base: Vec3,
        (bottom, top, height): (f64, f64, f64),
        material: usize,
        dice: &mut Dice,
    ) -> Option<usize> {
        let pose = Pose::new(base, Frame::turned(dice.range(0.0, TAU), 0.0));
        self.frustum(pose, (bottom, top, height), material, true)
    }

    /// A frustum from `pose`'s origin along its frame's `y`.
    fn frustum(
        &mut self,
        pose: Pose,
        (bottom, top, height): (f64, f64, f64),
        material: usize,
        framed: bool,
    ) -> Option<usize> {
        self.add(
            Shape::Frustum {
                pose,
                bottom,
                top,
                height,
            },
            material,
            pose,
            framed,
        )
    }

    /// A ring centred on `centre`, its axis the pose frame's `y`.
    fn ring(
        &mut self,
        centre: Vec3,
        frame: Frame,
        (major, minor): (f64, f64),
        material: usize,
    ) -> Option<usize> {
        self.arc(centre, frame, (major, minor, -1.0), material)
    }

    /// The part of a ring within the angle whose cosine is `arc` of its
    /// frame's −z.
    fn arc(
        &mut self,
        centre: Vec3,
        frame: Frame,
        (major, minor, arc): (f64, f64, f64),
        material: usize,
    ) -> Option<usize> {
        let pose = Pose::new(centre, frame);
        self.add(
            Shape::Torus {
                pose,
                major,
                minor,
                arc,
            },
            material,
            pose,
            true,
        )
    }

    /// An arch `span` wide standing upright across the heading `yaw`, its
    /// crown `rise` above `base`, its ring `thickness` thick.
    fn arch(
        &mut self,
        base: Vec3,
        yaw: f64,
        (span, thickness): (f64, f64),
        material: usize,
    ) -> Option<usize> {
        // Stood on end, the ring's frame −z points up, so the half the arc
        // keeps is the upper one.
        let upright = Frame::turned(yaw, FRAC_PI_2);
        self.arc(base, upright, (0.5 * span, 0.5 * thickness, 0.0), material)
    }

    /// A dome over `centre`.
    fn dome(&mut self, centre: Vec3, radius: f64, material: usize) -> Option<usize> {
        self.add(
            Shape::Dome { centre, radius },
            material,
            Pose::new(centre, Frame::WORLD),
            true,
        )
    }

    /// A gem cut as `cut`, its corners `size` from its centre, resting on one
    /// face at `base`, turned `yaw`.
    fn gem(&mut self, base: Vec3, cut: Cut, size: f64, yaw: f64, material: usize) -> Option<usize> {
        let mut faces = [Face {
            normal: Vec3::ZERO,
            offset: 0.0,
        }; MOST_FACES];
        let normals = cut.normals();
        let inradius = cut.inradius() * size;
        for (face, raw) in faces.iter_mut().zip(normals) {
            *face = Face {
                normal: Vec3::new(raw[0], raw[1], raw[2]).normalized(),
                offset: inradius,
            };
        }
        let down = faces.first()?.normal;
        let frame = Frame::WORLD
            .aligning(down, -Vec3::UP)
            .rotated_by(Frame::turned(yaw, 0.0));
        let pose = Pose::new(base + Vec3::UP * inradius, frame);
        self.hull(pose, faces.get(..normals.len())?, material)
    }

    /// A six-sided crystal growing from `base` along its frame's `y`,
    /// `radius` to its edges and `length` to where its point begins.
    fn crystal(
        &mut self,
        base: Vec3,
        frame: Frame,
        (radius, length): (f64, f64),
        material: usize,
    ) -> Option<usize> {
        // The terminal faces of quartz stand about 52 degrees off its axis.
        let (sin_cap, cos_cap) = (
            mathf::sin(51.8_f64.to_radians()),
            mathf::cos(51.8_f64.to_radians()),
        );
        let apothem = radius * mathf::cos(30.0_f64.to_radians());
        let buried = 0.35 * length;
        let mut faces = [Face {
            normal: -Vec3::UP,
            offset: buried,
        }; 13];
        let (pairs, _) = faces.as_chunks_mut::<2>();
        for (side, [wall, cap]) in (0u32..6).zip(pairs) {
            let angle = TAU * f64::from(side) / 6.0;
            let (cos, sin) = (mathf::cos(angle), mathf::sin(angle));
            *wall = Face {
                normal: Vec3::new(cos, 0.0, sin),
                offset: apothem,
            };
            *cap = Face {
                normal: Vec3::new(cos * sin_cap, cos_cap, sin * sin_cap),
                offset: apothem * sin_cap + length * cos_cap,
            };
        }
        self.hull(Pose::new(base, frame), &faces, material)
    }

    /// A lawn of grass in `material`.
    fn lawn(&mut self, lawn: Lawn, material: usize) -> Option<usize> {
        let index = u32::try_from(push(&mut self.lawns, MAX_LAWNS, lawn)?).ok()?;
        self.add(
            Shape::Lawn { lawn: index },
            material,
            Pose::new(Vec3::ZERO, Frame::WORLD),
            false,
        )
    }

    /// A tapering post from `from` to `to`, `bottom` and `top` in radius: a
    /// trunk, a limb, an arm.
    fn limb(
        &mut self,
        from: Vec3,
        to: Vec3,
        (bottom, top): (f64, f64),
        material: usize,
    ) -> Option<usize> {
        let axis = to - from;
        let length = axis.length();
        if length <= 0.0 {
            return None;
        }
        let frame = Frame::WORLD.aligning(Vec3::UP, axis / length);
        self.frustum(
            Pose::new(from, frame),
            (bottom, top, length),
            material,
            true,
        )
    }

    /// A boulder about `radius` across, bedded into the ground at `base`: a
    /// convex solid of faces spread over a sphere, each set in a little, and
    /// the whole flattened. Answers the boulder and its middle.
    fn boulder(
        &mut self,
        base: Vec3,
        radius: f64,
        material: usize,
        dice: &mut Dice,
    ) -> Option<(usize, Vec3)> {
        let squash = dice.range(0.55, 0.9);
        let mut faces = [Face {
            normal: Vec3::ZERO,
            offset: 0.0,
        }; MOST_FACES];
        let count = f64::from(u32::try_from(MOST_FACES).ok()?);
        for (index, face) in (0u32..).zip(faces.iter_mut()) {
            let rise = 1.0 - 2.0 * (f64::from(index) + 0.5) / count;
            let around = f64::from(index) * crate::sample::GOLDEN_ANGLE + dice.range(-0.25, 0.25);
            let level = mathf::sqrt(1.0 - rise * rise);
            let normal = Vec3::new(level * mathf::cos(around), rise, level * mathf::sin(around));
            // The plane through the flattened point, facing the flattened
            // normal: the face the unflattened one becomes.
            let point = normal * (radius * dice.range(0.8, 1.0));
            let flattened = Vec3::new(normal.x, normal.y / squash, normal.z).normalized();
            let offset = flattened.dot(Vec3::new(point.x, point.y * squash, point.z));
            *face = Face {
                normal: flattened,
                offset,
            };
        }
        let middle = base + Vec3::UP * (0.35 * radius * squash);
        let pose = Pose::new(
            middle,
            Frame::turned(dice.range(0.0, TAU), dice.range(-0.2, 0.2)),
        );
        Some((self.hull(pose, &faces, material)?, middle))
    }

    /// How far a line from `from`, inside object `index`, runs along `dir`
    /// before it leaves.
    fn exit(&self, index: usize, from: Vec3, dir: Vec3) -> Option<f64> {
        let object = self.objects.get(index)?;
        let ray = Ray::new(from, dir);
        let hit = object
            .shape
            .intersect(&ray, 0.0, f64::INFINITY, self.geometry())?;
        Some(hit.t)
    }

    /// A pyramid on a square base `half` across each way from its middle,
    /// its faces rising at `slope`, standing at `base` turned `yaw`.
    fn pyramid(
        &mut self,
        base: Vec3,
        half: f64,
        slope: f64,
        yaw: f64,
        material: usize,
    ) -> Option<usize> {
        let (sin, cos) = (mathf::sin(slope), mathf::cos(slope));
        let offset = half * sin;
        let faces = [
            Face {
                normal: Vec3::new(sin, cos, 0.0),
                offset,
            },
            Face {
                normal: Vec3::new(-sin, cos, 0.0),
                offset,
            },
            Face {
                normal: Vec3::new(0.0, cos, sin),
                offset,
            },
            Face {
                normal: Vec3::new(0.0, cos, -sin),
                offset,
            },
            Face {
                normal: -Vec3::UP,
                offset: 0.0,
            },
        ];
        self.hull(Pose::new(base, Frame::turned(yaw, 0.0)), &faces, material)
    }

    /// A square shaft `height` tall tapering from `bottom` to `top` across
    /// each way from its axis, capped by a pyramid `cap` tall: an obelisk,
    /// standing at `base` turned `yaw`.
    fn spire(
        &mut self,
        base: Vec3,
        (bottom, top, height, cap): (f64, f64, f64, f64),
        yaw: f64,
        material: usize,
    ) -> Option<usize> {
        let side = Vec3::new(height, bottom - top, 0.0).normalized();
        let point = Vec3::new(cap, top, 0.0).normalized();
        let (side_offset, point_offset) = (side.x * bottom, point.x * top + point.y * height);
        let mut faces = [Face {
            normal: -Vec3::UP,
            offset: 0.0,
        }; 9];
        let turns: [fn(Vec3) -> Vec3; 4] = [
            |v| v,
            |v| Vec3::new(-v.x, v.y, v.z),
            |v| Vec3::new(v.z, v.y, v.x),
            |v| Vec3::new(v.z, v.y, -v.x),
        ];
        let (pairs, _) = faces.as_chunks_mut::<2>();
        for ([shaft, tip], turn) in pairs.iter_mut().zip(turns) {
            *shaft = Face {
                normal: turn(side),
                offset: side_offset,
            };
            *tip = Face {
                normal: turn(point),
                offset: point_offset,
            };
        }
        self.hull(Pose::new(base, Frame::turned(yaw, 0.0)), &faces, material)
    }
}

/// Materials a stage makes.
impl Stage {
    fn coated(&mut self, pigment: Pigment, roughness: f64) -> Option<usize> {
        self.material(Material::new(pigment, Finish::Coated { roughness }))
    }

    fn metal(&mut self, reflectance: Vec3, roughness: f64) -> Option<usize> {
        self.material(Material::new(
            Pigment::Solid(reflectance),
            Finish::Metal { roughness },
        ))
    }

    /// Glass that lets `tint` through of the light crossing a unit of it.
    fn glass(&mut self, tint: Vec3, roughness: f64) -> Option<usize> {
        self.material(Material::new(
            Pigment::Solid(Vec3::ONE),
            Finish::Glass {
                ior: 1.52,
                absorb: absorption(tint),
                glow: Vec3::ZERO,
                roughness,
                dispersion: 0.0,
                foam: None,
            },
        ))
    }

    /// A dispersive gem: a diamond's index, or a flint glass's, its fire
    /// splitting white light into colour.
    fn fire(&mut self, dice: &mut Dice, tint: Vec3) -> Option<usize> {
        let (ior, dispersion) = if dice.chance(0.5) {
            (2.42, 0.05)
        } else {
            (1.72, 0.03)
        };
        self.material(Material::new(
            Pigment::Solid(Vec3::ONE),
            Finish::Glass {
                ior,
                absorb: absorption(tint),
                glow: Vec3::ZERO,
                roughness: 0.0,
                dispersion,
                foam: None,
            },
        ))
    }

    /// Water: clear at the surface, deepening to `glow` where it is deep,
    /// foaming above `foam` when it breaks.
    fn water(
        &mut self,
        absorb: Vec3,
        glow: Vec3,
        foam: Option<Foam>,
        relief: Relief,
    ) -> Option<usize> {
        self.material(
            Material::new(
                Pigment::Solid(Vec3::ONE),
                Finish::Glass {
                    ior: 1.333,
                    absorb,
                    glow,
                    roughness: 0.0,
                    dispersion: 0.0,
                    foam,
                },
            )
            .with_relief(relief),
        )
    }

    /// A soap bubble's film.
    fn bubble_film(&mut self, dice: &mut Dice) -> Option<usize> {
        self.material(Material::new(
            Pigment::Solid(Vec3::ONE),
            Finish::Film {
                thickness: (dice.range(150.0, 300.0), dice.range(550.0, 900.0)),
                index: 1.33,
                shell: true,
                seed: dice.seed(),
            },
        ))
    }

    /// One of the precious finishes a centrepiece is made in.
    fn precious(&mut self, dice: &mut Dice) -> Option<usize> {
        match dice.count(0, 13) {
            0 => self.metal(Vec3::splat(0.93), 0.0),
            1 => self.metal(GOLD, dice.pick(&[0.0, 0.08, 0.2])?),
            2 => self.metal(COPPER, dice.pick(&[0.0, 0.12, 0.25])?),
            3 => self.glass(Vec3::splat(0.97), 0.0),
            4 => {
                let tint = dice.pick(&GLASS_TINTS)?;
                self.glass(rgb(tint), 0.0)
            }
            5 => self.glass(Vec3::splat(0.96), dice.range(0.12, 0.22)),
            6 => self.material(
                Material::new(
                    Pigment::Solid(COPPER.lerp(GOLD, dice.unit())),
                    Finish::Metal { roughness: 0.1 },
                )
                .with_relief(Relief::Grain {
                    depth: 0.12,
                    scale: 14.0,
                    seed: dice.seed(),
                }),
            ),
            7 => {
                let metal = dice.pick(&[STEEL, ALUMINIUM, BRASS, TITANIUM])?;
                self.material(Material::new(
                    Pigment::Solid(metal),
                    Finish::Brushed {
                        along: dice.range(0.05, 0.12),
                        across: dice.range(0.35, 0.55),
                    },
                ))
            }
            8 => {
                let paint = rgb(dice.pick(&CAR_PAINTS)?);
                self.material(Material::new(
                    Pigment::Solid(paint),
                    Finish::Lacquer {
                        roughness: dice.range(0.25, 0.4),
                        flakes: dice.range(0.004, 0.012),
                    },
                ))
            }
            9 => self.material(Material::new(
                Pigment::Solid(rgb(dice.pick(&[0x10_10_14, 0x1A_10_24, 0x0C_18_14])?)),
                Finish::Film {
                    thickness: (dice.range(250.0, 350.0), dice.range(500.0, 700.0)),
                    index: 1.45,
                    shell: false,
                    seed: dice.seed(),
                },
            )),
            10 => {
                let tint = dice.pick(&GEM_TINTS)?;
                self.fire(dice, rgb(tint))
            }
            11 => self.stone(dice),
            12 => self.marble(dice),
            _ => self.metal(BRASS, dice.pick(&[0.05, 0.18])?),
        }
    }

    /// Lacquered wood, its rings about the piece's own axis.
    fn wood(&mut self, dice: &mut Dice) -> Option<usize> {
        let (light, dark) = dice.pick(&WOODS)?;
        self.coated(
            Pigment::Wood {
                light: rgb(light),
                dark: rgb(dark),
                scale: dice.range(5.0, 9.0),
                seed: dice.seed(),
            },
            0.18,
        )
    }

    /// A stone of marble, white or coloured.
    fn marble(&mut self, dice: &mut Dice) -> Option<usize> {
        let (base, vein) = dice.pick(&MARBLES)?;
        self.coated(
            Pigment::Marble {
                base: rgb(base),
                vein: rgb(vein),
                scale: dice.range(2.2, 4.0),
                seed: dice.seed(),
            },
            dice.pick(&[0.06, 0.1, 0.2])?,
        )
    }

    /// A crystalline or sedimentary stone: granite, terrazzo, sandstone,
    /// basalt.
    fn stone(&mut self, dice: &mut Dice) -> Option<usize> {
        let (base, a, b) = dice.pick(&GRANITES)?;
        self.coated(
            Pigment::Speckle {
                base: rgb(base),
                flecks: [rgb(a), rgb(b)],
                scale: dice.range(18.0, 40.0),
                seed: dice.seed(),
            },
            dice.pick(&[0.08, 0.3, 0.6])?,
        )
    }

    /// A painted or glazed finish in one of `colours`.
    fn paint(&mut self, dice: &mut Dice, colours: &[u32]) -> Option<usize> {
        let colour = dice.pick(colours)?;
        let roughness = dice.pick(&[0.05, 0.15, 0.3, 0.55, 0.85])?;
        self.coated(Pigment::Solid(rgb(colour)), roughness)
    }

    /// Something to stand a piece on: marble, granite, wood, or paint.
    fn plinth_stone(&mut self, dice: &mut Dice) -> Option<usize> {
        match dice.count(0, 3) {
            0 => self.marble(dice),
            1 => self.stone(dice),
            2 => self.wood(dice),
            _ => self.paint(dice, &[0xE8_E6_E0, 0x22_22_26, 0x8A_86_80, 0x5A_3C_2E]),
        }
    }
}

/// The absorption per unit length that lets `tint` of the light crossing a
/// unit through.
fn absorption(tint: Vec3) -> Vec3 {
    Vec3::new(
        -mathf::ln(tint.x.max(1e-3)),
        -mathf::ln(tint.y.max(1e-3)),
        -mathf::ln(tint.z.max(1e-3)),
    )
}

/// Reflectances at normal incidence.
const GOLD: Vec3 = Vec3::new(1.0, 0.766, 0.336);
const COPPER: Vec3 = Vec3::new(0.955, 0.638, 0.538);
const BRASS: Vec3 = Vec3::new(0.91, 0.78, 0.42);
const STEEL: Vec3 = Vec3::new(0.56, 0.57, 0.58);
const ALUMINIUM: Vec3 = Vec3::new(0.91, 0.92, 0.92);
const TITANIUM: Vec3 = Vec3::new(0.54, 0.5, 0.46);

/// Tints of coloured glass, as the light a unit of it lets through.
const GLASS_TINTS: [u32; 8] = [
    0xE0_7B_E0, 0x6F_C8_F0, 0xF4_C4_4C, 0x72_E0_A4, 0xF0_8A_A6, 0x9C_A8_F8, 0xF0_F0_F0, 0x5C_D8_D0,
];

/// Tints of gemstones.
const GEM_TINTS: [u32; 6] = [
    0xFA_FA_FA, 0xE8_60_7C, 0x60_A8_F0, 0x7C_E8_8C, 0xF8_D0_60, 0xC0_80_F0,
];

/// Car paints.
const CAR_PAINTS: [u32; 7] = [
    0xB0_14_1C, 0x14_3C_A0, 0x0E_6C_3C, 0x12_12_14, 0xC8_C8_CC, 0xE0_90_10, 0x6A_1C_8C,
];

/// Woods, as their light and dark growth.
const WOODS: [(u32, u32); 4] = [
    (0xC8_8A_52, 0x7A_44_22),
    (0xE0_B8_80, 0xA0_70_40),
    (0x6A_3A_22, 0x2C_14_0A),
    (0xD8_C8_A8, 0x9A_84_60),
];

/// Marbles, as base and vein.
const MARBLES: [(u32, u32); 7] = [
    (0xEE_EB_E5, 0x6E_6A_68),
    (0xE8_DC_CB, 0x9A_76_58),
    (0x2A_2A_2E, 0xD8_D2_C8),
    (0x9E_B8_A8, 0xF2_F0_E6),
    (0xD8_B4_B0, 0x7A_4C_48),
    (0x1C_3C_30, 0xC8_D8_C8),
    (0xE4_D8_B8, 0xB0_8C_44),
];

/// Granites and their like, as base and two flecks.
const GRANITES: [(u32, u32, u32); 5] = [
    (0xB8_B0_A8, 0x2A_28_28, 0xE8_D8_D0),
    (0xC0_9C_88, 0x3A_2C_28, 0xF0_E8_E0),
    (0x4A_4C_50, 0x1A_1A_1C, 0xA8_AC_B0),
    (0xD8_C8_A0, 0xA0_84_58, 0xF4_EC_D8),
    (0x7C_80_78, 0x30_34_30, 0xC8_C8_C0),
];

/// Bright, clean colours for paint and glaze.
const VIVID: [u32; 12] = [
    0xC8_2A_2A, 0xE8_8A_1C, 0xF0_C8_2A, 0x3C_A0_48, 0x1E_7A_C8, 0x6A_3C_B4, 0xE8_E6_E0, 0x22_22_26,
    0xD8_5A_8E, 0x2A_B0_B0, 0xF4_8C_70, 0x9C_C8_3C,
];

/// Checkerboards, as their two squares.
const CHECKERS: [(u32, u32); 6] = [
    (0xC8_2A_22, 0xF0_D2_3C),
    (0x14_1416, 0xEE_EC_E6),
    (0x1C_36_78, 0xEE_EC_E6),
    (0xA8_4A_2C, 0xF2_E6_CC),
    (0x2E_5E_4A, 0xEC_E4_D0),
    (0x6A_2C_6C, 0xF0_E4_B8),
];

/// Lamp colours for the night.
const LAMPS: [u32; 6] = [
    0xFF_A8_40, 0x4C_C8_FF, 0xFF_58_B8, 0x9C_FF_5C, 0xFF_F2_D8, 0xB0_78_FF,
];

/// A softbox `size` metres across centred at `at`, facing the middle of the
/// stage.
fn softbox(stage: &mut Stage, at: Vec3, (width, height): (f64, f64), radiance: Vec3) -> Option<()> {
    let facing = (Vec3::UP * 0.5 - at).normalized();
    let across = facing.cross(Vec3::UP).normalized() * width;
    let rise = across.cross(facing).normalized() * height;
    // Wound so its glowing face, `edge_u × edge_v`, looks at the stage.
    let corner = at - across * 0.5 - rise * 0.5;
    stage.panel(corner, rise, across, radiance)
}

#[cfg(test)]
#[path = "compose_tests.rs"]
mod tests;
