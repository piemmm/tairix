//! A machine of any model, as the worker runs one: what the models do
//! alike, and each model's own machine for what is its alone.

use alloc::boxed::Box;
use alloc::vec::{Drain, Vec};

use super::canvas::DirtyRows;
use super::{bbc, c64, electron, LetterCase, Model, Timing};
use crate::media::{tap, tape};
use crate::snapshot::SnapshotError;

/// A machine of one of the models.
pub enum Machine {
    /// A BBC Micro Model B.
    ModelB(Box<bbc::Machine>),
    /// An Acorn Electron.
    Electron(Box<electron::Machine>),
    /// A Commodore 64.
    C64(Box<c64::Machine>),
}

/// A tape image the model's deck cannot take.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BadTape;

/// `$body` with `$machine` bound to the model's own machine.
macro_rules! each {
    ($self:expr, $machine:ident => $body:expr) => {
        match $self {
            Machine::ModelB($machine) => $body,
            Machine::Electron($machine) => $body,
            Machine::C64($machine) => $body,
        }
    };
}

/// `$acorn` with `$machine` bound to an Acorn model's machine, `$c64` with
/// it bound to a Commodore 64.
macro_rules! by_maker {
    ($self:expr, $machine:ident => $acorn:expr, $c64:expr) => {
        match $self {
            Machine::ModelB($machine) => $acorn,
            Machine::Electron($machine) => $acorn,
            Machine::C64($machine) => $c64,
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
            Self::C64(_) => Model::C64,
        }
    }

    /// How it keeps time.
    #[must_use]
    pub const fn timing(&self) -> Timing {
        match self {
            Self::ModelB(_) | Self::Electron(_) => Timing::ACORN,
            Self::C64(machine) => machine.timing(),
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

    /// Keep the sound for the front end to take, or not.
    pub fn set_sound_output(&mut self, on: bool) {
        each!(self, machine => machine.set_sound_output(on));
    }

    /// The sound since last asked, a sample every
    /// [`Timing::cycles_per_sample`] of its cycles.
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
        by_maker!(self, machine => machine.video().shown(), machine.shown())
    }

    /// Whether CAPS LOCK's and SHIFT LOCK's lights are lit; a model without
    /// one has no light for it.
    #[must_use]
    pub const fn lock_lights(&self) -> (bool, bool) {
        match self {
            Self::ModelB(machine) => machine.lock_leds(),
            Self::Electron(machine) => (machine.caps_lock_led(), false),
            Self::C64(_) => (false, false),
        }
    }

    /// How the letter keys type letters now: by CAPS LOCK, or a C64's
    /// character set.
    #[must_use]
    pub fn letter_case(&self) -> LetterCase {
        match self {
            Self::ModelB(_) | Self::Electron(_) if self.lock_lights().0 => LetterCase::Capitals,
            Self::ModelB(_) | Self::Electron(_) => LetterCase::Small,
            Self::C64(machine) => machine.letter_case(),
        }
    }

    /// Whether the deck holds a tape.
    #[must_use]
    pub const fn holds_tape(&self) -> bool {
        match self {
            Self::ModelB(machine) => machine.holds_tape(),
            Self::Electron(machine) => machine.holds_tape(),
            Self::C64(machine) => machine.datasette().tape().is_some(),
        }
    }

    /// Put the tape `image` holds in the deck, the one there dropped: a UEF
    /// or CSW image in an Acorn's, a TAP image in a C64's.
    ///
    /// # Errors
    ///
    /// [`BadTape`] for an image that does not decode, nothing changed.
    pub fn insert_tape(&mut self, image: &[u8]) -> Result<(), BadTape> {
        by_maker!(
            self,
            machine => {
                machine.insert_tape(tape::Tape::decode(image).map_err(|_| BadTape)?);
            },
            {
                machine.insert_tape(tap::decode(image).map_err(|_| BadTape)?);
            }
        );
        Ok(())
    }

    /// Take the tape out, dropping it.
    pub fn eject_tape(&mut self) {
        each!(self, machine => {
            machine.eject_tape();
        });
    }

    /// The deck's tape as the image it is handed over in, if it was
    /// recorded on: UEF from an Acorn's, TAP from a C64's.
    pub fn recorded_tape(&mut self) -> Option<Vec<u8>> {
        by_maker!(
            self,
            machine => machine
                .tape()
                .filter(|tape| tape.is_dirty())
                .map(tape::Tape::encode_uef),
            machine
                .tape()
                .filter(|tape| tape.is_dirty())
                .map(tap::Tape::encode)
        )
    }

    /// Move the deck's tape into `other`'s, a machine of the same model.
    pub fn move_tape(&mut self, other: &mut Self) {
        match (self, other) {
            (Self::ModelB(from), Self::ModelB(to)) => {
                if let Some(tape) = from.eject_tape() {
                    to.insert_tape(tape);
                }
            }
            (Self::Electron(from), Self::Electron(to)) => {
                if let Some(tape) = from.eject_tape() {
                    to.insert_tape(tape);
                }
            }
            (Self::C64(from), Self::C64(to)) => {
                if let Some(tape) = from.eject_tape() {
                    to.insert_tape(tape);
                }
            }
            _ => {}
        }
    }

    /// Wind the tape to its start.
    pub fn rewind_tape(&mut self) {
        each!(self, machine => machine.wind_tape(0));
    }

    /// Whether RECORD is down, and whether the tape records.
    #[must_use]
    pub fn tape_record(&self) -> (bool, bool) {
        by_maker!(
            self,
            machine => machine.tape_record(),
            {
                let deck = machine.datasette();
                (deck.record_down(), deck.recording())
            }
        )
    }

    /// Whether the tape moves past the head: the cassette motor runs, and
    /// on a C64, PLAY is down too.
    #[must_use]
    pub const fn tape_motor(&self) -> bool {
        match self {
            Self::ModelB(machine) => machine.tape_motor(),
            Self::Electron(machine) => machine.tape_motor(),
            Self::C64(machine) => machine.datasette().running(),
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
