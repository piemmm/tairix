//! Host tests for the bar's sound: the signals, and the volume panel's
//! controls and what they ask of the session.

use alloc::string::String;

use tairix_abi::audio::AudioGain;
use tairix_geometry::{Point, Scale};
use tairix_input::{InputEvent, Key, Modifiers, NamedKey, PointerButton};
use tairix_theme::Theme;

use super::{OutputState, SoundAction, SoundState, RECORDING_SIGNAL, VOLUME_SIGNAL};
use crate::input::{TaskbarInput, TaskbarResponse};
use crate::notifications::{IconId, StatusKind, StatusSignal};
use crate::repaint::TaskbarRepaint;
use crate::taskbar::{Taskbar, TaskbarConfig};

const NOW_NS: u64 = 1_000_000_000;

fn bar() -> Taskbar {
    let mut bar = Taskbar::new(
        TaskbarConfig::bottom_bar(1000, 800),
        &Theme::dark().floating(),
    );
    let _ = bar.take_repaint();
    bar
}

fn speakers(level: i32, muted: bool, may_change: bool) -> SoundState {
    SoundState {
        output: Some(OutputState {
            device_id: 3,
            name: String::from("Speakers"),
            level: AudioGain::new(level).expect("attenuation"),
            muted,
            may_change,
        }),
        recording: false,
    }
}

fn send(input: &mut TaskbarInput, bar: &mut Taskbar, event: InputEvent) -> TaskbarResponse {
    input.handle(event, bar, Scale::ONE, NOW_NS)
}

fn press(input: &mut TaskbarInput, bar: &mut Taskbar, at: Point) -> TaskbarResponse {
    send(input, bar, InputEvent::PointerMoved { to: at });
    send(
        input,
        bar,
        InputEvent::PointerPressed {
            button: PointerButton::Primary,
        },
    )
}

fn release(input: &mut TaskbarInput, bar: &mut Taskbar) -> TaskbarResponse {
    send(
        input,
        bar,
        InputEvent::PointerReleased {
            button: PointerButton::Primary,
        },
    )
}

fn centre(rect: tairix_geometry::Rect) -> Point {
    Point::new(
        rect.left() + i32::try_from(rect.width / 2).unwrap_or(0),
        rect.top() + i32::try_from(rect.height / 2).unwrap_or(0),
    )
}

/// Open the panel from the volume signal, which is the first signal.
fn open(input: &mut TaskbarInput, bar: &mut Taskbar) {
    let slot = bar.layout(Scale::ONE).notifications[0];
    assert_eq!(press(input, bar, centre(slot)), TaskbarResponse::Ignored);
    release(input, bar);
    assert!(bar.sound().is_open(), "the volume signal opens its panel");
}

#[test]
fn the_signals_follow_the_output_and_the_recording() {
    let mut bar = bar();
    bar.set_status_signals(alloc::vec![StatusSignal::new(
        IconId(9),
        StatusKind::Network
    )]);
    let _ = bar.take_repaint();
    let _ = bar.set_sound(speakers(-600, false, true));
    let kinds = |bar: &Taskbar| -> alloc::vec::Vec<StatusKind> {
        bar.notifications()
            .signals()
            .iter()
            .map(|signal| signal.kind)
            .collect()
    };
    assert_eq!(kinds(&bar), [StatusKind::Network, StatusKind::Volume]);
    assert!(!bar.take_repaint().bar.is_clean());

    let mut recording = speakers(-600, true, true);
    recording.recording = true;
    let _ = bar.set_sound(recording.clone());
    assert_eq!(
        kinds(&bar),
        [
            StatusKind::Network,
            StatusKind::Muted,
            StatusKind::Recording
        ]
    );
    let _ = bar.take_repaint();
    let _ = bar.set_sound(recording);
    assert_eq!(bar.take_repaint(), TaskbarRepaint::NONE, "nothing moved");

    let _ = bar.set_sound(SoundState::default());
    assert_eq!(kinds(&bar), [StatusKind::Network], "no output, no signal");
    assert!(bar
        .notifications()
        .signals()
        .iter()
        .all(|signal| signal.id != VOLUME_SIGNAL && signal.id != RECORDING_SIGNAL));
}

#[test]
fn the_panel_moves_the_level_live_and_settles_where_the_drag_ends() {
    let mut bar = bar();
    let _ = bar.set_sound(speakers(-6_000, false, true));
    let mut input = TaskbarInput::new();
    open(&mut input, &mut bar);
    let layout = bar.sound_layout(Scale::ONE).expect("an open panel");
    let right = Point::new(layout.slider.right() - 2, centre(layout.slider).y);
    let pressed = press(&mut input, &mut bar, right);
    let TaskbarResponse::Sound(SoundAction::Level {
        device_id: 3,
        level: loud,
        settled: false,
    }) = pressed
    else {
        panic!("a press moves the level live: {pressed:?}");
    };
    assert!(loud.millibel() > -6_000, "louder to the right");
    let dragged = send(
        &mut input,
        &mut bar,
        InputEvent::PointerMoved {
            to: centre(layout.slider),
        },
    );
    let TaskbarResponse::Sound(SoundAction::Level {
        level,
        settled: false,
        ..
    }) = dragged
    else {
        panic!("a drag moves the level live: {dragged:?}");
    };
    assert!(
        level.millibel() < loud.millibel(),
        "quieter back to the middle"
    );
    // A report arriving mid-drag cannot pull the slider from under the pointer.
    let _ = bar.set_sound(speakers(-6_000, false, true));
    let settled = release(&mut input, &mut bar);
    assert_eq!(
        settled,
        TaskbarResponse::Sound(SoundAction::Level {
            device_id: 3,
            level,
            settled: true
        })
    );
    assert!(
        bar.sound().is_open(),
        "setting a level keeps the panel open"
    );

    let mute = centre(layout.mute);
    let pressed = press(&mut input, &mut bar, mute);
    let released = release(&mut input, &mut bar);
    let muted = TaskbarResponse::Sound(SoundAction::Mute {
        device_id: 3,
        muted: true,
    });
    assert!(
        pressed == muted || released == muted,
        "{pressed:?} {released:?}"
    );
}

#[test]
fn the_panel_closes_on_escape_outside_or_its_signal_and_when_the_output_goes() {
    let mut bar = bar();
    let _ = bar.set_sound(speakers(-600, false, true));
    let mut input = TaskbarInput::new();
    open(&mut input, &mut bar);
    send(
        &mut input,
        &mut bar,
        InputEvent::KeyPressed {
            key: Key::Named(NamedKey::Escape),
            modifiers: Modifiers::default(),
        },
    );
    assert!(!bar.sound().is_open());

    open(&mut input, &mut bar);
    assert_eq!(
        press(&mut input, &mut bar, Point::new(500, 100)),
        TaskbarResponse::Ignored
    );
    assert!(!bar.sound().is_open(), "a press outside closes it");

    open(&mut input, &mut bar);
    let slot = bar.layout(Scale::ONE).notifications[0];
    assert_eq!(
        press(&mut input, &mut bar, centre(slot)),
        TaskbarResponse::Ignored
    );
    assert!(!bar.sound().is_open(), "its own signal closes it");

    open(&mut input, &mut bar);
    let _ = bar.set_sound(SoundState::default());
    assert!(
        !bar.sound().is_open(),
        "an output that went away takes its panel"
    );
    assert!(bar.sound_layout(Scale::ONE).is_none());
}

#[test]
fn another_sessions_room_is_shown_not_changed() {
    let mut bar = bar();
    let _ = bar.set_sound(speakers(-600, false, false));
    let mut input = TaskbarInput::new();
    open(&mut input, &mut bar);
    let layout = bar.sound_layout(Scale::ONE).expect("an open panel");
    let at = Point::new(layout.slider.right() - 2, centre(layout.slider).y);
    let pressed = press(&mut input, &mut bar, at);
    assert!(
        !matches!(pressed, TaskbarResponse::Sound(_)),
        "a disabled control asks for nothing: {pressed:?}"
    );
    release(&mut input, &mut bar);
    let mute = centre(layout.mute);
    assert!(!matches!(
        press(&mut input, &mut bar, mute),
        TaskbarResponse::Sound(_)
    ));
    assert!(!matches!(
        release(&mut input, &mut bar),
        TaskbarResponse::Sound(_)
    ));
}

/// A drag Escape interrupts is settled where it stands, so the level the
/// session remembers is the one the listener left, and a second Escape closes
/// the panel.
#[test]
fn escape_mid_drag_settles_the_level_before_it_closes() {
    let mut bar = bar();
    let _ = bar.set_sound(speakers(-6_000, false, true));
    let mut input = TaskbarInput::new();
    open(&mut input, &mut bar);
    let layout = bar.sound_layout(Scale::ONE).expect("an open panel");
    let right = Point::new(layout.slider.right() - 2, centre(layout.slider).y);
    let TaskbarResponse::Sound(SoundAction::Level { level, .. }) =
        press(&mut input, &mut bar, right)
    else {
        panic!("the press moves the level");
    };
    let escape = InputEvent::KeyPressed {
        key: Key::Named(NamedKey::Escape),
        modifiers: Modifiers::default(),
    };
    assert_eq!(
        send(&mut input, &mut bar, escape),
        TaskbarResponse::Sound(SoundAction::Level {
            device_id: 3,
            level,
            settled: true,
        })
    );
    assert!(bar.sound().is_open());
    send(&mut input, &mut bar, escape);
    assert!(!bar.sound().is_open());
}
