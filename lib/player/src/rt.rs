//! The engine's live seams: files read through the runtime, the audio
//! service's stream, the seed a decoder's page cache is keyed by, and the
//! members a host parks the engine's sources on.

use alloc::sync::Arc;

use tairix_abi::audio::{AudioGain, AudioNotify, OpenParams, StreamGrant, StreamReport};
use tairix_abi::driver::audio::Frames;
use tairix_abi::waitset::{WaitSetOp, WaitSourceKind};
use tairix_abi::{Errno, OpenFlags};
use tairix_audio::live::{LiveStream, RtAudio};
use tairix_audio::stream::{OpenFailure, Written};
use tairix_hash::HashSeed;
use tairix_rt::File;
use tairix_sandbox::rt::{RtSessionLauncher, SessionMembers};

use crate::engine::{Engine, Failure, FileRefusal, Files, Settings, Speaker};
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
        RtSessionLauncher::own_binary(),
        tairix_rt::LogSink,
        files,
        RtSpeaker {
            transport: RtAudio::new(),
            stream: None,
        },
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
    /// The mailbox the open stream's notifications arrive in.
    #[must_use]
    pub const fn notify_port(&self) -> Option<u64> {
        self.transport.notify_port()
    }

    fn stream(&mut self) -> Result<(&mut LiveStream, &mut RtAudio), Errno> {
        match &mut self.stream {
            Some(stream) => Ok((stream, &mut self.transport)),
            None => Err(Errno::NotConnected),
        }
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

/// A fresh draw for a decoder's page cache.
#[must_use]
pub fn draw_seed() -> Option<HashSeed> {
    let mut key = [0u8; HashSeed::LEN];
    tairix_rt::random_fill(&mut key).ok()?;
    Some(HashSeed::from_bytes(key))
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

/// The engine's sources on a host's wait-set: the decoder's pipes in the
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
        let (descriptors, read, write) = engine.decoder_members();
        if let Err(errno) = self.members.sync(descriptors, read, write) {
            engine.abandon(Failure::Wait {
                what: "watch the decoder",
                errno,
            });
        }
        let port = engine.speaker().notify_port();
        if let Err(errno) = self.watch_mailbox(port) {
            engine.abandon(Failure::Wait {
                what: "watch the stream's notifications",
                errno,
            });
        }
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
