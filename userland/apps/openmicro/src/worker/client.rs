//! The owner's side: one request in flight at a time, and each reply
//! believed only once it answers what was asked within what that could
//! produce, the lamps and media among it the model's own. A reply that does
//! not condemns the worker.

use alloc::vec::Vec;

use tairix_sandbox::supervise::Requests;

use super::protocol::{Ran, Reply, Request, MAX_SAMPLES, SAMPLE_BYTES, SAMPLE_RATE};
use crate::machine::bbc::fdc::PhysicalDrive;
use crate::machine::{Model, CLOCK};

/// What was asked, as far as its answer depends on it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Asked {
    /// Answered by Done or a refusal.
    Done,
    /// A configure, whose Done makes its model the session's.
    Configure(Model),
    /// A slice of `cycles`, with its sound or without.
    Run {
        cycles: u32,
        sound: bool,
    },
    Snapshot,
    /// Media out of the drives marked, and the tape if `tape`.
    Media {
        drives: [bool; 2],
        tape: bool,
    },
}

impl Asked {
    const fn of(request: &Request<'_>) -> Self {
        match request {
            Request::Run { cycles, sound, .. } => Self::Run {
                cycles: *cycles,
                sound: *sound,
            },
            Request::TakeSnapshot => Self::Snapshot,
            Request::InsertDisc { drive, .. } | Request::EjectDisc(drive) => Self::Media {
                drives: [
                    matches!(drive, PhysicalDrive::Zero),
                    matches!(drive, PhysicalDrive::One),
                ],
                tape: false,
            },
            Request::CollectDiscs(drives) => Self::Media {
                drives: *drives,
                tape: false,
            },
            Request::InsertTape(_) | Request::EjectTape => Self::Media {
                drives: [false; 2],
                tape: true,
            },
            Request::RestoreSnapshot(_) => Self::Media {
                drives: [true; 2],
                tape: true,
            },
            Request::Configure(configuration) => Self::Configure(configuration.model()),
            Request::RewindTape | Request::RecordTape(_) | Request::PowerOn => Self::Done,
        }
    }
}

/// A request is already waiting for its answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Busy;

/// The owner's half of a session with the machine worker.
#[derive(Debug, Default)]
pub struct MachineClient {
    /// The request waiting to be sent, and what it asks.
    waiting: Option<(Vec<u8>, Asked)>,
    /// What the request sent and not yet answered asked.
    asked: Option<Asked>,
    /// The model the worker was configured as, once it said it was.
    model: Option<Model>,
}

impl MachineClient {
    /// A client with nothing asked.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            waiting: None,
            asked: None,
            model: None,
        }
    }

    /// Ask `request`.
    ///
    /// # Errors
    ///
    /// [`Busy`] while another request waits to be sent or answered.
    pub fn ask(&mut self, request: &Request<'_>) -> Result<(), Busy> {
        if !self.is_idle() {
            return Err(Busy);
        }
        self.waiting = Some((request.encode(), Asked::of(request)));
        Ok(())
    }

    /// Whether nothing is waiting to be sent or answered.
    #[must_use]
    pub const fn is_idle(&self) -> bool {
        self.waiting.is_none() && self.asked.is_none()
    }

    /// The reply `frame` holds, once it is believed.
    ///
    /// # Errors
    ///
    /// Why it is not: nothing was asked, it does not decode, it answers
    /// another request, or it carries more than its request could produce.
    /// The worker is to be condemned for it.
    pub fn answer<'f>(&mut self, frame: &'f [u8]) -> Result<Reply<'f>, &'static str> {
        let asked = self.asked.take().ok_or("a reply nothing asked for")?;
        let reply = Reply::decode(frame).map_err(|_| "a reply that does not decode")?;
        let drives_held = self.model.is_some_and(Model::has_drives);
        let believed = match (&reply, asked) {
            (Reply::Refused(_), _)
            | (Reply::Done, Asked::Done)
            | (Reply::Snapshot(_), Asked::Snapshot) => true,
            (Reply::Done, Asked::Configure(model)) => {
                self.model = Some(model);
                true
            }
            (Reply::Ran(ran), Asked::Run { cycles, sound }) => {
                ran.cycles == cycles
                    && (sound || ran.sound.is_empty())
                    && ran.sound.len() <= most_samples(cycles) * SAMPLE_BYTES
                    && self.model.is_some_and(|model| lamps_fit(model, ran))
            }
            (Reply::Media(media), Asked::Media { drives, tape }) => {
                (tape || media.tape.is_none())
                    && (drives_held || media.discs.is_empty())
                    && media
                        .discs
                        .iter()
                        .all(|written| drives[written.drive.index()])
            }
            _ => false,
        };
        if believed {
            Ok(reply)
        } else {
            Err("a reply that does not answer what was asked")
        }
    }

    /// Forget what was asked: the worker is gone and its replacement
    /// starts from nothing.
    pub fn restart(&mut self) {
        self.waiting = None;
        self.asked = None;
        self.model = None;
    }
}

impl Requests for MachineClient {
    fn outgoing(&self) -> Option<&[u8]> {
        self.waiting.as_ref().map(|(frame, _)| frame.as_slice())
    }

    fn sent(&mut self) {
        if let Some((_, asked)) = self.waiting.take() {
            self.asked = Some(asked);
        }
    }
}

/// Whether `ran`'s lamps and media are ones `model` has: SHIFT LOCK's light
/// and the drives only where it has them.
fn lamps_fit(model: Model, ran: &Ran<'_>) -> bool {
    (model.has_shift_lock() || !ran.lights.shift_lock)
        && (model.has_drives() || (ran.drives == [false; 2] && ran.loaded.discs == [false; 2]))
}

/// The most samples `cycles` make at the output rate, a frame either way
/// for where the resampler's phase falls.
fn most_samples(cycles: u32) -> usize {
    let samples = u64::from(cycles) * u64::from(SAMPLE_RATE) / CLOCK + 2;
    usize::try_from(samples).map_or(MAX_SAMPLES, |samples| samples.min(MAX_SAMPLES))
}

#[cfg(test)]
#[path = "client_tests.rs"]
mod tests;
