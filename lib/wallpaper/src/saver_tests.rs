use alloc::string::String;

use tairix_abi::bounded_text::BoundedText;
use tairix_abi::saver::{
    Choice, ChoiceValue, Choices, OptionKey, Range, SaverOption, SaverOptionKind, SaverValue,
};
use tairix_abi::switchboard_ipc::TaskOwner;
use tairix_abi::BundleId;
use tairix_appconf::Document;

use super::{
    keeps_values_of, read_value, spell_value, SaverValues, ScreensaverChoice, TaskNames,
    SAVER_VALUES_MAX,
};

fn saver(id: &str) -> BundleId {
    BundleId::new(id).expect("an identifier")
}

fn detail() -> SaverOption {
    let choice = |value: &str, label: &str| Choice {
        value: ChoiceValue::new(value).expect("a value"),
        label: BoundedText::new(label).expect("a label"),
    };
    SaverOption::new(
        OptionKey::new("detail").expect("a key"),
        (
            BoundedText::new("Detail").expect("a label"),
            BoundedText::EMPTY,
        ),
        SaverOptionKind::Choice {
            choices: Choices::new(&[choice("simple", "Simple"), choice("maximum", "Maximum")])
                .expect("two"),
            default: 0,
        },
    )
    .expect("an option")
}

fn interval() -> SaverOption {
    SaverOption::new(
        OptionKey::new("interval_s").expect("a key"),
        (
            BoundedText::new("Change every").expect("a label"),
            BoundedText::EMPTY,
        ),
        SaverOptionKind::Range {
            range: Range::new(5, 300, 5).expect("a range"),
            default: 30,
            unit: BoundedText::new("seconds").expect("a unit"),
        },
    )
    .expect("an option")
}

fn keep() -> SaverOption {
    SaverOption::new(
        OptionKey::new("keep").expect("a key"),
        (
            BoundedText::new("Keep").expect("a label"),
            BoundedText::EMPTY,
        ),
        SaverOptionKind::Switch { default: false },
    )
    .expect("an option")
}

#[test]
fn a_choice_is_blank_dim_or_a_bundle_identifier() {
    assert_eq!(
        ScreensaverChoice::from_value("blank"),
        Some(ScreensaverChoice::Blank)
    );
    assert_eq!(
        ScreensaverChoice::from_value("dim"),
        Some(ScreensaverChoice::Dim)
    );
    let clock = ScreensaverChoice::from_value("os.tairix.saver.clock").expect("a saver");
    assert_eq!(clock.as_str(), "os.tairix.saver.clock");
    for refused in ["", "Blank", "os..clock", "os/clock", "-lead"] {
        assert_eq!(ScreensaverChoice::from_value(refused), None, "{refused:?}");
    }
    assert!(matches!(
        ScreensaverChoice::default_saver(),
        ScreensaverChoice::Saver(_)
    ));
    assert_eq!(
        ScreensaverChoice::Dim.preview_path().as_deref(),
        Some("/System/Graphics/Screensavers/dim.png")
    );
    assert_eq!(clock.preview_path(), None);
    assert_eq!(
        ScreensaverChoice::of_preview("blank.png"),
        Some(ScreensaverChoice::Blank)
    );
    assert_eq!(
        ScreensaverChoice::of_preview("os.tairix.saver.clock.png"),
        None
    );
    assert_eq!(ScreensaverChoice::of_preview("dim.svg"), None);
}

#[test]
fn values_are_spelled_as_the_document_spells_them_and_read_back() {
    for (option, value, spelled) in [
        (keep(), SaverValue::Switch(true), "true"),
        (keep(), SaverValue::Switch(false), "false"),
        (
            detail(),
            SaverValue::Choice(ChoiceValue::new("maximum").expect("a value")),
            "maximum",
        ),
        (interval(), SaverValue::Range(60), "60"),
    ] {
        assert_eq!(spell_value(&value), spelled);
        assert_eq!(read_value(&option, spelled), Some(value));
    }
    assert_eq!(read_value(&keep(), "on"), Some(SaverValue::Switch(true)));
    assert_eq!(read_value(&detail(), "fine"), None);
    assert_eq!(read_value(&interval(), "61"), None);
    assert_eq!(read_value(&interval(), "1h"), None);
}

#[test]
fn a_value_is_held_only_where_it_differs_and_read_as_its_option_now_says() {
    let clock = saver("os.tairix.saver.clock");
    let mut values = SaverValues::default();
    assert_eq!(values.value(&clock, &interval()), SaverValue::Range(30));
    assert!(values.set(&clock, &interval(), SaverValue::Range(120)));
    assert_eq!(values.value(&clock, &interval()), SaverValue::Range(120));
    assert!(!values.set(&clock, &interval(), SaverValue::Range(121)));
    assert!(!values.set(&clock, &interval(), SaverValue::Switch(true)));
    let mut document = Document::new();
    values.spell(&mut document);
    assert_eq!(
        document.get("saver.os.tairix.saver.clock.interval_s"),
        Some("120")
    );
    assert!(values.set(&clock, &interval(), SaverValue::Range(30)));
    let mut emptied = Document::new();
    values.spell(&mut emptied);
    assert_eq!(
        emptied.settings().count(),
        0,
        "a default is held by holding nothing"
    );
    // A value its option no longer accepts reads as the default.
    let mut stale = SaverValues::default();
    assert!(stale.read("saver.os.tairix.saver.clock.interval_s", "301"));
    assert_eq!(stale.value(&clock, &interval()), SaverValue::Range(30));
}

#[test]
fn only_a_key_and_value_a_saver_could_hold_are_read() {
    let mut values = SaverValues::default();
    for (key, value) in [
        ("saver.os.tairix.saver.clock.detail", "maximum"),
        ("saver.os.tairix.saver.clock.interval_s", "-30"),
    ] {
        assert!(values.read(key, value), "{key} = {value}");
    }
    for (key, value) in [
        ("saver.clock", "maximum"),
        ("saver.os.tairix.saver.clock.Detail", "maximum"),
        ("saver.os.tairix.saver.clock.detail", "Maximum"),
        ("saver.os.tairix.saver.clock.detail", "two words"),
        ("saver.os..clock.detail", "maximum"),
        ("screensaver.os.tairix.clock.detail", "maximum"),
    ] {
        assert!(!values.read(key, value), "{key} = {value}");
    }
}

#[test]
fn values_are_bounded_and_pruned_to_the_savers_still_held() {
    let mut values = SaverValues::default();
    for n in 0..SAVER_VALUES_MAX {
        let key = alloc::format!("saver.os.tairix.saver.s{n}.keep");
        assert!(values.read(&key, "true"));
    }
    assert!(!values.read("saver.os.tairix.saver.over.keep", "true"));
    assert!(
        values.read("saver.os.tairix.saver.s0.keep", "false"),
        "a held key is replaced"
    );
    values.retain_savers(|id| id.as_str() == "os.tairix.saver.s1");
    let mut document = Document::new();
    values.spell(&mut document);
    let keys: alloc::vec::Vec<String> = document
        .settings()
        .map(|setting| String::from(setting.key))
        .collect();
    assert_eq!(keys, ["saver.os.tairix.saver.s1.keep"]);
}

#[test]
fn a_saver_whose_identifier_is_too_deep_keeps_no_values() {
    assert!(keeps_values_of(&saver("os.tairix.saver.clock")));
    assert!(!keeps_values_of(&saver("a.b.c.d.e.f.g")));
}

#[test]
fn task_names_follow_their_owner() {
    assert!(TaskNames::default().names(TaskOwner::Own));
    assert!(!TaskNames::default().names(TaskOwner::Other));
    assert!(!TaskNames::None.names(TaskOwner::Own));
    assert!(TaskNames::All.names(TaskOwner::Other));
    for names in TaskNames::ALL {
        assert_eq!(TaskNames::from_value(names.as_str()), Some(names));
    }
    assert_eq!(TaskNames::from_value("everyone"), None);
}
