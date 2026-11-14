//! In-process fake transport: the host-test double for the sandbox seam.
//!
//! [`LoopbackLauncher`] plays the [`crate::host::Launcher`] role without a
//! process: each "worker" is a fresh [`crate::worker::Service`] instance
//! from the injected factory, and the [`LoopbackChannel`] runs it inline —
//! bytes written by the parent are buffered until they form a complete
//! request frame, the service handles the payload, and the framed reply is
//! read back. The parent-side code above the seam (framing, containment,
//! the typed decode helpers) runs unchanged, exactly as an `Fs`/`Tty` fake
//! lets an app's state machine run unchanged on the host.
//!
//! [`LoopbackSession`] is the same idea for the duplex seam
//! ([`crate::session`]): one [`crate::session::SessionService`] run inline
//! behind a [`crate::session::SessionTransport`], so a consumer's host
//! tests drive `send` / `on_writable` / `on_readable` / `recv` exactly as
//! its production owner will. [`LoopbackSessionLauncher`] starts one per
//! launch for a supervised session ([`crate::supervise`]), and
//! [`PipeSessionLauncher`] starts each behind a model of the kernel's pipe
//! pair ([`PipeSession`]), for a test that reports readiness only as a
//! wait-set would: a write takes at most [`PIPE_ROOM`] bytes, and a read of an
//! empty pipe is counted on the [`PipeGauge`] rather than answered.
//!
//! [`LoopbackShareLauncher`] hands each worker it starts a fresh
//! [`HeapShare`], the stand-in for a region shared with a worker, which the
//! worker writes and its owner reads through a lock as two processes reach
//! one region.
//!
//! [`MemberLedger`] is a wait-set's membership as the kernel keeps it, for a
//! test of what an owner leaves watched ([`crate::members::Members`]).
//!
//! The fakes model a *healthy* worker. Containment paths are exercised by
//! scripting a failing [`crate::proto::Channel`] or
//! [`crate::session::SessionTransport`] directly (see `crate::host`'s and
//! `crate::session`'s tests); keeping failure injection out of these types
//! keeps their behaviour identical to a correct production worker.

use alloc::rc::Rc;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::cell::RefCell;
use tairix_abi::{Errno, WaitSourceKind};
use tairix_sync::SpinLock;

use crate::host::Launcher;
use crate::members::MemberSet;
use crate::proto::{head_frame, send_frame, Channel, ProtoError, FRAME_HEADER_LEN, MAX_FRAME};
use crate::session::{
    FrameOut, SessionDescriptors, SessionService, SessionStep, SessionTransport, Share, ShareWrite,
};
use crate::supervise::SessionLauncher;
use crate::worker::Service;

/// Builds one fresh service per launched loopback worker.
pub trait ServiceFactory {
    /// The service each loopback worker runs.
    type Service: Service;

    /// Construct the next worker's service.
    fn build(&mut self) -> Self::Service;
}

/// Every `Fn`-style closure that yields a service is a factory.
impl<S: Service, F: FnMut() -> S> ServiceFactory for F {
    type Service = S;

    fn build(&mut self) -> S {
        self()
    }
}

/// [`Launcher`] whose workers are in-process services.
pub struct LoopbackLauncher<F: ServiceFactory> {
    factory: F,
    room: u64,
}

impl<F: ServiceFactory> LoopbackLauncher<F> {
    /// Build the launcher over the service factory. Its workers share this
    /// process's heap, so no share bounds the document one is handed.
    pub fn new(factory: F) -> Self {
        Self {
            factory,
            room: u64::MAX,
        }
    }

    /// The same launcher, its workers handed documents of at most `room`
    /// bytes, as a worker held to a share of its own is.
    #[must_use]
    pub fn holding(self, room: u64) -> Self {
        Self { room, ..self }
    }
}

impl<F: ServiceFactory> Launcher for LoopbackLauncher<F> {
    type Channel = LoopbackChannel<F::Service>;

    fn launch(&mut self) -> Result<Self::Channel, Errno> {
        Ok(LoopbackChannel {
            service: self.factory.build(),
            request: Vec::new(),
            reply: Vec::new(),
            reply_at: 0,
        })
    }

    fn dispose(&mut self, _channel: Self::Channel) -> Option<i32> {
        // An in-process worker has no process to reap and no exit code.
        None
    }

    fn document_room(&self) -> u64 {
        self.room
    }
}

/// The channel to one in-process loopback worker.
pub struct LoopbackChannel<S: Service> {
    service: S,
    /// Parent→worker bytes not yet consumed as a complete frame.
    request: Vec<u8>,
    /// Worker→parent framed reply bytes.
    reply: Vec<u8>,
    reply_at: usize,
}

impl<S: Service> LoopbackChannel<S> {
    /// Run the service over the buffered request bytes if they hold a
    /// complete, in-bound frame.
    fn pump(&mut self) {
        // An oversize declaration cannot come from the in-crate sender
        // (send_frame refuses it first); leaving it unconsumed mirrors a
        // worker that stops reading, and the parent's own bound already
        // failed the request.
        let Some(declared) = head_frame(&self.request).filter(|&len| len <= MAX_FRAME) else {
            return;
        };
        let payload: Vec<u8> = self
            .request
            .drain(..FRAME_HEADER_LEN + declared)
            .skip(FRAME_HEADER_LEN)
            .collect();
        let reply = self.service.handle(&payload);
        // Reclaim the already-consumed prefix of earlier replies before
        // appending this one, so a worker reused across many requests keeps
        // only the reply bytes still in flight rather than every reply it has
        // ever produced (an unbounded buffer that would eventually exhaust
        // memory over a long-running fuzz/soak run).
        self.reply.drain(..self.reply_at);
        self.reply_at = 0;
        // The reply is framed exactly as a real worker's send_frame does.
        let len = u32::try_from(reply.len().min(MAX_FRAME)).unwrap_or(0);
        self.reply.extend_from_slice(&len.to_le_bytes());
        self.reply.extend_from_slice(&reply[..len as usize]);
    }
}

impl<S: Service> Channel for LoopbackChannel<S> {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, Errno> {
        self.pump();
        if buf.is_empty() || self.reply_at == self.reply.len() {
            // No reply pending: a real pipe would block; the loopback has
            // nothing further coming, which the framing reports as the
            // peer being gone. Reaching this is a caller bug (a read with
            // no outstanding request), surfaced loudly rather than hung.
            return Ok(0);
        }
        let take = buf.len().min(self.reply.len() - self.reply_at);
        buf[..take].copy_from_slice(&self.reply[self.reply_at..self.reply_at + take]);
        self.reply_at += take;
        Ok(take)
    }

    fn write(&mut self, buf: &[u8]) -> Result<usize, Errno> {
        self.request.extend_from_slice(buf);
        Ok(buf.len())
    }
}

/// In-process [`SessionTransport`]: one [`SessionService`] run inline.
///
/// The parent's writes accumulate until they form a complete frame, the
/// service handles it and whatever it emits is framed into the reply
/// buffer, and the parent's reads drain that. A read with nothing pending
/// reports [`Errno::WouldBlock`] — "nothing right now" — which the session
/// treats as the no-op it is, so a host test can drive `on_readable`
/// freely without a wait-set to tell it when to.
pub struct LoopbackSession<S: SessionService> {
    service: S,
    /// Parent→worker bytes not yet consumed as a complete frame.
    request: Vec<u8>,
    /// Worker→parent framed bytes, with the prefix the parent has read.
    reply: Vec<u8>,
    reply_at: usize,
    /// Set once the service closed the session: reads then report
    /// end-of-stream and writes broken-pipe, exactly as a real worker's
    /// exit does.
    finished: bool,
    share: Option<Arc<dyn Share>>,
}

impl<S: SessionService> LoopbackSession<S> {
    /// Build the fake over the service this session's worker runs.
    pub fn new(service: S) -> Self {
        Self {
            service,
            request: Vec::new(),
            reply: Vec::new(),
            reply_at: 0,
            finished: false,
            share: None,
        }
    }

    /// The same session, its worker handed `share`.
    #[must_use]
    pub fn sharing(mut self, share: Arc<dyn Share>) -> Self {
        self.share = Some(share);
        self
    }

    /// Run the service over every complete frame the parent has written.
    fn pump(&mut self) {
        // Reclaim what the parent has already read before producing more,
        // so a long-lived session holds only the frames still in flight
        // rather than every frame it has ever emitted.
        self.reply.drain(..self.reply_at);
        self.reply_at = 0;
        while !self.finished {
            // An oversize declaration cannot come from the session's own
            // sender, which bounds every payload first.
            let Some(declared) = head_frame(&self.request).filter(|&len| len <= MAX_FRAME) else {
                return;
            };
            let payload: Vec<u8> = self
                .request
                .drain(..FRAME_HEADER_LEN + declared)
                .skip(FRAME_HEADER_LEN)
                .collect();
            let Self { service, reply, .. } = self;
            let mut out = ReplyFrames { reply };
            if service.handle(&payload, &mut out) == SessionStep::Finished {
                self.finished = true;
            }
        }
    }
}

impl<S: SessionService> SessionTransport for LoopbackSession<S> {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, Errno> {
        if self.reply_at == self.reply.len() {
            return if self.finished {
                Ok(0)
            } else {
                Err(Errno::WouldBlock)
            };
        }
        let take = buf.len().min(self.reply.len() - self.reply_at);
        buf[..take].copy_from_slice(&self.reply[self.reply_at..self.reply_at + take]);
        self.reply_at += take;
        Ok(take)
    }

    fn write(&mut self, buf: &[u8]) -> Result<usize, Errno> {
        if self.finished {
            return Err(Errno::BrokenPipe);
        }
        self.request.extend_from_slice(buf);
        self.pump();
        Ok(buf.len())
    }

    fn descriptors(&self) -> Option<SessionDescriptors> {
        // An in-process worker occupies no descriptor, so a host test's
        // owner has nothing to register and drives the seam directly.
        None
    }

    fn share(&self) -> Option<&Arc<dyn Share>> {
        self.share.as_ref()
    }

    fn dispose(self) -> Option<i32> {
        None
    }
}

/// A region on the heap standing in for one an owner shares with its worker:
/// the owner reads it as a [`Share`] and the worker writes it as a
/// [`ShareWrite`], each through a lock.
#[derive(Clone)]
pub struct HeapShare(Arc<SpinLock<Vec<u8>>>);

impl HeapShare {
    /// A zeroed region of `len` bytes, or `None` where the heap will not give
    /// it.
    #[must_use]
    pub fn new(len: usize) -> Option<Self> {
        let bytes = tairix_util::fallible::filled(len, 0u8)?;
        Some(Self(Arc::new(SpinLock::new(bytes))))
    }
}

impl Share for HeapShare {
    fn byte_len(&self) -> usize {
        self.0.lock().len()
    }

    fn copy_out(&self, at: usize, out: &mut [u8]) -> bool {
        let held = self.0.lock();
        let Some(source) = at.checked_add(out.len()).and_then(|end| held.get(at..end)) else {
            return false;
        };
        out.copy_from_slice(source);
        true
    }
}

impl ShareWrite for HeapShare {
    fn byte_len(&self) -> usize {
        self.0.lock().len()
    }

    fn write_with(&mut self, write: &mut dyn FnMut(&mut [u8]) -> bool) -> bool {
        let mut held = self.0.lock();
        write(held.as_mut_slice())
    }
}

/// [`SessionLauncher`] whose workers are [`LoopbackSession`]s each handed a
/// fresh [`HeapShare`] of `len` bytes, as the production launcher hands each
/// worker a region of its own.
pub struct LoopbackShareLauncher<F> {
    len: usize,
    factory: F,
}

impl<F> LoopbackShareLauncher<F> {
    /// Build the launcher over the service factory, which is handed the
    /// worker's side of each launch's share.
    pub const fn new(len: usize, factory: F) -> Self {
        Self { len, factory }
    }
}

impl<S: SessionService, F: FnMut(HeapShare) -> S> SessionLauncher for LoopbackShareLauncher<F> {
    type Transport = LoopbackSession<S>;

    fn launch(&mut self) -> Result<LoopbackSession<S>, Errno> {
        let share = HeapShare::new(self.len).ok_or(Errno::OutOfMemory)?;
        let service = (self.factory)(share.clone());
        Ok(LoopbackSession::new(service).sharing(Arc::new(share)))
    }
}

/// [`SessionLauncher`] whose workers are [`LoopbackSession`]s over services
/// the factory builds, a fresh one per launch.
pub struct LoopbackSessionLauncher<F> {
    factory: F,
}

impl<F> LoopbackSessionLauncher<F> {
    /// Build the launcher over the service factory.
    pub fn new(factory: F) -> Self {
        Self { factory }
    }
}

impl<S: SessionService, F: FnMut() -> S> SessionLauncher for LoopbackSessionLauncher<F> {
    type Transport = LoopbackSession<S>;

    fn launch(&mut self) -> Result<LoopbackSession<S>, Errno> {
        Ok(LoopbackSession::new((self.factory)()))
    }
}

/// Bytes a [`PipeSession`]'s request pipe takes in one write: less than a
/// supply of the audio decoder's largest read and its header, as on a
/// machine.
pub const PIPE_ROOM: usize = tairix_sound::MAX_READ;

/// What the pipes of one [`PipeSessionLauncher`]'s workers hold and have been
/// through.
#[derive(Debug, Default)]
struct PipeState {
    /// Bytes the live worker has written that its owner has not read.
    unread: usize,
    /// The live worker has closed its reply pipe.
    closed: bool,
    parked: usize,
    launched: usize,
    ended: usize,
}

/// A test's view of the pipes of every worker one [`PipeSessionLauncher`]
/// starts, read as a wait-set reads them.
#[derive(Clone, Debug, Default)]
pub struct PipeGauge(Rc<RefCell<PipeState>>);

impl PipeGauge {
    /// Whether a wait-set would report the live worker's reply pipe readable:
    /// it holds bytes, or the worker has closed it.
    #[must_use]
    pub fn readable(&self) -> bool {
        let state = self.0.borrow();
        state.unread > 0 || state.closed
    }

    /// Reads of an empty pipe its worker had not closed. On a machine each
    /// parks its reader, so an owner that makes one has read ahead of an
    /// answer.
    #[must_use]
    pub fn parked(&self) -> usize {
        self.0.borrow().parked
    }

    /// Workers launched.
    #[must_use]
    pub fn launched(&self) -> usize {
        self.0.borrow().launched
    }

    /// Workers their owner ended: disposed of, or contained.
    #[must_use]
    pub fn ended(&self) -> usize {
        self.0.borrow().ended
    }
}

/// A [`LoopbackSession`] behind a model of the kernel's pipe pair, for a test
/// that reports readiness only as a wait-set would: a write takes at most
/// [`PIPE_ROOM`] bytes, and a read of an empty pipe, which on a machine parks
/// its reader, is counted on the [`PipeGauge`] instead of answered.
pub struct PipeSession<S: SessionService> {
    worker: LoopbackSession<S>,
    /// What the worker has written that the owner has not read.
    answers: Vec<u8>,
    gauge: PipeGauge,
}

impl<S: SessionService> PipeSession<S> {
    /// Move what the worker has written into the reply pipe.
    fn collect(&mut self) {
        let mut chunk = [0u8; 4096];
        let closed = loop {
            match self.worker.read(&mut chunk) {
                Ok(0) => break true,
                Ok(read) => self.answers.extend_from_slice(&chunk[..read]),
                Err(_) => break false,
            }
        };
        let mut state = self.gauge.0.borrow_mut();
        state.unread = self.answers.len();
        state.closed = closed;
    }
}

impl<S: SessionService> SessionTransport for PipeSession<S> {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, Errno> {
        let mut state = self.gauge.0.borrow_mut();
        if self.answers.is_empty() {
            if state.closed {
                return Ok(0);
            }
            state.parked += 1;
            return Err(Errno::WouldBlock);
        }
        let take = buf.len().min(self.answers.len());
        buf[..take].copy_from_slice(&self.answers[..take]);
        self.answers.drain(..take);
        state.unread = self.answers.len();
        Ok(take)
    }

    fn write(&mut self, buf: &[u8]) -> Result<usize, Errno> {
        let taken = buf.len().min(PIPE_ROOM);
        self.worker.write(&buf[..taken])?;
        self.collect();
        Ok(taken)
    }

    fn descriptors(&self) -> Option<SessionDescriptors> {
        None
    }

    fn dispose(self) -> Option<i32> {
        let mut state = self.gauge.0.borrow_mut();
        state.unread = 0;
        state.closed = false;
        state.ended += 1;
        None
    }
}

/// [`SessionLauncher`] whose workers are [`PipeSession`]s over services the
/// factory builds, a fresh one per launch, all read through one
/// [`PipeGauge`].
pub struct PipeSessionLauncher<F> {
    factory: F,
    gauge: PipeGauge,
}

impl<F> PipeSessionLauncher<F> {
    /// Build the launcher over the service factory.
    pub fn new(factory: F) -> Self {
        Self {
            factory,
            gauge: PipeGauge::default(),
        }
    }

    /// The gauge every worker this launches is read through.
    #[must_use]
    pub fn gauge(&self) -> PipeGauge {
        self.gauge.clone()
    }
}

impl<S: SessionService, F: FnMut() -> S> SessionLauncher for PipeSessionLauncher<F> {
    type Transport = PipeSession<S>;

    fn launch(&mut self) -> Result<PipeSession<S>, Errno> {
        {
            let mut state = self.gauge.0.borrow_mut();
            state.launched += 1;
            state.unread = 0;
            state.closed = false;
        }
        Ok(PipeSession {
            worker: LoopbackSession::new((self.factory)()),
            answers: Vec::new(),
            gauge: self.gauge.clone(),
        })
    }
}

/// A wait-set's membership as the kernel keeps it: one member for each kind
/// and descriptor number, a second refused whatever its token.
#[derive(Clone, Debug, Default)]
pub struct MemberLedger(Rc<RefCell<Vec<(WaitSourceKind, u32, u64)>>>);

impl MemberLedger {
    /// The members held, oldest first: each one's kind, descriptor number and
    /// token.
    #[must_use]
    pub fn members(&self) -> Vec<(WaitSourceKind, u32, u64)> {
        self.0.borrow().clone()
    }

    /// Whether any member names descriptor `fd`.
    #[must_use]
    pub fn names(&self, fd: u32) -> bool {
        self.0.borrow().iter().any(|&(_, held, _)| held == fd)
    }
}

impl MemberSet for MemberLedger {
    fn add(&mut self, kind: WaitSourceKind, fd: u32, token: u64) -> Result<(), Errno> {
        let mut members = self.0.borrow_mut();
        if members.iter().any(|&(k, f, _)| k == kind && f == fd) {
            return Err(Errno::AlreadyExists);
        }
        members.push((kind, fd, token));
        Ok(())
    }

    fn remove(&mut self, kind: WaitSourceKind, fd: u32, _token: u64) {
        self.0
            .borrow_mut()
            .retain(|&(k, f, _)| !(k == kind && f == fd));
    }
}

/// The loopback's reply buffer as a write-only channel, so the fake frames
/// through [`send_frame`] rather than re-encoding the header.
struct ReplyFrames<'a> {
    reply: &'a mut Vec<u8>,
}

impl Channel for ReplyFrames<'_> {
    fn read(&mut self, _buf: &mut [u8]) -> Result<usize, Errno> {
        // Write-only: the worker never reads its own output back.
        Ok(0)
    }

    fn write(&mut self, buf: &[u8]) -> Result<usize, Errno> {
        self.reply.extend_from_slice(buf);
        Ok(buf.len())
    }
}

impl FrameOut for ReplyFrames<'_> {
    fn frame(&mut self, payload: &[u8]) -> Result<(), ProtoError> {
        send_frame(self, payload)
    }
}

#[cfg(test)]
mod tests {
    use super::LoopbackLauncher;
    use crate::host::ParserSandbox;
    use crate::testing::NullSink;
    use crate::worker::Service;
    use alloc::vec::Vec;

    /// Echoes each request with a `>` prefix.
    struct Tagger;

    impl Service for Tagger {
        fn handle(&mut self, request: &[u8]) -> Vec<u8> {
            let mut out = Vec::with_capacity(request.len() + 1);
            out.push(b'>');
            out.extend_from_slice(request);
            out
        }
    }

    #[test]
    fn the_full_parent_path_runs_over_the_in_process_worker() {
        let mut sandbox = ParserSandbox::new(LoopbackLauncher::new(|| Tagger), NullSink);
        assert_eq!(sandbox.request(b"alpha"), Ok(b">alpha".to_vec()));
        assert_eq!(sandbox.request(b"beta"), Ok(b">beta".to_vec()));
    }

    #[test]
    fn a_reused_worker_does_not_grow_its_reply_buffer_across_requests() {
        // Drive one reused loopback worker through many round trips and
        // confirm its internal reply buffer tracks only the bytes still in
        // flight, never every reply ever produced. Before the consumed
        // prefix was reclaimed this buffer grew without bound, exhausting
        // memory over a long-running fuzz/soak run.
        use crate::host::Launcher;
        use crate::proto::{recv_frame, send_frame, FRAME_HEADER_LEN};

        let mut launcher = LoopbackLauncher::new(|| Tagger);
        let mut channel = launcher.launch().expect("loopback launch never fails");
        for _ in 0..10_000 {
            send_frame(&mut channel, b"payload").expect("send succeeds");
            let reply = recv_frame(&mut channel)
                .expect("recv succeeds")
                .expect("a reply frame arrives");
            assert_eq!(reply, b">payload".to_vec());
            // One framed reply (`>payload`) is 4 header + 8 payload bytes.
            // The buffer never accumulates past a single reply's worth.
            assert!(
                channel.reply.len() <= FRAME_HEADER_LEN + b">payload".len(),
                "reply buffer grew to {} bytes across reuse",
                channel.reply.len(),
            );
        }
    }

    /// A write takes at most a pipe's room, half a frame is not answered, a
    /// read of an empty pipe is counted, and an ended worker leaves nothing to
    /// read.
    #[test]
    fn a_model_pipe_takes_a_pipes_room_a_write_and_counts_reads_of_an_empty_one() {
        use super::{PipeSessionLauncher, PIPE_ROOM};
        use crate::proto::FRAME_HEADER_LEN;
        use crate::session::{FrameOut, SessionService, SessionStep, SessionTransport};
        use crate::supervise::SessionLauncher;
        use tairix_abi::Errno;

        /// Answers each frame with itself.
        struct Echo;

        impl SessionService for Echo {
            fn handle(&mut self, request: &[u8], out: &mut dyn FrameOut) -> SessionStep {
                let _ = out.frame(request);
                SessionStep::Continue
            }
        }

        let mut launcher = PipeSessionLauncher::new(|| Echo);
        let gauge = launcher.gauge();
        let mut pipe = launcher.launch().expect("a model pipe launches");
        let declared = u32::try_from(PIPE_ROOM).expect("small");
        let mut frame = declared.to_le_bytes().to_vec();
        frame.resize(FRAME_HEADER_LEN + PIPE_ROOM, 7);
        assert_eq!(pipe.write(&frame), Ok(PIPE_ROOM), "one pipe's room");
        let mut reply = [0u8; 64];
        assert_eq!(pipe.read(&mut reply), Err(Errno::WouldBlock));
        assert!(!gauge.readable(), "half a frame is not answered");
        assert_eq!(gauge.parked(), 1, "the read of an empty pipe is counted");
        assert_eq!(pipe.write(&frame[PIPE_ROOM..]), Ok(FRAME_HEADER_LEN));
        assert!(gauge.readable(), "the answer waits");
        assert_eq!(pipe.read(&mut reply), Ok(reply.len()));
        assert_eq!(reply[..FRAME_HEADER_LEN], frame[..FRAME_HEADER_LEN]);
        assert_eq!((gauge.launched(), gauge.ended()), (1, 0));
        assert_eq!(pipe.dispose(), None);
        assert_eq!(gauge.ended(), 1);
        assert!(!gauge.readable(), "an ended worker leaves nothing to read");
    }
}
