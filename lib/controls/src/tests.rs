//! Unit tests for the scroll geometry engine.
//!
//! These cover the checklist the design language requires of a scrollbar:
//! zero overflow, proportional thumb math, minimum thumb size, line and page
//! steps, range changes during drag, both orientations, keyboard bounds, and
//! the fail-closed behaviour for degenerate ranges.

use tairix_abi::window_ipc::SCROLL_UNITS_PER_DETENT;
use tairix_geometry::{Point, Rect, Region};
use tairix_raster::{Color, Pixel, Surface};

use crate::scroll::{
    wheel_steps, ScrollGeometry, ScrollModel, ScrollOrientation, ScrollRange, ScrollView, TrackHit,
};

// --- ScrollRange normalisation ------------------------------------------

#[test]
fn range_not_scrollable_when_viewport_covers_content() {
    let range = ScrollRange::new(100, 100, 50);
    assert!(!range.is_scrollable());
    assert_eq!(range.max_offset(), 0);
    assert_eq!(range.offset(), 0, "offset pins to zero when not scrollable");
}

#[test]
fn range_not_scrollable_when_viewport_exceeds_content() {
    let range = ScrollRange::new(80, 200, 10);
    assert!(!range.is_scrollable());
    assert_eq!(range.max_offset(), 0);
    assert_eq!(range.offset(), 0);
}

#[test]
fn zero_viewport_fails_closed_to_not_scrollable() {
    let range = ScrollRange::new(1000, 0, 500);
    assert!(!range.is_scrollable());
    assert_eq!(range.offset(), 0);
    assert_eq!(range.max_offset(), 0);
}

#[test]
fn offset_is_clamped_to_max() {
    let range = ScrollRange::new(1000, 200, 5_000);
    assert!(range.is_scrollable());
    assert_eq!(range.max_offset(), 800);
    assert_eq!(range.offset(), 800, "overflowing offset clamps to the end");
}

#[test]
fn resize_preserves_offset_where_it_still_fits() {
    let range = ScrollRange::new(1000, 200, 300);
    let grown = range.resize(2000, 200);
    assert_eq!(grown.offset(), 300, "offset kept when still valid");
    assert_eq!(grown.max_offset(), 1800);
}

#[test]
fn resize_clamps_offset_that_no_longer_fits() {
    let range = ScrollRange::new(1000, 200, 700);
    let shrunk = range.resize(400, 200);
    assert_eq!(shrunk.max_offset(), 200);
    assert_eq!(shrunk.offset(), 200, "offset clamped to the new end");
}

#[test]
fn extreme_extents_do_not_overflow() {
    let range = ScrollRange::new(u64::MAX, 1, u64::MAX);
    assert!(range.is_scrollable());
    assert_eq!(range.max_offset(), u64::MAX - 1);
    assert_eq!(range.offset(), u64::MAX - 1);
    // Geometry over a huge range must not panic and must stay in the track.
    let geom = ScrollGeometry::new(range, 400, 24);
    let thumb = geom.thumb();
    assert!(thumb.length >= 24);
    assert!(u64::from(thumb.start) + u64::from(thumb.length) <= 400);
}

// --- ScrollModel stepping ------------------------------------------------

fn model() -> ScrollModel {
    ScrollModel::new(ScrollRange::new(1000, 100, 0), 10, 90)
}

#[test]
fn line_and_page_steps_move_by_the_declared_distance() {
    let m = model().line_forward();
    assert_eq!(m.offset(), 10);
    let m = m.page_forward();
    assert_eq!(m.offset(), 100);
    let m = m.line_backward();
    assert_eq!(m.offset(), 90);
    let m = m.page_backward();
    assert_eq!(m.offset(), 0);
}

#[test]
fn steps_saturate_at_the_bounds() {
    let m = model().page_backward();
    assert_eq!(m.offset(), 0, "cannot scroll before the start");
    let m = model().to_end();
    assert_eq!(m.offset(), 900);
    let m = m.page_forward().line_forward();
    assert_eq!(m.offset(), 900, "cannot scroll past the end");
}

#[test]
fn home_and_end_reach_the_bounds() {
    assert_eq!(model().to_end().offset(), 900);
    assert_eq!(model().to_end().to_start().offset(), 0);
}

#[test]
fn scroll_by_negative_moves_toward_start() {
    let m = model().scroll_to(500).scroll_by(-200);
    assert_eq!(m.offset(), 300);
    let m = m.scroll_by(i64::MIN);
    assert_eq!(m.offset(), 0, "huge negative delta saturates, never wraps");
}

#[test]
fn zero_step_moves_nothing() {
    let m = ScrollModel::new(ScrollRange::new(1000, 100, 400), 0, 0);
    assert_eq!(m.line_forward().offset(), 400);
    assert_eq!(m.page_backward().offset(), 400);
}

#[test]
fn pathologically_large_step_saturates() {
    let m = ScrollModel::new(ScrollRange::new(1000, 100, 0), u64::MAX, u64::MAX);
    assert_eq!(
        m.line_forward().offset(),
        900,
        "clamped to the end, no panic"
    );
}

#[test]
fn model_resize_reclamps_offset() {
    let m = model().to_end();
    assert_eq!(m.offset(), 900);
    let m = m.resize(300, 100);
    assert_eq!(m.offset(), 200);
    assert_eq!(m.line_step(), 10, "steps unchanged by resize");
    assert_eq!(m.page_step(), 90);
}

// --- Thumb geometry ------------------------------------------------------

#[test]
fn thumb_is_proportional_to_visible_fraction() {
    // viewport is a quarter of the content -> thumb is a quarter of the track.
    let geom = ScrollGeometry::new(ScrollRange::new(400, 100, 0), 400, 10);
    assert_eq!(geom.thumb_length(), 100);
}

#[test]
fn thumb_respects_minimum_length() {
    // viewport is 1/1000 of content; proportional thumb would be sub-pixel.
    let geom = ScrollGeometry::new(ScrollRange::new(100_000, 100, 0), 400, 24);
    assert_eq!(geom.thumb_length(), 24, "floored at the theme minimum");
}

#[test]
fn thumb_never_exceeds_track() {
    let geom = ScrollGeometry::new(ScrollRange::new(101, 100, 0), 40, 100);
    assert_eq!(
        geom.thumb_length(),
        40,
        "minimum capped by the track length"
    );
}

#[test]
fn non_scrollable_thumb_fills_track_and_is_not_draggable() {
    let geom = ScrollGeometry::new(ScrollRange::new(100, 100, 0), 400, 24);
    assert_eq!(geom.thumb_length(), 400);
    assert_eq!(geom.travel(), 0);
    assert!(!geom.draggable());
    assert_eq!(geom.thumb().start, 0);
}

#[test]
fn zero_track_yields_empty_thumb() {
    let geom = ScrollGeometry::new(ScrollRange::new(1000, 100, 500), 0, 24);
    assert_eq!(geom.thumb_length(), 0);
    assert!(!geom.draggable());
    assert_eq!(geom.offset_for_thumb_start(50), 0);
}

#[test]
fn thumb_position_maps_offset_across_travel() {
    let range = ScrollRange::new(1000, 100, 0);
    let track = 400;
    let geom = ScrollGeometry::new(range, track, 40);
    let thumb_len = geom.thumb_length();
    assert_eq!(thumb_len, 40);
    let travel = geom.travel();
    assert_eq!(travel, 360);

    // At offset 0 the thumb is at the start.
    assert_eq!(geom.thumb().start, 0);
    // At max offset the thumb sits flush against the end.
    let at_end = ScrollGeometry::new(range.with_offset(900), track, 40);
    assert_eq!(at_end.thumb().start, travel);
    assert_eq!(
        u64::from(at_end.thumb().start) + u64::from(thumb_len),
        u64::from(track)
    );
    // Halfway.
    let mid = ScrollGeometry::new(range.with_offset(450), track, 40);
    assert_eq!(mid.thumb().start, travel / 2);
}

// --- Round-trip: thumb position <-> offset --------------------------------

#[test]
fn thumb_start_and_offset_are_inverses_at_the_bounds() {
    let range = ScrollRange::new(1000, 100, 0);
    let geom = ScrollGeometry::new(range, 400, 40);
    let travel = geom.travel();
    assert_eq!(geom.offset_for_thumb_start(0), 0);
    assert_eq!(geom.offset_for_thumb_start(travel), 900);
    // Past the end clamps.
    assert_eq!(geom.offset_for_thumb_start(travel + 500), 900);
}

#[test]
fn offset_for_thumb_start_rounds_to_nearest() {
    let range = ScrollRange::new(1000, 100, 0);
    let geom = ScrollGeometry::new(range, 400, 40);
    let travel = geom.travel();
    // Middle of the travel maps to the middle of the offset range.
    assert_eq!(geom.offset_for_thumb_start(travel / 2), 450);
}

// --- Hit testing ---------------------------------------------------------

#[test]
fn hit_classifies_track_regions() {
    let range = ScrollRange::new(1000, 100, 450);
    let geom = ScrollGeometry::new(range, 400, 40);
    let thumb = geom.thumb();
    assert_eq!(
        geom.hit(thumb.start.saturating_sub(1)),
        TrackHit::BeforeThumb
    );
    assert_eq!(geom.hit(thumb.start), TrackHit::Thumb);
    assert_eq!(geom.hit(thumb.start + thumb.length - 1), TrackHit::Thumb);
    assert_eq!(geom.hit(thumb.start + thumb.length), TrackHit::AfterThumb);
    assert_eq!(geom.hit(10_000), TrackHit::AfterThumb, "past the track");
}

// --- Drag ----------------------------------------------------------------

#[test]
fn drag_preserves_pointer_to_thumb_anchor() {
    let range = ScrollRange::new(1000, 100, 300);
    let geom = ScrollGeometry::new(range, 400, 40);
    let thumb = geom.thumb();
    // Pointer grabs the thumb 5px from its near edge.
    let grab = thumb.start + 5;
    let anchor = i32::try_from(grab).unwrap() - i32::try_from(thumb.start).unwrap();
    // No pointer movement -> same offset (no jump).
    let same = geom.offset_for_drag(i32::try_from(grab).unwrap(), anchor);
    assert_eq!(same, 300, "grabbing the thumb must not move content");
    // Drag the pointer to the very end.
    let end = geom.offset_for_drag(10_000, anchor);
    assert_eq!(end, 900);
    // Drag before the start pins to zero.
    let start = geom.offset_for_drag(-10_000, anchor);
    assert_eq!(start, 0);
}

#[test]
fn drag_on_non_draggable_bar_is_inert() {
    let geom = ScrollGeometry::new(ScrollRange::new(100, 100, 0), 400, 40);
    assert_eq!(geom.offset_for_drag(200, 0), 0);
}

#[test]
fn range_change_during_drag_stays_in_bounds() {
    // Simulate a content extent shrinking mid-drag: the geometry is rebuilt
    // from the new range and the preserved anchor, and never yields an invalid
    // offset.
    let anchor = 5;
    let before = ScrollGeometry::new(ScrollRange::new(2000, 100, 0), 400, 40);
    let _ = before.offset_for_drag(100, anchor);
    // Content shrank to 300 units; viewport unchanged.
    let after = ScrollGeometry::new(ScrollRange::new(300, 100, 0), 400, 40);
    let offset = after.offset_for_drag(100_000, anchor);
    assert!(offset <= after.range().max_offset());
    assert_eq!(offset, 200);
}

// --- Orientation ---------------------------------------------------------

#[test]
fn orientation_is_behaviourally_neutral() {
    // The same range and track produce identical thumb math regardless of
    // orientation; orientation only informs how a viewport maps the 1-D span.
    let range = ScrollRange::new(1000, 250, 500);
    let geom = ScrollGeometry::new(range, 320, 32);
    let v = ScrollOrientation::Vertical;
    let h = ScrollOrientation::Horizontal;
    assert_ne!(v, h);
    // There is exactly one geometry; both axes read the same numbers.
    assert_eq!(geom.thumb().length, 80);
}

// --- Pixel models, revealing, the wheel, and the scrolled view -----------

#[test]
fn a_pixel_model_pages_a_viewport_less_one_line() {
    let m = ScrollModel::in_pixels(ScrollRange::new(1000, 300, 0), 24);
    assert_eq!((m.line_step(), m.page_step()), (24, 276));
    let tall_line = ScrollModel::in_pixels(ScrollRange::new(1000, 20, 0), 24);
    assert_eq!(
        tall_line.page_step(),
        24,
        "a page is never shorter than a line"
    );
    assert_eq!(ScrollModel::in_pixels(ScrollRange::EMPTY, 0).line_step(), 1);
}

#[test]
fn revealing_moves_the_least_that_shows_the_span() {
    let m = ScrollModel::in_pixels(ScrollRange::new(1000, 100, 200), 10);
    assert_eq!(m.revealing(250, 20).offset(), 200, "already shown: unmoved");
    assert_eq!(
        m.revealing(290, 20).offset(),
        210,
        "below: its end at the end"
    );
    assert_eq!(
        m.revealing(150, 20).offset(),
        150,
        "above: its start at the start"
    );
    assert_eq!(
        m.revealing(500, 300).offset(),
        500,
        "taller than the viewport: its start"
    );
    assert_eq!(m.revealing(990, 50).offset(), 900, "clamped to the content");
}

#[test]
fn a_detent_is_its_step_and_parts_of_one_carry() {
    let mut carry = 0;
    assert_eq!(wheel_steps(SCROLL_UNITS_PER_DETENT, 48, &mut carry), 48);
    assert_eq!(
        wheel_steps(-2 * SCROLL_UNITS_PER_DETENT, 48, &mut carry),
        -96
    );
    assert_eq!(wheel_steps(0, 48, &mut carry), 0);
    let mut carry = 0;
    let moved: i64 = (0..5).map(|_| wheel_steps(1, 48, &mut carry)).sum();
    assert_eq!(moved, 2, "five units of 48/120 of a pixel make two");
    assert_eq!(
        wheel_steps(-1, 48, &mut carry),
        0,
        "a reversal drops the carry"
    );
    assert!(carry <= 0);
    let mut carry = 0;
    assert_eq!(
        wheel_steps(i32::MAX, u64::MAX, &mut carry),
        i64::MAX,
        "saturates"
    );
}

fn view(offset: u64) -> ScrollView {
    ScrollView::new(
        ScrollOrientation::Vertical,
        Rect::new(10, 20, 100, 50),
        offset,
    )
}

#[test]
fn a_scrolled_view_maps_points_and_rectangles_both_ways() {
    let v = view(30);
    assert_eq!(v.to_content(Point::new(15, 20)), Some(Point::new(15, 50)));
    assert_eq!(v.to_content(Point::new(15, 70)), None, "below the viewport");
    assert_eq!(v.to_content(Point::new(9, 30)), None, "left of it");
    assert_eq!(
        v.to_window(Rect::new(10, 40, 100, 30)),
        Some(Rect::new(10, 20, 100, 20)),
        "a row half scrolled off the top shows its lower part"
    );
    assert_eq!(
        v.to_window(Rect::new(10, 20, 100, 30)),
        None,
        "scrolled off"
    );
    assert_eq!(v.shown(), 50..100);
    let across = ScrollView::new(ScrollOrientation::Horizontal, Rect::new(10, 20, 100, 50), 7);
    assert_eq!(
        across.to_content(Point::new(10, 20)),
        Some(Point::new(17, 20))
    );
    assert_eq!(across.shown(), 17..117);
}

#[test]
fn a_scrolled_view_paints_its_layout_shifted_and_confined() {
    let red = Color::rgb(255, 0, 0).premultiply();
    let mut surface = Surface::new(120, 80).expect("a surface");
    // Unscrolled, a band from layout row 40 to 90; scrolled 30, it shows
    // from window row 20 (the viewport's top) to 60.
    view(30).paint(&mut surface, |layout| {
        layout.fill_rect(10, 40, 100, 50, Color::rgb(255, 0, 0));
        layout.fill_rect(0, 0, 120, 10, Color::rgb(255, 0, 0));
    });
    for y in 0..80 {
        let want = if (20..60).contains(&y) {
            red
        } else {
            Pixel::TRANSPARENT
        };
        assert_eq!(surface.get(50, y), Some(want), "row {y}");
    }
    assert_eq!(
        surface.get(5, 30),
        Some(Pixel::TRANSPARENT),
        "left of the viewport"
    );
}

#[test]
fn a_scrolled_view_names_every_line_any_part_of_which_it_shows() {
    // Lines 20 pixels apart in a 50-pixel viewport scrolled 30: the viewport
    // spans 30..80, which is part of line 1, all of 2 and 3, and none of 4.
    assert_eq!(view(30).lines(20, 10), 1..4);
    assert_eq!(view(0).lines(25, 10), 0..2, "exactly two whole lines");
    assert_eq!(
        view(30).lines(20, 2),
        1..2,
        "no further than the lines that exist"
    );
    assert_eq!(view(30).lines(0, 10), 0..0);
    assert_eq!(view(500).lines(20, 10), 10..10, "scrolled past every line");
}

#[test]
fn a_scrolled_view_reports_layout_damage_where_it_shows() {
    let v = view(30);
    let mut layout = Region::new();
    layout.add(Rect::new(10, 40, 100, 30));
    layout.add(Rect::new(10, 0, 100, 20));
    let mut damage = Region::new();
    v.report(&layout, &mut damage);
    assert_eq!(
        damage.rects(),
        [Rect::new(10, 20, 100, 20)],
        "only what shows"
    );
}

#[test]
fn a_move_outside_the_viewport_never_lands_on_what_is_scrolled_out_of_sight() {
    // A row laid out right under the viewport's top, scrolled 10 pixels: a
    // pointer 3 pixels above the viewport (on a header pinned over the list)
    // must not reach the row's hidden part, or the row would hover and arm on
    // a press the reader aimed at something else.
    let v = view(10);
    let hidden_row = Rect::new(10, 20, 100, 24);
    let mapped = mapped_move(&v, Point::new(50, 17));
    assert!(
        !hidden_row.contains(mapped),
        "{mapped:?} landed on the hidden row"
    );
    assert!(
        mapped.y < 20,
        "before the content's start, outside every item"
    );
    assert_eq!(mapped.x, 50, "the cross axis is kept");
    assert_eq!(
        mapped_move(&v, Point::new(50, 25)),
        Point::new(50, 35),
        "inside the viewport a move maps straight into the layout"
    );
    let press = tairix_input::InputEvent::PointerPressed {
        button: tairix_input::PointerButton::Primary,
    };
    assert_eq!(
        v.event_in_layout(&press),
        press,
        "a press carries no position"
    );
}

#[test]
fn a_move_outside_the_viewport_keeps_its_place_across_the_scrolling_axis() {
    let v = view(10);
    for (at, why) in [
        (Point::new(80, 75), "below the viewport, over a footer"),
        (Point::new(115, 40), "beside it, over the scrollbar gutter"),
    ] {
        let mapped = mapped_move(&v, at);
        assert_eq!(mapped.x, at.x, "{why}: x is kept");
        assert!(mapped.y < 20, "{why}: {mapped:?} stands before the content");
    }
    let across = ScrollView::new(
        ScrollOrientation::Horizontal,
        Rect::new(10, 20, 100, 50),
        10,
    );
    let mapped = mapped_move(&across, Point::new(50, 75));
    assert_eq!(mapped.y, 75, "a horizontal view keeps y");
    assert!(mapped.x < 10, "{mapped:?} stands before the content");
}

#[test]
fn a_slider_dragged_out_of_a_scrolled_column_follows_the_pointer_to_its_end() {
    // A slider whose track ends at the viewport's edge is set to its maximum
    // by overshooting into the gutter beside it, and a drag that drifts below
    // the column keeps the value the pointer's x names.
    let v = view(10);
    let bounds = Rect::new(10, 40, 100, 20);
    let mut slider = crate::Slider::new(500);
    let mut damage = Region::new();
    let mut drive = |slider: &mut crate::Slider, event: tairix_input::InputEvent| {
        slider.on_pointer(&v.event_in_layout(&event), bounds, &mut damage)
    };
    let moved = |to| tairix_input::InputEvent::PointerMoved { to };
    drive(&mut slider, moved(Point::new(60, 40)));
    drive(
        &mut slider,
        tairix_input::InputEvent::PointerPressed {
            button: tairix_input::PointerButton::Primary,
        },
    );
    drive(&mut slider, moved(Point::new(140, 40)));
    assert_eq!(slider.value(), 1000, "the gutter lies past the track's end");
    drive(&mut slider, moved(Point::new(60, 95)));
    let held = slider.value();
    assert!(
        held > 0 && held < 1000,
        "below the column the drag follows x: {held}"
    );
    assert_eq!(
        drive(
            &mut slider,
            tairix_input::InputEvent::PointerReleased {
                button: tairix_input::PointerButton::Primary,
            },
        ),
        Some(crate::SliderAction::Settled { permille: held })
    );
}

/// Where `view` maps a pointer move to `at` in its layout.
fn mapped_move(view: &ScrollView, at: Point) -> Point {
    match view.event_in_layout(&tairix_input::InputEvent::PointerMoved { to: at }) {
        tairix_input::InputEvent::PointerMoved { to } => to,
        other => panic!("a move stays a move, not {other:?}"),
    }
}
