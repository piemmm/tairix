//! Host tests of the retro games: the whole scene, what a frame repaints,
//! that a frame drawn in parts is the frame drawn whole — craft over the sky
//! included — and the exact coverage its lines and bands are drawn with.

use tairix_parallel::{Reversed, Threaded, SERIAL};
use tairix_rng::NonCryptoRng;
use tairix_wallpaper::{Pace, RetroGamesOptions};
use tairix_wm::{Color, Compositor, Point, Rect, Region, Scale, Surface, WindowId};

use super::{
    between, covered, cumulative, dither_biases, fill_dithered, paint_rows, pick,
    second_cumulative, swept, wrap, Moment, RetroGames, Rgb, View, FLIGHT_SPEED, SUN_WIDEST, SWAY,
};
use crate::saver::{seconds, SAVER_FRAME_NS};
use crate::tests::compositor;

/// A screen small enough to paint quickly and large enough for every part.
const SCREEN: (u32, u32) = (480, 270);

const SEC: u64 = 1_000_000_000;

fn scene(calm: bool, now_ns: u64) -> RetroGames {
    RetroGames::new(
        SCREEN,
        Scale::ONE,
        (calm, RetroGamesOptions::default()),
        now_ns,
    )
    .expect("a scene")
}

/// The scene painted whole as it stands at `moment`, its craft as last
/// staged, across `runner`.
fn whole(
    scene: &mut RetroGames,
    moment: Moment,
    runner: &dyn tairix_parallel::JobRunner,
) -> Surface {
    let mut surface = Surface::new(SCREEN.0, SCREEN.1).expect("a surface");
    scene.paint_backdrop(&mut surface, runner, moment);
    super::draw_craft(&mut surface, runner, &scene.display, &mut scene.scratch);
    surface
}

/// A window showing `scene` as it first paints, as the screensaver's own is.
fn shown(comp: &mut Compositor, scene: &mut RetroGames) -> WindowId {
    let mut first = Surface::new(SCREEN.0, SCREEN.1).expect("a surface");
    scene.paint(&mut first, &SERIAL);
    comp.add_window(Point::new(0, 0), first)
}

fn content(comp: &Compositor, wm: WindowId) -> Surface {
    comp.window(wm)
        .and_then(tairix_wm::Window::content)
        .cloned()
        .expect("the window keeps its pixels")
}

/// Whether `rect` holds column `x` of row `y`.
fn holds(rect: Rect, x: u32, y: u32) -> bool {
    rect.contains(Point::new(
        i32::try_from(x).expect("small"),
        i32::try_from(y).expect("small"),
    ))
}

/// Fill `area` of the window with a colour no frame draws.
fn mark(comp: &mut Compositor, wm: WindowId, area: Rect) {
    let mut marked = Region::new();
    marked.add(area);
    assert!(comp.repaint_window(wm, SCREEN, &marked, |surface, _| {
        surface.fill(MARK);
    }));
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
fn flight(scene: &RetroGames) -> Moment {
    let (_, time) = scene.flying.expect("flying");
    Moment::at(time, scene.speed)
}

/// Fly `scene`, shown in `wm`, on a frame at a time from `now` until `found`
/// holds of it, answering the time it held at; `None` if it never did within
/// `within` seconds.
fn fly_until(
    scene: &mut RetroGames,
    comp: &mut Compositor,
    wm: WindowId,
    now: &mut u64,
    within: u64,
    found: impl Fn(&RetroGames) -> bool,
) -> Option<u64> {
    let end = *now + within * SEC;
    while *now < end {
        *now += SAVER_FRAME_NS;
        scene.advance(*now, wm, comp);
        if found(scene) {
            return Some(*now);
        }
    }
    None
}

#[test]
fn the_scene_paints_every_pixel_opaque_a_sun_in_the_night() {
    let mut games = scene(false, 0);
    let mut surface = Surface::new(SCREEN.0, SCREEN.1).expect("a surface");
    games.paint(&mut surface, &SERIAL);
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
    let mut comp = compositor();
    let mut games = scene(false, 0);
    let wm = shown(&mut comp, &mut games);
    let (floor, zone) = (games.view.floor(), games.sky.zone());
    mark(&mut comp, wm, Rect::new(0, 0, SCREEN.0, SCREEN.1));
    games.advance(SAVER_FRAME_NS, wm, &mut comp);
    assert!(games.under.boxes().is_empty(), "nothing has come on yet");
    let after = content(&comp, wm);
    for y in 0..SCREEN.1 {
        for x in 0..SCREEN.0 {
            if holds(floor, x, y) {
                assert!(
                    !marked(&after, x, y),
                    "the floor at ({x}, {y}) is repainted"
                );
            } else if !holds(zone, x, y) {
                assert!(marked(&after, x, y), "({x}, {y}) is left as it was");
            }
        }
    }
    let (centre, radius) = games.view.sun;
    let banded = u32::try_from(tairix_util::mathf::round_i32(centre + radius / 3.0)).expect("row");
    assert!(
        !marked(&after, SCREEN.0 / 2, banded),
        "the disc's bands are repainted"
    );
}

/// With craft over the sky, a frame lays back what lay under the craft as
/// last drawn and repaints the floor and the sun's bands; every pixel of the
/// sky outside the boxes the craft reached last frame and reach now is left
/// as it was.
#[test]
fn a_frame_repaints_the_sky_only_where_craft_were_and_are() {
    let mut comp = compositor();
    let mut games = scene(false, 0);
    let wm = shown(&mut comp, &mut games);
    let mut now = 0;
    fly_until(&mut games, &mut comp, wm, &mut now, 120, |games| {
        !games.under.boxes().is_empty()
    })
    .expect("a craft comes on over the sky within two minutes");
    mark(&mut comp, wm, Rect::new(0, 0, SCREEN.0, SCREEN.1));
    let before: alloc::vec::Vec<Rect> = games.under.boxes().to_vec();
    now += SAVER_FRAME_NS;
    games.advance(now, wm, &mut comp);
    let after = content(&comp, wm);
    let (floor, zone) = (games.view.floor(), games.sky.zone());
    let was = |x, y| before.iter().any(|rect| holds(*rect, x, y));
    let is = |x, y| games.under.boxes().iter().any(|rect| holds(*rect, x, y));
    let mut laid_back = 0;
    for y in 0..games.view.horizon {
        for x in 0..SCREEN.0 {
            if was(x, y) {
                laid_back += 1;
                assert!(
                    !marked(&after, x, y),
                    "({x}, {y}) under a craft is laid back"
                );
            } else if !is(x, y) && !holds(zone, x, y) && !holds(floor, x, y) {
                assert!(marked(&after, x, y), "({x}, {y}) is left as it was");
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
    let mut comp = compositor();
    let mut games = scene(false, 0);
    let wm = shown(&mut comp, &mut games);
    let mut now = 0;
    for step in [SAVER_FRAME_NS, 2 * SAVER_FRAME_NS + 7, SAVER_FRAME_NS] {
        now += step;
        games.advance(now, wm, &mut comp);
        let moment = flight(&games);
        assert_eq!(
            difference(&content(&comp, wm), &whole(&mut games, moment, &SERIAL)),
            None,
            "the frame {now} ns in"
        );
    }
    fly_until(&mut games, &mut comp, wm, &mut now, 120, |games| {
        !games.under.boxes().is_empty()
    })
    .expect("a craft comes on over the sky within two minutes");
    for _ in 0..12 {
        now += SAVER_FRAME_NS;
        games.advance(now, wm, &mut comp);
        let moment = flight(&games);
        assert_eq!(
            difference(&content(&comp, wm), &whole(&mut games, moment, &SERIAL)),
            None,
            "the frame {now} ns in, craft over the sky"
        );
    }
}

/// A window whose buffer went is painted whole again, just as it first was.
#[test]
fn a_frame_into_a_fresh_buffer_paints_the_whole_scene() {
    let mut comp = compositor();
    let mut games = scene(false, 0);
    let wm = comp.add_window(Point::new(0, 0), Surface::new(8, 8).expect("a surface"));
    games.advance(SAVER_FRAME_NS, wm, &mut comp);
    let moment = flight(&games);
    assert_eq!(
        difference(&content(&comp, wm), &whole(&mut games, moment, &SERIAL)),
        None
    );
}

/// Split across cores, in any order, the scene — craft and all — paints
/// exactly what one core paints.
#[test]
fn the_bands_split_across_cores_paint_what_one_core_paints() {
    let mut comp = compositor();
    let mut games = scene(false, 5 * SEC);
    let wm = shown(&mut comp, &mut games);
    let mut now = 5 * SEC;
    fly_until(&mut games, &mut comp, wm, &mut now, 120, |games| {
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

#[test]
fn the_flight_asks_for_a_frame_each_saver_frame() {
    let mut comp = compositor();
    let mut games = scene(false, 100);
    let wm = shown(&mut comp, &mut games);
    assert_eq!(games.due_ns(), 100 + SAVER_FRAME_NS, "the first is drawn");
    games.advance(100 + SAVER_FRAME_NS / 2, wm, &mut comp);
    assert_eq!(
        games.due_ns(),
        100 + SAVER_FRAME_NS,
        "an early wake draws none"
    );
    let late = 100 + 9 * SAVER_FRAME_NS;
    games.advance(late, wm, &mut comp);
    assert_eq!(games.due_ns(), late + SAVER_FRAME_NS);
}

/// Under reduced motion the first frame is the only one: nothing is due, a
/// wake draws nothing, and nothing ever comes on.
#[test]
fn under_reduced_motion_nothing_is_drawn_after_the_first_frame() {
    let mut comp = compositor();
    let mut games = scene(true, 0);
    let wm = shown(&mut comp, &mut games);
    assert_eq!(games.due_ns(), u64::MAX);
    assert_eq!(games.display.rows(), 0..0, "nothing is on");
    mark(&mut comp, wm, games.view.floor());
    games.advance(60 * SEC, wm, &mut comp);
    assert!(
        marked(&content(&comp, wm), SCREEN.0 / 2, SCREEN.1 - 1),
        "no frame was drawn"
    );
    assert_eq!(games.due_ns(), u64::MAX);
}

/// A wake that came late carries the flight a few frames on, never all the way
/// to where the clock says, so the grid never leaps.
#[test]
fn a_late_wake_moves_the_flight_no_more_than_a_few_frames() {
    let mut comp = compositor();
    let mut games = scene(false, 0);
    let wm = shown(&mut comp, &mut games);
    games.advance(SAVER_FRAME_NS, wm, &mut comp);
    games.advance(60 * SEC, wm, &mut comp);
    let (last, flown) = games.flying.expect("flying");
    assert_eq!(last, 60 * SEC);
    let most = seconds(SAVER_FRAME_NS + crate::saver::MAX_STEP_FRAMES * SAVER_FRAME_NS);
    assert!((flown - most).abs() < 1e-9, "{flown} s flown");
}

/// The flight moves at the chosen pace of its own speed.
#[test]
fn the_flight_moves_at_the_chosen_pace() {
    let speed = |pace: Pace| {
        let options = RetroGamesOptions { speed: pace };
        RetroGames::new(SCREEN, Scale::ONE, (false, options), 0)
            .expect("a scene")
            .speed
    };
    assert!((speed(Pace::Normal) - FLIGHT_SPEED).abs() < 1e-12);
    assert!((speed(Pace::Slow) - FLIGHT_SPEED / 2.0).abs() < 1e-12);
    assert!((speed(Pace::Fast) - FLIGHT_SPEED * 1.5).abs() < 1e-12);
    let moment = Moment::at(10.0, 2.0);
    assert!((moment.flown - 20.0).abs() < 1e-12);
    let frame = 2.0 * seconds(SAVER_FRAME_NS);
    assert!(
        moment.travel > 0.0 && moment.travel < frame,
        "exposed for less than a frame"
    );
    assert!(moment.sway.abs() <= SWAY);
    let still = Moment::default();
    assert_eq!((still.flown, still.travel, still.sway), (0.0, 0.0, 0.0));
}

#[test]
fn every_screen_shape_is_drawn_and_one_with_no_floor_or_sky_is_refused() {
    for size in [(64, 36), (1080, 1920), (2560, 1080), (800, 600), (3, 3)] {
        let mut games = RetroGames::new(size, Scale::ONE, (false, RetroGamesOptions::default()), 7)
            .expect("a scene");
        let mut surface = Surface::new(size.0, size.1).expect("a surface");
        games.paint(&mut surface, &SERIAL);
        assert!(
            surface.pixels().iter().all(|pixel| pixel.a == u8::MAX),
            "{size:?}"
        );
    }
    for size in [(0, 270), (480, 0), (480, 1)] {
        assert!(
            RetroGames::new(size, Scale::ONE, (false, RetroGamesOptions::default()), 7).is_none(),
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
    let mut span = [tairix_wm::Pixel::TRANSPARENT; 21];
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
