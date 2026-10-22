//! Host tests of the minimal clock: where its text sits and that it stays
//! there, and the frames and readings it draws for.

extern crate std;

use tairix_abi::bounded_text::BoundedText;
use tairix_abi::saver_ipc::Start;
use tairix_raster::Surface;
use tairix_saver::testing::{self, Screen, FRAME_NS};
use tairix_saver::{NextFrame, SaverValue, Told, WallReading};

use super::{Ribbon, TIME_WEIGHT};

const SCREEN: (u32, u32) = (1920, 1080);

const SEC: u64 = 1_000_000_000;

/// A reading of `time` on `date`, spelled out, next changing at `next_ns`.
fn reading(time: &str, date: &str, next_ns: u64) -> WallReading {
    WallReading {
        time: BoundedText::new(time).expect("a time"),
        numeric_date: BoundedText::EMPTY,
        long_date: BoundedText::new(date).expect("a date"),
        next_change_ns: next_ns,
    }
}

fn first() -> WallReading {
    reading("13:46", "Thu 29 Feb 2024", 53 * SEC)
}

fn start_on(screen: (u32, u32), date: bool, calm: bool) -> Start {
    tairix_font::install_test_transport();
    let mut given = testing::start(screen.0, screen.1);
    given.reduced_motion = calm;
    testing::give(&mut given, "date", SaverValue::Switch(date));
    given
}

/// A face on `screen` and its screen, its first frame drawn at nought.
fn shown(screen: (u32, u32), date: bool, calm: bool) -> (Ribbon, Screen, u64) {
    let given = start_on(screen, date, calm);
    let mut face = Ribbon::new(&testing::setup(&given, 0)).expect("a ribbon");
    let mut screen = Screen::new(given);
    let due = draw(&mut screen, &mut face, 0, first());
    (face, screen, due)
}

/// Draw `face`'s frame at `now_ns` reading `wall`, answering when it wants
/// the next.
fn draw(screen: &mut Screen, face: &mut Ribbon, now_ns: u64, wall: WallReading) -> u64 {
    screen.wall = wall;
    let (next, asked) = screen.draw(face, 0, now_ns);
    assert!(asked.is_empty(), "the face asks nothing of its host");
    let NextFrame::At(due) = next else {
        panic!("a face always wants another frame: {next:?}");
    };
    due
}

fn height(face: &Ribbon) -> u32 {
    face.block.as_ref().map_or(0, Surface::height)
}

#[test]
fn the_options_leave_the_date_out() {
    let (dated, ..) = shown(SCREEN, true, false);
    let (bare, ..) = shown(SCREEN, false, false);
    assert_eq!(bare.shown.as_ref().map(Told::date), Some(""));
    assert!(height(&bare) < height(&dated));
    assert_eq!(bare.at.y, dated.at.y, "the time stays where it stands");
}

/// The text is set where the storyboard sets it: the time a hairline weight
/// across the upper middle, its figures a little under a fifth of the
/// screen's height, and the date beneath it, both centred.
#[test]
fn the_text_sits_where_the_storyboard_sets_it() {
    let (face, ..) = shown(SCREEN, true, false);
    let [time, date] = face.fonts;
    assert_eq!(time.weight(), TIME_WEIGHT);
    assert_eq!(time.pixel_height(), 313, "29% of the screen's height");
    assert!(date.pixel_height() < time.pixel_height() / 3);
    assert_eq!(face.baselines, [410, 544]);
    let block = face.block.as_ref().expect("the lines");
    let middle = face.at.x + i32::try_from(block.width() / 2).expect("small");
    assert!((middle - 960).abs() <= 1, "centred, at {middle}");
}

/// On a screen too narrow for the storyboard's time, the type is narrowed
/// until the time fits its room, and the date follows it in proportion — a
/// tall one too, whose storyboard size the type's largest cannot reach.
#[test]
fn a_narrow_screen_narrows_the_type_to_fit() {
    for (size, storyboard) in [((600, 1080), 313), ((1080, 1920), 556)] {
        let (tall, ..) = shown(size, true, false);
        let [time, date] = tall.fonts;
        let width = super::tabular_width(time, tall.figure, super::LONGEST_TIME);
        let room = super::share(size.0, super::TIME_WIDEST);
        assert!(width <= room, "{size:?}: {width} wide in {room}");
        assert!(
            width + width / 20 > room,
            "{size:?}: {width} wide in {room}"
        );
        assert!(time.pixel_height() < storyboard);
        assert!(date.pixel_height() < time.pixel_height() / 3);
        assert!(tall.baselines[1] > tall.baselines[0]);
    }
}

/// The first frame paints the whole screen: the time lettered over a sky
/// that is black where the ribbon does not reach.
#[test]
fn the_first_frame_letters_the_time_over_a_dark_sky() {
    let (face, screen, _) = shown(SCREEN, true, false);
    assert_eq!(
        screen.damage.bounds(),
        tairix_geometry::Rect::new(0, 0, SCREEN.0, SCREEN.1)
    );
    let block = face.block.as_ref().expect("the lines");
    let (Ok(left), Ok(top)) = (u32::try_from(face.at.x), u32::try_from(face.at.y)) else {
        panic!("the text is on the screen");
    };
    let surface = &screen.surface;
    let lettered = (top..top + block.height())
        .flat_map(|y| (left..left + block.width()).map(move |x| (x, y)))
        .filter(|(x, y)| surface.get(*x, *y).is_some_and(|pixel| pixel.r > 200))
        .count();
    assert!(lettered > 0, "the figures show");
    assert_eq!(
        surface.get(0, 0).map(|pixel| (pixel.r, pixel.g, pixel.b)),
        Some((0, 0, 0))
    );
}

/// When the reading changes the time is told anew and the text stays exactly
/// where it was: only the ribbon moves.
#[test]
fn the_text_holds_still_as_the_reading_changes() {
    let (mut face, mut screen, _) = shown(SCREEN, true, false);
    let at = face.at;
    let mut now = 0;
    while now < 53 * SEC + 3 * FRAME_NS {
        now += FRAME_NS;
        let wall = if now < 53 * SEC {
            first()
        } else {
            reading("13:47", "Thu 29 Feb 2024", 113 * SEC)
        };
        draw(&mut screen, &mut face, now, wall);
    }
    assert_eq!(face.shown.as_ref().map(Told::time), Some("13:47"));
    assert_eq!(face.at, at);
}

/// Moving, the face wants every frame and the reading's change; held still
/// under reduced motion, only the change.
#[test]
fn the_face_wants_frames_while_it_moves_and_the_change_alone_when_still() {
    let (mut moving, mut screen, due) = shown(SCREEN, true, false);
    assert_eq!(due, FRAME_NS, "painted at once, then a frame on");
    assert_eq!(
        draw(&mut screen, &mut moving, FRAME_NS, first()),
        2 * FRAME_NS
    );
    let early = draw(&mut screen, &mut moving, FRAME_NS + 1, first());
    assert_eq!(early, 2 * FRAME_NS);
    assert!(screen.damage.is_empty(), "an early frame draws nothing");

    let (mut still, mut screen, due) = shown(SCREEN, true, true);
    assert_eq!(due, 53 * SEC, "nothing until the reading changes");
    let due = draw(
        &mut screen,
        &mut still,
        53 * SEC,
        reading("13:47", "Thu 29 Feb 2024", 113 * SEC),
    );
    assert_eq!(still.shown.as_ref().map(Told::time), Some("13:47"));
    assert_eq!(due, 113 * SEC);
}

/// A frame that came late draws once and asks for the next a frame on,
/// rather than a burst to catch up. How far that frame carries the ribbon is
/// the ribbon's own clock's to bound.
#[test]
fn a_late_frame_draws_once_and_asks_for_the_next_a_frame_on() {
    let (mut moving, mut screen, _) = shown(SCREEN, true, false);
    draw(&mut screen, &mut moving, FRAME_NS, first());
    let due = draw(
        &mut screen,
        &mut moving,
        50 * SEC,
        reading("13:46", "Thu 29 Feb 2024", 53 * SEC),
    );
    assert_eq!(due, 50 * SEC + FRAME_NS);
}
