//! Host tests for the player's state: what each input asks of the playback
//! thread and the worker, and the damage every change owes — no more.

use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use tairix_abi::audio::{
    AudioDeviceDescriptor, AudioGain, AudioLocation, ControlAccess, DefaultChoice,
};
use tairix_abi::driver::audio::{
    AudioName, ChannelMap, JackState, Rate, RateSupport, SampleFormat, SampleFormats,
    StreamDirection,
};
use tairix_abi::time::Duration64;
use tairix_abi::window_ipc::PickPurpose;
use tairix_controls::damage;
use tairix_font::BitmapFont;
use tairix_geometry::{Point, Rect, Region, Scale};
use tairix_input::{InputEvent, Key, Modifiers, NamedKey, PointerButton};
use tairix_player::{Control, EntryId, Span, Status, Transport};
use tairix_sound::{CoverRange, Encoding, Metadata, SoundFormat, SoundInfo, Tag, TagKind};
use tairix_theme::{TextRole, ThemeRegistry};

use super::{row, Command, Device, Outcome, Player, Request, Saved};
use crate::layout::Layout;
use crate::playlist::{Edit, Repeat};

const DOUBLE_CLICK: Duration64 = Duration64::from_millis(500);

fn layout() -> (Layout, ThemeRegistry) {
    let registry = ThemeRegistry::with_builtins();
    let theme = registry.active();
    let font = BitmapFont::for_role(theme.fonts(), TextRole::Body, Scale::ONE);
    (
        Layout::for_window(720, 520, theme, Scale::ONE, font),
        registry,
    )
}

fn info(seconds: u64) -> SoundInfo {
    SoundInfo {
        format: SoundFormat::Flac,
        encoding: Encoding::Flac,
        rate: Rate::HZ_48000,
        channels: ChannelMap::STEREO,
        sample: SampleFormat::S16,
        frames: Some(48_000 * seconds),
        seekable: true,
        data_length: None,
    }
}

fn player_with(names: &[&str]) -> (Player, Vec<EntryId>, Layout, ThemeRegistry) {
    let (layout, registry) = layout();
    let mut player = Player::new(Saved::default(), 1, DOUBLE_CLICK);
    let (added, _) = player.add(
        names.iter().map(ToString::to_string).collect(),
        &layout,
        &mut damage::sink(),
    );
    (player, added, layout, registry)
}

fn playing(entry: EntryId, position: u64, peaks: u8) -> Status {
    let mut levels = [0u8; 8];
    levels[0] = peaks;
    Status {
        heard: Some((entry, info(200))),
        position,
        transport: Transport::Playing,
        peaks: (levels, 2),
        ..Status::new(AudioGain::UNITY)
    }
}

fn centre(rect: Rect) -> Point {
    Point::new(
        rect.left() + i32::try_from(rect.width / 2).expect("small"),
        rect.top() + i32::try_from(rect.height / 2).expect("small"),
    )
}

/// Drag from `from` to `to` and release, answering every outcome.
fn drag(player: &mut Player, layout: &Layout, from: Point, to: Point) -> Vec<Outcome> {
    let registry = ThemeRegistry::with_builtins();
    let theme = registry.active();
    let mut outcomes = Vec::new();
    for event in [
        InputEvent::PointerMoved { to: from },
        InputEvent::PointerPressed {
            button: PointerButton::Primary,
        },
        InputEvent::PointerMoved { to },
        InputEvent::PointerReleased {
            button: PointerButton::Primary,
        },
    ] {
        outcomes.push(player.input(&event, 0, layout, Scale::ONE, theme, &mut damage::sink()));
    }
    outcomes
}

fn press(
    player: &mut Player,
    layout: &Layout,
    key: Key,
    modifiers: Modifiers,
) -> (Outcome, Region) {
    let registry = ThemeRegistry::with_builtins();
    let mut region = damage::sink();
    let outcome = player.input(
        &InputEvent::KeyPressed { key, modifiers },
        0,
        layout,
        Scale::ONE,
        registry.active(),
        &mut region,
    );
    (outcome, region)
}

fn covers(region: &Region, rect: Rect) -> bool {
    region
        .rects()
        .iter()
        .any(|part| part.intersection(&rect) == rect)
}

#[test]
fn files_added_to_a_stopped_player_are_read_and_the_first_plays() {
    let (layout, _registry) = layout();
    let mut player = Player::new(Saved::default(), 1, DOUBLE_CLICK);
    player.adopt(
        Status {
            transport: Transport::Stopped,
            ..Status::new(AudioGain::UNITY)
        },
        &layout,
        &mut damage::sink(),
    );
    let mut region = damage::sink();
    let (added, outcome) = player.add(
        vec![String::from("a.flac"), String::from("b.flac")],
        &layout,
        &mut region,
    );
    assert_eq!(added.len(), 2);
    assert_eq!(player.playlist().arranged(), added);
    assert_eq!(
        outcome.requests,
        vec![Request::Probe(added[0]), Request::Probe(added[1])]
    );
    assert_eq!(
        outcome.commands,
        vec![Command::Control(Control::Jump(added[0]))]
    );
    assert_eq!(player.selected(), Some(added[0]));
    assert!(covers(&region, layout.rows()));

    player.adopt(playing(added[0], 0, 0), &layout, &mut damage::sink());
    let (_, outcome) = player.add(vec![String::from("c.flac")], &layout, &mut damage::sink());
    assert!(outcome.commands.is_empty(), "a playing player plays on");
}

/// The meter moves on every period; a repaint of anything else for it would
/// cost the window on the machine that can least afford it.
#[test]
fn a_change_of_level_alone_repaints_the_meters_alone() {
    let (mut player, added, layout, _registry) = player_with(&["a.flac"]);
    player.adopt(playing(added[0], 0, 10), &layout, &mut damage::sink());
    let mut region = damage::sink();
    player.adopt(playing(added[0], 0, 200), &layout, &mut region);
    assert!(covers(&region, layout.meters()));
    for part in [layout.rows(), layout.title(), layout.seek(), layout.art()] {
        assert!(!region.intersects(part), "{part:?} is untouched");
    }
    let mut region = damage::sink();
    player.adopt(playing(added[0], 0, 200), &layout, &mut region);
    assert!(region.is_empty(), "nothing changed, so nothing is owed");
}

#[test]
fn a_new_track_repaints_what_names_it_and_asks_for_its_cover() {
    let (mut player, added, layout, _registry) = player_with(&["a.flac", "b.flac"]);
    let mut metadata = Metadata::default();
    metadata.tags.push(Tag {
        kind: TagKind::Title,
        value: String::from("Overture"),
    });
    metadata.cover = Some(CoverRange {
        offset: 100,
        len: 50,
    });
    player.probed(
        added[1],
        Some((info(200), &metadata)),
        &layout,
        &mut damage::sink(),
    );
    assert_eq!(
        player.playlist().get(added[1]).map(super::Row::title),
        Some("Overture")
    );
    player.adopt(playing(added[0], 0, 0), &layout, &mut damage::sink());
    let mut region = damage::sink();
    let outcome = player.adopt(playing(added[1], 0, 0), &layout, &mut region);
    for part in [
        layout.art(),
        layout.title(),
        layout.subtitle(),
        layout.format(),
    ] {
        assert!(covers(&region, part), "{part:?}");
    }
    assert_eq!(
        outcome.requests,
        vec![Request::Art {
            entry: added[1],
            cover: CoverRange {
                offset: 100,
                len: 50
            },
            side: layout.art().width,
        }]
    );
}

#[test]
fn the_lists_length_follows_each_reading_rereading_and_removal() {
    let (mut player, added, layout, _registry) = player_with(&["a.flac", "b.flac", "c.flac"]);
    let untagged = Metadata::default();
    let seconds = |count: u64| Span::from_nanos(count * 1_000_000_000);
    for (entry, read) in added.iter().zip([Some(90), Some(150), None]) {
        player.probed(
            *entry,
            read.map(|length| (info(length), &untagged)),
            &layout,
            &mut damage::sink(),
        );
    }
    assert_eq!(
        player.listed_length(),
        seconds(240),
        "an unreadable file adds nothing"
    );
    player.probed(
        added[1],
        Some((info(30), &untagged)),
        &layout,
        &mut damage::sink(),
    );
    assert_eq!(
        player.listed_length(),
        seconds(120),
        "a reread replaces its length"
    );
    let plain = Modifiers::default();
    press(&mut player, &layout, Key::Named(NamedKey::Delete), plain);
    assert_eq!(player.playlist().len(), 2);
    assert_eq!(
        player.listed_length(),
        seconds(30),
        "a removal takes back its length"
    );
    player.choose(row::CLEAR, &layout, &mut damage::sink());
    assert_eq!(player.listed_length(), Span::ZERO);
}

#[test]
fn a_row_read_out_of_sight_repaints_no_part_of_the_list() {
    let (layout, _registry) = layout();
    let mut player = Player::new(Saved::default(), 1, DOUBLE_CLICK);
    let names = (0..500).map(|n| alloc::format!("{n}.flac")).collect();
    let (added, _) = player.add(names, &layout, &mut damage::sink());
    let untagged = Metadata::default();
    let mut region = damage::sink();
    player.probed(added[1], Some((info(60), &untagged)), &layout, &mut region);
    assert!(
        covers(&region, layout.row(1, 0)),
        "a row in sight is repainted"
    );
    let mut region = damage::sink();
    player.probed(
        added[499],
        Some((info(60), &untagged)),
        &layout,
        &mut region,
    );
    assert!(
        region
            .rects()
            .iter()
            .all(|part| part.intersection(&layout.rows()).is_empty()),
        "{region:?}"
    );
    assert!(covers(&region, layout.status()), "the total it changed is");
}

/// One drag is one seek.
#[test]
fn the_seek_slider_seeks_once_where_it_settles() {
    let (mut player, added, layout, _registry) = player_with(&["a.flac"]);
    player.adopt(playing(added[0], 0, 0), &layout, &mut damage::sink());
    let seek = layout.seek();
    let start = Point::new(seek.left() + 2, centre(seek).y);
    let outcomes = drag(&mut player, &layout, start, centre(seek));
    let seeks: Vec<&Command> = outcomes
        .iter()
        .flat_map(|outcome| outcome.commands.iter())
        .collect();
    assert_eq!(seeks.len(), 1, "{seeks:?}");
    let Command::Control(Control::SeekTo(at)) = seeks[0] else {
        panic!("a seek: {seeks:?}");
    };
    let middle = Span::from_nanos(100_000_000_000);
    let off = at.nanos().abs_diff(middle.nanos());
    assert!(off < 10_000_000_000, "near the middle: {at:?}");
    assert!(
        outcomes[..3]
            .iter()
            .all(|outcome| outcome.commands.is_empty()),
        "the drag itself seeks nothing"
    );
}

/// The level follows the drag; it is saved once.
#[test]
fn the_volume_moves_live_and_is_saved_once_it_settles() {
    let (mut player, _, layout, _registry) = player_with(&[]);
    let volume = layout.volume();
    let right = Point::new(volume.right() - 2, centre(volume).y);
    let outcomes = drag(&mut player, &layout, right, centre(volume));
    let saves = outcomes
        .iter()
        .flat_map(|outcome| outcome.requests.iter())
        .filter(|request| matches!(request, Request::Save(_)))
        .count();
    assert_eq!(saves, 1);
    assert!(outcomes
        .iter()
        .flat_map(|outcome| outcome.commands.iter())
        .any(|command| matches!(command, Command::Control(Control::SetGain(_)))));
    assert!(player.saved().gain < AudioGain::UNITY);
}

#[test]
fn keys_drive_the_transport_the_selection_and_the_picker() {
    let (mut player, added, layout, _registry) = player_with(&["a.flac", "b.flac", "c.flac"]);
    let plain = Modifiers::default();
    let ctrl = Modifiers {
        ctrl: true,
        ..Modifiers::default()
    };
    player.adopt(playing(added[0], 0, 0), &layout, &mut damage::sink());
    assert_eq!(
        press(&mut player, &layout, Key::Char(' '), plain)
            .0
            .commands,
        vec![Command::Control(Control::TogglePause)]
    );
    assert_eq!(
        press(&mut player, &layout, Key::Named(NamedKey::Right), ctrl)
            .0
            .commands,
        vec![Command::Control(Control::Next)]
    );
    assert_eq!(
        press(&mut player, &layout, Key::Named(NamedKey::Left), plain)
            .0
            .commands,
        vec![Command::Control(Control::Back)]
    );
    press(&mut player, &layout, Key::Named(NamedKey::Down), plain);
    assert_eq!(player.selected(), Some(added[1]));
    let alt = Modifiers {
        alt: true,
        ..Modifiers::default()
    };
    assert_eq!(
        press(&mut player, &layout, Key::Named(NamedKey::Down), alt)
            .0
            .commands,
        vec![Command::Edit(Edit::Move {
            entry: added[1],
            to: 2
        })]
    );
    assert_eq!(player.playlist().arranged(), [added[0], added[2], added[1]]);
    let (outcome, _) = press(&mut player, &layout, Key::Named(NamedKey::Delete), plain);
    assert_eq!(
        outcome.commands,
        vec![Command::Edit(Edit::Remove(vec![added[1]]))]
    );
    assert_eq!(
        player.selected(),
        Some(added[2]),
        "the selection stays near"
    );
    assert_eq!(
        press(&mut player, &layout, Key::Char('o'), ctrl).0.pick,
        Some(PickPurpose::Open)
    );
    let both = Modifiers {
        ctrl: true,
        shift: true,
        ..Modifiers::default()
    };
    assert_eq!(
        press(&mut player, &layout, Key::Char('O'), both).0.pick,
        Some(PickPurpose::Folder)
    );
}

#[test]
fn a_level_step_past_unity_changes_nothing_and_owes_nothing() {
    let (mut player, _, layout, _registry) = player_with(&[]);
    let (outcome, region) = press(&mut player, &layout, Key::Char('+'), Modifiers::default());
    assert_eq!(outcome, Outcome::default());
    assert!(region.is_empty());
    let (outcome, region) = press(&mut player, &layout, Key::Char('-'), Modifiers::default());
    assert_eq!(
        outcome.commands,
        vec![Command::Control(Control::SetGain(
            AudioGain::new(-300).expect("attenuation")
        ))]
    );
    assert!(covers(&region, layout.volume()));
}

#[test]
fn the_menus_carry_the_listeners_settings_and_their_choices_act() {
    let (mut player, added, layout, _registry) = player_with(&["a.flac", "b.flac"]);
    player.devices_listed(vec![Device {
        id: 3,
        target: String::from("audio:hda0"),
        name: String::from("Speakers"),
    }]);
    let mut region = damage::sink();
    let outcome = player.choose(row::REPEAT_ALL, &layout, &mut region);
    assert_eq!(player.saved().repeat, Repeat::All);
    assert_eq!(
        outcome.commands,
        vec![Command::Edit(Edit::Repeat(Repeat::All))]
    );
    assert!(matches!(outcome.requests[..], [Request::Save(_)]));
    let outcome = player.choose(row::FIRST_DEVICE, &layout, &mut region);
    assert_eq!(
        outcome.commands,
        vec![Command::Control(Control::SetDevice(3))]
    );
    assert_eq!(player.saved().device.as_deref(), Some("audio:hda0"));
    let outcome = player.choose(row::DEFAULT_DEVICE, &layout, &mut region);
    assert_eq!(
        outcome.commands,
        vec![Command::Control(Control::SetDevice(0))]
    );
    assert_eq!(player.saved().device, None);
    assert_eq!(
        player.choose(row::FIRST_DEVICE + 1, &layout, &mut region),
        Outcome::default(),
        "a device past the list chooses nothing"
    );
    assert_eq!(
        player.choose(row::OPEN_FOLDER, &layout, &mut region).pick,
        Some(PickPurpose::Folder)
    );
    let rows = player.bar_rows();
    assert!(rows.len() >= 6, "{rows:?}");
    let menu = player.context_menu();
    assert!(menu.len() > 6);
    assert_eq!(
        player.choose(row::REMOVE, &layout, &mut region),
        Outcome::default(),
        "a row command with no row the menu was opened on does nothing"
    );
    let outcome = player.choose(row::CLEAR, &layout, &mut region);
    assert_eq!(outcome.commands, vec![Command::Edit(Edit::Clear)]);
    assert!(player.playlist().is_empty());
    assert_eq!(player.selected(), None);
    let _ = added;
}

fn speakers(device_id: u32, place: u64) -> AudioDeviceDescriptor {
    AudioDeviceDescriptor {
        device_id,
        direction: StreamDirection::Playback,
        jack: JackState::Present,
        default: DefaultChoice::No,
        formats: SampleFormats::EMPTY,
        channel_map: ChannelMap::STEREO,
        rates: RateSupport::Continuous {
            min: Rate::HZ_48000,
            max: Rate::HZ_48000,
        },
        gain: None,
        name: AudioName::new("Speakers").expect("a short name"),
        location: AudioLocation::new(place, 0).expect("a place"),
        level: AudioGain::UNITY,
        muted: false,
        own_level: false,
        access: ControlAccess::Shared,
        clock_millihertz: 0,
        lost_frames: 0,
    }
}

/// A device's id is one boot's, so the output the listener chose is
/// remembered by where it is: the same speakers under another id are still
/// the choice, and another device given the old id is not.
#[test]
fn a_chosen_output_is_remembered_by_where_it_is() {
    let chosen = Device::of(&speakers(3, 0x51));
    let next_boot = Device::of(&speakers(7, 0x51));
    let usurper = Device::of(&speakers(3, 0x52));
    assert_eq!(chosen.target, next_boot.target);
    assert_ne!(chosen.target, usurper.target);
    assert_eq!(chosen.id, 3);
    assert_eq!(chosen.name, "Speakers");
}

#[test]
fn a_saved_shuffle_and_repeat_are_what_a_new_playback_thread_is_told() {
    let saved = Saved {
        shuffle: true,
        repeat: Repeat::One,
        ..Saved::default()
    };
    let player = Player::new(saved, 9, DOUBLE_CLICK);
    let [shuffle, repeat] = player.opening_edits();
    assert!(matches!(shuffle, Edit::Shuffle(Some(_))));
    assert_eq!(repeat, Edit::Repeat(Repeat::One));
    let plain = Player::new(Saved::default(), 9, DOUBLE_CLICK);
    assert_eq!(
        plain.opening_edits(),
        [Edit::Shuffle(None), Edit::Repeat(Repeat::Off)]
    );
}
