use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::audio::AudioGain;
use tairix_abi::pinboard_ipc::{SoundFile, SoundReading};
use tairix_abi::sound_cue::SoundEvent;
use tairix_abi::Errno;
use tairix_audio::volume::level_at_permille;
use tairix_controls::testkit::covers;
use tairix_controls::{FieldAction, FieldControl, FieldGroup};
use tairix_geometry::Scale;
use tairix_soundtheme::{layered, merge_pushed, SoundKey, SoundSettings};

use super::{
    apply, effects_ask, groups, show_level, touches, EffectFile, EffectsAsk, EffectsAsks,
    EffectsReading, EffectsRow, EffectsTrip,
};
use crate::form::{FormOutcome, FormPlace};
use crate::test_support::{showing, theme, WIDE};

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

/// Where the row for `event` is on the events plate.
fn event_row(event: SoundEvent) -> usize {
    SoundEvent::ALL
        .into_iter()
        .filter(|held| layered(*held))
        .position(|held| held == event)
        .expect("a sound the user decides")
}

/// What `event`'s chooser offers, and which it rests on.
fn chooser(groups: &[FieldGroup], event: SoundEvent) -> (Vec<String>, Option<usize>) {
    let FieldControl::Combo(combo) = groups[1].rows()[event_row(event)].control() else {
        panic!("an event's sound is a choice list");
    };
    (combo.choices().to_vec(), combo.selected())
}

#[test]
fn a_reading_is_the_sessions_document_and_files_by_name() {
    let reading = reading("sound.enabled = false\nsound.gain = -6dB\n");
    assert!(!reading.settings.enabled());
    assert_eq!(
        reading.settings.gain(),
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

/// One plate holds what applies to every sound and one row each event a
/// user's choices reach; the sounds they do not reach are stated, not
/// offered.
#[test]
fn the_pane_is_the_effects_plate_and_one_row_per_event_a_user_decides() {
    let (plates, owners) = groups(Some(&reading("")));
    let captions: Vec<&str> = plates.iter().map(FieldGroup::caption).collect();
    assert_eq!(captions, ["SOUND EFFECTS", "EVENT SOUNDS"]);
    assert_eq!(
        owners[0],
        [EffectsRow::Enabled, EffectsRow::Theme, EffectsRow::Gain]
    );
    assert!(plates[0]
        .footnote()
        .is_some_and(|note| note.contains("administrator")));
    let events: Vec<SoundEvent> = owners[1]
        .iter()
        .map(|row| match row {
            EffectsRow::Event(event) => *event,
            other => panic!("{other:?} on the events plate"),
        })
        .collect();
    let decided: Vec<SoundEvent> = SoundEvent::ALL
        .into_iter()
        .filter(|event| layered(*event))
        .collect();
    assert_eq!(events, decided);
    assert_eq!(plates[1].rows().len(), events.len(), "one row an event");
    let (unread, unowned) = groups(None);
    assert_eq!(
        (unread.len(), unowned.iter().map(Vec::len).sum::<usize>()),
        (1, 0)
    );
}

/// An event's one chooser names what it plays: the theme's sound, the file
/// chosen for it while there is one, nothing, or a file to choose.
#[test]
fn an_events_chooser_names_what_it_plays() {
    let groups = groups(Some(&reading("sound.warning.enabled = false\n"))).0;
    let words = |choices: &[&str]| choices.iter().map(|word| String::from(*word)).collect();
    assert_eq!(
        chooser(&groups, SoundEvent::Bell),
        (words(&["Theme sound", "None", "Choose File…"]), Some(0))
    );
    assert_eq!(
        chooser(&groups, SoundEvent::Error),
        (
            words(&["Theme sound", "klaxon.wav", "None", "Choose File…"]),
            Some(1)
        )
    );
    assert_eq!(
        chooser(&groups, SoundEvent::Warning),
        (words(&["Theme sound", "None", "Choose File…"]), Some(1)),
        "off is None"
    );
    let info = &groups[1].rows()[event_row(SoundEvent::Info)];
    assert_eq!(chooser(&groups, SoundEvent::Info).1, Some(1));
    assert!(info
        .description()
        .is_some_and(|note| note.starts_with("gone.flac cannot be read")));
}

/// While the desktop makes no sound none of its other settings applies.
#[test]
fn every_row_but_the_switch_waits_on_it() {
    let (off, _) = groups(Some(&reading("sound.enabled = false\n")));
    let rows: Vec<_> = off.iter().flat_map(FieldGroup::rows).collect();
    assert!(rows[0].state().enabled, "the switch itself");
    assert!(rows[1..].iter().all(|row| !row.state().enabled));
    let (on, _) = groups(Some(&reading("")));
    assert!(on
        .iter()
        .flat_map(FieldGroup::rows)
        .all(|row| row.state().enabled));
}

/// Each control asks for exactly the setting it shows, rendered by the one
/// registry; the volume asks once it settles, never while dragged.
#[test]
fn each_control_asks_for_its_own_setting_and_the_volume_only_where_it_settles() {
    let reading = reading("");
    let apply = |key: SoundKey, line: &str| {
        Some(EffectsAsk::Apply {
            key,
            line: String::from(line),
        })
    };
    assert_eq!(
        effects_ask(
            EffectsRow::Enabled,
            &FieldAction::Set { on: false },
            &reading
        ),
        apply(SoundKey::Enabled, "sound.enabled = false\n")
    );
    assert_eq!(
        effects_ask(
            EffectsRow::Theme,
            &FieldAction::Selected { index: 0 },
            &reading
        ),
        apply(SoundKey::Theme, "sound.theme = Glass\n")
    );
    assert_eq!(
        effects_ask(
            EffectsRow::Gain,
            &FieldAction::SetValue { permille: 500 },
            &reading
        ),
        None,
        "a drag on its way asks nothing"
    );
    let Some(EffectsAsk::Apply {
        key: SoundKey::Gain,
        line: document,
    }) = effects_ask(
        EffectsRow::Gain,
        &FieldAction::Settled { permille: 500 },
        &reading,
    )
    else {
        panic!("a settled volume is a change");
    };
    assert!(document.starts_with("sound.gain = -"), "{document}");
    assert!(merge_pushed(&SoundSettings::default(), &document).is_ok());
    let chose = |event: SoundEvent, index: usize| {
        effects_ask(
            EffectsRow::Event(event),
            &FieldAction::Selected { index },
            &reading,
        )
    };
    assert_eq!(
        chose(SoundEvent::Error, 0),
        Some(EffectsAsk::UseTheme(SoundEvent::Error))
    );
    assert_eq!(
        chose(SoundEvent::Error, 1),
        apply(
            SoundKey::EventEnabled(SoundEvent::Error),
            "sound.error.enabled = true\n"
        )
    );
    assert_eq!(
        chose(SoundEvent::Error, 2),
        apply(
            SoundKey::EventEnabled(SoundEvent::Error),
            "sound.error.enabled = false\n"
        )
    );
    assert_eq!(
        chose(SoundEvent::Error, 3),
        Some(EffectsAsk::Pick(SoundEvent::Error))
    );
    assert_eq!(
        chose(SoundEvent::Bell, 2),
        Some(EffectsAsk::Pick(SoundEvent::Bell)),
        "an event with no file of its own offers none"
    );
    assert_eq!(chose(SoundEvent::Bell, 3), None, "past the offers");
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

/// What the pane shows before the session answers is what the session then
/// holds; a pick shows nothing until the session has the file.
#[test]
fn a_change_is_shown_as_the_session_will_hold_it() {
    let mut reading = reading("sound.error.enabled = false\n");
    let ask = effects_ask(
        EffectsRow::Enabled,
        &FieldAction::Set { on: false },
        &reading,
    )
    .expect("a change");
    assert!(apply(&mut reading, &ask));
    assert!(!reading.settings.enabled());
    assert!(apply(
        &mut reading,
        &EffectsAsk::UseTheme(SoundEvent::Error)
    ));
    assert_eq!(reading.files[SoundEvent::Error.index()], EffectFile::Theme);
    assert!(
        reading.settings.event(SoundEvent::Error).enabled,
        "the theme's sound is asked to be heard"
    );
    assert!(!apply(&mut reading, &EffectsAsk::Pick(SoundEvent::Bell)));
    assert!(show_level(&mut reading, 500));
    assert_eq!(reading.settings.gain(), level_at_permille(500));
    assert!(!show_level(&mut reading, 500), "already there");
}

#[test]
fn the_switch_touches_every_row_and_any_other_only_itself() {
    let bell = EffectsRow::Event(SoundEvent::Bell);
    assert!(touches(EffectsRow::Enabled, bell));
    assert!(touches(bell, bell));
    assert!(!touches(EffectsRow::Gain, bell));
    assert!(!touches(bell, EffectsRow::Event(SoundEvent::Error)));
}

/// A chooser asked for a pick goes back, at once and in place, to what its
/// event plays: the pick is a command, and the session has no file yet.
#[test]
fn a_pick_puts_its_chooser_back_in_place() {
    let theme = theme();
    let mut shell = showing("sound-effects");
    shell.adopt_effects(Some(reading("")));
    let form = shell.form_mut_for_test().expect("a composed form");
    form.take_reshaped();
    let place = FormPlace {
        bounds: WIDE,
        viewport: WIDE,
        scale: Scale::ONE,
        theme: &theme,
    };
    let at = (1, event_row(SoundEvent::Error));
    let rect = form.row_rect_for_test(at.0, at.1, place).expect("the row");
    if let FieldControl::Combo(combo) =
        form.groups_mut_for_test()[at.0].rows_mut()[at.1].control_mut()
    {
        combo.set_selected(3);
    }
    let (outcome, damage) =
        form.act_in_place_for_test(at, FieldAction::Selected { index: 3 }, place);
    assert_eq!(
        outcome,
        FormOutcome::Effects {
            ask: Some(EffectsAsk::Pick(SoundEvent::Error)),
            settled: true,
        }
    );
    assert!(!form.take_reshaped(), "restated, not built again");
    assert_eq!(chooser(form.groups(), SoundEvent::Error).1, Some(1));
    assert!(covers(&damage, rect));
}

/// Throwing the switch off shows every other row disabled at once, in place.
#[test]
fn the_switch_restates_every_row_in_place() {
    let theme = theme();
    let mut shell = showing("sound-effects");
    shell.adopt_effects(Some(reading("")));
    let form = shell.form_mut_for_test().expect("a composed form");
    form.take_reshaped();
    let place = FormPlace {
        bounds: WIDE,
        viewport: WIDE,
        scale: Scale::ONE,
        theme: &theme,
    };
    let (outcome, _) = form.act_in_place_for_test((0, 0), FieldAction::Set { on: false }, place);
    assert!(matches!(outcome, FormOutcome::Effects { ask: Some(_), .. }));
    assert!(!form.take_reshaped());
    let rows: Vec<_> = form.groups().iter().flat_map(FieldGroup::rows).collect();
    assert!(matches!(rows[0].control(), FieldControl::Toggle(toggle) if !toggle.is_on()));
    assert!(rows[1..].iter().all(|row| !row.state().enabled));
}

/// The volume's figure follows a drag as it goes, before anything is asked.
#[test]
fn the_volume_figure_follows_a_drag() {
    let mut shell = showing("sound-effects");
    shell.adopt_effects(Some(reading("")));
    let form = shell.form_mut_for_test().expect("a composed form");
    let outcome = form.act_for_test(0, 2, FieldAction::SetValue { permille: 500 });
    assert_eq!(
        outcome,
        FormOutcome::Effects {
            ask: None,
            settled: false,
        }
    );
    let level = alloc::format!("{}", level_at_permille(500));
    assert_eq!(
        form.groups()[0].rows()[2].description(),
        Some(level.as_str())
    );
}

/// Settings stepped faster than the session answers cost one round trip each
/// time it does, carrying every setting's newest value: the last asked is the
/// one that lands, and none is dropped for want of room.
#[test]
fn settings_stepped_faster_than_the_session_answers_are_carried_together() {
    let level = |db: i32| EffectsAsk::Apply {
        key: SoundKey::Gain,
        line: alloc::format!("sound.gain = {db}dB\n"),
    };
    let mut asks = EffectsAsks::new();
    asks.ask(level(-1));
    assert_eq!(
        asks.next_trip(),
        Some(EffectsTrip::Apply(String::from("sound.gain = -1dB\n")))
    );
    for db in -40..-2 {
        asks.ask(level(db));
    }
    asks.ask(EffectsAsk::Apply {
        key: SoundKey::EventEnabled(SoundEvent::Bell),
        line: String::from("sound.bell.enabled = false\n"),
    });
    asks.ask(level(-2));
    assert_eq!(asks.next_trip(), None, "one round trip at a time");
    asks.landed();
    assert_eq!(
        asks.next_trip(),
        Some(EffectsTrip::Apply(String::from(
            "sound.gain = -2dB\nsound.bell.enabled = false\n"
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
