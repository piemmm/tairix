//! The form dialog's routing: a press goes to the part it began in, an open
//! list is modal, Escape dismisses, an Enter no field takes presses the
//! trailing action, and a switch shows what its owner took.

use alloc::string::String;
use alloc::vec;

use tairix_geometry::{to_i32, Point, Rect, Scale};
use tairix_input::{InputEvent, Key, NamedKey, PointerButton};
use tairix_theme::Theme;

use super::{FormAction, FormDialog};
use crate::button::{Button, ButtonContent};
use crate::column::FieldColumn;
use crate::damage::sink;
use crate::decision::Dialog;
use crate::form::{FieldAction, FieldControl, FieldGroup, FieldLayout, FieldRow};
use crate::selector::Toggle;
use crate::state::ControlRole;
use crate::testkit::keystroke;

const WIDTH: u32 = 480;
const MODEL: usize = 0;
const FAST: usize = 1;

fn form() -> FormDialog {
    let group = FieldGroup::new(
        "MACHINE",
        vec![
            FieldRow::choice("Model", &["First", "Second"], 0),
            FieldRow::new("Fast", FieldControl::Toggle(Toggle::new("", false))),
        ],
    );
    let dialog = Dialog::new("Form").in_window().with_actions(vec![
        Button::labelled("Cancel"),
        Button::new(
            ButtonContent::Label(String::from("Apply")),
            ControlRole::Recommended,
        ),
    ]);
    FormDialog::new(dialog, FieldColumn::new(vec![group]), WIDTH)
}

fn key(form: &mut FormDialog, key: Key, theme: &Theme) -> Option<FormAction> {
    let event = InputEvent::KeyPressed {
        key,
        modifiers: keystroke(key).modifiers,
    };
    form.on_input(&event, 0, (Scale::ONE, theme), &mut sink())
}

fn pointer(form: &mut FormDialog, event: InputEvent, theme: &Theme) -> Option<FormAction> {
    form.on_input(&event, 0, (Scale::ONE, theme), &mut sink())
}

fn centre(rect: Rect) -> Point {
    Point::new(
        rect.left() + to_i32(rect.width) / 2,
        rect.top() + to_i32(rect.height) / 2,
    )
}

fn action_rect(form: &FormDialog, index: usize, theme: &Theme) -> Rect {
    let (bounds, _) = form.place(Scale::ONE, theme);
    form.dialog().action_rects(bounds, Scale::ONE, theme)[index]
}

fn row_rect(form: &FormDialog, row: usize, theme: &Theme) -> Rect {
    let (_, place) = form.place(Scale::ONE, theme);
    let layouts = form.column().layouts(place);
    form.column().groups()[0]
        .row_rect(row, layouts[0].1, Scale::ONE, theme)
        .expect("the row is drawn")
}

/// The middle of row `row`'s control, where a press reaches it.
fn control_centre(form: &FormDialog, row: usize, theme: &Theme) -> Point {
    let (_, place) = form.place(Scale::ONE, theme);
    let layout = form.column().layouts(place)[0].1;
    let bounds = row_rect(form, row, theme);
    let rect = form.column().groups()[0].rows()[row]
        .control_rect(FieldLayout { bounds, ..layout }, Scale::ONE, theme)
        .expect("the control is drawn");
    centre(rect)
}

fn click(form: &mut FormDialog, at: Point, theme: &Theme) -> Option<FormAction> {
    let _ = pointer(form, InputEvent::PointerMoved { to: at }, theme);
    let pressed = pointer(
        form,
        InputEvent::PointerPressed {
            button: PointerButton::Primary,
        },
        theme,
    );
    let released = pointer(
        form,
        InputEvent::PointerReleased {
            button: PointerButton::Primary,
        },
        theme,
    );
    pressed.or(released)
}

#[test]
fn escape_dismisses_and_enter_presses_the_trailing_action() {
    tairix_font::install_test_transport();
    let theme = Theme::light();
    let mut form = form();
    assert_eq!(
        key(&mut form, Key::Named(NamedKey::Escape), &theme),
        Some(FormAction::Dismissed)
    );
    assert_eq!(
        key(&mut form, Key::Named(NamedKey::Enter), &theme),
        Some(FormAction::Action(1))
    );
}

#[test]
fn a_click_on_an_action_activates_it() {
    tairix_font::install_test_transport();
    let theme = Theme::light();
    let mut form = form();
    let apply = centre(action_rect(&form, 1, &theme));
    assert_eq!(click(&mut form, apply, &theme), Some(FormAction::Action(1)));
    let cancel = centre(action_rect(&form, 0, &theme));
    assert_eq!(
        click(&mut form, cancel, &theme),
        Some(FormAction::Action(0))
    );
}

#[test]
fn a_press_begun_in_the_fields_presses_no_action_it_is_released_over() {
    tairix_font::install_test_transport();
    let theme = Theme::light();
    let mut form = form();
    let switch = control_centre(&form, FAST, &theme);
    let apply = centre(action_rect(&form, 1, &theme));
    let _ = pointer(&mut form, InputEvent::PointerMoved { to: switch }, &theme);
    let _ = pointer(
        &mut form,
        InputEvent::PointerPressed {
            button: PointerButton::Primary,
        },
        &theme,
    );
    let _ = pointer(&mut form, InputEvent::PointerMoved { to: apply }, &theme);
    let released = pointer(
        &mut form,
        InputEvent::PointerReleased {
            button: PointerButton::Primary,
        },
        &theme,
    );
    assert!(
        !matches!(released, Some(FormAction::Action(_))),
        "{released:?}"
    );
}

#[test]
fn an_open_list_keeps_escape_and_enter_and_reports_its_choice() {
    tairix_font::install_test_transport();
    let theme = Theme::light();
    let mut form = form();
    let model = control_centre(&form, MODEL, &theme);
    let _ = click(&mut form, model, &theme);
    assert!(form.column().is_listing(), "the click opens the list");
    let escaped = key(&mut form, Key::Named(NamedKey::Escape), &theme);
    assert!(
        matches!(
            escaped,
            Some(FormAction::Field(ref acted))
                if acted.action.action == FieldAction::Choices { expanded: false }
        ),
        "Escape is the list's, not the dialog's: {escaped:?}"
    );
    assert!(!form.column().is_listing(), "Escape closes the list");
    let _ = click(&mut form, model, &theme);
    assert_eq!(key(&mut form, Key::Named(NamedKey::Down), &theme), None);
    let chosen = key(&mut form, Key::Named(NamedKey::Enter), &theme);
    match chosen {
        Some(FormAction::Field(acted)) => {
            assert_eq!((acted.group, acted.action.row), (0, MODEL));
            assert_eq!(acted.action.action, FieldAction::Selected { index: 1 });
        }
        other => panic!("the list answers its choice: {other:?}"),
    }
}

#[test]
fn a_switch_shows_what_its_owner_took() {
    tairix_font::install_test_transport();
    let theme = Theme::light();
    let mut form = form();
    let mut damage = sink();
    form.show_toggle((0, FAST), true, (Scale::ONE, &theme), &mut damage);
    let on = match form.column().groups()[0].rows()[FAST].control() {
        FieldControl::Toggle(toggle) => toggle.is_on(),
        _ => false,
    };
    assert!(on);
    assert!(!damage.is_empty(), "the switch's row is redrawn");
}

#[test]
fn the_window_is_as_tall_as_what_it_holds() {
    tairix_font::install_test_transport();
    let theme = Theme::light();
    let form = form();
    let (width, height) = form.extent(Scale::ONE, &theme);
    assert_eq!(width, WIDTH);
    let band = form.dialog().content_width(width, Scale::ONE, &theme);
    let content = form.column().measured_height(band, Scale::ONE, &theme);
    assert!(height > content, "{height} holds {content} and the actions");
}
