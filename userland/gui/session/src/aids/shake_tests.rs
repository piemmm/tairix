use tairix_abi::time::NANOS_PER_MILLI as MS;
use tairix_geometry::{Point, Scale};
use tairix_theme::{MotionInteraction, MotionTheme, Theme};
use tairix_wm::FULLY_ENLARGED;

use super::{Shake, HOLD_NS, SHAKE_STROKES, STROKE_MAX_NS, STROKE_MIN_PX};

fn motion() -> MotionTheme {
    Theme::dark().motion()
}

/// Feed `detector` `strokes` strokes across, each `width` pixels wide and
/// lasting `stroke_ns`, `down` pixels steep, in four samples each; answer
/// the instant the last sample was taken at.
fn stroke(
    detector: &mut Shake,
    start_ns: u64,
    strokes: u32,
    width: i32,
    down: i32,
    stroke_ns: u64,
) -> u64 {
    let mut at = Point::new(500, 500);
    let mut now = start_ns;
    detector.observe(now, at, Scale::ONE);
    for index in 0..strokes {
        let sign = if index % 2 == 0 { 1 } else { -1 };
        for _ in 0..4 {
            now += stroke_ns / 4;
            at = Point::new(at.x + sign * width / 4, at.y + down / 4);
            detector.observe(now, at, Scale::ONE);
        }
    }
    now
}

/// Run the detector's size forward from `from_ns` for `span_ns`, a frame at a
/// time, as the loop would, and answer the size it ends at.
fn run_for(detector: &mut Shake, from_ns: u64, span_ns: u64) -> u16 {
    let mut level = detector.level(from_ns, motion());
    let mut now = from_ns;
    while now < from_ns + span_ns {
        now += 16 * MS;
        level = detector.level(now, motion());
    }
    level
}

fn wide() -> i32 {
    i32::try_from(STROKE_MIN_PX).expect("small") + 20
}

#[test]
fn a_quick_shake_grows_the_pointer_to_its_full_size() {
    let mut detector = Shake::new();
    let end = stroke(&mut detector, 0, SHAKE_STROKES.into(), wide(), 0, 80 * MS);
    // The last stroke only counts once the pointer turns back.
    detector.observe(end + 10 * MS, Point::new(400, 500), Scale::ONE);
    let grown = run_for(&mut detector, end + 10 * MS, 400 * MS);
    assert_eq!(grown, FULLY_ENLARGED);
}

#[test]
fn a_shake_short_of_its_strokes_does_nothing() {
    let mut detector = Shake::new();
    let end = stroke(
        &mut detector,
        0,
        u32::from(SHAKE_STROKES) - 1,
        wide(),
        0,
        80 * MS,
    );
    detector.observe(end + 10 * MS, Point::new(420, 500), Scale::ONE);
    assert_eq!(run_for(&mut detector, end, 400 * MS), 0);
    assert_eq!(detector.next_frame_in(end + 400 * MS), None);
}

#[test]
fn slow_short_or_steep_strokes_are_not_a_shake() {
    let slow = STROKE_MAX_NS + 60 * MS;
    let short = i32::try_from(STROKE_MIN_PX).expect("small") - 8;
    for (width, down, stroke_ns) in [
        (wide(), 0, slow),
        (short, 0, 80 * MS),
        (wide(), 3 * wide(), 80 * MS),
    ] {
        let mut detector = Shake::new();
        let end = stroke(&mut detector, 0, 8, width, down, stroke_ns);
        assert_eq!(
            run_for(&mut detector, end, 400 * MS),
            0,
            "{width} across, {down} down, over {stroke_ns} ns"
        );
    }
}

#[test]
fn one_fast_sweep_is_not_a_shake() {
    let mut detector = Shake::new();
    let mut now = 0;
    for step in 0..40 {
        now += 4 * MS;
        detector.observe(now, Point::new(step * 40, 300), Scale::ONE);
    }
    assert_eq!(run_for(&mut detector, now, 400 * MS), 0);
}

#[test]
fn a_shaken_pointer_holds_then_settles_home_and_asks_for_nothing_more() {
    let mut detector = Shake::new();
    let end = stroke(&mut detector, 0, 6, wide(), 0, 80 * MS);
    let last_turn = end - 80 * MS;
    let grow = u64::from(motion().duration(MotionInteraction::PointerEnlarge)) * MS;
    assert_eq!(run_for(&mut detector, end, grow + 16 * MS), FULLY_ENLARGED);

    // Grown, it asks to be woken exactly when the hold runs out.
    let held_at = end + grow + 32 * MS;
    let expected = (last_turn + HOLD_NS).saturating_sub(held_at);
    assert_eq!(detector.next_frame_in(held_at), Some(expected));

    let restore = u64::from(motion().duration(MotionInteraction::PointerRestore)) * MS;
    let home = run_for(&mut detector, held_at, expected + restore + 32 * MS);
    assert_eq!(home, 0);
    assert_eq!(
        detector.next_frame_in(held_at + expected + restore + 64 * MS),
        None
    );
}

#[test]
fn it_shrinks_through_every_size_between_rather_than_jumping() {
    let mut detector = Shake::new();
    let end = stroke(&mut detector, 0, 6, wide(), 0, 80 * MS);
    let _ = run_for(&mut detector, end, 200 * MS);
    let mut now = end + HOLD_NS;
    let mut previous = detector.level(now, motion());
    let mut steps = 0;
    while previous > 0 {
        now += 16 * MS;
        let level = detector.level(now, motion());
        assert!(level <= previous, "it never grows again on its way home");
        assert!(
            previous - level < FULLY_ENLARGED / 3,
            "a frame never jumps most of the way"
        );
        previous = level;
        steps += 1;
    }
    assert!(steps > 4, "it took {steps} frames");
}

#[test]
fn with_motion_reduced_it_changes_size_at_once() {
    let reduced = motion().with_reduced_motion(true);
    let mut detector = Shake::new();
    let end = stroke(&mut detector, 0, 6, wide(), 0, 80 * MS);
    assert_eq!(detector.level(end, reduced), FULLY_ENLARGED);
    assert_eq!(
        detector.next_frame_in(end),
        Some((end - 80 * MS + HOLD_NS) - end)
    );
    assert_eq!(detector.level(end - 80 * MS + HOLD_NS, reduced), 0);
    assert_eq!(detector.next_frame_in(end + HOLD_NS), None);
}

#[test]
fn a_stroke_is_measured_in_logical_pixels() {
    let doubled = Scale::from_percent(200).expect("a scale");
    let mut detector = Shake::new();
    let mut at = Point::new(500, 500);
    let mut now = 0;
    detector.observe(now, at, doubled);
    let width = i32::try_from(STROKE_MIN_PX).expect("small") + 8;
    for index in 0..8 {
        let sign = if index % 2 == 0 { 1 } else { -1 };
        now += 60 * MS;
        at = Point::new(at.x + sign * width, at.y);
        detector.observe(now, at, doubled);
    }
    assert_eq!(
        run_for(&mut detector, now, 400 * MS),
        0,
        "wide enough at one scale is half as wide at twice it"
    );
}

#[test]
fn a_reset_puts_the_pointer_back_at_once() {
    let mut detector = Shake::new();
    let end = stroke(&mut detector, 0, 6, wide(), 0, 80 * MS);
    let _ = run_for(&mut detector, end, 200 * MS);
    detector.reset();
    assert_eq!(detector.level(end + 200 * MS, motion()), 0);
    assert_eq!(detector.next_frame_in(end + 200 * MS), None);
}
