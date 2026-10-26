//! Host tests of the hold rule alerts and owner-modal dialogs share.

use tairix_abi::input::{KeyInput, KeyValue, Modifiers, NamedKeyCode, PointerButtonCode};
use tairix_abi::window_ipc::{PointerAction, WindowEvent};
use tairix_wm::{Compositor, Point, Surface, WindowId};

use super::{is_press, reaches_held_window, Holds};
use crate::tests::headless_desktop;

fn windows(comp: &mut Compositor, count: usize) -> alloc::vec::Vec<WindowId> {
    (0..count)
        .map(|_| comp.add_window(Point::ORIGIN, Surface::new(4, 4).expect("a surface")))
        .collect()
}

#[test]
fn the_newest_holder_is_in_front_and_releasing_it_hands_back_to_the_next() {
    let (_, mut comp) = headless_desktop();
    let [owner, alert, dialog, other] = windows(&mut comp, 4)[..] else {
        unreachable!("four windows");
    };
    let mut holds = Holds::new();
    assert_eq!(holds.holder(owner), None);
    assert_eq!(holds.front(owner), owner);
    holds.take(owner, dialog);
    holds.take(owner, alert);
    assert_eq!(holds.holder(owner), Some(alert));
    assert_eq!(holds.holder(other), None, "a hold is on one window");
    assert_eq!(
        holds.release(alert),
        Some(dialog),
        "the keyboard goes to the holder behind"
    );
    assert_eq!(
        holds.release(dialog),
        Some(owner),
        "then to the window held"
    );
    assert_eq!(holds.release(dialog), None, "a hold is released once");
    assert_eq!(holds.holder(owner), None);
}

#[test]
fn a_window_gone_takes_its_holds_and_the_holds_on_it_with_it() {
    let (_, mut comp) = headless_desktop();
    let [owner, dialog, question, alert] = windows(&mut comp, 4)[..] else {
        unreachable!("four windows");
    };
    let mut holds = Holds::new();
    holds.take(owner, dialog);
    holds.take(dialog, question);
    holds.take(owner, alert);
    holds.forget(dialog);
    assert_eq!(holds.holder(dialog), None);
    assert_eq!(holds.release(question), None, "it held a window now gone");
    assert_eq!(holds.holder(owner), Some(alert));
}

#[test]
fn a_held_window_still_hears_a_release_and_nothing_that_starts_anything() {
    let pointer = |action| WindowEvent::Pointer {
        window_id: 4,
        x: 1,
        y: 1,
        action,
        modifiers: Modifiers::default(),
        at_ns: 0,
    };
    let release = pointer(PointerAction::Released(PointerButtonCode::Primary));
    assert!(reaches_held_window(&release));
    assert!(!reaches_held_window(&pointer(PointerAction::Moved)));
    assert!(!reaches_held_window(&pointer(PointerAction::Pressed(
        PointerButtonCode::Primary
    ))));
    let up = WindowEvent::Key {
        window_id: 4,
        key: KeyInput::Released {
            key: KeyValue::Named(NamedKeyCode::Enter),
            modifiers: Modifiers::default(),
        },
    };
    assert!(reaches_held_window(&up));
    let down = WindowEvent::Key {
        window_id: 4,
        key: KeyInput::Pressed {
            key: KeyValue::Named(NamedKeyCode::Enter),
            modifiers: Modifiers::default(),
        },
    };
    assert!(!reaches_held_window(&down));
}

#[test]
fn only_a_button_or_a_key_going_down_is_a_press() {
    let pointer = |action| WindowEvent::Pointer {
        window_id: 4,
        x: 1,
        y: 1,
        action,
        modifiers: Modifiers::default(),
        at_ns: 0,
    };
    assert!(is_press(&pointer(PointerAction::Pressed(
        PointerButtonCode::Primary
    ))));
    assert!(!is_press(&pointer(PointerAction::Released(
        PointerButtonCode::Primary
    ))));
    assert!(!is_press(&pointer(PointerAction::Moved)));
    let key = |key| WindowEvent::Key { window_id: 4, key };
    assert!(is_press(&key(KeyInput::Pressed {
        key: KeyValue::Char(' '),
        modifiers: Modifiers::default(),
    })));
    assert!(!is_press(&key(KeyInput::Released {
        key: KeyValue::Char(' '),
        modifiers: Modifiers::default(),
    })));
    assert!(!is_press(&WindowEvent::Scrolled {
        window_id: 4,
        x: 1,
        y: 1,
        dx: 0,
        dy: 1,
        modifiers: Modifiers::default(),
    }));
}
