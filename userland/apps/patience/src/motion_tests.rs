//! Motion tests: a change arrives as movement from wherever a card is drawn,
//! every movement ends, and nothing owes a frame once it has.

use alloc::vec;
use alloc::vec::Vec;

use super::*;

/// One frame of the desktop the motion is paced on.
const FRAME_NS: u64 = RefreshRate::ASSUMED.frame_ns();

const SIZE: (u32, u32) = (80, 112);
const MS: u64 = 1_000_000;

fn card(index: usize) -> CardId {
    CardId::new(index).expect("a card")
}

fn at(x: i32, y: i32, up: bool) -> Placement {
    Placement {
        at: Point::new(x, y),
        up,
    }
}

fn resting(placements: &[Placement], reduced: bool) -> Motion {
    let mut motion = Motion::new(placements.len(), SIZE, reduced, RefreshRate::ASSUMED);
    motion.snap(placements, SIZE);
    motion
}

/// Advance a frame at a time from `from_ns` until nothing moves, answering
/// when that was.
fn run_still(motion: &mut Motion, from_ns: u64) -> u64 {
    let mut now = from_ns;
    while let Some(next) = motion.deadline_ns(now) {
        now = next;
        motion.advance(now, &mut Region::new());
        assert!(now - from_ns < 60_000 * MS, "motion ends");
    }
    now
}

#[test]
fn nothing_moving_owes_no_frame() {
    let motion = resting(&[at(0, 0, true), at(100, 0, true)], false);
    assert_eq!(motion.deadline_ns(5), None);
}

#[test]
fn a_card_in_the_air_is_drawn_on_every_frame_of_its_own_desktop() {
    let mut motion = resting(&[at(0, 0, true)], false);
    motion.retarget(
        0,
        &[at(300, 200, true)],
        &[],
        |_| Timing::SLIDE,
        &mut Region::new(),
    );
    let fast = RefreshRate::from_mhz(144_000).expect("in bounds");
    motion.set_refresh(fast);
    assert_eq!(motion.deadline_ns(0), Some(fast.frame_ns()));
}

#[test]
fn a_moved_card_flies_from_where_it_was_to_where_it_goes() {
    let mut motion = resting(&[at(0, 0, true), at(100, 0, true)], false);
    let mut damage = Region::new();
    motion.retarget(
        0,
        &[at(300, 200, true), at(100, 0, true)],
        &[],
        |_| Timing::SLIDE,
        &mut damage,
    );
    assert!(damage.intersects(card_rect(Point::new(0, 0), SIZE)));
    assert!(damage.intersects(card_rect(Point::new(300, 200), SIZE)));
    assert_eq!(motion.shown(card(0), 0).at, Point::new(0, 0));
    assert!(motion.flying(card(0)) && !motion.flying(card(1)));
    assert_eq!(motion.airborne(), &[card(0)]);
    assert_eq!(motion.deadline_ns(0), Some(FRAME_NS));
    let midway = motion.shown(card(0), Timing::SLIDE.span_ns / 2).at;
    assert!(midway.x > 0 && midway.x < 300 && midway.y > 0 && midway.y < 200);
    let landed = run_still(&mut motion, 0);
    assert!(landed >= Timing::SLIDE.span_ns);
    assert_eq!(
        motion.shown(card(0), landed),
        Shown {
            at: Point::new(300, 200),
            facing: Facing::Up,
            flying: false,
        }
    );
    assert!(motion.airborne().is_empty());
}

#[test]
fn each_frame_repaints_where_the_card_was_and_where_it_is() {
    let mut motion = resting(&[at(0, 0, true)], false);
    motion.retarget(
        0,
        &[at(400, 0, true)],
        &[],
        |_| Timing::SLIDE,
        &mut Region::new(),
    );
    let mut first = Region::new();
    motion.advance(FRAME_NS, &mut first);
    let mut second = Region::new();
    motion.advance(2 * FRAME_NS, &mut second);
    let was = motion.shown(card(0), FRAME_NS).at;
    let now = motion.shown(card(0), 2 * FRAME_NS).at;
    assert!(second.intersects(card_rect(was, SIZE)) && second.intersects(card_rect(now, SIZE)));
}

#[test]
fn reduced_motion_moves_a_card_at_once() {
    let mut motion = resting(&[at(0, 0, false)], true);
    let mut damage = Region::new();
    motion.retarget(0, &[at(300, 0, true)], &[], |_| Timing::SLIDE, &mut damage);
    assert!(!damage.is_empty());
    assert!(!motion.flying(card(0)));
    assert_eq!(motion.shown(card(0), 0).at, Point::new(300, 0));
    assert_eq!(motion.shown(card(0), 0).facing, Facing::Up);
    assert_eq!(motion.deadline_ns(0), None);
}

#[test]
fn a_card_that_changes_face_turns_over_on_the_way() {
    let mut motion = resting(&[at(0, 0, false)], false);
    motion.retarget(
        0,
        &[at(200, 0, true)],
        &[],
        |_| Timing::SLIDE,
        &mut Region::new(),
    );
    let span = Timing::SLIDE.span_ns;
    assert_eq!(
        motion.shown(card(0), 0).facing,
        Facing::Turning {
            face: false,
            width: 1000
        }
    );
    let Facing::Turning { width, .. } = motion.shown(card(0), span / 2).facing else {
        panic!("turning midway");
    };
    assert!(width < 100, "edge on midway, {width}");
    let Facing::Turning { face, .. } = motion.shown(card(0), span * 3 / 4).facing else {
        panic!("turning");
    };
    assert!(face, "the face shows over the second half");
    let landed = run_still(&mut motion, 0);
    assert_eq!(motion.shown(card(0), landed).facing, Facing::Up);
}

#[test]
fn a_delayed_flight_waits_where_it_was_even_with_no_span() {
    let mut motion = resting(&[at(0, 0, true)], false);
    let timing = Timing {
        delay_ns: 100 * MS,
        span_ns: 0,
        arc: 0,
    };
    motion.retarget(0, &[at(50, 50, true)], &[], |_| timing, &mut Region::new());
    assert_eq!(motion.shown(card(0), 50 * MS).at, Point::new(0, 0));
    assert_eq!(motion.shown(card(0), 100 * MS).at, Point::new(50, 50));
    run_still(&mut motion, 0);
    assert!(!motion.flying(card(0)));
}

#[test]
fn a_held_card_sets_off_from_under_the_pointer() {
    let mut motion = resting(&[at(0, 0, true)], false);
    let held = [(card(0), Point::new(500, 300))];
    motion.retarget(
        0,
        &[at(0, 0, true)],
        &held,
        |_| Timing::SLIDE,
        &mut Region::new(),
    );
    assert!(
        motion.flying(card(0)),
        "a card let go returns though its place is unchanged"
    );
    assert_eq!(motion.shown(card(0), 0).at, Point::new(500, 300));
    let landed = run_still(&mut motion, 0);
    assert_eq!(motion.shown(card(0), landed).at, Point::new(0, 0));
}

#[test]
fn a_new_change_starts_from_the_middle_of_the_last() {
    let mut motion = resting(&[at(0, 0, true)], false);
    motion.retarget(
        0,
        &[at(400, 0, true)],
        &[],
        |_| Timing::SLIDE,
        &mut Region::new(),
    );
    let half = Timing::SLIDE.span_ns / 2;
    let midway = motion.shown(card(0), half).at;
    motion.retarget(
        half,
        &[at(0, 0, true)],
        &[],
        |_| Timing::SLIDE,
        &mut Region::new(),
    );
    assert_eq!(motion.shown(card(0), half).at, midway);
    assert_eq!(motion.airborne(), &[card(0)], "one flight, not two");
}

#[test]
fn a_refused_card_hops_and_settles_where_it_rests() {
    let mut motion = resting(&[at(100, 100, true), at(200, 100, true)], false);
    let mut damage = Region::new();
    motion.hop(&[card(1)], 12, 0, &mut damage);
    assert!(!damage.is_empty());
    let top = motion.shown(card(1), HOP_NS / 2).at;
    assert_eq!(top.x, 200);
    assert!(
        (88..100).contains(&top.y),
        "near the top of its hop, {top:?}"
    );
    let landed = run_still(&mut motion, 0);
    assert_eq!(motion.shown(card(1), landed).at, Point::new(200, 100));
    assert!(!motion.flying(card(0)));

    let mut still = resting(&[at(100, 100, true)], true);
    still.hop(&[card(0)], 12, 0, &mut Region::new());
    assert_eq!(still.deadline_ns(0), None);
}

#[test]
fn a_card_taken_in_hand_leaves_the_air() {
    let mut motion = resting(&[at(0, 0, true), at(10, 0, true)], false);
    let moved = [at(300, 0, true), at(310, 0, true)];
    motion.retarget(0, &moved, &[], |_| Timing::SLIDE, &mut Region::new());
    motion.ground(&[card(0)]);
    assert!(!motion.flying(card(0)) && motion.flying(card(1)));
    assert_eq!(motion.airborne(), &[card(1)]);
    motion.ground(&[card(1)]);
    assert_eq!(motion.deadline_ns(0), None);
}

#[test]
fn a_glide_arrives_and_stops() {
    let from = Point::new(10, 10);
    let to = Point::new(110, 60);
    let glide = Glide::new(from, to, 1_000, false);
    assert_eq!(glide.at(1_000), from);
    assert!(glide.moving(1_000));
    let end = 1_000 + Timing::SLIDE.span_ns;
    assert_eq!(glide.at(end), to);
    assert!(!glide.moving(end));
    assert_eq!(glide.to(), to);

    let at_once = Glide::new(from, to, 1_000, true);
    assert_eq!(at_once.at(1_000), to);
    assert!(!at_once.moving(1_000));
}

#[test]
fn the_victory_waits_for_every_card_to_land() {
    let mut motion = resting(&[at(0, 0, true), at(100, 0, true)], false);
    let slow = Timing {
        delay_ns: 0,
        span_ns: 900 * MS,
        arc: 0,
    };
    motion.retarget(
        0,
        &[at(0, 0, true), at(300, 0, true)],
        &[],
        |_| slow,
        &mut Region::new(),
    );
    let home = vec![(card(0), Point::new(0, 0)), (card(1), Point::new(300, 0))];
    motion.celebrate(Cascade::new(home, (0, 1000, 700), SIZE, 0, 7));
    let mut now = 0;
    while motion.flying(card(1)) {
        now += FRAME_NS;
        motion.advance(now, &mut Region::new());
        let begun = motion.cascade().is_some_and(Cascade::begun);
        assert!(
            !begun || !motion.flying(card(1)),
            "nothing thrown while a card is in the air"
        );
    }
    assert!(
        !motion.cascade().is_some_and(Cascade::begun),
        "the landing frame is drawn whole"
    );
    now += FRAME_NS;
    motion.advance(now, &mut Region::new());
    assert!(motion.cascade().is_some_and(Cascade::begun));
}

#[test]
fn the_victory_ends_once_every_card_has_left_the_table() {
    let mut motion = resting(&[at(0, 0, true), at(100, 0, true), at(200, 0, true)], false);
    let home = (0..3_i32)
        .map(|index| {
            (
                card(usize::try_from(index).expect("small")),
                Point::new(index * 100, 0),
            )
        })
        .collect();
    motion.celebrate(Cascade::new(home, (0, 1000, 700), SIZE, 0, 3));
    let mut now = 0;
    while !motion.cascade().is_some_and(Cascade::done) {
        now += FRAME_NS;
        let mut damage = Region::new();
        motion.advance(now, &mut damage);
        let bouncing: Vec<(CardId, Point)> = motion
            .cascade()
            .map_or(Vec::new(), |c| c.bouncing().collect());
        for (_, at) in bouncing {
            assert!(at.y <= 700 - 112, "a card bounces off the table's foot");
            assert!(damage.intersects(Rect::new(at.x, at.y, SIZE.0, SIZE.1)));
        }
        assert!(now < 120_000 * MS, "every card leaves");
    }
    assert!(motion.end_cascade());
    assert_eq!(motion.deadline_ns(now), None);
}

#[test]
fn reduced_motion_celebrates_nothing() {
    let mut motion = resting(&[at(0, 0, true)], true);
    motion.celebrate(Cascade::new(
        vec![(card(0), Point::ORIGIN)],
        (0, 1000, 700),
        SIZE,
        0,
        1,
    ));
    assert!(motion.cascade().is_none());
    assert!(!motion.end_cascade());
}

#[test]
fn a_hint_pulses_and_ends_dark() {
    let mut motion = resting(&[at(0, 0, true)], false);
    let rect = Rect::new(0, 0, 80, 112);
    let mut damage = Region::new();
    motion.glow(vec![rect], 0, &mut damage);
    assert!(damage.intersects(rect));
    assert!(motion.lit(GLOW_NS / 4).is_some());
    let ended = run_still(&mut motion, 0);
    assert!(ended >= GLOW_NS);
    assert!(motion.lit(ended).is_none());

    let mut still = resting(&[at(0, 0, true)], true);
    still.glow(vec![rect], 0, &mut Region::new());
    assert_eq!(
        still.deadline_ns(0),
        Some(GLOW_NS),
        "a still hint wakes only to end"
    );
    assert_eq!(still.lit(10).map(|(_, strength)| strength), Some(200));
}

#[test]
fn turning_reduced_motion_on_lands_everything() {
    let mut motion = resting(&[at(0, 0, true)], false);
    motion.retarget(
        0,
        &[at(300, 0, true)],
        &[],
        |_| Timing::SLIDE,
        &mut Region::new(),
    );
    motion.set_reduced_motion(true);
    assert!(motion.reduced_motion());
    assert!(!motion.flying(card(0)));
    assert_eq!(motion.shown(card(0), 0).at, Point::new(300, 0));
    assert_eq!(motion.deadline_ns(0), None);
}
