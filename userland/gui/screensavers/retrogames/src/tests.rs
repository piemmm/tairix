//! Host tests of the retro games: the whole scene, what a frame repaints,
//! that a frame drawn in parts is the frame drawn whole — craft over the sky
//! included — and the exact coverage its lines and bands are drawn with.

extern crate std;

use tairix_abi::saver_ipc::Start;
use tairix_abi::Errno;
use tairix_geometry::{Point, Rect, Region, Scale};
use tairix_parallel::{Reversed, Threaded, SERIAL};
use tairix_raster::{Color, Surface};
use tairix_rng::NonCryptoRng;
use tairix_saver::testing::{self, Screen, FRAME_NS as TEST_SCENE_FRAME_NS};
use tairix_saver::NextFrame;
use tairix_theme::motion::{seconds, SceneClock};

use super::{
    between, covered, cumulative, dither_biases, fill_dithered, paint_rows, pick,
    second_cumulative, swept, wrap, Flight, Moment, RetroGames, Rgb, View, FLIGHT_SPEED,
    SUN_WIDEST, SWAY,
};

/// A screen small enough to paint quickly and large enough for every part.
const SCREEN: (u32, u32) = (480, 270);

const SEC: u64 = 1_000_000_000;

fn start_on(size: (u32, u32), speed: &str, calm: bool, now_ns: u64) -> Start {
    let mut given = testing::start(size.0, size.1);
    given.reduced_motion = calm;
    given.now_ns = now_ns;
    testing::give(&mut given, "speed", testing::choice(speed));
    given
}

fn scene(calm: bool, now_ns: u64) -> Flight {
    Flight::new(&testing::setup(
        &start_on(SCREEN, "normal", calm, now_ns),
        0,
    ))
    .expect("a scene")
}

/// The scene painted whole as it stands at `moment`, its craft as last
/// staged, across `runner`.
fn whole(scene: &mut Flight, moment: Moment, runner: &dyn tairix_parallel::JobRunner) -> Surface {
    let mut surface = Surface::new(SCREEN.0, SCREEN.1).expect("a surface");
    scene.paint_backdrop(&mut surface, runner, moment);
    super::draw_craft(&mut surface, runner, &scene.display, &mut scene.scratch);
    surface
}

/// The screen `scene` is first painted on, as the saver's own picture is.
fn shown(scene: &mut Flight) -> Surface {
    let mut first = Surface::new(SCREEN.0, SCREEN.1).expect("a surface");
    scene.paint(&mut first, &SERIAL);
    first
}

/// Fly `scene` on to `now_ns` over `screen`, answering what it reported.
fn advance(scene: &mut Flight, screen: &mut Surface, now_ns: u64) -> Region {
    let mut damage = Region::new();
    if scene.clock.frame_due(now_ns) {
        scene.fly(screen, &mut damage, now_ns, &SERIAL);
    }
    damage
}

/// Whether `rect` holds column `x` of row `y`.
fn holds(rect: Rect, x: u32, y: u32) -> bool {
    rect.contains(Point::new(
        i32::try_from(x).expect("small"),
        i32::try_from(y).expect("small"),
    ))
}

const MARK: Color = Color::rgb(1, 2, 3);

/// The first pixel `a` and `b` differ in, if any.
fn difference(a: &Surface, b: &Surface) -> Option<(u32, u32)> {
    assert_eq!((a.width(), a.height()), (b.width(), b.height()));
    (0..a.height())
        .flat_map(|y| (0..a.width()).map(move |x| (x, y)))
        .find(|&(x, y)| a.get(x, y) != b.get(x, y))
}

fn marked(surface: &Surface, x: u32, y: u32) -> bool {
    let pixel = surface.get(x, y).expect("in bounds");
    (pixel.r, pixel.g, pixel.b) == (MARK.r, MARK.g, MARK.b)
}

/// The flight as the scene last drew it.
fn flight(scene: &Flight) -> Moment {
    assert!(scene.clock.due_ns().is_some(), "flying");
    Moment::at(scene.clock.moved(), scene.speed, scene.clock.frame_ns())
}

/// Fly `scene`, shown on `screen`, on a frame at a time from `now` until
/// `found` holds of it, answering the time it held at; `None` if it never did
/// within `within` seconds.
fn fly_until(
    scene: &mut Flight,
    screen: &mut Surface,
    now: &mut u64,
    within: u64,
    found: impl Fn(&Flight) -> bool,
) -> Option<u64> {
    let end = *now + within * SEC;
    while *now < end {
        *now += TEST_SCENE_FRAME_NS;
        advance(scene, screen, *now);
        if found(scene) {
            return Some(*now);
        }
    }
    None
}

#[test]
fn the_scene_paints_every_pixel_opaque_a_sun_in_the_night() {
    let mut games = scene(false, 0);
    let surface = shown(&mut games);
    assert!(surface.pixels().iter().all(|pixel| pixel.a == u8::MAX));
    let high_sun = games.view.sun.0 - games.view.sun.1 / 2.0;
    let sun = surface
        .get(
            SCREEN.0 / 2,
            u32::try_from(tairix_util::mathf::round_i32(high_sun)).expect("on screen"),
        )
        .expect("in bounds");
    assert!(
        sun.r > 200 && sun.g > 100 && sun.b < 110,
        "the sun: {sun:?}"
    );
    let night = surface.get(4, 4).expect("in bounds");
    assert!(night.r < 12 && night.b < 24, "the night: {night:?}");
}

/// Before anything comes on, a frame keeps the sky and the mountains painted
/// once and repaints the floor and the sun's bands.
#[test]
fn a_frame_repaints_the_floor_and_the_suns_bands_alone() {
    let mut games = scene(false, 0);
    let mut screen = shown(&mut games);
    let (floor, zone) = (games.view.floor(), games.sky.zone());
    screen.fill(MARK);
    advance(&mut games, &mut screen, TEST_SCENE_FRAME_NS);
    assert!(games.under.boxes().is_empty(), "nothing has come on yet");
    for y in 0..SCREEN.1 {
        for x in 0..SCREEN.0 {
            if holds(floor, x, y) {
                assert!(
                    !marked(&screen, x, y),
                    "the floor at ({x}, {y}) is repainted"
                );
            } else if !holds(zone, x, y) {
                assert!(marked(&screen, x, y), "({x}, {y}) is left as it was");
            }
        }
    }
    let (centre, radius) = games.view.sun;
    let banded = u32::try_from(tairix_util::mathf::round_i32(centre + radius / 3.0)).expect("row");
    assert!(
        !marked(&screen, SCREEN.0 / 2, banded),
        "the disc's bands are repainted"
    );
}

/// With craft over the sky, a frame lays back what lay under the craft as
/// last drawn and repaints the floor and the sun's bands; every pixel of the
/// sky outside the boxes the craft reached last frame and reach now is left
/// as it was.
#[test]
fn a_frame_repaints_the_sky_only_where_craft_were_and_are() {
    let mut games = scene(false, 0);
    let mut screen = shown(&mut games);
    let mut now = 0;
    fly_until(&mut games, &mut screen, &mut now, 120, |games| {
        !games.under.boxes().is_empty()
    })
    .expect("a craft comes on over the sky within two minutes");
    screen.fill(MARK);
    let before: alloc::vec::Vec<Rect> = games.under.boxes().to_vec();
    now += TEST_SCENE_FRAME_NS;
    let damage = advance(&mut games, &mut screen, now);
    let (floor, zone) = (games.view.floor(), games.sky.zone());
    let was = |x, y| before.iter().any(|rect| holds(*rect, x, y));
    let is = |x, y| games.under.boxes().iter().any(|rect| holds(*rect, x, y));
    let mut laid_back = 0;
    for y in 0..games.view.horizon {
        for x in 0..SCREEN.0 {
            if was(x, y) {
                laid_back += 1;
                assert!(
                    !marked(&screen, x, y),
                    "({x}, {y}) under a craft is laid back"
                );
                assert!(damage.bounds().contains(Point::new(
                    i32::try_from(x).expect("small"),
                    i32::try_from(y).expect("small"),
                )));
            } else if !is(x, y) && !holds(zone, x, y) && !holds(floor, x, y) {
                assert!(marked(&screen, x, y), "({x}, {y}) is left as it was");
            }
        }
    }
    assert!(laid_back > 0, "the craft reached the sky");
}

/// The frame the flight draws in parts — the kept sky and mountains, laid
/// back only where a craft was or is, the sun's bands repainted beneath the
/// mountains' kept copy, the floor, and the craft over all — is the frame
/// painted whole, pixel for pixel, before anything comes on and while craft
/// cross the sky.
#[test]
fn a_frame_drawn_in_parts_is_the_frame_painted_whole() {
    let mut games = scene(false, 0);
    let mut screen = shown(&mut games);
    let mut now = 0;
    for step in [
        TEST_SCENE_FRAME_NS,
        2 * TEST_SCENE_FRAME_NS + 7,
        TEST_SCENE_FRAME_NS,
    ] {
        now += step;
        advance(&mut games, &mut screen, now);
        let moment = flight(&games);
        assert_eq!(
            difference(&screen, &whole(&mut games, moment, &SERIAL)),
            None,
            "the frame {now} ns in"
        );
    }
    fly_until(&mut games, &mut screen, &mut now, 120, |games| {
        !games.under.boxes().is_empty()
    })
    .expect("a craft comes on over the sky within two minutes");
    for _ in 0..12 {
        now += TEST_SCENE_FRAME_NS;
        advance(&mut games, &mut screen, now);
        let moment = flight(&games);
        assert_eq!(
            difference(&screen, &whole(&mut games, moment, &SERIAL)),
            None,
            "the frame {now} ns in, craft over the sky"
        );
    }
}

/// Split across cores, in any order, the scene — craft and all — paints
/// exactly what one core paints.
#[test]
fn the_bands_split_across_cores_paint_what_one_core_paints() {
    let mut games = scene(false, 5 * SEC);
    let mut screen = shown(&mut games);
    let mut now = 5 * SEC;
    fly_until(&mut games, &mut screen, &mut now, 120, |games| {
        !games.under.boxes().is_empty() && games.display.rows().end > games.view.horizon
    })
    .expect("craft come on");
    let moment = flight(&games);
    let one = whole(&mut games, moment, &SERIAL);
    let reversed = Reversed::new(4);
    let threaded = Threaded::new(4);
    assert_eq!(
        difference(&whole(&mut games, moment, &reversed), &one),
        None,
        "backwards"
    );
    assert_eq!(
        difference(&whole(&mut games, moment, &threaded), &one),
        None,
        "on threads"
    );
    assert!(reversed.widest() > 1, "the work was split");
}

/// The bundle paints the whole scene on its first frame and asks for one each
/// scene frame after; an early frame draws nothing.
#[test]
fn the_flight_asks_for_a_frame_each_scene_frame() {
    let given = start_on(SCREEN, "normal", false, 100);
    let mut games = RetroGames::new(&testing::setup(&given, 0)).expect("a scene");
    let mut screen = Screen::new(given);
    let (next, _) = screen.draw(&mut games, 0, 100);
    assert_eq!(
        next,
        NextFrame::At(100 + TEST_SCENE_FRAME_NS),
        "the first is drawn"
    );
    assert_eq!(screen.damage.bounds(), Rect::new(0, 0, SCREEN.0, SCREEN.1));
    let (next, _) = screen.draw(&mut games, 0, 100 + TEST_SCENE_FRAME_NS / 2);
    assert!(screen.damage.is_empty(), "an early frame draws none");
    assert_eq!(next, NextFrame::At(100 + TEST_SCENE_FRAME_NS));
    let late = 100 + 9 * TEST_SCENE_FRAME_NS;
    let (next, _) = screen.draw(&mut games, 0, late);
    assert_eq!(next, NextFrame::At(late + TEST_SCENE_FRAME_NS));
}

/// Under reduced motion the first frame is the only one: nothing is wanted
/// after it, a frame draws nothing, and nothing ever comes on.
#[test]
fn under_reduced_motion_nothing_is_drawn_after_the_first_frame() {
    let given = start_on(SCREEN, "normal", true, 0);
    let mut games = RetroGames::new(&testing::setup(&given, 0)).expect("a scene");
    let mut screen = Screen::new(given);
    assert_eq!(screen.draw(&mut games, 0, 0).0, NextFrame::Idle);
    assert_eq!(games.flight.display.rows(), 0..0, "nothing is on");
    screen.surface.fill(MARK);
    assert_eq!(screen.draw(&mut games, 0, 60 * SEC).0, NextFrame::Idle);
    assert!(screen.damage.is_empty());
    assert!(
        marked(&screen.surface, SCREEN.0 / 2, SCREEN.1 - 1),
        "no frame was drawn"
    );
}

/// A frame that came late carries the flight a few frames on, never all the
/// way to where the clock says, so the grid never leaps.
#[test]
fn a_late_frame_moves_the_flight_no_more_than_a_few_frames() {
    let mut games = scene(false, 0);
    let mut screen = shown(&mut games);
    advance(&mut games, &mut screen, TEST_SCENE_FRAME_NS);
    advance(&mut games, &mut screen, 60 * SEC);
    let flown = games.clock.moved();
    assert_eq!(games.clock.due_ns(), Some(60 * SEC + TEST_SCENE_FRAME_NS));
    let most = seconds(TEST_SCENE_FRAME_NS + SceneClock::MOST_FRAMES * TEST_SCENE_FRAME_NS);
    assert!((flown - most).abs() < 1e-9, "{flown} s flown");
}

/// The flight moves at the chosen pace of its own speed.
#[test]
fn the_flight_moves_at_the_chosen_pace() {
    let speed = |pace: &str| {
        Flight::new(&testing::setup(&start_on(SCREEN, pace, false, 0), 0))
            .expect("a scene")
            .speed
    };
    assert!((speed("normal") - FLIGHT_SPEED).abs() < 1e-12);
    assert!((speed("slow") - FLIGHT_SPEED / 2.0).abs() < 1e-12);
    assert!((speed("fast") - FLIGHT_SPEED * 1.5).abs() < 1e-12);
    let moment = Moment::at(10.0, 2.0, TEST_SCENE_FRAME_NS);
    assert!((moment.flown - 20.0).abs() < 1e-12);
    let frame = 2.0 * seconds(TEST_SCENE_FRAME_NS);
    assert!(
        moment.travel > 0.0 && moment.travel < frame,
        "exposed for less than a frame"
    );
    assert!(moment.sway.abs() <= SWAY);
    let still = Moment::default();
    assert_eq!((still.flown, still.travel, still.sway), (0.0, 0.0, 0.0));
    let refused = Flight::new(&testing::setup(&start_on(SCREEN, "warp", false, 0), 0));
    assert_eq!(refused.map(|_| ()), Err(Errno::OutOfRange));
}

#[test]
fn every_screen_shape_is_drawn_and_one_with_no_floor_or_sky_is_refused() {
    for size in [(64, 36), (1080, 1920), (2560, 1080), (800, 600), (3, 3)] {
        let mut games =
            Flight::new(&testing::setup(&start_on(size, "normal", false, 7), 0)).expect("a scene");
        let mut surface = Surface::new(size.0, size.1).expect("a surface");
        games.paint(&mut surface, &SERIAL);
        assert!(
            surface.pixels().iter().all(|pixel| pixel.a == u8::MAX),
            "{size:?}"
        );
    }
    for size in [(480, 1), (1, 1)] {
        assert_eq!(
            Flight::new(&testing::setup(&start_on(size, "normal", false, 7), 0)).map(|_| ()),
            Err(Errno::OutOfRange),
            "{size:?}"
        );
    }
}

/// The sun is never wider than the screen allows, and always sets: its centre
/// stands above the horizon and its lowest part below it.
#[test]
fn the_sun_fits_the_screen_and_sets_into_the_horizon() {
    for size in [(1920, 1080), (1080, 1920), (640, 480)] {
        let view = View::new(size, Scale::ONE).expect("a view");
        let (centre, radius) = view.sun;
        assert!(
            2.0 * radius <= f64::from(size.0) * SUN_WIDEST + 1e-9,
            "{size:?}"
        );
        let horizon = f64::from(view.horizon);
        assert!(centre < horizon && centre + radius > horizon, "{size:?}");
    }
}

/// The exact coverage of a pulse train, against a fine sum.
#[test]
fn a_pulse_train_covers_exactly_its_share_of_any_interval() {
    let fine = |from: f64, to: f64, width: f64| {
        let steps = 200_000u32;
        let step = (to - from) / f64::from(steps);
        let lit = (0..steps)
            .filter(|at| {
                let x = from + (f64::from(*at) + 0.5) * step;
                (x - tairix_util::mathf::round(x)).abs() < width / 2.0
            })
            .count();
        f64::from(u32::try_from(lit).expect("small")) / f64::from(steps)
    };
    for (from, to, width) in [
        (0.1, 0.3, 0.2),
        (0.9, 1.15, 0.2),
        (-3.7, 2.2, 0.35),
        (5.02, 5.03, 0.5),
        (1000.4, 1004.9, 0.1),
    ] {
        let exact = covered(from, to, width);
        assert!((exact - fine(from, to, width)).abs() < 1e-3, "{from}..{to}");
    }
    assert_eq!(
        covered(0.2, 0.2, 0.5).to_bits(),
        0.0_f64.to_bits(),
        "an empty interval"
    );
    assert!(
        (covered(0.25, 7.25, 0.3) - 0.3).abs() < 1e-12,
        "whole periods"
    );
    assert!((cumulative(3.0, 0.4) - 1.2).abs() < 1e-12);
    assert!((second_cumulative(1.0, 0.4) - 0.2).abs() < 1e-12);
}

/// A train seen across its travel is its coverage averaged over the travel,
/// and one that barely moved is its coverage as it stands.
#[test]
fn a_moving_train_is_its_coverage_averaged_over_the_travel() {
    let (from, to, width, travel) = (0.3, 0.45, 0.2, 0.6);
    let samples = 20_000u32;
    let mean = (0..samples)
        .map(|at| {
            let shift = travel * (f64::from(at) + 0.5) / f64::from(samples);
            covered(from - travel + shift, to - travel + shift, width)
        })
        .sum::<f64>()
        / f64::from(samples);
    assert!((swept(from, to, width, travel) - mean).abs() < 1e-6);
    assert_eq!(
        swept(from, to, width, 0.0).to_bits(),
        covered(from, to, width).to_bits(),
        "still"
    );
}

#[test]
fn a_dithered_fill_rounds_each_column_at_its_own_bias_and_clamps() {
    let mut span = [tairix_raster::Pixel::TRANSPARENT; 21];
    let biases = dither_biases(3);
    let light = Rgb::new(10.5, 300.0, -4.0);
    fill_dithered(&mut span, 5, light, &biases);
    for (at, pixel) in span.iter().enumerate() {
        let column = 5 + at;
        assert_eq!(*pixel, light.pixel(biases[column & 7]), "{column}");
        assert_eq!((pixel.g, pixel.b, pixel.a), (255, 0, 255), "clamped");
    }
    // Rounded at its bias, a level's fraction lands on both levels about it.
    let levels: alloc::vec::Vec<u8> = biases
        .iter()
        .map(|bias| Rgb::new(10.5, 0.0, 0.0).pixel(*bias).r)
        .collect();
    assert!(levels.contains(&10) && levels.contains(&11), "{levels:?}");
}

#[test]
fn every_row_is_painted_once_whatever_the_split() {
    let mut surface = Surface::new(40, 90).expect("a surface");
    let runner = Reversed::new(8);
    paint_rows(&mut surface, 10..83, &runner, &|y, span| {
        for pixel in span {
            pixel.r = pixel.r.saturating_add(1);
            pixel.g = u8::try_from(y).expect("small");
        }
    });
    for y in 0..90 {
        for x in 0..40 {
            let pixel = surface.get(x, y).expect("in bounds");
            let inside = (10..83).contains(&y);
            assert_eq!(pixel.r, u8::from(inside), "({x}, {y})");
            if inside {
                assert_eq!(u32::from(pixel.g), y);
            }
        }
    }
}

/// The draws the acts scatter from stay within what they ask for.
#[test]
fn a_draw_stays_within_its_bounds() {
    let mut rng = NonCryptoRng::seed_from_u64(11);
    for _ in 0..2_000 {
        let value = between(&mut rng, (-2.5, 4.0));
        assert!((-2.5..4.0).contains(&value), "{value}");
        assert!(pick(&mut rng, 7) < 7);
    }
    assert_eq!(pick(&mut rng, 0), 0, "no choice is the first");
    for angle in [-7.0, -3.2, -0.1, 0.0, 3.0, 3.2, 12.9] {
        let wrapped = wrap(angle);
        assert!(
            (-core::f64::consts::PI..core::f64::consts::PI).contains(&wrapped),
            "{angle}"
        );
        let turns = (angle - wrapped) / core::f64::consts::TAU;
        assert!(
            (turns - turns.round()).abs() < 1e-9,
            "{angle} keeps its direction"
        );
    }
}

/// A runner wide enough is handed as many bands as a large paint is worth,
/// past the sixty-four a fixed scratch once held it to, and paints what one
/// thread does.
#[test]
fn a_wide_runner_splits_a_large_paint_as_finely_as_it_is_worth() {
    let size = (3840, 2160);
    let new =
        || Flight::new(&testing::setup(&start_on(size, "normal", false, 7), 0)).expect("a scene");
    let mut one = Surface::new(size.0, size.1).expect("a surface");
    new().paint(&mut one, &SERIAL);
    let wide = Reversed::new(32);
    let mut split = Surface::new(size.0, size.1).expect("a surface");
    new().paint(&mut split, &wide);
    assert!(wide.widest() > 64, "{}", wide.widest());
    assert_eq!(difference(&split, &one), None);
}
