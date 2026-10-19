//! The reference scene: one realm, one cast and one moment, drawn the same
//! wherever it is drawn.
//!
//! Three consumers draw it, and they must agree to the bit: the
//! cross-target digest folds it at two fixed sizes, `Run --reference-scene`
//! holds it in a live window, and the host renders it at that window's size
//! to check the pixels the compositor scanned out. So everything that decides
//! a pixel is fixed here rather than taken from the machine: the realm's
//! seed, the cast and the moment it is caught at, and the material cache's
//! size. Pressure alone is the caller's, and a drawing it cost a tile is
//! refused rather than returned, so no caller can hold one as the reference.

use tairix_log::{Event, Sink};
use tairix_parallel::JobRunner;
use tairix_raster::surface::Surface;
use tairix_reclaim::PressureGauge;
use tairix_wintersun_art::cache::MaterialCache;
use tairix_wintersun_art::decal::Fray;
use tairix_wintersun_art::splat::Warp;
use tairix_wintersun_figure::actor::Actor;
use tairix_wintersun_figure::motion::{Clips, Kind};
use tairix_wintersun_figure::reference as figures;
use tairix_wintersun_figure::species::Species;
use tairix_wintersun_net::value::{EntityId, Facing, WorldPoint};
use tairix_wintersun_world::land::{Keeping, RealmLand};
use tairix_wintersun_world::params::{RealmParams, RealmSpec};
use tairix_wintersun_world::realm::RealmField;

use crate::budget::FRAME_NS;
use crate::camera::{realm_bounds, Camera, Tilt, Zoom};
use crate::error::ClientError;
use crate::figures::Cast;
use crate::frame::{Renderer, Scene, Stopped};
use crate::light::{Sky, Sun};
use crate::quality::{Detail, RenderScale};
use crate::terrain::{HeldGround, RoadDecals, CHUNK_SUB_UNITS};
use crate::view::{Reduced, Viewport};

/// The seed the reference realm is generated from.
pub const SEED: u64 = 0x5749_4E54_4552_4652;

/// The most bytes of worn ground the reference scene keeps between the
/// chunks its views need, which lie side by side and share most of it.
const WORN_BYTES: usize = 24 * 1024 * 1024;

/// How many chunks either side of the focus's own the scene solves: one, so
/// the ground about the cast is solved, the rest of the view is drawn from
/// far patches, and a guest solves the lot in a moment.
const SOLVE_RADIUS: i32 = 1;

/// Where the scene's camera looks: the middle of the chunk at the realm's
/// middle, so the cast stands on solved ground with solved ground about it.
const FOCUS: WorldPoint = WorldPoint {
    x: CHUNK_SUB_UNITS / 2,
    y: CHUNK_SUB_UNITS / 2,
};

/// Memory the reference scene's material cache is sized from.
///
/// Stated rather than discovered: a cache that refused a tile would draw that
/// material coarser and move the picture, so its size cannot be the
/// machine's. It admits every level of every ground at once, and is a budget
/// rather than an allocation.
pub const CACHE_BACKING_BYTES: usize = 256 * 1024 * 1024;

/// How the reference scene is looked at: through which view, from how far
/// back and how high, in what detail.
#[derive(Copy, Clone, Debug)]
pub struct Shot<'v> {
    /// The window, and the render scale the detail chose for it.
    pub view: &'v Viewport,
    /// How far back the camera stands.
    pub zoom: Zoom,
    /// How far down it looks.
    pub tilt: Tilt,
    /// The detail the frame is drawn at.
    pub detail: Detail,
}

impl<'v> Shot<'v> {
    /// How a window holding the scene frames it: at the default zoom and
    /// tilt in the finest detail, so its pixels depend on its size alone.
    #[must_use]
    pub const fn window(view: &'v Viewport) -> Self {
        Self {
            view,
            zoom: Zoom::DEFAULT,
            tilt: Tilt::DEFAULT,
            detail: Detail::FINEST,
        }
    }
}

/// The reference realm and its land, generated once, the ground art every
/// view of it shares, the ground the last view needed, and the worn ground
/// its chunks were solved from.
#[derive(Debug)]
pub struct World {
    land: RealmLand,
    roads: RoadDecals,
    warp: Warp,
    fray: Fray,
    ground: HeldGround,
    worn: Keeping,
}

impl World {
    /// Generate the reference realm, its land refined across `runner`.
    ///
    /// # Errors
    ///
    /// [`ClientError::World`] if the realm or its land does not generate,
    /// and whatever its roads refuse.
    pub fn generate(runner: &dyn JobRunner) -> Result<Self, ClientError> {
        let params = params()?;
        let field = RealmField::generate(params).map_err(|_| ClientError::World)?;
        let roads = RoadDecals::from_realm(&field)?;
        Ok(Self {
            land: RealmLand::new(field, runner).map_err(|_| ClientError::World)?,
            roads,
            warp: Warp::new(params.seed()),
            fray: Fray::new(params.seed()),
            ground: HeldGround::new(),
            worn: Keeping::within(WORN_BYTES),
        })
    }

    /// The realm's parameters.
    #[must_use]
    pub fn params(&self) -> RealmParams {
        self.land.field().params()
    }

    /// Draw the scene as `shot` sees it into `target`, which must be the
    /// shot's render extent, with all the ground the view needs made first so
    /// none is missing from it.
    ///
    /// # Errors
    ///
    /// [`ClientError::World`] for ground that does not generate,
    /// [`ClientError::Figure`] for a figure that cannot be built,
    /// [`ClientError::OutOfMemory`] where the cache refused a tile, whose
    /// material was then drawn coarser or in its flat tone, and whatever the
    /// renderer refuses. `target` holds a partial picture after any of them.
    pub fn draw(
        &mut self,
        clips: &Clips<'_>,
        shot: Shot<'_>,
        target: &mut Surface,
        renderer: &mut Renderer,
        cache: &mut MaterialCache,
        runner: &dyn JobRunner,
    ) -> Result<(), ClientError> {
        let params = self.params();
        let mut camera = Camera::new(FOCUS, shot.zoom, realm_bounds(params));
        camera.set_tilt(shot.tilt);
        self.ground.hold(
            &self.land,
            &mut self.worn,
            &camera,
            shot.view,
            SOLVE_RADIUS,
            runner,
        )?;
        let decals = self.roads.decals()?;
        let cast = cast(clips, camera.focus())?;
        renderer.render_whole(
            target,
            shot.view,
            &Scene {
                camera,
                ground: &self.ground,
                decals: &decals,
                fray: &self.fray,
                warp: &self.warp,
                sun: Sun::daylight(),
                sky: Sky::daylight(),
                detail: shot.detail,
                cast: &cast,
            },
            cache,
            runner,
            &Stopped,
        )?;
        if renderer.refused_tiles() != 0 {
            return Err(ClientError::OutOfMemory);
        }
        Ok(())
    }

    /// Draw the scene into `window`, a whole window's surface, exactly as a
    /// window of that size shows it: framed by [`Shot::window`], and past
    /// the software path's cap drawn into `reduced` and resampled up
    /// ([`Viewport::draw_into`]).
    ///
    /// # Errors
    ///
    /// [`ClientError::Viewport`] for a window with no pixels, and whatever
    /// [`Self::draw`] refuses.
    pub fn draw_window(
        &mut self,
        clips: &Clips<'_>,
        window: &mut Surface,
        reduced: &mut Reduced,
        renderer: &mut Renderer,
        cache: &mut MaterialCache,
        runner: &dyn JobRunner,
    ) -> Result<(), ClientError> {
        let view = Viewport::new(window.width(), window.height(), RenderScale::ONE)?;
        view.draw_into(window, reduced, |target| {
            self.draw(clips, Shot::window(&view), target, renderer, cache, runner)
        })
    }
}

/// One ask of the worker a window holding the scene is drawn by.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct SceneAsk {
    /// The extent of the frame owed, where one is.
    pub extent: Option<(u32, u32)>,
    /// Whether the pressure band moved since the last ask.
    pub give_back: bool,
}

/// What a window holding the scene still wants of the worker that draws it.
///
/// The worker's desk keeps only the newest ask and drops the answer to one a
/// newer ask finds in flight, so a frame outstanding is never asked again: a
/// give-back waits for it to land, and a frame's own tile lookups apply the
/// band it is drawn under.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct Owed {
    asked: Option<(u32, u32)>,
    give_back: bool,
}

impl Owed {
    /// Nothing asked for.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            asked: None,
            give_back: false,
        }
    }

    /// The pressure band moved: a give-back is owed.
    pub fn pressure_moved(&mut self) {
        self.give_back = true;
    }

    /// What to ask with the window `extent` big and a frame `shown` big on
    /// screen: the frame of an extent neither shown nor asked for, carrying
    /// any give-back owed, or the give-back alone once no frame is
    /// outstanding.
    pub fn ask(
        &mut self,
        extent: Option<(u32, u32)>,
        shown: Option<(u32, u32)>,
    ) -> Option<SceneAsk> {
        if extent.is_some() && extent != shown && extent != self.asked {
            self.asked = extent;
            return Some(SceneAsk {
                extent,
                give_back: core::mem::take(&mut self.give_back),
            });
        }
        (self.asked.is_none() && core::mem::take(&mut self.give_back)).then_some(SceneAsk {
            extent: None,
            give_back: true,
        })
    }

    /// A frame `at` big landed with the window `now` big: whether it is the
    /// window's to show. A frame for an extent the window has left is shown
    /// nowhere.
    pub fn landed(&mut self, at: (u32, u32), now: Option<(u32, u32)>) -> bool {
        if self.asked == Some(at) {
            self.asked = None;
        }
        now == Some(at)
    }
}

/// A material cache sized from [`CACHE_BACKING_BYTES`], admitting tiles as
/// `pressure` allows.
///
/// A drawing whose pixels must not depend on the machine takes it under
/// [`Unpressured`](tairix_reclaim::Unpressured); a live window holding the scene takes it under the
/// process's own gauge, so it gives memory back like any other cache, and a
/// drawing that cost a tile is refused ([`World::draw`]).
#[must_use]
pub fn cache(pressure: &'static (dyn PressureGauge + 'static)) -> MaterialCache {
    MaterialCache::new(
        "wintersun-reference",
        CACHE_BACKING_BYTES,
        pressure,
        &Discard,
    )
}

/// A sink that drops what it is given: a refused tile already fails the
/// drawing it was refused for, and a reference drawing keeps no journal.
struct Discard;

impl Sink for Discard {
    fn write_event(&self, _: &Event<'_>) {}
}

/// The realm the reference scene stands in.
///
/// Small and coarse: the scene is about the rendering, and a realm large
/// enough to be played in would spend a guest's whole budget being generated.
pub(crate) fn params() -> Result<RealmParams, ClientError> {
    let default = RealmParams::default_realm(SEED).spec();
    RealmParams::new(RealmSpec {
        // Thirty-two chunks rather than the default realm's two hundred and
        // fifty-six: large enough that the ground around the origin has the
        // slopes and the several grounds a real one does — measured, not
        // assumed — and small enough that a guest solves it in a moment. Its
        // relief shrinks with it, so its hills stand as the default realm's
        // do seen from the same height. Everything else is the default
        // realm's, so the origin stands in its middle latitudes.
        extent_chunks: 32,
        coarse_samples: 32,
        plates: 8,
        relief_units: default.relief_units * 32 / 256,
        ..default
    })
    .map_err(|_| ClientError::World)
}

/// One figure of the reference cast: who it is, where it starts from the
/// view's centre and how far it moves each frame, in world sub-units, which
/// way it faces, what it performs, and how deep the water it stands in is.
struct Extra {
    species: Species,
    from: (i32, i32),
    per_frame: (i32, i32),
    toward: Facing,
    performs: Option<Kind>,
    submerged: i32,
}

/// Every species, and between them every part of the figure pass: a walk
/// and a run, an upper-body action over a stride, a whole-body action in the
/// air, and a figure wading.
const EXTRAS: [Extra; 5] = [
    Extra {
        species: Species::Human,
        from: (-1400, 200),
        per_frame: (20, 0),
        toward: Facing(0),
        performs: None,
        submerged: 0,
    },
    Extra {
        species: Species::Elf,
        from: (-500, -700),
        per_frame: (0, 20),
        toward: Facing(0x4000),
        performs: Some(Kind::Cast),
        submerged: 0,
    },
    Extra {
        species: Species::Dwarf,
        from: (500, 400),
        per_frame: (0, 0),
        toward: Facing(0xC000),
        performs: None,
        submerged: 48,
    },
    Extra {
        species: Species::Beastkin,
        from: (1300, -400),
        per_frame: (-61, 0),
        toward: Facing(0x8000),
        performs: None,
        submerged: 0,
    },
    Extra {
        species: Species::Dragonkin,
        from: (100, 1000),
        per_frame: (0, 0),
        toward: Facing(0x2000),
        performs: Some(Kind::Dodge),
        submerged: 0,
    },
];

/// How many frames the cast plays before it is caught: into the middle of the
/// dodge's flight and the cast's release.
const FRAMES_PLAYED: u32 = 12;

/// How many figures the reference cast has.
pub const CAST_LEN: usize = EXTRAS.len();

/// The reference cast, standing around `centre` and played through to the
/// reference moment.
///
/// # Errors
///
/// [`ClientError::Figure`] for a figure that cannot be built or moved, and
/// [`ClientError::OutOfMemory`] where the scene has no room for it.
pub fn cast<'a>(clips: &'a Clips<'a>, centre: WorldPoint) -> Result<Cast<'a>, ClientError> {
    let mut cast = Cast::new();
    for (index, extra) in (0u64..).zip(&EXTRAS) {
        let identity = figures::identity(extra.species).map_err(|_| ClientError::Figure)?;
        let mut actor =
            Actor::new(&identity, clips, extra.toward).map_err(|_| ClientError::Figure)?;
        if let Some(kind) = extra.performs {
            actor.perform(kind).map_err(|_| ClientError::Figure)?;
        }
        let id = EntityId(index);
        let mut at = WorldPoint {
            x: centre.x + extra.from.0,
            y: centre.y + extra.from.1,
        };
        cast.join(id, actor, at)?;
        let figure = cast.get_mut(id).ok_or(ClientError::Figure)?;
        for _ in 0..FRAMES_PLAYED {
            at.x += extra.per_frame.0;
            at.y += extra.per_frame.1;
            figure.step(FRAME_NS, at, extra.toward, extra.submerged)?;
        }
    }
    Ok(cast)
}

#[cfg(test)]
#[path = "reference_tests.rs"]
mod tests;
