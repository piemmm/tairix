//! Host tests for the seat's drag carrier.

use tairix_abi::window_ipc::{BundleRunPath, DocumentName, DragItems, DropOperation, DropTarget};
use tairix_abi::Errno;
use tairix_theme::CursorKind;
use tairix_window::DragConclusion;
use tairix_wm::{Compositor, InputEvent, Point, WindowId};

use alloc::string::String;

use crate::drag::{
    DragEnd, DragStep, DragSurface, DropRefusal, ItemsEnd, ItemsReport, OfferEnd, OwedReport,
    SaveDrop, SaveEnd, SaveOffer, SaveReport, SavedDocument, SlotAnswer,
};
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
            crate::tests::slot_owner(0),
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
        run_path: BundleRunPath::new("/System/Applications/textedit.app/Run")
            .expect("a valid path"),
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

/// The report an application's drag owes, which `why` says it does.
fn owed_report(shell: &mut DesktopShell, why: &str) -> ItemsReport {
    match shell.take_drag_report() {
        Some(OwedReport::Items(report)) => report,
        other => panic!("{why}: {other:?}"),
    }
}

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
            answer: Some(SlotAnswer::Opens(alloc::boxed::Box::new(editor()))),
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
        DragStep::Ended(DragEnd::Items(ItemsEnd {
            source: 7,
            ended: DragConclusion::Application(alloc::boxed::Box::new(editor())),
            refused: None,
        }))
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
        DragStep::Ended(DragEnd::Items(ItemsEnd {
            source: 7,
            ended: DragConclusion::Nothing,
            refused: None,
        }))
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
    let refusal = DropRefusal::new("Paint", "notes.bin", true, &[String::from("image/png")]);
    let refusing = DragSurface::Slot {
        index: 0,
        answer: Some(SlotAnswer::Refuses(alloc::boxed::Box::new(refusal.clone()))),
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
        DragStep::Ended(DragEnd::Items(ItemsEnd {
            source: 7,
            ended: DragConclusion::Nothing,
            refused: Some(alloc::boxed::Box::new(refusal)),
        })),
        "nothing is dropped on an application that never took the file, and \
         the drop is answered with why"
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
            answer: Some(SlotAnswer::Opens(alloc::boxed::Box::new(editor()))),
        },
    );
    shell.set_apps(
        &mut comp,
        alloc::vec![tairix_taskbar::AppSlot::new(
            crate::tests::slot_owner(0),
            "Viewer",
            tairix_icon::IconKind::AppBundle
        )],
    );
    assert_eq!(
        shell.drag_pointer(&mut comp, &RELEASE),
        DragStep::Ended(DragEnd::Items(ItemsEnd {
            source: 7,
            ended: DragConclusion::Nothing,
            refused: None,
        }))
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
        Some(OwedReport::Items(ItemsReport {
            source: 7,
            serial: 1,
            at: window_at(4, 13, 20),
            shift: false,
        }))
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
        Some(OwedReport::Items(ItemsReport {
            source: 7,
            serial: 2,
            at: DragSurface::Nothing,
            shift: false,
        }))
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
    let first = owed_report(&mut shell, "owed");
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
    let second = owed_report(&mut shell, "owed");
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
    let third = owed_report(&mut shell, "owed");
    assert_eq!(third.at, desktop("/Users/ann/Desktop", None));
    assert!(shell.drag_verdict(&mut comp, 7, third.serial, Some(DropOperation::Copy)));
    assert_eq!(
        shell.drag_pointer(&mut comp, &RELEASE),
        DragStep::Ended(DragEnd::Items(ItemsEnd {
            source: 7,
            ended: DragConclusion::Desktop {
                serial: third.serial,
                operation: DropOperation::Copy,
            },
            refused: None,
        }))
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
        DragStep::Ended(DragEnd::Items(ItemsEnd {
            source: 7,
            ended: DragConclusion::Nothing,
            refused: None,
        })),
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
    let report = owed_report(&mut shell, "owed");
    assert!(shell.drag_verdict(&mut comp, 7, report.serial, None));
    assert_eq!(
        shell.drag_pointer(&mut comp, &RELEASE),
        DragStep::Ended(DragEnd::Items(ItemsEnd {
            source: 7,
            ended: DragConclusion::Nothing,
            refused: None,
        })),
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
    let first = owed_report(&mut shell, "owed");
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
    let again = owed_report(&mut shell, "Shift asks again");
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
        DragStep::Ended(DragEnd::Items(ItemsEnd {
            source: 7,
            ended: DragConclusion::Nothing,
            refused: None,
        }))
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
    let first = owed_report(&mut shell, "owed");
    assert!(shell.drag_verdict(&mut comp, 7, first.serial, Some(DropOperation::Copy)));
    let shift = InputEvent::ModifiersChanged {
        modifiers: tairix_wm::Modifiers {
            shift: true,
            ..tairix_wm::Modifiers::default()
        },
    };
    assert_eq!(shell.drag_key(&mut comp, &shift), None);
    let again = owed_report(&mut shell, "Shift asks again");
    assert!(shell.drag_verdict(&mut comp, 7, again.serial, Some(DropOperation::Move)));
    assert_eq!(shell.drag_drop_icon(), Some(2));
    assert!(matches!(
        shell.drag_pointer(&mut comp, &RELEASE),
        DragStep::Ended(DragEnd::Items(ItemsEnd {
            source: 7,
            ended: DragConclusion::Desktop {
                operation: DropOperation::Move,
                ..
            },
            refused: None,
        }))
    ));
}

#[test]
fn a_refused_drop_says_what_the_application_opens() {
    let claims = [String::from("image/png"), String::from("image/jpeg")];
    let spec = DropRefusal::new("Paint", "notes.txt", true, &claims)
        .spec()
        .expect("an alert");
    assert_eq!(
        spec.headline(),
        "Paint cannot open \u{201c}notes.txt\u{201d}"
    );
    assert_eq!(
        spec.detail(),
        "Paint opens files ending .png, .jpg and .jpeg."
    );

    let folder = DropRefusal::new("Paint", "Holiday", false, &claims)
        .spec()
        .expect("an alert");
    assert!(
        folder.detail().contains("one file at a time"),
        "{}",
        folder.detail()
    );

    let none = DropRefusal::new("Clock", "notes.txt", true, &[])
        .spec()
        .expect("an alert");
    assert_eq!(none.detail(), "Clock opens no files dropped on it.");

    let many: alloc::vec::Vec<String> = [
        "image/png",
        "image/jpeg",
        "image/gif",
        "image/bmp",
        "image/webp",
        "image/tiff",
        "image/x-icon",
        "image/x-riscos-sprite",
    ]
    .iter()
    .map(|claim| String::from(*claim))
    .collect();
    let long = DropRefusal::new("Viewer", "notes.txt", true, &many)
        .spec()
        .expect("an alert");
    assert!(long.detail().ends_with(" and others."), "{}", long.detail());
}

/// A refusal names the item and the application as shown, so what hides in
/// either cannot make the alert read as another's; so does the plate.
#[test]
fn a_refused_drop_names_what_hides_in_the_item_and_the_application() {
    let refusal = DropRefusal::new("Pa\u{202e}int", "a\u{202e}b.txt", true, &[]);
    let spec = refusal.spec().expect("an alert");
    assert_eq!(
        spec.headline(),
        "Pa\u{fffd}int cannot open \u{201c}a\u{fffd}b.txt\u{201d}"
    );
    assert_eq!(spec.detail(), "Pa\u{fffd}int opens no files dropped on it.");
    let folder = DropRefusal::new("Pa\u{202e}int", "a", false, &[])
        .spec()
        .expect("an alert");
    assert!(folder.detail().starts_with("Pa\u{fffd}int opens one file"));
    let named = DropRefusal::new("Pa\u{202e}int", "a", true, &[String::from("image/png")])
        .spec()
        .expect("an alert");
    assert!(named
        .detail()
        .starts_with("Pa\u{fffd}int opens files ending"));
    let items = DragItems::new(
        DocumentName::new("a\u{202e}b.txt").expect("a name"),
        1,
        true,
    )
    .expect("items");
    assert_eq!(super::plate_text(&items), "a\u{fffd}b.txt");
}

/// A window of another application takes a drop as its slot does: the file is
/// handed to that application, or the drop is answered with why not — and the
/// strip lights nothing, since the pointer is not over it.
#[test]
fn another_applications_window_takes_a_drop_as_its_slot_does() {
    let (mut shell, mut comp, window) = pressed();
    shell
        .begin_drag(&mut comp, 7, window, file("notes.txt"))
        .expect("carried");
    let at = Point::new(200, 200);
    over(
        &mut shell,
        &mut comp,
        at,
        DragSurface::AppWindow {
            index: 0,
            answer: Some(SlotAnswer::Opens(alloc::boxed::Box::new(editor()))),
        },
    );
    assert_eq!(
        shell.session().taskbar().apps().hover(),
        None,
        "no slot is lit"
    );
    assert_eq!(
        shell.take_drag_report(),
        None,
        "the window's own app is not asked"
    );
    assert_eq!(
        shell.drag_pointer(&mut comp, &RELEASE),
        DragStep::Ended(DragEnd::Items(ItemsEnd {
            source: 7,
            ended: DragConclusion::Application(alloc::boxed::Box::new(editor())),
            refused: None,
        }))
    );

    let (mut shell, mut comp, window) = pressed();
    shell
        .begin_drag(&mut comp, 7, window, file("notes.bin"))
        .expect("carried");
    let refusal = DropRefusal::new("Paint", "notes.bin", true, &[String::from("image/png")]);
    over(
        &mut shell,
        &mut comp,
        at,
        DragSurface::AppWindow {
            index: 0,
            answer: Some(SlotAnswer::Refuses(alloc::boxed::Box::new(refusal.clone()))),
        },
    );
    assert_eq!(
        shell.drag_pointer(&mut comp, &RELEASE),
        DragStep::Ended(DragEnd::Items(ItemsEnd {
            source: 7,
            ended: DragConclusion::Nothing,
            refused: Some(alloc::boxed::Box::new(refusal)),
        }))
    );
}

#[test]
fn a_window_an_alert_holds_takes_no_drop() {
    use crate::drag::{DragPlace, WindowUnder};
    for under in [
        WindowUnder::Own(Some((4, 9))),
        WindowUnder::Foreign(Some(2)),
    ] {
        assert_eq!(DragPlace::over_window(31, true, under), DragPlace::Nothing);
    }
    assert_eq!(
        DragPlace::over_window(31, false, WindowUnder::Own(Some((4, 9)))),
        DragPlace::Window {
            window_id: 31,
            x: 4,
            y: 9
        }
    );
    assert_eq!(
        DragPlace::over_window(31, false, WindowUnder::Foreign(Some(2))),
        DragPlace::AppWindow { index: 2 }
    );
    assert_eq!(
        DragPlace::over_window(31, false, WindowUnder::Foreign(None)),
        DragPlace::Nothing,
        "an application the strip does not show takes nothing"
    );
}

/// The document a save drag carries in these tests.
fn report_doc() -> SavedDocument {
    SavedDocument {
        name: String::from("Report.txt"),
        icon: tairix_icon::IconKind::File,
    }
}

/// A save carried out of the panel window `panel` for the window 7 asked
/// in.
fn saving() -> (DesktopShell, Compositor) {
    let (mut shell, mut comp, panel) = pressed();
    shell
        .begin_save_drag(&mut comp, (panel, 7), report_doc())
        .expect("carried");
    (shell, comp)
}

fn offer_at(window_id: u64, x: u32, y: u32) -> DragSurface {
    DragSurface::Offer { window_id, x, y }
}

/// The offers the carried save owes.
fn save_report(shell: &mut DesktopShell) -> Option<SaveReport> {
    match shell.take_drag_report()? {
        OwedReport::Save(report) => Some(report),
        other @ OwedReport::Items(_) => panic!("a save owes save reports: {other:?}"),
    }
}

/// A drag numbers its offers on from the last drag's, so a late answer to an
/// offer of a save the user gave up on is never taken for the next one's.
#[test]
fn an_answer_to_an_earlier_drag_is_never_taken_for_the_next_ones() {
    let (mut shell, mut comp, panel) = pressed();
    let offered = |shell: &mut DesktopShell, comp: &mut Compositor| {
        shell
            .begin_save_drag(comp, (panel, 7), report_doc())
            .expect("carried");
        over(shell, comp, Point::new(110, 110), offer_at(9, 10, 10));
        save_report(shell)
            .and_then(|report| report.over)
            .expect("offered")
            .serial
    };
    let first = offered(&mut shell, &mut comp);
    let _ = shell.drag_pointer(&mut comp, &RELEASE);
    let _ = shell.handle(moved(150, 140), &mut comp, 0);
    let _ = shell.handle(PRIMARY_PRESS, &mut comp, 0);
    let second = offered(&mut shell, &mut comp);
    assert_ne!(first, second);
    assert!(
        !shell.save_offer_answered(&mut comp, 9, first, true),
        "the earlier drag's answer"
    );
    assert!(shell.save_offer_answered(&mut comp, 9, second, true));
}

/// A save is a press carried on from the panel, and is carried alone.
#[test]
fn a_save_begins_only_from_the_press_on_its_panel() {
    let (mut shell, mut comp, panel) = pressed();
    let elsewhere = opaque_window(&mut comp, Point::new(300, 300), 40, 40);
    assert_eq!(
        shell.begin_save_drag(&mut comp, (elsewhere, 7), report_doc()),
        Err(Errno::PermissionDenied),
        "no press is held there"
    );
    shell
        .begin_save_drag(&mut comp, (panel, 7), report_doc())
        .expect("carried");
    assert_eq!(shell.save_source(), Some(7));
    assert_eq!(
        shell.drag_items(),
        None,
        "a save carries no application's items"
    );
    assert_eq!(
        shell.begin_drag(&mut comp, 7, panel, file("notes.txt")),
        Err(Errno::AlreadyExists),
        "one drag at a time"
    );
}

/// The desktop takes a save at once and lights its folder, and a drop there
/// saves into that folder.
#[test]
fn the_desktop_takes_a_save_into_the_folder_under_the_pointer() {
    let (mut shell, mut comp) = saving();
    over(
        &mut shell,
        &mut comp,
        Point::new(30, 30),
        desktop("/Users/ann/Desktop/Projects", Some(2)),
    );
    assert_eq!(shell.drag_drop_icon(), Some(2), "accepted, so lit");
    assert_eq!(
        save_report(&mut shell),
        Some(SaveReport {
            left: None,
            over: None
        }),
        "owed only to relight the folder"
    );
    assert_eq!(
        shell.drag_pointer(&mut comp, &RELEASE),
        DragStep::Ended(DragEnd::Save(SaveEnd {
            dropped: SaveDrop::Folder(String::from("/Users/ann/Desktop/Projects")),
            offer: None,
        }))
    );
}

/// Over a window the save is offered, numbered, at each point; the pointer
/// shows only an answer to an offer standing there; and leaving the window
/// tells it the offer ended.
#[test]
fn a_window_is_offered_the_save_and_told_when_it_leaves() {
    let (mut shell, mut comp) = saving();
    over(
        &mut shell,
        &mut comp,
        Point::new(110, 110),
        offer_at(9, 10, 10),
    );
    assert_eq!(
        save_report(&mut shell),
        Some(SaveReport {
            left: None,
            over: Some(SaveOffer {
                window_id: 9,
                serial: 1,
                x: 10,
                y: 10
            }),
        })
    );
    assert!(
        !shell.save_offer_answered(&mut comp, 4, 1, true),
        "not over that window"
    );
    assert!(
        !shell.save_offer_answered(&mut comp, 9, 2, true),
        "no such offer yet"
    );
    assert!(shell.save_offer_answered(&mut comp, 9, 1, true));
    assert_eq!(shell.cursor().kind(), CursorKind::DragCopy);
    over(
        &mut shell,
        &mut comp,
        Point::new(112, 110),
        offer_at(9, 12, 10),
    );
    assert_eq!(
        save_report(&mut shell).and_then(|report| report.over),
        Some(SaveOffer {
            window_id: 9,
            serial: 2,
            x: 12,
            y: 10
        }),
        "each point is offered afresh"
    );
    assert_eq!(
        shell.cursor().kind(),
        CursorKind::DragCopy,
        "the answer stands until the next arrives"
    );
    over(
        &mut shell,
        &mut comp,
        Point::new(500, 20),
        DragSurface::Nothing,
    );
    assert_eq!(
        save_report(&mut shell),
        Some(SaveReport {
            left: Some(OfferEnd {
                window_id: 9,
                serial: 2,
                dropped: None
            }),
            over: None,
        })
    );
    assert!(
        !shell.save_offer_answered(&mut comp, 9, 2, true),
        "an answer for a window left is not shown"
    );
    assert_eq!(shell.cursor().kind(), CursorKind::Arrow);
}

/// A drop on a window whose application accepted the standing offer is
/// that window's to name a folder for; anywhere else it saves nothing, and
/// the window offered last still hears the offer end.
#[test]
fn a_drop_saves_only_where_the_standing_offer_was_accepted() {
    let (mut shell, mut comp) = saving();
    over(
        &mut shell,
        &mut comp,
        Point::new(110, 110),
        offer_at(9, 10, 10),
    );
    let _ = save_report(&mut shell);
    assert!(shell.save_offer_answered(&mut comp, 9, 1, true));
    assert_eq!(
        shell.drag_pointer(&mut comp, &RELEASE),
        DragStep::Ended(DragEnd::Save(SaveEnd {
            dropped: SaveDrop::Offer {
                window_id: 9,
                serial: 1
            },
            offer: Some(OfferEnd {
                window_id: 9,
                serial: 1,
                dropped: Some((10, 10))
            }),
        }))
    );

    let (mut shell, mut comp) = saving();
    over(
        &mut shell,
        &mut comp,
        Point::new(110, 110),
        offer_at(9, 10, 10),
    );
    let _ = save_report(&mut shell);
    assert!(shell.save_offer_answered(&mut comp, 9, 1, false));
    assert_eq!(
        shell.drag_pointer(&mut comp, &RELEASE),
        DragStep::Ended(DragEnd::Save(SaveEnd {
            dropped: SaveDrop::Nothing,
            offer: Some(OfferEnd {
                window_id: 9,
                serial: 1,
                dropped: None
            }),
        })),
        "declined"
    );

    let (mut shell, mut comp) = saving();
    over(
        &mut shell,
        &mut comp,
        Point::new(110, 110),
        offer_at(9, 10, 10),
    );
    assert_eq!(
        shell.drag_pointer(&mut comp, &RELEASE),
        DragStep::Ended(DragEnd::Save(SaveEnd {
            dropped: SaveDrop::Unanswered,
            offer: None,
        })),
        "dropped before the window could answer, which the panel says"
    );
}

/// `Shift` changes nothing about a save, and `Escape` ends it unsaved,
/// telling the window it was offered to.
#[test]
fn shift_asks_nothing_again_and_escape_drops_nothing() {
    let (mut shell, mut comp) = saving();
    over(
        &mut shell,
        &mut comp,
        Point::new(110, 110),
        offer_at(9, 10, 10),
    );
    let _ = save_report(&mut shell);
    assert!(shell.save_offer_answered(&mut comp, 9, 1, true));
    let shift = tairix_wm::Modifiers {
        shift: true,
        ..tairix_wm::Modifiers::default()
    };
    assert_eq!(
        shell.drag_key(
            &mut comp,
            &InputEvent::ModifiersChanged { modifiers: shift }
        ),
        None
    );
    assert_eq!(shell.take_drag_report(), None, "nothing to ask again");
    assert_eq!(shell.cursor().kind(), CursorKind::DragCopy);
    let escape = InputEvent::KeyPressed {
        key: tairix_wm::Key::Named(tairix_wm::NamedKey::Escape),
        modifiers: tairix_wm::Modifiers::default(),
    };
    assert_eq!(
        shell.drag_key(&mut comp, &escape),
        Some(DragEnd::Save(SaveEnd {
            dropped: SaveDrop::Nothing,
            offer: Some(OfferEnd {
                window_id: 9,
                serial: 1,
                dropped: None
            }),
        }))
    );
}

/// The window the save is for going ends the save, owing the window it was
/// offered to the end; the window offered going is forgotten, and the save
/// carries on.
#[test]
fn windows_going_end_or_forget_a_save_as_they_concern_it() {
    let (mut shell, mut comp) = saving();
    over(
        &mut shell,
        &mut comp,
        Point::new(110, 110),
        offer_at(9, 10, 10),
    );
    let _ = save_report(&mut shell);
    assert!(shell.save_offer_answered(&mut comp, 9, 1, true));
    assert_eq!(shell.abort_drag_for(&mut comp, 9), None);
    assert!(shell.drag_active(), "the save carries on");
    assert_eq!(shell.cursor().kind(), CursorKind::Arrow);
    assert_eq!(
        shell.drag_pointer(&mut comp, &RELEASE),
        DragStep::Ended(DragEnd::Save(SaveEnd {
            dropped: SaveDrop::Nothing,
            offer: None,
        })),
        "the window gone hears nothing"
    );

    let (mut shell, mut comp) = saving();
    over(
        &mut shell,
        &mut comp,
        Point::new(110, 110),
        offer_at(9, 10, 10),
    );
    let _ = save_report(&mut shell);
    assert_eq!(
        shell.abort_drag_for(&mut comp, 7),
        Some(OfferEnd {
            window_id: 9,
            serial: 1,
            dropped: None
        })
    );
    assert!(!shell.drag_active());
}
