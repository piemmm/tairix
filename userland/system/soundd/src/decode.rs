//! One sound decoded whole in the parser sandbox.
//!
//! The file's bytes are read before the decode begins, so the worker is handed
//! exactly the pages it asks for and reaches nothing else. A sound is bounded
//! in length as well as in bytes, because a FLAC holds a minute of silence in
//! a few hundred: the decode stops at the cue's sample bound and the sound is
//! refused. A worker that crashes, breaks the protocol or overruns its budget
//! refuses its sound and is replaced for the next one rather than resumed — a
//! file that crashed the decoder once is not one to offer it again.

use alloc::vec::Vec;

use tairix_abi::driver::audio::{ChannelMap, Rate, SampleFormat};
use tairix_abi::Errno;
use tairix_hash::HashSeed;
use tairix_log::Sink;
use tairix_sandbox::audiodecode::{AudioDecodeClient, DecodeEvent, MAX_BLOCK_FRAMES};
use tairix_sandbox::session::{SessionDescriptors, SessionError};
use tairix_sandbox::supervise::{SessionLauncher, SupervisedSession};
use tairix_sound::{SoundFormat, SoundInfo};
use tairix_soundtheme::{MAX_CUE_SAMPLES, MAX_CUE_SECONDS};

use crate::pcm::CuePcm;

/// How long one sound may take to decode. A shipped cue decodes in
/// milliseconds on the slowest core TAIRiX targets; this bounds what a hostile
/// file can make the cues queued behind it wait.
pub const DECODE_BUDGET_NS: u64 = 5_000_000_000;

/// How a decode ended.
#[derive(Debug, Eq, PartialEq)]
pub enum Decoded {
    /// The whole sound.
    Pcm(CuePcm),
    /// The sound itself could not be decoded within its bounds: it is not one
    /// to decode again until it changes.
    Refused,
    /// The machine could not decode it this time, for the reason given; the
    /// sound itself was never judged.
    Failed(Errno),
}

/// Where a decode has got to.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Stage {
    /// Waiting for a worker.
    Unopened,
    /// The open is outstanding.
    Opening,
    /// Blocks are being taken.
    Decoding,
    /// A chained file's next link is asked for.
    Linking,
}

struct Job {
    ticket: u64,
    bytes: Vec<u8>,
    format: Option<SoundFormat>,
    stage: Stage,
    pcm: Vec<u8>,
    /// Bytes the sound may decode to, once its shape is known.
    room: usize,
    /// The sample format, rate and channels its first link states, which
    /// every link of a chained cue must share.
    shape: Option<(SampleFormat, Rate, ChannelMap)>,
    deadline: u64,
}

/// The decoder: one supervised worker, one sound at a time.
pub struct Decoder<L: SessionLauncher, S: Sink + Clone> {
    session: SupervisedSession<L, S>,
    client: AudioDecodeClient,
    job: Option<Job>,
    seed: fn() -> Option<HashSeed>,
}

/// What one frame from the worker meant for the job.
enum Step {
    Continue,
    Done(Decoded),
    /// The worker broke the protocol: contain it, and refuse the sound.
    Condemn(&'static str),
}

impl<L: SessionLauncher, S: Sink + Clone> Decoder<L, S> {
    /// A decoder over `session`'s workers, keying each worker's cache with a
    /// fresh `seed`.
    pub fn new(session: SupervisedSession<L, S>, seed: fn() -> Option<HashSeed>) -> Self {
        Self {
            session,
            client: AudioDecodeClient::new(),
            job: None,
            seed,
        }
    }

    /// Whether a sound is being decoded.
    #[must_use]
    pub const fn is_busy(&self) -> bool {
        self.job.is_some()
    }

    /// Decode `bytes`, a file in `format` — or, for `None`, whichever format
    /// the worker finds it in — answering with `ticket`.
    ///
    /// # Errors
    ///
    /// The bytes back, when a sound is already being decoded.
    pub fn begin(
        &mut self,
        now: u64,
        ticket: u64,
        bytes: Vec<u8>,
        format: Option<SoundFormat>,
    ) -> Result<(), Vec<u8>> {
        if self.job.is_some() {
            return Err(bytes);
        }
        self.job = Some(Job {
            ticket,
            bytes,
            format,
            stage: Stage::Unopened,
            pcm: Vec::new(),
            room: 0,
            shape: None,
            deadline: now.saturating_add(DECODE_BUDGET_NS),
        });
        Ok(())
    }

    /// When the decoder next needs a turn with nothing else to wake it: the
    /// job's budget, or the instant a replacement worker may start.
    #[must_use]
    pub fn deadline(&self) -> Option<u64> {
        let job = self.job.as_ref()?;
        Some(
            self.session
                .restart_deadline()
                .map_or(job.deadline, |restart| restart.min(job.deadline)),
        )
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

    /// Make every step the decode can make at `now` from what the worker has
    /// already sent, answering the job's ticket and outcome once it ends.
    ///
    /// The worker's pipe is read only on the wait-set's word
    /// ([`Self::on_ready`]): a read of an empty pipe parks, and with a request
    /// still part-written the worker can never answer it.
    pub fn pump(&mut self, now: u64) -> Option<(u64, Decoded)> {
        loop {
            let job = self.job.as_mut()?;
            if now >= job.deadline {
                self.session
                    .condemn(now, "a cue's decode overran its budget");
                return self.finish(Decoded::Refused);
            }
            if !self.session.is_live() {
                if job.stage != Stage::Unopened {
                    return self.finish(Decoded::Refused);
                }
                self.session.start(now)?;
                self.client = AudioDecodeClient::new();
            }
            if job.stage == Stage::Unopened {
                let Some(seed) = (self.seed)() else {
                    return self.finish(Decoded::Failed(Errno::EntropyNotReady));
                };
                let len = u64::try_from(job.bytes.len()).unwrap_or(u64::MAX);
                if self.client.open(len, job.format, seed).is_err() {
                    // Only a request left outstanding refuses an open: that
                    // worker would answer this sound with another's frames.
                    self.session
                        .condemn(now, "the decoder was left mid-request");
                    continue;
                }
                job.stage = Stage::Opening;
            }
            let Ok(mut moved) = self.send(now) else {
                return self.finish(Decoded::Refused);
            };
            loop {
                match self.take(now) {
                    Ok(None) => break,
                    Ok(Some(Step::Continue)) => moved = true,
                    Ok(Some(Step::Done(decoded))) => return self.finish(decoded),
                    Ok(Some(Step::Condemn(reason))) => {
                        self.session.condemn(now, reason);
                        return self.finish(Decoded::Refused);
                    }
                    Err(()) => return self.finish(Decoded::Refused),
                }
            }
            if !moved {
                return None;
            }
        }
    }

    /// Queue the client's waiting frame to the worker and write what the pipe
    /// takes, answering whether one went.
    ///
    /// The write cannot park: a request goes only once the worker has
    /// answered the last, which it does only having read all of it, so the
    /// pipe is empty. What does not fit waits for the pipe's room.
    fn send(&mut self, now: u64) -> Result<bool, ()> {
        let Some(frame) = self.client.outgoing() else {
            return Ok(false);
        };
        match self.session.send(frame) {
            Ok(()) => {
                self.client.sent();
                self.session.on_writable(now).map_err(|_| ())?;
                Ok(true)
            }
            Err(SessionError::OutboundFull) => Ok(false),
            Err(SessionError::WorkerFailed) => Err(()),
            Err(_) => {
                self.session
                    .condemn(now, "a request to the decoder could not be framed");
                Err(())
            }
        }
    }

    /// Take one frame the worker sent, if one is waiting.
    fn take(&mut self, now: u64) -> Result<Option<Step>, ()> {
        let Self {
            session,
            client,
            job,
            ..
        } = self;
        let Some(job) = job.as_mut() else {
            return Ok(None);
        };
        session
            .recv(now, |frame| match client.on_frame(frame) {
                Ok(event) => Self::step(client, job, event),
                Err(_) => Step::Condemn("the decoder sent an answer out of turn"),
            })
            .map_err(|_| ())
    }

    /// What `event` means for `job`.
    fn step(client: &mut AudioDecodeClient, job: &mut Job, event: DecodeEvent<'_>) -> Step {
        match event {
            DecodeEvent::Need { offset, len } => {
                let wanted = usize::try_from(offset)
                    .ok()
                    .and_then(|start| job.bytes.get(start..start.checked_add(len)?));
                let answered = match wanted {
                    Some(bytes) => client.supply(bytes),
                    None => client.unreadable(),
                };
                match answered {
                    Ok(()) => Step::Continue,
                    Err(_) => Step::Condemn("the decoder asked for input out of turn"),
                }
            }
            DecodeEvent::Opened if job.stage == Stage::Linking => {
                // A cue is one sound of one shape: a link of another cannot
                // be part of it, and the links together keep to the bound.
                let (Some(info), Some(first)) = (client.info().copied(), job.shape) else {
                    return Step::Done(Decoded::Refused);
                };
                if (info.sample, info.rate, info.channels) != first {
                    return Step::Done(Decoded::Refused);
                }
                job.room = bound_frames(&info).saturating_mul(info.frame_bytes());
                job.stage = Stage::Decoding;
                Self::ask(client)
            }
            DecodeEvent::Opened => {
                let Some(info) = client.info().copied() else {
                    return Step::Done(Decoded::Refused);
                };
                let Some(room) = room_for(&info) else {
                    return Step::Done(Decoded::Refused);
                };
                // Only a length the stream declares is reserved up front; one
                // of unknown length grows as it decodes, within the bound.
                if info.frames.is_some() && job.pcm.try_reserve_exact(room).is_err() {
                    return Step::Done(Decoded::Failed(Errno::OutOfMemory));
                }
                job.room = room;
                job.shape = Some((info.sample, info.rate, info.channels));
                job.stage = Stage::Decoding;
                Self::ask(client)
            }
            DecodeEvent::Block { pcm, .. } => {
                if job.stage != Stage::Decoding || job.pcm.len() + pcm.len() > job.room {
                    return Step::Done(Decoded::Refused);
                }
                if job.pcm.try_reserve(pcm.len()).is_err() {
                    return Step::Done(Decoded::Failed(Errno::OutOfMemory));
                }
                job.pcm.extend_from_slice(pcm);
                Self::ask(client)
            }
            DecodeEvent::Ended { .. } => match client.next_link() {
                Ok(()) => {
                    job.stage = Stage::Linking;
                    Step::Continue
                }
                Err(_) => Step::Condemn("the decoder was not ready for the next link"),
            },
            DecodeEvent::LastLink => match (job.shape, job.pcm.is_empty()) {
                (Some((format, rate, channel_map)), false) => Step::Done(Decoded::Pcm(CuePcm {
                    format,
                    rate,
                    channel_map,
                    bytes: core::mem::take(&mut job.pcm),
                })),
                _ => Step::Done(Decoded::Refused),
            },
            DecodeEvent::Refused(_) => Step::Done(Decoded::Refused),
            DecodeEvent::Sought { .. } | DecodeEvent::Resumed { .. } | DecodeEvent::Resuming => {
                Step::Condemn("the decoder answered a request it was never sent")
            }
        }
    }

    /// Ask for the next block.
    fn ask(client: &mut AudioDecodeClient) -> Step {
        match client.decode(MAX_BLOCK_FRAMES) {
            Ok(()) => Step::Continue,
            Err(_) => Step::Condemn("the decoder was not ready for the next block"),
        }
    }

    /// End the job with `decoded`.
    fn finish(&mut self, decoded: Decoded) -> Option<(u64, Decoded)> {
        self.job.take().map(|job| (job.ticket, decoded))
    }
}

/// The most frames a cue shaped as `info` may hold: no more samples than a
/// cue may hold, and no longer than one may sound.
fn bound_frames(info: &SoundInfo) -> usize {
    let channels = usize::from(info.channels.channels());
    let seconds = usize::try_from(u64::from(MAX_CUE_SECONDS) * u64::from(info.rate.hz()))
        .unwrap_or(usize::MAX);
    (MAX_CUE_SAMPLES / channels.max(1)).min(seconds)
}

/// The bytes a sound shaped as `info` may decode to, within the cue bound,
/// or `None` when one that says how long it is says it is too long.
fn room_for(info: &SoundInfo) -> Option<usize> {
    let frames = bound_frames(info);
    if info
        .frames
        .is_some_and(|declared| declared > u64::try_from(frames).unwrap_or(u64::MAX))
    {
        return None;
    }
    let frames = info
        .frames
        .and_then(|declared| usize::try_from(declared).ok())
        .map_or(frames, |declared| declared.min(frames));
    frames.checked_mul(info.frame_bytes())
}

#[cfg(test)]
#[path = "decode_tests.rs"]
mod tests;
