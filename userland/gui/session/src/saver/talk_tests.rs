//! Host tests of the host's half of a saver's conversation: its turns, its
//! pacing, the feed area's owner, and the deadlines.

use tairix_abi::bounded_text::BoundedText;
use tairix_abi::driver::display::{DamageRect, RefreshRate};
use tairix_abi::saver::SaverFeeds;
use tairix_abi::saver_ipc::{
    NextFrame, SaverDamage, SaverMessage, SaverRegion, Start, SAVER_FONT_CALLS_MAX,
    SAVER_FRAME_DEADLINE_NS, SAVER_KEEPS_MAX, SAVER_KEEP_SPACING_NS, SAVER_START_DEADLINE_NS,
};
use tairix_abi::{APPINFO_FLAG_FEED_KEEP, APPINFO_FLAG_FEED_SLIDES};

use super::{Fault, FeedHolder, Said, Shown, Talk};

const FRAME_NS: u64 = RefreshRate::ASSUMED.frame_ns();

fn start(feeds: u32, slides: u16) -> Start {
    let feeds = SaverFeeds::from_bits(feeds).expect("feeds");
    let region = SaverRegion::new(64, 36, feeds).expect("a region");
    let mut start = Start::new(region, 100, RefreshRate::ASSUMED);
    start.feeds = feeds;
    start.slides = slides;
    start
}

/// A conversation that has heard `Ready` at nought.
fn ready(feeds: u32, slides: u16) -> Talk {
    let mut talk = Talk::begin(&start(feeds, slides), 0);
    assert_eq!(talk.hear(&SaverMessage::Ready, 0), Ok(Said::Ready));
    talk
}

fn presented(serial: u32, next: NextFrame) -> SaverMessage {
    SaverMessage::Presented {
        serial,
        damage: SaverDamage::new(&[DamageRect {
            x: 0,
            y: 0,
            width_px: 4,
            height_px: 4,
        }])
        .ok(),
        next,
    }
}

#[test]
fn a_saver_is_given_the_start_deadline_to_say_it_is_ready() {
    let talk = Talk::begin(&start(0, 0), 100);
    assert_eq!(talk.due_ns(), Some(100 + SAVER_START_DEADLINE_NS));
    assert_eq!(talk.check(100 + SAVER_START_DEADLINE_NS - 1), Ok(()));
    assert_eq!(
        talk.check(100 + SAVER_START_DEADLINE_NS),
        Err(Fault::NeverReady)
    );
    assert!(!talk.ready());
}

/// Its first frame is asked for at once; each is owed a presentation by the
/// frame deadline, and the next is asked for when it asks.
#[test]
fn frames_are_asked_for_one_at_a_time_when_the_saver_wants_them() {
    let mut talk = ready(0, 0);
    assert_eq!(talk.frame_due(0), Some(1));
    assert_eq!(talk.frame_due(0), None, "one outstanding at a time");
    assert_eq!(talk.due_ns(), Some(SAVER_FRAME_DEADLINE_NS));
    assert_eq!(talk.check(SAVER_FRAME_DEADLINE_NS), Err(Fault::Late));
    let message = presented(1, NextFrame::At(5 * FRAME_NS));
    let said = talk.hear(&message, 10);
    assert!(matches!(said, Ok(Said::Presented { damage: Some(_) })));
    assert_eq!(talk.due_ns(), Some(5 * FRAME_NS));
    assert_eq!(talk.frame_due(5 * FRAME_NS - 1), None);
    assert_eq!(talk.frame_due(5 * FRAME_NS), Some(2));
}

/// A frame presented while the one laid before it awaits the screen is held,
/// nothing more is asked for meanwhile, and the screen taking the first hands
/// the held one over to be laid.
#[test]
fn a_frame_presented_ahead_of_the_screen_is_held_until_it_is_shown() {
    let mut talk = ready(0, 0);
    assert_eq!(talk.shown(), Shown::Nothing, "nothing laid yet");
    assert_eq!(talk.frame_due(0), Some(1));
    assert!(matches!(
        talk.hear(&presented(1, NextFrame::Soon), 1),
        Ok(Said::Presented { damage: Some(_) })
    ));
    talk.laid();
    assert_eq!(talk.frame_due(FRAME_NS), Some(2), "asked while one is laid");
    assert!(matches!(
        talk.hear(&presented(2, NextFrame::Soon), FRAME_NS + 1),
        Ok(Said::Held(damage)) if damage.len() == 1
    ));
    assert_eq!(
        talk.due_ns(),
        None,
        "the screen wakes the host, not a timer"
    );
    assert_eq!(talk.frame_due(10 * FRAME_NS), None);
    assert_eq!(talk.shown(), Shown::Next, "the held frame is handed over");
    talk.laid();
    assert_eq!(talk.frame_due(10 * FRAME_NS), Some(3));
    assert_eq!(talk.shown(), Shown::Laid);
    assert_eq!(talk.shown(), Shown::Nothing);
}

/// A frame that changed nothing is never held: there is nothing in the
/// region to keep from the saver.
#[test]
fn a_frame_that_changed_nothing_is_never_held() {
    let mut talk = ready(0, 0);
    assert_eq!(talk.frame_due(0), Some(1));
    let _ = talk.hear(&presented(1, NextFrame::Soon), 1);
    talk.laid();
    assert_eq!(talk.frame_due(FRAME_NS), Some(2));
    let unchanged = SaverMessage::Presented {
        serial: 2,
        damage: None,
        next: NextFrame::Soon,
    };
    assert_eq!(
        talk.hear(&unchanged, FRAME_NS + 1),
        Ok(Said::Presented { damage: None })
    );
    assert_eq!(talk.frame_due(2 * FRAME_NS), Some(3));
}

/// A saver asking for its next frame at once is drawn at the display's pace,
/// never faster.
#[test]
fn a_saver_asking_at_once_is_paced_to_the_display() {
    let mut talk = ready(0, 0);
    assert_eq!(talk.frame_due(0), Some(1));
    talk.hear(&presented(1, NextFrame::Soon), 1).expect("heard");
    assert_eq!(talk.due_ns(), Some(FRAME_NS));
    assert_eq!(talk.frame_due(2), None, "not before a display frame");
    assert_eq!(talk.frame_due(FRAME_NS), Some(2));
}

/// A saver waiting on its feeds is asked for nothing until one brings
/// something; a feed message to a saver drawing is owed the frame straight
/// after.
#[test]
fn a_feed_brings_a_frame() {
    let mut talk = ready(0, 0);
    talk.frame_due(0);
    talk.hear(&presented(1, NextFrame::Idle), 1).expect("heard");
    assert_eq!(talk.due_ns(), None, "nothing owed");
    assert_eq!(talk.frame_due(u64::MAX), None);
    talk.fed(100 * FRAME_NS);
    assert_eq!(talk.frame_due(100 * FRAME_NS), Some(2));
    talk.fed(100 * FRAME_NS + 1);
    talk.hear(&presented(2, NextFrame::Idle), 100 * FRAME_NS + 2)
        .expect("heard");
    assert_eq!(
        talk.due_ns(),
        Some(101 * FRAME_NS),
        "the frame after, paced"
    );
}

#[test]
fn a_message_out_of_turn_is_a_fault() {
    let mut starting = Talk::begin(&start(0, 0), 0);
    assert_eq!(
        starting.hear(&presented(1, NextFrame::Idle), 0),
        Err(Fault::OutOfTurn),
        "nothing was asked for"
    );
    let mut talk = ready(0, 0);
    assert_eq!(talk.hear(&SaverMessage::Ready, 0), Err(Fault::OutOfTurn));
    talk.frame_due(0);
    assert_eq!(
        talk.hear(&presented(7, NextFrame::Idle), 0),
        Err(Fault::OutOfTurn),
        "a frame not asked for"
    );
}

fn font_call() -> SaverMessage {
    SaverMessage::FontCall([0; tairix_abi::font_ipc::FontRequest::WIRE_LEN])
}

/// A font call is relayed one at a time, from the start on: a scene sets its
/// type out before it is ready.
#[test]
fn font_calls_are_relayed_one_at_a_time() {
    let mut talk = Talk::begin(&start(0, 0), 0);
    assert!(matches!(talk.hear(&font_call(), 0), Ok(Said::FontCall(_))));
    assert_eq!(
        talk.hear(&font_call(), 0),
        Err(Fault::OutOfTurn),
        "one at a time"
    );
    talk.font_answered(0);
    assert!(matches!(talk.hear(&font_call(), 0), Ok(Said::FontCall(_))));
}

/// A start, and each frame asked for, may make so many font calls and no more:
/// the relay is the font service's to share, not a saver's own workload.
#[test]
fn font_calls_are_bounded_from_one_frame_to_the_next() {
    let mut talk = Talk::begin(&start(0, 0), 0);
    let relay_all = |talk: &mut Talk| {
        for _ in 0..SAVER_FONT_CALLS_MAX {
            assert!(matches!(talk.hear(&font_call(), 0), Ok(Said::FontCall(_))));
            assert!(talk.font_answered(0));
        }
    };
    relay_all(&mut talk);
    assert_eq!(talk.hear(&font_call(), 0), Err(Fault::OutOfTurn));
    let mut talk = ready(0, 0);
    relay_all(&mut talk);
    assert_eq!(talk.frame_due(FRAME_NS), Some(1));
    assert!(
        matches!(talk.hear(&font_call(), FRAME_NS), Ok(Said::FontCall(_))),
        "a frame asked for gives the saver its calls again"
    );
}

/// An answer the saver no longer waits for is not the saver's to be told.
#[test]
fn a_font_answer_with_no_call_out_is_not_told() {
    let mut talk = ready(0, 0);
    assert!(!talk.font_answered(0));
    talk.hear(&font_call(), 0).expect("relayed");
    assert!(talk.font_answered(0));
    assert!(!talk.font_answered(0), "answered once");
}

/// A frame naming damage off the screen is refused: nothing it names is laid.
#[test]
fn a_frame_naming_damage_off_the_screen_is_refused() {
    let mut talk = ready(0, 0);
    assert_eq!(talk.frame_due(0), Some(1));
    let beyond = SaverMessage::Presented {
        serial: 1,
        damage: SaverDamage::new(&[DamageRect {
            x: 60,
            y: 0,
            width_px: 5,
            height_px: 1,
        }])
        .ok(),
        next: NextFrame::Idle,
    };
    assert_eq!(talk.hear(&beyond, 0), Err(Fault::OutOfTurn));
}

/// The time a font call spends being relayed is the host's, not the saver's:
/// no deadline runs while it is out, and the one it held up moves on by as
/// long.
#[test]
fn a_font_call_holds_the_deadline_while_it_is_relayed() {
    let mut starting = Talk::begin(&start(0, 0), 0);
    starting.hear(&font_call(), 10).expect("relayed");
    assert_eq!(starting.due_ns(), None, "nothing owed while it waits");
    assert_eq!(starting.check(SAVER_START_DEADLINE_NS * 3), Ok(()));
    starting.font_answered(110);
    assert_eq!(starting.due_ns(), Some(SAVER_START_DEADLINE_NS + 100));
    assert_eq!(
        starting.check(SAVER_START_DEADLINE_NS + 100),
        Err(Fault::NeverReady)
    );

    let mut drawing = ready(0, 0);
    drawing.hear(&font_call(), 0).expect("relayed");
    assert_eq!(drawing.frame_due(5), Some(1));
    assert_eq!(drawing.due_ns(), None);
    drawing.font_answered(25);
    assert_eq!(
        drawing.due_ns(),
        Some(5 + SAVER_FRAME_DEADLINE_NS + 20),
        "only the time since the frame was asked for"
    );
}

/// A slide is wanted only through the slides feed, one at a time and from
/// those offered; the feed area is the host's until it answers.
#[test]
fn a_slide_is_wanted_through_the_feed_alone() {
    let mut plain = ready(0, 0);
    assert_eq!(
        plain.hear(&SaverMessage::WantSlide(0), 0),
        Err(Fault::OutOfTurn)
    );
    let mut talk = ready(APPINFO_FLAG_FEED_SLIDES, 3);
    assert_eq!(
        talk.hear(&SaverMessage::WantSlide(3), 0),
        Err(Fault::OutOfTurn),
        "past the slides offered"
    );
    assert_eq!(
        talk.hear(&SaverMessage::WantSlide(2), 0),
        Ok(Said::WantSlide(2))
    );
    assert_eq!(talk.feed(), FeedHolder::Slide(2));
    assert_eq!(
        talk.hear(&SaverMessage::WantSlide(1), 0),
        Err(Fault::OutOfTurn),
        "the feed area is the host's"
    );
    talk.feed_answered(5);
    assert_eq!(talk.feed(), FeedHolder::Saver);
    assert_eq!(talk.due_ns(), Some(5), "the answer brings a frame");
}

/// A picture is kept only through the keeping feed, no larger than the
/// screen.
#[test]
fn a_picture_is_kept_through_the_feed_alone_and_within_the_screen() {
    let keep = |width: u32, height: u32| SaverMessage::Keep {
        title: BoundedText::new("Classic").expect("a title"),
        width,
        height,
    };
    assert_eq!(ready(0, 0).hear(&keep(4, 4), 0), Err(Fault::OutOfTurn));
    let mut talk = ready(APPINFO_FLAG_FEED_KEEP, 0);
    assert_eq!(talk.hear(&keep(65, 4), 0), Err(Fault::OutOfTurn));
    assert_eq!(talk.hear(&keep(4, 0), 0), Err(Fault::OutOfTurn));
    assert!(matches!(
        talk.hear(&keep(64, 36), 0),
        Ok(Said::Keep { size: (64, 36), .. })
    ));
    assert_eq!(talk.feed(), FeedHolder::Keep);
    assert_eq!(
        talk.hear(&keep(4, 4), 0),
        Err(Fault::OutOfTurn),
        "one at a time"
    );
}

/// A saver keeps a picture no sooner than the spacing after the last, and no
/// more of them a run than the bound: asking anyway is answered as not kept,
/// the feed area handed straight back.
#[test]
fn keeping_is_spaced_and_bounded_a_run() {
    let keep = || SaverMessage::Keep {
        title: BoundedText::new("Classic").expect("a title"),
        width: 4,
        height: 4,
    };
    let mut talk = ready(APPINFO_FLAG_FEED_KEEP, 0);
    assert!(matches!(talk.hear(&keep(), 0), Ok(Said::Keep { .. })));
    talk.feed_answered(0);
    assert_eq!(
        talk.hear(&keep(), SAVER_KEEP_SPACING_NS - 1),
        Ok(Said::KeepRefused)
    );
    assert_eq!(
        talk.feed(),
        FeedHolder::Keep,
        "until the refusal is answered"
    );
    talk.feed_answered(SAVER_KEEP_SPACING_NS - 1);
    let mut now_ns = SAVER_KEEP_SPACING_NS;
    for _ in 1..SAVER_KEEPS_MAX {
        assert!(matches!(talk.hear(&keep(), now_ns), Ok(Said::Keep { .. })));
        talk.feed_answered(now_ns);
        now_ns += SAVER_KEEP_SPACING_NS;
    }
    assert_eq!(
        talk.hear(&keep(), now_ns),
        Ok(Said::KeepRefused),
        "the run's last"
    );
}
