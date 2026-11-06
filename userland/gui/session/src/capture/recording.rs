//! A screen recording from the session's side: the recorder it starts, the
//! frames it hands over, and every way a recording ends (`plans/CAPTURE.md`).
//!
//! The recorder is `recorder.app`, over two pipes only the session holds: one
//! the session starts, or, for a recording whose sound the signed-in account
//! may not record, one an administrator allowed through the system's
//! authorisation sheet, which the login broker started as them and handed the
//! session the pipes of. Its file and its slots go to the process the kernel
//! attests — the session's child, or the broker's as the broker read it —
//! never to one the recorder names.
//! The slots are two capture-sized regions of the session's own memory, made
//! once the recorder says it is up and granted to that process alone. The
//! session keeps what changed in tiles, composes their cover into the latest
//! picture, brings the free slot up to date from it, and names the slot in a
//! frame — at most thirty a second, and only when something changed. While
//! both slots are busy, or the last frame is still queued, damage merges into
//! the next frame, so nothing queues and the desktop never waits on the
//! encoder.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

use tairix_abi::elevate::WiredLaunch;
use tairix_abi::recorder_ipc::{
    RecordEnding, RecordSound, RecordStart, RecorderDamage, RecorderMessage, RecorderSlot,
    SessionMessage, RECORDER_DAMAGE_RECTS, RECORDER_FINISH_DEADLINE_NS, RECORDER_FRAME_SPACING_NS,
    RECORDER_MESSAGE_MAX, RECORDER_RUN_PATH, RECORDER_SESSION_MESSAGE_MAX, RECORDER_SLOTS,
    RECORDER_START_DEADLINE_NS,
};
use tairix_abi::time::CivilTime;
use tairix_abi::{load_failure_reason, Errno, ProcId};
use tairix_display::as_damage;
use tairix_geometry::{StaleFrames, Tiles, DAMAGE_TILE};
use tairix_greeter::Ticket;
use tairix_log::{Event, Field, FieldValue, Level, Sink};
use tairix_sandbox::{
    SandboxSession, SessionBounds, SessionDescriptors, SessionError, SessionTransport,
};
use tairix_wallpaper::sound_value;
use tairix_wm::{CaptureError, CaptureTap, Compositor, Rect, Region, Surface};

use super::still::CaptureBuffer;
use super::{
    KeptFolder, RecordPlan, Screened, RECORDING_ENDED, RECORDING_ENDED_MESSAGE,
    RECORDING_REQUESTER, RECORDING_STARTED, RECORDING_STARTED_MESSAGE,
};
use crate::elevate::{Answer, Asker, Elevation, Elevator, Request};
use crate::keep::{
    claim_scratch, folder_path, home_of, kept_folder, settle_scratch, KeptFiles, Unkept,
};
use crate::saver::SaverMemory;
use crate::switchuser::park_within;

/// What the session may hold queued for the recorder: a few frames, so a
/// recorder slow to read refuses the next frame rather than growing a backlog.
const OUTBOUND_BYTES: usize = 4 * (RECORDER_SESSION_MESSAGE_MAX + 4);

/// What the recorder may have said that the session has not read yet.
const INBOUND_BYTES: usize = 8 * (RECORDER_MESSAGE_MAX + 4);

/// How long a recorder gone before it recorded anything is waited for to be
/// reaped: its pipes can close before its exit can be read, and its exit says
/// whether its program ever loaded.
const EXIT_WAIT_NS: u64 = 1_000_000_000;

/// What a recorder is started and its slots granted through.
pub trait RecorderHost {
    /// The recorder's pipes.
    type Transport: SessionTransport;

    /// Start a recorder: its pipes, and its process as the kernel attests the
    /// child started — what its file and its slots are granted to, whatever
    /// the recorder says of itself.
    ///
    /// # Errors
    ///
    /// Why it could not be started.
    fn launch(&mut self) -> Result<(Self::Transport, ProcId), Errno>;

    /// Take up the recorder an administrator allowed, which the login broker
    /// started: its pipes, redeemed from the delegations `launch` carries,
    /// and its process as the broker read it from the kernel.
    ///
    /// # Errors
    ///
    /// Why its pipes could not be taken up.
    fn adopt(&mut self, launch: WiredLaunch) -> Result<(Self::Transport, ProcId), Errno>;

    /// Make a slot of `len` bytes of the session's own memory, mapped here,
    /// and grant it to `recorder` alone: the handle the grant resolves by for
    /// that process, and the slot.
    ///
    /// # Errors
    ///
    /// Why the slot could not be made or mapped, or its grant was refused;
    /// what was made of it is released.
    fn grant_slot(
        &mut self,
        recorder: ProcId,
        len: usize,
    ) -> Result<(u64, Box<dyn SaverMemory>), Errno>;
}

/// Why the session ended a recording.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum StopReason {
    /// The user asked: a chord or the indicator.
    Asked,
    /// Its duration ran out.
    Duration,
    /// The lock, a change of user or the session's end took the screen.
    Screened,
    /// The desktop changed size under it.
    Resized,
}

impl StopReason {
    /// The reason's logged spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Asked => "asked",
            Self::Duration => "duration",
            Self::Screened => "screened",
            Self::Resized => "resized",
        }
    }

    /// Whether what changed since the last frame goes with the end: not once
    /// something took the screen, nor once the desktop changed size.
    const fn keeps_last_changes(self) -> bool {
        matches!(self, Self::Asked | Self::Duration)
    }
}

/// How an ended recording's record names a recording that lost nothing.
pub const NOTHING_LOST: &str = "none";

/// How a recording fell short of what was asked.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Lost {
    /// The disk filled; the file was finished as far as it got.
    DiskFull,
    /// The recorder failed; the file is kept as far as it got.
    RecorderFailed,
    /// The recorder failed before it recorded anything.
    Unrecorded,
    /// The recorder's program could not be loaded, for this reason.
    Unloaded(&'static str),
    /// The recorder refused to start, and why.
    Refused(Errno),
    /// The recorder could not be started, and why.
    Unstarted(Errno),
    /// The recorder did not answer in time.
    Unanswered,
    /// Its file was not made in time.
    OutputLate,
    /// Its file could not be made, and why.
    NoOutput(Unkept),
    /// Nobody allowed the sound its account may not record, so nothing was
    /// started.
    Declined,
}

impl Lost {
    /// The loss's logged spelling.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::DiskFull => "disk-full",
            Self::RecorderFailed => "recorder-failed",
            Self::Unrecorded => "unrecorded",
            Self::Unloaded(_) => "unloaded",
            Self::Refused(_) => "refused",
            Self::Unstarted(_) => "unstarted",
            Self::Unanswered => "unanswered",
            Self::OutputLate => "output-late",
            Self::NoOutput(_) => "no-output",
            Self::Declined => "declined",
        }
    }
}

impl fmt::Display for Lost {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DiskFull => f.write_str("The disk is full, so it was saved as far as it got"),
            Self::RecorderFailed => {
                f.write_str("The recorder stopped unexpectedly, so it was saved as far as it got")
            }
            Self::Unrecorded => f.write_str("The recorder stopped before it recorded anything"),
            Self::Unloaded(reason) => write!(f, "The recorder could not be started: {reason}"),
            Self::Refused(Errno::PermissionDenied) => {
                f.write_str("This account may not record sound")
            }
            Self::Refused(errno) => write!(f, "The recorder refused to start: {errno}"),
            Self::Unstarted(errno) => write!(f, "The recorder could not be started: {errno}"),
            Self::Unanswered => f.write_str("The recorder did not answer"),
            Self::OutputLate => f.write_str("Its file was not made in time"),
            Self::NoOutput(why) => write!(f, "Its file could not be made: {why}"),
            Self::Declined => {
                f.write_str("Sound needs an administrator's permission, so nothing was recorded")
            }
        }
    }
}

/// A recording's file: made in its folder and granted to its recorder.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecordingFile {
    /// The folder it is kept in.
    pub folder: KeptFolder,
    /// The scratch file it is written to until its end names it.
    pub scratch: String,
    /// The handle it was granted to the recorder under.
    pub handle: u64,
}

/// How a recording ended, for the session to settle.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecordEnd {
    /// Why the session ended it, where it did.
    pub reason: Option<StopReason>,
    /// What of it was lost.
    pub lost: Option<Lost>,
    /// The scratch file made for it, once made: named, or removed where it
    /// holds nothing.
    pub scratch: Option<String>,
    /// Whether the recorder ran, so the recording is shown as a thumbnail.
    pub kept: bool,
    /// The folder it was made in.
    pub folder: KeptFolder,
}

/// What a recording asks of the session.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RecordStep {
    /// Make the scratch output in `folder` and grant it to `recorder`,
    /// answering through [`take_output`].
    WantOutput {
        /// The recorder the file is granted to, as the kernel attests it.
        recorder: ProcId,
        /// Where it is made.
        folder: KeptFolder,
    },
    /// It ended.
    Ended(RecordEnd),
    /// A file made for a recording that ended before it landed, never
    /// written: settle it as a recording's end would.
    Unused(RecordingFile),
    /// Ask an administrator, through the system's authorisation sheet, to
    /// allow the sound this recording's account may not record, answering
    /// through [`Recording::authorised`].
    Authorise(Request),
}

/// What came of asking for the authority a recording's sound needs.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Allowed {
    /// The signed-in account may record the sound itself: the recorder is
    /// started as that account, as a silent recording's is.
    Own,
    /// An administrator allowed it, for this one recording: the recorder they
    /// started, wired to the session.
    Administrator(WiredLaunch),
    /// Nobody allowed it: the sheet was cancelled, or could not be put up.
    Declined,
}

/// Whose word a recording with sound waits on before it has a recorder.
#[derive(Copy, Clone, Eq, PartialEq)]
enum Authorising {
    /// The broker's, to whether the account may record the sound itself: the
    /// question this ticket names.
    Own(Ticket),
    /// An administrator's, through the authorisation sheet.
    Administrator,
}

#[derive(Copy, Clone, Eq, PartialEq)]
enum Phase {
    /// No recorder yet: the authority to hear its sound is being asked for.
    /// Nothing is taken and no deadline runs.
    Authorising(Authorising),
    /// Waiting for the recorder to say it is up.
    Greeting,
    /// The output is being made for the recorder.
    Opening,
    /// Started; waiting for the slots.
    Starting,
    Running,
    /// Told to stop; waiting for it to finish.
    Finishing,
    Ended,
}

/// Whether a message went.
enum Sent {
    Yes,
    /// The queue has no room now: tried again once it has.
    Full,
    Failed,
}

/// What came of bringing a slot up to date and naming it in a frame.
enum Framed {
    Sent,
    /// No slot is free, or the last message is still queued: tried again once
    /// a slot is released or the pipe has room.
    Waiting,
    Failed,
}

/// One recording, from the recorder's start to its end.
pub struct Recording<T: SessionTransport, S: Sink + Clone> {
    /// The recorder's pipes, once it has a recorder and until it ends.
    session: Option<SandboxSession<T, S>>,
    /// What those pipes may hold queued, checked before any recorder exists.
    bounds: SessionBounds,
    sink: S,
    plan: RecordPlan,
    /// The recorder's process, as the kernel attests it; the no-process
    /// sentinel until it has one.
    recorder: ProcId,
    slot: RecorderSlot,
    phase: Phase,
    picture: CaptureBuffer,
    /// The slots, the session's own memory granted to the recorder, from its
    /// `Hello` until the recording ends.
    slots: Vec<Box<dyn SaverMemory>>,
    /// Each slot's grant handle, in slot order, as its `Start` carries them.
    granted: [u64; RECORDER_SLOTS],
    busy: [bool; RECORDER_SLOTS],
    stale: StaleFrames,
    /// What the tap handed over last, in desktop pixels.
    changed: Region,
    /// What changed on the desktop since the picture was composed, in tiles
    /// of the area.
    owed: Tiles,
    /// Where the cover of `owed` is worked out.
    room: Tiles,
    /// The cover of `owed` a frame composes.
    composing: Region,
    /// What the picture changed by since the last frame, area-local.
    pending: Region,
    screened: Screened,
    paused: bool,
    /// The scratch file made for it, once made.
    scratch: Option<String>,
    recorded: bool,
    /// What the recorder exited with, as the session's own reap found it.
    exited: Option<i32>,
    /// When a recorder found gone before it recorded anything stops being
    /// waited for to be reaped.
    exit_due_ns: Option<u64>,
    next_frame_ns: u64,
    deadline_ns: Option<u64>,
    ends_ns: Option<u64>,
    stopped: Option<StopReason>,
    out: [u8; RECORDER_SESSION_MESSAGE_MAX],
}

impl<T: SessionTransport, S: Sink + Clone> Recording<T, S> {
    /// Start recording as `plan` says into a folder of `home`, the recorder
    /// launched through `host` and the tap armed at once, so every change from
    /// now is owed.
    ///
    /// # Errors
    ///
    /// [`Lost::NoOutput`] where there is no home to keep it in, found before
    /// any recorder is started, and [`Lost::Unstarted`] where the recorder,
    /// its picture or its tap could not be had; nothing is left running.
    pub fn start<H: RecorderHost<Transport = T>>(
        host: &mut H,
        plan: RecordPlan,
        (sink, screened): (S, Screened),
        home: Option<&str>,
        compositor: &mut Compositor,
        now_ns: u64,
    ) -> Result<Self, Lost> {
        let mut recording = Self::prepared(plan, (sink, screened), home)?;
        let launched = host.launch().map_err(Lost::Unstarted)?;
        recording.begin(launched, compositor, now_ns)?;
        Ok(recording)
    }

    /// The recording `plan` asks for, whose sound its account may not be
    /// allowed to record: no recorder is started and nothing is taken until
    /// [`Self::authorised`] says how it may be. `elevator` is asked at once
    /// whether the account may record the sound itself, and its answer is
    /// taken up by [`Self::answered`].
    ///
    /// # Errors
    ///
    /// As [`Self::start`], before anything is asked.
    pub fn authorising(
        plan: RecordPlan,
        (sink, screened): (S, Screened),
        home: Option<&str>,
        elevator: &mut dyn Elevator,
    ) -> Result<Self, Lost> {
        let mut recording = Self::prepared(plan, (sink, screened), home)?;
        let asked = elevator.authorisers(RECORDER_RUN_PATH);
        recording.phase = Phase::Authorising(Authorising::Own(asked));
        Ok(recording)
    }

    /// Everything a recording of `plan` needs before it has a recorder, each
    /// checked before any recorder is started or anything asked.
    fn prepared(
        plan: RecordPlan,
        (sink, screened): (S, Screened),
        home: Option<&str>,
    ) -> Result<Self, Lost> {
        if home_of(home).is_none() {
            return Err(Lost::NoOutput(Unkept::NoHome));
        }
        let area = plan.area;
        let size = (area.width, area.height);
        let slot = RecorderSlot::new(area.width, area.height).map_err(Lost::Unstarted)?;
        let picture = CaptureBuffer::new(area.width, area.height)
            .ok_or(Lost::Unstarted(Errno::OutOfMemory))?;
        let owed = Tiles::new(size, DAMAGE_TILE).ok_or(Lost::Unstarted(Errno::OutOfMemory))?;
        let room = Tiles::new(size, DAMAGE_TILE).ok_or(Lost::Unstarted(Errno::OutOfMemory))?;
        let bounds = SessionBounds::new(OUTBOUND_BYTES, INBOUND_BYTES)
            .map_err(|_| Lost::Unstarted(Errno::OutOfRange))?;
        let local = Rect::new(0, 0, area.width, area.height);
        Ok(Self {
            session: None,
            bounds,
            sink,
            plan,
            recorder: ProcId::KERNEL,
            slot,
            phase: Phase::Greeting,
            picture,
            slots: Vec::new(),
            granted: [0; RECORDER_SLOTS],
            busy: [false; RECORDER_SLOTS],
            stale: StaleFrames::new(RECORDER_SLOTS, local, RECORDER_DAMAGE_RECTS),
            changed: Region::new(),
            owed,
            room,
            composing: Region::new(),
            pending: Region::new(),
            screened,
            paused: false,
            scratch: None,
            recorded: false,
            exited: None,
            exit_due_ns: None,
            next_frame_ns: 0,
            deadline_ns: None,
            ends_ns: None,
            stopped: None,
            out: [0; RECORDER_SESSION_MESSAGE_MAX],
        })
    }

    /// Begin speaking to `recorder` over `transport` from `now_ns`, the tap
    /// armed at once, so every change from now is owed.
    fn begin(
        &mut self,
        (transport, recorder): (T, ProcId),
        compositor: &mut Compositor,
        now_ns: u64,
    ) -> Result<(), Lost> {
        let session = SandboxSession::new(transport, self.bounds, self.sink.clone())
            .map_err(|_| Lost::Unstarted(Errno::OutOfMemory))?;
        self.session = Some(session);
        self.recorder = recorder;
        self.phase = Phase::Greeting;
        self.next_frame_ns = now_ns;
        self.deadline_ns = Some(now_ns.saturating_add(RECORDER_START_DEADLINE_NS));
        self.arm(compositor)
            .map_err(|_| Lost::Unstarted(Errno::OutOfMemory))
    }

    /// The recorder's pipes, for the wait-set, until it ends.
    #[must_use]
    pub fn descriptors(&self) -> Option<SessionDescriptors> {
        self.session.as_ref()?.descriptors()
    }

    /// Whether the recorder's reply pipe is to be watched.
    #[must_use]
    pub fn wants_read(&self) -> bool {
        self.session
            .as_ref()
            .is_some_and(SandboxSession::wants_read)
    }

    /// Whether the request pipe's room is to be watched.
    #[must_use]
    pub fn wants_write(&self) -> bool {
        self.session
            .as_ref()
            .is_some_and(SandboxSession::wants_write)
    }

    /// Take what the recorder wrote; a fault is seen by the next serve.
    pub fn on_readable(&mut self) {
        if let Some(session) = self.session.as_mut() {
            let _ = session.on_readable();
        }
    }

    /// Write what is queued for the recorder.
    pub fn on_writable(&mut self) {
        if let Some(session) = self.session.as_mut() {
            let _ = session.on_writable();
        }
    }

    /// The latest picture, which the thumbnail after the end is made from.
    #[must_use]
    pub const fn picture(&self) -> &Surface {
        self.picture.surface()
    }

    /// The bytes of slot `index`, as the session last wrote them.
    #[cfg(test)]
    pub(super) fn slot_bytes(&self, index: usize) -> Option<&[u8]> {
        self.slots.get(index).map(|slot| slot.bytes())
    }

    /// Whether it has ended.
    #[must_use]
    pub const fn ended(&self) -> bool {
        matches!(self.phase, Phase::Ended)
    }

    /// Whether it runs and has not been asked to stop: what a chord stops and
    /// the indicator shows. One finishing its file is no longer live, and one
    /// waiting on the authority to hear its sound is not yet.
    #[must_use]
    pub const fn is_live(&self) -> bool {
        self.stopped.is_none() && !self.ended() && !self.is_authorising()
    }

    /// Whether it waits on the authority to hear its sound, with no recorder
    /// yet.
    #[must_use]
    pub const fn is_authorising(&self) -> bool {
        matches!(self.phase, Phase::Authorising(_))
    }

    /// Whether it waits on the broker's answer to the question `ticket`
    /// names.
    #[must_use]
    pub fn asked(&self, ticket: Ticket) -> bool {
        self.phase == Phase::Authorising(Authorising::Own(ticket))
    }

    /// Whether a press rings the pointer while it records.
    #[must_use]
    pub const fn shows_clicks(&self) -> bool {
        self.plan.clicks
    }

    /// Note that the recorder's process exited with `code`, as the session's
    /// own reap of its children found it: what a recorder that failed before
    /// it recorded anything is told by.
    pub fn exited(&mut self, code: i32) {
        self.exited = Some(code);
    }

    /// Take up `answer`, the broker's to whether the account may record the
    /// sound itself: it may, and the recorder is started as it; or it may not,
    /// and an administrator is to be asked. An answer to another question is
    /// none of this recording's. A broker that could not say is taken as a
    /// yes: the kernel and the audio service still refuse what the account
    /// may not do, and the recorder's refusal is stated.
    pub fn answered<H: RecorderHost<Transport = T>>(
        &mut self,
        answer: &Answer,
        host: &mut H,
        compositor: &mut Compositor,
        now_ns: u64,
    ) -> Option<RecordStep> {
        let Answer::Listing(ticket, listing) = answer else {
            return None;
        };
        if !self.asked(*ticket) {
            return None;
        }
        if listing.as_ref().is_none_or(|listing| listing.own.is_some()) {
            return self.authorised(host, Allowed::Own, compositor, now_ns);
        }
        self.phase = Phase::Authorising(Authorising::Administrator);
        Some(RecordStep::Authorise(administrator_request(
            self.plan.sound,
        )))
    }

    /// Take up what came of asking for the recording's sound: the recorder
    /// started as its account, or the one an administrator started wired to
    /// the session — over whose pipes the protocol runs exactly as over its
    /// own child's — or nothing, which ends it with nothing started. An
    /// answer this recording is not waiting on is released.
    pub fn authorised<H: RecorderHost<Transport = T>>(
        &mut self,
        host: &mut H,
        allowed: Allowed,
        compositor: &mut Compositor,
        now_ns: u64,
    ) -> Option<RecordStep> {
        let launched = match (self.phase, allowed) {
            (Phase::Authorising(_), Allowed::Declined) => {
                return Some(self.end(Some(Lost::Declined), compositor));
            }
            (Phase::Authorising(Authorising::Own(_)), Allowed::Own) => host.launch(),
            (Phase::Authorising(Authorising::Administrator), Allowed::Administrator(launch)) => {
                host.adopt(launch)
            }
            (_, allowed) => {
                release_allowed(host, allowed);
                return None;
            }
        };
        match launched
            .map_err(Lost::Unstarted)
            .and_then(|launched| self.begin(launched, compositor, now_ns))
        {
            Ok(()) => None,
            Err(lost) => Some(self.end(Some(lost), compositor)),
        }
    }

    /// Serve the recording at `now_ns`: what the recorder said, its
    /// deadlines, and the next frame when one is due.
    pub fn serve<H: RecorderHost<Transport = T>>(
        &mut self,
        host: &mut H,
        compositor: &mut Compositor,
        now_ns: u64,
    ) -> Option<RecordStep> {
        if self.ended() || self.is_authorising() {
            return None;
        }
        while let Some(session) = self.session.as_mut() {
            match session.recv(RecorderMessage::decode) {
                Ok(None) => break,
                Ok(Some(Ok(message))) => {
                    if let Some(step) = self.heard(message, host, compositor, now_ns) {
                        return Some(step);
                    }
                }
                Ok(Some(Err(_))) | Err(_) => {
                    return Some(self.end(Some(Lost::RecorderFailed), compositor));
                }
            }
        }
        if self
            .session
            .as_ref()
            .is_some_and(SandboxSession::peer_finished)
        {
            let due = *self
                .exit_due_ns
                .get_or_insert(now_ns.saturating_add(EXIT_WAIT_NS));
            if self.recorded || self.exited.is_some() || now_ns >= due {
                return Some(self.end(Some(Lost::RecorderFailed), compositor));
            }
            return None;
        }
        if self.deadline_ns.is_some_and(|deadline| now_ns >= deadline) {
            let lost = if self.phase == Phase::Opening {
                Lost::OutputLate
            } else {
                Lost::Unanswered
            };
            return Some(self.end(Some(lost), compositor));
        }
        if self.ends_ns.is_some_and(|ends| now_ns >= ends) {
            if let Some(step) = self.stop(StopReason::Duration, compositor, now_ns) {
                return Some(step);
            }
        }
        if self.stopped.is_none() && !self.gather(compositor) {
            return self.stop(StopReason::Resized, compositor, now_ns);
        }
        match (self.phase, self.stopped) {
            (Phase::Running, None) => self.take_frame(compositor, now_ns),
            (Phase::Running, Some(_)) => self.finish(compositor, now_ns),
            _ => None,
        }
    }

    /// End the recording for `reason`: no more frames are taken but for what
    /// changed since the last, which goes with an end the user or the duration
    /// asked for, and the recorder finishes its file. One never started ends
    /// at once.
    pub fn stop(
        &mut self,
        reason: StopReason,
        compositor: &mut Compositor,
        now_ns: u64,
    ) -> Option<RecordStep> {
        if self.stopped.is_some() || self.ended() {
            return None;
        }
        let last = self.phase == Phase::Running && !self.paused && reason.keeps_last_changes();
        if last && self.gather(compositor) {
            self.compose(compositor);
        }
        self.stopped = Some(reason);
        // One still waiting on the authority to hear its sound has no tap.
        if !self.is_authorising() {
            compositor.disarm_capture();
        }
        self.ends_ns = None;
        match self.phase {
            Phase::Authorising(_) | Phase::Greeting => Some(self.end(None, compositor)),
            Phase::Opening | Phase::Finishing | Phase::Ended => None,
            Phase::Starting => match self.send(&SessionMessage::Stop) {
                Sent::Yes => {
                    self.phase = Phase::Finishing;
                    self.deadline_ns = Some(now_ns.saturating_add(RECORDER_FINISH_DEADLINE_NS));
                    None
                }
                Sent::Full | Sent::Failed => Some(self.end(Some(Lost::RecorderFailed), compositor)),
            },
            Phase::Running => {
                self.deadline_ns = Some(now_ns.saturating_add(RECORDER_FINISH_DEADLINE_NS));
                self.finish(compositor, now_ns)
            }
        }
    }

    /// Show `screened` blanked from now on, and take no frames while
    /// `paused` — the screensaver is up. A change of what is blanked is
    /// re-armed, which owes the whole picture again; a tap that cannot be
    /// re-armed stops the recording, and what that asks is answered.
    pub fn screen(
        &mut self,
        screened: Screened,
        paused: bool,
        compositor: &mut Compositor,
        now_ns: u64,
    ) -> Option<RecordStep> {
        self.paused = paused;
        if screened == self.screened || self.stopped.is_some() || self.ended() {
            return None;
        }
        self.screened = screened;
        // One with no recorder yet is armed as it gets one.
        if self.is_authorising() || self.arm(compositor).is_ok() {
            return None;
        }
        self.stop(StopReason::Screened, compositor, now_ns)
    }

    /// The park the recording needs: its deadlines, and the next frame while
    /// something changed and a frame can go. Nothing changing arms nothing,
    /// and nor does a frame waiting on a slot or on room in the pipe: the
    /// release and the room wake the loop.
    #[must_use]
    pub fn park_deadline_ns(&self, now_ns: u64, park_ns: u64) -> u64 {
        let frame = (self.phase == Phase::Running
            && self.stopped.is_none()
            && !self.paused
            && self.owes()
            && self.can_frame())
        .then_some(self.next_frame_ns);
        // An exit waited for that has been reaped is served at once.
        let exit = self
            .exit_due_ns
            .map(|due| if self.exited.is_some() { now_ns } else { due });
        [self.deadline_ns, self.ends_ns, exit, frame]
            .into_iter()
            .flatten()
            .fold(park_ns, |park, due| {
                park_within(park, Some(due.saturating_sub(now_ns)))
            })
    }

    /// Whether it waits on a file made for `recorder`.
    fn awaits(&self, recorder: ProcId) -> bool {
        self.phase == Phase::Opening && self.recorder == recorder
    }

    /// Take the file the host made, or why it could not be made: the recording
    /// starts on it, or, stopped meanwhile, ends with it made.
    fn output(
        &mut self,
        made: Result<RecordingFile, Unkept>,
        compositor: &mut Compositor,
        now_ns: u64,
    ) -> Option<RecordStep> {
        let file = match made {
            Ok(file) => file,
            Err(why) => return Some(self.end(Some(Lost::NoOutput(why)), compositor)),
        };
        self.scratch = Some(file.scratch);
        if self.stopped.is_some() {
            return Some(self.end(None, compositor));
        }
        let start = SessionMessage::Start(RecordStart {
            slot: self.slot,
            sound: self.plan.sound,
            output: file.handle,
            slots: self.granted,
            started_ns: now_ns,
        });
        match self.send(&start) {
            Sent::Yes => {
                self.phase = Phase::Starting;
                self.deadline_ns = Some(now_ns.saturating_add(RECORDER_START_DEADLINE_NS));
                None
            }
            Sent::Full | Sent::Failed => Some(self.end(Some(Lost::RecorderFailed), compositor)),
        }
    }

    fn heard<H: RecorderHost<Transport = T>>(
        &mut self,
        message: RecorderMessage,
        host: &mut H,
        compositor: &mut Compositor,
        now_ns: u64,
    ) -> Option<RecordStep> {
        match (message, self.phase) {
            (RecorderMessage::Hello, Phase::Greeting) => {
                if let Err(errno) = self.grant_slots(host) {
                    return Some(self.end(Some(Lost::Unstarted(errno)), compositor));
                }
                self.phase = Phase::Opening;
                Some(RecordStep::WantOutput {
                    recorder: self.recorder,
                    folder: self.plan.folder,
                })
            }
            (RecorderMessage::Ready, Phase::Starting) => {
                self.phase = Phase::Running;
                self.recorded = true;
                self.deadline_ns = None;
                self.ends_ns = self
                    .plan
                    .duration_ns
                    .map(|duration| now_ns.saturating_add(duration));
                self.log_started();
                None
            }
            // A start that crossed the session's `Stop`: nothing more is
            // framed, and its end follows.
            (RecorderMessage::Ready, Phase::Finishing) if !self.recorded => None,
            (RecorderMessage::Released { slot }, Phase::Running | Phase::Finishing) => {
                match self.busy.get_mut(usize::from(slot)) {
                    Some(busy) => {
                        *busy = false;
                        None
                    }
                    None => Some(self.end(Some(Lost::RecorderFailed), compositor)),
                }
            }
            (RecorderMessage::Finished { ending, .. }, Phase::Running | Phase::Finishing) => {
                let lost = (ending == RecordEnding::DiskFull).then_some(Lost::DiskFull);
                Some(self.end(lost, compositor))
            }
            (RecorderMessage::Unstarted(errno), Phase::Starting | Phase::Finishing)
                if !self.recorded =>
            {
                Some(self.end(Some(Lost::Refused(errno)), compositor))
            }
            _ => Some(self.end(Some(Lost::RecorderFailed), compositor)),
        }
    }

    /// Make the slots and grant each to the recorder alone, keeping the
    /// handles its `Start` carries. A refusal keeps none of them: every slot
    /// made before it is released.
    fn grant_slots<H: RecorderHost<Transport = T>>(&mut self, host: &mut H) -> Result<(), Errno> {
        let mut slots = Vec::new();
        slots
            .try_reserve_exact(RECORDER_SLOTS)
            .map_err(|_| Errno::OutOfMemory)?;
        let mut granted = [0; RECORDER_SLOTS];
        for handle in &mut granted {
            let (minted, slot) = host.grant_slot(self.recorder, self.slot.len())?;
            *handle = minted;
            slots.push(slot);
        }
        self.slots = slots;
        self.granted = granted;
        Ok(())
    }

    /// Take what the tap was owed into what the picture owes, answering
    /// whether a tap is still armed: none is once the desktop changed size.
    fn gather(&mut self, compositor: &mut Compositor) -> bool {
        if !compositor.take_capture_changes(&mut self.changed) {
            return false;
        }
        let origin = self.plan.area.origin;
        for rect in self.changed.rects() {
            self.owed.mark(Rect::new(
                rect.left().saturating_sub(origin.x),
                rect.top().saturating_sub(origin.y),
                rect.width,
                rect.height,
            ));
        }
        true
    }

    /// When a frame is due, something changed and a frame can go, compose
    /// what changed into the picture and name a slot brought up to date from
    /// it. A picture that cannot be composed is tried again a frame later.
    fn take_frame(&mut self, compositor: &mut Compositor, now_ns: u64) -> Option<RecordStep> {
        if self.paused || now_ns < self.next_frame_ns || !self.owes() || !self.can_frame() {
            return None;
        }
        if !self.compose(compositor) {
            self.next_frame_ns = now_ns.saturating_add(RECORDER_FRAME_SPACING_NS);
            return None;
        }
        match self.frame(now_ns) {
            Framed::Sent | Framed::Waiting => None,
            Framed::Failed => Some(self.end(Some(Lost::RecorderFailed), compositor)),
        }
    }

    /// Compose the cover of what the picture owes into it and add it to what
    /// the next frame names; `false`, all of it still owed, where composing
    /// was refused.
    fn compose(&mut self, compositor: &mut Compositor) -> bool {
        if self.owed.is_empty() {
            return true;
        }
        self.composing.clear();
        self.owed
            .cover(RECORDER_DAMAGE_RECTS, &mut self.room, &mut self.composing);
        let origin = self.plan.area.origin;
        self.composing.translate(origin.x, origin.y);
        let composed = compositor.compose_capture(self.picture.surface_mut(), &self.composing);
        self.composing.translate(-origin.x, -origin.y);
        if composed.is_err() {
            return false;
        }
        self.pending.add_region(&self.composing);
        self.owed.clear();
        true
    }

    /// Bring a free slot up to date with the picture and name it in a frame
    /// shown at `now_ns`; nothing left to name is as good as sent.
    fn frame(&mut self, now_ns: u64) -> Framed {
        if !self.can_frame() {
            return Framed::Waiting;
        }
        let Some(index) = self.busy.iter().position(|busy| !busy) else {
            return Framed::Waiting;
        };
        let Some(damage) =
            RecorderDamage::fitted(self.pending.rects().iter().filter_map(as_damage))
        else {
            return Framed::Sent;
        };
        let Ok(at) = u8::try_from(index) else {
            return Framed::Failed;
        };
        let stride = self.slot.stride();
        let (picture, slots, pending) = (self.picture.surface(), &mut self.slots, &self.pending);
        let Some(slot) = slots.get_mut(index) else {
            return Framed::Failed;
        };
        let copied = self
            .stale
            .bring_up_to_date(index, pending, Errno::NotFound, |refresh| {
                copy_rects(picture, slot.bytes_mut(), stride, refresh)
            });
        if copied.is_err() {
            return Framed::Failed;
        }
        let frame = SessionMessage::Frame {
            slot: at,
            at_ns: now_ns,
            damage,
        };
        match self.send(&frame) {
            Sent::Yes => {
                if let Some(busy) = self.busy.get_mut(index) {
                    *busy = true;
                }
                self.pending.clear();
                self.next_frame_ns = now_ns.saturating_add(RECORDER_FRAME_SPACING_NS);
                Framed::Sent
            }
            Sent::Full => Framed::Waiting,
            Sent::Failed => Framed::Failed,
        }
    }

    /// End a recording stopped while it ran: what changed since the last frame
    /// goes as a last frame once a slot is free, then `Stop`.
    fn finish(&mut self, compositor: &mut Compositor, now_ns: u64) -> Option<RecordStep> {
        if !self.pending.is_empty() {
            match self.frame(now_ns) {
                Framed::Sent => {}
                Framed::Waiting => return None,
                Framed::Failed => return Some(self.end(Some(Lost::RecorderFailed), compositor)),
            }
        }
        match self.send(&SessionMessage::Stop) {
            Sent::Yes => {
                self.phase = Phase::Finishing;
                None
            }
            Sent::Full => None,
            Sent::Failed => Some(self.end(Some(Lost::RecorderFailed), compositor)),
        }
    }

    /// Whether a frame can go now: a slot is free, and the request pipe holds
    /// nothing still to be written, so frames never queue behind one another.
    fn can_frame(&self) -> bool {
        self.busy.contains(&false)
            && self
                .session
                .as_ref()
                .is_some_and(|session| !session.wants_write())
    }

    /// Whether anything is owed a frame.
    fn owes(&self) -> bool {
        !self.owed.is_empty() || !self.pending.is_empty()
    }

    /// Arm the tap over the plan's area, showing only its window where it
    /// records one and blanking what is screened.
    fn arm(&self, compositor: &mut Compositor) -> Result<(), CaptureError> {
        compositor.arm_capture(CaptureTap {
            area: self.plan.area,
            only: self.plan.window,
            exclude: Vec::new(),
            blank: self.screened.windows().ok_or(CaptureError::OutOfMemory)?,
            pointer: self.plan.pointer,
        })
    }

    fn send(&mut self, message: &SessionMessage) -> Sent {
        let Ok(len) = message.encode(&mut self.out) else {
            return Sent::Failed;
        };
        let Some(session) = self.session.as_mut() else {
            return Sent::Failed;
        };
        match session.send(&self.out[..len]) {
            Ok(()) => Sent::Yes,
            Err(SessionError::OutboundFull) => Sent::Full,
            Err(_) => Sent::Failed,
        }
    }

    fn end(&mut self, lost: Option<Lost>, compositor: &mut Compositor) -> RecordStep {
        if self.stopped.is_none() && !self.is_authorising() {
            compositor.disarm_capture();
        }
        self.phase = Phase::Ended;
        self.deadline_ns = None;
        self.ends_ns = None;
        self.exit_due_ns = None;
        self.slots.clear();
        let code = self.session.take().and_then(SandboxSession::end);
        let end = RecordEnd {
            reason: self.stopped,
            lost: self.explained(lost, self.exited.or(code)),
            scratch: self.scratch.take(),
            kept: self.recorded,
            folder: self.plan.folder,
        };
        let level = if end.lost.is_some() {
            Level::Warn
        } else {
            Level::Info
        };
        tairix_log::log(
            &self.sink,
            &Event {
                level,
                id: RECORDING_ENDED,
                message: RECORDING_ENDED_MESSAGE,
                fields: &[
                    Field {
                        key: "reason",
                        value: FieldValue::Str(end.reason.map_or("recorder", StopReason::as_str)),
                    },
                    Field {
                        key: "lost",
                        value: FieldValue::Str(
                            end.lost.as_ref().map_or(NOTHING_LOST, Lost::as_str),
                        ),
                    },
                ],
            },
        );
        RecordStep::Ended(end)
    }

    /// `lost` as told of a recorder that exited with `code`: one that failed
    /// before it recorded anything kept nothing, and one whose program never
    /// loaded says why.
    fn explained(&self, lost: Option<Lost>, code: Option<i32>) -> Option<Lost> {
        match lost {
            Some(Lost::RecorderFailed) if !self.recorded => Some(
                code.and_then(load_failure_reason)
                    .map_or(Lost::Unrecorded, Lost::Unloaded),
            ),
            other => other,
        }
    }

    fn log_started(&self) {
        tairix_log::log(
            &self.sink,
            &Event {
                level: Level::Info,
                id: RECORDING_STARTED,
                message: RECORDING_STARTED_MESSAGE,
                fields: &[
                    Field {
                        key: "target",
                        value: FieldValue::Str(self.plan.target.as_str()),
                    },
                    Field {
                        key: "sound",
                        value: FieldValue::Str(sound_value(self.plan.sound)),
                    },
                ],
            },
        );
    }
}

/// Hand `made`, a file made for `made_for`, to `recording` where it is the one
/// that recording waits on: what it asks next. One no recording waits on —
/// made for a recording that ended before it landed — was never written, and
/// is answered as [`RecordStep::Unused`] for its file to be settled.
pub fn take_output<T: SessionTransport, S: Sink + Clone>(
    recording: Option<&mut Recording<T, S>>,
    (made_for, made): (ProcId, Result<RecordingFile, Unkept>),
    compositor: &mut Compositor,
    now_ns: u64,
) -> Option<RecordStep> {
    match recording.filter(|recording| recording.awaits(made_for)) {
        Some(recording) => recording.output(made, compositor, now_ns),
        None => made.ok().map(RecordStep::Unused),
    }
}

/// What the system's authorisation sheet asks for a recording that hears
/// `sound`: an administrator's leave to run the recorder as them, for that one
/// recording, wired to the session.
#[must_use]
pub fn administrator_request(sound: RecordSound) -> Request {
    let purpose = match (sound.microphone(), sound.desktop()) {
        (true, true) => "wants to record the microphone and the sound this computer plays.",
        (true, false) => "wants to record the microphone.",
        (false, true) => "wants to record the sound this computer plays.",
        (false, false) => "wants to record the screen.",
    };
    Request {
        program: String::from(RECORDER_RUN_PATH),
        elevation: Elevation::Wired,
        requester: String::from(RECORDING_REQUESTER),
        bundle: tairix_appstore::bundle_of_entry(RECORDER_RUN_PATH).map(String::from),
        purpose: String::from(purpose),
        asker: Some(Asker::Recording),
    }
}

/// Release `allowed`, which the recording does not wait on: an administrator's
/// recorder is taken up only to be closed, which ends it.
fn release_allowed<H: RecorderHost>(host: &mut H, allowed: Allowed) {
    if let Allowed::Administrator(launch) = allowed {
        if let Ok((transport, _)) = host.adopt(launch) {
            let _ = transport.dispose();
        }
    }
}

/// Claim the scratch file a recording kept in `folder` under `home` is written
/// to, every folder on the way made and the file made afresh by `create`,
/// which is handed `files` and the path: the path claimed, and what `create`
/// answered. A scratch file already there — an earlier recording whose naming
/// failed, which may be its only copy — is never taken over.
///
/// # Errors
///
/// Why the folders or the file could not be made.
pub fn claim_recording<F>(
    files: &mut dyn KeptFiles,
    home: Option<&str>,
    folder: KeptFolder,
    mut create: impl FnMut(&mut dyn KeptFiles, &str) -> Result<F, Errno>,
) -> Result<(String, F), Unkept> {
    let keeping = folder.keeping(true);
    let path = kept_folder(files, home, &keeping)?;
    claim_scratch(&path, keeping.ending, |scratch| {
        create(&mut *files, scratch)
    })
}

/// Settle `scratch`, the file a recording kept in `folder` under `home` was
/// written to: give it its name for `when`, or remove it where it holds
/// nothing — a recorder that ended before it wrote. The path it took, if it
/// took one.
///
/// # Errors
///
/// Why it could not be named. It is then left in its scratch file, which the
/// next session start names.
pub fn settle_recording(
    files: &mut dyn KeptFiles,
    home: Option<&str>,
    (folder, scratch): (KeptFolder, &str),
    when: Option<CivilTime>,
) -> Result<Option<String>, Unkept> {
    let keeping = folder.keeping(true);
    let home = home_of(home).ok_or(Unkept::NoHome)?;
    let path = folder_path(home, keeping.folders);
    settle_scratch(files, (&path, scratch), &keeping, |_| when)
}

/// Copy `region` of `picture` into `slot`, rows `stride` bytes apart.
fn copy_rects(
    picture: &Surface,
    slot: &mut [u8],
    stride: usize,
    region: &Region,
) -> Result<(), Errno> {
    for rect in region.rects() {
        let (Ok(x), Ok(top)) = (u32::try_from(rect.left()), u32::try_from(rect.top())) else {
            return Err(Errno::OutOfRange);
        };
        for y in top..top.saturating_add(rect.height) {
            let Some((from, span)) = picture.row_span(y, x, rect.width) else {
                return Err(Errno::OutOfRange);
            };
            let start = usize::try_from(y)
                .ok()
                .and_then(|y| y.checked_mul(stride))
                .and_then(|row| row.checked_add(usize::try_from(from).ok()?.checked_mul(4)?))
                .ok_or(Errno::LengthOutOfRange)?;
            let end = start
                .checked_add(span.len().checked_mul(4).ok_or(Errno::LengthOutOfRange)?)
                .ok_or(Errno::LengthOutOfRange)?;
            let row = slot.get_mut(start..end).ok_or(Errno::LengthOutOfRange)?;
            for (out, pixel) in row.as_chunks_mut::<4>().0.iter_mut().zip(span) {
                *out = [pixel.r, pixel.g, pixel.b, pixel.a];
            }
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "recording_tests.rs"]
mod tests;
