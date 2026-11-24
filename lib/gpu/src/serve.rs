//! The served device: `gpu-v1`'s runtime, one device hosted on its driver's
//! channel for every client process.
//!
//! A request is received on the serve loop and run on one of the runtime's
//! workers, which answers it, so no client's slow request — a context made,
//! memory made, a pipeline lowered — holds back another's. A wait is held on
//! the loop and answered when its value lands, its context ends, or its
//! timeout passes. A client's exit, watched from its `Open`, lets go of
//! everything it held. The loop's own wait is bounded by the soonest of
//! those timeouts and the device's soonest job deadline: the watchdog.
//!
//! A client names a context by the id the runtime answered it, found among
//! the caller's own — the caller its kernel-attested process — before
//! anything else is read; an object within it by its key, which the model's
//! tables check. A context of another client and one that never was are
//! refused alike, as stale.

use alloc::collections::VecDeque;
use alloc::sync::Arc;
use alloc::vec::Vec;

use tairix_abi::gpu::{
    encode_reply, GpuError, GpuOp, GpuRequest, GpuStatus, RegionSpan, GPU_MAX_BODY, GPU_MAX_REPLY,
    GPU_REGION_MAX, GPU_REGION_MIN, MAX_CLIENT_CONTEXTS, MAX_CLIENT_EXPORTS,
};
use tairix_abi::reply::encode_status_reply;
use tairix_abi::{Errno, ProcId};
use tairix_collections::Key;
use tairix_inline::ArrayVec;
use tairix_rt::sync::{Condvar, Mutex};
use tairix_spirv::{Admitted, Module, SpecValue, MAX_BINDINGS, MAX_SPECIALIZED};

use crate::binding::{Named, SetRequest};
use crate::command::{Command, Recorded};
use crate::host::{ExportId, Served};
use crate::memory::Charge;
use crate::model::{BindSetLayoutKey, ContextKey, ImageKey, Request, TimelineKey, Watch};
use crate::render::OwnedRender;
use crate::software::Spawn;
use crate::wire::ops::{self, ContextId, ExportToken};
use crate::wire::{
    put_error, put_info, put_submit_error, take_commands, take_name, Arrival, Reader, Wire, Writer,
};
use crate::{
    Error, ImageDesc, LayoutBinding, SubmitError, MAX_SUBMISSION_BUFFERS, MAX_SUBMISSION_COMMANDS,
    MAX_SUBMISSION_POINTS,
};

/// What the runtime asks of the channel it serves: its answers, its clients'
/// regions and exits, the randomness of an export token, and its own wake.
pub(crate) trait Channel: Send + Sync {
    /// Answer the call `ticket`; a caller gone meanwhile is no one to answer.
    fn reply(&self, ticket: u64, frame: &[u8]);
    /// Map the region `client` granted as `grant`.
    fn map(&self, client: ProcId, grant: u64) -> Result<Arc<dyn Region>, Errno>;
    /// Be told when `client` exits.
    fn watch(&self, client: ProcId) -> Result<(), Errno>;
    /// Be told no longer.
    fn unwatch(&self, client: ProcId);
    /// A fresh export token, which no client can guess.
    fn token(&self) -> Result<ExportToken, Errno>;
    /// Rouse the serve loop: the device changed something a wait may be
    /// owed for.
    fn wake(&self);
}

/// A client's region, as the runtime maps it: shared with the client, which
/// may change it at any time, so the runtime copies out what it reads.
pub(crate) trait Region: Send + Sync {
    fn len(&self) -> usize;
    /// Copy `out.len()` bytes at `at` out of the region.
    fn read(&self, at: usize, out: &mut [u8]);
    /// Copy `data` into the region at `at`.
    fn write(&self, at: usize, data: &[u8]);
}

/// A client with its region open.
struct Client {
    proc: ProcId,
    region: Arc<dyn Region>,
    contexts: Vec<ContextKey>,
    exports: usize,
}

/// A wait the runtime holds for a client.
struct Waiter {
    ticket: u64,
    timeline: TimelineKey,
    value: u64,
    deadline: u64,
}

/// What a submission's uploads left for it to run.
#[derive(Default)]
struct Staged {
    buffers: Vec<Vec<Command>>,
    open: Vec<Command>,
    counted: usize,
    charges: Vec<Charge>,
}

/// A context a client holds.
struct Held {
    key: ContextKey,
    client: ProcId,
    staged: Staged,
    waits: Vec<Waiter>,
}

/// An export token, the export it names, and whose it is.
struct Exported {
    token: ExportToken,
    export: ExportId,
    client: ProcId,
}

#[derive(Default)]
struct State {
    /// By process, ascending.
    clients: Vec<Client>,
    /// By the slot a context's key names.
    held: Vec<Option<Held>>,
    /// By token, ascending.
    exports: Vec<Exported>,
    /// No later than the soonest wait's timeout: found again only once one
    /// times out, so a wait answered first costs one early look, not a
    /// search on every pass of the loop.
    earliest: Option<u64>,
}

/// A request a worker runs.
struct Job {
    ticket: u64,
    caller: ProcId,
    op: GpuOp,
    body: ArrayVec<u8, GPU_MAX_BODY>,
}

#[derive(Default)]
struct Work {
    jobs: VecDeque<Job>,
    closed: bool,
}

/// What the model told the runtime of meanwhile.
#[derive(Default)]
struct Moved {
    changes: Vec<(ContextKey, Option<TimelineKey>)>,
    /// A change the list had no room for: every wait is looked at again.
    overflowed: bool,
}

/// The model's watcher: notes each change and rouses the loop.
struct Watcher {
    moved: Mutex<Moved>,
    channel: Arc<dyn Channel>,
}

impl Watch for Watcher {
    fn moved(&self, context: ContextKey, timeline: Option<TimelineKey>) {
        {
            let mut moved = self.moved.lock();
            if moved.changes.try_reserve(1).is_ok() {
                moved.changes.push((context, timeline));
            } else {
                moved.overflowed = true;
            }
        }
        self.channel.wake();
    }
}

/// The runtime: a device, the channel it is served on, and every client's
/// state.
pub(crate) struct Server {
    host: Arc<dyn Served>,
    channel: Arc<dyn Channel>,
    watcher: Arc<Watcher>,
    state: Mutex<State>,
    work: Mutex<Work>,
    jobs: Condvar,
    /// The most memory one context may be given: half the device's.
    ceiling: u64,
}

/// What an op answers: its body, or why it refused.
enum Outcome {
    Done(Vec<u8>),
    Refused(Error),
    SubmitRefused(SubmitError),
}

impl Server {
    /// The runtime for the device `host`, served on `channel`.
    pub(crate) fn new(host: Arc<dyn Served>, channel: Arc<dyn Channel>) -> Arc<Self> {
        let total = host
            .info()
            .heaps
            .iter()
            .fold(0u64, |total, heap| total.saturating_add(heap.size));
        let watcher = Arc::new(Watcher {
            moved: Mutex::new(Moved::default()),
            channel: Arc::clone(&channel),
        });
        host.watch(Arc::clone(&watcher) as Arc<dyn Watch>);
        Arc::new(Self {
            host,
            channel,
            watcher,
            state: Mutex::new(State::default()),
            work: Mutex::new(Work::default()),
            jobs: Condvar::new(),
            ceiling: total / 2,
        })
    }

    /// Start `count` workers on `threads`, answering how many started; with
    /// none, each request runs on the loop's own thread.
    pub(crate) fn start_workers(self: &Arc<Self>, threads: &dyn Spawn, count: usize) -> usize {
        (0..count)
            .take_while(|_| {
                let server = Arc::clone(self);
                threads.spawn(alloc::boxed::Box::new(move || server.work_loop()))
            })
            .count()
    }

    /// Stop the workers once the requests queued are run.
    #[cfg(test)]
    pub(crate) fn close(&self) {
        self.work.lock().closed = true;
        self.jobs.notify_all();
    }

    fn work_loop(&self) {
        loop {
            let job = {
                let mut work = self.work.lock();
                loop {
                    if let Some(job) = work.jobs.pop_front() {
                        break job;
                    }
                    if work.closed {
                        return;
                    }
                    work = self.jobs.wait(work);
                }
            };
            self.execute(&job);
        }
    }

    /// Serve the call `ticket` from `caller`, whose frame is `frame`; with
    /// `workers` started, a request is queued for them.
    pub(crate) fn serve(&self, ticket: u64, caller: ProcId, frame: &[u8], workers: usize) {
        let request = match GpuRequest::decode(frame) {
            Ok(request) => request,
            // No op to name: the shared status frame says why.
            Err(errno) => {
                self.channel.reply(ticket, &encode_status_reply(Err(errno)));
                return;
            }
        };
        match request.op {
            GpuOp::Open => return self.open(ticket, caller, request.body),
            GpuOp::Close => {
                self.client_exited(caller);
                self.channel.unwatch(caller);
                return self.answer(ticket, GpuOp::Close, Outcome::Done(Vec::new()));
            }
            _ => {}
        }
        if self.client_region(caller).is_none() {
            return self.refuse(ticket, request.op, Errno::NotFound);
        }
        if request.op == GpuOp::Wait {
            return self.wait(ticket, caller, request.body);
        }
        let mut body = ArrayVec::new();
        if body.try_extend_from_slice(request.body).is_err() {
            return self.refuse(ticket, request.op, Errno::LengthOutOfRange);
        }
        let job = Job {
            ticket,
            caller,
            op: request.op,
            body,
        };
        if workers == 0 {
            return self.execute(&job);
        }
        let mut work = self.work.lock();
        if work.jobs.try_reserve(1).is_err() {
            drop(work);
            return self.refuse(job.ticket, job.op, Errno::OutOfMemory);
        }
        work.jobs.push_back(job);
        drop(work);
        self.jobs.notify_one();
    }

    fn open(&self, ticket: u64, caller: ProcId, body: &[u8]) {
        let refuse = |errno| self.refuse(ticket, GpuOp::Open, errno);
        let Ok(open) = decode::<ops::Open>(body) else {
            return refuse(Errno::BadMagic);
        };
        if self.client_region(caller).is_some() {
            return refuse(Errno::Busy);
        }
        let region = match self.channel.map(caller, open.grant) {
            Ok(region) => region,
            Err(errno) => return refuse(errno),
        };
        if !(GPU_REGION_MIN..=GPU_REGION_MAX).contains(&region.len()) {
            return refuse(Errno::LengthOutOfRange);
        }
        // Answered only once its exit is watched: one already gone, or one
        // that cannot be watched, holds nothing.
        if let Err(errno) = self.channel.watch(caller) {
            return refuse(errno);
        }
        let mut state = self.state.lock();
        let Err(at) = state
            .clients
            .binary_search_by_key(&caller, |client| client.proc)
        else {
            drop(state);
            return refuse(Errno::Busy);
        };
        if state.clients.try_reserve(1).is_err() {
            drop(state);
            self.channel.unwatch(caller);
            return refuse(Errno::OutOfMemory);
        }
        state.clients.insert(
            at,
            Client {
                proc: caller,
                region,
                contexts: Vec::new(),
                exports: 0,
            },
        );
        drop(state);
        self.answer(ticket, GpuOp::Open, Outcome::Done(Vec::new()));
    }

    /// Let go of everything `client` held: it exited, or closed. A client
    /// that closed lives on, so its waits are answered as destroyed.
    pub(crate) fn client_exited(&self, client: ProcId) {
        let contexts = {
            let mut state = self.state.lock();
            let Ok(at) = state
                .clients
                .binary_search_by_key(&client, |held| held.proc)
            else {
                return;
            };
            // Its tokens name memory of its own contexts, let go with them.
            state.exports.retain(|exported| exported.client != client);
            state.clients.remove(at).contexts
        };
        for context in contexts {
            // A context destroyed meanwhile may have left its slot to another.
            let held = {
                let mut state = self.state.lock();
                let slot = state.held.get_mut(context.slot().index());
                slot.filter(|slot| slot.as_ref().is_some_and(|held| held.key == context))
                    .and_then(Option::take)
            };
            self.host.destroy_context(context);
            for waiter in held.into_iter().flat_map(|held| held.waits) {
                self.answer(
                    waiter.ticket,
                    GpuOp::Wait,
                    Outcome::Refused(Error::Destroyed),
                );
            }
        }
    }

    /// How many waits the runtime holds.
    #[cfg(test)]
    pub(crate) fn waiting(&self) -> usize {
        let state = self.state.lock();
        state
            .held
            .iter()
            .flatten()
            .map(|held| held.waits.len())
            .sum()
    }

    /// When the loop must next look: the soonest wait's timeout or job's
    /// deadline, on the device's clock.
    pub(crate) fn deadline(&self) -> Option<u64> {
        let waits = self.state.lock().earliest;
        match (waits, self.host.earliest_deadline()) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (one, other) => one.or(other),
        }
    }

    /// Answer what fell due by `now`: overrunning jobs and timed-out waits.
    pub(crate) fn expire(&self, now: u64) {
        self.host.expire(now);
        let mut due = Vec::new();
        {
            let mut state = self.state.lock();
            if state.earliest.is_none_or(|earliest| earliest > now) {
                return;
            }
            let mut earliest = None::<u64>;
            for held in state.held.iter_mut().flatten() {
                held.waits.retain(|waiter| {
                    // Without room to note it, it is answered next time.
                    if waiter.deadline > now || due.try_reserve(1).is_err() {
                        earliest =
                            Some(earliest.map_or(waiter.deadline, |e| e.min(waiter.deadline)));
                        return true;
                    }
                    due.push(waiter.ticket);
                    false
                });
            }
            state.earliest = earliest;
        }
        for ticket in due {
            self.answer(ticket, GpuOp::Wait, Outcome::Refused(Error::TimedOut));
        }
    }

    /// Answer every wait the model's changes since the last look settled.
    pub(crate) fn moved(&self) {
        let Moved {
            changes,
            overflowed,
        } = core::mem::take(&mut *self.watcher.moved.lock());
        let mut settled = Vec::new();
        {
            let mut state = self.state.lock();
            let mut look = |held: &mut Held, timeline: Option<TimelineKey>| {
                held.waits.retain(|waiter| {
                    if timeline.is_some_and(|moved| moved != waiter.timeline) {
                        return true;
                    }
                    let answer = match self.host.reached(held.key, waiter.timeline, waiter.value) {
                        Ok(false) => return true,
                        Ok(true) => Ok(()),
                        Err(error) => Err(error),
                    };
                    if settled.try_reserve(1).is_err() {
                        return true;
                    }
                    settled.push((waiter.ticket, answer));
                    false
                });
            };
            if overflowed {
                for held in state.held.iter_mut().flatten() {
                    look(held, None);
                }
            }
            for (context, timeline) in changes {
                if let Some(Some(held)) = state.held.get_mut(context.slot().index()) {
                    if held.key == context {
                        look(held, timeline);
                    }
                }
            }
        }
        for (ticket, answer) in settled {
            let outcome = match answer {
                Ok(()) => Outcome::Done(Vec::new()),
                Err(error) => Outcome::Refused(error),
            };
            self.answer(ticket, GpuOp::Wait, outcome);
        }
    }

    fn wait(&self, ticket: u64, caller: ProcId, body: &[u8]) {
        let Ok(wait) = decode::<ops::Wait>(body) else {
            return self.answer(ticket, GpuOp::Wait, Outcome::Refused(Error::Malformed));
        };
        let deadline = self
            .host
            .now()
            .saturating_add(wait.timeout.saturating_total_nanos());
        let mut state = self.state.lock();
        let Some(held) = held_mut(&mut state, caller, wait.context) else {
            drop(state);
            return self.answer(ticket, GpuOp::Wait, Outcome::Refused(Error::Stale));
        };
        // Looked at under the runtime's lock, so a change landing after it is
        // seen by the next look at the waits it now joins.
        match self.host.reached(held.key, wait.timeline, wait.value) {
            Ok(false) if held.waits.try_reserve(1).is_ok() => {
                held.waits.push(Waiter {
                    ticket,
                    timeline: wait.timeline,
                    value: wait.value,
                    deadline,
                });
                state.earliest = Some(state.earliest.map_or(deadline, |e| e.min(deadline)));
                drop(state);
                // The loop's wait is bounded by the soonest timeout.
                self.channel.wake();
            }
            Ok(false) => {
                drop(state);
                self.answer(ticket, GpuOp::Wait, Outcome::Refused(Error::OutOfMemory));
            }
            Ok(true) => {
                drop(state);
                self.answer(ticket, GpuOp::Wait, Outcome::Done(Vec::new()));
            }
            Err(error) => {
                drop(state);
                self.answer(ticket, GpuOp::Wait, Outcome::Refused(error));
            }
        }
    }

    fn execute(&self, job: &Job) {
        let outcome = match self.run(job) {
            Ok(outcome) => outcome,
            Err(error) => Outcome::Refused(error),
        };
        self.answer(job.ticket, job.op, outcome);
    }

    fn run(&self, job: &Job) -> Result<Outcome, Error> {
        match job.op {
            GpuOp::Info
            | GpuOp::CreateContext
            | GpuOp::DestroyContext
            | GpuOp::Status
            | GpuOp::MemoryUsed
            | GpuOp::Release => self.run_device(job),
            GpuOp::Allocate
            | GpuOp::Write
            | GpuOp::Read
            | GpuOp::Export
            | GpuOp::Import
            | GpuOp::DropExport
            | GpuOp::CreateBuffer => self.run_memory(job),
            GpuOp::CreateTimeline
            | GpuOp::TimelineValue
            | GpuOp::Signal
            | GpuOp::Reached
            | GpuOp::Upload
            | GpuOp::Submit
            | GpuOp::Discard => self.run_work(job),
            GpuOp::CreateBindSetLayout
            | GpuOp::CreatePipelineLayout
            | GpuOp::CreateShaderModule
            | GpuOp::CreateComputePipeline
            | GpuOp::CreateRenderPipeline => self.run_described(job),
            GpuOp::Open | GpuOp::Close | GpuOp::Wait => Err(Error::Malformed),
            _ => self.run_object(job),
        }
    }

    /// Run a request for the device's description and its contexts.
    fn run_device(&self, job: &Job) -> Result<Outcome, Error> {
        let caller = job.caller;
        let body = &job.body[..];
        Ok(match job.op {
            GpuOp::Info => {
                let ops::Spanned { span } = decode(body)?;
                let mut bytes = Vec::new();
                put_info(&mut Writer::new(&mut bytes), self.host.info())?;
                let len = u32::try_from(bytes.len()).map_err(|_| Error::TooLarge)?;
                if len > span.len {
                    return Err(Error::TooLarge);
                }
                self.write_region(caller, span, &bytes)?;
                done(&ops::Written { len })?
            }
            GpuOp::CreateContext => {
                let ops::CreateContext { memory_limit } = decode(body)?;
                done(&ops::OfContext {
                    context: self.create_context(caller, memory_limit)?,
                })?
            }
            GpuOp::DestroyContext => {
                let ops::OfContext { context } = decode(body)?;
                self.destroy_context(caller, context)?;
                none()
            }
            GpuOp::Status => {
                let ops::OfContext { context } = decode(body)?;
                self.host.status(self.context(caller, context)?)?;
                none()
            }
            GpuOp::MemoryUsed => {
                let ops::OfContext { context } = decode(body)?;
                let used = self.host.memory_used(self.context(caller, context)?);
                done(&ops::Used { used })?
            }
            GpuOp::Release => {
                let ops::Release { context, object } = decode(body)?;
                self.host.release(self.context(caller, context)?, object);
                none()
            }
            _ => return Err(Error::Malformed),
        })
    }

    /// Run a request for memory and buffers.
    fn run_memory(&self, job: &Job) -> Result<Outcome, Error> {
        let caller = job.caller;
        let body = &job.body[..];
        Ok(match job.op {
            GpuOp::Allocate => {
                let ops::Allocate { context, desc } = decode(body)?;
                let key = self.host.allocate(self.context(caller, context)?, &desc)?;
                done(&ops::Made::of(key))?
            }
            GpuOp::Write => {
                let access: ops::HostAccess = decode(body)?;
                let key = self.context(caller, access.context)?;
                let bytes = self.read_region(caller, access.span)?;
                self.host
                    .write(key, access.allocation, access.offset, &bytes)?;
                none()
            }
            GpuOp::Read => {
                let access: ops::HostAccess = decode(body)?;
                let key = self.context(caller, access.context)?;
                let len = usize::try_from(access.span.len).map_err(|_| Error::TooLarge)?;
                let mut bytes = tairix_util::fallible::zeroed(len).ok_or(Error::OutOfMemory)?;
                self.host
                    .read(key, access.allocation, access.offset, &mut bytes)?;
                self.write_region(caller, access.span, &bytes)?;
                none()
            }
            GpuOp::Export => {
                let ops::Export {
                    context,
                    allocation,
                } = decode(body)?;
                done(&ops::Token {
                    token: self.export(caller, context, allocation)?,
                })?
            }
            GpuOp::Import => {
                let ops::Import { context, token } = decode(body)?;
                let key = self.context(caller, context)?;
                let export = self.take_export(token).ok_or(Error::Stale)?;
                let imported = self.host.import(key, export)?;
                done(&ops::Imported {
                    allocation: imported.allocation,
                    desc: imported.desc,
                })?
            }
            GpuOp::DropExport => {
                let ops::Token { token } = decode(body)?;
                if let Some(export) = self.take_export(token) {
                    self.host.drop_export(export);
                }
                none()
            }
            GpuOp::CreateBuffer => {
                let ops::CreateBuffer {
                    context,
                    allocation,
                    desc,
                } = decode(body)?;
                let key =
                    self.host
                        .create_buffer(self.context(caller, context)?, allocation, &desc)?;
                done(&ops::Made::of(key))?
            }
            _ => return Err(Error::Malformed),
        })
    }

    /// Run a request for timelines and work.
    fn run_work(&self, job: &Job) -> Result<Outcome, Error> {
        let caller = job.caller;
        let body = &job.body[..];
        Ok(match job.op {
            GpuOp::CreateTimeline => {
                let ops::CreateTimeline { context, initial } = decode(body)?;
                let key = self
                    .host
                    .create_timeline(self.context(caller, context)?, initial)?;
                done(&ops::Made::of(key))?
            }
            GpuOp::TimelineValue => {
                let ops::OfTimeline { context, timeline } = decode(body)?;
                let value = self
                    .host
                    .timeline_value(self.context(caller, context)?, timeline)?;
                done(&ops::Value { value })?
            }
            GpuOp::Signal => {
                let ops::AtValue {
                    context,
                    timeline,
                    value,
                } = decode(body)?;
                self.host
                    .signal(self.context(caller, context)?, timeline, value)?;
                none()
            }
            GpuOp::Reached => {
                let ops::AtValue {
                    context,
                    timeline,
                    value,
                } = decode(body)?;
                let truth = self
                    .host
                    .reached(self.context(caller, context)?, timeline, value)?;
                done(&ops::Truth { truth })?
            }
            GpuOp::Upload => {
                let upload: ops::Upload = decode(body)?;
                self.upload(caller, &upload)?;
                none()
            }
            GpuOp::Submit => {
                let submit: ops::Submit = decode(body)?;
                match self.submit(caller, &submit)? {
                    Ok(fence) => done(&fence)?,
                    Err(refused) => Outcome::SubmitRefused(refused),
                }
            }
            GpuOp::Discard => {
                let ops::OfContext { context } = decode(body)?;
                let staged = self.take_staged(caller, context)?;
                drop(staged);
                none()
            }
            _ => return Err(Error::Malformed),
        })
    }

    /// Run a request for the objects pipelines are made of.
    fn run_object(&self, job: &Job) -> Result<Outcome, Error> {
        let caller = job.caller;
        let body = &job.body[..];
        Ok(match job.op {
            GpuOp::CreateBindSet => {
                let ops::CreateBindSet {
                    context,
                    layout,
                    span,
                } = decode(body)?;
                let key = self.context(caller, context)?;
                let bytes = self.read_region(caller, span)?;
                done(&ops::Made::of(self.create_bind_set(key, layout, &bytes)?))?
            }
            GpuOp::ImageRequirements => {
                let ops::ImageRequirements { context, desc } = decode(body)?;
                let requirements = self
                    .host
                    .image_requirements(self.context(caller, context)?, &desc)?;
                done(&ops::Requirements { requirements })?
            }
            GpuOp::CreateImage => {
                let ops::CreateImage {
                    context,
                    allocation,
                    offset,
                    desc,
                } = decode(body)?;
                let key = self.host.create_image(
                    self.context(caller, context)?,
                    (allocation, offset),
                    &desc,
                )?;
                done(&ops::Made::of(key))?
            }
            GpuOp::CreateView => {
                let ops::CreateView {
                    context,
                    image,
                    desc,
                } = decode(body)?;
                let key = self
                    .host
                    .create_view(self.context(caller, context)?, image, &desc)?;
                done(&ops::Made::of(key))?
            }
            GpuOp::CreateBufferView => {
                let ops::CreateBufferView {
                    context,
                    buffer,
                    desc,
                } = decode(body)?;
                let key =
                    self.host
                        .create_buffer_view(self.context(caller, context)?, buffer, &desc)?;
                done(&ops::Made::of(key))?
            }
            GpuOp::CreateSampler => {
                let ops::CreateSampler { context, desc } = decode(body)?;
                let key = self
                    .host
                    .create_sampler(self.context(caller, context)?, &desc)?;
                done(&ops::Made::of(key))?
            }
            _ => return Err(Error::Malformed),
        })
    }

    /// Run a request for an object whose description lies in the region.
    fn run_described(&self, job: &Job) -> Result<Outcome, Error> {
        let caller = job.caller;
        let body = &job.body[..];
        Ok(match job.op {
            GpuOp::CreateBindSetLayout => {
                let ops::Described { context, span } = decode(body)?;
                let key = self.context(caller, context)?;
                let bytes = self.read_region(caller, span)?;
                let mut reader = Reader::new(&bytes);
                let bindings: Vec<LayoutBinding> = reader.list(MAX_BINDINGS, Wire::take)?;
                reader.finish()?;
                done(&ops::Made::of(
                    self.host.create_bind_set_layout(key, &bindings)?,
                ))?
            }
            GpuOp::CreatePipelineLayout => {
                let ops::CreatePipelineLayout {
                    context,
                    push_constants,
                    span,
                } = decode(body)?;
                let key = self.context(caller, context)?;
                let bytes = self.read_region(caller, span)?;
                let mut reader = Reader::new(&bytes);
                let most = self.host.info().limits.max_bind_sets as usize;
                let sets: Vec<BindSetLayoutKey> = reader.list(most, Wire::take)?;
                reader.finish()?;
                done(&ops::Made::of(self.host.create_pipeline_layout(
                    key,
                    &sets,
                    push_constants,
                )?))?
            }
            GpuOp::CreateShaderModule => {
                let ops::Described { context, span } = decode(body)?;
                let key = self.context(caller, context)?;
                let bytes = self.read_region(caller, span)?;
                // Admitted again where it arrives: the client's admission is
                // not the device's.
                let module = Module::decode(&bytes).map_err(|_| Error::Malformed)?;
                let admitted = Admitted::new(module).map_err(|_| Error::Malformed)?;
                let admitted = crate::shared(admitted)?;
                done(&ops::Made::of(
                    self.host.create_shader_module(key, &admitted)?,
                ))?
            }
            GpuOp::CreateComputePipeline => {
                let ops::CreateComputePipeline {
                    context,
                    module,
                    layout,
                    span,
                } = decode(body)?;
                let key = self.context(caller, context)?;
                let bytes = self.read_region(caller, span)?;
                let mut reader = Reader::new(&bytes);
                let entry = take_name(&mut reader)?;
                let specialization: Vec<SpecValue> = reader.list(MAX_SPECIALIZED, Wire::take)?;
                reader.finish()?;
                done(&ops::Made::of(self.host.create_compute_pipeline(
                    key,
                    (module, &entry, &specialization),
                    layout,
                )?))?
            }
            GpuOp::CreateRenderPipeline => {
                let ops::Described { context, span } = decode(body)?;
                let key = self.context(caller, context)?;
                let bytes = self.read_region(caller, span)?;
                let mut reader = Reader::new(&bytes);
                let owned = OwnedRender::take(&mut reader)?;
                reader.finish()?;
                let made =
                    owned.with_request(|request| self.host.create_render_pipeline(key, request))?;
                done(&ops::Made::of(made))?
            }
            _ => return Err(Error::Malformed),
        })
    }

    /// Answer `ticket`'s op with `outcome`.
    fn answer(&self, ticket: u64, op: GpuOp, outcome: Outcome) {
        let mut detail = Vec::new();
        let status = match outcome {
            Outcome::Done(body) => {
                detail = body;
                Ok(GpuStatus::Ok)
            }
            Outcome::Refused(error) => {
                put_error(&mut Writer::new(&mut detail), error).map(GpuStatus::Device)
            }
            Outcome::SubmitRefused(refused) => {
                put_submit_error(&mut Writer::new(&mut detail), refused).map(GpuStatus::Device)
            }
        };
        // A detail that cannot be had is answered without it.
        let status = status.unwrap_or_else(|_| {
            detail.clear();
            GpuStatus::Device(GpuError::OutOfMemory)
        });
        let mut frame = [0u8; GPU_MAX_REPLY];
        if let Ok(len) = encode_reply(op, status, &detail, &mut frame) {
            self.channel.reply(ticket, &frame[..len]);
        }
    }

    fn refuse(&self, ticket: u64, op: GpuOp, errno: Errno) {
        let mut frame = [0u8; GPU_MAX_REPLY];
        if let Ok(len) = encode_reply(op, GpuStatus::Refused(errno), &[], &mut frame) {
            self.channel.reply(ticket, &frame[..len]);
        }
    }

    fn client_region(&self, caller: ProcId) -> Option<Arc<dyn Region>> {
        let state = self.state.lock();
        state
            .clients
            .binary_search_by_key(&caller, |client| client.proc)
            .ok()
            .and_then(|at| state.clients.get(at))
            .map(|client| Arc::clone(&client.region))
    }

    /// The context `id` names among `caller`'s own.
    fn context(&self, caller: ProcId, id: ContextId) -> Result<ContextKey, Error> {
        let mut state = self.state.lock();
        held_mut(&mut state, caller, id)
            .map(|held| held.key)
            .ok_or(Error::Stale)
    }

    /// A copy of the bytes `span` names in `caller`'s region.
    fn read_region(&self, caller: ProcId, span: RegionSpan) -> Result<Vec<u8>, Error> {
        let region = self.client_region(caller).ok_or(Error::Stale)?;
        let range = span.within(region.len()).map_err(|_| Error::OutOfRange)?;
        let mut bytes = tairix_util::fallible::zeroed(range.len()).ok_or(Error::OutOfMemory)?;
        region.read(range.start, &mut bytes);
        Ok(bytes)
    }

    fn write_region(&self, caller: ProcId, span: RegionSpan, bytes: &[u8]) -> Result<(), Error> {
        let region = self.client_region(caller).ok_or(Error::Stale)?;
        let range = span.within(region.len()).map_err(|_| Error::OutOfRange)?;
        if bytes.len() > range.len() {
            return Err(Error::TooLarge);
        }
        region.write(range.start, bytes);
        Ok(())
    }

    fn create_context(&self, caller: ProcId, limit: Option<u64>) -> Result<ContextId, Error> {
        {
            let state = self.state.lock();
            let client = state
                .clients
                .iter()
                .find(|client| client.proc == caller)
                .ok_or(Error::Stale)?;
            if client.contexts.len() >= MAX_CLIENT_CONTEXTS {
                return Err(Error::TooMany);
            }
        }
        let limit = Some(limit.unwrap_or(self.ceiling).min(self.ceiling));
        let key = self.host.create_context(limit)?;
        let filed = (|| {
            let mut state = self.state.lock();
            let index = key.slot().index();
            if state.held.len() <= index {
                let more = index + 1 - state.held.len();
                state
                    .held
                    .try_reserve(more)
                    .map_err(|_| Error::OutOfMemory)?;
                state.held.resize_with(index + 1, || None);
            }
            let client = state
                .clients
                .iter_mut()
                .find(|client| client.proc == caller)
                .ok_or(Error::Stale)?;
            // Two requests at once may both have passed the look above.
            if client.contexts.len() >= MAX_CLIENT_CONTEXTS {
                return Err(Error::TooMany);
            }
            client
                .contexts
                .try_reserve(1)
                .map_err(|_| Error::OutOfMemory)?;
            client.contexts.push(key);
            if let Some(slot) = state.held.get_mut(index) {
                *slot = Some(Held {
                    key,
                    client: caller,
                    staged: Staged::default(),
                    waits: Vec::new(),
                });
            }
            Ok(())
        })();
        match filed {
            Ok(()) => Ok(key.slot().to_bits()),
            Err(error) => {
                self.host.destroy_context(key);
                Err(error)
            }
        }
    }

    fn destroy_context(&self, caller: ProcId, id: ContextId) -> Result<(), Error> {
        let (key, waits) = {
            let mut state = self.state.lock();
            let held = held_mut(&mut state, caller, id).ok_or(Error::Stale)?;
            let key = held.key;
            let waits = core::mem::take(&mut held.waits);
            if let Some(slot) = state.held.get_mut(key.slot().index()) {
                *slot = None;
            }
            if let Some(client) = state
                .clients
                .iter_mut()
                .find(|client| client.proc == caller)
            {
                client.contexts.retain(|&context| context != key);
            }
            (key, waits)
        };
        self.host.destroy_context(key);
        for waiter in waits {
            self.answer(
                waiter.ticket,
                GpuOp::Wait,
                Outcome::Refused(Error::Destroyed),
            );
        }
        Ok(())
    }

    fn export(
        &self,
        caller: ProcId,
        id: ContextId,
        allocation: crate::model::AllocationKey,
    ) -> Result<ExportToken, Error> {
        let key = self.context(caller, id)?;
        let token = self.channel.token().map_err(|_| Error::OutOfMemory)?;
        let export = self.host.export(key, allocation)?;
        let filed = (|| {
            let mut state = self.state.lock();
            let client = state
                .clients
                .iter_mut()
                .find(|client| client.proc == caller)
                .ok_or(Error::Stale)?;
            if client.exports >= MAX_CLIENT_EXPORTS {
                return Err(Error::TooMany);
            }
            client.exports += 1;
            let at = state
                .exports
                .binary_search_by_key(&token, |exported| exported.token)
                .unwrap_or_else(|at| at);
            state
                .exports
                .try_reserve(1)
                .map_err(|_| Error::OutOfMemory)?;
            state.exports.insert(
                at,
                Exported {
                    token,
                    export,
                    client: caller,
                },
            );
            Ok(())
        })();
        match filed {
            Ok(()) => Ok(token),
            Err(error) => {
                self.host.drop_export(export);
                Err(error)
            }
        }
    }

    /// The export `token` names, spent.
    fn take_export(&self, token: ExportToken) -> Option<ExportId> {
        let mut state = self.state.lock();
        let at = state
            .exports
            .binary_search_by_key(&token, |exported| exported.token)
            .ok()?;
        let exported = state.exports.remove(at);
        if let Some(client) = state
            .clients
            .iter_mut()
            .find(|client| client.proc == exported.client)
        {
            client.exports = client.exports.saturating_sub(1);
        }
        Some(exported.export)
    }

    fn create_bind_set(
        &self,
        context: ContextKey,
        layout: BindSetLayoutKey,
        bytes: &[u8],
    ) -> Result<crate::model::BindSetKey, Error> {
        let most = self.host.info().limits.max_bind_set_descriptors as usize;
        let mut reader = Reader::new(bytes);
        let entries = reader.list(MAX_BINDINGS, |reader| {
            let binding = u32::take(reader)?;
            let count = usize::try_from(u32::take(reader)?).map_err(|_| Error::Malformed)?;
            Ok((binding, count))
        })?;
        let resources: Vec<Named> = reader.list(most, Wire::take)?;
        reader.finish()?;
        self.host.create_bind_set(
            context,
            &SetRequest {
                layout,
                entries: &entries,
                resources: &resources,
            },
        )
    }

    fn upload(&self, caller: ProcId, upload: &ops::Upload) -> Result<(), Error> {
        let key = self.context(caller, upload.context)?;
        let bytes = self.read_region(caller, upload.span)?;
        let room = {
            let mut state = self.state.lock();
            let held = held_mut(&mut state, caller, upload.context).ok_or(Error::Stale)?;
            MAX_SUBMISSION_COMMANDS.saturating_sub(held.staged.counted)
        };
        // What the commands keep queued until submitted is charged to the
        // context, as the bytes they arrived as.
        let charge = self.host.charge(key, bytes.len() as u64)?;
        let arrival = Arriving {
            host: &*self.host,
            context: key,
        };
        let mut commands = Vec::new();
        let mut leading = Vec::new();
        let mut reader = Reader::new(&bytes);
        let counted = take_commands(&mut reader, &arrival, (&mut commands, &mut leading), room)?;
        reader.finish()?;
        let mut state = self.state.lock();
        let held = held_mut(&mut state, caller, upload.context).ok_or(Error::Stale)?;
        let staged = &mut held.staged;
        let total = staged
            .counted
            .checked_add(counted)
            .filter(|&total| total <= MAX_SUBMISSION_COMMANDS)
            .ok_or(Error::TooMany)?;
        if upload.last && staged.buffers.len() >= MAX_SUBMISSION_BUFFERS {
            return Err(Error::TooMany);
        }
        staged
            .open
            .try_reserve(commands.len())
            .map_err(|_| Error::OutOfMemory)?;
        staged
            .charges
            .try_reserve(1)
            .map_err(|_| Error::OutOfMemory)?;
        if upload.last {
            staged
                .buffers
                .try_reserve(1)
                .map_err(|_| Error::OutOfMemory)?;
        }
        if !leading.is_empty() {
            match staged.open.last_mut() {
                Some(Command::Pass(pass)) => pass.extend(&mut leading)?,
                _ => return Err(Error::Malformed),
            }
        }
        staged.open.append(&mut commands);
        staged.charges.push(charge);
        staged.counted = total;
        if upload.last {
            let buffer = core::mem::take(&mut staged.open);
            staged.buffers.push(buffer);
        }
        Ok(())
    }

    fn take_staged(&self, caller: ProcId, id: ContextId) -> Result<Staged, Error> {
        let mut state = self.state.lock();
        let held = held_mut(&mut state, caller, id).ok_or(Error::Stale)?;
        Ok(core::mem::take(&mut held.staged))
    }

    fn submit(
        &self,
        caller: ProcId,
        submit: &ops::Submit,
    ) -> Result<Result<ops::Fence, SubmitError>, Error> {
        let key = self.context(caller, submit.context)?;
        // The uploads are spent whatever the answer.
        let staged = self.take_staged(caller, submit.context)?;
        let bytes = self.read_region(caller, submit.span)?;
        let mut reader = Reader::new(&bytes);
        let point = |reader: &mut Reader<'_>| Ok((TimelineKey::take(reader)?, u64::take(reader)?));
        let waits = reader.list(MAX_SUBMISSION_POINTS, point)?;
        let signals = reader.list(MAX_SUBMISSION_POINTS, point)?;
        reader.finish()?;
        if !staged.open.is_empty() {
            return Err(Error::Malformed);
        }
        let mut buffers: ArrayVec<&[Command], MAX_SUBMISSION_BUFFERS> = ArrayVec::new();
        for buffer in &staged.buffers {
            buffers
                .try_push(buffer.as_slice())
                .map_err(|_| Error::TooMany)?;
        }
        let request = Request {
            queue: usize::try_from(submit.queue).map_err(|_| Error::NoSuchQueue)?,
            buffers: &buffers,
            waits: &waits,
            signals: &signals,
            span: submit.deadline.saturating_total_nanos(),
        };
        Ok(self
            .host
            .submit(key, &request)
            .map(|(timeline, value)| ops::Fence { timeline, value }))
    }
}

/// What uploaded commands ask of the context they arrive for.
struct Arriving<'a> {
    host: &'a dyn Served,
    context: ContextKey,
}

impl Arrival for Arriving<'_> {
    fn record_update(&self, bytes: Vec<u8>) -> Result<Arc<Recorded>, Error> {
        self.host.record_update(self.context, bytes)
    }

    fn image_desc(&self, image: ImageKey) -> Option<ImageDesc> {
        self.host.image_desc(self.context, image)
    }
}

/// The context `id` names among `caller`'s own.
fn held_mut(state: &mut State, caller: ProcId, id: ContextId) -> Option<&mut Held> {
    let slot = tairix_collections::SlotKey::from_bits(id)?;
    let held = state.held.get_mut(slot.index())?.as_mut()?;
    (held.key.slot() == slot && held.client == caller).then_some(held)
}

/// A body read whole.
fn decode<T: Wire>(body: &[u8]) -> Result<T, Error> {
    let mut reader = Reader::new(body);
    let value = T::take(&mut reader)?;
    reader.finish()?;
    Ok(value)
}

/// `value`'s body.
fn done<T: Wire>(value: &T) -> Result<Outcome, Error> {
    let mut body = Vec::new();
    value.put(&mut Writer::new(&mut body))?;
    if body.len() > GPU_MAX_BODY {
        return Err(Error::TooLarge);
    }
    Ok(Outcome::Done(body))
}

const fn none() -> Outcome {
    Outcome::Done(Vec::new())
}

#[cfg(test)]
#[path = "serve_tests.rs"]
mod tests;
