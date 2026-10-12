//! The client half of the cue protocol: a program asks the cue authority for
//! one sound over an injected transport, so the whole surface is host-tested
//! without a machine.
//!
//! A program that parks on a wait-set of its own posts instead
//! ([`PostedCues`]) — its cues, and a session's push of its user's choices: a
//! blocking call would stall its loop on another service, and neither is ever
//! worth that.

use tairix_abi::reply::{decode_status_reply, STATUS_REPLY_LEN};
use tairix_abi::sound_cue::{
    decode_cue_reply, Answer, CueOutcome, CueRequest, SoundEvent, CUE_REPLY_LEN, CUE_REQUEST_LEN,
    CUE_REQUEST_MAX,
};
use tairix_abi::Errno;

use crate::catalog::MAX_CUE_SECONDS;

/// The round trip to the cue authority.
pub trait CueTransport {
    /// Send `request` and fill `reply`, answering how many bytes the answer
    /// took.
    ///
    /// # Errors
    ///
    /// The transport's own failure, or no authority answering.
    fn call(&mut self, request: &[u8], reply: &mut [u8]) -> Result<usize, Errno>;
}

/// Ask for `event` once, in the caller's room, to be answered at `answer`.
///
/// # Errors
///
/// The authority's refusal ([`decode_cue_reply`] lists them), the transport's
/// failure, or an answer that is not a cue reply.
pub fn cue(
    transport: &mut dyn CueTransport,
    event: SoundEvent,
    answer: Answer,
) -> Result<CueOutcome, Errno> {
    let mut request = [0u8; CUE_REQUEST_LEN];
    let sent = CueRequest::Cue { event, answer }.encode(&mut request)?;
    let mut reply = [0u8; CUE_REPLY_LEN];
    let len = transport.call(&request[..sent], &mut reply)?;
    decode_cue_reply(reply.get(..len).ok_or(Errno::LengthOutOfRange)?)
}

/// The posted round trip: a request handed over at once and its answer
/// collected later.
pub trait CuePost {
    /// Post `request`, given up on after `deadline_ns`, answering its ticket.
    ///
    /// # Errors
    ///
    /// The post's refusal: no authority, or its rendezvous full.
    fn post(&mut self, request: &[u8], deadline_ns: u64) -> Result<u64, Errno>;

    /// `ticket`'s answer into `reply`, its length, or `None` while it is
    /// still pending.
    ///
    /// # Errors
    ///
    /// [`Errno::TimedOut`] past its deadline, or the transport's refusal.
    fn reap(&mut self, ticket: u64, reply: &mut [u8]) -> Result<Option<usize>, Errno>;

    /// Give up on `ticket`.
    fn cancel(&mut self, ticket: u64);

    /// Start or stop waking the caller for answers.
    ///
    /// # Errors
    ///
    /// The wake's refusal: no authority to watch.
    fn watch(&mut self, armed: bool) -> Result<(), Errno>;
}

/// How long a posted cue's answer is waited for: past the longest any
/// healthy authority takes — the decodes queued ahead of it, the longest
/// sound, and its play-out — so only an authority that stopped answering
/// runs into it.
pub const CUE_ANSWER_DEADLINE_NS: u64 = 40_000_000_000;

/// How long a posted push of a session's choices is waited for. The authority
/// answers one in its next turn — it redeems the handles and reads the
/// machine's document, waiting on no decode — so only one that stopped
/// answering runs into it.
pub const POLICY_ANSWER_DEADLINE_NS: u64 = 5_000_000_000;

/// How long past its own length a sound may take to play out before the
/// authority stops it: room for the device's latency and a loaded machine.
pub const DRAIN_SLACK_NS: u64 = 1_000_000_000;

/// How long a caller that must not outlast its sound — the machine powering
/// off — waits for one asked to be answered once played out: the longest a
/// cue may sound and its slack. A sound a slow decode started late is cut
/// short rather than holding the machine up.
pub const PLAYED_OUT_WAIT_NS: u64 = MAX_CUE_SECONDS as u64 * 1_000_000_000 + DRAIN_SLACK_NS;

/// Cues one caller may have unanswered at once: as many as may sound at
/// once, so one more could only be dropped.
pub const MAX_POSTED: usize = 4;

/// The cues a caller has posted and not yet heard back about, and the push of
/// a session's choices awaiting its answer.
///
/// One caller's answers on the rendezvous are one wake, so everything it
/// posts there is collected here. The caller is woken for answers only while
/// one is outstanding: an authority that went away leaves its rendezvous
/// permanently ready, which must not keep waking a caller with nothing posted.
#[derive(Debug, Default)]
pub struct PostedCues {
    pending: [Option<(u64, SoundEvent)>; MAX_POSTED],
    policy: Option<u64>,
    watched: bool,
}

impl PostedCues {
    /// Nothing posted.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            pending: [None; MAX_POSTED],
            policy: None,
            watched: false,
        }
    }

    /// Post `event`, to be answered at `answer` and given up on after
    /// `deadline_ns`.
    ///
    /// # Errors
    ///
    /// [`Errno::LimitExceeded`] while [`MAX_POSTED`] are unanswered, or the
    /// post's or the wake's refusal; a refused cue holds nothing.
    pub fn post(
        &mut self,
        transport: &mut dyn CuePost,
        event: SoundEvent,
        answer: Answer,
        deadline_ns: u64,
    ) -> Result<(), Errno> {
        let slot = self
            .pending
            .iter_mut()
            .find(|slot| slot.is_none())
            .ok_or(Errno::LimitExceeded)?;
        let mut request = [0u8; CUE_REQUEST_LEN];
        let sent = CueRequest::Cue { event, answer }.encode(&mut request)?;
        *slot = Some((
            Self::send(&mut self.watched, transport, &request[..sent], deadline_ns)?,
            event,
        ));
        Ok(())
    }

    /// Post a push of a session's choices — `document`, naming no file, and
    /// each event's own file as a read-only handle minted to the authority's
    /// instance, in [`SoundEvent::ALL`] order with `0` for none — given up on
    /// after `deadline_ns`.
    ///
    /// # Errors
    ///
    /// [`Errno::Busy`] while an earlier push is unanswered, a document the
    /// request cannot carry, or the post's or the wake's refusal; a refused
    /// push holds nothing.
    pub fn push(
        &mut self,
        transport: &mut dyn CuePost,
        document: &str,
        files: [u64; SoundEvent::ALL.len()],
        deadline_ns: u64,
    ) -> Result<(), Errno> {
        if self.policy.is_some() {
            return Err(Errno::Busy);
        }
        let mut request = [0u8; CUE_REQUEST_MAX];
        let sent = CueRequest::Policy { document, files }.encode(&mut request)?;
        self.policy = Some(Self::send(
            &mut self.watched,
            transport,
            &request[..sent],
            deadline_ns,
        )?);
        Ok(())
    }

    /// Post `request`, watching for its answer: its ticket.
    fn send(
        watched: &mut bool,
        transport: &mut dyn CuePost,
        request: &[u8],
        deadline_ns: u64,
    ) -> Result<u64, Errno> {
        let ticket = transport.post(request, deadline_ns)?;
        if !*watched {
            if let Err(err) = transport.watch(true) {
                transport.cancel(ticket);
                return Err(err);
            }
            *watched = true;
        }
        Ok(ticket)
    }

    /// Whether a cue or a push is unanswered, so its caller still watches for
    /// the answer.
    #[must_use]
    pub fn waiting(&self) -> bool {
        self.policy.is_some() || self.pending.iter().any(Option::is_some)
    }

    /// Hand every cue's answer that has arrived to `answered`, never waiting
    /// for one that has not, and answer the push's, once it has arrived.
    pub fn collect(
        &mut self,
        transport: &mut dyn CuePost,
        mut answered: impl FnMut(SoundEvent, Result<CueOutcome, Errno>),
    ) -> Option<Result<(), Errno>> {
        for slot in &mut self.pending {
            let Some((ticket, event)) = *slot else {
                continue;
            };
            let mut reply = [0u8; CUE_REPLY_LEN];
            let result = match transport.reap(ticket, &mut reply) {
                Ok(None) => continue,
                Ok(Some(len)) => reply
                    .get(..len)
                    .ok_or(Errno::LengthOutOfRange)
                    .and_then(decode_cue_reply),
                Err(err) => Err(err),
            };
            *slot = None;
            answered(event, result);
        }
        let pushed = self.policy.and_then(|ticket| {
            let mut reply = [0u8; STATUS_REPLY_LEN];
            match transport.reap(ticket, &mut reply) {
                Ok(None) => None,
                Ok(Some(len)) => Some(
                    reply
                        .get(..len)
                        .ok_or(Errno::LengthOutOfRange)
                        .and_then(decode_status_reply),
                ),
                Err(err) => Some(Err(err)),
            }
        });
        if pushed.is_some() {
            self.policy = None;
        }
        self.unwatch_if_idle(transport);
        pushed
    }

    /// Give up on every cue and push still unanswered.
    pub fn abandon(&mut self, transport: &mut dyn CuePost) {
        for slot in &mut self.pending {
            if let Some((ticket, _)) = slot.take() {
                transport.cancel(ticket);
            }
        }
        if let Some(ticket) = self.policy.take() {
            transport.cancel(ticket);
        }
        self.unwatch_if_idle(transport);
    }

    fn unwatch_if_idle(&mut self, transport: &mut dyn CuePost) {
        if self.watched && !self.waiting() {
            // Nothing is left to wake for, whether or not the wake could be
            // withdrawn cleanly.
            let _ = transport.watch(false);
            self.watched = false;
        }
    }
}

/// The cue authority's reserved rendezvous, called and waited on.
#[cfg(feature = "rt")]
#[derive(Copy, Clone, Debug, Default)]
pub struct RtCue;

#[cfg(feature = "rt")]
impl CueTransport for RtCue {
    fn call(&mut self, request: &[u8], reply: &mut [u8]) -> Result<usize, Errno> {
        tairix_rt::ipc_call(tairix_abi::sound_cue::SOUND_CUE_ENDPOINT, request, reply)
            .map_err(Errno::from_syscall)
    }
}

/// The cue authority's reserved rendezvous, posted to from a loop parked on
/// the wait-set `set`, whose answers wake it with `token`.
#[cfg(feature = "rt")]
#[derive(Copy, Clone, Debug)]
pub struct RtCuePost {
    set: u64,
    token: u64,
}

#[cfg(feature = "rt")]
impl RtCuePost {
    /// Post from the loop parked on `set`, woken with `token`.
    #[must_use]
    pub const fn new(set: u64, token: u64) -> Self {
        Self { set, token }
    }
}

#[cfg(feature = "rt")]
impl CuePost for RtCuePost {
    fn post(&mut self, request: &[u8], deadline_ns: u64) -> Result<u64, Errno> {
        tairix_rt::call_post(
            tairix_abi::sound_cue::SOUND_CUE_ENDPOINT,
            request,
            deadline_ns,
        )
        .map_err(Errno::from_syscall)
    }

    fn reap(&mut self, ticket: u64, reply: &mut [u8]) -> Result<Option<usize>, Errno> {
        match tairix_rt::call_reap(tairix_abi::sound_cue::SOUND_CUE_ENDPOINT, ticket, reply) {
            Ok(len) => Ok(Some(len)),
            Err(raw) => match Errno::from_syscall(raw) {
                Errno::WouldBlock => Ok(None),
                err => Err(err),
            },
        }
    }

    fn cancel(&mut self, ticket: u64) {
        let _ = tairix_rt::call_cancel(tairix_abi::sound_cue::SOUND_CUE_ENDPOINT, ticket);
    }

    fn watch(&mut self, armed: bool) -> Result<(), Errno> {
        let op = if armed {
            tairix_abi::WaitSetOp::Add
        } else {
            tairix_abi::WaitSetOp::Del
        };
        let ret = tairix_rt::waitset_ctl(
            self.set,
            op,
            tairix_abi::WaitSourceKind::CallReply,
            tairix_abi::sound_cue::SOUND_CUE_ENDPOINT,
            self.token,
        );
        if ret == 0 {
            Ok(())
        } else {
            Err(Errno::from_syscall(ret))
        }
    }
}

#[cfg(test)]
#[path = "cue_tests.rs"]
mod tests;
