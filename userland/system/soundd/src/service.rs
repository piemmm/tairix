//! The cue authority: who may cue what, what a cue resolves to, and how many
//! may sound (`plans/SOUND.md` §Default desktop sounds).
//!
//! # Authority
//!
//! Nothing in a request decides who may ask: the event's class does, checked
//! against the caller's kernel-attested origin. A lifecycle sound is played
//! only for the principal that owns the transition it announces, as the
//! machine's own; an application sound is played as its caller's session's,
//! and the audio service — not this one, which cannot see the seat — admits it
//! only while that session holds the room. So no program can imitate the
//! machine, and one user's cue never sounds in another's room.
//!
//! # The user's layer
//!
//! The session at the seat pushes its user's choices ([`CueRequest::Policy`]),
//! admitted only from the anchor of the login session the seat's display lease
//! names, and only while it names it: the layer is dropped, its files closed,
//! the moment the lease moves on, and the holder is read again before every
//! request. The choices govern every sound but the machine's start and a
//! session's start and end, which sound when no session holds the seat. A
//! chosen file arrives as a delegation, read under the session's identity, so
//! this service never opens a user's file by name.
//!
//! # Silence
//!
//! A cue that makes no sound still succeeds and says why: turned off, no theme
//! holds it, undecodable, a repeat, too many at once, or no output. Only a
//! refusal of the caller is an error.

use alloc::rc::Rc;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

use tairix_abi::audio::{AudioGain, AudioNotify, OpenParams, StreamRole, StreamState, Tenancy};
use tairix_abi::driver::audio::{ring_bounds, Frames, StreamDirection};
use tairix_abi::origin::{Origin, ProcId};
use tairix_abi::reply::{encode_status_reply, STATUS_REPLY_LEN};
use tairix_abi::sound_cue::{
    encode_cue_reply, names_policy, Answer, CueAuthority as Class, CueOutcome, CueRequest, Silence,
    SoundEvent, CUE_REPLY_LEN,
};
use tairix_abi::Errno;
use tairix_appconf::{Document, Registry, MAX_DOCUMENT_LEN};
use tairix_log::{log, Event, Field, FieldValue, Level, Sink};
use tairix_sandbox::session::SessionDescriptors;
use tairix_sandbox::supervise::SessionLauncher;
use tairix_sound::SoundFormat;
use tairix_soundtheme::{
    asset_path, layered, merge_pushed, resolve, CueSource, Resolution, SoundSettings,
    CUE_ANSWER_DEADLINE_NS, DRAIN_SLACK_NS, MACHINE_SETTINGS, MAX_CUE_SECONDS, MAX_SOUND_BYTES,
};

use crate::bounds::{Admit, Principals, MAX_CONCURRENT_CUES};
use crate::decode::{Decoded, Decoder, DECODE_BUDGET_NS};
use crate::events;
use crate::pcm::{CachedCue, CueCache, CueKey, CueOrigin, CuePcm};
use crate::speaker::Speakers;
use crate::store::{FileIdentity, SoundStore};

/// Undecodable sounds remembered, so a broken file is not read and decoded
/// again for every cue until it changes.
const MAX_REFUSED: usize = 16;

// A cue's answer comes once every decode queued ahead of its own has run, and
// at the latest its own sound has played out: within the deadline a client
// posts with, so only an authority that stopped answering meets it.
const _: () = assert!(
    MAX_CONCURRENT_CUES as u64 * DECODE_BUDGET_NS
        + MAX_CUE_SECONDS as u64 * 1_000_000_000
        + DRAIN_SLACK_NS
        < CUE_ANSWER_DEADLINE_NS
);

/// How many cues made no sound, and why, and how many were refused.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct Tally {
    /// Cues that sounded.
    pub played: u64,
    /// Cues for a sound turned off.
    pub disabled: u64,
    /// Cues for a sound no theme holds.
    pub no_asset: u64,
    /// Cues for a sound that could not be decoded.
    pub undecodable: u64,
    /// Repeats coalesced into the sound before.
    pub coalesced: u64,
    /// Cues dropped because as many as may were already sounding.
    pub too_many: u64,
    /// Cues with no output to play on.
    pub no_device: u64,
    /// Lifecycle cues refused to a principal that does not own them.
    pub refused: u64,
    /// Cues refused for their caller's rate.
    pub limited: u64,
}

/// One admitted cue, before it sounds.
#[derive(Copy, Clone, Debug)]
struct Cue {
    event: SoundEvent,
    answer: Answer,
    tenancy: Tenancy,
    gain: AudioGain,
    ticket: u64,
}

/// A cue waiting for its sound to decode.
struct Waiting {
    cue: Cue,
    job: u64,
}

/// A decode waiting for the decoder.
struct Queued {
    job: u64,
    bytes: Vec<u8>,
    format: Option<SoundFormat>,
}

/// Where a cue's bytes are read from.
#[derive(Clone)]
enum Source {
    /// A file by its path: a theme's asset, or the machine's replacement.
    Path(String),
    /// The file the session at the seat handed over for an event.
    Handed(SoundEvent),
}

/// A decode begun or queued, and which sound it is of.
struct Job {
    id: u64,
    key: CueKey,
    source: Source,
}

/// The choices of the session at the seat, while it holds it.
struct UserLayer<H> {
    /// The login session whose anchor pushed them.
    session: ProcId,
    /// What it pushed, naming no file, kept to lay over the machine's
    /// document again when that changes.
    document: String,
    /// The settings in effect: the pushed document over the machine's.
    settings: SoundSettings,
    /// Each event's own file, held open, in [`SoundEvent::ALL`] order; one
    /// file chosen for several events is held once.
    files: [Option<Rc<H>>; EVENTS],
    /// The decode of each event's own file the cache holds, so the session's
    /// sounds leave memory with it.
    cached: [Option<CueKey>; EVENTS],
}

const EVENTS: usize = SoundEvent::ALL.len();

/// An answer to send at once: a cue's, or a policy's status.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Reply {
    bytes: [u8; CUE_REPLY_LEN],
    len: usize,
}

impl Reply {
    fn cue(result: Result<CueOutcome, Errno>) -> Self {
        Self {
            bytes: encode_cue_reply(result),
            len: CUE_REPLY_LEN,
        }
    }

    fn status(result: Result<(), Errno>) -> Self {
        let mut bytes = [0u8; CUE_REPLY_LEN];
        bytes[..STATUS_REPLY_LEN].copy_from_slice(&encode_status_reply(result));
        Self {
            bytes,
            len: STATUS_REPLY_LEN,
        }
    }

    /// The refusal of `request`, in the shape its operation is answered in.
    #[must_use]
    pub fn refusal(request: &[u8], err: Errno) -> Self {
        if names_policy(request) {
            Self::status(Err(err))
        } else {
            Self::cue(Err(err))
        }
    }

    /// The frame.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.len]
    }
}

/// A cue sounding.
struct Playing<St> {
    stream: St,
    mailbox: u64,
    pcm: Arc<CuePcm>,
    written: u64,
    draining: bool,
    deadline: u64,
    event: SoundEvent,
    /// Whether it sounds a machine's moment, whose playing out is recorded.
    machine: bool,
    /// The call to answer once it has played out.
    reply: Option<u64>,
}

/// How a sounding cue's stream ended.
enum Ending {
    /// Every frame reached the device.
    PlayedOut,
    /// The stream broke off: the room moved, the device went, or it failed.
    BrokeOff,
}

/// What became of a cue at once.
enum Decision {
    Now(CueOutcome),
    Later,
}

/// The machine's sound document as last read.
struct MachineSettings {
    identity: Option<FileIdentity>,
    settings: SoundSettings,
}

/// The cue authority over its five seams.
pub struct CueAuthority<St, Sp, L, S>
where
    St: SoundStore,
    Sp: Speakers,
    L: SessionLauncher,
    S: Sink + Clone,
{
    store: St,
    speakers: Sp,
    decoder: Decoder<L, S>,
    cache: CueCache,
    sink: S,
    principals: Principals,
    machine: MachineSettings,
    layer: Option<UserLayer<St::Handed>>,
    jobs: Vec<Job>,
    queued: Vec<Queued>,
    waiting: Vec<Waiting>,
    playing: Vec<Playing<Sp::Stream>>,
    refused: Vec<CueKey>,
    replies: Vec<(u64, [u8; CUE_REPLY_LEN])>,
    next_job: u64,
    tally: Tally,
}

impl<St, Sp, L, S> CueAuthority<St, Sp, L, S>
where
    St: SoundStore,
    Sp: Speakers,
    L: SessionLauncher,
    S: Sink + Clone,
{
    /// An authority with nothing sounding, reading `store`, playing through
    /// `speakers`, decoding through `decoder`, keeping decodes in `cache`
    /// and recording its decisions to `sink`.
    pub fn new(store: St, speakers: Sp, decoder: Decoder<L, S>, cache: CueCache, sink: S) -> Self {
        Self {
            store,
            speakers,
            decoder,
            cache,
            sink,
            principals: Principals::new(),
            machine: MachineSettings {
                identity: None,
                settings: SoundSettings::default(),
            },
            layer: None,
            jobs: Vec::new(),
            queued: Vec::new(),
            waiting: Vec::new(),
            playing: Vec::new(),
            refused: Vec::new(),
            replies: Vec::new(),
            next_job: 1,
            tally: Tally::default(),
        }
    }

    /// What every cue so far came to.
    #[must_use]
    pub const fn tally(&self) -> Tally {
        self.tally
    }

    /// Serve one request from `caller`, the call answered with `ticket`,
    /// while `seat` is the login session the seat's display lease names: the
    /// reply now, or `None` when the answer comes later through
    /// [`Self::take_replies`].
    pub fn serve(
        &mut self,
        now: u64,
        caller: &Origin,
        ticket: u64,
        request: &[u8],
        seat: Option<ProcId>,
    ) -> Option<Reply> {
        self.keep_time(now);
        self.seat_moved(now, seat);
        let (event, answer) = match CueRequest::from_bytes(request) {
            Ok(CueRequest::Cue { event, answer }) => (event, answer),
            Ok(CueRequest::Policy { document, files }) => {
                return Some(Reply::status(self.adopt(caller, seat, document, files)));
            }
            Err(err) => return Some(Reply::refusal(request, err)),
        };
        let result = match self.admit(now, caller, ticket, event, answer) {
            Ok(Decision::Later) => return None,
            Ok(Decision::Now(outcome)) => Ok(outcome),
            Err(err) => Err(err),
        };
        Some(Reply::cue(result))
    }

    /// The seat's display lease names `seat` at `now`: a user's layer the
    /// seat has left is dropped, its files closed and its sounds forgotten,
    /// and a machine's moment still waiting on one of its files sounds as the
    /// seat now decides.
    pub fn seat_moved(&mut self, now: u64, seat: Option<ProcId>) {
        self.keep_time(now);
        if self
            .layer
            .as_ref()
            .is_some_and(|layer| Some(layer.session) != seat)
        {
            self.drop_layer();
            self.audit_session(
                events::POLICY_DROPPED,
                Level::Info,
                "a session's sound choices left the seat with it",
                None,
            );
            self.resound(now);
        }
    }

    /// Whether a session's choices are in force.
    #[must_use]
    pub const fn holds_choices(&self) -> bool {
        self.layer.is_some()
    }

    /// Answers that have become due, each with the ticket of the call it
    /// answers.
    pub fn take_replies(&mut self) -> impl Iterator<Item = (u64, [u8; CUE_REPLY_LEN])> + '_ {
        self.replies.drain(..)
    }

    /// When the authority next needs a turn with nothing else to wake it.
    #[must_use]
    pub fn deadline(&self) -> Option<u64> {
        let playing = self.playing.iter().map(|cue| cue.deadline).min();
        match (self.decoder.deadline(), playing) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    /// The decoder's descriptors and which of them to watch.
    #[must_use]
    pub fn decoder_members(&self) -> (Option<SessionDescriptors>, bool, bool) {
        self.decoder.members()
    }

    /// The mailboxes of every sounding cue.
    pub fn mailboxes(&self) -> impl Iterator<Item = u64> + '_ {
        self.playing.iter().map(|cue| cue.mailbox)
    }

    /// Give back what the decoded-cue cache holds above the memory band now
    /// in force.
    pub fn trim(&mut self) {
        self.cache.enforce_pressure();
    }

    /// The decoder's descriptors were reported ready.
    pub fn on_decoder(&mut self, now: u64, readable: bool, writable: bool) {
        self.keep_time(now);
        self.decoder.on_ready(now, readable, writable);
        self.pump_decoder(now);
    }

    /// Keep every deadline that has come by `now`, whatever woke the
    /// authority: a source kept ready must not hold a decode past its budget
    /// or a sound past its end.
    fn keep_time(&mut self, now: u64) {
        if self.deadline().is_some_and(|at| at <= now) {
            self.on_timer(now);
        }
    }

    /// The deadline [`Self::deadline`] named has come.
    pub fn on_timer(&mut self, now: u64) {
        self.pump_decoder(now);
        let mut at = 0;
        while at < self.playing.len() {
            if now >= self.playing[at].deadline {
                let cue = self.playing.swap_remove(at);
                self.audit_event(
                    events::CUE_CUT,
                    Level::Warn,
                    "a sound overran its deadline",
                    cue.event,
                );
                self.retire(cue);
            } else {
                at += 1;
            }
        }
    }

    /// A sounding cue's mailbox was reported ready at `now`.
    pub fn on_stream(&mut self, now: u64, mailbox: u64) {
        self.keep_time(now);
        let Some(at) = self.playing.iter().position(|cue| cue.mailbox == mailbox) else {
            return;
        };
        let Some(ending) = self.advance(at) else {
            return;
        };
        let cue = self.playing.swap_remove(at);
        if matches!(ending, Ending::PlayedOut) && cue.machine {
            self.audit_event(
                events::MACHINE_CUE_PLAYED,
                Level::Info,
                events::played_out_message(cue.event),
                cue.event,
            );
        }
        self.retire(cue);
    }

    /// Adopt a session's pushed choices, or refuse them whole: only the anchor
    /// of the login session at the seat may push, a document is read
    /// strictly, and every handle is redeemed or the push is refused. A
    /// refused push's handles are redeemed and closed all the same, so none is
    /// left pending against its grantor.
    fn adopt(
        &mut self,
        caller: &Origin,
        seat: Option<ProcId>,
        document: &str,
        handles: [u64; EVENTS],
    ) -> Result<(), Errno> {
        let grantor = caller.proc_id();
        let anchors_seat = seat == Some(grantor) && caller.login_session() == Some(grantor);
        let layer = if anchors_seat {
            self.layer_from(grantor, document, &handles)
        } else {
            Err(Errno::SeatNotOwner)
        };
        match layer {
            Ok(layer) => {
                self.replace_layer(Some(layer));
                self.audit_session(
                    events::POLICY_ADOPTED,
                    Level::Info,
                    "the session at the seat handed over its sound choices",
                    Some(caller),
                );
                Ok(())
            }
            Err(err) => {
                for handle in handles.into_iter().filter(|&handle| handle != 0) {
                    drop(self.store.redeem(handle, grantor));
                }
                self.audit_session(
                    events::POLICY_REFUSED,
                    Level::Warn,
                    "a push of sound choices was refused",
                    Some(caller),
                );
                Err(err)
            }
        }
    }

    /// The layer `session`'s pushed `document` and `handles` make over the
    /// machine's document.
    fn layer_from(
        &mut self,
        session: ProcId,
        document: &str,
        handles: &[u64; EVENTS],
    ) -> Result<UserLayer<St::Handed>, Errno> {
        self.refresh_settings();
        let settings =
            merge_pushed(&self.machine.settings, document).map_err(|_| Errno::OutOfRange)?;
        let stray = SoundEvent::ALL
            .iter()
            .zip(handles)
            .any(|(event, &handle)| handle != 0 && !layered(*event));
        if stray {
            return Err(Errno::OutOfRange);
        }
        let mut kept = String::new();
        kept.try_reserve_exact(document.len())
            .map_err(|_| Errno::OutOfMemory)?;
        kept.push_str(document);
        // A delegation is redeemed once, and identical ones are one handle:
        // a file chosen for several events is shared between them.
        let mut files: [Option<Rc<St::Handed>>; EVENTS] = core::array::from_fn(|_| None);
        for at in 0..EVENTS {
            let handle = handles[at];
            if handle == 0 {
                continue;
            }
            let shared = handles[..at]
                .iter()
                .zip(&files[..at])
                .find_map(|(&earlier, file)| file.as_ref().filter(|_| earlier == handle));
            files[at] = Some(match shared {
                Some(file) => Rc::clone(file),
                None => Rc::new(self.store.redeem(handle, session)?),
            });
        }
        Ok(UserLayer {
            session,
            document: kept,
            settings,
            files,
            cached: core::array::from_fn(|_| None),
        })
    }

    /// Drop the user's layer, closing its files and forgetting its sounds.
    fn drop_layer(&mut self) {
        self.replace_layer(None);
    }

    /// Put `layer` in force in place of the layer that was, closing that
    /// layer's files. What is known of a file the same session hands over
    /// again unchanged — its decode, or that it does not decode — is kept,
    /// since a session pushes again on every change its user makes; all else
    /// of the earlier layer is forgotten.
    fn replace_layer(&mut self, mut layer: Option<UserLayer<St::Handed>>) {
        let earlier = self.layer.take();
        let pushed_again = match (&earlier, &layer) {
            (Some(earlier), Some(layer)) if earlier.session == layer.session => Some(layer),
            _ => None,
        };
        let mut unchanged: [Option<FileIdentity>; EVENTS] = [None; EVENTS];
        if let Some(layer) = pushed_again {
            for (identity, file) in unchanged.iter_mut().zip(&layer.files) {
                *identity = file
                    .as_ref()
                    .and_then(|file| self.store.identify_handed(file).ok());
            }
        }
        let again = pushed_again.map(|layer| layer.session);
        let kept = |session: ProcId, event: SoundEvent, identity: FileIdentity| {
            again == Some(session) && unchanged[event.index()] == Some(identity)
        };
        if let Some(mut earlier) = earlier {
            for cached in &mut earlier.cached {
                let Some(key) = cached.take() else {
                    continue;
                };
                let carried = match key.origin {
                    CueOrigin::Handed { session, event } if kept(session, event, key.identity) => {
                        Some(event)
                    }
                    _ => None,
                };
                match (carried, layer.as_mut()) {
                    (Some(event), Some(layer)) => layer.cached[event.index()] = Some(key),
                    _ => self.cache.invalidate(&key),
                }
            }
        }
        self.refused.retain(|key| match key.origin {
            CueOrigin::Handed { session, event } => kept(session, event, key.identity),
            CueOrigin::Theme { .. } | CueOrigin::Machine(_) => true,
        });
        self.layer = layer;
    }

    /// Whether `session`'s choices are the ones in force.
    fn holds(&self, session: ProcId) -> bool {
        self.layer
            .as_ref()
            .is_some_and(|layer| layer.session == session)
    }

    /// Whether `key` is a sound of no session, or of the session whose
    /// choices are in force: a decode or a refusal that lands after its
    /// session left the seat is not kept against it.
    fn in_force(&self, key: &CueKey) -> bool {
        match key.origin {
            CueOrigin::Handed { session, .. } => self.holds(session),
            CueOrigin::Theme { .. } | CueOrigin::Machine(_) => true,
        }
    }

    /// Sound again, as the seat now decides, every machine's moment still
    /// waiting on a file of a session no longer at the seat, and drop the
    /// decodes no cue waits on any more.
    fn resound(&mut self, now: u64) {
        let mut at = 0;
        while let Some(waiting) = self.waiting.get(at) {
            let cue = waiting.cue;
            let departed = matches!(cue.event.authority(), Class::Lifecycle)
                && matches!(cue.tenancy, Tenancy::Session(session) if !self.holds(session));
            if !departed {
                at += 1;
                continue;
            }
            self.waiting.remove(at);
            match self.sound(now, cue.event, cue.answer, Tenancy::Machine, cue.ticket) {
                Ok(Decision::Now(outcome)) => self.answer(cue.ticket, Ok(outcome)),
                Ok(Decision::Later) => {}
                Err(err) => self.answer(cue.ticket, Err(err)),
            }
        }
        let Self {
            jobs,
            queued,
            waiting,
            ..
        } = self;
        queued.retain(|held| {
            let wanted = waiting.iter().any(|cue| cue.job == held.job);
            if !wanted {
                jobs.retain(|job| job.id != held.job);
            }
            wanted
        });
        self.pump_decoder(now);
    }

    /// Decide a cue of `event` from `caller`, to be answered at `answer`.
    fn admit(
        &mut self,
        now: u64,
        caller: &Origin,
        ticket: u64,
        event: SoundEvent,
        answer: Answer,
    ) -> Result<Decision, Errno> {
        let tenancy = match event.authority() {
            Class::Lifecycle if event.owned_by(caller) => Tenancy::Machine,
            Class::Lifecycle => {
                self.tally.refused += 1;
                self.audit_caller(
                    events::CUE_REFUSED,
                    "a lifecycle sound was asked for by a principal that does not own it",
                    caller,
                    event,
                );
                return Err(Errno::PermissionDenied);
            }
            Class::Application => caller
                .login_session()
                .map_or(Tenancy::Caller, Tenancy::Session),
        };
        match self.principals.admit(caller.uid(), event, now) {
            Admit::Admitted => {}
            Admit::Coalesced => return Ok(self.silent(Silence::Coalesced)),
            Admit::Limited { first } => {
                self.tally.limited += 1;
                if first {
                    self.audit_caller(
                        events::CUE_LIMITED,
                        "a principal went past its cue rate",
                        caller,
                        event,
                    );
                }
                return Err(Errno::LimitExceeded);
            }
        }
        let decision = self.sound(now, event, answer, tenancy, ticket)?;
        if matches!(decision, Decision::Later) {
            self.pump_decoder(now);
        }
        Ok(decision)
    }

    /// Sound `event` for a cue of `tenancy` answered at `answer` with
    /// `ticket`: at once from the cache, or once its decode lands. The caller
    /// pumps the decoder, so no cue is answered while the waiting list is
    /// being walked.
    fn sound(
        &mut self,
        now: u64,
        event: SoundEvent,
        answer: Answer,
        tenancy: Tenancy,
        ticket: u64,
    ) -> Result<Decision, Errno> {
        self.refresh_settings();
        let (gain, source, key) = match self.locate(event, tenancy) {
            Ok(found) => found,
            Err(silence) => return Ok(self.silent(silence)),
        };
        if self.waiting.len() + self.playing.len() >= MAX_CONCURRENT_CUES {
            return Ok(self.silent(Silence::TooMany));
        }
        // A session's own file is that session's data: it sounds only in its
        // room, so the audio service stops it when the room moves on.
        let tenancy = match key.origin {
            CueOrigin::Handed { session, .. } => Tenancy::Session(session),
            CueOrigin::Theme { .. } | CueOrigin::Machine(_) => tenancy,
        };
        let cue = Cue {
            event,
            answer,
            tenancy,
            gain,
            ticket,
        };
        if let Some(CachedCue(pcm)) = self.cache.find(&(), &key).cloned() {
            return self.play(now, cue, pcm);
        }
        if self.refused.contains(&key) {
            self.tally.undecodable += 1;
            return Ok(Decision::Now(CueOutcome::Silent(Silence::Undecodable)));
        }
        let job = match self.jobs.iter().find(|job| job.key == key) {
            Some(job) => job.id,
            None => match self.load(now, key, source)? {
                Some(job) => job,
                None => return Ok(self.silent(Silence::Undecodable)),
            },
        };
        if self.waiting.try_reserve(1).is_err() {
            return Err(Errno::OutOfMemory);
        }
        self.waiting.push(Waiting { cue, job });
        Ok(Decision::Later)
    }

    /// Where `event`'s sound, cued for `tenancy`, is and at what level, or why
    /// it makes none.
    ///
    /// The seat's session's choices decide it where they reach the event and
    /// the cue is heard at the seat — the machine's or that session's own —
    /// and the machine's document otherwise, so no other session's cue learns
    /// or reaches them. A file chosen for the event — the session's own, else
    /// the machine's replacement — plays ahead of any theme.
    fn locate(
        &mut self,
        event: SoundEvent,
        tenancy: Tenancy,
    ) -> Result<(AudioGain, Source, CueKey), Silence> {
        let Self {
            store,
            machine,
            layer,
            ..
        } = self;
        let layer = layer.as_ref().filter(|layer| {
            layered(event)
                && match tenancy {
                    Tenancy::Machine => true,
                    Tenancy::Session(session) => session == layer.session,
                    Tenancy::Caller => false,
                }
        });
        let settings = layer.map_or(&machine.settings, |layer| &layer.settings);
        let handed = layer.and_then(|layer| {
            layer.files[event.index()]
                .as_ref()
                .map(|file| (layer.session, file))
        });
        let replacement = settings.event(event).file.as_ref();
        let mut found = None;
        let resolution = resolve(
            event,
            settings,
            handed.is_some() || replacement.is_some(),
            |theme, asset| {
                let Some(path) = asset_path(theme, asset) else {
                    return false;
                };
                match store.identify(&path) {
                    Ok(Some(identity)) => {
                        found = Some((path, identity));
                        true
                    }
                    Ok(None) | Err(_) => false,
                }
            },
        );
        let located = match resolution {
            Resolution::Play {
                source: CueSource::Own,
                gain,
            } => match (handed, replacement) {
                (Some((session, file)), _) => store.identify_handed(file).map(|identity| {
                    (
                        gain,
                        Source::Handed(event),
                        CueKey {
                            origin: CueOrigin::Handed { session, event },
                            identity,
                        },
                    )
                }),
                (None, Some(path)) => match store.identify(path.as_str()) {
                    Ok(Some(identity)) => Ok((
                        gain,
                        Source::Path(String::from(path.as_str())),
                        CueKey {
                            origin: CueOrigin::Machine(event),
                            identity,
                        },
                    )),
                    Ok(None) => Err(Errno::NotFound),
                    Err(err) => Err(err),
                },
                (None, None) => Err(Errno::NotFound),
            },
            Resolution::Play {
                source: CueSource::Theme { theme, asset },
                gain,
            } => match found {
                Some((path, identity)) => Ok((
                    gain,
                    Source::Path(path),
                    CueKey {
                        origin: CueOrigin::Theme {
                            theme: String::from(theme),
                            asset,
                        },
                        identity,
                    },
                )),
                None => Err(Errno::NotFound),
            },
            Resolution::Silent(Silence::NoAsset) => {
                self.audit_event(
                    events::CUE_NO_ASSET,
                    Level::Warn,
                    "no theme holds the sound",
                    event,
                );
                return Err(Silence::NoAsset);
            }
            Resolution::Silent(silence) => return Err(silence),
        };
        located.map_err(|err| {
            self.unreadable_choice(event, err);
            Silence::Undecodable
        })
    }

    /// Read the sound `key` names from `source` and queue its decode,
    /// answering the decode's job, or `None` when the file cannot be read.
    fn load(&mut self, now: u64, key: CueKey, source: Source) -> Result<Option<u64>, Errno> {
        let read = match &source {
            Source::Path(path) => self.store.read(path, key.identity, MAX_SOUND_BYTES),
            Source::Handed(event) => {
                let Self { store, layer, .. } = self;
                match layer
                    .as_ref()
                    .and_then(|layer| layer.files[event.index()].as_ref())
                {
                    Some(file) => store.read_handed(file, key.identity, MAX_SOUND_BYTES),
                    None => Err(Errno::NotFound),
                }
            }
        };
        let bytes = match read {
            Ok(bytes) => bytes,
            Err(err) => {
                self.undecodable(&source, Some(err));
                // Only a version too long to be a cue is the file's own
                // doing; memory, the volume and a file that changed are not.
                if err == Errno::LengthOutOfRange {
                    self.remember_refused(key);
                }
                return Ok(None);
            }
        };
        // A theme's asset is the shipped format; any other file is the
        // decoder's to identify.
        let format = matches!(key.origin, CueOrigin::Theme { .. }).then_some(SoundFormat::Flac);
        let job = self.next_job;
        self.next_job = self.next_job.wrapping_add(1).max(1);
        if self.jobs.try_reserve(1).is_err() || self.queued.try_reserve(1).is_err() {
            return Err(Errno::OutOfMemory);
        }
        self.jobs.push(Job {
            id: job,
            key,
            source,
        });
        self.queued.push(Queued { job, bytes, format });
        self.start_next(now);
        Ok(Some(job))
    }

    /// Hand the oldest queued decode to the decoder, if it is free.
    fn start_next(&mut self, now: u64) {
        if self.decoder.is_busy() || self.queued.is_empty() {
            return;
        }
        let Queued { job, bytes, format } = self.queued.remove(0);
        if let Err(bytes) = self.decoder.begin(now, job, bytes, format) {
            self.queued.insert(0, Queued { job, bytes, format });
        }
    }

    /// Drive the decoder, and sound or silence every cue a finished decode
    /// was holding.
    fn pump_decoder(&mut self, now: u64) {
        while let Some((job, decoded)) = self.decoder.pump(now) {
            self.decoded(now, job, decoded);
            self.start_next(now);
        }
    }

    /// `job`'s decode ended in `decoded`.
    fn decoded(&mut self, now: u64, job: u64, decoded: Decoded) {
        let Some(at) = self.jobs.iter().position(|held| held.id == job) else {
            return;
        };
        let Job { key, source, .. } = self.jobs.swap_remove(at);
        let pcm = match decoded {
            Decoded::Pcm(pcm) => {
                let pcm = Arc::new(pcm);
                self.keep_decode(key, &pcm);
                Some(pcm)
            }
            Decoded::Refused => {
                self.undecodable(&source, None);
                self.remember_refused(key);
                None
            }
            Decoded::Failed(err) => {
                self.undecodable(&source, Some(err));
                None
            }
        };
        let mut index = 0;
        while index < self.waiting.len() {
            if self.waiting[index].job != job {
                index += 1;
                continue;
            }
            let Waiting { cue, .. } = self.waiting.remove(index);
            let decision = match &pcm {
                Some(pcm) => self.play(now, cue, Arc::clone(pcm)),
                None => Ok(self.silent(Silence::Undecodable)),
            };
            match decision {
                Ok(Decision::Now(outcome)) => self.answer(cue.ticket, Ok(outcome)),
                Ok(Decision::Later) => {}
                Err(err) => self.answer(cue.ticket, Err(err)),
            }
        }
    }

    /// Keep the decode `pcm` of `key`: a session's own file only while its
    /// choices are in force, and then in place of its earlier version.
    fn keep_decode(&mut self, key: CueKey, pcm: &Arc<CuePcm>) {
        if !self.in_force(&key) {
            return;
        }
        if let (CueOrigin::Handed { event, .. }, Some(layer)) = (&key.origin, self.layer.as_mut()) {
            let earlier = layer.cached[event.index()].replace(key.clone());
            if let Some(earlier) = earlier.filter(|earlier| *earlier != key) {
                self.cache.invalidate(&earlier);
            }
        }
        self.cache.retain(&(), key, CachedCue(Arc::clone(pcm)));
    }

    /// Open `cue`'s stream and start it sounding `pcm`.
    fn play(&mut self, now: u64, cue: Cue, pcm: Arc<CuePcm>) -> Result<Decision, Errno> {
        let frames = pcm.frames();
        let params = OpenParams {
            device_id: 0,
            direction: StreamDirection::Playback,
            format: pcm.format,
            rate: pcm.rate,
            channel_map: pcm.channel_map,
            role: StreamRole::Notification,
            latency_target_frames: u32::try_from(frames)
                .unwrap_or(u32::MAX)
                .clamp(1, ring_bounds::MAX_FRAMES),
            tenancy: cue.tenancy,
        };
        let mut stream = match self.speakers.open(&params) {
            Ok(stream) => stream,
            Err(Errno::DeviceOffline | Errno::NotFound) => {
                return Ok(self.silent(Silence::NoDevice));
            }
            Err(err) => return Err(err),
        };
        if cue.gain != AudioGain::UNITY {
            if let Err(err) = self.speakers.set_gain(&mut stream, cue.gain) {
                self.speakers.close(stream);
                return Err(err);
            }
        }
        let mut playing = Playing {
            mailbox: self.speakers.mailbox(&stream),
            stream,
            deadline: now
                .saturating_add(pcm.duration().saturating_total_nanos())
                .saturating_add(DRAIN_SLACK_NS),
            pcm,
            written: 0,
            draining: false,
            event: cue.event,
            machine: matches!(cue.event.authority(), Class::Lifecycle),
            reply: None,
        };
        if let Err(err) = Self::begin(&mut self.speakers, &mut playing) {
            self.speakers.close(playing.stream);
            return Err(err);
        }
        if self.playing.try_reserve(1).is_err() {
            self.speakers.close(playing.stream);
            return Err(Errno::OutOfMemory);
        }
        self.tally.played += 1;
        let decision = match cue.answer {
            Answer::Started => Decision::Now(CueOutcome::Played),
            Answer::PlayedOut => {
                playing.reply = Some(cue.ticket);
                Decision::Later
            }
        };
        self.playing.push(playing);
        Ok(decision)
    }

    /// Queue what the ring takes, start the stream, and drain it if the whole
    /// cue went in.
    fn begin(speakers: &mut Sp, playing: &mut Playing<Sp::Stream>) -> Result<(), Errno> {
        Self::refill(speakers, playing)?;
        speakers.start(&mut playing.stream)?;
        Self::drain_if_queued(speakers, playing)
    }

    /// Take every notification the `at`th cue's stream has, answering how
    /// it ended once it is over.
    fn advance(&mut self, at: usize) -> Option<Ending> {
        let Self {
            speakers, playing, ..
        } = self;
        let cue = &mut playing[at];
        loop {
            let notify = match speakers.next_notify(&mut cue.stream) {
                Ok(Some(notify)) => notify,
                Ok(None) => return None,
                Err(_) => return Some(Ending::BrokeOff),
            };
            match notify {
                AudioNotify::SpaceAvailable { .. } => {
                    let moved = Self::refill(speakers, cue)
                        .and_then(|()| Self::drain_if_queued(speakers, cue));
                    if moved.is_err() {
                        return Some(Ending::BrokeOff);
                    }
                }
                AudioNotify::StateChanged { state, .. } => match state {
                    StreamState::Idle if cue.draining => return Some(Ending::PlayedOut),
                    StreamState::SeatInactive | StreamState::DeviceLost | StreamState::Faulted => {
                        return Some(Ending::BrokeOff);
                    }
                    _ => {}
                },
                AudioNotify::Xrun { .. } => {}
            }
        }
    }

    /// Publish as much of the cue as its ring takes.
    fn refill(speakers: &mut Sp, playing: &mut Playing<Sp::Stream>) -> Result<(), Errno> {
        let frame_bytes = playing.pcm.frame_bytes();
        let from = usize::try_from(playing.written)
            .ok()
            .and_then(|frames| frames.checked_mul(frame_bytes))
            .ok_or(Errno::OutOfRange)?;
        let Some(rest) = playing
            .pcm
            .bytes
            .get(from..)
            .filter(|rest| !rest.is_empty())
        else {
            return Ok(());
        };
        let took = speakers.write(&mut playing.stream, Frames::new(playing.written), rest)?;
        playing.written += u64::from(took);
        Ok(())
    }

    /// Drain the cue once all of it is queued.
    fn drain_if_queued(speakers: &mut Sp, playing: &mut Playing<Sp::Stream>) -> Result<(), Errno> {
        if playing.draining || playing.written < playing.pcm.frames() {
            return Ok(());
        }
        speakers.drain(&mut playing.stream)?;
        playing.draining = true;
        Ok(())
    }

    /// Close an ended cue and answer whoever waits on it.
    fn retire(&mut self, cue: Playing<Sp::Stream>) {
        if let Some(ticket) = cue.reply {
            self.answer(ticket, Ok(CueOutcome::Played));
        }
        self.speakers.close(cue.stream);
    }

    /// Queue `result` as the answer to `ticket`.
    fn answer(&mut self, ticket: u64, result: Result<CueOutcome, Errno>) {
        if self.replies.try_reserve(1).is_ok() {
            self.replies.push((ticket, encode_cue_reply(result)));
        }
    }

    /// Count a cue that makes no sound.
    fn silent(&mut self, silence: Silence) -> Decision {
        let counter = match silence {
            Silence::Disabled => &mut self.tally.disabled,
            Silence::NoAsset => &mut self.tally.no_asset,
            Silence::Undecodable => &mut self.tally.undecodable,
            Silence::Coalesced => &mut self.tally.coalesced,
            Silence::TooMany => &mut self.tally.too_many,
            Silence::NoDevice => &mut self.tally.no_device,
        };
        *counter += 1;
        Decision::Now(CueOutcome::Silent(silence))
    }

    /// Remember `key` will not decode, forgetting the oldest past the bound.
    fn remember_refused(&mut self, key: CueKey) {
        if !self.in_force(&key) {
            return;
        }
        if self.refused.len() >= MAX_REFUSED {
            self.refused.remove(0);
        }
        if self.refused.try_reserve(1).is_ok() {
            self.refused.push(key);
        }
    }

    /// Bring the machine's sound document in line with the volume: read
    /// again only when the file changed, keeping the last one read when it
    /// cannot be.
    fn refresh_settings(&mut self) {
        let identity = match self.store.identify(MACHINE_SETTINGS) {
            Ok(identity) => identity,
            Err(err) => {
                self.settings_trouble("the machine's sound document could not be looked at", err);
                return;
            }
        };
        if identity == self.machine.identity {
            return;
        }
        let Some(identity) = identity else {
            self.machine = MachineSettings {
                identity: None,
                settings: SoundSettings::default(),
            };
            self.relayer();
            return;
        };
        let text = match self
            .store
            .read(MACHINE_SETTINGS, identity, MAX_DOCUMENT_LEN)
        {
            Ok(bytes) => bytes,
            Err(err) => {
                self.settings_trouble("the machine's sound document could not be read", err);
                return;
            }
        };
        let document = core::str::from_utf8(&text)
            .map_err(|_| Errno::BadMagic)
            .and_then(|text| Document::parse(text).map_err(|_| Errno::BadMagic));
        let document = match document {
            Ok(document) => document,
            Err(err) => {
                self.settings_trouble("the machine's sound document is malformed", err);
                return;
            }
        };
        let (settings, refused) = SoundSettings::load(&document);
        if !refused.is_empty() {
            self.settings_trouble(
                "the machine's sound document holds a setting it refuses",
                Errno::OutOfRange,
            );
        }
        self.machine = MachineSettings {
            identity: Some(identity),
            settings,
        };
        self.relayer();
    }

    /// Lay the session's pushed document over the machine's again, which
    /// changed beneath it.
    fn relayer(&mut self) {
        let Self { machine, layer, .. } = self;
        let Some(layer) = layer.as_mut() else {
            return;
        };
        if let Ok(settings) = merge_pushed(&machine.settings, &layer.document) {
            layer.settings = settings;
        } else {
            // A strict reading does not depend on the document beneath it, so
            // this is a defect, and the choices go rather than half-apply.
            self.drop_layer();
            self.audit_session(
                events::POLICY_DROPPED,
                Level::Error,
                "a session's sound choices no longer read over the machine's",
                None,
            );
        }
    }

    fn settings_trouble(&self, message: &'static str, err: Errno) {
        log(
            &self.sink,
            &Event {
                level: Level::Warn,
                id: events::SETTINGS_REFUSED,
                message,
                fields: &[Field {
                    key: "error",
                    value: FieldValue::Error(err),
                }],
            },
        );
    }

    /// Record that the sound `source` holds could not be read or decoded: by
    /// its path, or by its event for a file a session handed over, whose path
    /// this service never learns.
    fn undecodable(&self, source: &Source, err: Option<Errno>) {
        let (path, event) = match source {
            Source::Path(path) => (FieldValue::Str(path), FieldValue::Null),
            Source::Handed(event) => (FieldValue::Null, FieldValue::Str(event.name())),
        };
        log(
            &self.sink,
            &Event {
                level: Level::Warn,
                id: events::CUE_UNDECODABLE,
                message: "a sound could not be read or decoded",
                fields: &[
                    Field {
                        key: "path",
                        value: path,
                    },
                    Field {
                        key: "event",
                        value: event,
                    },
                    Field {
                        key: "error",
                        value: err.map_or(FieldValue::Null, FieldValue::Error),
                    },
                ],
            },
        );
    }

    /// Record that the file chosen for `event` could not be looked at.
    fn unreadable_choice(&self, event: SoundEvent, err: Errno) {
        log(
            &self.sink,
            &Event {
                level: Level::Warn,
                id: events::CUE_UNDECODABLE,
                message: "the file chosen for a sound could not be read",
                fields: &[
                    Field {
                        key: "event",
                        value: FieldValue::Str(event.name()),
                    },
                    Field {
                        key: "error",
                        value: FieldValue::Error(err),
                    },
                ],
            },
        );
    }

    /// Record a decision about a session's choices, naming who pushed them
    /// where it is known.
    fn audit_session(
        &self,
        id: tairix_log::EventId,
        level: Level,
        message: &'static str,
        caller: Option<&Origin>,
    ) {
        let (uid, pid) = caller.map_or((FieldValue::Null, FieldValue::Null), |caller| {
            (
                FieldValue::UnsignedInt(u64::from(caller.uid())),
                FieldValue::UnsignedInt(caller.pid()),
            )
        });
        log(
            &self.sink,
            &Event {
                level,
                id,
                message,
                fields: &[
                    Field {
                        key: "uid",
                        value: uid,
                    },
                    Field {
                        key: "pid",
                        value: pid,
                    },
                ],
            },
        );
    }

    fn audit_event(
        &self,
        id: tairix_log::EventId,
        level: Level,
        message: &'static str,
        event: SoundEvent,
    ) {
        log(
            &self.sink,
            &Event {
                level,
                id,
                message,
                fields: &[Field {
                    key: "event",
                    value: FieldValue::Str(event.name()),
                }],
            },
        );
    }

    fn audit_caller(
        &self,
        id: tairix_log::EventId,
        message: &'static str,
        caller: &Origin,
        event: SoundEvent,
    ) {
        log(
            &self.sink,
            &Event {
                level: Level::Warn,
                id,
                message,
                fields: &[
                    Field {
                        key: "uid",
                        value: FieldValue::UnsignedInt(u64::from(caller.uid())),
                    },
                    Field {
                        key: "pid",
                        value: FieldValue::UnsignedInt(caller.pid()),
                    },
                    Field {
                        key: "event",
                        value: FieldValue::Str(event.name()),
                    },
                ],
            },
        );
    }
}

#[cfg(test)]
#[path = "service_tests.rs"]
mod tests;
