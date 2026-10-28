//! The device model: contexts, the objects they hold, the work they submit,
//! and the order that work runs in.
//!
//! Every request is checked, and every allocation it needs made, before
//! anything changes, so a refused request leaves the model as it found it.

use alloc::collections::VecDeque;
use alloc::sync::Arc;
use alloc::vec::Vec;

use tairix_collections::{Key, SlotKey, SlotMap};
use tairix_inline::ArrayVec;
use tairix_rt::sync::Condvar;

use crate::command::{resolve, Command, Op};
use crate::info::{
    BufferUsage, DeviceInfo, MemoryProperties, MAX_MEMORY_HEAPS, MAX_QUEUE_FAMILIES,
};
use crate::memory::{Budget, Charge, Memory};
use crate::{
    Error, Loss, SubmitError, MAX_SUBMISSION_BUFFERS, MAX_SUBMISSION_COMMANDS,
    MAX_SUBMISSION_POINTS,
};

macro_rules! key {
    ($($name:ident),* $(,)?) => {$(
        #[derive(Copy, Clone, Debug, Eq, PartialEq)]
        pub(crate) struct $name(SlotKey);

        impl Key for $name {
            fn from_slot(slot: SlotKey) -> Self {
                Self(slot)
            }

            fn slot(self) -> SlotKey {
                self.0
            }
        }
    )*};
}

key!(ContextKey, AllocationKey, BufferKey, TimelineKey, JobKey);

/// The condition variables of the timelines one completion advanced: its
/// signals and its queue's fence.
pub(crate) type Woken = ArrayVec<Arc<Condvar>, { MAX_SUBMISSION_POINTS + 1 }>;

fn exhausted<E>(_: E) -> Error {
    Error::OutOfMemory
}

pub(crate) struct AllocationState {
    pub(crate) memory: Arc<Memory>,
    pub(crate) memory_type: u32,
    pub(crate) size: u64,
}

pub(crate) struct BufferState {
    pub(crate) memory: Arc<Memory>,
    pub(crate) offset: u64,
    pub(crate) size: u64,
    pub(crate) usage: BufferUsage,
}

/// A signal queued work will make, marked once that work completes.
struct Pending {
    value: u64,
    done: bool,
}

struct TimelineState {
    value: u64,
    /// In increasing order of value.
    pending: VecDeque<Pending>,
    /// How many queued jobs wait on or signal it.
    holds: usize,
    /// Its holder let it go while work still held it.
    orphaned: bool,
    /// A queue's fence, which only its queue signals.
    fence: bool,
    wake: Arc<Condvar>,
}

impl TimelineState {
    fn new(value: u64, fence: bool) -> Self {
        Self {
            value,
            pending: VecDeque::new(),
            holds: 0,
            orphaned: false,
            fence,
            wake: Arc::new(Condvar::new()),
        }
    }

    /// The value the timeline reaches once every queued signal has landed.
    fn promised(&self) -> u64 {
        self.pending
            .back()
            .map_or(self.value, |pending| pending.value)
    }

    /// Mark the signal of `value` landed, advancing the timeline over every
    /// landed signal no unlanded one precedes: a value is reached only once
    /// every signal up to it has been.
    fn land(&mut self, value: u64) -> bool {
        if let Ok(at) = self
            .pending
            .binary_search_by_key(&value, |pending| pending.value)
        {
            if let Some(pending) = self.pending.get_mut(at) {
                pending.done = true;
            }
        }
        let before = self.value;
        while let Some(front) = self.pending.front() {
            if !front.done {
                break;
            }
            self.value = front.value;
            self.pending.pop_front();
        }
        self.value != before
    }
}

struct QueueState {
    engine: usize,
    fence: TimelineKey,
    next_fence: u64,
    /// In submission order; the first `dispatched` are on its engine.
    jobs: VecDeque<JobKey>,
    dispatched: usize,
    /// Whether it is in its engine's runnable list.
    listed: bool,
}

struct ContextState {
    budget: Arc<Budget>,
    lost: Option<Loss>,
    allocations: SlotMap<AllocationKey, AllocationState>,
    buffers: SlotMap<BufferKey, BufferState>,
    timelines: SlotMap<TimelineKey, TimelineState>,
    queues: Vec<QueueState>,
}

impl ContextState {
    fn live(&self) -> Result<(), Error> {
        self.lost.map_or(Ok(()), |loss| Err(Error::Lost(loss)))
    }

    /// Let go of one hold on each timeline `points` names, removing an
    /// orphaned one no work holds any longer.
    fn release(&mut self, points: &[(TimelineKey, u64)]) {
        for &(key, _) in points {
            let Some(timeline) = self.timelines.get_mut(key) else {
                continue;
            };
            timeline.holds = timeline.holds.saturating_sub(1);
            if timeline.orphaned && timeline.holds == 0 {
                let _ = self.timelines.remove(key);
            }
        }
    }
}

struct Job {
    context: ContextKey,
    queue: usize,
    engine: usize,
    ops: Arc<Vec<Op>>,
    waits: Vec<(TimelineKey, u64)>,
    /// Its own signals, then its queue's fence.
    signals: Vec<(TimelineKey, u64)>,
    span: u64,
    deadline: Option<u64>,
}

/// One queue family's engine: how much it runs at once, what it runs now,
/// and the queues with work ready for it, served in turn.
struct Engine {
    depth: usize,
    in_flight: Vec<JobKey>,
    runnable: VecDeque<(ContextKey, usize)>,
}

/// A job handed to an engine to run.
pub(crate) struct Dispatch {
    pub(crate) engine: usize,
    pub(crate) job: JobKey,
    pub(crate) ops: Arc<Vec<Op>>,
    pub(crate) deadline: u64,
}

/// What a submission asks for, by key.
pub(crate) struct Request<'a> {
    pub(crate) queue: usize,
    pub(crate) buffers: &'a [&'a [Command]],
    pub(crate) waits: &'a [(TimelineKey, u64)],
    pub(crate) signals: &'a [(TimelineKey, u64)],
    pub(crate) span: u64,
}

/// Where a wait stands.
pub(crate) enum Point {
    Reached,
    Pending(Arc<Condvar>),
}

pub(crate) struct Model {
    info: DeviceInfo,
    heaps: ArrayVec<Arc<Budget>, MAX_MEMORY_HEAPS>,
    engines: ArrayVec<Engine, MAX_QUEUE_FAMILIES>,
    contexts: SlotMap<ContextKey, ContextState>,
    jobs: SlotMap<JobKey, Job>,
}

impl Model {
    /// A model of the device `info` describes, whose family `i` runs on an
    /// engine holding `depths[i]` jobs at once.
    pub(crate) fn new(info: DeviceInfo, depths: &[usize]) -> Result<Self, Error> {
        info.check()?;
        if depths.len() != info.queue_families.len() || depths.contains(&0) {
            return Err(Error::Malformed);
        }
        let mut heaps = ArrayVec::new();
        for heap in &info.heaps {
            heaps
                .try_push(Arc::new(Budget::new(heap.size)))
                .map_err(|_| Error::Malformed)?;
        }
        let mut engines = ArrayVec::new();
        for &depth in depths {
            let mut in_flight = Vec::new();
            in_flight.try_reserve_exact(depth).map_err(exhausted)?;
            engines
                .try_push(Engine {
                    depth,
                    in_flight,
                    runnable: VecDeque::new(),
                })
                .map_err(|_| Error::Malformed)?;
        }
        Ok(Self {
            info,
            heaps,
            engines,
            contexts: SlotMap::new(),
            jobs: SlotMap::new(),
        })
    }

    pub(crate) const fn info(&self) -> &DeviceInfo {
        &self.info
    }

    /// A context whose allocations may take `limit` bytes, or as much as the
    /// heaps hold.
    pub(crate) fn create_context(&mut self, limit: Option<u64>) -> Result<ContextKey, Error> {
        let limit = limit.unwrap_or_else(|| {
            self.info
                .heaps
                .iter()
                .fold(0, |total, heap| total.saturating_add(heap.size))
        });
        let count = self
            .info
            .queue_families
            .iter()
            .fold(0usize, |total, family| {
                total.saturating_add(family.queues as usize)
            });
        let mut timelines = SlotMap::try_with_capacity(count).map_err(exhausted)?;
        let mut queues = Vec::new();
        queues.try_reserve_exact(count).map_err(exhausted)?;
        for (engine, family) in self.info.queue_families.iter().enumerate() {
            let room = family.queues as usize;
            if let Some(engine) = self.engines.get_mut(engine) {
                engine.runnable.try_reserve(room).map_err(exhausted)?;
            }
            for _ in 0..family.queues {
                let fence = timelines
                    .try_insert(TimelineState::new(0, true))
                    .map_err(exhausted)?;
                queues.push(QueueState {
                    engine,
                    fence,
                    next_fence: 1,
                    jobs: VecDeque::new(),
                    dispatched: 0,
                    listed: false,
                });
            }
        }
        self.contexts
            .try_insert(ContextState {
                budget: Arc::new(Budget::new(limit)),
                lost: None,
                allocations: SlotMap::new(),
                buffers: SlotMap::new(),
                timelines,
                queues,
            })
            .map_err(exhausted)
    }

    /// Release the context and everything it holds. Its queued work is
    /// dropped; work already on an engine is let finish and then discarded.
    pub(crate) fn destroy_context(&mut self, key: ContextKey) {
        let Some(context) = self.contexts.remove(key) else {
            return;
        };
        for queue in &context.queues {
            for &job in queue.jobs.iter().skip(queue.dispatched) {
                let _ = self.jobs.remove(job);
            }
        }
        for engine in &mut self.engines {
            engine.runnable.retain(|&(owner, _)| owner != key);
        }
        for timeline in context.timelines.values() {
            timeline.wake.notify_all();
        }
    }

    fn existing(&self, key: ContextKey) -> Result<&ContextState, Error> {
        self.contexts.get(key).ok_or(Error::Destroyed)
    }

    fn existing_mut(&mut self, key: ContextKey) -> Result<&mut ContextState, Error> {
        self.contexts.get_mut(key).ok_or(Error::Destroyed)
    }

    fn live(&self, key: ContextKey) -> Result<&ContextState, Error> {
        let context = self.existing(key)?;
        context.live()?;
        Ok(context)
    }

    fn live_mut(&mut self, key: ContextKey) -> Result<&mut ContextState, Error> {
        let context = self.existing_mut(key)?;
        context.live()?;
        Ok(context)
    }

    pub(crate) fn status(&self, key: ContextKey) -> Result<(), Error> {
        self.live(key).map(|_| ())
    }

    /// The bytes the context's allocations take from its limit.
    pub(crate) fn memory_taken(&self, key: ContextKey) -> Result<u64, Error> {
        Ok(self.existing(key)?.budget.taken())
    }

    /// `size` zeroed bytes of memory type `memory_type`, charged to the
    /// context and the type's heap: refused past either, and then refused if
    /// the memory cannot be had.
    pub(crate) fn allocate(
        &mut self,
        key: ContextKey,
        size: u64,
        memory_type: u32,
    ) -> Result<AllocationKey, Error> {
        let kind = self.info.memory_type(memory_type)?;
        if size == 0 {
            return Err(Error::Empty);
        }
        if size > self.info.limits.max_allocation_size {
            return Err(Error::TooLarge);
        }
        let heap = usize::try_from(kind.heap)
            .ok()
            .and_then(|heap| self.heaps.get(heap))
            .cloned()
            .ok_or(Error::Malformed)?;
        let context = self.live_mut(key)?;
        context.allocations.try_reserve(1).map_err(exhausted)?;
        let charge = Charge::take(&context.budget, &heap, size)?;
        let memory = Arc::new(Memory::zeroed(size, charge)?);
        context
            .allocations
            .try_insert(AllocationState {
                memory,
                memory_type,
                size,
            })
            .map_err(exhausted)
    }

    pub(crate) fn free(&mut self, key: ContextKey, allocation: AllocationKey) {
        if let Ok(context) = self.existing_mut(key) {
            let _ = context.allocations.remove(allocation);
        }
    }

    /// The memory behind the allocation, for another context of this device
    /// to import.
    pub(crate) fn export(
        &self,
        key: ContextKey,
        allocation: AllocationKey,
    ) -> Result<(Arc<Memory>, u32, u64), Error> {
        let held = self
            .live(key)?
            .allocations
            .get(allocation)
            .ok_or(Error::Stale)?;
        Ok((Arc::clone(&held.memory), held.memory_type, held.size))
    }

    /// File memory another context exported. It stays charged to the context
    /// that allocated it, for as long as anything holds it.
    pub(crate) fn import(
        &mut self,
        key: ContextKey,
        memory: Arc<Memory>,
        memory_type: u32,
        size: u64,
    ) -> Result<AllocationKey, Error> {
        self.live_mut(key)?
            .allocations
            .try_insert(AllocationState {
                memory,
                memory_type,
                size,
            })
            .map_err(exhausted)
    }

    /// The memory the CPU reads and writes the allocation through.
    pub(crate) fn host_memory(
        &self,
        key: ContextKey,
        allocation: AllocationKey,
    ) -> Result<(Arc<Memory>, u64), Error> {
        let held = self
            .live(key)?
            .allocations
            .get(allocation)
            .ok_or(Error::Stale)?;
        let kind = self.info.memory_type(held.memory_type)?;
        if !kind.properties.contains(MemoryProperties::HOST_VISIBLE) {
            return Err(Error::NotHostVisible);
        }
        Ok((Arc::clone(&held.memory), held.size))
    }

    /// A buffer over `size` bytes at `offset` into the allocation, used only
    /// as `usage` says.
    pub(crate) fn create_buffer(
        &mut self,
        key: ContextKey,
        allocation: AllocationKey,
        offset: u64,
        size: u64,
        usage: BufferUsage,
    ) -> Result<BufferKey, Error> {
        if usage == BufferUsage::NONE || size == 0 {
            return Err(Error::Empty);
        }
        if !BufferUsage::ALL.contains(usage) {
            return Err(Error::Usage);
        }
        if !offset.is_multiple_of(self.info.limits.buffer_alignment) {
            return Err(Error::Misaligned);
        }
        let context = self.live_mut(key)?;
        let held = context.allocations.get(allocation).ok_or(Error::Stale)?;
        if offset.checked_add(size).is_none_or(|end| end > held.size) {
            return Err(Error::OutOfRange);
        }
        let memory = Arc::clone(&held.memory);
        context
            .buffers
            .try_insert(BufferState {
                memory,
                offset,
                size,
                usage,
            })
            .map_err(exhausted)
    }

    pub(crate) fn destroy_buffer(&mut self, key: ContextKey, buffer: BufferKey) {
        if let Ok(context) = self.existing_mut(key) {
            let _ = context.buffers.remove(buffer);
        }
    }

    pub(crate) fn create_timeline(
        &mut self,
        key: ContextKey,
        initial: u64,
    ) -> Result<TimelineKey, Error> {
        self.live_mut(key)?
            .timelines
            .try_insert(TimelineState::new(initial, false))
            .map_err(exhausted)
    }

    /// Let the timeline go: at once, or, while queued work still waits on or
    /// signals it, once that work is done.
    pub(crate) fn destroy_timeline(&mut self, key: ContextKey, timeline: TimelineKey) {
        let Ok(context) = self.existing_mut(key) else {
            return;
        };
        let Some(held) = context.timelines.get_mut(timeline) else {
            return;
        };
        if held.holds == 0 {
            let _ = context.timelines.remove(timeline);
        } else {
            held.orphaned = true;
        }
    }

    pub(crate) fn timeline_value(
        &self,
        key: ContextKey,
        timeline: TimelineKey,
    ) -> Result<u64, Error> {
        let context = self.live(key)?;
        Ok(context.timelines.get(timeline).ok_or(Error::Stale)?.value)
    }

    /// Where a wait for the timeline to reach `value` stands.
    pub(crate) fn point(
        &self,
        key: ContextKey,
        timeline: TimelineKey,
        value: u64,
    ) -> Result<Point, Error> {
        let context = self.live(key)?;
        let held = context.timelines.get(timeline).ok_or(Error::Stale)?;
        if held.value >= value {
            Ok(Point::Reached)
        } else {
            Ok(Point::Pending(Arc::clone(&held.wake)))
        }
    }

    /// Set the timeline to `value` from the CPU: past where it stands, and
    /// short of every signal queued work will make, so it only ever
    /// advances.
    pub(crate) fn signal(
        &mut self,
        key: ContextKey,
        timeline: TimelineKey,
        value: u64,
    ) -> Result<Arc<Condvar>, Error> {
        let context = self.live_mut(key)?;
        let held = context.timelines.get_mut(timeline).ok_or(Error::Stale)?;
        let below_queued = held.pending.front().is_none_or(|first| value < first.value);
        if held.fence || value <= held.value || !below_queued {
            return Err(Error::TimelineOrder);
        }
        held.value = value;
        let wake = Arc::clone(&held.wake);
        self.list_ready(key);
        Ok(wake)
    }

    /// Check the request whole and queue it, answering the queue's fence and
    /// the value it reaches once the work completes.
    pub(crate) fn submit(
        &mut self,
        key: ContextKey,
        request: &Request<'_>,
    ) -> Result<(TimelineKey, u64), SubmitError> {
        let longest = self.info.limits.max_job_time.saturating_total_nanos();
        if request.span == 0 || request.span > longest {
            return Err(Error::Deadline.into());
        }
        let commands: usize = request.buffers.iter().map(|buffer| buffer.len()).sum();
        if request.buffers.len() > MAX_SUBMISSION_BUFFERS
            || commands > MAX_SUBMISSION_COMMANDS
            || request.waits.len() > MAX_SUBMISSION_POINTS
            || request.signals.len() > MAX_SUBMISSION_POINTS
        {
            return Err(Error::TooMany.into());
        }
        let context = self.live(key)?;
        let queue = context
            .queues
            .get(request.queue)
            .ok_or(Error::NoSuchQueue)?;
        for &(timeline, _) in request.waits {
            context.timelines.get(timeline).ok_or(Error::Stale)?;
        }
        for (at, &(timeline, value)) in request.signals.iter().enumerate() {
            let held = context.timelines.get(timeline).ok_or(Error::Stale)?;
            let repeated = request.signals[..at]
                .iter()
                .any(|&(other, _)| other == timeline);
            if held.fence || repeated || value <= held.promised() {
                return Err(Error::TimelineOrder.into());
            }
        }
        let mut ops = Vec::new();
        ops.try_reserve_exact(commands).map_err(exhausted)?;
        for (index, command) in request
            .buffers
            .iter()
            .flat_map(|buffer| buffer.iter())
            .enumerate()
        {
            ops.push(
                resolve(&context.buffers, command).map_err(|error| SubmitError {
                    command: Some(index),
                    error,
                })?,
            );
        }
        let fence = (queue.fence, queue.next_fence);
        let mut waits = Vec::new();
        waits
            .try_reserve_exact(request.waits.len())
            .map_err(exhausted)?;
        waits.extend_from_slice(request.waits);
        let mut signals = Vec::new();
        signals
            .try_reserve_exact(request.signals.len() + 1)
            .map_err(exhausted)?;
        signals.extend_from_slice(request.signals);
        signals.push(fence);
        let engine = queue.engine;
        let job = Job {
            context: key,
            queue: request.queue,
            engine,
            ops: Arc::new(ops),
            waits,
            signals,
            span: request.span,
            deadline: None,
        };
        self.queue_job(key, job)?;
        Ok(fence)
    }

    /// Make the room `job` needs, then queue it: nothing changes unless all
    /// of it was had.
    fn queue_job(&mut self, key: ContextKey, job: Job) -> Result<(), Error> {
        let Self { contexts, jobs, .. } = self;
        let context = contexts.get_mut(key).ok_or(Error::Destroyed)?;
        jobs.try_reserve(1).map_err(exhausted)?;
        context
            .queues
            .get_mut(job.queue)
            .ok_or(Error::NoSuchQueue)?
            .jobs
            .try_reserve(1)
            .map_err(exhausted)?;
        for &(timeline, _) in &job.signals {
            if let Some(held) = context.timelines.get_mut(timeline) {
                held.pending.try_reserve(1).map_err(exhausted)?;
            }
        }
        let job_key = jobs.try_insert(job).map_err(exhausted)?;
        let Some(job) = jobs.get(job_key) else {
            return Ok(());
        };
        for &(timeline, value) in &job.signals {
            if let Some(held) = context.timelines.get_mut(timeline) {
                held.pending.push_back(Pending { value, done: false });
                held.holds += 1;
            }
        }
        for &(timeline, _) in &job.waits {
            if let Some(held) = context.timelines.get_mut(timeline) {
                held.holds += 1;
            }
        }
        if let Some(queue) = context.queues.get_mut(job.queue) {
            queue.jobs.push_back(job_key);
            queue.next_fence += 1;
        }
        self.list_ready(key);
        Ok(())
    }

    /// File, with its engine, every queue of the context whose next job is
    /// not yet running and has every wait met.
    fn list_ready(&mut self, key: ContextKey) {
        let Self {
            contexts,
            jobs,
            engines,
            ..
        } = self;
        let Some(context) = contexts.get_mut(key) else {
            return;
        };
        if context.lost.is_some() {
            return;
        }
        let ContextState {
            queues, timelines, ..
        } = context;
        for (index, queue) in queues.iter_mut().enumerate() {
            if queue.listed {
                continue;
            }
            let Some(job) = queue
                .jobs
                .get(queue.dispatched)
                .and_then(|&next| jobs.get(next))
            else {
                continue;
            };
            if let (true, Some(engine)) = (
                reached(timelines, &job.waits),
                engines.get_mut(queue.engine),
            ) {
                engine.runnable.push_back((key, index));
                queue.listed = true;
            }
        }
    }

    /// The next job any engine with room should run, the queues ready for
    /// each served in turn.
    pub(crate) fn dispatch(&mut self, now: u64) -> Option<Dispatch> {
        for engine in 0..self.engines.len() {
            if let Some(dispatch) = self.dispatch_on(engine, now) {
                return Some(dispatch);
            }
        }
        None
    }

    fn dispatch_on(&mut self, engine_index: usize, now: u64) -> Option<Dispatch> {
        loop {
            let Self {
                contexts,
                jobs,
                engines,
                ..
            } = self;
            let engine = engines.get_mut(engine_index)?;
            if engine.in_flight.len() >= engine.depth {
                return None;
            }
            let (key, queue_index) = engine.runnable.pop_front()?;
            let Some(context) = contexts.get_mut(key) else {
                continue;
            };
            let live = context.lost.is_none();
            let Some(queue) = context.queues.get_mut(queue_index) else {
                continue;
            };
            queue.listed = false;
            let Some(&job_key) = queue.jobs.get(queue.dispatched) else {
                continue;
            };
            let Some(job) = jobs.get_mut(job_key) else {
                continue;
            };
            if !live || !reached(&context.timelines, &job.waits) {
                continue;
            }
            queue.dispatched += 1;
            let deadline = now.saturating_add(job.span);
            job.deadline = Some(deadline);
            engine.in_flight.push(job_key);
            let dispatch = Dispatch {
                engine: engine_index,
                job: job_key,
                ops: Arc::clone(&job.ops),
                deadline,
            };
            self.list_ready(key);
            return Some(dispatch);
        }
    }

    /// The job ran to its end, or the device found it malformed. A job whose
    /// context was lost or released meanwhile signals nothing.
    pub(crate) fn complete(&mut self, job_key: JobKey, fault: bool) -> Woken {
        let mut woken = Woken::new();
        let Some(job) = self.jobs.remove(job_key) else {
            return woken;
        };
        if let Some(engine) = self.engines.get_mut(job.engine) {
            engine.in_flight.retain(|&running| running != job_key);
        }
        let Self {
            contexts,
            jobs,
            engines,
            ..
        } = self;
        let Some(context) = contexts.get_mut(job.context) else {
            return woken;
        };
        leave_queue(context, &job, job_key);
        if fault {
            lose(context, job.context, Loss::Fault, jobs, engines);
        } else if context.lost.is_none() {
            for &(timeline, value) in &job.signals {
                let Some(held) = context.timelines.get_mut(timeline) else {
                    continue;
                };
                if held.land(value) {
                    let _ = woken.try_push(Arc::clone(&held.wake));
                }
            }
        }
        context.release(&job.waits);
        context.release(&job.signals);
        self.list_ready(job.context);
        woken
    }

    /// The engine was reset because a job on it overran: the context of every
    /// job past its deadline is lost, as is the context of every other job
    /// `started` says had begun; the rest go back to wait their turn, ahead
    /// of what came after them.
    pub(crate) fn reset(
        &mut self,
        engine_index: usize,
        now: u64,
        started: &dyn Fn(JobKey) -> bool,
    ) {
        let Self {
            contexts,
            jobs,
            engines,
            ..
        } = self;
        let Some(engine) = engines.get_mut(engine_index) else {
            return;
        };
        let mut in_flight = core::mem::take(&mut engine.in_flight);
        for &key in &in_flight {
            let Some(job) = jobs.get(key) else {
                continue;
            };
            let overran = job.deadline.is_some_and(|deadline| deadline <= now);
            let loss = if overran {
                Some(Loss::Overran)
            } else if started(key) {
                Some(Loss::Reset)
            } else {
                None
            };
            if let (Some(loss), Some(context)) = (loss, contexts.get_mut(job.context)) {
                lose(context, job.context, loss, jobs, engines);
            }
        }
        // In reverse, so the queue whose job went first ends at the front.
        for &key in in_flight.iter().rev() {
            let Some(job) = jobs.get_mut(key) else {
                continue;
            };
            let Some(context) = contexts.get_mut(job.context) else {
                let _ = jobs.remove(key);
                continue;
            };
            if context.lost.is_some() {
                if let Some(job) = jobs.remove(key) {
                    leave_queue(context, &job, key);
                }
                continue;
            }
            job.deadline = None;
            let Some(queue) = context.queues.get_mut(job.queue) else {
                continue;
            };
            queue.dispatched = queue.dispatched.saturating_sub(1);
            if !queue.listed {
                if let Some(engine) = engines.get_mut(engine_index) {
                    engine.runnable.push_front((job.context, job.queue));
                    queue.listed = true;
                }
            }
        }
        in_flight.clear();
        if let Some(engine) = engines.get_mut(engine_index) {
            engine.in_flight = in_flight;
        }
    }
}

/// Whether every wait has been met.
fn reached(timelines: &SlotMap<TimelineKey, TimelineState>, waits: &[(TimelineKey, u64)]) -> bool {
    waits.iter().all(|&(timeline, value)| {
        timelines
            .get(timeline)
            .is_some_and(|timeline| timeline.value >= value)
    })
}

/// Take a job that was on its engine out of its queue.
fn leave_queue(context: &mut ContextState, job: &Job, key: JobKey) {
    let Some(queue) = context.queues.get_mut(job.queue) else {
        return;
    };
    let running = queue.dispatched.min(queue.jobs.len());
    if let Some(at) = queue
        .jobs
        .iter()
        .take(running)
        .position(|&queued| queued == key)
    {
        let _ = queue.jobs.remove(at);
        queue.dispatched -= 1;
    }
}

/// Lose the context for `loss`, unless it is already lost: its queued work is
/// dropped, and every thread waiting on one of its timelines is woken to find
/// it so.
fn lose(
    context: &mut ContextState,
    key: ContextKey,
    loss: Loss,
    jobs: &mut SlotMap<JobKey, Job>,
    engines: &mut ArrayVec<Engine, MAX_QUEUE_FAMILIES>,
) {
    if context.lost.is_some() {
        return;
    }
    context.lost = Some(loss);
    for queue in &mut context.queues {
        for &job in queue.jobs.iter().skip(queue.dispatched) {
            let _ = jobs.remove(job);
        }
        queue.jobs.truncate(queue.dispatched);
        queue.listed = false;
    }
    for engine in engines.iter_mut() {
        engine.runnable.retain(|&(owner, _)| owner != key);
    }
    for timeline in context.timelines.values() {
        timeline.wake.notify_all();
    }
}

#[cfg(test)]
#[path = "model_tests.rs"]
mod tests;
