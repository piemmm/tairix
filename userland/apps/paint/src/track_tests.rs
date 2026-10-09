use tairix_controls::Keystroke;
use tairix_geometry::{Point, Rect, Region, Scale};
use tairix_input::{InputEvent, Key, Modifiers, NamedKey, PointerButton};
use tairix_raster::{Color, Surface};
use tairix_theme::ThemeRegistry;

use super::{Moved, Track, TRACK_HEIGHT};

const BLACK: Color = Color::rgba(0, 0, 0, 255);
const WHITE: Color = Color::rgba(255, 255, 255, 255);

fn levels() -> (Track, Rect) {
    let track = Track::new(0, 255, &[(0, BLACK), (255, WHITE)]);
    (track, Rect::new(10, 20, 264, TRACK_HEIGHT))
}

fn at(track: &mut Track, bounds: Rect, x: i32) -> Option<Moved> {
    track.on_pointer(
        &InputEvent::PointerMoved {
            to: Point::new(x, bounds.top() + 12),
        },
        bounds,
        Scale::ONE,
        &mut Region::new(),
    )
}

fn press(track: &mut Track, bounds: Rect) -> Option<Moved> {
    track.on_pointer(
        &InputEvent::PointerPressed {
            button: PointerButton::Primary,
        },
        bounds,
        Scale::ONE,
        &mut Region::new(),
    )
}

fn release(track: &mut Track, bounds: Rect) -> Option<Moved> {
    track.on_pointer(
        &InputEvent::PointerReleased {
            button: PointerButton::Primary,
        },
        bounds,
        Scale::ONE,
        &mut Region::new(),
    )
}

#[test]
fn a_press_takes_the_nearest_handle_and_drags_it_until_let_go() {
    let (mut track, bounds) = levels();
    at(&mut track, bounds, bounds.right() - 30);
    let pressed = press(&mut track, bounds).expect("the white handle moved");
    assert_eq!(pressed.handle, 1);
    assert!(!pressed.settled);
    let moved = at(&mut track, bounds, bounds.left() + 140).expect("follows");
    assert_eq!(moved.handle, 1);
    assert!(track.dragging());
    let released = release(&mut track, bounds).expect("settles");
    assert!(released.settled);
    assert_eq!(Some(released.value), track.value(1));
    assert!(
        at(&mut track, bounds, bounds.left() + 20).is_none(),
        "let go"
    );
    assert_eq!(track.value(0), Some(0));
}

#[test]
fn the_ends_reach_the_least_and_the_most() {
    let (mut track, bounds) = levels();
    at(&mut track, bounds, bounds.left() + 40);
    press(&mut track, bounds);
    let low = at(&mut track, bounds, bounds.left() - 50).expect("held to the least");
    assert_eq!((low.handle, low.value), (0, 0));
    release(&mut track, bounds);
    track.set(0, 100);
    at(&mut track, bounds, bounds.right() + 20);
    assert!(
        press(&mut track, bounds).is_none(),
        "off the track's end the press is not its"
    );
    at(&mut track, bounds, bounds.right() - 2);
    press(&mut track, bounds);
    assert_eq!(
        release(&mut track, bounds).map(|moved| moved.value),
        Some(255)
    );
}

#[test]
fn keys_step_the_focused_handle_and_nothing_else() {
    let (mut track, bounds) = levels();
    let key = |key| Keystroke {
        key: Key::Named(key),
        modifiers: Modifiers::default(),
        at_ns: 0,
    };
    assert!(track
        .on_key(key(NamedKey::Right), bounds, &mut Region::new())
        .is_none());
    track.focus(Some(0));
    let stepped = track
        .on_key(key(NamedKey::Right), bounds, &mut Region::new())
        .expect("stepped");
    assert_eq!(
        (stepped.handle, stepped.value, stepped.settled),
        (0, 1, true)
    );
    let shifted = Keystroke {
        modifiers: Modifiers {
            shift: true,
            ..Modifiers::default()
        },
        ..key(NamedKey::Right)
    };
    assert_eq!(
        track
            .on_key(shifted, bounds, &mut Region::new())
            .map(|moved| moved.value),
        Some(11)
    );
    track.focus(Some(1));
    assert!(
        track
            .on_key(key(NamedKey::End), bounds, &mut Region::new())
            .is_none(),
        "already there"
    );
    assert_eq!(
        track
            .on_key(key(NamedKey::Home), bounds, &mut Region::new())
            .map(|moved| moved.value),
        Some(0)
    );
    track.set_enabled(false);
    assert!(track
        .on_key(key(NamedKey::End), bounds, &mut Region::new())
        .is_none());
}

#[test]
fn a_track_draws_within_its_bounds() {
    let registry = ThemeRegistry::with_builtins();
    let (track, bounds) = levels();
    let mut surface = Surface::new(300, 60).expect("room");
    let grey = |along: u32| {
        let level = u8::try_from(along * 255 / 1000).unwrap_or(u8::MAX);
        Color::rgba(level, level, level, 255)
    };
    track.render(
        &mut surface,
        bounds,
        Scale::ONE,
        registry.active(),
        Some(&grey),
    );
    let outside = surface.get(2, 2).expect("in the surface");
    assert_eq!(
        outside,
        Surface::new(1, 1)
            .expect("room")
            .get(0, 0)
            .expect("a pixel")
    );
    let groove_mid = surface.get(150, 24).expect("in the surface");
    assert_ne!(groove_mid, outside, "the groove is drawn");
}
