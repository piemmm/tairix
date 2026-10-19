//! The cast is who is in the scene, and the stage draws exactly them: every
//! figure the camera can see, far to near, hidden behind nearer ground, and
//! the same picture however the frame is banded.

use tairix_parallel::{JobRunner, Reversed, SERIAL};
use tairix_raster::mesh::{Depth, DepthBuffer, DepthTest, EdgeRecord, MeshVertex, Triangle};
use tairix_raster::surface::Surface;
use tairix_raster::view::Perspective;
use tairix_wintersun_art::decal::Bounds;
use tairix_wintersun_figure::actor::{Actor, Relief, Shade};
use tairix_wintersun_figure::motion::{Clips, Kind, Set};
use tairix_wintersun_figure::paint::Brush;
use tairix_wintersun_figure::reference;
use tairix_wintersun_figure::species::Species;
use tairix_wintersun_net::value::{EntityId, Facing, WorldPoint};

use tairix_wintersun_rules::terrain::{SyntheticTerrain, WADE_DEPTH_SUB_UNITS};
use tairix_wintersun_world::geom::{CELL_SUB_UNITS, ELEVATION_SUB_UNITS};

use super::{submerged, Cast, Figure, Framing, Stage};
use crate::budget::FRAME_NS;
use crate::camera::{Camera, Zoom};
use crate::error::ClientError;
use crate::light::{Atmosphere, Sky, Sun};
use crate::quality::RenderScale;
use crate::view::Viewport;

/// A realm large enough that the camera is never held back by its edge.
const OPEN: Bounds = Bounds {
    min_x: -(1 << 22),
    min_y: -(1 << 22),
    max_x: 1 << 22,
    max_y: 1 << 22,
};

fn actor<'a>(clips: &'a Clips<'a>, species: Species) -> Actor<'a> {
    let identity = reference::identity(species).expect("a reference record");
    Actor::new(&identity, clips, Facing(0x4000)).expect("a figure")
}

/// Level ground at height nought, held everywhere within a few kilometres of
/// the origin.
struct Level;

impl Relief for Level {
    fn height(&self, at: WorldPoint) -> Option<i32> {
        let held = |value: i32| value.unsigned_abs() < 4_000 * CELL_SUB_UNITS.unsigned_abs();
        (held(at.x) && held(at.y)).then_some(0)
    }
}

/// The camera at the game's own tilt, `zoom` back from the origin.
struct Shot {
    view: Viewport,
    camera: Perspective,
    atmosphere: Atmosphere,
}

impl Shot {
    fn new() -> Self {
        Self::at(Zoom::DEFAULT)
    }

    fn at(zoom: Zoom) -> Self {
        let view = Viewport::new(160, 120, RenderScale::ONE).expect("a real window");
        let camera = Camera::new(WorldPoint { x: 0, y: 0 }, zoom, OPEN)
            .perspective(&view, 0, |_| Some(0))
            .expect("a camera");
        let (_, height) = view.render();
        let atmosphere = Atmosphere::new(&Sun::daylight(), Sky::daylight(), &camera, height, true)
            .expect("room");
        Self {
            view,
            camera,
            atmosphere,
        }
    }

    fn framing(&self) -> Framing<'_> {
        Framing {
            camera: &self.camera,
            light: Sun::daylight().light().expect("the sun"),
            shade: Shade::Soft,
        }
    }

    fn placed(&self, cast: &Cast<'_>, runner: &dyn JobRunner) -> Stage {
        let mut stage = Stage::default();
        stage
            .place(cast, &self.framing(), &Level, &self.atmosphere, runner)
            .expect("the figures place");
        stage
    }

    /// `cast` drawn onto a clear target cut into bands of `rows` rows, over
    /// ground `depth` deep wherever `scene` is drawn.
    fn draw(
        &self,
        cast: &Cast<'_>,
        rows: u32,
        runner: &dyn JobRunner,
        scene: &DepthBuffer,
    ) -> Surface {
        let stage = self.placed(cast, runner);
        let (width, height) = self.view.render();
        let mut target = Surface::new(width, height).expect("a target");
        let mut brush = Brush::new();
        for mut band in target.row_bands_mut(0..height, rows) {
            stage.paint(&mut band, scene, &mut brush);
        }
        target
    }

    /// A depth buffer over the whole view, nothing drawn in it but `depth`
    /// over rows `rows`.
    fn scene(&self, rows: core::ops::Range<u32>, depth: Depth) -> DepthBuffer {
        let (width, height) = self.view.render();
        let mut buffer = DepthBuffer::new();
        assert!(buffer.reset(width, height));
        let at = |x: u32, y: u32| MeshVertex {
            x: i32::try_from(x * 256).expect("small"),
            y: i32::try_from(y * 256).expect("small"),
            depth,
        };
        for mut band in buffer.bands_mut(height) {
            for corners in [
                [at(0, rows.start), at(width, rows.start), at(0, rows.end)],
                [at(width, rows.start), at(width, rows.end), at(0, rows.end)],
            ] {
                if let Some(triangle) = Triangle::new(corners) {
                    triangle.fill(&mut band, DepthTest::OPAQUE, EdgeRecord::Skip, |_| true);
                }
            }
        }
        buffer
    }
}

#[test]
fn the_cast_holds_each_entity_once() {
    let set = Set::new().expect("the shipped set");
    let clips = set.clips().expect("the shipped clips");
    let mut cast = Cast::new();
    assert!(cast.is_empty());
    let here = WorldPoint { x: 40, y: -40 };
    cast.join(EntityId(7), actor(&clips, Species::Human), here)
        .expect("a first figure joins");
    assert_eq!(
        cast.join(EntityId(7), actor(&clips, Species::Elf), here),
        Err(ClientError::Figure),
        "one entity was brought in twice"
    );
    assert_eq!(cast.len(), 1);
    assert_eq!(cast.get(EntityId(7)).map(Figure::ground), Some(here));
    assert!(cast.get(EntityId(8)).is_none());
    assert!(cast.leave(EntityId(7)));
    assert!(!cast.leave(EntityId(7)), "a figure left twice");
    assert!(cast.is_empty());
}

#[test]
fn a_step_moves_the_figure_and_a_stand_does_not_walk_it() {
    let set = Set::new().expect("the shipped set");
    let clips = set.clips().expect("the shipped clips");
    let mut cast = Cast::new();
    cast.join(
        EntityId(1),
        actor(&clips, Species::Human),
        WorldPoint::default(),
    )
    .expect("it joins");
    let figure = cast.get_mut(EntityId(1)).expect("it is there");
    let mut at = WorldPoint::default();
    for _ in 0..60 {
        at.x += 20;
        figure.step(FRAME_NS, at, Facing(0), 0).expect("a frame");
    }
    assert_eq!(figure.ground(), WorldPoint { x: 1200, y: 0 });
    assert_eq!(
        figure.actor().heading(),
        Facing(0),
        "it never turned to go east"
    );

    figure.stand(WorldPoint { x: -9000, y: 9000 });
    assert_eq!(figure.ground(), WorldPoint { x: -9000, y: 9000 });
    figure
        .actor_mut()
        .perform(Kind::Cast)
        .expect("an action plays");
    assert_eq!(figure.actor().performing(), (None, Some(Kind::Cast)));
}

#[test]
fn only_figures_the_camera_can_see_are_placed() {
    let set = Set::new().expect("the shipped set");
    let clips = set.clips().expect("the shipped clips");
    let shot = Shot::new();
    let cell = CELL_SUB_UNITS;
    let mut cast = Cast::new();
    let places = [
        (WorldPoint { x: 0, y: 0 }, true),
        (
            WorldPoint {
                x: 3 * cell,
                y: -5 * cell,
            },
            true,
        ),
        // Behind the eye, which stands to the south.
        (
            WorldPoint {
                x: 0,
                y: 400 * cell,
            },
            false,
        ),
        // Far past the screen's edge.
        (
            WorldPoint {
                x: 900 * cell,
                y: -20 * cell,
            },
            false,
        ),
        // On ground the scene does not hold.
        (
            WorldPoint {
                x: 0,
                y: -5_000 * cell,
            },
            false,
        ),
    ];
    for (id, (at, _)) in (0u64..).zip(places) {
        cast.join(EntityId(id), actor(&clips, Species::Dwarf), at)
            .expect("it joins");
    }
    let stage = shot.placed(&cast, &SERIAL);
    let expected = places.iter().filter(|(_, seen)| *seen).count();
    assert_eq!(stage.placed(), expected);
}

#[test]
fn figures_are_drawn_far_to_near_and_ties_the_same_way_every_frame() {
    let set = Set::new().expect("the shipped set");
    let clips = set.clips().expect("the shipped clips");
    let shot = Shot::new();
    let mut cast = Cast::new();
    // North is farther from the camera, which looks north.
    for (id, y) in [(2, 1_000), (1, 1_000), (0, -2_000), (3, 4_000)] {
        cast.join(
            EntityId(id),
            actor(&clips, Species::Human),
            WorldPoint { x: 0, y },
        )
        .expect("it joins");
    }
    let order: alloc::vec::Vec<u64> = shot
        .placed(&cast, &Reversed::new(3))
        .standing
        .iter()
        .map(|standing| standing.order.1)
        .collect();
    assert_eq!(order, alloc::vec![0, 1, 2, 3]);
}

/// Ground nearer the eye than a figure hides it, which is also what keeps a
/// wading figure's legs under the water drawn over them.
#[test]
fn nearer_ground_hides_a_figure() {
    let set = Set::new().expect("the shipped set");
    let clips = set.clips().expect("the shipped clips");
    let shot = Shot::new();
    let mut cast = Cast::new();
    cast.join(
        EntityId(1),
        actor(&clips, Species::Human),
        WorldPoint { x: 0, y: 0 },
    )
    .expect("it joins");
    let (_, height) = shot.view.render();
    let open = shot.draw(&cast, height, &SERIAL, &shot.scene(0..0, Depth::CLEAR));
    let shown = open.pixels().iter().filter(|pixel| pixel.a != 0).count();
    assert!(shown > 0, "nothing of the figure was drawn");
    let walled = shot.draw(
        &cast,
        height,
        &SERIAL,
        &shot.scene(0..height, Depth::NEAREST),
    );
    assert!(
        walled.pixels().iter().all(|pixel| pixel.a == 0),
        "a figure was drawn through ground nearer than it"
    );
    // Ground over the bottom half alone hides only what is behind it there.
    let half = shot.draw(
        &cast,
        height,
        &SERIAL,
        &shot.scene(height / 2..height, Depth::NEAREST),
    );
    let (width, _) = shot.view.render();
    for (index, pixel) in half.pixels().iter().enumerate() {
        let row = u32::try_from(index / width as usize).expect("a row");
        if row >= height / 2 {
            assert_eq!(pixel.a, 0, "drawn behind ground at row {row}");
        }
    }
}

/// Close in, so every figure spans several of the narrowest bands.
#[test]
fn banding_does_not_change_the_picture() {
    let set = Set::new().expect("the shipped set");
    let clips = set.clips().expect("the shipped clips");
    let shot = Shot::at(Zoom::NEAREST);
    let mut cast = Cast::new();
    for (id, species) in (0u64..).zip(Species::ALL) {
        let at = WorldPoint {
            x: i32::try_from(id).expect("a handful") * 700 - 1400,
            y: i32::try_from(id % 2).expect("a bit") * 300 - 150,
        };
        cast.join(EntityId(id), actor(&clips, species), at)
            .expect("it joins");
        let figure = cast.get_mut(EntityId(id)).expect("it is there");
        let mut walked = at;
        for _ in 0..20 {
            walked.y += 25;
            figure
                .step(FRAME_NS, walked, Facing(0x4000), 0)
                .expect("a frame");
        }
    }
    let (_, height) = shot.view.render();
    let open = shot.scene(0..0, Depth::CLEAR);
    let whole = shot.draw(&cast, height, &SERIAL, &open);
    assert!(
        whole.pixels().iter().filter(|pixel| pixel.a != 0).count() > 400,
        "the figures were barely drawn"
    );
    for rows in [1, 7, 16, 33] {
        for runner in [&SERIAL as &dyn JobRunner, &Reversed::new(4)] {
            assert!(
                shot.draw(&cast, rows, runner, &open) == whole,
                "bands of {rows} rows drew a different picture"
            );
        }
    }
}

#[test]
fn a_body_is_submerged_by_the_rules_own_depth_in_world_units() {
    let pools = SyntheticTerrain::lattice(0, 4);
    let cell = |x: i32, y: i32| WorldPoint {
        x: x * CELL_SUB_UNITS + CELL_SUB_UNITS / 2,
        y: y * CELL_SUB_UNITS + CELL_SUB_UNITS / 2,
    };
    assert_eq!(
        submerged(&pools, cell(0, 0)),
        0,
        "dry ground drowned a figure"
    );
    let deep = SyntheticTerrain::POOL_DEPTH_SUB_UNITS * CELL_SUB_UNITS / ELEVATION_SUB_UNITS;
    assert_eq!(submerged(&pools, cell(1, 1)), deep);
    assert_eq!(
        submerged(&pools, cell(-3, -3)),
        deep,
        "a pool west of the origin"
    );
    // The deepest water the rules let a body stand in is under a cell's
    // height, so a wading figure keeps its head above water.
    const { assert!(WADE_DEPTH_SUB_UNITS * CELL_SUB_UNITS / ELEVATION_SUB_UNITS < CELL_SUB_UNITS) };

    let empty: [&tairix_wintersun_world::chunk::Chunk; 0] = [];
    let unheld = tairix_wintersun_rules::terrain::ChunkTerrain::new(&empty).expect("sorted");
    assert_eq!(
        submerged(&unheld, cell(0, 0)),
        0,
        "ground not held drowned a figure"
    );
}
