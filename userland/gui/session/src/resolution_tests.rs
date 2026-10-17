//! Host tests of the resolution trial: its countdown, its answers, and that
//! nothing but *Keep Changes* keeps.

use tairix_abi::input::{KeyInput, KeyValue, Modifiers, NamedKeyCode};
use tairix_abi::time::NANOS_PER_SEC;

use tairix_wallpaper::Resolution;

use super::{
    resolution_ask, ResizedRecord, ResolutionAsk, ResolutionPrompt, Trial, TrialEnd, TrialOutcome,
    TRIAL_SECONDS,
};
use crate::alert::ALERT_ARM_NS;
use crate::tests::headless_desktop;

const OUTPUT: (u32, u32) = (1024, 768);
const SMALL: Resolution = Resolution::Size {
    width: 800,
    height: 600,
};

#[test]
fn an_apply_is_a_trial_only_when_it_changes_the_size_the_setting_draws() {
    assert_eq!(
        resolution_ask(SMALL, Resolution::Native, OUTPUT, false),
        ResolutionAsk::Trial((800, 600))
    );
    // A wallpaper change carries the setting in force: no trial, whatever
    // size the desktop could last be drawn at.
    assert_eq!(
        resolution_ask(SMALL, SMALL, OUTPUT, false),
        ResolutionAsk::Same
    );
    // A size this output does not offer draws the output's own, as native does.
    let unoffered = Resolution::Size {
        width: 1920,
        height: 1080,
    };
    assert_eq!(
        resolution_ask(unoffered, Resolution::Native, OUTPUT, false),
        ResolutionAsk::Same
    );
}

#[test]
fn a_new_size_is_never_put_up_behind_a_lock_or_a_screensaver() {
    assert_eq!(
        resolution_ask(Resolution::Native, SMALL, OUTPUT, true),
        ResolutionAsk::Covered
    );
    assert_eq!(
        resolution_ask(SMALL, SMALL, OUTPUT, true),
        ResolutionAsk::Same,
        "what changes no size is no trial, covered or not"
    );
}

const SECOND: u64 = NANOS_PER_SEC as u64;

fn trial() -> Trial {
    Trial {
        request: alloc::string::String::from("resolution = 800x600\n"),
        ticket: 7,
    }
}

fn pressed(key: NamedKeyCode) -> KeyInput {
    KeyInput::Pressed {
        key: KeyValue::Named(key),
        modifiers: Modifiers::default(),
    }
}

#[test]
fn the_prompt_opens_once_counting_down_and_parks_on_each_second() {
    let (mut shell, mut comp) = headless_desktop();
    let mut prompt = ResolutionPrompt::new();
    assert!(prompt.ask(trial(), 0, &mut shell, &mut comp).is_ok());
    assert!(prompt.wm_id().is_some());
    assert_eq!(prompt.park_deadline_ns(0, u64::MAX), SECOND);
    assert_eq!(prompt.park_deadline_ns(SECOND / 2, u64::MAX), SECOND / 2);
    assert_eq!(prompt.park_deadline_ns(0, 10), 10, "a nearer park stands");
    assert_eq!(
        prompt.ask(trial(), 0, &mut shell, &mut comp),
        Err(trial()),
        "one trial at a time"
    );
    assert!(!prompt.tick(SECOND, &mut shell, &mut comp));
    assert!(prompt.is_open());
    assert_eq!(prompt.take_ended(), None);
    assert_eq!(
        ResolutionPrompt::new().park_deadline_ns(0, 42),
        42,
        "idle parks nothing"
    );
}

#[test]
fn the_countdown_running_out_reverts() {
    let (mut shell, mut comp) = headless_desktop();
    let mut prompt = ResolutionPrompt::new();
    assert!(prompt.ask(trial(), 0, &mut shell, &mut comp).is_ok());
    let wm = prompt.wm_id().expect("showing");
    let due = TRIAL_SECONDS * SECOND;
    assert!(!prompt.tick(due - 1, &mut shell, &mut comp));
    assert!(prompt.tick(due, &mut shell, &mut comp));
    assert_eq!(prompt.take_ended(), Some(TrialEnd::Reverted(trial())));
    assert_eq!(prompt.take_ended(), None, "taken once");
    assert!(!prompt.is_open());
    assert!(comp.window(wm).is_none(), "the prompt's window is gone");
}

#[test]
fn enter_keeps_and_escape_or_the_other_button_reverts() {
    let (mut shell, mut comp) = headless_desktop();
    let mut prompt = ResolutionPrompt::new();
    assert!(prompt.ask(trial(), 0, &mut shell, &mut comp).is_ok());
    assert!(prompt.handle_key(
        &pressed(NamedKeyCode::Enter),
        ALERT_ARM_NS,
        &mut shell,
        &mut comp
    ));
    assert_eq!(prompt.take_ended(), Some(TrialEnd::Kept(trial())));

    assert!(prompt.ask(trial(), 0, &mut shell, &mut comp).is_ok());
    assert!(prompt.handle_key(
        &pressed(NamedKeyCode::Escape),
        ALERT_ARM_NS,
        &mut shell,
        &mut comp
    ));
    assert_eq!(prompt.take_ended(), Some(TrialEnd::Reverted(trial())));

    assert!(prompt.ask(trial(), 0, &mut shell, &mut comp).is_ok());
    assert!(!prompt.handle_key(
        &pressed(NamedKeyCode::Tab),
        ALERT_ARM_NS,
        &mut shell,
        &mut comp
    ));
    assert!(prompt.handle_key(
        &pressed(NamedKeyCode::Enter),
        ALERT_ARM_NS,
        &mut shell,
        &mut comp
    ));
    assert_eq!(
        prompt.take_ended(),
        Some(TrialEnd::Reverted(trial())),
        "the focus moved to Revert"
    );
}

#[test]
fn a_click_on_keep_keeps_and_a_click_beside_the_buttons_asks_on() {
    let (mut shell, mut comp) = headless_desktop();
    let mut prompt = ResolutionPrompt::new();
    assert!(prompt.ask(trial(), 0, &mut shell, &mut comp).is_ok());
    let keep = prompt
        .button_centre(crate::choice::ACCEPT, &shell, &comp)
        .expect("showing");
    assert!(!prompt.handle_click(
        tairix_wm::Point::new(2, 2),
        ALERT_ARM_NS,
        &mut shell,
        &mut comp
    ));
    assert!(prompt.is_open());
    assert!(
        !prompt.handle_click(keep, ALERT_ARM_NS - 1, &mut shell, &mut comp),
        "not before the buttons arm"
    );
    assert!(prompt.handle_click(keep, ALERT_ARM_NS, &mut shell, &mut comp));
    assert_eq!(prompt.take_ended(), Some(TrialEnd::Kept(trial())));
}

#[test]
fn an_abandoned_trial_reverts() {
    let (mut shell, mut comp) = headless_desktop();
    let mut prompt = ResolutionPrompt::new();
    assert!(!prompt.abandon(&mut shell, &mut comp), "nothing to abandon");
    assert!(prompt.ask(trial(), 0, &mut shell, &mut comp).is_ok());
    assert!(prompt.abandon(&mut shell, &mut comp));
    assert_eq!(prompt.take_ended(), Some(TrialEnd::Reverted(trial())));
}

#[test]
fn the_countdown_reads_in_whole_seconds_down_to_one() {
    assert_eq!(super::seconds_left(15 * SECOND, 0), 15);
    assert_eq!(super::seconds_left(15 * SECOND, 1), 15);
    assert_eq!(super::seconds_left(15 * SECOND, 14 * SECOND + 1), 1);
    assert_eq!(super::seconds_left(15 * SECOND, 15 * SECOND), 0);
    assert_eq!(super::next_step(15 * SECOND, 0), SECOND);
    assert_eq!(super::next_step(15 * SECOND, 14 * SECOND + 1), 15 * SECOND);
    assert!(super::countdown(1).ends_with("1 second."));
    assert!(super::countdown(2).ends_with("2 seconds."));
}

/// Each field rendered as a host reading the log sees it.
fn rendered(
    fields: &[tairix_log::Field<'static>],
) -> alloc::vec::Vec<(&'static str, alloc::string::String)> {
    use alloc::string::ToString;
    use tairix_log::FieldValue;
    fields
        .iter()
        .map(|field| {
            let value = match field.value {
                FieldValue::Str(text) => text.to_string(),
                FieldValue::UnsignedInt(value) => value.to_string(),
                ref other => alloc::format!("{other:?}"),
            };
            (field.key, value)
        })
        .collect()
}

/// The value `rendered` holds under `key`.
fn under<'a>(rendered: &'a [(&'static str, alloc::string::String)], key: &str) -> Option<&'a str> {
    rendered
        .iter()
        .find(|(name, _)| *name == key)
        .map(|(_, value)| value.as_str())
}

#[test]
fn the_witness_records_read_back_and_refuse_what_they_never_write() {
    let resized = ResizedRecord {
        width: 800,
        height: 600,
    };
    let fields = rendered(&resized.fields());
    assert_eq!(
        ResizedRecord::read(|key| under(&fields, key)),
        Some(resized)
    );
    assert_eq!(
        ResizedRecord::read(|key| (key == "width").then_some("800")),
        None
    );
    assert_eq!(
        ResizedRecord::read(|key| Some(if key == "width" { "800" } else { "tall" })),
        None
    );

    for outcome in [
        TrialOutcome::Kept,
        TrialOutcome::Refused,
        TrialOutcome::Reverted,
    ] {
        let fields = rendered(&outcome.fields());
        assert_eq!(TrialOutcome::read(|key| under(&fields, key)), Some(outcome));
    }
    assert_eq!(TrialOutcome::read(|_| Some("declined")), None);
    assert_eq!(TrialOutcome::read(|_| None), None);
}
