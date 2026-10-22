//! Host tests for an operation's progress window: what its events ask of the
//! operation, located through the renderer's own hit-test — never a second
//! copy of its geometry — and pressed exactly where the user would.

use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::input::{KeyInput, KeyValue, Modifiers, NamedKeyCode, PointerButtonCode};
use tairix_abi::window_ipc::{PointerAction, WindowEvent};
use tairix_browse::render::{progress_cancel_at, progress_window_extent};
use tairix_geometry::{Point, Rect, Scale};
use tairix_theme::Theme;

use super::{
    attribute_refusal, change_refusal, completion_closes_at, confirmation_may_open, create_refusal,
    empty_confirmation_may_open, item_refusal, mode_flip_allowed, move_refusal, progress_control,
    read_refusal, remove_refusal, trash_read_refusal, BarPace, ProgressControl,
    COMPLETION_DWELL_NS,
};
use tairix_abi::Errno;

/// The progress window this app is given, addressed by every synthesised
/// event.
const WINDOW_ID: u64 = 7;

/// The client of a progress window at its own extent.
fn client() -> Rect {
    let (width, height) = progress_window_extent(Scale::ONE, &Theme::dark());
    Rect::new(0, 0, width, height)
}

/// Where in `client` the Cancel button is drawn, asked of the renderer's own
/// hit-test.
fn cancel_point(client: Rect, theme: &Theme) -> Point {
    for y in 0..i32::try_from(client.height).unwrap_or(0) {
        for x in 0..i32::try_from(client.width).unwrap_or(0) {
            let point = Point::new(x, y);
            if progress_cancel_at(client, Scale::ONE, theme, point) {
                return point;
            }
        }
    }
    panic!("a progress window draws a Cancel button")
}

/// A primary press at `point`.
fn press(point: Point) -> WindowEvent {
    WindowEvent::Pointer {
        window_id: WINDOW_ID,
        x: u32::try_from(point.x).unwrap_or(0),
        y: u32::try_from(point.y).unwrap_or(0),
        action: PointerAction::Pressed(PointerButtonCode::Primary),
        modifiers: Modifiers::default(),
    }
}

/// A pointer motion to `point`.
fn motion(point: Point) -> WindowEvent {
    WindowEvent::Pointer {
        window_id: WINDOW_ID,
        x: u32::try_from(point.x).unwrap_or(0),
        y: u32::try_from(point.y).unwrap_or(0),
        action: PointerAction::Moved,
        modifiers: Modifiers::default(),
    }
}

/// A named key press with no modifiers held.
fn named(code: NamedKeyCode) -> WindowEvent {
    WindowEvent::Key {
        window_id: WINDOW_ID,
        key: KeyInput::Pressed {
            key: KeyValue::Named(code),
            modifiers: Modifiers::default(),
        },
    }
}

fn route(client: Rect, event: &WindowEvent) -> ProgressControl {
    progress_control(client, Scale::ONE, &Theme::dark(), event)
}

#[test]
fn a_press_on_the_drawn_cancel_button_cancels_the_operation() {
    let client = client();
    let drawn = cancel_point(client, &Theme::dark());
    assert_eq!(route(client, &press(drawn)), ProgressControl::Cancel);
    // A motion over the button is not a press, and changes nothing.
    assert_eq!(route(client, &motion(drawn)), ProgressControl::Ignore);
    assert_eq!(
        route(client, &press(Point::new(1, 1))),
        ProgressControl::Ignore,
        "a press off the button cancels nothing"
    );
}

#[test]
fn escape_cancels_and_nothing_else_reaches_the_run() {
    let client = client();
    assert_eq!(
        route(client, &named(NamedKeyCode::Escape)),
        ProgressControl::Cancel
    );
    assert_eq!(
        route(client, &named(NamedKeyCode::Enter)),
        ProgressControl::Ignore
    );
    assert_eq!(
        route(
            client,
            &WindowEvent::Focus {
                window_id: WINDOW_ID,
                focused: true,
            }
        ),
        ProgressControl::Ignore
    );
}

#[test]
fn a_window_too_small_for_the_button_resolves_no_cancel() {
    let tiny = Rect::new(0, 0, 1, 1);
    for point in [Point::new(0, 0), Point::new(400, 400)] {
        assert_eq!(route(tiny, &press(point)), ProgressControl::Ignore);
    }
    // Escape still stops the run whatever the window's size.
    assert_eq!(
        route(tiny, &named(NamedKeyCode::Escape)),
        ProgressControl::Cancel
    );
}

/// A new count is shown once a desktop frame, however many land in it, on
/// whatever display the desktop refreshes at.
#[test]
fn the_progress_is_shown_once_a_frame() {
    use super::progress_due;
    use tairix_abi::driver::display::RefreshRate;

    let shown = 5_000_000;
    for rate in [
        RefreshRate::ASSUMED,
        RefreshRate::from_mhz(144_000).expect("in bounds"),
    ] {
        let frame = rate.frame_ns();
        assert!(!progress_due(shown + frame - 1, shown, rate));
        assert!(progress_due(shown + frame, shown, rate));
    }
}

/// A refused create says why it was refused, and claims a taken name only for
/// a taken name: every refusal used to read as one.
#[test]
fn a_refused_create_names_its_own_reason() {
    assert_eq!(
        create_refusal(Errno::AlreadyExists),
        "a destination of that name already exists"
    );
    for other in [
        Errno::NoSpace,
        Errno::PermissionDenied,
        Errno::NotSupported,
        Errno::DeviceFault,
    ] {
        assert_ne!(
            create_refusal(other),
            create_refusal(Errno::AlreadyExists),
            "{other:?}"
        );
    }
}

/// A refusal names the item it concerned by what the user sees of it, whether
/// a walk holds its whole path or a New ▸ only its name.
#[test]
fn a_refusal_names_the_item_it_concerned() {
    let path = [
        String::from("Storage"),
        String::from("usb_25cm"),
        String::from("Notes"),
    ];
    assert_eq!(
        item_refusal("delete", &path, remove_refusal(Errno::ReadOnly)),
        "could not delete \u{201c}Notes\u{201d}: its volume is read-only"
    );
    assert_eq!(
        item_refusal(
            "make",
            &["New Folder"],
            create_refusal(Errno::PermissionDenied)
        ),
        "could not make \u{201c}New Folder\u{201d}: the destination folder may not be written to"
    );
}

/// A read-only destination is named as one, never as a permission the user
/// could be granted.
#[test]
fn a_read_only_destination_is_told_apart_from_a_denied_one() {
    assert_eq!(
        create_refusal(Errno::ReadOnly),
        "the destination volume is read-only"
    );
    assert_ne!(
        create_refusal(Errno::ReadOnly),
        create_refusal(Errno::PermissionDenied)
    );
}

/// Each refusal says what stopped it, a read-only volume apart from a missing
/// permission, and a failing device the same way whatever was being done.
#[test]
fn a_refusal_names_its_cause_and_a_failing_device_reads_alike() {
    for refusal in [
        create_refusal,
        remove_refusal,
        move_refusal,
        change_refusal,
        trash_read_refusal,
    ] {
        assert_ne!(refusal(Errno::ReadOnly), refusal(Errno::PermissionDenied));
        assert_eq!(refusal(Errno::DeviceOffline), "its device was removed");
    }
    assert_eq!(remove_refusal(Errno::ReadOnly), "its volume is read-only");
    assert_eq!(
        move_refusal(Errno::AlreadyExists),
        "an item with that name appeared at the destination"
    );
    assert_eq!(read_refusal(Errno::MediumError), "its device failed");
    assert_eq!(change_refusal(Errno::ReadOnly), "its volume is read-only");
    assert_eq!(
        change_refusal(Errno::NotSupported),
        "its volume cannot store that"
    );
    assert_eq!(
        read_refusal(Errno::OutOfRange),
        "a folder in it could not be read"
    );
}

/// A plan that comes back after the user moved on — to another selection,
/// another folder, or into something else on screen — opens no confirmation.
#[test]
fn a_delete_confirmation_opens_only_over_the_selection_it_plans() {
    let path = |name: &str| alloc::vec![String::from("home"), String::from(name)];
    let asked = tairix_browse::DeletePlan::new(alloc::vec![(path("a"), false)]).expect("a plan");
    let other = tairix_browse::DeletePlan::new(alloc::vec![(path("b"), false)]).expect("a plan");
    assert!(confirmation_may_open(&asked, Some(&asked), false));
    assert!(
        !confirmation_may_open(&asked, Some(&other), false),
        "another selection"
    );
    assert!(
        !confirmation_may_open(&asked, None, false),
        "nothing selected"
    );
    assert!(
        !confirmation_may_open(&asked, Some(&asked), true),
        "something else open"
    );
}

/// The Trash's contents come back after the user may have gone elsewhere: the
/// confirmation opens only over the Trash that was read.
#[test]
fn an_empty_confirmation_opens_only_over_the_trash_it_read() {
    let trash: Vec<String> = ["home", "Library", "Trash"].map(String::from).to_vec();
    assert!(empty_confirmation_may_open(&trash, &trash, false));
    assert!(
        !empty_confirmation_may_open(&trash, &trash[..2], false),
        "the window moved on"
    );
    assert!(
        !empty_confirmation_may_open(&trash, &trash, true),
        "something else open"
    );
}

/// The window is presented only as its count moves, so one the desktop asks
/// to draw again is drawn: a stalled operation would leave it blank.
#[test]
fn a_redraw_request_during_an_operation_is_answered() {
    let event = WindowEvent::RedrawRequested { window_id: 1 };
    assert_eq!(route(client(), &event), ProgressControl::Redraw);
}

/// A second flip taken from the mode as it was before the first landed would
/// undo the first: it waits for the node to be read again.
#[test]
fn a_permission_flip_waits_for_the_last_one_to_be_read_back() {
    assert!(mode_flip_allowed(false, false));
    assert!(!mode_flip_allowed(true, false), "in flight");
    assert!(!mode_flip_allowed(false, true), "landed, not yet read back");
}

/// A moving bar is drawn when its frame falls due and not before, so the loop
/// parks between frames rather than turning for every one it might draw.
#[test]
fn a_bar_is_drawn_when_its_frame_falls_due_and_parks_between() {
    let mut pace = BarPace::new();
    assert!(pace.due(0), "owed its first frame at once");
    pace.drawn(100, Some(16));
    assert!(!pace.due(115), "not before its frame");
    assert_eq!(pace.deadline(), Some(116), "the park ends at its frame");
    assert!(pace.due(116));
    pace.drawn(116, None);
    assert!(!pace.due(u64::MAX), "a still bar is owed nothing");
    assert_eq!(pace.deadline(), None, "and wakes nobody");
    pace.restated();
    assert!(pace.due(117), "a change is drawn at once");
}

/// A minimised window's bar is not drawn and wakes nobody; shown again, it is
/// owed a frame at once.
#[test]
fn a_bar_off_show_is_neither_drawn_nor_woken_for() {
    let mut pace = BarPace::new();
    pace.drawn(0, Some(16));
    pace.observe(&WindowEvent::Minimized {
        window_id: WINDOW_ID,
    });
    assert!(!pace.on_show());
    assert!(!pace.due(1_000));
    assert_eq!(pace.deadline(), None);
    pace.observe(&WindowEvent::Focus {
        window_id: WINDOW_ID,
        focused: true,
    });
    assert!(pace.on_show());
    assert!(pace.due(1_000), "shown again, it is drawn at once");
}

/// A completed operation's window stays until its fill has eased to full and
/// the dwell has passed, and no longer.
#[test]
fn a_completed_operation_closes_once_its_full_bar_has_been_seen() {
    let motion = Theme::dark().motion();
    let ease = u64::from(motion.duration(tairix_theme::MotionInteraction::JobProgressPulse))
        * tairix_abi::time::NANOS_PER_MILLI;
    assert_eq!(
        completion_closes_at(5, motion),
        5 + ease + COMPLETION_DWELL_NS
    );
    assert_eq!(
        completion_closes_at(u64::MAX - 1, motion),
        u64::MAX,
        "late on the clock it saturates rather than wrapping to now"
    );
}

#[test]
fn attributes_the_source_withheld_are_told_from_ones_the_copy_refused() {
    assert_eq!(
        attribute_refusal(&tairix_fscopy::AttrError::Read(Errno::PermissionDenied)),
        "the source's extended attributes could not be read"
    );
    let refused = tairix_fscopy::AttrError::Write {
        key: String::from("user.k"),
        error: Errno::Busy,
    };
    assert_eq!(
        attribute_refusal(&refused),
        "a copy's extended attributes could not be set"
    );
    assert_eq!(
        attribute_refusal(&tairix_fscopy::AttrError::Read(Errno::DeviceOffline)),
        "its device was removed"
    );
}
