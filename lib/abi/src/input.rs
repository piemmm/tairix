//! Wire-level pointer input event delivered over the kernel input channel.
//!
//! A pointing device reports motion, presses, and releases. On a running
//! system those reports reach the user-space desktop as a stream of framed
//! records over a capability-checked kernel input channel; this module fixes
//! the *contract* of one such record, [`PointerInput`]. The kernel never hands a parser the device's structure: it hands a
//! buffer, and [`PointerInput::from_bytes`] validates every field and fails
//! closed.
//!
//! The record is **device-resolved but screen-independent**: motion is the
//! relative displacement the device reported ([`MovedBy`](PointerInput::MovedBy)),
//! and a button edge names a *resolved* button (primary / secondary /
//! middle), never a raw platform keycode. Turning this stream into an
//! absolute on-screen pointer position — accumulating displacements and
//! clamping to the display extent — is the **seat owner's** policy: only the
//! desktop session that owns the compositor knows the screen's pixel size
//! (and its runtime scale), so the position lives there, never in a driver.
//! An input driver therefore needs no display-geometry authority at all —
//! it decodes its device and injects, and the kernel seat channel is a dumb,
//! validated pipe. (The desktop's `no_std` GUI vocabulary — `lib/input`'s
//! absolute `PointerMoved` — is what the seat owner produces *after* that
//! accumulation.)
//!
//! Keyboard input has two records. A keyboard driver reports a [`KeyEdge`]:
//! the [`PhysicalKey`] that went down or came up, and nothing it typed. The
//! seat reads every edge under its one set of modifiers, locks and layout and
//! answers a [`KeyInput`]: the edge, the [`KeyValue`] it typed if any (a
//! Unicode character, or a named non-character key) and the [`Modifiers`]
//! held. A physical key no layout changes, so a binding made by position holds
//! under any layout. Both decoders validate every field and fail closed on
//! untrusted input.
//!
//! # Relationship to the driver input ABI
//!
//! This is **not** a duplicate of [`crate::driver::input::InputEvent`]. That type is the *device-level* contract an input
//! driver reports across the [`Input`](crate::driver::input::Input) driver
//! trait: single-axis pointer *deltas*, scroll in scroll units, and platform keycodes,
//! one event per axis or edge. [`PointerInput`] is the *seat-channel* record
//! an input-driver process injects (`pointer_inject`): button keycodes are
//! resolved to the closed button set, and a scroll wheel becomes a
//! [`Scrolled`](PointerInput::Scrolled) record in scroll units. [`PointerInput::from_device_event`] is the one
//! spelling of that mapping, shared by every pointer-input driver.
//!
//! # Wire layout
//!
//! A record is exactly [`PointerInput::WIRE_LEN`] bytes, little-endian:
//!
//! | offset | size | field      | meaning                                  |
//! |-------:|-----:|------------|------------------------------------------|
//! |      0 |    4 | `magic`    | [`POINTER_INPUT_MAGIC`]                   |
//! |      4 |    2 | `version`  | ABI version ([`crate::ABI_VERSION_CURRENT`]) |
//! |      6 |    2 | `kind`     | [`KIND_MOVED_BY`] / [`KIND_PRESSED`] / [`KIND_RELEASED`] / [`KIND_SCROLLED`] |
//! |      8 |    2 | `button`   | [`BUTTON_NONE`] for motion/scroll, else a button code |
//! |     10 |    2 | `reserved` | must be zero                             |
//! |     12 |    4 | `dx`       | signed x displacement / scroll units     |
//! |     16 |    4 | `dy`       | signed y displacement / scroll units     |
//!
//! The displacement fields carry the reported motion for a
//! [`MovedBy`](PointerInput::MovedBy) record, the signed scroll units for a
//! [`Scrolled`](PointerInput::Scrolled) record, and must be zero for a press
//! or release (a real pointing device reports motion separately from clicks,
//! and the seat owner applies a button at the position its accumulated motion
//! established — the same model as `lib/input`). The `button` field is
//! [`BUTTON_NONE`] for motion and scroll and a valid button code for a press
//! or release. Any other combination is wire corruption and is refused.

use crate::driver::input::{
    InputEvent, InputEventKind, AXIS_X, AXIS_Y, POINTER_BUTTON_CODE_BASE, POINTER_BUTTON_COUNT,
};
use crate::le::{put_i32, put_u16, put_u32, put_u64, read_i32, read_u16, read_u32, read_u64};
use crate::Errno;

/// Magic number identifying an `abi-v1` pointer input record (`"PIN1"`
/// little-endian).
pub const POINTER_INPUT_MAGIC: u32 = u32::from_le_bytes(*b"PIN1");

/// `kind` code for a relative pointer move.
pub const KIND_MOVED_BY: u16 = 0;
/// `kind` code for a pointer-button press.
pub const KIND_PRESSED: u16 = 1;
/// `kind` code for a pointer-button release.
pub const KIND_RELEASED: u16 = 2;
/// `kind` code for a scroll-wheel turn.
pub const KIND_SCROLLED: u16 = 3;

/// `button` code carried by a [`MovedBy`](PointerInput::MovedBy) or
/// [`Scrolled`](PointerInput::Scrolled) record: no button is involved in a
/// motion or scroll event.
pub const BUTTON_NONE: u16 = 0;

/// The `pointer_inject` device time a driver states when it cannot say when
/// its device reported the record — a paravirtual device's transport carries no
/// such time — so the seat's chatter filter never judges it.
pub const POINTER_EDGE_UNTIMED: u64 = u64::MAX;

/// The pointer button a press or release record names.
///
/// A `#[repr(u16)]` enum so the wire code is exactly the integer carried in
/// the record's `button` field. The discriminants are part of the frozen
/// pointer-input ABI; [`PointerButtonCode::from_code`] rejects any other
/// value rather than guessing (validate every input).
#[repr(u16)]
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum PointerButtonCode {
    /// The primary (typically left) button: activates and grabs.
    Primary = 1,
    /// The secondary (typically right) button: context actions.
    Secondary = 2,
    /// The middle button.
    Middle = 3,
}

impl PointerButtonCode {
    /// The wire code carried in a record's `button` field.
    #[must_use]
    pub const fn code(self) -> u16 {
        self as u16
    }

    /// Decode a `button` field into a [`PointerButtonCode`].
    ///
    /// # Errors
    ///
    /// Returns [`Errno::OutOfRange`] for any value other than the three
    /// defined button codes (including [`BUTTON_NONE`], which is valid only
    /// on a motion record).
    pub const fn from_code(code: u16) -> Result<Self, Errno> {
        match code {
            1 => Ok(Self::Primary),
            2 => Ok(Self::Secondary),
            3 => Ok(Self::Middle),
            _ => Err(Errno::OutOfRange),
        }
    }
}

/// A single decoded pointer input event.
///
/// The type makes illegal states unrepresentable: a
/// [`MovedBy`](Self::MovedBy) carries a displacement and no button, while a
/// [`Pressed`](Self::Pressed) / [`Released`](Self::Released) carries a button
/// and no displacement. [`from_bytes`](Self::from_bytes) is the only way to
/// build one from untrusted bytes and validates the whole record before
/// returning it.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum PointerInput {
    /// The pointer moved by a relative displacement, in the device's
    /// count units (positive x rightward, positive y downward — the
    /// `evdev` orientation). The seat owner accumulates displacements
    /// into its absolute, extent-clamped screen position.
    MovedBy {
        /// Signed x displacement.
        dx: i32,
        /// Signed y displacement.
        dy: i32,
    },
    /// A pointer button went down at the current pointer position.
    Pressed(PointerButtonCode),
    /// A pointer button came up at the current pointer position.
    Released(PointerButtonCode),
    /// The scroll wheel turned, in scroll units
    /// ([`SCROLL_UNITS_PER_DETENT`](crate::driver::input::SCROLL_UNITS_PER_DETENT)
    /// to a detent; positive x toward the logical end, positive y downward —
    /// the `evdev` orientation). A scroll acts at the current pointer
    /// position, exactly as a button does; the seat owner routes it to the
    /// viewport under that position.
    Scrolled {
        /// Signed horizontal scroll, in scroll units.
        dx: i32,
        /// Signed vertical scroll, in scroll units.
        dy: i32,
    },
}

impl PointerInput {
    /// Encoded size of a pointer input record on the wire, in bytes.
    pub const WIRE_LEN: usize = 20;

    /// Encode `self` into its little-endian wire representation.
    #[must_use]
    pub fn to_le_bytes(&self) -> [u8; Self::WIRE_LEN] {
        let mut out = [0u8; Self::WIRE_LEN];
        put_u32(&mut out, 0, POINTER_INPUT_MAGIC);
        put_u16(&mut out, 4, crate::ABI_VERSION_CURRENT_U16);
        let (kind, button, dx, dy) = match *self {
            Self::MovedBy { dx, dy } => (KIND_MOVED_BY, BUTTON_NONE, dx, dy),
            Self::Pressed(button) => (KIND_PRESSED, button.code(), 0, 0),
            Self::Released(button) => (KIND_RELEASED, button.code(), 0, 0),
            Self::Scrolled { dx, dy } => (KIND_SCROLLED, BUTTON_NONE, dx, dy),
        };
        put_u16(&mut out, 6, kind);
        put_u16(&mut out, 8, button);
        // bytes 10..12 reserved, already zero.
        put_i32(&mut out, 12, dx);
        put_i32(&mut out, 16, dy);
        out
    }

    /// Decode `bytes` into a [`PointerInput`].
    ///
    /// Every field is validated; an invalid record is refused rather than
    /// interpreted (fail closed on untrusted
    /// input).
    ///
    /// # Errors
    ///
    /// * [`Errno::BufferTooSmall`] if `bytes.len() < WIRE_LEN`.
    /// * [`Errno::BadMagic`] if the magic word does not match, or if the
    ///   reserved field is non-zero, or if a press/release record carries a
    ///   non-zero displacement (a displacement belongs to a motion record
    ///   only).
    /// * [`Errno::AbiVersionUnsupported`] if `version` is not
    ///   [`crate::ABI_VERSION_CURRENT`].
    /// * [`Errno::OutOfRange`] if `kind` is not a defined kind, or if the
    ///   `button` field is inconsistent with the kind (a button code on a
    ///   motion or scroll record, or [`BUTTON_NONE`]/an unknown code on a
    ///   press or release).
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Errno> {
        if bytes.len() < Self::WIRE_LEN {
            return Err(Errno::BufferTooSmall);
        }
        if read_u32(bytes, 0) != POINTER_INPUT_MAGIC {
            return Err(Errno::BadMagic);
        }
        if u32::from(read_u16(bytes, 4)) != crate::ABI_VERSION_CURRENT {
            return Err(Errno::AbiVersionUnsupported);
        }
        if read_u16(bytes, 10) != 0 {
            return Err(Errno::BadMagic);
        }
        let kind = read_u16(bytes, 6);
        let button = read_u16(bytes, 8);
        let dx = read_i32(bytes, 12);
        let dy = read_i32(bytes, 16);
        match kind {
            KIND_MOVED_BY => {
                if button != BUTTON_NONE {
                    return Err(Errno::OutOfRange);
                }
                Ok(Self::MovedBy { dx, dy })
            }
            KIND_SCROLLED => {
                if button != BUTTON_NONE {
                    return Err(Errno::OutOfRange);
                }
                Ok(Self::Scrolled { dx, dy })
            }
            KIND_PRESSED | KIND_RELEASED => {
                if dx != 0 || dy != 0 {
                    return Err(Errno::BadMagic);
                }
                let button = PointerButtonCode::from_code(button)?;
                Ok(if kind == KIND_PRESSED {
                    Self::Pressed(button)
                } else {
                    Self::Released(button)
                })
            }
            _ => Err(Errno::OutOfRange),
        }
    }

    /// Translate one device-level [`InputEvent`] into the seat-channel
    /// record an input driver injects, or [`None`] when the event carries
    /// no pointer record.
    ///
    /// The one shared spelling of the device→seat pointer mapping, so the
    /// virtio and USB HID driver processes can never diverge:
    ///
    /// * a [`Pointer`](InputEventKind::Pointer) axis delta becomes a
    ///   single-axis [`MovedBy`](Self::MovedBy);
    /// * a [`Key`](InputEventKind::Key) edge whose code is one of the
    ///   [`POINTER_BUTTON_CODE_BASE`] button codes becomes a
    ///   [`Pressed`](Self::Pressed) / [`Released`](Self::Released) of the
    ///   resolved button;
    /// * a [`Scroll`](InputEventKind::Scroll) axis turn becomes a
    ///   single-axis [`Scrolled`](Self::Scrolled) in the same units;
    /// * everything else — keyboard keys (a keyboard producer's job), an
    ///   unknown axis, or a non-edge button value (a repeat) — produces no
    ///   record rather than a guessed one (fail closed, never fabricate
    ///   input).
    #[must_use]
    pub fn from_device_event(event: &InputEvent) -> Option<Self> {
        match event.kind {
            InputEventKind::Pointer => match event.code {
                AXIS_X => Some(Self::MovedBy {
                    dx: event.value,
                    dy: 0,
                }),
                AXIS_Y => Some(Self::MovedBy {
                    dx: 0,
                    dy: event.value,
                }),
                _ => None,
            },
            InputEventKind::Key => {
                let offset = event.code.checked_sub(POINTER_BUTTON_CODE_BASE)?;
                if offset >= POINTER_BUTTON_COUNT {
                    return None;
                }
                // The wire button codes are 1-based (`BUTTON_NONE` is 0),
                // the device codes 0-based from the base; `from_code`
                // keeps the closed set authoritative.
                let button = PointerButtonCode::from_code(offset + 1).ok()?;
                Some(if event.key_edge()? {
                    Self::Pressed(button)
                } else {
                    Self::Released(button)
                })
            }
            InputEventKind::Scroll => match event.code {
                AXIS_X => Some(Self::Scrolled {
                    dx: event.value,
                    dy: 0,
                }),
                AXIS_Y => Some(Self::Scrolled {
                    dx: 0,
                    dy: event.value,
                }),
                _ => None,
            },
        }
    }
}

/// A pointer record as the seat hands it to its owner (`pointer_read`): the
/// record, then when its device reported it on the monotonic clock — as its
/// driver stated, or else when the kernel took it — so a click, a double-click
/// or a stroke is timed by the device rather than by when the reader drained
/// it.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct TimedPointer {
    /// The record.
    pub input: PointerInput,
    /// When it happened.
    pub at_ns: u64,
}

impl TimedPointer {
    /// Encoded size: the [`PointerInput`] record, then the `u64` time.
    pub const WIRE_LEN: usize = PointerInput::WIRE_LEN + 8;

    /// Encode `self` little-endian.
    #[must_use]
    pub fn to_le_bytes(&self) -> [u8; Self::WIRE_LEN] {
        let mut out = [0u8; Self::WIRE_LEN];
        out[..PointerInput::WIRE_LEN].copy_from_slice(&self.input.to_le_bytes());
        put_u64(&mut out, PointerInput::WIRE_LEN, self.at_ns);
        out
    }

    /// Decode `bytes`, validating the record as [`PointerInput::from_bytes`]
    /// does.
    ///
    /// # Errors
    ///
    /// [`Errno::BufferTooSmall`] for fewer than [`Self::WIRE_LEN`] bytes, else
    /// as [`PointerInput::from_bytes`].
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Errno> {
        let time = bytes
            .get(PointerInput::WIRE_LEN..Self::WIRE_LEN)
            .ok_or(Errno::BufferTooSmall)?;
        Ok(Self {
            input: PointerInput::from_bytes(bytes)?,
            at_ns: read_u64(time, 0),
        })
    }
}

/// Magic number identifying an `abi-v1` keyboard input record (`"KIN1"`
/// little-endian).
pub const KEY_INPUT_MAGIC: u32 = u32::from_le_bytes(*b"KIN1");

/// `kind` code for a key-press record.
pub const KIND_KEY_PRESSED: u16 = 1;
/// `kind` code for a key-release record.
pub const KIND_KEY_RELEASED: u16 = 2;

/// `key_class` code for a record whose key typed nothing: a modifier, a lock,
/// or a key the layout names no value for.
pub const KEY_CLASS_NONE: u16 = 0;
/// `key_class` code for a [`KeyValue::Char`] record (a produced character).
pub const KEY_CLASS_CHAR: u16 = 1;
/// `key_class` code for a [`KeyValue::Named`] record (a named non-character
/// key).
pub const KEY_CLASS_NAMED: u16 = 2;

/// Modifier bit set when the shift key was held.
pub const MOD_SHIFT: u16 = 0x1;
/// Modifier bit set when the control key was held.
pub const MOD_CTRL: u16 = 0x2;
/// Modifier bit set when the alt key was held.
pub const MOD_ALT: u16 = 0x4;
/// Modifier bit set when the meta (super / command) key was held.
pub const MOD_META: u16 = 0x8;
/// Mask of every defined modifier bit; any bit outside it is wire corruption.
pub const MOD_MASK: u16 = MOD_SHIFT | MOD_CTRL | MOD_ALT | MOD_META;

/// The keyboard modifiers held while a key event was produced.
///
/// A plain set of booleans rather than a bitmask type so a caller names the
/// modifier it cares about; [`from_bits`](Self::from_bits) rejects any
/// undefined bit rather than silently dropping it.
//
// The four booleans are independent modifier-key states, not a state
// machine: any combination is legal (Ctrl+Shift, Alt+Meta, none), so a flat
// record models them more clearly than an enum.
#[allow(clippy::struct_excessive_bools)]
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct Modifiers {
    /// The shift key was held.
    pub shift: bool,
    /// The control key was held.
    pub ctrl: bool,
    /// The alt key was held.
    pub alt: bool,
    /// The meta (super / command) key was held.
    pub meta: bool,
}

impl Modifiers {
    /// Encode the modifiers as the record's little-endian bitmask field.
    #[must_use]
    pub const fn to_bits(self) -> u16 {
        let mut bits = 0;
        if self.shift {
            bits |= MOD_SHIFT;
        }
        if self.ctrl {
            bits |= MOD_CTRL;
        }
        if self.alt {
            bits |= MOD_ALT;
        }
        if self.meta {
            bits |= MOD_META;
        }
        bits
    }

    /// Decode a modifier bitmask, rejecting any bit outside [`MOD_MASK`].
    ///
    /// # Errors
    ///
    /// Returns [`Errno::OutOfRange`] if `bits` sets a bit this ABI version does
    /// not define (validate every input, fail closed).
    pub const fn from_bits(bits: u16) -> Result<Self, Errno> {
        if bits & !MOD_MASK != 0 {
            return Err(Errno::OutOfRange);
        }
        Ok(Self {
            shift: bits & MOD_SHIFT != 0,
            ctrl: bits & MOD_CTRL != 0,
            alt: bits & MOD_ALT != 0,
            meta: bits & MOD_META != 0,
        })
    }
}

/// A named non-character key the desktop distinguishes.
///
/// A `#[repr(u16)]` enum so the wire code is exactly the integer carried in a
/// record's `named` field. The discriminants are part of the keyboard ABI;
/// [`NamedKeyCode::from_code`] rejects any other value rather than guessing. Character-producing keys (letters, digits, punctuation,
/// space) are *not* listed here — they travel as a [`KeyValue::Char`], so this
/// set stays the small, closed list of keys that produce no character.
#[repr(u16)]
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum NamedKeyCode {
    /// The Enter / Return key.
    Enter = 1,
    /// The Escape key.
    Escape = 2,
    /// The Backspace key.
    Backspace = 3,
    /// The Tab key.
    Tab = 4,
    /// The forward-delete key.
    Delete = 5,
    /// The Insert key.
    Insert = 6,
    /// The Home key.
    Home = 7,
    /// The End key.
    End = 8,
    /// The Page Up key.
    PageUp = 9,
    /// The Page Down key.
    PageDown = 10,
    /// The left-arrow key.
    Left = 11,
    /// The right-arrow key.
    Right = 12,
    /// The up-arrow key.
    Up = 13,
    /// The down-arrow key.
    Down = 14,
    /// Function key F1.
    F1 = 15,
    /// Function key F2.
    F2 = 16,
    /// Function key F3.
    F3 = 17,
    /// Function key F4.
    F4 = 18,
    /// Function key F5.
    F5 = 19,
    /// Function key F6.
    F6 = 20,
    /// Function key F7.
    F7 = 21,
    /// Function key F8.
    F8 = 22,
    /// Function key F9.
    F9 = 23,
    /// Function key F10.
    F10 = 24,
    /// Function key F11.
    F11 = 25,
    /// Function key F12.
    F12 = 26,
}

impl NamedKeyCode {
    /// The wire code carried in a record's `named` field.
    #[must_use]
    pub const fn code(self) -> u16 {
        self as u16
    }

    /// Decode a `named` field into a [`NamedKeyCode`].
    ///
    /// # Errors
    ///
    /// Returns [`Errno::OutOfRange`] for any value that is not a defined named
    /// key (including `0`, which marks "no named key" on a character record).
    pub const fn from_code(code: u16) -> Result<Self, Errno> {
        match code {
            1 => Ok(Self::Enter),
            2 => Ok(Self::Escape),
            3 => Ok(Self::Backspace),
            4 => Ok(Self::Tab),
            5 => Ok(Self::Delete),
            6 => Ok(Self::Insert),
            7 => Ok(Self::Home),
            8 => Ok(Self::End),
            9 => Ok(Self::PageUp),
            10 => Ok(Self::PageDown),
            11 => Ok(Self::Left),
            12 => Ok(Self::Right),
            13 => Ok(Self::Up),
            14 => Ok(Self::Down),
            15 => Ok(Self::F1),
            16 => Ok(Self::F2),
            17 => Ok(Self::F3),
            18 => Ok(Self::F4),
            19 => Ok(Self::F5),
            20 => Ok(Self::F6),
            21 => Ok(Self::F7),
            22 => Ok(Self::F8),
            23 => Ok(Self::F9),
            24 => Ok(Self::F10),
            25 => Ok(Self::F11),
            26 => Ok(Self::F12),
            _ => Err(Errno::OutOfRange),
        }
    }
}

/// Where a key sits on the keyboard, whatever the layout makes of it: its
/// usage ID on the Keyboard/Keypad page (`0x07`) of the USB HID Usage Tables.
///
/// USB and I2C keyboards report these usages natively, and every other
/// keyboard producer maps its own codes onto them, so a binding made by
/// position — a system chord — holds under any layout. Only a usage the page
/// assigns a key is representable.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct PhysicalKey(u16);

impl PhysicalKey {
    /// The `A` key.
    pub const A: Self = Self(0x04);
    /// The main block's `3` key.
    pub const DIGIT_3: Self = Self(0x20);
    /// The main block's `4` key.
    pub const DIGIT_4: Self = Self(0x21);
    /// The main block's `5` key.
    pub const DIGIT_5: Self = Self(0x22);
    /// The Escape key.
    pub const ESCAPE: Self = Self(0x29);
    /// The space bar.
    pub const SPACE: Self = Self(0x2C);
    /// The Caps Lock key.
    pub const CAPS_LOCK: Self = Self(0x39);
    /// The keypad's Num Lock key.
    pub const NUM_LOCK: Self = Self(0x53);
    /// The left Control key.
    pub const LEFT_CTRL: Self = Self(0xE0);
    /// The left Shift key.
    pub const LEFT_SHIFT: Self = Self(0xE1);
    /// The left Alt key.
    pub const LEFT_ALT: Self = Self(0xE2);
    /// The left GUI key: Meta, marked with its maker's logo on a PC keyboard
    /// and Command on a Mac one.
    pub const LEFT_GUI: Self = Self(0xE3);
    /// The right Control key.
    pub const RIGHT_CTRL: Self = Self(0xE4);
    /// The right Shift key.
    pub const RIGHT_SHIFT: Self = Self(0xE5);
    /// The right Alt key.
    pub const RIGHT_ALT: Self = Self(0xE6);
    /// The right GUI key.
    pub const RIGHT_GUI: Self = Self(0xE7);

    /// The key HID usage `usage` names, or `None` where the page assigns it
    /// none: no event and the three error codes (`0x00..=0x03`), the reserved
    /// runs `0xA5..=0xAF` and `0xDE..=0xDF`, and everything past Right GUI.
    #[must_use]
    pub const fn from_usage(usage: u16) -> Option<Self> {
        match usage {
            0x04..=0xA4 | 0xB0..=0xDD | 0xE0..=0xE7 => Some(Self(usage)),
            _ => None,
        }
    }

    /// The key's HID usage.
    #[must_use]
    pub const fn usage(self) -> u16 {
        self.0
    }

    /// Whether the key is one of the eight modifier keys: Control, Shift,
    /// Alt and GUI, left and right.
    #[must_use]
    pub const fn is_modifier(self) -> bool {
        self.0 >= Self::LEFT_CTRL.0 && self.0 <= Self::RIGHT_GUI.0
    }
}

/// What a key typed: either a produced character or a named non-character
/// key.
///
/// The type makes illegal states unrepresentable: a
/// character key carries exactly its Unicode scalar, a named key exactly its
/// code, and the two never coexist in one record.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum KeyValue {
    /// A character-producing key, carrying the produced Unicode scalar (after
    /// the keyboard layout and any modifiers have been applied).
    Char(char),
    /// A named non-character key (an editing, navigation, or function key).
    Named(NamedKeyCode),
}

/// A key a seat has read: one key going down or coming up, where it sits on
/// the keyboard, the [`KeyValue`] it typed under the seat's modifiers, locks
/// and layout if it typed one, and the [`Modifiers`] held once it had gone
/// down or come up.
///
/// Every key a keyboard has reaches its seat as a record — a modifier, a lock
/// and a key no layout names among them, each typing nothing — so a window
/// that reads the keyboard raw sees every key, and one that reads text sees
/// only the keys that type. [`from_bytes`](Self::from_bytes) is the only way
/// to build one from untrusted bytes and validates the whole record before
/// returning it.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum KeyInput {
    /// A key went down.
    Pressed {
        /// What it typed, if anything.
        key: Option<KeyValue>,
        /// The modifiers held once it had gone down.
        modifiers: Modifiers,
        /// Where the key sits on the keyboard.
        physical: PhysicalKey,
    },
    /// A key came up.
    Released {
        /// What its press typed, whatever was let go or toggled meanwhile, so
        /// a reader pairs the two by value as surely as by position.
        key: Option<KeyValue>,
        /// The modifiers held once it had come up.
        modifiers: Modifiers,
        /// Where the key sits on the keyboard.
        physical: PhysicalKey,
    },
}

impl KeyInput {
    /// Encoded size of a keyboard input record on the wire, in bytes.
    pub const WIRE_LEN: usize = 20;

    /// What the key typed, or `None` for a key that types nothing.
    #[must_use]
    pub const fn key(&self) -> Option<KeyValue> {
        match *self {
            Self::Pressed { key, .. } | Self::Released { key, .. } => key,
        }
    }

    /// The [`Modifiers`] held once the key had gone down or come up.
    #[must_use]
    pub const fn modifiers(&self) -> Modifiers {
        match *self {
            Self::Pressed { modifiers, .. } | Self::Released { modifiers, .. } => modifiers,
        }
    }

    /// Where the key sits on the keyboard.
    #[must_use]
    pub const fn physical(&self) -> PhysicalKey {
        match *self {
            Self::Pressed { physical, .. } | Self::Released { physical, .. } => physical,
        }
    }

    /// Whether the key went down.
    #[must_use]
    pub const fn is_press(&self) -> bool {
        matches!(self, Self::Pressed { .. })
    }

    /// Encode `self` into its little-endian wire representation.
    #[must_use]
    pub fn to_le_bytes(&self) -> [u8; Self::WIRE_LEN] {
        let mut out = [0u8; Self::WIRE_LEN];
        put_u32(&mut out, 0, KEY_INPUT_MAGIC);
        put_u16(&mut out, 4, crate::ABI_VERSION_CURRENT_U16);
        let kind = if self.is_press() {
            KIND_KEY_PRESSED
        } else {
            KIND_KEY_RELEASED
        };
        put_u16(&mut out, 6, kind);
        put_u16(&mut out, 8, self.modifiers().to_bits());
        // A key that types nothing spells its value fields all-zero, the one
        // encoding `from_bytes` accepts for it.
        let (class, codepoint, named) = match self.key() {
            None => (KEY_CLASS_NONE, 0, 0),
            Some(KeyValue::Char(c)) => (KEY_CLASS_CHAR, c as u32, 0),
            Some(KeyValue::Named(named)) => (KEY_CLASS_NAMED, 0, named.code()),
        };
        put_u16(&mut out, 10, class);
        put_u32(&mut out, 12, codepoint);
        put_u16(&mut out, 16, named);
        put_u16(&mut out, 18, self.physical().usage());
        out
    }

    /// Decode `bytes` into a [`KeyInput`].
    ///
    /// Every field is validated; an invalid record is refused rather than
    /// interpreted.
    ///
    /// # Errors
    ///
    /// * [`Errno::BufferTooSmall`] if `bytes.len() < WIRE_LEN`.
    /// * [`Errno::BadMagic`] if the magic word does not match, or if a value
    ///   field the record does not use is non-zero (a named code on a
    ///   character record, a codepoint on a named record, either on a record
    ///   that types nothing).
    /// * [`Errno::AbiVersionUnsupported`] if `version` is not
    ///   [`crate::ABI_VERSION_CURRENT`].
    /// * [`Errno::OutOfRange`] if `kind`, `key_class`, the modifier bits, the
    ///   named-key code, the character scalar or the physical key is not valid
    ///   (an unpaired-surrogate or out-of-range codepoint is not a `char`, and
    ///   a usage the page assigns no key is not a key).
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Errno> {
        if bytes.len() < Self::WIRE_LEN {
            return Err(Errno::BufferTooSmall);
        }
        if read_u32(bytes, 0) != KEY_INPUT_MAGIC {
            return Err(Errno::BadMagic);
        }
        if u32::from(read_u16(bytes, 4)) != crate::ABI_VERSION_CURRENT {
            return Err(Errno::AbiVersionUnsupported);
        }
        let kind = read_u16(bytes, 6);
        let modifiers = Modifiers::from_bits(read_u16(bytes, 8))?;
        let class = read_u16(bytes, 10);
        let codepoint = read_u32(bytes, 12);
        let named = read_u16(bytes, 16);
        let physical = PhysicalKey::from_usage(read_u16(bytes, 18)).ok_or(Errno::OutOfRange)?;
        let key = match class {
            KEY_CLASS_NONE => {
                if codepoint != 0 || named != 0 {
                    return Err(Errno::BadMagic);
                }
                None
            }
            KEY_CLASS_CHAR => {
                if named != 0 {
                    return Err(Errno::BadMagic);
                }
                Some(KeyValue::Char(
                    char::from_u32(codepoint).ok_or(Errno::OutOfRange)?,
                ))
            }
            KEY_CLASS_NAMED => {
                if codepoint != 0 {
                    return Err(Errno::BadMagic);
                }
                Some(KeyValue::Named(NamedKeyCode::from_code(named)?))
            }
            _ => return Err(Errno::OutOfRange),
        };
        match kind {
            KIND_KEY_PRESSED => Ok(Self::Pressed {
                key,
                modifiers,
                physical,
            }),
            KIND_KEY_RELEASED => Ok(Self::Released {
                key,
                modifiers,
                physical,
            }),
            _ => Err(Errno::OutOfRange),
        }
    }
}

/// Magic number identifying an `abi-v1` key edge record (`"KED1"`
/// little-endian).
pub const KEY_EDGE_MAGIC: u32 = u32::from_le_bytes(*b"KED1");

/// One key going down or coming up, as its keyboard reports it: where the key
/// sits, and nothing it typed.
///
/// What a keyboard driver injects (`key_inject`). The seat the keyboard
/// belongs to reads every edge under one set of modifiers, locks and layout,
/// so two keyboards on a seat share them — Shift held on one capitalises a
/// letter typed on the other, as on every desktop — and the seat answers the
/// [`KeyInput`] each edge makes.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct KeyEdge {
    /// The key.
    pub key: PhysicalKey,
    /// Whether it went down.
    pub pressed: bool,
}

impl KeyEdge {
    /// Encoded size of a key edge record on the wire, in bytes: the magic,
    /// the version, the kind, the usage and two reserved bytes.
    pub const WIRE_LEN: usize = 12;

    /// Encode `self` into its little-endian wire representation.
    #[must_use]
    pub fn to_le_bytes(&self) -> [u8; Self::WIRE_LEN] {
        let mut out = [0u8; Self::WIRE_LEN];
        put_u32(&mut out, 0, KEY_EDGE_MAGIC);
        put_u16(&mut out, 4, crate::ABI_VERSION_CURRENT_U16);
        put_u16(
            &mut out,
            6,
            if self.pressed {
                KIND_KEY_PRESSED
            } else {
                KIND_KEY_RELEASED
            },
        );
        put_u16(&mut out, 8, self.key.usage());
        out
    }

    /// Decode `bytes` into a [`KeyEdge`].
    ///
    /// # Errors
    ///
    /// * [`Errno::BufferTooSmall`] if `bytes.len() < WIRE_LEN`.
    /// * [`Errno::BadMagic`] if the magic word does not match or the reserved
    ///   bytes are not zero.
    /// * [`Errno::AbiVersionUnsupported`] if `version` is not
    ///   [`crate::ABI_VERSION_CURRENT`].
    /// * [`Errno::OutOfRange`] if `kind` is not a press or a release, or the
    ///   usage names no key.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Errno> {
        if bytes.len() < Self::WIRE_LEN {
            return Err(Errno::BufferTooSmall);
        }
        if read_u32(bytes, 0) != KEY_EDGE_MAGIC || read_u16(bytes, 10) != 0 {
            return Err(Errno::BadMagic);
        }
        if u32::from(read_u16(bytes, 4)) != crate::ABI_VERSION_CURRENT {
            return Err(Errno::AbiVersionUnsupported);
        }
        let pressed = match read_u16(bytes, 6) {
            KIND_KEY_PRESSED => true,
            KIND_KEY_RELEASED => false,
            _ => return Err(Errno::OutOfRange),
        };
        Ok(Self {
            key: PhysicalKey::from_usage(read_u16(bytes, 8)).ok_or(Errno::OutOfRange)?,
            pressed,
        })
    }
}

/// Per-button history for suppressing a re-press that follows a release too
/// closely to be a distinct click.
///
/// A mechanical switch can chatter on release, emitting a second press a few
/// milliseconds later that the device intended as one click. The seat cannot
/// ask the device which it meant, so it applies a window: a press inside the
/// window of the same button's last release is dropped, and the release that
/// closes that same pulse is dropped with it, so no consumer is left holding
/// half an edge pair as a latched grab.
///
/// The window is measured between the times the *device* reported the two
/// edges, as its driver states them, never between their injections: a driver
/// catching up on a backlog injects distinct clicks back to back. A press with
/// no stated time, after a release with none, or stated at the release's own
/// instant — the two read together — cannot be told from a distinct click, and
/// is admitted.
///
/// Holds one device's history only. The window and the times are both the
/// caller's to know, so this type is pure, host-testable, and reads neither
/// behind the caller's back.
///
/// A window of zero disables the filter: a device whose rapid-fire mode
/// deliberately emits click pairs at ~10 ms is reporting real intent, and
/// suppressing that would destroy real input. Motion and scroll are never
/// debounced.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct ClickDebounce {
    /// When the device reported each button's last release, if its driver
    /// said, indexed by `PointerButtonCode` discriminant minus one.
    released_ns: [Option<u64>; POINTER_BUTTON_COUNT as usize],
    /// Buttons whose press was dropped, so the release closing the same
    /// chatter pulse is dropped too.
    suppressed: [bool; POINTER_BUTTON_COUNT as usize],
}

impl ClickDebounce {
    /// A filter with no history: the first edge of any button is admitted.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            released_ns: [None; POINTER_BUTTON_COUNT as usize],
            suppressed: [false; POINTER_BUTTON_COUNT as usize],
        }
    }

    /// Whether `record`, which the device reported at monotonic `at_ns` if its
    /// driver knows when, is delivered under a chatter window of `window_ns`
    /// (zero disabling the filter).
    ///
    /// Motion and scroll are always delivered: only a button edge can chatter.
    pub fn admits(&mut self, record: PointerInput, at_ns: Option<u64>, window_ns: u64) -> bool {
        let (button, pressed) = match record {
            PointerInput::Pressed(button) => (button, true),
            PointerInput::Released(button) => (button, false),
            PointerInput::MovedBy { .. } | PointerInput::Scrolled { .. } => return true,
        };
        let slot = usize::from(button.code() - 1);
        let Some(released_ns) = self.released_ns.get_mut(slot) else {
            return true;
        };
        if !pressed {
            *released_ns = at_ns;
            // A window turned off between the press and its release must not
            // swallow the release: the suppression is cleared either way.
            return match self.suppressed.get_mut(slot) {
                Some(flag) if *flag => {
                    *flag = false;
                    window_ns == 0
                }
                _ => true,
            };
        }
        if window_ns == 0 {
            return true;
        }
        let chatter = match (*released_ns, at_ns) {
            (Some(released), Some(pressed)) => pressed > released && pressed - released < window_ns,
            _ => false,
        };
        if chatter {
            if let Some(flag) = self.suppressed.get_mut(slot) {
                *flag = true;
            }
        }
        !chatter
    }
}

#[cfg(test)]
mod tests {
    use crate::time::NANOS_PER_MILLI as MS;

    /// The default chatter window the tests exercise.
    const WINDOW: u64 = 25 * MS;

    fn debounce() -> ClickDebounce {
        ClickDebounce::new()
    }

    #[test]
    fn an_ordinary_click_is_admitted_whole() {
        let mut filter = debounce();
        assert!(filter.admits(
            PointerInput::Pressed(PointerButtonCode::Primary),
            Some(0),
            WINDOW
        ));
        assert!(filter.admits(
            PointerInput::Released(PointerButtonCode::Primary),
            Some(80 * MS),
            WINDOW
        ));
    }

    #[test]
    fn a_repress_inside_the_window_is_dropped_with_its_release() {
        // The on-metal chatter: a second press 16 ms after the release, and the
        // 32 ms pulse that closes it. Both go, so no consumer sees an unpaired
        // edge it would hold as a latched grab.
        let mut filter = debounce();
        assert!(filter.admits(
            PointerInput::Pressed(PointerButtonCode::Primary),
            Some(0),
            WINDOW
        ));
        assert!(filter.admits(
            PointerInput::Released(PointerButtonCode::Primary),
            Some(80 * MS),
            WINDOW
        ));
        assert!(!filter.admits(
            PointerInput::Pressed(PointerButtonCode::Primary),
            Some(96 * MS),
            WINDOW
        ));
        assert!(!filter.admits(
            PointerInput::Released(PointerButtonCode::Primary),
            Some(128 * MS),
            WINDOW
        ));
        // The next genuine click is unaffected.
        assert!(filter.admits(
            PointerInput::Pressed(PointerButtonCode::Primary),
            Some(400 * MS),
            WINDOW
        ));
    }

    #[test]
    fn a_repress_at_the_window_edge_is_admitted() {
        let mut filter = debounce();
        assert!(filter.admits(
            PointerInput::Released(PointerButtonCode::Primary),
            Some(0),
            WINDOW
        ));
        assert!(filter.admits(
            PointerInput::Pressed(PointerButtonCode::Primary),
            Some(25 * MS),
            WINDOW
        ));
    }

    #[test]
    fn the_window_is_per_button() {
        // A chattering left switch must not swallow a deliberate right click.
        let mut filter = debounce();
        assert!(filter.admits(
            PointerInput::Released(PointerButtonCode::Primary),
            Some(0),
            WINDOW
        ));
        assert!(filter.admits(
            PointerInput::Pressed(PointerButtonCode::Secondary),
            Some(5 * MS),
            WINDOW
        ));
    }

    #[test]
    fn a_zero_window_admits_a_rapid_fire_pair() {
        // A device whose rapid-fire mode emits click pairs at ~10 ms is
        // reporting real intent; zero must deliver it untouched.
        let mut filter = ClickDebounce::new();
        for step in 0..4 {
            let t = step * 10 * MS;
            assert!(filter.admits(
                PointerInput::Pressed(PointerButtonCode::Primary),
                Some(t),
                0
            ));
            assert!(filter.admits(
                PointerInput::Released(PointerButtonCode::Primary),
                Some(t + 5 * MS),
                0
            ));
        }
    }

    #[test]
    fn turning_the_window_off_mid_gesture_still_delivers_the_release() {
        // A press dropped as chatter, then the operator sets the window to
        // zero. The release must reach consumers rather than be swallowed by a
        // window that no longer applies.
        let mut filter = debounce();
        assert!(filter.admits(
            PointerInput::Released(PointerButtonCode::Primary),
            Some(0),
            WINDOW
        ));
        assert!(!filter.admits(
            PointerInput::Pressed(PointerButtonCode::Primary),
            Some(5 * MS),
            WINDOW
        ));
        assert!(filter.admits(
            PointerInput::Released(PointerButtonCode::Primary),
            Some(9 * MS),
            0
        ));
    }

    #[test]
    fn motion_and_scroll_are_never_debounced() {
        let mut filter = debounce();
        assert!(filter.admits(
            PointerInput::Released(PointerButtonCode::Primary),
            Some(0),
            WINDOW
        ));
        assert!(filter.admits(PointerInput::MovedBy { dx: 3, dy: -2 }, Some(MS), WINDOW));
        assert!(filter.admits(
            PointerInput::Scrolled { dx: 0, dy: 1 },
            Some(2 * MS),
            WINDOW
        ));
    }

    #[test]
    fn an_edge_without_a_device_time_is_never_judged() {
        // A paravirtual device's driver knows no device time, so however
        // closely it injects distinct clicks after a stall, each is delivered.
        let press = PointerInput::Pressed(PointerButtonCode::Primary);
        let release = PointerInput::Released(PointerButtonCode::Primary);
        let mut untimed = debounce();
        for record in [press, release, press, release] {
            assert!(untimed.admits(record, None, WINDOW));
        }
        let mut press_untimed = debounce();
        assert!(press_untimed.admits(release, Some(0), WINDOW));
        assert!(press_untimed.admits(press, None, WINDOW));
        let mut release_untimed = debounce();
        assert!(release_untimed.admits(release, None, WINDOW));
        assert!(release_untimed.admits(press, Some(MS), WINDOW));
    }

    #[test]
    fn edges_stated_at_one_instant_are_not_judged() {
        // One report carries one state per button, so a release and the next
        // press stated at the same instant are two reports read together.
        let mut filter = debounce();
        assert!(filter.admits(
            PointerInput::Released(PointerButtonCode::Primary),
            Some(40 * MS),
            WINDOW
        ));
        assert!(filter.admits(
            PointerInput::Pressed(PointerButtonCode::Primary),
            Some(40 * MS),
            WINDOW
        ));
    }

    use super::{
        ClickDebounce, KeyEdge, KeyInput, KeyValue, Modifiers, NamedKeyCode, PhysicalKey,
        PointerButtonCode, PointerInput, TimedPointer, BUTTON_NONE, KEY_CLASS_CHAR,
        KEY_CLASS_NAMED, KEY_CLASS_NONE, KEY_EDGE_MAGIC, KEY_INPUT_MAGIC, KIND_KEY_PRESSED,
        KIND_KEY_RELEASED, KIND_MOVED_BY, KIND_PRESSED, KIND_RELEASED, KIND_SCROLLED, MOD_ALT,
        MOD_CTRL, MOD_META, MOD_SHIFT, POINTER_INPUT_MAGIC,
    };
    use crate::driver::input::{
        InputEvent, InputEventKind, AXIS_X, AXIS_Y, POINTER_BUTTON_CODE_BASE,
        SCROLL_UNITS_PER_DETENT,
    };
    use crate::le::read_u16;
    use crate::{Errno, ABI_VERSION_CURRENT};

    #[test]
    fn wire_size_is_twenty() {
        assert_eq!(PointerInput::WIRE_LEN, 20);
    }

    #[test]
    fn moved_round_trips() {
        let event = PointerInput::MovedBy { dx: -17, dy: 42 };
        let decoded = PointerInput::from_bytes(&event.to_le_bytes()).expect("valid record");
        assert_eq!(decoded, event);
    }

    #[test]
    fn a_timed_record_round_trips_and_validates_its_record() {
        let timed = TimedPointer {
            input: PointerInput::Pressed(PointerButtonCode::Secondary),
            at_ns: 0x0011_2233_4455_6677,
        };
        let bytes = timed.to_le_bytes();
        assert_eq!(TimedPointer::from_bytes(&bytes), Ok(timed));
        assert_eq!(
            TimedPointer::from_bytes(&bytes[..TimedPointer::WIRE_LEN - 1]),
            Err(Errno::BufferTooSmall)
        );
        let mut corrupt = bytes;
        corrupt[0] ^= 0xFF;
        assert_eq!(TimedPointer::from_bytes(&corrupt), Err(Errno::BadMagic));
    }

    #[test]
    fn press_and_release_round_trip_each_button() {
        for button in [
            PointerButtonCode::Primary,
            PointerButtonCode::Secondary,
            PointerButtonCode::Middle,
        ] {
            for event in [
                PointerInput::Pressed(button),
                PointerInput::Released(button),
            ] {
                let decoded = PointerInput::from_bytes(&event.to_le_bytes()).expect("valid record");
                assert_eq!(decoded, event);
            }
        }
    }

    #[test]
    fn rejects_short_buffer() {
        assert_eq!(
            PointerInput::from_bytes(&[0u8; 8]),
            Err(Errno::BufferTooSmall)
        );
    }

    #[test]
    fn rejects_bad_magic() {
        let mut bytes = PointerInput::MovedBy { dx: 1, dy: 2 }.to_le_bytes();
        bytes[0] ^= 0xFF;
        assert_eq!(PointerInput::from_bytes(&bytes), Err(Errno::BadMagic));
    }

    #[test]
    fn rejects_bad_version() {
        let mut bytes = PointerInput::MovedBy { dx: 1, dy: 2 }.to_le_bytes();
        bytes[4] = 99;
        assert_eq!(
            PointerInput::from_bytes(&bytes),
            Err(Errno::AbiVersionUnsupported)
        );
    }

    #[test]
    fn rejects_nonzero_reserved() {
        let mut bytes = PointerInput::MovedBy { dx: 1, dy: 2 }.to_le_bytes();
        bytes[10] = 1;
        assert_eq!(PointerInput::from_bytes(&bytes), Err(Errno::BadMagic));
    }

    #[test]
    fn rejects_unknown_kind() {
        let mut bytes = PointerInput::MovedBy { dx: 0, dy: 0 }.to_le_bytes();
        bytes[6] = 9;
        assert_eq!(PointerInput::from_bytes(&bytes), Err(Errno::OutOfRange));
    }

    #[test]
    fn rejects_button_on_motion() {
        let mut bytes = PointerInput::MovedBy { dx: 0, dy: 0 }.to_le_bytes();
        // Low byte of the `button` field at offset 8: set it to Primary (1).
        bytes[8] = 1;
        assert_eq!(PointerInput::from_bytes(&bytes), Err(Errno::OutOfRange));
    }

    #[test]
    fn rejects_no_button_on_press() {
        // A well-formed press, then clear its button field to BUTTON_NONE (0).
        let mut bytes = PointerInput::Pressed(PointerButtonCode::Primary).to_le_bytes();
        bytes[8] = 0;
        assert_eq!(PointerInput::from_bytes(&bytes), Err(Errno::OutOfRange));
    }

    #[test]
    fn rejects_unknown_button_on_release() {
        let mut bytes = PointerInput::Released(PointerButtonCode::Middle).to_le_bytes();
        bytes[8] = 7;
        assert_eq!(PointerInput::from_bytes(&bytes), Err(Errno::OutOfRange));
    }

    #[test]
    fn rejects_displacement_on_press() {
        let mut bytes = PointerInput::Pressed(PointerButtonCode::Primary).to_le_bytes();
        bytes[12] = 1;
        assert_eq!(PointerInput::from_bytes(&bytes), Err(Errno::BadMagic));
    }

    #[test]
    fn scroll_round_trips_with_signed_units() {
        let event = PointerInput::Scrolled { dx: -15, dy: 240 };
        let decoded = PointerInput::from_bytes(&event.to_le_bytes()).expect("valid record");
        assert_eq!(decoded, event);
    }

    #[test]
    fn rejects_button_on_scroll() {
        // A well-formed scroll record, then plant a button code: a scroll
        // carries no button, exactly like a motion record.
        let mut bytes = PointerInput::Scrolled { dx: 0, dy: 1 }.to_le_bytes();
        bytes[8] = 1;
        assert_eq!(PointerInput::from_bytes(&bytes), Err(Errno::OutOfRange));
    }

    /// One device event per axis maps onto a single-axis displacement.
    #[test]
    fn device_axis_deltas_map_to_single_axis_moves() {
        let x = InputEvent {
            kind: InputEventKind::Pointer,
            reserved0: 0,
            code: AXIS_X,
            value: -6,
        };
        let y = InputEvent {
            kind: InputEventKind::Pointer,
            reserved0: 0,
            code: AXIS_Y,
            value: 11,
        };
        assert_eq!(
            PointerInput::from_device_event(&x),
            Some(PointerInput::MovedBy { dx: -6, dy: 0 })
        );
        assert_eq!(
            PointerInput::from_device_event(&y),
            Some(PointerInput::MovedBy { dx: 0, dy: 11 })
        );
    }

    /// The three `evdev` button codes resolve to the closed button set,
    /// press and release alike.
    #[test]
    fn device_button_edges_resolve_each_button() {
        for (offset, button) in [
            (0, PointerButtonCode::Primary),
            (1, PointerButtonCode::Secondary),
            (2, PointerButtonCode::Middle),
        ] {
            let mut event = InputEvent {
                kind: InputEventKind::Key,
                reserved0: 0,
                code: POINTER_BUTTON_CODE_BASE + offset,
                value: 1,
            };
            assert_eq!(
                PointerInput::from_device_event(&event),
                Some(PointerInput::Pressed(button))
            );
            event.value = 0;
            assert_eq!(
                PointerInput::from_device_event(&event),
                Some(PointerInput::Released(button))
            );
        }
    }

    /// One device scroll event per axis maps onto a single-axis record in the
    /// same units: no layer rounds a fine wheel's fraction of a detent away.
    #[test]
    fn device_scroll_maps_to_single_axis_scrolls_in_the_same_units() {
        let x = InputEvent {
            kind: InputEventKind::Scroll,
            reserved0: 0,
            code: AXIS_X,
            value: 2 * SCROLL_UNITS_PER_DETENT,
        };
        let y = InputEvent {
            kind: InputEventKind::Scroll,
            reserved0: 0,
            code: AXIS_Y,
            value: -15,
        };
        assert_eq!(
            PointerInput::from_device_event(&x),
            Some(PointerInput::Scrolled { dx: 240, dy: 0 })
        );
        assert_eq!(
            PointerInput::from_device_event(&y),
            Some(PointerInput::Scrolled { dx: 0, dy: -15 })
        );
    }

    /// Keyboard keys, unknown axes, out-of-set buttons, and repeat values
    /// produce no record (fail closed, never fabricate).
    #[test]
    fn device_events_outside_the_pointer_vocabulary_produce_nothing() {
        let cases = [
            // A keyboard key (`KEY_A` = 30) is the keyboard producer's job.
            InputEvent {
                kind: InputEventKind::Key,
                reserved0: 0,
                code: 30,
                value: 1,
            },
            // A code past the modelled button set (`BTN_SIDE` = 0x113).
            InputEvent {
                kind: InputEventKind::Key,
                reserved0: 0,
                code: POINTER_BUTTON_CODE_BASE + 3,
                value: 1,
            },
            // A button repeat carries no edge.
            InputEvent {
                kind: InputEventKind::Key,
                reserved0: 0,
                code: POINTER_BUTTON_CODE_BASE,
                value: 2,
            },
            // A pointer axis this vocabulary does not model.
            InputEvent {
                kind: InputEventKind::Pointer,
                reserved0: 0,
                code: 2,
                value: 1,
            },
            // A scroll axis this vocabulary does not model.
            InputEvent {
                kind: InputEventKind::Scroll,
                reserved0: 0,
                code: 2,
                value: -1,
            },
        ];
        for event in cases {
            assert_eq!(PointerInput::from_device_event(&event), None);
        }
    }

    #[test]
    fn button_code_rejects_unknown() {
        assert_eq!(PointerButtonCode::from_code(0), Err(Errno::OutOfRange));
        assert_eq!(PointerButtonCode::from_code(4), Err(Errno::OutOfRange));
    }

    #[test]
    fn known_constants_are_frozen() {
        assert_eq!(POINTER_INPUT_MAGIC, u32::from_le_bytes(*b"PIN1"));
        assert_eq!(KIND_MOVED_BY, 0);
        assert_eq!(KIND_PRESSED, 1);
        assert_eq!(KIND_RELEASED, 2);
        assert_eq!(KIND_SCROLLED, 3);
        assert_eq!(BUTTON_NONE, 0);
        assert_eq!(PointerButtonCode::Primary.code(), 1);
        assert_eq!(PointerButtonCode::Secondary.code(), 2);
        assert_eq!(PointerButtonCode::Middle.code(), 3);
        // The version stamped into a record matches the crate's current ABI.
        assert_eq!(
            u32::from(crate::ABI_VERSION_CURRENT_U16),
            ABI_VERSION_CURRENT
        );
    }

    #[test]
    fn key_wire_size_is_twenty() {
        assert_eq!(KeyInput::WIRE_LEN, 20);
    }

    /// A press of `A` typing `a`, with nothing held.
    fn a_pressed() -> KeyInput {
        KeyInput::Pressed {
            key: Some(KeyValue::Char('a')),
            modifiers: Modifiers::default(),
            physical: PhysicalKey::A,
        }
    }

    #[test]
    fn key_char_round_trips_with_modifiers() {
        let event = KeyInput::Pressed {
            key: Some(KeyValue::Char('Q')),
            modifiers: Modifiers {
                shift: true,
                ctrl: true,
                ..Modifiers::default()
            },
            physical: PhysicalKey::from_usage(0x14).expect("a key"),
        };
        let decoded = KeyInput::from_bytes(&event.to_le_bytes()).expect("valid record");
        assert_eq!(decoded, event);
        assert_eq!(decoded.key(), Some(KeyValue::Char('Q')));
        assert!(decoded.modifiers().shift && decoded.modifiers().ctrl);
        assert!(!decoded.modifiers().alt && !decoded.modifiers().meta);
        assert!(decoded.is_press());
    }

    #[test]
    fn key_named_round_trips_for_press_and_release() {
        for code in [
            NamedKeyCode::Enter,
            NamedKeyCode::Escape,
            NamedKeyCode::Left,
            NamedKeyCode::F12,
        ] {
            for event in [
                KeyInput::Pressed {
                    key: Some(KeyValue::Named(code)),
                    modifiers: Modifiers::default(),
                    physical: PhysicalKey::ESCAPE,
                },
                KeyInput::Released {
                    key: Some(KeyValue::Named(code)),
                    modifiers: Modifiers {
                        alt: true,
                        meta: true,
                        ..Modifiers::default()
                    },
                    physical: PhysicalKey::ESCAPE,
                },
            ] {
                let decoded = KeyInput::from_bytes(&event.to_le_bytes()).expect("valid record");
                assert_eq!(decoded, event);
            }
        }
    }

    #[test]
    fn key_char_carries_non_ascii_scalar() {
        let event = KeyInput::Pressed {
            key: Some(KeyValue::Char('é')),
            modifiers: Modifiers::default(),
            physical: PhysicalKey::A,
        };
        let decoded = KeyInput::from_bytes(&event.to_le_bytes()).expect("valid record");
        assert_eq!(decoded.key(), Some(KeyValue::Char('é')));
    }

    #[test]
    fn a_key_that_types_nothing_round_trips_and_names_its_key() {
        // A modifier, a lock and a key no layout names reach the seat as
        // edges like any other, typing nothing.
        let shift = Modifiers {
            shift: true,
            ..Modifiers::default()
        };
        for event in [
            KeyInput::Pressed {
                key: None,
                modifiers: shift,
                physical: PhysicalKey::LEFT_SHIFT,
            },
            KeyInput::Released {
                key: None,
                modifiers: Modifiers::default(),
                physical: PhysicalKey::LEFT_SHIFT,
            },
            KeyInput::Pressed {
                key: None,
                modifiers: Modifiers::default(),
                physical: PhysicalKey::CAPS_LOCK,
            },
        ] {
            let bytes = event.to_le_bytes();
            assert_eq!(read_u16(&bytes, 10), KEY_CLASS_NONE);
            let decoded = KeyInput::from_bytes(&bytes).expect("valid record");
            assert_eq!(decoded, event);
            assert_eq!(decoded.key(), None);
        }
    }

    #[test]
    fn a_key_that_types_nothing_rejects_a_dirty_value_field() {
        for offset in [12usize, 16] {
            let mut bytes = KeyInput::Pressed {
                key: None,
                modifiers: Modifiers::default(),
                physical: PhysicalKey::LEFT_CTRL,
            }
            .to_le_bytes();
            bytes[offset] = 1;
            assert_eq!(
                KeyInput::from_bytes(&bytes),
                Err(Errno::BadMagic),
                "value field at {offset} must be zero on a key that types nothing"
            );
        }
    }

    #[test]
    fn key_rejects_short_buffer() {
        assert_eq!(KeyInput::from_bytes(&[0u8; 8]), Err(Errno::BufferTooSmall));
    }

    #[test]
    fn key_rejects_bad_magic() {
        let mut bytes = a_pressed().to_le_bytes();
        bytes[0] ^= 0xFF;
        assert_eq!(KeyInput::from_bytes(&bytes), Err(Errno::BadMagic));
    }

    #[test]
    fn key_rejects_bad_version() {
        let mut bytes = a_pressed().to_le_bytes();
        bytes[4] = 99;
        assert_eq!(
            KeyInput::from_bytes(&bytes),
            Err(Errno::AbiVersionUnsupported)
        );
    }

    #[test]
    fn a_key_record_round_trips_its_physical_key() {
        let chord = Modifiers {
            shift: true,
            meta: true,
            ..Modifiers::default()
        };
        for event in [
            KeyInput::Pressed {
                key: Some(KeyValue::Char('#')),
                modifiers: chord,
                physical: PhysicalKey::DIGIT_3,
            },
            KeyInput::Released {
                key: Some(KeyValue::Named(NamedKeyCode::Escape)),
                modifiers: Modifiers::default(),
                physical: PhysicalKey::ESCAPE,
            },
        ] {
            let bytes = event.to_le_bytes();
            assert_eq!(read_u16(&bytes, 18), event.physical().usage());
            assert_eq!(KeyInput::from_bytes(&bytes), Ok(event));
        }
    }

    #[test]
    fn a_record_naming_no_key_is_refused() {
        // No event, `ErrorRollOver`, a reserved run, past Right GUI: a record
        // the seat made names the key it read.
        for usage in [0x0000u16, 0x0001, 0x00A5, 0x00DF, 0x00E8, 0xFFFF] {
            let mut bytes = a_pressed().to_le_bytes();
            bytes[18..20].copy_from_slice(&usage.to_le_bytes());
            assert_eq!(
                KeyInput::from_bytes(&bytes),
                Err(Errno::OutOfRange),
                "usage {usage:#06x}"
            );
        }
    }

    #[test]
    fn the_eight_modifier_keys_and_no_other_are_modifiers() {
        let modifiers = [
            PhysicalKey::LEFT_CTRL,
            PhysicalKey::LEFT_SHIFT,
            PhysicalKey::LEFT_ALT,
            PhysicalKey::LEFT_GUI,
            PhysicalKey::RIGHT_CTRL,
            PhysicalKey::RIGHT_SHIFT,
            PhysicalKey::RIGHT_ALT,
            PhysicalKey::RIGHT_GUI,
        ];
        for usage in 0..=u16::MAX {
            if let Some(key) = PhysicalKey::from_usage(usage) {
                assert_eq!(
                    key.is_modifier(),
                    modifiers.contains(&key),
                    "usage {usage:#06x}"
                );
            }
        }
    }

    #[test]
    fn a_physical_key_keeps_the_usages_the_page_assigns_a_key_and_no_other() {
        for usage in [0x04u16, 0x20, 0xA4, 0xB0, 0xDD, 0xE0, 0xE7] {
            assert_eq!(
                PhysicalKey::from_usage(usage).map(PhysicalKey::usage),
                Some(usage)
            );
        }
        for usage in [
            0x00u16, 0x01, 0x03, 0xA5, 0xAF, 0xDE, 0xDF, 0xE8, 0x0100, 0xFFFF,
        ] {
            assert_eq!(PhysicalKey::from_usage(usage), None, "usage {usage:#06x}");
        }
    }

    #[test]
    fn key_rejects_unknown_kind() {
        let mut bytes = a_pressed().to_le_bytes();
        bytes[6] = 9;
        assert_eq!(KeyInput::from_bytes(&bytes), Err(Errno::OutOfRange));
    }

    #[test]
    fn key_rejects_unknown_modifier_bit() {
        let mut bytes = a_pressed().to_le_bytes();
        // The high bits of the modifier field are not defined.
        bytes[8] = 0x10;
        assert_eq!(KeyInput::from_bytes(&bytes), Err(Errno::OutOfRange));
    }

    #[test]
    fn key_rejects_unknown_class() {
        let mut bytes = a_pressed().to_le_bytes();
        bytes[10] = 9;
        assert_eq!(KeyInput::from_bytes(&bytes), Err(Errno::OutOfRange));
    }

    #[test]
    fn key_rejects_named_code_on_char_record() {
        // A well-formed char record, then plant a named code in the unused
        // field: the two value fields must never both be set.
        let mut bytes = a_pressed().to_le_bytes();
        // Enter's wire code is 1.
        bytes[16] = 1;
        assert_eq!(KeyInput::from_bytes(&bytes), Err(Errno::BadMagic));
    }

    #[test]
    fn key_rejects_codepoint_on_named_record() {
        let mut bytes = KeyInput::Pressed {
            key: Some(KeyValue::Named(NamedKeyCode::Enter)),
            modifiers: Modifiers::default(),
            physical: PhysicalKey::from_usage(0x28).expect("a key"),
        }
        .to_le_bytes();
        bytes[12] = 1;
        assert_eq!(KeyInput::from_bytes(&bytes), Err(Errno::BadMagic));
    }

    #[test]
    fn key_rejects_unknown_named_code() {
        let mut bytes = KeyInput::Pressed {
            key: Some(KeyValue::Named(NamedKeyCode::Enter)),
            modifiers: Modifiers::default(),
            physical: PhysicalKey::from_usage(0x28).expect("a key"),
        }
        .to_le_bytes();
        bytes[16] = 99;
        assert_eq!(KeyInput::from_bytes(&bytes), Err(Errno::OutOfRange));
    }

    #[test]
    fn key_rejects_surrogate_codepoint() {
        // 0xD800 is an unpaired surrogate: not a valid Unicode scalar.
        let mut bytes = a_pressed().to_le_bytes();
        bytes[12] = 0x00;
        bytes[13] = 0xD8;
        assert_eq!(KeyInput::from_bytes(&bytes), Err(Errno::OutOfRange));
    }

    #[test]
    fn a_key_edge_round_trips_and_names_a_real_key() {
        assert_eq!(KeyEdge::WIRE_LEN, 12);
        for edge in [
            KeyEdge {
                key: PhysicalKey::A,
                pressed: true,
            },
            KeyEdge {
                key: PhysicalKey::RIGHT_GUI,
                pressed: false,
            },
            KeyEdge {
                key: PhysicalKey::CAPS_LOCK,
                pressed: true,
            },
        ] {
            let bytes = edge.to_le_bytes();
            assert_eq!(read_u16(&bytes, 8), edge.key.usage());
            assert_eq!(KeyEdge::from_bytes(&bytes), Ok(edge));
        }
        let pressed = KeyEdge {
            key: PhysicalKey::A,
            pressed: true,
        }
        .to_le_bytes();
        assert_eq!(
            KeyEdge::from_bytes(&pressed[..11]),
            Err(Errno::BufferTooSmall)
        );
        let mut bad = pressed;
        bad[0] ^= 1;
        assert_eq!(KeyEdge::from_bytes(&bad), Err(Errno::BadMagic));
        let mut reserved = pressed;
        reserved[11] = 1;
        assert_eq!(KeyEdge::from_bytes(&reserved), Err(Errno::BadMagic));
        let mut version = pressed;
        version[4] = 99;
        assert_eq!(
            KeyEdge::from_bytes(&version),
            Err(Errno::AbiVersionUnsupported)
        );
        let mut kind = pressed;
        kind[6] = 3;
        assert_eq!(KeyEdge::from_bytes(&kind), Err(Errno::OutOfRange));
        let mut unknown = pressed;
        unknown[8..10].copy_from_slice(&0x00A5u16.to_le_bytes());
        assert_eq!(KeyEdge::from_bytes(&unknown), Err(Errno::OutOfRange));
    }

    #[test]
    fn key_constants_are_frozen() {
        assert_eq!(KEY_INPUT_MAGIC, u32::from_le_bytes(*b"KIN1"));
        assert_eq!(KEY_EDGE_MAGIC, u32::from_le_bytes(*b"KED1"));
        assert_eq!(KIND_KEY_PRESSED, 1);
        assert_eq!(KIND_KEY_RELEASED, 2);
        assert_eq!((KEY_CLASS_NONE, KEY_CLASS_CHAR, KEY_CLASS_NAMED), (0, 1, 2));
        assert_eq!((MOD_SHIFT, MOD_CTRL, MOD_ALT, MOD_META), (1, 2, 4, 8));
        assert_eq!(NamedKeyCode::Enter.code(), 1);
        assert_eq!(NamedKeyCode::F12.code(), 26);
        assert_eq!(PhysicalKey::A.usage(), 0x04);
        assert_eq!(PhysicalKey::DIGIT_3.usage(), 0x20);
        assert_eq!(PhysicalKey::LEFT_GUI.usage(), 0xE3);
    }

    #[test]
    fn named_key_code_round_trips_every_code() {
        for code in 1..=26u16 {
            let named = NamedKeyCode::from_code(code).expect("a defined named key");
            assert_eq!(named.code(), code);
        }
        assert_eq!(NamedKeyCode::from_code(0), Err(Errno::OutOfRange));
        assert_eq!(NamedKeyCode::from_code(27), Err(Errno::OutOfRange));
    }
}
