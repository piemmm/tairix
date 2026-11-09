//! Shared host-test stand-ins for the process-global publications a real
//! boot makes exactly once, and the host boot whose tasks' waits block.
//!
//! The unit-test binary runs many independent boots in one process, so
//! sharing a set-once boot publication lets whichever test reached it first
//! decide what every later one observes (`plans/OPEN-DEFECTS.md` D90). Each
//! stand-in here is therefore either identical for every caller (the per-boot
//! hash key) or private to the calling test — libtest runs each test on a
//! thread of its own — so no start order changes what a test sees.
//!
//! A [`HostBoot`] is one test's kernel instance. Its tasks park through the
//! scheduler's own handshake and block until a wake ends the park, so a round
//! trip between two of them costs a wakeup rather than a spinning time slice
//! (`plans/OPEN-DEFECTS.md` D1662), and a fixture server ([`serve`]) receives
//! through the kernel's own blocking receive and answers as `call_reply`
//! does. A thread that only claims the scheduler
//! (`claim_scheduler`) runs on a CPU with no per-CPU slot, so its parks fail
//! closed. The per-thread claims serve this crate's unit tests; the boot is
//! public under `test-arch` for the kernel binary's tests too.

use core::cell::Cell;
#[cfg(test)]
use core::cell::RefCell;
use core::marker::PhantomData;
use core::ptr::NonNull;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::boxed::Box;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use tairix_abi::Errno;
use tairix_kernel_ipc::{CallEndpoint, ReceivedCall};
use tairix_kernel_sched_api::park::{self, AtomicLifecycle, Lifecycle, Settled};
use tairix_kernel_sched_api::{CpuId, ParkableTask, TaskAction, TaskId, TaskState};
use tairix_log::Sink;

use crate::cpu_state::ResumeHandle;
use crate::sleeplock::LineRow;
use crate::waitq::WaitQueueArch;

/// The key every test publishes, standing in for the boot path's draw from
/// the CSPRNG output reserve.
#[cfg(test)]
const TEST_HASH_KEY: tairix_hash::HashSeed =
    tairix_hash::HashSeed::from_words(0x4C41_554E_4348_0001, 0x4C41_554E_4348_0002);

/// Publish the per-boot hash key, so a container whose index is keyed under
/// it (the semantic launch cache) is keyed here exactly as on a booted
/// system rather than born poisoned.
///
/// Every caller publishes the same key and the cell refuses a second write,
/// so what a container files its keys under does not depend on which test
/// reached this first.
#[cfg(test)]
pub(crate) fn publish_hash_key() {
    let _ = tairix_hash::publish(TEST_HASH_KEY);
}

/// The scheduler wait hook a test claims for itself: a monotonic clock it
/// advances, plus the CPU and task the in-kernel wait loops resolve through
/// [`crate::waitq::wait_arch`].
///
/// A stateless handle; every answer comes from the calling thread's own
/// claim, so two tests running in parallel never observe each other's. It
/// answers [`Self::current_task`] from that claim regardless of the CPU
/// asked about, which is all a wait loop's parkability decision reads. A
/// boot's task is woken through, and reads the clock of, its boot.
struct HostWaitArch;

static HOST_WAIT_ARCH: HostWaitArch = HostWaitArch;

/// One thread's claim: the CPU and task the kernel's process-wide,
/// task-id-keyed state sees it as, and the boot it is a task of.
#[derive(Copy, Clone)]
struct Claim {
    cpu: CpuId,
    task: TaskId,
    boot: Option<&'static HostBoot>,
}

std::thread_local! {
    /// The calling thread's claim, `None` until it makes one.
    static CLAIM: Cell<Option<Claim>> = const { Cell::new(None) };
    /// Whether the calling thread resolves the wait hook: it claimed the
    /// scheduler, or it is a boot's task.
    static HOOK: Cell<bool> = const { Cell::new(false) };
    /// The calling test's wait clock, advanced only by its own
    /// `advance_clock` so a concurrent test's ticks can never shorten or
    /// lengthen this one's deadlines.
    static CLOCK: Cell<u64> = const { Cell::new(0) };
    /// The row the calling thread stands in a sleeping lock's line with, made
    /// as it claims the hook so a wait on an exhausted heap finds it made.
    static LINE_ROW: Arc<LineRow> = Arc::new(LineRow::new());
    /// How many further ids of this test's block [`claim_peer_task`] has
    /// handed out.
    #[cfg(test)]
    static PEERS: Cell<u32> = const { Cell::new(0) };
    /// Every task the calling test's wakes unparked, in order.
    #[cfg(test)]
    static UNPARKED: RefCell<std::vec::Vec<TaskId>> = const { RefCell::new(std::vec::Vec::new()) };
    /// Every CPU [`claim_cpu`] handed the calling test, in order.
    #[cfg(test)]
    static CLAIMED_CPUS: RefCell<std::vec::Vec<CpuId>> = const { RefCell::new(std::vec::Vec::new()) };
}

/// Serial number of the next claim, which both its CPU and its task id are
/// derived from.
static NEXT_CLAIM: AtomicU32 = AtomicU32::new(0);

/// The first id of the first claim's block, and so the floor of the whole
/// reserved range.
///
/// Task ids are issued far above every id the suite spells by hand or draws
/// from a test scheduler, so a claimed task's entries in the kill-gate
/// registry, wait queues, and the registries the reclaim path scrubs
/// by id can neither be mistaken for nor cleared by another test's. An id
/// below this floor is therefore one no claim ever answers.
pub(crate) const CLAIM_TASK_BASE: TaskId = 1 << 56;

/// How many task ids one claim owns: every one a `u32` index names.
///
/// A claim is a block rather than a single id because a test that models
/// several principals — a service owner, its caller, a foreign reader —
/// needs an id per principal, and a boot one for every thread it ever runs;
/// hand-picking the extras is the very collision the reserved range exists
/// to prevent. A scenario suite runs hundreds of tasks on one boot, so the
/// block is sized by its index rather than by a guess at the widest test;
/// exhausting it panics rather than wrapping into the neighbouring claim.
const CLAIM_TASK_STRIDE: TaskId = 1 << u32::BITS;

/// This test's own task id.
///
/// A test whose call path reads kernel state keyed by task id alone — the
/// kill-gate registry, a wait queue — needs an id no
/// other test can name. A scheduler-minted one will not do: every test
/// builds its own scheduler and each mints the same low ids, so a
/// termination one test legitimately defers against *its* task 1 is
/// indistinguishable from one against another's.
///
/// Idempotent: a second call returns the same id.
#[cfg(test)]
pub(crate) fn claim_task() -> TaskId {
    claim().task
}

/// A further task id of this test's own claim, distinct from
/// [`claim_task`]'s and from every id any other test can name.
///
/// A test that models more than one principal needs more than one such id,
/// and one claimed id cannot serve as all of them. Hand-picking the extras
/// is what the reserved range exists to prevent: several registries the
/// reclaim path scrubs are process-global and keyed on a task id — the
/// call-endpoint registry by endpoint owner and by call poster, the wait-set
/// table, the shared-region table — and `exit` reaches all of them from
/// sibling tests that hold no registry guard, so a shared id loses this
/// test's in-flight state mid-assert.
///
/// Each call answers the next id of the block, so distinct principals come
/// from distinct calls. A boot's task draws the next id of its boot's block
/// instead, so no task the boot admits can share it.
///
/// # Panics
///
/// Once the calling test has drawn the whole block, rather than wrap into
/// the neighbouring claim's ids.
#[cfg(test)]
pub(crate) fn claim_peer_task() -> TaskId {
    if let Some(boot) = booted() {
        return boot.draw(None);
    }
    let base = claim().task;
    let index = PEERS.with(|peers| {
        let next = peers
            .get()
            .checked_add(1)
            .expect("a test drew more ids than one claim owns");
        peers.set(next);
        next
    });
    base + TaskId::from(index)
}

/// Claim the wait hook for the calling test as well, and report the CPU and
/// task it will resolve through it.
///
/// The CPU is issued from beyond the per-CPU state table, so a scheduler
/// park against it fails closed at the missing slot — the production
/// unconfigured-CPU answer, which is what a host test with no live dispatch
/// loop must get — whatever resume handle another test publishes for a CPU
/// of its own. A boot's task answers its boot's CPU, where it parks.
#[cfg(test)]
pub(crate) fn claim_scheduler() -> (CpuId, TaskId) {
    let claim = claim();
    HOOK.with(|hook| hook.set(true));
    make_line_row();
    (claim.cpu, claim.task)
}

/// The calling thread's claim, minting one on first use.
#[cfg(test)]
fn claim() -> Claim {
    if let Some(claim) = CLAIM.with(Cell::get) {
        return claim;
    }
    let claim = mint_claim();
    CLAIM.with(|c| c.set(Some(claim)));
    claim
}

/// A fresh claim block: its first task id, and a CPU past the per-CPU state
/// table, so a park against it fails closed at the missing slot.
fn mint_claim() -> Claim {
    let serial = NEXT_CLAIM.fetch_add(1, Ordering::Relaxed);
    let table = u32::try_from(crate::cpu_state::TEST_CPUS).expect("the test table is small");
    Claim {
        cpu: table
            .checked_add(serial)
            .expect("a claim per test stays inside the CPU id space"),
        task: TaskId::from(serial)
            .checked_mul(CLAIM_TASK_STRIDE)
            .and_then(|offset| offset.checked_add(CLAIM_TASK_BASE))
            .expect("a claim per test stays inside the task id space"),
        boot: None,
    }
}

/// Make the calling thread's line row now, so a sleeping lock it later
/// contends on needs no allocation for it.
fn make_line_row() {
    LINE_ROW.with(|_| {});
}

/// Resume the calling test's claimed task on `cpu` from now on, as a task
/// woken on another core than it parked on.
///
/// `cpu` should be one [`claim_cpu`] handed this test, so a park there finds
/// a per-CPU slot.
#[cfg(test)]
pub(crate) fn run_claim_on(cpu: CpuId) {
    let held = claim();
    CLAIM.with(|c| c.set(Some(Claim { cpu, ..held })));
}

/// A resume handle published on a claimed CPU, standing in for the task the
/// dispatcher switched in there: a park through it switches nothing, counts
/// itself, and runs `on_park` with its ordinal before returning as the task
/// resuming.
#[cfg(test)]
pub(crate) struct ParkProbe<'a> {
    count: AtomicU32,
    on_park: &'a (dyn Fn(u32) + Sync + 'a),
}

#[cfg(test)]
impl ParkProbe<'_> {
    /// How many times a task has parked through this probe.
    pub(crate) fn parks(&self) -> u32 {
        self.count.load(Ordering::SeqCst)
    }
}

/// Run `body` with a [`ParkProbe`] published on `cpu`, which [`claim_cpu`]
/// handed this test.
///
/// Scoped rather than an owned guard so the probe cannot be forgotten while
/// published: it is retracted when `body` returns or unwinds, before the
/// probe or anything `on_park` borrows goes away.
#[cfg(test)]
pub(crate) fn with_park_probe<R>(
    cpu: CpuId,
    on_park: impl Fn(u32) + Sync,
    body: impl FnOnce(&ParkProbe<'_>) -> R,
) -> R {
    struct Retract(&'static crate::cpu_state::CpuState);
    impl Drop for Retract {
        fn drop(&mut self) {
            *self.0.resume.lock() = None;
        }
    }

    let probe = ParkProbe {
        count: AtomicU32::new(0),
        on_park: &on_park,
    };
    let slot = crate::cpu_state::get(cpu).expect("a claimed CPU has a slot");
    // SAFETY: `count_park` is compiled for `ParkProbe`, the type the pointer
    // addresses, and `Retract` withdraws the handle before `probe` leaves
    // scope on every path out of this frame.
    *slot.resume.lock() = Some(unsafe { ResumeHandle::new(NonNull::from(&probe), count_park) });
    let _retract = Retract(slot);
    body(&probe)
}

/// A [`ParkProbe`]'s thunk: count the park and run the probe's closure.
///
/// # Safety
///
/// `ctl` addresses a [`ParkProbe`] whose [`with_park_probe`] has not
/// returned.
#[cfg(test)]
unsafe fn count_park(ctl: NonNull<()>, _action: TaskAction) {
    // SAFETY: the caller's contract; the probe is reached only through
    // shared references, and its counter is atomic.
    let probe = unsafe { ctl.cast::<ParkProbe<'_>>().as_ref() };
    let count = probe.count.fetch_add(1, Ordering::SeqCst) + 1;
    (probe.on_park)(count);
}

/// The first CPU [`claim_cpu`] hands out. Slot 0 is the CPU every unpinned
/// `TestArch` reports, so state published there would reach every such test.
pub(crate) const CLAIMED_CPU_BASE: CpuId = 64;

/// The next CPU [`claim_cpu`] hands out.
static NEXT_CPU: AtomicU32 = AtomicU32::new(CLAIMED_CPU_BASE);

/// A CPU of the shared per-CPU state table for the calling test alone.
///
/// Per-CPU state is process-wide and keyed by CPU id alone, so two tests
/// naming one CPU observe each other's ticks, live spaces and stacks. A
/// claimed slot is handed to no other test and never handed out twice, so
/// the claimant finds it as no test has left it. Each call answers a fresh
/// slot, so a test pinning several CPUs claims each.
///
/// # Panics
///
/// Once the suite has claimed every slot of the table, rather than share one.
pub(crate) fn claim_cpu() -> CpuId {
    let cpu = NEXT_CPU.fetch_add(1, Ordering::Relaxed);
    assert!(
        usize::try_from(cpu).is_ok_and(|cpu| cpu < crate::cpu_state::TEST_CPUS),
        "the suite claimed more CPUs than the test state table holds"
    );
    #[cfg(test)]
    CLAIMED_CPUS.with(|claimed| claimed.borrow_mut().push(cpu));
    cpu
}

/// Every CPU [`claim_cpu`] handed the calling test, in order.
#[cfg(test)]
pub(crate) fn claimed_cpus() -> std::vec::Vec<CpuId> {
    CLAIMED_CPUS.with(|claimed| claimed.borrow().clone())
}

/// Whether [`claim_cpu`] has handed `cpu` out.
#[cfg(test)]
pub(crate) fn is_claimed_cpu(cpu: CpuId) -> bool {
    (CLAIMED_CPU_BASE..NEXT_CPU.load(Ordering::Relaxed)).contains(&cpu)
}

/// The calling thread's claimed hook, or `None` while it has claimed none —
/// which is what every test that never asks for one sees, exactly as before
/// any boot publication. A boot's task always has one. Read by
/// [`crate::waitq::wait_arch`].
pub(crate) fn claimed_wait_arch() -> Option<&'static (dyn WaitQueueArch + 'static)> {
    HOOK.with(Cell::get)
        .then_some(&HOST_WAIT_ARCH as &(dyn WaitQueueArch + 'static))
}

/// The tasks the calling test's wakes have unparked since it last asked,
/// oldest first. A boot's wakes reach its own tasks and are not recorded.
#[cfg(test)]
pub(crate) fn take_unparked() -> std::vec::Vec<TaskId> {
    UNPARKED.with(RefCell::take)
}

/// Advance the calling test's wait clock, so a bounded wait it drives
/// reaches its deadline; on a boot's task, the boot's
/// ([`HostBoot::advance_clock`]).
#[cfg(test)]
pub(crate) fn advance_clock(ns: u64) {
    if let Some(boot) = booted() {
        boot.advance_clock(ns);
    } else {
        CLOCK.with(|c| c.set(c.get().saturating_add(ns)));
    }
}

/// The boot the calling thread is a task of.
fn booted() -> Option<&'static HostBoot> {
    CLAIM.with(Cell::get).and_then(|claim| claim.boot)
}

impl WaitQueueArch for HostWaitArch {
    fn unpark(&self, id: TaskId) -> bool {
        if let Some(boot) = booted() {
            return boot.unpark(id);
        }
        #[cfg(test)]
        UNPARKED.with(|unparked| unparked.borrow_mut().push(id));
        true
    }

    fn now_ns(&self) -> u64 {
        booted().map_or_else(|| CLOCK.with(Cell::get), HostBoot::now_ns)
    }

    fn set_wakeup(&self, _deadline_ns: Option<u64>) {}

    fn current_cpu(&self) -> Option<CpuId> {
        CLAIM.with(Cell::get).map(|claim| claim.cpu)
    }

    fn current_task(&self, _cpu: CpuId) -> Option<TaskId> {
        CLAIM.with(Cell::get).map(|claim| claim.task)
    }

    fn line_row(&self, _cpu: CpuId) -> Option<Arc<LineRow>> {
        HOOK.with(Cell::get).then(|| LINE_ROW.with(Arc::clone))
    }
}

/// How long [`HostBoot::until_blocked`] waits for a task to block before it
/// fails the test: a detector for a task that never parks, not a budget.
const LIVENESS_BOUND: Duration = Duration::from_secs(60);

/// What a boot's tasks fail with once every live one waits on another.
const LOST_WAKE: &str = "every live task of the host boot waits on another: a wake was lost";

/// One test's kernel instance: a claimed CPU slot whose resume handle blocks
/// the parking thread, a virtual clock, and one record per task.
///
/// Leaked once per test, and reached only through its tasks' claims and its
/// CPU slot. Every thread that waits or wakes in such a test is one of its
/// tasks: a wake runs the scheduler's `unpark` on a task of the waking
/// thread's boot, and an id outside that boot — another test's task on a
/// shared queue — is answered as landed and left alone.
///
/// A task about to block while every other live task waits on another could
/// never be woken, so it fails the test instead and wakes the rest to fail
/// too: a lost wake fails rather than hangs.
pub struct HostBoot {
    /// The claimed per-CPU slot its parkable tasks run on.
    cpu: CpuId,
    /// A CPU past the per-CPU table, so a task there cannot park.
    unparkable_cpu: CpuId,
    /// The first id of its claim block.
    base: TaskId,
    /// Its virtual monotonic clock, in nanoseconds.
    clock: AtomicU64,
    state: Mutex<BootState>,
    /// Signalled when a task blocks or retires.
    blocks: Condvar,
    /// Signalled when a task retires.
    exits: Condvar,
}

/// What a boot's lock guards.
struct BootState {
    /// One entry per id drawn from the block, in order: an admitted task's
    /// record, or `None` for a principal no thread runs.
    tasks: std::vec::Vec<Option<TaskRecord>>,
    /// Every live task was found waiting on another.
    lost: bool,
}

/// One admitted task.
struct TaskRecord {
    lifecycle: AtomicLifecycle,
    /// What a park of the task waits on, so a wake reaches only the task it
    /// is for.
    resume: Arc<Condvar>,
    /// Waiting for a wake to end a park the scheduler committed.
    blocked: bool,
    /// How many of its parks blocked rather than being cut short.
    waits: u32,
    /// The block index of the task it is joining.
    joining: Option<usize>,
}

impl TaskRecord {
    /// A task admitted on the thread it runs on.
    fn running() -> Self {
        Self {
            lifecycle: AtomicLifecycle::new(TaskState::Running),
            resume: Arc::new(Condvar::new()),
            blocked: false,
            waits: 0,
            joining: None,
        }
    }

    fn is_live(&self) -> bool {
        self.load_state() != TaskState::Exited
    }
}

impl ParkableTask for TaskRecord {
    fn load_lifecycle(&self) -> Lifecycle {
        self.lifecycle.load()
    }

    fn cas_lifecycle(&self, expected: Lifecycle, new: Lifecycle) -> Result<(), Lifecycle> {
        self.lifecycle.compare_exchange(expected, new)
    }

    fn swap_lifecycle(&self, new: Lifecycle) -> Lifecycle {
        self.lifecycle.swap(new)
    }
}

impl BootState {
    fn record(&self, index: usize) -> Option<&TaskRecord> {
        self.tasks.get(index).and_then(Option::as_ref)
    }

    fn record_mut(&mut self, index: usize) -> Option<&mut TaskRecord> {
        self.tasks.get_mut(index).and_then(Option::as_mut)
    }

    /// The record of a thread the boot admitted, which it keeps for good.
    fn task(&self, index: usize) -> &TaskRecord {
        self.record(index).expect("an admitted task's record")
    }

    fn task_mut(&mut self, index: usize) -> &mut TaskRecord {
        self.record_mut(index).expect("an admitted task's record")
    }

    fn is_live(&self, index: usize) -> bool {
        self.record(index).is_some_and(TaskRecord::is_live)
    }

    fn any_live(&self) -> bool {
        self.tasks.iter().flatten().any(TaskRecord::is_live)
    }

    /// Whether the task at `index` can run again only once another acts: it
    /// is blocked in a park, or joining a task that is still live.
    fn waits_on_another(&self, index: usize) -> bool {
        let Some(record) = self.record(index) else {
            return false;
        };
        record.blocked || record.joining.is_some_and(|joined| self.is_live(joined))
    }

    /// Whether every live task but the one at `except` waits on another, so
    /// none is left to wake them.
    fn all_waiting_but(&self, except: Option<usize>) -> bool {
        (0..self.tasks.len())
            .filter(|&index| Some(index) != except && self.is_live(index))
            .all(|index| self.waits_on_another(index))
    }
}

impl HostBoot {
    /// A new boot, with the calling thread its first task for as long as the
    /// boot lasts.
    ///
    /// # Panics
    ///
    /// If the calling thread is already a task of a claim.
    #[must_use]
    pub fn claim() -> &'static Self {
        let block = mint_claim();
        let boot: &'static Self = crate::forever(Box::new(Self {
            cpu: claim_cpu(),
            unparkable_cpu: block.cpu,
            base: block.task,
            clock: AtomicU64::new(0),
            state: Mutex::new(BootState {
                tasks: std::vec::Vec::new(),
                lost: false,
            }),
            blocks: Condvar::new(),
            exits: Condvar::new(),
        }));
        boot.publish_resume();
        boot.adopt_caller(boot.cpu);
        boot
    }

    /// Admit the calling thread as a further task until the returned guard
    /// drops. A task waiting for it to act must not block before it is
    /// admitted, or the boot finds no task left to wake that one.
    ///
    /// # Panics
    ///
    /// If the calling thread is already a task of a claim, or the boot's
    /// block is spent.
    pub fn enter(&'static self) -> EnteredTask {
        self.guard(self.adopt_caller(self.cpu))
    }

    /// [`Self::enter`], for a task no scheduler can park: its parks fail
    /// closed, as a claim's do, while its wakes still reach the boot's tasks.
    ///
    /// # Panics
    ///
    /// As [`Self::enter`].
    pub fn enter_unparkable(&'static self) -> EnteredTask {
        self.guard(self.adopt_caller(self.unparkable_cpu))
    }

    /// Run `body` on a new thread admitted as a further task, retired when
    /// `body` returns or unwinds.
    ///
    /// # Panics
    ///
    /// If the boot's block is spent, or the thread cannot start.
    pub fn spawn<T: Send + 'static>(
        &'static self,
        body: impl FnOnce() -> T + Send + 'static,
    ) -> HostTask<T> {
        let task = self.draw(Some(TaskRecord::running()));
        let started = std::thread::Builder::new().spawn(move || {
            adopt(self, task, self.cpu);
            let _retires = self.guard(task);
            body()
        });
        match started {
            Ok(thread) => HostTask {
                boot: self,
                task,
                thread,
            },
            Err(error) => {
                self.retire(task);
                panic!("a host boot task could not start: {error}");
            }
        }
    }

    /// The calling thread's task id.
    ///
    /// # Panics
    ///
    /// If the calling thread is not one of the boot's tasks.
    #[must_use]
    pub fn task(&self) -> TaskId {
        let claim = self.own_claim().expect("a task of this boot");
        claim.task
    }

    /// The boot the calling thread is a task of.
    ///
    /// # Panics
    ///
    /// If the calling thread is no boot's task.
    #[must_use]
    pub fn current() -> &'static Self {
        booted().expect("a task of a host boot")
    }

    /// The CPU the boot's tasks park on, which an architecture double that a
    /// task's syscalls read their CPU from must report.
    #[must_use]
    pub fn cpu(&self) -> CpuId {
        self.cpu
    }

    /// [`Self::spawn`], handing `body` the question whether it has been asked
    /// to stop, which it asks after each park as a kernel wait asks after a
    /// kill.
    ///
    /// # Panics
    ///
    /// As [`Self::spawn`].
    pub fn spawn_until_stopped(
        &'static self,
        body: impl FnOnce(&dyn Fn() -> bool) + Send + 'static,
    ) -> StoppableTask {
        let stop = Arc::new(AtomicBool::new(false));
        let asked = Arc::clone(&stop);
        let task = self.spawn(move || body(&|| asked.load(Ordering::Acquire)));
        StoppableTask { task, stop }
    }

    /// Advance the boot's clock by `ns`, as its timer interrupt: every other
    /// live task is woken to re-check its deadline. A blocked task's park
    /// ends, and a running one's next park is cut short, so a deadline it read
    /// before the advance is not slept through.
    ///
    /// # Panics
    ///
    /// If the calling thread is not one of the boot's tasks: only a task may
    /// wake the others.
    pub fn advance_clock(&self, ns: u64) {
        let caller = self.own_index().expect("a task of this boot");
        let mut state = self.lock();
        let now = self.clock.load(Ordering::Relaxed).saturating_add(ns);
        self.clock.store(now, Ordering::Release);
        for index in 0..state.tasks.len() {
            if index != caller {
                Self::wake_index(&mut state, index);
            }
        }
    }

    /// Wait until `task` is blocked in a park.
    ///
    /// # Panics
    ///
    /// If `task` is not another of the boot's tasks, retires without blocking
    /// or does not block within a minute, or a wake is found lost. The minute
    /// detects a task that never parks; it is not a budget.
    pub fn until_blocked(&self, task: TaskId) {
        let index = self.index_of(task).expect("a task of this boot");
        assert_ne!(self.own_index(), Some(index), "it would wait on itself");
        let deadline = Instant::now() + LIVENESS_BOUND;
        let mut state = self.lock();
        while !state.task(index).blocked {
            assert!(
                state.is_live(index),
                "task {task} retired before it blocked"
            );
            assert!(!state.lost, "{LOST_WAKE}");
            let left = deadline.saturating_duration_since(Instant::now());
            assert!(
                !left.is_zero(),
                "task {task} did not block within {LIVENESS_BOUND:?}"
            );
            state = match self.blocks.wait_timeout(state, left) {
                Ok((guard, _)) => guard,
                Err(poisoned) => poisoned.into_inner().0,
            };
        }
    }

    /// Whether `task` is blocked in a park now. A wake ends the block before
    /// the waking call returns, so a test reads straight after one whether it
    /// came.
    ///
    /// # Panics
    ///
    /// If `task` is not one of the boot's tasks.
    #[must_use]
    pub fn is_blocked(&self, task: TaskId) -> bool {
        let index = self.index_of(task).expect("a task of this boot");
        self.lock().task(index).blocked
    }

    /// How many of `task`'s parks blocked, rather than being cut short by a
    /// wake it was already owed.
    ///
    /// # Panics
    ///
    /// If `task` is not one of the boot's tasks.
    #[must_use]
    pub fn waits(&self, task: TaskId) -> u32 {
        let index = self.index_of(task).expect("a task of this boot");
        self.lock().task(index).waits
    }

    /// Make the calling thread a task of the boot, running on `cpu`.
    fn adopt_caller(&'static self, cpu: CpuId) -> TaskId {
        assert!(
            CLAIM.with(Cell::get).is_none(),
            "the calling thread is already a task of a claim"
        );
        let task = self.draw(Some(TaskRecord::running()));
        adopt(self, task, cpu);
        task
    }

    /// The guard that retires `task` when it drops.
    fn guard(&'static self, task: TaskId) -> EnteredTask {
        EnteredTask {
            boot: self,
            task,
            _not_send: PhantomData,
        }
    }

    /// The next id of the block, for `record`'s task or, with none, for a
    /// principal no thread runs.
    ///
    /// # Panics
    ///
    /// Once the block is spent, rather than wrap into the neighbouring
    /// claim's ids.
    fn draw(&self, record: Option<TaskRecord>) -> TaskId {
        let mut state = self.lock();
        let offset = TaskId::try_from(state.tasks.len()).expect("a block index fits a task id");
        assert!(
            offset < CLAIM_TASK_STRIDE,
            "a host boot drew more ids than its claim block holds"
        );
        state.tasks.push(record);
        self.base + offset
    }

    /// Publish the boot's resume handle on its CPU slot, which no other test
    /// is ever handed.
    fn publish_resume(&'static self) {
        let slot = crate::cpu_state::get(self.cpu).expect("a claimed CPU has a slot");
        // SAFETY: `suspend_task` is compiled for `HostBoot`, the type the
        // pointer addresses, and the boot is leaked, so the handle can never
        // outlive it.
        let handle = unsafe { ResumeHandle::new(NonNull::from(self), suspend_task) };
        *slot.resume.lock() = Some(handle);
    }

    /// The scheduler's settle of the calling task's request, the wait a
    /// dispatch leaves a parked task in, and the dispatch that runs it again.
    fn suspend(&self, action: TaskAction) {
        let index = self.own_index().expect("a task of this boot");
        let mut state = self.lock();
        let settled = park::settle(state.task(index), action, false, |transition| transition());
        match settled {
            Settled::Woken | Settled::Requeue => {}
            Settled::Released => {
                if state.lost || state.all_waiting_but(Some(index)) {
                    self.fail(state, index);
                }
                let record = state.task_mut(index);
                record.blocked = true;
                record.waits = record.waits.saturating_add(1);
                let resume = Arc::clone(&record.resume);
                self.blocks.notify_all();
                while state.task(index).load_state() == TaskState::Parked {
                    if state.lost {
                        self.fail(state, index);
                    }
                    state = wait(&resume, state);
                }
            }
            Settled::Retire => {
                drop(state);
                panic!("a host boot task cannot leave its thread through an exit");
            }
        }
        let record = state.task(index);
        let dispatched = record.cas_state(TaskState::Ready, TaskState::Running);
        assert!(dispatched.is_ok(), "a woken host task is ready to run");
        drop(state);
        if settled == Settled::Requeue {
            std::thread::yield_now();
        }
    }

    /// Fail the wait of the task at `index`, which nothing is left to end,
    /// and wake every other waiter to fail too.
    fn fail(&self, mut state: MutexGuard<'_, BootState>, index: usize) -> ! {
        let record = state.task_mut(index);
        record.blocked = false;
        record.joining = None;
        // A joining task never parked.
        let _ = record.cas_state(TaskState::Parked, TaskState::Running);
        state.lost = true;
        self.release_all(&state);
        drop(state);
        panic!("{LOST_WAKE}");
    }

    /// Wake every waiter on the boot.
    fn release_all(&self, state: &BootState) {
        for record in state.tasks.iter().flatten() {
            record.resume.notify_one();
        }
        self.blocks.notify_all();
        self.exits.notify_all();
    }

    /// The scheduler's `unpark` of `task` on this boot: `false` only for a
    /// task of the boot that has retired. An id the boot never admitted —
    /// another test's task, or a principal no thread runs — is answered as
    /// landed and left alone.
    fn unpark(&self, task: TaskId) -> bool {
        let Some(index) = self.index_of(task) else {
            return true;
        };
        Self::wake_index(&mut self.lock(), index)
    }

    /// Wake the task at `index` as the scheduler's `unpark` does, ending its
    /// park if it is blocked, and report whether it can still run.
    fn wake_index(state: &mut BootState, index: usize) -> bool {
        let Some(record) = state.record_mut(index) else {
            return true;
        };
        let admitted = Cell::new(false);
        let landed = park::unpark_task(&*record, |_| admitted.set(true)).is_ok();
        if admitted.get() {
            record.blocked = false;
            record.resume.notify_one();
        }
        landed
    }

    /// Retire `task`, as a thread's death does: a wake of it then reports it
    /// can never run, and its rows on every queue the kernel can name go. A
    /// retirement leaving every live task waiting on another is a lost wake,
    /// and wakes them to fail.
    fn retire(&self, task: TaskId) {
        let Some(index) = self.index_of(task) else {
            return;
        };
        let mut state = self.lock();
        if let Some(record) = state.record_mut(index) {
            park::retire(&*record, |transition| transition());
            record.blocked = false;
            record.joining = None;
        }
        let lost = !state.lost && state.any_live() && state.all_waiting_but(None);
        state.lost |= lost;
        if lost {
            self.release_all(&state);
        } else {
            self.blocks.notify_all();
            self.exits.notify_all();
        }
        drop(state);
        crate::waitq::retire_task(task);
    }

    /// Wait until `task` retires, counting the calling thread as waiting on it
    /// when it is one of the boot's tasks.
    fn await_retired(&self, task: TaskId) {
        let Some(target) = self.index_of(task) else {
            return;
        };
        let waiter = self.own_index();
        assert_ne!(waiter, Some(target), "a task cannot join itself");
        let mut state = self.lock();
        if let Some(index) = waiter {
            if !state.lost && state.is_live(target) && state.all_waiting_but(Some(index)) {
                self.fail(state, index);
            }
            state.task_mut(index).joining = Some(target);
        }
        while state.is_live(target) {
            state = wait(&self.exits, state);
        }
        if let Some(index) = waiter {
            state.task_mut(index).joining = None;
        }
    }

    /// The boot's state; a task that panicked holding the lock left it
    /// consistent.
    fn lock(&self) -> MutexGuard<'_, BootState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn now_ns(&self) -> u64 {
        self.clock.load(Ordering::Acquire)
    }

    /// The block index of `task`, if it lies in this boot's block.
    fn index_of(&self, task: TaskId) -> Option<usize> {
        let offset = task.checked_sub(self.base)?;
        if offset >= CLAIM_TASK_STRIDE {
            return None;
        }
        usize::try_from(offset).ok()
    }

    /// The calling thread's claim, if it is one of this boot's tasks.
    fn own_claim(&self) -> Option<Claim> {
        let claim = CLAIM.with(Cell::get)?;
        let ours = claim.boot.is_some_and(|boot| core::ptr::eq(boot, self));
        ours.then_some(claim)
    }

    fn own_index(&self) -> Option<usize> {
        self.index_of(self.own_claim()?.task)
    }
}

/// A thread admitted to a [`HostBoot`]: its task retires, as a thread's death
/// does, when this drops.
///
/// Deliberately not [`Send`]: the admission is the thread's own.
#[must_use = "dropping it retires the task at once"]
pub struct EnteredTask {
    boot: &'static HostBoot,
    task: TaskId,
    _not_send: PhantomData<*const ()>,
}

impl EnteredTask {
    /// The task's id.
    #[must_use]
    pub fn id(&self) -> TaskId {
        self.task
    }
}

impl Drop for EnteredTask {
    fn drop(&mut self) {
        self.boot.retire(self.task);
        CLAIM.with(|claim| claim.set(None));
        HOOK.with(|hook| hook.set(false));
    }
}

/// A task [`HostBoot::spawn`] runs on a thread of its own, answering what its
/// body returned.
#[must_use = "a task never joined runs on, detached"]
pub struct HostTask<T = ()> {
    boot: &'static HostBoot,
    task: TaskId,
    thread: std::thread::JoinHandle<T>,
}

impl<T> HostTask<T> {
    /// The task's id.
    #[must_use]
    pub fn id(&self) -> TaskId {
        self.task
    }

    /// Wait until the task retires, counted as a wait on it when the caller
    /// is a task of the same boot, and answer what its body returned.
    ///
    /// # Panics
    ///
    /// With the task's own panic, or if the join is a lost wake: every other
    /// live task already waits on another.
    // A unit task's join is called for its wait alone, so its value is not
    // one every caller must use.
    #[allow(clippy::must_use_candidate)]
    pub fn join(self) -> T {
        self.boot.await_retired(self.task);
        match self.thread.join() {
            Ok(value) => value,
            Err(panic) => std::panic::resume_unwind(panic),
        }
    }
}

/// A task asked to stop by a flag it reads after each park, as a kernel wait
/// reads a kill ([`HostBoot::spawn_until_stopped`]).
#[must_use = "a task never stopped runs on, detached"]
pub struct StoppableTask {
    task: HostTask,
    stop: Arc<AtomicBool>,
}

impl StoppableTask {
    /// The task's id.
    #[must_use]
    pub fn id(&self) -> TaskId {
        self.task.id()
    }

    /// Ask the task to stop, end the park it is in or cut its next one short,
    /// and wait for it to retire.
    ///
    /// # Panics
    ///
    /// As [`HostTask::join`].
    pub fn stop(self) {
        self.stop.store(true, Ordering::Release);
        let _ = self.task.boot.unpark(self.task.task);
        self.task.join();
    }

    /// Wait for the task to end on its own.
    ///
    /// # Panics
    ///
    /// As [`HostTask::join`].
    pub fn join(self) {
        self.task.join();
    }
}

/// Make the calling thread `boot`'s task `task`, running on `cpu`.
fn adopt(boot: &'static HostBoot, task: TaskId, cpu: CpuId) {
    let claim = Claim {
        cpu,
        task,
        boot: Some(boot),
    };
    CLAIM.with(|held| held.set(Some(claim)));
    HOOK.with(|hook| hook.set(true));
    make_line_row();
}

/// Wait on `condvar` under `state`; a task that panicked holding the lock
/// left the state consistent.
fn wait<'a>(condvar: &Condvar, state: MutexGuard<'a, BootState>) -> MutexGuard<'a, BootState> {
    condvar.wait(state).unwrap_or_else(PoisonError::into_inner)
}

/// A boot's resume handle: suspend the calling task as the scheduler would.
///
/// # Safety
///
/// `ctl` addresses a leaked [`HostBoot`].
unsafe fn suspend_task(ctl: NonNull<()>, action: TaskAction) {
    // SAFETY: the caller's contract; a leaked boot outlives every call, and
    // it is reached only through shared references.
    let boot = unsafe { ctl.cast::<HostBoot>().as_ref() };
    boot.suspend(action);
}

/// Serve the next call posted to `endpoint` as the calling boot task, as a
/// server does through `call_recv` and `call_reply`: received through the
/// kernel's own blocking receive, answered with the bytes of `reply` that
/// `answer` writes and counts, in the room the call came in, and the answer
/// waking whom `call_reply`'s wakes. A call withdrawn before it is answered
/// is answered into nothing.
///
/// # Errors
///
/// What ended the receive: [`Errno::Interrupted`] once `stopped` reports the
/// server ending, [`Errno::NotFound`] once the endpoint has gone, and
/// [`Errno::NotImplemented`] on a thread no boot parks.
pub fn serve_next(
    endpoint: &CallEndpoint,
    reply: &mut [u8],
    audit: &dyn Sink,
    stopped: &dyn Fn() -> bool,
    answer: impl FnOnce(&ReceivedCall, &mut [u8]) -> usize,
) -> Result<(), Errno> {
    let server = CLAIM.with(Cell::get).ok_or(Errno::NotImplemented)?.task;
    let call = crate::callreg::receive_call(
        endpoint,
        server,
        true,
        &mut crate::waitq::park_calling_task,
        stopped,
        || Ok(endpoint.recv_call(usize::MAX).into()),
    )?;
    let len = answer(&call, reply);
    if let Ok(woken) = endpoint.reply_with(call, &reply[..len], audit) {
        crate::waitq::call_woken(&woken);
    }
    Ok(())
}

/// Serve every call posted to `endpoint` from a further task of the calling
/// thread's boot, one [`serve_next`] at a time into a reply of the endpoint's
/// largest, until the task is stopped or the endpoint goes.
///
/// # Panics
///
/// If the calling thread is no boot's task.
pub fn serve(
    endpoint: Arc<CallEndpoint>,
    audit: &'static (dyn Sink + Sync),
    mut answer: impl FnMut(&ReceivedCall, &mut [u8]) -> usize + Send + 'static,
) -> StoppableTask {
    HostBoot::current().spawn_until_stopped(move |stopped| {
        let mut reply = std::vec![0; endpoint.max_reply() as usize];
        while serve_next(&endpoint, &mut reply, audit, stopped, &mut answer).is_ok() {}
    })
}

#[cfg(test)]
mod tests {
    use core::sync::atomic::{AtomicU32, Ordering};
    use std::panic::{catch_unwind, AssertUnwindSafe};
    use std::sync::{mpsc, Arc};

    use tairix_kernel_sched_api::TaskId;

    use super::{
        advance_clock, claim_cpu, claim_peer_task, claim_scheduler, claim_task, is_claimed_cpu,
        publish_hash_key, HostBoot, HostTask, CLAIMED_CPU_BASE, LOST_WAKE,
    };
    use crate::sleep::park_on_until;
    use crate::waitq::{park_calling_task, wait_arch, WaitQueue, NO_DEADLINE};

    #[test]
    fn a_claimed_cpu_is_one_no_other_claim_and_no_unpinned_test_names() {
        let first = claim_cpu();
        let second = claim_cpu();
        assert_ne!(first, second, "each claim is a slot of its own");
        for cpu in [first, second] {
            assert!(
                cpu >= CLAIMED_CPU_BASE,
                "above every unpinned or spelled id"
            );
            assert!(crate::cpu_state::get(cpu).is_some(), "inside the table");
            assert!(is_claimed_cpu(cpu));
        }
        assert!(!is_claimed_cpu(0));
        assert!(!is_claimed_cpu(CLAIMED_CPU_BASE - 1));
    }

    #[test]
    fn an_unclaimed_test_sees_no_wait_hook() {
        assert!(crate::waitq::wait_arch().is_none());
        assert_eq!(crate::waitq::wait_now_ns(), None);
    }

    #[test]
    fn a_claim_is_idempotent_and_answers_from_this_thread() {
        let (cpu, task) = claim_scheduler();
        assert_eq!(claim_scheduler(), (cpu, task));
        assert_eq!(claim_task(), task);
        let hook = crate::waitq::wait_arch().expect("the claim published a hook");
        assert_eq!(hook.current_cpu(), Some(cpu));
        assert_eq!(hook.current_task(0), Some(task));
    }

    #[test]
    fn a_task_claim_alone_publishes_no_wait_hook() {
        // A test that only needs an id of its own must not gain a scheduler
        // it never asked for: that would change what its park loops do.
        let task = claim_task();
        assert!(
            task >= super::CLAIM_TASK_BASE,
            "reserved above every hand-written id"
        );
        assert!(crate::waitq::wait_arch().is_none());
    }

    /// A test that models several principals gets an id per principal, all
    /// distinct, all inside its own reserved block.
    #[test]
    fn peer_task_ids_are_distinct_and_inside_this_claims_block() {
        let own = claim_task();
        let drawn = [own, claim_peer_task(), claim_peer_task()];
        let distinct: std::collections::BTreeSet<_> = drawn.iter().collect();
        assert_eq!(distinct.len(), drawn.len(), "one id per principal");
        for id in drawn {
            assert!(
                id >= super::CLAIM_TASK_BASE,
                "reserved above hand-written ids"
            );
            assert!(
                id - own < super::CLAIM_TASK_STRIDE,
                "inside this claim's own block, so no other claim can name it"
            );
        }
    }

    #[test]
    fn a_claimed_cpu_has_no_per_cpu_slot_so_a_park_fails_closed() {
        // What makes the fallback park deterministic: the claim names a CPU
        // the state table does not cover, so no other test's resume handle
        // can be found for it.
        let (cpu, _task) = claim_scheduler();
        assert!(crate::cpu_state::get(cpu).is_none());
    }

    #[test]
    fn the_clock_starts_at_zero_and_only_this_test_advances_it() {
        let _ = claim_scheduler();
        assert_eq!(crate::waitq::wait_now_ns(), Some(0));
        advance_clock(700);
        assert_eq!(crate::waitq::wait_now_ns(), Some(700));
    }

    #[test]
    fn the_hash_key_publication_is_the_same_for_every_caller() {
        publish_hash_key();
        publish_hash_key();
        assert_eq!(tairix_hash::published(), Some(super::TEST_HASH_KEY));
    }

    /// The text a panic carried.
    fn panic_text(payload: &(dyn core::any::Any + Send)) -> &str {
        payload
            .downcast_ref::<std::string::String>()
            .map(std::string::String::as_str)
            .or_else(|| payload.downcast_ref::<&'static str>().copied())
            .unwrap_or_default()
    }

    /// A task that wakes `task` once it has blocked.
    fn waker_of(boot: &'static HostBoot, task: TaskId) -> HostTask {
        boot.spawn(move || {
            boot.until_blocked(task);
            assert!(wait_arch().is_some_and(|hook| hook.unpark(task)));
        })
    }

    /// A wake the task is owed when it parks cuts the park short, never
    /// blocking; one that lands once it has blocked ends the block.
    #[test]
    fn a_wake_before_the_park_cuts_it_short_and_one_after_ends_it() {
        let boot = HostBoot::claim();
        let me = boot.task();
        assert!(wait_arch().is_some_and(|hook| hook.unpark(me)));
        assert!(park_calling_task());
        assert_eq!(boot.waits(me), 0, "the owed wake cut the park short");

        let waker = waker_of(boot, me);
        assert!(park_calling_task());
        assert_eq!(boot.waits(me), 1, "the park blocked until the wake");
        waker.join();
    }

    /// A wake issued from another test's boot leaves this boot's task alone:
    /// its row stays, and no token cuts its next park short.
    #[test]
    fn a_wake_from_another_boot_neither_wakes_nor_reaps() {
        let boot = HostBoot::claim();
        let me = boot.task();
        let queue = Arc::new(WaitQueue::new());
        queue.register(me, NO_DEADLINE).expect("filed");
        let theirs = Arc::clone(&queue);
        let foreign = std::thread::spawn(move || {
            let _other = HostBoot::claim();
            wait_arch().is_some_and(|hook| theirs.wake_task(hook, me))
        });
        assert!(
            foreign.join().expect("the other boot's task finishes"),
            "answered as landed, so the row is not reaped"
        );
        assert!(!queue.is_empty(), "the row is still this task's");

        let waker = waker_of(boot, me);
        assert!(park_calling_task());
        assert_eq!(boot.waits(me), 1, "no token cut the park short");
        waker.join();
        queue.deregister(me);
    }

    /// A task that retires while still registered, as a thread killed in a
    /// wait does, leaves no row: retirement drops it from every queue the
    /// kernel can name, and a wake that finds it on any other reaps it.
    #[test]
    fn a_retired_task_cannot_be_woken_and_leaves_no_row() {
        let boot = HostBoot::claim();
        let queue = Arc::new(WaitQueue::new());
        let theirs = Arc::clone(&queue);
        let gone = boot.spawn(move || {
            let task = boot.task();
            theirs.register(task, NO_DEADLINE).expect("filed");
            crate::waitq::SLEEP_WAITQ
                .register(task, NO_DEADLINE)
                .expect("filed");
        });
        let id = gone.id();
        gone.join();
        assert!(
            !crate::waitq::SLEEP_WAITQ.arm(id, NO_DEADLINE),
            "retirement dropped the row on a queue the kernel names"
        );
        let hook = wait_arch().expect("a boot's task has the hook");
        assert!(!hook.unpark(id), "a retired task can never run");
        assert!(!queue.wake_one(hook));
        assert!(queue.is_empty(), "the wake reaped the row");
    }

    /// Once every task waits on another nothing can wake any of them: the
    /// task that would block last fails, and wakes the rest to fail too.
    #[test]
    fn a_boot_whose_every_task_parks_fails_rather_than_hangs() {
        let boot = HostBoot::claim();
        let sleeper = boot.spawn(|| {
            let _ = park_calling_task();
        });
        boot.until_blocked(sleeper.id());
        let own = catch_unwind(park_calling_task).expect_err("no wake can come");
        let joined = AssertUnwindSafe(|| sleeper.join());
        let theirs = catch_unwind(joined).expect_err("woken to fail too");
        for payload in [own, theirs] {
            assert!(panic_text(payload.as_ref()).contains(LOST_WAKE));
        }
    }

    /// Advancing the boot's clock wakes its timed waiters to look again: short
    /// of the deadline a wait parks once more, and at it the wait ends.
    #[test]
    fn advancing_the_clock_wakes_the_boots_timed_waiters() {
        let boot = HostBoot::claim();
        let sleeper = boot.spawn(|| {
            let queue = WaitQueue::new();
            assert_eq!(park_on_until(&queue, 1_000, || false), Ok(false));
        });
        let id = sleeper.id();
        boot.until_blocked(id);
        boot.advance_clock(400);
        boot.until_blocked(id);
        assert_eq!(boot.waits(id), 2, "short of the deadline it parked again");
        boot.advance_clock(600);
        sleeper.join();
        assert_eq!(boot.waits(id), 2, "at the deadline the wait ended");
    }

    /// A waiter registered before it looks, as every kernel wait loop is,
    /// blocks exactly once per wake it waits for: no stale token cuts a park
    /// short, and it never spins.
    #[test]
    fn a_register_then_check_round_trip_blocks_exactly_once_per_wake() {
        const ROUNDS: u32 = 3;
        let boot = HostBoot::claim();
        let shared = Arc::new((WaitQueue::new(), AtomicU32::new(0)));
        let theirs = Arc::clone(&shared);
        let waiter = boot.spawn(move || {
            let (queue, posted) = &*theirs;
            let task = boot.task();
            for round in 1..=ROUNDS {
                queue.register(task, NO_DEADLINE).expect("filed");
                while posted.load(Ordering::Acquire) < round {
                    assert!(park_calling_task(), "a boot's task parks");
                }
                queue.deregister(task);
            }
        });
        let hook = wait_arch().expect("a boot's task has the hook");
        let (queue, posted) = &*shared;
        for round in 1..=ROUNDS {
            boot.until_blocked(waiter.id());
            posted.store(round, Ordering::Release);
            assert!(queue.wake_one(hook));
        }
        let id = waiter.id();
        waiter.join();
        assert_eq!(boot.waits(id), ROUNDS);
    }

    /// A thread entered into the boot is one of its tasks until its guard
    /// drops, which retires it.
    #[test]
    fn an_entered_thread_is_a_task_until_its_guard_drops() {
        let boot = HostBoot::claim();
        let (tx, rx) = mpsc::channel();
        let entered = std::thread::spawn(move || {
            let task = boot.enter();
            let _ = tx.send(task.id());
            assert!(park_calling_task());
        });
        let id = rx.recv().expect("the thread entered");
        boot.until_blocked(id);
        let hook = wait_arch().expect("a boot's task has the hook");
        assert!(hook.unpark(id));
        entered.join().expect("the entered task finishes");
        assert_eq!(boot.waits(id), 1);
        assert!(!hook.unpark(id), "its guard retired it");
    }

    /// A joined task answers what its body returned.
    #[test]
    fn a_joined_task_answers_what_its_body_returned() {
        let boot = HostBoot::claim();
        assert_eq!(boot.spawn(|| 7).join(), 7);
    }

    /// Stopping a task ends the park it is in, and a stop that lands before
    /// the task parks cuts its next park short: either way the task sees the
    /// request and retires, so no stop is lost.
    #[test]
    fn a_stopped_task_wakes_from_its_park_and_retires() {
        let boot = HostBoot::claim();
        let parks = |stopped: &dyn Fn() -> bool| {
            while !stopped() {
                assert!(park_calling_task(), "a boot's task parks");
            }
        };
        let parked = boot.spawn_until_stopped(parks);
        boot.until_blocked(parked.id());
        parked.stop();
        let early = boot.spawn_until_stopped(parks);
        early.stop();
    }

    /// An unparkable task's park fails closed, as a claim's does, while its
    /// wakes still reach the boot's tasks.
    #[test]
    fn an_unparkable_task_fails_closed_and_still_wakes_the_boot() {
        let boot = HostBoot::claim();
        let me = boot.task();
        let (tx, rx) = mpsc::channel();
        let unparkable = std::thread::spawn(move || {
            let _task = boot.enter_unparkable();
            let _ = tx.send(());
            assert!(!park_calling_task(), "no slot to park through");
            boot.until_blocked(me);
            assert!(wait_arch().is_some_and(|hook| hook.unpark(me)));
        });
        rx.recv().expect("the thread entered");
        assert!(park_calling_task());
        unparkable.join().expect("the unparkable task finishes");
        assert_eq!(boot.waits(me), 1);
    }
}
