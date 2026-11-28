use alloc::string::String;

use tairix_controls::FieldGroupAction;
use tairix_geometry::Scale;
use tairix_theme::ThemeRegistry;

use super::*;

fn spelled(settings: &Settings) -> alloc::vec::Vec<(&'static str, String)> {
    Settings::KEYS
        .iter()
        .filter_map(|&key| {
            let mut out = String::new();
            settings
                .spell(key, &mut out)
                .then(|| (Settings::name(key), out))
        })
        .collect()
}

#[test]
fn every_choice_is_kept_as_it_is_read_back() {
    let chosen = Settings {
        buttons: ButtonOrder::Mouse,
        ceiling_mib: Some(512),
        isolate_all: true,
        scaling: Scaling::Smooth,
    };
    let mut read = Settings::default();
    for (name, text) in spelled(&chosen) {
        let key = *Settings::KEYS
            .iter()
            .find(|&&key| Settings::name(key) == name)
            .unwrap();
        assert!(read.read(key, &text), "{name} = {text}");
    }
    assert_eq!(read, chosen);
}

#[test]
fn the_defaults_store_no_ceiling() {
    let names: alloc::vec::Vec<&str> = spelled(&Settings::default())
        .into_iter()
        .map(|(name, _)| name)
        .collect();
    assert_eq!(
        names,
        ["input.buttons", "sessions.isolate-all", "screens.scaling"]
    );
    assert_eq!(Settings::default().ceiling_bytes(), None);
}

#[test]
fn a_value_no_choice_has_is_refused() {
    let mut settings = Settings::default();
    assert!(!settings.read(Key::Ceiling, "100"), "not a choice");
    assert!(!settings.read(Key::Ceiling, "lots"));
    assert!(!settings.read(Key::Buttons, "left"));
    assert!(!settings.read(Key::IsolateAll, "perhaps"));
    assert!(!settings.read(Key::Scaling, "blurry"));
    assert_eq!(settings, Settings::default(), "left as it was");
    assert!(settings.read(Key::Ceiling, "2048"));
    assert_eq!(settings.ceiling_bytes(), Some(2 << 30));
}

fn act(form: &mut Form, group: usize, row: usize, action: FieldAction) -> Option<Edited> {
    let themes = ThemeRegistry::with_builtins();
    let mut damage = Region::new();
    form.take(
        &ColumnAction {
            group,
            action: FieldGroupAction { row, action },
        },
        (Scale::ONE, themes.active()),
        &mut damage,
    )
}

#[test]
fn each_control_changes_its_setting_and_says_when_it_settled() {
    let mut form = Form::new(Settings::default());
    let changed = Some(Edited::Changed { settled: true });
    assert_eq!(
        act(
            &mut form,
            at::INPUT,
            at::BUTTONS,
            FieldAction::Selected { index: 1 }
        ),
        changed
    );
    assert_eq!(form.settings().buttons, ButtonOrder::Mouse);
    assert_eq!(
        act(
            &mut form,
            at::SESSIONS,
            at::CEILING,
            FieldAction::Selected { index: 3 }
        ),
        changed
    );
    assert_eq!(form.settings().ceiling_mib, Some(CEILINGS_MIB[2]));
    assert_eq!(
        act(
            &mut form,
            at::SESSIONS,
            at::CEILING,
            FieldAction::Selected { index: 0 }
        ),
        changed
    );
    assert_eq!(form.settings().ceiling_mib, None, "automatic");
    assert_eq!(
        act(
            &mut form,
            at::SESSIONS,
            at::ISOLATE,
            FieldAction::Set { on: true }
        ),
        changed
    );
    assert!(form.settings().isolate_all);
    assert_eq!(
        act(
            &mut form,
            at::SCREENS,
            at::SCALING,
            FieldAction::Selected { index: 1 }
        ),
        changed
    );
    assert_eq!(form.settings().scaling, Scaling::Smooth);
}

#[test]
fn choosing_what_is_already_chosen_changes_nothing() {
    let mut form = Form::new(Settings::default());
    assert_eq!(
        act(
            &mut form,
            at::SCREENS,
            at::SCALING,
            FieldAction::Selected { index: 0 }
        ),
        None
    );
    assert_eq!(
        act(
            &mut form,
            at::SESSIONS,
            at::CEILING,
            FieldAction::Selected { index: 99 }
        ),
        None,
        "no such choice: automatic, as it was"
    );
}

#[test]
fn a_form_opens_on_what_was_chosen() {
    let chosen = Settings {
        ceiling_mib: Some(4096),
        ..Settings::default()
    };
    let mut form = Form::new(chosen);
    assert_eq!(form.settings(), &chosen);
    assert_eq!(
        act(
            &mut form,
            at::SESSIONS,
            at::CEILING,
            FieldAction::Selected { index: 7 }
        ),
        None,
        "4 GB is the seventh size, already chosen"
    );
}

#[test]
fn sizes_are_named_in_the_larger_unit_they_fill() {
    assert_eq!(size(64), "64 MB");
    assert_eq!(size(1024), "1 GB");
    assert_eq!(size(4096), "4 GB");
}
