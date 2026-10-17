use alloc::vec::Vec;

use tairix_abi::window_ipc::{AppMenuMark, AppMenuRole, AppMenuRowView, AppMenuSymbol};

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
    menu.item(
        1u16,
        AppMenuSymbol::Open,
        "Copy",
        "Ctrl+C",
        true,
        Plate::Root,
    );
    menu.separator(Plate::Root);
    let file = menu
        .submenu("File", AppMenuSymbol::File, Plate::Root)
        .expect("room");
    menu.item(2u16, AppMenuSymbol::Open, "Open", "", true, file);
    let rows = shape(menu);
    assert_eq!(rows.len(), 4);
    assert_eq!(rows[2], (alloc::string::String::from("File"), None));
    assert_eq!(rows[3], (alloc::string::String::from("Open"), Some(2)));
}

#[test]
fn marks_state_what_a_row_turns_on_or_chooses() {
    let mut menu = MenuBuilder::new();
    menu.mark(1u16, AppMenuSymbol::Open, "Grid", "", true, Plate::Root);
    menu.radio(2u16, AppMenuSymbol::Open, "100%", "", true, Plate::Root);
    menu.radio(3u16, AppMenuSymbol::Open, "200%", "", false, Plate::Root);
    menu.item(4u16, AppMenuSymbol::Open, "Paste", "", false, Plate::Root);
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
    menu.entry(
        5u16,
        50u16,
        AppMenuSymbol::Rename,
        "Rename",
        "old",
        Plate::Root,
    );
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
    menu.item(0u16, AppMenuSymbol::Open, "Nothing", "", true, Plate::Root);
    menu.item(1u16, AppMenuSymbol::Open, "Kept", "", true, Plate::Root);
    assert_eq!(shape(menu).len(), 1, "an id of zero is not a row's");
    let mut menu = MenuBuilder::new();
    assert!(menu
        .submenu("", AppMenuSymbol::File, Plate::Under(7))
        .is_none());
}

#[test]
fn every_row_states_its_function_and_a_destructive_one_its_emphasis() {
    let mut menu = MenuBuilder::new();
    let file = menu
        .submenu("File", AppMenuSymbol::File, Plate::Root)
        .expect("room");
    menu.item(1u16, AppMenuSymbol::Save, "Save", "Ctrl+S", true, file);
    menu.destructive(2u16, AppMenuSymbol::Delete, "Delete", "", true, file);
    let built = menu.finish();
    let rows: Vec<_> = built
        .rows()
        .map(|(row, _)| match row {
            AppMenuRowView::Item(item) => (item.symbol, item.role),
            AppMenuRowView::Submenu { symbol, .. } => (symbol, AppMenuRole::Neutral),
            AppMenuRowView::Separator | AppMenuRowView::Info => panic!("no such row"),
        })
        .collect();
    assert_eq!(
        rows,
        [
            (AppMenuSymbol::File, AppMenuRole::Neutral),
            (AppMenuSymbol::Save, AppMenuRole::Neutral),
            (AppMenuSymbol::Delete, AppMenuRole::Destructive),
        ]
    );
}
