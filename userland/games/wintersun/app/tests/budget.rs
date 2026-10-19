//! The software path's frame cost, measured rather than asserted about.
//!
//! The passes are timed at the baseline resolution on a long view across
//! broken ground at the lowest tilt the camera allows, with the plan's
//! sixty-four rigged figures standing in it, along with what placing one of
//! them costs a single core. The numbers are printed: `plans/WINTERSUN.md`
//! records them as the baseline a regression is judged against.
//!
//! # Why no elapsed time is asserted here
//!
//! A wall-clock threshold is a claim about the machine, not the renderer: the
//! same code passes on a fast host and fails on a slow one, a flake no retry
//! can fix. The deterministic claims live where no clock is needed — pass
//! attribution against a controlled clock in the crate's `frame` tests, the
//! band decomposition on a serial runner in `tests/bands.rs` — and the one
//! this file makes is that handing the bands to real threads draws the
//! identical picture, which a torn hand-off breaks on any machine.
//!
//! Read the numbers with:
//! `cargo test -p tairix-wintersun-app --release --test budget -- --nocapture`

use std::hint::black_box;
use std::time::Instant;

use tairix_parallel::{JobRunner, Threaded};
use tairix_raster::color::Pixel;
use tairix_raster::surface::Surface;
use tairix_raster::view::Perspective;
use tairix_reclaim::{PressureBand, ReportedPressure};
use tairix_wintersun_app::budget::{FrameTimes, Pass, BASELINE_HEIGHT, BASELINE_WIDTH, FRAME_NS};
use tairix_wintersun_app::camera::{realm_bounds, Camera, Tilt, Zoom};
use tairix_wintersun_app::figures::Cast;
use tairix_wintersun_app::frame::{Clock, Renderer, Scene, Stopped};
use tairix_wintersun_app::light::{Sky, Sun};
use tairix_wintersun_app::quality::{Detail, RenderScale};
use tairix_wintersun_app::terrain::{HeldGround, RoadDecals, CHUNK_SUB_UNITS, SOLVE_RADIUS};
use tairix_wintersun_app::view::Viewport;
use tairix_wintersun_art::cache::MaterialCache;
use tairix_wintersun_art::decal::Fray;
use tairix_wintersun_art::splat::Warp;
use tairix_wintersun_figure::actor::{Actor, Relief};
use tairix_wintersun_figure::motion::{Clips, Kind, Set};
use tairix_wintersun_figure::reference;
use tairix_wintersun_figure::rig::Placement;
use tairix_wintersun_figure::species::Species;
use tairix_wintersun_net::value::{ChunkCoord, EntityId, Facing, WorldPoint};
use tairix_wintersun_world::land::{Keeping, RealmLand};
use tairix_wintersun_world::params::{RealmParams, RealmSpec};
use tairix_wintersun_world::realm::RealmField;

/// Frames timed, so a single scheduling hiccup does not decide the answer.
/// The best is reported: the question is what the renderer costs, not what
/// the machine was doing at the time.
const RUNS: usize = 5;

/// How many cores the budget is stated for, and so how many threads the
/// bands are handed to.
const REFERENCE_CORES: usize = 4;

/// How many figures carrying a full rig the budget is stated for.
const RIGS: u64 = 64;

/// What the rigs are doing, a figure each in turn: a mixture of strides,
/// both action layers and a wader, so the pass is costed at the poses the
/// game draws rather than at one.
const ACTIVITIES: [(Option<Kind>, i32, i32); 6] = [
    (None, 20, 0),
    (None, 61, 0),
    (Some(Kind::Cast), 20, 0),
    (Some(Kind::MeleeLight), 0, 0),
    (Some(Kind::Dodge), 0, 0),
    (None, 0, 60),
];

/// A clock that reads the host's monotonic time.
struct Host(Instant);

impl Clock for Host {
    fn now_ns(&self) -> u64 {
        u64::try_from(self.0.elapsed().as_nanos()).unwrap_or(u64::MAX)
    }
}

struct Quiet;

impl tairix_log::Sink for Quiet {
    fn write_event(&self, _: &tairix_log::Event<'_>) {}
}

static SINK: Quiet = Quiet;
static PRESSURE: ReportedPressure = ReportedPressure::unknown();

/// The held ground, as a figure stands on it.
struct Held<'a>(&'a HeldGround);

impl Relief for Held<'_> {
    fn height(&self, at: WorldPoint) -> Option<i32> {
        self.0.height_at(at)
    }
}

/// The chunk of the realm's southern half whose far reading spans the most
/// height, read every fourth chunk: the most broken ground with most of the
/// realm north of it for the view to cross.
fn broken(land: &RealmLand) -> ChunkCoord {
    let params = land.field().params();
    let (low, past) = (params.min_chunk(), params.max_chunk());
    let mut best = (0, ChunkCoord { x: 0, y: 0 });
    for y in (0..past).step_by(4) {
        for x in (low..past).step_by(4) {
            let coord = ChunkCoord { x, y };
            let patch = land.far(coord).expect("the realm holds it");
            let heights = (0..patch.side())
                .flat_map(|j| (0..patch.side()).map(move |i| (i, j)))
                .filter_map(|(i, j)| patch.surface(i, j))
                .map(|height| i32::from(height.0));
            let (lowest, highest) = heights.fold((i32::MAX, i32::MIN), |(lo, hi), height| {
                (lo.min(height), hi.max(height))
            });
            if highest - lowest > best.0 {
                best = (highest - lowest, coord);
            }
        }
    }
    best.1
}

/// Where the screen point `(x, y)` meets level ground at `floor` through
/// `camera`.
fn on_ground(camera: &Perspective, x: f64, y: f64, floor: i32) -> WorldPoint {
    let (eye, ray) = (camera.eye(), camera.ray(x, y));
    assert!(ray[2] < 0.0, "a rig was set on a row above the horizon");
    let reach = (f64::from(floor) - eye[2]) / ray[2];
    let round = |value: f64| {
        let rounded = value.round();
        assert!(
            rounded.abs() < f64::from(i32::MAX),
            "off the coordinate space"
        );
        #[allow(
            clippy::cast_possible_truncation,
            reason = "checked in range just above"
        )]
        let rounded = rounded as i32;
        rounded
    };
    WorldPoint {
        x: round(eye[0] + ray[0] * reach),
        y: round(eye[1] + ray[1] * reach),
    }
}

/// The rigs, set on an eight-by-eight grid over the screen's lower half, and
/// how far each moves east a frame and how deep the water it stands in is.
fn rigs<'a>(
    clips: &'a Clips<'a>,
    camera: &Perspective,
    floor: i32,
) -> (Cast<'a>, Vec<(EntityId, i32, i32)>) {
    let (width, height) = (f64::from(BASELINE_WIDTH), f64::from(BASELINE_HEIGHT));
    let mut cast = Cast::new();
    let mut walks = Vec::new();
    for id in 0..RIGS {
        let index = usize::try_from(id).expect("a small id");
        let identity =
            reference::identity(Species::ALL[index % Species::ALL.len()]).expect("a record");
        let (performs, dx, depth) = ACTIVITIES[index % ACTIVITIES.len()];
        let mut actor = Actor::new(&identity, clips, Facing(0)).expect("a figure");
        if let Some(kind) = performs {
            actor.perform(kind).expect("it plays");
        }
        #[allow(clippy::cast_precision_loss, reason = "a grid of eight")]
        let (column, row) = ((id % 8) as f64, (id / 8) as f64);
        let at = on_ground(
            camera,
            width * (2.0 * column + 1.0) / 16.0,
            height * (0.5 + (2.0 * row + 1.0) / 32.0),
            floor,
        );
        cast.join(EntityId(id), actor, at).expect("it joins");
        walks.push((EntityId(id), dx, depth));
    }
    (cast, walks)
}

/// What one frame of the baseline costs: the cheapest run's per-pass times,
/// what placing one figure costs a single core, how many meshes it drew, and
/// the picture drawn.
struct Measured {
    times: FrameTimes,
    placement_ns: u64,
    meshes: usize,
    pixels: Vec<Pixel>,
}

/// The cheapest of [`RUNS`] serial placements of every rig, per figure: the
/// pose, planting and placement each figure costs whichever core it lands on,
/// seen through the frame's own camera.
fn placement_ns(cast: &Cast<'_>, camera: &Perspective, held: &HeldGround) -> u64 {
    let light = Sun::daylight()
        .light()
        .expect("the daylight sun lights figures");
    let shade = Detail::FINEST.shadows.shade();
    let figures: Vec<_> = (0..RIGS)
        .map(|id| cast.get(EntityId(id)).expect("it is there"))
        .collect();
    let relief = Held(held);
    let mut placement = Placement::new();
    let mut best = u64::MAX;
    for _ in 0..RUNS {
        let started = Instant::now();
        for figure in &figures {
            let drawn = figure
                .actor()
                .place(
                    figure.ground(),
                    &relief,
                    camera,
                    light,
                    shade,
                    &mut placement,
                )
                .expect("it places");
            black_box((&drawn, &placement));
        }
        let spent = u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX);
        best = best.min(spent);
    }
    best / RIGS
}

/// The baseline realm's land, the camera's view across it, and the ground
/// that view draws, held once across `runner` for every frame drawn from it.
struct Baseline {
    land: RealmLand,
    held: HeldGround,
    camera: Camera,
    view: Viewport,
}

impl Baseline {
    fn hold(runner: &dyn JobRunner) -> Self {
        let params = RealmParams::new(RealmSpec {
            extent_chunks: 64,
            coarse_samples: 64,
            plates: 8,
            ..RealmParams::default_realm(0x4255_4447_4554).spec()
        })
        .expect("the spec is in range");
        let land = RealmField::generate(params)
            .and_then(|field| RealmLand::new(field, runner))
            .expect("the realm generates");
        // Its south edge, so the view north crosses the whole of it.
        let rough = broken(&land);
        let focus = WorldPoint {
            x: rough.x * CHUNK_SUB_UNITS + CHUNK_SUB_UNITS / 2,
            y: (rough.y + 1) * CHUNK_SUB_UNITS - 1,
        };
        let mut camera = Camera::new(focus, Zoom::DEFAULT, realm_bounds(params));
        camera.set_tilt(Tilt::LOWEST);
        let view = Viewport::new(BASELINE_WIDTH, BASELINE_HEIGHT, RenderScale::ONE)
            .expect("the baseline is a real window");
        let mut held = HeldGround::new();
        held.hold(
            &land,
            &mut Keeping::within(usize::MAX),
            &camera,
            &view,
            SOLVE_RADIUS,
            runner,
        )
        .expect("the ground generates");
        Self {
            land,
            held,
            camera,
            view,
        }
    }
}

/// Draw the baseline frame [`RUNS`] times across `runner`, once every mesh is
/// built and every tile made, returning the cheapest run's per-pass costs, the
/// per-figure placement cost and the picture every run drew.
fn measure(runner: &dyn JobRunner, baseline: &Baseline) -> Measured {
    let Baseline {
        land,
        held,
        camera,
        view,
    } = baseline;
    let (camera, view) = (*camera, *view);
    let params = land.field().params();
    let (w, h) = view.render();
    let roads = RoadDecals::from_realm(land.field()).expect("the roads fit");
    let decals = roads.decals().expect("the decals fit");
    let warp = Warp::new(params.seed());
    let fray = Fray::new(params.seed());

    let floor = held.height_at(camera.focus()).expect("the focus is held");
    let seen = camera
        .perspective(&view, floor, |at| held.height_at(at))
        .expect("the baseline projects");
    let set = Set::new().expect("the shipped set");
    let clips = set.clips().expect("the shipped clips");
    let (mut cast, walks) = rigs(&clips, &seen, floor);

    PRESSURE.report(PressureBand::Normal);
    let mut cache = MaterialCache::new("wintersun-budget", 256 * 1024 * 1024, &PRESSURE, &SINK);
    let mut renderer = Renderer::new();
    let mut target = Surface::new(w, h).expect("the baseline frame fits");
    let clock = Host(Instant::now());

    // The first frame builds every mesh and tile untimed, so the frames timed
    // build none.
    let mut best = FrameTimes::new();
    for run in 0..=RUNS {
        if run > 0 {
            for &(id, dx, depth) in &walks {
                let figure = cast.get_mut(id).expect("it is there");
                let to = WorldPoint {
                    x: figure.ground().x + dx,
                    y: figure.ground().y,
                };
                figure
                    .step(FRAME_NS, to, Facing(0), depth)
                    .expect("a frame");
            }
        }
        let scene = Scene {
            camera,
            ground: held,
            decals: &decals,
            fray: &fray,
            warp: &warp,
            sun: Sun::daylight(),
            sky: Sky::daylight(),
            detail: Detail::FINEST,
            cast: &cast,
        };
        if run == 0 {
            renderer
                .render_whole(&mut target, &view, &scene, &mut cache, runner, &Stopped)
                .expect("the frame draws");
            continue;
        }
        let times = renderer
            .render(&mut target, &view, &scene, &mut cache, runner, &clock)
            .expect("the frame draws");
        assert!(renderer.complete(), "a timed frame built a mesh");
        assert_eq!(
            renderer.figures() as u64,
            RIGS,
            "a rig was culled from the view it stands in"
        );
        // The frame is timed and then not read until the last run, so the
        // measurement is only honest if the optimiser cannot see that.
        black_box(&target);
        if run == 1 || times.total() < best.total() {
            best = times;
        }
    }
    Measured {
        times: best,
        placement_ns: placement_ns(&cast, &seen, held),
        meshes: renderer.drawn(),
        pixels: target.pixels().to_vec(),
    }
}

/// Print one measurement's per-pass costs against their budgets.
fn report(label: &str, measured: &Measured) {
    let times = &measured.times;
    let micros = |ns: u64| ns / 1_000;
    println!(
        "WinterSun frame at {BASELINE_WIDTH}x{BASELINE_HEIGHT}, {label}, {} meshes:",
        measured.meshes
    );
    for pass in Pass::ALL {
        let spent = times.spent(pass);
        // A derived `Debug` ignores the width, so the name is rendered
        // before it is padded or the columns come out ragged.
        let name = format!("{pass:?}");
        println!(
            "  {name:<10} {:>7} us  (budget {:>7} us, {:>4}%)",
            micros(spent),
            micros(pass.budget_ns()),
            spent.saturating_mul(100) / pass.budget_ns().max(1),
        );
    }
    println!(
        "  {:<10} {:>7} us  (frame {:>7} us, {:>4}%)",
        "total",
        micros(times.total()),
        micros(FRAME_NS),
        times.total().saturating_mul(100) / FRAME_NS.max(1),
    );
    // Building meshes and tiles the frame did not hold, outside every pass.
    println!(
        "  {:<10} {:>7} us  (not a cost of drawing)",
        "warm",
        micros(times.warm())
    );
}

#[test]
fn the_baseline_frame_is_measured_and_threading_does_not_change_it() {
    let threads = Threaded::new(REFERENCE_CORES);
    let baseline = Baseline::hold(&threads);
    let serial = measure(&tairix_parallel::SERIAL, &baseline);
    report("one thread", &serial);
    println!(
        "  placing one figure: {}.{:01} us on one core",
        serial.placement_ns / 1_000,
        serial.placement_ns % 1_000 / 100,
    );

    let threaded = measure(&threads, &baseline);
    report(&format!("{REFERENCE_CORES} threads"), &threaded);
    let (one, many) = (serial.times.total(), threaded.times.total().max(1));
    println!(
        "  speedup {}.{:02}x on {REFERENCE_CORES} threads (bench estimate, not a guarantee)",
        one / many,
        (one * 100 / many) % 100,
    );

    // The claim that does not depend on the machine: the bands are disjoint,
    // so running them at once draws what running them in turn drew.
    let (serial, concurrent) = (serial.pixels, threaded.pixels);
    assert_eq!(
        serial.len(),
        concurrent.len(),
        "the two runners drew different extents"
    );
    // Two blank frames are identical, so the comparison below only means
    // something once the baseline frame is known to be painted.
    assert!(
        serial.iter().all(|p| p.a == 255),
        "the baseline frame left holes"
    );
    let differing = serial
        .iter()
        .zip(&concurrent)
        .enumerate()
        .find(|(_, (a, b))| a != b);
    assert!(
        differing.is_none(),
        "handing the bands to {REFERENCE_CORES} threads changed the picture: {differing:?}"
    );
}
