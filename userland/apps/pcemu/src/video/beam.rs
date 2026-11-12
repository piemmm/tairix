//! A 6845 card's beam: the controller, the character clock it runs on, and
//! how far it has run.

use super::crtc6845::{Crtc, Tick};
use crate::time::{Clock, Hertz, Nanos};

/// The controller and its clock.
#[derive(Clone, Debug)]
pub struct Beam {
    /// The controller.
    pub crtc: Crtc,
    clock: Clock,
    done: u64,
}

impl Beam {
    /// A beam clocked at `rate` character clocks a second.
    #[must_use]
    pub fn new(rate: Hertz) -> Self {
        Self {
            crtc: Crtc::new(),
            clock: Clock::new(rate),
            done: 0,
        }
    }

    /// Runs every character clock due by `t`, handing each to `emit`.
    pub fn run_to(&mut self, t: Nanos, mut emit: impl FnMut(Tick)) {
        let target = self.clock.ticks_by(t);
        while self.done < target {
            emit(self.crtc.step());
            self.done += 1;
        }
    }

    /// The emulated time the beam has run to.
    #[must_use]
    pub fn drawn_to(&self) -> Nanos {
        self.clock.time_of(self.done)
    }

    /// Clocks the controller at `rate` from here on, as a mode change that
    /// selects another dot clock or divider does.
    pub fn set_rate(&mut self, rate: Hertz) {
        if rate != self.clock.rate() {
            self.clock.set_rate(rate, self.done);
        }
    }
}

crate::snapshot::state!(Beam { crtc, clock, done });
