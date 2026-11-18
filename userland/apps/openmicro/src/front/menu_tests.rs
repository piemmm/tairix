use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::window_ipc::AppMenuRowView;

use super::*;

fn shown(model: Model) -> Shown {
    Shown {
        model,
        drives: [model == Model::ModelB; 2],
        discs: [false; 2],
        images: [false; 2],
        tape: false,
        recording: false,
        playing: false,
        cartridge: false,
        paused: false,
        speed: Speed::Normal,
        full_screen: false,
        scaling: Scaling::Sharp,
        scan_lines: false,
        keyboard: Mapping::Logical,
    }
}

fn built(shown: &Shown, offered: &Offered, place: Place) -> AppMenu {
    let mut menu = AppMenu::EMPTY;
    push_rows(&mut menu, (shown, offered), place).expect("the rows fit");
    menu
}

/// The view of the first row labelled `label`.
fn row<'a>(menu: &'a AppMenu, label: &str) -> tairix_abi::window_ipc::AppMenuItemView<'a> {
    menu.rows()
        .find_map(|(row, _)| match row {
            AppMenuRowView::Item(item) if item.label == label => Some(item),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no row {label}"))
}

fn labels(menu: &AppMenu) -> Vec<String> {
    menu.rows()
        .filter_map(|(row, _)| match row {
            AppMenuRowView::Item(item) => Some(String::from(item.label)),
            _ => None,
        })
        .collect()
}

#[test]
fn every_command_is_its_rows_id_and_back() {
    let folders = (0..FOLDER_ROWS).flat_map(|n| {
        let n = u8::try_from(n).expect("small");
        [
            Command::FolderDisc(PhysicalDrive::Zero, n),
            Command::FolderDisc(PhysicalDrive::One, n),
            Command::FolderTape(n),
        ]
    });
    for command in COMMANDS
        .into_iter()
        .chain([Command::Library])
        .chain(folders)
    {
        let id = AppMenuItemId::new(id(command).expect("an id")).expect("not zero");
        assert!(id.get() < tairix_machines::menu::CARD_ROWS);
        assert_eq!(super::command(id), Some(command), "{command:?}");
    }
    let past = u8::try_from(FOLDER_ROWS).expect("small");
    assert_eq!(id(Command::FolderTape(past)), Err(Errno::OutOfRange));
}

#[test]
fn rows_offer_only_what_the_machine_can_do_and_mark_what_it_does() {
    let none = Offered::default();
    let empty = built(&shown(Model::ModelB), &none, Place::Window);
    assert!(!row(&empty, "Eject").enabled, "an empty drive");
    assert!(!row(&empty, "Rewind").enabled, "no tape");
    assert!(!row(&empty, "New Disc").enabled, "no folder to make it in");
    assert!(!row(&empty, "From Folder").enabled);
    assert_eq!(row(&empty, "Normal").mark, AppMenuMark::Radio);
    let busy = built(
        &Shown {
            discs: [true, false],
            images: [true, false],
            tape: true,
            recording: true,
            paused: true,
            ..shown(Model::ModelB)
        },
        &Offered {
            reachable: true,
            folder: true,
            discs: alloc::vec![String::from("Games.ssd")],
            tapes: Vec::new(),
        },
        Place::Window,
    );
    assert!(row(&busy, "Eject").enabled);
    assert!(row(&busy, "New Disc").enabled);
    assert!(
        row(&busy, "Games.ssd").enabled,
        "the folder's disc is offered"
    );
    assert_eq!(row(&busy, "Record").mark, AppMenuMark::Check);
    assert_eq!(row(&busy, "Pause").mark, AppMenuMark::Check);
}

#[test]
fn an_electron_has_no_drives_and_a_card_carries_no_settings_of_its_own() {
    let electron = built(&shown(Model::Electron), &Offered::default(), Place::Window);
    let named = labels(&electron);
    assert!(!named.iter().any(|label| label == "Insert Disc\u{2026}"));
    assert!(named.iter().any(|label| label == "Insert Tape\u{2026}"));
    assert!(named.iter().any(|label| label == "Machine Library"));
    let card = labels(&built(
        &shown(Model::ModelB),
        &Offered::default(),
        Place::Card,
    ));
    assert!(!card
        .iter()
        .any(|label| label == "Settings\u{2026}" || label == "Machine Library"));
    assert!(
        card.iter().any(|label| label == "Shut Down") && named.iter().any(|l| l == "Shut Down"),
        "a running machine is shut down from its window or its card"
    );
}

/// A machine's window offers its folder to be shown once it has one, and
/// says why not before, a reachable documents folder or not; a card's rows
/// leave the row to the card itself.
#[test]
fn show_folder_is_offered_where_the_machine_has_a_folder() {
    let unmade = Offered {
        reachable: true,
        ..Offered::default()
    };
    let without = built(&shown(Model::ModelB), &unmade, Place::Window);
    let row_of = |menu: &AppMenu| {
        menu.rows().find_map(|(row, _)| match row {
            AppMenuRowView::Item(item) if item.label == "Show Folder" => {
                Some((item.enabled, String::from(item.reason)))
            }
            _ => None,
        })
    };
    assert_eq!(
        row_of(&without),
        Some((false, String::from("The machine has no folder yet")))
    );
    let made = Offered {
        reachable: true,
        folder: true,
        ..Offered::default()
    };
    let with = built(&shown(Model::Electron), &made, Place::Window);
    assert_eq!(row_of(&with), Some((true, String::new())));
    let card = built(&shown(Model::ModelB), &made, Place::Card);
    assert_eq!(
        row_of(&card),
        None,
        "the card adds it whatever the machine's standing"
    );
    assert!(
        labels(&with).iter().any(|label| label == "Save Snapshot"),
        "kept in the folder"
    );
    assert!(
        labels(&without)
            .iter()
            .any(|label| label == "Save Snapshot\u{2026}"),
        "where the picker is told"
    );
    let mut alone = AppMenu::EMPTY;
    alone.push(show_folder(true).expect("a row")).expect("fits");
    assert_eq!(row_of(&alone), Some((true, String::new())));
}

/// A window opened once the documents folder is reached offers a blank disc
/// and tape even before its machine has a folder, and the folder itself only
/// once it has one; before the documents folder is reached it offers neither.
#[test]
fn a_new_window_is_offered_what_its_documents_and_folder_allow() {
    let window = |reachable, folder| {
        built(
            &shown(Model::ModelB),
            &Offered::unlisted(reachable, folder),
            Place::Window,
        )
    };
    let unreached = window(false, true);
    for label in ["New Disc", "New Tape", "Show Folder"] {
        assert!(!row(&unreached, label).enabled, "{label}");
    }
    assert!(labels(&unreached)
        .iter()
        .any(|l| l == "Save Snapshot\u{2026}"));
    let unmade = window(true, false);
    assert!(row(&unmade, "New Disc").enabled && row(&unmade, "New Tape").enabled);
    assert!(!row(&unmade, "Show Folder").enabled);
    assert!(labels(&unmade).iter().any(|l| l == "Save Snapshot\u{2026}"));
    let made = window(true, true);
    assert!(row(&made, "Show Folder").enabled);
    assert!(labels(&made).iter().any(|l| l == "Save Snapshot"));
}

/// SHIFT-BREAK is offered beside BREAK on the models it boots, and a C64,
/// whose interrupt is RESTORE, is offered neither.
#[test]
fn shift_break_is_offered_where_it_boots_the_machine() {
    for model in Model::ALL {
        let named = labels(&built(&shown(model), &Offered::default(), Place::Window));
        let at = |label: &str| named.iter().position(|named| named == label);
        if model.boots_with_shift_break() {
            let interrupt = at("BREAK").expect("BREAK");
            assert_eq!(at(SHIFT_BREAK), Some(interrupt + 1), "{model:?}");
        } else {
            assert_eq!(at(SHIFT_BREAK), None, "{model:?}");
            assert!(at("RESTORE").is_some());
        }
    }
}

/// A folder of the most images a menu lists, each with the longest name,
/// still fits a Model B's whole menu.
#[test]
fn the_fullest_folder_fits_the_menu() {
    let names = |ending: &str| {
        (0..FOLDER_ROWS * 2)
            .map(|n| alloc::format!("{n:0>60}.{ending}"))
            .collect::<Vec<_>>()
    };
    let offered = Offered {
        reachable: true,
        folder: true,
        discs: names("ssd"),
        tapes: names("uef"),
    };
    let menu = built(&shown(Model::ModelB), &offered, Place::Window);
    assert!(labels(&menu)
        .iter()
        .any(|label| label.ends_with('\u{2026}')));
}
