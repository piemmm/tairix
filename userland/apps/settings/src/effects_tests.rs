use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::audio::AudioGain;
use tairix_abi::pinboard_ipc::{SoundFile, SoundReading};
use tairix_abi::sound_cue::SoundEvent;
use tairix_abi::Errno;
use tairix_controls::{FieldAction, FieldControl};
use tairix_soundtheme::{layered, merge_pushed, SoundKey, SoundSettings};

use super::{
    effects_ask, groups, EffectFile, EffectsAsk, EffectsAsks, EffectsReading, EffectsRow,
    EffectsTrip,
};

fn reading(document: &str) -> EffectsReading {
    let mut files = [SoundFile::Theme; 13];
    files[SoundEvent::Error.index()] = SoundFile::Chosen("klaxon.wav");
    files[SoundEvent::Info.index()] = SoundFile::Unavailable("gone.flac");
    EffectsReading::of(
        &SoundReading { document, files },
        alloc::vec![String::from("Glass"), String::from("TAIRiX")],
    )
    .expect("a document the registry reads")
}

#[test]
fn a_reading_is_the_sessions_document_and_files_by_name() {
    let reading = reading("sound.enabled = false\nsound.bell.gain = -6dB\n");
    assert!(!reading.settings.enabled());
    assert_eq!(
        reading.settings.event(SoundEvent::Bell).gain,
        AudioGain::new(-600).expect("attenuation")
    );
    assert_eq!(
        reading.files[SoundEvent::Error.index()],
        EffectFile::Chosen(String::from("klaxon.wav"))
    );
    assert_eq!(
        EffectsReading::of(
            &SoundReading {
                document: "sound.bell.file = /a\n",
                files: [SoundFile::Theme; 13],
            },
            Vec::new()
        ),
        Err(Errno::OutOfRange),
        "a path is never what the session answers"
    );
}

/// The pane offers every sound a user's choices reach, and states the ones
/// they do not reach rather than offering a switch that would change nothing.
#[test]
fn the_pane_offers_each_sound_a_user_decides_and_states_the_rest() {
    let (groups, owners) = groups(Some(&reading("")));
    let layered = SoundEvent::ALL
        .into_iter()
        .filter(|event| layered(*event))
        .count();
    assert_eq!(groups.len(), 1 + layered);
    assert_eq!(owners[0], [EffectsRow::Enabled, EffectsRow::Theme]);
    let machine = &groups[0].rows()[2];
    assert!(matches!(machine.control(), FieldControl::Reading(_)));
    for (group, owned) in groups.iter().zip(&owners).skip(1) {
        assert_eq!(
            owned.len(),
            group.rows().len(),
            "every event row changes something"
        );
    }
    let events: Vec<SoundEvent> = owners
        .iter()
        .skip(1)
        .filter_map(|rows| match rows[0] {
            EffectsRow::EventEnabled(event) => Some(event),
            _ => None,
        })
        .collect();
    assert!(!events.contains(&SoundEvent::Login));
    assert!(events.contains(&SoundEvent::Bell));
    let (unread, unowned) = groups_of_none();
    assert_eq!((unread, unowned), (1, 0));
}

fn groups_of_none() -> (usize, usize) {
    let (groups, owners) = groups(None);
    (groups.len(), owners.iter().map(Vec::len).sum())
}

/// Each control asks for exactly the setting it shows, rendered by the one
/// registry; a level asks once it settles, never while dragged.
#[test]
fn each_control_asks_for_its_own_setting_and_a_level_only_where_it_settles() {
    let reading = reading("");
    assert_eq!(
        effects_ask(
            EffectsRow::Enabled,
            &FieldAction::Set { on: false },
            &reading
        ),
        Some(EffectsAsk::Apply {
            key: SoundKey::Enabled,
            line: String::from("sound.enabled = false\n")
        })
    );
    assert_eq!(
        effects_ask(
            EffectsRow::Theme,
            &FieldAction::Selected { index: 0 },
            &reading
        ),
        Some(EffectsAsk::Apply {
            key: SoundKey::Theme,
            line: String::from("sound.theme = Glass\n")
        })
    );
    assert_eq!(
        effects_ask(
            EffectsRow::EventEnabled(SoundEvent::Bell),
            &FieldAction::Set { on: false },
            &reading
        ),
        Some(EffectsAsk::Apply {
            key: SoundKey::EventEnabled(SoundEvent::Bell),
            line: String::from("sound.bell.enabled = false\n")
        })
    );
    let gain = EffectsRow::EventGain(SoundEvent::Error);
    assert_eq!(
        effects_ask(gain, &FieldAction::SetValue { permille: 500 }, &reading),
        None,
        "a drag on its way asks nothing"
    );
    let Some(EffectsAsk::Apply {
        key: SoundKey::EventGain(SoundEvent::Error),
        line: document,
    }) = effects_ask(gain, &FieldAction::Settled { permille: 500 }, &reading)
    else {
        panic!("a settled level is a change");
    };
    assert!(document.starts_with("sound.error.gain = -"), "{document}");
    assert!(merge_pushed(&SoundSettings::default(), &document).is_ok());
    assert_eq!(
        effects_ask(
            EffectsRow::Choose(SoundEvent::Error),
            &FieldAction::Activated,
            &reading
        ),
        Some(EffectsAsk::Pick(SoundEvent::Error))
    );
    assert_eq!(
        effects_ask(
            EffectsRow::UseTheme(SoundEvent::Error),
            &FieldAction::Activated,
            &reading
        ),
        Some(EffectsAsk::UseTheme(SoundEvent::Error))
    );
}

/// A theme the settings name but no store holds is still the one shown.
#[test]
fn the_theme_in_effect_is_offered_though_none_is_installed_under_it() {
    let reading = reading("sound.theme = Gone\n");
    let (groups, _) = groups(Some(&reading));
    let FieldControl::Combo(combo) = groups[0].rows()[1].control() else {
        panic!("the theme row is a choice list");
    };
    assert_eq!(combo.choices().last().map(String::as_str), Some("Gone"));
    assert_eq!(combo.selected(), Some(combo.choices().len() - 1));
}

/// Levels stepped faster than the session answers cost one round trip each
/// time it does, carrying every setting's newest value: the last asked is the
/// one that lands, and none is dropped for want of room.
#[test]
fn levels_stepped_faster_than_the_session_answers_are_carried_together() {
    let level = |event: SoundEvent, db: i32| EffectsAsk::Apply {
        key: SoundKey::EventGain(event),
        line: alloc::format!("sound.{}.gain = {db}dB\n", event.name()),
    };
    let mut asks = EffectsAsks::new();
    asks.ask(level(SoundEvent::Error, -1));
    assert_eq!(
        asks.next_trip(),
        Some(EffectsTrip::Apply(String::from(
            "sound.error.gain = -1dB\n"
        )))
    );
    for db in -40..-2 {
        asks.ask(level(SoundEvent::Error, db));
    }
    asks.ask(level(SoundEvent::Bell, -6));
    asks.ask(level(SoundEvent::Error, -2));
    assert_eq!(asks.next_trip(), None, "one round trip at a time");
    asks.landed();
    assert_eq!(
        asks.next_trip(),
        Some(EffectsTrip::Apply(String::from(
            "sound.error.gain = -2dB\nsound.bell.gain = -6dB\n"
        )))
    );
    asks.landed();
    assert_eq!(asks.next_trip(), None, "nothing left");
}

/// Picks and returns to a theme's sound keep their order around the settings,
/// and one already waiting is not asked twice.
#[test]
fn actions_keep_their_order_and_are_not_asked_twice() {
    let mut asks = EffectsAsks::new();
    asks.ask(EffectsAsk::Pick(SoundEvent::Error));
    asks.ask(EffectsAsk::Apply {
        key: SoundKey::Enabled,
        line: String::from("sound.enabled = false\n"),
    });
    asks.ask(EffectsAsk::UseTheme(SoundEvent::Bell));
    asks.ask(EffectsAsk::UseTheme(SoundEvent::Bell));
    let mut trips = Vec::new();
    while let Some(trip) = asks.next_trip() {
        trips.push(trip);
        asks.landed();
    }
    assert_eq!(
        trips,
        [
            EffectsTrip::Pick(SoundEvent::Error),
            EffectsTrip::Apply(String::from("sound.enabled = false\n")),
            EffectsTrip::UseTheme(SoundEvent::Bell),
        ]
    );
}
