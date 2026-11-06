//! Host tests for the file manager's icon-bar declaration.
//!
//! What the component's slot offers is a pure function of the places rail, so
//! all of it is exercised here without a kernel: which places become rows, in
//! what order, where the volume rule falls, what the row cap drops, and how a
//! chosen row maps back to the place the user saw.

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use tairix_abi::blkio::{BlkMedium, BlkRotation, BlkRotationEvidence, BlkTransport};
use tairix_abi::driver::filesystem::{VolumeTitle, VOLUME_TITLE_MAX};
use tairix_abi::sysinfo::VolumeClass;

use tairix_abi::window_ipc::{
    AppBar, AppMenuIconView, AppMenuItemId, AppMenuRowView, AppMenuSymbol, APP_MENU_LABEL_MAX,
    APP_MENU_MAX_ROWS,
};
use tairix_browse::mount::{MountAct, MountRefusal, Titling, TITLE_CONTROL, TITLE_TOO_LONG};
use tairix_browse::places::{REASON_NOT_YOURS, REASON_TITLE_READ_ONLY, REASON_UNTITLED_FORMAT};
use tairix_browse::{Places, Retitler, Volume};

use super::{
    component_declaration, BarAnswer, BarRow, Declarations, CLOSING_ROWS, DECLARATIONS_HELD,
    DESKTOP_SLOT_CLICK, NO_HOME, OUTDATED, VOLUME_GONE,
};
use crate::test_fs::mounted;

/// A USB unit that reported a non-rotating medium: served as removable.
const STICK: BlkMedium = BlkMedium::of_transport(BlkTransport::Usb).with_rotation(
    BlkRotation::NonRotating(BlkRotationEvidence::RotationRateVpd),
);

/// The user the file manager runs as.
const VIEWER: Option<u32> = Some(1000);

/// A home whose two derived places (Home, Desktop) open the rail.
fn home() -> Vec<String> {
    alloc::vec!["Users".to_string(), "ada".to_string()]
}

/// A volume attached at `/Storage/<label>` with no reported medium.
fn volume(label: &str) -> Volume {
    mounted(label, &alloc::format!("/Storage/{label}"), None)
}

/// The kinds a declaration's rows carried, in order.
fn kinds(places: &Places) -> Vec<AppMenuRowKind> {
    let (bar, _) = component_declaration(7, places, VIEWER, 0).expect("the rows fit");
    bar.menu
        .rows()
        .map(|(row, _)| AppMenuRowKind::of(row))
        .collect()
}

/// The labels of a declaration's place rows, in order.
fn labels(places: &Places) -> Vec<String> {
    let (bar, _) = component_declaration(7, places, VIEWER, 0).expect("the rows fit");
    place_labels(&bar)
}

/// The labels of the closing *Network* and *Trash* rows.
const NETWORK: &str = "Network";
const TRASH: &str = "Trash";

/// The labels of `bar`'s place rows, in order: every item but the closing
/// *Network* and *Trash*.
fn place_labels(bar: &AppBar) -> Vec<String> {
    let mut shown = item_labels(bar);
    assert_eq!(shown.pop().as_deref(), Some(TRASH), "Trash is last");
    assert_eq!(shown.pop().as_deref(), Some(NETWORK), "Network is above it");
    shown
}

/// The labels of `bar`'s item rows, in order.
fn item_labels(bar: &AppBar) -> Vec<String> {
    bar.menu
        .rows()
        .filter_map(|(row, _)| match row {
            AppMenuRowView::Item(item) => Some(item.label.to_string()),
            _ => None,
        })
        .collect()
}

/// Which kind a row is, so a test can assert over kinds without holding a
/// borrow of the menu that reported them.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum AppMenuRowKind {
    Item,
    Separator,
    Submenu,
    Info,
}

impl AppMenuRowKind {
    fn of(row: AppMenuRowView<'_>) -> Self {
        match row {
            AppMenuRowView::Item(_) => Self::Item,
            AppMenuRowView::Separator => Self::Separator,
            AppMenuRowView::Submenu { .. } => Self::Submenu,
            AppMenuRowView::Info => Self::Info,
        }
    }
}

/// The slot stays with every window closed, so a click on it must be able to
/// bring one back.
#[test]
fn the_slot_can_always_produce_a_window() {
    assert!(DESKTOP_SLOT_CLICK.opens_when_windowless());
}

#[test]
fn the_component_offers_the_places_and_neither_of_the_conventions_rows() {
    let places = Places::new(&home(), &[]);
    let (bar, skipped) = component_declaration(7, &places, VIEWER, 3).expect("the rows fit");
    assert_eq!(bar.event_endpoint, 7);
    assert_eq!(bar.revision, 3, "made under the revision it was asked for");
    assert_eq!(
        bar.click, DESKTOP_SLOT_CLICK,
        "a click on a component's slot opens a window rather than raising one"
    );
    assert_eq!(skipped, 0);
    // A component states no identity panel of its own and is not the user's to
    // quit, so neither convention row is declared.
    assert!(
        !kinds(&places).contains(&AppMenuRowKind::Info),
        "a component declares no information row"
    );
    assert!(
        !item_labels(&bar).iter().any(|label| label == "Quit"),
        "a component declares no Quit row"
    );
    // Every row is the rail's, in the rail's own order.
    assert_eq!(
        labels(&places),
        alloc::vec!["Home".to_string(), "Desktop".to_string()]
    );
    assert!(
        bar.menu.rows().all(|(_, parent)| parent.is_none()),
        "the component declares no submenu"
    );
}

/// Each row draws the picture the rail draws its place with: a folder for the
/// user's own places, and the drive a volume is on for each volume.
#[test]
fn each_row_draws_its_places_own_picture() {
    let mut usb = volume("Stick");
    usb.medium = Some(STICK);
    let places = Places::new(&home(), &[usb]);
    let (bar, _) = component_declaration(7, &places, VIEWER, 0).expect("the rows fit");
    let pictures: Vec<(String, AppMenuIconView<'_>)> = bar
        .menu
        .rows()
        .filter_map(|(row, _)| match row {
            AppMenuRowView::Item(item) => Some((item.label.to_string(), item.icon)),
            _ => None,
        })
        .collect();
    for (place, (label, icon)) in places.rows().iter().zip(&pictures) {
        assert_eq!(place.label(), label);
        assert_eq!(
            *icon,
            AppMenuIconView::Asset(place.icon().asset_id()),
            "{label} draws the rail's picture"
        );
    }
    assert_eq!(
        pictures.get(places.len() - 1).map(|(_, icon)| *icon),
        Some(AppMenuIconView::Asset("disk-usb")),
        "a removable volume draws its drive"
    );
}

#[test]
fn a_rule_opens_the_mounted_volumes_and_only_when_there_are_some() {
    // Nothing mounted: no divider among the places, because there is nothing
    // to divide — only the closing rows' own two.
    let bare = Places::new(&home(), &[]);
    let rules = |kinds: &[AppMenuRowKind]| {
        kinds
            .iter()
            .filter(|kind| **kind == AppMenuRowKind::Separator)
            .count()
    };
    assert_eq!(rules(&kinds(&bare)), 2, "no volumes, no rule among them");

    // Mounted: the rule falls exactly where the volume rows begin, and the
    // volumes follow the user's own places.
    let mounted = Places::new(&home(), &[volume("Backup"), volume("Stick")]);
    let declared = kinds(&mounted);
    let rule = declared
        .iter()
        .position(|kind| *kind == AppMenuRowKind::Separator)
        .expect("the rule is declared");
    let volume_start = mounted.volume_start().expect("a volume row exists");
    assert_eq!(
        rule, volume_start,
        "one divider, at the rail's own volume boundary"
    );
    assert_eq!(
        rules(&declared),
        3,
        "one rule for the volumes, not one per volume, and the closing rows' two"
    );
    // Sorted by label, after the fixed places.
    assert_eq!(
        labels(&mounted)[2..],
        ["Backup".to_string(), "Stick".to_string()]
    );
}

#[test]
fn a_chosen_row_names_the_place_the_user_saw() {
    let places = Places::new(&home(), &[volume("Backup")]);
    let (bar, _) = component_declaration(7, &places, VIEWER, 0).expect("the rows fit");
    for (row, _) in bar.menu.rows() {
        let AppMenuRowView::Item(item) = row else {
            continue;
        };
        let Some(BarRow::Place(index)) = BarRow::of(item.id) else {
            continue;
        };
        assert_eq!(
            places.rows()[index].label(),
            item.label,
            "the row resolves to the place whose label it drew"
        );
    }
    assert_eq!(
        BarRow::of(AppMenuItemId::new(1).expect("non-zero")),
        Some(BarRow::Place(0))
    );
    let Some(BarRow::Place(past_the_end)) = BarRow::of(AppMenuItemId::new(999).expect("non-zero"))
    else {
        panic!("the id names a place");
    };
    assert!(
        places.rows().get(past_the_end).is_none(),
        "an index past the rail is the caller's to reject, and it is out of range"
    );
}

/// Every row's id names exactly the row it was declared for, and an id no
/// declaration carries names nothing.
#[test]
fn every_rows_id_names_it_back() {
    for row in [
        BarRow::Place(0),
        BarRow::Eject(0),
        BarRow::Title(3),
        BarRow::Retitled(30),
        BarRow::Place(super::VERB_STRIDE as usize - 2),
        BarRow::Network,
        BarRow::Trash,
    ] {
        assert_eq!(BarRow::of(row.id().expect("an id")), Some(row), "{row:?}");
    }
    assert!(
        BarRow::Eject(super::VERB_STRIDE as usize - 1).id().is_err(),
        "an index past its range has no id"
    );
    for raw in [
        super::VERB_STRIDE,
        2 * super::VERB_STRIDE,
        super::VERBS * super::VERB_STRIDE + 1,
    ] {
        assert_eq!(BarRow::of(AppMenuItemId::new(raw).expect("non-zero")), None);
    }
}

/// A volume of `class` at `/Storage/<label>`, attached for `owner` and
/// recording `title` in a format that takes one, or none in a format that
/// keeps none, published under the identity its label spells.
fn attached(label: &str, class: VolumeClass, owner: Option<u32>, title: &str) -> Volume {
    let mut volume = volume(label);
    volume.facts.class = class;
    volume.facts.owner = owner;
    volume.facts.title = title.to_string();
    volume.facts.titled = !title.is_empty();
    for (id, spelled) in volume.facts.volume_id.iter_mut().zip(label.bytes()) {
        *id = spelled;
    }
    volume
}

/// Where the place labelled `label` stands on `places`' rail.
fn index_of(places: &Places, label: &str) -> usize {
    places
        .rows()
        .iter()
        .position(|place| place.label() == label)
        .expect("listed")
}

/// The revision a declaration of `places` was made under, which the session
/// accepted.
fn accepted(declarations: &mut Declarations, places: &Places) -> u32 {
    let revision = declarations.next_revision();
    declarations.accepted(revision, places);
    revision
}

/// A plate row: what it names, whether it may be chosen, and its field's
/// initial text, where the caret stands and whether choosing opens it.
type PlateRow = (Option<BarRow>, bool, Option<(String, usize, bool)>);

/// The rows of the plate the place labelled `label` opens, each as what it
/// names, whether it may be chosen, and the field it carries.
fn plate_of(bar: &AppBar, label: &str) -> Vec<PlateRow> {
    let rows: Vec<_> = bar.menu.rows().collect();
    let parent = rows
        .iter()
        .position(|(row, _)| matches!(row, AppMenuRowView::Item(item) if item.label == label))
        .expect("the place is declared");
    rows.iter()
        .filter(|(_, under)| *under == Some(parent))
        .map(|(row, _)| match row {
            AppMenuRowView::Item(item) => (
                BarRow::of(item.id),
                item.enabled,
                item.entry.map(|entry| {
                    (
                        entry.initial.to_string(),
                        entry.selected.range().start,
                        entry.opens_on_choose,
                    )
                }),
            ),
            other => panic!("a plate row is an item, not {other:?}"),
        })
        .collect()
}

/// The name of the volume `answer` ejects.
fn ejected(answer: BarAnswer<'_>) -> String {
    match answer {
        BarAnswer::Mount(MountAct::Eject { name, .. }) => name,
        other => panic!("an eject, not {other:?}"),
    }
}

/// A menu answered after the rail moved acts on what its own declaration
/// showed — a place where it stood, a volume wherever the rail lists it now —
/// never on whatever slid into its index; one copied from the latest acts on
/// the latest.
#[test]
fn an_answer_acts_on_what_its_own_declaration_showed() {
    let card = attached("Card", VolumeClass::Removable, None, "");
    let before = Places::new(&home(), core::slice::from_ref(&card));
    let after = Places::new(
        &home(),
        &[attached("Alpha", VolumeClass::Removable, None, ""), card],
    );
    let mut declarations = Declarations::default();
    let first = accepted(&mut declarations, &before);
    let latest = accepted(&mut declarations, &after);
    let card_at = index_of(&before, "Card");
    assert_eq!(
        after.rows()[card_at].label(),
        "Alpha",
        "the rail re-sorted beneath the open menu"
    );
    let eject = BarRow::Eject(card_at).id().expect("an id");
    assert_eq!(
        ejected(declarations.chosen((eject, first), &after, VIEWER)),
        "Card",
        "the volume its row showed"
    );
    assert_eq!(
        ejected(declarations.chosen((eject, latest), &after, VIEWER)),
        "Alpha"
    );
    let open = BarRow::Place(card_at).id().expect("an id");
    assert_eq!(
        declarations.chosen((open, first), &after, VIEWER),
        BarAnswer::Open(before.rows()[card_at].components()),
        "a place where it stood"
    );
    let home_row = BarRow::Eject(0).id().expect("an id");
    assert_eq!(
        declarations.chosen((home_row, first), &after, VIEWER),
        BarAnswer::Nothing,
        "a row naming no volume"
    );
}

/// A declaration the session refused was never shown, so the menu up is the
/// one before it: its answers act on what it showed, and the refused one's
/// revision, fresh as every one is, names nothing.
#[test]
fn a_refused_declaration_moves_nothing_answers_resolve_against() {
    let card = attached("Card", VolumeClass::Removable, None, "");
    let before = Places::new(&home(), core::slice::from_ref(&card));
    let after = Places::new(
        &home(),
        &[attached("Alpha", VolumeClass::Removable, None, ""), card],
    );
    let mut declarations = Declarations::default();
    let shown = accepted(&mut declarations, &before);
    let refused = declarations.next_revision();
    assert_ne!(refused, shown, "each declaration is made under its own");
    let eject = BarRow::Eject(index_of(&before, "Card"))
        .id()
        .expect("an id");
    assert_eq!(
        ejected(declarations.chosen((eject, shown), &after, VIEWER)),
        "Card"
    );
    assert!(matches!(
        declarations.chosen((eject, refused), &after, VIEWER),
        BarAnswer::Refused(MountRefusal {
            reason: OUTDATED,
            ..
        })
    ));
}

/// Only the latest few declarations are held: an answer copied from one let
/// go is refused saying why, though the closing rows mean the same in every
/// declaration.
#[test]
fn an_answer_from_a_declaration_let_go_is_refused_saying_why() {
    let places = Places::new(
        &home(),
        &[attached("Card", VolumeClass::Removable, None, "")],
    );
    let mut declarations = Declarations::default();
    let revisions: Vec<u32> = (0..=DECLARATIONS_HELD)
        .map(|_| accepted(&mut declarations, &places))
        .collect();
    let card = index_of(&places, "Card");
    let answer = |row: BarRow, revision| {
        declarations.chosen((row.id().expect("an id"), revision), &places, VIEWER)
    };
    for row in [BarRow::Place(0), BarRow::Eject(card)] {
        assert!(
            matches!(
                answer(row, revisions[0]),
                BarAnswer::Refused(MountRefusal {
                    reason: OUTDATED,
                    ..
                })
            ),
            "{row:?}"
        );
    }
    assert_eq!(answer(BarRow::Network, revisions[0]), BarAnswer::Network);
    assert_eq!(answer(BarRow::Trash, revisions[0]), BarAnswer::Trash);
    assert!(
        matches!(
            answer(BarRow::Eject(card), revisions[1]),
            BarAnswer::Mount(_)
        ),
        "the oldest still held"
    );
}

/// An eject or a title chosen for a volume that has left since is refused
/// naming the volume the row showed, never taken without a word.
#[test]
fn a_volume_that_left_is_refused_naming_it() {
    let before = Places::new(
        &home(),
        &[attached("Floppy", VolumeClass::Image, VIEWER, "Workbench")],
    );
    let gone = Places::new(&home(), &[]);
    let mut declarations = Declarations::default();
    let revision = accepted(&mut declarations, &before);
    let at = index_of(&before, "Floppy");
    let eject = BarRow::Eject(at).id().expect("an id");
    assert_eq!(
        declarations.chosen((eject, revision), &gone, VIEWER),
        BarAnswer::Refused(MountRefusal {
            headline: String::from("\u{201c}Floppy\u{201d} could not be ejected"),
            reason: VOLUME_GONE,
        })
    );
    assert_eq!(
        declarations.retitled((at, revision), &gone, VIEWER, "Games"),
        Err(MountRefusal {
            headline: String::from("\u{201c}Floppy\u{201d} could not be retitled"),
            reason: VOLUME_GONE,
        })
    );
}

/// A committed title is checked against the volume as it stands now, by the
/// one check: the title it records asks nothing, an empty one clears it, one
/// longer than any disk records or holding a control character is refused
/// saying which, and one no longer the user's to retitle is refused saying why.
#[test]
fn a_committed_title_is_checked_against_the_disk_as_it_stands() {
    let floppy = attached("Floppy", VolumeClass::Image, VIEWER, "Workbench");
    let places = Places::new(&home(), core::slice::from_ref(&floppy));
    let mut declarations = Declarations::default();
    let row = (
        index_of(&places, "Floppy"),
        accepted(&mut declarations, &places),
    );
    let retitled = |text: &str, now: &Places| declarations.retitled(row, now, VIEWER, text);
    let retitle = |title: &str| MountAct::Retitle {
        volume_id: floppy.facts.volume_id,
        name: String::from("Floppy"),
        titling: alloc::boxed::Box::new(Titling {
            title: VolumeTitle::new(title).expect("a title"),
            retitler: Retitler::Owner(1000),
        }),
    };
    assert_eq!(
        retitled("Workbench", &places),
        Ok(None),
        "the title it records"
    );
    assert_eq!(retitled("Games", &places), Ok(Some(retitle("Games"))));
    assert_eq!(
        retitled("", &places),
        Ok(Some(retitle(""))),
        "an empty title clears it"
    );
    let reason = |text: &str, now: &Places| retitled(text, now).map_err(|refused| refused.reason);
    assert_eq!(
        reason(&"t".repeat(VOLUME_TITLE_MAX + 1), &places),
        Err(TITLE_TOO_LONG)
    );
    assert_eq!(
        reason("Work\u{7}bench", &places),
        Err(TITLE_CONTROL),
        "a control character is not a length"
    );
    let mut theirs = floppy.clone();
    theirs.facts.owner = Some(1001);
    let mut untitled = floppy.clone();
    untitled.facts.titled = false;
    for (changed, why) in [
        (theirs, REASON_NOT_YOURS),
        (untitled, REASON_UNTITLED_FORMAT),
    ] {
        assert_eq!(reason("Games", &Places::new(&home(), &[changed])), Err(why));
    }
}

/// Every volume the one rule lets the user retitle has its *Title* row: a
/// removable or fixed device's volume as much as a disk image of theirs, and
/// one whose format takes a title but records none yet; a share, another
/// user's disk image and a format that keeps no title have none.
#[test]
fn every_volume_the_rule_offers_has_its_title_row() {
    let mut blank = attached("Blank", VolumeClass::Image, VIEWER, "");
    blank.facts.titled = true;
    let mut share = attached("Share", VolumeClass::Network, VIEWER, "Projects");
    share.facts.titled = true;
    let places = Places::new(
        &home(),
        &[
            attached("Card", VolumeClass::Removable, None, "Holiday"),
            attached("Data", VolumeClass::Local, None, "Data"),
            blank,
            share,
            attached("Theirs", VolumeClass::Image, Some(1001), "Their Disk"),
            attached("Disc", VolumeClass::Removable, None, ""),
        ],
    );
    let (bar, _) = component_declaration(7, &places, VIEWER, 0).expect("the rows fit");
    let eject = |label: &str| (Some(BarRow::Eject(index_of(&places, label))), true, None);
    let title = |label: &str, text: &str| {
        let field = Some((text.to_string(), text.len(), true));
        (Some(BarRow::Title(index_of(&places, label))), true, field)
    };
    assert_eq!(
        plate_of(&bar, "Card"),
        [eject("Card"), title("Card", "Holiday")]
    );
    assert_eq!(
        plate_of(&bar, "Data"),
        [title("Data", "Data")],
        "an administrator is asked where the user may not"
    );
    assert_eq!(
        plate_of(&bar, "Blank"),
        [eject("Blank"), title("Blank", "")],
        "a title can be given"
    );
    assert_eq!(
        plate_of(&bar, "Share"),
        [eject("Share")],
        "a share's title is its server's"
    );
    assert!(plate_of(&bar, "Theirs").is_empty());
    assert_eq!(
        plate_of(&bar, "Disc"),
        [eject("Disc")],
        "a format keeping no title"
    );
}

/// A removable device's volume opens a plate offering *Eject*; a fixed one,
/// and another user's disk image, open none.
#[test]
fn a_removable_volume_offers_eject_on_its_plate() {
    let places = Places::new(
        &home(),
        &[
            attached("Card", VolumeClass::Removable, None, ""),
            attached("Data", VolumeClass::Local, None, ""),
            attached("Theirs", VolumeClass::Image, Some(1001), "Their Disk"),
        ],
    );
    let (bar, _) = component_declaration(7, &places, VIEWER, 0).expect("the rows fit");
    let card = places
        .rows()
        .iter()
        .position(|place| place.label() == "Card")
        .expect("listed");
    assert_eq!(
        plate_of(&bar, "Card"),
        [(Some(BarRow::Eject(card)), true, None)]
    );
    assert!(
        plate_of(&bar, "Data").is_empty(),
        "a fixed volume stays put"
    );
    assert!(
        plate_of(&bar, "Theirs").is_empty(),
        "another user's disk image is theirs to eject and title"
    );
    let (anonymous, _) = component_declaration(7, &places, None, 0).expect("the rows fit");
    assert!(
        plate_of(&anonymous, "Card").is_empty(),
        "nothing is offered to a user the kernel would not name"
    );
}

/// A disk image of the user's that records a title offers it in a field
/// holding the title with the caret at its end, opened by choosing the row
/// and answering as its own; read-only, the row says why it cannot.
#[test]
fn a_titled_image_offers_its_title_in_a_field() {
    let mut locked = attached("Locked", VolumeClass::Image, VIEWER, "Archive");
    locked.facts.read_only = true;
    let places = Places::new(
        &home(),
        &[
            attached("Floppy", VolumeClass::Image, VIEWER, "Workbench"),
            attached("Blank", VolumeClass::Image, VIEWER, ""),
            locked,
        ],
    );
    let (bar, _) = component_declaration(7, &places, VIEWER, 0).expect("the rows fit");
    let at = |label: &str| {
        places
            .rows()
            .iter()
            .position(|place| place.label() == label)
            .expect("listed")
    };
    let floppy = at("Floppy");
    assert_eq!(
        plate_of(&bar, "Floppy"),
        [
            (Some(BarRow::Eject(floppy)), true, None),
            (
                Some(BarRow::Title(floppy)),
                true,
                Some(("Workbench".to_string(), "Workbench".len(), true))
            ),
        ]
    );
    let entry_id = bar
        .menu
        .rows()
        .find_map(|(row, _)| match row {
            AppMenuRowView::Item(item) if item.label == "Title" => item.entry.map(|e| e.id),
            _ => None,
        })
        .expect("the field");
    assert_eq!(BarRow::of(entry_id), Some(BarRow::Retitled(floppy)));
    assert_eq!(
        plate_of(&bar, "Blank"),
        [(Some(BarRow::Eject(at("Blank"))), true, None)],
        "a format keeping no title has none to change"
    );
    let reason = bar
        .menu
        .rows()
        .filter_map(|(row, _)| match row {
            AppMenuRowView::Item(item) if item.label == "Title" && !item.enabled => {
                Some(item.reason.to_string())
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        plate_of(&bar, "Locked")[1],
        (Some(BarRow::Title(at("Locked"))), false, None)
    );
    assert_eq!(reason, [REASON_TITLE_READ_ONLY.to_string()]);
}

#[test]
fn a_label_the_menus_bounds_refuse_is_skipped_and_counted() {
    // A volume label the rail accepts but a menu row cannot hold: skipped
    // rather than truncated into something that reads like another volume,
    // and counted so the caller can say some are not shown.
    let long = "v".repeat(APP_MENU_LABEL_MAX + 1);
    let places = Places::new(&home(), &[volume(&long), volume("Stick")]);
    let (bar, skipped) = component_declaration(7, &places, VIEWER, 0).expect("the rows fit");
    assert_eq!(skipped, 1);
    let shown = item_labels(&bar);
    assert!(!shown.iter().any(|label| label.starts_with("vv")));
    assert!(shown.contains(&"Stick".to_string()));
}

#[test]
fn the_row_cap_drops_the_tail_and_reports_how_many() {
    // More volumes than a plate holds however many fixed places precede
    // them, so the tail is dropped rather than silently overflowing the
    // declaration. The count follows the shared bound rather than restating
    // it, so raising the bound does not quietly stop testing the cap.
    let volumes: Vec<Volume> = (0..APP_MENU_MAX_ROWS)
        .map(|n| volume(&alloc::format!("v{n:02}")))
        .collect();
    let places = Places::new(&home(), &volumes);
    let (bar, skipped) = component_declaration(7, &places, VIEWER, 0).expect("the rows fit");
    assert_eq!(bar.menu.len(), APP_MENU_MAX_ROWS, "the cap is filled");
    assert_eq!(
        skipped,
        places.rows().len() - (APP_MENU_MAX_ROWS - 1 - CLOSING_ROWS),
        "every place past the cap — the volumes' rule and the closing rows \
         taking rows of it — is counted"
    );
    // What is shown is still a prefix of the rail, so the rows the user sees
    // are the ones the rail would have shown first.
    let shown = labels(&places);
    let expected: Vec<String> = places
        .rows()
        .iter()
        .take(shown.len())
        .map(|place| place.label().to_string())
        .collect();
    assert_eq!(shown, expected);
}

/// The cap is the root plate's: a place's own plate rows are not counted
/// against it, so the user's titled images fill it as far as plain volumes
/// do, each with its whole plate.
#[test]
fn the_row_cap_counts_the_root_plates_rows_alone() {
    let images: Vec<Volume> = (0..APP_MENU_MAX_ROWS)
        .map(|n| attached(&alloc::format!("i{n:02}"), VolumeClass::Image, VIEWER, "T"))
        .collect();
    let places = Places::new(&home(), &images);
    let (bar, skipped) = component_declaration(7, &places, VIEWER, 0).expect("the rows fit");
    assert_eq!(
        bar.menu
            .rows()
            .filter(|(_, parent)| parent.is_none())
            .count(),
        APP_MENU_MAX_ROWS,
        "the root plate is filled"
    );
    assert_eq!(
        skipped,
        places.rows().len() - (APP_MENU_MAX_ROWS - 1 - CLOSING_ROWS)
    );
    let last = APP_MENU_MAX_ROWS - 2 - CLOSING_ROWS;
    assert_eq!(
        shown_places(&bar).last(),
        Some(&last),
        "the last place shown"
    );
    assert_eq!(
        plate_of(&bar, places.rows()[last].label()).len(),
        2,
        "shown with its whole plate"
    );
}

/// The places `bar`'s rows open, by their index.
fn shown_places(bar: &AppBar) -> Vec<usize> {
    bar.menu
        .rows()
        .filter_map(|(row, _)| match row {
            AppMenuRowView::Item(item) => match BarRow::of(item.id) {
                Some(BarRow::Place(index)) => Some(index),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

/// A place whose rows the menu's text cannot hold is dropped whole and
/// counted, however long its title, and never at the closing rows' cost — the
/// Trash's reason among them where there is no home: every declaration is
/// admissible, ends on its closing rows, and shows each image it shows with
/// all of its plate.
#[test]
fn a_place_the_menus_text_cannot_hold_is_dropped_whole_and_counted() {
    for owner in [home(), Vec::new()] {
        for title in 150..=250 {
            let images: Vec<Volume> = (0..10)
                .map(|n| {
                    let label = alloc::format!("{n:02}{}", "x".repeat(28));
                    attached(&label, VolumeClass::Image, VIEWER, &"t".repeat(title))
                })
                .collect();
            let places = Places::new(&owner, &images);
            let (bar, skipped) =
                component_declaration(7, &places, VIEWER, 0).expect("the menu is admissible");
            let shown = shown_places(&bar);
            assert!(skipped > 0, "{title}: the text is exceeded");
            assert_eq!(shown.len() + skipped, places.rows().len(), "{title}");
            let items = item_labels(&bar);
            assert_eq!(
                items[items.len() - 2..],
                [NETWORK.to_string(), TRASH.to_string()],
                "{title}: the closing rows always fit"
            );
            for index in shown
                .into_iter()
                .filter(|&index| places.rows()[index].facts().is_some())
            {
                assert_eq!(
                    plate_of(&bar, places.rows()[index].label()).len(),
                    2,
                    "{title}"
                );
            }
        }
    }
}

#[test]
fn an_empty_rail_still_offers_the_closing_rows() {
    // A session with no home and nothing attached has no place to offer, but
    // the network is still the user's to reach, and the Trash says why it is
    // not there.
    let places = Places::default();
    let (bar, skipped) =
        component_declaration(7, &places, VIEWER, 0).expect("the menu is admissible");
    assert_eq!(item_labels(&bar), [NETWORK.to_string(), TRASH.to_string()]);
    assert_eq!(
        bar.menu.len(),
        3,
        "no rule above them with nothing to set them apart from, and the Trash's own"
    );
    assert_eq!(skipped, 0);
}

/// The desktop's file manager never quits, so its menu never offers *Quit*,
/// and its closing rows answer with ids no place uses.
#[test]
fn the_file_manager_never_offers_quit() {
    let places = Places::new(&home(), &[]);
    let (bar, _) = component_declaration(7, &places, VIEWER, 0).expect("the rows fit");
    assert!(
        bar.menu.rows().all(|(row, _)| !matches!(
            row,
            AppMenuRowView::Item(item)
                if item.symbol == AppMenuSymbol::Quit || item.label == tairix_window::QUIT_LABEL
        )),
        "{:?}",
        item_labels(&bar)
    );
    let id_of = |label: &str| {
        bar.menu
            .rows()
            .find_map(|(row, _)| match row {
                AppMenuRowView::Item(item) if item.label == label => Some(item.id),
                _ => None,
            })
            .expect("declared")
    };
    assert_eq!(BarRow::of(id_of(NETWORK)), Some(BarRow::Network));
    assert_eq!(BarRow::of(id_of(TRASH)), Some(BarRow::Trash));
}

/// The file manager's settings are the Settings application's, so its menu
/// offers no way there, whatever the rail holds.
#[test]
fn the_file_manager_offers_no_settings_row() {
    for places in [Places::default(), Places::new(&home(), &[volume("Backup")])] {
        let (bar, _) = component_declaration(7, &places, VIEWER, 0).expect("the rows fit");
        assert!(
            bar.menu.rows().all(|(row, _)| !matches!(
                row,
                AppMenuRowView::Item(item) if item.symbol == AppMenuSymbol::Settings
            )),
            "{:?}",
            item_labels(&bar)
        );
    }
}

/// The menu closes with a rule and *Network*, then *Trash* last beneath a
/// rule of its own, whatever the rail holds; with no place shown there is
/// nothing above the first group to rule off.
#[test]
fn the_trash_is_last_below_its_own_rule_with_or_without_places() {
    use AppMenuRowKind::{Item, Separator};
    let ruled = [Separator, Item, Separator, Item];
    assert_eq!(ruled.len(), CLOSING_ROWS, "the closing rows are counted");
    for (places, closing) in [
        (Places::default(), &ruled[1..]),
        (Places::new(&home(), &[]), &ruled[..]),
        (Places::new(&home(), &[volume("Backup")]), &ruled[..]),
    ] {
        let declared = kinds(&places);
        assert!(declared.ends_with(closing), "{declared:?}");
        let (bar, _) = component_declaration(7, &places, VIEWER, 0).expect("the rows fit");
        let Some((AppMenuRowView::Item(trash), None)) = bar.menu.rows().last() else {
            panic!("the menu ends on a root row");
        };
        assert_eq!((trash.label, trash.symbol), (TRASH, AppMenuSymbol::Trash));
        assert_eq!(BarRow::of(trash.id), Some(BarRow::Trash));
    }
}

/// With no home there is no Trash: its row is greyed and says why, in the
/// words the window's own Go to Trash refuses with.
#[test]
fn the_trash_row_says_why_without_a_home() {
    let trash = |places: &Places| {
        let (bar, _) = component_declaration(7, places, VIEWER, 0).expect("the rows fit");
        let row = bar
            .menu
            .rows()
            .find_map(|(row, _)| match row {
                AppMenuRowView::Item(item) if BarRow::of(item.id) == Some(BarRow::Trash) => {
                    Some((item.enabled, item.reason.to_string()))
                }
                _ => None,
            })
            .expect("the Trash row");
        row
    };
    assert_eq!(trash(&Places::new(&home(), &[])), (true, String::new()));
    for homeless in [Places::default(), Places::new(&[], &[volume("Stick")])] {
        assert_eq!(trash(&homeless), (false, NO_HOME.to_string()));
    }
}
