//! Host tests of the starfield: its density, its flight, and a frame that
//! repaints only where stars were and are.

extern crate std;

use alloc::vec::Vec;

use tairix_abi::saver_ipc::Start;
use tairix_geometry::{Point, Rect};
use tairix_raster::{Color, Surface};
use tairix_saver::testing::{self, Screen, FRAME_NS};
use tairix_saver::{NextFrame, SaverValue};
use tairix_theme::motion::{seconds, SceneClock};

use super::{
    speed_at, Star, Starfield, CRUISE_S, CRUISE_SPEED, MAX_STARS, MIN_STARS, SETTLE_S, SURGE_S,
    WARP_S, WARP_SPEED, Z_FAR, Z_NEAR,
};

const SCREEN: (u32, u32) = (1920, 1080);

const SEC: u64 = 1_000_000_000;

/// The start the host would send for a `size` screen with these options.
fn start(size: (u32, u32), stars: &str, warp: bool) -> Start {
    let mut given = testing::start(size.0, size.1);
    testing::give(&mut given, "stars", testing::choice(stars));
    testing::give(&mut given, "warp", SaverValue::Switch(warp));
    given
}

fn made(start: &Start, seed: u64) -> Starfield {
    Starfield::new(&testing::setup(start, seed)).expect("a field")
}

fn field() -> Starfield {
    made(&start(SCREEN, "normal", true), 0)
}

/// The screen a field is drawn on.
fn screen() -> Screen {
    Screen::new(start(SCREEN, "normal", true))
}

/// Draw `stars`' frame at `now_ns`, answering when it wants the next.
fn draw(screen: &mut Screen, stars: &mut Starfield, now_ns: u64) -> NextFrame {
    let (next, asked) = screen.draw(stars, 0, now_ns);
    assert!(asked.is_empty(), "the field asks nothing of its host");
    next
}

/// Fly `stars` a frame at a time until its flight stands at `until`, as a
/// field shown without a stall would; answers the instant of the last frame.
fn fly_until(screen: &mut Screen, stars: &mut Starfield, until: u64) -> u64 {
    let mut now = FRAME_NS;
    while now < until {
        draw(screen, stars, now);
        now += FRAME_NS;
    }
    draw(screen, stars, now);
    now
}

fn footprints(stars: &Starfield) -> Vec<Rect> {
    stars
        .streaks
        .iter()
        .map(|streak| streak.footprint)
        .collect()
}

fn lit_outside(surface: &Surface, footprints: &[Rect]) -> Option<(u32, u32)> {
    for y in 0..surface.height() {
        for x in 0..surface.width() {
            let pixel = surface.get(x, y).expect("in bounds");
            if pixel.r == 0 && pixel.g == 0 && pixel.b == 0 {
                continue;
            }
            let point = Point::new(
                i32::try_from(x).expect("small"),
                i32::try_from(y).expect("small"),
            );
            if !footprints.iter().any(|rect| rect.contains(point)) {
                return Some((x, y));
            }
        }
    }
    None
}

#[test]
fn the_field_is_as_dense_as_the_screen_is_large_and_bounded_either_way() {
    let full_hd = field();
    assert!(
        (1_100..1_250).contains(&full_hd.stars.len()),
        "{}",
        full_hd.stars.len()
    );
    let tiny = made(&start((64, 48), "normal", true), 0);
    assert_eq!(tiny.stars.len(), usize::try_from(MIN_STARS).expect("small"));
    let vast = made(&start((15_360, 8_640), "normal", true), 0);
    assert_eq!(vast.stars.len(), usize::try_from(MAX_STARS).expect("small"));
}

/// Each density is its own field on any screen: the bounds scale with it,
/// so a vast screen that caps the normal field still seats a denser one.
#[test]
fn a_denser_field_has_more_stars_and_a_sparser_fewer_at_every_size() {
    for size in [(64, 48), SCREEN, (15_360, 8_640)] {
        let count = |stars: &str| made(&start(size, stars, true), 0).stars.len();
        let (sparse, normal, dense) = (count("sparse"), count("normal"), count("dense"));
        assert!(
            sparse < normal && normal < dense,
            "{size:?}: {sparse} {normal} {dense}"
        );
    }
}

/// The field is built only from what the host sends: an option it did not
/// send, or a density the manifest never declared, starts nothing.
#[test]
fn a_field_needs_both_of_its_options_as_declared() {
    let mut no_warp = testing::start(SCREEN.0, SCREEN.1);
    testing::give(&mut no_warp, "stars", testing::choice("normal"));
    let refused = |start: &Start| Starfield::new(&testing::setup(start, 0)).map(|_| ());
    assert_eq!(refused(&no_warp), Err(tairix_abi::Errno::NotFound));
    assert_eq!(
        refused(&start(SCREEN, "galactic", true)),
        Err(tairix_abi::Errno::OutOfRange)
    );
}

/// The flight cruises, surges into warp, holds, and settles back, and never
/// jumps between two speeds from one frame to the next.
#[test]
fn the_flight_surges_into_warp_and_back_without_a_jump() {
    assert!((speed_at(0.0) - CRUISE_SPEED).abs() < 1e-9);
    assert!((speed_at(CRUISE_S + SURGE_S + WARP_S / 2.0) - WARP_SPEED).abs() < 1e-9);
    let cycle = CRUISE_S + SURGE_S + WARP_S + SETTLE_S;
    assert!((speed_at(cycle) - CRUISE_SPEED).abs() < 1e-9, "and round");
    let frame = seconds(FRAME_NS);
    let most = (WARP_SPEED - CRUISE_SPEED) * 1.5 * frame / SURGE_S.min(SETTLE_S);
    let mut t = 0.0;
    while t < 2.0 * cycle {
        let step = (speed_at(t + frame) - speed_at(t)).abs();
        assert!(step <= most, "{step} at {t}");
        t += frame;
    }
}

/// Every frame asks for the next a scene frame later, so a moving field is
/// drawn at the scene clock's own cadence.
#[test]
fn each_frame_asks_for_the_next_a_scene_frame_on() {
    let mut screen = screen();
    let mut stars = field();
    assert_eq!(
        draw(&mut screen, &mut stars, FRAME_NS),
        NextFrame::At(2 * FRAME_NS)
    );
    assert_eq!(
        draw(&mut screen, &mut stars, 5 * FRAME_NS),
        NextFrame::At(6 * FRAME_NS)
    );
}

/// Every star stays inside the volume however long the flight, warp
/// included, and every one drawn stays inside the screen.
#[test]
fn stars_stay_in_the_volume_and_their_footprints_on_the_screen() {
    let mut screen = screen();
    let mut stars = field();
    let bounds = Rect::new(0, 0, SCREEN.0, SCREEN.1);
    let mut now = FRAME_NS;
    while now < 40 * SEC {
        draw(&mut screen, &mut stars, now);
        for star in &stars.stars {
            assert!(star.z > Z_NEAR && star.z <= Z_FAR, "{}", star.z);
        }
        for streak in &stars.streaks {
            assert_eq!(streak.footprint.intersection(&bounds), streak.footprint);
        }
        now += FRAME_NS * 7;
    }
}

/// What a frame leaves lit is exactly this frame's stars: the last frame's
/// footprints are erased, so a star leaves no trail behind it however fast
/// it flies.
#[test]
fn a_frame_leaves_only_its_own_stars_lit() {
    let mut screen = screen();
    let mut stars = field();
    // Into the warp, where the streaks are longest.
    assert!((speed_at(20.0) - WARP_SPEED).abs() < 1e-9, "in warp");
    let in_warp = fly_until(&mut screen, &mut stars, 20 * SEC);
    draw(&mut screen, &mut stars, in_warp + FRAME_NS);
    let footprints = footprints(&stars);
    assert!(!footprints.is_empty(), "stars to draw");
    assert_eq!(lit_outside(&screen.surface, &footprints), None);
    let reported = screen.damage.bounds();
    for rect in &footprints {
        assert_eq!(rect.intersection(&reported), *rect, "{rect:?} reported");
    }
}

const MARK: Color = Color::rgb(1, 2, 3);

/// However coarsely a frame's damage is kept — a field this dense passes the
/// rectangle budget at once — its pixel work is where its stars were and
/// are: a mark anywhere else survives the frame.
#[test]
fn a_frame_writes_only_where_its_stars_were_and_are() {
    let mut screen = screen();
    let mut stars = field();
    let in_warp = fly_until(&mut screen, &mut stars, 20 * SEC);
    let before = footprints(&stars);
    let grid: Vec<(u32, u32)> = (1..16)
        .flat_map(|i| (1..9).map(move |j| (i * SCREEN.0 / 16, j * SCREEN.1 / 9)))
        .collect();
    for &(x, y) in &grid {
        screen.surface.fill_rect(x, y, 1, 1, MARK);
    }
    draw(&mut screen, &mut stars, in_warp + FRAME_NS);
    let after = footprints(&stars);
    let mut untouched = 0;
    for &(x, y) in &grid {
        let at = Point::new(
            i32::try_from(x).expect("small"),
            i32::try_from(y).expect("small"),
        );
        if before.iter().chain(&after).any(|rect| rect.contains(at)) {
            continue;
        }
        assert!(
            screen.damage.bounds().contains(at),
            "inside the coarse damage"
        );
        let pixel = screen.surface.get(x, y).expect("in bounds");
        assert_eq!(
            (pixel.r, pixel.g, pixel.b),
            (1, 2, 3),
            "({x}, {y}) was repainted"
        );
        untouched += 1;
    }
    assert!(untouched > 0, "marks the frame had no star on");
}

#[test]
fn a_late_frame_moves_the_field_no_more_than_a_few_frames() {
    let mut screen = screen();
    let mut stars = field();
    draw(&mut screen, &mut stars, FRAME_NS);
    let before: Vec<Star> = stars.stars.clone();
    draw(&mut screen, &mut stars, 60 * SEC);
    let most = WARP_SPEED * seconds(SceneClock::MOST_FRAMES * FRAME_NS);
    let mut flown = 0;
    for (star, was) in stars.stars.iter().zip(before) {
        // A respawn scatters a star afresh; the same star kept its place
        // across the line of flight.
        if star.x.to_bits() != was.x.to_bits() || star.y.to_bits() != was.y.to_bits() {
            continue;
        }
        let moved = was.z - star.z;
        assert!(moved > 0.0 && moved <= most + 1e-9, "{moved}");
        flown += 1;
    }
    assert!(flown > 0, "most of the field is the same stars");
}

/// A late frame carries the warp cycle only as far as it carries the field: a
/// field drawn twenty seconds after its first frame, where the wall clock
/// alone would put it in full warp, is still cruising, every star moved by no
/// more than the cruise of the few frames the late frame was worth.
#[test]
fn a_late_frame_carries_the_warp_cycle_no_further_than_the_field() {
    let mut screen = screen();
    let mut stars = field();
    draw(&mut screen, &mut stars, FRAME_NS);
    let before: Vec<Star> = stars.stars.clone();
    assert!(
        (speed_at(20.0) - WARP_SPEED).abs() < 1e-9,
        "the wall clock is in warp"
    );

    draw(&mut screen, &mut stars, 20 * SEC);

    let most = CRUISE_SPEED * seconds(SceneClock::MOST_FRAMES * FRAME_NS);
    let mut flown = 0;
    for (star, was) in stars.stars.iter().zip(before) {
        if star.x.to_bits() != was.x.to_bits() || star.y.to_bits() != was.y.to_bits() {
            continue;
        }
        assert!(was.z - star.z <= most + 1e-9, "{} cruised", was.z - star.z);
        flown += 1;
    }
    assert!(flown > 0, "most of the field is the same stars");
}

/// Under reduced motion the field cruises and does not turn: in the middle of
/// what would be warp, every star has moved only a cruise frame's depth, and
/// along its own line.
#[test]
fn a_calm_field_only_cruises() {
    let mut screen = screen();
    let mut calm = start(SCREEN, "normal", true);
    calm.reduced_motion = true;
    let mut stars = made(&calm, 0);
    let warp = fly_until(&mut screen, &mut stars, 20 * SEC);
    let before: Vec<Star> = stars.stars.clone();
    draw(&mut screen, &mut stars, warp + FRAME_NS);
    let cruise = CRUISE_SPEED * seconds(FRAME_NS);
    let mut flown = 0;
    for (star, was) in stars.stars.iter().zip(before) {
        if star.x.to_bits() != was.x.to_bits() {
            continue;
        }
        assert!((was.z - star.z - cruise).abs() < 1e-9);
        flown += 1;
    }
    assert!(flown > 0);
    assert!(!stars.streaks.is_empty(), "the field is still drawn");
}

#[test]
fn the_same_seed_flies_the_same_field() {
    let mut screen = screen();
    let given = start(SCREEN, "normal", true);
    let (mut one, mut two, mut other) = (made(&given, 42), made(&given, 42), made(&given, 43));
    for stars in [&mut one, &mut two, &mut other] {
        draw(&mut screen, stars, FRAME_NS);
    }
    assert_eq!(footprints(&one), footprints(&two));
    assert_ne!(
        footprints(&one),
        footprints(&other),
        "a different seed differs"
    );
}

/// The depth each star kept between two frames moved over the second, at
/// `now` and a frame later.
fn steps(screen: &mut Screen, stars: &mut Starfield, now: u64) -> Vec<f64> {
    let before: Vec<Star> = stars.stars.clone();
    draw(screen, stars, now + FRAME_NS);
    stars
        .stars
        .iter()
        .zip(before)
        .filter(|(star, was)| star.x.to_bits() == was.x.to_bits())
        .map(|(star, was)| was.z - star.z)
        .collect()
}

/// With warp turned off the flight only cruises, however long it runs: where a
/// warping field flies at warp speed, every star of one without warp moves a
/// cruise frame's depth.
#[test]
fn a_field_without_warp_never_surges() {
    let mut screen = screen();
    let mut cruising = made(&start(SCREEN, "normal", false), 0);
    let in_warp = fly_until(&mut screen, &mut cruising, 20 * SEC);
    let cruised = steps(&mut screen, &mut cruising, in_warp);
    let cruise = CRUISE_SPEED * seconds(FRAME_NS);
    assert!(!cruised.is_empty());
    for step in cruised {
        assert!((step - cruise).abs() < 1e-9, "{step} against {cruise}");
    }
    let mut warping = field();
    let warped = fly_until(&mut screen, &mut warping, 20 * SEC);
    let warp = WARP_SPEED * seconds(FRAME_NS);
    for step in steps(&mut screen, &mut warping, warped) {
        assert!((step - warp).abs() < 1e-9, "{step} against {warp}");
    }
}
