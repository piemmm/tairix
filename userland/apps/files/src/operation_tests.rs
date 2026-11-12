//! Host tests for an operation's progress window: what its events ask of the
//! operation, located through the renderer's own hit-test — never a second
//! copy of its geometry — and pressed exactly where the user would.

use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::input::{KeyInput, KeyValue, Modifiers, NamedKeyCode, PointerButtonCode};
use tairix_abi::window_ipc::{PointerAction, WindowEvent};
use tairix_browse::render::{
    progress_cancel_at, progress_window_extent, ClashAnswer, ClashChoice, ClashPanel,
};
use tairix_browse::Clash;
use tairix_geometry::{Point, Rect, Region, Scale};
use tairix_theme::Theme;

use super::{
    change_refusal, clash_of, completion_closes_at, create_refusal, decision_of,
    delete_confirmation, destination_refusal, empty_confirmation, item_refusal, mode_flip_allowed,
    move_refusal, operation_refusal, progress_control, reach_refusal, read_refusal, remove_refusal,
    trash_read_refusal, write_refusal, BarPace, Confirmation, ProgressControl, COMPLETION_DWELL_NS,
};
use tairix_abi::Errno;
use tairix_fscopy::op::{Conflict, Decision, OpError};
use tairix_fscopy::tree::Kind;

/// The progress window this app is given, addressed by every synthesised
/// event.
const WINDOW_ID: u64 = 7;

/// The client of a progress window at its own extent.
fn client() -> Rect {
    let (width, height) = progress_window_extent(Scale::ONE, &Theme::dark(), false);
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
        at_ns: 0,
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
        at_ns: 0,
    }
}

/// A named key press with no modifiers held.
fn named(code: NamedKeyCode) -> WindowEvent {
    WindowEvent::Key {
        window_id: WINDOW_ID,
        key: KeyInput::Pressed {
            key: Some(KeyValue::Named(code)),
            modifiers: Modifiers::default(),
            // Which key typed it is not what a progress window reads.
            physical: tairix_abi::input::PhysicalKey::A,
        },
    }
}

fn route(client: Rect, event: &WindowEvent) -> ProgressControl {
    let mut damage = Region::new();
    progress_control(
        (client, Scale::ONE, &Theme::dark()),
        None,
        event,
        &mut damage,
    )
}

/// A primary release at `point`.
fn release(point: Point) -> WindowEvent {
    WindowEvent::Pointer {
        window_id: WINDOW_ID,
        x: u32::try_from(point.x).unwrap_or(0),
        y: u32::try_from(point.y).unwrap_or(0),
        action: PointerAction::Released(PointerButtonCode::Primary),
        modifiers: Modifiers::default(),
        at_ns: 0,
    }
}

/// A key press producing `ch`, with no modifiers held.
fn typed(ch: char) -> WindowEvent {
    WindowEvent::Key {
        window_id: WINDOW_ID,
        key: KeyInput::Pressed {
            key: Some(KeyValue::Char(ch)),
            modifiers: Modifiers::default(),
            // Which key typed it is not what a progress window reads.
            physical: tairix_abi::input::PhysicalKey::A,
        },
    }
}

/// The question about `notes.txt`, a file taken in `Desktop`.
fn asking_panel() -> ClashPanel {
    ClashPanel::new(&Clash {
        name: String::from("notes.txt"),
        folder: String::from("Desktop"),
        folders: false,
    })
}

/// `event` to a progress window, at its own extent, asking through `panel`.
fn asked(panel: &mut ClashPanel, event: &WindowEvent) -> ProgressControl {
    let theme = Theme::dark();
    let (width, height) = progress_window_extent(Scale::ONE, &theme, true);
    let client = Rect::new(0, 0, width, height);
    let mut damage = Region::new();
    progress_control(
        (client, Scale::ONE, &theme),
        Some(panel),
        event,
        &mut damage,
    )
}

/// While it asks, the window's keys answer the question: `Enter` gives Skip,
/// the default, `Escape` is Stop, and `Tab` and `Space` reach and tick the box
/// and move on to Replace.
#[test]
fn a_question_is_answered_from_the_keyboard_through_its_window() {
    let answer = |choice, for_the_rest| {
        ProgressControl::Answer(ClashAnswer {
            choice,
            for_the_rest,
        })
    };
    let mut panel = asking_panel();
    assert_eq!(
        asked(&mut panel, &named(NamedKeyCode::Enter)),
        answer(ClashChoice::Skip, false),
        "Skip is the default"
    );
    assert_eq!(
        asked(&mut panel, &named(NamedKeyCode::Escape)),
        answer(ClashChoice::Stop, false),
        "Escape is Stop"
    );
    assert_eq!(
        asked(&mut panel, &named(NamedKeyCode::Tab)),
        ProgressControl::Ignore
    );
    assert_eq!(asked(&mut panel, &typed(' ')), ProgressControl::Ignore);
    for _ in 0..2 {
        assert_eq!(
            asked(&mut panel, &named(NamedKeyCode::Tab)),
            ProgressControl::Ignore
        );
    }
    assert_eq!(
        asked(&mut panel, &named(NamedKeyCode::Enter)),
        answer(ClashChoice::Replace, true)
    );
    let redraw = WindowEvent::RedrawRequested { window_id: 1 };
    assert_eq!(asked(&mut panel, &redraw), ProgressControl::Redraw);
}

/// While it asks, a click on a drawn choice answers with it, found where the
/// window draws it: Skip, the default, stands at the trailing edge.
#[test]
fn a_question_is_answered_by_a_click_through_its_window() {
    let theme = Theme::dark();
    let (width, height) = progress_window_extent(Scale::ONE, &theme, true);
    let found = (0..i32::try_from(height).unwrap_or(0)).rev().find_map(|y| {
        (0..i32::try_from(width).unwrap_or(0)).rev().find_map(|x| {
            let mut panel = asking_panel();
            let point = Point::new(x, y);
            asked(&mut panel, &press(point));
            match asked(&mut panel, &release(point)) {
                ProgressControl::Answer(answer) => Some(answer),
                _ => None,
            }
        })
    });
    assert_eq!(
        found,
        Some(ClashAnswer {
            choice: ClashChoice::Skip,
            for_the_rest: false
        })
    );
}

/// The question names the name taken and the folder it is taken in, a folder
/// meeting a folder merging, and Stop is no answer to the engine but a
/// cancel.
#[test]
fn a_question_names_the_taken_name_and_stop_cancels() {
    let mut conflict = Conflict {
        source: String::from("/Users/ann/Home/notes.txt"),
        target: String::from("/Users/ann/Desktop/notes.txt"),
        occupant: Kind::File,
    };
    assert_eq!(
        clash_of(&conflict),
        Clash {
            name: String::from("notes.txt"),
            folder: String::from("Desktop"),
            folders: false,
        }
    );
    conflict.occupant = Kind::Directory;
    assert!(clash_of(&conflict).folders);
    assert_eq!(decision_of(ClashChoice::Replace), Some(Decision::Overwrite));
    assert_eq!(decision_of(ClashChoice::Skip), Some(Decision::Skip));
    assert_eq!(decision_of(ClashChoice::Stop), None);
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
        write_refusal,
        read_refusal,
        reach_refusal,
        destination_refusal,
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
    // The kernel's rename refuses a folder into its own subtree as out of
    // range; the user is told so, not that it "could not be moved".
    assert_eq!(move_refusal(Errno::OutOfRange), super::INTO_ITSELF);
    assert_eq!(read_refusal(Errno::MediumError), "its device failed");
    assert_eq!(change_refusal(Errno::ReadOnly), "its volume is read-only");
    assert_eq!(
        change_refusal(Errno::NotSupported),
        "its volume cannot store that"
    );
    assert_eq!(
        change_refusal(Errno::Stale),
        "it is no longer what this window showed"
    );
    assert_eq!(read_refusal(Errno::OutOfRange), "it could not be read");
}

/// A plan that comes back after the user moved on — to another selection or
/// another folder — asks nothing; one that comes back while something else
/// is open over the window is refused, saying so, rather than dropped.
#[test]
fn a_delete_confirmation_opens_only_over_the_selection_it_plans() {
    let plan = |name: &str, node: u64| {
        let listed = tairix_browse::Listed {
            path: alloc::vec![String::from("home"), String::from(name)],
            id: tairix_abi::FileId {
                volume: [1; 16],
                node,
            },
        };
        let target = tairix_browse::DeleteTarget::new(listed, tairix_browse::EntryKind::File);
        tairix_browse::DeletePlan::new(alloc::vec![target]).expect("a plan")
    };
    let asked = plan("a", 1);
    let other = plan("b", 2);
    let swapped = plan("a", 3);
    assert_eq!(
        delete_confirmation(&asked, Some(&asked), false),
        Confirmation::Asked
    );
    for (now, why) in [
        (Some(&other), "another selection"),
        (Some(&swapped), "another node under the selected name"),
        (None, "nothing selected"),
    ] {
        for busy in [false, true] {
            assert_eq!(
                delete_confirmation(&asked, now, busy),
                Confirmation::MovedOn,
                "{why}"
            );
        }
    }
    assert_eq!(
        delete_confirmation(&asked, Some(&asked), true),
        Confirmation::Busy,
        "something else open"
    );
}

/// The Trash's contents come back after the user may have gone elsewhere: the
/// confirmation opens only over the Trash that was read.
#[test]
fn an_empty_confirmation_opens_only_over_the_trash_it_read() {
    let trash: Vec<String> = ["home", "Library", "Trash"].map(String::from).to_vec();
    assert_eq!(
        empty_confirmation(&trash, &trash, false),
        Confirmation::Asked
    );
    for busy in [false, true] {
        assert_eq!(
            empty_confirmation(&trash, &trash[..2], busy),
            Confirmation::MovedOn,
            "the window moved on"
        );
    }
    assert_eq!(
        empty_confirmation(&trash, &trash, true),
        Confirmation::Busy,
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

/// An operation's refusal names the entry it stopped at, else the item asked
/// for, and says why in the words the rest of the manager uses.
#[test]
fn an_operations_refusal_names_the_entry_it_stopped_at() {
    let item = [String::from("Users"), String::from("Photos")];
    let full = OpError::Write(
        String::from("/Storage/usb/Photos/2024/a.jpg"),
        Errno::NoSpace,
    );
    assert_eq!(
        operation_refusal("copy", &item, &full),
        "could not copy \u{201c}a.jpg\u{201d}: the destination is full"
    );
    assert_eq!(
        operation_refusal("move", &item, &OpError::IntoItself),
        alloc::format!(
            "could not move \u{201c}Photos\u{201d}: {}",
            super::INTO_ITSELF
        )
    );
    assert_eq!(
        operation_refusal(
            "copy",
            &item,
            &OpError::Read(String::new(), Errno::OutOfMemory)
        ),
        "could not copy \u{201c}Photos\u{201d}: there was not enough memory for it",
        "a refusal spelling no path names the item"
    );
    assert_eq!(
        operation_refusal(
            "delete",
            &item,
            &OpError::Changed(String::from("/Users/Photos/b"))
        ),
        "could not delete \u{201c}b\u{201d}: it changed meanwhile, so it was left as it is"
    );
    let taken = OpError::Rename(String::from("/Users/Photos"), Errno::AlreadyExists);
    assert_eq!(
        operation_refusal("move", &item, &taken),
        "could not move \u{201c}Photos\u{201d}: an item with that name appeared at the destination"
    );
    let unwritable = OpError::Write(String::from("/Storage/usb/Photos"), Errno::DeviceFault);
    assert_eq!(
        operation_refusal("copy", &item, &unwritable),
        "could not copy \u{201c}Photos\u{201d}: its device failed"
    );
}
