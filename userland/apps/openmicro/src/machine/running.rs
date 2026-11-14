//! A machine of either model, as the worker runs one: what the models do
//! alike, and each model's own machine for what is its alone.

use alloc::boxed::Box;
use alloc::vec::{Drain, Vec};

use super::canvas::DirtyRows;
use super::{bbc, electron, Model};
use crate::media::tape::Tape;
use crate::snapshot::SnapshotError;

/// A machine of one of the models.
pub enum Machine {
    /// A BBC Micro Model B.
    ModelB(Box<bbc::Machine>),
    /// An Acorn Electron.
    Electron(Box<electron::Machine>),
}

/// `$body` with `$machine` bound to the model's own machine.
macro_rules! each {
    ($self:expr, $machine:ident => $body:expr) => {
        match $self {
            Machine::ModelB($machine) => $body,
            Machine::Electron($machine) => $body,
        }
    };
}

impl Machine {
    /// Its model.
    #[must_use]
    pub const fn model(&self) -> Model {
        match self {
            Self::ModelB(_) => Model::ModelB,
            Self::Electron(_) => Model::Electron,
        }
    }

    /// Cycles since power-on.
    #[must_use]
    pub fn cycles(&self) -> u64 {
        each!(self, machine => machine.cycles())
    }

    /// Run to the first instruction boundary at or past `until`.
    pub fn run_until(&mut self, until: u64) {
        each!(self, machine => machine.run_until(until));
    }

    /// Hold or release BREAK.
    pub fn set_break(&mut self, down: bool) {
        each!(self, machine => machine.set_break(down));
    }

    /// Keep the sound for the front end to take, or not.
    pub fn set_sound_output(&mut self, on: bool) {
        each!(self, machine => machine.set_sound_output(on));
    }

    /// The sound since last asked, at [`super::SOUND_RATE`].
    pub fn take_audio(&mut self) -> Drain<'_, f32> {
        each!(self, machine => machine.take_audio())
    }

    /// The rows of the finished frame that changed since last asked.
    pub fn take_dirty_rows(&mut self) -> DirtyRows {
        each!(self, machine => machine.take_dirty_rows())
    }

    /// The last finished frame.
    #[must_use]
    pub fn shown(&self) -> &[u8] {
        each!(self, machine => machine.video().shown())
    }

    /// Whether CAPS LOCK's and SHIFT LOCK's lights are lit; a model without
    /// SHIFT LOCK has no light for it.
    #[must_use]
    pub const fn lock_lights(&self) -> (bool, bool) {
        match self {
            Self::ModelB(machine) => machine.lock_leds(),
            Self::Electron(machine) => (machine.caps_lock_led(), false),
        }
    }

    /// Put `tape` in the deck, handing back the one it replaces.
    pub fn insert_tape(&mut self, tape: Tape) -> Option<Tape> {
        each!(self, machine => machine.insert_tape(tape))
    }

    /// Take the tape out.
    pub fn eject_tape(&mut self) -> Option<Tape> {
        each!(self, machine => machine.eject_tape())
    }

    /// Whether the deck holds a tape.
    #[must_use]
    pub const fn holds_tape(&self) -> bool {
        match self {
            Self::ModelB(machine) => machine.holds_tape(),
            Self::Electron(machine) => machine.holds_tape(),
        }
    }

    /// The tape, brought up to the machine's time.
    pub fn tape(&mut self) -> Option<&Tape> {
        each!(self, machine => machine.tape())
    }

    /// Wind the tape to `position`.
    pub fn wind_tape(&mut self, position: u64) {
        each!(self, machine => machine.wind_tape(position));
    }

    /// Press or release RECORD.
    pub fn set_tape_record(&mut self, record: bool) {
        each!(self, machine => machine.set_tape_record(record));
    }

    /// Whether RECORD is down, and whether the tape records.
    #[must_use]
    pub fn tape_record(&self) -> (bool, bool) {
        each!(self, machine => machine.tape_record())
    }

    /// Whether the cassette motor runs.
    #[must_use]
    pub const fn tape_motor(&self) -> bool {
        match self {
            Self::ModelB(machine) => machine.tape_motor(),
            Self::Electron(machine) => machine.tape_motor(),
        }
    }

    /// A snapshot of the machine as it stands.
    ///
    /// # Errors
    ///
    /// The model's [`SnapshotError`].
    pub fn snapshot(&mut self) -> Result<Vec<u8>, SnapshotError> {
        each!(self, machine => machine.snapshot())
    }

    /// Take `snapshot` back, nothing changed unless all of it restores onto
    /// this model and fitting.
    ///
    /// # Errors
    ///
    /// The model's [`SnapshotError`].
    pub fn restore(&mut self, snapshot: &[u8]) -> Result<(), SnapshotError> {
        each!(self, machine => machine.restore(snapshot))
    }
}

#[cfg(test)]
#[path = "running_tests.rs"]
mod tests;
