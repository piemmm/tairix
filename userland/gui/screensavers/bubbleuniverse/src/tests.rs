//! Host tests of the screensaver: its options, its pace, a frame that paints
//! only the square about the bubble, the bubble's wander, reduced motion,
//! and the same frame drawn across cores.

extern crate std;

use alloc::vec::Vec;

use tairix_abi::saver_ipc::Start;
use tairix_abi::Errno;
use tairix_geometry::{Point, Rect, Region};
use tairix_parallel::{JobRunner, Reversed, Threaded, SERIAL};
use tairix_raster::{Color, Surface};
use tairix_saver::testing::{self, Screen, FRAME_NS};
use tairix_saver::{Frame, Host, NextFrame, SaverValue, Scene};
use tairix_theme::motion::{seconds, SceneClock};
use tairix_util::mathf;

use super::{
    BubbleUniverse, Dot, Pace, Universe, CURVES, EXPOSURES, REFERENCE_RADIUS, STEPS, STILL_NS,
    TIME_RATE,
};

/// A universe drawn across `runner`, as the saver draws one across its pool.
struct Across<'r> {
    universe: Universe,
    runner: &'r dyn JobRunner,
}

impl Scene for Across<'_> {
    fn frame(
        &mut self,
        surface: &mut Surface,
        damage: &mut Region,
        frame: &Frame<'_>,
        _host: &mut Host<'_>,
    ) -> NextFrame {
        self.universe
            .frame(surface, damage, frame.now_ns, self.runner)
    }
}

const SCREEN: (u32, u32) = (1280, 720);

const SEC: u64 = 1_000_000_000;

/// The start the host would send for a `size` screen with these options.
fn start(size: (u32, u32), colours: &str, speed: &str, glow: bool) -> Start {
    let mut given = testing::start(size.0, size.1);
    testing::give(&mut given, "colours", testing::choice(colours));
    testing::give(&mut given, "speed", testing::choice(speed));
    testing::give(&mut given, "glow", SaverValue::Switch(glow));
    given
}

fn usual() -> Start {
    start(SCREEN, "original", "normal", true)
}

fn across<'r>(start: &Start, seed: u64, runner: &'r dyn JobRunner) -> Across<'r> {
    Across {
        universe: Universe::new(&testing::setup(start, seed), runner).expect("a universe"),
        runner,
    }
}

fn made(start: &Start, seed: u64) -> Across<'static> {
    across(start, seed, &SERIAL)
}

/// Draw `universe`'s frame at `now_ns`, answering when it wants the next.
fn draw(screen: &mut Screen, universe: &mut impl Scene, now_ns: u64) -> NextFrame {
    let (next, asked) = screen.draw(universe, 0, now_ns);
    assert!(asked.is_empty(), "the universe asks nothing of its host");
    next
}

/// The pixels of `surface` lit off its ground.
fn lit(surface: &Surface) -> usize {
    surface
        .pixels()
        .iter()
        .filter(|pixel| pixel.r != 0 || pixel.g != 0 || pixel.b != 0)
        .count()
}

#[test]
fn a_universe_needs_each_option_as_its_manifest_declares_it() {
    let refused = |start: &Start| BubbleUniverse::new(&testing::setup(start, 0)).map(|_| ());
    let mut no_glow = testing::start(SCREEN.0, SCREEN.1);
    testing::give(&mut no_glow, "colours", testing::choice("nebula"));
    testing::give(&mut no_glow, "speed", testing::choice("slow"));
    assert_eq!(refused(&no_glow), Err(Errno::NotFound));
    assert_eq!(
        refused(&start(SCREEN, "plaid", "normal", true)),
        Err(Errno::OutOfRange)
    );
    assert_eq!(
        refused(&start(SCREEN, "ice", "warp", true)),
        Err(Errno::OutOfRange)
    );
    for colours in ["original", "nebula", "aurora", "ember", "ice", "spectrum"] {
        for speed in ["slow", "normal", "fast"] {
            for glow in [true, false] {
                assert_eq!(refused(&start(SCREEN, colours, speed, glow)), Ok(()));
            }
        }
    }
}

/// Every frame asks for the next a scene frame later, so the universe moves
/// at the scene clock's own cadence.
#[test]
fn each_frame_asks_for_the_next_a_scene_frame_on() {
    let mut screen = Screen::new(usual());
    let mut universe = made(&usual(), 1);
    assert_eq!(
        draw(&mut screen, &mut universe, FRAME_NS),
        NextFrame::At(2 * FRAME_NS)
    );
    assert_eq!(
        draw(&mut screen, &mut universe, 7 * FRAME_NS),
        NextFrame::At(8 * FRAME_NS)
    );
}

/// A frame repaints the square about the bubble and nothing else: every
/// pixel outside it keeps whatever it held, and the damage names the square.
#[test]
fn a_frame_paints_only_the_square_about_the_bubble() {
    const MARK: Color = Color::rgb(1, 2, 3);
    let mut screen = Screen::new(usual());
    let mut universe = made(&usual(), 2);
    draw(&mut screen, &mut universe, FRAME_NS);
    screen.surface.fill(MARK);
    draw(&mut screen, &mut universe, 2 * FRAME_NS);
    let painted = universe.universe.painted;
    assert!(!painted.is_empty());
    assert_eq!(
        screen.damage.bounds(),
        painted,
        "a wander this short uncovers nothing"
    );
    let mut outside = 0;
    for y in 0..SCREEN.1 {
        for x in 0..SCREEN.0 {
            let at = Point::new(
                i32::try_from(x).expect("small"),
                i32::try_from(y).expect("small"),
            );
            let pixel = screen.surface.get(x, y).expect("inside");
            if painted.contains(at) {
                assert_eq!(pixel.a, u8::MAX, "({x}, {y}) opaque");
            } else {
                assert_eq!(
                    (pixel.r, pixel.g, pixel.b),
                    (1, 2, 3),
                    "({x}, {y}) untouched"
                );
                outside += 1;
            }
        }
    }
    assert!(outside > 0, "the square leaves the screen's sides alone");
    assert!(lit(&screen.surface) > 50_000, "the bubble is drawn");
}

/// However long it wanders, the bubble's square stays about the screen's
/// centre and its disc on the screen.
#[test]
fn the_bubble_wanders_but_never_leaves_the_screen() {
    let start = usual();
    let universe = made(&start, 3);
    let course = &universe.universe.course;
    let screen = Rect::new(0, 0, SCREEN.0, SCREEN.1);
    let mut moved = 0.0f64;
    let first = course.at(0.0).centre;
    for second in 0..2_000 {
        let instant = course.at(f64::from(second) * 1.7);
        let (x, y) = instant.centre;
        assert!(
            x - course.radius >= 0.0 && x + course.radius <= f64::from(SCREEN.0),
            "{x}"
        );
        assert!(
            y - course.radius >= 0.0 && y + course.radius <= f64::from(SCREEN.1),
            "{y}"
        );
        let placed = course.placed(instant.centre);
        assert_eq!(placed.painted, placed.frame.intersection(&screen));
        moved = moved.max((x - first.0).abs()).max((y - first.1).abs());
    }
    assert!(moved > 4.0, "it does wander: {moved}");
}

/// Where the bubble wandered from, the frame clears to the ground what the
/// last frame painted, and says so.
#[test]
fn a_frame_clears_where_the_bubble_no_longer_reaches() {
    let mut screen = Screen::new(usual());
    let mut universe = made(&usual(), 4);
    let mut now = FRAME_NS;
    draw(&mut screen, &mut universe, now);
    let (before, after) = loop {
        let before = universe.universe.painted;
        now += SceneClock::MOST_FRAMES * FRAME_NS;
        draw(&mut screen, &mut universe, now);
        let after = universe.universe.painted;
        if after != before {
            break (before, after);
        }
        assert!(now < 600 * SEC, "the bubble wanders within ten minutes");
    };
    let mut left = Region::new();
    left.add(before);
    left.subtract(after);
    assert!(!left.is_empty());
    for rect in left.rects() {
        assert_eq!(
            rect.intersection(&screen.damage.bounds()),
            *rect,
            "reported"
        );
        for y in rect.top()..rect.bottom() {
            for x in rect.left()..rect.right() {
                let pixel = screen
                    .surface
                    .get(
                        u32::try_from(x).expect("on screen"),
                        u32::try_from(y).expect("on screen"),
                    )
                    .expect("inside");
                assert_eq!(
                    (pixel.r, pixel.g, pixel.b, pixel.a),
                    (0, 0, 0, u8::MAX),
                    "({x}, {y}) cleared"
                );
            }
        }
    }
}

#[test]
fn the_same_seed_draws_the_same_universe_and_another_another() {
    let given = usual();
    let drawn = |seed: u64| {
        let mut universe = made(&given, seed);
        let mut screen = Screen::new(given.clone());
        draw(&mut screen, &mut universe, FRAME_NS);
        screen.surface
    };
    let (one, two, other) = (drawn(42), drawn(42), drawn(43));
    assert!(one == two, "the same seed draws the same picture");
    assert!(one != other, "another seed differs");
}

/// Under reduced motion the picture is still: a frame asked for early draws
/// nothing, and only after a minute does another picture take its place.
#[test]
fn under_reduced_motion_each_picture_holds_a_minute() {
    let mut calm = usual();
    calm.reduced_motion = true;
    let mut universe = made(&calm, 5);
    let mut screen = Screen::new(calm);
    assert_eq!(
        draw(&mut screen, &mut universe, FRAME_NS),
        NextFrame::At(FRAME_NS + STILL_NS)
    );
    assert!(!screen.damage.is_empty());
    let first = screen.surface.try_clone().expect("a copy");
    assert_eq!(
        draw(&mut screen, &mut universe, 30 * SEC),
        NextFrame::At(FRAME_NS + STILL_NS)
    );
    assert!(screen.damage.is_empty(), "nothing new to draw");
    assert!(screen.surface == first, "still");
    let later = FRAME_NS + STILL_NS;
    assert_eq!(
        draw(&mut screen, &mut universe, later),
        NextFrame::At(later + STILL_NS)
    );
    assert!(!screen.damage.is_empty());
    assert!(screen.surface != first, "another picture");
}

/// A frame drawn long after the last carries the picture on only the few
/// frames the scene clock allows, never all the way to the wall clock.
#[test]
fn a_late_frame_moves_the_picture_only_a_few_frames_on() {
    let start = usual();
    let mut universe = made(&start, 6);
    let mut screen = Screen::new(start);
    draw(&mut screen, &mut universe, FRAME_NS);
    let moved = |universe: &Universe| match &universe.pace {
        Pace::Moving(clock) => clock.moved(),
        Pace::Still { .. } => unreachable!("moving"),
    };
    let before = moved(&universe.universe);
    draw(&mut screen, &mut universe, 90 * SEC);
    let after = moved(&universe.universe);
    let step = after - before;
    assert!(
        (step - seconds(SceneClock::MOST_FRAMES * FRAME_NS)).abs() < 1e-9,
        "{step}"
    );
    let course = &universe.universe.course;
    let advanced = course.at(after).time - course.at(before).time;
    assert!((advanced - TIME_RATE * step).abs() < 1e-9);
}

/// A faster pace carries the picture further each second, a slower one less.
#[test]
fn the_speed_scales_the_picture_s_motion() {
    let at = |speed: &str| {
        let universe = made(&start(SCREEN, "original", speed, true), 7);
        let course = &universe.universe.course;
        course.at(10.0).time - course.at(0.0).time
    };
    assert!((at("normal") - 10.0 * TIME_RATE).abs() < 1e-9);
    assert!((at("slow") - 5.0 * TIME_RATE).abs() < 1e-9);
    assert!((at("fast") - 15.0 * TIME_RATE).abs() < 1e-9);
}

/// With its glow on the light hazes the square about the bubble; with it off
/// the dark between the curves, and beyond the disc, is black.
#[test]
fn the_glow_lights_the_dark_about_the_curves() {
    let mut counts = Vec::new();
    for glow in [true, false] {
        let given = start(SCREEN, "original", "normal", glow);
        let mut universe = made(&given, 8);
        let mut screen = Screen::new(given);
        draw(&mut screen, &mut universe, FRAME_NS);
        counts.push(lit(&screen.surface));
    }
    assert!(counts[0] > counts[1] * 3 / 2, "{counts:?}");
}

/// Every screen sheds as much light, area for area, as every other: a dot as
/// large as its screen asks for carries the light its size is worth, however
/// its radius rounds. What is shown of that light follows the tone curve, so a
/// small screen's denser, dimmer dots show a little brighter, as a picture
/// shrunk in light rather than in its encoding does.
#[test]
fn every_screen_sheds_the_same_light_area_for_area() {
    let shed = |size: (u32, u32)| {
        let universe = made(&start(size, "original", "normal", false), 9);
        let radius = universe.universe.course.radius;
        let rounded = mathf::round_i32(radius / REFERENCE_RADIUS).max(1);
        let dot = Dot::new(u32::try_from(rounded).expect("a radius"));
        let light: u64 = universe
            .universe
            .colours
            .iter()
            .flatten()
            .map(|&c| u64::from(c))
            .sum();
        let light = f64::from(u32::try_from(light).expect("a frame's light"));
        let energy = f64::from(u32::try_from(dot.energy()).expect("small"));
        light * energy * mathf::real(EXPOSURES) / (radius * radius)
    };
    let reference = shed((1920, 1080));
    for size in [(1280, 720), (2560, 1440), (3840, 2160), (7680, 4320)] {
        let ratio = shed(size) / reference;
        assert!((ratio - 1.0).abs() < 0.01, "{size:?}: {ratio}");
    }
}

/// A frame traces every curve's every point at each of its moments, and draws
/// them across cores exactly as on one, whichever band runs first and when
/// they run at once.
#[test]
fn a_frame_drawn_across_cores_is_the_frame_drawn_on_one() {
    let given = usual();
    let mut one = made(&given, 10);
    let mut whole = Screen::new(given.clone());
    draw(&mut whole, &mut one, FRAME_NS);
    assert_eq!(one.universe.spots.len(), EXPOSURES * CURVES * STEPS);
    let reversed = Reversed::new(4);
    let threaded = Threaded::new(4);
    let runners: [&dyn JobRunner; 2] = [&reversed, &threaded];
    for runner in runners {
        let mut split = across(&given, 10, runner);
        let mut banded = Screen::new(given.clone());
        draw(&mut banded, &mut split, FRAME_NS);
        assert!(banded.surface == whole.surface, "the cores drew the frame");
        assert_eq!(banded.damage, whole.damage);
    }
    assert!(reversed.widest() > 1, "the frame was split");
}
