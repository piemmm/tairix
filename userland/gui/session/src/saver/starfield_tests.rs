//! Host tests of the starfield: its density, its flight, and a frame that
//! repaints only where stars were and are.

use tairix_wallpaper::{StarDensity, StarfieldOptions};
use tairix_wm::{Color, Compositor, Point, Rect, Region, Scale, Surface};

use super::{
    speed_at, Starfield, CRUISE_S, CRUISE_SPEED, MAX_STARS, MIN_STARS, SETTLE_S, SURGE_S, WARP_S,
    WARP_SPEED, Z_FAR, Z_NEAR,
};
use crate::tests::compositor;
use tairix_theme::motion::SceneClock;

const SCREEN: (u32, u32) = (1920, 1080);

const SEC: u64 = 1_000_000_000;

fn field(now_ns: u64) -> Starfield {
    Starfield::new(
        SCREEN,
        Scale::ONE,
        (false, StarfieldOptions::default()),
        now_ns,
    )
    .expect("a field")
}

/// A black window the field draws into, as the screensaver's own is.
fn canvas(comp: &mut Compositor) -> tairix_wm::WindowId {
    let mut black = Surface::new(SCREEN.0, SCREEN.1).expect("a surface");
    black.fill(tairix_wm::Color::rgb(0, 0, 0));
    comp.add_window(Point::new(0, 0), black)
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
    let full_hd = field(0);
    assert!(
        (1_100..1_250).contains(&full_hd.stars.len()),
        "{}",
        full_hd.stars.len()
    );
    let tiny = Starfield::new(
        (64, 48),
        Scale::ONE,
        (false, StarfieldOptions::default()),
        0,
    )
    .expect("a field");
    assert_eq!(tiny.stars.len(), usize::try_from(MIN_STARS).expect("small"));
    let vast = Starfield::new(
        (15_360, 8_640),
        Scale::ONE,
        (false, StarfieldOptions::default()),
        0,
    )
    .expect("a field");
    assert_eq!(vast.stars.len(), usize::try_from(MAX_STARS).expect("small"));
}

/// The flight cruises, surges into warp, holds, and settles back, and never
/// jumps between two speeds from one frame to the next.
#[test]
fn the_flight_surges_into_warp_and_back_without_a_jump() {
    assert!((speed_at(0.0) - CRUISE_SPEED).abs() < 1e-9);
    assert!((speed_at(CRUISE_S + SURGE_S + WARP_S / 2.0) - WARP_SPEED).abs() < 1e-9);
    let cycle = CRUISE_S + SURGE_S + WARP_S + SETTLE_S;
    assert!((speed_at(cycle) - CRUISE_SPEED).abs() < 1e-9, "and round");
    let frame = super::seconds(SceneClock::FRAME_NS);
    let most = (WARP_SPEED - CRUISE_SPEED) * 1.5 * frame / SURGE_S.min(SETTLE_S);
    let mut t = 0.0;
    while t < 2.0 * cycle {
        let step = (speed_at(t + frame) - speed_at(t)).abs();
        assert!(step <= most, "{step} at {t}");
        t += frame;
    }
}

/// Every star stays inside the volume however long the flight, warp
/// included, and every one drawn stays inside the screen.
#[test]
fn stars_stay_in_the_volume_and_their_footprints_on_the_screen() {
    let mut comp = compositor();
    let wm = canvas(&mut comp);
    let mut stars = field(0);
    let screen = Rect::new(0, 0, SCREEN.0, SCREEN.1);
    let mut now = 0;
    while now < 40 * SEC {
        stars.advance(now, wm, &mut comp);
        for star in &stars.stars {
            assert!(star.z > Z_NEAR && star.z <= Z_FAR, "{}", star.z);
        }
        for streak in &stars.streaks {
            assert_eq!(streak.footprint.intersection(&screen), streak.footprint);
        }
        now += SceneClock::FRAME_NS * 7;
    }
}

/// What a frame leaves lit is exactly this frame's stars: the last frame's
/// footprints are erased, so a star leaves no trail behind it however fast
/// it flies.
#[test]
fn a_frame_leaves_only_its_own_stars_lit() {
    let mut comp = compositor();
    let wm = canvas(&mut comp);
    let mut stars = field(0);
    // Into the warp, where the streaks are longest.
    let in_warp = 20 * SEC;
    assert!((speed_at(20.0) - WARP_SPEED).abs() < 1e-9, "in warp");
    stars.advance(in_warp, wm, &mut comp);
    stars.advance(in_warp + SceneClock::FRAME_NS, wm, &mut comp);
    let footprints: alloc::vec::Vec<Rect> = stars
        .streaks
        .iter()
        .map(|streak| streak.footprint)
        .collect();
    assert!(!footprints.is_empty(), "stars to draw");
    let content = comp
        .window(wm)
        .and_then(tairix_wm::Window::content)
        .expect("the window keeps its pixels");
    assert_eq!(lit_outside(content, &footprints), None);
}

/// Mark `points` on the field's window, as a frame never would.
fn mark(comp: &mut Compositor, wm: tairix_wm::WindowId, points: &[(u32, u32)]) {
    let mut area = Region::new();
    for &(x, y) in points {
        area.add(Rect::new(
            i32::try_from(x).expect("small"),
            i32::try_from(y).expect("small"),
            1,
            1,
        ));
    }
    assert!(comp.repaint_window(wm, SCREEN, &area, |surface, _| {
        for &(x, y) in points {
            surface.fill_rect(x, y, 1, 1, MARK);
        }
    }));
}

const MARK: Color = Color::rgb(1, 2, 3);

/// However coarsely a frame's damage is kept — a field this dense passes the
/// rectangle budget at once — its pixel work is where its stars were and
/// are: a mark anywhere else survives the frame.
#[test]
fn a_frame_writes_only_where_its_stars_were_and_are() {
    let mut comp = compositor();
    let wm = canvas(&mut comp);
    let mut stars = field(0);
    let in_warp = 20 * SEC;
    stars.advance(in_warp, wm, &mut comp);
    let before: alloc::vec::Vec<Rect> = stars.streaks.iter().map(|s| s.footprint).collect();
    let grid: alloc::vec::Vec<(u32, u32)> = (1..16)
        .flat_map(|i| (1..9).map(move |j| (i * SCREEN.0 / 16, j * SCREEN.1 / 9)))
        .collect();
    mark(&mut comp, wm, &grid);
    stars.advance(in_warp + SceneClock::FRAME_NS, wm, &mut comp);
    let after: alloc::vec::Vec<Rect> = stars.streaks.iter().map(|s| s.footprint).collect();
    let content = comp
        .window(wm)
        .and_then(tairix_wm::Window::content)
        .expect("the window keeps its pixels");
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
            stars.damage.bounds().contains(at),
            "inside the coarse damage"
        );
        let pixel = content.get(x, y).expect("in bounds");
        assert_eq!(
            (pixel.r, pixel.g, pixel.b),
            (1, 2, 3),
            "({x}, {y}) was repainted"
        );
        untouched += 1;
    }
    assert!(untouched > 0, "marks the frame had no star on");
}

/// A window whose pixels were lost is handed back a fresh buffer, which the
/// frame lays black whole before drawing its stars.
#[test]
fn a_frame_into_a_fresh_buffer_is_black_but_for_its_stars() {
    let mut comp = compositor();
    let wm = canvas(&mut comp);
    let mut stars = field(0);
    stars.advance(0, wm, &mut comp);
    let mut stale = Surface::new(8, 8).expect("a surface");
    stale.fill(MARK);
    assert!(comp.set_surface(wm, stale));
    stars.advance(SceneClock::FRAME_NS, wm, &mut comp);
    let footprints: alloc::vec::Vec<Rect> = stars
        .streaks
        .iter()
        .map(|streak| streak.footprint)
        .collect();
    let content = comp
        .window(wm)
        .and_then(tairix_wm::Window::content)
        .expect("the window keeps its pixels");
    assert_eq!((content.width(), content.height()), SCREEN);
    assert_eq!(lit_outside(content, &footprints), None);
}

#[test]
fn a_late_wake_moves_the_field_no_more_than_a_few_frames() {
    let mut comp = compositor();
    let wm = canvas(&mut comp);
    let mut stars = field(0);
    stars.advance(0, wm, &mut comp);
    let before: alloc::vec::Vec<super::Star> = stars.stars.clone();
    stars.advance(60 * SEC, wm, &mut comp);
    let most =
        WARP_SPEED * tairix_theme::motion::seconds(SceneClock::MOST_FRAMES * SceneClock::FRAME_NS);
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

/// Under reduced motion the field cruises and does not turn: in the middle of
/// what would be warp, every star has moved only a cruise frame's depth, and
/// along its own line.
#[test]
fn a_calm_field_only_cruises() {
    let mut comp = compositor();
    let wm = canvas(&mut comp);
    let mut stars = Starfield::new(SCREEN, Scale::ONE, (true, StarfieldOptions::default()), 0)
        .expect("a field");
    let warp = 20 * SEC;
    stars.advance(warp, wm, &mut comp);
    let before: alloc::vec::Vec<super::Star> = stars.stars.clone();
    stars.advance(warp + SceneClock::FRAME_NS, wm, &mut comp);
    let cruise = CRUISE_SPEED * super::seconds(SceneClock::FRAME_NS);
    let mut flown = 0;
    for (star, was) in stars.stars.iter().zip(before) {
        if star.x.to_bits() != was.x.to_bits() {
            continue;
        }
        assert!((was.z - star.z - cruise).abs() < 1e-9);
        flown += 1;
    }
    assert!(flown > 0);
    let drawn = |field: &Starfield| field.streaks.len();
    assert!(drawn(&stars) > 0, "the field is still drawn");
}

#[test]
fn the_same_start_flies_the_same_field() {
    let mut comp = compositor();
    let wm = canvas(&mut comp);
    let (mut one, mut two, mut other) = (field(42), field(42), field(43));
    for stars in [&mut one, &mut two, &mut other] {
        stars.advance(42, wm, &mut comp);
    }
    let spots = |field: &Starfield| -> alloc::vec::Vec<Rect> {
        field
            .streaks
            .iter()
            .map(|streak| streak.footprint)
            .collect()
    };
    assert_eq!(spots(&one), spots(&two));
    assert_ne!(spots(&one), spots(&other), "a different start differs");
}

/// Each density is its own field on any screen: the bounds scale with it,
/// so a vast screen that caps the normal field still seats a denser one.
#[test]
fn a_denser_field_has_more_stars_and_a_sparser_fewer_at_every_size() {
    for size in [(64, 48), SCREEN, (15_360, 8_640)] {
        let count = |stars: StarDensity| {
            Starfield::new(
                size,
                Scale::ONE,
                (false, StarfieldOptions { stars, warp: true }),
                0,
            )
            .expect("a field")
            .stars
            .len()
        };
        let (sparse, normal, dense) = (
            count(StarDensity::Sparse),
            count(StarDensity::Normal),
            count(StarDensity::Dense),
        );
        assert!(
            sparse < normal && normal < dense,
            "{size:?}: {sparse} {normal} {dense}"
        );
    }
}

/// With warp turned off the flight only cruises, however long it runs.
#[test]
fn a_field_without_warp_never_surges() {
    let mut comp = compositor();
    let wm = canvas(&mut comp);
    let cruising = StarfieldOptions {
        stars: StarDensity::Normal,
        warp: false,
    };
    let mut stars = Starfield::new(SCREEN, Scale::ONE, (false, cruising), 0).expect("a field");
    // Well into where a warping field would be at full warp.
    let in_warp = 20 * SEC;
    stars.advance(in_warp, wm, &mut comp);
    stars.advance(in_warp + SceneClock::FRAME_NS, wm, &mut comp);
    let longest = stars
        .streaks
        .iter()
        .map(|streak| {
            tairix_util::mathf::hypot(streak.head.0 - streak.tail.0, streak.head.1 - streak.tail.1)
        })
        .fold(0.0f64, f64::max);
    let mut warping = field(0);
    warping.advance(in_warp, wm, &mut comp);
    warping.advance(in_warp + SceneClock::FRAME_NS, wm, &mut comp);
    let warp_longest = warping
        .streaks
        .iter()
        .map(|streak| {
            tairix_util::mathf::hypot(streak.head.0 - streak.tail.0, streak.head.1 - streak.tail.1)
        })
        .fold(0.0f64, f64::max);
    assert!(
        longest < warp_longest / 4.0,
        "{longest} against {warp_longest}"
    );
}
