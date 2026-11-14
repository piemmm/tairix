use super::*;
use tairix_geometry::{Region, Scale};
use tairix_input::{InputEvent, Key, Modifiers, NamedKey};
use tairix_machines::assistant::Answer;
use tairix_theme::Theme;

#[test]
fn enter_makes_the_turbo_xt_with_a_disk_under_the_name_offered() {
    tairix_font::install_test_transport();
    let names = TEMPLATES
        .iter()
        .map(|(_, name)| String::from(*name))
        .collect();
    let mut assistant = assistant(names);
    let enter = InputEvent::KeyPressed {
        key: Key::Named(NamedKey::Enter),
        modifiers: Modifiers::default(),
    };
    let answer = assistant.on_input(&enter, 0, (Scale::ONE, &Theme::light()), &mut Region::new());
    let Some(Answer::Create {
        model,
        name,
        choice,
    }) = answer
    else {
        panic!("created: {answer:?}");
    };
    assert_eq!(name, "Turbo XT");
    assert_eq!(chosen(model, choice), Some((Template::TurboXt, true)));
}

#[test]
fn only_a_machine_with_a_controller_is_offered_a_disk() {
    for (offer, (template, label)) in offers().iter().zip(TEMPLATES) {
        assert_eq!(offer.label, label);
        let offered = matches!(offer.choice, Choice::Offered { .. });
        assert_eq!(offered, template.config().fixed.is_some(), "{label}");
    }
    assert!(matches!(offers()[0].choice, Choice::Fixed { .. }), "the PC");
    assert_eq!(chosen(0, None), Some((Template::IbmPc, false)));
    assert_eq!(chosen(1, Some(1)), Some((Template::IbmXt, false)));
    assert_eq!(chosen(TEMPLATES.len(), Some(0)), None);
}

#[test]
fn the_description_counts_the_disk_chosen() {
    assert!(describe(1, Some(A_NEW_DISK)).ends_with("1 hard disk \u{b7} AdLib"));
    assert!(!describe(1, Some(1)).contains("hard disk"));
    assert!(describe(9, None).is_empty());
}
