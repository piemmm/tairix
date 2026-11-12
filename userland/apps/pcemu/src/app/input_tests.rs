use super::*;
use alloc::vec::Vec;
use tairix_abi::input::{Modifiers, PhysicalKey};

#[derive(Debug, PartialEq, Eq)]
enum Did {
    Key(Scan, bool),
    ReleaseKeys,
    Mouse(i32, i32, u8),
    Point(u8, u16, u16, u8),
}

#[derive(Default)]
struct Recorded(Vec<Did>);

impl Machine for Recorded {
    fn key(&mut self, scan: Scan, down: bool) {
        self.0.push(Did::Key(scan, down));
    }
    fn release_keys(&mut self) {
        self.0.push(Did::ReleaseKeys);
    }
    fn mouse(&mut self, dx: i32, dy: i32, buttons: u8) {
        self.0.push(Did::Mouse(dx, dy, buttons));
    }
    fn point(&mut self, adapter: u8, x: u16, y: u16, buttons: u8) {
        self.0.push(Did::Point(adapter, x, y, buttons));
    }
}

const OVER: Over = Over {
    adapter: 1,
    frame: (640, 200),
    target: Region {
        x: 0,
        y: 0,
        width: 1280,
        height: 960,
    },
};

const PRIMARY: PointerButtonCode = PointerButtonCode::Primary;

fn pressed(physical: PhysicalKey) -> KeyInput {
    KeyInput::Pressed {
        key: None,
        modifiers: Modifiers::default(),
        physical,
    }
}

fn released(physical: PhysicalKey) -> KeyInput {
    KeyInput::Released {
        key: None,
        modifiers: Modifiers::default(),
        physical,
    }
}

#[test]
fn a_key_is_the_machines_key_at_its_place() {
    let (mut input, mut machine) = (Input::new(), Recorded::default());
    input.key(&mut machine, &pressed(PhysicalKey::A));
    input.key(&mut machine, &released(PhysicalKey::A));
    input.key(&mut machine, &pressed(PhysicalKey::LEFT_GUI));
    assert_eq!(
        machine.0,
        [
            Did::Key(Scan::plain(0x1E), true),
            Did::Key(Scan::plain(0x1E), false)
        ],
        "the keyboard has no GUI key"
    );
}

#[test]
fn a_followed_pointer_is_placed_on_the_picture_and_hidden_there() {
    let (mut input, mut machine) = (Input::new(), Recorded::default());
    assert_eq!(input.pointing(&mut machine, true), Asks::default());
    let asks = input.pointer(&mut machine, OVER, (640, 480), PointerAction::Moved);
    assert_eq!(asks.hidden, Some(true));
    let asks = input.pointer(
        &mut machine,
        OVER,
        (640, 480),
        PointerAction::Pressed(PRIMARY),
    );
    assert_eq!(asks, Asks::default(), "already hidden");
    assert!(input.holding(), "the guest holds the press");
    let asks = input.pointer(&mut machine, OVER, (2000, 480), PointerAction::Moved);
    assert_eq!(asks.hidden, Some(false), "shown off the picture");
    input.pointer(
        &mut machine,
        OVER,
        (2000, 480),
        PointerAction::Released(PRIMARY),
    );
    assert!(!input.holding());
    input.pointer(&mut machine, OVER, (2000, 10), PointerAction::Moved);
    assert_eq!(
        machine.0,
        [
            Did::Point(1, 320, 100, 0),
            Did::Point(1, 320, 100, button::LEFT),
            Did::Point(1, 639, 100, button::LEFT),
            Did::Point(1, 639, 100, 0),
        ],
        "a drag off the picture follows to its edge, then nothing until it is back"
    );
}

#[test]
fn a_pointer_the_guest_reads_itself_is_captured_by_a_click_and_moves_by_motion() {
    let (mut input, mut machine) = (Input::new(), Recorded::default());
    let asks = input.pointer(
        &mut machine,
        OVER,
        (10, 10),
        PointerAction::Pressed(PRIMARY),
    );
    assert_eq!(asks, Asks::default(), "the press holds the seat");
    let asks = input.pointer(
        &mut machine,
        OVER,
        (10, 10),
        PointerAction::Released(PRIMARY),
    );
    assert_eq!(asks.lock, Some(true), "its release asks for the lock");
    assert!(
        machine.0.is_empty(),
        "the capturing click is not the guest's"
    );
    input.lock_changed(&mut machine, true);
    input.motion(&mut machine, 3, -2);
    let secondary = PointerButtonCode::Secondary;
    input.pointer(
        &mut machine,
        OVER,
        (10, 10),
        PointerAction::Pressed(secondary),
    );
    input.pointer(
        &mut machine,
        OVER,
        (10, 10),
        PointerAction::Pressed(PointerButtonCode::Middle),
    );
    input.lock_changed(&mut machine, false);
    input.motion(&mut machine, 9, 9);
    assert_eq!(
        machine.0,
        [
            Did::Mouse(3, -2, 0),
            Did::Mouse(0, 0, button::RIGHT),
            Did::Mouse(0, 0, 0),
        ],
        "the middle button is no mouse's, and the lock's end lets the right go"
    );
}

#[test]
fn losing_the_keyboard_lets_go_of_keys_buttons_and_the_lock() {
    let (mut input, mut machine) = (Input::new(), Recorded::default());
    input.lock_changed(&mut machine, true);
    input.pointer(&mut machine, OVER, (0, 0), PointerAction::Pressed(PRIMARY));
    assert_eq!(input.focus(&mut machine, true), Asks::default());
    let asks = input.focus(&mut machine, false);
    assert_eq!(asks.lock, Some(false));
    assert_eq!(
        machine.0,
        [
            Did::Mouse(0, 0, button::LEFT),
            Did::ReleaseKeys,
            Did::Mouse(0, 0, 0)
        ]
    );
}

#[test]
fn a_change_of_who_reads_the_mouse_lets_the_buttons_go_and_the_lock_with_it() {
    let (mut input, mut machine) = (Input::new(), Recorded::default());
    input.pointing(&mut machine, true);
    input.pointer(
        &mut machine,
        OVER,
        (640, 480),
        PointerAction::Pressed(PRIMARY),
    );
    let asks = input.pointing(&mut machine, false);
    assert_eq!(asks.hidden, Some(false), "the pointer shown again");
    input.lock_changed(&mut machine, true);
    let asks = input.pointing(&mut machine, true);
    assert_eq!(asks.lock, Some(false), "a followed pointer needs no lock");
    assert_eq!(
        machine.0,
        [
            Did::Point(1, 320, 100, button::LEFT),
            Did::Point(1, 320, 100, 0)
        ]
    );
}

#[test]
fn a_cancelled_pointer_lets_its_buttons_go() {
    let (mut input, mut machine) = (Input::new(), Recorded::default());
    input.pointing(&mut machine, true);
    input.pointer(
        &mut machine,
        OVER,
        (640, 480),
        PointerAction::Pressed(PRIMARY),
    );
    input.pointer(&mut machine, OVER, (640, 480), PointerAction::Cancelled);
    input.pointer(&mut machine, OVER, (640, 480), PointerAction::Cancelled);
    assert_eq!(
        machine.0,
        [
            Did::Point(1, 320, 100, button::LEFT),
            Did::Point(1, 320, 100, 0)
        ],
        "once"
    );
}

#[test]
fn the_held_title_names_the_machine_and_always_fits() {
    assert_eq!(held_title("Turbo XT"), format!("Turbo XT{HELD_HINT}"));
    let long = "\u{e9}".repeat(WINDOW_TITLE_MAX / 2);
    let title = held_title(&long);
    assert!(title.len() <= WINDOW_TITLE_MAX, "{} bytes", title.len());
    assert!(
        title.ends_with(HELD_HINT) && title.contains('\u{2026}'),
        "{title}"
    );
    assert!(tairix_abi::window_ipc::WindowTitle::new(&title).is_ok());
}

#[test]
fn ctrl_alt_delete_goes_down_in_order_and_comes_up_in_reverse() {
    let mut machine = Recorded::default();
    ctrl_alt_delete(&mut machine);
    let (ctrl, alt, del) = (Scan::plain(0x1D), Scan::plain(0x38), Scan::plain(0x53));
    assert_eq!(
        machine.0,
        [
            Did::Key(ctrl, true),
            Did::Key(alt, true),
            Did::Key(del, true),
            Did::Key(del, false),
            Did::Key(alt, false),
            Did::Key(ctrl, false),
        ]
    );
}
