//! A frame covers every pixel exactly once, reads nothing but its own
//! description, draws its figures over the lit ground, and measures what it
//! spent.

use super::*;
use tairix_wintersun_figure::actor::Actor;
use tairix_wintersun_figure::motion::Set;
use tairix_wintersun_figure::reference as figures;
use tairix_wintersun_figure::species::Species;
use tairix_wintersun_net::value::{EntityId, Facing, WorldPoint};
use tairix_wintersun_world::land::{Keeping, RealmLand};
use tairix_wintersun_world::params::{RealmParams, RealmSpec};
use tairix_wintersun_world::realm::RealmField;

use crate::camera::{realm_bounds, Zoom};
use crate::terrain::{RoadDecals, CHUNK_SUB_UNITS};

/// A clock that advances a fixed amount per reading, so a test can
/// assert what was measured without depending on how fast the host is.
struct Ticking(core::cell::Cell<u64>);

impl Clock for Ticking {
    fn now_ns(&self) -> u64 {
        let now = self.0.get();
        self.0.set(now + 1_000);
        now
    }
}

struct Quiet;

impl tairix_log::Sink for Quiet {
    fn write_event(&self, _: &tairix_log::Event<'_>) {}
}

static SINK: Quiet = Quiet;
static PRESSURE: tairix_reclaim::ReportedPressure = tairix_reclaim::ReportedPressure::unknown();

fn realm() -> RealmParams {
    RealmParams::new(RealmSpec {
        extent_chunks: 8,
        coarse_samples: 32,
        plates: 8,
        relief_units: 300,
        ..RealmParams::default_realm(0xF2A3_1D07).spec()
    })
    .expect("the spec is in range")
}

/// Where the test camera looks: the middle of the realm's middle chunk.
const FOCUS: WorldPoint = WorldPoint {
    x: CHUNK_SUB_UNITS / 2,
    y: CHUNK_SUB_UNITS / 2,
};

/// Everything a frame of the test realm is drawn from.
struct Ground {
    held: HeldGround,
    camera: Camera,
    roads: RoadDecals,
}

impl Ground {
    fn new(view: &Viewport, zoom: Zoom) -> Self {
        let land = RealmField::generate(realm())
            .and_then(|field| RealmLand::new(field, &tairix_parallel::SERIAL))
            .expect("the realm generates");
        let camera = Camera::new(FOCUS, zoom, realm_bounds(realm()));
        let mut held = HeldGround::new();
        held.hold(
            &land,
            &mut Keeping::within(usize::MAX),
            &camera,
            view,
            0,
            &tairix_parallel::SERIAL,
        )
        .expect("the ground generates");
        let roads = RoadDecals::from_realm(land.field()).expect("the roads fit");
        Self {
            held,
            camera,
            roads,
        }
    }

    fn cache() -> MaterialCache {
        PRESSURE.report(tairix_reclaim::PressureBand::Normal);
        MaterialCache::new("wintersun-frame-test", 256 * 1024 * 1024, &PRESSURE, &SINK)
    }

    /// Draw one frame into `target`, every mesh the view wants built first.
    fn render(
        &self,
        target: &mut Surface,
        view: &Viewport,
        detail: Detail,
        cast: &Cast<'_>,
        renderer: &mut Renderer,
        clock: &dyn Clock,
    ) -> Result<FrameTimes, ClientError> {
        let decals = self.roads.decals().expect("the decals fit");
        let warp = Warp::new(realm().seed());
        let fray = Fray::new(realm().seed());
        let mut cache = Self::cache();
        let scene = Scene {
            camera: self.camera,
            ground: &self.held,
            decals: &decals,
            fray: &fray,
            warp: &warp,
            sun: Sun::daylight(),
            sky: Sky::daylight(),
            detail,
            cast,
        };
        renderer.render_whole(
            target,
            view,
            &scene,
            &mut cache,
            &tairix_parallel::SERIAL,
            &Stopped,
        )?;
        // Measured once every mesh is built, so the frame timed is one that
        // builds none.
        renderer.render(
            target,
            view,
            &scene,
            &mut cache,
            &tairix_parallel::SERIAL,
            clock,
        )
    }
}

fn draw(view: &Viewport, detail: Detail, cast: &Cast<'_>) -> (Surface, usize) {
    let ground = Ground::new(view, Zoom::FURTHEST);
    let (width, height) = view.render();
    let mut target = Surface::new(width, height).expect("a target");
    let mut renderer = Renderer::new();
    ground
        .render(&mut target, view, detail, cast, &mut renderer, &Stopped)
        .expect("the frame draws");
    (target, renderer.figures())
}

fn view() -> Viewport {
    Viewport::new(128, 96, crate::quality::RenderScale::ONE).expect("a real window")
}

/// The frame description carries everything a pass draws: executing one
/// description twice, into separate buffers, gives the same picture.
#[test]
fn a_frame_drawn_twice_from_one_description_is_one_picture() {
    let view = view();
    let ground = Ground::new(&view, Zoom::DEFAULT);
    let decals = ground.roads.decals().expect("the decals fit");
    let warp = Warp::new(realm().seed());
    let fray = Fray::new(realm().seed());
    let mut cache = Ground::cache();
    let cast = Cast::new();
    let scene = Scene {
        camera: ground.camera,
        ground: &ground.held,
        decals: &decals,
        fray: &fray,
        warp: &warp,
        sun: Sun::daylight(),
        sky: Sky::daylight(),
        detail: Detail::FINEST,
        cast: &cast,
    };
    let mut renderer = Renderer::new();
    let frame = renderer
        .describe(
            &view,
            &scene,
            &mut cache,
            &tairix_parallel::SERIAL,
            &Stopped,
        )
        .expect("it describes");
    assert!(frame.meshes() > 0, "the frame draws no ground");
    let (width, height) = view.render();
    let mut pictures = alloc::vec::Vec::new();
    for _ in 0..2 {
        let mut target = Surface::new(width, height).expect("a target");
        let mut scratch = Scratch::new();
        execute(
            &frame,
            &mut scratch,
            &mut target,
            &tairix_parallel::SERIAL,
            &Stopped,
        )
        .expect("it draws");
        pictures.push(target);
    }
    assert!(
        pictures[0] == pictures[1],
        "one description drew two pictures"
    );
}

/// A frame builds at most a bounded number of meshes, and draws what it has
/// built while the rest arrive.
#[test]
fn a_frame_builds_a_bounded_number_of_meshes() {
    let view = view();
    let ground = Ground::new(&view, Zoom::FURTHEST);
    let decals = ground.roads.decals().expect("the decals fit");
    let warp = Warp::new(realm().seed());
    let fray = Fray::new(realm().seed());
    let mut cache = Ground::cache();
    let cast = Cast::new();
    let scene = Scene {
        camera: ground.camera,
        ground: &ground.held,
        decals: &decals,
        fray: &fray,
        warp: &warp,
        sun: Sun::daylight(),
        sky: Sky::daylight(),
        detail: Detail::FINEST,
        cast: &cast,
    };
    let (width, height) = view.render();
    let mut target = Surface::new(width, height).expect("a target");
    let mut renderer = Renderer::new();
    let mut drawn = alloc::vec::Vec::new();
    for _ in 0..8 {
        renderer
            .render(
                &mut target,
                &view,
                &scene,
                &mut cache,
                &tairix_parallel::SERIAL,
                &Stopped,
            )
            .expect("it draws");
        drawn.push(renderer.drawn());
    }
    assert!(
        drawn[0] <= MESH_BUILDS,
        "the first frame built {} meshes",
        drawn[0]
    );
    assert!(drawn.windows(2).all(|pair| pair[0] <= pair[1]), "{drawn:?}");
    assert!(
        drawn[7] > drawn[0],
        "later frames drew no more ground: {drawn:?}"
    );
}

#[test]
fn every_pixel_is_written() {
    let (target, _) = draw(&view(), Detail::FINEST, &Cast::new());
    assert!(
        target.pixels().iter().all(|p| p.a == 255),
        "the frame left transparent pixels"
    );
}

#[test]
fn a_target_of_the_wrong_size_is_refused_rather_than_partly_drawn() {
    let view = view();
    let ground = Ground::new(&view, Zoom::FURTHEST);
    let (width, height) = view.render();
    let mut target = Surface::new(width - 1, height).expect("a target");
    let refused = ground.render(
        &mut target,
        &view,
        Detail::FINEST,
        &Cast::new(),
        &mut Renderer::new(),
        &Stopped,
    );
    assert_eq!(refused.err(), Some(ClientError::Viewport));
    assert!(
        target.pixels().iter().all(|p| p.a == 0),
        "a refused frame drew"
    );
}

#[test]
fn a_target_a_clip_window_cuts_is_refused_rather_than_drawn_wrongly() {
    let view = view();
    let ground = Ground::new(&view, Zoom::FURTHEST);
    let (width, height) = view.render();
    let mut target = Surface::new(width, height).expect("a target");
    let mut renderer = Renderer::new();
    for (x, y, w, h) in [(1, 0, width - 1, height), (0, 1, width, height - 1)] {
        let mut outcome = None;
        target.with_clip(x, y, w, h, |clipped| {
            outcome = Some(ground.render(
                clipped,
                &view,
                Detail::FINEST,
                &Cast::new(),
                &mut renderer,
                &Stopped,
            ));
        });
        assert_eq!(outcome.and_then(Result::err), Some(ClientError::Viewport));
    }
    assert!(
        target.pixels().iter().all(|p| p.a == 0),
        "a refused frame drew"
    );
}

#[test]
fn every_pass_with_work_is_measured_and_the_rest_report_none() {
    let view = Viewport::new(64, 48, crate::quality::RenderScale::ONE).expect("a real window");
    let ground = Ground::new(&view, Zoom::FURTHEST);
    let mut target = Surface::new(64, 48).expect("a target");
    let times = ground
        .render(
            &mut target,
            &view,
            Detail::FINEST,
            &Cast::new(),
            &mut Renderer::new(),
            &Ticking(core::cell::Cell::new(0)),
        )
        .expect("the frame draws");
    for pass in [Pass::Terrain, Pass::Light, Pass::Scenery] {
        assert!(times.spent(pass) > 0, "{pass:?} was not timed");
    }
    for pass in [Pass::Particles, Pass::Ui] {
        assert_eq!(
            times.spent(pass),
            0,
            "a pass with no work reports none rather than a guess"
        );
    }
}

#[test]
fn the_plainest_detail_changes_the_picture_rather_than_breaking_it() {
    let (drawn, _) = draw(&view(), Detail::FINEST, &Cast::new());
    let plainest = Detail::PLAINEST;
    let shed_view = Viewport::new(128, 96, plainest.resolution.scale()).expect("a real window");
    let (shed, _) = draw(&shed_view, plainest, &Cast::new());
    assert!(
        shed.pixels().len() < drawn.pixels().len(),
        "the coarsest resolution did not shrink the target"
    );
    assert!(
        shed.pixels().iter().all(|p| p.a == 255),
        "the shed frame left holes"
    );
}

/// Close in, so no rise of the test realm's ground stands between the eye and
/// the figure.
#[test]
fn figures_are_drawn_over_the_ground_where_they_stand_and_nowhere_else() {
    let view = Viewport::new(128, 96, crate::quality::RenderScale::ONE).expect("a real window");
    let ground = Ground::new(&view, Zoom::NEAREST);
    let set = Set::new().expect("the shipped set");
    let clips = set.clips().expect("the shipped clips");
    let mut cast = Cast::new();
    let identity = figures::identity(Species::Human).expect("a record");
    let actor = Actor::new(&identity, &clips, Facing(0x4000)).expect("a figure");
    cast.join(EntityId(1), actor, FOCUS).expect("it joins");

    let (width, height) = view.render();
    let mut renderer = Renderer::new();
    let mut bare = Surface::new(width, height).expect("a target");
    ground
        .render(
            &mut bare,
            &view,
            Detail::FINEST,
            &Cast::new(),
            &mut renderer,
            &Stopped,
        )
        .expect("it draws");
    assert_eq!(renderer.figures(), 0);
    let mut peopled = Surface::new(width, height).expect("a target");
    ground
        .render(
            &mut peopled,
            &view,
            Detail::FINEST,
            &cast,
            &mut renderer,
            &Stopped,
        )
        .expect("it draws");
    assert_eq!(renderer.figures(), 1);

    // The camera looks at the figure, so the figure stands at the screen's
    // middle and fills a share of it.
    let (cx, cy) = (i64::from(width / 2), i64::from(height / 2));
    let reach = i64::from(height / 2);
    let mut changed = 0usize;
    for (index, (was, now)) in bare.pixels().iter().zip(peopled.pixels()).enumerate() {
        if was == now {
            continue;
        }
        changed += 1;
        let x = i64::try_from(index % width as usize).expect("a column");
        let y = i64::try_from(index / width as usize).expect("a row");
        assert!(
            (x - cx).abs() < reach && (y - cy).abs() < reach,
            "a figure at ({cx},{cy}) drew at ({x},{y})"
        );
        assert_eq!(now.a, 255, "a figure left the ground translucent");
    }
    assert!(
        changed > 20,
        "the figure was barely drawn: {changed} pixels"
    );
}
