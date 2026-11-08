//! The column of form groups: its layout, the cursor carried between plates,
//! a press taking the cursor, a modal list, and the settle of a row the
//! cursor leaves.

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use tairix_geometry::{to_i32, Point, Rect, Scale};
use tairix_input::{InputEvent, Key, NamedKey, PointerButton};
use tairix_theme::Theme;

use crate::column::{ColumnAction, ColumnPlace, FieldColumn};
use crate::combo::ComboBox;
use crate::damage::sink;
use crate::form::{FieldAction, FieldControl, FieldGroup, FieldGroupAction, FieldRow};
use crate::number::{NumberAction, NumberField};
use crate::selector::Toggle;
use crate::stack;
use crate::testkit::keystroke;

const BOUNDS: Rect = Rect::new(0, 0, 360, 600);

fn toggle(label: &str) -> FieldRow {
    FieldRow::new(label, FieldControl::Toggle(Toggle::new("", false)))
}

/// A choice plate over a plate of a number and a toggle.
fn column() -> FieldColumn {
    let choices = vec![String::from("Lines"), String::from("Dots")];
    FieldColumn::new(vec![
        FieldGroup::new(
            "STYLE",
            vec![
                FieldRow::new("Style", FieldControl::Combo(ComboBox::new(choices))),
                toggle("Shown"),
            ],
        ),
        FieldGroup::new(
            "SPACING",
            vec![
                FieldRow::new("Across", FieldControl::Number(NumberField::new(16, 1, 64))),
                toggle("Snap"),
            ],
        ),
    ])
}

fn place(theme: &Theme) -> ColumnPlace<'_> {
    ColumnPlace {
        bounds: BOUNDS,
        viewport: BOUNDS,
        scale: Scale::ONE,
        theme,
    }
}

fn key(column: &mut FieldColumn, key: Key, theme: &Theme) -> Option<ColumnAction> {
    column.on_key(keystroke(key), place(theme), &mut sink())
}

fn down(column: &mut FieldColumn, theme: &Theme) -> Option<ColumnAction> {
    key(column, Key::Named(NamedKey::Down), theme)
}

/// A press at the centre of row `row` of group `group`.
fn press(
    column: &mut FieldColumn,
    (group, row): (usize, usize),
    theme: &Theme,
) -> Option<ColumnAction> {
    let layouts = column.layouts(place(theme));
    let (_, layout) = layouts[group];
    let rect = column.groups()[group]
        .row_rect(row, layout, Scale::ONE, theme)
        .expect("the row is drawn");
    let centre = Point::new(
        rect.left() + to_i32(rect.width) / 4,
        rect.top() + to_i32(rect.height) / 2,
    );
    let mut damage = sink();
    let _ = column.on_pointer(
        &InputEvent::PointerMoved { to: centre },
        place(theme),
        &mut damage,
    );
    column.on_pointer(
        &InputEvent::PointerPressed {
            button: PointerButton::Primary,
        },
        place(theme),
        &mut damage,
    )
}

#[test]
fn groups_stack_on_one_slot_column_at_their_measured_heights() {
    let theme = Theme::light();
    let column = column();
    let layouts = column.layouts(place(&theme));
    assert_eq!(layouts.len(), 2);
    assert_eq!(layouts[0].1.column, layouts[1].1.column, "one slot column");
    assert!(layouts[0].1.bounds.bottom() < layouts[1].1.bounds.top());
    let plate = stack::plate_width(BOUNDS.width, Scale::ONE, &theme);
    let heights: Vec<u32> = layouts
        .iter()
        .map(|(_, layout)| layout.bounds.height)
        .collect();
    assert_eq!(
        column.measured_height(BOUNDS.width, Scale::ONE, &theme),
        stack::height(heights, Scale::ONE, &theme)
    );
    assert_eq!(layouts[0].1.bounds.width, plate);
    assert!(column.natural_width(Scale::ONE, &theme) > 0);
}

#[test]
fn the_cursor_crosses_plates_and_clamps_at_the_column_ends() {
    let theme = Theme::light();
    let mut column = column();
    column.set_focused(true);
    assert_eq!(column.cursor(), Some((0, 0)));
    down(&mut column, &theme);
    assert_eq!(column.cursor(), Some((0, 1)));
    down(&mut column, &theme);
    assert_eq!(
        column.cursor(),
        Some((1, 0)),
        "past a plate's end into the next"
    );
    assert_eq!(
        column.groups()[0].focus(),
        None,
        "the plate left has no ring"
    );
    down(&mut column, &theme);
    down(&mut column, &theme);
    assert_eq!(column.cursor(), Some((1, 1)), "clamped at the last row");
    key(&mut column, Key::Named(NamedKey::Up), &theme);
    key(&mut column, Key::Named(NamedKey::Up), &theme);
    assert_eq!(
        column.cursor(),
        Some((0, 1)),
        "into the previous plate's last row"
    );
}

/// Type `digits` over what the number row at the cursor holds.
fn retype(column: &mut FieldColumn, digits: &str, theme: &Theme) {
    for typed in [
        Key::Named(NamedKey::End),
        Key::Named(NamedKey::Backspace),
        Key::Named(NamedKey::Backspace),
    ] {
        key(column, typed, theme);
    }
    for digit in digits.chars() {
        key(column, Key::Char(digit), theme);
    }
}

fn settled_at(group: usize, row: usize, value: i32) -> ColumnAction {
    ColumnAction {
        group,
        action: FieldGroupAction {
            row,
            action: FieldAction::Number(NumberAction::Settled { value }),
        },
    }
}

#[test]
fn a_number_row_the_cursor_leaves_for_another_plate_settles() {
    let theme = Theme::light();
    let mut column = column();
    let _ = press(&mut column, (1, 0), &theme);
    assert_eq!(column.cursor(), Some((1, 0)));
    retype(&mut column, "9", &theme);
    let stepped = key(&mut column, Key::Named(NamedKey::Up), &theme);
    assert_eq!(
        stepped,
        Some(settled_at(1, 0, 9)),
        "Up out of the plate settles it"
    );
    assert_eq!(column.cursor(), Some((0, 1)));
    let _ = press(&mut column, (1, 0), &theme);
    retype(&mut column, "12", &theme);
    let pressed = press(&mut column, (0, 1), &theme);
    assert_eq!(
        pressed,
        Some(settled_at(1, 0, 12)),
        "a press on another plate settles what was typed"
    );
    assert_eq!(column.cursor(), Some((0, 1)));
}

#[test]
fn leaving_the_column_settles_the_row_and_takes_the_ring() {
    let theme = Theme::light();
    let mut column = column();
    let _ = press(&mut column, (1, 0), &theme);
    retype(&mut column, "70", &theme);
    let left = column.leave(place(&theme), &mut sink());
    assert_eq!(left, Some(settled_at(1, 0, 64)), "70 is held to the bound");
    assert_eq!(column.cursor(), None);
    assert_eq!(column.leave(place(&theme), &mut sink()), None);
}

#[test]
fn an_open_list_is_modal_across_the_column() {
    let theme = Theme::light();
    let mut column = column();
    column.set_focused(true);
    let opened = key(&mut column, Key::Named(NamedKey::Enter), &theme);
    assert_eq!(
        opened.map(|action| action.action.action),
        Some(FieldAction::Choices { expanded: true })
    );
    assert_eq!(column.listing(), Some(0));
    assert!(column.is_listing() && column.mid_gesture());
    down(&mut column, &theme);
    assert_eq!(column.cursor(), Some((0, 0)), "every key is the list's");
    let pressed = press(&mut column, (1, 1), &theme);
    assert_ne!(
        pressed.map(|action| action.group),
        Some(1),
        "a press never reaches a plate beneath the list"
    );
}

#[test]
fn replacing_the_groups_keeps_the_cursor_where_it_was() {
    let theme = Theme::light();
    let mut column = column();
    let _ = press(&mut column, (1, 1), &theme);
    assert_eq!(column.cursor(), Some((1, 1)));
    let mut rebuilt = self::column();
    rebuilt.groups_mut()[1].rows_mut()[1] = toggle("Snap to the grid");
    let groups = core::mem::take(rebuilt.groups_mut());
    column.replace(groups);
    assert_eq!(column.cursor(), Some((1, 1)));
    column.replace(vec![FieldGroup::new("ONE", vec![toggle("Only")])]);
    assert_eq!(
        column.cursor(),
        Some((0, 0)),
        "the nearest a shorter column has"
    );
}

#[test]
fn the_cursor_is_revealed_with_its_plate_where_that_fits() {
    let theme = Theme::light();
    let mut column = column();
    let _ = press(&mut column, (1, 0), &theme);
    let layouts = column.layouts(place(&theme));
    let plate = layouts[1].1.bounds;
    let whole = column
        .cursor_reveal(place(&theme), BOUNDS.height)
        .expect("a cursor to reveal");
    assert!(whole.top() < plate.top() && whole.bottom() > plate.bottom());
    let row = column
        .cursor_reveal(place(&theme), 4)
        .expect("a cursor to reveal");
    assert!(
        row.height < whole.height,
        "a short view shows the row alone"
    );
}
