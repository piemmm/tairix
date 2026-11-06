//! Host tests of the screen-recording indicator: shown with the recording and
//! taken down after it, standing where no other feed moves it, and asking for
//! the recording to stop when pressed.

use alloc::vec::Vec;

use tairix_geometry::Scale;
use tairix_input::{InputEvent, PointerButton};
use tairix_theme::Theme;

use crate::input::{TaskbarInput, TaskbarResponse};
use crate::notifications::StatusKind;
use crate::sound::SoundState;
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

fn kinds(bar: &Taskbar) -> Vec<StatusKind> {
    bar.notifications()
        .signals()
        .iter()
        .map(|signal| signal.kind)
        .collect()
}

fn recording_sound(recording: bool) -> SoundState {
    SoundState {
        output: None,
        recording,
    }
}

#[test]
fn the_indicator_shows_while_the_screen_is_recorded() {
    let mut bar = bar();
    assert!(bar.set_screen_recording(true, Scale::ONE));
    assert_eq!(kinds(&bar), [StatusKind::ScreenRecording]);
    assert!(!bar.set_screen_recording(true, Scale::ONE), "nothing moved");
    assert!(bar.set_screen_recording(false, Scale::ONE));
    assert!(kinds(&bar).is_empty());
}

#[test]
fn the_sound_feed_never_moves_the_indicator() {
    let mut bar = bar();
    let _ = bar.set_sound(recording_sound(true), Scale::ONE);
    let _ = bar.set_screen_recording(true, Scale::ONE);
    assert_eq!(
        kinds(&bar),
        [StatusKind::ScreenRecording, StatusKind::Recording]
    );
    let _ = bar.set_sound(recording_sound(false), Scale::ONE);
    let _ = bar.set_sound(recording_sound(true), Scale::ONE);
    assert_eq!(
        kinds(&bar),
        [StatusKind::ScreenRecording, StatusKind::Recording]
    );
}

#[test]
fn a_press_on_the_indicator_asks_for_the_recording_to_stop() {
    let mut bar = bar();
    let _ = bar.set_screen_recording(true, Scale::ONE);
    let at = bar.layout(Scale::ONE).notifications[0].center();
    let mut input = TaskbarInput::new();
    input.handle(
        InputEvent::PointerMoved { to: at },
        &mut bar,
        Scale::ONE,
        NOW_NS,
    );
    assert_eq!(
        input.handle(
            InputEvent::PointerPressed {
                button: PointerButton::Primary,
            },
            &mut bar,
            Scale::ONE,
            NOW_NS,
        ),
        TaskbarResponse::StopScreenRecording
    );
}
