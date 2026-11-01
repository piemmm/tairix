//! The device model: contexts, the objects they hold, the work they submit,
//! and the order that work runs in.
//!
//! Every request is checked, and everything it is charged for taken, before
//! anything changes, so a refused request leaves the model as it found it,
//! but for room a table or queue made ahead of it, which stays, charged.

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::sync::Arc;
use alloc::vec::Vec;

use tairix_collections::{Key, SlotKey, SlotMap};
use tairix_inline::ArrayVec;
use tairix_rt::sync::Condvar;
use tairix_util::fallible;

use crate::binding::{BindSetLayoutState, BindSetState, PipelineLayoutState};
use crate::command::{resolve, Command, Op};
use crate::info::{
    BufferUsage, DeviceInfo, MemoryProperties, QueueCapabilities, MAX_MEMORY_HEAPS,
    MAX_QUEUE_FAMILIES,
};
use crate::memory::{Budget, Charge, Held, Memory, MemoryUse};
use crate::pipeline::ComputePipelineState;
use crate::resource::{BufferViewState, ImageState, SamplerState, ViewState};
use crate::room::{Charged, Table};
use crate::software::dispatch::{Grown, Hold, Need, Plan, Workspace, Workspaces};
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

key!(
    ContextKey,
    AllocationKey,
    BufferKey,
    ImageKey,
    ViewKey,
    BufferViewKey,
    SamplerKey,
    BindSetLayoutKey,
    BindSetKey,
    PipelineLayoutKey,
    ShaderModuleKey,
    ComputePipelineKey,
    TimelineKey,
    JobKey,
);

/// The condition variables of the timelines one completion advanced: its
/// signals and its queue's fence.
pub(crate) type Woken = ArrayVec<Arc<Condvar>, { MAX_SUBMISSION_POINTS + 1 }>;

pub(crate) fn exhausted<E>(_: E) -> Error {
    Error::OutOfMemory
}

/// The bytes a table's growth takes, none for none.
fn grown_bytes(growth: Option<(usize, u64)>) -> u64 {
    growth.map_or(0, |(_, bytes)| bytes)
}

/// What the device's room for one more context grows by: its table of
/// contexts, and each engine's list of queues.
struct ContextRoom {
    contexts: Option<(usize, u64)>,
    lists: ArrayVec<Option<(usize, u64)>, MAX_QUEUE_FAMILIES>,
}

impl ContextRoom {
    fn bytes(&self) -> u64 {
        self.lists
            .iter()
            .fold(grown_bytes(self.contexts), |total, &list| {
                total.saturating_add(grown_bytes(list))
            })
    }
}

/// What an `Arc<T>` keeps on the device: the value and the counts that
/// share it. Its slot in its context's table is the table's room, charged
/// apart.
pub(crate) const fn shared_bytes<T>() -> u64 {
    (size_of::<T>() + 2 * size_of::<usize>()) as u64
}

pub(crate) struct AllocationState {
    pub(crate) memory: Arc<Memory>,
    pub(crate) memory_type: u32,
    pub(crate) size: u64,
}

#[derive(Debug)]
pub(crate) struct BufferState {
    pub(crate) memory: Arc<Memory>,
    pub(crate) offset: u64,
    pub(crate) size: u64,
    pub(crate) usage: BufferUsage,
    _charge: Charge,
}

/// A signal queued work will make, marked once that work completes.
struct Pending {
    value: u64,
    done: bool,
}

pub(crate) struct TimelineState {
    value: u64,
    /// In increasing order of value.
    pending: Charged<VecDeque<Pending>>,
    /// How many queued jobs wait on or signal it.
    holds: usize,
    /// Its holder let it go while work still held it.
    orphaned: bool,
    /// A queue's fence, which only its queue signals.
    fence: bool,
    wake: Arc<Condvar>,
    /// The wake its waiters share.
    _charge: Charge,
}

impl TimelineState {
    /// A timeline at `value`, its pending signals' room charged by the
    /// first charge and its wake by the second.
    fn new(value: u64, fence: bool, (room, wake): (Charge, Charge)) -> Result<Self, Error> {
        Ok(Self {
            value,
            pending: Charged::new(VecDeque::new(), room)?,
            holds: 0,
            orphaned: false,
            fence,
            wake: crate::shared(Condvar::new())?,
            _charge: wake,
        })
    }

    /// The wake its waiters park on, where any does: each holds it while it
    /// parks, so one held only here has none to wake.
    fn waited(&self) -> Option<&Arc<Condvar>> {
        (Arc::strong_count(&self.wake) > 1).then_some(&self.wake)
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
            let _ = self.pending.pop_front();
        }
        self.pending.settle();
        self.value != before
    }
}

pub(crate) struct QueueState {
    engine: usize,
    fence: TimelineKey,
    next_fence: u64,
    /// In submission order; the first `dispatched` are on its engine.
    jobs: Charged<VecDeque<JobKey>>,
    dispatched: usize,
    /// Whether it is in its engine's runnable list.
    listed: bool,
}

/// A context: its memory budget, whether it was lost, a table for each kind
/// of object it makes, its queued and running jobs, and its queues.
pub(crate) struct ContextState {
    pub(crate) budget: Arc<Budget>,
    lost: Option<Loss>,
    pub(crate) allocations: Table<AllocationKey, AllocationState>,
    /// Allocations charged and not yet filed, whose entries `allocations`
    /// keeps room for.
    allocating: usize,
    pub(crate) buffers: Table<BufferKey, Arc<BufferState>>,
    pub(crate) images: Table<ImageKey, Arc<ImageState>>,
    pub(crate) views: Table<ViewKey, Arc<ViewState>>,
    pub(crate) buffer_views: Table<BufferViewKey, Arc<BufferViewState>>,
    pub(crate) samplers: Table<SamplerKey, Arc<SamplerState>>,
    pub(crate) bind_set_layouts: Table<BindSetLayoutKey, Arc<BindSetLayoutState>>,
    pub(crate) bind_sets: Table<BindSetKey, Arc<BindSetState>>,
    pub(crate) pipeline_layouts: Table<PipelineLayoutKey, Arc<PipelineLayoutState>>,
    pub(crate) shader_modules: Table<ShaderModuleKey, Arc<crate::binding::ShaderModuleState>>,
    pub(crate) compute_pipelines: Table<ComputePipelineKey, Arc<ComputePipelineState>>,
    /// Pipelines planned and not yet filed, whose entries `compute_pipelines`
    /// keeps room for.
    planning: usize,
    timelines: Table<TimelineKey, TimelineState>,
    jobs: Table<JobKey, Job>,
    queues: Vec<QueueState>,
    /// Itself and its queues' list.
    _state: Charge,
    /// Its budget, against its own limit while it lives.
    _budget: Charge,
}

/// Which of a context's tables a request files its object in.
pub(crate) type Pick<K, V> = fn(&mut ContextState) -> &mut Table<K, V>;

impl ContextState {
    fn live(&self) -> Result<(), Error> {
        self.lost.map_or(Ok(()), |loss| Err(Error::Lost(loss)))
    }

    fn release(&mut self, points: &[(TimelineKey, u64)]) {
        release(&mut self.timelines, points);
    }
}

/// Let go of one hold on each timeline `points` names, removing an orphaned
/// one no work holds any longer.
fn release(timelines: &mut Table<TimelineKey, TimelineState>, points: &[(TimelineKey, u64)]) {
    for &(key, _) in points {
        let Some(timeline) = timelines.get_mut(key) else {
            continue;
        };
        timeline.holds = timeline.holds.saturating_sub(1);
        if timeline.orphaned && timeline.holds == 0 {
            let _ = timelines.remove(key);
        }
    }
}

/// A submission's work, and the charge for the host memory it and its
/// dispatches' tables keep while it is queued and running.
pub(crate) struct Work {
    pub(crate) ops: Vec<Op>,
    _charge: Charge,
}

/// Timeline values a job waits for or signals.
type Points = Vec<(TimelineKey, u64)>;

/// Work queued on one of a context's queues, with what it waits for and
/// signals.
pub(crate) struct Job {
    queue: usize,
    ops: Arc<Work>,
    waits: Points,
    /// Its own signals, then its queue's fence.
    signals: Points,
    span: u64,
    deadline: Option<u64>,
    /// The working memory its dispatches need of each participant.
    need: Need,
}

/// The host bytes a job keeps queued, its `points` waited for and signalled
/// among them, the tables of whichever of its commands makes the most as it
/// runs on a device of `participants`, as one runs at a time, and the most
/// placements any of its dispatches takes, as they share them. Its own entry
/// and its signals' are the room of the tables and queues they join.
fn job_bytes(ops: &Vec<Op>, (waits, signals): (&Points, &Points), participants: usize) -> u64 {
    let most = |bytes: fn(&Op, usize) -> u64| {
        ops.iter()
            .map(|op| bytes(op, participants))
            .max()
            .unwrap_or(0)
    };
    ops.iter()
        .map(Op::held_bytes)
        .chain([
            most(Op::run_bytes),
            most(Op::place_bytes),
            shared_bytes::<Work>(),
            fallible::held(ops),
            fallible::held(waits),
            fallible::held(signals),
        ])
        .fold(0, u64::saturating_add)
}

/// A job on an engine: its context's, and its own in that context's table.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct JobId {
    pub(crate) context: ContextKey,
    pub(crate) job: JobKey,
}

/// One queue family's engine: how much it runs at once, what it runs now,
/// the queues of the jobs a reset spared, in the order those first ran, to
/// be served ahead of the queues with work ready for it, which are served in
/// turn, and how many queues of live contexts it serves, so its list of them
/// always has room for all, that room charged to the heap.
struct Engine {
    depth: usize,
    in_flight: Vec<JobId>,
    spared: VecDeque<(ContextKey, usize)>,
    runnable: Charged<VecDeque<(ContextKey, usize)>>,
    queues: usize,
}

/// A job handed to an engine to run.
pub(crate) struct Dispatch {
    pub(crate) engine: usize,
    pub(crate) job: JobId,
    pub(crate) ops: Arc<Work>,
    pub(crate) deadline: u64,
    /// The workspaces its dispatches run in, given back when it is done.
    pub(crate) workspaces: Vec<Workspace>,
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
    /// Each context's state, apart, so a slot a released one leaves keeps
    /// little: the table's room is the device's, charged to the heap.
    contexts: Table<ContextKey, Box<ContextState>>,
    workspaces: Workspaces,
    /// The queued jobs of contexts lost meanwhile, to be let go once the
    /// device's lock is: a job may hold the last of a memory.
    discarded: Vec<Job>,
}

/// A table or queue a job joins, as the room it needs is made.
#[derive(Copy, Clone)]
enum Joins {
    Jobs,
    Queue,
    Pending(TimelineKey),
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
                .try_push(crate::shared(Budget::new(heap.size, Error::HeapFull))?)
                .map_err(|_| Error::Malformed)?;
        }
        let heap = heaps.first().cloned().ok_or(Error::Malformed)?;
        let none = || Charge::take(&[&heap], 0, Held::Host);
        let mut engines = ArrayVec::new();
        for &depth in depths {
            let mut in_flight = Vec::new();
            in_flight.try_reserve_exact(depth).map_err(exhausted)?;
            let mut spared = VecDeque::new();
            spared.try_reserve_exact(depth).map_err(exhausted)?;
            engines
                .try_push(Engine {
                    depth,
                    in_flight,
                    spared,
                    runnable: Charged::new(VecDeque::new(), none()?)?,
                    queues: 0,
                })
                .map_err(|_| Error::Malformed)?;
        }
        Ok(Self {
            info,
            heaps,
            engines,
            contexts: Charged::new(SlotMap::new(), none()?)?,
            workspaces: Workspaces::new()?,
            discarded: Vec::new(),
        })
    }

    /// The queued jobs losses discarded, for the caller to let go without
    /// the device's lock.
    pub(crate) fn take_discarded(&mut self) -> Vec<Job> {
        core::mem::take(&mut self.discarded)
    }

    /// Whether an engine with room has work ready for it: what
    /// [`Self::dispatch`] would hand out.
    pub(crate) fn has_ready(&self) -> bool {
        self.engines.iter().any(|engine| {
            engine.in_flight.len() < engine.depth
                && !(engine.spared.is_empty() && engine.runnable.is_empty())
        })
    }

    pub(crate) const fn info(&self) -> &DeviceInfo {
        &self.info
    }

    /// Size the workspaces for a runner `width` participants wide.
    pub(crate) fn set_width(&mut self, width: usize) {
        self.workspaces.set_width(width);
    }

    /// The heap working memory is charged to: the device's first.
    fn work_heap(&self) -> Result<Arc<Budget>, Error> {
        self.heaps.first().cloned().ok_or(Error::Malformed)
    }

    /// `bytes` for `held` charged to the context and `heap`: working memory
    /// no live pipeline holds goes back before the heap the workspaces
    /// come from refuses.
    pub(crate) fn charge(
        &mut self,
        key: ContextKey,
        heap: &Arc<Budget>,
        bytes: u64,
        held: Held,
    ) -> Result<Charge, Error> {
        let budget = Arc::clone(&self.live(key)?.budget);
        self.take(&[&budget, heap], bytes, held)
    }

    /// `bytes` for `held` taken from every one of `budgets`, as
    /// [`Self::after_a_look`] tries it.
    fn take(&mut self, budgets: &[&Arc<Budget>], bytes: u64, held: Held) -> Result<Charge, Error> {
        self.after_a_look(budgets, || Charge::take(budgets, bytes, held))
    }

    /// `attempt` at what `budgets` admit, tried again once working memory no
    /// live pipeline holds has gone back where the heap it comes from refused.
    fn after_a_look<T>(
        &mut self,
        budgets: &[&Arc<Budget>],
        attempt: impl Fn() -> Result<T, Error>,
    ) -> Result<T, Error> {
        match attempt() {
            Err(Error::HeapFull) => {
                let work = self.work_heap()?;
                if !budgets.iter().any(|budget| Arc::ptr_eq(budget, &work)) {
                    return Err(Error::HeapFull);
                }
                // Let go here, under the lock: the heap was about to refuse.
                drop(self.workspaces.prune());
                attempt()
            }
            done => done,
        }
    }

    /// Refused past the context's limit for the `limit` bytes a request
    /// charges it, then past each of `heaps` for all the request charges
    /// that one, before anything is taken or made: so a request past both is
    /// refused for the limit, and one within both only by the host.
    fn admit(
        &mut self,
        key: ContextKey,
        limit: u64,
        heaps: &[(&Arc<Budget>, u64)],
    ) -> Result<(), Error> {
        self.live(key)?.budget.admit(limit)?;
        for (at, &(heap, _)) in heaps.iter().enumerate() {
            if heaps
                .iter()
                .take(at)
                .any(|&(seen, _)| Arc::ptr_eq(seen, heap))
            {
                continue;
            }
            let bytes = heaps
                .iter()
                .filter(|&&(other, _)| Arc::ptr_eq(other, heap))
                .fold(0u64, |total, &(_, bytes)| total.saturating_add(bytes));
            self.after_a_look(&[heap], || heap.admit(bytes))?;
        }
        Ok(())
    }

    /// The host memory an object or queued work of the context keeps,
    /// charged as an allocation is, to the heap the device's own memory is
    /// drawn from.
    pub(crate) fn host_charge(&mut self, key: ContextKey, bytes: u64) -> Result<Charge, Error> {
        let heap = self.work_heap()?;
        self.charge(key, &heap, bytes, Held::Host)
    }

    /// Make `growth` in the context's table `pick` names, charged to the
    /// context and the heap the device's own memory is drawn from.
    fn grow<K: Key, V>(
        &mut self,
        key: ContextKey,
        pick: Pick<K, V>,
        growth: Option<(usize, u64)>,
    ) -> Result<(), Error> {
        let Some((more, bytes)) = growth else {
            return Ok(());
        };
        let charge = self.host_charge(key, bytes)?;
        pick(self.live_mut(key)?).grow(more, charge)
    }

    /// The growth the context's allocations table takes to hold every
    /// allocation charged and not yet filed, and one more.
    fn allocation_growth(&mut self, key: ContextKey) -> Result<Option<(usize, u64)>, Error> {
        let context = self.live_mut(key)?;
        let room = context
            .allocations
            .len()
            .saturating_add(context.allocating)
            .saturating_add(1);
        Ok(context.allocations.growth_to(room))
    }

    /// File the value `make` builds under a charge of `bytes` in the
    /// context's table `pick` names, the table's room made first, all of it
    /// admitted first.
    pub(crate) fn file_charged<K: Key, V>(
        &mut self,
        key: ContextKey,
        pick: Pick<K, V>,
        bytes: u64,
        make: impl FnOnce(Charge) -> Result<V, Error>,
    ) -> Result<K, Error> {
        let growth = pick(self.live_mut(key)?).growth();
        let total = bytes.saturating_add(grown_bytes(growth));
        let work = self.work_heap()?;
        self.admit(key, total, &[(&work, total)])?;
        self.grow(key, pick, growth)?;
        let value = make(self.host_charge(key, bytes)?)?;
        pick(self.live_mut(key)?).try_insert(value)
    }

    /// Holding room in the workspaces for a pipeline of the context needing
    /// `need`, which charges the context and the heap `besides` beside: what
    /// the set lacks of it, charged, to be made without the device's lock;
    /// and what the pool let go meanwhile, to be dropped without it.
    pub(crate) fn plan_workspaces(
        &mut self,
        key: ContextKey,
        need: Need,
        besides: u64,
    ) -> (Result<Plan, Error>, Grown) {
        let budgets = self
            .live(key)
            .map(|context| Arc::clone(&context.budget))
            .and_then(|budget| Ok((budget, self.work_heap()?)));
        match budgets {
            Ok((budget, heap)) => self.workspaces.plan(need, (&budget, besides), &heap),
            Err(error) => (Err(error), Grown::default()),
        }
    }

    /// The first phase of making a pipeline of the context needing `need`
    /// and keeping `held` bytes beside: its room kept in the context's table,
    /// `held` charged, and the workspaces planned, all admitted before any is
    /// taken, so what the plan makes is not made only to be refused after;
    /// and what the pool let go meanwhile, to be dropped without the device's
    /// lock. Filed with [`Self::create_compute_pipeline`], or its room given
    /// back with [`Self::unreserve_pipeline`].
    pub(crate) fn plan_pipeline(
        &mut self,
        key: ContextKey,
        need: Need,
        held: u64,
    ) -> (Result<(Plan, Charge), Error>, Grown) {
        let growth = self.live_mut(key).map(|context| {
            let room = context
                .compute_pipelines
                .len()
                .saturating_add(context.planning)
                .saturating_add(1);
            context.compute_pipelines.growth_to(room)
        });
        let growth = match growth {
            Ok(growth) => growth,
            Err(error) => return (Err(error), Grown::default()),
        };
        let (plan, shrunk) =
            self.plan_workspaces(key, need, held.saturating_add(grown_bytes(growth)));
        let plan = match plan {
            Ok(plan) => plan,
            Err(error) => return (Err(error), shrunk),
        };
        // Admitted with the plan, so only the host can refuse them now.
        let charged = self
            .grow(key, |context| &mut context.compute_pipelines, growth)
            .and_then(|()| self.host_charge(key, held))
            .and_then(|charge| {
                let context = self.live_mut(key)?;
                context.planning = context.planning.saturating_add(1);
                Ok(charge)
            });
        match charged {
            Ok(charge) => (Ok((plan, charge)), shrunk),
            Err(error) => (Err(error), shrunk),
        }
    }

    /// Give back the room kept for a pipeline [`Self::plan_pipeline`]
    /// planned and that is not to be filed.
    pub(crate) fn unreserve_pipeline(&mut self, key: ContextKey) {
        if let Ok(context) = self.existing_mut(key) {
            context.planning = context.planning.saturating_sub(1);
        }
    }

    /// The last phase of holding room in the workspaces: what `grown` made
    /// installed, answering its hold and what the pool let go.
    pub(crate) fn install_workspaces(&mut self, grown: Grown) -> (Option<Hold>, Grown) {
        self.workspaces.install(grown)
    }

    /// Give back the workspaces a job ran in, the pool fitted to the
    /// pipelines still alive: answering what it let go, to be dropped without
    /// the device's lock.
    pub(crate) fn give_back(&mut self, workspaces: Vec<Workspace>) -> [Grown; 2] {
        self.workspaces.give(workspaces)
    }

    /// What the heap working memory is drawn from has given.
    #[cfg(test)]
    pub(crate) fn work_heap_taken(&self) -> u64 {
        self.heaps.first().map_or(0, |heap| heap.taken())
    }

    /// Fit the workspaces to the pipelines still alive, answering what they
    /// let go, to be dropped without the device's lock.
    pub(crate) fn prune_workspaces(&mut self) -> Grown {
        self.workspaces.prune()
    }

    /// The growth the device's room for one more context takes: a slot, and
    /// a place on each engine's list for each of its queues.
    fn context_room(&self) -> Result<ContextRoom, Error> {
        let mut lists = ArrayVec::new();
        for (engine, family) in self.engines.iter().zip(&self.info.queue_families) {
            let room = engine.queues.saturating_add(family.queues as usize);
            lists
                .try_push(engine.runnable.growth_to(room))
                .map_err(|_| Error::Malformed)?;
        }
        Ok(ContextRoom {
            contexts: self.contexts.growth(),
            lists,
        })
    }

    /// Make the device's room `room` names, charged to `heap`.
    fn make_context_room(&mut self, heap: &Arc<Budget>, room: ContextRoom) -> Result<(), Error> {
        if let Some((more, bytes)) = room.contexts {
            let charge = self.take(&[heap], bytes, Held::Host)?;
            self.contexts.grow(more, charge)?;
        }
        for (index, list) in room.lists.into_iter().enumerate() {
            if let Some((more, bytes)) = list {
                let charge = self.take(&[heap], bytes, Held::Host)?;
                if let Some(engine) = self.engines.get_mut(index) {
                    engine.runnable.grow(more, charge)?;
                }
            }
        }
        Ok(())
    }

    /// A context whose memory may take `limit` bytes, or as much as the
    /// heaps hold: its allocations and the host memory its objects, its own
    /// state and its queued work keep on the device. The device's room for
    /// it — its slot, and a place on each engine's list for each of its
    /// queues — is charged to the heap, and stays as long as the device
    /// does, shrinking as the contexts it was made for go; its budget is
    /// charged to the heap for as long as anything charged to it is held.
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
        let heap = self.work_heap()?;
        let room = self.context_room()?;
        let mut fences = SlotMap::new();
        fences.try_reserve_exact(count).map_err(exhausted)?;
        let mut queues = Vec::new();
        queues.try_reserve_exact(count).map_err(exhausted)?;
        let own = (fences.held_bytes() as u64)
            .saturating_add(fallible::held(&queues))
            .saturating_add(size_of::<ContextState>() as u64);
        let wakes = (count as u64).saturating_mul(shared_bytes::<Condvar>());
        let state_bytes = shared_bytes::<Budget>()
            .saturating_add(wakes)
            .saturating_add(own);
        // Its own state asked of its limit, then that and the device's room
        // for it of the heap, before any is taken.
        if state_bytes > limit {
            return Err(Error::OverLimit);
        }
        let heaped = room.bytes().saturating_add(state_bytes);
        self.after_a_look(&[&heap], || heap.admit(heaped))?;
        self.make_context_room(&heap, room)?;
        let itself = self.take(&[&heap], shared_bytes::<Budget>(), Held::Host)?;
        let budget = crate::shared(Budget::of_context(limit, itself))?;
        let counted = self.take(&[&budget], shared_bytes::<Budget>(), Held::Host)?;
        let none = || Charge::take(&[&budget, &heap], 0, Held::Host);
        for engine in 0..self.info.queue_families.len() {
            let serves = self
                .info
                .queue_families
                .get(engine)
                .map_or(0, |family| family.queues);
            for _ in 0..serves {
                let wake = self.take(&[&budget, &heap], shared_bytes::<Condvar>(), Held::Host)?;
                let timeline = TimelineState::new(0, true, (none()?, wake))?;
                let fence = fences.try_insert(timeline).map_err(exhausted)?;
                queues.push(QueueState {
                    engine,
                    fence,
                    next_fence: 1,
                    jobs: Charged::new(VecDeque::new(), none()?)?,
                    dispatched: 0,
                    listed: false,
                });
            }
        }
        let mut state = self.take(&[&budget, &heap], own, Held::Host)?;
        let fences_room = state.split(fences.held_bytes() as u64);
        let context = Box::try_new(ContextState {
            lost: None,
            allocations: Charged::new(SlotMap::new(), none()?)?,
            allocating: 0,
            buffers: Charged::new(SlotMap::new(), none()?)?,
            images: Charged::new(SlotMap::new(), none()?)?,
            views: Charged::new(SlotMap::new(), none()?)?,
            buffer_views: Charged::new(SlotMap::new(), none()?)?,
            samplers: Charged::new(SlotMap::new(), none()?)?,
            bind_set_layouts: Charged::new(SlotMap::new(), none()?)?,
            bind_sets: Charged::new(SlotMap::new(), none()?)?,
            pipeline_layouts: Charged::new(SlotMap::new(), none()?)?,
            shader_modules: Charged::new(SlotMap::new(), none()?)?,
            compute_pipelines: Charged::new(SlotMap::new(), none()?)?,
            planning: 0,
            timelines: Charged::new(fences, fences_room)?,
            jobs: Charged::new(SlotMap::new(), none()?)?,
            queues,
            _state: state,
            _budget: counted,
            budget,
        })
        .map_err(exhausted)?;
        let key = self.contexts.try_insert(context)?;
        for (engine, family) in self.engines.iter_mut().zip(&self.info.queue_families) {
            engine.queues = engine.queues.saturating_add(family.queues as usize);
        }
        Ok(key)
    }

    /// Release the context, answering everything it held for the caller to
    /// let go without the device's lock, and then to fit the workspaces to
    /// the pipelines still alive. Its queued work goes with it; work already
    /// on an engine is let finish and then discarded.
    pub(crate) fn destroy_context(&mut self, key: ContextKey) -> Option<Box<ContextState>> {
        let context = self.contexts.remove(key)?;
        for engine in &mut self.engines {
            engine.runnable.retain(|&(owner, _)| owner != key);
            engine.spared.retain(|&(owner, _)| owner != key);
        }
        for queue in &context.queues {
            if let Some(engine) = self.engines.get_mut(queue.engine) {
                engine.queues = engine.queues.saturating_sub(1);
            }
        }
        for engine in &mut self.engines {
            engine.runnable.settle_to(engine.queues);
        }
        for wake in context.timelines.values().filter_map(TimelineState::waited) {
            wake.notify_all();
        }
        Some(context)
    }

    pub(crate) fn existing(&self, key: ContextKey) -> Result<&ContextState, Error> {
        self.contexts
            .get(key)
            .map(Box::as_ref)
            .ok_or(Error::Destroyed)
    }

    pub(crate) fn existing_mut(&mut self, key: ContextKey) -> Result<&mut ContextState, Error> {
        self.contexts
            .get_mut(key)
            .map(Box::as_mut)
            .ok_or(Error::Destroyed)
    }

    pub(crate) fn live(&self, key: ContextKey) -> Result<&ContextState, Error> {
        let context = self.existing(key)?;
        context.live()?;
        Ok(context)
    }

    pub(crate) fn live_mut(&mut self, key: ContextKey) -> Result<&mut ContextState, Error> {
        let context = self.existing_mut(key)?;
        context.live()?;
        Ok(context)
    }

    pub(crate) fn status(&self, key: ContextKey) -> Result<(), Error> {
        self.live(key).map(|_| ())
    }

    /// What the context's limit is spent on.
    pub(crate) fn memory_taken(&self, key: ContextKey) -> Result<MemoryUse, Error> {
        let budget = &self.existing(key)?.budget;
        Ok(MemoryUse {
            allocations: budget.taken_by(Held::Allocation),
            objects: budget.taken_by(Held::Host),
        })
    }

    /// The charges for `size` bytes of memory type `memory_type`, to the
    /// context and the type's heap, and for what the memory keeps besides,
    /// room kept first in the context's table for it and every allocation
    /// charged before it and not yet filed: refused past the context's limit
    /// for all of it, then past a heap. The memory
    /// is made under them without the device's lock, as zeroing it takes as
    /// long as it is large, and filed with [`Self::file_allocation`], or its
    /// room given back with [`Self::unreserve_allocation`].
    pub(crate) fn charge_allocation(
        &mut self,
        key: ContextKey,
        size: u64,
        memory_type: u32,
    ) -> Result<(Charge, Charge), Error> {
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
        let overhead = Memory::overhead(size);
        let growth = self.allocation_growth(key)?;
        let host = overhead.saturating_add(grown_bytes(growth));
        let work = self.work_heap()?;
        self.admit(
            key,
            size.saturating_add(host),
            &[(&heap, size), (&work, host)],
        )?;
        // Its entry's room first, kept until it is filed, so the memory is not
        // made only to be let go for want of it.
        self.grow(key, |context| &mut context.allocations, growth)?;
        let charge = self.charge(key, &heap, size, Held::Allocation)?;
        let overhead = self.host_charge(key, overhead)?;
        let context = self.live_mut(key)?;
        context.allocating = context.allocating.saturating_add(1);
        Ok((charge, overhead))
    }

    /// Give back the room kept for an allocation [`Self::charge_allocation`]
    /// charged and that is not to be filed.
    pub(crate) fn unreserve_allocation(&mut self, key: ContextKey) {
        if let Ok(context) = self.existing_mut(key) {
            context.allocating = context.allocating.saturating_sub(1);
        }
    }

    /// File memory made under [`Self::charge_allocation`]'s charges, in the
    /// room kept for it, as [`Self::file_memory`] does.
    pub(crate) fn file_allocation(
        &mut self,
        key: ContextKey,
        memory: Arc<Memory>,
        memory_type: u32,
        size: u64,
    ) -> Result<AllocationKey, (Error, Arc<Memory>)> {
        self.unreserve_allocation(key);
        self.file_memory(key, memory, memory_type, size)
    }

    /// Take `item` out of the context's table `pick` names, for the caller to
    /// let go without the device's lock: it may hold the last of a memory.
    pub(crate) fn release<K: Key, V>(
        &mut self,
        key: ContextKey,
        pick: Pick<K, V>,
        item: K,
    ) -> Option<V> {
        self.existing_mut(key)
            .ok()
            .and_then(|context| pick(context).remove(item))
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

    /// File memory the context allocated, or another context of the device
    /// exported to it, past the room kept for allocations not yet filed. It
    /// stays charged to the context that allocated it, for as long as
    /// anything holds it; a refusal hands it back, for the caller to let go
    /// without the device's lock.
    pub(crate) fn file_memory(
        &mut self,
        key: ContextKey,
        memory: Arc<Memory>,
        memory_type: u32,
        size: u64,
    ) -> Result<AllocationKey, (Error, Arc<Memory>)> {
        let kept = Arc::clone(&memory);
        let state = AllocationState {
            memory,
            memory_type,
            size,
        };
        self.allocation_growth(key)
            .and_then(|growth| self.grow(key, |context| &mut context.allocations, growth))
            .and_then(|()| self.live_mut(key)?.allocations.try_insert(state))
            .map_err(|error| (error, kept))
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
        let memory = {
            let held = self
                .live(key)?
                .allocations
                .get(allocation)
                .ok_or(Error::Stale)?;
            if offset.checked_add(size).is_none_or(|end| end > held.size) {
                return Err(Error::OutOfRange);
            }
            Arc::clone(&held.memory)
        };
        let bytes = shared_bytes::<BufferState>();
        self.file_charged(
            key,
            |context| &mut context.buffers,
            bytes,
            |charge| {
                crate::shared(BufferState {
                    memory,
                    offset,
                    size,
                    usage,
                    _charge: charge,
                })
            },
        )
    }

    pub(crate) fn create_timeline(
        &mut self,
        key: ContextKey,
        initial: u64,
    ) -> Result<TimelineKey, Error> {
        let bytes = shared_bytes::<Condvar>();
        self.file_charged(
            key,
            |context| &mut context.timelines,
            bytes,
            |mut wake| {
                // Its pending signals' room, empty until a signal is queued.
                let room = wake.split(0);
                TimelineState::new(initial, false, (room, wake))
            },
        )
    }

    /// Let the timeline go: at once, answering it for the caller to let go
    /// without the device's lock, or, while queued work still waits on or
    /// signals it, once that work is done.
    pub(crate) fn destroy_timeline(
        &mut self,
        key: ContextKey,
        timeline: TimelineKey,
    ) -> Option<TimelineState> {
        let context = self.existing_mut(key).ok()?;
        let held = context.timelines.get_mut(timeline)?;
        if held.holds == 0 {
            context.timelines.remove(timeline)
        } else {
            held.orphaned = true;
            None
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
    /// advances. Answers the wake to rouse, where anything waits on it.
    pub(crate) fn signal(
        &mut self,
        key: ContextKey,
        timeline: TimelineKey,
        value: u64,
    ) -> Result<Option<Arc<Condvar>>, Error> {
        let context = self.live_mut(key)?;
        let held = context.timelines.get_mut(timeline).ok_or(Error::Stale)?;
        let below_queued = held.pending.front().is_none_or(|first| value < first.value);
        if held.fence || value <= held.value || !below_queued {
            return Err(Error::TimelineOrder);
        }
        held.value = value;
        let wake = held.waited().cloned();
        self.list_ready(key);
        Ok(wake)
    }

    /// Check the request whole and queue it, answering the queue's fence and
    /// the value it reaches once the work completes. Its commands are
    /// resolved into `resolved`, the caller's, so what a refusal leaves there
    /// goes without the device's lock.
    pub(crate) fn submit(
        &mut self,
        key: ContextKey,
        request: &Request<'_>,
        resolved: &mut Vec<Op>,
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
        let runs = self
            .info
            .queue_families
            .get(queue.engine)
            .map_or(QueueCapabilities::NONE, |family| family.capabilities);
        resolved.try_reserve_exact(commands).map_err(exhausted)?;
        for (index, command) in request
            .buffers
            .iter()
            .flat_map(|buffer| buffer.iter())
            .enumerate()
        {
            let refused = |error| SubmitError {
                command: Some(index),
                error,
            };
            if !runs.contains(command.needs()) {
                return Err(refused(Error::Unsupported));
            }
            resolved.push(resolve(&self.info, context, command).map_err(refused)?);
        }
        let need = resolved.iter().fold(Need::default(), |need, op| match op {
            Op::Dispatch(dispatch) => need.max(dispatch.pipeline.need),
            _ => need,
        });
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
        let bytes = job_bytes(resolved, (&waits, &signals), self.workspaces.width());
        // Before its work leaves `resolved`, so placing it is never refused.
        self.make_job_room(key, request.queue, &signals, bytes)?;
        let charge = self.host_charge(key, bytes)?;
        let job = Job {
            queue: request.queue,
            ops: crate::shared(Work {
                ops: core::mem::take(resolved),
                _charge: charge,
            })?,
            waits,
            signals,
            span: request.span,
            deadline: None,
            need,
        };
        let Self {
            contexts, engines, ..
        } = self;
        let context = contexts
            .get_mut(key)
            .map(Box::as_mut)
            .ok_or(Error::Destroyed)?;
        place(context, job)?;
        list_ready(contexts, engines, key);
        Ok(fence)
    }

    /// Make the room a job queued on `queue` and signalling `signals` needs
    /// in every table and queue it joins, charged at once, it and the job's
    /// own `besides` bytes admitted first; nothing but that room changes
    /// unless all of it was had.
    fn make_job_room(
        &mut self,
        key: ContextKey,
        queue: usize,
        signals: &[(TimelineKey, u64)],
        besides: u64,
    ) -> Result<(), Error> {
        let mut joins: ArrayVec<(Joins, usize, u64), { MAX_SUBMISSION_POINTS + 3 }> =
            ArrayVec::new();
        {
            let context = self.live(key)?;
            let target = context.queues.get(queue).ok_or(Error::NoSuchQueue)?;
            let pending = signals.iter().filter_map(|&(timeline, _)| {
                let growth = context.timelines.get(timeline)?.pending.growth()?;
                Some((Joins::Pending(timeline), growth))
            });
            for (joined, (more, bytes)) in [
                context.jobs.growth().map(|growth| (Joins::Jobs, growth)),
                target.jobs.growth().map(|growth| (Joins::Queue, growth)),
            ]
            .into_iter()
            .flatten()
            .chain(pending)
            {
                joins
                    .try_push((joined, more, bytes))
                    .map_err(|_| Error::TooMany)?;
            }
        }
        let total = joins
            .iter()
            .fold(0, |total: u64, &(_, _, bytes)| total.saturating_add(bytes));
        let all = total.saturating_add(besides);
        let work = self.work_heap()?;
        self.admit(key, all, &[(&work, all)])?;
        let mut charge = self.host_charge(key, total)?;
        let context = self.live_mut(key)?;
        for &(joined, more, bytes) in &joins {
            let part = charge.split(bytes);
            match joined {
                Joins::Jobs => context.jobs.grow(more, part)?,
                Joins::Queue => context
                    .queues
                    .get_mut(queue)
                    .ok_or(Error::NoSuchQueue)?
                    .jobs
                    .grow(more, part)?,
                Joins::Pending(timeline) => context
                    .timelines
                    .get_mut(timeline)
                    .ok_or(Error::Stale)?
                    .pending
                    .grow(more, part)?,
            }
        }
        Ok(())
    }

    /// File, with its engine, every queue of the context whose next job is
    /// not yet running and has every wait met.
    fn list_ready(&mut self, key: ContextKey) {
        list_ready(&mut self.contexts, &mut self.engines, key);
    }

    /// The next job any engine with room should run, the queues ready for
    /// each served in turn, its deadline counted from `clock` once the
    /// workspaces it runs in are had. The workspaces are looked at first, as
    /// a job just ended may have let go the last of a pipeline, and fitted
    /// again as they are lent: answers too what the look let go, to be
    /// dropped without the device's lock.
    pub(crate) fn dispatch(&mut self, clock: &dyn Fn() -> u64) -> (Option<Dispatch>, Grown) {
        let pruned = self.workspaces.prune();
        let next = (0..self.engines.len()).find_map(|engine| self.dispatch_on(engine, clock));
        (next, pruned)
    }

    fn dispatch_on(&mut self, engine_index: usize, clock: &dyn Fn() -> u64) -> Option<Dispatch> {
        loop {
            let Self {
                contexts,
                engines,
                workspaces,
                ..
            } = self;
            let engine = engines.get_mut(engine_index)?;
            if engine.in_flight.len() >= engine.depth {
                return None;
            }
            let spared = engine.spared.pop_front();
            let (key, queue_index) = spared.or_else(|| engine.runnable.pop_front())?;
            let Some(context) = contexts.get_mut(key).map(Box::as_mut) else {
                continue;
            };
            let live = context.lost.is_none();
            let ContextState {
                queues,
                jobs,
                timelines,
                ..
            } = context;
            let Some(queue) = queues.get_mut(queue_index) else {
                continue;
            };
            // A spared job's queue may be listed too, for the job after it.
            if spared.is_none() {
                queue.listed = false;
            }
            let Some(&job_key) = queue.jobs.get(queue.dispatched) else {
                continue;
            };
            let Some(job) = jobs.get_mut(job_key) else {
                continue;
            };
            if !live || !reached(timelines, &job.waits) {
                continue;
            }
            queue.dispatched += 1;
            let workspaces = workspaces.take(job.need);
            let deadline = clock().saturating_add(job.span);
            job.deadline = Some(deadline);
            let id = JobId {
                context: key,
                job: job_key,
            };
            engine.in_flight.push(id);
            let dispatch = Dispatch {
                engine: engine_index,
                job: id,
                ops: Arc::clone(&job.ops),
                deadline,
                workspaces,
            };
            self.list_ready(key);
            return Some(dispatch);
        }
    }

    /// The job ran to its end on `engine`, or ended losing its context for
    /// `lost`. A job whose context was lost or released meanwhile signals
    /// nothing.
    pub(crate) fn complete(&mut self, engine: usize, id: JobId, lost: Option<Loss>) -> Woken {
        let mut woken = Woken::new();
        let Self {
            contexts,
            engines,
            discarded,
            ..
        } = self;
        if let Some(engine) = engines.get_mut(engine) {
            engine.in_flight.retain(|&running| running != id);
        }
        let Some(context) = contexts.get_mut(id.context).map(Box::as_mut) else {
            return woken;
        };
        let Some(job) = context.jobs.remove(id.job) else {
            return woken;
        };
        leave_queue(context, &job, id.job);
        if let Some(loss) = lost {
            lose((context, id.context), loss, engines, discarded);
        } else if context.lost.is_none() {
            for &(timeline, value) in &job.signals {
                let Some(held) = context.timelines.get_mut(timeline) else {
                    continue;
                };
                if let (true, Some(wake)) = (held.land(value), held.waited()) {
                    let _ = woken.try_push(Arc::clone(wake));
                }
            }
        }
        context.release(&job.waits);
        context.release(&job.signals);
        self.list_ready(id.context);
        woken
    }

    /// The engine was reset because a job on it overran: of the jobs
    /// `started` says had begun, the context of every one past its deadline
    /// is lost for that, and the context of every other is lost for the
    /// reset; the rest, which overran nothing, run again first, in the order
    /// they first ran, ahead of what came after them.
    pub(crate) fn reset(&mut self, engine_index: usize, now: u64, started: &dyn Fn(JobId) -> bool) {
        let Self {
            contexts,
            engines,
            discarded,
            ..
        } = self;
        let Some(engine) = engines.get_mut(engine_index) else {
            return;
        };
        let mut in_flight = core::mem::take(&mut engine.in_flight);
        // Overruns first, so a context whose own work overran is lost for
        // that, whatever else of its had begun.
        for &id in &in_flight {
            if let Some((context, true)) = begun(contexts, id, now, started) {
                lose((context, id.context), Loss::Overran, engines, discarded);
            }
        }
        for &id in &in_flight {
            if let Some((context, _)) = begun(contexts, id, now, started) {
                lose((context, id.context), Loss::Reset, engines, discarded);
            }
        }
        // In reverse, each ahead of the last: ahead too of any an earlier
        // reset spared, as those first ran after these.
        for &id in in_flight.iter().rev() {
            let Some(context) = contexts.get_mut(id.context).map(Box::as_mut) else {
                continue;
            };
            if context.lost.is_some() {
                if let Some(job) = context.jobs.remove(id.job) {
                    leave_queue(context, &job, id.job);
                    context.release(&job.waits);
                    context.release(&job.signals);
                }
                continue;
            }
            let Some(job) = context.jobs.get_mut(id.job) else {
                continue;
            };
            job.deadline = None;
            let Some(queue) = context.queues.get_mut(job.queue) else {
                continue;
            };
            queue.dispatched = queue.dispatched.saturating_sub(1);
            if let Some(engine) = engines.get_mut(engine_index) {
                // The room reserved for the engine's depth holds it: spared
                // and running jobs together never pass that depth.
                if engine.spared.len() < engine.spared.capacity() {
                    engine.spared.push_front((id.context, job.queue));
                }
            }
        }
        in_flight.clear();
        if let Some(engine) = engines.get_mut(engine_index) {
            engine.in_flight = in_flight;
        }
    }
}

/// The context of a job `started` says had begun, and whether the job is
/// past its deadline at `now`: work that had not begun overran nothing.
fn begun<'c>(
    contexts: &'c mut Table<ContextKey, Box<ContextState>>,
    id: JobId,
    now: u64,
    started: &dyn Fn(JobId) -> bool,
) -> Option<(&'c mut ContextState, bool)> {
    if !started(id) {
        return None;
    }
    let context = contexts.get_mut(id.context).map(Box::as_mut)?;
    let overran = context
        .jobs
        .get(id.job)?
        .deadline
        .is_some_and(|deadline| deadline <= now);
    Some((context, overran))
}

/// Whether every wait has been met.
fn reached(timelines: &SlotMap<TimelineKey, TimelineState>, waits: &[(TimelineKey, u64)]) -> bool {
    waits.iter().all(|&(timeline, value)| {
        timelines
            .get(timeline)
            .is_some_and(|timeline| timeline.value >= value)
    })
}

/// File, with its engine, every queue of the context whose next job is not
/// yet running and has every wait met. An engine has room for every queue
/// it serves, and a queue is filed once, so filing allocates nothing.
fn list_ready(
    contexts: &mut Table<ContextKey, Box<ContextState>>,
    engines: &mut ArrayVec<Engine, MAX_QUEUE_FAMILIES>,
    key: ContextKey,
) {
    let Some(context) = contexts.get_mut(key).map(Box::as_mut) else {
        return;
    };
    if context.lost.is_some() {
        return;
    }
    let ContextState {
        queues,
        timelines,
        jobs,
        ..
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
            queue.listed = engine.runnable.push_back((key, index)).is_ok();
        }
    }
}

/// Queue `job` in the room its tables and queues have made: refused, with
/// nothing changed, where one of them has none.
fn place(context: &mut ContextState, job: Job) -> Result<(), Error> {
    let ContextState {
        timelines,
        jobs,
        queues,
        ..
    } = context;
    let queue = queues.get_mut(job.queue).ok_or(Error::NoSuchQueue)?;
    let fits = jobs.fits()
        && queue.jobs.fits()
        && job.signals.iter().all(|&(timeline, _)| {
            timelines
                .get(timeline)
                .is_some_and(|held| held.pending.fits())
        });
    if !fits {
        return Err(Error::OutOfMemory);
    }
    // Every table and queue has its room, so nothing below is refused.
    for &(timeline, value) in &job.signals {
        if let Some(held) = timelines.get_mut(timeline) {
            let _ = held.pending.push_back(Pending { value, done: false });
            held.holds += 1;
        }
    }
    for &(timeline, _) in &job.waits {
        if let Some(held) = timelines.get_mut(timeline) {
            held.holds += 1;
        }
    }
    let job_key = jobs.try_insert(job)?;
    let _ = queue.jobs.push_back(job_key);
    queue.next_fence += 1;
    Ok(())
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
        queue.jobs.settle();
        queue.dispatched -= 1;
    }
}

/// Lose the context for `loss`, unless it is already lost: its queued work is
/// put in `discarded`, its holds on the timelines given back, and every
/// thread waiting on one of its timelines is woken to find it so.
fn lose(
    (context, key): (&mut ContextState, ContextKey),
    loss: Loss,
    engines: &mut ArrayVec<Engine, MAX_QUEUE_FAMILIES>,
    discarded: &mut Vec<Job>,
) {
    if context.lost.is_some() {
        return;
    }
    context.lost = Some(loss);
    let ContextState {
        queues,
        jobs,
        timelines,
        ..
    } = context;
    for queue in queues.iter_mut() {
        for &queued in queue.jobs.iter().skip(queue.dispatched) {
            if let Some(job) = jobs.remove(queued) {
                release(timelines, &job.waits);
                release(timelines, &job.signals);
                // Let go here only where no room can be had for it.
                if discarded.try_reserve(1).is_ok() {
                    discarded.push(job);
                }
            }
        }
        queue.jobs.truncate(queue.dispatched);
        queue.jobs.settle();
        queue.listed = false;
    }
    for engine in engines.iter_mut() {
        engine.runnable.retain(|&(owner, _)| owner != key);
        engine.spared.retain(|&(owner, _)| owner != key);
    }
    for wake in timelines.values().filter_map(TimelineState::waited) {
        wake.notify_all();
    }
}

#[cfg(test)]
#[path = "model_tests.rs"]
mod tests;
