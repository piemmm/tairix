//! A movie's playback: the sandboxed movie decoder's pictures, paced by the
//! movie's own sound, and its sound played into one `audio-v1` stream through
//! the players' stream seam ([`Speaker`]).
//!
//! [`Player`] is the playback thread's state machine. It holds the decoder's
//! session, reads the file the decoder asks for, and writes the sound into
//! the stream as it has room, half a second of it decoded ahead. It keeps the
//! pictures the decoder's ring holds, handing each to the window when its
//! time comes; the window copies it out of its slot and hands the slot back.
//! Nothing here waits: the host parks on [`Player::decoder_members`], the
//! stream's mailbox and [`Player::deadline`], and hands back what woke it.
//!
//! # The clock
//!
//! With sound the clock is the sound's: each report of how far the audio
//! service has read the ring re-anchors it, and between reports it runs on the
//! monotonic clock, so the pictures follow the sound however the device's
//! clock drifts from the machine's. A silent movie, or one whose stream was
//! lost, runs on the monotonic clock alone. A picture is shown at its time, a
//! one-shot of the host's timer each; one still waiting when the next is due
//! is dropped, and one the decoder reaches too late is not drawn at all, so a
//! slow machine shows fewer pictures rather than holding the sound back.

use alloc::collections::VecDeque;
use alloc::sync::Arc;
use alloc::vec::Vec;

use tairix_abi::audio::{AudioNotify, OpenParams, StreamRole, StreamState, Tenancy};
use tairix_abi::driver::audio::{ring_bounds, ChannelMap, Frames, SampleFormat, StreamDirection};
use tairix_abi::Errno;
use tairix_hash::HashSeed;
use tairix_log::Sink;
use tairix_player::{fetch, Failure, Span, Speaker, Unfetched};
use tairix_sandbox::moviedecode::{
    session_bounds, Ending, MovieDecodeClient, MovieDecodeError, MovieEvent, MovieInfo,
    MovieRefusal, PictureHead, RingLayout, RING_SLOTS, SOUND_RATE,
};
use tairix_sandbox::session::{SessionDescriptors, Share};
use tairix_sandbox::supervise::{send_waiting, Sent, SessionLauncher, SupervisedSession};

/// Pictures decoded ahead of the one shown: every slot of the ring but the
/// one the window takes a picture from.
const AHEAD_PICTURES: usize = RING_SLOTS - 1;

/// Sound decoded ahead of what the service has read.
const SOUND_AHEAD_NS: u64 = 500_000_000;

/// A ring of half a second, as the music players keep: deep enough that a slow
/// read never reaches the device, and still no slower to pause or seek.
const RING_SECONDS_DIVISOR: u32 = 2;

/// How late a picture may be decoded and still be drawn.
const LATE_NS: u64 = 100_000_000;

/// The longest the window goes without a picture while playing: past this the
/// next is drawn however late, so a machine slower than the movie still shows
/// some of it.
const MAX_UNSHOWN_NS: u64 = 250_000_000;

/// Sound blocks kept for reuse.
const SPARE_BLOCKS: usize = 16;

/// What a window asks of a playback.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Order {
    /// Play from where it is; from the start once it has stopped.
    Play,
    /// Pause on the picture shown.
    Pause,
    /// Go to the latest keyframe at or before this time of the movie.
    Seek(u64),
}

/// Where a playback stands.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Transport {
    /// The movie is being opened.
    Opening,
    /// Paused on its picture.
    Paused,
    /// Playing.
    Playing,
    /// Stopped, and why.
    Stopped(Stop),
}

/// Why a playback stopped.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Stop {
    /// At the movie's end.
    Ended,
    /// Where the file is cut short.
    CutShort,
    /// Where the decoder refused what followed.
    Refused(MovieRefusal),
    /// Where the decoder failed a second time.
    DecoderGaveUp,
    /// The sandboxed decoder could not be started.
    NoDecoder,
    /// There was not the memory to go on.
    OutOfMemory,
    /// The file could not be read.
    Unreadable(Errno),
    /// What playback waits on could not be watched.
    Abandoned(Failure),
}

impl Stop {
    /// Whether playback cannot go on from here at all: a movie stopped for
    /// any other reason plays again from its start.
    #[must_use]
    pub const fn is_final(self) -> bool {
        matches!(
            self,
            Self::NoDecoder | Self::OutOfMemory | Self::DecoderGaveUp | Self::Abandoned(_)
        )
    }
}

impl core::fmt::Display for Stop {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Ended => f.write_str("the movie has ended"),
            Self::CutShort => f.write_str("the file ends part-way through the movie"),
            Self::Refused(refusal) => write!(f, "{refusal}"),
            Self::DecoderGaveUp => f.write_str("the decoder failed twice at the same place"),
            Self::NoDecoder => f.write_str("the sandboxed decoder could not be started"),
            Self::OutOfMemory => f.write_str("there is not the memory to play the movie"),
            Self::Unreadable(errno) => write!(f, "the file could not be read ({errno})"),
            Self::Abandoned(failure) => write!(f, "{failure}"),
        }
    }
}

/// What a host hears of a playback as it happens.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Note {
    /// The movie opened, paused on its first picture.
    Opened(MovieInfo),
    /// The movie would not open, and why.
    Unopened(Stop),
    /// The decoder failed and was replaced, and playback went on from the
    /// latest keyframe it had passed.
    Replaced,
    /// The sound cannot be played, and why; the pictures go on silent.
    SoundLost(Failure),
}

/// A picture to show: when, and the slot of the decoder's ring it waits in.
pub struct Picture {
    /// When it is shown, in nanoseconds of the movie.
    pub at_ns: u64,
    /// Where its pixels are.
    pub slot: Slot,
}

/// A slot of a decoder's ring holding a picture, held until the window has
/// copied the picture out and handed the slot back ([`Player::release`]).
///
/// The ring stays mapped while a slot of it is held, however soon its decoder
/// is replaced.
pub struct Slot {
    ring: Arc<dyn Share>,
    generation: u64,
    index: u8,
    at: usize,
    size: (u32, u32),
}

impl Slot {
    /// The picture's size: the stream's own, or scaled down to fit a slot.
    #[must_use]
    pub const fn size(&self) -> (u32, u32) {
        self.size
    }

    /// Copy the picture's straight-alpha RGBA8 pixels out of the ring into
    /// `out`, which holds exactly the picture, answering whether it could.
    /// The decoder may be writing the ring, so this copy is the only read of
    /// it.
    pub fn copy_out(&self, out: &mut [u8]) -> bool {
        let len = self.size.0 as usize * self.size.1 as usize * 4;
        out.len() == len && self.ring.copy_out(self.at, out)
    }

    /// What hands the slot back to its playback.
    #[must_use]
    pub const fn release(&self) -> Release {
        Release {
            generation: self.generation,
            index: self.index,
        }
    }
}

#[cfg(test)]
impl Slot {
    /// Slot `index` of `ring`, starting at `at`, as a decoder of `generation`
    /// drew a picture of `size` into it.
    pub(crate) fn of(
        ring: Arc<dyn Share>,
        (generation, index): (u64, u8),
        at: usize,
        size: (u32, u32),
    ) -> Self {
        Self {
            ring,
            generation,
            index,
            at,
            size,
        }
    }
}

/// A slot the window is done with, for [`Player::release`].
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Release {
    generation: u64,
    index: u8,
}

impl Release {
    /// The slot's place in its ring.
    #[must_use]
    pub const fn index(self) -> usize {
        self.index as usize
    }
}

/// What a window shows of a playback.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Status {
    /// Where the transport stands.
    pub transport: Transport,
    /// The time of the movie shown, in nanoseconds.
    pub position_ns: u64,
    /// Pictures dropped for arriving too late to show.
    pub late: u32,
    /// How many seeks the playback has been ordered, so a window tells a
    /// position from before its latest seek.
    pub seeks: u32,
}

/// The movie's time: stopped at a reading, or running from one.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Clock {
    Stopped(u64),
    /// At monotonic instant `at_ns` the movie was at `media_ns`.
    Running {
        media_ns: u64,
        at_ns: u64,
    },
}

impl Clock {
    /// The movie's time at monotonic instant `now_ns`.
    const fn media_at(self, now_ns: u64) -> u64 {
        match self {
            Self::Stopped(media_ns) => media_ns,
            Self::Running { media_ns, at_ns } => {
                media_ns.saturating_add(now_ns.saturating_sub(at_ns))
            }
        }
    }

    /// The monotonic instant the movie reaches `media_ns`, while it runs.
    const fn instant_of(self, media_ns: u64) -> Option<u64> {
        match self {
            Self::Stopped(_) => None,
            Self::Running {
                media_ns: from,
                at_ns,
            } => Some(at_ns.saturating_add(media_ns.saturating_sub(from))),
        }
    }
}

/// Where the stream is.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Phase {
    /// Filling before it starts, or after a seek.
    Primed,
    Running,
    Paused,
    /// Playing out the last of the movie.
    Draining,
}

/// Sound decoded and not yet in the ring.
struct Block {
    at_ns: u64,
    bytes: Vec<u8>,
    /// Bytes already written.
    offset: usize,
}

/// The open stream.
struct Sound {
    id: u64,
    frame_bytes: usize,
    /// The ring frame the next write lands at.
    next: u64,
    /// The ring frame the service has read up to.
    read: u64,
    /// Where a start begins.
    resume_at: u64,
    phase: Phase,
    /// The ring took less than offered, so the next write waits for room.
    full: bool,
    /// The seat holds the stream, so its clock stands still.
    held: bool,
    /// Where runs of ring frames start, and the movie's time each plays.
    segments: VecDeque<(u64, u64)>,
    pending: VecDeque<Block>,
}

impl Sound {
    fn new(id: u64, channels: ChannelMap) -> Self {
        Self {
            id,
            frame_bytes: 4 * usize::from(channels.channels()),
            next: 0,
            read: 0,
            resume_at: 0,
            phase: Phase::Primed,
            full: false,
            held: false,
            segments: VecDeque::new(),
            pending: VecDeque::new(),
        }
    }

    /// How far past what the service has read the sound is decoded.
    fn ahead_ns(&self) -> u64 {
        let pending: usize = self
            .pending
            .iter()
            .map(|block| block.bytes.len() - block.offset)
            .sum();
        let frames = (pending / self.frame_bytes) as u64 + (self.next - self.read);
        Span::of_frames(frames, SOUND_RATE.hz()).nanos()
    }
}

/// The movie's time ring frame `ring` plays, by the run it lies in.
fn media_of(segments: &VecDeque<(u64, u64)>, ring: u64) -> Option<u64> {
    let (start, media) = segments.iter().rev().find(|(start, _)| *start <= ring)?;
    let into = Span::of_frames(ring - start, SOUND_RATE.hz()).nanos();
    Some(media.saturating_add(into))
}

/// Record that ring frame `ring` plays the movie's `media_ns`, unless the run
/// it lies in already says so to within a frame.
fn place(segments: &mut VecDeque<(u64, u64)>, ring: u64, media_ns: u64) {
    let frame_ns = Span::of_frames(1, SOUND_RATE.hz()).nanos() + 1;
    let continues =
        media_of(segments, ring).is_some_and(|known| known.abs_diff(media_ns) <= frame_ns);
    if !continues {
        segments.push_back((ring, media_ns));
    }
}

/// Forget the runs that end at or before `read`.
fn settle(segments: &mut VecDeque<(u64, u64)>, read: u64) {
    while segments.len() > 1 && segments.get(1).is_some_and(|(start, _)| *start <= read) {
        segments.pop_front();
    }
}

/// Keep `buffer` for reuse, while `pool` holds fewer than `most`.
fn keep(pool: &mut Vec<Vec<u8>>, buffer: Vec<u8>, most: usize) {
    if pool.len() < most {
        pool.push(buffer);
    }
}

/// What the decoder was last asked.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Asked {
    Nothing,
    Open,
    Next,
    Sound,
    Seek,
    /// A replacement's return to where the movie was.
    Resume,
}

/// What the units a replacement plays again had already brought: the latest
/// picture, and where the sound ended.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct Replay {
    picture_ns: Option<u64>,
    sound_ns: Option<u64>,
}

/// One frame from the decoder, acted on as far as it can be while the session
/// lends it.
enum Taken {
    Need { offset: u64, len: usize },
    Opened,
    Picture(PictureHead),
    Skipped(u64),
    Sound(u64, Vec<u8>),
    Ended(Ending),
    Sought,
    Refused(MovieRefusal),
    Resumed,
    Resuming,
    OutOfMemory,
    Error(MovieDecodeError),
}

/// Read one frame through `client`, a block of sound into a buffer from
/// `blocks`.
fn take(client: &mut MovieDecodeClient, blocks: &mut Vec<Vec<u8>>, frame: &[u8]) -> Taken {
    match client.on_frame(frame) {
        Ok(MovieEvent::Need { offset, len }) => Taken::Need { offset, len },
        Ok(MovieEvent::Opened) => Taken::Opened,
        Ok(MovieEvent::Picture(head)) => Taken::Picture(head),
        Ok(MovieEvent::Skipped { at_ns }) => Taken::Skipped(at_ns),
        Ok(MovieEvent::Sound { at_ns, pcm }) => {
            let mut bytes = blocks.pop().unwrap_or_default();
            bytes.clear();
            if bytes.try_reserve(pcm.len()).is_err() {
                return Taken::OutOfMemory;
            }
            bytes.extend_from_slice(pcm);
            Taken::Sound(at_ns, bytes)
        }
        Ok(MovieEvent::Ended(ending)) => Taken::Ended(ending),
        Ok(MovieEvent::Sought) => Taken::Sought,
        Ok(MovieEvent::Refused(refusal)) => Taken::Refused(refusal),
        Ok(MovieEvent::Resumed) => Taken::Resumed,
        Ok(MovieEvent::Resuming) => Taken::Resuming,
        Err(error) => Taken::Error(error),
    }
}

/// What an audio-service notification leaves to do once the stream is let go.
enum After {
    Stop(Stop),
    Lose(Failure),
}

/// A movie's playback.
pub struct Player<L, S, R, O>
where
    L: SessionLauncher,
    S: Sink + Clone,
    R: FnMut(u64, &mut [u8]) -> Result<usize, Errno>,
    O: Speaker,
{
    session: SupervisedSession<L, S>,
    client: MovieDecodeClient,
    read: R,
    len: u64,
    ceiling: u64,
    layout: RingLayout,
    speaker: O,
    seed: fn() -> Option<HashSeed>,
    reading: Vec<u8>,
    info: Option<MovieInfo>,
    asked: Asked,
    /// The outstanding request was asked before a seek, so its answer is not
    /// wanted.
    stale: bool,
    /// The live decoder's ring, and which of the decoders it is.
    ring: Option<Arc<dyn Share>>,
    generation: u64,
    transport: Transport,
    clock: Clock,
    /// Pictures decoded ahead, in the order they are shown.
    ahead: VecDeque<Picture>,
    /// The picture due, for the host to hand the window.
    posted: Option<Picture>,
    /// When the picture last handed out is shown.
    shown_ns: Option<u64>,
    blocks: Vec<Vec<u8>>,
    sound: Option<Sound>,
    /// A seek waiting for the decoder to be free.
    seek: Option<u64>,
    replay: Option<Replay>,
    /// The latest picture taken, and where the sound taken ends.
    pictured_ns: Option<u64>,
    sounded_ns: Option<u64>,
    /// Why the file's bytes could not be read, for the refusal that follows.
    unread: Option<Errno>,
    /// How the pictures' walk and the sound's ended, once each has.
    pictures_end: Option<Ending>,
    sound_end: Option<Ending>,
    /// Why the decoder will not go on, once it will not: the movie plays out
    /// what is decoded, then stops for this.
    halted: Option<Stop>,
    changed: bool,
    notes: Vec<Note>,
    late: u32,
    seeks: u32,
}

impl<L, S, R, O> Player<L, S, R, O>
where
    L: SessionLauncher,
    S: Sink + Clone,
    R: FnMut(u64, &mut [u8]) -> Result<usize, Errno>,
    O: Speaker,
{
    /// A player of the `len`-byte movie `read` reads, decoded in workers
    /// `launcher` starts, each handed a ring of `layout` and allowed to map
    /// `ceiling` bytes, their containment logged to `sink`, its sound played
    /// through `speaker`, and each worker's cache keyed by a draw from `seed`.
    ///
    /// # Errors
    ///
    /// [`Stop::NoDecoder`] when the session's bounds are refused.
    pub fn new(
        launcher: L,
        sink: S,
        (read, len): (R, u64),
        (ceiling, layout): (u64, RingLayout),
        speaker: O,
        seed: fn() -> Option<HashSeed>,
    ) -> Result<Self, Stop> {
        let bounds = session_bounds().map_err(|_| Stop::NoDecoder)?;
        Ok(Self {
            session: SupervisedSession::new(launcher, bounds, sink),
            client: MovieDecodeClient::new(),
            read,
            len,
            ceiling,
            layout,
            speaker,
            seed,
            reading: Vec::new(),
            info: None,
            asked: Asked::Nothing,
            stale: false,
            ring: None,
            generation: 0,
            transport: Transport::Opening,
            clock: Clock::Stopped(0),
            ahead: VecDeque::new(),
            posted: None,
            shown_ns: None,
            blocks: Vec::new(),
            sound: None,
            seek: None,
            replay: None,
            pictured_ns: None,
            sounded_ns: None,
            unread: None,
            pictures_end: None,
            sound_end: None,
            halted: None,
            changed: true,
            notes: Vec::new(),
            late: 0,
            seeks: 0,
        })
    }

    /// Start the decoder and open the movie.
    pub fn begin(&mut self, now: u64) {
        let seed = (self.seed)();
        let started = self.session.start(now);
        let ringed = started.is_some_and(|generation| self.adopt_ring(generation));
        let (true, Some(seed)) = (ringed, seed) else {
            self.unopened(Stop::NoDecoder);
            return;
        };
        match self.client.open(self.len, self.ceiling, self.layout, seed) {
            Ok(()) => self.asked = Asked::Open,
            Err(_) => self.unopened(Stop::OutOfMemory),
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

    /// The stream the sound plays into, for the host to watch its mailbox.
    #[must_use]
    pub const fn speaker(&self) -> &O {
        &self.speaker
    }

    /// The instant the player next wants [`Self::on_timer`]: the next picture's
    /// time while playing, or a replacement decoder's start.
    #[must_use]
    pub fn deadline(&self) -> Option<u64> {
        let picture = if self.transport == Transport::Playing {
            self.ahead
                .front()
                .and_then(|next| self.clock.instant_of(next.at_ns))
        } else {
            None
        };
        match (picture, self.session.restart_deadline()) {
            (Some(due), Some(restart)) => Some(due.min(restart)),
            (due, None) | (None, due) => due,
        }
    }

    /// What the movie is, once open.
    #[must_use]
    pub const fn info(&self) -> Option<MovieInfo> {
        self.info
    }

    /// What the window shows.
    #[must_use]
    pub fn status(&self) -> Status {
        let clock = match self.clock {
            Clock::Stopped(media_ns) | Clock::Running { media_ns, .. } => media_ns,
        };
        Status {
            transport: self.transport,
            position_ns: self.shown_ns.unwrap_or(clock),
            late: self.late,
            seeks: self.seeks,
        }
    }

    /// Whether the status changed since this was last asked.
    pub fn take_changed(&mut self) -> bool {
        core::mem::take(&mut self.changed)
    }

    /// Hand over what happened since this was last asked.
    pub fn take_notes(&mut self) -> Vec<Note> {
        core::mem::take(&mut self.notes)
    }

    /// The picture due, for the window to show; one not taken before the next
    /// is due is dropped as late.
    pub fn take_picture(&mut self) -> Option<Picture> {
        self.posted.take()
    }

    /// Take back a slot the window is done with, at `now`: the decoder may
    /// draw into it again. A slot of a ring since replaced is that ring's,
    /// let go with the last of its pictures.
    pub fn release(&mut self, now: u64, release: Release) {
        self.hand_back(release);
        self.pump(now);
    }

    /// Close the sound's stream, as a host does before it drops the player:
    /// nothing else tells the audio service.
    pub fn close(&mut self) {
        if self.sound.take().is_some() {
            let _ = self.speaker.close();
        }
    }

    /// Stop for good: what playback waits on could not be watched, so the
    /// host can deliver nothing more.
    pub fn abandon(&mut self, failure: Failure) {
        self.stop(Stop::Abandoned(failure));
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
                Ok(Some(notify)) => self.adopt_notify(now, notify),
                Ok(None) => break,
                Err(errno) => {
                    let what = "read the stream's notifications";
                    self.lose_sound(now, Failure::Wait { what, errno });
                    break;
                }
            }
        }
        self.pump(now);
    }

    /// The deadline passed.
    pub fn on_timer(&mut self, now: u64) {
        let due = self.session.restart_deadline().is_some_and(|at| now >= at);
        if let Some(generation) = due.then(|| self.session.start(now)).flatten() {
            self.resume_decoder(generation);
        }
        self.pump(now);
    }

    /// The window asked something of the playback.
    pub fn order(&mut self, now: u64, order: Order) {
        match order {
            Order::Play => self.play(now),
            Order::Pause => self.pause(now),
            Order::Seek(target) => {
                self.seeks = self.seeks.wrapping_add(1);
                self.seek_to(now, target);
            }
        }
        self.changed = true;
        self.pump(now);
    }

    /// Make every move the state allows, until none is left.
    fn pump(&mut self, now: u64) {
        loop {
            let moved = self.take_answers(now)
                | self.send(now)
                | self.ask(now)
                | self.write_sound(now)
                | self.start_sound(now)
                | self.show_due(now)
                | self.play_out_sound(now)
                | self.finish(now);
            if !moved {
                return;
            }
        }
    }

    fn take_answers(&mut self, now: u64) -> bool {
        let mut moved = false;
        loop {
            let Self {
                session,
                client,
                blocks,
                ..
            } = self;
            let taken = session.recv(now, |frame| take(client, blocks, frame));
            match taken {
                Ok(Some(taken)) => {
                    moved = true;
                    self.handle(now, taken);
                }
                Ok(None) => return moved,
                // No worker is live until the paced replacement.
                Err(_) => {
                    self.worker_lost();
                    return moved;
                }
            }
        }
    }

    fn handle(&mut self, now: u64, taken: Taken) {
        match taken {
            Taken::Need { offset, len } => self.serve_need(offset, len),
            Taken::Opened => self.opened(now),
            Taken::Resuming => {}
            Taken::Picture(head) => self.pictured(head),
            Taken::Skipped(at_ns) => self.skipped(at_ns),
            Taken::Sound(at_ns, bytes) => self.sounded(at_ns, bytes),
            Taken::Ended(ending) => self.ended(ending),
            Taken::Sought => {
                self.answered();
            }
            Taken::Refused(refusal) => self.refused(refusal),
            Taken::Resumed => self.resumed(),
            Taken::OutOfMemory => self.stop(Stop::OutOfMemory),
            Taken::Error(error) => self.decoder_error(now, error),
        }
    }

    /// The outstanding request has its answer: whether it is still wanted.
    fn answered(&mut self) -> bool {
        self.asked = Asked::Nothing;
        !core::mem::take(&mut self.stale)
    }

    /// Read what the decoder asked for, off the file.
    fn serve_need(&mut self, offset: u64, len: usize) {
        let supplied = match fetch(&mut self.reading, &mut self.read, offset, len) {
            Ok(bytes) => self.client.supply(bytes),
            Err(Unfetched::NoMemory) => {
                self.stop(Stop::OutOfMemory);
                return;
            }
            Err(Unfetched::Refused(errno)) => {
                self.unread = Some(errno);
                self.client.unreadable()
            }
            Err(Unfetched::Short) => self.client.unreadable(),
        };
        if supplied == Err(MovieDecodeError::OutOfMemory) {
            self.stop(Stop::OutOfMemory);
        }
    }

    fn opened(&mut self, now: u64) {
        self.answered();
        let Some(info) = self.client.info() else {
            return;
        };
        self.info = Some(info);
        if let Some(channels) = info.sound {
            self.open_sound(now, channels);
        }
        self.sound_end = self.silent_end();
        self.transport = Transport::Paused;
        self.notes.push(Note::Opened(info));
        self.changed = true;
    }

    fn open_sound(&mut self, now: u64, channels: ChannelMap) {
        let latency = (SOUND_RATE.hz() / RING_SECONDS_DIVISOR).clamp(1, ring_bounds::MAX_FRAMES);
        let params = OpenParams {
            device_id: 0,
            direction: StreamDirection::Playback,
            format: SampleFormat::F32,
            rate: SOUND_RATE,
            channel_map: channels,
            role: StreamRole::Media,
            latency_target_frames: latency,
            tenancy: Tenancy::Caller,
        };
        match self.speaker.open(&params) {
            Ok(grant) => self.sound = Some(Sound::new(grant.stream_id, channels)),
            Err(failure) => self.lose_sound(now, Failure::Stream(failure)),
        }
    }

    /// A picture was drawn into its slot: kept to be shown, unless it is one
    /// no longer wanted or one a replacement plays again, whose slot goes
    /// back at once.
    fn pictured(&mut self, head: PictureHead) {
        let wanted = self.answered() && !self.replayed(head.at_ns);
        let Some(ring) = self.ring.clone().filter(|_| wanted) else {
            let _ = self.client.release(head.slot);
            return;
        };
        self.pictured_ns = Some(head.at_ns);
        self.ahead.push_back(Picture {
            at_ns: head.at_ns,
            slot: Slot {
                ring,
                generation: self.generation,
                index: head.slot,
                at: self.layout.slot_at(head.slot),
                size: (head.width, head.height),
            },
        });
    }

    /// A walk reached the end of the file's frames.
    fn ended(&mut self, ending: Ending) {
        let asked = self.asked;
        if !self.answered() {
            return;
        }
        match asked {
            Asked::Next => self.pictures_end = Some(ending),
            Asked::Sound => self.sound_end = Some(ending),
            Asked::Nothing | Asked::Open | Asked::Seek | Asked::Resume => {}
        }
    }

    /// Whether a picture at `at_ns` is one a replacement plays again.
    fn replayed(&self, at_ns: u64) -> bool {
        self.replay
            .and_then(|replay| replay.picture_ns)
            .is_some_and(|until| at_ns <= until)
    }

    fn skipped(&mut self, at_ns: u64) {
        if !self.answered() || self.replayed(at_ns) {
            return;
        }
        self.pictured_ns = Some(at_ns);
        self.late = self.late.saturating_add(1);
        self.changed = true;
    }

    /// A block of sound arrived: what of it a replacement has not played
    /// already waits for the ring.
    fn sounded(&mut self, at_ns: u64, mut bytes: Vec<u8>) {
        let wanted = self.answered();
        let frame_bytes = self.sound.as_ref().map_or(0, |sound| sound.frame_bytes);
        if !wanted || frame_bytes == 0 {
            keep(&mut self.blocks, bytes, SPARE_BLOCKS);
            return;
        }
        let hz = SOUND_RATE.hz();
        let frames = (bytes.len() / frame_bytes) as u64;
        let heard = self
            .replay
            .and_then(|replay| replay.sound_ns)
            .map_or(0, |until| {
                Span::from_nanos(until.saturating_sub(at_ns)).frames_at(hz)
            });
        let had = heard.min(frames);
        bytes.drain(..usize::try_from(had).unwrap_or(0) * frame_bytes);
        if bytes.is_empty() {
            keep(&mut self.blocks, bytes, SPARE_BLOCKS);
            return;
        }
        let start = at_ns.saturating_add(Span::of_frames(had, hz).nanos());
        self.sounded_ns = Some(start.saturating_add(Span::of_frames(frames - had, hz).nanos()));
        if let Some(sound) = self.sound.as_mut() {
            sound.pending.push_back(Block {
                at_ns: start,
                bytes,
                offset: 0,
            });
        }
    }

    fn refused(&mut self, refusal: MovieRefusal) {
        let asked = self.asked;
        if !self.answered() {
            return;
        }
        let stop = self
            .unread
            .take()
            .map_or(Stop::Refused(refusal), Stop::Unreadable);
        match asked {
            Asked::Open => self.unopened(stop),
            _ => self.halted = Some(stop),
        }
    }

    /// A replacement worker is back at the latest keyframe passed: what it
    /// plays again up to where the movie had got is not taken twice.
    fn resumed(&mut self) {
        self.answered();
        self.replay = Some(Replay {
            picture_ns: self.pictured_ns,
            sound_ns: self.sounded_ns,
        });
        self.notes.push(Note::Replaced);
        self.changed = true;
    }

    fn decoder_error(&mut self, now: u64, error: MovieDecodeError) {
        match error {
            MovieDecodeError::CannotResume => self.gave_up(),
            MovieDecodeError::OutOfMemory => self.stop(Stop::OutOfMemory),
            MovieDecodeError::Unbelievable
            | MovieDecodeError::OutOfTurn
            | MovieDecodeError::WrongLength => {
                self.session.condemn(
                    now,
                    "the movie decoder sent an answer it could not have meant",
                );
            }
        }
    }

    /// The worker failed; a replacement is due at the session's deadline. The
    /// pictures it drew stay to be shown, each holding their ring.
    fn worker_lost(&mut self) {
        self.ring = None;
        self.changed = true;
    }

    /// Take the ring the decoder of `generation` was handed, answering whether
    /// it was handed one the layout fits.
    fn adopt_ring(&mut self, generation: u64) -> bool {
        self.generation = generation;
        let least = self.layout.ring_bytes();
        self.ring = self
            .session
            .share()
            .filter(|ring| ring.byte_len() >= least)
            .cloned();
        self.ring.is_some()
    }

    /// Hand a slot back to the client, if it is of the live decoder's ring.
    fn hand_back(&mut self, release: Release) {
        if release.generation == self.generation {
            let _ = self.client.release(release.index);
        }
    }

    /// A movie with no sound to play has no sound walk to wait for.
    fn silent_end(&self) -> Option<Ending> {
        self.sound.is_none().then_some(Ending::Complete)
    }

    /// A replacement worker of `generation` is live: bring the movie back to
    /// where it was, its pictures drawn into the replacement's own ring.
    fn resume_decoder(&mut self, generation: u64) {
        let (true, Some(seed)) = (self.adopt_ring(generation), (self.seed)()) else {
            self.stop(Stop::NoDecoder);
            return;
        };
        match self.client.restart(seed) {
            Ok(()) => {
                self.stale = false;
                self.asked = match (self.client.outgoing(), self.info) {
                    (None, _) => Asked::Nothing,
                    (Some(_), None) => Asked::Open,
                    (Some(_), Some(_)) => Asked::Resume,
                };
            }
            Err(MovieDecodeError::CannotResume) => self.gave_up(),
            Err(_) => self.stop(Stop::OutOfMemory),
        }
    }

    /// The decoder failed twice at one place: a movie still opening will not
    /// open, and one playing plays out what is decoded and stops there.
    fn gave_up(&mut self) {
        if self.transport == Transport::Opening {
            self.unopened(Stop::DecoderGaveUp);
        } else {
            self.halted = Some(Stop::DecoderGaveUp);
        }
    }

    fn unopened(&mut self, stop: Stop) {
        self.transport = Transport::Stopped(stop);
        self.notes.push(Note::Unopened(stop));
        self.changed = true;
    }

    /// Stop for good: nothing more is decoded or heard.
    fn stop(&mut self, stop: Stop) {
        if self.transport == Transport::Opening {
            self.unopened(stop);
        } else {
            self.transport = Transport::Stopped(stop);
            self.changed = true;
        }
        self.close();
    }

    /// Queue the decoder's waiting request to its worker.
    fn send(&mut self, now: u64) -> bool {
        match send_waiting(&mut self.client, &mut self.session, now) {
            Sent::Queued => true,
            Sent::Nothing => false,
            Sent::Lost => {
                self.worker_lost();
                false
            }
        }
    }

    /// Ask the decoder for what the movie wants next, when it is free: a seek
    /// waiting; the next picture while none waits to be shown; sound to its
    /// depth; then the pictures the ring has room for.
    fn ask(&mut self, now: u64) -> bool {
        if !self.client.is_idle() || matches!(self.transport, Transport::Opening) {
            return false;
        }
        if let Some(target) = self.seek.take() {
            return self.request(Asked::Seek, |client| client.seek(target));
        }
        if self.halted.is_some() || matches!(self.transport, Transport::Stopped(_)) {
            return false;
        }
        let pictures = self.pictures_end.is_none()
            && self.ahead.len() < AHEAD_PICTURES
            && self.client.has_free_slot();
        let sound = self.sound_end.is_none()
            && self.sound.as_ref().map_or(0, Sound::ahead_ns) < SOUND_AHEAD_NS;
        if pictures && (self.ahead.is_empty() || !sound) {
            let skip_before_ns = self.skip_before(now);
            return self.request(Asked::Next, |client| client.next(skip_before_ns));
        }
        if sound {
            return self.request(Asked::Sound, MovieDecodeClient::sound);
        }
        false
    }

    fn request(
        &mut self,
        asked: Asked,
        send: impl FnOnce(&mut MovieDecodeClient) -> Result<(), MovieDecodeError>,
    ) -> bool {
        match send(&mut self.client) {
            Ok(()) => {
                self.asked = asked;
                true
            }
            Err(MovieDecodeError::OutOfMemory) => {
                self.stop(Stop::OutOfMemory);
                true
            }
            Err(_) => false,
        }
    }

    /// The instant before which a picture decoded now is too late to draw: none
    /// while paused, nor once the window has gone too long without one.
    fn skip_before(&self, now: u64) -> u64 {
        if self.transport != Transport::Playing {
            return 0;
        }
        let media = self.clock.media_at(now);
        let starved = self
            .shown_ns
            .is_none_or(|shown| media.saturating_sub(shown) >= MAX_UNSHOWN_NS);
        if starved {
            0
        } else {
            media.saturating_sub(LATE_NS)
        }
    }

    /// Write pending sound into the ring while it takes it.
    fn write_sound(&mut self, now: u64) -> bool {
        let Some(sound) = self.sound.as_mut() else {
            return false;
        };
        let mut moved = false;
        let mut refused = None;
        while !sound.full && sound.phase != Phase::Draining {
            let Some(block) = sound.pending.front_mut() else {
                break;
            };
            let written = match self
                .speaker
                .write(Frames::new(sound.next), &block.bytes[block.offset..])
            {
                Ok(written) => written,
                Err(errno) => {
                    refused = Some(errno);
                    break;
                }
            };
            let frames = u64::from(written.sample_frames);
            if frames == 0 {
                sound.full = true;
                break;
            }
            moved = true;
            let offered = ((block.bytes.len() - block.offset) / sound.frame_bytes) as u64;
            let into = (block.offset / sound.frame_bytes) as u64;
            let media = block
                .at_ns
                .saturating_add(Span::of_frames(into, SOUND_RATE.hz()).nanos());
            place(&mut sound.segments, sound.next, media);
            sound.next += frames;
            block.offset += usize::try_from(frames).unwrap_or(usize::MAX) * sound.frame_bytes;
            if frames < offered {
                sound.full = true;
            }
            if block.offset >= block.bytes.len() {
                if let Some(done) = sound.pending.pop_front() {
                    keep(&mut self.blocks, done.bytes, SPARE_BLOCKS);
                }
            }
        }
        if let Some(errno) = refused {
            let what = "take sound into the stream's ring";
            self.lose_sound(now, Failure::Audio { what, errno });
            return true;
        }
        moved
    }

    /// Start the stream once it is primed and a picture is ready, the clock
    /// running from the sound it starts on.
    fn start_sound(&mut self, now: u64) -> bool {
        if self.transport != Transport::Playing {
            return false;
        }
        let ready = !self.ahead.is_empty() || self.pictures_end.is_some() || self.halted.is_some();
        let heard_all = self.sound_end.is_some() || self.halted.is_some();
        let Some(sound) = self.sound.as_mut() else {
            return false;
        };
        if sound.phase != Phase::Primed || sound.held {
            return false;
        }
        if sound.next == sound.resume_at && sound.pending.is_empty() {
            // No sound is coming: the pictures run on the monotonic clock.
            if heard_all && matches!(self.clock, Clock::Stopped(_)) {
                let media_ns = self.clock.media_at(now);
                self.clock = Clock::Running {
                    media_ns,
                    at_ns: now,
                };
                return true;
            }
            return false;
        }
        let primed = sound.full
            || sound.ahead_ns() >= SOUND_AHEAD_NS
            || (heard_all && sound.pending.is_empty());
        if !ready || !primed {
            return false;
        }
        let at = sound.resume_at;
        match self.speaker.start(Frames::new(at)) {
            Ok(()) => {
                sound.phase = Phase::Running;
                let media_ns =
                    media_of(&sound.segments, at).unwrap_or_else(|| self.clock.media_at(now));
                self.clock = Clock::Running {
                    media_ns,
                    at_ns: now,
                };
            }
            Err(errno) => {
                let what = "start the stream";
                self.lose_sound(now, Failure::Audio { what, errno });
            }
        }
        true
    }

    /// Hand out the picture due: while paused, the first after an open or a
    /// seek; while playing, the latest whose time has come, dropping any it
    /// passed.
    fn show_due(&mut self, now: u64) -> bool {
        if self.transport != Transport::Playing {
            if self.shown_ns.is_some() {
                return false;
            }
            let Some(picture) = self.ahead.pop_front() else {
                return false;
            };
            self.post(picture);
            return true;
        }
        if matches!(self.clock, Clock::Stopped(_)) {
            return false;
        }
        let media = self.clock.media_at(now);
        let mut due = None;
        while self.ahead.front().is_some_and(|next| next.at_ns <= media) {
            if let Some(passed) = core::mem::replace(&mut due, self.ahead.pop_front()) {
                self.late = self.late.saturating_add(1);
                self.hand_back(passed.slot.release());
            }
        }
        let Some(picture) = due else {
            return false;
        };
        self.post(picture);
        true
    }

    fn post(&mut self, picture: Picture) {
        self.shown_ns = Some(picture.at_ns);
        if let Some(unseen) = self.posted.replace(picture) {
            self.late = self.late.saturating_add(1);
            self.hand_back(unseen.slot.release());
        }
        self.changed = true;
    }

    /// Stop once the walk has ended and everything decoded has been shown and
    /// heard.
    fn finish(&mut self, now: u64) -> bool {
        let Some(stop) = self.walked() else {
            return false;
        };
        if self.transport != Transport::Playing
            || !self.ahead.is_empty()
            || !self.client.takes_request()
        {
            return false;
        }
        let Some(sound) = self.sound.as_mut() else {
            self.stop_at(now, stop);
            return true;
        };
        if !sound.pending.is_empty() || sound.phase == Phase::Draining {
            return false;
        }
        if sound.phase != Phase::Running {
            self.stop_at(now, stop);
            return true;
        }
        match self.speaker.drain() {
            Ok(()) => sound.phase = Phase::Draining,
            Err(errno) => {
                let what = "play out the stream";
                self.lose_sound(now, Failure::Audio { what, errno });
            }
        }
        true
    }

    /// Why the movie stops once everything decoded has played: the decoder
    /// would not go on, or both walks reached the file's end.
    fn walked(&self) -> Option<Stop> {
        if let Some(stop) = self.halted {
            return Some(stop);
        }
        let (pictures, sound) = (self.pictures_end?, self.sound_end?);
        let cut = pictures == Ending::Truncated || sound == Ending::Truncated;
        Some(if cut { Stop::CutShort } else { Stop::Ended })
    }

    /// Play the sound out once its walk has ended and all of it is in the
    /// ring, so the device ends it cleanly while the pictures go on.
    fn play_out_sound(&mut self, now: u64) -> bool {
        let playing = self.transport == Transport::Playing;
        if !playing || self.sound_end.is_none() || self.walked().is_some() {
            return false;
        }
        let Some(sound) = self.sound.as_mut() else {
            return false;
        };
        if !sound.pending.is_empty() || sound.phase != Phase::Running {
            return false;
        }
        match self.speaker.drain() {
            Ok(()) => sound.phase = Phase::Draining,
            Err(errno) => {
                let what = "play out the stream";
                self.lose_sound(now, Failure::Audio { what, errno });
            }
        }
        true
    }

    /// The movie has played as far as it goes: stop there, and why.
    fn stop_at(&mut self, now: u64, stop: Stop) {
        self.transport = Transport::Stopped(stop);
        self.clock = Clock::Stopped(self.clock.media_at(now));
        self.changed = true;
    }

    fn adopt_notify(&mut self, now: u64, notify: AudioNotify) {
        let playing = self.transport == Transport::Playing;
        let end = self.walked();
        let Some(sound) = self
            .sound
            .as_mut()
            .filter(|sound| sound.id == notify.stream_id())
        else {
            return;
        };
        let after = match notify {
            AudioNotify::SpaceAvailable { position, .. } => {
                sound.read = position.get().min(sound.next);
                sound.full = false;
                settle(&mut sound.segments, sound.read);
                let heard = media_of(&sound.segments, sound.read);
                if let (true, Phase::Running | Phase::Draining, false, Some(media_ns)) =
                    (playing, sound.phase, sound.held, heard)
                {
                    // The sound is the clock, playing out included: the
                    // pictures follow what it has reached, however its
                    // device's clock drifts.
                    self.clock = Clock::Running {
                        media_ns,
                        at_ns: now,
                    };
                }
                None
            }
            AudioNotify::StateChanged { state, .. } => match state {
                StreamState::Idle if sound.phase == Phase::Draining => {
                    sound.phase = Phase::Primed;
                    sound.resume_at = sound.next;
                    end.map(After::Stop)
                }
                StreamState::SeatInactive => {
                    sound.held = true;
                    self.clock = Clock::Stopped(self.clock.media_at(now));
                    None
                }
                StreamState::Running | StreamState::Draining | StreamState::Paused
                    if sound.held =>
                {
                    sound.held = false;
                    if playing {
                        let media_ns = self.clock.media_at(now);
                        self.clock = Clock::Running {
                            media_ns,
                            at_ns: now,
                        };
                    }
                    None
                }
                StreamState::DeviceLost => Some(After::Lose(Failure::DeviceLost)),
                StreamState::Faulted => Some(After::Lose(Failure::RingFaulted)),
                _ => None,
            },
            AudioNotify::Xrun { .. } => None,
        };
        match after {
            Some(After::Stop(stop)) => self.stop_at(now, stop),
            Some(After::Lose(failure)) => self.lose_sound(now, failure),
            None => {}
        }
    }

    /// Give the sound up and go on silent: the pictures keep the clock on the
    /// monotonic one from where the movie is.
    fn lose_sound(&mut self, now: u64, failure: Failure) {
        if let Some(sound) = self.sound.take() {
            for block in sound.pending {
                keep(&mut self.blocks, block.bytes, SPARE_BLOCKS);
            }
            let _ = self.speaker.close();
        }
        // Sound no stream can play is not decoded.
        self.sound_end = self.sound_end.or(Some(Ending::Complete));
        if self.transport == Transport::Playing && matches!(self.clock, Clock::Stopped(_)) {
            let media_ns = self.clock.media_at(now);
            self.clock = Clock::Running {
                media_ns,
                at_ns: now,
            };
        }
        self.notes.push(Note::SoundLost(failure));
        self.changed = true;
    }

    fn play(&mut self, now: u64) {
        match self.transport {
            Transport::Paused => {}
            Transport::Stopped(stop) if !stop.is_final() => self.seek_to(now, 0),
            _ => return,
        }
        self.transport = Transport::Playing;
        let Some(sound) = self.sound.as_mut() else {
            let media_ns = self.clock.media_at(now);
            self.clock = Clock::Running {
                media_ns,
                at_ns: now,
            };
            return;
        };
        // A primed stream starts once it is ready; a paused one plays on.
        if sound.phase != Phase::Paused {
            return;
        }
        let at = sound.resume_at;
        match self.speaker.start(Frames::new(at)) {
            Ok(()) => {
                sound.phase = Phase::Running;
                let media_ns =
                    media_of(&sound.segments, at).unwrap_or_else(|| self.clock.media_at(now));
                self.clock = Clock::Running {
                    media_ns,
                    at_ns: now,
                };
            }
            Err(errno) => {
                let what = "play on";
                self.lose_sound(now, Failure::Audio { what, errno });
            }
        }
    }

    fn pause(&mut self, now: u64) {
        if self.transport != Transport::Playing {
            return;
        }
        self.transport = Transport::Paused;
        self.clock = Clock::Stopped(self.clock.media_at(now));
        let Some(sound) = self.sound.as_mut() else {
            return;
        };
        // A stream playing out is still playing: pausing it holds what is left
        // until play starts it again and it is played out anew.
        if !matches!(sound.phase, Phase::Running | Phase::Draining) {
            return;
        }
        match self.speaker.pause() {
            Ok(at) => {
                sound.resume_at = at.get();
                sound.phase = Phase::Paused;
                if let Some(media_ns) = media_of(&sound.segments, sound.resume_at) {
                    self.clock = Clock::Stopped(media_ns);
                }
            }
            Err(errno) => {
                let what = "pause";
                self.lose_sound(now, Failure::Audio { what, errno });
            }
        }
    }

    /// Go to `target`: everything decoded ahead and queued is let go, and the
    /// decoder seeks once it is free.
    fn seek_to(&mut self, now: u64, target: u64) {
        let gone = match self.transport {
            Transport::Opening => true,
            Transport::Stopped(stop) => stop.is_final(),
            Transport::Paused | Transport::Playing => false,
        };
        if gone {
            return;
        }
        while let Some(picture) = self.ahead.pop_front() {
            self.hand_back(picture.slot.release());
        }
        self.stale = !self.client.takes_request();
        self.seek = Some(target);
        self.pictures_end = None;
        self.halted = None;
        self.replay = None;
        self.shown_ns = None;
        self.pictured_ns = None;
        self.sounded_ns = None;
        self.unread = None;
        self.clock = Clock::Stopped(target);
        if matches!(self.transport, Transport::Stopped(_)) {
            self.transport = Transport::Paused;
        }
        if let Some(failure) = self.flush_sound() {
            self.lose_sound(now, failure);
        }
        self.sound_end = self.silent_end();
        // A silent movie runs on from the target; one with sound waits for its
        // stream to start again.
        if self.transport == Transport::Playing && self.sound.is_none() {
            self.clock = Clock::Running {
                media_ns: target,
                at_ns: now,
            };
        }
    }

    /// Stop the stream and discard what it holds and what waits for it.
    fn flush_sound(&mut self) -> Option<Failure> {
        let sound = self.sound.as_mut()?;
        if matches!(sound.phase, Phase::Running | Phase::Draining) {
            if let Err(errno) = self.speaker.pause() {
                return Some(Failure::Audio {
                    what: "pause",
                    errno,
                });
            }
        }
        if let Err(errno) = self.speaker.flush() {
            return Some(Failure::Audio {
                what: "discard what is queued",
                errno,
            });
        }
        for block in sound.pending.drain(..) {
            keep(&mut self.blocks, block.bytes, SPARE_BLOCKS);
        }
        sound.segments.clear();
        sound.phase = Phase::Primed;
        sound.full = false;
        sound.resume_at = sound.next;
        sound.read = sound.next;
        None
    }
}

#[cfg(test)]
#[path = "movie_tests.rs"]
mod tests;
