use alloc::string::String;

use tairix_controls::FieldGroupAction;
use tairix_window::docapp::settings::Preferences as _;

use super::*;

fn acted(group: usize, row: usize, action: FieldAction) -> ColumnAction {
    ColumnAction {
        group,
        action: FieldGroupAction { row, action },
    }
}

fn select(index: usize) -> FieldAction {
    FieldAction::Selected { index }
}

#[test]
fn the_sheet_has_four_categories_and_a_row_for_every_socket() {
    assert_eq!(Preferences::categories().len(), 4);
    let groups = Preferences::default().groups(category::ROMS);
    assert_eq!(groups.len(), 2);
    assert_eq!(groups[0].rows().len(), 1, "the OS");
    assert_eq!(groups[1].rows().len(), SLOTS, "every slot, 15 first");
    assert_eq!(groups[1].rows()[0].label(), "Slot 15");
}

#[test]
fn a_slot_takes_what_is_chosen_and_asks_for_an_image_to_install() {
    let mut record = Preferences::default();
    assert_eq!(record.act(category::ROMS, &acted(1, 15, select(1))), None);
    assert_eq!(record.slots[0], Image::Ram, "row 15 is slot 0");
    assert_eq!(
        record.act(category::ROMS, &acted(1, 0, select(4))),
        Some(Socket::Slot(15)),
        "past the fittings, the install"
    );
    assert_eq!(
        record.slots[15],
        Image::Shipped(Shipped::Basic),
        "unchanged meanwhile"
    );
    record.slots[3] = Image::Installed(String::from("rom.viewsheet"));
    assert_eq!(
        record.act(category::ROMS, &acted(1, 12, select(4))),
        None,
        "its own image kept"
    );
    assert_eq!(
        record.slots[3],
        Image::Installed(String::from("rom.viewsheet"))
    );
    assert_eq!(
        record.act(category::ROMS, &acted(1, 12, select(5))),
        Some(Socket::Slot(3))
    );
    assert_eq!(
        record.act(category::ROMS, &acted(0, 0, select(1))),
        Some(Socket::Os)
    );
    let was = Preferences::default();
    assert!(
        record.reshapes(&was, category::ROMS),
        "a slot's list follows what it holds"
    );
    assert!(!record.reshapes(&was, category::DISPLAY));
}

#[test]
fn the_other_categories_carry_their_rows_into_the_settings() {
    let mut record = Preferences::default();
    record.act(category::STARTUP, &acted(0, 0, select(3)));
    record.act(category::STARTUP, &acted(0, 1, select(1)));
    record.act(category::STARTUP, &acted(0, 2, select(0)));
    assert_eq!(
        (record.mode, record.shift_boots, record.disc_timing),
        (3, false, 0)
    );
    record.act(category::DISPLAY, &acted(0, 0, select(1)));
    record.act(
        category::DISPLAY,
        &acted(0, 1, FieldAction::Set { on: true }),
    );
    assert_eq!((record.scaling, record.scan_lines), (Scaling::Smooth, true));
    record.act(category::INPUT, &acted(0, 0, select(1)));
    record.act(
        category::INPUT,
        &acted(1, 0, FieldAction::SetValue { permille: 400 }),
    );
    assert_eq!(record.volume, 400, "live while dragged");
    record.act(
        category::INPUT,
        &acted(1, 0, FieldAction::Settled { permille: 650 }),
    );
    record.act(
        category::INPUT,
        &acted(2, 0, FieldAction::Set { on: false }),
    );
    assert_eq!(
        (record.keyboard, record.volume, record.fast_tape),
        (Mapping::Physical, 650, false)
    );
}
