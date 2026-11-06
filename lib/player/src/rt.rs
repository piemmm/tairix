//! The engine's live seams: files read through the runtime, the audio
//! service's stream, the seed a decoder's page cache is keyed by, and the
//! members a host parks the engine's sources on; and the tag reader a worker
//! thread drives to the end of each file ([`RtTags`]).

use alloc::sync::Arc;

use tairix_abi::audio::{AudioGain, AudioNotify, OpenParams, StreamGrant, StreamReport};
use tairix_abi::driver::audio::Frames;
use tairix_abi::waitset::{WaitSetOp, WaitSourceKind};
use tairix_abi::{Errno, OpenFlags};
use tairix_audio::live::{LiveStream, RtAudio};
use tairix_audio::stream::{OpenFailure, Written};
use tairix_rt::File;
use tairix_sandbox::audiodecode::session_bounds;
use tairix_sandbox::rt::{draw_seed, RtSessionLauncher, SessionMembers};
use tairix_sandbox::session::SessionDescriptors;
use tairix_sandbox::supervise::SupervisedSession;

use crate::engine::{Engine, Failure, FileRefusal, Files, Settings, Speaker};
use crate::probe::{TagReader, Tags};
use crate::programme::Programme;

/// The engine a live player runs over `F`: its decoders are the program's own
/// binary in the sandbox's worker role, and their containment is logged to
/// the system log.
pub type RtEngine<P, F> = Engine<P, RtSessionLauncher, tairix_rt::LogSink, F, RtSpeaker>;

/// A live engine playing `programme` as `settings` say, reading through
/// `files`.
///
/// # Errors
///
/// What [`Engine::new`] refuses.
pub fn live<P: Programme, F: Files<P::Item>>(
    programme: P,
    settings: Settings,
    files: F,
) -> Result<RtEngine<P, F>, Failure> {
    Engine::new(
        programme,
        settings,
        RtSessionLauncher::own_binary(tairix_sandbox::WorkerMemory::Messages),
        tairix_rt::LogSink,
        files,
        RtSpeaker::new(),
        draw_seed,
    )
}

/// The files a programme names by path, opened under the program's own
/// authority.
pub struct RtFiles {
    file: Option<File>,
}

impl RtFiles {
    /// No file open yet.
    #[must_use]
    pub const fn new() -> Self {
        Self { file: None }
    }
}

impl Default for RtFiles {
    fn default() -> Self {
        Self::new()
    }
}

impl Files<str> for RtFiles {
    fn open(&mut self, path: &str) -> Result<u64, FileRefusal> {
        self.file = None;
        let file = File::open(path.as_bytes(), OpenFlags::READ)
            .map_err(|ret| FileRefusal::Os(Errno::from_syscall(ret)))?;
        let len = file
            .regular_len()
            .map_err(FileRefusal::Os)?
            .ok_or(FileRefusal::NotRegular)?;
        self.file = Some(file);
        Ok(len)
    }

    fn read(&mut self, offset: u64, buf: &mut [u8]) -> Result<usize, Errno> {
        self.file
            .as_ref()
            .ok_or(Errno::NotFound)?
            .read_at(offset, buf)
            .map_err(Errno::from_syscall)
    }
}

/// The files a programme holds open: descriptors a program was handed, which
/// the engine shares while it reads, so a removed entry's descriptor closes
/// only once the engine is done with it and its number is never reused under
/// a read.
pub struct HeldFiles {
    file: Option<Arc<File>>,
}

impl HeldFiles {
    /// No file open yet.
    #[must_use]
    pub const fn new() -> Self {
        Self { file: None }
    }
}

impl Default for HeldFiles {
    fn default() -> Self {
        Self::new()
    }
}

impl Files<Arc<File>> for HeldFiles {
    fn open(&mut self, held: &Arc<File>) -> Result<u64, FileRefusal> {
        self.file = None;
        let len = held
            .regular_len()
            .map_err(FileRefusal::Os)?
            .ok_or(FileRefusal::NotRegular)?;
        self.file = Some(Arc::clone(held));
        Ok(len)
    }

    fn read(&mut self, offset: u64, buf: &mut [u8]) -> Result<usize, Errno> {
        self.file
            .as_ref()
            .ok_or(Errno::NotFound)?
            .read_at(offset, buf)
            .map_err(Errno::from_syscall)
    }
}

/// The audio service's stream.
pub struct RtSpeaker {
    transport: RtAudio,
    stream: Option<LiveStream>,
}

impl RtSpeaker {
    /// A speaker with no stream open.
    #[must_use]
    pub fn new() -> Self {
        Self {
            transport: RtAudio::new(),
            stream: None,
        }
    }

    /// The mailbox the open stream's notifications arrive in.
    #[must_use]
    pub fn notify_port(&self) -> Option<u64> {
        self.stream.as_ref().map(LiveStream::notify_port)
    }

    fn stream(&mut self) -> Result<(&mut LiveStream, &mut RtAudio), Errno> {
        match &mut self.stream {
            Some(stream) => Ok((stream, &mut self.transport)),
            None => Err(Errno::NotConnected),
        }
    }
}

impl Default for RtSpeaker {
    fn default() -> Self {
        Self::new()
    }
}

impl Speaker for RtSpeaker {
    fn open(&mut self, params: &OpenParams) -> Result<StreamGrant, OpenFailure> {
        let stream = LiveStream::open(&mut self.transport, params)?;
        let grant = stream.client().grant();
        self.stream = Some(stream);
        Ok(grant)
    }

    fn write(&mut self, at: Frames, samples: &[u8]) -> Result<Written, Errno> {
        self.stream()?.0.write_at(at, samples)
    }

    fn start(&mut self, at: Frames) -> Result<(), Errno> {
        let (stream, transport) = self.stream()?;
        stream.client_mut().start(transport, at)
    }

    fn pause(&mut self) -> Result<Frames, Errno> {
        let (stream, transport) = self.stream()?;
        stream.client_mut().stop(transport, Frames::ZERO)?;
        Ok(stream.client_mut().report(transport)?.changed_at)
    }

    fn drain(&mut self) -> Result<(), Errno> {
        let (stream, transport) = self.stream()?;
        stream.client_mut().drain(transport)
    }

    fn flush(&mut self) -> Result<(), Errno> {
        let (stream, transport) = self.stream()?;
        stream.client().flush(transport)
    }

    fn set_gain(&mut self, gain: AudioGain) -> Result<(), Errno> {
        let (stream, transport) = self.stream()?;
        stream.client().set_gain(transport, gain)
    }

    fn close(&mut self) -> Result<(), Errno> {
        let stream = self.stream.take().ok_or(Errno::NotConnected)?;
        stream.close(&mut self.transport)
    }

    fn take_notify(&mut self) -> Result<Option<AudioNotify>, Errno> {
        match &mut self.stream {
            Some(stream) => stream.take_notify(&mut self.transport),
            None => Ok(None),
        }
    }

    fn report(&mut self) -> Result<StreamReport, Errno> {
        let (stream, transport) = self.stream()?;
        stream.client_mut().report(transport)
    }
}

/// The tokens a host's wait-set reports the engine's sources under.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Tokens {
    /// The decoder's answers are readable.
    pub decoder_read: u64,
    /// The decoder's request pipe has room.
    pub decoder_write: u64,
    /// The open stream's mailbox holds a notification.
    pub notify: u64,
}

/// A player's sources on a host's wait-set: the decoder's pipes in the
/// directions it wants, and the open stream's mailbox.
pub struct EngineWaits {
    set: u64,
    tokens: Tokens,
    members: SessionMembers,
    watched: Option<u64>,
}

impl EngineWaits {
    /// The engine's sources on wait-set `set`, reported as `tokens`.
    #[must_use]
    pub const fn new(set: u64, tokens: Tokens) -> Self {
        Self {
            set,
            tokens,
            members: SessionMembers::new(set, tokens.decoder_read, tokens.decoder_write),
            watched: None,
        }
    }

    /// Bring the wait-set up to date with `engine`, abandoning its playback
    /// when a source cannot be watched: a source no one watches would leave it
    /// waiting for ever.
    pub fn sync<P: Programme, F: Files<P::Item>>(&mut self, engine: &mut RtEngine<P, F>) {
        let port = engine.speaker().notify_port();
        if let Err(failure) = self.watch(engine.decoder_members(), port) {
            engine.abandon(failure);
        }
    }

    /// Bring the wait-set up to date with a decoder's descriptors and the
    /// directions it wants, and with the stream mailbox `port`.
    ///
    /// # Errors
    ///
    /// Which source the wait-set would not watch, and why.
    pub fn watch(
        &mut self,
        (descriptors, read, write): (Option<SessionDescriptors>, bool, bool),
        port: Option<u64>,
    ) -> Result<(), Failure> {
        self.members
            .sync(descriptors, read, write)
            .map_err(|errno| Failure::Wait {
                what: "watch the decoder",
                errno,
            })?;
        self.watch_mailbox(port).map_err(|errno| Failure::Wait {
            what: "watch the stream's notifications",
            errno,
        })
    }

    /// Hand `token`, reported at `now`, to `engine`, answering whether it was
    /// one of the engine's own.
    pub fn deliver<P: Programme, F: Files<P::Item>>(
        &self,
        engine: &mut RtEngine<P, F>,
        token: u64,
        now: u64,
    ) -> bool {
        match token {
            token if token == self.tokens.decoder_read => engine.on_decoder(now, true, false),
            token if token == self.tokens.decoder_write => engine.on_decoder(now, false, true),
            token if token == self.tokens.notify => engine.on_notify(now),
            _ => return false,
        }
        true
    }

    /// Keep the wait-set watching `port`, the current stream's mailbox.
    fn watch_mailbox(&mut self, port: Option<u64>) -> Result<(), Errno> {
        if self.watched == port {
            return Ok(());
        }
        if let Some(old) = self.watched.take() {
            let left = tairix_rt::waitset_ctl(
                self.set,
                WaitSetOp::Del,
                WaitSourceKind::Port,
                old,
                self.tokens.notify,
            );
            if left != 0 {
                return Err(Errno::from_syscall(left));
            }
        }
        if let Some(port) = port {
            let joined = tairix_rt::waitset_ctl(
                self.set,
                WaitSetOp::Add,
                WaitSourceKind::Port,
                port,
                self.tokens.notify,
            );
            if joined != 0 {
                return Err(Errno::from_syscall(joined));
            }
            self.watched = Some(port);
        }
        Ok(())
    }
}

/// The tag reader's tokens on its own wait-set.
const TAGS_READABLE: u64 = 1;
const TAGS_WRITABLE: u64 = 2;

/// A tag reader whose decoders are the program's own binary in the sandbox's
/// worker role, driven to the end of each file on the calling thread and
/// parked on a wait-set of its own: file I/O and a wait, so a worker's to
/// call and never an interactive loop's.
pub struct RtTags {
    reader: TagReader<RtSessionLauncher, tairix_rt::LogSink>,
    set: u64,
    members: SessionMembers,
}

impl RtTags {
    /// A reader that starts its first decoder when first asked, or `None`
    /// when the kernel refuses the wait-set it parks on.
    #[must_use]
    pub fn new() -> Option<Self> {
        let bounds = session_bounds().ok()?;
        let set = u64::try_from(tairix_rt::waitset_create()).ok()?;
        let session = SupervisedSession::new(
            RtSessionLauncher::own_binary(tairix_sandbox::WorkerMemory::Messages),
            bounds,
            tairix_rt::LogSink,
        );
        Some(Self {
            reader: TagReader::new(session, draw_seed),
            set,
            members: SessionMembers::new(set, TAGS_READABLE, TAGS_WRITABLE),
        })
    }

    /// `file`'s stream and tags, read to the end within
    /// [`crate::probe::BUDGET_NS`] of a decoder taking it.
    #[must_use]
    pub fn read_tags(&mut self, file: &File) -> Tags {
        let Ok(Some(len)) = file.regular_len() else {
            return Tags::Unread;
        };
        let mut read =
            |offset, buf: &mut [u8]| file.read_at(offset, buf).map_err(Errno::from_syscall);
        self.reader.begin(tairix_rt::clock_get(), len);
        loop {
            if let Some(tags) = self.reader.pump(tairix_rt::clock_get(), &mut read) {
                return tags;
            }
            if self.park().is_err() {
                // A worker left mid-request would answer the next file with
                // this one's frames.
                self.reader.abandon(tairix_rt::clock_get());
                return Tags::Unread;
            }
        }
    }

    /// Wait for the reader's worker, or its deadline, and hand on what fired.
    fn park(&mut self) -> Result<(), Errno> {
        let (descriptors, read, write) = self.reader.members();
        self.members.sync(descriptors, read, write)?;
        let now = tairix_rt::clock_get();
        let timeout = self
            .reader
            .deadline()
            .map_or(u64::MAX, |at| at.saturating_sub(now));
        let mut token = 0;
        let waited = tairix_rt::waitset_wait(self.set, timeout, &mut token);
        if waited == 0 {
            let now = tairix_rt::clock_get();
            let (readable, writable) = (token == TAGS_READABLE, token == TAGS_WRITABLE);
            self.reader.on_ready(now, readable, writable);
            return Ok(());
        }
        match Errno::from_syscall(waited) {
            Errno::TimedOut => Ok(()),
            errno => Err(errno),
        }
    }
}

impl Drop for RtTags {
    fn drop(&mut self) {
        let _ = tairix_rt::waitset_close(self.set);
    }
}
