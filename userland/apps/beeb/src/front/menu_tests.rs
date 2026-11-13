use tairix_abi::window_ipc::AppMenuRowView;

use super::*;

fn built(shown: &Shown) -> AppMenu {
    let mut menu = AppMenu::EMPTY;
    push_rows(&mut menu, shown).expect("the rows fit");
    menu
}

/// The view of the row labelled `label`.
fn row<'a>(menu: &'a AppMenu, label: &str) -> tairix_abi::window_ipc::AppMenuItemView<'a> {
    menu.rows()
        .find_map(|(row, _)| match row {
            AppMenuRowView::Item(item) if item.label == label => Some(item),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no row {label}"))
}

#[test]
fn every_command_is_its_rows_id_and_back() {
    for command in COMMANDS {
        let id = id(command).expect("an id");
        assert!(id.get() >= FIRST_ID, "clear of the icon bar's Quit");
        assert_eq!(super::command(id), Some(command));
    }
    assert_eq!(
        super::command(AppMenuItemId::new(1).expect("an id")),
        None,
        "Quit is not ours"
    );
    let past = u16::try_from(COMMANDS.len()).expect("small") + FIRST_ID;
    assert_eq!(
        super::command(AppMenuItemId::new(past).expect("an id")),
        None
    );
}

#[test]
fn rows_offer_only_what_the_machine_can_do_and_mark_what_it_does() {
    let empty = built(&Shown::default());
    assert!(!row(&empty, "Eject").enabled, "an empty drive");
    assert!(!row(&empty, "Rewind").enabled, "no tape");
    assert!(!row(&empty, "Record").enabled);
    assert_eq!(row(&empty, "Normal").mark, AppMenuMark::Radio);
    assert_eq!(row(&empty, "Fastest").mark, AppMenuMark::None);
    assert_eq!(row(&empty, "Pause").mark, AppMenuMark::None);
    let busy = built(&Shown {
        discs: [true, false],
        images: [true, false],
        tape: true,
        recording: true,
        paused: true,
        speed: Speed::Fastest,
        scaling: Scaling::Smooth,
        scan_lines: true,
        keyboard: Mapping::Physical,
        full_screen: true,
    });
    assert!(row(&busy, "Eject").enabled, "drive 0's disc");
    assert!(row(&busy, "Save Disc As…").enabled, "its image held");
    let restored = built(&Shown {
        discs: [true, false],
        ..Shown::default()
    });
    assert!(row(&restored, "Eject").enabled, "a snapshot's disc");
    assert!(
        !row(&restored, "Save Disc As…").enabled,
        "no image held to save"
    );
    assert_eq!(row(&busy, "Record").mark, AppMenuMark::Check);
    assert_eq!(row(&busy, "Pause").mark, AppMenuMark::Check);
    assert_eq!(row(&busy, "Fastest").mark, AppMenuMark::Radio);
    assert_eq!(row(&busy, "Smooth").mark, AppMenuMark::Radio);
    assert_eq!(row(&busy, "By Position").mark, AppMenuMark::Radio);
    assert_eq!(row(&busy, "Full Screen").mark, AppMenuMark::Check);
    assert!(
        row(&busy, "Scan Lines").repeatable,
        "a tick keeps the menu open"
    );
}

#[test]
fn the_icon_bars_menu_holds_the_rows_between_its_info_and_quit() {
    let menu = bar(&Shown::default()).expect("the rows fit");
    let rows: alloc::vec::Vec<_> = menu.rows().map(|(row, _)| row).collect();
    assert!(matches!(rows.first(), Some(AppMenuRowView::Info)));
    let quit = row(&menu, tairix_window::QUIT_LABEL);
    assert!(
        tairix_window::is_quit(quit.id),
        "Quit is the icon bar's own"
    );
    assert!(super::command(quit.id).is_none());
}
