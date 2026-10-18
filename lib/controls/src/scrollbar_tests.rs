//! Unit tests for the scrollbar renderer (spec §11.28–§11.30, §20 checklist).
//!
//! These cover the one orientation-parameterized behaviour on both axes: thumb
//! math, the preserved drag anchor and mid-drag re-clamp, end-button line
//! steps, track paging, press-and-hold repeat, orientation-aware keys, the
//! wheel, the spec §13 denied/disabled fail-closed treatment, dark/light and
//! high-contrast rendering, the focus ring, the render-equivalence repaint
//! gate, and the fail-closed degenerate/non-scrollable cases.

use tairix_geometry::{Point, Rect, Scale};
use tairix_input::{InputEvent, Key, NamedKey, PointerButton};
use tairix_raster::Surface;
use tairix_theme::Theme;

use tairix_abi::driver::input::SCROLL_UNITS_PER_DETENT;

use crate::damage::sink;
use crate::scroll::{
    ScrollModel, ScrollOrientation, ScrollRange, REPEAT_DELAY_NS, REPEAT_INTERVAL_NS, WHEEL_STEP,
};
use crate::scrollbar::{ScrollAction, ScrollBar, ScrollPart};
use crate::state::{AuthorityState, ControlState};
use crate::testkit::{has_pixel, high_contrast, premul};

const VW: u32 = 16;
const VH: u32 = 300;

fn model() -> ScrollModel {
    ScrollModel::new(ScrollRange::new(1000, 300, 0), 10, 100)
}

fn vbar() -> ScrollBar {
    ScrollBar::new(ScrollOrientation::Vertical, model())
}

fn hbar() -> ScrollBar {
    ScrollBar::new(ScrollOrientation::Horizontal, model())
}

fn vbounds() -> Rect {
    Rect::new(0, 0, VW, VH)
}

/// The same bar laid along the other axis: the vertical extents transposed.
fn hbounds() -> Rect {
    Rect::new(0, 0, VH, VW)
}

fn theme() -> Theme {
    Theme::dark()
}

const PRESS: InputEvent = InputEvent::PointerPressed {
    button: PointerButton::Primary,
};
const RELEASE: InputEvent = InputEvent::PointerReleased {
    button: PointerButton::Primary,
};

fn moved(x: i32, y: i32) -> InputEvent {
    InputEvent::PointerMoved {
        to: Point::new(x, y),
    }
}

/// The requested offset from an action, if any.
fn off(action: Option<ScrollAction>) -> Option<u64> {
    action.map(|ScrollAction::ScrollTo { offset }| offset)
}

fn render(bar: &ScrollBar, bounds: Rect, theme: &Theme) -> Surface {
    let mut surface = Surface::new(bounds.width, bounds.height).expect("surface");
    bar.render(&mut surface, bounds, Scale::ONE, theme);
    surface
}

/// One wheel detent's worth of scroll units.
const DETENT: i32 = SCROLL_UNITS_PER_DETENT;

/// How far one detent scrolls at the reference density.
const STEP: u64 = WHEEL_STEP as u64;

#[test]
fn both_orientations_are_one_behaviour() {
    // The same model wheeled the same turn moves identically on either axis —
    // the bars are one component parameterised by orientation.
    let mut v = vbar();
    let mut h = hbar();
    assert_eq!(
        off(v.wheel(0, 3 * DETENT, Scale::ONE, vbounds(), &mut sink())),
        Some(3 * STEP)
    );
    assert_eq!(
        off(h.wheel(3 * DETENT, 0, Scale::ONE, hbounds(), &mut sink())),
        Some(3 * STEP)
    );
    assert_eq!(v.model().offset(), h.model().offset());
}

#[test]
fn a_detent_scrolls_the_wheel_step_whatever_the_line_and_clamps() {
    let mut bar = vbar();
    assert_eq!(
        off(bar.wheel(0, DETENT, Scale::ONE, vbounds(), &mut sink())),
        Some(STEP)
    );
    // A cross-axis turn does nothing to a vertical bar.
    assert_eq!(
        bar.wheel(DETENT, 0, Scale::ONE, vbounds(), &mut sink()),
        None
    );
    // Scrolling back past the start clamps at zero, then stops changing.
    assert_eq!(
        off(bar.wheel(0, -100 * DETENT, Scale::ONE, vbounds(), &mut sink())),
        Some(0)
    );
    assert_eq!(
        bar.wheel(0, -DETENT, Scale::ONE, vbounds(), &mut sink()),
        None
    );
}

#[test]
fn a_detent_scrolls_further_at_a_denser_scale() {
    let mut bar = vbar();
    let double = Scale::from_percent(200).expect("a supported density");
    assert_eq!(
        off(bar.wheel(0, DETENT, double, vbounds(), &mut sink())),
        Some(2 * STEP)
    );
}

#[test]
fn scroll_short_of_a_pixel_is_carried_to_the_next_turn() {
    // One unit is 48/120 of a pixel: nothing moves until the carried turn
    // makes a whole one, and none of it is lost on the way.
    let mut bar = vbar();
    for _ in 0..2 {
        assert_eq!(bar.wheel(0, 1, Scale::ONE, vbounds(), &mut sink()), None);
    }
    assert_eq!(
        off(bar.wheel(0, 1, Scale::ONE, vbounds(), &mut sink())),
        Some(1)
    );
    for _ in 0..2 {
        bar.wheel(0, 1, Scale::ONE, vbounds(), &mut sink());
    }
    assert_eq!(
        bar.model().offset(),
        2,
        "five units carried to two whole pixels"
    );
}

#[test]
fn a_reversal_drops_what_the_last_turn_carried() {
    let mut bar = ScrollBar::new(
        ScrollOrientation::Vertical,
        ScrollModel::new(ScrollRange::new(1000, 300, 100), 10, 100),
    );
    assert_eq!(bar.wheel(0, 2, Scale::ONE, vbounds(), &mut sink()), None);
    // Kept, the downward carry would swallow this upward turn entirely.
    assert_eq!(
        off(bar.wheel(0, -3, Scale::ONE, vbounds(), &mut sink())),
        Some(99)
    );
}

#[test]
fn a_wheel_over_the_bar_itself_scrolls_it_and_one_beside_it_does_not() {
    let theme = theme();
    let mut bar = vbar();
    let turn = InputEvent::PointerScrolled { dx: 0, dy: DETENT };
    bar.on_pointer(&moved(4, 150), vbounds(), Scale::ONE, &theme, &mut sink());
    assert_eq!(
        off(bar.on_pointer(&turn, vbounds(), Scale::ONE, &theme, &mut sink())),
        Some(STEP)
    );
    bar.on_pointer(&moved(40, 150), vbounds(), Scale::ONE, &theme, &mut sink());
    assert_eq!(
        bar.on_pointer(&turn, vbounds(), Scale::ONE, &theme, &mut sink()),
        None
    );
}

#[test]
fn keys_step_line_page_and_bounds_when_focused() {
    let mut bar = vbar();
    bar.set_focused(true);
    assert_eq!(
        off(bar.on_key(Key::Named(NamedKey::Down), vbounds(), &mut sink())),
        Some(10)
    );
    assert_eq!(
        off(bar.on_key(Key::Named(NamedKey::PageDown), vbounds(), &mut sink())),
        Some(110)
    );
    assert_eq!(
        off(bar.on_key(Key::Named(NamedKey::End), vbounds(), &mut sink())),
        Some(700)
    );
    assert_eq!(
        off(bar.on_key(Key::Named(NamedKey::PageUp), vbounds(), &mut sink())),
        Some(600)
    );
    assert_eq!(
        off(bar.on_key(Key::Named(NamedKey::Home), vbounds(), &mut sink())),
        Some(0)
    );
    // At the start, another line back does nothing (fail-closed bounds).
    assert_eq!(
        bar.on_key(Key::Named(NamedKey::Up), vbounds(), &mut sink()),
        None
    );
}

#[test]
fn horizontal_bar_uses_left_right_not_up_down() {
    let mut bar = hbar();
    bar.set_focused(true);
    assert_eq!(
        bar.on_key(Key::Named(NamedKey::Down), hbounds(), &mut sink()),
        None
    );
    assert_eq!(
        off(bar.on_key(Key::Named(NamedKey::Right), hbounds(), &mut sink())),
        Some(10)
    );
    assert_eq!(
        off(bar.on_key(Key::Named(NamedKey::Left), hbounds(), &mut sink())),
        Some(0)
    );
}

#[test]
fn unfocused_bar_ignores_keys() {
    let mut bar = vbar();
    assert_eq!(
        bar.on_key(Key::Named(NamedKey::Down), vbounds(), &mut sink()),
        None
    );
    assert_eq!(bar.model().offset(), 0);
}

#[test]
fn end_button_press_steps_one_line() {
    let theme = theme();
    let mut bar = vbar();
    bar.set_model(bar.model().scroll_to(500));
    // Decrement button at the top.
    assert_eq!(
        off(bar.on_pointer(&PRESS, vbounds(), Scale::ONE, &theme, &mut sink())),
        Some(490)
    );
    bar.on_pointer(&RELEASE, vbounds(), Scale::ONE, &theme, &mut sink());
    // Increment button at the bottom.
    bar.on_pointer(&moved(2, 290), vbounds(), Scale::ONE, &theme, &mut sink());
    assert_eq!(
        off(bar.on_pointer(&PRESS, vbounds(), Scale::ONE, &theme, &mut sink())),
        Some(500)
    );
}

#[test]
fn track_press_pages_toward_the_pointer() {
    let theme = theme();
    let mut bar = vbar();
    // At offset 0 the thumb sits at the track start; a press well below it in
    // the after-thumb region pages forward.
    bar.on_pointer(&moved(2, 200), vbounds(), Scale::ONE, &theme, &mut sink());
    assert_eq!(
        off(bar.on_pointer(&PRESS, vbounds(), Scale::ONE, &theme, &mut sink())),
        Some(100)
    );
}

#[test]
fn thumb_drag_preserves_anchor_and_does_not_jump_on_grab() {
    let theme = theme();
    let mut bar = vbar();
    // Press on the thumb (near the track start at offset 0): no jump.
    bar.on_pointer(&moved(2, 20), vbounds(), Scale::ONE, &theme, &mut sink());
    assert_eq!(
        bar.on_pointer(&PRESS, vbounds(), Scale::ONE, &theme, &mut sink()),
        None
    );
    assert_eq!(bar.model().offset(), 0);
    // Dragging down moves the offset forward, clamped within the range.
    let a = off(bar.on_pointer(&moved(2, 150), vbounds(), Scale::ONE, &theme, &mut sink()));
    let moved_to = a.expect("drag moved");
    assert!(moved_to > 0 && moved_to <= 700);
    bar.on_pointer(&RELEASE, vbounds(), Scale::ONE, &theme, &mut sink());
}

#[test]
fn mid_drag_range_shrink_stays_valid() {
    let theme = theme();
    let mut bar = vbar();
    bar.on_pointer(&moved(2, 20), vbounds(), Scale::ONE, &theme, &mut sink());
    bar.on_pointer(&PRESS, vbounds(), Scale::ONE, &theme, &mut sink());
    // The content shrinks mid-drag; the bar recomputes from the new range.
    bar.set_model(ScrollModel::new(ScrollRange::new(400, 300, 0), 10, 100));
    let dragged = off(bar.on_pointer(&moved(2, 290), vbounds(), Scale::ONE, &theme, &mut sink()));
    let value = dragged.unwrap_or_else(|| bar.model().offset());
    assert!(
        value <= 100,
        "offset {value} must stay within the new max 100"
    );
}

#[test]
fn denied_bar_keeps_position_and_ignores_input() {
    let theme = theme();
    let mut bar = vbar();
    bar.set_model(bar.model().scroll_to(200));
    bar.set_focused(true);
    bar.set_state(ControlState::idle().with_authority(AuthorityState::Denied));
    assert_eq!(
        bar.on_pointer(&PRESS, vbounds(), Scale::ONE, &theme, &mut sink()),
        None
    );
    assert_eq!(
        bar.on_key(Key::Named(NamedKey::Down), vbounds(), &mut sink()),
        None
    );
    assert_eq!(
        bar.wheel(0, 3 * DETENT, Scale::ONE, vbounds(), &mut sink()),
        None
    );
    assert_eq!(bar.model().offset(), 200);
    // It renders the denied colour, distinct from a disabled look.
    let surface = render(&bar, vbounds(), &theme);
    assert!(has_pixel(&surface, premul(theme.palette().denied)));
}

#[test]
fn disabled_bar_ignores_input() {
    let theme = theme();
    let mut bar = vbar();
    bar.set_focused(true);
    bar.set_state(ControlState::disabled());
    assert_eq!(
        bar.on_pointer(&PRESS, vbounds(), Scale::ONE, &theme, &mut sink()),
        None
    );
    assert_eq!(
        bar.on_key(Key::Named(NamedKey::Down), vbounds(), &mut sink()),
        None
    );
    assert_eq!(
        bar.wheel(0, 3 * DETENT, Scale::ONE, vbounds(), &mut sink()),
        None
    );
}

#[test]
fn part_at_classifies_every_region() {
    let theme = theme();
    let mut bar = vbar();
    let s = Scale::ONE;
    assert_eq!(
        bar.part_at(vbounds(), Point::new(2, 2), s, &theme),
        ScrollPart::Decrement
    );
    assert_eq!(
        bar.part_at(vbounds(), Point::new(2, 290), s, &theme),
        ScrollPart::Increment
    );
    assert_eq!(
        bar.part_at(vbounds(), Point::new(2, 20), s, &theme),
        ScrollPart::Thumb
    );
    assert_eq!(
        bar.part_at(vbounds(), Point::new(2, 200), s, &theme),
        ScrollPart::TrackAfter
    );
    // A point off the bar entirely.
    assert_eq!(
        bar.part_at(vbounds(), Point::new(100, 5), s, &theme),
        ScrollPart::Outside
    );
    // With the thumb scrolled to the end, a point above it is the before region.
    bar.set_model(bar.model().to_end());
    assert_eq!(
        bar.part_at(vbounds(), Point::new(2, 100), s, &theme),
        ScrollPart::TrackBefore
    );
}

/// Every rectangle `part_rect` reports is classified as that part at each of
/// its corners, the parts tile the bar end to end on both axes, and a part the
/// thumb leaves no room for — or the outside — has no rectangle at all.
#[test]
fn part_rect_is_the_forward_mirror_of_part_at() {
    let theme = theme();
    let s = Scale::ONE;
    let parts = [
        ScrollPart::Decrement,
        ScrollPart::TrackBefore,
        ScrollPart::Thumb,
        ScrollPart::TrackAfter,
        ScrollPart::Increment,
    ];
    for (bar, bounds, long) in [(vbar(), vbounds(), VH), (hbar(), hbounds(), VH)] {
        let mut bar = bar;
        bar.set_model(bar.model().scroll_to(400));
        let mut covered = 0;
        for part in parts {
            let rect = bar
                .part_rect(part, bounds, s, &theme)
                .unwrap_or_else(|| panic!("{part:?} is drawn mid-scroll"));
            let (right, bottom) = (rect.right() - 1, rect.bottom() - 1);
            for corner in [
                Point::new(rect.left(), rect.top()),
                Point::new(right, rect.top()),
                Point::new(rect.left(), bottom),
                Point::new(right, bottom),
            ] {
                assert_eq!(bar.part_at(bounds, corner, s, &theme), part, "{corner:?}");
            }
            covered += match bar.orientation() {
                ScrollOrientation::Vertical => rect.height,
                ScrollOrientation::Horizontal => rect.width,
            };
        }
        assert_eq!(covered, long, "the parts tile the bar end to end");
        assert_eq!(bar.part_rect(ScrollPart::Outside, bounds, s, &theme), None);

        bar.set_model(bar.model().to_end());
        assert_eq!(
            bar.part_rect(ScrollPart::TrackAfter, bounds, s, &theme),
            None,
            "a thumb at the end leaves no track after it"
        );
    }
}

/// Step the held press of `bar` at `now_ns`, answering the offset it asked for.
fn repeat_at(bar: &mut ScrollBar, now_ns: u64) -> Option<u64> {
    off(bar.repeat(now_ns, vbounds(), Scale::ONE, &theme(), &mut sink()))
}

#[test]
fn press_hold_repeat_steps_on_its_own_cadence_and_stops_at_a_bound() {
    let theme = theme();
    let mut bar = vbar();
    bar.set_model(bar.model().scroll_to(25));
    bar.on_pointer(&moved(5, 5), vbounds(), Scale::ONE, &theme, &mut sink());
    bar.on_pointer(&PRESS, vbounds(), Scale::ONE, &theme, &mut sink()); // decrement, held
    assert_eq!(bar.held(), Some(ScrollPart::Decrement));
    assert_eq!(bar.model().offset(), 15, "the press itself steps");

    let start = 1_000;
    let first = start + REPEAT_DELAY_NS;
    assert_eq!(
        bar.repeat_deadline(start),
        Some(first),
        "armed by the press"
    );
    assert_eq!(bar.repeat_deadline(start + 5), Some(first), "and only once");
    assert_eq!(repeat_at(&mut bar, first - 1), None, "not before it is due");
    assert_eq!(repeat_at(&mut bar, first), Some(5));
    assert_eq!(
        bar.repeat_deadline(first),
        Some(first + REPEAT_INTERVAL_NS),
        "then at the repeat interval"
    );
    // The next repeat reaches the start, after which nothing is owed.
    assert_eq!(repeat_at(&mut bar, first + REPEAT_INTERVAL_NS), Some(0));
    assert_eq!(bar.repeat_deadline(first + REPEAT_INTERVAL_NS), None);
    assert_eq!(repeat_at(&mut bar, first + 2 * REPEAT_INTERVAL_NS), None);
    // Releasing clears the held part.
    bar.on_pointer(&RELEASE, vbounds(), Scale::ONE, &theme, &mut sink());
    assert_eq!(bar.held(), None);
    assert_eq!(bar.repeat_deadline(0), None);
}

#[test]
fn a_held_step_goes_on_only_while_the_pointer_stays_on_its_button() {
    let theme = theme();
    let mut bar = vbar();
    let on_increment = (5, i32::try_from(VH).expect("small") - 5);
    bar.on_pointer(
        &moved(on_increment.0, on_increment.1),
        vbounds(),
        Scale::ONE,
        &theme,
        &mut sink(),
    );
    bar.on_pointer(&PRESS, vbounds(), Scale::ONE, &theme, &mut sink());
    assert_eq!(bar.held(), Some(ScrollPart::Increment));
    assert!(bar.repeat_deadline(0).is_some());

    bar.on_pointer(&moved(5, 150), vbounds(), Scale::ONE, &theme, &mut sink());
    assert_eq!(
        bar.repeat_deadline(0),
        None,
        "slid off the button, it stops"
    );
    assert_eq!(repeat_at(&mut bar, u64::MAX), None);

    bar.on_pointer(
        &moved(on_increment.0, on_increment.1),
        vbounds(),
        Scale::ONE,
        &theme,
        &mut sink(),
    );
    assert!(
        bar.repeat_deadline(0).is_some(),
        "and goes on once back on it"
    );
}

#[test]
fn a_held_track_press_pages_until_the_thumb_reaches_the_pointer() {
    let theme = theme();
    let mut bar = vbar();
    let target = i32::try_from(VH * 2 / 3).expect("small");
    bar.on_pointer(
        &moved(5, target),
        vbounds(),
        Scale::ONE,
        &theme,
        &mut sink(),
    );
    bar.on_pointer(&PRESS, vbounds(), Scale::ONE, &theme, &mut sink());
    assert_eq!(bar.held(), Some(ScrollPart::TrackAfter));
    let mut now = 0;
    let mut steps = 0;
    while let Some(due) = bar.repeat_deadline(now) {
        now = due;
        repeat_at(&mut bar, now);
        steps += 1;
        assert!(steps < 32, "a track press that never stops");
    }
    assert_eq!(
        bar.part_at(vbounds(), Point::new(5, target), Scale::ONE, &theme),
        ScrollPart::Thumb,
        "the thumb came to rest under the pointer"
    );
    assert!(
        bar.model().offset() < bar.model().range().max_offset(),
        "rather than paging on to the end"
    );
}

#[test]
fn a_press_toward_an_end_already_reached_presses_nothing() {
    let theme = theme();
    let mut bar = vbar();
    bar.on_pointer(&moved(5, 5), vbounds(), Scale::ONE, &theme, &mut sink());
    let mut damage = sink();
    assert_eq!(
        bar.on_pointer(&PRESS, vbounds(), Scale::ONE, &theme, &mut damage),
        None
    );
    assert_eq!(bar.held(), None, "nothing is held");
    assert!(damage.is_empty(), "and nothing looks pressed");
    assert_eq!(bar.repeat_deadline(0), None, "or owes a repeat");
}

#[test]
fn a_release_ends_the_press_even_once_the_bar_has_collapsed() {
    let theme = theme();
    let mut bar = vbar();
    bar.on_pointer(&moved(5, 290), vbounds(), Scale::ONE, &theme, &mut sink());
    bar.on_pointer(&PRESS, vbounds(), Scale::ONE, &theme, &mut sink());
    assert!(bar.is_pressing());
    let collapsed = Rect::new(0, 0, 0, 0);
    bar.on_pointer(&RELEASE, collapsed, Scale::ONE, &theme, &mut sink());
    assert!(!bar.is_pressing());
    assert_eq!(bar.repeat_deadline(0), None);
}

/// A press the desktop took — the lock, a prompt, a menu — ends where it
/// stands: a held step stops, and a dragged thumb stays where it was left.
#[test]
fn a_cancelled_press_ends_without_moving_anything() {
    let theme = theme();
    let mut bar = vbar();
    bar.on_pointer(&moved(5, 290), vbounds(), Scale::ONE, &theme, &mut sink());
    bar.on_pointer(&PRESS, vbounds(), Scale::ONE, &theme, &mut sink());
    let held = bar.model().offset();
    bar.on_pointer(
        &InputEvent::PointerCancelled,
        vbounds(),
        Scale::ONE,
        &theme,
        &mut sink(),
    );
    assert_eq!(bar.held(), None);
    assert_eq!(bar.repeat_deadline(0), None);
    assert_eq!(bar.model().offset(), held);

    let thumb = bar
        .part_rect(ScrollPart::Thumb, vbounds(), Scale::ONE, &theme)
        .expect("a thumb");
    let grip = thumb.center();
    bar.on_pointer(
        &moved(grip.x, grip.y),
        vbounds(),
        Scale::ONE,
        &theme,
        &mut sink(),
    );
    bar.on_pointer(&PRESS, vbounds(), Scale::ONE, &theme, &mut sink());
    bar.on_pointer(
        &moved(grip.x, grip.y + 40),
        vbounds(),
        Scale::ONE,
        &theme,
        &mut sink(),
    );
    let dragged = bar.model().offset();
    assert!(bar.is_pressing());
    assert_eq!(
        bar.on_pointer(
            &InputEvent::PointerCancelled,
            vbounds(),
            Scale::ONE,
            &theme,
            &mut sink()
        ),
        None
    );
    assert!(!bar.is_pressing());
    bar.on_pointer(
        &moved(grip.x, grip.y + 120),
        vbounds(),
        Scale::ONE,
        &theme,
        &mut sink(),
    );
    assert_eq!(bar.model().offset(), dragged, "the drag is over");
}

/// Each end button is a plate raised in the channel with its chevron across
/// it, in the theme's foreground, so the arrows read at a glance; one toward
/// an end the content already rests at draws no plate.
#[test]
fn an_end_button_is_a_raised_plate_unless_there_is_nothing_that_way() {
    let theme = theme();
    let mut bar = vbar();
    bar.set_model(bar.model().scroll_to(300));
    let surface = render(&bar, vbounds(), &theme);
    let button = bar
        .part_rect(ScrollPart::Decrement, vbounds(), Scale::ONE, &theme)
        .expect("a decrement button");
    let inside = |surface: &Surface, colour| {
        (0..button.height)
            .any(|y| (0..button.width).any(|x| surface.get(x, y) == Some(premul(colour))))
    };
    let palette = theme.palette();
    assert!(inside(&surface, palette.scroll_thumb), "a plate");
    assert!(inside(&surface, palette.on_surface), "a readable chevron");

    bar.set_model(bar.model().to_start());
    let at_start = render(&bar, vbounds(), &theme);
    assert!(
        !inside(&at_start, palette.scroll_thumb),
        "no plate at the start"
    );
    assert!(
        !inside(&at_start, palette.on_surface),
        "and a dimmed chevron"
    );
}

#[test]
fn render_draws_the_channel_and_an_idle_thumb() {
    let theme = theme();
    let bar = vbar();
    let surface = render(&bar, vbounds(), &theme);
    assert!(has_pixel(&surface, premul(theme.palette().scroll_track)));
    assert!(has_pixel(&surface, premul(theme.palette().scroll_thumb)));
}

/// An awake bar's thumb, and an end button hovered or held, take the neutral
/// active grey: the accent is the selection's, and a bar in use is handled,
/// not selected.
#[test]
fn an_awake_bar_takes_the_active_grey_never_the_accent() {
    let theme = theme();
    let palette = theme.palette();
    let mut bar = vbar();
    bar.set_model(bar.model().scroll_to(300));
    bar.on_pointer(&moved(2, 200), vbounds(), Scale::ONE, &theme, &mut sink());
    let surface = render(&bar, vbounds(), &theme);
    assert!(has_pixel(&surface, premul(palette.scroll_thumb_active)));
    assert!(!has_pixel(&surface, premul(palette.rim_active)));
    assert!(!has_pixel(&surface, premul(palette.accent)));

    let button = bar
        .part_rect(ScrollPart::Decrement, vbounds(), Scale::ONE, &theme)
        .expect("a decrement button");
    let centre = button.center();
    bar.on_pointer(
        &moved(centre.x, centre.y),
        vbounds(),
        Scale::ONE,
        &theme,
        &mut sink(),
    );
    let hovered = render(&bar, vbounds(), &theme);
    let on_button = |surface: &Surface, colour| {
        (0..button.height)
            .any(|y| (0..button.width).any(|x| surface.get(x, y) == Some(premul(colour))))
    };
    assert!(
        on_button(&hovered, palette.scroll_thumb_active),
        "a hovered end button lights grey"
    );
    assert!(
        on_button(&hovered, palette.on_surface),
        "its chevron stays the plain foreground"
    );
    assert!(!has_pixel(&hovered, premul(palette.rim_active)));
}

/// The channel's long sides carry a one-pixel edge in the frame colour, so the
/// bar's extent reads against any ground; its ends do not.
#[test]
fn the_channel_is_edged_along_both_long_sides() {
    let theme = theme();
    let frame = premul(theme.palette().frame);
    let surface = render(&vbar(), vbounds(), &theme);
    let (w, h) = (surface.width(), surface.height());
    for y in 0..h {
        assert_eq!(surface.get(0, y), Some(frame), "left edge at row {y}");
        assert_eq!(surface.get(w - 1, y), Some(frame), "right edge at row {y}");
    }
    let middle = w / 2;
    assert_ne!(
        surface.get(middle, h / 2),
        Some(frame),
        "the channel is not filled with it"
    );
    assert_ne!(
        surface.get(1, h / 2),
        Some(frame),
        "the edge is one pixel wide"
    );
}

#[test]
fn a_focused_bar_draws_a_focus_ring() {
    let theme = theme();
    let mut bar = vbar();
    bar.set_focused(true);
    let surface = render(&bar, vbounds(), &theme);
    // The reactive rim appears on the outermost edge (the focus outline).
    assert_eq!(surface.get(0, 0), Some(premul(theme.palette().rim_active)));
}

#[test]
fn high_contrast_rims_the_thumb() {
    let theme = high_contrast();
    let bar = vbar();
    let surface = render(&bar, vbounds(), &theme);
    assert!(has_pixel(&surface, premul(theme.palette().on_surface)));
}

#[test]
fn dark_and_light_thumbs_differ() {
    let dark = Theme::dark();
    let light = Theme::light();
    let bar = vbar();
    assert!(has_pixel(
        &render(&bar, vbounds(), &dark),
        premul(dark.palette().scroll_thumb)
    ));
    assert!(has_pixel(
        &render(&bar, vbounds(), &light),
        premul(light.palette().scroll_thumb)
    ));
    assert_ne!(
        premul(dark.palette().scroll_thumb),
        premul(light.palette().scroll_thumb)
    );
}

#[test]
fn degenerate_bounds_never_panic_and_never_move() {
    let theme = theme();
    let mut bar = vbar();
    let zero = Rect::new(0, 0, 0, 0);
    // Rendering a zero surface is a no-op; input yields nothing.
    let mut surface = Surface::new(1, 1).expect("surface");
    bar.render(&mut surface, zero, Scale::ONE, &theme);
    assert_eq!(
        bar.on_pointer(&PRESS, zero, Scale::ONE, &theme, &mut sink()),
        None
    );
    assert_eq!(
        bar.part_at(zero, Point::ORIGIN, Scale::ONE, &theme),
        ScrollPart::Outside
    );
    assert_eq!(bar.model().offset(), 0);
}

#[test]
fn a_non_scrollable_bar_has_a_non_draggable_full_thumb() {
    let theme = theme();
    let mut bar = ScrollBar::new(
        ScrollOrientation::Vertical,
        ScrollModel::new(ScrollRange::new(200, 300, 0), 10, 100),
    );
    let geometry = bar
        .geometry(vbounds(), Scale::ONE, &theme)
        .expect("geometry");
    assert!(!geometry.draggable());
    // A press on the "thumb" starts no drag and the wheel cannot move it.
    bar.on_pointer(&moved(2, 100), vbounds(), Scale::ONE, &theme, &mut sink());
    assert_eq!(
        bar.on_pointer(&PRESS, vbounds(), Scale::ONE, &theme, &mut sink()),
        None
    );
    assert_eq!(
        bar.wheel(0, 3 * DETENT, Scale::ONE, vbounds(), &mut sink()),
        None
    );
    assert_eq!(bar.model().offset(), 0);
}

#[test]
fn scale_floors_the_thumb_at_the_scaled_minimum() {
    let theme = theme();
    // A tiny viewport fraction: the proportional thumb is below the minimum, so
    // the minimum (a theme metric) floors it — and scaling the metric up
    // lengthens the thumb.
    let bar = ScrollBar::new(
        ScrollOrientation::Vertical,
        ScrollModel::new(ScrollRange::new(100_000, 300, 0), 10, 100),
    );
    let one = bar
        .geometry(vbounds(), Scale::ONE, &theme)
        .expect("1x")
        .thumb()
        .length;
    let two = bar
        .geometry(vbounds(), Scale::from_percent(200).expect("2x"), &theme)
        .expect("2x")
        .thumb()
        .length;
    assert!(
        two > one,
        "scaled min thumb {two} must exceed unscaled {one}"
    );
}

// --- Render-equivalence equality (the host's repaint gate) ----------------

#[test]
fn the_drag_anchor_alone_never_changes_a_scrollbar_render() {
    let theme = theme();
    // Two bars grab the thumb at different points, so each keeps a different
    // grab offset within it, then both release and come to rest on the same
    // point. Everything a reader can see — the offset, the hover, the held
    // part — now matches; only the anchor differs, and it is consulted solely
    // while a drag is in flight.
    let grab = |y: i32| {
        let mut bar = vbar();
        bar.on_pointer(&moved(2, y), vbounds(), Scale::ONE, &theme, &mut sink());
        bar.on_pointer(&PRESS, vbounds(), Scale::ONE, &theme, &mut sink());
        bar.on_pointer(&RELEASE, vbounds(), Scale::ONE, &theme, &mut sink());
        bar.on_pointer(&moved(2, 40), vbounds(), Scale::ONE, &theme, &mut sink());
        bar
    };
    let near = grab(20);
    let far = grab(30);

    assert_eq!(
        near.model().offset(),
        far.model().offset(),
        "a grab inside the thumb must not move the content"
    );
    // Both presses really do capture a different grab offset: while a drag is
    // live the anchor decides where the same pointer lands.
    let drag_from = |y: i32| {
        let mut bar = vbar();
        bar.on_pointer(&moved(2, y), vbounds(), Scale::ONE, &theme, &mut sink());
        bar.on_pointer(&PRESS, vbounds(), Scale::ONE, &theme, &mut sink());
        off(bar.on_pointer(&moved(2, 150), vbounds(), Scale::ONE, &theme, &mut sink()))
    };
    assert_ne!(
        drag_from(20),
        drag_from(30),
        "both presses must land inside the thumb, or this proves nothing"
    );
    assert_eq!(near, far, "the drag anchor is not a drawn property");
    assert_eq!(
        render(&near, vbounds(), &theme).pixels(),
        render(&far, vbounds(), &theme).pixels(),
        "…and the two must therefore paint identically"
    );
}

#[test]
fn a_move_within_one_part_leaves_a_scrollbar_equal() {
    let theme = theme();
    // A host feeds every pointer sample to every control it holds, so a bar
    // must not report a change for a sample that lands on a new coordinate
    // without changing which part is beneath it — off the bar entirely, or
    // further along the same region. Reporting one would repaint an unchanged
    // surface on every mouse move.
    let settle = |x: i32, y: i32| {
        let mut bar = vbar();
        bar.on_pointer(&moved(x, y), vbounds(), Scale::ONE, &theme, &mut sink());
        bar
    };
    for (from, to, region) in [
        ((200, 400), (205, 401), "clear of the bar"),
        ((2, 2), (3, 4), "the decrement button"),
        ((2, 150), (3, 170), "the track after the thumb"),
    ] {
        let before = settle(from.0, from.1);
        let after = settle(to.0, to.1);
        assert_eq!(
            before, after,
            "two samples within {region} draw the same pixels"
        );
        assert_eq!(
            render(&before, vbounds(), &theme).pixels(),
            render(&after, vbounds(), &theme).pixels(),
            "…and must therefore paint identically within {region}"
        );
    }
}

#[test]
fn the_part_under_the_pointer_is_a_drawn_property() {
    let theme = theme();
    // The end button beneath the pointer brightens, so *which* part the
    // pointer sits over does compare — excluding it would let the gate pass a
    // bar that paints a lit chevron off as one that does not. The increment,
    // because at the start there is nothing above for the decrement to light.
    let foot = i32::try_from(VH).expect("small") - 2;
    let mut on_end = vbar();
    on_end.on_pointer(&moved(2, foot), vbounds(), Scale::ONE, &theme, &mut sink());
    let mut on_track = vbar();
    on_track.on_pointer(&moved(2, 150), vbounds(), Scale::ONE, &theme, &mut sink());

    assert_ne!(
        on_end, on_track,
        "a lit increment chevron is a different composition"
    );
    assert_ne!(
        render(&on_end, vbounds(), &theme).pixels(),
        render(&on_track, vbounds(), &theme).pixels(),
        "…and the two must therefore paint differently"
    );
}

/// A release clears the drag latch and the pressed look the bar draws, so it
/// repaints even though the pointer has not moved.
#[test]
fn a_release_reports_the_bar_it_wakes_from() {
    let theme = theme();
    let mut bar = vbar();
    let thumb = moved(8, 20);
    bar.on_pointer(&thumb, vbounds(), Scale::ONE, &theme, &mut sink());
    bar.on_pointer(&PRESS, vbounds(), Scale::ONE, &theme, &mut sink());
    assert!(bar.is_pressing(), "the thumb press captured the drag");

    let mut damage = sink();
    bar.on_pointer(&RELEASE, vbounds(), Scale::ONE, &theme, &mut damage);
    assert_eq!(
        damage.bounds(),
        vbounds(),
        "the whole bar: its awake look is not confined to one part"
    );
}

/// A turn that cannot move the offset reports nothing.
#[test]
fn a_wheel_at_the_end_reports_nothing() {
    let mut bar = vbar();
    // Park the offset at the end through the owner's setter, the way a viewport
    // that has scrolled to its bottom refreshes the bar.
    bar.set_model(bar.model().to_end());

    let mut damage = sink();
    assert_eq!(
        off(bar.wheel(0, DETENT, Scale::ONE, vbounds(), &mut damage)),
        None
    );
    assert!(damage.is_empty(), "the thumb is already at the end");
}
