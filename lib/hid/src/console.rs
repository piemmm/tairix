//! The keyboard's console producer: a key edge, named by its HID usage — the
//! physical key itself — resolved through the shared `lib/input` keyboard
//! state, and emitted as the [`KeyInput`] record the seat routes
//! (`plans/PI.md` P11).
//!
//! The layout and the modifier and lock keys are `lib/input`'s; the
//! key-to-[`KeyInput`] map is `lib/keymap`'s. An unknown usage or a non-key
//! event produces nothing.

use tairix_abi::driver::input::InputEvent;
use tairix_abi::input::{KeyInput, PhysicalKey};
use tairix_input::KeyboardState;
use tairix_keymap::key_record;

/// Key edges, each an [`InputEvent`] naming a HID usage, resolved into
/// [`KeyInput`] records.
///
/// The only state carried between [`feed`](Self::feed) calls is the
/// keyboard's: the held modifiers and the two lock toggles.
#[derive(Debug, Default)]
pub struct KeyboardConsole {
    keyboard: KeyboardState,
}

impl KeyboardConsole {
    /// A producer with no keys held and both locks off.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            keyboard: KeyboardState::new(),
        }
    }

    /// Feed one decoded keyboard [`InputEvent`], returning the
    /// [`KeyInput`] record its key edge resolves to, or [`None`] when the
    /// edge produces no record.
    ///
    /// A modifier edge that changes the *observable* set of held modifiers
    /// produces a [`KeyInput::ModifiersChanged`] record — the desktop needs it
    /// to qualify a gesture that is not a key (a shift-click) — while one that
    /// does not (a repeat, or letting go of one shift key while the other is
    /// held) produces nothing. The lock keys update the internal state and
    /// produce no record. A *press* or *release* of a printable or named
    /// key produces the corresponding [`KeyInput::Pressed`] /
    /// [`KeyInput::Released`] record carrying the key the layout made of it,
    /// the modifiers held, and the usage as its physical key; an unknown usage
    /// or a non-keyboard event produces nothing. Both edges are emitted so the
    /// desktop sees key-up as well as key-down; the text path ignores releases
    /// in the kernel arbiter.
    ///
    /// # Capabilities
    ///
    /// None (the producer holds no authority; delivery is the sink's).
    pub fn feed(&mut self, event: InputEvent) -> Option<KeyInput> {
        let pressed = event.key_edge()?;
        key_record(
            &mut self.keyboard,
            PhysicalKey::from_usage(event.code),
            pressed,
        )
    }
}

#[cfg(test)]
mod tests;
