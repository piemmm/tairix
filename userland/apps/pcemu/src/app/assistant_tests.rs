use super::*;
use tairix_input::Modifiers;

fn names() -> [String; 4] {
    TEMPLATES.map(|(_, name)| String::from(name))
}

fn setup() -> (Assistant, Scale, Theme) {
    tairix_font::install_test_transport();
    (Assistant::new(names()), Scale::ONE, Theme::light())
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
    let (bounds, _) = assistant.layout(Scale::ONE, &Theme::light());
    let action = FieldGroupAction {
        row,
        action: FieldAction::Selected { index },
    };
    assistant.take(&action, &mut Region::new(), bounds);
}

#[test]
fn enter_creates_the_machine_offered_with_its_disk_and_escape_cancels() {
    let (mut assistant, ..) = setup();
    assert_eq!(
        press(&mut assistant, Key::Named(NamedKey::Enter)),
        Some(Answer::Create {
            template: Template::TurboXt,
            name: String::from("Turbo XT"),
            disk: true
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
    choose(&mut assistant, MODEL, 1);
    choose(&mut assistant, DISK, 1);
    let description = assistant.fields.rows()[MODEL].description().unwrap();
    assert!(description.starts_with("8088"), "{description}");
    assert!(!description.contains("hard disk"), "{description}");
    assert_eq!(
        assistant.answer(1),
        Answer::Create {
            template: Template::IbmXt,
            name: String::from("IBM Personal Computer XT"),
            disk: false
        },
        "the offered name follows the model"
    );
}

#[test]
fn a_machine_with_no_controller_is_offered_no_disk() {
    let (mut assistant, ..) = setup();
    choose(&mut assistant, MODEL, 0);
    choose(&mut assistant, DISK, 0);
    let Answer::Create { template, disk, .. } = assistant.answer(1) else {
        panic!("created");
    };
    assert_eq!((template, disk), (Template::IbmPc, false));
    assert!(!assistant.fields.rows()[DISK].state().enabled);
    choose(&mut assistant, MODEL, 2);
    assert!(
        matches!(assistant.answer(1), Answer::Create { disk: true, .. }),
        "a machine with one is offered its disk again"
    );
}

#[test]
fn a_name_typed_is_kept_whatever_model_is_chosen_and_a_blank_one_is_the_offered() {
    let (mut assistant, ..) = setup();
    if let FieldControl::Text(field) = assistant.fields.rows_mut()[NAME].control_mut() {
        field.set_text("  Games box ");
    }
    choose(&mut assistant, MODEL, 3);
    assert!(matches!(
        assistant.answer(1),
        Answer::Create { ref name, .. } if name == "Games box"
    ));
    if let FieldControl::Text(field) = assistant.fields.rows_mut()[NAME].control_mut() {
        field.set_text("   ");
    }
    assert!(matches!(
        assistant.answer(1),
        Answer::Create { ref name, .. } if name == "80186 workstation"
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
    let (bounds, fields) = assistant.layout(scale, &theme);
    let below = Point::new(bounds.right() - 20, bounds.bottom() - 20);
    assert!(
        !fields.bounds.contains(below),
        "the actions are beneath the fields"
    );
    let mut damage = Region::new();
    let mut answer = None;
    for event in [
        InputEvent::PointerMoved { to: below },
        InputEvent::PointerPressed {
            button: tairix_input::PointerButton::Primary,
        },
        InputEvent::PointerReleased {
            button: tairix_input::PointerButton::Primary,
        },
    ] {
        answer = answer.or(assistant.on_input(&event, 0, (scale, &theme), &mut damage));
    }
    assert!(
        matches!(
            answer,
            Some(Answer::Create {
                template: Template::TurboXt,
                disk: true,
                ..
            })
        ),
        "the trailing action is Create: {answer:?}"
    );
}

fn pointer(assistant: &mut Assistant, event: InputEvent) -> Option<Answer> {
    assistant.on_input(&event, 0, (Scale::ONE, &Theme::light()), &mut Region::new())
}

fn click(assistant: &mut Assistant, at: Point) -> Option<Answer> {
    let primary = tairix_input::PointerButton::Primary;
    pointer(assistant, InputEvent::PointerMoved { to: at });
    pointer(assistant, InputEvent::PointerPressed { button: primary });
    pointer(assistant, InputEvent::PointerReleased { button: primary })
}

#[test]
fn the_model_list_stays_open_under_the_pointer_and_takes_the_model_clicked() {
    let (mut assistant, scale, theme) = setup();
    let (_, fields) = assistant.layout(scale, &theme);
    let bounds = assistant
        .fields
        .row_rect(MODEL, fields, scale, &theme)
        .unwrap();
    let field = assistant.fields.rows()[MODEL]
        .control_rect(FieldLayout { bounds, ..fields }, scale, &theme)
        .unwrap()
        .center();
    click(&mut assistant, field);
    let (_, fields) = assistant.layout(scale, &theme);
    assert!(!fields.popup.is_empty(), "a click opens a placed list");
    let FieldControl::Combo(combo) = assistant.fields.rows()[MODEL].control() else {
        panic!("the model row is a choice");
    };
    let pc = combo
        .menu()
        .row_rect(0, fields.popup, scale, &theme)
        .unwrap()
        .center();
    pointer(&mut assistant, InputEvent::PointerMoved { to: pc });
    assert!(assistant.listing(), "moving onto the list leaves it open");
    click(&mut assistant, pc);
    assert!(!assistant.listing());
    assert_eq!(
        assistant.answer(1),
        Answer::Create {
            template: Template::IbmPc,
            name: String::from("IBM Personal Computer"),
            disk: false
        }
    );
}
