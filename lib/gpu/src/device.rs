//! The seam: a device, the contexts made on it, and what a context holds and
//! runs.

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::fmt;
use core::sync::atomic::{AtomicBool, Ordering};

use tairix_abi::time::MonotonicClock;
use tairix_abi::Duration64;
use tairix_collections::Key;
use tairix_inline::ArrayVec;
use tairix_parallel::JobRunner;
use tairix_rt::sync::{Condvar, Mutex};

use crate::command::{Command, CopyRegion, Recorded};
use crate::fair::{Fair, FairGuard};
use crate::info::{BufferUsage, DeviceInfo};
use crate::memory::{self, Memory, MemoryUse};
use crate::model::{
    AllocationKey, BufferKey, ContextKey, Model, Pick, Point, Request, TimelineKey, Woken,
};
use crate::software::texel::Srgb;
use crate::software::{self, Outcome, Spawn};
use crate::Loss;
use crate::{
    Error, SubmitError, MAX_COPY_REGIONS, MAX_SUBMISSION_BUFFERS, MAX_SUBMISSION_COMMANDS,
    MAX_SUBMISSION_POINTS, MAX_UPDATE_BYTES,
};

/// What every handle of one device shares.
pub(crate) struct Shared {
    /// Had in turn, so no thread that locks it again at once keeps another
    /// out.
    pub(crate) model: Fair<Model>,
    pub(crate) info: DeviceInfo,
    pub(crate) clock: Arc<dyn MonotonicClock + Send + Sync>,
    pub(crate) runner: Arc<dyn JobRunner + Send>,
    pub(crate) srgb: Srgb,
    wake: Arc<Wake>,
}

/// How work reaches the device's own worker: a pending flag it parks on,
/// whether the device is gone, and whether there is a worker at all.
pub(crate) struct Wake {
    state: Mutex<WakeState>,
    ready: Condvar,
    /// Set once the worker's thread is started; until then, and for good where
    /// none can be, work runs on the thread whose act made it ready.
    running: AtomicBool,
}

#[derive(Default)]
struct WakeState {
    pending: bool,
    closed: bool,
    /// Whether the worker waits to be poked, so a poke must wake it.
    parked: bool,
}

impl Wake {
    fn new() -> Self {
        Self {
            state: Mutex::new(WakeState::default()),
            ready: Condvar::new(),
            running: AtomicBool::new(false),
        }
    }

    fn poke(&self, close: bool) {
        let mut state = self.state.lock();
        state.pending = true;
        state.closed |= close;
        let parked = state.parked;
        drop(state);
        if parked {
            self.ready.notify_one();
        }
    }

    /// Park until poked, answering whether the device is still there.
    fn park(&self) -> bool {
        let mut state = self.state.lock();
        while !state.pending && !state.closed {
            state.parked = true;
            #[cfg(test)]
            waits::note(state.parked);
            state = self.ready.wait(state);
        }
        state.parked = false;
        state.pending = false;
        !state.closed
    }
}

impl Drop for Shared {
    fn drop(&mut self) {
        self.wake.poke(true);
    }
}

/// The device's worker: it runs every job that is ready, then parks holding
/// only a weak reference, so an idle device it serves is never kept alive by
/// it; a job it runs keeps the device until that job ends.
fn work(device: &alloc::sync::Weak<Shared>, wake: &Wake) {
    loop {
        let Some(shared) = device.upgrade() else {
            return;
        };
        shared.run_ready(shared.model.lock());
        drop(shared);
        if !wake.park() {
            return;
        }
    }
}

impl Shared {
    /// Take `item` out of the context's table `pick` names, letting it go
    /// once the device's lock is: it may hold the last of a memory.
    pub(crate) fn release<K: Key, V>(&self, context: ContextKey, pick: Pick<K, V>, item: K) {
        let released = self.model.lock().release(context, pick, item);
        drop(released);
    }

    /// Have the work a change just made ready run: on the device's worker,
    /// or on this thread where it has none, or, where its engine is already
    /// running work, on the thread running it, which takes it next.
    fn drive<'a>(&'a self, model: FairGuard<'a, Model>) {
        if !model.has_ready() {
            return;
        }
        if self.wake.running.load(Ordering::Acquire) {
            drop(model);
            self.wake.poke(false);
        } else {
            self.run_ready(model);
        }
    }

    /// Run every job the device has room for and that is ready, until none
    /// is.
    fn run_ready<'a>(&'a self, mut model: FairGuard<'a, Model>) {
        let mut woken = Woken::new();
        let now = || self.clock.now_ns();
        loop {
            let (next, pruned) = model.dispatch(&now);
            drop(model);
            drop(pruned);
            notify(&mut woken);
            let Some(mut dispatch) = next else {
                return;
            };
            let outcome = software::run(
                &dispatch.ops.ops,
                dispatch.deadline,
                self,
                &mut dispatch.workspaces,
            );
            model = self.model.lock();
            let (engine, job) = (dispatch.engine, dispatch.job);
            match outcome {
                Outcome::Done => woken = model.complete(engine, job, None),
                Outcome::Fault => woken = model.complete(engine, job, Some(Loss::Fault)),
                Outcome::OutOfMemory => {
                    woken = model.complete(engine, job, Some(Loss::OutOfMemory));
                }
                Outcome::Overran => model.reset(engine, now(), &|_| true),
            }
            let workspaces = core::mem::take(&mut dispatch.workspaces);
            let given = model.give_back(workspaces);
            let discarded = model.take_discarded();
            drop(model);
            // The job may have held the last of a memory, so it goes with the
            // lock let go; the next dispatch fits the pool after it.
            drop((dispatch, given, discarded));
            model = self.model.lock();
        }
    }

    /// Wait until `timeline` reaches `value`, the context is lost or
    /// released, or `timeout` elapses.
    fn wait(
        &self,
        context: ContextKey,
        timeline: TimelineKey,
        value: u64,
        timeout: Duration64,
    ) -> Result<(), Error> {
        let limit = self
            .clock
            .now_ns()
            .saturating_add(timeout.saturating_total_nanos());
        let mut model = self.model.lock();
        loop {
            let wake = match model.point(context, timeline, value)? {
                Point::Reached => return Ok(()),
                Point::Pending(wake) => wake,
            };
            let now = self.clock.now_ns();
            if now >= limit {
                return Err(Error::TimedOut);
            }
            model = self
                .model
                .wait_timeout(model, &wake, Duration64::from_nanos(limit - now));
        }
    }
}

fn notify(woken: &mut Woken) {
    for wake in woken.iter() {
        wake.notify_all();
    }
    woken.clear();
}

/// Whether a handle of `shared`'s device and `context` belongs where it is
/// used.
pub(crate) fn owned(
    shared: &Arc<Shared>,
    context: ContextKey,
    by: &Arc<Shared>,
    key: ContextKey,
) -> Result<(), Error> {
    if Arc::ptr_eq(shared, by) && context == key {
        Ok(())
    } else {
        Err(Error::Foreign)
    }
}

/// A device work is made on: a GPU, or the CPU standing in for one.
pub struct Device {
    shared: Arc<Shared>,
}

impl Device {
    /// A device over `model`, its work run on a thread `threads` starts or,
    /// where none can be started, on the thread whose act made it ready.
    pub(crate) fn from_model(
        model: Model,
        clock: Arc<dyn MonotonicClock + Send + Sync>,
        runner: Arc<dyn JobRunner + Send>,
        threads: &dyn Spawn,
    ) -> Result<Self, Error> {
        let mut model = model;
        model.set_width(runner.width());
        let info = model.info().clone();
        let wake = crate::shared(Wake::new())?;
        let shared = crate::shared(Shared {
            model: Fair::new(model),
            info,
            clock,
            runner,
            srgb: Srgb::new(),
            wake: Arc::clone(&wake),
        })?;
        let device = Arc::downgrade(&shared);
        let parked = Arc::clone(&wake);
        let body = alloc::boxed::Box::try_new(move || work(&device, &parked))
            .map_err(|_| Error::OutOfMemory)?;
        if threads.spawn(body) {
            wake.running.store(true, Ordering::Release);
        }
        Ok(Self { shared })
    }

    /// What the device is.
    #[must_use]
    pub fn info(&self) -> &DeviceInfo {
        &self.shared.info
    }

    /// A context: its own memory, objects and queues, which no other context
    /// can reach.
    ///
    /// # Errors
    ///
    /// [`Error::OverLimit`] where its own state would pass its limit, then
    /// [`Error::HeapFull`] where that and the device's room for it would pass
    /// the heap, and [`Error::OutOfMemory`] where the host cannot hold it. The
    /// room the device made for it ahead of a refusal stays, charged to the
    /// heap, for the next.
    pub fn create_context(&self, desc: &ContextDesc) -> Result<Context, Error> {
        let key = self.shared.model.lock().create_context(desc.memory_limit)?;
        Ok(Context {
            shared: Arc::clone(&self.shared),
            key,
        })
    }
}

impl fmt::Debug for Device {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Device")
            .field("info", &self.shared.info)
            .finish_non_exhaustive()
    }
}

/// How a context is made.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct ContextDesc {
    /// The most bytes it may take, as [`Context`] says what is charged, or,
    /// when `None`, as much as the device's heaps hold.
    pub memory_limit: Option<u64>,
}

/// The unit of isolation: memory, objects and queues no other context can
/// name. Dropping it releases all of them; handles it made then refuse every
/// request with [`Error::Destroyed`].
///
/// # Memory
///
/// Every request that makes the device hold memory — an allocation, an
/// object, room in one of the context's tables or queues, a recorded update,
/// a submission — is charged to the context's limit. It is refused with
/// [`Error::OverLimit`] past the limit, [`Error::HeapFull`] past its heap's
/// size, and [`Error::OutOfMemory`] where, within both, the host refuses the
/// memory; a lost context refuses it with [`Error::Lost`], a released one
/// with [`Error::Destroyed`]. A refusal changes nothing but the room a table
/// or queue made ahead of it, which stays, charged, for the next.
pub struct Context {
    pub(crate) shared: Arc<Shared>,
    pub(crate) key: ContextKey,
}

impl Context {
    /// Whether the context still takes work.
    ///
    /// # Errors
    ///
    /// [`Error::Lost`] once it is lost.
    pub fn status(&self) -> Result<(), Error> {
        self.shared.model.lock().status(self.key)
    }

    /// What its memory limit is spent on: its allocations, and the host
    /// memory its own state, its objects, its tables' room and its queued
    /// work keep on the device, each for as long as anything holds it.
    #[must_use]
    pub fn memory_used(&self) -> MemoryUse {
        self.shared
            .model
            .lock()
            .memory_taken(self.key)
            .unwrap_or_default()
    }

    /// `desc.size` zeroed bytes of memory type `desc.memory_type`.
    ///
    /// # Errors
    ///
    /// [`Error::OverLimit`] past the context's limit, [`Error::HeapFull`] past
    /// the heap's size, and [`Error::OutOfMemory`] when, within both, the
    /// memory cannot be had; [`Error::Empty`], [`Error::TooLarge`] and
    /// [`Error::NoSuchMemoryType`] for what no device could allocate.
    pub fn allocate(&self, desc: &AllocationDesc) -> Result<Allocation, Error> {
        let (charge, overhead) =
            self.shared
                .model
                .lock()
                .charge_allocation(self.key, desc.size, desc.memory_type)?;
        // Made between two holds of the device's lock, as zeroing it takes as
        // long as it is large; its charges stand for it meanwhile.
        let made = Memory::zeroed(desc.size, charge, overhead).and_then(crate::shared);
        let memory = match made {
            Ok(memory) => memory,
            Err(error) => {
                self.shared.model.lock().unreserve_allocation(self.key);
                return Err(error);
            }
        };
        let filed =
            self.shared
                .model
                .lock()
                .file_allocation(self.key, memory, desc.memory_type, desc.size);
        let key = filed.map_err(|(error, refused)| {
            drop(refused);
            error
        })?;
        Ok(Allocation {
            shared: Arc::clone(&self.shared),
            context: self.key,
            key,
            size: desc.size,
            memory_type: desc.memory_type,
        })
    }

    /// Take into this context memory another context of the same device
    /// exported.
    ///
    /// # Errors
    ///
    /// [`Error::Foreign`] for memory of another device, and those of every
    /// request that makes memory ([`Context#memory`]).
    pub fn import(&self, export: Export) -> Result<Allocation, Error> {
        if !Arc::ptr_eq(&self.shared, &export.shared) {
            return Err(Error::Foreign);
        }
        let Export {
            memory,
            memory_type,
            size,
            ..
        } = export;
        let filed = self
            .shared
            .model
            .lock()
            .file_memory(self.key, memory, memory_type, size);
        let key = filed.map_err(|(error, refused)| {
            drop(refused);
            error
        })?;
        Ok(Allocation {
            shared: Arc::clone(&self.shared),
            context: self.key,
            key,
            size,
            memory_type,
        })
    }

    /// A buffer over part of one of this context's allocations.
    ///
    /// # Errors
    ///
    /// [`Error::Foreign`] for another context's allocation; [`Error::Empty`],
    /// [`Error::Usage`], [`Error::Misaligned`] and [`Error::OutOfRange`] for
    /// a buffer the allocation cannot hold as asked; and those of every
    /// request that makes memory ([`Context#memory`]).
    pub fn create_buffer(
        &self,
        allocation: &Allocation,
        desc: &BufferDesc,
    ) -> Result<Buffer, Error> {
        owned(
            &allocation.shared,
            allocation.context,
            &self.shared,
            self.key,
        )?;
        let key = self.shared.model.lock().create_buffer(
            self.key,
            allocation.key,
            desc.offset,
            desc.size,
            desc.usage,
        )?;
        Ok(Buffer {
            shared: Arc::clone(&self.shared),
            context: self.key,
            key,
            size: desc.size,
            usage: desc.usage,
        })
    }

    /// A timeline starting at `initial`.
    ///
    /// # Errors
    ///
    /// Those of every request that makes memory ([`Context#memory`]).
    pub fn create_timeline(&self, initial: u64) -> Result<Timeline, Error> {
        let key = self
            .shared
            .model
            .lock()
            .create_timeline(self.key, initial)?;
        Ok(Timeline {
            shared: Arc::clone(&self.shared),
            context: self.key,
            key,
        })
    }

    /// Queue `index` of family `family`.
    ///
    /// # Errors
    ///
    /// [`Error::NoSuchQueue`] for one the device does not have.
    pub fn queue(&self, family: u32, index: u32) -> Result<Queue, Error> {
        let slot = self.shared.info.queue_slot(family, index)?;
        Ok(Queue {
            shared: Arc::clone(&self.shared),
            context: self.key,
            slot,
            family,
            index,
        })
    }

    /// An empty command buffer for this context's queues.
    #[must_use]
    pub fn command_buffer(&self) -> CommandBuffer {
        CommandBuffer {
            shared: Arc::clone(&self.shared),
            context: self.key,
            commands: Vec::new(),
        }
    }
}

impl Drop for Context {
    fn drop(&mut self) {
        let released = self.shared.model.lock().destroy_context(self.key);
        // What it held goes without the device's lock, its memories among it.
        drop(released);
        let pruned = self.shared.model.lock().prune_workspaces();
        drop(pruned);
    }
}

impl fmt::Debug for Context {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Context")
            .field("key", &self.key)
            .finish_non_exhaustive()
    }
}

/// How an allocation is made.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct AllocationDesc {
    /// Its size in bytes.
    pub size: u64,
    /// Its memory type, an index into [`DeviceInfo::memory_types`].
    pub memory_type: u32,
}

/// Memory of one memory type, held by one context, and released when this
/// and everything made over it is.
pub struct Allocation {
    pub(crate) shared: Arc<Shared>,
    pub(crate) context: ContextKey,
    pub(crate) key: AllocationKey,
    size: u64,
    memory_type: u32,
}

impl Allocation {
    /// Its size in bytes.
    #[must_use]
    pub const fn size(&self) -> u64 {
        self.size
    }

    /// Its memory type.
    #[must_use]
    pub const fn memory_type(&self) -> u32 {
        self.memory_type
    }

    /// The memory the CPU reaches it through, and what of it `len` bytes at
    /// `offset` are.
    fn host(
        &self,
        offset: u64,
        len: usize,
    ) -> Result<(Arc<Memory>, core::ops::Range<usize>), Error> {
        let (memory, size) = self
            .shared
            .model
            .lock()
            .host_memory(self.context, self.key)?;
        let end = offset
            .checked_add(len as u64)
            .filter(|&end| end <= size)
            .ok_or(Error::OutOfRange)?;
        let start = usize::try_from(offset).map_err(|_| Error::OutOfRange)?;
        let end = usize::try_from(end).map_err(|_| Error::OutOfRange)?;
        Ok((memory, start..end))
    }

    /// Write `data` at `offset` from the CPU.
    ///
    /// # Errors
    ///
    /// [`Error::NotHostVisible`] for memory the CPU cannot reach, and
    /// [`Error::OutOfRange`] past its end.
    pub fn write(&self, offset: u64, data: &[u8]) -> Result<(), Error> {
        let (memory, range) = self.host(offset, data.len())?;
        let mut words = memory.lock();
        memory::write_bytes(&mut words, range.start, data).ok_or(Error::OutOfRange)
    }

    /// Read `out.len()` bytes at `offset` from the CPU.
    ///
    /// # Errors
    ///
    /// As [`write`](Self::write).
    pub fn read(&self, offset: u64, out: &mut [u8]) -> Result<(), Error> {
        let (memory, range) = self.host(offset, out.len())?;
        let words = memory.lock();
        memory::read_bytes(&words, range.start, out).ok_or(Error::OutOfRange)
    }

    /// A token another context of this device imports the memory through.
    /// The memory stays charged to this context.
    ///
    /// # Errors
    ///
    /// [`Error::Lost`] once the context is lost.
    pub fn export(&self) -> Result<Export, Error> {
        let (memory, memory_type, size) =
            self.shared.model.lock().export(self.context, self.key)?;
        Ok(Export {
            shared: Arc::clone(&self.shared),
            memory,
            memory_type,
            size,
        })
    }
}

impl Drop for Allocation {
    fn drop(&mut self) {
        self.shared
            .release(self.context, |context| &mut context.allocations, self.key);
    }
}

impl fmt::Debug for Allocation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Allocation")
            .field("size", &self.size)
            .field("memory_type", &self.memory_type)
            .finish_non_exhaustive()
    }
}

/// An allocation's memory on its way to another context, which
/// [`Context::import`] takes it into.
pub struct Export {
    shared: Arc<Shared>,
    memory: Arc<Memory>,
    memory_type: u32,
    size: u64,
}

impl fmt::Debug for Export {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Export")
            .field("size", &self.size)
            .field("memory_type", &self.memory_type)
            .finish_non_exhaustive()
    }
}

/// How a buffer is made over an allocation.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct BufferDesc {
    /// Where it starts in the allocation: a multiple of
    /// [`Limits::buffer_alignment`](crate::Limits::buffer_alignment).
    pub offset: u64,
    /// Its size in bytes.
    pub size: u64,
    /// What it may be used for.
    pub usage: BufferUsage,
}

/// A typed span of an allocation, used only as its usage says. It keeps its
/// memory while it lives, whatever becomes of the allocation's handle.
pub struct Buffer {
    pub(crate) shared: Arc<Shared>,
    pub(crate) context: ContextKey,
    pub(crate) key: BufferKey,
    size: u64,
    usage: BufferUsage,
}

impl Buffer {
    /// Its size in bytes.
    #[must_use]
    pub const fn size(&self) -> u64 {
        self.size
    }

    /// What it may be used for.
    #[must_use]
    pub const fn usage(&self) -> BufferUsage {
        self.usage
    }
}

impl Drop for Buffer {
    fn drop(&mut self) {
        self.shared
            .release(self.context, |context| &mut context.buffers, self.key);
    }
}

impl fmt::Debug for Buffer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Buffer")
            .field("size", &self.size)
            .field("usage", &self.usage)
            .finish_non_exhaustive()
    }
}

/// A counter that only advances, set from the CPU or by completed work, which
/// work and the CPU wait on.
///
/// A value is reached only once every signal up to it has landed, so a wait
/// for it sees all the work those signals followed.
pub struct Timeline {
    shared: Arc<Shared>,
    context: ContextKey,
    key: TimelineKey,
}

impl Timeline {
    /// Where it stands.
    ///
    /// # Errors
    ///
    /// [`Error::Lost`] once the context is lost.
    pub fn value(&self) -> Result<u64, Error> {
        self.shared
            .model
            .lock()
            .timeline_value(self.context, self.key)
    }

    /// Set it to `value` from the CPU, readying any work that was waiting for
    /// it: that runs on the device's worker, or, where it has none, on this
    /// thread or the one already running its engine's work.
    ///
    /// # Errors
    ///
    /// [`Error::TimelineOrder`] unless `value` is past where it stands and
    /// short of every signal queued work will make.
    pub fn signal(&self, value: u64) -> Result<(), Error> {
        let mut model = self.shared.model.lock();
        let waited = model.signal(self.context, self.key, value)?;
        // Before the work it readied runs, which may be on this thread.
        if let Some(wake) = waited {
            wake.notify_all();
        }
        self.shared.drive(model);
        Ok(())
    }

    /// Wait until it reaches `value`.
    ///
    /// # Errors
    ///
    /// [`Error::TimedOut`] when `timeout` elapses first, and [`Error::Lost`]
    /// when the context is lost meanwhile.
    pub fn wait(&self, value: u64, timeout: Duration64) -> Result<(), Error> {
        self.shared.wait(self.context, self.key, value, timeout)
    }
}

impl Drop for Timeline {
    fn drop(&mut self) {
        let released = self
            .shared
            .model
            .lock()
            .destroy_timeline(self.context, self.key);
        drop(released);
    }
}

impl fmt::Debug for Timeline {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Timeline")
            .field("key", &self.key)
            .finish_non_exhaustive()
    }
}

/// One of a context's queues: its work runs in the order submitted, each
/// submission seeing everything the ones before it did.
#[derive(Clone)]
pub struct Queue {
    shared: Arc<Shared>,
    context: ContextKey,
    slot: usize,
    family: u32,
    index: u32,
}

impl Queue {
    /// Its family.
    #[must_use]
    pub const fn family(&self) -> u32 {
        self.family
    }

    /// Its index within its family.
    #[must_use]
    pub const fn index(&self) -> u32 {
        self.index
    }

    /// Check `submission` whole and queue it to run once its waits are met.
    ///
    /// # Errors
    ///
    /// Refused whole, changing nothing but the room its queues may have made
    /// ahead of it, for anything of another context
    /// ([`Error::Foreign`]), any object since released ([`Error::Stale`]),
    /// any range past what it names, any use a buffer was not made for, any
    /// signal that would not advance its timeline, a deadline of zero or past
    /// the device's longest, or more than a submission carries, and for
    /// what every request that makes memory is ([`Context#memory`]); the
    /// error names the refused command where there is one.
    pub fn submit(&self, submission: &Submission<'_>) -> Result<Fence, SubmitError> {
        let mut buffers: ArrayVec<&[Command], MAX_SUBMISSION_BUFFERS> = ArrayVec::new();
        for buffer in submission.command_buffers {
            owned(&buffer.shared, buffer.context, &self.shared, self.context)?;
            buffers
                .try_push(&buffer.commands)
                .map_err(|_| Error::TooMany)?;
        }
        let waits = self.points(submission.waits)?;
        let signals = self.points(submission.signals)?;
        let request = Request {
            queue: self.slot,
            buffers: &buffers,
            waits: &waits,
            signals: &signals,
            span: submission.deadline.saturating_total_nanos(),
        };
        // Before the lock, so what a refusal leaves in it goes after it.
        let mut resolved = Vec::new();
        let mut model = self.shared.model.lock();
        let (timeline, value) = model.submit(self.context, &request, &mut resolved)?;
        self.shared.drive(model);
        Ok(Fence {
            shared: Arc::clone(&self.shared),
            context: self.context,
            timeline,
            value,
        })
    }

    fn points(
        &self,
        points: &[(&Timeline, u64)],
    ) -> Result<ArrayVec<(TimelineKey, u64), MAX_SUBMISSION_POINTS>, Error> {
        let mut keys = ArrayVec::new();
        for &(timeline, value) in points {
            owned(
                &timeline.shared,
                timeline.context,
                &self.shared,
                self.context,
            )?;
            keys.try_push((timeline.key, value))
                .map_err(|_| Error::TooMany)?;
        }
        Ok(keys)
    }
}

impl fmt::Debug for Queue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Queue")
            .field("family", &self.family)
            .field("index", &self.index)
            .finish_non_exhaustive()
    }
}

/// What one submission asks a queue for.
#[derive(Copy, Clone, Debug)]
pub struct Submission<'a> {
    /// Its command buffers, run in order.
    pub command_buffers: &'a [&'a CommandBuffer],
    /// The timeline values it waits for before it starts.
    pub waits: &'a [(&'a Timeline, u64)],
    /// The timeline values it sets once it completes.
    pub signals: &'a [(&'a Timeline, u64)],
    /// The longest it may run once started before its context is lost.
    pub deadline: Duration64,
}

/// A submission's completion, as the CPU waits on it.
#[derive(Clone)]
pub struct Fence {
    shared: Arc<Shared>,
    context: ContextKey,
    timeline: TimelineKey,
    value: u64,
}

impl Fence {
    /// Wait until the submission has completed.
    ///
    /// # Errors
    ///
    /// [`Error::TimedOut`] when `timeout` elapses first, and [`Error::Lost`]
    /// when the context is lost before the submission completes.
    pub fn wait(&self, timeout: Duration64) -> Result<(), Error> {
        self.shared
            .wait(self.context, self.timeline, self.value, timeout)
    }

    /// Whether the submission has completed.
    ///
    /// # Errors
    ///
    /// [`Error::Lost`] once the context is lost.
    pub fn is_signalled(&self) -> Result<bool, Error> {
        let reached = self
            .shared
            .model
            .lock()
            .point(self.context, self.timeline, self.value)?;
        Ok(matches!(reached, Point::Reached))
    }
}

impl fmt::Debug for Fence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Fence")
            .field("value", &self.value)
            .finish_non_exhaustive()
    }
}

/// Commands recorded for one context's queues. Recording checks only that
/// what is named belongs to the context; the rest is checked whole when a
/// submission carries it, and the buffer can be submitted again.
pub struct CommandBuffer {
    pub(crate) shared: Arc<Shared>,
    pub(crate) context: ContextKey,
    commands: Vec<Command>,
}

impl CommandBuffer {
    /// How many commands it holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.commands.len()
    }

    /// Whether it holds none.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.commands.is_empty()
    }

    /// Drop every command, keeping the room they took.
    pub fn clear(&mut self) {
        self.commands.clear();
    }

    pub(crate) fn record(&mut self, command: Command) -> Result<(), Error> {
        if self.commands.len() >= MAX_SUBMISSION_COMMANDS {
            return Err(Error::TooMany);
        }
        self.commands
            .try_reserve(1)
            .map_err(|_| Error::OutOfMemory)?;
        self.commands.push(command);
        Ok(())
    }

    pub(crate) fn named(&self, buffer: &Buffer) -> Result<BufferKey, Error> {
        owned(&buffer.shared, buffer.context, &self.shared, self.context)?;
        Ok(buffer.key)
    }

    /// Copy `regions` of `src` into `dst`. No byte may be written twice, nor,
    /// within one memory, both read and written.
    ///
    /// # Errors
    ///
    /// [`Error::Foreign`] for a buffer of another context, and
    /// [`Error::TooMany`] past [`MAX_COPY_REGIONS`] regions.
    pub fn copy_buffer(
        &mut self,
        src: &Buffer,
        dst: &Buffer,
        regions: &[CopyRegion],
    ) -> Result<(), Error> {
        let (src, dst) = (self.named(src)?, self.named(dst)?);
        if regions.len() > MAX_COPY_REGIONS {
            return Err(Error::TooMany);
        }
        let regions = tairix_util::fallible::collected(regions.len(), regions.iter().copied())
            .ok_or(Error::OutOfMemory)?;
        self.record(Command::Copy { src, dst, regions })
    }

    /// Fill `size` bytes of `dst` at `offset`, both whole words, with `word`
    /// in little-endian byte order.
    ///
    /// # Errors
    ///
    /// [`Error::Foreign`] for a buffer of another context.
    pub fn fill_buffer(
        &mut self,
        dst: &Buffer,
        offset: u64,
        size: u64,
        word: u32,
    ) -> Result<(), Error> {
        let dst = self.named(dst)?;
        self.record(Command::Fill {
            dst,
            offset,
            size,
            word,
        })
    }

    /// Write `data`, whole words and at most [`MAX_UPDATE_BYTES`], into `dst`
    /// at `offset` when the command runs. The bytes are kept, charged to the
    /// context, until the command buffer and every submission of it are done
    /// with them.
    ///
    /// # Errors
    ///
    /// [`Error::Foreign`] for a buffer of another context,
    /// [`Error::TooMany`] past [`MAX_UPDATE_BYTES`], [`Error::OverLimit`] or
    /// [`Error::HeapFull`] where the bytes would pass the context's limit or
    /// the heap, and [`Error::Lost`] or [`Error::Destroyed`] for a context
    /// that can take no more.
    pub fn update_buffer(&mut self, dst: &Buffer, offset: u64, data: &[u8]) -> Result<(), Error> {
        let dst = self.named(dst)?;
        if data.len() > MAX_UPDATE_BYTES {
            return Err(Error::TooMany);
        }
        let bytes = tairix_util::fallible::collected(data.len(), data.iter().copied())
            .ok_or(Error::OutOfMemory)?;
        let charge = self
            .shared
            .model
            .lock()
            .host_charge(self.context, Recorded::held(&bytes))?;
        self.record(Command::Update {
            dst,
            offset,
            data: crate::shared(Recorded::new(bytes, charge))?,
        })
    }
}

impl fmt::Debug for CommandBuffer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CommandBuffer")
            .field("commands", &self.commands.len())
            .finish_non_exhaustive()
    }
}

/// How many of this thread's waits it made unflagged as parked, so a test
/// can see that a poke during any of them would have woken it.
#[cfg(test)]
mod waits {
    use core::cell::Cell;

    std::thread_local! {
        static UNFLAGGED: Cell<usize> = const { Cell::new(0) };
    }

    pub(super) fn note(parked: bool) {
        if !parked {
            UNFLAGGED.with(|unflagged| unflagged.set(unflagged.get() + 1));
        }
    }

    pub(super) fn unflagged() -> usize {
        UNFLAGGED.with(Cell::get)
    }
}

#[cfg(test)]
#[path = "device_wake_tests.rs"]
mod wake_tests;
