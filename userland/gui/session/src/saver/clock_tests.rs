//! Host tests of the clock screensaver: what it says, where it goes, and
//! that a clock it cannot read waits rather than spins.

use tairix_abi::time::{Time64, WallClockReading, WallTimeState};
use tairix_theme::{Accessibility, Motion, Theme};
use tairix_wallpaper::ClockOptions;
use tairix_wm::{Compositor, Point, Rect, Scale, Surface, WindowId};

use super::{ClockFace, SaverIdentity};
use crate::saver::SAVER_FRAME_NS;
use crate::tests::compositor;
use tairix_taskbar::clock::UNSET_LABEL;

const SCREEN: (u32, u32) = (1920, 1080);
const SEC: u64 = 1_000_000_000;

/// 2024-02-29 13:46:07 UTC.
fn reading() -> WallClockReading {
    after(0)
}

/// The reading `secs` seconds after [`reading`]: the wall clock as it reads
/// that much later.
fn after(secs: i64) -> WallClockReading {
    WallClockReading::new(
        Time64::from_secs(1_709_214_367 + secs),
        WallTimeState::Trusted,
    )
}

fn face(motion: Motion, wall: Option<WallClockReading>) -> ClockFace {
    face_showing(motion, wall, ClockOptions::default())
}

fn face_showing(
    motion: Motion,
    wall: Option<WallClockReading>,
    options: ClockOptions,
) -> ClockFace {
    let identity = SaverIdentity {
        user: "ann".into(),
        host: "tairix-box".into(),
    };
    let theme = Theme::dark().with_axes(Accessibility {
        motion,
        ..Accessibility::default()
    });
    ClockFace::new(&identity, &theme, (Scale::ONE, SCREEN), (wall, 0), options)
}

/// A face that tells neither the date nor who is signed in shows the time
/// alone, and a smaller block for it.
#[test]
fn the_options_leave_out_the_date_and_who_is_signed_in() {
    let whole = face(Motion::Full, Some(reading()));
    let bare = face_showing(
        Motion::Full,
        Some(reading()),
        ClockOptions {
            date: false,
            identity: false,
        },
    );
    assert!(bare.date.is_empty());
    assert!(bare.identity.is_empty());
    let height = |face: &ClockFace| face.block.as_ref().map_or(0, Surface::height);
    assert!(height(&bare) < height(&whole));
    // A minute on, the date it was told to leave out stays out.
    let mut later = bare;
    later.read(Some(after(60)), 60 * SEC);
    assert!(later.date.is_empty());
}

fn canvas(comp: &mut Compositor) -> WindowId {
    let mut black = Surface::new(SCREEN.0, SCREEN.1).expect("a surface");
    black.fill(tairix_wm::Color::rgb(0, 0, 0));
    comp.add_window(Point::new(0, 0), black)
}

fn inside_the_screen(face: &ClockFace) -> bool {
    let screen = Rect::new(0, 0, SCREEN.0, SCREEN.1);
    let block = face.block_rect();
    !block.is_empty() && block.intersection(&screen) == block
}

#[test]
fn the_identity_line_names_who_and_where() {
    let named = |user: &str, host: &str| {
        SaverIdentity {
            user: user.into(),
            host: host.into(),
        }
        .line()
    };
    assert_eq!(named("ann", "tairix-box"), "ann \u{b7} tairix-box");
    assert_eq!(named("ann", ""), "ann");
    assert_eq!(named("", "tairix-box"), "tairix-box");
    assert_eq!(named("", ""), "");
}

/// The face tells the icon bar's own time, and the day's ISO date.
#[test]
fn the_face_tells_the_bars_time_and_the_date() {
    let face = face(Motion::Full, Some(reading()));
    assert_eq!(face.clock.label(), "13:46");
    assert_eq!(face.date, "2024-02-29");
    assert!(inside_the_screen(&face));
    assert_eq!(face.due_ns(), 53 * SEC, "the next minute boundary");
}

#[test]
fn an_unset_clock_shows_the_placeholder_and_no_date() {
    let unset = WallClockReading::new(Time64::UNIX_EPOCH, WallTimeState::Unset);
    let face = face(Motion::Full, Some(unset));
    assert_eq!(face.clock.label(), UNSET_LABEL);
    assert!(face.date.is_empty());
}

/// At each minute the block fades out, moves, and fades back in, and ends
/// somewhere else on the screen at full strength.
#[test]
fn the_face_moves_when_the_minute_turns() {
    let mut comp = compositor();
    let wm = canvas(&mut comp);
    let mut face = face(Motion::Full, Some(reading()));
    assert!(face.fade_ns > 0, "the theme animates a move");
    let was = face.at;
    let tick = face.due_ns();
    let mut later = || Some(after(53));
    face.advance(tick, wm, &mut comp, &mut later);
    assert!(face.moving.is_some());
    assert_eq!(
        face.due_ns(),
        tick + SAVER_FRAME_NS,
        "frames while it moves"
    );
    let mut now = tick;
    while face.moving.is_some() {
        now += SAVER_FRAME_NS;
        face.advance(now, wm, &mut comp, &mut later);
        assert!(now < tick + 3 * face.fade_ns, "a move ends");
    }
    assert_eq!(face.strength, u8::MAX);
    assert!(inside_the_screen(&face));
    assert_ne!(face.at, was, "moved on");
    assert!(face.due_ns() > now, "and waits for the next minute");
}

#[test]
fn under_reduced_motion_the_face_moves_at_once() {
    let mut comp = compositor();
    let wm = canvas(&mut comp);
    let mut face = face(Motion::Reduced, Some(reading()));
    let tick = face.due_ns();
    face.advance(tick, wm, &mut comp, &mut || Some(after(53)));
    assert!(face.moving.is_none());
    assert_eq!(face.strength, u8::MAX);
    assert_eq!(face.clock.label(), "13:47");
    assert_eq!(face.due_ns(), tick + 60 * SEC, "the next minute boundary");
}

/// A wall clock the face cannot read is asked again a minute later: a past
/// deadline would have the loop spin on it.
#[test]
fn a_clock_it_cannot_read_is_asked_again_a_minute_later() {
    let mut comp = compositor();
    let wm = canvas(&mut comp);
    let mut face = face(Motion::Reduced, None);
    assert_eq!(face.due_ns(), 60 * SEC);
    face.advance(60 * SEC, wm, &mut comp, &mut || None);
    assert_eq!(face.due_ns(), 120 * SEC);
}
