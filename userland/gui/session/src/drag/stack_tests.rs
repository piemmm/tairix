//! Host tests for what a carried drag shows under the pointer.

use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::driver::display::RefreshRate;
use tairix_abi::time::NANOS_PER_MILLI as MS;
use tairix_browse::render::Tiles;
use tairix_controls::ControlState;
use tairix_geometry::{Point, Rect, Scale};
use tairix_icon::{IconKind, NoArtwork};
use tairix_theme::{Accessibility, Contrast, Density, Motion, MotionInteraction, Theme};
use tairix_wm::Compositor;

use super::{between, staged, Look, PictureGrab, Stack};
use crate::tests::compositor;

/// The one item a drag shows, as its tile, named `label`.
fn tile(label: &str) -> Look {
    Look {
        label: Some(String::from(label)),
        icon: IconKind::File,
        own: None,
        state: ControlState::idle(),
        count: None,
    }
}

/// A card of a stack, the front wearing `count`.
fn card(count: Option<u32>) -> Look {
    Look {
        label: None,
        icon: IconKind::Folder,
        own: None,
        state: ControlState::idle(),
        count,
    }
}

/// Each card's drawn origin, the front first.
fn origins(stack: &Stack) -> Vec<Point> {
    stack.cards.iter().map(|card| card.at).collect()
}

/// Each card's origin at rest with the pointer at `pointer`.
fn rests(stack: &Stack, pointer: Point) -> Vec<Point> {
    stack.cards.iter().map(|card| card.rest(pointer)).collect()
}

/// The gather's span in `compositor`'s theme.
fn span(compositor: &Compositor) -> u64 {
    u64::from(
        compositor
            .theme()
            .motion()
            .duration(MotionInteraction::DragGather),
    ) * MS
}

/// One item is drawn as the tile its listing draws, held where the press
/// held it, so the pointer stays on the same point of its picture; it rests on
/// the pointer from the first and never asks for a frame.
#[test]
fn one_item_is_its_listed_tile_held_where_it_was_pressed() {
    let theme = Theme::dark();
    let pointer = Point::new(400, 300);
    let grab = PictureGrab {
        offset: (-9, 6),
        side: 40,
    };
    let stack = Stack::new(
        [(tile("notes.txt"), None)],
        pointer,
        grab,
        (Scale::ONE, &theme),
    );
    let metrics = Tiles::Large.metrics(Scale::ONE, &theme);
    let front = &stack.cards[0];
    assert_eq!(front.size, (metrics.cell_width, metrics.cell_height));
    let layout = Tiles::Large.layout();
    let bounds = Rect::new(0, 0, front.size.0, front.size.1);
    let picture = layout
        .parts(
            bounds,
            Scale::ONE,
            &theme,
            "notes.txt",
            tairix_browse::render::name_cut("notes.txt"),
        )
        .and_then(|parts| parts.picture)
        .expect("a picture");
    let side = layout.icon_side(bounds, Scale::ONE, &theme);
    let (dx, dy) = grab.on(side);
    let held = Point::new(
        front.at.x + picture.left() + tairix_geometry::to_i32(picture.width / 2) + dx,
        front.at.y + picture.top() + tairix_geometry::to_i32(picture.height / 2) + dy,
    );
    assert_eq!(
        held, pointer,
        "the pointer holds the picture where it was pressed"
    );
    assert_eq!(stack.next_frame_in(0, RefreshRate::ASSUMED), None);
}

/// One item never travels: it is drawn at rest under the pointer from the
/// first, however far from it its picture stood.
///
/// Regression: a lone card started where its picture stood and jumped to the
/// pointer at the first move.
#[test]
fn one_item_starts_at_rest_wherever_it_stood() {
    let theme = Theme::dark();
    let pointer = Point::new(400, 300);
    let far = Some(Point::new(5, 5));
    let stack = Stack::new(
        [(tile("a"), far)],
        pointer,
        PictureGrab::CENTRE,
        (Scale::ONE, &theme),
    );
    assert_eq!(origins(&stack), rests(&stack, pointer));
    assert_eq!(stack.next_frame_in(0, RefreshRate::ASSUMED), None);
}

/// A press held outside the picture keeps the tile under the pointer: the
/// hold is kept to the tile's edge, so the pointer is always over what it
/// carries.
#[test]
fn a_grab_off_the_tile_is_held_to_its_edge() {
    let theme = Theme::dark();
    let pointer = Point::new(400, 300);
    for offset in [(5_000, 5_000), (-5_000, -5_000), (i32::MAX, i32::MIN)] {
        let grab = PictureGrab { offset, side: 1 };
        let stack = Stack::new([(tile("a"), None)], pointer, grab, (Scale::ONE, &theme));
        let front = &stack.cards[0];
        let shown = Rect::new(front.at.x, front.at.y, front.size.0, front.size.1);
        assert!(shown.contains(pointer), "{offset:?}");
    }
}

/// Several items gather from where they stood to a stack under the pointer:
/// the cards set out at the next frame, each later than the one in front of
/// it, all at rest once the gather's span has run, and nothing is asked for
/// after.
#[test]
fn several_items_gather_from_where_they_stood() {
    let theme = Theme::dark();
    let mut comp = compositor();
    let pointer = Point::new(600, 400);
    let looks = [
        (card(Some(3)), Some(Point::new(100, 120))),
        (card(None), Some(Point::new(160, 120))),
        (card(None), Some(Point::new(220, 120))),
    ];
    let mut stack = Stack::new(looks, pointer, PictureGrab::CENTRE, (Scale::ONE, &theme));
    let stood = origins(&stack);
    assert!(stood
        .iter()
        .zip(rests(&stack, pointer))
        .all(|(at, rest)| *at != rest));
    assert_eq!(
        stack.next_frame_in(0, RefreshRate::ASSUMED),
        Some(0),
        "it sets out at once"
    );
    stack.advance(10 * MS, pointer, &mut comp);
    let span = span(&comp);
    stack.advance(10 * MS + span / 2, pointer, &mut comp);
    let midway = origins(&stack);
    let rest = rests(&stack, pointer);
    let travelled = |index: usize| {
        let whole = (rest[index].x - stood[index].x).abs();
        let gone = (midway[index].x - stood[index].x).abs();
        gone * 1000 / whole.max(1)
    };
    assert!(travelled(0) > travelled(1) && travelled(1) > travelled(2));
    assert!(stack
        .next_frame_in(10 * MS + span / 2, RefreshRate::ASSUMED)
        .is_some());
    stack.advance(10 * MS + span, pointer, &mut comp);
    assert_eq!(origins(&stack), rest, "all at rest");
    assert!(stack.gathered());
    assert_eq!(
        stack.next_frame_in(10 * MS + span, RefreshRate::ASSUMED),
        None
    );
}

/// Once gathered, the front card rests on the pointer with each batch, and
/// the cards behind set out after it at the next frame and catch up.
#[test]
fn the_front_card_follows_at_once_and_the_rest_catch_up() {
    let theme = Theme::dark();
    let mut comp = compositor();
    let start = Point::new(600, 400);
    let looks = [(card(Some(2)), None), (card(None), None)];
    let mut stack = Stack::new(looks, start, PictureGrab::CENTRE, (Scale::ONE, &theme));
    let span = span(&comp);
    stack.advance(0, start, &mut comp);
    stack.advance(span, start, &mut comp);
    assert!(stack.gathered());
    let moved = Point::new(700, 450);
    stack.follow(moved, &mut comp);
    assert_eq!(
        origins(&stack)[0],
        rests(&stack, moved)[0],
        "the front moved with the batch"
    );
    assert_ne!(
        origins(&stack)[1],
        rests(&stack, moved)[1],
        "the back has yet to"
    );
    assert_eq!(stack.next_frame_in(span, RefreshRate::ASSUMED), Some(0));
    stack.advance(2 * span, moved, &mut comp);
    stack.advance(3 * span, moved, &mut comp);
    assert_eq!(origins(&stack), rests(&stack, moved));
    stack.follow(moved, &mut comp);
    assert_eq!(
        stack.next_frame_in(3 * span, RefreshRate::ASSUMED),
        None,
        "a pointer at rest owes nothing"
    );
}

/// A pointer moving all the while keeps every card moving with it, the front
/// exactly under it once gathered and each card behind a fixed while after
/// the one in front, rather than waiting for the pointer to stop.
///
/// Regression: every batch restarted the cards' travel, so a card behind the
/// front never got past its stagger while the pointer kept moving.
#[test]
fn a_moving_pointer_keeps_every_card_moving() {
    let theme = Theme::dark();
    let mut comp = compositor();
    let looks = [
        (card(Some(3)), None),
        (card(None), None),
        (card(None), None),
    ];
    let start = Point::new(300, 300);
    let mut stack = Stack::new(looks, start, PictureGrab::CENTRE, (Scale::ONE, &theme));
    let span = span(&comp);
    let (mut frames, mut moved) = (0_u32, 0_u32);
    for step in 1..=60_i32 {
        let now = u64::try_from(step).expect("positive") * 16 * MS;
        let pointer = Point::new(start.x + step * 8, start.y);
        stack.follow(pointer, &mut comp);
        let behind = origins(&stack)[2];
        stack.advance(now, pointer, &mut comp);
        if now > span + 100 * MS {
            frames += 1;
            moved += u32::from(origins(&stack)[2] != behind);
            assert_eq!(
                origins(&stack)[0],
                rests(&stack, pointer)[0],
                "the front rests on it"
            );
            assert!(
                origins(&stack)[2].x < origins(&stack)[0].x,
                "the back card trails"
            );
        }
    }
    assert!(
        moved * 10 >= frames * 9,
        "the back card moved on {moved} of {frames} frames"
    );
}

/// At the fastest rate a display may refresh, the back card still follows
/// where the pointer was its whole lag ago, rather than stopping where the
/// oldest place the trail kept left it.
///
/// Regression: the trail kept one place a frame, which above about three
/// hundred frames a second spanned less than the back card's lag.
#[test]
fn the_back_card_keeps_its_lag_at_the_fastest_display() {
    let theme = Theme::dark();
    let mut comp = compositor();
    let looks = [
        (card(Some(4)), None),
        (card(None), None),
        (card(None), None),
        (card(None), None),
    ];
    let start = Point::new(100, 300);
    let mut stack = Stack::new(looks, start, PictureGrab::CENTRE, (Scale::ONE, &theme));
    let fastest = RefreshRate::from_mhz(RefreshRate::MAX_MHZ).expect("the bound is a rate");
    let frame = fastest.frame_ns();
    let frames = i32::try_from(3 * span(&comp) / frame).expect("a short drag");
    for step in 1..=frames {
        let now = u64::try_from(step).expect("positive") * frame;
        let pointer = Point::new(start.x + step, start.y);
        stack.follow(pointer, &mut comp);
        stack.advance(now, pointer, &mut comp);
    }
    let lag = i32::try_from(3 * stack.lag_ns / frame).expect("a short lag");
    let then = Point::new(start.x + frames - lag, start.y);
    let drift = (origins(&stack)[3].x - rests(&stack, then)[3].x).abs();
    assert!(
        drift <= lag / 10,
        "the back card is {drift} px from where its lag puts it"
    );
}

/// With motion reduced the stack is under the pointer at the first frame and
/// asks for no other.
#[test]
fn reduced_motion_places_the_stack_at_once() {
    let theme = Theme::dark();
    let mut comp = compositor();
    comp.set_theme(Theme::dark().with_axes(Accessibility {
        contrast: Contrast::Normal,
        density: Density::Normal,
        motion: Motion::Reduced,
    }));
    let pointer = Point::new(600, 400);
    let looks = [
        (card(Some(2)), Some(Point::new(100, 100))),
        (card(None), Some(Point::new(200, 100))),
    ];
    let mut stack = Stack::new(looks, pointer, PictureGrab::CENTRE, (Scale::ONE, &theme));
    stack.advance(5 * MS, pointer, &mut comp);
    assert_eq!(origins(&stack), rests(&stack, pointer));
    assert_eq!(stack.next_frame_in(5 * MS, RefreshRate::ASSUMED), None);
}

/// Each card is a window of its own that nothing lands on, painted where it
/// is drawn, and painted again in place; closing takes every one down.
#[test]
fn each_card_is_a_window_the_pointer_passes_through() {
    let theme = Theme::dark();
    let mut comp = compositor();
    let pointer = Point::new(600, 400);
    let looks = [(card(Some(2)), None), (card(None), None)];
    let mut stack = Stack::new(looks, pointer, PictureGrab::CENTRE, (Scale::ONE, &theme));
    stack.paint(&mut comp, &theme, &mut NoArtwork);
    let windows: Vec<_> = stack.windows().collect();
    assert_eq!(windows.len(), 2);
    for (window, card) in windows.iter().zip(&stack.cards) {
        let shown = comp.window(*window).expect("open").bounds();
        assert_eq!((shown.left(), shown.top()), (card.at.x, card.at.y));
    }
    assert_eq!(
        comp.window_at(pointer),
        None,
        "the pointer passes through them"
    );
    stack.paint(&mut comp, &theme, &mut NoArtwork);
    assert_eq!(
        stack.windows().collect::<Vec<_>>(),
        windows,
        "painted again in place"
    );
    stack.close(&mut comp);
    assert_eq!(stack.windows().count(), 0);
    assert!(windows.iter().all(|window| comp.window(*window).is_none()));
}

/// A card later in a travel sets out later and arrives with the rest; a point
/// part way along lies between its ends.
#[test]
fn a_staged_travel_ends_together_and_lies_between_its_ends() {
    assert_eq!(staged(0, 40), 0);
    assert_eq!(staged(40, 40), 0);
    assert!(staged(100, 40) < 100);
    assert_eq!(staged(u8::MAX, 40), u8::MAX);
    assert_eq!(staged(u8::MAX, u8::MAX), u8::MAX, "even the latest arrives");
    let (from, to) = (Point::new(-10, 50), Point::new(90, -50));
    assert_eq!(between(from, to, 0), from);
    assert_eq!(between(from, to, u8::MAX), to);
    let middle = between(from, to, 128);
    assert!(middle.x > from.x && middle.x < to.x && middle.y < from.y && middle.y > to.y);
    let far = between(
        Point::new(i32::MIN, i32::MIN),
        Point::new(i32::MAX, i32::MAX),
        200,
    );
    assert!(far.x > 0 && far.y > 0, "no overflow across the whole range");
}
