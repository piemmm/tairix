//! Host tests of the clock screensaver: what it shows, where it goes, and
//! that it draws only when the reading changes.

extern crate std;

use tairix_abi::bounded_text::BoundedText;
use tairix_abi::saver_ipc::Start;
use tairix_geometry::Rect;
use tairix_raster::Surface;
use tairix_saver::testing::{self, Screen, FRAME_NS};
use tairix_saver::{footprint, NextFrame, SaverValue, Told, WallReading};

use super::{identity_line, ClockFace};

const SCREEN: (u32, u32) = (1920, 1080);

const SEC: u64 = 1_000_000_000;

/// A reading of `time` on `date`, next changing at `next_ns`.
fn reading(time: &str, date: &str, next_ns: u64) -> WallReading {
    WallReading {
        time: BoundedText::new(time).expect("a time"),
        numeric_date: BoundedText::new(date).expect("a date"),
        long_date: BoundedText::EMPTY,
        next_change_ns: next_ns,
    }
}

fn start(date: bool, identity: bool, calm: bool) -> Start {
    tairix_font::install_test_transport();
    let mut given = testing::start(SCREEN.0, SCREEN.1);
    given.reduced_motion = calm;
    given.account = BoundedText::new("ann").expect("a name");
    given.machine = BoundedText::new("tairix-box").expect("a name");
    testing::give(&mut given, "date", SaverValue::Switch(date));
    testing::give(&mut given, "identity", SaverValue::Switch(identity));
    given
}

fn face(given: &Start) -> ClockFace {
    ClockFace::new(&testing::setup(given, 7)).expect("a face")
}

/// Draw `face`'s frame at `now_ns` reading `wall`, answering when it wants
/// the next.
fn draw(screen: &mut Screen, face: &mut ClockFace, now_ns: u64, wall: WallReading) -> u64 {
    screen.wall = wall;
    let (next, asked) = screen.draw(face, 0, now_ns);
    assert!(asked.is_empty(), "the face asks nothing of its host");
    let NextFrame::At(due) = next else {
        panic!("a face always wants another frame: {next:?}");
    };
    due
}

fn inside_the_screen(face: &ClockFace) -> bool {
    let screen = Rect::new(0, 0, SCREEN.0, SCREEN.1);
    let block = footprint(face.block.as_ref(), face.at);
    !block.is_empty() && block.intersection(&screen) == block
}

fn height(face: &ClockFace) -> u32 {
    face.block.as_ref().map_or(0, Surface::height)
}

#[test]
fn the_identity_line_names_who_and_where() {
    assert_eq!(identity_line("ann", "tairix-box"), "ann \u{b7} tairix-box");
    assert_eq!(identity_line("ann", ""), "ann");
    assert_eq!(identity_line("", "tairix-box"), "tairix-box");
    assert_eq!(identity_line("", ""), "");
}

/// The first frame shows the reading at once, on the screen, and asks for
/// the next when the reading next changes.
#[test]
fn the_first_frame_shows_the_reading_and_waits_for_its_change() {
    let given = start(true, true, false);
    let mut clock = face(&given);
    let mut screen = Screen::new(given);
    let due = draw(
        &mut screen,
        &mut clock,
        SEC,
        reading("13:46", "2024-02-29", 53 * SEC),
    );
    assert_eq!(due, 53 * SEC, "the next change");
    let shown = clock.shown.expect("a reading is shown");
    assert_eq!((shown.time(), shown.date()), ("13:46", "2024-02-29"));
    assert_eq!(clock.strength, u8::MAX, "shown whole at once");
    assert!(inside_the_screen(&clock));
    let block = footprint(clock.block.as_ref(), clock.at);
    assert_eq!(screen.damage.bounds(), block, "only the block is drawn");
}

/// A face that tells neither the date nor who is signed in shows the time
/// alone, and a smaller block for it.
#[test]
fn the_options_leave_out_the_date_and_who_is_signed_in() {
    let wall = || reading("13:46", "2024-02-29", 53 * SEC);
    let whole_start = start(true, true, false);
    let mut whole = face(&whole_start);
    draw(&mut Screen::new(whole_start), &mut whole, 0, wall());
    let bare_start = start(false, false, false);
    let mut bare = face(&bare_start);
    draw(&mut Screen::new(bare_start), &mut bare, 0, wall());
    assert!(bare.identity.is_empty());
    assert_eq!(bare.shown.as_ref().map(Told::date), Some(""));
    assert!(height(&bare) < height(&whole));
}

/// A frame whose reading is the one shown draws nothing and waits again for
/// the reading's change.
#[test]
fn an_unchanged_reading_draws_nothing() {
    let given = start(true, true, false);
    let mut clock = face(&given);
    let mut screen = Screen::new(given);
    draw(
        &mut screen,
        &mut clock,
        0,
        reading("13:46", "2024-02-29", 53 * SEC),
    );
    let due = draw(
        &mut screen,
        &mut clock,
        20 * SEC,
        reading("13:46", "2024-02-29", 53 * SEC),
    );
    assert!(screen.damage.is_empty(), "nothing changed, nothing drawn");
    assert_eq!(due, 53 * SEC);
}

/// A change the host says is already due is drawn again a scene frame on,
/// never in a tight loop.
#[test]
fn a_change_already_due_waits_a_scene_frame() {
    let given = start(true, true, true);
    let mut clock = face(&given);
    let mut screen = Screen::new(given);
    let due = draw(
        &mut screen,
        &mut clock,
        10 * SEC,
        reading("13:46", "", 9 * SEC),
    );
    assert_eq!(due, 10 * SEC + FRAME_NS);
}

/// When the reading changes the block fades out, moves, and fades back in,
/// and ends somewhere else on the screen at full strength.
#[test]
fn the_face_moves_when_the_reading_changes() {
    let given = start(true, true, false);
    let mut clock = face(&given);
    let mut screen = Screen::new(given);
    assert!(clock.fade_ns > 0, "the theme animates a move");
    let tick = draw(
        &mut screen,
        &mut clock,
        0,
        reading("13:46", "2024-02-29", 53 * SEC),
    );
    let was = clock.at;
    let later = || reading("13:47", "2024-02-29", 113 * SEC);
    let due = draw(&mut screen, &mut clock, tick, later());
    assert!(clock.moving.is_some());
    assert_eq!(due, tick + FRAME_NS, "frames while it moves");
    let mut now = due;
    while clock.moving.is_some() {
        assert!(now < tick + 3 * clock.fade_ns, "a move ends");
        now = draw(&mut screen, &mut clock, now, later());
    }
    assert_eq!(clock.strength, u8::MAX);
    assert!(inside_the_screen(&clock));
    assert_ne!(clock.at, was, "moved on");
    assert_eq!(now, 113 * SEC, "and waits for the next change");
}

#[test]
fn under_reduced_motion_the_face_moves_at_once() {
    let given = start(true, true, true);
    let mut clock = face(&given);
    let mut screen = Screen::new(given);
    let tick = draw(
        &mut screen,
        &mut clock,
        0,
        reading("13:46", "2024-02-29", 53 * SEC),
    );
    let was = footprint(clock.block.as_ref(), clock.at);
    let due = draw(
        &mut screen,
        &mut clock,
        tick,
        reading("13:47", "2024-02-29", 113 * SEC),
    );
    assert!(clock.moving.is_none());
    assert_eq!(clock.strength, u8::MAX);
    assert_eq!(due, 113 * SEC, "the next change");
    let now = footprint(clock.block.as_ref(), clock.at);
    assert_eq!(
        screen.damage.bounds(),
        was.union(&now),
        "where it was and is"
    );
}
