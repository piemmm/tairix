//! What a machine window's input means to its machine: the raw keyboard as
//! the machine's keys, and the pointer either followed by the guest's own
//! mouse driver or, while the guest reads the mouse itself, captured.

use alloc::format;
use alloc::string::String;

use tairix_abi::input::{KeyInput, PhysicalKey, PointerButtonCode};
use tairix_abi::window_ipc::{PointerAction, WINDOW_TITLE_MAX};
use tairix_crt::screen::frame_point;
use tairix_log::Sink;
use tairix_raster::resample::Region;
use tairix_sandbox::session::SessionTransport;

use crate::chips::keyboard::{scan_of, Scan};
use crate::front::Host;
use crate::ports::mouse::button;

/// The input a machine takes: the part of [`Host`] a window drives.
pub trait Machine {
    /// A key goes down or comes up.
    fn key(&mut self, scan: Scan, down: bool);
    /// Every held key comes up.
    fn release_keys(&mut self);
    /// The serial mouse moves with `buttons` held.
    fn mouse(&mut self, dx: i32, dy: i32, buttons: u8);
    /// The host's pointer stands at a dot of adapter `adapter`'s frame.
    fn point(&mut self, adapter: u8, x: u16, y: u16, buttons: u8);
}

impl<T: SessionTransport, S: Sink> Machine for Host<T, S> {
    fn key(&mut self, scan: Scan, down: bool) {
        Self::key(self, scan, down);
    }

    fn release_keys(&mut self) {
        Self::release_keys(self);
    }

    fn mouse(&mut self, dx: i32, dy: i32, buttons: u8) {
        Self::mouse(self, dx, dy, buttons);
    }

    fn point(&mut self, adapter: u8, x: u16, y: u16, buttons: u8) {
        Self::point(self, adapter, x, y, buttons);
    }
}

/// The keypad's `.` and Delete key, HID usage `0x63`: the Delete the BIOS
/// reboots on.
const KEYPAD_DELETE: Option<PhysicalKey> = PhysicalKey::from_usage(0x63);

/// Press Ctrl, Alt and Delete on `machine`'s keyboard, and let them go, as
/// fingers on its keys would: the desktop keeps the chord for itself, so the
/// window offers it as a command.
pub fn ctrl_alt_delete(machine: &mut impl Machine) {
    let scans = [
        Some(PhysicalKey::LEFT_CTRL),
        Some(PhysicalKey::LEFT_ALT),
        KEYPAD_DELETE,
    ]
    .map(|key| key.and_then(scan_of));
    for scan in scans.iter().flatten() {
        machine.key(*scan, true);
    }
    for scan in scans.iter().rev().flatten() {
        machine.key(*scan, false);
    }
}

/// What a machine window's title says while the machine holds the pointer.
pub const HELD_HINT: &str = " \u{2014} tap Meta to release the mouse";

/// The title of machine `name`'s window while it holds the pointer: the name,
/// shortened where the hint would not fit after it, and the hint.
#[must_use]
pub fn held_title(name: &str) -> String {
    const ELLIPSIS: &str = "\u{2026}";
    let room = WINDOW_TITLE_MAX - HELD_HINT.len();
    if name.len() <= room {
        return format!("{name}{HELD_HINT}");
    }
    let keep = name.floor_char_boundary(room - ELLIPSIS.len());
    format!("{}{ELLIPSIS}{HELD_HINT}", &name[..keep])
}

/// What the window asks of the desktop for its input to go on reaching the
/// machine.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Asks {
    /// Lock the pointer to the window, or let it go.
    pub lock: Option<bool>,
    /// Hide the pointer over the window, or show it.
    pub hidden: Option<bool>,
}

/// Where the pointer is as the window sees it: over which adapter's picture,
/// of what size, drawn into what rectangle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Over {
    /// The adapter the picture is.
    pub adapter: u8,
    /// Its frame's dots and lines.
    pub frame: (u32, u32),
    /// Where it is drawn.
    pub target: Region,
}

/// One machine window's input.
#[derive(Clone, Copy, Debug, Default)]
pub struct Input {
    /// The guest's own driver follows the host's pointer.
    follows: bool,
    /// The desktop has the pointer locked to the window.
    locked: bool,
    /// The pointer is hidden over the picture.
    hidden: bool,
    /// The serial mouse's buttons held.
    buttons: u8,
    /// Where the pointer was last placed for the driver.
    placed: Option<(u8, u16, u16)>,
}

/// The serial mouse's button for a pointer's; a two-button mouse has no
/// middle one.
const fn mouse_button(button: PointerButtonCode) -> u8 {
    match button {
        PointerButtonCode::Primary => button::LEFT,
        PointerButtonCode::Secondary => button::RIGHT,
        PointerButtonCode::Middle => 0,
    }
}

fn contains(target: Region, (x, y): (i32, i32)) -> bool {
    let inside = |at: i32, from: u32, extent: u32| {
        let at = i64::from(at);
        at >= i64::from(from) && at < i64::from(from) + i64::from(extent)
    };
    inside(x, target.x, target.width) && inside(y, target.y, target.height)
}

impl Input {
    /// A window's input before anything happened: the pointer not followed.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            follows: false,
            locked: false,
            hidden: false,
            buttons: 0,
            placed: None,
        }
    }

    /// Whether the desktop has the pointer locked to the window.
    #[must_use]
    pub const fn locked(&self) -> bool {
        self.locked
    }

    /// Whether the guest holds a button down: a press it took, whose release
    /// is its too.
    #[must_use]
    pub const fn holding(&self) -> bool {
        self.buttons != 0
    }

    /// Key `key` went down or came up: the machine's key at its place, if
    /// the machine's keyboard has one there.
    pub fn key(&mut self, machine: &mut impl Machine, key: &KeyInput) {
        if let Some(scan) = scan_of(key.physical()) {
            machine.key(scan, key.is_press());
        }
    }

    /// The window gained or lost the keyboard: losing it lets every key and
    /// button go, and the pointer.
    pub fn focus(&mut self, machine: &mut impl Machine, focused: bool) -> Asks {
        if focused {
            return Asks::default();
        }
        machine.release_keys();
        self.release_buttons(machine);
        Asks {
            lock: self.locked.then_some(false),
            hidden: None,
        }
    }

    /// The guest started reading its mouse through the emulator's own driver
    /// (`follows`), or stopped and reads it itself.
    pub fn pointing(&mut self, machine: &mut impl Machine, follows: bool) -> Asks {
        if follows == self.follows {
            return Asks::default();
        }
        self.release_buttons(machine);
        self.follows = follows;
        let mut asks = Asks::default();
        if follows && self.locked {
            asks.lock = Some(false);
        }
        if !follows && self.hidden {
            self.hidden = false;
            asks.hidden = Some(false);
        }
        asks
    }

    /// The desktop locked the pointer to the window, or let it go; going
    /// lets the buttons go.
    pub fn lock_changed(&mut self, machine: &mut impl Machine, locked: bool) {
        self.locked = locked;
        if !locked {
            self.release_buttons(machine);
        }
    }

    /// The pointer did `action` at window point `at`, over the picture `over`.
    pub fn pointer(
        &mut self,
        machine: &mut impl Machine,
        over: Over,
        at: (i32, i32),
        action: PointerAction,
    ) -> Asks {
        let inside = contains(over.target, at);
        let mut asks = Asks::default();
        if matches!(action, PointerAction::Cancelled) {
            self.release_buttons(machine);
            return asks;
        }
        if self.follows {
            if inside != self.hidden {
                self.hidden = inside;
                asks.hidden = Some(inside);
            }
            let changed = self.press_or_release(action);
            if inside || changed || self.buttons != 0 {
                let (x, y) = frame_point(over.target, over.frame, at);
                self.placed = Some((over.adapter, x, y));
                machine.point(over.adapter, x, y, self.buttons);
            }
        } else if self.locked {
            if self.press_or_release(action) {
                machine.mouse(0, 0, self.buttons);
            }
        } else if inside && action == PointerAction::Released(PointerButtonCode::Primary) {
            // A held press keeps the seat, which a lock is refused under.
            asks.lock = Some(true);
        }
        asks
    }

    /// The locked pointer moved by `dx`, `dy` device counts.
    pub fn motion(&mut self, machine: &mut impl Machine, dx: i32, dy: i32) {
        if self.locked && !self.follows {
            machine.mouse(dx, dy, self.buttons);
        }
    }

    /// Fold a press or release into the buttons held: whether they changed.
    fn press_or_release(&mut self, action: PointerAction) -> bool {
        let before = self.buttons;
        match action {
            PointerAction::Pressed(code) => self.buttons |= mouse_button(code),
            PointerAction::Released(code) => self.buttons &= !mouse_button(code),
            PointerAction::Moved | PointerAction::Cancelled => {}
        }
        self.buttons != before
    }

    fn release_buttons(&mut self, machine: &mut impl Machine) {
        if self.buttons == 0 {
            return;
        }
        self.buttons = 0;
        match self.placed {
            Some((adapter, x, y)) if self.follows => machine.point(adapter, x, y, 0),
            _ => machine.mouse(0, 0, 0),
        }
    }
}

#[cfg(test)]
#[path = "input_tests.rs"]
mod tests;
