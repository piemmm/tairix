//! Host tests for the seat's drag carrier.

use tairix_abi::fs::{DocumentStamp, FileId};
use tairix_abi::time::Time64;
use tairix_abi::window_ipc::{
    BundleRunPath, DocumentName, DragItems, DragOwnIcon, DragOwnSource, DragPicture, DropOperation,
    DropTarget, IconAsset, OfferDrop, OfferedItem,
};
use tairix_abi::Errno;
use tairix_browse::render::Tiles;
use tairix_geometry::Rect;
use tairix_icon::{ArtworkKey, BundleVersion, IconKind};
use tairix_theme::CursorKind;
use tairix_window::DragConclusion;
use tairix_wm::{Compositor, InputEvent, Point, WindowId};

use alloc::string::String;
use alloc::vec::Vec;

use crate::drag::{
    DesktopCargo, DesktopDrop, DesktopEnd, DesktopItem, DesktopReport, DragEnd, DragStep,
    DragSurface, DropRefusal, ItemsEnd, ItemsOffer, ItemsOfferEnd, ItemsReport, OfferEnd,
    OwedReport, SaveDrop, SaveEnd, SaveOffer, SaveReport, SavedDocument, SlotAnswer,
};
use crate::picker::PickId;
use crate::shell::DesktopShell;
use crate::tests::{
    app_slot_point, compositor, deferred_artwork, moved, opaque_window, shell, PRIMARY_PRESS,
    SECONDARY_PRESS,
};

/// The pick the tests' saves are carried out of.
fn saved() -> PickId {
    PickId::numbered(1)
}

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

/// A picture of the class `asset`, its item's picture centred `from` the press.
fn picture(asset: &str, from: Option<(i32, i32)>) -> DragPicture {
    DragPicture {
        icon: IconAsset::new(asset).expect("a valid asset id"),
        from,
        own: None,
    }
}

/// One openable file named `name`.
fn file(name: &str) -> DragItems {
    DragItems::new(
        DocumentName::new(name).expect("a valid name"),
        1,
        true,
        [picture("text", None)],
    )
    .expect("one file")
}

/// Several items.
fn several() -> DragItems {
    DragItems::new(
        DocumentName::new("a.txt").expect("a valid name"),
        3,
        false,
        [
            picture("text", Some((0, 0))),
            picture("folder", Some((90, 0))),
            picture("image-png", None),
        ],
    )
    .expect("three items")
}

/// The version every item these drags carry its own picture of was listed at.
const STAMP: DocumentStamp = DocumentStamp {
    size: 4096,
    modified: Time64::from_secs(1_700_000_000),
    id: FileId {
        volume: [7; 16],
        node: 42,
    },
    content_gen: 3,
};

/// A picture of the class `kind`, carrying the item's own read from `path` as
/// `source` says, listed at [`STAMP`].
fn own_picture(kind: IconKind, source: DragOwnSource, path: &str) -> DragPicture {
    DragPicture {
        own: Some(DragOwnIcon::new(source, path, Some(STAMP)).expect("an own picture")),
        ..picture(kind.asset_id(), None)
    }
}

/// Every decode the shell has asked `artwork` for since it was installed: the
/// icons, then the thumbnails.
fn asked(artwork: &core::cell::RefCell<tairix_icon::ArtworkDesk>) -> Vec<ArtworkKey> {
    let mut desk = artwork.borrow_mut();
    let mut asked = Vec::new();
    while let Some(job) = desk.next_job() {
        asked.push(job.key);
    }
    while let Some(job) = desk.next_thumbnail() {
        asked.push(job.key);
    }
    asked
}

/// A lone bundle is carried as its own picture: its card asks for the icon its
/// own bundle declares, at the version listed, rather than its class's.
#[test]
fn a_lone_bundle_card_asks_for_its_own_bundle() {
    let (mut shell, mut comp, window) = pressed();
    let artwork = deferred_artwork(&mut shell);
    let editor = "/Users/ann/Desktop/Editor.app";
    let items = DragItems::new(
        DocumentName::new("Editor.app").expect("a valid name"),
        1,
        false,
        [own_picture(
            IconKind::AppBundle,
            DragOwnSource::Bundle,
            editor,
        )],
    )
    .expect("one bundle");
    shell
        .begin_drag(&mut comp, 7, window, items)
        .expect("carried");
    let bundle = ArtworkKey::Bundle {
        dir: String::from(editor),
        version: Some(BundleVersion::Listed(STAMP)),
    };
    assert_eq!(asked(&artwork), [bundle]);
}

/// Several items carried together each ask for their own picture where they
/// carry one — an application and a command, each its own bundle's — and the
/// file and the folder beside them for no own picture at all.
#[test]
fn a_mixed_stack_asks_own_pictures_only_for_its_bundles() {
    let (mut shell, mut comp, window) = pressed();
    let artwork = deferred_artwork(&mut shell);
    let (editor, ls) = ("/Users/ann/Desktop/Editor.app", "/System/Commands/ls.app");
    let items = DragItems::new(
        DocumentName::new("Editor.app").expect("a valid name"),
        4,
        false,
        [
            own_picture(IconKind::AppBundle, DragOwnSource::Bundle, editor),
            picture(IconKind::Text.asset_id(), Some((90, 0))),
            own_picture(IconKind::AppBundle, DragOwnSource::Bundle, ls),
            picture(IconKind::Folder.asset_id(), None),
        ],
    )
    .expect("four items");
    shell
        .begin_drag(&mut comp, 7, window, items)
        .expect("carried");
    let mut own: Vec<ArtworkKey> = asked(&artwork)
        .into_iter()
        .filter(|key| !matches!(key, ArtworkKey::Raster(_) | ArtworkKey::Vector(_)))
        .collect();
    own.sort();
    let bundle = |dir: &str| ArtworkKey::Bundle {
        dir: String::from(dir),
        version: Some(BundleVersion::Listed(STAMP)),
    };
    let mut expected = alloc::vec![bundle(editor), bundle(ls)];
    expected.sort();
    assert_eq!(own, expected);
}

/// A RISC OS application's sprite is produced among the thumbnails, which a
/// sweep withdraws when nothing asked for them since the last: the carried
/// stack asks for its cards before each sweep, so the sprite stays wanted for
/// as long as it is carried, and goes once it is not.
#[test]
fn a_sweep_keeps_a_carried_sprite() {
    let (mut shell, mut comp, window) = pressed();
    let artwork = deferred_artwork(&mut shell);
    let app = "/Storage/disc/!Edit";
    let items = DragItems::new(
        DocumentName::new("!Edit").expect("a valid name"),
        1,
        false,
        [own_picture(
            IconKind::Folder,
            DragOwnSource::RiscOsApplication,
            app,
        )],
    )
    .expect("one application");
    shell
        .begin_drag(&mut comp, 7, window, items)
        .expect("carried");
    while artwork.borrow_mut().next_job().is_some() {}
    for _ in 0..3 {
        shell.want_drag_artwork(&comp);
        artwork.borrow_mut().sweep_thumbnails();
    }
    assert!(
        artwork.borrow().has_work(),
        "the carried sprite was withdrawn"
    );

    let _ = shell.end_drag(&mut comp, false);
    shell.want_drag_artwork(&comp);
    artwork.borrow_mut().sweep_thumbnails();
    assert!(
        !artwork.borrow().has_work(),
        "a sprite nothing shows is kept"
    );
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

/// What is carried follows and never takes the pointer, and a slot that takes
/// the file is where a release drops it.
#[test]
fn what_is_carried_follows_and_a_slot_that_takes_the_file_is_dropped_on() {
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
    shell.settle(&mut comp);
    let carried: Vec<WindowId> = shell.drag_windows().collect();
    assert!(
        carried.iter().any(|window| comp
            .window(*window)
            .is_some_and(|shown| shown.bounds().contains(slot))),
        "the tile came with the pointer"
    );
    assert_eq!(
        comp.window_at(probe),
        beneath,
        "what is carried never becomes what is under the pointer"
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

/// One item is carried as its listed tile, held under the pointer, and
/// several as a stack of their pictures, a window each; ending the drag takes
/// every one down.
#[test]
fn one_item_is_carried_as_its_tile_and_several_as_a_stack() {
    let (mut shell, mut comp, window) = pressed();
    shell
        .begin_drag(&mut comp, 7, window, file("notes.txt"))
        .expect("carried");
    let one: Vec<WindowId> = shell.drag_windows().collect();
    assert_eq!(one.len(), 1);
    let metrics = Tiles::Large.metrics(comp.scale(), shell.session().active_theme());
    let shown = comp.window(one[0]).expect("shown").bounds();
    assert_eq!(
        (shown.width, shown.height),
        (metrics.cell_width, metrics.cell_height),
        "a listed tile's size"
    );
    assert!(
        shown.contains(shell.router().pointer()),
        "held under the pointer"
    );
    let _ = shell.end_drag(&mut comp, false);
    assert!(comp.window(one[0]).is_none());

    let (mut shell, mut comp, window) = pressed();
    shell
        .begin_drag(&mut comp, 7, window, several())
        .expect("carried");
    let stack: Vec<WindowId> = shell.drag_windows().collect();
    assert_eq!(stack.len(), 3, "a card for each item shown");
    for card in &stack {
        let shown = comp.window(*card).expect("shown").bounds();
        assert!(
            shown.width < metrics.cell_width,
            "smaller than a listed tile"
        );
    }
    let _ = shell.end_drag(&mut comp, false);
    assert!(stack.iter().all(|card| comp.window(*card).is_none()));
}

/// Artwork landing repaints the cards whose picture it moved, and no other
/// part of what is carried.
///
/// Regression: every landing painted every card afresh.
#[test]
fn a_landing_repaints_only_the_cards_whose_picture_it_moved() {
    let (mut shell, mut comp, window) = pressed();
    let desk = crate::tests::deferred_artwork(&mut shell);
    shell
        .begin_drag(&mut comp, 7, window, several())
        .expect("carried");
    let cards: Vec<Rect> = shell
        .drag_windows()
        .filter_map(|card| comp.window(card).map(tairix_wm::Window::bounds))
        .collect();
    assert_eq!(cards.len(), 3);
    let _ = comp.composite();
    let nothing = desk.borrow_mut().take_landed();
    shell.present_icon_artwork(&mut comp, &nothing);
    assert!(
        !comp.has_damage(),
        "a landing that moves nothing paints nothing"
    );
    let folder = crate::tests::land(&desk, |job| {
        matches!(
            job.key,
            tairix_icon::ArtworkKey::Raster(tairix_icon::IconKind::Folder)
                | tairix_icon::ArtworkKey::Vector(tairix_icon::IconKind::Folder)
        )
    });
    shell.present_icon_artwork(&mut comp, &folder);
    let damage = comp.composite();
    let painted = |card: &Rect| {
        damage
            .rects()
            .iter()
            .any(|rect| !rect.intersection(card).is_empty())
    };
    assert!(painted(&cards[1]), "the folder's card is painted afresh");
    assert!(
        !painted(&cards[0]) && !painted(&cards[2]),
        "the others keep their pixels"
    );
}

/// A theme switched under a drag lays what is carried out afresh in the new
/// theme, still under the pointer.
///
/// Regression: the carried plate kept the look it was first drawn in.
#[test]
fn a_theme_switched_under_a_drag_lays_it_out_afresh() {
    let (mut shell, mut comp, window) = pressed();
    shell
        .begin_drag(&mut comp, 7, window, file("notes.txt"))
        .expect("carried");
    let before: Vec<WindowId> = shell.drag_windows().collect();
    let dark = shell.session().active_theme().name() == tairix_theme::Theme::dark().name();
    let next = if dark {
        tairix_theme::ThemeId::LIGHT
    } else {
        tairix_theme::ThemeId::DARK
    };
    shell
        .session_mut()
        .set_theme(next)
        .expect("a built-in theme");
    assert!(shell.sync_theme(&mut comp));
    let after: Vec<WindowId> = shell.drag_windows().collect();
    assert_eq!(after.len(), 1);
    assert_ne!(after, before, "drawn afresh");
    assert!(before.iter().all(|window| comp.window(*window).is_none()));
    let shown = comp.window(after[0]).expect("shown").bounds();
    assert!(shown.contains(shell.router().pointer()));
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

/// The desktop folder under a resting pointer resolved again from the
/// listing — its icon re-sorted to another index, at another revision — asks
/// nothing again, and lights the folder where it now stands.
#[test]
fn a_desktop_folder_resolved_again_owes_no_second_report() {
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
    shell.drag_over(
        &mut comp,
        DragSurface::Desktop {
            folder: String::from("/Users/ann/Desktop/Work"),
            icon: Some(3),
            revision: 1,
        },
    );
    assert_eq!(shell.take_drag_report(), None, "the same folder");
    assert!(shell.drag_verdict(&mut comp, 7, first.serial, Some(DropOperation::Copy)));
    assert_eq!(shell.drag_drop_icon(), Some(3), "lit where it now stands");
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
        [picture("text", None)],
    )
    .expect("items");
    assert_eq!(super::item_label(&items), "a\u{fffd}b.txt");
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
        grab: super::PictureGrab::CENTRE,
    }
}

/// A save carried out of the panel window `panel` for the window 7 asked
/// in.
fn saving() -> (DesktopShell, Compositor) {
    let (mut shell, mut comp, panel) = pressed();
    shell
        .begin_save_drag(&mut comp, (panel, saved(), 7), report_doc())
        .expect("carried");
    (shell, comp)
}

/// A save is carried as the document's listed tile — its name cut as a
/// listing cuts it, its picture through the artwork tier — held where the
/// press held the panel's picture.
///
/// Regression: the save's plate drew its picture as a tinted glyph beside a
/// name cut at its end, losing the extension.
#[test]
fn a_save_is_carried_as_the_documents_tile_held_where_it_was_pressed() {
    let (mut shell, mut comp, panel) = pressed();
    let grab = super::PictureGrab {
        offset: (8, -6),
        side: 32,
    };
    let document = SavedDocument {
        grab,
        ..report_doc()
    };
    shell
        .begin_save_drag(&mut comp, (panel, saved(), 7), document)
        .expect("carried");
    let shown: Vec<WindowId> = shell.drag_windows().collect();
    assert_eq!(shown.len(), 1);
    let theme = shell.session().active_theme();
    let metrics = Tiles::Large.metrics(comp.scale(), theme);
    let tile = comp.window(shown[0]).expect("shown").bounds();
    assert_eq!(
        (tile.width, tile.height),
        (metrics.cell_width, metrics.cell_height)
    );
    let pointer = shell.router().pointer();
    let picture = Tiles::Large
        .layout()
        .parts(
            Rect::new(0, 0, tile.width, tile.height),
            comp.scale(),
            theme,
            "Report.txt",
            tairix_browse::render::name_cut("Report.txt"),
        )
        .and_then(|parts| parts.picture)
        .expect("a picture");
    let centre = Point::new(
        tile.left() + picture.left() + tairix_geometry::to_i32(picture.width / 2),
        tile.top() + picture.top() + tairix_geometry::to_i32(picture.height / 2),
    );
    assert!(
        pointer.x > centre.x && pointer.y < centre.y,
        "held right of and above its centre, as the panel's picture was"
    );
}

fn offer_at(window_id: u64, x: u32, y: u32) -> DragSurface {
    DragSurface::Offer { window_id, x, y }
}

/// The offers the carried save owes.
fn save_report(shell: &mut DesktopShell) -> Option<SaveReport> {
    match shell.take_drag_report()? {
        OwedReport::Save(report) => Some(report),
        other @ (OwedReport::Items(_) | OwedReport::Desktop(_)) => {
            panic!("a save owes save reports: {other:?}")
        }
    }
}

/// A drag numbers its offers on from the last drag's, so a late answer to an
/// offer of a save the user gave up on is never taken for the next one's.
#[test]
fn an_answer_to_an_earlier_drag_is_never_taken_for_the_next_ones() {
    let (mut shell, mut comp, panel) = pressed();
    let offered = |shell: &mut DesktopShell, comp: &mut Compositor| {
        shell
            .begin_save_drag(comp, (panel, saved(), 7), report_doc())
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
        shell.begin_save_drag(&mut comp, (elsewhere, saved(), 7), report_doc()),
        Err(Errno::PermissionDenied),
        "no press is held there"
    );
    shell
        .begin_save_drag(&mut comp, (panel, saved(), 7), report_doc())
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
            pick: saved(),
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
            pick: saved(),
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
            pick: saved(),
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
            pick: saved(),
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
            pick: saved(),
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
            pick: saved(),
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

/// A shell holding a primary press on the bare desktop, a window beside it.
fn pressed_on_desktop() -> (DesktopShell, Compositor) {
    let mut shell = shell();
    let mut comp = compositor();
    let _ = opaque_window(&mut comp, Point::new(100, 100), 120, 80);
    let _ = shell.handle(moved(40, 40), &mut comp, 0);
    let _ = shell.handle(PRIMARY_PRESS, &mut comp, 0);
    (shell, comp)
}

/// The node the desktop listed its `n`th icon as.
const fn listed(n: u64) -> FileId {
    FileId {
        volume: [4; 16],
        node: n,
    }
}

/// Three desktop icons, `a.txt` pressed.
fn desktop_icons() -> DesktopCargo {
    let item = |path: &str, n| DesktopItem {
        path: String::from(path),
        id: listed(n),
    };
    DesktopCargo {
        shown: several(),
        items: alloc::vec![
            item("/Users/ann/Desktop/a.txt", 1),
            item("/Users/ann/Desktop/Pictures", 2),
            item("/Users/ann/Desktop/b.png", 3),
        ],
    }
}

/// The offers icons carried off the desktop owe.
fn desktop_report(shell: &mut DesktopShell) -> Option<DesktopReport> {
    match shell.take_drag_report()? {
        OwedReport::Desktop(report) => Some(report),
        other @ (OwedReport::Items(_) | OwedReport::Save(_)) => {
            panic!("icons off the desktop owe their own reports: {other:?}")
        }
    }
}

/// Carry the desktop's icons over window 9 and take the offer made there.
fn offered_to_nine(shell: &mut DesktopShell, comp: &mut Compositor) -> ItemsOffer {
    shell
        .begin_desktop_drag(comp, desktop_icons())
        .expect("carried");
    over(shell, comp, Point::new(110, 110), offer_at(9, 10, 10));
    desktop_report(shell)
        .and_then(|report| report.over)
        .expect("offered")
}

/// The seat carries icons off the desktop only on from a held press, one
/// drag at a time.
#[test]
fn icons_are_carried_off_the_desktop_only_from_a_held_press() {
    let mut shell = shell();
    let mut comp = compositor();
    let _ = shell.handle(moved(40, 40), &mut comp, 0);
    assert_eq!(
        shell.begin_desktop_drag(&mut comp, desktop_icons()),
        Err(Errno::PermissionDenied),
        "a drag is a press carried on"
    );
    let _ = shell.handle(PRIMARY_PRESS, &mut comp, 0);
    shell
        .begin_desktop_drag(&mut comp, desktop_icons())
        .expect("carried");
    assert!(shell.carries_desktop_icons());
    assert_eq!(
        shell.begin_desktop_drag(&mut comp, desktop_icons()),
        Err(Errno::AlreadyExists),
        "one drag at a time"
    );
    assert!(shell.drag_items().is_none() && shell.save_source().is_none());
}

/// Icons off the desktop held over a window are offered to it, its
/// application's answer is shown, and only that window reads them by the
/// drag's number; dropped there, it may still read them, and nothing else may.
#[test]
fn icons_off_the_desktop_are_offered_to_the_window_under_them_alone() {
    let (mut shell, mut comp) = pressed_on_desktop();
    let offer = offered_to_nine(&mut shell, &mut comp);
    assert_eq!(
        (offer.window_id, offer.x, offer.y, offer.shift),
        (9, 10, 10, false)
    );
    assert_eq!(
        shell.offered_item(9, offer.drag, 0),
        Ok(OfferedItem {
            path: "/Users/ann/Desktop/a.txt",
            id: listed(1),
        }),
        "the pressed icon first"
    );
    assert_eq!(
        shell.offered_item(9, offer.drag, 2).map(|item| item.path),
        Ok("/Users/ann/Desktop/b.png")
    );
    assert_eq!(shell.offered_items(9, offer.drag).map(<[_]>::len), Ok(3));
    assert_eq!(shell.offered_item(9, offer.drag, 3), Err(Errno::NotFound));
    assert_eq!(
        shell.offered_items(8, offer.drag),
        Err(Errno::NotFound),
        "another window reads nothing"
    );
    assert_eq!(
        shell.offered_items(9, offer.drag.wrapping_add(1)),
        Err(Errno::NotFound),
        "nor does another drag's number"
    );
    assert!(shell.items_offer_answered(&mut comp, 9, offer.serial, Some(DropOperation::Move)));
    assert_eq!(shell.cursor().kind(), CursorKind::DragMove);
    assert_eq!(
        shell.drag_pointer(&mut comp, &RELEASE),
        DragStep::Ended(DragEnd::Desktop(DesktopEnd {
            dropped: DesktopDrop::Window,
            offer: Some(ItemsOfferEnd {
                window_id: 9,
                serial: offer.serial,
                dropped: Some(OfferDrop {
                    x: 10,
                    y: 10,
                    operation: DropOperation::Move,
                }),
            }),
        }))
    );
    assert_eq!(
        shell.offered_item(9, offer.drag, 1),
        Ok(OfferedItem {
            path: "/Users/ann/Desktop/Pictures",
            id: listed(2),
        }),
        "the window dropped on reads what it was given"
    );
    assert_eq!(shell.offered_items(8, offer.drag), Err(Errno::NotFound));
    let _ = shell.abort_drag_for(&mut comp, 9);
    assert_eq!(
        shell.offered_items(9, offer.drag),
        Err(Errno::NotFound),
        "until it closes"
    );
}

/// A window let go over before its application answered is told the offer
/// ended with nothing dropped, and keeps no way to read the icons.
#[test]
fn icons_let_go_over_a_window_yet_to_answer_drop_nothing() {
    let (mut shell, mut comp) = pressed_on_desktop();
    let offer = offered_to_nine(&mut shell, &mut comp);
    assert_eq!(
        shell.drag_pointer(&mut comp, &RELEASE),
        DragStep::Ended(DragEnd::Desktop(DesktopEnd {
            dropped: DesktopDrop::Nothing,
            offer: Some(ItemsOfferEnd {
                window_id: 9,
                serial: offer.serial,
                dropped: None,
            }),
        }))
    );
    assert_eq!(shell.offered_items(9, offer.drag), Err(Errno::NotFound));
}

/// An answer to an offer the icons have moved on from is not shown, and a
/// refusal shows no verdict.
#[test]
fn a_late_answer_to_an_offer_of_icons_is_not_shown() {
    let (mut shell, mut comp) = pressed_on_desktop();
    let offer = offered_to_nine(&mut shell, &mut comp);
    assert!(shell.items_offer_answered(&mut comp, 9, offer.serial, None));
    assert_eq!(shell.cursor().kind(), CursorKind::Arrow, "refused");
    over(
        &mut shell,
        &mut comp,
        Point::new(500, 400),
        offer_at(4, 3, 3),
    );
    let report = desktop_report(&mut shell).expect("owed");
    assert_eq!(
        report.left,
        Some(ItemsOfferEnd {
            window_id: 9,
            serial: offer.serial,
            dropped: None,
        }),
        "the window left is told"
    );
    assert!(
        !shell.items_offer_answered(&mut comp, 9, offer.serial, Some(DropOperation::Copy)),
        "an answer from the window left is not shown"
    );
    let now = report.over.expect("offered to the next");
    assert!(
        !shell.items_offer_answered(&mut comp, 4, offer.serial, Some(DropOperation::Copy)),
        "nor one naming the offer before"
    );
    assert!(shell.items_offer_answered(&mut comp, 4, now.serial, Some(DropOperation::Copy)));
    assert_eq!(shell.cursor().kind(), CursorKind::DragCopy);
}

/// Brought back over the desktop the icons are put down where let go, and the
/// window they left is told.
#[test]
fn icons_brought_back_over_the_desktop_are_put_down_where_let_go() {
    let (mut shell, mut comp) = pressed_on_desktop();
    let offer = offered_to_nine(&mut shell, &mut comp);
    over(
        &mut shell,
        &mut comp,
        Point::new(700, 500),
        desktop("/Users/ann/Desktop", None),
    );
    assert_eq!(shell.cursor().kind(), CursorKind::DragMove);
    assert_eq!(
        desktop_report(&mut shell),
        Some(DesktopReport {
            left: Some(ItemsOfferEnd {
                window_id: 9,
                serial: offer.serial,
                dropped: None,
            }),
            over: None,
        })
    );
    assert_eq!(
        shell.drag_pointer(&mut comp, &RELEASE),
        DragStep::Ended(DragEnd::Desktop(DesktopEnd {
            dropped: DesktopDrop::Desktop(Point::new(700, 500)),
            offer: None,
        }))
    );
}

/// `Shift` asks the window under the icons afresh, the answer given for the
/// other modifier forgotten.
#[test]
fn shift_offers_icons_off_the_desktop_afresh() {
    let (mut shell, mut comp) = pressed_on_desktop();
    let offer = offered_to_nine(&mut shell, &mut comp);
    assert!(shell.items_offer_answered(&mut comp, 9, offer.serial, Some(DropOperation::Move)));
    let shifted = InputEvent::ModifiersChanged {
        modifiers: tairix_wm::Modifiers {
            shift: true,
            ..tairix_wm::Modifiers::default()
        },
    };
    assert_eq!(shell.drag_key(&mut comp, &shifted), None);
    assert_eq!(shell.cursor().kind(), CursorKind::Arrow, "asked afresh");
    let again = desktop_report(&mut shell)
        .and_then(|report| report.over)
        .expect("offered again");
    assert!(again.shift && again.serial != offer.serial);
    assert!(!shell.items_offer_answered(&mut comp, 9, offer.serial, Some(DropOperation::Move)));
}

/// A window closing under icons offered it forgets them: the drag goes on,
/// and a release then drops nothing and tells it nothing.
#[test]
fn a_window_closing_under_icons_offered_it_forgets_them() {
    let (mut shell, mut comp) = pressed_on_desktop();
    let offer = offered_to_nine(&mut shell, &mut comp);
    assert_eq!(shell.abort_drag_for(&mut comp, 9), None);
    assert!(shell.drag_active(), "the drag goes on");
    assert_eq!(shell.offered_items(9, offer.drag), Err(Errno::NotFound));
    assert_eq!(
        shell.drag_pointer(&mut comp, &RELEASE),
        DragStep::Ended(DragEnd::Desktop(DesktopEnd {
            dropped: DesktopDrop::Nothing,
            offer: None,
        }))
    );
}
