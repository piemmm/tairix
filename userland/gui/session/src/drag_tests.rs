//! Host tests for the seat's drag carrier.

use tairix_abi::window_ipc::{BundleRunPath, DropTarget};
use tairix_abi::Errno;
use tairix_wm::{Compositor, InputEvent, Point, WindowId};

use crate::drag::DragEnd;
use crate::shell::DesktopShell;
use crate::tests::{
    app_slot_point, compositor, moved, opaque_window, shell, PRIMARY_PRESS, SECONDARY_PRESS,
};

/// A shell with one application slot and a window holding a primary press.
fn pressed() -> (DesktopShell, Compositor, WindowId) {
    let mut shell = shell();
    let mut comp = compositor();
    shell.set_apps(
        &mut comp,
        alloc::vec![tairix_taskbar::AppSlot::new(
            "Editor",
            tairix_icon::IconKind::AppBundle
        )],
    );
    let window = opaque_window(&mut comp, Point::new(100, 100), 120, 80);
    let _ = shell.handle(moved(150, 140), &mut comp, 0);
    let _ = shell.handle(PRIMARY_PRESS, &mut comp, 0);
    (shell, comp, window)
}

fn editor() -> DropTarget {
    DropTarget {
        run_path: BundleRunPath::new("/Apps/textedit.app/Run").expect("a valid path"),
        writes_documents: true,
    }
}

/// The plate follows the pointer and never takes it; the icon-bar slot is
/// asked what it does with the file once as the pointer arrives on it, never
/// per motion sample across it.
#[test]
fn the_plate_follows_and_a_slot_is_asked_once_per_arrival() {
    let (mut shell, mut comp, window) = pressed();
    shell
        .begin_drag(&mut comp, 7, window, "notes.txt")
        .expect("carried");
    assert_eq!(
        shell.begin_drag(&mut comp, 7, window, "notes.txt"),
        Err(Errno::AlreadyExists),
        "one drag at a time"
    );
    let slot = app_slot_point(&shell, 0);
    let probe = Point::new(slot.x + 20, slot.y + 20);
    let beneath = comp.window_at(probe);
    let mut asked = 0;
    for dx in 0..3 {
        let event = moved(slot.x + dx, slot.y);
        let ended = shell.drag_pointer(&mut comp, &event, &mut |_, name| {
            assert_eq!(name, "notes.txt");
            asked += 1;
            Some(editor())
        });
        assert_eq!(ended, None);
    }
    assert_eq!(asked, 1, "resting on one slot is one question");
    assert_eq!(
        comp.window_at(probe),
        beneath,
        "the plate beside the pointer never becomes what is under it"
    );
    assert_eq!(
        shell.drag_pointer(
            &mut comp,
            &InputEvent::PointerReleased {
                button: tairix_wm::PointerButton::Primary
            },
            &mut |_, _| None,
        ),
        Some(DragEnd {
            source: 7,
            target: Some(editor())
        })
    );
}

/// Another button ends the drag with nothing dropped, and a window closing
/// ends only the drag it began.
#[test]
fn another_press_cancels_and_an_abort_is_scoped_to_the_source() {
    let (mut shell, mut comp, window) = pressed();
    shell
        .begin_drag(&mut comp, 7, window, "notes.txt")
        .expect("carried");
    assert_eq!(
        shell.drag_pointer(&mut comp, &SECONDARY_PRESS, &mut |_, _| Some(editor())),
        Some(DragEnd {
            source: 7,
            target: None
        })
    );

    let (mut shell, mut comp, window) = pressed();
    shell
        .begin_drag(&mut comp, 7, window, "notes.txt")
        .expect("carried");
    shell.abort_drag_for(&mut comp, 9);
    assert!(
        shell.drag_active(),
        "another window closing leaves the drag"
    );
    shell.abort_drag_for(&mut comp, 7);
    assert!(!shell.drag_active());
}

/// A drag takes the rest of the press, so once it has ended no motion is
/// reported to the window it began in as though that press were still held.
#[test]
fn after_a_drag_no_motion_is_reported_to_its_source_window() {
    let (mut shell, mut comp, window) = pressed();
    assert_eq!(shell.router().wm().client_grab(), Some(window));
    shell
        .begin_drag(&mut comp, 7, window, "notes.txt")
        .expect("carried");
    assert_eq!(
        shell.router().wm().client_grab(),
        None,
        "the drag took the press over"
    );
    let released = InputEvent::PointerReleased {
        button: tairix_wm::PointerButton::Primary,
    };
    assert!(shell
        .drag_pointer(&mut comp, &released, &mut |_, _| None)
        .is_some());
    let outcome = shell.handle(moved(600, 500), &mut comp, 0);
    assert!(
        !matches!(
            outcome,
            crate::shell::ShellOutcome::WindowManager(
                tairix_wm::InputResponse::ClientPointerMoved { window: to, .. }
            ) if to == window
        ),
        "motion after the drop is not the source's in-content drag"
    );
}

/// A slot that does not take what is carried stays dark, however the screen
/// is brought up to date meanwhile: the drag holds the pointer, so the bar's
/// own hover never lights it either.
#[test]
fn a_slot_that_refuses_the_file_is_never_lit() {
    let (mut shell, mut comp, window) = pressed();
    shell
        .begin_drag(&mut comp, 7, window, "notes.bin")
        .expect("carried");
    let slot = app_slot_point(&shell, 0);
    assert_eq!(
        shell.drag_pointer(&mut comp, &moved(slot.x, slot.y), &mut |_, _| None),
        None
    );
    shell.settle(&mut comp);
    assert_eq!(shell.session().taskbar().apps().hover(), None);
    assert_eq!(
        shell.drag_pointer(&mut comp, &moved(slot.x + 1, slot.y), &mut |_, _| None),
        None
    );
    shell.settle(&mut comp);
    assert_eq!(
        shell.session().taskbar().apps().hover(),
        None,
        "not lit by a later settle either"
    );
}

/// The strip changing under a drag forgets the slot it was over: an index may
/// now name another application, so a drop there is decided by asking again.
#[test]
fn a_strip_replaced_mid_drag_asks_its_slot_afresh() {
    let (mut shell, mut comp, window) = pressed();
    shell
        .begin_drag(&mut comp, 7, window, "notes.txt")
        .expect("carried");
    let slot = app_slot_point(&shell, 0);
    let _ = shell.drag_pointer(
        &mut comp,
        &moved(slot.x, slot.y),
        &mut |_, _| Some(editor()),
    );
    shell.set_apps(
        &mut comp,
        alloc::vec![tairix_taskbar::AppSlot::new(
            "Viewer",
            tairix_icon::IconKind::AppBundle
        )],
    );
    let mut asked = 0;
    let _ = shell.drag_pointer(&mut comp, &moved(slot.x + 1, slot.y), &mut |_, _| {
        asked += 1;
        None
    });
    assert_eq!(asked, 1, "the new occupant of the slot is asked");
    assert_eq!(
        shell.drag_pointer(
            &mut comp,
            &InputEvent::PointerReleased {
                button: tairix_wm::PointerButton::Primary
            },
            &mut |_, _| None,
        ),
        Some(DragEnd {
            source: 7,
            target: None
        }),
        "nothing is dropped on an application that never took the file"
    );
}
