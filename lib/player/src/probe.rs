//! A track's stream and tags, read in the parser sandbox without blocking.
//!
//! [`TagReader`] is a state machine its host feeds what it is woken by, as the
//! engine is: [`TagReader::begin`] a file, [`TagReader::pump`] after every
//! wake, park on [`TagReader::members`] no later than
//! [`TagReader::deadline`], and hand what fired to [`TagReader::on_ready`]. A
//! pump goes as far as what has arrived allows — every request queued to the
//! worker, every answer taken, every byte asked for read — so the worker is
//! never left waiting on a frame its session was not given.
//!
//! A file's budget runs from when its open reaches a live worker, so a
//! replacement the restart pacing holds back costs it time and never its tags;
//! a worker that cannot be launched leaves it unread. A worker that fails,
//! overruns the budget, or is left part-way through a request is not resumed:
//! its file is unread and the next is read by a replacement. A file that broke
//! a decoder once is not one to offer it again, and a worker abandoned
//! mid-request would answer the next file with this one's frames.

use alloc::vec::Vec;

use tairix_abi::Errno;
use tairix_hash::HashSeed;
use tairix_log::Sink;
use tairix_sandbox::audiodecode::{AudioDecodeClient, AudioDecodeError, DecodeEvent};
use tairix_sandbox::session::SessionDescriptors;
use tairix_sandbox::supervise::{send_waiting, Sent, SessionLauncher, SupervisedSession};
use tairix_sound::{Metadata, SoundInfo};

use crate::engine::fetch;

/// The longest a file's tags may take to read once a live decoder is asked for
/// them before it is given up as hung: generous for a slow disk, and a bound,
/// so one stuck file cannot hold every later one behind it.
pub const BUDGET_NS: u64 = 10_000_000_000;

/// How a reading ended.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Tags {
    /// The file's stream, and what it says about itself.
    Read(SoundInfo, Metadata),
    /// No decoder read it: the decoder refused it, its bytes could not be
    /// had, or no answer came within the budget.
    Unread,
}

/// The file being read.
struct Reading {
    len: u64,
    /// When its budget runs out, once its open has been asked of a live
    /// worker: no budget runs while none is.
    budget_ends: Option<u64>,
}

/// What one frame from the worker came to.
enum Step {
    Moved,
    Done(Tags),
    /// The worker cannot be believed or left as it is: contain it, and leave
    /// the file unread.
    Condemn(&'static str),
}

/// A track's stream and tags, read by one supervised decoder worker a file
/// at a time.
pub struct TagReader<L: SessionLauncher, S: Sink + Clone> {
    session: SupervisedSession<L, S>,
    client: AudioDecodeClient,
    seed: fn() -> Option<HashSeed>,
    reading: Option<Reading>,
    /// Where a need's bytes are read, kept from one need to the next.
    bytes: Vec<u8>,
}

impl<L: SessionLauncher, S: Sink + Clone> TagReader<L, S> {
    /// A reader over `session`'s workers, keying each file's page cache with
    /// a fresh draw from `seed`.
    pub fn new(session: SupervisedSession<L, S>, seed: fn() -> Option<HashSeed>) -> Self {
        Self {
            session,
            client: AudioDecodeClient::new(),
            seed,
            reading: None,
            bytes: Vec::new(),
        }
    }

    /// Read the tags of a file `len` bytes long, within [`BUDGET_NS`] of its
    /// open reaching a live decoder. A reading still under way is abandoned
    /// first, as [`Self::abandon`] abandons it.
    pub fn begin(&mut self, now: u64, len: u64) {
        self.abandon(now);
        self.reading = Some(Reading {
            len,
            budget_ends: None,
        });
    }

    /// Give up the reading under way. A worker part-way through a request
    /// would answer the next file with this one's frames, so it is killed and
    /// the next file is read by a replacement.
    pub fn abandon(&mut self, now: u64) {
        if self.reading.take().is_some() && !self.client.takes_request() {
            self.session.condemn(
                now,
                "a track's tags were given up part-way through a request",
            );
        }
    }

    /// When the reader next needs a turn with nothing else to wake it: the
    /// budget, or the instant a replacement worker may start.
    #[must_use]
    pub fn deadline(&self) -> Option<u64> {
        let reading = self.reading.as_ref()?;
        [self.session.restart_deadline(), reading.budget_ends]
            .into_iter()
            .flatten()
            .min()
    }

    /// The live worker's descriptors and which of them to watch.
    #[must_use]
    pub fn members(&self) -> (Option<SessionDescriptors>, bool, bool) {
        (
            self.session.descriptors(),
            self.session.wants_read(),
            self.session.wants_write(),
        )
    }

    /// The worker's descriptors were reported ready.
    pub fn on_ready(&mut self, now: u64, readable: bool, writable: bool) {
        if writable {
            let _ = self.session.on_writable(now);
        }
        if readable {
            let _ = self.session.on_readable(now);
        }
    }

    /// Make every step the reading can make at `now`, reading the bytes the
    /// worker asks for through `read`, and answer how it ended once it has.
    ///
    /// It returns only when nothing more can move, every request queued to
    /// the worker. `read` is file I/O, so an interactive host pumps off its
    /// loop; the worker's pipe is read only on the wait-set's word
    /// ([`Self::on_ready`]), since a read of an empty pipe parks.
    #[must_use]
    pub fn pump(
        &mut self,
        now: u64,
        read: &mut impl FnMut(u64, &mut [u8]) -> Result<usize, Errno>,
    ) -> Option<Tags> {
        loop {
            let reading = self.reading.as_mut()?;
            if reading.budget_ends.is_some_and(|ends| now >= ends) {
                self.session
                    .condemn(now, "a track's tags were not read in the time allowed");
                return self.finish(Tags::Unread);
            }
            if !self.session.is_live() {
                if reading.budget_ends.is_some() {
                    return self.finish(Tags::Unread);
                }
                // A start the pacing holds back costs the file time; one that
                // fails leaves no decoder to wait for.
                let due = self.session.restart_deadline().is_some_and(|at| at <= now);
                if self.session.start(now).is_none() {
                    return if due { self.finish(Tags::Unread) } else { None };
                }
                self.client = AudioDecodeClient::new();
            }
            if reading.budget_ends.is_none() {
                let Some(seed) = (self.seed)() else {
                    return self.finish(Tags::Unread);
                };
                match self.client.open(reading.len, None, seed) {
                    Ok(()) => reading.budget_ends = Some(now.saturating_add(BUDGET_NS)),
                    // That worker would answer this file with another's
                    // frames.
                    Err(AudioDecodeError::OutOfTurn) => {
                        self.session
                            .condemn(now, "the decoder was left mid-request");
                        continue;
                    }
                    Err(_) => return self.finish(Tags::Unread),
                }
            }
            let mut moved = match send_waiting(&mut self.client, &mut self.session, now) {
                Sent::Queued => true,
                Sent::Nothing => false,
                Sent::Lost => return self.finish(Tags::Unread),
            };
            loop {
                match self.take(now, read) {
                    Ok(None) => break,
                    Ok(Some(Step::Moved)) => moved = true,
                    Ok(Some(Step::Done(tags))) => return self.finish(tags),
                    Ok(Some(Step::Condemn(reason))) => {
                        self.session.condemn(now, reason);
                        return self.finish(Tags::Unread);
                    }
                    Err(()) => return self.finish(Tags::Unread),
                }
            }
            if !moved {
                return None;
            }
        }
    }

    /// Take one frame the worker sent, if one is waiting, serving a need for
    /// bytes through `read`.
    fn take(
        &mut self,
        now: u64,
        read: &mut impl FnMut(u64, &mut [u8]) -> Result<usize, Errno>,
    ) -> Result<Option<Step>, ()> {
        let Self {
            session,
            client,
            bytes,
            ..
        } = self;
        session
            .recv(now, |frame| match client.on_frame(frame) {
                Ok(event) => step(client, bytes, read, event),
                Err(_) => Step::Condemn("the decoder sent an answer it could not have meant"),
            })
            .map_err(|_| ())
    }

    /// End the reading with `tags`.
    fn finish(&mut self, tags: Tags) -> Option<Tags> {
        self.reading.take().map(|_| tags)
    }
}

/// What `event` comes to: a need served from `read` into `bytes`, or the
/// reading's end.
fn step(
    client: &mut AudioDecodeClient,
    bytes: &mut Vec<u8>,
    read: &mut impl FnMut(u64, &mut [u8]) -> Result<usize, Errno>,
    event: DecodeEvent<'_>,
) -> Step {
    match event {
        // Bytes that cannot be had are the worker's to refuse, which keeps it
        // in step for the next file.
        DecodeEvent::Need { offset, len } => {
            let answered = match fetch(bytes, read, offset, len) {
                Ok(held) => client.supply(held),
                Err(_) => client.unreadable(),
            };
            match answered {
                Ok(()) => Step::Moved,
                Err(_) => Step::Condemn("a request for bytes could not be answered"),
            }
        }
        DecodeEvent::Opened => match (client.info(), client.metadata()) {
            (Some(info), Some(metadata)) => Step::Done(Tags::Read(*info, metadata.clone())),
            _ => Step::Condemn("the decoder opened a file it did not describe"),
        },
        DecodeEvent::Refused(_) => Step::Done(Tags::Unread),
        DecodeEvent::LastLink
        | DecodeEvent::Block { .. }
        | DecodeEvent::Ended { .. }
        | DecodeEvent::Sought { .. }
        | DecodeEvent::Resumed { .. }
        | DecodeEvent::Resuming => {
            Step::Condemn("the decoder answered a request it was never sent")
        }
    }
}

#[cfg(test)]
#[path = "probe_tests.rs"]
mod tests;
