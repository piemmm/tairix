//! The front end's end of one machine's worker: its supervised session, the
//! session's pipes on the owner's wait-set, and the one request in flight.
//!
//! A worker lost — found reading, writing, or not believing what it sent —
//! leaves nothing asked and nothing watched, so its replacement is asked
//! afresh and no member is left naming a descriptor the next pipe may be
//! given.

use alloc::sync::Arc;
use alloc::vec::Vec;

use tairix_abi::Errno;
use tairix_log::Sink;
use tairix_sandbox::members::{MemberSet, Members};
use tairix_sandbox::session::Share;
use tairix_sandbox::supervise::{send_waiting, Sent, SessionLauncher, SupervisedSession};
use tairix_util::fallible;

use super::client::{Busy, MachineClient};
use super::protocol::Request;

/// The worker's pipe failed, or it ended.
const FAILED: &str = "its worker failed";
/// The worker was gone when a request was sent to it.
const UNREACHED: &str = "its worker could not be reached";
/// An answer could not be kept.
const NO_ROOM: &str = "there was no memory for its answer";

/// One machine's worker, as its window drives it.
pub struct Link<L: SessionLauncher, S: Sink + Clone, W: MemberSet> {
    // First, so its members leave the wait-set before the session closes the
    // descriptors they name.
    members: Members<W>,
    session: SupervisedSession<L, S>,
    client: MachineClient,
}

impl<L: SessionLauncher, S: Sink + Clone, W: MemberSet> Link<L, S, W> {
    /// The link over `session`, its pipes watched through `members`; no
    /// worker runs until [`Self::start`].
    #[must_use]
    pub const fn new(session: SupervisedSession<L, S>, members: Members<W>) -> Self {
        Self {
            members,
            session,
            client: MachineClient::new(),
        }
    }

    /// Start a worker where none is live and the pacing allows one at `now`,
    /// answering its generation; it is asked nothing yet.
    pub fn start(&mut self, now: u64) -> Option<u64> {
        self.session.start(now)
    }

    /// Whether a worker is live.
    #[must_use]
    pub fn is_live(&self) -> bool {
        self.session.is_live()
    }

    /// The region the live worker writes its picture into.
    #[must_use]
    pub fn share(&self) -> Option<&Arc<dyn Share>> {
        self.session.share()
    }

    /// When a replacement may be started, while no worker is live.
    #[must_use]
    pub fn restart_deadline(&self) -> Option<u64> {
        self.session.restart_deadline()
    }

    /// Whether nothing is waiting to be sent or answered.
    #[must_use]
    pub const fn is_idle(&self) -> bool {
        self.client.is_idle()
    }

    /// Ask `request`.
    ///
    /// # Errors
    ///
    /// [`Busy`] while another request waits to be sent or answered.
    pub fn ask(&mut self, request: &Request<'_>) -> Result<(), Busy> {
        self.client.ask(request)
    }

    /// Send the request waiting, then watch the pipes in the directions the
    /// session wants; why the worker was lost sending, if it was.
    ///
    /// # Errors
    ///
    /// The wait-set's refusal to watch a pipe: the worker cannot be heard,
    /// and its owner is to end it.
    pub fn sync(&mut self, now: u64) -> Result<Option<&'static str>, Errno> {
        let lost = match send_waiting(&mut self.client, &mut self.session, now) {
            Sent::Lost => Some(self.forget(UNREACHED)),
            Sent::Queued | Sent::Nothing => None,
        };
        self.members.sync(
            self.session.descriptors(),
            self.session.wants_read(),
            self.session.wants_write(),
        )?;
        Ok(lost)
    }

    /// Serve the pipes as `ready` says they are, each reply the worker sent
    /// believed and pushed onto `frames` in order; why the worker was lost, if
    /// it was. A read is made only on readiness, as it blocks.
    pub fn serve(
        &mut self,
        now: u64,
        (readable, writable): (bool, bool),
        frames: &mut Vec<Vec<u8>>,
    ) -> Option<&'static str> {
        let session = &mut self.session;
        let moved = (!writable || !session.wants_write() || session.on_writable(now).is_ok())
            && (!readable || !session.wants_read() || session.on_readable(now).is_ok());
        if !moved {
            return Some(self.forget(FAILED));
        }
        loop {
            let client = &mut self.client;
            let got = self.session.recv(now, |frame| {
                client.answer(frame).map(|_| fallible::copied(frame))
            });
            let why = match got {
                Ok(None) => return None,
                Ok(Some(Ok(Some(frame)))) => {
                    if fallible::push(frames, frame) {
                        continue;
                    }
                    NO_ROOM
                }
                Ok(Some(Ok(None))) => NO_ROOM,
                Ok(Some(Err(why))) => why,
                Err(_) => return Some(self.forget(FAILED)),
            };
            self.session.condemn(now, why);
            return Some(self.forget(why));
        }
    }

    /// Nothing asked of the worker that was lost `why`, and none of its pipes
    /// watched.
    fn forget(&mut self, why: &'static str) -> &'static str {
        self.client.restart();
        self.members.release();
        why
    }
}

#[cfg(test)]
#[path = "link_tests.rs"]
mod tests;
