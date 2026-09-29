//! Host tests of the minimal clock: what it tells, where its text sits and
//! that it stays there, and the frames and minutes it wakes for.

use tairix_abi::time::{Time64, WallClockReading, WallTimeState};
use tairix_taskbar::clock::UNSET_LABEL;
use tairix_theme::{Accessibility, Motion, Theme};
use tairix_wallpaper::RibbonOptions;
use tairix_wm::{Compositor, Point, Scale, Surface, WindowId};

use super::{Ribbon, TIME_WEIGHT};
use crate::saver::telling::fixtures::{after, SCREEN, SEC};
use crate::saver::SAVER_FRAME_NS;
use crate::tests::compositor;

fn theme(motion: Motion) -> Theme {
    Theme::dark().with_axes(Accessibility {
        motion,
        ..Accessibility::default()
    })
}

fn face(motion: Motion, wall: Option<WallClockReading>, options: RibbonOptions) -> Ribbon {
    face_on(SCREEN, motion, wall, options)
}

fn face_on(
    screen: (u32, u32),
    motion: Motion,
    wall: Option<WallClockReading>,
    options: RibbonOptions,
) -> Ribbon {
    let calm = motion == Motion::Reduced;
    Ribbon::new(
        &theme(motion),
        (Scale::ONE, screen),
        (wall, 0),
        (calm, options),
    )
    .expect("the heap gives a ribbon")
}

fn canvas(comp: &mut Compositor, face: &mut Ribbon) -> WindowId {
    let mut surface = Surface::new(SCREEN.0, SCREEN.1).expect("a surface");
    face.paint(&mut surface);
    comp.add_window(Point::new(0, 0), surface)
}

#[test]
fn the_face_tells_the_bars_time_and_the_date_spelled_out() {
    let face = face(Motion::Full, Some(after(0)), RibbonOptions::default());
    assert_eq!(face.telling.time(), "13:46");
    assert_eq!(face.telling.date(), "Thu 29 Feb 2024");
    assert!(face.block.is_some());
}

#[test]
fn the_options_leave_the_date_out() {
    let dated = face(Motion::Full, Some(after(0)), RibbonOptions::default());
    let bare = face(Motion::Full, Some(after(0)), RibbonOptions { date: false });
    assert!(bare.telling.date().is_empty());
    let height = |face: &Ribbon| face.block.as_ref().map_or(0, Surface::height);
    assert!(height(&bare) < height(&dated));
    assert_eq!(bare.at.y, dated.at.y, "the time stays where it stands");
}

#[test]
fn an_unset_clock_shows_the_placeholder_and_no_date() {
    let unset = WallClockReading::new(Time64::UNIX_EPOCH, WallTimeState::Unset);
    let face = face(Motion::Full, Some(unset), RibbonOptions::default());
    assert_eq!(face.telling.time(), UNSET_LABEL);
    assert!(face.telling.date().is_empty());
}

/// The text is set where the storyboard sets it: the time a hairline weight
/// across the upper middle, its figures a little under a fifth of the
/// screen's height, and the date beneath it, both centred.
#[test]
fn the_text_sits_where_the_storyboard_sets_it() {
    let face = face(Motion::Full, Some(after(0)), RibbonOptions::default());
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
    for (screen, storyboard) in [((600, 1080), 313), ((1080, 1920), 556)] {
        let tall = face_on(
            screen,
            Motion::Full,
            Some(after(0)),
            RibbonOptions::default(),
        );
        let [time, date] = tall.fonts;
        let width = super::tabular_width(time, tall.figure, super::LONGEST_TIME);
        let room = super::share(screen.0, super::TIME_WIDEST);
        assert!(width <= room, "{screen:?}: {width} wide in {room}");
        assert!(
            width + width / 20 > room,
            "{screen:?}: {width} wide in {room}"
        );
        assert!(time.pixel_height() < storyboard);
        assert!(date.pixel_height() < time.pixel_height() / 3);
        assert!(tall.baselines[1] > tall.baselines[0]);
    }
}

/// Each minute the time changes and the text stays exactly where it was:
/// only the ribbon moves.
#[test]
fn the_text_holds_still_as_the_minute_turns() {
    let mut comp = compositor();
    let mut face = face(Motion::Full, Some(after(0)), RibbonOptions::default());
    let wm = canvas(&mut comp, &mut face);
    let (at, tick) = (face.at, face.telling.tick_ns());
    assert_eq!(tick, 53 * SEC, "the next minute boundary");
    let mut now = 0;
    while now < tick + 3 * SAVER_FRAME_NS {
        now += SAVER_FRAME_NS;
        let secs = i64::try_from(now / SEC).expect("small");
        face.advance(now, wm, &mut comp, &mut || Some(after(secs)));
    }
    assert_eq!(face.telling.time(), "13:47");
    assert_eq!(face.at, at);
}

/// Moving, the face wakes for every frame and for the minute; held still
/// under reduced motion, only for the minute.
#[test]
fn the_face_wakes_for_frames_while_it_moves_and_for_the_minute_alone_when_still() {
    let mut comp = compositor();
    let mut moving = face(Motion::Full, Some(after(0)), RibbonOptions::default());
    let wm = canvas(&mut comp, &mut moving);
    assert_eq!(
        moving.due_ns(),
        SAVER_FRAME_NS,
        "painted at once, then a frame on"
    );
    moving.advance(SAVER_FRAME_NS, wm, &mut comp, &mut || Some(after(0)));
    assert_eq!(moving.due_ns(), 2 * SAVER_FRAME_NS);
    moving.advance(SAVER_FRAME_NS + 1, wm, &mut comp, &mut || Some(after(0)));
    assert_eq!(
        moving.due_ns(),
        2 * SAVER_FRAME_NS,
        "an early wake draws nothing"
    );

    let mut still = face(Motion::Reduced, Some(after(0)), RibbonOptions::default());
    let wm = canvas(&mut comp, &mut still);
    assert_eq!(still.due_ns(), 53 * SEC, "nothing until the minute turns");
    still.advance(53 * SEC, wm, &mut comp, &mut || Some(after(53)));
    assert_eq!(still.telling.time(), "13:47");
    assert_eq!(still.due_ns(), 113 * SEC);
}

/// A wall clock the face cannot read is asked again a minute later: a past
/// deadline would have the loop spin on it.
#[test]
fn a_clock_it_cannot_read_is_asked_again_a_minute_later() {
    let mut comp = compositor();
    let mut face = face(Motion::Reduced, None, RibbonOptions::default());
    let wm = canvas(&mut comp, &mut face);
    assert_eq!(face.due_ns(), 60 * SEC);
    face.advance(60 * SEC, wm, &mut comp, &mut || None);
    assert_eq!(face.due_ns(), 120 * SEC);
}

/// The time's figures are drawn over the ribbon, and the ribbon beneath them
/// stays black where it does not reach.
#[test]
fn the_time_is_lettered_over_a_dark_sky() {
    let mut face = face(Motion::Full, Some(after(0)), RibbonOptions::default());
    let mut surface = Surface::new(SCREEN.0, SCREEN.1).expect("a surface");
    face.paint(&mut surface);
    let block = face.block.as_ref().expect("the lines");
    let (Ok(left), Ok(top)) = (u32::try_from(face.at.x), u32::try_from(face.at.y)) else {
        panic!("the text is on the screen");
    };
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
