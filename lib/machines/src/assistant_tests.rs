use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use super::*;
use tairix_controls::{ColumnAction, FieldGroupAction, FieldLayout};
use tairix_geometry::Point;
use tairix_input::{Key, Modifiers, NamedKey, PointerButton};

const DISKS: [&str; 2] = ["A new disk", "None"];

/// A toy application's models: one with no disk to make, two with one.
const OFFERS: [Offer; 3] = [
    Offer {
        label: "Plain",
        choice: Choice::Fixed {
            label: "Disk",
            shown: "None",
            why: "This machine takes no disk.",
        },
    },
    Offer {
        label: "Disked",
        choice: Choice::Offered {
            label: "Disk",
            choices: &DISKS,
        },
    },
    Offer {
        label: "Big",
        choice: Choice::Offered {
            label: "Disk",
            choices: &DISKS,
        },
    },
];

fn describe(model: usize, choice: Option<usize>) -> String {
    let disk = if choice == Some(0) {
        "with a disk"
    } else {
        "without"
    };
    format!("{} {disk}", OFFERS[model].label)
}

fn names() -> Vec<String> {
    OFFERS
        .iter()
        .map(|offer| String::from(offer.label))
        .collect()
}

/// The assistant offering the second model first.
fn setup() -> (Assistant, Scale, Theme) {
    tairix_font::install_test_transport();
    (
        Assistant::new(OFFERS.to_vec(), names(), 1, describe),
        Scale::ONE,
        Theme::light(),
    )
}

fn press(assistant: &mut Assistant, key: Key) -> Option<Answer> {
    let mut damage = Region::new();
    let event = InputEvent::KeyPressed {
        key,
        modifiers: Modifiers::default(),
    };
    assistant.on_input(&event, 0, (Scale::ONE, &Theme::light()), &mut damage)
}

fn choose(assistant: &mut Assistant, row: usize, index: usize) {
    let acted = ColumnAction {
        group: 0,
        action: FieldGroupAction {
            row,
            action: FieldAction::Selected { index },
        },
    };
    assistant.take(&acted, (Scale::ONE, &Theme::light()), &mut Region::new());
}

fn set_typed(assistant: &mut Assistant, text: &str) {
    if let Some(FieldControl::Text(field)) = assistant.row_mut(NAME).map(FieldRow::control_mut) {
        field.set_text(String::from(text));
    }
}

fn pointer(assistant: &mut Assistant, event: InputEvent) -> Option<Answer> {
    assistant.on_input(&event, 0, (Scale::ONE, &Theme::light()), &mut Region::new())
}

fn click(assistant: &mut Assistant, at: Point) -> Option<Answer> {
    pointer(assistant, InputEvent::PointerMoved { to: at });
    let pressed = pointer(
        assistant,
        InputEvent::PointerPressed {
            button: PointerButton::Primary,
        },
    );
    let released = pointer(
        assistant,
        InputEvent::PointerReleased {
            button: PointerButton::Primary,
        },
    );
    pressed.or(released)
}

#[test]
fn enter_creates_the_machine_offered_with_its_choice_and_escape_cancels() {
    let (mut assistant, ..) = setup();
    assert_eq!(
        press(&mut assistant, Key::Named(NamedKey::Enter)),
        Some(Answer::Create {
            model: 1,
            name: String::from("Disked"),
            choice: Some(0)
        })
    );
    let (mut assistant, ..) = setup();
    assert_eq!(
        press(&mut assistant, Key::Named(NamedKey::Escape)),
        Some(Answer::Cancel)
    );
}

#[test]
fn a_choice_made_is_what_is_created_and_the_description_follows_it() {
    let (mut assistant, ..) = setup();
    choose(&mut assistant, MODEL, 2);
    choose(&mut assistant, CHOICE, 1);
    let description = assistant.row(MODEL).unwrap().description().unwrap();
    assert_eq!(description, "Big without");
    assert_eq!(
        assistant.answer(1),
        Answer::Create {
            model: 2,
            name: String::from("Big"),
            choice: Some(1)
        },
        "the offered name follows the model"
    );
}

#[test]
fn a_fixed_choice_is_shown_unchangeable_and_answers_none() {
    let (mut assistant, ..) = setup();
    choose(&mut assistant, MODEL, 0);
    choose(&mut assistant, CHOICE, 0);
    assert!(matches!(
        assistant.answer(1),
        Answer::Create {
            model: 0,
            choice: None,
            ..
        }
    ));
    assert!(!assistant.row(CHOICE).unwrap().state().enabled);
    choose(&mut assistant, MODEL, 2);
    assert!(
        matches!(
            assistant.answer(1),
            Answer::Create {
                choice: Some(0),
                ..
            }
        ),
        "a model with the choice offers it again, as it was"
    );
}

#[test]
fn a_name_typed_is_kept_whatever_model_is_chosen_and_a_blank_one_is_the_offered() {
    let (mut assistant, ..) = setup();
    set_typed(&mut assistant, "  Games box ");
    choose(&mut assistant, MODEL, 2);
    assert!(matches!(
        assistant.answer(1),
        Answer::Create { ref name, .. } if name == "Games box"
    ));
    set_typed(&mut assistant, "   ");
    assert!(matches!(
        assistant.answer(1),
        Answer::Create { ref name, .. } if name == "Big"
    ));
}

#[test]
fn the_window_is_as_tall_as_what_it_holds_and_paints() {
    let (assistant, scale, theme) = setup();
    let (width, height) = assistant.extent(scale, &theme);
    assert_eq!(width, WIDTH);
    assert!(height > 100 && height < 600, "{height}");
    let mut surface = Surface::new(width, height).unwrap();
    assistant.render(&mut surface, scale, &theme);
}

#[test]
fn a_click_on_create_answers_with_what_is_chosen() {
    let (mut assistant, scale, theme) = setup();
    let (bounds, place) = assistant.form.place(scale, &theme);
    let below = Point::new(bounds.right() - 20, bounds.bottom() - 20);
    assert!(
        !place.bounds.contains(below),
        "the actions are beneath the fields"
    );
    let answer = click(&mut assistant, below);
    assert!(
        matches!(
            answer,
            Some(Answer::Create {
                model: 1,
                choice: Some(0),
                ..
            })
        ),
        "the trailing action is Create: {answer:?}"
    );
}

#[test]
fn the_model_list_stays_open_under_the_pointer_and_takes_the_model_clicked() {
    let (mut assistant, scale, theme) = setup();
    let (_, place) = assistant.form.place(scale, &theme);
    let layout = assistant.column().layouts(place)[0].1;
    let group = &assistant.column().groups()[0];
    let bounds = group.row_rect(MODEL, layout, scale, &theme).unwrap();
    let field = group.rows()[MODEL]
        .control_rect(FieldLayout { bounds, ..layout }, scale, &theme)
        .unwrap()
        .center();
    click(&mut assistant, field);
    let (_, place) = assistant.form.place(scale, &theme);
    let layout = assistant.column().layouts(place)[0].1;
    assert!(!layout.popup.is_empty(), "a click opens a placed list");
    let FieldControl::Combo(combo) = assistant.column().groups()[0].rows()[MODEL].control() else {
        panic!("the model row is a choice");
    };
    let plain = combo
        .menu()
        .row_rect(0, layout.popup, scale, &theme)
        .unwrap()
        .center();
    pointer(&mut assistant, InputEvent::PointerMoved { to: plain });
    assert!(
        assistant.column().is_listing(),
        "moving onto the list leaves it open"
    );
    click(&mut assistant, plain);
    assert!(!assistant.column().is_listing());
    assert_eq!(
        assistant.answer(1),
        Answer::Create {
            model: 0,
            name: String::from("Plain"),
            choice: None
        }
    );
}
