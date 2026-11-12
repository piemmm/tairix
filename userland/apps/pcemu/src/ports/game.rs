//! The game port: the IBM game control adapter's four one-shots, timed by
//! two joysticks' potentiometers, and their four buttons.
//!
//! A write fires every one-shot that has run out; each holds its bit high
//! for 24.2 µs plus 0.011 µs an ohm of its potentiometer's 0 to 100 kΩ, as
//! IBM's technical reference gives it, so an axis is read by counting until
//! its bit falls. An axis with nothing plugged in never runs out. A button
//! reads low while held.

use crate::time::Nanos;

/// The port.
pub const PORT: u16 = 0x201;
/// The one-shots' bits; the buttons' are above them.
pub const AXES: u8 = 0x0F;

/// A one-shot's time at no resistance, and how much longer each ohm makes
/// it.
const FIXED: Nanos = 24_200;
const PER_OHM: Nanos = 11;
/// A potentiometer's whole travel, in ohms.
const TRAVEL: u64 = 100_000;

/// Two joysticks: where their axes stand and which buttons are held.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Sticks {
    /// Each axis's position from 0 to 65 535 — joystick A's X and Y, then
    /// B's — or nothing plugged in.
    pub axes: [Option<u16>; 4],
    /// The buttons held, A's two then B's, in bits 0 to 3.
    pub buttons: u8,
}

/// A one-shot's output.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Shot {
    #[default]
    Idle,
    Until(Nanos),
    /// Fired with no potentiometer to charge it.
    Stuck,
}

impl Shot {
    fn high(self, t: Nanos) -> bool {
        match self {
            Self::Idle => false,
            Self::Until(end) => t < end,
            Self::Stuck => true,
        }
    }
}

/// The adapter; by default nothing is plugged in and its one-shots idle.
#[derive(Clone, Copy, Debug, Default)]
pub struct GamePort {
    sticks: Sticks,
    shots: [Shot; 4],
}

impl GamePort {
    /// The joysticks stand as `sticks` from now on; a one-shot already
    /// running keeps the time it was fired with.
    pub fn set(&mut self, sticks: Sticks) {
        self.sticks = sticks;
    }

    /// Reads the port at `t`: the one-shots in bits 0 to 3, the buttons
    /// inverted above them.
    #[must_use]
    pub fn input(&self, t: Nanos) -> u8 {
        let axes = self
            .shots
            .iter()
            .enumerate()
            .fold(0, |bits, (n, shot)| bits | (u8::from(shot.high(t)) << n));
        axes | (!self.sticks.buttons << 4)
    }

    /// A write at `t` fires each one-shot that has run out.
    pub fn output(&mut self, t: Nanos) {
        for (shot, axis) in self.shots.iter_mut().zip(self.sticks.axes) {
            if !shot.high(t) {
                *shot = axis.map_or(Shot::Stuck, |position| {
                    Shot::Until(t + charge_time(position))
                });
            }
        }
    }
}

/// How long a one-shot runs with its potentiometer at `position`.
fn charge_time(position: u16) -> Nanos {
    let ohms = u64::from(position) * TRAVEL / u64::from(u16::MAX);
    FIXED + ohms * PER_OHM
}

crate::snapshot::state!(Sticks { axes, buttons });

crate::snapshot::state_enum!(Shot {
    0 => Idle,
    1 => Until(at),
    2 => Stuck,
});

crate::snapshot::state!(GamePort { sticks, shots });

#[cfg(test)]
#[path = "game_tests.rs"]
mod tests;
