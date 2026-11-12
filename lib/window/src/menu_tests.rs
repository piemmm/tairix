use alloc::vec::Vec;

use tairix_abi::window_ipc::{
    AppMenuMark, AppMenuRole, AppMenuRowView, AppMenuSymbol, MENU_OPEN_REVISION,
};

use super::{MenuBuilder, MenuCommand, MenuStatements, Plate};

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
fn a_switch_an_alternative_and_an_in_place_command_keep_their_menu_up_and_nothing_else() {
    let mut menu = MenuBuilder::new();
    menu.mark(1u16, AppMenuSymbol::Open, "Grid", "", false, Plate::Root);
    menu.radio(2u16, AppMenuSymbol::Open, "100%", "", false, Plate::Root);
    menu.repeating(3u16, AppMenuSymbol::Undo, "Undo", "", true, Plate::Root);
    menu.item(
        4u16,
        AppMenuSymbol::Open,
        "Open\u{2026}",
        "",
        true,
        Plate::Root,
    );
    menu.destructive(5u16, AppMenuSymbol::Delete, "Delete", "", true, Plate::Root);
    let built = menu.finish();
    let kept: Vec<bool> = built
        .rows()
        .filter_map(|(row, _)| match row {
            AppMenuRowView::Item(item) => Some(item.repeatable),
            _ => None,
        })
        .collect();
    assert_eq!(kept, [true, true, true, false, false]);
}

/// A command that knows where its work is done.
#[derive(Copy, Clone)]
enum Verb {
    Undo,
    Open,
    Float,
}

impl From<Verb> for u16 {
    fn from(verb: Verb) -> Self {
        match verb {
            Verb::Undo => 1,
            Verb::Open => 2,
            Verb::Float => 3,
        }
    }
}

impl MenuCommand for Verb {
    fn acts_in_place(self) -> bool {
        matches!(self, Self::Undo)
    }
}

#[test]
fn a_command_row_keeps_its_menu_up_as_its_command_says() {
    let mut menu = MenuBuilder::new();
    menu.command(
        Verb::Undo,
        AppMenuSymbol::Undo,
        "Undo",
        "Ctrl+Z",
        false,
        Plate::Root,
    );
    menu.command(
        Verb::Open,
        AppMenuSymbol::Open,
        "Open\u{2026}",
        "",
        true,
        Plate::Root,
    );
    let built = menu.finish();
    let rows: Vec<(u16, bool, bool)> = built
        .rows()
        .filter_map(|(row, _)| match row {
            AppMenuRowView::Item(item) => Some((item.id.get(), item.enabled, item.repeatable)),
            _ => None,
        })
        .collect();
    assert_eq!(rows, [(1, false, true), (2, true, false)]);
}

/// A switch or an alternative built from a command is kept up as the command
/// says, not as every switch is.
#[test]
fn a_command_mark_or_alternative_keeps_its_menu_up_as_its_command_says() {
    let mut menu = MenuBuilder::new();
    menu.command_mark(
        Verb::Undo,
        AppMenuSymbol::Undo,
        "Undo",
        "",
        true,
        Plate::Root,
    );
    menu.command_mark(
        Verb::Float,
        AppMenuSymbol::Open,
        "Float",
        "",
        false,
        Plate::Root,
    );
    menu.command_radio(
        Verb::Open,
        AppMenuSymbol::Open,
        "Docked",
        "",
        true,
        Plate::Root,
    );
    let built = menu.finish();
    let rows: Vec<(AppMenuMark, bool)> = built
        .rows()
        .filter_map(|(row, _)| match row {
            AppMenuRowView::Item(item) => Some((item.mark, item.repeatable)),
            _ => None,
        })
        .collect();
    assert_eq!(
        rows,
        [
            (AppMenuMark::Check, true),
            (AppMenuMark::None, false),
            (AppMenuMark::Radio, false),
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

#[test]
fn the_information_row_sits_on_the_root_plate() {
    let mut menu = MenuBuilder::new();
    menu.info();
    let games = menu
        .submenu("Game", AppMenuSymbol::NewGame, Plate::Root)
        .expect("room");
    menu.item(2u16, AppMenuSymbol::NewGame, "Deal", "", true, games);
    assert_eq!(
        shape(menu),
        [
            (alloc::string::String::from("info"), None),
            (alloc::string::String::from("Game"), None),
            (alloc::string::String::from("Deal"), Some(1)),
        ]
    );
}

/// A row chosen from the rows shown, or from those the last restate replaced,
/// names what they stood for; one chosen from any older rows names nothing.
#[test]
fn a_menu_answers_from_the_rows_shown_and_those_its_last_restate_replaced() {
    let mut menu = MenuStatements::new(9, "first");
    assert_eq!(menu.open_id(), 9);
    assert_eq!(menu.revision(), MENU_OPEN_REVISION);
    assert_eq!(menu.chosen_from(MENU_OPEN_REVISION), Some(&"first"));
    let second = menu.next_revision();
    menu.adopt(second, "second");
    assert_eq!(menu.shown(), &"second");
    assert_eq!(menu.chosen_from(second), Some(&"second"));
    assert_eq!(
        menu.chosen_from(MENU_OPEN_REVISION),
        Some(&"first"),
        "chosen as the restate crossed it"
    );
    menu.adopt(second, "second, its places moved");
    assert_eq!(
        menu.chosen_from(MENU_OPEN_REVISION),
        Some(&"first"),
        "a restate under the revision shown keeps the one before"
    );
    let third = menu.next_revision();
    menu.adopt(third, "third");
    assert_eq!(menu.chosen_from(second), Some(&"second, its places moved"));
    assert_eq!(
        menu.chosen_from(MENU_OPEN_REVISION),
        None,
        "two restates back"
    );
    assert_eq!(menu.chosen_from(third.wrapping_add(1)), None);
}

#[test]
fn a_menu_numbers_its_statements_past_the_last_revision() {
    let mut menu = MenuStatements::new(1, 0u8);
    menu.adopt(u32::MAX, 1);
    assert_eq!(menu.next_revision(), 0);
    menu.adopt(menu.next_revision(), 2);
    assert_eq!(
        (menu.chosen_from(0), menu.chosen_from(u32::MAX)),
        (Some(&2), Some(&1))
    );
}
