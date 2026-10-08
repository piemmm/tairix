//! Host tests for the seat's drag carrier.

use tairix_abi::window_ipc::{BundleRunPath, DocumentName, DragItems, DropOperation, DropTarget};
use tairix_abi::Errno;
use tairix_theme::CursorKind;
use tairix_window::DragConclusion;
use tairix_wm::{Compositor, InputEvent, Point, WindowId};

use crate::drag::{DragEnd, DragStep, DragSurface, OwedReport};
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

/// One openable file named `name`.
fn file(name: &str) -> DragItems {
    DragItems::new(DocumentName::new(name).expect("a valid name"), 1, true).expect("one file")
}

/// Several items.
fn several() -> DragItems {
    DragItems::new(DocumentName::new("a.txt").expect("a valid name"), 3, false)
        .expect("three items")
}

const RELEASE: InputEvent = InputEvent::PointerReleased {
    button: tairix_wm::PointerButton::Primary,
};

/// Move the carried drag to `to`, over `surface`.
fn over(shell: &mut DesktopShell, comp: &mut Compositor, to: Point, surface: DragSurface) {
    assert_eq!(
        shell.drag_pointer(comp, &moved(to.x, to.y)),
        DragStep::Moved(to)
    );
    shell.drag_over(comp, surface);
}

fn window_at(window_id: u64, x: u32, y: u32) -> DragSurface {
    DragSurface::Window { window_id, x, y }
}

fn desktop(folder: &str, icon: Option<usize>) -> DragSurface {
    DragSurface::Desktop {
        folder: alloc::string::String::from(folder),
        icon,
        revision: 0,
    }
}

/// The plate follows and never takes the pointer, and a slot that takes the
/// file is where a release drops it.
#[test]
fn the_plate_follows_and_a_slot_that_takes_the_file_is_dropped_on() {
    let (mut shell, mut comp, window) = pressed();
    shell
        .begin_drag(&mut comp, 7, window, file("notes.txt"))
        .expect("carried");
    assert_eq!(
        shell.begin_drag(&mut comp, 7, window, file("notes.txt")),
        Err(Errno::AlreadyExists),
        "one drag at a time"
    );
    let slot = app_slot_point(&shell, 0);
    let probe = Point::new(slot.x + 20, slot.y + 20);
    let beneath = comp.window_at(probe);
    over(
        &mut shell,
        &mut comp,
        slot,
        DragSurface::Slot {
            index: 0,
            target: Some(alloc::boxed::Box::new(editor())),
        },
    );
    assert_eq!(
        comp.window_at(probe),
        beneath,
        "the plate beside the pointer never becomes what is under it"
    );
    assert_eq!(shell.take_drag_report(), None, "a slot is not reported");
    assert_eq!(
        shell.drag_pointer(&mut comp, &RELEASE),
        DragStep::Ended(DragEnd {
            source: 7,
            ended: DragConclusion::Application(alloc::boxed::Box::new(editor())),
        })
    );
}

/// Another button ends the drag with nothing dropped, and a window closing
/// ends only the drag it began.
#[test]
fn another_press_cancels_and_an_abort_is_scoped_to_the_source() {
    let (mut shell, mut comp, window) = pressed();
    shell
        .begin_drag(&mut comp, 7, window, file("notes.txt"))
        .expect("carried");
    assert_eq!(
        shell.drag_pointer(&mut comp, &SECONDARY_PRESS),
        DragStep::Ended(DragEnd {
            source: 7,
            ended: DragConclusion::Nothing,
        })
    );

    let (mut shell, mut comp, window) = pressed();
    shell
        .begin_drag(&mut comp, 7, window, file("notes.txt"))
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
        .begin_drag(&mut comp, 7, window, file("notes.txt"))
        .expect("carried");
    assert_eq!(
        shell.router().wm().client_grab(),
        None,
        "the drag took the press over"
    );
    assert!(matches!(
        shell.drag_pointer(&mut comp, &RELEASE),
        DragStep::Ended(_)
    ));
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
        .begin_drag(&mut comp, 7, window, file("notes.bin"))
        .expect("carried");
    let slot = app_slot_point(&shell, 0);
    let refusing = DragSurface::Slot {
        index: 0,
        target: None,
    };
    over(&mut shell, &mut comp, slot, refusing.clone());
    shell.settle(&mut comp);
    assert_eq!(shell.session().taskbar().apps().hover(), None);
    over(
        &mut shell,
        &mut comp,
        Point::new(slot.x + 1, slot.y),
        refusing,
    );
    shell.settle(&mut comp);
    assert_eq!(
        shell.session().taskbar().apps().hover(),
        None,
        "not lit by a later settle either"
    );
    assert_eq!(
        shell.drag_pointer(&mut comp, &RELEASE),
        DragStep::Ended(DragEnd {
            source: 7,
            ended: DragConclusion::Nothing,
        }),
        "nothing is dropped on an application that never took the file"
    );
}

/// The strip changing under a drag forgets the slot it was over, so a drop
/// there is decided by what the next motion finds.
#[test]
fn a_strip_replaced_mid_drag_forgets_its_slot() {
    let (mut shell, mut comp, window) = pressed();
    shell
        .begin_drag(&mut comp, 7, window, file("notes.txt"))
        .expect("carried");
    let slot = app_slot_point(&shell, 0);
    over(
        &mut shell,
        &mut comp,
        slot,
        DragSurface::Slot {
            index: 0,
            target: Some(alloc::boxed::Box::new(editor())),
        },
    );
    shell.set_apps(
        &mut comp,
        alloc::vec![tairix_taskbar::AppSlot::new(
            "Viewer",
            tairix_icon::IconKind::AppBundle
        )],
    );
    assert_eq!(
        shell.drag_pointer(&mut comp, &RELEASE),
        DragStep::Ended(DragEnd {
            source: 7,
            ended: DragConclusion::Nothing,
        })
    );
}

/// A burst of motion over the application's own window owes one report,
/// numbered, for where the burst ended; leaving it owes one more, so the
/// window can let go of what it lit.
#[test]
fn a_burst_over_a_window_owes_one_report_and_leaving_owes_another() {
    let (mut shell, mut comp, window) = pressed();
    shell
        .begin_drag(&mut comp, 7, window, several())
        .expect("carried");
    for x in 10..14 {
        over(
            &mut shell,
            &mut comp,
            Point::new(110 + i32::try_from(x).expect("small"), 120),
            window_at(4, x, 20),
        );
    }
    assert_eq!(
        shell.take_drag_report(),
        Some(OwedReport {
            source: 7,
            serial: 1,
            at: window_at(4, 13, 20),
            shift: false,
        })
    );
    assert_eq!(shell.take_drag_report(), None, "taken once");
    over(
        &mut shell,
        &mut comp,
        Point::new(700, 500),
        DragSurface::Nothing,
    );
    assert_eq!(
        shell.take_drag_report(),
        Some(OwedReport {
            source: 7,
            serial: 2,
            at: DragSurface::Nothing,
            shift: false,
        })
    );
    over(
        &mut shell,
        &mut comp,
        Point::new(701, 500),
        DragSurface::Nothing,
    );
    assert_eq!(shell.take_drag_report(), None, "nothing new to say");
}

/// Only an answer about where the pointer is shows, and the drop does what
/// the pointer showed.
#[test]
fn the_pointer_shows_the_current_answer_and_the_drop_does_what_it_showed() {
    let (mut shell, mut comp, window) = pressed();
    shell
        .begin_drag(&mut comp, 7, window, several())
        .expect("carried");
    over(
        &mut shell,
        &mut comp,
        Point::new(120, 120),
        window_at(4, 10, 20),
    );
    let first = shell.take_drag_report().expect("owed");
    assert!(
        !shell.drag_verdict(&mut comp, 9, first.serial, Some(DropOperation::Copy)),
        "another window's drag answers nothing here"
    );
    assert!(shell.drag_verdict(&mut comp, 7, first.serial, Some(DropOperation::Copy)));
    assert_eq!(shell.cursor().kind(), CursorKind::DragCopy);

    // Moving within the window keeps what is shown until the next answer.
    over(
        &mut shell,
        &mut comp,
        Point::new(125, 120),
        window_at(4, 15, 20),
    );
    let second = shell.take_drag_report().expect("owed");
    assert_eq!(shell.cursor().kind(), CursorKind::DragCopy);
    assert!(shell.drag_verdict(&mut comp, 7, second.serial, Some(DropOperation::Move)));
    assert_eq!(shell.cursor().kind(), CursorKind::DragMove);

    // Arriving somewhere else shows nothing until that place answers, and an
    // answer about the old place is not taken for it.
    over(
        &mut shell,
        &mut comp,
        Point::new(500, 300),
        desktop("/Users/ann/Desktop", None),
    );
    assert_eq!(shell.cursor().kind(), CursorKind::Arrow);
    assert!(!shell.drag_verdict(&mut comp, 7, second.serial, Some(DropOperation::Move)));
    let third = shell.take_drag_report().expect("owed");
    assert_eq!(third.at, desktop("/Users/ann/Desktop", None));
    assert!(shell.drag_verdict(&mut comp, 7, third.serial, Some(DropOperation::Copy)));
    assert_eq!(
        shell.drag_pointer(&mut comp, &RELEASE),
        DragStep::Ended(DragEnd {
            source: 7,
            ended: DragConclusion::Desktop {
                serial: third.serial,
                operation: DropOperation::Copy,
            },
        })
    );
    assert_eq!(
        shell.cursor().kind(),
        CursorKind::Arrow,
        "the shape is let go"
    );
}

/// A place the application refused, or has not answered yet, drops nothing.
#[test]
fn an_unanswered_or_refused_place_drops_nothing() {
    let (mut shell, mut comp, window) = pressed();
    shell
        .begin_drag(&mut comp, 7, window, several())
        .expect("carried");
    over(
        &mut shell,
        &mut comp,
        Point::new(120, 120),
        window_at(4, 10, 20),
    );
    assert_eq!(
        shell.drag_pointer(&mut comp, &RELEASE),
        DragStep::Ended(DragEnd {
            source: 7,
            ended: DragConclusion::Nothing,
        }),
        "no answer yet"
    );

    let (mut shell, mut comp, window) = pressed();
    shell
        .begin_drag(&mut comp, 7, window, several())
        .expect("carried");
    over(
        &mut shell,
        &mut comp,
        Point::new(120, 120),
        window_at(4, 10, 20),
    );
    let report = shell.take_drag_report().expect("owed");
    assert!(shell.drag_verdict(&mut comp, 7, report.serial, None));
    assert_eq!(
        shell.drag_pointer(&mut comp, &RELEASE),
        DragStep::Ended(DragEnd {
            source: 7,
            ended: DragConclusion::Nothing,
        }),
        "refused"
    );
}

/// Pressing or letting go of `Shift` over a reported place asks again, and a
/// lit desktop folder is the one the pointer is on while it is accepted.
#[test]
fn shift_asks_again_and_an_accepted_desktop_folder_is_lit() {
    let (mut shell, mut comp, window) = pressed();
    shell
        .begin_drag(&mut comp, 7, window, several())
        .expect("carried");
    over(
        &mut shell,
        &mut comp,
        Point::new(500, 300),
        desktop("/Users/ann/Desktop/Work", Some(2)),
    );
    let first = shell.take_drag_report().expect("owed");
    assert_eq!(shell.drag_drop_icon(), None, "not lit until accepted");
    assert!(shell.drag_verdict(&mut comp, 7, first.serial, Some(DropOperation::Copy)));
    assert_eq!(shell.drag_drop_icon(), Some(2));

    let shift = InputEvent::ModifiersChanged {
        modifiers: tairix_wm::Modifiers {
            shift: true,
            ..tairix_wm::Modifiers::default()
        },
    };
    assert_eq!(shell.drag_key(&mut comp, &shift), None);
    let again = shell.take_drag_report().expect("Shift asks again");
    assert!(again.shift);
    assert_eq!(again.at, first.at);
    assert_eq!(shell.drag_key(&mut comp, &shift), None);
    assert_eq!(shell.take_drag_report(), None, "no change, no question");

    // The answer given before Shift changed is about another drop: it is not
    // shown, the folder is dark until the new one, and a release before it
    // drops nothing.
    assert!(!shell.drag_verdict(&mut comp, 7, first.serial, Some(DropOperation::Copy)));
    assert_eq!(shell.drag_drop_icon(), None);
    assert_eq!(
        shell.drag_pointer(&mut comp, &RELEASE),
        DragStep::Ended(DragEnd {
            source: 7,
            ended: DragConclusion::Nothing,
        })
    );
}

/// An answer to the report made after Shift changed is shown, and decides the
/// drop.
#[test]
fn the_answer_after_shift_changed_decides_the_drop() {
    let (mut shell, mut comp, window) = pressed();
    shell
        .begin_drag(&mut comp, 7, window, several())
        .expect("carried");
    over(
        &mut shell,
        &mut comp,
        Point::new(500, 300),
        desktop("/Users/ann/Desktop/Work", Some(2)),
    );
    let first = shell.take_drag_report().expect("owed");
    assert!(shell.drag_verdict(&mut comp, 7, first.serial, Some(DropOperation::Copy)));
    let shift = InputEvent::ModifiersChanged {
        modifiers: tairix_wm::Modifiers {
            shift: true,
            ..tairix_wm::Modifiers::default()
        },
    };
    assert_eq!(shell.drag_key(&mut comp, &shift), None);
    let again = shell.take_drag_report().expect("Shift asks again");
    assert!(shell.drag_verdict(&mut comp, 7, again.serial, Some(DropOperation::Move)));
    assert_eq!(shell.drag_drop_icon(), Some(2));
    assert!(matches!(
        shell.drag_pointer(&mut comp, &RELEASE),
        DragStep::Ended(DragEnd {
            source: 7,
            ended: DragConclusion::Desktop {
                operation: DropOperation::Move,
                ..
            },
        })
    ));
}
