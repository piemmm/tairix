//! The production transport: pipes plus the kernel sandbox spawn mode.
//!
//! Compiled only into freestanding `Run` binaries (feature `program`).
//! The parent side ([`RtLauncher`]) spawns **its own binary** in a worker
//! role: two fresh pipes are created, the child's fd 0 is wired to the
//! request pipe's read end and its fd 1 to the reply pipe's write end
//! through `SpawnAttach::sandbox`, and everything else is closed — the
//! kernel then brands the child capability-empty and confines it to the
//! sandbox syscall allow-list (`docs/src/security/sandbox.md`). The worker
//! side ([`serve_stdio`]) serves the protocol over those standard streams,
//! exactly the surface the allow-list admits.
//!
//! A program wires the two halves together in its `Run` binary: early in
//! `main`, [`worker_role`] detects the worker invocation and hands control
//! to [`serve_stdio`]; otherwise the program builds a
//! [`ParserSandbox`](crate::host::ParserSandbox) over an [`RtLauncher`]
//! naming its own program path.
//!
//! The duplex seam ([`crate::session`]) rides the same spawn: a parent
//! constructs an [`RtSessionChannel`] (or has an [`RtSessionLauncher`] do so
//! for a supervised session), keeps its wait-set in step with the two
//! descriptors through [`SessionMembers`], and drives the session from those
//! wakes; the worker detects [`session_worker_role`] and hands control to
//! [`serve_session_stdio`].

use alloc::sync::Arc;
use alloc::vec::Vec;

use tairix_abi::{
    Errno, FdWire, Signal, SpawnAttach, WaitSetOp, WaitSourceKind, STDIN, STDOUT, STD_STREAM_COUNT,
};
use tairix_rt::io::{Error as IoError, Read, Stdin, Stdout, Stream, Write};

use crate::host::Launcher;
use crate::memory::WorkerMemory;
use crate::proto::Channel;
use crate::session::{
    let_go, serve_session, Reaper, SessionDescriptors, SessionService, SessionTransport,
};
use crate::supervise::SessionLauncher;
use crate::worker::{serve, ServeEnd, Service};

/// The argument-vector marker a parent passes (as `argv[1]`) when
/// spawning its own binary as a **one-shot** sandbox worker, and
/// [`worker_role`] detects. One shared spelling, so no program invents a
/// colliding flag.
pub const WORKER_ROLE_ARG: &[u8] = b"--parser-sandbox-worker";

/// The same marker for a **session** worker ([`crate::session`]), so one
/// binary can serve both roles and tell them apart.
pub const SESSION_ROLE_ARG: &[u8] = b"--sandbox-session-worker";

/// How long a one-shot worker may take to answer one request: a
/// containment bound, so a parse that never ends — a hostile file driving a
/// decoder round a loop — costs its caller this long and not for ever.
///
/// The slowest legitimate answer is a full decode of the largest document
/// the limits admit: about two seconds on a desktop core, so a deadline of
/// two minutes leaves room for a core fifteen times slower that is also
/// heavily loaded.
pub const REPLY_DEADLINE_NS: u64 = 120_000_000_000;

/// Whether this invocation is a one-shot sandbox-worker role: `argv[1]` is
/// exactly [`WORKER_ROLE_ARG`].
///
/// A `Run` binary checks this before any other argument handling and, when
/// true, runs [`serve_stdio`] and exits — a worker never behaves as the
/// interactive program.
#[must_use]
pub fn worker_role() -> bool {
    tairix_rt::arg(1).is_some_and(|arg| arg == WORKER_ROLE_ARG)
}

/// Whether this invocation is a one-shot worker asked to serve `service`:
/// [`worker_role`], with `argv[2]` exactly `service`.
///
/// For a program whose binary serves more than one one-shot service, each in
/// a worker of its own ([`RtLauncher::serving`]) so no two parsers share an
/// address space. A worker asked for none of them serves nothing and exits
/// [`crate::WorkerExit::Unserved`].
#[must_use]
pub fn serves(service: &[u8]) -> bool {
    worker_role() && tairix_rt::arg(2).is_some_and(|arg| arg == service)
}

/// Whether this invocation is a session-worker role: `argv[1]` is exactly
/// [`SESSION_ROLE_ARG`]. Checked alongside [`worker_role`], and handed to
/// [`serve_session_stdio`].
#[must_use]
pub fn session_worker_role() -> bool {
    tairix_rt::arg(1).is_some_and(|arg| arg == SESSION_ROLE_ARG)
}

/// Serve the sandbox protocol over the wired standard streams until the
/// parent closes the request pipe.
pub fn serve_stdio<S: Service>(service: &mut S) -> ServeEnd {
    let mut chan = StdioChannel;
    serve(&mut chan, service)
}

/// A fresh draw to key a decoder's page cache with, so a file cannot choose
/// which of its pages collide; `None` when the kernel has no secure bytes to
/// give.
#[must_use]
pub fn draw_seed() -> Option<tairix_hash::HashSeed> {
    let mut key = [0u8; tairix_hash::HashSeed::LEN];
    tairix_rt::random_fill(&mut key).ok()?;
    Some(tairix_hash::HashSeed::from_bytes(key))
}

/// Serve a duplex session over the wired standard streams until the parent
/// closes the request pipe or the service closes the session.
///
/// The blocking standard-stream channel is what the worker *should* use:
/// the sandbox allow-list gives it no other wake source, and the parent's
/// never-blocking seam keeps both directions moving
/// ([`crate::session`]).
pub fn serve_session_stdio<S: SessionService>(service: &mut S) -> ServeEnd {
    let mut chan = StdioChannel;
    serve_session(&mut chan, service)
}

/// The worker's channel: fd 0 in, fd 1 out — the only descriptors a
/// canonically wired sandbox holds.
pub struct StdioChannel;

impl Channel for StdioChannel {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, Errno> {
        // A zero timeout waits indefinitely; the pipe backing parks the
        // worker until bytes arrive or every write end closes (then
        // end-of-stream, 0).
        Stdin.read(buf).map_err(IoError::as_errno)
    }

    fn write(&mut self, buf: &[u8]) -> Result<usize, Errno> {
        match Stdout.write(buf) {
            // `Stdout::write` reports a kernel refusal through the error
            // channel; with a pipe backing that means the parent is gone,
            // which the framing's zero-progress rule reports as the peer
            // closed.
            Ok(0) if !buf.is_empty() => Err(Errno::BrokenPipe),
            Ok(accepted) => Ok(accepted),
            Err(e) => Err(e.as_errno()),
        }
    }
}

/// [`Launcher`] that spawns `path` (normally the program's own binary) as
/// a sandboxed worker over a fresh pipe pair per launch.
pub struct RtLauncher {
    path: Vec<u8>,
    reply_deadline_ns: u64,
    memory: WorkerMemory,
    service: Option<&'static [u8]>,
}

impl RtLauncher {
    /// Build a launcher over the program path to spawn as the worker, whose
    /// workers parse what `memory` names and answer within
    /// [`REPLY_DEADLINE_NS`].
    #[must_use]
    pub fn new(path: &[u8], memory: WorkerMemory) -> Self {
        Self {
            path: path.to_vec(),
            reply_deadline_ns: REPLY_DEADLINE_NS,
            memory,
            service: None,
        }
    }

    /// The same launcher, its workers told to serve `service`, which the
    /// worker reads back through [`serves`].
    #[must_use]
    pub const fn serving(mut self, service: &'static [u8]) -> Self {
        self.service = Some(service);
        self
    }

    /// The same launcher, its workers given `deadline_ns` to answer each
    /// request instead.
    #[must_use]
    pub const fn answering_within(mut self, deadline_ns: u64) -> Self {
        self.reply_deadline_ns = deadline_ns;
        self
    }

    /// Build a launcher over this program's own binary, via the kernel's
    /// reserved self token ([`tairix_abi::SPAWN_SELF`]): the kernel
    /// substitutes the exact path it admitted the calling process from and
    /// runs the ordinary load gate over it. `argv[0]` is deliberately not
    /// used — it is data the spawner chose (a shell passes the typed
    /// word), never a spawnable spelling the worker launch could trust.
    #[must_use]
    pub fn own_binary(memory: WorkerMemory) -> Self {
        Self::new(tairix_abi::SPAWN_SELF, memory)
    }
}

/// The memory ceiling a `memory` worker launched now is held to: a document
/// worker's is a share of the machine's installed RAM, read when it starts.
fn ceiling_of(memory: WorkerMemory) -> u64 {
    let ram = match memory {
        WorkerMemory::Messages | WorkerMemory::HeldDocument => 0,
        WorkerMemory::Documents => {
            tairix_procinfo::memory_total_bytes(&tairix_procinfo::IpcTransport).unwrap_or(0)
        }
    };
    memory.ceiling(ram)
}

/// Spawn `path` as a sandboxed worker in `role`, serving `service` where one
/// is named, over a fresh pipe pair, held to `ceiling` bytes of mapped
/// memory, returning the parent's `(pid, request write end, reply read end)`.
fn spawn_sandboxed_worker(
    path: &[u8],
    (role, service): (&[u8], Option<&[u8]>),
    ceiling: u64,
) -> Result<(i64, u32, u32), Errno> {
    let attach = |wires| SpawnAttach::sandbox(wires, ceiling);
    match service {
        Some(service) => spawn_wired(path, &[path, role, service], attach),
        None => spawn_wired(path, &[path, role], attach),
    }
}

/// Spawn `path` with `args` over a fresh pipe pair, its attach block built by
/// `attach` from the wiring of fd 0 to the request pipe and fd 1 to the reply
/// pipe, returning the parent's `(pid, request write end, reply read end)`.
///
/// The one place the pipe pair, the wiring and the unwind are written, for
/// every worker shape — one-shot, session and drawing.
fn spawn_wired(
    path: &[u8],
    args: &[&[u8]],
    attach: impl FnOnce([FdWire; STD_STREAM_COUNT]) -> SpawnAttach,
) -> Result<(i64, u32, u32), Errno> {
    // Request pipe: parent writes, worker fd 0 reads.
    let (request_read, request_write) = tairix_rt::pipe_create().map_err(Errno::from_syscall)?;
    // Reply pipe: worker fd 1 writes, parent reads.
    let (reply_read, reply_write) = match tairix_rt::pipe_create() {
        Ok(pair) => pair,
        Err(ret) => {
            let _ = tairix_rt::fs_close(request_read);
            let _ = tairix_rt::fs_close(request_write);
            return Err(Errno::from_syscall(ret));
        }
    };
    let mut wires = [FdWire::Closed; STD_STREAM_COUNT];
    wires[STDIN as usize] = FdWire::Handle(request_read);
    wires[STDOUT as usize] = FdWire::Handle(reply_write);
    let pid = tairix_rt::spawn_attached(path, &attach(wires), args, &[]);
    // The child holds counted clones of its two wired ends; the parent's
    // own copies are closed regardless of the spawn outcome, so a dead
    // worker's reply pipe reports end-of-stream instead of idling on the
    // parent's dangling write end.
    let _ = tairix_rt::fs_close(request_read);
    let _ = tairix_rt::fs_close(reply_write);
    if pid < 0 {
        let _ = tairix_rt::fs_close(request_write);
        let _ = tairix_rt::fs_close(reply_read);
        return Err(Errno::from_syscall(pid));
    }
    Ok((pid, request_write, reply_read))
}

impl Launcher for RtLauncher {
    type Channel = RtChannel;

    fn launch(&mut self) -> Result<RtChannel, Errno> {
        let (pid, write_fd, read_fd) = spawn_sandboxed_worker(
            &self.path,
            (WORKER_ROLE_ARG, self.service),
            ceiling_of(self.memory),
        )?;
        Ok(RtChannel {
            pid,
            write_fd,
            read_fd,
            reply_deadline_ns: self.reply_deadline_ns,
            deadline_ns: 0,
        })
    }

    fn dispose(&mut self, channel: RtChannel) -> Option<i32> {
        let pid = channel.pid;
        drop(channel);
        end_worker(pid)
    }
}

/// End worker `pid`, whose pipes its owner has closed, and reap it,
/// answering its exit code.
///
/// It is killed first: a worker still parsing, or one that ignores the end
/// of its input, would never exit by itself, and the reap would wait on it
/// for ever.
fn end_worker(pid: i64) -> Option<i32> {
    let _ = tairix_rt::signal(pid, Signal::Kill);
    wait_worker(pid)
}

/// Wait for worker `pid`, already told to end, and reap it.
fn wait_worker(pid: i64) -> Option<i32> {
    let mut code = 0i32;
    let reaped = tairix_rt::wait_exit(pid, &mut code);
    (reaped >= 0).then_some(code)
}

/// The parent's channel to one spawned worker: the request pipe's write
/// end and the reply pipe's read end in the parent's own descriptor table.
pub struct RtChannel {
    pid: i64,
    write_fd: u32,
    read_fd: u32,
    /// How long the worker has to answer each request.
    reply_deadline_ns: u64,
    /// When the answer to the request in flight is due, on the monotonic
    /// clock.
    deadline_ns: u64,
}

impl Channel for RtChannel {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, Errno> {
        // A timeout of zero would wait for ever, so a deadline already
        // reached is answered here.
        let left = self.deadline_ns.saturating_sub(tairix_rt::clock_get());
        if left == 0 {
            return Err(Errno::TimedOut);
        }
        // A pipe ignores the file offset; end-of-stream reads 0.
        Stream::new(self.read_fd)
            .read_timeout(buf, left)
            .map_err(IoError::as_errno)
    }

    fn write(&mut self, buf: &[u8]) -> Result<usize, Errno> {
        tairix_rt::fs_write(self.write_fd, 0, buf).map_err(Errno::from_syscall)
    }

    fn begin_exchange(&mut self) {
        self.deadline_ns = tairix_rt::clock_get().saturating_add(self.reply_deadline_ns);
    }
}

impl Drop for RtChannel {
    fn drop(&mut self) {
        // Closing the parent's ends is what tells the worker its parent is
        // done (end-of-stream on fd 0): the worker's serve loop then
        // finishes cleanly and the process exits.
        let _ = tairix_rt::fs_close(self.write_fd);
        let _ = tairix_rt::fs_close(self.read_fd);
    }
}

/// The production [`SessionTransport`]: one sandboxed session worker over
/// a pipe pair, with the descriptor numbers its owner registers on a
/// wait-set.
///
/// Each call is exactly one `fs_read`/`fs_write`, taken only when the
/// owner's readiness said it would complete, so the owner's serve loop
/// never parks on one session while others wait.
pub struct RtSessionChannel {
    pid: i64,
    write_fd: u32,
    read_fd: u32,
    /// Where its worker is reaped should it still be running at the end.
    reaper: Option<Arc<dyn Reaper + Send + Sync>>,
}

impl RtSessionChannel {
    /// Spawn `path` as this session's sandboxed worker, parsing what `memory`
    /// names.
    ///
    /// Pass [`tairix_abi::SPAWN_SELF`] to run the caller's **own** binary:
    /// the kernel substitutes the exact path it admitted the caller from
    /// and runs the ordinary load gate over it, where `argv[0]` is data the
    /// spawner chose and never a spawnable spelling.
    ///
    /// # Errors
    ///
    /// The typed reason the worker could not be started; the caller logs it
    /// through [`crate::host::log_unavailable`].
    pub fn launch(path: &[u8], memory: WorkerMemory) -> Result<Self, Errno> {
        let (pid, write_fd, read_fd) =
            spawn_sandboxed_worker(path, (SESSION_ROLE_ARG, None), ceiling_of(memory))?;
        Ok(Self {
            pid,
            write_fd,
            read_fd,
            reaper: None,
        })
    }

    /// Spawn the bundle entry point `path` as a drawing sandbox: held to
    /// `memory` bytes of mapped memory and `threads` threads, and handed the
    /// shared region `share`, which the caller holds a grant for.
    ///
    /// # Errors
    ///
    /// The typed reason the program could not be started.
    pub fn launch_drawing(
        path: &[u8],
        memory: u64,
        threads: u64,
        share: u64,
    ) -> Result<Self, Errno> {
        let (pid, write_fd, read_fd) = spawn_wired(path, &[path], |wires| {
            SpawnAttach::drawing(wires, memory, threads, share)
        })?;
        Ok(Self {
            pid,
            write_fd,
            read_fd,
            reaper: None,
        })
    }

    /// This channel, its worker reaped by `reaper` if it is still running when
    /// its session ends, rather than waited for.
    #[must_use]
    pub fn reaped_by(mut self, reaper: Arc<dyn Reaper + Send + Sync>) -> Self {
        self.reaper = Some(reaper);
        self
    }
}

impl SessionTransport for RtSessionChannel {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, Errno> {
        // A pipe ignores the file offset; end-of-stream reads 0.
        tairix_rt::fs_read(self.read_fd, 0, buf).map_err(Errno::from_syscall)
    }

    fn write(&mut self, buf: &[u8]) -> Result<usize, Errno> {
        tairix_rt::fs_write(self.write_fd, 0, buf).map_err(Errno::from_syscall)
    }

    fn descriptors(&self) -> Option<SessionDescriptors> {
        Some(SessionDescriptors {
            read_fd: self.read_fd,
            write_fd: self.write_fd,
        })
    }

    fn dispose(self) -> Option<i32> {
        let (pid, reaper) = (self.pid, self.reaper.clone());
        drop(self);
        let _ = tairix_rt::signal(pid, Signal::Kill);
        let reaper = reaper.as_deref().map(|reaper| reaper as &dyn Reaper);
        let_go(pid, tairix_rt::try_reap(pid), reaper, wait_worker)
    }
}

impl Drop for RtSessionChannel {
    fn drop(&mut self) {
        let _ = tairix_rt::fs_close(self.write_fd);
        let _ = tairix_rt::fs_close(self.read_fd);
    }
}

/// The production [`SessionLauncher`]: each launch is a fresh
/// [`RtSessionChannel`] over `path`.
pub struct RtSessionLauncher {
    path: Vec<u8>,
    memory: WorkerMemory,
}

impl RtSessionLauncher {
    /// Launch `path`, whose workers parse what `memory` names, for a fixture
    /// whose worker roles are distinct paths.
    #[must_use]
    pub fn new(path: &[u8], memory: WorkerMemory) -> Self {
        Self {
            path: path.to_vec(),
            memory,
        }
    }

    /// Launch this program's own binary through [`tairix_abi::SPAWN_SELF`],
    /// the one spelling of it a worker spawn trusts.
    #[must_use]
    pub fn own_binary(memory: WorkerMemory) -> Self {
        Self::new(tairix_abi::SPAWN_SELF, memory)
    }
}

impl SessionLauncher for RtSessionLauncher {
    type Transport = RtSessionChannel;

    fn launch(&mut self) -> Result<RtSessionChannel, Errno> {
        RtSessionChannel::launch(&self.path, self.memory)
    }
}

/// An owner's wait-set registration for one session's two descriptors:
/// `Stream` on the read end while the session wants to read, `StreamRoom`
/// on the write end while it wants to write.
///
/// A direction is registered only while wanted, which is what keeps a
/// level-triggered readiness source from waking an owner with nothing to
/// do. A replaced worker brings new descriptors, so a member naming one the
/// session no longer holds is removed before the new one is added.
pub struct SessionMembers {
    set: u64,
    read_token: u64,
    write_token: u64,
    read: Option<u32>,
    write: Option<u32>,
}

impl SessionMembers {
    /// Track the members of wait-set `set`, reporting the read end's
    /// readiness as `read_token` and the write end's room as `write_token`.
    #[must_use]
    pub const fn new(set: u64, read_token: u64, write_token: u64) -> Self {
        Self {
            set,
            read_token,
            write_token,
            read: None,
            write: None,
        }
    }

    /// Register exactly the wanted directions of `descriptors` — none at all
    /// when there are no descriptors.
    ///
    /// # Errors
    ///
    /// The wait-set's typed refusal to add or remove a member.
    pub fn sync(
        &mut self,
        descriptors: Option<SessionDescriptors>,
        wants_read: bool,
        wants_write: bool,
    ) -> Result<(), Errno> {
        let read = descriptors.filter(|_| wants_read).map(|d| d.read_fd);
        let write = descriptors.filter(|_| wants_write).map(|d| d.write_fd);
        sync_member(
            self.set,
            WaitSourceKind::Stream,
            self.read_token,
            &mut self.read,
            read,
        )?;
        sync_member(
            self.set,
            WaitSourceKind::StreamRoom,
            self.write_token,
            &mut self.write,
            write,
        )
    }
}

/// Move one direction's registration from `armed` to `want`.
fn sync_member(
    set: u64,
    kind: WaitSourceKind,
    token: u64,
    armed: &mut Option<u32>,
    want: Option<u32>,
) -> Result<(), Errno> {
    if *armed == want {
        return Ok(());
    }
    if let Some(fd) = armed.take() {
        let ret = tairix_rt::waitset_ctl(set, WaitSetOp::Del, kind, u64::from(fd), token);
        if ret != 0 {
            return Err(Errno::from_syscall(ret));
        }
    }
    if let Some(fd) = want {
        let ret = tairix_rt::waitset_ctl(set, WaitSetOp::Add, kind, u64::from(fd), token);
        if ret != 0 {
            return Err(Errno::from_syscall(ret));
        }
        *armed = Some(fd);
    }
    Ok(())
}
