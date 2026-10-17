use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write as _;

use tairix_abi::audio::AudioGain;
use tairix_abi::sound_cue::POLICY_DOCUMENT_MAX;
use tairix_appconf::{Document, Refusal, Registry, SettingPath};

use super::{layered, merge_pushed, pushable, UserChoices, PUSHABLE};
use crate::{is_theme_name, SoundEvent, SoundKey, SoundSettings};

fn path(text: &str) -> SettingPath {
    SettingPath::new(text).expect("a setting path")
}

fn stored(text: &str) -> (UserChoices, Vec<String>) {
    UserChoices::load(&Document::parse(text).expect("parses"))
}

#[test]
fn only_the_sounds_heard_while_a_session_holds_the_seat_are_a_users() {
    let between: Vec<SoundEvent> = SoundEvent::ALL
        .into_iter()
        .filter(|event| !layered(*event))
        .collect();
    assert_eq!(
        between,
        [SoundEvent::Startup, SoundEvent::Login, SoundEvent::Logout]
    );
}

#[test]
fn a_session_hands_over_every_switch_and_gain_it_decides_and_no_file() {
    assert_eq!(PUSHABLE.len(), 3 + 10);
    let in_order: Vec<SoundKey> = SoundSettings::KEYS
        .iter()
        .copied()
        .filter(|key| pushable(*key))
        .collect();
    assert_eq!(PUSHABLE.as_slice(), in_order.as_slice());
    for key in PUSHABLE {
        match key {
            SoundKey::Enabled | SoundKey::Theme | SoundKey::Gain => {}
            SoundKey::EventEnabled(event) => {
                assert!(layered(event), "{key}");
            }
            SoundKey::EventFile(_) => panic!("{key} travels as a descriptor"),
        }
    }
}

#[test]
fn a_pushed_document_naming_a_file_or_a_machine_sound_is_refused_whole() {
    let machine = SoundSettings::default();
    let pushed = merge_pushed(&machine, "sound.enabled = false\nsound.gain = -6dB\n")
        .expect("a session's choices");
    assert!(!pushed.enabled());
    assert_eq!(pushed.gain(), AudioGain::new(-600).expect("attenuation"));
    assert_eq!(
        merge_pushed(&machine, "sound.bell.file = /Users/ada/bell.flac\n"),
        Err(Refusal::OutsideGroup(SoundKey::EventFile(SoundEvent::Bell)))
    );
    assert_eq!(
        merge_pushed(
            &machine,
            "sound.enabled = false\nsound.login.enabled = false\n"
        ),
        Err(Refusal::OutsideGroup(SoundKey::EventEnabled(
            SoundEvent::Login
        )))
    );
    assert_eq!(
        merge_pushed(&machine, "sound.gain = 6dB\n"),
        Err(Refusal::InvalidValue(SoundKey::Gain))
    );
    assert_eq!(
        merge_pushed(&machine, "sound.volume = 11\n"),
        Err(Refusal::UnknownKey(String::from("sound.volume")))
    );
}

#[test]
fn a_stored_document_keeps_only_what_a_user_may_set() {
    let (choices, refused) = stored(
        "sound.bell.enabled = off\n\
         sound.error.file = /Users/ada/Home/Music/klaxon.flac\n\
         sound.login.enabled = false\n\
         sound.login.file = /Users/ada/hello.flac\n\
         sound.gain = 3dB\n\
         sound.volume = 11\n",
    );
    assert_eq!(
        refused,
        [
            "sound.login.enabled",
            "sound.login.file",
            "sound.gain",
            "sound.volume"
        ]
    );
    assert_eq!(
        choices.file(SoundEvent::Error),
        Some("/Users/ada/Home/Music/klaxon.flac")
    );
    assert_eq!(choices.file(SoundEvent::Login), None);
    assert_eq!(
        choices.pushed(),
        "sound.bell.enabled = false\n",
        "kept in its canonical spelling, and the file not handed over"
    );
}

#[test]
fn what_the_user_left_is_the_machines_to_decide() {
    let machine = merge_pushed(
        &SoundSettings::default(),
        "sound.theme = Glass\nsound.bell.enabled = false\nsound.gain = -3dB\n",
    )
    .expect("the machine's document");
    let (choices, _) = stored("sound.gain = -9dB\n");
    let over = choices.over(&machine);
    assert_eq!(over.theme(), "Glass");
    assert!(!over.event(SoundEvent::Bell).enabled);
    assert_eq!(over.gain(), AudioGain::new(-900).expect("attenuation"));
}

#[test]
fn an_applied_document_is_laid_over_the_choices_or_refused_whole() {
    let (mut choices, _) = stored("sound.gain = -6dB\n");
    choices
        .apply("sound.enabled = off\nsound.info.enabled = true\n")
        .expect("a pane's choices");
    assert_eq!(
        choices.pushed(),
        "sound.enabled = false\nsound.gain = -6dB\nsound.info.enabled = true\n"
    );
    let before = choices.clone();
    assert_eq!(
        choices.apply("sound.info.enabled = false\nsound.info.file = /a\n"),
        Err(Refusal::OutsideGroup(SoundKey::EventFile(SoundEvent::Info)))
    );
    assert_eq!(choices, before, "nothing of a refused document is taken");
}

#[test]
fn a_chosen_file_is_kept_and_cleared_but_never_handed_over_as_a_path() {
    let mut choices = UserChoices::default();
    assert!(choices.choose(SoundEvent::Error, &path("/Users/ada/klaxon.wav")));
    assert!(
        !choices.choose(SoundEvent::Logout, &path("/Users/ada/bye.wav")),
        "no user's file plays for a session's end"
    );
    assert_eq!(
        choices.file(SoundEvent::Error),
        Some("/Users/ada/klaxon.wav")
    );
    assert_eq!(choices.file(SoundEvent::Logout), None);
    assert_eq!(
        choices.pushed(),
        "sound.error.enabled = true\n",
        "the event's switch, never its file"
    );
    let stored = choices.document().render();
    assert_eq!(
        stored,
        "sound.error.enabled = true\nsound.error.file = /Users/ada/klaxon.wav\n"
    );
    assert_eq!(super::UserChoices::load(&choices.document()).0, choices);
    choices.use_theme(SoundEvent::Error);
    assert_eq!(choices.file(SoundEvent::Error), None);
    assert_eq!(choices.document().render(), "sound.error.enabled = true\n");
}

/// Choosing what an event plays — a file, or its theme's sound — is asking to
/// hear it, so a sound that was off comes on; a sound no user's choices reach
/// is not switched by either.
#[test]
fn choosing_what_an_event_plays_turns_it_on() {
    let off = |event: SoundEvent| stored(&format!("sound.{}.enabled = false\n", event.name())).0;
    let mut choices = off(SoundEvent::Error);
    assert!(choices.choose(SoundEvent::Error, &path("/Users/ada/klaxon.wav")));
    assert!(
        choices
            .over(&SoundSettings::default())
            .event(SoundEvent::Error)
            .enabled
    );
    let mut choices = off(SoundEvent::Bell);
    choices.use_theme(SoundEvent::Bell);
    assert!(
        choices
            .over(&SoundSettings::default())
            .event(SoundEvent::Bell)
            .enabled
    );
    let mut choices = UserChoices::default();
    choices.use_theme(SoundEvent::Login);
    assert!(
        choices.document().render().is_empty(),
        "a machine sound is not the user's to switch"
    );
}

/// The cue protocol carries the longest document a session can hand over.
#[test]
fn the_longest_pushed_document_fits_the_policy() {
    // The longest leaf name, every byte of it one the document escapes.
    let theme = "\\".repeat(255);
    assert!(is_theme_name(&theme));
    let mut text = format!(
        "sound.enabled = false\nsound.theme = \"{}\"\n",
        "\\\\".repeat(255)
    );
    for key in PUSHABLE {
        match key {
            SoundKey::EventEnabled(_) => {
                writeln!(text, "{} = false", SoundSettings::name(key)).expect("writes");
            }
            SoundKey::Gain => {
                writeln!(text, "{} = -21474836.48dB", SoundSettings::name(key)).expect("writes");
            }
            SoundKey::Enabled | SoundKey::Theme | SoundKey::EventFile(_) => {}
        }
    }
    let mut choices = UserChoices::default();
    choices.apply(&text).expect("every setting at its longest");
    assert_eq!(choices.over(&SoundSettings::default()).theme(), theme);
    let pushed = choices.pushed();
    assert!(
        pushed.len() <= POLICY_DOCUMENT_MAX,
        "{} bytes",
        pushed.len()
    );
    assert_eq!(
        merge_pushed(&SoundSettings::default(), &pushed),
        Ok(choices.over(&SoundSettings::default()))
    );
}
