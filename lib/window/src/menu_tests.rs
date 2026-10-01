use alloc::vec::Vec;

use tairix_abi::window_ipc::{AppMenuMark, AppMenuRowView};

use super::{MenuBuilder, Plate};

/// Each row as `(label, parent)`, a rule as `"-"`.
fn shape(builder: MenuBuilder) -> Vec<(alloc::string::String, Option<usize>)> {
    builder
        .finish()
        .rows()
        .map(|(row, parent)| {
            let label = match row {
                AppMenuRowView::Item(item) => item.label,
                AppMenuRowView::Submenu { label, .. } => label,
                AppMenuRowView::Separator => "-",
                AppMenuRowView::Info => "info",
            };
            (alloc::string::String::from(label), parent)
        })
        .collect()
}

#[test]
fn rows_land_on_the_plate_they_are_given() {
    let mut menu = MenuBuilder::titled("App");
    menu.item(1u16, "Copy", "Ctrl+C", true, Plate::Root);
    menu.separator(Plate::Root);
    let file = menu.submenu("File", Plate::Root).expect("room");
    menu.item(2u16, "Open", "", true, file);
    let rows = shape(menu);
    assert_eq!(rows.len(), 4);
    assert_eq!(rows[2], (alloc::string::String::from("File"), None));
    assert_eq!(rows[3], (alloc::string::String::from("Open"), Some(2)));
}

#[test]
fn marks_state_what_a_row_turns_on_or_chooses() {
    let mut menu = MenuBuilder::new();
    menu.mark(1u16, "Grid", "", true, Plate::Root);
    menu.radio(2u16, "100%", "", true, Plate::Root);
    menu.radio(3u16, "200%", "", false, Plate::Root);
    menu.item(4u16, "Paste", "", false, Plate::Root);
    let built = menu.finish();
    let marks: Vec<(AppMenuMark, bool)> = built
        .rows()
        .filter_map(|(row, _)| match row {
            AppMenuRowView::Item(item) => Some((item.mark, item.enabled)),
            _ => None,
        })
        .collect();
    assert_eq!(
        marks,
        [
            (AppMenuMark::Check, true),
            (AppMenuMark::Radio, true),
            (AppMenuMark::None, true),
            (AppMenuMark::None, false),
        ]
    );
}

#[test]
fn an_entry_row_carries_its_field_and_its_own_id() {
    let mut menu = MenuBuilder::new();
    menu.entry(5u16, 50u16, "Rename", "old", Plate::Root);
    let built = menu.finish();
    let Some((AppMenuRowView::Item(item), None)) = built.rows().next() else {
        panic!("one item");
    };
    let entry = item.entry.expect("a field");
    assert_eq!(
        (item.id.get(), entry.id.get(), entry.initial),
        (5, 50, "old")
    );
}

#[test]
fn a_row_that_cannot_be_carried_is_left_out_alone() {
    let mut menu = MenuBuilder::new();
    menu.item(0u16, "Nothing", "", true, Plate::Root);
    menu.item(1u16, "Kept", "", true, Plate::Root);
    assert_eq!(shape(menu).len(), 1, "an id of zero is not a row's");
    let mut menu = MenuBuilder::new();
    assert!(menu.submenu("", Plate::Under(7)).is_none());
}
