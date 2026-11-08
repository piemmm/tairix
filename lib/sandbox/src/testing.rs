//! What the protocol tests drive their workers through: a sink that keeps
//! nothing, a sandbox over a worker in this process, workers that lie, and a
//! worker on a thread of its own for the sweeps that refuse the parent's heap.

extern crate std;

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::vec::Vec;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;

use tairix_abi::Errno;
use tairix_log::{Event, Sink};

use crate::host::{Launcher, ParserSandbox, SandboxError};
use crate::imagerender::DocumentFailure;
use crate::loopback::LoopbackLauncher;
use crate::proto::{Channel, ProtoError};
use crate::session::{FrameOut, SessionService};
use crate::worker::{serve, Service};

/// Keeps no event: a test that asserts on what is logged brings its own.
#[derive(Clone, Copy)]
pub(crate) struct NullSink;

impl Sink for NullSink {
    fn write_event(&self, _event: &Event<'_>) {}
}

/// A sandbox starting a fresh `S` in this process for every worker.
pub(crate) fn loopback<S: Service + Default>(
) -> ParserSandbox<LoopbackLauncher<fn() -> S>, NullSink> {
    ParserSandbox::new(LoopbackLauncher::new(S::default as fn() -> S), NullSink)
}

/// Answers every request with the one reply it holds.
pub(crate) struct Scripted(pub(crate) Vec<u8>);

impl Service for Scripted {
    fn handle(&mut self, _request: &[u8]) -> Vec<u8> {
        self.0.clone()
    }
}

/// A sandbox whose every worker answers `reply`, whatever it is asked.
pub(crate) fn scripted(
    reply: Vec<u8>,
) -> ParserSandbox<LoopbackLauncher<impl FnMut() -> Scripted>, NullSink> {
    ParserSandbox::new(
        LoopbackLauncher::new(move || Scripted(reply.clone())),
        NullSink,
    )
}

/// Serves as `S` does until the request naming `op`, whose reply it
/// corrupts: a worker that turns hostile part-way through a session.
pub(crate) struct Tampering<S> {
    inner: S,
    op: u8,
    tamper: fn(Vec<u8>) -> Vec<u8>,
}

impl<S: Service> Service for Tampering<S> {
    fn handle(&mut self, request: &[u8]) -> Vec<u8> {
        let reply = self.inner.handle(request);
        if request.first().copied() == Some(self.op) {
            (self.tamper)(reply)
        } else {
            reply
        }
    }
}

/// A sandbox over `S` whose workers corrupt `op`'s reply with `tamper`.
pub(crate) fn tampering<S: Service + Default>(
    op: u8,
    tamper: fn(Vec<u8>) -> Vec<u8>,
) -> ParserSandbox<LoopbackLauncher<impl FnMut() -> Tampering<S>>, NullSink> {
    ParserSandbox::new(
        LoopbackLauncher::new(move || Tampering {
            inner: S::default(),
            op,
            tamper,
        }),
        NullSink,
    )
}

/// Sweep `work` over a fresh sandbox whose worker serves an `S` on a thread
/// of its own, this thread's heap refused at each of `work`'s requests in
/// turn: every run completes or is refused as `refused` accepts, and holds
/// nothing past its answer.
///
/// The meter refuses only the thread that asks, so the worker allocates as a
/// sandboxed worker does and every refusal is the parent's own.
pub(crate) fn swept<S, T, E>(
    work: impl Fn(&mut ParserSandbox<ThreadedLauncher, NullSink>) -> Result<T, E>,
    refused: impl Fn(&E) -> bool,
) where
    S: Service + Default + 'static,
    E: core::fmt::Debug,
{
    let worker = Threaded::start(S::default);
    tairix_fuzzseed::meter::refused_at_each(|| work(&mut worker.sandbox()), refused);
}

/// Where a session client's requests go and its worker's frames come from.
pub(crate) trait Link {
    /// Hand `bytes`, the client's request, on.
    fn request(&mut self, bytes: &[u8]);

    /// The next frame the worker sent, or `None` when it sent no more.
    fn frame(&mut self) -> Option<&[u8]>;
}

/// A session worker answering in this thread, every frame it sends kept, so
/// a sweep can replay them with no worker allocating beside the client.
pub(crate) struct Recorder<S> {
    worker: S,
    pending: VecDeque<Vec<u8>>,
    /// Every frame handed out, in order.
    pub(crate) kept: Vec<Vec<u8>>,
}

impl<S: SessionService> Recorder<S> {
    pub(crate) const fn new(worker: S) -> Self {
        Self {
            worker,
            pending: VecDeque::new(),
            kept: Vec::new(),
        }
    }
}

impl FrameOut for VecDeque<Vec<u8>> {
    fn frame(&mut self, payload: &[u8]) -> Result<(), ProtoError> {
        self.push_back(payload.to_vec());
        Ok(())
    }
}

impl<S: SessionService> Link for Recorder<S> {
    fn request(&mut self, bytes: &[u8]) {
        let _ = self.worker.handle(bytes, &mut self.pending);
    }

    fn frame(&mut self) -> Option<&[u8]> {
        let frame = self.pending.pop_front()?;
        self.kept.push(frame);
        self.kept.last().map(Vec::as_slice)
    }
}

/// A recorder's frames played back in order; asking costs nothing.
pub(crate) struct Replay<'t>(core::slice::Iter<'t, Vec<u8>>);

impl<'t> Replay<'t> {
    pub(crate) fn of(frames: &'t [Vec<u8>]) -> Self {
        Self(frames.iter())
    }
}

impl Link for Replay<'_> {
    fn request(&mut self, _bytes: &[u8]) {}

    fn frame(&mut self) -> Option<&[u8]> {
        self.0.next().map(Vec::as_slice)
    }
}

/// Want of memory on this side of the exchange, and nothing else.
pub(crate) fn this_side(failure: SandboxError) -> bool {
    failure == SandboxError::OutOfMemory
}

/// An upload refused this side's memory, by the seam or by the exchange.
pub(crate) fn document_refused(failure: DocumentFailure) -> bool {
    match failure {
        DocumentFailure::NoMemory => true,
        DocumentFailure::Sandbox(inner) => this_side(inner),
        _ => false,
    }
}

/// Bytes one direction holds before its writer waits for the reader.
const RING: usize = 1 << 16;

/// One direction of a threaded conversation.
struct Ring {
    buf: Box<[u8]>,
    start: usize,
    len: usize,
    /// Its writer is gone: a reader that has taken everything sees the end.
    closed: bool,
    /// Its reader is gone: a write fails, as a pipe's does.
    deaf: bool,
}

impl Ring {
    fn new() -> Self {
        Self {
            buf: alloc::vec![0; RING].into_boxed_slice(),
            start: 0,
            len: 0,
            closed: false,
            deaf: false,
        }
    }

    fn reset(&mut self) {
        self.start = 0;
        self.len = 0;
        self.closed = false;
        self.deaf = false;
    }

    fn put(&mut self, bytes: &[u8]) -> usize {
        let count = (RING - self.len).min(bytes.len());
        for (at, &byte) in bytes[..count].iter().enumerate() {
            self.buf[(self.start + self.len + at) % RING] = byte;
        }
        self.len += count;
        count
    }

    fn take(&mut self, out: &mut [u8]) -> usize {
        let count = self.len.min(out.len());
        for slot in &mut out[..count] {
            *slot = self.buf[self.start];
            self.start = (self.start + 1) % RING;
        }
        self.len -= count;
        count
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Phase {
    /// No conversation: the next launch begins one.
    Idle,
    /// The worker serves the conversation a launch began.
    Serving,
    /// The worker has finished serving it.
    Served,
    /// The worker thread is to end.
    Ending,
}

struct State {
    to_worker: Ring,
    to_parent: Ring,
    phase: Phase,
}

struct Shared {
    state: Mutex<State>,
    changed: Condvar,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn wait<'g>(&self, guard: MutexGuard<'g, State>) -> MutexGuard<'g, State> {
        self.changed
            .wait(guard)
            .unwrap_or_else(PoisonError::into_inner)
    }
}

/// The worker's end: it reads the parent's requests and writes its replies.
struct WorkerSide<'s> {
    shared: &'s Shared,
}

impl Channel for WorkerSide<'_> {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, Errno> {
        let mut state = self.shared.lock();
        while state.to_worker.len == 0 && !state.to_worker.closed {
            state = self.shared.wait(state);
        }
        let read = state.to_worker.take(buf);
        self.shared.changed.notify_all();
        Ok(read)
    }

    fn write(&mut self, buf: &[u8]) -> Result<usize, Errno> {
        let mut state = self.shared.lock();
        loop {
            if state.to_parent.deaf {
                return Err(Errno::BrokenPipe);
            }
            if state.to_parent.len < RING || buf.is_empty() {
                let written = state.to_parent.put(buf);
                self.shared.changed.notify_all();
                return Ok(written);
            }
            state = self.shared.wait(state);
        }
    }
}

/// The parent's end of one threaded conversation.
pub(crate) struct ThreadedChannel {
    shared: Arc<Shared>,
}

impl Channel for ThreadedChannel {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, Errno> {
        let mut state = self.shared.lock();
        while state.to_parent.len == 0 && !state.to_parent.closed {
            state = self.shared.wait(state);
        }
        let read = state.to_parent.take(buf);
        self.shared.changed.notify_all();
        Ok(read)
    }

    fn write(&mut self, buf: &[u8]) -> Result<usize, Errno> {
        let mut state = self.shared.lock();
        loop {
            if state.phase != Phase::Serving {
                return Err(Errno::BrokenPipe);
            }
            if state.to_worker.len < RING || buf.is_empty() {
                let written = state.to_worker.put(buf);
                self.shared.changed.notify_all();
                return Ok(written);
            }
            state = self.shared.wait(state);
        }
    }
}

/// Launches the one worker its thread serves, a conversation at a time.
/// Neither a launch nor a disposal allocates.
#[derive(Clone)]
pub(crate) struct ThreadedLauncher {
    shared: Arc<Shared>,
}

impl Launcher for ThreadedLauncher {
    type Channel = ThreadedChannel;

    fn launch(&mut self) -> Result<ThreadedChannel, Errno> {
        let mut state = self.shared.lock();
        while state.phase != Phase::Idle {
            state = self.shared.wait(state);
        }
        state.to_worker.reset();
        state.to_parent.reset();
        state.phase = Phase::Serving;
        self.shared.changed.notify_all();
        Ok(ThreadedChannel {
            shared: Arc::clone(&self.shared),
        })
    }

    fn dispose(&mut self, channel: ThreadedChannel) -> Option<i32> {
        let mut state = channel.shared.lock();
        state.to_worker.closed = true;
        state.to_parent.deaf = true;
        self.shared.changed.notify_all();
        while state.phase == Phase::Serving {
            state = self.shared.wait(state);
        }
        state.phase = Phase::Idle;
        self.shared.changed.notify_all();
        None
    }

    /// The worker shares this process's heap, so no share bounds it.
    fn document_room(&self) -> u64 {
        u64::MAX
    }
}

/// A thread serving a fresh `factory`-made service each conversation, ended
/// when this is dropped.
pub(crate) struct Threaded {
    launcher: ThreadedLauncher,
    thread: Option<JoinHandle<()>>,
}

impl Threaded {
    /// Start the thread; its rings are made here, before any sweep runs.
    pub(crate) fn start<S: Service + 'static>(factory: fn() -> S) -> Self {
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                to_worker: Ring::new(),
                to_parent: Ring::new(),
                phase: Phase::Idle,
            }),
            changed: Condvar::new(),
        });
        let serving = Arc::clone(&shared);
        let thread = std::thread::spawn(move || loop {
            {
                let mut state = serving.lock();
                while matches!(state.phase, Phase::Idle | Phase::Served) {
                    state = serving.wait(state);
                }
                if state.phase == Phase::Ending {
                    return;
                }
            }
            let mut service = factory();
            let _ = serve(&mut WorkerSide { shared: &serving }, &mut service);
            drop(service);
            let mut state = serving.lock();
            state.to_parent.closed = true;
            if state.phase == Phase::Serving {
                state.phase = Phase::Served;
            }
            serving.changed.notify_all();
        });
        Self {
            launcher: ThreadedLauncher { shared },
            thread: Some(thread),
        }
    }

    /// A sandbox over this thread's worker; making one allocates nothing.
    pub(crate) fn sandbox(&self) -> ParserSandbox<ThreadedLauncher, NullSink> {
        ParserSandbox::new(self.launcher.clone(), NullSink)
    }
}

impl Drop for Threaded {
    fn drop(&mut self) {
        {
            let mut state = self.launcher.shared.lock();
            state.phase = Phase::Ending;
            state.to_worker.closed = true;
            state.to_parent.deaf = true;
            self.launcher.shared.changed.notify_all();
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
