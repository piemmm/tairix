use tairix_abi::origin::ProcId;
use tairix_abi::pinboard_ipc::SoundFile;
use tairix_abi::sound_cue::SoundEvent;
use tairix_abi::Errno;
use tairix_appconf::{Document, SettingPath};
use tairix_appdata::fake::FakeService;
use tairix_soundtheme::{merge_pushed, SoundSettings, UserChoices};

use super::{
    load_choices, store_choices, Effects, EffectsWork, ReadAsk, Shown, RETRY_MAX_NS, RETRY_NS,
};

fn service() -> FakeService {
    FakeService::for_word("desktop")
}

fn choices(text: &str) -> UserChoices {
    UserChoices::load(&Document::parse(text).expect("parses")).0
}

fn path(text: &str) -> SettingPath {
    SettingPath::new(text).expect("a setting path")
}

const AUTHORITY: ProcId = ProcId::from_raw([0x5d; 16]);
const RESTARTED: ProcId = ProcId::from_raw([0x6e; 16]);

#[test]
fn a_fresh_account_holds_no_choices_and_warns_of_nothing() {
    let loaded = load_choices(&mut service());
    assert_eq!(loaded.choices, Some(UserChoices::default()));
    assert!(loaded.warnings.is_empty());
}

#[test]
fn stored_choices_read_back_and_a_setting_not_kept_is_said() {
    let mut host =
        service().with_store("sound.bell.enabled = false\nsound.login.enabled = false\n");
    let loaded = load_choices(&mut host);
    assert_eq!(
        loaded.choices,
        Some(choices("sound.bell.enabled = false\n"))
    );
    assert_eq!(loaded.warnings.len(), 1);
    assert!(loaded.warnings[0].contains("sound.login.enabled"));
}

#[test]
fn an_unreadable_store_says_so_and_yields_no_choices_to_store_over() {
    let mut host = service();
    host.refusal().set(Some(Errno::DeviceOffline));
    let loaded = load_choices(&mut host);
    assert_eq!(loaded.choices, None);
    assert!(loaded.warnings[0].contains("DeviceOffline"));
}

/// The private scope holds the choices, files and all, and nothing the store
/// held of anything else is touched.
#[test]
fn storing_replaces_the_sound_settings_and_nothing_else() {
    let mut host = service().with_store("sound.info.gain = -3dB\nother.key = kept\n");
    let mut wanted = choices("sound.bell.enabled = false\n");
    assert!(wanted.choose(SoundEvent::Error, &path("/Users/ada/klaxon.wav")));
    assert_eq!(store_choices(&mut host, &wanted), Ok(wanted.clone()));
    let held = host.committed();
    assert_eq!(held.get("sound.bell.enabled"), Some("false"));
    assert_eq!(held.get("sound.error.file"), Some("/Users/ada/klaxon.wav"));
    assert_eq!(
        held.get("sound.info.gain"),
        None,
        "a choice no longer made is gone"
    );
    assert_eq!(held.get("other.key"), Some("kept"));
    assert_eq!(host.published().settings().count(), 0, "never published");
}

#[test]
fn a_refused_store_stores_nothing() {
    let mut host = service();
    host.refusal().set(Some(Errno::NoSpace));
    assert!(store_choices(&mut host, &choices("sound.enabled = false\n")).is_err());
    assert_eq!(host.committed().settings().count(), 0);
}

fn holding(stored: &str) -> Effects {
    let mut effects = Effects::new();
    assert!(effects.next_load(0));
    effects.loaded(0, Some(choices(stored)));
    effects.authority_is(Some(AUTHORITY));
    effects.seat(true);
    effects
}

fn pushed_choices(work: Option<EffectsWork>) -> UserChoices {
    match work {
        Some(EffectsWork::Push { authority, choices }) => {
            assert_eq!(authority, AUTHORITY);
            choices
        }
        other => panic!("no push: {other:?}"),
    }
}

/// The choices are handed over only while the session holds the seat, the
/// authority is known and the store has been read, once each.
#[test]
fn a_push_is_owed_at_the_seat_once_per_change() {
    let mut effects = Effects::new();
    assert_eq!(effects.next_push(0), None, "nothing read");
    assert!(effects.next_load(0));
    effects.loaded(0, Some(choices("sound.bell.enabled = false\n")));
    assert_eq!(effects.next_push(0), None, "no authority known");
    effects.authority_is(Some(AUTHORITY));
    assert_eq!(effects.next_push(0), None, "not at the seat");
    effects.seat(true);
    pushed_choices(effects.next_push(0));
    assert_eq!(effects.next_push(0), None, "one at a time");
    assert_eq!(effects.pushed(0, Ok(())), None);
    assert_eq!(effects.next_push(0), None, "already in force");

    // A restarted authority holds nothing of this session's.
    effects.authority_is(Some(RESTARTED));
    let Some(EffectsWork::Push { authority, .. }) = effects.next_push(0) else {
        panic!("owed to the new authority");
    };
    assert_eq!(authority, RESTARTED);
    assert_eq!(effects.pushed(0, Ok(())), None);
    // Leaving the seat and taking it again owes them anew.
    effects.seat(false);
    assert_eq!(effects.next_push(0), None);
    effects.seat(true);
    assert!(effects.next_push(0).is_some());
}

/// A push refused for back-pressure is tried again on a growing deadline; any
/// other refusal waits for a change rather than repeating at once.
#[test]
fn a_refused_push_is_retried_on_a_deadline_or_at_the_next_change() {
    let mut effects = holding("sound.enabled = false\n");
    pushed_choices(effects.next_push(0));
    assert_eq!(
        effects.pushed(10, Err(Errno::WouldBlock)),
        Some(Errno::WouldBlock)
    );
    assert_eq!(effects.due_at(), Some(10 + RETRY_NS));
    assert_eq!(effects.next_push(10 + RETRY_NS - 1), None);
    pushed_choices(effects.next_push(10 + RETRY_NS));
    effects.pushed(20, Err(Errno::TimedOut));
    assert_eq!(effects.due_at(), Some(20 + 2 * RETRY_NS), "twice as long");
    for _ in 0..16 {
        let at = effects.due_at().expect("still owed");
        pushed_choices(effects.next_push(at));
        effects.pushed(at, Err(Errno::WouldBlock));
    }
    let at = effects.due_at().expect("still owed");
    pushed_choices(effects.next_push(at));
    effects.pushed(at, Err(Errno::WouldBlock));
    assert_eq!(effects.due_at(), Some(at + RETRY_MAX_NS), "never longer");

    let at = effects.due_at().expect("still owed");
    pushed_choices(effects.next_push(at));
    assert_eq!(
        effects.pushed(at, Err(Errno::SeatNotOwner)),
        Some(Errno::SeatNotOwner)
    );
    assert_eq!(effects.due_at(), None);
    assert_eq!(effects.next_push(u64::MAX), None, "not repeated unchanged");
    let Ok(EffectsWork::Store {
        number, choices, ..
    }) = effects.apply("sound.bell.enabled = false\n", 1)
    else {
        panic!("a change");
    };
    effects.stored(number, Ok(choices));
    assert!(effects.next_push(0).is_some(), "a change is owed");
}

/// A change is made over the newest asked for, so two changes on their way to
/// the store never undo each other; a refused store leaves what was held.
#[test]
fn a_change_is_made_over_the_newest_ask_until_it_lands() {
    let mut effects = holding("");
    let Ok(EffectsWork::Store {
        number: first,
        choices: off,
        ticket,
    }) = effects.apply("sound.bell.enabled = false\n", 7)
    else {
        panic!("a store");
    };
    assert_eq!(ticket, Some(7));
    let Ok(EffectsWork::Store {
        number: second,
        choices: both,
        ..
    }) = effects.apply("sound.info.gain = -6dB\n", 8)
    else {
        panic!("a store");
    };
    assert_eq!(
        both.pushed(),
        "sound.info.gain = -6dB\nsound.bell.enabled = false\n",
        "both, in the registry's order"
    );
    effects.stored(first, Ok(off));
    effects.stored(second, Err(Errno::NoSpace));
    let Ok(EffectsWork::Store { choices, .. }) = effects.apply("sound.enabled = true\n", 9) else {
        panic!("a store");
    };
    assert_eq!(
        choices.pushed(),
        "sound.enabled = true\nsound.bell.enabled = false\n",
        "over what the store held"
    );
}

#[test]
fn a_change_before_the_store_is_read_or_naming_a_file_is_refused() {
    let mut effects = Effects::new();
    assert_eq!(
        effects
            .apply("sound.enabled = false\n", 1)
            .err()
            .map(|(err, _)| err),
        Some(Errno::Busy)
    );
    assert!(effects.next_load(0));
    effects.loaded(0, Some(UserChoices::default()));
    let (err, refusal) = effects
        .apply("sound.bell.file = /Users/ada/bell.wav\n", 1)
        .expect_err("a file never arrives as a path");
    assert_eq!(err, Errno::OutOfRange);
    assert!(refusal.is_some());
    assert_eq!(
        effects.use_theme(SoundEvent::Login, 1),
        Err(Errno::OutOfRange)
    );
}

/// A pick records the chosen path, answering its call once stored; a pick of a
/// sound no user decides, or a second while one shows, is refused.
#[test]
fn a_pick_records_its_file_and_answers_its_call_once_stored() {
    let mut effects = holding("");
    assert_eq!(
        effects.begin_pick(SoundEvent::Logout, 3),
        Err(Errno::OutOfRange)
    );
    assert_eq!(effects.begin_pick(SoundEvent::Error, 3), Ok(()));
    assert!(effects.is_picking());
    assert_eq!(effects.begin_pick(SoundEvent::Bell, 4), Err(Errno::Busy));
    let Some(EffectsWork::Store {
        number,
        choices,
        ticket,
    }) = effects.chosen(&path("/Users/ada/UserFiles/Music/klaxon.wav"))
    else {
        panic!("a store");
    };
    assert_eq!(ticket, Some(3));
    assert_eq!(
        choices.file(SoundEvent::Error),
        Some("/Users/ada/UserFiles/Music/klaxon.wav")
    );
    assert!(!effects.is_picking());
    effects.stored(number, Ok(choices));
    assert_eq!(effects.begin_pick(SoundEvent::Bell, 5), Ok(()));
    assert_eq!(effects.cancelled(), Some(5));
    assert_eq!(effects.chosen(&path("/a")), None, "nothing picking");
}

/// The pane is told the settings in effect over the machine's, and each file
/// by its name alone.
#[test]
fn a_reading_shows_what_is_in_effect_and_names_files_by_their_names() {
    let machine =
        merge_pushed(&SoundSettings::default(), "sound.theme = Glass\n").expect("the machine's");
    let mut stored = choices("sound.bell.enabled = false\n");
    assert!(stored.choose(SoundEvent::Error, &path("/Users/ada/klaxon.wav")));
    assert!(stored.choose(SoundEvent::Info, &path("/Users/ada/gone.flac")));
    let mut unavailable = [false; 13];
    unavailable[SoundEvent::Info.index()] = true;
    let reading = ReadAsk {
        choices: stored,
        unavailable,
    }
    .reading(&machine);
    assert!(reading.document.contains("sound.theme = Glass\n"));
    assert!(reading.document.contains("sound.bell.enabled = false\n"));
    assert!(
        !reading.document.contains("file"),
        "no path reaches the pane"
    );
    assert_eq!(
        reading.files[SoundEvent::Error.index()],
        Shown::Chosen("klaxon.wav".into())
    );
    assert_eq!(
        reading.files[SoundEvent::Info.index()],
        Shown::Unavailable("gone.flac".into())
    );
    let wire = reading.as_wire();
    assert_eq!(wire.files[SoundEvent::Bell.index()], SoundFile::Theme);
    assert_eq!(Effects::new().read_ask().err(), Some(Errno::Busy));
}

#[test]
fn a_file_is_named_by_its_last_named_component() {
    assert_eq!(super::leaf_of("/Users/ada/klaxon.wav"), "klaxon.wav");
    assert_eq!(super::leaf_of("/Users/ada/Sounds/"), "Sounds");
    assert_eq!(super::leaf_of("/"), "/");
}

/// A retry stands only while a push is owed and none is on its way, so a
/// deadline never outlives its work and the loop is never made to spin.
#[test]
fn a_retry_owed_nothing_sets_no_deadline() {
    let mut effects = holding("sound.enabled = false\n");
    pushed_choices(effects.next_push(0));
    assert_eq!(effects.pushed(0, Ok(())), None);
    let store = |effects: &mut Effects, text: &str| {
        let Ok(EffectsWork::Store {
            number, choices, ..
        }) = effects.apply(text, 1)
        else {
            panic!("a change");
        };
        effects.stored(number, Ok(choices));
    };
    store(&mut effects, "sound.enabled = true\n");
    pushed_choices(effects.next_push(1));
    assert_eq!(effects.due_at(), None, "none while a push is on its way");
    effects.pushed(1, Err(Errno::WouldBlock));
    assert_eq!(effects.due_at(), Some(1 + RETRY_NS));
    store(&mut effects, "sound.enabled = false\n");
    assert_eq!(effects.next_push(u64::MAX), None, "already handed over");
    assert_eq!(effects.due_at(), None, "a retry owed nothing");
    assert_eq!(effects.park_deadline_ns(u64::MAX, 7), 7);
}

/// An answer to a push made before the seat was lost and taken again says
/// nothing of what the authority holds now: it is not taken as a refusal of
/// the choices, which are owed again.
#[test]
fn an_answer_to_a_push_made_before_the_seat_moved_is_not_taken() {
    let mut effects = holding("sound.enabled = false\n");
    pushed_choices(effects.next_push(0));
    effects.seat(false);
    effects.seat(true);
    assert_eq!(effects.next_push(0), None, "one push on its way at a time");
    assert_eq!(effects.pushed(0, Err(Errno::SeatNotOwner)), None);
    assert!(effects.next_push(0).is_some(), "owed again, not refused");
}

/// A read of the store that came to nothing is tried again on a growing
/// deadline, and no change is made over choices the store was not read for.
#[test]
fn a_read_that_came_to_nothing_is_tried_again_and_nothing_is_stored_over_it() {
    let mut effects = Effects::new();
    assert!(effects.next_load(0));
    assert!(!effects.next_load(0), "one read at a time");
    effects.loaded(5, None);
    assert_eq!(effects.due_at(), Some(5 + RETRY_NS));
    assert_eq!(
        effects
            .apply("sound.enabled = false\n", 1)
            .err()
            .map(|(err, _)| err),
        Some(Errno::Busy),
        "nothing to make a change over"
    );
    assert!(!effects.next_load(5 + RETRY_NS - 1));
    assert!(effects.next_load(5 + RETRY_NS));
    assert_eq!(effects.due_at(), None, "a read on its way");
    effects.loaded(6 + RETRY_NS, None);
    assert_eq!(effects.due_at(), Some(6 + 3 * RETRY_NS), "twice as long");
    assert!(effects.next_load(6 + 3 * RETRY_NS));
    effects.loaded(
        7 + 3 * RETRY_NS,
        Some(choices("sound.bell.enabled = false\n")),
    );
    assert_eq!(effects.due_at(), None);
    assert!(!effects.next_load(u64::MAX), "read");
    assert!(effects.apply("sound.enabled = false\n", 2).is_ok());
}

/// A push's frame is posted once its files are delegated, once, and what it
/// came to is what the pane is then told.
#[test]
fn a_push_posts_its_frame_once_its_files_are_delegated() {
    let mut effects = holding("sound.enabled = false\n");
    pushed_choices(effects.next_push(0));
    assert_eq!(effects.take_post(), None, "its files are not yet delegated");
    let mut files = [0u64; 13];
    files[SoundEvent::Error.index()] = 4;
    let mut unavailable = [false; 13];
    unavailable[SoundEvent::Info.index()] = true;
    effects.handed_over("sound.enabled = false\n".into(), files, unavailable);
    assert_eq!(
        effects.take_post(),
        Some(("sound.enabled = false\n".into(), files))
    );
    assert_eq!(effects.take_post(), None, "posted once");
    assert_eq!(effects.pushed(0, Ok(())), None);
    assert_eq!(
        effects.read_ask().map(|ask| ask.unavailable),
        Ok(unavailable)
    );
}
