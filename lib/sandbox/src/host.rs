//! The parent side of the sandbox seam: request dispatch and crash
//! containment.
//!
//! [`ParserSandbox`] is what a calling program holds: it sends one request
//! payload to the sandboxed worker its [`Launcher`] started and returns the
//! reply payload. Every way the worker can fail — a crash, a protocol
//! violation, an oversize reply, an exit without replying — is contained
//! identically: the caller receives a typed [`SandboxError`] saying what
//! became of the worker, the dead worker is disposed of (reaped), the event
//! is logged with a stable [`EventId`] and the cause its exit status carries,
//! and the next request starts a fresh worker. A parser crash never takes
//! down the calling program.
//!
//! The replacement is deliberately not started at once. A worker that ran out
//! of memory would be followed by another process spawned into the same
//! exhausted machine, and a worker killed by its input would buy its sender a
//! spawn per message, whether or not another request ever comes.
//!
//! The worker is treated as hostile from the moment it has parsed a byte:
//! nothing it sends is trusted beyond the framing bound here, and the typed
//! payload decoders above this layer validate every field fail-closed. A
//! reply one of them cannot believe is a worker that is broken or subverted,
//! so it is contained as a crash is ([`ParserSandbox::ask`]).

use tairix_abi::{Errno, FieldValue, Signal};
use tairix_log::{Event, EventId, Field, Level, Sink};

use alloc::vec::Vec;

use crate::proto::{recv_frame, send_frame, Channel, ProtoError, MAX_FRAME};
use crate::worker::WorkerExit;

/// Stable event id: a sandboxed worker crashed or violated the protocol
/// mid-request and was disposed of and replaced.
///
/// `lib/sandbox` owns the `6_000..7_000` identifier range.
pub const EVENT_WORKER_CRASHED: EventId = EventId(6000);

/// Stable event id: a sandboxed worker could not be started (an initial
/// launch or a post-crash replacement failed).
pub const EVENT_WORKER_UNAVAILABLE: EventId = EventId(6001);

/// Starts sandboxed workers and reaps dead ones.
///
/// The production launcher spawns the program's own binary in a worker
/// role inside the kernel sandbox spawn mode over a fresh pipe pair
/// (`crate::rt`); host tests inject in-process fakes
/// (`crate::loopback`).
pub trait Launcher {
    /// The transport connected to one launched worker.
    type Channel: Channel;

    /// Start a fresh worker and return the channel to it.
    ///
    /// # Errors
    ///
    /// The typed reason the worker could not be started.
    fn launch(&mut self) -> Result<Self::Channel, Errno>;

    /// Tear down a worker whose channel failed: close the transport, reap
    /// the process, and report its exit code when one is known.
    fn dispose(&mut self, channel: Self::Channel) -> Option<i32>;
}

/// A service's decoding failure, as far as containment needs to know it.
pub trait Unbelieved {
    /// Whether this is a reply that broke its service's grammar or
    /// invariants, rather than a refusal the worker was entitled to make.
    fn unbelieved(&self) -> bool;
}

/// What became of a worker that failed a request, as its exit status tells.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum WorkerEnd {
    /// It ran out of memory: it could not reserve room for a request, an
    /// allocation it could not do without failed, or the kernel could not
    /// load it at all.
    OutOfMemory,
    /// The kernel killed it for a fault it could not resolve.
    Crashed,
    /// It was still running and was killed: it had not answered in time, or
    /// what it answered could not be believed.
    Killed,
    /// It exited because its transport failed or its parent broke the
    /// protocol.
    TransportFailed,
    /// Its status names no cause this seam knows, or could not be read.
    Unknown,
}

impl WorkerEnd {
    /// The end a disposed worker's exit status `status` records.
    #[must_use]
    pub const fn of_status(status: Option<i32>) -> Self {
        let Some(code) = status else {
            return Self::Unknown;
        };
        match WorkerExit::from_code(code) {
            Some(WorkerExit::OutOfMemory) => Self::OutOfMemory,
            Some(WorkerExit::PeerClosed | WorkerExit::Oversize | WorkerExit::Transport) => {
                Self::TransportFailed
            }
            None if code == tairix_abi::LOAD_OOM || code == tairix_abi::OOM_EXIT_STATUS => {
                Self::OutOfMemory
            }
            None if code == tairix_abi::FAULT_EXIT_STATUS => Self::Crashed,
            None if matches!(Signal::Kill.termination_status(), Some(kill) if kill == code) => {
                Self::Killed
            }
            Some(WorkerExit::Finished) | None => Self::Unknown,
        }
    }

    /// A terse human-readable statement of this end.
    #[must_use]
    pub const fn reason(self) -> &'static str {
        match self {
            Self::OutOfMemory => "out of memory",
            Self::Crashed => "crashed",
            Self::Killed => "killed",
            Self::TransportFailed => "its transport failed",
            Self::Unknown => "cause unknown",
        }
    }
}

/// Typed failure a [`ParserSandbox::request`] can report.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum SandboxError {
    /// No worker could be started; the carried errno names the launch
    /// failure.
    WorkerUnavailable(Errno),
    /// The worker crashed, violated the protocol, or did not answer within
    /// its transport's deadline, and ended as the carried [`WorkerEnd`]. It
    /// has been disposed of; the next request starts a fresh one. The request
    /// was not answered.
    WorkerFailed(WorkerEnd),
    /// The request payload exceeds [`MAX_FRAME`]; nothing was sent.
    RequestTooLarge,
}

impl SandboxError {
    /// Whether the request failed for want of memory, and may succeed once
    /// memory is freed — as opposed to an input or worker that is broken.
    #[must_use]
    pub const fn out_of_memory(&self) -> bool {
        matches!(
            self,
            Self::WorkerUnavailable(Errno::OutOfMemory)
                | Self::WorkerFailed(WorkerEnd::OutOfMemory)
        )
    }
}

impl core::fmt::Display for SandboxError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::WorkerUnavailable(errno) => write!(f, "no parser could be started ({errno})"),
            Self::WorkerFailed(end) => {
                write!(f, "the parser stopped without answering ({})", end.reason())
            }
            Self::RequestTooLarge => f.write_str("the request is larger than a parser takes"),
        }
    }
}

/// The parent-side seam: one sandboxed worker, one outstanding request.
///
/// The type deliberately serialises requests (send, then block for the
/// reply): a parse job is a synchronous question, and one-at-a-time keeps
/// the framing unambiguous with no request ids to validate.
pub struct ParserSandbox<L: Launcher, S: Sink> {
    launcher: L,
    sink: S,
    live: Option<L::Channel>,
    /// Workers contained so far: an [`ask`](Self::ask) that sees it move
    /// knows the worker that answered it is already gone.
    contained: u64,
}

impl<L: Launcher, S: Sink> ParserSandbox<L, S> {
    /// Build the seam over `launcher`, logging containment events to
    /// `sink`. No worker is started until the first request needs one.
    pub fn new(launcher: L, sink: S) -> Self {
        Self {
            launcher,
            sink,
            live: None,
            contained: 0,
        }
    }

    /// Whether a worker is running.
    #[must_use]
    pub const fn is_live(&self) -> bool {
        self.live.is_some()
    }

    /// Shut the live worker down through the launcher (transport closed,
    /// process reaped), keeping the seam: the next request starts a fresh
    /// one. What an owner with no work for its worker does while memory is
    /// short, and what dropping the seam does, so no worker outlives it.
    pub fn release(&mut self) {
        if let Some(channel) = self.live.take() {
            let _ = self.launcher.dispose(channel);
        }
    }

    /// Send one request payload and return the worker's reply payload.
    ///
    /// On any worker failure the error path runs the full containment
    /// discipline before returning: dispose (reap) and log
    /// [`EVENT_WORKER_CRASHED`] with the exit code and the cause it carries.
    /// A request with no live worker starts one, and a start that fails is
    /// logged as [`EVENT_WORKER_UNAVAILABLE`].
    ///
    /// # Errors
    ///
    /// [`SandboxError`], as above. The failed request is never retried
    /// automatically: the caller decides whether the parse mattered, and
    /// [`SandboxError::out_of_memory`] tells it whether trying again later
    /// could help.
    pub fn request(&mut self, payload: &[u8]) -> Result<Vec<u8>, SandboxError> {
        if payload.len() > MAX_FRAME {
            return Err(SandboxError::RequestTooLarge);
        }
        let channel = if let Some(channel) = self.live.as_mut() {
            channel
        } else {
            let launched = self.launcher.launch().map_err(|errno| {
                self.log_unavailable(errno);
                SandboxError::WorkerUnavailable(errno)
            })?;
            self.live.insert(launched)
        };
        channel.begin_exchange();
        let outcome = send_frame(channel, payload).and_then(|()| recv_frame(channel));
        let reason = match outcome {
            // A reply arrived; the worker stays live for the next request.
            Ok(Some(reply)) => return Ok(reply),
            Err(ProtoError::Channel(Errno::TimedOut)) => "worker did not answer in time",
            // Exited without answering, died mid-frame, declared an oversize
            // reply, or the transport failed: the worker is gone or can no
            // longer be believed.
            Ok(None) | Err(_) => "worker failed mid-request",
        };
        Err(SandboxError::WorkerFailed(self.contain_failure(reason)))
    }

    /// Run `exchange` — its requests and the decoding of their replies — and
    /// contain the worker when what it answered could not be believed,
    /// exactly as a crash is: disposed of, logged and replaced, so a worker
    /// that has shown itself broken never answers another request. A worker
    /// already contained meanwhile, by a crash or an inner `ask`, is not
    /// contained twice.
    ///
    /// # Errors
    ///
    /// `exchange`'s own failure, unchanged.
    pub fn ask<T, E: Unbelieved>(
        &mut self,
        exchange: impl FnOnce(&mut Self) -> Result<T, E>,
    ) -> Result<T, E> {
        let before = self.contained;
        let answer = exchange(self);
        if self.contained == before && answer.as_ref().is_err_and(Unbelieved::unbelieved) {
            self.contain_failure("worker reply could not be believed");
        }
        answer
    }

    /// Dispose of the failed worker and log why, answering what became of it.
    fn contain_failure(&mut self, reason: &'static str) -> WorkerEnd {
        self.contained = self.contained.wrapping_add(1);
        let exit_code = self
            .live
            .take()
            .and_then(|channel| self.launcher.dispose(channel));
        log_worker_crashed(&self.sink, reason, exit_code)
    }

    /// Log a failed launch (initial or replacement).
    fn log_unavailable(&self, errno: Errno) {
        log_unavailable(&self.sink, errno);
    }
}

impl<L: Launcher, S: Sink> Drop for ParserSandbox<L, S> {
    fn drop(&mut self) {
        self.release();
    }
}

/// Emit [`EVENT_WORKER_CRASHED`]: a worker that will be replaced was
/// disposed of for `reason`, with its exit code when one is known and the
/// cause that code carries, answering that cause.
///
/// The one emitter of that id, shared by [`ParserSandbox`] and the
/// supervised session ([`crate::supervise`]) — the two seams whose failed
/// worker is replaced rather than ended.
pub fn log_worker_crashed<S: Sink>(
    sink: &S,
    reason: &'static str,
    exit_code: Option<i32>,
) -> WorkerEnd {
    let end = WorkerEnd::of_status(exit_code);
    let exit_field = match exit_code {
        Some(code) => FieldValue::SignedInt(i64::from(code)),
        None => FieldValue::Null,
    };
    tairix_log::log(
        sink,
        &Event {
            level: Level::Warn,
            id: EVENT_WORKER_CRASHED,
            message: "parser sandbox worker crashed; replaced",
            fields: &[
                Field {
                    key: "reason",
                    value: FieldValue::Str(reason),
                },
                Field {
                    key: "cause",
                    value: FieldValue::Str(end.reason()),
                },
                Field {
                    key: "exit_code",
                    value: exit_field,
                },
            ],
        },
    );
    end
}

/// Emit [`EVENT_WORKER_UNAVAILABLE`]: a sandboxed worker could not be
/// started.
///
/// The one emitter of that id. [`ParserSandbox`] and the supervised session
/// log their own failed launches through it, and a plain session's owner
/// logs its transport constructor's — a plain session has no launcher of
/// its own ([`crate::session`]).
pub fn log_unavailable<S: Sink>(sink: &S, errno: Errno) {
    tairix_log::log(
        sink,
        &Event {
            level: Level::Error,
            id: EVENT_WORKER_UNAVAILABLE,
            message: "parser sandbox worker could not be started",
            fields: &[Field {
                key: "error",
                value: FieldValue::Error(errno),
            }],
        },
    );
}

#[cfg(test)]
mod tests {
    use super::{
        Launcher, ParserSandbox, SandboxError, Unbelieved, WorkerEnd, EVENT_WORKER_CRASHED,
        EVENT_WORKER_UNAVAILABLE,
    };
    use crate::proto::{Channel, MAX_FRAME};
    use crate::worker::WorkerExit;
    use alloc::rc::Rc;
    use alloc::vec;
    use alloc::vec::Vec;
    use core::cell::RefCell;
    use tairix_abi::{Errno, Signal};
    use tairix_log::{Event, EventId, Level, Sink};

    /// Captures `(id, level)` pairs of every logged event.
    #[derive(Clone, Default)]
    struct RecordingSink {
        events: Rc<RefCell<Vec<(EventId, Level)>>>,
    }

    impl Sink for RecordingSink {
        fn write_event(&self, event: &Event<'_>) {
            self.events.borrow_mut().push((event.id, event.level));
        }
    }

    /// One scripted worker: answers every request with `reply` until
    /// `answers` runs out, then reports end-of-stream (the worker "died").
    struct ScriptedChannel {
        reply: Vec<u8>,
        answers: usize,
        pending: Vec<u8>,
        at: usize,
    }

    impl Channel for ScriptedChannel {
        fn read(&mut self, buf: &mut [u8]) -> Result<usize, Errno> {
            if self.at == self.pending.len() {
                if self.answers == 0 {
                    return Ok(0);
                }
                self.answers -= 1;
                let mut framed = Vec::new();
                framed.extend_from_slice(
                    &u32::try_from(self.reply.len())
                        .expect("small reply")
                        .to_le_bytes(),
                );
                framed.extend_from_slice(&self.reply);
                self.pending = framed;
                self.at = 0;
            }
            let take = buf.len().min(self.pending.len() - self.at);
            buf[..take].copy_from_slice(&self.pending[self.at..self.at + take]);
            self.at += take;
            Ok(take)
        }

        fn write(&mut self, buf: &[u8]) -> Result<usize, Errno> {
            Ok(buf.len())
        }
    }

    /// Launcher whose successive `launch` calls produce the scripted
    /// workers; records launches and disposals.
    struct ScriptedLauncher {
        scripts: Vec<Result<ScriptedChannel, Errno>>,
        launched: usize,
        disposed: usize,
    }

    impl Launcher for ScriptedLauncher {
        type Channel = ScriptedChannel;

        fn launch(&mut self) -> Result<ScriptedChannel, Errno> {
            self.launched += 1;
            if self.scripts.is_empty() {
                return Err(Errno::NotFound);
            }
            self.scripts.remove(0)
        }

        fn dispose(&mut self, _channel: ScriptedChannel) -> Option<i32> {
            self.disposed += 1;
            Some(139)
        }
    }

    fn worker(answers: usize) -> ScriptedChannel {
        ScriptedChannel {
            reply: b"ok".to_vec(),
            answers,
            pending: Vec::new(),
            at: 0,
        }
    }

    #[test]
    fn a_reply_flows_back_and_the_worker_is_reused() {
        let launcher = ScriptedLauncher {
            scripts: vec![Ok(worker(2))],
            launched: 0,
            disposed: 0,
        };
        let sink = RecordingSink::default();
        let mut sandbox = ParserSandbox::new(launcher, sink.clone());
        assert_eq!(sandbox.request(b"one"), Ok(b"ok".to_vec()));
        assert_eq!(sandbox.request(b"two"), Ok(b"ok".to_vec()));
        assert_eq!(sandbox.launcher.launched, 1);
        assert!(sink.events.borrow().is_empty());
    }

    #[test]
    fn a_dead_worker_is_contained_logged_and_replaced_by_the_next_request() {
        // First worker answers once then dies; the replacement answers.
        let launcher = ScriptedLauncher {
            scripts: vec![Ok(worker(1)), Ok(worker(1))],
            launched: 0,
            disposed: 0,
        };
        let sink = RecordingSink::default();
        let mut sandbox = ParserSandbox::new(launcher, sink.clone());

        assert_eq!(sandbox.request(b"one"), Ok(b"ok".to_vec()));
        // The worker's stream ends before this reply: a typed failure naming
        // what its status says became of it...
        assert_eq!(
            sandbox.request(b"two"),
            Err(SandboxError::WorkerFailed(WorkerEnd::Crashed))
        );
        // ...the dead worker reaped and the crash logged with the stable id,
        // and nothing started in its place until something needs it.
        assert_eq!(sandbox.launcher.disposed, 1);
        assert_eq!(sandbox.launcher.launched, 1);
        assert!(!sandbox.is_live());
        assert_eq!(
            sink.events.borrow().as_slice(),
            &[(EVENT_WORKER_CRASHED, Level::Warn)]
        );
        // The caller survives and the next request starts the replacement.
        assert_eq!(sandbox.request(b"three"), Ok(b"ok".to_vec()));
        assert_eq!(sandbox.launcher.launched, 2);
    }

    /// A service failure that is, or is not, a reply beyond belief.
    struct Judged(bool);

    impl Unbelieved for Judged {
        fn unbelieved(&self) -> bool {
            self.0
        }
    }

    /// An exchange of one request whose reply its service judged `judged`.
    fn judged(
        sandbox: &mut ParserSandbox<ScriptedLauncher, RecordingSink>,
        payload: &[u8],
        judged: bool,
    ) -> Result<Vec<u8>, Judged> {
        let reply = sandbox.request(payload).map_err(|_| Judged(false))?;
        if reply == b"ok" {
            Err(Judged(judged))
        } else {
            Ok(reply)
        }
    }

    #[test]
    fn an_unbelievable_answer_replaces_the_worker_once_and_a_refusal_does_not() {
        let fresh = |answers| ScriptedChannel {
            reply: b"fresh".to_vec(),
            ..worker(answers)
        };
        let launcher = ScriptedLauncher {
            scripts: vec![Ok(worker(3)), Ok(fresh(1)), Ok(fresh(1))],
            launched: 0,
            disposed: 0,
        };
        let sink = RecordingSink::default();
        let mut sandbox = ParserSandbox::new(launcher, sink.clone());

        assert!(sandbox.ask(|s| judged(s, b"one", false)).is_err());
        assert_eq!(sandbox.launcher.disposed, 0, "a refusal is an answer");
        assert!(sink.events.borrow().is_empty());

        assert!(sandbox.ask(|s| judged(s, b"two", true)).is_err());
        assert_eq!(sandbox.launcher.disposed, 1);
        assert_eq!(
            sink.events.borrow().as_slice(),
            &[(EVENT_WORKER_CRASHED, Level::Warn)]
        );
        // The replacement, not the discredited worker, answers next.
        assert_eq!(sandbox.request(b"three"), Ok(b"fresh".to_vec()));
        assert_eq!(sandbox.launcher.launched, 2);

        // The first worker's last answer is never heard: an inner `ask`
        // contains it, and the outer one leaves the replacement alone.
        let mut script = ParserSandbox::new(
            ScriptedLauncher {
                scripts: vec![Ok(worker(1)), Ok(fresh(1))],
                launched: 0,
                disposed: 0,
            },
            RecordingSink::default(),
        );
        assert!(script
            .ask(|outer| outer.ask(|inner| judged(inner, b"four", true)))
            .is_err());
        assert_eq!(script.launcher.disposed, 1, "one containment, not two");
        assert_eq!(script.request(b"five"), Ok(b"fresh".to_vec()));
    }

    #[test]
    fn a_failed_initial_launch_is_typed_and_logged() {
        let launcher = ScriptedLauncher {
            scripts: vec![Err(Errno::PermissionDenied)],
            launched: 0,
            disposed: 0,
        };
        let sink = RecordingSink::default();
        let mut sandbox = ParserSandbox::new(launcher, sink.clone());
        assert_eq!(
            sandbox.request(b"one"),
            Err(SandboxError::WorkerUnavailable(Errno::PermissionDenied))
        );
        assert_eq!(
            sink.events.borrow().as_slice(),
            &[(EVENT_WORKER_UNAVAILABLE, Level::Error)]
        );
    }

    #[test]
    fn a_replacement_that_cannot_start_is_reported_to_the_request_that_needed_it() {
        // One worker that dies immediately; no replacement available; then
        // a later launch succeeds.
        let launcher = ScriptedLauncher {
            scripts: vec![Ok(worker(0)), Err(Errno::OutOfMemory), Ok(worker(1))],
            launched: 0,
            disposed: 0,
        };
        let sink = RecordingSink::default();
        let mut sandbox = ParserSandbox::new(launcher, sink.clone());

        assert_eq!(
            sandbox.request(b"one"),
            Err(SandboxError::WorkerFailed(WorkerEnd::Crashed))
        );
        let failed = sandbox.request(b"two");
        assert_eq!(
            failed,
            Err(SandboxError::WorkerUnavailable(Errno::OutOfMemory))
        );
        assert!(failed.is_err_and(|err| err.out_of_memory()));
        assert_eq!(
            sink.events.borrow().as_slice(),
            &[
                (EVENT_WORKER_CRASHED, Level::Warn),
                (EVENT_WORKER_UNAVAILABLE, Level::Error),
            ]
        );
        assert_eq!(sandbox.request(b"three"), Ok(b"ok".to_vec()));
    }

    #[test]
    fn a_disposed_workers_status_names_what_became_of_it() {
        let cases = [
            (Some(WorkerExit::OutOfMemory.code()), WorkerEnd::OutOfMemory),
            (Some(tairix_abi::LOAD_OOM), WorkerEnd::OutOfMemory),
            (Some(tairix_abi::OOM_EXIT_STATUS), WorkerEnd::OutOfMemory),
            (Some(tairix_abi::FAULT_EXIT_STATUS), WorkerEnd::Crashed),
            (Signal::Kill.termination_status(), WorkerEnd::Killed),
            (
                Some(WorkerExit::PeerClosed.code()),
                WorkerEnd::TransportFailed,
            ),
            (
                Some(WorkerExit::Transport.code()),
                WorkerEnd::TransportFailed,
            ),
            (Some(WorkerExit::Finished.code()), WorkerEnd::Unknown),
            (Some(tairix_abi::LOAD_UNVERIFIED), WorkerEnd::Unknown),
            (None, WorkerEnd::Unknown),
        ];
        for (status, end) in cases {
            assert_eq!(WorkerEnd::of_status(status), end, "{status:?}");
        }
    }

    #[test]
    fn only_a_failure_for_want_of_memory_is_worth_asking_again() {
        assert!(SandboxError::WorkerFailed(WorkerEnd::OutOfMemory).out_of_memory());
        assert!(SandboxError::WorkerUnavailable(Errno::OutOfMemory).out_of_memory());
        for broken in [
            SandboxError::WorkerFailed(WorkerEnd::Crashed),
            SandboxError::WorkerFailed(WorkerEnd::Killed),
            SandboxError::WorkerFailed(WorkerEnd::TransportFailed),
            SandboxError::WorkerFailed(WorkerEnd::Unknown),
            SandboxError::WorkerUnavailable(Errno::PermissionDenied),
            SandboxError::RequestTooLarge,
        ] {
            assert!(!broken.out_of_memory(), "{broken:?}");
        }
    }

    #[test]
    fn an_oversize_request_is_refused_before_any_launch() {
        let launcher = ScriptedLauncher {
            scripts: vec![Ok(worker(1))],
            launched: 0,
            disposed: 0,
        };
        let sink = RecordingSink::default();
        let mut sandbox = ParserSandbox::new(launcher, sink.clone());
        let oversize = vec![0u8; MAX_FRAME + 1];
        assert_eq!(
            sandbox.request(&oversize),
            Err(SandboxError::RequestTooLarge)
        );
        assert_eq!(sandbox.launcher.launched, 0);
        assert!(sink.events.borrow().is_empty());
    }

    #[test]
    fn dropping_the_seam_disposes_the_live_worker() {
        /// Launcher that records disposals somewhere the test can still
        /// see after the seam (which owns the launcher) is dropped.
        struct CountingLauncher {
            disposed: Rc<RefCell<usize>>,
        }

        impl Launcher for CountingLauncher {
            type Channel = ScriptedChannel;

            fn launch(&mut self) -> Result<ScriptedChannel, Errno> {
                Ok(worker(1))
            }

            fn dispose(&mut self, _channel: ScriptedChannel) -> Option<i32> {
                *self.disposed.borrow_mut() += 1;
                None
            }
        }

        let disposed = Rc::new(RefCell::new(0));
        let launcher = CountingLauncher {
            disposed: disposed.clone(),
        };
        let mut sandbox = ParserSandbox::new(launcher, RecordingSink::default());
        assert_eq!(sandbox.request(b"one"), Ok(b"ok".to_vec()));
        drop(sandbox);
        // The healthy live worker was shut down through the launcher.
        assert_eq!(*disposed.borrow(), 1);
    }

    /// An idle worker can be let go without losing the seam: the next
    /// request starts another, and releasing nothing does nothing.
    #[test]
    fn releasing_disposes_the_worker_and_the_next_request_starts_another() {
        let launcher = ScriptedLauncher {
            scripts: vec![Ok(worker(1)), Ok(worker(1))],
            launched: 0,
            disposed: 0,
        };
        let mut sandbox = ParserSandbox::new(launcher, RecordingSink::default());
        assert!(!sandbox.is_live(), "nothing started before a request");
        assert_eq!(sandbox.request(b"one"), Ok(b"ok".to_vec()));
        assert!(sandbox.is_live());
        sandbox.release();
        assert!(!sandbox.is_live());
        assert_eq!(sandbox.launcher.disposed, 1);
        sandbox.release();
        assert_eq!(
            sandbox.launcher.disposed, 1,
            "releasing nothing disposes nothing"
        );
        assert_eq!(sandbox.request(b"two"), Ok(b"ok".to_vec()));
        assert_eq!(sandbox.launcher.launched, 2, "a fresh worker");
    }

    #[test]
    fn the_event_ids_are_frozen() {
        // The identifiers are a contract with log consumers; renumbering
        // them is an ABI break this test refuses.
        assert_eq!(EVENT_WORKER_CRASHED, EventId(6000));
        assert_eq!(EVENT_WORKER_UNAVAILABLE, EventId(6001));
    }
}
