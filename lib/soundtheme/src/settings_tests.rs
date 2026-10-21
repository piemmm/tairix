use alloc::format;
use alloc::vec::Vec;

use tairix_abi::audio::AudioGain;
use tairix_appconf::{validate_key, Document, Refusal, Registry, MOST_KEYS};

use super::{in_namespace, SoundKey, SoundSettings};
use crate::user::merge_pushed;
use crate::{SoundEvent, DEFAULT_THEME};

fn load(text: &str) -> (SoundSettings, Vec<SoundKey>) {
    SoundSettings::load(&Document::parse(text).expect("a document"))
}

#[test]
fn every_key_is_spelled_once_as_its_event_names_it() {
    assert!(SoundSettings::KEYS.len() <= MOST_KEYS);
    let mut names: Vec<&str> = SoundSettings::KEYS
        .iter()
        .map(|&key| SoundSettings::name(key))
        .collect();
    for name in &names {
        assert_eq!(validate_key(name), Ok(()), "{name}");
        assert!(in_namespace(name), "{name} lives under the sound namespace");
    }
    assert_eq!(SoundSettings::name(SoundKey::Gain), "sound.gain");
    for event in SoundEvent::ALL {
        for (key, field) in [
            (SoundKey::EventEnabled(event), "enabled"),
            (SoundKey::EventFile(event), "file"),
        ] {
            assert_eq!(
                SoundSettings::name(key),
                format!("sound.{}.{field}", event.name())
            );
            assert!(SoundSettings::KEYS.contains(&key));
        }
    }
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), SoundSettings::KEYS.len());
}

#[test]
fn nothing_stored_means_every_sound_on_at_unity_from_the_shipped_theme() {
    let (settings, refused) = load("");
    assert!(refused.is_empty());
    assert_eq!(settings, SoundSettings::default());
    assert!(settings.enabled());
    assert_eq!(settings.theme(), DEFAULT_THEME);
    assert_eq!(settings.gain(), AudioGain::UNITY);
    for event in SoundEvent::ALL {
        let own = settings.event(event);
        assert!(own.enabled);
        assert_eq!(own.file, None);
    }
}

#[test]
fn a_document_round_trips_through_its_own_spelling() {
    let text = "sound.enabled = false\n\
                sound.theme = High Contrast\n\
                sound.gain = -6.5dB\n\
                sound.bell.enabled = off\n\
                sound.error.file = /Users/ada/Home/Music/oops.flac\n";
    let (settings, refused) = load(text);
    assert!(refused.is_empty());
    assert!(!settings.enabled());
    assert_eq!(settings.theme(), "High Contrast");
    assert!(!settings.event(SoundEvent::Bell).enabled);
    assert_eq!(settings.gain(), AudioGain::new(-650).expect("attenuation"));
    assert_eq!(
        settings
            .event(SoundEvent::Error)
            .file
            .as_ref()
            .map(tairix_appconf::SettingPath::as_str),
        Some("/Users/ada/Home/Music/oops.flac")
    );
    let rendered = settings.document_of(SoundSettings::KEYS).render();
    let (again, refused) = load(&rendered);
    assert!(refused.is_empty());
    assert_eq!(again, settings);
}

#[test]
fn a_stored_value_refused_costs_only_its_own_key() {
    let (settings, refused) = load(
        "sound.enabled = maybe\n\
         sound.theme = ../escape\n\
         sound.gain = 3dB\n\
         sound.info.file = relative.flac\n\
         sound.startup.enabled = false\n",
    );
    assert_eq!(
        refused,
        [
            SoundKey::Enabled,
            SoundKey::Theme,
            SoundKey::Gain,
            SoundKey::EventFile(SoundEvent::Info),
        ]
    );
    assert!(settings.enabled());
    assert_eq!(settings.theme(), DEFAULT_THEME);
    assert_eq!(settings.gain(), AudioGain::UNITY, "never a boost");
    assert!(
        !settings.event(SoundEvent::Startup).enabled,
        "a good key beside bad ones is still read"
    );
}

#[test]
fn an_empty_file_names_no_replacement() {
    let (settings, refused) =
        load("sound.login.file = \"\"\nsound.error.file = /Users/ada/klaxon.flac\n");
    assert!(refused.is_empty());
    assert_eq!(settings.event(SoundEvent::Login).file, None);
    assert_eq!(
        settings
            .event(SoundEvent::Error)
            .file
            .as_ref()
            .map(tairix_appconf::SettingPath::as_str),
        Some("/Users/ada/klaxon.flac")
    );
}

#[test]
fn a_refusal_says_which_setting_refused_its_value() {
    let boost = merge_pushed(&SoundSettings::default(), "sound.gain = 6dB\n");
    assert_eq!(boost, Err(Refusal::InvalidValue(SoundKey::Gain)));
    assert_eq!(
        format!("{}", boost.unwrap_err()),
        "`sound.gain` is not a value that setting accepts"
    );
}

/// A gain is one setting for every sound, not one an event can carry: a
/// per-event spelling names nothing.
#[test]
fn a_gain_is_the_documents_not_an_events() {
    assert_eq!(
        merge_pushed(&SoundSettings::default(), "sound.bell.gain = -6dB\n"),
        Err(Refusal::UnknownKey(alloc::string::String::from(
            "sound.bell.gain"
        )))
    );
}
