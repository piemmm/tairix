//! Console-input producer for a virtio-input keyboard (`plans/PI.md` P11 /
//! P10 chunk 5d-2-ii).
//!
//! [`VirtioInput`](crate::VirtioInput) decodes the device's eventq into
//! [`InputEvent`] key edges whose `code` is the raw evdev keycode
//! (virtio-input reports in the `evdev` namespace, virtio 1.1 §5.8). This
//! module is the producer half: it names each edge's [`PhysicalKey`] — the HID
//! usage a USB keyboard reports for the same key — through its one table,
//! resolves it through the shared `lib/input` keyboard state (the held
//! modifiers, the caps and num locks and the US layout), and emits the
//! *device-resolved key edge*: a [`KeyInput`] record built through the shared
//! [`tairix_keymap::key_record`], leaving the encoding and routing to the
//! kernel input-focus arbiter. A keyboard driver injects each record into the
//! kernel through the `key_inject` syscall, which decides by who holds focus
//! whether to encode the press to a text console's tty bytes or deliver the
//! whole record to the desktop.
//!
//! Everything here is allocation-free and fail-closed: an
//! unknown keycode or a non-key event produces no record rather than guessing.

use tairix_abi::driver::input::InputEvent;
use tairix_abi::input::{KeyInput, PhysicalKey};
use tairix_input::KeyboardState;
use tairix_keymap::key_record;

/// Stateful translation of virtio-input keyboard [`InputEvent`] edges into the
/// [`KeyInput`] records a driver injects through `key_inject`.
///
/// The only state carried between [`feed`](Self::feed) calls is the
/// keyboard's — the held modifiers and the two lock toggles — so a reload
/// starts from a clean state by constructing a fresh instance.
#[derive(Debug, Default)]
pub struct VirtioKeyboardConsole {
    keyboard: KeyboardState,
}

impl VirtioKeyboardConsole {
    /// A producer with no keys held and both locks off.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            keyboard: KeyboardState::new(),
        }
    }

    /// Feed one decoded keyboard [`InputEvent`], returning the [`KeyInput`]
    /// record its key edge resolves to, or [`None`] when the edge produces no
    /// record.
    ///
    /// A modifier edge that changes the *observable* set of held modifiers
    /// produces a [`KeyInput::ModifiersChanged`] record — the desktop needs it
    /// to qualify a gesture that is not a key (a shift-click) — while one that
    /// does not (a repeat, or letting go of one shift key while the other is
    /// held) produces nothing. The lock keys update the internal state and
    /// produce no record. A *press* or *release* of a printable or named key
    /// produces the corresponding [`KeyInput::Pressed`] / [`KeyInput::Released`]
    /// record carrying the key the layout made of it, the modifiers held, and
    /// its physical key; an unknown keycode or a non-keyboard event produces
    /// nothing. Both edges are emitted so the desktop sees key-up as well as
    /// key-down; the text path ignores releases in the kernel arbiter.
    ///
    /// # Capabilities
    ///
    /// None (the producer holds no authority; delivery is the caller's).
    pub fn feed(&mut self, event: InputEvent) -> Option<KeyInput> {
        // A key repeat carries no edge, so it produces nothing (fail closed).
        let pressed = event.key_edge()?;
        key_record(&mut self.keyboard, physical_key(event.code), pressed)
    }
}

/// Each `evdev` keycode this producer reads (FreeBSD's `input-event-codes.h`)
/// and the HID usage of the key it names (HID Usage Tables, page `0x07`), in
/// keycode order: the one table from the `evdev` namespace into the system's.
const KEYCODES: &[(u16, u16)] = &[
    (1, 0x29),   // KEY_ESC
    (2, 0x1E),   // KEY_1
    (3, 0x1F),   // KEY_2
    (4, 0x20),   // KEY_3
    (5, 0x21),   // KEY_4
    (6, 0x22),   // KEY_5
    (7, 0x23),   // KEY_6
    (8, 0x24),   // KEY_7
    (9, 0x25),   // KEY_8
    (10, 0x26),  // KEY_9
    (11, 0x27),  // KEY_0
    (12, 0x2D),  // KEY_MINUS
    (13, 0x2E),  // KEY_EQUAL
    (14, 0x2A),  // KEY_BACKSPACE
    (15, 0x2B),  // KEY_TAB
    (16, 0x14),  // KEY_Q
    (17, 0x1A),  // KEY_W
    (18, 0x08),  // KEY_E
    (19, 0x15),  // KEY_R
    (20, 0x17),  // KEY_T
    (21, 0x1C),  // KEY_Y
    (22, 0x18),  // KEY_U
    (23, 0x0C),  // KEY_I
    (24, 0x12),  // KEY_O
    (25, 0x13),  // KEY_P
    (26, 0x2F),  // KEY_LEFTBRACE
    (27, 0x30),  // KEY_RIGHTBRACE
    (28, 0x28),  // KEY_ENTER
    (29, 0xE0),  // KEY_LEFTCTRL
    (30, 0x04),  // KEY_A
    (31, 0x16),  // KEY_S
    (32, 0x07),  // KEY_D
    (33, 0x09),  // KEY_F
    (34, 0x0A),  // KEY_G
    (35, 0x0B),  // KEY_H
    (36, 0x0D),  // KEY_J
    (37, 0x0E),  // KEY_K
    (38, 0x0F),  // KEY_L
    (39, 0x33),  // KEY_SEMICOLON
    (40, 0x34),  // KEY_APOSTROPHE
    (41, 0x35),  // KEY_GRAVE
    (42, 0xE1),  // KEY_LEFTSHIFT
    (43, 0x31),  // KEY_BACKSLASH
    (44, 0x1D),  // KEY_Z
    (45, 0x1B),  // KEY_X
    (46, 0x06),  // KEY_C
    (47, 0x19),  // KEY_V
    (48, 0x05),  // KEY_B
    (49, 0x11),  // KEY_N
    (50, 0x10),  // KEY_M
    (51, 0x36),  // KEY_COMMA
    (52, 0x37),  // KEY_DOT
    (53, 0x38),  // KEY_SLASH
    (54, 0xE5),  // KEY_RIGHTSHIFT
    (55, 0x55),  // KEY_KPASTERISK
    (56, 0xE2),  // KEY_LEFTALT
    (57, 0x2C),  // KEY_SPACE
    (58, 0x39),  // KEY_CAPSLOCK
    (59, 0x3A),  // KEY_F1
    (60, 0x3B),  // KEY_F2
    (61, 0x3C),  // KEY_F3
    (62, 0x3D),  // KEY_F4
    (63, 0x3E),  // KEY_F5
    (64, 0x3F),  // KEY_F6
    (65, 0x40),  // KEY_F7
    (66, 0x41),  // KEY_F8
    (67, 0x42),  // KEY_F9
    (68, 0x43),  // KEY_F10
    (69, 0x53),  // KEY_NUMLOCK
    (71, 0x5F),  // KEY_KP7
    (72, 0x60),  // KEY_KP8
    (73, 0x61),  // KEY_KP9
    (74, 0x56),  // KEY_KPMINUS
    (75, 0x5C),  // KEY_KP4
    (76, 0x5D),  // KEY_KP5
    (77, 0x5E),  // KEY_KP6
    (78, 0x57),  // KEY_KPPLUS
    (79, 0x59),  // KEY_KP1
    (80, 0x5A),  // KEY_KP2
    (81, 0x5B),  // KEY_KP3
    (82, 0x62),  // KEY_KP0
    (83, 0x63),  // KEY_KPDOT
    (87, 0x44),  // KEY_F11
    (88, 0x45),  // KEY_F12
    (96, 0x58),  // KEY_KPENTER
    (97, 0xE4),  // KEY_RIGHTCTRL
    (98, 0x54),  // KEY_KPSLASH
    (100, 0xE6), // KEY_RIGHTALT
    (102, 0x4A), // KEY_HOME
    (103, 0x52), // KEY_UP
    (104, 0x4B), // KEY_PAGEUP
    (105, 0x50), // KEY_LEFT
    (106, 0x4F), // KEY_RIGHT
    (107, 0x4D), // KEY_END
    (108, 0x51), // KEY_DOWN
    (109, 0x4E), // KEY_PAGEDOWN
    (110, 0x49), // KEY_INSERT
    (111, 0x4C), // KEY_DELETE
    (125, 0xE3), // KEY_LEFTMETA
    (126, 0xE7), // KEY_RIGHTMETA
];

/// The physical key `keycode` names, or [`PhysicalKey::UNKNOWN`] for a keycode
/// this producer reads no key from.
fn physical_key(keycode: u16) -> PhysicalKey {
    let usage = KEYCODES
        .binary_search_by_key(&keycode, |&(code, _)| code)
        .ok()
        .and_then(|at| KEYCODES.get(at))
        .map_or(0, |&(_, usage)| usage);
    PhysicalKey::from_usage(usage)
}

#[cfg(test)]
mod tests;
