//! The service thread: the conversation with the session, the slots, the
//! sound, and the work handed to the encoding thread.
//!
//! The conversation runs in one order (`tairix_abi::recorder_ipc`): `Hello`,
//! the session's `Start` with the slots' grants, `Ready` once they are mapped,
//! a `Released` for each frame once it is coded, and `Finished` for the
//! session's `Stop` or a full volume. What the recorder cannot begin on is
//! answered `Unstarted` with its reason. Once it records, a frame naming a
//! rectangle outside its slot, a slot still being encoded or an instant before
//! the frame before it, and anything said out of turn, ends the recording: the
//! file is finished as far as it got, the reason is stated, and no `Finished`
//! follows, so the session hears the recorder fail. A frame that arrives once
//! the recording has ended of itself is released uncoded, so the session never
//! waits on a slot.

use core::fmt;

use alloc::vec::Vec;

use tairix_abi::audio::CaptureSource;
use tairix_abi::driver::audio::ChannelMap;
use tairix_abi::recorder_ipc::{
    RecordEnding, RecordSound, RecordStart, RecorderDamage, RecorderMessage, RecorderSlot,
    SessionMessage, RECORDER_MESSAGE_MAX, RECORDER_SESSION_MESSAGE_MAX, RECORDER_SLOTS,
};
use tairix_abi::{Errno, ProcId, PAGE_SIZE};
use tairix_log::{Event, EventId, Field, FieldValue, Level, Sink};
use tairix_parallel::JobRunner;
use tairix_sandbox::proto::FRAME_HEADER_LEN;
use tairix_sandbox::{
    SandboxSession, SessionBounds, SessionDescriptors, SessionError, SessionTransport,
};
use tairix_util::fallible;
use tairix_video::vp8::{self, Quality, Vp8Error};
use tairix_video::webm::Layout;

use crate::encode::{
    frame_room, Answer, Closed, Coded, Encoding, Failure, Halt, Job, Output, SlotMemory,
};
use crate::sound::{self, Sound, SoundSource};
use crate::{FRAME_DROPPED, RECORDER_ENDED, RECORDER_REFUSED, RECORDER_UNSTARTED, SOUND_LOST};

/// What the recorder may hold queued for the session: well past the few
/// messages it ever has outstanding.
const OUTBOUND_BYTES: usize = 8 * (RECORDER_MESSAGE_MAX + FRAME_HEADER_LEN);

/// What the session may have sent that the recorder has not read: twice what
/// the session itself holds queued.
const INBOUND_BYTES: usize = 8 * (RECORDER_SESSION_MESSAGE_MAX + FRAME_HEADER_LEN);

/// The keyframes the index reserved at a file's start names: an hour's.
const INDEXED_KEYFRAMES: u32 = 360;

/// The session a recorder records for, as its spawner names it: the one
/// operand, that instance in lowercase hexadecimal. Its slots map only as that
/// instance's grants, so the recorder never takes a pipe peer's word for whose
/// memory it reads. `None` for any other command line, the kernel's sentinel
/// among them.
#[must_use]
pub fn client_of(operands: &[&str]) -> Option<ProcId> {
    match operands {
        [client] => ProcId::from_hex(client.as_bytes()).filter(|id| !id.is_kernel()),
        _ => None,
    }
}

/// The exit status of a recorder that answered `Finished`.
pub const EXIT_FINISHED: i32 = 0;

/// The exit status of one that ended any other way, its reason stated.
pub const EXIT_ENDED: i32 = 1;

/// Where the encoding thread takes its work from.
pub trait Desk {
    /// Hand `job` over.
    ///
    /// # Errors
    ///
    /// [`Errno::LimitExceeded`] where no more is taken; the job is let go.
    fn submit(&mut self, job: Job) -> Result<(), Errno>;

    /// The oldest answer that has landed.
    fn collect(&mut self) -> Option<Answer>;
}

/// Everything the recorder reaches outside itself.
pub trait Host {
    /// The session's delegated output.
    type Output: Output;
    /// A slot's memory.
    type Slot: SlotMemory;
    /// A capture stream.
    type Sound: SoundSource;
    /// The encoding thread's desk.
    type Desk: Desk;
    /// The cores a frame is coded across.
    type Runner: JobRunner + Send;

    /// The monotonic clock, in nanoseconds.
    fn now_ns(&self) -> u64;

    /// The bytes this process may map, which a recording's footprint is held
    /// to.
    fn memory_room(&self) -> u64;

    /// Redeem the output the session delegated under `handle`.
    ///
    /// # Errors
    ///
    /// The refusal of the redemption.
    fn open_output(&mut self, handle: u64) -> Result<Self::Output, Errno>;

    /// Map the slot the session granted this process under `handle`, at
    /// least `len` bytes of it.
    ///
    /// # Errors
    ///
    /// The refusal of the mapping.
    fn map_slot(&mut self, handle: u64, len: usize) -> Result<Self::Slot, Errno>;

    /// Open a capture of `source` in `layout`, F32 at [`sound::RATE`].
    ///
    /// # Errors
    ///
    /// The audio service's refusal.
    fn open_sound(
        &mut self,
        source: CaptureSource,
        layout: ChannelMap,
    ) -> Result<Self::Sound, Errno>;

    /// The cores a recording's frames are coded across, the encoding thread
    /// one of them.
    fn runner(&mut self) -> Self::Runner;

    /// Hand `encoding` to the encoding thread, answering the desk it takes its
    /// work from.
    ///
    /// # Errors
    ///
    /// [`Errno::OutOfMemory`] where the desk cannot be made.
    fn start_encoding(
        &mut self,
        encoding: Encoding<Self::Output, Self::Slot, Self::Runner>,
    ) -> Result<Self::Desk, Errno>;
}

/// A reason as it is stated: what happened, and the error beneath it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Said {
    /// What happened.
    pub what: &'static str,
    /// The error beneath it, where there is one.
    pub errno: Option<Errno>,
}

impl fmt::Display for Said {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.errno {
            Some(errno) => write!(f, "{}: {errno}", self.what),
            None => f.write_str(self.what),
        }
    }
}

/// How the recorder's process ends.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Exit {
    /// Its exit status.
    pub code: i32,
    /// Why it ended, where it ended other than as asked: what standard error
    /// says.
    pub reason: Option<Said>,
}

/// Why a recording ended other than as the session asked.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Reason {
    Undecodable(Errno),
    OutOfTurn,
    Unknown,
    Busy,
    Outside,
    Backwards,
    Desk,
    Orphaned,
    Halted(Failure),
    Unfinished(Failure),
}

impl Reason {
    /// The reason, as it is stated.
    const fn said(self) -> Said {
        let (what, errno) = match self {
            Self::Undecodable(errno) => (
                "the session sent a message that does not decode",
                Some(errno),
            ),
            Self::OutOfTurn => ("the session spoke out of turn", None),
            Self::Unknown => ("a frame named a slot the recorder never granted", None),
            Self::Busy => ("a frame named a slot still being encoded", None),
            Self::Outside => ("a frame named a rectangle outside its slot", None),
            Self::Backwards => ("a frame was shown before the one before it", None),
            Self::Desk => ("the encoding thread took no more work", None),
            Self::Orphaned => (
                "the session closed its pipe before the recording finished",
                None,
            ),
            Self::Halted(failure) => (failure.what(), failure.errno()),
            Self::Unfinished(failure) => ("the file could not be finished", failure.errno()),
        };
        Said { what, errno }
    }
}

/// How a recording is ending.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum End {
    /// The session asked: `Finished` answers it.
    Asked,
    /// The volume filled: `Finished` says so.
    DiskFull,
    /// It cannot go on, for this reason, and nothing more is said of it.
    Failed(Reason),
}

impl End {
    /// The end that stands once `later` follows this one: a failure over a
    /// full disk over the session's asking, and the first failure kept.
    const fn then(self, later: Self) -> Self {
        match (self, later) {
            (Self::Failed(_), _) => self,
            (_, Self::Failed(_)) => later,
            (Self::DiskFull, _) | (_, Self::DiskFull) => Self::DiskFull,
            (Self::Asked, Self::Asked) => Self::Asked,
        }
    }
}

/// Why a recording could not begin: what the session is answered, the error
/// beneath it, and what is stated.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct Unstart {
    answer: Errno,
    cause: Errno,
    what: &'static str,
}

impl Unstart {
    /// A recording whose buffers the process may not have.
    const MEMORY: Self = Self::new(
        Errno::OutOfMemory,
        "a recording of this size needs more memory than the recorder may have",
    );

    const fn new(errno: Errno, what: &'static str) -> Self {
        Self {
            answer: errno,
            cause: errno,
            what,
        }
    }

    /// The audio service's refusal: for want of the capture capability or of
    /// the room, it is the account's not being allowed to record sound.
    const fn sound(errno: Errno) -> Self {
        match errno {
            Errno::PermissionDenied | Errno::SeatNotOwner => Self {
                answer: Errno::PermissionDenied,
                cause: errno,
                what: "the audio service refused the sound: this account may not record it, or its session does not hold the room",
            },
            _ => Self::new(errno, "the sound could not be opened"),
        }
    }

    const fn coder(error: Vp8Error) -> Self {
        match error {
            Vp8Error::OutOfMemory => Self::MEMORY,
            _ => Self::new(
                Errno::OutOfRange,
                "the recorded area is a size VP8 cannot carry",
            ),
        }
    }
}

/// The session's two pipes.
struct Link<T: SessionTransport, S: Sink> {
    /// `None` once they have failed.
    session: Option<SandboxSession<T, S>>,
    out: [u8; RECORDER_MESSAGE_MAX],
}

impl<T: SessionTransport, S: Sink> Link<T, S> {
    /// Queue `message` for the session. One its pipe has no room for is not
    /// heard: the session has stopped reading, and its own deadline ends the
    /// recording.
    fn say(&mut self, message: &RecorderMessage) {
        let Ok(len) = message.encode(&mut self.out) else {
            return;
        };
        if let Some(session) = self.session.as_mut() {
            let _ = session.send(self.out.get(..len).unwrap_or_default());
        }
    }

    /// Whether nothing is left to write.
    fn quiet(&self) -> bool {
        self.session
            .as_ref()
            .is_none_or(|session| !session.wants_write())
    }
}

/// Where the conversation stands.
// One per process, held for its life: boxing the recording would be an
// allocation that cannot be refused.
#[allow(clippy::large_enum_variant)]
enum Phase<H: Host> {
    /// `Hello` said: the session's `Start` is awaited.
    Greeting,
    /// Recording.
    Recording(Recording<H>),
    /// Over, ending as this says once what is queued for the session has gone.
    Over(Exit),
}

/// The recorder's side of the conversation, and the recording it makes.
pub struct Recorder<T: SessionTransport, S: Sink + Clone, H: Host> {
    link: Link<T, S>,
    sink: S,
    phase: Phase<H>,
}

impl<T: SessionTransport, S: Sink + Clone, H: Host> Recorder<T, S, H> {
    /// The recorder over the session's pipes `transport`, its `Hello` queued.
    ///
    /// # Errors
    ///
    /// [`SessionError::OutOfMemory`] where the pipes' queues cannot be
    /// committed.
    pub fn new(transport: T, sink: S) -> Result<Self, SessionError> {
        let bounds = SessionBounds::new(OUTBOUND_BYTES, INBOUND_BYTES)?;
        let session = SandboxSession::new(transport, bounds, sink.clone())?;
        let mut link = Link {
            session: Some(session),
            out: [0; RECORDER_MESSAGE_MAX],
        };
        link.say(&RecorderMessage::Hello);
        Ok(Self {
            link,
            sink,
            phase: Phase::Greeting,
        })
    }

    /// The session's pipes, for the wait-set, while they are open.
    #[must_use]
    pub fn descriptors(&self) -> Option<SessionDescriptors> {
        self.link.session.as_ref()?.descriptors()
    }

    /// Whether the session's pipe is to be read.
    #[must_use]
    pub fn wants_read(&self) -> bool {
        self.link
            .session
            .as_ref()
            .is_some_and(SandboxSession::wants_read)
    }

    /// Whether the pipe to the session is to be written.
    #[must_use]
    pub fn wants_write(&self) -> bool {
        self.link
            .session
            .as_ref()
            .is_some_and(SandboxSession::wants_write)
    }

    /// Read what the session wrote; a failure is seen by the next serve.
    pub fn on_readable(&mut self) {
        if let Some(session) = self.link.session.as_mut() {
            let _ = session.on_readable();
        }
    }

    /// Write what is queued for the session.
    pub fn on_writable(&mut self) {
        if let Some(session) = self.link.session.as_mut() {
            let _ = session.on_writable();
        }
    }

    /// Serve everything that has happened: what the session said, the sound
    /// that arrived, and what the encoding thread answered. `Some` once the
    /// process is to end, with how.
    pub fn serve(&mut self, host: &mut H) -> Option<Exit> {
        self.hear(host);
        let now_ns = host.now_ns();
        if let Phase::Recording(recording) = &mut self.phase {
            if let Some(exit) = recording.advance(&mut self.link, &self.sink, now_ns) {
                self.phase = Phase::Over(exit);
            }
        }
        match self.phase {
            Phase::Over(exit) if self.link.quiet() => Some(exit),
            _ => None,
        }
    }

    /// Answer every whole message the session has sent.
    fn hear(&mut self, host: &mut H) {
        loop {
            let Some(session) = self.link.session.as_mut() else {
                return self.orphaned();
            };
            let message = match session.recv(SessionMessage::decode) {
                Ok(Some(message)) => message,
                Ok(None) if session.peer_finished() => return self.orphaned(),
                Ok(None) => return,
                Err(_) => {
                    self.link.session = None;
                    return self.orphaned();
                }
            };
            self.answer(&message, host);
        }
    }

    /// The session's pipe has closed, or failed.
    fn orphaned(&mut self) {
        match self.phase {
            Phase::Greeting => self.phase = Phase::Over(ended(&self.sink, Reason::Orphaned)),
            Phase::Recording(ref mut recording) => {
                recording.end_with(End::Failed(Reason::Orphaned));
            }
            Phase::Over(_) => {}
        }
    }

    fn answer(&mut self, message: &Result<SessionMessage, Errno>, host: &mut H) {
        match self.phase {
            Phase::Greeting => match message {
                Ok(SessionMessage::Start(start)) => self.begin(host, *start),
                Ok(_) => self.unstarted(Unstart::new(
                    Errno::BadMagic,
                    "the session spoke before it began a recording",
                )),
                Err(errno) => {
                    self.unstarted(Unstart::new(*errno, "the session's start does not decode"));
                }
            },
            Phase::Recording(ref mut recording) => {
                recording.heard(message, &mut self.link, &self.sink);
            }
            Phase::Over(_) => {}
        }
    }

    fn begin(&mut self, host: &mut H, start: RecordStart) {
        match Recording::begin(host, start) {
            Ok(recording) => {
                self.phase = Phase::Recording(recording);
                self.link.say(&RecorderMessage::Ready);
            }
            Err(unstart) => self.unstarted(unstart),
        }
    }

    fn unstarted(&mut self, unstart: Unstart) {
        self.link.say(&RecorderMessage::Unstarted(unstart.answer));
        let said = Said {
            what: unstart.what,
            errno: Some(unstart.cause),
        };
        report(
            &self.sink,
            Level::Error,
            RECORDER_UNSTARTED,
            said.what,
            said.errno,
        );
        self.phase = Phase::Over(Exit {
            code: EXIT_ENDED,
            reason: Some(said),
        });
    }
}

/// A recording under way.
struct Recording<H: Host> {
    slot: RecorderSlot,
    started_ns: u64,
    desk: H::Desk,
    busy: [bool; RECORDER_SLOTS],
    /// No frame may be shown before this.
    last_at_ns: u64,
    sound: Option<Sound<H::Sound>>,
    /// A batch of packets is with the encoding thread.
    batch_out: bool,
    /// Whether a loss of packets has been stated.
    sound_lost: bool,
    end: Option<End>,
    /// The file's finish is with the encoding thread.
    finishing: bool,
    /// Frames dropped for want of memory.
    dropped: u64,
}

impl<H: Host> Recording<H> {
    /// Begin the recording `start` asks for: the slots it grants mapped,
    /// everything else it holds made, and its sound moving.
    fn begin(host: &mut H, start: RecordStart) -> Result<Self, Unstart> {
        let slot = start.slot;
        let runner = host.runner();
        let lanes = runner.width();
        let needed = footprint(slot, start.sound, lanes).ok_or(Unstart::MEMORY)?;
        if needed > host.memory_room() {
            return Err(Unstart::MEMORY);
        }
        let output = host.open_output(start.output).map_err(|errno| {
            Unstart::new(
                errno,
                "the output the session delegated could not be redeemed",
            )
        })?;
        match output.regular_len() {
            Ok(Some(0)) => {}
            Ok(Some(_)) => {
                let what = "the output the session delegated is not empty";
                return Err(Unstart::new(Errno::AlreadyExists, what));
            }
            Ok(None) => {
                let what = "the output the session delegated is not a file";
                return Err(Unstart::new(Errno::OutOfRange, what));
            }
            Err(errno) => {
                let what = "the output the session delegated could not be measured";
                return Err(Unstart::new(errno, what));
            }
        }
        let slots = map_slots(host, slot, start.slots)?;
        let mut sound = Sound::open(start.sound, |source, layout| {
            host.open_sound(source, layout)
        })
        .map_err(Unstart::sound)?;
        let video = vp8::Encoder::new(slot.width(), slot.height(), Quality::default(), lanes)
            .map_err(Unstart::coder)?;
        let layout = Layout {
            width: slot.width(),
            height: slot.height(),
            audio: sound::track_of(start.sound),
            indexed_keyframes: INDEXED_KEYFRAMES,
        };
        let encoding = Encoding::new(output, layout, (video, runner), slots, slot)
            .map_err(|errno| Unstart::new(errno, "no room for a coded frame"))?;
        let desk = host.start_encoding(encoding).map_err(|errno| {
            Unstart::new(errno, "the encoding thread could not be given its work")
        })?;
        if let Some(sound) = sound.as_mut() {
            sound
                .start(start.started_ns, host.now_ns())
                .map_err(Unstart::sound)?;
        }
        Ok(Self {
            slot,
            started_ns: start.started_ns,
            desk,
            busy: [false; RECORDER_SLOTS],
            last_at_ns: start.started_ns,
            sound,
            batch_out: false,
            sound_lost: false,
            end: None,
            finishing: false,
            dropped: 0,
        })
    }

    /// Answer `message`, said while recording.
    fn heard<T: SessionTransport, L: Sink, K: Sink>(
        &mut self,
        message: &Result<SessionMessage, Errno>,
        link: &mut Link<T, L>,
        sink: &K,
    ) {
        match message {
            Ok(SessionMessage::Frame {
                slot,
                at_ns,
                damage,
            }) => self.frame(link, sink, *slot, *at_ns, damage),
            Ok(SessionMessage::Stop) => self.end_with(End::Asked),
            Ok(SessionMessage::Start(_)) => self.refuse(sink, Reason::OutOfTurn),
            Err(errno) => self.refuse(sink, Reason::Undecodable(*errno)),
        }
    }

    /// Check the frame in slot `slot`, shown at `at_ns`, against its slot, and
    /// hand it to the encoding thread.
    fn frame<T: SessionTransport, L: Sink, K: Sink>(
        &mut self,
        link: &mut Link<T, L>,
        sink: &K,
        slot: u8,
        at_ns: u64,
        damage: &RecorderDamage,
    ) {
        let index = usize::from(slot);
        match self.end {
            Some(End::DiskFull | End::Failed(_)) => {
                if index < RECORDER_SLOTS {
                    link.say(&RecorderMessage::Released { slot });
                }
                return;
            }
            Some(End::Asked) => return self.refuse(sink, Reason::OutOfTurn),
            None => {}
        }
        let mode = self.slot.display_mode();
        let outside = damage
            .rects()
            .iter()
            .any(|rect| rect.validate_in(&mode).is_err());
        let refusal = match self.busy.get(index) {
            None => Some(Reason::Unknown),
            Some(true) => Some(Reason::Busy),
            Some(false) if outside => Some(Reason::Outside),
            Some(false) if at_ns < self.last_at_ns => Some(Reason::Backwards),
            Some(false) => None,
        };
        if let Some(reason) = refusal {
            return self.refuse(sink, reason);
        }
        let job = Job::Frame {
            slot: index,
            at_ns: at_ns.saturating_sub(self.started_ns),
            damage: *damage,
        };
        if self.desk.submit(job).is_err() {
            return self.end_with(End::Failed(Reason::Desk));
        }
        if let Some(busy) = self.busy.get_mut(index) {
            *busy = true;
        }
        self.last_at_ns = at_ns;
    }

    /// Refuse what the session sent, for `reason`, which ends the recording.
    fn refuse<K: Sink>(&mut self, sink: &K, reason: Reason) {
        let said = reason.said();
        report(sink, Level::Error, RECORDER_REFUSED, said.what, said.errno);
        self.end_with(End::Failed(reason));
    }

    /// End the recording as `end` says, an earlier failure standing. Its sound
    /// is let go once nothing more of it can be written.
    fn end_with(&mut self, end: End) {
        let end = self.end.map_or(end, |earlier| earlier.then(end));
        if matches!(
            end,
            End::DiskFull | End::Failed(Reason::Halted(_) | Reason::Desk)
        ) {
            self.sound = None;
        }
        self.end = Some(end);
    }

    /// Serve the recording: the sound that arrived, the encoding thread's
    /// answers, and the work now due. `Some` once it is over, with how.
    fn advance<T: SessionTransport, L: Sink, K: Sink>(
        &mut self,
        link: &mut Link<T, L>,
        sink: &K,
        now_ns: u64,
    ) -> Option<Exit> {
        self.listen(sink);
        loop {
            let (collected, finished) = self.collect(link, sink);
            if finished.is_some() {
                return finished;
            }
            let passed = self.pass_sound();
            let wound = match self.wind_up(now_ns) {
                Ok(wound) => wound,
                Err(reason) => return Some(ended(sink, reason)),
            };
            if !(collected || passed || wound) {
                return None;
            }
        }
    }

    /// Hear the sound that has arrived and code it. A sound that fails ends,
    /// and the picture goes on.
    fn listen<K: Sink>(&mut self, sink: &K) {
        let Some(sound) = self.sound.as_mut() else {
            return;
        };
        if let Err(errno) = sound.hear() {
            let what = "the recording's sound stopped; the picture goes on";
            report(sink, Level::Warn, SOUND_LOST, what, Some(errno));
            self.sound = None;
            return;
        }
        if sound.take_lost() > 0 && !self.sound_lost {
            self.sound_lost = true;
            let what = "sound was lost while the encoding thread fell behind";
            report(sink, Level::Warn, SOUND_LOST, what, None);
        }
    }

    /// Hand the encoding thread the packets that wait, where no batch is out.
    /// Whether it handed any over.
    fn pass_sound(&mut self) -> bool {
        if self.batch_out || self.finishing {
            return false;
        }
        let Some(batch) = self.sound.as_mut().and_then(Sound::take_batch) else {
            return false;
        };
        if self.desk.submit(Job::Sound(batch)).is_err() {
            self.end_with(End::Failed(Reason::Desk));
            return false;
        }
        self.batch_out = true;
        true
    }

    /// Once the recording has ended and nothing handed over is outstanding,
    /// hand over the sound's tail and the file's finish. Whether it handed
    /// anything over.
    fn wind_up(&mut self, now_ns: u64) -> Result<bool, Reason> {
        if self.end.is_none() || self.finishing || self.batch_out || self.busy.contains(&true) {
            return Ok(false);
        }
        if let Some(mut sound) = self.sound.take() {
            // What was coded before a source failed is still kept.
            let _ = sound.finish();
            if let Some(batch) = sound.take_batch() {
                self.desk
                    .submit(Job::Sound(batch))
                    .map_err(|_| Reason::Desk)?;
                self.batch_out = true;
            }
        }
        let ends_at_ns = now_ns.saturating_sub(self.started_ns);
        self.desk
            .submit(Job::Finish { ends_at_ns })
            .map_err(|_| Reason::Desk)?;
        self.finishing = true;
        Ok(true)
    }

    /// Take every answer the encoding thread has left: whether there were any,
    /// and how the recording is over once its file is.
    fn collect<T: SessionTransport, L: Sink, K: Sink>(
        &mut self,
        link: &mut Link<T, L>,
        sink: &K,
    ) -> (bool, Option<Exit>) {
        let mut collected = false;
        while let Some(answer) = self.desk.collect() {
            collected = true;
            match answer {
                Answer::Frame { slot, outcome } => self.released(link, sink, slot, outcome),
                Answer::Sound { batch, outcome } => {
                    self.batch_out = false;
                    if let Some(sound) = self.sound.as_mut() {
                        sound.give_back(batch);
                    }
                    if let Err(halt) = outcome {
                        self.halted(halt);
                    }
                }
                Answer::Finished(closed) => return (true, Some(self.closed(link, sink, closed))),
            }
        }
        (collected, None)
    }

    /// A frame's slot is the session's again, and what came of the frame.
    fn released<T: SessionTransport, L: Sink, K: Sink>(
        &mut self,
        link: &mut Link<T, L>,
        sink: &K,
        slot: usize,
        outcome: Result<Coded, Halt>,
    ) {
        if let Some(busy) = self.busy.get_mut(slot) {
            *busy = false;
        }
        if let Ok(slot) = u8::try_from(slot) {
            link.say(&RecorderMessage::Released { slot });
        }
        match outcome {
            Ok(Coded::Kept) => {}
            Ok(Coded::Dropped) => {
                if self.dropped == 0 {
                    let what = "a frame was dropped for want of memory, and the next coded whole";
                    report(
                        sink,
                        Level::Warn,
                        FRAME_DROPPED,
                        what,
                        Some(Errno::OutOfMemory),
                    );
                }
                self.dropped = self.dropped.saturating_add(1);
            }
            Err(halt) => self.halted(halt),
        }
    }

    /// The encoding thread can go no further, for `halt`.
    fn halted(&mut self, halt: Halt) {
        let end = match halt {
            Halt::DiskFull => End::DiskFull,
            Halt::Failed(failure) => End::Failed(Reason::Halted(failure)),
        };
        self.end_with(end);
    }

    /// The file is finished, or could not be: what the session is told, and
    /// how the recorder ends.
    fn closed<T: SessionTransport, L: Sink, K: Sink>(
        &self,
        link: &mut Link<T, L>,
        sink: &K,
        closed: Result<Closed, Failure>,
    ) -> Exit {
        let closed = match (self.end, closed) {
            (_, Err(failure)) => return ended(sink, Reason::Unfinished(failure)),
            (Some(End::Failed(reason)), Ok(_)) => return ended(sink, reason),
            (_, Ok(closed)) => closed,
        };
        let ending = if self.end == Some(End::DiskFull) {
            RecordEnding::DiskFull
        } else {
            closed.ending
        };
        link.say(&RecorderMessage::Finished {
            bytes: closed.bytes,
            ending,
        });
        if ending == RecordEnding::Stopped {
            return Exit {
                code: EXIT_FINISHED,
                reason: None,
            };
        }
        let said = Said {
            what: "the volume filled, and the recording was finished as far as it got",
            errno: None,
        };
        report(sink, Level::Warn, RECORDER_ENDED, said.what, said.errno);
        Exit {
            code: EXIT_FINISHED,
            reason: Some(said),
        }
    }
}

/// Map the slots the session granted under `handles`, in slot order, each of
/// exactly a slot's own pages, so the session cannot have the recorder map
/// more than a slot is.
fn map_slots<H: Host>(
    host: &mut H,
    slot: RecorderSlot,
    handles: [u64; RECORDER_SLOTS],
) -> Result<Vec<H::Slot>, Unstart> {
    let len = slot.len();
    let most = len
        .checked_next_multiple_of(PAGE_SIZE)
        .ok_or(Unstart::MEMORY)?;
    let mut slots = Vec::new();
    if !fallible::reserve(&mut slots, RECORDER_SLOTS) {
        return Err(Unstart::MEMORY);
    }
    for handle in handles {
        let memory = host.map_slot(handle, len).map_err(|errno| {
            Unstart::new(errno, "a slot the session granted could not be mapped")
        })?;
        if !(len..=most).contains(&memory.bytes().len()) {
            let what = "a slot the session granted is not the slot's size";
            return Err(Unstart::new(Errno::LengthOutOfRange, what));
        }
        slots.push(memory);
    }
    Ok(slots)
}

/// The bytes a recording of `slot` hearing `sound` commits before `Ready`: the
/// encoder at its largest on `lanes` lanes, the room for a coded frame, and
/// its sound; `None` past what a `u64` counts. Its slots are the session's
/// memory.
fn footprint(slot: RecorderSlot, sound: RecordSound, lanes: usize) -> Option<u64> {
    vp8::Encoder::peak_bytes(slot.width(), slot.height(), lanes)
        .checked_add(u64::try_from(frame_room(slot)).ok()?)?
        .checked_add(sound::footprint(sound))
}

/// Record `what` at `level` under `id`, with the error beneath it.
fn report<K: Sink>(sink: &K, level: Level, id: EventId, what: &str, errno: Option<Errno>) {
    let error = errno.map_or(FieldValue::Null, FieldValue::Error);
    tairix_log::log(
        sink,
        &Event {
            level,
            id,
            message: what,
            fields: &[Field {
                key: "error",
                value: error,
            }],
        },
    );
}

/// Record that the recording ended for `reason`, answering the exit that
/// states it.
fn ended<K: Sink>(sink: &K, reason: Reason) -> Exit {
    let said = reason.said();
    report(sink, Level::Error, RECORDER_ENDED, said.what, said.errno);
    Exit {
        code: EXIT_ENDED,
        reason: Some(said),
    }
}

#[cfg(test)]
#[path = "recorder_tests.rs"]
mod tests;
