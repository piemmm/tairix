//! The playback engine: a programme's files through the sandboxed decoder into
//! one audio stream, and none of the I/O.
//!
//! The engine reads a file only as the decoder asks for its bytes, writes what
//! the decoder answers into the stream's ring as the ring has room, and
//! answers each thing it is told — a frame from the decoder, a notification
//! from the audio service, a command — with what follows from it. The files,
//! the stream and the decoder's worker are seams, so every rule here is tested
//! on a host.
//!
//! # One stream while the shape holds
//!
//! Consecutive entries of one rate, sample format and channel layout are
//! written into the same stream back to back, so a programme plays gapless.
//! An entry of another shape waits for the stream to drain — every frame
//! already queued is heard — and then opens its own.
//!
//! # Positions
//!
//! The ring counts frames from zero for each stream; a file counts its own.
//! Where the two stop moving together — a new entry, a seek, a new pass — a
//! segment records the ring frame and the file frame it carries, so the
//! position the service reports reading is turned back into the entry and
//! frame being heard.
//!
//! # Edits
//!
//! The programme may change while it plays. The entry being heard plays on;
//! whatever is queued after it that no longer follows from the programme is
//! dropped — work not yet in the stream silently, frames already in it by
//! stopping on the frame being heard and refilling from there — so a removed
//! or moved entry is never heard out of place.
//!
//! # Loudness
//!
//! With [`Settings::normalise`] each track is played at the gain its own tags
//! state ([`crate::loudness`]), applied to its frames before they reach the
//! ring so a gapless boundary between two tracks is exact. A track at unity is
//! passed through untouched.

use alloc::collections::VecDeque;
use alloc::vec::Vec;

use tairix_abi::audio::{
    AudioGain, AudioNotify, OpenParams, StreamGrant, StreamReport, StreamRole, StreamState,
};
use tairix_abi::driver::audio::{
    ring_bounds, ChannelMap, Frames, Rate, SampleFormat, StreamDirection, MAX_CHANNELS,
};
use tairix_abi::Errno;
use tairix_audio::convert::{self, DitherSource};
use tairix_audio::stream::{OpenFailure, Written};
use tairix_audio::volume::{millibel_to_linear, UNITY_MILLIBEL};
use tairix_hash::HashSeed;
use tairix_log::Sink;
use tairix_sandbox::audiodecode::{
    session_bounds, AudioDecodeClient, AudioDecodeError, AudioRefusal, DecodeEvent,
    MAX_BLOCK_FRAMES,
};
use tairix_sandbox::session::{SessionDescriptors, SessionError};
use tairix_sandbox::supervise::{SessionLauncher, SupervisedSession};
use tairix_sound::SoundInfo;

use crate::loudness;
use crate::programme::{Advance, EntryId, Extent, Programme};
use crate::Span;

/// Blocks decoded ahead of the ring: one being written while the next is
/// asked for, so the decoder's round trip hides behind the ring's latency.
const AHEAD_BLOCKS: usize = 2;

/// A ring of half a second: deep enough that a slow file read or a busy
/// machine never reaches the device, and still no slower to pause, seek or
/// change level — those act on the stream, not on what it has queued.
const RING_SECONDS_DIVISOR: u32 = 2;

/// How far a seek command moves.
const SEEK_SPAN: Span = Span::from_nanos(10_000_000_000);

/// How far into an entry "previous" restarts it rather than going back one.
const RESTART_SPAN: Span = Span::from_nanos(3_000_000_000);

/// How far one level command moves the gain, in hundredths of a decibel.
const LEVEL_STEP_MILLIBEL: i32 = 300;

/// How many peak spans a second of sound is metered in.
const METER_SPANS_PER_SECOND: u32 = 25;

/// The seed of the dither a track's gain is requantised with: dither wants
/// decorrelation, not unpredictability, so a fixed sequence serves.
pub(crate) const DITHER_SEED: u64 = 0x7072_6f67_7261_6d6d;

/// Where the engine reads the files it plays, opening what a programme's
/// entries name.
pub trait Files<I: ?Sized> {
    /// Open the file `item` names, which is then the file every read comes
    /// from, answering its length.
    ///
    /// # Errors
    ///
    /// [`FileRefusal`].
    fn open(&mut self, item: &I) -> Result<u64, FileRefusal>;

    /// Fill `buf` from `offset`, answering how much the file held.
    ///
    /// # Errors
    ///
    /// The read's refusal.
    fn read(&mut self, offset: u64, buf: &mut [u8]) -> Result<usize, Errno>;
}

/// Why a file could not be opened.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum FileRefusal {
    /// It is not a regular file, so it has no length to decode within.
    NotRegular,
    /// The filesystem refused it.
    Os(Errno),
}

/// The audio stream the engine plays into.
pub trait Speaker {
    /// Open a stream with its ring attached.
    ///
    /// # Errors
    ///
    /// [`OpenFailure`].
    fn open(&mut self, params: &OpenParams) -> Result<StreamGrant, OpenFailure>;
    /// Publish `samples` at ring frame `at`.
    ///
    /// # Errors
    ///
    /// The ring's refusal.
    fn write(&mut self, at: Frames, samples: &[u8]) -> Result<Written, Errno>;
    /// Begin playing at ring frame `at`.
    ///
    /// # Errors
    ///
    /// The service's refusal.
    fn start(&mut self, at: Frames) -> Result<(), Errno>;
    /// Stop at once, answering the frame it stopped on.
    ///
    /// # Errors
    ///
    /// The service's refusal.
    fn pause(&mut self) -> Result<Frames, Errno>;
    /// Play out what is queued, then stop.
    ///
    /// # Errors
    ///
    /// The service's refusal.
    fn drain(&mut self) -> Result<(), Errno>;
    /// Discard what is queued.
    ///
    /// # Errors
    ///
    /// The service's refusal.
    fn flush(&mut self) -> Result<(), Errno>;
    /// Set the stream's level.
    ///
    /// # Errors
    ///
    /// The service's refusal.
    fn set_gain(&mut self, gain: AudioGain) -> Result<(), Errno>;
    /// Close the stream.
    ///
    /// # Errors
    ///
    /// The service's refusal.
    fn close(&mut self) -> Result<(), Errno>;
    /// The next notification the service sent, never waiting for one.
    ///
    /// # Errors
    ///
    /// A mailbox that cannot be read, or a malformed notification.
    fn take_notify(&mut self) -> Result<Option<AudioNotify>, Errno>;
    /// The service's own account of the stream.
    ///
    /// # Errors
    ///
    /// The service's refusal.
    fn report(&mut self) -> Result<StreamReport, Errno>;
}

/// How a playback is heard.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Settings {
    /// The stream's level.
    pub gain: AudioGain,
    /// The device to play on, as enumerated, or zero for the default sink.
    pub device_id: u32,
    /// Whether each track is played at the loudness its own tags state.
    pub normalise: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            gain: AudioGain::UNITY,
            device_id: 0,
            normalise: false,
        }
    }
}

/// What a listener may ask of a playback.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Control {
    /// Pause, or play on from where it paused.
    TogglePause,
    /// Ten seconds on.
    Forward,
    /// Ten seconds back.
    Back,
    /// The next entry.
    Next,
    /// The start of this entry, or the one before when this one has barely
    /// begun.
    Previous,
    /// Three decibels nearer unity.
    Louder,
    /// Three decibels quieter.
    Quieter,
    /// Stop.
    Stop,
    /// This point of the entry being heard.
    SeekTo(Span),
    /// This entry, from the start of its extent; after playback has ended, a
    /// new playback beginning there.
    Jump(EntryId),
    /// This level.
    SetGain(AudioGain),
    /// This device, from where the listener is.
    SetDevice(u32),
    /// Each track at its own stated loudness, or not.
    SetNormalise(bool),
}

/// How playback ended.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Outcome {
    /// Every pass of the programme was played.
    Played,
    /// The listener stopped it.
    Stopped,
    /// It could not go on.
    Failed(Failure),
}

/// Why playback could not go on.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Failure {
    /// The sandboxed decoder could not be started.
    NoDecoder,
    /// The stream could not be opened.
    Stream(OpenFailure),
    /// The audio service refused what playback needed of an open stream.
    Audio {
        /// What was asked.
        what: &'static str,
        /// Its refusal.
        errno: Errno,
    },
    /// The device the stream played on went away.
    DeviceLost,
    /// The audio service stopped reading the stream's ring, whose positions
    /// it found corrupt.
    RingFaulted,
    /// What playback waits on could not be watched or read.
    Wait {
        /// What could not be done.
        what: &'static str,
        /// Why.
        errno: Errno,
    },
    /// No entry of a whole pass could be played.
    NothingPlayable,
    /// There was not the memory to go on.
    OutOfMemory,
}

impl core::fmt::Display for Failure {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NoDecoder => f.write_str("the sandboxed decoder could not be started"),
            Self::Stream(failure) => write!(f, "{failure}"),
            Self::Audio { what, errno } => write!(f, "the audio service would not {what}: {errno}"),
            Self::DeviceLost => f.write_str("the audio device went away"),
            Self::RingFaulted => {
                f.write_str("the audio service found the stream's ring corrupt and stopped it")
            }
            Self::Wait { what, errno } => write!(f, "could not {what}: {errno}"),
            Self::NothingPlayable => f.write_str("no file could be played"),
            Self::OutOfMemory => f.write_str("there is not the memory to play"),
        }
    }
}

/// Why an entry was left out, or ended early.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Skip {
    /// The programme no longer holds it.
    Withdrawn,
    /// It could not be opened.
    Open(FileRefusal),
    /// The decoder refused it.
    Refused(AudioRefusal),
    /// Its bytes could not be read.
    Unreadable(Errno),
    /// It ends before the start it was asked to begin at.
    StartsPastEnd,
    /// The decoder failed twice at the same place in it.
    DecoderGaveUp,
}

impl core::fmt::Display for Skip {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Withdrawn => f.write_str("it is no longer in the programme"),
            Self::Open(FileRefusal::NotRegular) => f.write_str("not a regular file"),
            Self::Open(FileRefusal::Os(errno)) | Self::Unreadable(errno) => write!(f, "{errno}"),
            Self::Refused(refusal) => write!(f, "{refusal}"),
            Self::StartsPastEnd => f.write_str("it ends before the start asked for"),
            Self::DecoderGaveUp => f.write_str("the decoder failed twice at the same place"),
        }
    }
}

impl Skip {
    /// The stable machine word for why.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Withdrawn => "withdrawn",
            Self::Open(FileRefusal::NotRegular) => "not_regular",
            Self::Open(FileRefusal::Os(_)) => "open_refused",
            Self::Refused(_) => "decoder_refused",
            Self::Unreadable(_) => "unreadable",
            Self::StartsPastEnd => "starts_past_end",
            Self::DecoderGaveUp => "decoder_failed",
        }
    }
}

/// Something the host reports as it happens.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Note {
    /// An entry is open and about to be played.
    Opened {
        /// Which.
        entry: EntryId,
        /// What it is.
        info: SoundInfo,
    },
    /// An entry was left out.
    Skipped {
        /// Which.
        entry: EntryId,
        /// Why.
        why: Skip,
    },
    /// An entry stopped before its end.
    Cut {
        /// Which.
        entry: EntryId,
        /// Why.
        why: Skip,
    },
}

/// Where the transport stands.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Transport {
    /// Filling the ring before the first frame plays.
    Starting,
    /// Playing.
    Playing,
    /// Paused by the listener.
    Paused,
    /// Held while the seat is elsewhere.
    Held,
    /// Playing out the last of the programme.
    Ending,
    /// Not playing: the playback has ended.
    Stopped,
}

/// What an interface shows.
#[derive(Clone, Debug, PartialEq)]
pub struct Status {
    /// The entry being heard, and what it is.
    pub heard: Option<(EntryId, SoundInfo)>,
    /// The frame of it being heard.
    pub position: u64,
    /// Where the transport stands.
    pub transport: Transport,
    /// The stream's level.
    pub gain: AudioGain,
    /// The peak of each channel being heard, of 255, and how many there are.
    pub peaks: ([u8; MAX_CHANNELS], usize),
    /// Times the device ran short of frames.
    pub underruns: u32,
    /// Frames lost to them.
    pub lost_frames: u64,
    /// Frames heard in all.
    pub heard_frames: u64,
    /// The pass of the programme being heard, from zero.
    pub pass: u32,
}

impl Status {
    /// Nothing heard yet, at `gain`.
    #[must_use]
    pub const fn new(gain: AudioGain) -> Self {
        Self {
            heard: None,
            position: 0,
            transport: Transport::Starting,
            gain,
            peaks: ([0; MAX_CHANNELS], 0),
            underruns: 0,
            lost_frames: 0,
            heard_frames: 0,
            pass: 0,
        }
    }
}

/// A stream's rate, sample format and channel layout: what decides whether
/// an entry can follow another into it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct Shape {
    rate: Rate,
    sample: SampleFormat,
    channels: ChannelMap,
}

impl Shape {
    fn of(info: &SoundInfo) -> Self {
        Self {
            rate: info.rate,
            sample: info.sample,
            channels: info.channels,
        }
    }

    fn frame_bytes(self) -> usize {
        self.sample.bytes_per_sample() * usize::from(self.channels.channels())
    }
}

/// A place in an entry.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Point {
    /// A time into it, its frame found once its rate is known.
    Time(Span),
    /// A frame of it, counted at `hz`.
    Frame { frame: u64, hz: u32 },
}

impl Point {
    fn frame_at(self, hz: u32) -> u64 {
        match self {
            Self::Frame { frame, hz: counted } if counted == hz => frame,
            point => point.span().frames_at(hz),
        }
    }

    fn span(self) -> Span {
        match self {
            Self::Time(span) => span,
            Self::Frame { frame, hz } => Span::of_frames(frame, hz),
        }
    }
}

/// What the decoder is doing with the file it holds.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Phase {
    /// Opening it.
    Opening,
    /// Seeking to where this pass of it begins.
    Seeking,
    /// Decoding, discarding what comes before `from`.
    Decoding { from: u64 },
    /// Every frame this pass wants is decoded.
    Done,
}

/// The entry the decoder holds.
#[derive(Copy, Clone, Debug)]
struct Current {
    entry: EntryId,
    /// This play of it, which a repeat of the same entry does not share.
    serial: u64,
    /// Where decoding it begins: its extent's start, or where a seek went.
    enter: Point,
    info: Option<SoundInfo>,
    /// The frame its pass ends at.
    end: Option<u64>,
    phase: Phase,
    /// Why its bytes could not be read, for the refusal that follows.
    unreadable: Option<Errno>,
    /// The multiplier its stated loudness asks for, when that is not unity.
    gain: Option<f32>,
}

/// Decoded frames not yet in the ring.
struct Block {
    entry: EntryId,
    serial: u64,
    /// The pass of the programme it was decoded in.
    pass: u32,
    info: SoundInfo,
    /// The file frame of its first unwritten frame.
    frame: u64,
    bytes: Vec<u8>,
    /// Bytes already written.
    offset: usize,
}

impl Block {
    fn shape(&self) -> Shape {
        Shape::of(&self.info)
    }
}

/// Where a run of ring frames came from.
#[derive(Copy, Clone, Debug)]
struct Segment {
    ring: u64,
    entry: EntryId,
    serial: u64,
    pass: u32,
    frame: u64,
    info: SoundInfo,
}

/// The loudest sample of each channel over ring frames up to `end`.
#[derive(Copy, Clone, Debug)]
struct PeakSpan {
    end: u64,
    levels: [u8; MAX_CHANNELS],
}

/// The open stream.
struct Stream {
    shape: Shape,
    /// The service's name for the stream.
    id: u64,
    /// The ring frame the next write lands at.
    next: u64,
    /// The ring frame the service has read up to.
    read: u64,
    started: bool,
    /// Where a start begins.
    resume_at: u64,
    /// The ring took less than offered, so the next write waits for room.
    full: bool,
    draining: bool,
    /// Frames a seek discarded unheard, which the service counts as read.
    discarded: u64,
    segments: VecDeque<Segment>,
    peaks: VecDeque<PeakSpan>,
    /// Ring frames a peak span covers.
    span_frames: u64,
    /// The underrun totals of the streams before this one, which this one's
    /// own report completes.
    underruns_before: u32,
    lost_before: u64,
}

/// How far through the programme decoding has got.
#[derive(Copy, Clone, Debug, Default)]
struct Progress {
    /// The pass being decoded.
    pass: u32,
    /// An entry of this pass reached the decoder.
    playable: bool,
    /// Every pass is decoded.
    exhausted: bool,
}

/// What keeps a primed stream from playing.
#[derive(Copy, Clone, Debug, Default)]
struct Holds {
    /// The listener paused it.
    user: bool,
    /// The seat it plays on is elsewhere.
    seat: bool,
}

impl Holds {
    const fn any(self) -> bool {
        self.user || self.seat
    }
}

/// Where decoding is to go once the decoder is free.
#[derive(Copy, Clone, Debug)]
struct Jump {
    entry: EntryId,
    point: Point,
    pass: u32,
}

/// Where the listener is: an entry, the pass it is heard in, how far into it,
/// and what it is once that is known.
#[derive(Copy, Clone)]
struct Heard {
    entry: EntryId,
    pass: u32,
    at: Span,
    info: Option<SoundInfo>,
}

/// One play of an entry somewhere in the queue, at its first place.
#[derive(Copy, Clone)]
struct Group {
    serial: u64,
    entry: EntryId,
    pass: u32,
    place: Place,
}

/// Where a group's first frames lie.
#[derive(Copy, Clone)]
enum Place {
    /// In the stream, from this ring frame.
    Ring(u64),
    Pending(usize),
    Decoder,
}

/// The playback engine.
pub struct Engine<P: Programme, L: SessionLauncher, S: Sink + Clone, F: Files<P::Item>, O: Speaker>
{
    programme: P,
    settings: Settings,
    files: F,
    speaker: O,
    session: SupervisedSession<L, S>,
    decoder: AudioDecodeClient,
    seed: fn() -> Option<HashSeed>,
    current: Option<Current>,
    jump: Option<Jump>,
    /// The serial the next play of an entry takes.
    serials: u64,
    progress: Progress,
    pending: VecDeque<Block>,
    spare: Vec<Vec<u8>>,
    reading: Vec<u8>,
    pivot: Vec<f32>,
    dither: DitherSource,
    stream: Option<Stream>,
    holds: Holds,
    status: Status,
    changed: bool,
    notes: Vec<Note>,
    outcome: Option<Outcome>,
}

impl<P: Programme, L: SessionLauncher, S: Sink + Clone, F: Files<P::Item>, O: Speaker>
    Engine<P, L, S, F, O>
{
    /// An engine playing `programme` as `settings` say, decoding in workers
    /// `launcher` starts and logging their containment to `sink`, each file's
    /// cache keyed by a draw from `seed`.
    ///
    /// # Errors
    ///
    /// [`Failure::OutOfMemory`] when the working buffers cannot be had, and
    /// [`Failure::NoDecoder`] when the session's bounds are refused.
    pub fn new(
        programme: P,
        settings: Settings,
        launcher: L,
        sink: S,
        files: F,
        speaker: O,
        seed: fn() -> Option<HashSeed>,
    ) -> Result<Self, Failure> {
        let bounds = session_bounds().map_err(|_| Failure::NoDecoder)?;
        let mut pivot = Vec::new();
        pivot
            .try_reserve_exact(MAX_BLOCK_FRAMES as usize * MAX_CHANNELS)
            .map_err(|_| Failure::OutOfMemory)?;
        pivot.resize(MAX_BLOCK_FRAMES as usize * MAX_CHANNELS, 0.0);
        Ok(Self {
            programme,
            settings,
            files,
            speaker,
            session: SupervisedSession::new(launcher, bounds, sink),
            decoder: AudioDecodeClient::new(),
            seed,
            current: None,
            jump: None,
            serials: 0,
            progress: Progress::default(),
            pending: VecDeque::new(),
            spare: Vec::new(),
            reading: Vec::new(),
            pivot,
            dither: DitherSource::new(DITHER_SEED),
            stream: None,
            holds: Holds::default(),
            status: Status::new(settings.gain),
            changed: true,
            notes: Vec::new(),
            outcome: None,
        })
    }

    /// Start the decoder and open the programme's first entry; an empty
    /// programme has played.
    pub fn begin(&mut self, now: u64) {
        if self.session.start(now).is_none() {
            self.fail(Failure::NoDecoder);
            return;
        }
        match self.programme.first_of_pass(0) {
            Some(first) => self.open_track(first),
            None => self.end(Outcome::Played),
        }
        self.pump(now);
    }

    /// The decoder's descriptors and the directions it wants, for the host's
    /// wait-set.
    #[must_use]
    pub fn decoder_members(&self) -> (Option<SessionDescriptors>, bool, bool) {
        (
            self.session.descriptors(),
            self.session.wants_read(),
            self.session.wants_write(),
        )
    }

    /// The instant the engine next wants [`Self::on_timer`] called.
    #[must_use]
    pub fn deadline(&self) -> Option<u64> {
        self.session.restart_deadline()
    }

    /// What an interface shows.
    #[must_use]
    pub fn status(&self) -> &Status {
        &self.status
    }

    /// Whether the status changed since this was last asked.
    pub fn take_changed(&mut self) -> bool {
        core::mem::take(&mut self.changed)
    }

    /// Hand over what happened since this was last asked.
    pub fn take_notes(&mut self) -> Vec<Note> {
        core::mem::take(&mut self.notes)
    }

    /// How the last playback ended, while none is under way.
    #[must_use]
    pub fn outcome(&self) -> Option<Outcome> {
        self.outcome
    }

    /// What plays.
    #[must_use]
    pub fn programme(&self) -> &P {
        &self.programme
    }

    /// How it is heard.
    #[must_use]
    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    /// Change the programme, then re-plan whatever is queued that no longer
    /// follows from it.
    pub fn edit<R>(&mut self, now: u64, change: impl FnOnce(&mut P) -> R) -> R {
        let answer = change(&mut self.programme);
        self.replan();
        self.programme.settle();
        self.changed = true;
        self.pump(now);
        answer
    }

    /// The stream the engine plays into, for the host to watch its mailbox.
    #[must_use]
    pub fn speaker(&self) -> &O {
        &self.speaker
    }

    /// The decoder's descriptors were reported ready.
    pub fn on_decoder(&mut self, now: u64, readable: bool, writable: bool) {
        if writable && self.session.on_writable(now).is_err() {
            self.worker_lost();
        }
        if readable && self.session.on_readable(now).is_err() {
            self.worker_lost();
        }
        self.pump(now);
    }

    /// The stream's mailbox was reported ready.
    pub fn on_notify(&mut self, now: u64) {
        loop {
            match self.speaker.take_notify() {
                Ok(Some(notify)) => self.adopt_notify(notify),
                Ok(None) => break,
                Err(errno) => {
                    self.fail(Failure::Wait {
                        what: "read the stream's notifications",
                        errno,
                    });
                    break;
                }
            }
        }
        self.pump(now);
    }

    /// Stop for `failure`, met by the host waiting on the engine's behalf;
    /// a playback already ended keeps its outcome.
    pub fn abandon(&mut self, failure: Failure) {
        if self.outcome.is_none() {
            self.fail(failure);
        }
    }

    /// The engine's deadline passed.
    pub fn on_timer(&mut self, now: u64) {
        if self.session.restart_deadline().is_some_and(|at| now >= at)
            && self.session.start(now).is_some()
        {
            self.resume_decoder();
        }
        self.pump(now);
    }

    /// A listener asked something of the playback.
    pub fn on_control(&mut self, now: u64, control: Control) {
        if self.outcome.is_some() {
            self.on_idle_control(now, control);
        } else {
            match control {
                Control::TogglePause => self.toggle_pause(),
                Control::Forward => self.seek_by(SEEK_SPAN, true),
                Control::Back => self.seek_by(SEEK_SPAN, false),
                Control::Next => self.skip_track(true),
                Control::Previous => self.skip_track(false),
                Control::Louder => self.step_level(LEVEL_STEP_MILLIBEL),
                Control::Quieter => self.step_level(-LEVEL_STEP_MILLIBEL),
                Control::Stop => self.stop(Outcome::Stopped),
                Control::SeekTo(at) => self.seek_to(at),
                Control::Jump(entry) => self.jump_to(entry, self.progress.pass),
                Control::SetGain(gain) => self.set_gain(gain),
                Control::SetDevice(device_id) => self.set_device(device_id),
                Control::SetNormalise(on) => self.settings.normalise = on,
            }
        }
        self.changed = true;
        self.pump(now);
    }

    /// What a control means while no playback is under way: a jump begins a
    /// new one, and the settings it would be heard with may still change.
    fn on_idle_control(&mut self, now: u64, control: Control) {
        match control {
            Control::Jump(entry) => {
                if self.rearm(now) {
                    self.jump_to(entry, 0);
                }
            }
            Control::SetGain(gain) => {
                self.settings.gain = gain;
                self.status.gain = gain;
            }
            Control::SetDevice(device_id) => self.settings.device_id = device_id,
            Control::SetNormalise(on) => self.settings.normalise = on,
            Control::TogglePause
            | Control::Forward
            | Control::Back
            | Control::Next
            | Control::Previous
            | Control::Louder
            | Control::Quieter
            | Control::Stop
            | Control::SeekTo(_) => {}
        }
    }

    /// Ready a new playback, its decoder started unless one is live or its
    /// paced replacement is already due; a decoder that cannot be started ends
    /// the attempt saying so.
    fn rearm(&mut self, now: u64) -> bool {
        let paced = self.session.restart_deadline().is_some_and(|at| now < at);
        if !self.session.is_live() && !paced && self.session.start(now).is_none() {
            self.end(Outcome::Failed(Failure::NoDecoder));
            return false;
        }
        self.outcome = None;
        self.progress = Progress::default();
        self.holds = Holds::default();
        true
    }

    /// Make every move the state allows, until none is left.
    fn pump(&mut self, now: u64) {
        while self.outcome.is_none() {
            let moved = self.take_answers(now)
                | self.send_outgoing(now)
                | self.apply_jump()
                | self.ask_decoder()
                | self.write_pending()
                | self.change_stream()
                | self.start_when_primed()
                | self.finish_when_played();
            if !moved {
                break;
            }
        }
    }

    /// Handle every answer the decoder has sent.
    fn take_answers(&mut self, now: u64) -> bool {
        let mut moved = false;
        loop {
            let mut block = self.spare.pop().unwrap_or_default();
            let decoder = &mut self.decoder;
            let taken = self
                .session
                .recv(now, |frame| match decoder.on_frame(frame) {
                    Ok(DecodeEvent::Block { position, pcm }) => {
                        block.clear();
                        if block.try_reserve(pcm.len()).is_err() {
                            return Taken::OutOfMemory;
                        }
                        block.extend_from_slice(pcm);
                        Taken::Answer(Answer::Block { position })
                    }
                    Ok(event) => Taken::Answer(Answer::from_event(event)),
                    Err(error) => Taken::Error(error),
                });
            match taken {
                Ok(Some(Taken::Answer(answer))) => {
                    moved = true;
                    self.handle(answer, block);
                }
                Ok(Some(Taken::OutOfMemory)) => {
                    self.fail(Failure::OutOfMemory);
                    return true;
                }
                Ok(Some(Taken::Error(error))) => {
                    moved = true;
                    self.spare.push(block);
                    self.decoder_error(now, error);
                }
                Ok(None) => {
                    self.spare.push(block);
                    return moved;
                }
                // No worker is live until the paced replacement: nothing more
                // can be taken, which is not progress.
                Err(_) => {
                    self.spare.push(block);
                    self.worker_lost();
                    return moved;
                }
            }
        }
    }

    fn handle(&mut self, answer: Answer, block: Vec<u8>) {
        if let Answer::Block { position } = answer {
            if self.jump.is_none() {
                self.decoded(position, block);
                return;
            }
        }
        self.spare.push(block);
        match answer {
            Answer::Need { offset, len } => self.serve_need(offset, len),
            // A jump is waiting for the decoder to be free: what it was doing
            // before is no longer wanted, only finished.
            Answer::Opened
            | Answer::Block { .. }
            | Answer::Ended
            | Answer::Sought { .. }
            | Answer::Refused(_)
                if self.jump.is_some() =>
            {
                if let Some(current) = &mut self.current {
                    current.phase = Phase::Done;
                }
            }
            Answer::Opened => self.opened(),
            Answer::Ended => self.file_done(),
            Answer::Sought { position } => {
                if let Some(current) = &mut self.current {
                    current.phase = Phase::Decoding { from: position };
                }
            }
            Answer::Refused(refusal) => self.refused(refusal),
            Answer::Block { .. } | Answer::Resumed | Answer::Resuming => {}
        }
    }

    /// Read what the decoder asked for, off the file it holds.
    fn serve_need(&mut self, offset: u64, len: usize) {
        if self.reading.len() < len {
            if self.reading.try_reserve(len - self.reading.len()).is_err() {
                self.fail(Failure::OutOfMemory);
                return;
            }
            self.reading.resize(len, 0);
        }
        let outcome = match self.files.read(offset, &mut self.reading[..len]) {
            Ok(read) if read == len => self.decoder.supply(&self.reading[..len]),
            Ok(_) => self.decoder.unreadable(),
            Err(errno) => {
                if let Some(current) = &mut self.current {
                    current.unreadable = Some(errno);
                }
                self.decoder.unreadable()
            }
        };
        if outcome == Err(AudioDecodeError::OutOfMemory) {
            self.fail(Failure::OutOfMemory);
        }
    }

    fn opened(&mut self) {
        let Some(info) = self.decoder.info().copied() else {
            return;
        };
        let Some(entry) = self.current.map(|current| current.entry) else {
            return;
        };
        let end = pass_end(self.programme.extent(entry), &info);
        let gain = self
            .decoder
            .metadata()
            .and_then(loudness::track_millibel)
            .filter(|&millibel| millibel != UNITY_MILLIBEL)
            .map(millibel_to_linear);
        let Some(current) = &mut self.current else {
            return;
        };
        let start = current.enter.frame_at(info.rate.hz());
        if info.frames.is_some_and(|frames| start >= frames) {
            self.notes.push(Note::Skipped {
                entry,
                why: Skip::StartsPastEnd,
            });
            self.file_done();
            return;
        }
        current.info = Some(info);
        current.end = end;
        current.gain = gain;
        current.phase = if start == 0 {
            Phase::Decoding { from: 0 }
        } else if info.seekable && self.decoder.seek(start).is_ok() {
            Phase::Seeking
        } else {
            Phase::Decoding { from: start }
        };
        self.progress.playable = true;
        self.notes.push(Note::Opened { entry, info });
    }

    fn decoded(&mut self, position: u64, mut bytes: Vec<u8>) {
        let Some(current) = self.current else {
            self.spare.push(bytes);
            return;
        };
        let (Some(info), Phase::Decoding { from }) = (current.info, current.phase) else {
            self.spare.push(bytes);
            return;
        };
        let frame_bytes = Shape::of(&info).frame_bytes();
        let frames = bytes.len() / frame_bytes;
        let skip =
            usize::try_from(from.saturating_sub(position)).map_or(frames, |skip| skip.min(frames));
        bytes.drain(..skip * frame_bytes);
        let first = position + skip as u64;
        let mut done = false;
        if let Some(end) = current.end {
            let left = usize::try_from(end.saturating_sub(first)).unwrap_or(usize::MAX);
            if frames - skip >= left {
                bytes.truncate(left * frame_bytes);
                done = true;
            }
        }
        if let (true, Some(gain)) = (self.settings.normalise, current.gain) {
            scale(
                info.sample,
                &mut bytes,
                gain,
                &mut self.pivot,
                &mut self.dither,
            );
        }
        if bytes.is_empty() {
            self.spare.push(bytes);
        } else {
            self.pending.push_back(Block {
                entry: current.entry,
                serial: current.serial,
                pass: self.progress.pass,
                info,
                frame: first,
                bytes,
                offset: 0,
            });
        }
        if done {
            self.file_done();
        }
    }

    fn refused(&mut self, refusal: AudioRefusal) {
        let Some(current) = self.current else {
            return;
        };
        let why = current
            .unreadable
            .map_or(Skip::Refused(refusal), Skip::Unreadable);
        let entry = current.entry;
        self.notes.push(if current.phase == Phase::Opening {
            Note::Skipped { entry, why }
        } else {
            Note::Cut { entry, why }
        });
        self.file_done();
    }

    fn decoder_error(&mut self, now: u64, error: AudioDecodeError) {
        match error {
            AudioDecodeError::Unbelievable => {
                self.session
                    .condemn(now, "the decoder sent an answer it could not have meant");
            }
            AudioDecodeError::CannotResume => self.gave_up(),
            AudioDecodeError::OutOfMemory => self.fail(Failure::OutOfMemory),
            AudioDecodeError::OutOfTurn
            | AudioDecodeError::WrongLength
            | AudioDecodeError::BlockSize => {
                self.session
                    .condemn(now, "the decoder's session left the protocol's turns");
            }
        }
    }

    /// The decoder failed twice at the same place in the current entry: cut
    /// it there and go on.
    fn gave_up(&mut self) {
        if let Some(current) = self.current {
            self.notes.push(Note::Cut {
                entry: current.entry,
                why: Skip::DecoderGaveUp,
            });
        }
        self.file_done();
    }

    /// The worker failed; a replacement is due at the session's deadline.
    fn worker_lost(&mut self) {
        self.changed = true;
    }

    /// A replacement worker is live: bring the file back to where it was.
    fn resume_decoder(&mut self) {
        let Some(seed) = (self.seed)() else {
            self.fail(Failure::NoDecoder);
            return;
        };
        match self.decoder.restart(seed) {
            Ok(()) => {}
            Err(AudioDecodeError::CannotResume) => self.gave_up(),
            Err(_) => self.fail(Failure::OutOfMemory),
        }
    }

    /// Queue the decoder's waiting frame to its worker.
    fn send_outgoing(&mut self, now: u64) -> bool {
        let Some(frame) = self.decoder.outgoing() else {
            return false;
        };
        match self.session.send(frame) {
            Ok(()) => {
                self.decoder.sent();
                if self.session.on_writable(now).is_err() {
                    self.worker_lost();
                }
                true
            }
            Err(SessionError::OutboundFull) => false,
            Err(SessionError::WorkerFailed) => {
                self.worker_lost();
                false
            }
            Err(_) => {
                self.session
                    .condemn(now, "a request to the decoder could not be framed");
                false
            }
        }
    }

    /// Ask the decoder for the next block, when it is free and the queue has
    /// room.
    fn ask_decoder(&mut self) -> bool {
        if !self.decoder.is_idle() || self.pending.len() >= AHEAD_BLOCKS || self.jump.is_some() {
            return false;
        }
        let Some(current) = self.current else {
            return false;
        };
        if !matches!(current.phase, Phase::Decoding { .. }) {
            return false;
        }
        let frames = current.end.map_or(MAX_BLOCK_FRAMES, |end| {
            let left = end.saturating_sub(self.decoder.position());
            u32::try_from(left)
                .unwrap_or(MAX_BLOCK_FRAMES)
                .min(MAX_BLOCK_FRAMES)
        });
        if frames == 0 {
            self.file_done();
            return true;
        }
        self.decoder.decode(frames).is_ok()
    }

    /// This pass of the current entry is decoded: open what follows it.
    fn file_done(&mut self) {
        let Some(current) = self.current.take() else {
            return;
        };
        match self.programme.next(current.entry, Advance::Played) {
            Some(next) => self.open_track(next),
            None => self.pass_done(),
        }
    }

    /// A pass of the programme is decoded: begin another, or say there is no
    /// more.
    fn pass_done(&mut self) {
        if !self.progress.playable {
            if self.stream.is_none() && self.pending.is_empty() {
                self.fail(Failure::NothingPlayable);
            } else {
                self.progress.exhausted = true;
            }
            return;
        }
        self.progress.pass = self.progress.pass.saturating_add(1);
        match self.programme.first_of_pass(self.progress.pass) {
            Some(first) => {
                self.progress.playable = false;
                self.open_track(first);
            }
            None => self.progress.exhausted = true,
        }
    }

    /// Open `entry` in the decoder at the start of its extent.
    fn open_track(&mut self, entry: EntryId) {
        self.open_at(entry, self.start_of(entry));
    }

    fn start_of(&self, entry: EntryId) -> Point {
        Point::Time(self.programme.extent(entry).start)
    }

    /// Open `entry` at `enter`, or failing that the first entry after it that
    /// opens; one walk, however many are left out, rather than one call deep
    /// for each.
    fn open_at(&mut self, mut entry: EntryId, mut enter: Point) {
        loop {
            let Err(why) = self.try_open(entry, enter) else {
                return;
            };
            self.notes.push(Note::Skipped { entry, why });
            let Some(next) = self.programme.next(entry, Advance::Played) else {
                self.current = None;
                self.pass_done();
                return;
            };
            entry = next;
            enter = self.start_of(next);
        }
    }

    /// Open `entry`'s file and hand it to the decoder, or say why it cannot be
    /// played.
    fn try_open(&mut self, entry: EntryId, enter: Point) -> Result<(), Skip> {
        let len = match self.programme.item(entry) {
            Some(item) => self.files.open(item).map_err(Skip::Open)?,
            None => return Err(Skip::Withdrawn),
        };
        let Some(seed) = (self.seed)() else {
            self.fail(Failure::NoDecoder);
            return Ok(());
        };
        self.current = Some(Current {
            entry,
            serial: self.serials,
            enter,
            info: None,
            end: None,
            phase: Phase::Opening,
            unreadable: None,
            gain: None,
        });
        self.serials = self.serials.wrapping_add(1);
        if self.decoder.open(len, None, seed).is_err() {
            self.fail(Failure::OutOfMemory);
        }
        Ok(())
    }

    /// Go where the waiting jump says, once the decoder is free.
    fn apply_jump(&mut self) -> bool {
        let Some(jump) = self.jump else {
            return false;
        };
        if !self.decoder.is_idle() {
            return false;
        }
        self.jump = None;
        if let Some(current) = &mut self.current {
            if let (true, Some(info)) = (current.entry == jump.entry, current.info) {
                if info.seekable
                    && self
                        .decoder
                        .seek(jump.point.frame_at(info.rate.hz()))
                        .is_ok()
                {
                    current.enter = jump.point;
                    current.phase = Phase::Seeking;
                    return true;
                }
            }
        }
        self.current = None;
        self.open_at(jump.entry, jump.point);
        true
    }

    /// Write queued blocks into the ring while it takes them.
    fn write_pending(&mut self) -> bool {
        let mut moved = false;
        loop {
            let Some(stream) = &mut self.stream else {
                return moved;
            };
            let Some(head) = self.pending.front_mut() else {
                return moved;
            };
            if stream.full || stream.draining || head.shape() != stream.shape {
                return moved;
            }
            let frame_bytes = stream.shape.frame_bytes();
            let offered = (head.bytes.len() - head.offset) / frame_bytes;
            let at = Frames::new(stream.next);
            let written = match self.speaker.write(at, &head.bytes[head.offset..]) {
                Ok(written) => written,
                Err(errno) => {
                    self.fail(Failure::Audio {
                        what: "take frames into the stream's ring",
                        errno,
                    });
                    return true;
                }
            };
            let frames = u64::from(written.sample_frames);
            if frames == 0 {
                stream.full = true;
                return moved;
            }
            moved = true;
            let continues = stream.segments.back().is_some_and(|segment| {
                segment.serial == head.serial
                    && segment.frame + (stream.next - segment.ring) == head.frame
            });
            if !continues {
                stream.segments.push_back(Segment {
                    ring: stream.next,
                    entry: head.entry,
                    serial: head.serial,
                    pass: head.pass,
                    frame: head.frame,
                    info: head.info,
                });
            }
            let bytes = usize::try_from(frames).unwrap_or(usize::MAX) * frame_bytes;
            meter(
                stream,
                &head.bytes[head.offset..head.offset + bytes],
                &mut self.pivot,
            );
            stream.next += frames;
            head.offset += bytes;
            head.frame += frames;
            if usize::try_from(frames).is_ok_and(|frames| frames < offered) {
                stream.full = true;
            }
            if head.offset >= head.bytes.len() {
                if let Some(done) = self.pending.pop_front() {
                    self.spare.push(done.bytes);
                }
            }
        }
    }

    /// Open a stream for the head block's shape, once the stream of another
    /// shape has played out.
    fn change_stream(&mut self) -> bool {
        let Some(head) = self.pending.front() else {
            return false;
        };
        let shape = head.shape();
        match &self.stream {
            Some(stream) if stream.shape == shape => false,
            Some(stream) if stream.draining || self.holds.any() => false,
            Some(_) => self.play_out(),
            None => {
                self.open_stream(shape);
                true
            }
        }
    }

    /// Start the stream if it has not begun, and have it play out what it
    /// holds and stop.
    fn play_out(&mut self) -> bool {
        let Some(stream) = &mut self.stream else {
            return false;
        };
        if !stream.started {
            if let Err(errno) = self.speaker.start(Frames::new(stream.resume_at)) {
                self.fail(Failure::Audio {
                    what: "start the stream",
                    errno,
                });
                return true;
            }
            stream.started = true;
        }
        if let Err(errno) = self.speaker.drain() {
            self.fail(Failure::Audio {
                what: "play out the stream",
                errno,
            });
            return true;
        }
        stream.draining = true;
        self.status.transport = Transport::Ending;
        self.changed = true;
        true
    }

    fn open_stream(&mut self, shape: Shape) {
        let latency = (shape.rate.hz() / RING_SECONDS_DIVISOR).clamp(1, ring_bounds::MAX_FRAMES);
        let params = OpenParams {
            device_id: self.settings.device_id,
            direction: StreamDirection::Playback,
            format: shape.sample,
            rate: shape.rate,
            channel_map: shape.channels,
            role: StreamRole::Media,
            latency_target_frames: latency,
        };
        let grant = match self.speaker.open(&params) {
            Ok(grant) => grant,
            Err(failure) => {
                self.fail(Failure::Stream(failure));
                return;
            }
        };
        if self.settings.gain != AudioGain::UNITY {
            if let Err(errno) = self.speaker.set_gain(self.settings.gain) {
                self.fail(Failure::Audio {
                    what: "set the stream's level",
                    errno,
                });
                return;
            }
        }
        self.stream = Some(Stream {
            shape,
            id: grant.stream_id,
            next: 0,
            read: 0,
            started: false,
            resume_at: 0,
            full: false,
            draining: false,
            discarded: 0,
            segments: VecDeque::new(),
            peaks: VecDeque::new(),
            span_frames: u64::from((shape.rate.hz() / METER_SPANS_PER_SECOND).max(1)),
            underruns_before: self.status.underruns,
            lost_before: self.status.lost_frames,
        });
    }

    /// Start a primed stream: its ring is full, or nothing more is coming for
    /// it.
    fn start_when_primed(&mut self) -> bool {
        if self.holds.any() {
            return false;
        }
        let Some(stream) = &mut self.stream else {
            return false;
        };
        if stream.started || stream.next == stream.resume_at {
            return false;
        }
        let nothing_more =
            self.progress.exhausted && self.current.is_none() && self.pending.is_empty()
                || self
                    .pending
                    .front()
                    .is_some_and(|head| head.shape() != stream.shape);
        if !stream.full && !nothing_more {
            return false;
        }
        if let Err(errno) = self.speaker.start(Frames::new(stream.resume_at)) {
            self.fail(Failure::Audio {
                what: "start the stream",
                errno,
            });
            return true;
        }
        stream.started = true;
        self.status.transport = Transport::Playing;
        self.changed = true;
        true
    }

    /// Play the last of the programme out, once every frame of it is in the
    /// ring.
    fn finish_when_played(&mut self) -> bool {
        if !self.progress.exhausted
            || self.current.is_some()
            || !self.pending.is_empty()
            || self.jump.is_some()
        {
            return false;
        }
        let Some(stream) = &self.stream else {
            self.end(Outcome::Played);
            return true;
        };
        if stream.draining || self.holds.any() {
            return false;
        }
        self.play_out()
    }

    fn adopt_notify(&mut self, notify: AudioNotify) {
        let Some(stream) = &mut self.stream else {
            return;
        };
        match notify {
            AudioNotify::SpaceAvailable {
                stream_id,
                position,
            } if stream_id == stream.id => {
                stream.read = position.get().min(stream.next);
                stream.full = false;
                self.show_position();
            }
            AudioNotify::StateChanged {
                stream_id,
                state,
                at,
            } if stream_id == stream.id => self.state_changed(state, at),
            AudioNotify::Xrun {
                stream_id,
                lost_frames,
                ..
            } if stream_id == stream.id => {
                self.status.underruns = self.status.underruns.saturating_add(1);
                self.status.lost_frames = self.status.lost_frames.saturating_add(lost_frames);
                self.changed = true;
            }
            _ => {}
        }
    }

    fn state_changed(&mut self, state: StreamState, at: Frames) {
        match state {
            StreamState::Idle => {
                let draining = self.stream.as_ref().is_some_and(|stream| stream.draining);
                if draining {
                    self.close_stream(at.get());
                }
            }
            StreamState::SeatInactive => {
                self.holds.seat = true;
                self.status.transport = Transport::Held;
            }
            // Released from the seat into whatever the stream was doing: a
            // stream held mid-drain comes back draining, a paused one paused.
            StreamState::Running | StreamState::Draining | StreamState::Paused => {
                if self.holds.seat {
                    self.holds.seat = false;
                    self.status.transport = if self.holds.user {
                        Transport::Paused
                    } else {
                        Transport::Playing
                    };
                }
            }
            StreamState::DeviceLost => self.fail(Failure::DeviceLost),
            StreamState::Faulted => self.fail(Failure::RingFaulted),
        }
        self.changed = true;
    }

    /// The stream played out: close it, counting what it played.
    fn close_stream(&mut self, played: u64) {
        let Some(stream) = self.stream.take() else {
            return;
        };
        self.status.heard_frames = self
            .status
            .heard_frames
            .saturating_add(played.max(stream.read).saturating_sub(stream.discarded));
        self.settle_underruns(&stream);
        if let Err(errno) = self.speaker.close() {
            self.fail(Failure::Audio {
                what: "close the stream",
                errno,
            });
            return;
        }
        if self.progress.exhausted && self.current.is_none() && self.pending.is_empty() {
            self.end(Outcome::Played);
        }
    }

    /// Turn the ring frame the service has read up to into the entry and
    /// frame being heard.
    fn show_position(&mut self) {
        let Some(stream) = &mut self.stream else {
            return;
        };
        let read = stream.read;
        while stream.segments.len() > 1
            && stream.segments.get(1).is_some_and(|next| next.ring <= read)
        {
            stream.segments.pop_front();
        }
        while stream.peaks.len() > 1 && stream.peaks.front().is_some_and(|span| span.end <= read) {
            stream.peaks.pop_front();
        }
        if let Some(segment) = stream.segments.front() {
            self.status.heard = Some((segment.entry, segment.info));
            self.status.pass = segment.pass;
            self.status.position = segment.frame + read.saturating_sub(segment.ring);
            let channels = usize::from(segment.info.channels.channels());
            let levels = stream
                .peaks
                .front()
                .map_or([0; MAX_CHANNELS], |span| span.levels);
            self.status.peaks = (levels, channels);
        }
        self.changed = true;
    }

    fn toggle_pause(&mut self) {
        let Some(stream) = &mut self.stream else {
            self.holds.user = !self.holds.user;
            self.status.transport = if self.holds.user {
                Transport::Paused
            } else {
                Transport::Starting
            };
            return;
        };
        if self.holds.user {
            self.holds.user = false;
            self.status.transport = Transport::Starting;
            if stream.started {
                if let Err(errno) = self.speaker.start(Frames::new(stream.resume_at)) {
                    self.fail(Failure::Audio {
                        what: "play on",
                        errno,
                    });
                    return;
                }
                // Resumed while the seat is elsewhere, it waits for the room.
                self.status.transport = if self.holds.seat {
                    Transport::Held
                } else {
                    Transport::Playing
                };
                if stream.draining {
                    if let Err(errno) = self.speaker.drain() {
                        self.fail(Failure::Audio {
                            what: "play out the stream",
                            errno,
                        });
                    }
                }
            }
            return;
        }
        self.holds.user = true;
        self.status.transport = Transport::Paused;
        if stream.started {
            match self.speaker.pause() {
                Ok(at) => stream.resume_at = at.get(),
                Err(errno) => self.fail(Failure::Audio {
                    what: "pause",
                    errno,
                }),
            }
        }
    }

    /// Where the listener is: a waiting jump's target, else the entry being
    /// heard, else the one being decoded.
    fn heard(&self) -> Option<Heard> {
        if let Some(jump) = self.jump {
            let info = self
                .current
                .filter(|current| current.entry == jump.entry)
                .and_then(|current| current.info);
            return Some(Heard {
                entry: jump.entry,
                pass: jump.pass,
                at: jump.point.span(),
                info,
            });
        }
        let ring = self
            .stream
            .as_ref()
            .and_then(|stream| Some((stream.segments.front()?, stream.read)));
        if let Some((segment, read)) = ring {
            let frame = segment.frame + read.saturating_sub(segment.ring);
            return Some(Heard {
                entry: segment.entry,
                pass: segment.pass,
                at: Span::of_frames(frame, segment.info.rate.hz()),
                info: Some(segment.info),
            });
        }
        let current = self.current?;
        let at = current.info.map_or(current.enter.span(), |info| {
            Span::of_frames(self.decoder.position(), info.rate.hz())
        });
        Some(Heard {
            entry: current.entry,
            pass: self.progress.pass,
            at,
            info: current.info,
        })
    }

    fn seek_by(&mut self, by: Span, forward: bool) {
        let Some(heard) = self.heard() else {
            return;
        };
        let target = if forward {
            heard.at.plus(by)
        } else {
            heard.at.less(by)
        };
        self.seek_in(heard, target);
    }

    fn seek_to(&mut self, at: Span) {
        if let Some(heard) = self.heard() {
            self.seek_in(heard, at);
        }
    }

    /// Go to `target` in the entry `heard` names, never before its extent's
    /// start; a target at or past its end is the next entry.
    fn seek_in(&mut self, heard: Heard, target: Span) {
        let target = target.max(self.programme.extent(heard.entry).start);
        let past_end = heard.info.is_some_and(|info| {
            info.frames
                .is_some_and(|frames| target.frames_at(info.rate.hz()) >= frames)
        });
        if past_end {
            self.skip_track(true);
            return;
        }
        self.go_to(Jump {
            entry: heard.entry,
            point: Point::Time(target),
            pass: heard.pass,
        });
    }

    fn skip_track(&mut self, forward: bool) {
        let Some(heard) = self.heard() else {
            return;
        };
        let played = heard.at.less(self.programme.extent(heard.entry).start);
        let target = if forward {
            self.programme
                .next(heard.entry, Advance::Skipped)
                .map(|entry| (entry, heard.pass))
                .or_else(|| {
                    let pass = heard.pass.checked_add(1)?;
                    Some((self.programme.first_of_pass(pass)?, pass))
                })
        } else if played > RESTART_SPAN {
            Some((heard.entry, heard.pass))
        } else {
            Some((
                self.programme.previous(heard.entry).unwrap_or(heard.entry),
                heard.pass,
            ))
        };
        match target {
            Some((entry, pass)) => self.jump_to(entry, pass),
            None => self.stop(Outcome::Stopped),
        }
    }

    fn jump_to(&mut self, entry: EntryId, pass: u32) {
        self.go_to(Jump {
            entry,
            point: self.start_of(entry),
            pass,
        });
    }

    /// Drop what is queued and play where `jump` says.
    fn go_to(&mut self, jump: Jump) {
        if let Some(reached) = self.halt() {
            self.discard(reached, false);
        }
        if self.outcome.is_some() {
            return;
        }
        self.drop_pending_from(0);
        self.set_jump(jump);
        self.status.transport = if self.holds.user {
            Transport::Paused
        } else {
            Transport::Starting
        };
    }

    /// Make `jump` the next thing decoded, in its pass.
    fn set_jump(&mut self, jump: Jump) {
        // An entry of an earlier pass was heard, so that pass played; a later
        // one has yet to prove an entry of it can.
        if jump.pass != self.progress.pass {
            self.progress.playable = jump.pass < self.progress.pass;
            self.progress.pass = jump.pass;
        }
        self.progress.exhausted = false;
        self.jump = Some(jump);
    }

    /// Stop the open stream, answering the ring frame it stopped on: where it
    /// was stopped already, where the listener paused it, or where the seat
    /// held it.
    fn halt(&mut self) -> Option<u64> {
        let stream = self.stream.as_mut()?;
        if stream.started && !self.holds.any() {
            match self.speaker.pause() {
                Ok(at) => Some(at.get()),
                Err(errno) => {
                    self.fail(Failure::Audio {
                        what: "pause",
                        errno,
                    });
                    None
                }
            }
        } else if self.holds.seat {
            Some(stream.read)
        } else {
            Some(stream.resume_at)
        }
    }

    /// Give up everything the stream queued past `reached`: flushed from it,
    /// or the stream closed whole.
    fn discard(&mut self, reached: u64, close: bool) {
        if close {
            if let Some(stream) = self.stream.take() {
                self.status.heard_frames = self
                    .status
                    .heard_frames
                    .saturating_add(reached.saturating_sub(stream.discarded));
                self.settle_underruns(&stream);
                if let Err(errno) = self.speaker.close() {
                    self.fail(Failure::Audio {
                        what: "close the stream",
                        errno,
                    });
                }
            }
            return;
        }
        let Some(stream) = &mut self.stream else {
            return;
        };
        if let Err(errno) = self.speaker.flush() {
            self.fail(Failure::Audio {
                what: "discard what is queued",
                errno,
            });
            return;
        }
        stream.discarded = stream
            .discarded
            .saturating_add(stream.next.saturating_sub(reached));
        stream.started = false;
        stream.draining = false;
        stream.full = false;
        stream.resume_at = stream.next;
        stream.read = stream.next;
        stream.segments.clear();
        stream.peaks.clear();
    }

    /// Where the stream's ring frame `reached` lies in the programme.
    fn place_of(&self, reached: u64) -> Option<Jump> {
        let segment = self
            .stream
            .as_ref()?
            .segments
            .iter()
            .rev()
            .find(|segment| segment.ring <= reached)?;
        Some(Jump {
            entry: segment.entry,
            point: Point::Frame {
                frame: segment.frame + (reached - segment.ring),
                hz: segment.info.rate.hz(),
            },
            pass: segment.pass,
        })
    }

    /// Stop on the frame being heard, give up everything queued after it, and
    /// decode on from exactly there.
    ///
    /// With `stale`, the ring frames from its first onwards no longer follow
    /// from the programme: a listener already past that frame goes on with
    /// what the programme now has there instead. Without it the stream is
    /// closed and what follows opens a fresh one.
    fn refill_from_heard(&mut self, stale: Option<(u64, Option<(EntryId, u32)>)>) {
        let Some(reached) = self.halt() else {
            return;
        };
        let place = match stale {
            Some((from, expected)) if reached >= from => expected.map(|(entry, pass)| Jump {
                entry,
                point: self.start_of(entry),
                pass,
            }),
            _ => self.place_of(reached).or_else(|| {
                self.current.map(|current| Jump {
                    entry: current.entry,
                    point: current.enter,
                    pass: self.progress.pass,
                })
            }),
        };
        self.discard(reached, stale.is_none());
        if self.outcome.is_some() {
            return;
        }
        self.drop_pending_from(0);
        match place {
            Some(place) => self.set_jump(place),
            None => self.redirect(None),
        }
    }

    /// Release the pending blocks from `at` on.
    fn drop_pending_from(&mut self, at: usize) {
        while self.pending.len() > at {
            if let Some(block) = self.pending.pop_back() {
                self.spare.push(block.bytes);
            }
        }
    }

    /// The programme changed: keep the entry being heard and everything queued
    /// after it that still follows from the programme, and re-plan the rest; an
    /// entry being heard that left the programme is left at once.
    fn replan(&mut self) {
        if self.outcome.is_some() || self.jump.is_some() {
            return;
        }
        let mut last: Option<Group> = None;
        let mut stale = None;
        for group in self.groups() {
            let expected = match last {
                Some(before) if before.serial == group.serial => continue,
                Some(before) => self.follow(before.entry, before.pass),
                // The entry being heard has gone: what followed it, at once.
                None if self.programme.item(group.entry).is_none() => {
                    self.follow(group.entry, group.pass)
                }
                None => Some((group.entry, group.pass)),
            };
            if expected != Some((group.entry, group.pass)) {
                stale = Some((group.place, expected));
                break;
            }
            last = Some(group);
        }
        match stale {
            Some((Place::Ring(from), expected)) => self.refill_from_heard(Some((from, expected))),
            Some((Place::Pending(at), expected)) => {
                self.drop_pending_from(at);
                self.redirect(expected);
            }
            Some((Place::Decoder, expected)) => self.redirect(expected),
            None => {
                let ran_out = self.current.is_none() && self.progress.exhausted;
                if let (true, Some(last)) = (ran_out, last) {
                    let expected = self.follow(last.entry, last.pass);
                    if expected.is_some() {
                        self.redirect(expected);
                    }
                }
            }
        }
    }

    /// Every play of an entry the stream, the pending blocks and the decoder
    /// hold, in the order they are heard.
    fn groups(&self) -> impl Iterator<Item = Group> + '_ {
        let ring = self
            .stream
            .iter()
            .flat_map(|stream| stream.segments.iter())
            .map(|segment| Group {
                serial: segment.serial,
                entry: segment.entry,
                pass: segment.pass,
                place: Place::Ring(segment.ring),
            });
        let pending = self.pending.iter().enumerate().map(|(at, block)| Group {
            serial: block.serial,
            entry: block.entry,
            pass: block.pass,
            place: Place::Pending(at),
        });
        let decoder = self.current.iter().map(|current| Group {
            serial: current.serial,
            entry: current.entry,
            pass: self.progress.pass,
            place: Place::Decoder,
        });
        ring.chain(pending).chain(decoder)
    }

    /// What the programme plays after `entry` in pass `pass`, and in which
    /// pass.
    fn follow(&self, entry: EntryId, pass: u32) -> Option<(EntryId, u32)> {
        if let Some(next) = self.programme.next(entry, Advance::Played) {
            return Some((next, pass));
        }
        let pass = pass.checked_add(1)?;
        Some((self.programme.first_of_pass(pass)?, pass))
    }

    /// Have the decoder leave what it holds and go on with `to`; with nothing
    /// to go on with, what is queued is the last of it.
    fn redirect(&mut self, to: Option<(EntryId, u32)>) {
        if let Some((entry, pass)) = to {
            self.set_jump(Jump {
                entry,
                point: self.start_of(entry),
                pass,
            });
        } else {
            self.current = None;
            self.progress.exhausted = true;
        }
    }

    fn step_level(&mut self, step: i32) {
        let level = self.status.gain.millibel().saturating_add(step).min(0);
        if let Ok(gain) = AudioGain::new(level) {
            self.set_gain(gain);
        }
    }

    fn set_gain(&mut self, gain: AudioGain) {
        self.status.gain = gain;
        self.settings.gain = gain;
        if self.stream.is_some() {
            if let Err(errno) = self.speaker.set_gain(gain) {
                self.fail(Failure::Audio {
                    what: "set the stream's level",
                    errno,
                });
            }
        }
    }

    fn set_device(&mut self, device_id: u32) {
        if device_id == self.settings.device_id {
            return;
        }
        self.settings.device_id = device_id;
        if self.stream.is_some() {
            self.refill_from_heard(None);
        }
    }

    /// End playback with `outcome`, giving the stream back.
    fn stop(&mut self, outcome: Outcome) {
        if let Some(stream) = self.stream.take() {
            self.status.heard_frames = self
                .status
                .heard_frames
                .saturating_add(stream.read.saturating_sub(stream.discarded));
            if stream.started && !self.holds.user {
                let _ = self.speaker.pause();
            }
            self.settle_underruns(&stream);
            let _ = self.speaker.close();
        }
        self.drop_pending_from(0);
        self.end(outcome);
    }

    /// Playback is over: nothing waits to be decoded and the transport rests.
    fn end(&mut self, outcome: Outcome) {
        self.jump = None;
        self.outcome = Some(outcome);
        self.status.transport = Transport::Stopped;
        self.changed = true;
    }

    /// Take `stream`'s underruns from the service's own count, which its
    /// best-effort notifications may have fallen short of. Without a report
    /// the notified count stands: the tally is incidental to the playback.
    fn settle_underruns(&mut self, stream: &Stream) {
        if let Ok(report) = self.speaker.report() {
            self.status.underruns = stream.underruns_before.saturating_add(report.xruns);
            self.status.lost_frames = stream.lost_before.saturating_add(report.xrun_frames);
        }
    }

    fn fail(&mut self, failure: Failure) {
        self.stop(Outcome::Failed(failure));
    }
}

/// An answer from the decoder, its frames moved into the block it carries.
#[derive(Copy, Clone)]
enum Answer {
    Need { offset: u64, len: usize },
    Opened,
    Block { position: u64 },
    Ended,
    Sought { position: u64 },
    Refused(AudioRefusal),
    Resumed,
    Resuming,
}

/// What taking one frame from the decoder came to.
enum Taken {
    Answer(Answer),
    OutOfMemory,
    Error(AudioDecodeError),
}

impl Answer {
    fn from_event(event: DecodeEvent<'_>) -> Self {
        match event {
            DecodeEvent::Need { offset, len } => Self::Need { offset, len },
            DecodeEvent::Opened => Self::Opened,
            DecodeEvent::Block { position, .. } => Self::Block { position },
            DecodeEvent::Ended { .. } => Self::Ended,
            DecodeEvent::Sought { position } => Self::Sought { position },
            DecodeEvent::Refused(refusal) => Self::Refused(refusal),
            DecodeEvent::Resumed { .. } => Self::Resumed,
            DecodeEvent::Resuming => Self::Resuming,
        }
    }
}

/// The frame `extent`'s pass of a file `info` describes ends at: its start
/// plus its duration, and never past its last frame.
fn pass_end(extent: Extent, info: &SoundInfo) -> Option<u64> {
    let hz = info.rate.hz();
    let duration = extent.duration?;
    let end = extent
        .start
        .frames_at(hz)
        .saturating_add(duration.frames_at(hz));
    Some(info.frames.map_or(end, |frames| end.min(frames)))
}

/// Multiply every sample of `bytes` by `gain` through the `f32` pivot,
/// dithered back onto the format's grid: a gain leaves the material finer than
/// the grid it is requantised to.
fn scale(
    sample: SampleFormat,
    bytes: &mut [u8],
    gain: f32,
    pivot: &mut [f32],
    dither: &mut DitherSource,
) {
    let samples = convert::decode(sample, bytes, pivot);
    for value in &mut pivot[..samples] {
        *value *= gain;
    }
    dither.apply(sample, &mut pivot[..samples]);
    convert::encode(sample, &pivot[..samples], bytes);
}

/// Fold `bytes`, frames just written at the stream's write position, into its
/// peak spans.
fn meter(stream: &mut Stream, bytes: &[u8], pivot: &mut [f32]) {
    let channels = usize::from(stream.shape.channels.channels());
    let samples = convert::decode(stream.shape.sample, bytes, pivot);
    let frames = samples / channels.max(1);
    for (at, frame) in (stream.next..).zip(pivot[..frames * channels].chunks_exact(channels)) {
        let opens = stream.peaks.back().is_none_or(|span| at >= span.end);
        if opens {
            stream.peaks.push_back(PeakSpan {
                end: at + stream.span_frames,
                levels: [0; MAX_CHANNELS],
            });
        }
        if let Some(span) = stream.peaks.back_mut() {
            for (level, sample) in span.levels.iter_mut().zip(frame) {
                *level = (*level).max(level_of(*sample));
            }
        }
    }
}

/// A sample's magnitude, of 255.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the value is clamped into 0..=255 first, so the cast is exact"
)]
fn level_of(sample: f32) -> u8 {
    let magnitude = if sample < 0.0 { -sample } else { sample };
    (magnitude.min(1.0) * 255.0) as u8
}

#[cfg(test)]
#[path = "engine_tests.rs"]
mod tests;
