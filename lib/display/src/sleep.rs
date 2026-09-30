//! Putting a presenter's display to sleep and waking it again: switched off
//! where the display can be, kept black by its presenter where it cannot.
//!
//! The desktop's screensaver and the graphical login screen both sleep a
//! display they own, so the three states, which refusal means what, and
//! which display is switched back on have one definition here.

use tairix_abi::driver::display::{Display, DisplayPower};
use tairix_abi::DriverError;

/// What asking a display to switch off came to.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum SwitchedOff {
    /// The display is off.
    Off,
    /// The display has no power control of its own, so it is kept black and
    /// still in its place.
    Blanked,
    /// The display refused for this reason, so it is kept black and still in
    /// its place.
    Refused(DriverError),
}

/// Whether a presenter's display is awake, switched off, or kept black in
/// place of switching off.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct DisplaySleep {
    state: State,
}

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
enum State {
    #[default]
    Awake,
    /// Switched off: nothing presented can be seen until it wakes.
    Off,
    /// A display that would not switch off, kept black and still by its
    /// presenter instead.
    Blanked,
}

impl DisplaySleep {
    /// A display that is awake.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            state: State::Awake,
        }
    }

    /// Whether the display is awake: neither switched off nor kept black.
    #[must_use]
    pub const fn is_awake(&self) -> bool {
        matches!(self.state, State::Awake)
    }

    /// Whether the display is switched off, so its presenter presents
    /// nothing: no frame sent to it could be seen.
    #[must_use]
    pub const fn is_off(&self) -> bool {
        matches!(self.state, State::Off)
    }

    /// Ask `display` to switch off, answering what came of it; `None` when it
    /// already sleeps.
    ///
    /// A display that cannot or will not switch off sleeps black in its place
    /// when `can_blank` — its presenter has something to keep black over it —
    /// and otherwise stays awake, its presenter carrying on as it was. With no
    /// `display` to ask there is no power control to use.
    pub fn switch_off(
        &mut self,
        display: Option<&mut dyn Display>,
        can_blank: bool,
    ) -> Option<SwitchedOff> {
        if !self.is_awake() {
            return None;
        }
        let answer = display.map_or(Err(DriverError::Unsupported), |display| {
            display.set_power(DisplayPower::Off)
        });
        let refusal = match answer {
            Ok(()) => {
                self.state = State::Off;
                return Some(SwitchedOff::Off);
            }
            Err(refusal) => refusal,
        };
        if can_blank {
            self.state = State::Blanked;
        }
        Some(match refusal {
            DriverError::Unsupported | DriverError::NotImplemented => SwitchedOff::Blanked,
            refusal => SwitchedOff::Refused(refusal),
        })
    }

    /// Wake the display, answering whether it slept: one switched off is
    /// switched back on first.
    ///
    /// With no `display` to ask — its presenter has handed the screen to
    /// another — the sleep simply ends: the display service lights a display
    /// for whoever owns it next.
    ///
    /// # Errors
    ///
    /// The display's refusal to switch back on. It stays off, so the next
    /// wake asks again rather than presenting to a screen nobody can see.
    pub fn wake(&mut self, display: Option<&mut dyn Display>) -> Result<bool, DriverError> {
        if self.is_off() {
            if let Some(display) = display {
                display.set_power(DisplayPower::On)?;
            }
        }
        let slept = !self.is_awake();
        self.state = State::Awake;
        Ok(slept)
    }
}

#[cfg(test)]
#[path = "sleep_tests.rs"]
mod tests;
