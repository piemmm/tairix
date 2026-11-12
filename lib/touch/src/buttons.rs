//! A touch surface's own buttons: which went down or came up between two
//! frames, and the press each made, so its release undoes exactly that press.

use tairix_abi::input::PointerButtonCode;
use tairix_abi::touch::TouchButtons;

use crate::{Gesture, TouchPress};

/// The physical button bits, in the order their edges are read.
const BUTTONS: [(u8, PointerButtonCode); 3] = [
    (TouchButtons::PRIMARY, PointerButtonCode::Primary),
    (TouchButtons::SECONDARY, PointerButtonCode::Secondary),
    (TouchButtons::MIDDLE, PointerButtonCode::Middle),
];

/// The buttons that went down and came up between two frames.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct Edges {
    rising: u8,
    falling: u8,
}

impl Edges {
    /// Whether a button went down.
    pub(crate) const fn pressed(self) -> bool {
        self.rising != 0
    }
}

/// A surface's buttons as its last frame stated them, and the press each one
/// held down made.
#[derive(Clone, Copy, Debug)]
pub(crate) struct DeviceButtons {
    held: TouchButtons,
    pressed: [Option<TouchPress>; 3],
}

impl DeviceButtons {
    pub(crate) const fn new() -> Self {
        Self {
            held: TouchButtons::NONE,
            pressed: [None; 3],
        }
    }

    /// Take the buttons a frame states, answering what changed since the
    /// last.
    pub(crate) fn adopt(&mut self, now: TouchButtons) -> Edges {
        let edges = Edges {
            rising: now.bits() & !self.held.bits(),
            falling: self.held.bits() & !now.bits(),
        };
        self.held = now;
        edges
    }

    /// Whether any button is down.
    pub(crate) fn any_held(&self) -> bool {
        self.held != TouchButtons::NONE
    }

    /// Report `edges`: every release first, each undoing the press its button
    /// made, then every press as `meaning` names it for its bit and code.
    pub(crate) fn report(
        &mut self,
        edges: Edges,
        meaning: impl Fn(u8, PointerButtonCode) -> TouchPress,
        out: &mut dyn FnMut(Gesture),
    ) {
        for (index, (bit, _)) in BUTTONS.iter().enumerate() {
            if edges.falling & bit != 0 {
                if let Some(press) = self.pressed[index].take() {
                    out(Gesture::Released(press));
                }
            }
        }
        for (index, &(bit, code)) in BUTTONS.iter().enumerate() {
            if edges.rising & bit == 0 {
                continue;
            }
            let press = meaning(bit, code);
            self.pressed[index] = Some(press);
            out(Gesture::Pressed(press));
        }
    }

    /// Release every press still down and forget every button.
    pub(crate) fn end(&mut self, out: &mut dyn FnMut(Gesture)) {
        for press in self.pressed.iter_mut().filter_map(Option::take) {
            out(Gesture::Released(press));
        }
        *self = Self::new();
    }

    /// Whether no press is down.
    pub(crate) fn is_idle(&self) -> bool {
        self.pressed.iter().all(Option::is_none)
    }
}
