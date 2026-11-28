use alloc::vec::Vec;

use super::*;

/// Mode 12's screen: 640 by 256 pixels, each two OS units across and four
/// up.
const MODE_12: Screen = Screen {
    width: 640,
    height: 256,
    x_eig: 1,
    y_eig: 2,
    border: 0,
    blanked: false,
};

#[test]
fn each_key_s_edge_goes_once_by_its_low_level_number() {
    let mut held = Held::default();
    let a = PhysicalKey::A;
    let number = low_level(a).unwrap();
    assert_eq!(
        held.key(a, true),
        Some(InputEvent::Transition {
            key: number,
            down: true
        })
    );
    assert_eq!(held.key(a, true), None, "a press of a key already down");
    assert_eq!(
        held.key(a, false),
        Some(InputEvent::Transition {
            key: number,
            down: false
        })
    );
    assert_eq!(held.key(a, false), None, "a release of one that is up");
}

#[test]
fn a_key_with_no_low_level_number_goes_nowhere() {
    let unmapped = (0..=u16::MAX)
        .filter_map(PhysicalKey::from_usage)
        .find(|&key| low_level(key).is_none())
        .unwrap();
    assert_eq!(Held::default().key(unmapped, true), None);
}

#[test]
fn losing_the_keyboard_lets_go_of_every_key_and_button() {
    let mut held = Held::default();
    let (a, space) = (PhysicalKey::A, PhysicalKey::SPACE);
    held.key(a, true);
    held.key(space, true);
    held.button(ButtonOrder::Roles, PointerButtonCode::Primary, true);
    let (keys, buttons) = held.release();
    let mut released: Vec<InputEvent> = keys.collect();
    released.sort_by_key(|event| match event {
        InputEvent::Transition { key, .. } => *key,
        _ => u32::MAX,
    });
    let mut expected = [low_level(a).unwrap(), low_level(space).unwrap()];
    expected.sort_unstable();
    let expected: Vec<InputEvent> = expected
        .into_iter()
        .map(|key| InputEvent::Transition { key, down: false })
        .collect();
    assert_eq!(released, expected);
    assert!(buttons);
    assert_eq!(held, Held::default());
    let (keys, buttons) = held.release();
    assert_eq!(keys.count(), 0);
    assert!(!buttons, "nothing held twice");
}

#[test]
fn the_buttons_go_in_either_order() {
    let roles = [
        (PointerButtonCode::Primary, buttons::SELECT),
        (PointerButtonCode::Secondary, buttons::MENU),
        (PointerButtonCode::Middle, buttons::ADJUST),
    ];
    let mouse = [
        (PointerButtonCode::Primary, buttons::SELECT),
        (PointerButtonCode::Middle, buttons::MENU),
        (PointerButtonCode::Secondary, buttons::ADJUST),
    ];
    for (order, pairs) in [(ButtonOrder::Roles, roles), (ButtonOrder::Mouse, mouse)] {
        for (code, button) in pairs {
            assert_eq!(order.button(code), button, "{order:?} {code:?}");
        }
    }
    let mut held = Held::default();
    assert_eq!(
        held.button(ButtonOrder::Roles, PointerButtonCode::Primary, true),
        buttons::SELECT
    );
    assert_eq!(
        held.button(ButtonOrder::Roles, PointerButtonCode::Secondary, true),
        buttons::SELECT | buttons::MENU
    );
    assert_eq!(
        held.button(ButtonOrder::Roles, PointerButtonCode::Primary, false),
        buttons::MENU
    );
}

#[test]
fn a_window_pixel_is_its_screen_pixel_in_os_units_from_the_bottom_left() {
    let whole = Region {
        x: 0,
        y: 0,
        width: 640,
        height: 256,
    };
    assert_eq!(os_units(&MODE_12, whole, (0, 255)), (0, 0));
    assert_eq!(os_units(&MODE_12, whole, (0, 0)), (0, 255 * 4));
    assert_eq!(os_units(&MODE_12, whole, (639, 0)), (639 * 2, 255 * 4));
    let doubled = Region {
        x: 40,
        y: 20,
        width: 1280,
        height: 512,
    };
    assert_eq!(
        os_units(&MODE_12, doubled, (40 + 2 * 100, 20 + 2 * 55)),
        (200, 200 * 4)
    );
}

#[test]
fn a_pixel_beyond_the_picture_is_held_to_its_edge() {
    let placed = Region {
        x: 100,
        y: 50,
        width: 640,
        height: 256,
    };
    assert_eq!(os_units(&MODE_12, placed, (0, 0)), (0, 255 * 4));
    assert_eq!(os_units(&MODE_12, placed, (5000, 5000)), (639 * 2, 0));
}

#[test]
fn locked_motion_goes_up_rather_than_down() {
    assert_eq!(
        motion(3, 5, buttons::ADJUST),
        InputEvent::Motion {
            dx: 3,
            dy: -5,
            buttons: buttons::ADJUST
        }
    );
    assert_eq!(
        motion(0, i32::MIN, 0),
        InputEvent::Motion {
            dx: 0,
            dy: i32::MAX,
            buttons: 0
        }
    );
}

#[test]
fn the_pointer_goes_as_a_place_until_the_guest_sets_it_and_the_window_locks_it() {
    let mut pointing = Pointing::default();
    let place = (100, 200);
    assert_eq!(
        pointing.at(place, 0),
        Some(InputEvent::Pointer {
            x: 100,
            y: 200,
            buttons: 0
        })
    );
    assert_eq!(pointing.moved((3, 4), 0), None, "no motion unlocked");
    let (event, ask) = pointing.buttons(place, 0, true);
    assert_eq!(
        event,
        InputEvent::Pointer {
            x: 100,
            y: 200,
            buttons: 0
        }
    );
    assert!(!ask, "the guest has not set the pointer");
    pointing.guest_set();
    assert!(pointing.buttons(place, 0, true).1, "asked once it has");
    assert!(
        !pointing.buttons(place, buttons::SELECT, false).1,
        "only on a release"
    );
    let mut held = Held::default();
    assert_eq!(pointing.locked(true, &mut held), None);
    assert!(pointing.is_locked());
    assert_eq!(pointing.at(place, 0), None, "no place while locked");
    assert_eq!(
        pointing.moved((3, 4), buttons::MENU),
        Some(motion(3, 4, buttons::MENU))
    );
    assert_eq!(
        pointing.buttons(place, buttons::SELECT, false),
        (motion(0, 0, buttons::SELECT), false)
    );
    assert_eq!(pointing.locked(false, &mut held), None, "nothing held");
    assert!(
        pointing.buttons(place, 0, true).1,
        "asked again once released"
    );
}

#[test]
fn a_lock_s_end_lets_go_of_the_buttons_held_into_it_where_the_pointer_stands() {
    let mut pointing = Pointing::default();
    pointing.guest_set();
    let mut held = Held::default();
    assert_eq!(
        pointing.locked(true, &mut held),
        None,
        "a start lets go of nothing"
    );
    let buttons = held.button(ButtonOrder::Roles, PointerButtonCode::Primary, true);
    assert_eq!(
        pointing.buttons((0, 0), buttons, false).0,
        motion(0, 0, buttons::SELECT)
    );
    assert_eq!(
        pointing.locked(false, &mut held),
        Some(motion(0, 0, 0)),
        "the desktop leaves the window to let go of what the lock held"
    );
    assert_eq!(held.buttons(), 0);
    assert!(!pointing.is_locked());
    assert_eq!(pointing.locked(false, &mut held), None, "an end ends once");
}
