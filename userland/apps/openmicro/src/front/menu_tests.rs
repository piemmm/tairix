use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::window_ipc::AppMenuRowView;

use super::*;

fn shown(model: Model) -> Shown {
    Shown {
        model,
        discs: [false; 2],
        images: [false; 2],
        tape: false,
        recording: false,
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
        discs: names("ssd"),
        tapes: names("uef"),
    };
    let menu = built(&shown(Model::ModelB), &offered, Place::Window);
    assert!(labels(&menu)
        .iter()
        .any(|label| label.ends_with('\u{2026}')));
}
