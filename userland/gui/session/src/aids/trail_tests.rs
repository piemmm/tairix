use tairix_geometry::Point;
use tairix_inline::ArrayVec;
use tairix_wallpaper::PointerTrail;
use tairix_wm::{Ghost, MAX_GHOSTS};

use super::{shape, Trail, PATH_SAMPLES};

const MS: u64 = 1_000_000;

/// A trail of `length` fed a pointer moving right at one pixel a millisecond,
/// one sample every 4 ms, until `until_ns`.
fn moving(length: PointerTrail, until_ns: u64) -> Trail {
    let mut trail = Trail::new();
    trail.set_length(length);
    let mut now = 0;
    while now <= until_ns {
        trail.observe(
            now,
            Point::new(i32::try_from(now / MS).expect("small"), 100),
        );
        now += 4 * MS;
    }
    trail
}

fn ghosts(trail: &Trail, now_ns: u64, current: Point) -> ArrayVec<Ghost, MAX_GHOSTS> {
    let mut out = ArrayVec::new();
    trail.ghosts(now_ns, current, &mut out);
    out
}

#[test]
fn an_absent_trail_draws_nothing_and_keeps_no_path() {
    let trail = moving(PointerTrail::Off, 400 * MS);
    assert!(ghosts(&trail, 400 * MS, Point::new(400, 100)).is_empty());
    assert_eq!(trail.next_frame_in(400 * MS), None);
    assert!(trail.path.is_empty());
}

#[test]
fn a_moving_pointer_leaves_evenly_spaced_copies_fading_with_age() {
    let now = 400 * MS;
    let trail = moving(PointerTrail::Medium, now);
    let (count, spacing) = shape(PointerTrail::Medium).expect("a trail");
    let drawn = ghosts(&trail, now, Point::new(400, 100));
    assert_eq!(drawn.len(), count);
    let step = i32::try_from(spacing / MS).expect("small");
    for (index, ghost) in drawn.iter().enumerate() {
        let behind = i32::try_from(count - index).expect("small");
        assert_eq!(
            ghost.at,
            Point::new(400 - behind * step, 100),
            "copy {index}"
        );
    }
    assert!(
        drawn
            .windows(2)
            .all(|pair| pair[0].opacity < pair[1].opacity),
        "the oldest is the faintest"
    );
    assert!(drawn
        .last()
        .is_some_and(|nearest| nearest.opacity < u8::MAX));
}

#[test]
fn copies_draw_back_into_a_pointer_that_stops() {
    let stopped = 400 * MS;
    let trail = moving(PointerTrail::Long, stopped);
    let (count, spacing) = shape(PointerTrail::Long).expect("a trail");
    let span = u64::try_from(count).expect("small") * spacing;
    assert!(trail.next_frame_in(stopped).is_some());
    let mid = ghosts(&trail, stopped + span / 2, Point::new(400, 100));
    assert!(!mid.is_empty() && mid.len() < count, "some have caught up");
    assert!(ghosts(&trail, stopped + span, Point::new(400, 100)).is_empty());
    assert_eq!(
        trail.next_frame_in(stopped + span),
        None,
        "nothing more is owed"
    );
}

#[test]
fn a_pointer_leaving_rest_trails_from_where_it_rested() {
    let mut trail = Trail::new();
    trail.set_length(PointerTrail::Short);
    trail.observe(0, Point::new(10, 10));
    // Still for a long while, then found somewhere new.
    let now = 5_000 * MS;
    trail.observe(now, Point::new(210, 10));
    let drawn = ghosts(&trail, now, Point::new(210, 10));
    assert!(!drawn.is_empty());
    for ghost in &drawn {
        assert_eq!(ghost.at.y, 10);
        assert!(
            ghost.at.x >= 10 && ghost.at.x < 210,
            "{:?} is on the way",
            ghost.at
        );
    }
    assert!(
        drawn.first().is_some_and(|oldest| oldest.at.x < 110),
        "the move happened in the last frame, not across the rest"
    );
    assert!(
        drawn.windows(2).all(|pair| pair[0].at != pair[1].at),
        "copies left on one spot are drawn once"
    );
}

#[test]
fn a_longer_trail_reaches_further_back_with_more_copies() {
    let now = 400 * MS;
    let lengths = [
        PointerTrail::Short,
        PointerTrail::Medium,
        PointerTrail::Long,
    ];
    let reach: alloc::vec::Vec<(usize, i32)> = lengths
        .iter()
        .map(|length| {
            let drawn = ghosts(&moving(*length, now), now, Point::new(400, 100));
            (drawn.len(), drawn.first().map_or(400, |oldest| oldest.at.x))
        })
        .collect();
    assert!(reach
        .windows(2)
        .all(|pair| pair[0].0 < pair[1].0 && pair[0].1 > pair[1].1));
}

#[test]
fn a_pointer_that_has_not_moved_owes_no_frame() {
    let mut trail = Trail::new();
    trail.set_length(PointerTrail::Long);
    trail.observe(0, Point::new(10, 10));
    trail.observe(50 * MS, Point::new(10, 10));
    assert_eq!(trail.next_frame_in(50 * MS), None);
    assert!(ghosts(&trail, 50 * MS, Point::new(10, 10)).is_empty());
}

#[test]
fn samples_of_one_instant_are_one_sample() {
    let mut trail = Trail::new();
    trail.set_length(PointerTrail::Medium);
    trail.observe(0, Point::new(0, 0));
    trail.observe(10 * MS, Point::new(5, 0));
    trail.observe(10 * MS, Point::new(9, 0));
    assert_eq!(
        trail.path.back().copied(),
        Some((10 * MS, Point::new(9, 0)))
    );
    assert_eq!(trail.path.len(), 2);
}

#[test]
fn the_path_is_bounded_and_forgets_what_no_copy_can_reach() {
    let trail = moving(PointerTrail::Long, 4_000 * MS);
    assert!(trail.path.len() <= PATH_SAMPLES);
    let (count, spacing) = shape(PointerTrail::Long).expect("a trail");
    let reach = u64::try_from(count).expect("small") * spacing;
    let oldest = trail.path.front().map_or(0, |(ns, _)| *ns);
    assert!(
        4_000 * MS - oldest <= reach + 4 * MS,
        "only what the trail can reach is kept"
    );
}

#[test]
fn a_device_reporting_every_millisecond_still_fills_the_longest_trail() {
    let mut trail = Trail::new();
    trail.set_length(PointerTrail::Long);
    let mut now = 0;
    while now <= 300 * MS {
        trail.observe(
            now,
            Point::new(i32::try_from(now / MS).expect("small"), 100),
        );
        now += MS;
    }
    let now = 300 * MS;
    let (count, spacing) = shape(PointerTrail::Long).expect("a trail");
    let reach = u64::try_from(count).expect("small") * spacing;
    let oldest = trail.path.front().map_or(now, |(ns, _)| *ns);
    assert!(
        oldest <= now - reach,
        "the path reaches back past the trail"
    );
    let drawn = ghosts(&trail, now, Point::new(300, 100));
    assert_eq!(drawn.len(), count, "every copy has somewhere of its own");
    let step = i32::try_from(spacing / MS).expect("small");
    let oldest_x = drawn.first().map(|ghost| ghost.at.x).expect("drawn");
    assert!(oldest_x.abs_diff(300 - step * i32::try_from(count).expect("small")) <= 4);
}

#[test]
fn turning_a_trail_off_forgets_its_path() {
    let mut trail = moving(PointerTrail::Long, 400 * MS);
    trail.set_length(PointerTrail::Off);
    assert!(trail.path.is_empty());
    trail.set_length(PointerTrail::Long);
    assert!(ghosts(&trail, 400 * MS, Point::new(400, 100)).is_empty());
}
