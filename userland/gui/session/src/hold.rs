//! Which windows are held modal, and by what: the one rule an alert and an
//! owner-modal dialog share (`plans/APPWIN.md`).
//!
//! A held window takes no new input — only the release of a press it already
//! saw — a press on it hands the keyboard to its holder, and it takes no drop.

use alloc::vec::Vec;

use tairix_abi::input::KeyInput;
use tairix_abi::window_ipc::{PointerAction, WindowEvent};
use tairix_wm::WindowId;

/// Every hold up, in the order taken.
///
/// Holders of one window are its family's transients that keep their order,
/// so the last taken is the one in front. Bounded by what may hold — each
/// application's alerts and its dialogs, both bounded by the window server —
/// so a scan is the right structure for so few.
#[derive(Default)]
pub struct Holds {
    up: Vec<Hold>,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct Hold {
    held: WindowId,
    by: WindowId,
}

impl Holds {
    /// An empty set.
    #[must_use]
    pub const fn new() -> Self {
        Self { up: Vec::new() }
    }

    /// `by` holds `held` from now until it is released.
    pub fn take(&mut self, held: WindowId, by: WindowId) {
        self.up.push(Hold { held, by });
    }

    /// The holder in front of `held`, if anything holds it.
    #[must_use]
    pub fn holder(&self, held: WindowId) -> Option<WindowId> {
        self.up
            .iter()
            .rev()
            .find(|hold| hold.held == held)
            .map(|hold| hold.by)
    }

    /// Where the keyboard goes to reach `window`: the holder in front of it,
    /// else the window itself.
    #[must_use]
    pub fn front(&self, window: WindowId) -> WindowId {
        self.holder(window).unwrap_or(window)
    }

    /// Release the hold `by` took, answering where the keyboard goes if `by`
    /// held it: the front of the window it held. `None` when `by` held
    /// nothing.
    pub fn release(&mut self, by: WindowId) -> Option<WindowId> {
        let index = self.up.iter().position(|hold| hold.by == by)?;
        let held = self.up.remove(index).held;
        Some(self.front(held))
    }

    /// Forget window `gone`: every hold on it and any it took.
    pub fn forget(&mut self, gone: WindowId) {
        self.up.retain(|hold| hold.held != gone && hold.by != gone);
    }
}

/// Whether `event` still reaches a held window: nothing that starts anything
/// does — a press, a motion, a key, a scroll, a pinch — but a release or a
/// cancel does, so a press the window saw before it was held is not left down
/// when the hold ends.
#[must_use]
pub const fn reaches_held_window(event: &WindowEvent) -> bool {
    match event {
        WindowEvent::Pointer {
            action: PointerAction::Released(_) | PointerAction::Cancelled,
            ..
        }
        | WindowEvent::Key {
            key: KeyInput::Released { .. },
            ..
        } => true,
        WindowEvent::Pointer { .. }
        | WindowEvent::Key { .. }
        | WindowEvent::Scrolled { .. }
        | WindowEvent::Pinch { .. } => false,
        _ => true,
    }
}

/// Whether `event` is a press — a button or a key going down — which a dialog
/// ignores until it has been seen a moment, so a press meant for what was
/// there before it appeared cannot answer it.
#[must_use]
pub const fn is_press(event: &WindowEvent) -> bool {
    matches!(
        event,
        WindowEvent::Pointer {
            action: PointerAction::Pressed(_),
            ..
        } | WindowEvent::Key {
            key: KeyInput::Pressed { .. },
            ..
        }
    )
}

#[cfg(test)]
#[path = "hold_tests.rs"]
mod tests;
