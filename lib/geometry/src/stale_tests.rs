//! Unit tests for [`StaleFrames`], including a model run: a ring of pixel
//! frames brought up to date only through what the record hands each one must
//! match the picture after every present.

use alloc::vec;
use alloc::vec::Vec;

use super::StaleFrames;
use crate::{Rect, Region};

const BOUNDS: Rect = Rect::new(0, 0, 16, 12);

fn region(rects: &[Rect]) -> Region {
    let mut region = Region::new();
    for rect in rects {
        region.add(*rect);
    }
    region
}

/// What `update` is handed when frame `index` is brought up to date for
/// `changed`, the update succeeding.
fn refreshed(stale: &mut StaleFrames, index: usize, changed: &Region) -> Region {
    let mut seen = Region::new();
    let updated: Result<(), ()> = stale.bring_up_to_date(index, changed, |refresh| {
        seen.clone_from(refresh);
        Ok(())
    });
    assert_eq!(updated, Ok(()));
    seen
}

#[test]
fn a_fresh_ring_hands_every_frame_the_whole_picture_once() {
    let mut stale = StaleFrames::new(2, BOUNDS, 8);
    let changed = region(&[Rect::new(1, 1, 2, 2)]);
    assert_eq!(refreshed(&mut stale, 0, &changed), Region::from(BOUNDS));
    assert_eq!(refreshed(&mut stale, 1, &changed), Region::from(BOUNDS));
    assert_eq!(refreshed(&mut stale, 0, &changed), changed);
}

#[test]
fn a_change_is_owed_to_every_frame_but_the_one_it_was_drawn_in() {
    let mut stale = StaleFrames::new(3, BOUNDS, 8);
    let nothing = Region::new();
    for index in 0..3 {
        refreshed(&mut stale, index, &nothing);
    }
    let first = region(&[Rect::new(0, 0, 4, 4)]);
    assert_eq!(refreshed(&mut stale, 0, &first), first);
    let second = region(&[Rect::new(8, 6, 3, 3)]);
    let mut lacks = first;
    lacks.add_region(&second);
    assert_eq!(refreshed(&mut stale, 1, &second), lacks);
    assert_eq!(refreshed(&mut stale, 2, &nothing), lacks);
    assert_eq!(refreshed(&mut stale, 0, &nothing), second);
}

#[test]
fn a_refused_update_leaves_the_frame_owing_everything_and_owes_nothing_on() {
    let mut stale = StaleFrames::new(2, BOUNDS, 8);
    let nothing = Region::new();
    refreshed(&mut stale, 0, &nothing);
    refreshed(&mut stale, 1, &nothing);
    let changed = region(&[Rect::new(2, 2, 5, 5)]);
    let refused: Result<(), &str> = stale.bring_up_to_date(0, &changed, |_| Err("refused"));
    assert_eq!(refused, Err("refused"));
    assert_eq!(refreshed(&mut stale, 0, &nothing), changed);
    assert!(
        refreshed(&mut stale, 1, &nothing).is_empty(),
        "a change never drawn is owed to nobody else"
    );
}

#[test]
fn what_lies_outside_the_bounds_is_never_handed_or_owed() {
    let mut stale = StaleFrames::new(2, BOUNDS, 8);
    let nothing = Region::new();
    refreshed(&mut stale, 0, &nothing);
    refreshed(&mut stale, 1, &nothing);
    let changed = region(&[Rect::new(14, 10, 6, 6)]);
    let inside = region(&[Rect::new(14, 10, 2, 2)]);
    assert_eq!(refreshed(&mut stale, 0, &changed), inside);
    assert_eq!(refreshed(&mut stale, 1, &nothing), inside);
}

#[test]
fn a_reset_owes_every_frame_the_new_bounds() {
    let mut stale = StaleFrames::new(2, BOUNDS, 8);
    let nothing = Region::new();
    refreshed(&mut stale, 0, &nothing);
    refreshed(&mut stale, 1, &nothing);
    let bigger = Rect::new(0, 0, 40, 30);
    stale.reset(bigger);
    assert_eq!(refreshed(&mut stale, 0, &nothing), Region::from(bigger));
    assert_eq!(refreshed(&mut stale, 1, &nothing), Region::from(bigger));
}

#[test]
fn a_frame_the_ring_does_not_have_is_handed_its_whole_bounds() {
    let mut stale = StaleFrames::new(2, BOUNDS, 8);
    let nothing = Region::new();
    refreshed(&mut stale, 0, &nothing);
    refreshed(&mut stale, 1, &nothing);
    let changed = region(&[Rect::new(1, 1, 1, 1)]);
    assert_eq!(refreshed(&mut stale, 2, &changed), Region::from(BOUNDS));
    assert!(refreshed(&mut stale, 0, &nothing).is_empty());
    assert!(refreshed(&mut stale, 1, &nothing).is_empty());
}

#[test]
fn an_owed_region_coarsens_past_its_budget() {
    let mut stale = StaleFrames::new(2, BOUNDS, 2);
    let nothing = Region::new();
    refreshed(&mut stale, 0, &nothing);
    refreshed(&mut stale, 1, &nothing);
    for x in [0, 4, 8, 12] {
        refreshed(&mut stale, 0, &region(&[Rect::new(x, x / 2, 1, 1)]));
    }
    let owed = refreshed(&mut stale, 1, &nothing);
    assert!(owed.rects().len() <= 2, "{owed:?}");
    for x in [0, 4, 8, 12] {
        assert!(owed.contains(crate::Point::new(x, x / 2)), "{owed:?}");
    }
}

/// A ring of pixel frames, each brought up to date only by copying from the
/// picture what the record hands it, matches the picture after every present,
/// whichever frame each present is drawn in.
#[test]
fn every_frame_brought_up_to_date_matches_the_picture() {
    const FRAMES: usize = 3;
    let side = |extent: u32| usize::try_from(extent).unwrap_or(0);
    let (width, height) = (side(BOUNDS.width), side(BOUNDS.height));
    let mut rng = tairix_fuzzseed::Prng::new(0x57a1_e0f0_7a3e_5eed);
    for _ in 0..200 {
        let mut picture = vec![0u32; width * height];
        let mut frames: Vec<Vec<u32>> = (0..FRAMES)
            .map(|_| vec![u32::MAX; width * height])
            .collect();
        let mut stale = StaleFrames::new(FRAMES, BOUNDS, 3);
        for generation in 1..=24u32 {
            let draw = rng.next_u64();
            let changed = Rect::new(
                i32::try_from(draw % 18).unwrap_or(0),
                i32::try_from((draw >> 8) % 14).unwrap_or(0),
                u32::try_from((draw >> 16) % 7).unwrap_or(0),
                u32::try_from((draw >> 24) % 7).unwrap_or(0),
            )
            .intersection(&BOUNDS);
            for row in 0..side(changed.height) {
                for column in 0..side(changed.width) {
                    let x = usize::try_from(changed.left()).unwrap_or(0) + column;
                    let y = usize::try_from(changed.top()).unwrap_or(0) + row;
                    picture[y * width + x] = generation;
                }
            }
            let index = usize::try_from((draw >> 32) % 4).unwrap_or(0) % FRAMES;
            let frame = &mut frames[index];
            let copied: Result<(), ()> =
                stale.bring_up_to_date(index, &Region::from(changed), |refresh| {
                    for rect in refresh.rects() {
                        for row in 0..side(rect.height) {
                            for column in 0..side(rect.width) {
                                let x = usize::try_from(rect.left()).unwrap_or(0) + column;
                                let y = usize::try_from(rect.top()).unwrap_or(0) + row;
                                frame[y * width + x] = picture[y * width + x];
                            }
                        }
                    }
                    Ok(())
                });
            assert_eq!(copied, Ok(()));
            assert_eq!(
                frames[index], picture,
                "frame {index} at generation {generation}"
            );
        }
    }
}
