//! The kernel-side process-signal seam the `signal` (`abi-v1` number 64)
//! syscall uses (`plans/SPAWN.md` SP7).
//!
//! [`ProcessSignal`] is the one object-safe boundary between the
//! arch-neutral syscall handler in `kernel/core` and the scheduler-side
//! producer that delivers a control signal. It carries the two halves
//! separately — resolving a pid against the sender's own children, and
//! delivering to an already-authorised target — because who may signal whom
//! is decided by the syscall handler alone (own child, else the target's own
//! principal, else `CAP_PROC_CONTROL`, `plans/NEW-TASKBAR.md` T11). Like the
//! [`ProcessWait`], [`ArchImageBuilder`](crate::spawn::ArchImageBuilder), and
//! [`MemMap`](crate::memmap::MemMap) seams, the concrete producer is
//! installed at boot through the `with_process_signal` builder and the
//! handler reaches it through this trait.
//!
//! Until a producer is installed the handler holds [`NULL_PROCESS_SIGNAL`],
//! which fails closed: both halves return [`Errno::NotImplemented`], never
//! pretending a signal was delivered — exactly as
//! [`NULL_PROCESS_WAIT`](crate::procwait::NULL_PROCESS_WAIT) does for
//! `wait`. The scheduler-side producer that actually delivers the signal is
//! [`KernelProcessSignal`] (`plans/SPAWN.md` `SP7b`), installed at boot in
//! place of the fail-closed floor.

use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

use tairix_abi::{Errno, ProcId, Signal};
use tairix_collections::HashMap;
use tairix_hash::BuildSipHash13;
use tairix_inline::ArrayVec;
use tairix_kernel_sched_api::{ExitDisposition, SchedError, SchedulerArch, SchedulerPolicy};
use tairix_kernel_sec::{CapTable, JobGeneration, ProcessId, TaskId};
use tairix_log::Sink;
use tairix_sync::once::OnceCell;
use tairix_sync::{IrqSafeSpinLock, RwLock, SpinLock};

use crate::dispatch_slot::RescheduleAction;
use crate::foreground::ForegroundOwner;
use crate::procwait::{KernelProcessWait, ProcessWait};

/// Per-task signal-intake state (`plans/STRESSTEST.md` ST3): key present
/// means the task opted in through `signal_intake` (`SignalIntakeOp::Enable`),
/// and the value is its **one** pending observed termination-request signal
/// (`Interrupt`/`Terminate`), `None` while nothing is pending.
///
/// A single slot, not a queue, by design: while one observed signal is
/// pending undrained, a second termination-request signal **escalates to
/// the default terminate path** ([`try_intake`] declines it), so an
/// opted-in process that stops draining stays killable with a plain
/// `^C ^C`. Entries are cleared on task teardown ([`clear_intake`], driven
/// by the shared reclaim) and never inherited: a fresh task id is never in
/// the map. Grows with the number of concurrently opted-in tasks, never a
/// fixed ceiling.
static SIGNAL_INTAKE: SpinLock<BTreeMap<u64, Option<Signal>>> = SpinLock::new(BTreeMap::new());

/// Opt `task` into observable delivery of its own `Interrupt`/`Terminate`.
/// Idempotent: enabling an already-enabled intake keeps its pending slot
/// (a recorded signal is never discarded by a re-enable).
pub fn intake_enable(task: u64) {
    SIGNAL_INTAKE.lock().entry(task).or_insert(None);
}

/// Restore `task`'s default terminate disposition.
///
/// Idempotent: already disabled is success. Refused with
/// [`Errno::WouldBlock`] while an observed signal is pending undrained — a
/// recorded termination request is never silently discarded; the caller
/// drains it ([`intake_take`]) and acts on it first.
///
/// # Errors
///
/// [`Errno::WouldBlock`] when a pending observed signal is undrained.
pub fn intake_disable(task: u64) -> Result<(), Errno> {
    let mut intake = SIGNAL_INTAKE.lock();
    match intake.get(&task) {
        Some(Some(_)) => Err(Errno::WouldBlock),
        Some(None) => {
            intake.remove(&task);
            Ok(())
        }
        None => Ok(()),
    }
}

/// Drain `task`'s one pending observed signal.
///
/// # Errors
///
/// [`Errno::NotFound`] when the intake was never enabled;
/// [`Errno::WouldBlock`] when nothing is pending (the intake stays
/// enabled — the caller parks on its wait-set member, never a poll loop).
pub fn intake_take(task: u64) -> Result<Signal, Errno> {
    match SIGNAL_INTAKE.lock().get_mut(&task) {
        Some(pending) => pending.take().ok_or(Errno::WouldBlock),
        None => Err(Errno::NotFound),
    }
}

/// Whether `task` has opted into signal observation — the `waitset_ctl`
/// add-time check for a `WaitSourceKind::Signal` member (without the
/// opt-in there is no intake to observe).
#[must_use]
pub fn intake_enabled(task: u64) -> bool {
    SIGNAL_INTAKE.lock().contains_key(&task)
}

/// Whether `task` has an observed signal pending undrained — the
/// non-consuming `waitset_wait` readiness peek for a
/// `WaitSourceKind::Signal` member (the woken owner drains through
/// [`intake_take`], never the wait).
#[must_use]
pub fn intake_ready(task: u64) -> bool {
    matches!(SIGNAL_INTAKE.lock().get(&task), Some(Some(_)))
}

/// Drop `task`'s intake state on teardown. Idempotent; driven by the one
/// shared task-reclaim path so an exited or killed task leaves no stale
/// opt-in or pending slot behind — the entry goes before the id can be drawn
/// again, so a later task never inherits one.
pub fn clear_intake(task: u64) {
    SIGNAL_INTAKE.lock().remove(&task);
}

/// One thread's kill gate: whether it is executing **inside the kernel on its
/// own stack**, and the death it owes.
///
/// A thread inside a kernel body — a syscall handler, the deferred-load body,
/// the user-fault resolver — holds state only its own unwind can release, so
/// its death is owed at the body's boundary (`plans/OPEN-DEFECTS.md` D112). A
/// thread outside the kernel holds none but may still be executing, so its
/// death is owed where the scheduler retires it.
///
/// A death is recorded before anything acts on it — recorded later, it could
/// arrive after the only point that looks for it — and exactly one party takes
/// it: the boundary, the dispatch loop once the scheduler has retired the
/// thread, or a killer that retired it itself. A thread entering a kernel body
/// owing one never runs the body. Where a death is owed is decided on the same
/// word the thread's own entry into the kernel sets, so a claim and an entry
/// are each one read-modify-write and linearise against each other.
///
/// The word also carries the job-control generation the thread was last
/// given. A thread outside any kernel body is stopped by the scheduler, but
/// one inside a body is never: it may hold kernel state another thread waits
/// on — a lock handed to it on release — so it stops itself at the edge of
/// the body instead, where it holds none ([`stop_at_edge`]).
///
/// The thread reaches its own gate through the copy its CPU publishes while it
/// runs, so a syscall touches no structure another thread contends on;
/// killers find it by id in the gate registry ([`install_gate`]).
pub(crate) struct ThreadGate {
    /// The thread this gate is.
    task: u64,
    /// The process a death owed here tears down: the thread's own, for life.
    process: ProcessId,
    /// [`IN_KERNEL`], the owed death's kind, the job-control generation, and
    /// an `Exit`'s status.
    word: AtomicU64,
}

/// Set between a kernel body's entry and its boundary, parked or running.
const IN_KERNEL: u64 = 1;
/// A [`DeferredTeardown::Exit`] is owed; its status is the high half.
const OWES_EXIT: u64 = 1 << 1;
/// A [`DeferredTeardown::Plain`] is owed.
const OWES_PLAIN: u64 = 1 << 2;
const OWED: u64 = OWES_EXIT | OWES_PLAIN;
/// The [`JobGeneration`] the thread was last given, between the owed death's
/// kind and its status.
const JOB_SHIFT: u32 = 3;
const JOB_FIELD: u64 = ((1 << JobGeneration::BITS) - 1) << JOB_SHIFT;
const STATUS_SHIFT: u32 = 32;
const _: () = assert!(JOB_SHIFT + JobGeneration::BITS <= STATUS_SHIFT);

/// What a thread at the edge of a kernel body owes before it may go on.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Owed {
    /// Nothing: the body runs.
    Nothing,
    /// A job-control stop, taken here, where the thread holds no kernel state.
    Stop,
    /// A death: the body is skipped and the boundary lands it.
    Death,
}

/// What leaving a kernel body found.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Leave {
    /// The thread has left the kernel.
    Left,
    /// The thread is still in the kernel and owes a stop it takes first.
    Stop,
    /// The thread has left the kernel owing this death, taken here.
    Death(DeferredTeardown),
}

/// The `Exit` status a gate word's high half holds.
const fn status_of(word: u64) -> i32 {
    let [_, _, _, _, a, b, c, d] = word.to_le_bytes();
    i32::from_le_bytes([a, b, c, d])
}

impl ThreadGate {
    const fn new(task: u64, process: ProcessId) -> Self {
        Self {
            task,
            process,
            word: AtomicU64::new(0),
        }
    }

    /// The job-control generation `word` holds.
    fn job_in(word: u64) -> JobGeneration {
        JobGeneration::from_bits(u32::try_from((word & JOB_FIELD) >> JOB_SHIFT).unwrap_or(0))
    }

    /// `word` holding `job` as its generation.
    fn with_job(word: u64, job: JobGeneration) -> u64 {
        (word & !JOB_FIELD) | (u64::from(job.bits()) << JOB_SHIFT)
    }

    fn owed_at(word: u64) -> Owed {
        if word & OWED != 0 {
            Owed::Death
        } else if Self::job_in(word).is_stopped() {
            Owed::Stop
        } else {
            Owed::Nothing
        }
    }

    /// The death `word` records: a [`DeferredTeardown`] rather than a status,
    /// since a driver unload's carries none.
    fn owed_in(&self, word: u64) -> Option<DeferredTeardown> {
        match word & OWED {
            OWES_EXIT => Some(DeferredTeardown::Exit {
                process: self.process,
                status: status_of(word),
            }),
            OWES_PLAIN => Some(DeferredTeardown::Plain {
                process: self.process,
            }),
            _ => None,
        }
    }

    fn owing(teardown: DeferredTeardown) -> u64 {
        match teardown {
            DeferredTeardown::Exit { status, .. } => {
                OWES_EXIT | u64::from(status.cast_unsigned()) << STATUS_SHIFT
            }
            DeferredTeardown::Plain { .. } => OWES_PLAIN,
        }
    }

    /// The process a death owed here tears down — the thread's own.
    #[must_use]
    pub(crate) const fn process(&self) -> ProcessId {
        self.process
    }

    /// Mark the thread as executing inside the kernel on its own stack,
    /// reporting what it owes before its body may run. Paired with
    /// [`Self::leave`] by every kernel body a thread runs on its own stack.
    ///
    /// [`Owed::Death`] obliges the caller to skip its body and go straight to
    /// its boundary, which lands the death: a thread owing one may already
    /// have been told to die, and the scheduler retires such a thread at its
    /// next stopping point — which, inside a body, would free a stack whose
    /// frames still own kernel state. [`Owed::Stop`] obliges it to take the
    /// stop first ([`stop_at_edge`]) and ask again ([`Self::owed`]).
    #[must_use]
    pub(crate) fn enter(&self) -> Owed {
        Self::owed_at(self.word.fetch_or(IN_KERNEL, Ordering::AcqRel))
    }

    /// What the thread owes now: asked again at an edge once a stop taken
    /// there has ended.
    #[must_use]
    pub(crate) fn owed(&self) -> Owed {
        Self::owed_at(self.ordered())
    }

    /// Leave a kernel body, taking any death the thread owes.
    ///
    /// [`Leave::Death`] obliges the caller to land the death now: the body has
    /// unwound (every lock and buffer it held is released), so this is the
    /// first safe point the thread can die at, and it never returns to user
    /// mode. A thread `returning` to user mode while a stop is owed stays in
    /// the kernel and takes the stop first ([`Leave::Stop`]); one leaving for
    /// good — an exit — owes no stop. The generation itself stays: only a
    /// continue moves it.
    #[must_use]
    pub(crate) fn leave(&self, returning: bool) -> Leave {
        let mut word = self.word.load(Ordering::Acquire);
        loop {
            if returning && Self::owed_at(word) == Owed::Stop {
                return Leave::Stop;
            }
            match self.word.compare_exchange_weak(
                word,
                word & JOB_FIELD,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(previous) => return self.taken(previous).map_or(Leave::Left, Leave::Death),
                Err(current) => word = current,
            }
        }
    }

    /// Whether a death is owed here.
    ///
    /// The in-kernel park loops consult this after every wake and unwind with
    /// `Errno::Interrupted` instead of re-parking, so a doomed thread reaches
    /// its boundary promptly rather than sleeping on as an unkillable waiter.
    /// The errno never reaches user space — the boundary lands the death first.
    #[must_use]
    pub(crate) fn kill_pending(&self) -> bool {
        self.owed_in(self.word.load(Ordering::Acquire)).is_some()
    }

    /// Record `teardown` unless a death is already owed, reporting whether
    /// this claim recorded it and where the death is owed.
    fn claim(&self, teardown: DeferredTeardown) -> (bool, KillSite) {
        self.claim_then(teardown, || {})
    }

    /// [`Self::claim`], running `recorded` the moment the death is on the
    /// word and so takeable.
    fn claim_then(&self, teardown: DeferredTeardown, recorded: impl FnOnce()) -> (bool, KillSite) {
        // Counted before it can be owed, and uncounted only after: the count
        // may read high but never below the deaths owed, so a dispatch that
        // reads it zero has none to land.
        OWED_KILLS.fetch_add(1, Ordering::Relaxed);
        let mut word = self.word.load(Ordering::Acquire);
        loop {
            let site = if word & IN_KERNEL == 0 {
                KillSite::Retire
            } else {
                KillSite::Boundary
            };
            if word & OWED != 0 {
                OWED_KILLS.fetch_sub(1, Ordering::Relaxed);
                return (false, site);
            }
            match self.word.compare_exchange_weak(
                word,
                word | Self::owing(teardown),
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    recorded();
                    return (true, site);
                }
                Err(current) => word = current,
            }
        }
    }

    /// Take the death owed here, leaving the in-kernel mark and the
    /// job-control generation as they were.
    fn take_owed(&self) -> Option<DeferredTeardown> {
        self.taken(self.word.fetch_and(IN_KERNEL | JOB_FIELD, Ordering::AcqRel))
    }

    /// The word, read as a read-modify-write: it sees every other party's
    /// last write, and is ordered against them through the word's own
    /// modification order, so two parties each acting on one location and
    /// then reading this one cannot both miss each other.
    fn ordered(&self) -> u64 {
        self.word.fetch_or(0, Ordering::AcqRel)
    }

    /// Give a thread joining its group the group's generation as its own.
    ///
    /// Unconditional, since a fresh gate holds no decision to keep: compared,
    /// a group past half the generation space would read older than the
    /// fresh gate's, and the thread would ignore its job control.
    pub(crate) fn adopt_job(&self, job: JobGeneration) {
        let _ = self
            .word
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |word| {
                Some(Self::with_job(word, job))
            });
    }

    /// Give the thread `job`, unless it already holds a newer one: a fan-out
    /// that falls behind the next one cannot undo it.
    pub(crate) fn apply_job(&self, job: JobGeneration) {
        let _ = self
            .word
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |word| {
                job.is_newer_than(Self::job_in(word))
                    .then(|| Self::with_job(word, job))
            });
    }

    /// Advance the thread's own generation toward `stopped`, returning the
    /// generation that decides it and whether this call moved it: the job
    /// control of a process no thread-group table keeps, which is its one
    /// thread.
    fn advance_job(&self, stopped: bool) -> (JobGeneration, bool) {
        let mut word = self.word.load(Ordering::Acquire);
        loop {
            let current = Self::job_in(word);
            let next = current.toward(stopped);
            if next == current {
                return (current, false);
            }
            match self.word.compare_exchange_weak(
                word,
                Self::with_job(word, next),
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return (next, true),
                Err(seen) => word = seen,
            }
        }
    }

    /// The death a word just replaced recorded, keeping the owed count in
    /// step with it.
    fn taken(&self, previous: u64) -> Option<DeferredTeardown> {
        let taken = self.owed_in(previous);
        if taken.is_some() {
            OWED_KILLS.fetch_sub(1, Ordering::Relaxed);
        }
        taken
    }
}

/// Every live thread's gate, by task id, for the parties that reach a thread
/// other than the one running: killers, the dispatch loop's landing, and the
/// in-kernel park loops' check after a wake. A thread's own syscalls never
/// read it.
///
/// Keyed under the per-boot hash key: which ids stay registered is shaped by
/// what an unprivileged user spawns and keeps, so an unkeyed table would let
/// one pile its threads into one bucket. Built on first use; a boot that never
/// got a key hashes unkeyed, the honest fallback the futex table takes.
static GATES: RwLock<Option<HashMap<u64, Arc<ThreadGate>, BuildSipHash13>>> = RwLock::new(None);

/// How many deaths the gates owe, so the dispatch loop's per-dispatch
/// [`land_retired_kill`] is one relaxed load and no lookup while nothing is
/// owed anywhere.
static OWED_KILLS: AtomicUsize = AtomicUsize::new(0);

/// Give `task`, a thread of `process`, its kill gate.
///
/// Every admission of a user thread installs one before the thread can run —
/// before its first syscall, and before it joins a group a kill could claim —
/// and the kthread dispatch shim adopts it at the thread's first dispatch. A
/// thread without one is never claimed, so no kill reaches it.
///
/// # Errors
///
/// [`Errno::OutOfMemory`] when the gate or the registry cannot grow; the
/// admission is then refused. [`Errno::AlreadyExists`] when `task` already has
/// a gate.
pub fn install_gate(task: u64, process: ProcessId) -> Result<(), Errno> {
    let gate = Arc::try_new(ThreadGate::new(task, process)).map_err(|_| Errno::OutOfMemory)?;
    let mut gates = GATES.write();
    let gates = gates.get_or_insert_with(|| {
        HashMap::with_hasher(BuildSipHash13::keyed().unwrap_or(BuildSipHash13::UNKEYED))
    });
    if gates.get(&task).is_some() {
        return Err(Errno::AlreadyExists);
    }
    gates
        .try_insert(task, gate)
        .map(|_| ())
        .map_err(|_| Errno::OutOfMemory)
}

/// The gate of `task`, while it has one.
#[must_use]
pub(crate) fn gate_of(task: u64) -> Option<Arc<ThreadGate>> {
    GATES
        .read()
        .as_ref()
        .and_then(|gates| gates.get(&task).cloned())
}

/// What teardown a thread's death owes. Both kinds reclaim the dying
/// **process's** kernel resources once its last thread is down; they differ
/// only in whether a parent's `wait` is also given a status.
///
/// The death is keyed by the *thread* that owes it — that is what a boundary
/// and the dispatch loop can recognise — but the teardown names the
/// **process**, because reclaiming an address space, capability record,
/// endpoints, and open files is a process operation.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum DeferredTeardown {
    /// The process dies carrying this terminal status: record it for the
    /// parent's `wait` **and** reclaim, once the group's last thread is down.
    Exit {
        /// The process to reap and reclaim.
        process: ProcessId,
        /// The status the parent's `wait` reports — a signal's `128 + n`, a
        /// group `exit`'s own code, or a fault kill's crash status.
        status: i32,
    },
    /// A teardown nobody reaps (an unloaded driver): reclaim the process's
    /// kernel resources only.
    Plain {
        /// The process to reclaim.
        process: ProcessId,
    },
}

impl DeferredTeardown {
    /// The process this death tears down.
    #[must_use]
    pub const fn process(self) -> ProcessId {
        match self {
            Self::Exit { process, .. } | Self::Plain { process } => process,
        }
    }

    /// The status a parent's `wait` reports, or [`None`] when nobody reaps
    /// this death (an unloaded driver is not a waited-for child).
    #[must_use]
    pub const fn reaped_status(self) -> Option<i32> {
        match self {
            Self::Exit { status, .. } => Some(status),
            Self::Plain { .. } => None,
        }
    }
}

/// Where a claimed death is owed.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum KillSite {
    /// Inside a kernel body: its boundary lands the death, and the killer
    /// wakes the thread so a body parked in the kernel unwinds to it.
    Boundary,
    /// Outside the kernel: the killer asks the scheduler to retire the thread,
    /// and the death lands wherever that retire happens.
    Retire,
}

/// One thread's share of a group death recorded by [`claim_group_kill`].
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ClaimedKill {
    /// The thread that owes the death.
    pub thread: u64,
    /// Where it is owed.
    pub site: KillSite,
    /// Whether this claim recorded the death, rather than finding one another
    /// death already owed, which this claim must not undo.
    pub recorded: bool,
}

/// Record `teardown` as the death every live thread of its process owes, but
/// `spare`, and report where each is owed.
///
/// The thread set is read, and every death recorded, under the thread-group
/// table's read lock, which makes the claim exact against both ends of a
/// thread's life: a thread registered after it sees its creator's death owed
/// and is refused ([`kill_pending`]), and a thread's teardown withdraws its
/// membership before it clears its gate, so no death is recorded that the
/// teardown would not clear. A build with no table treats the process as its
/// single leader thread.
pub fn claim_group_kill(
    caps: Option<&RwLock<CapTable>>,
    teardown: DeferredTeardown,
    spare: Option<u64>,
) -> Vec<ClaimedKill> {
    let process = teardown.process();
    let members = caps.map(RwLock::read);
    let threads: Vec<u64> = match &members {
        Some(table) => table.threads_of(process).map(|thread| thread.0).collect(),
        None => alloc::vec![process.leader_task().0],
    };
    claim_threads(threads, teardown, spare)
}

/// [`claim_group_kill`] for the process `instance`, and only while its number
/// still names that instance: a number drawn again after its holder died is
/// a different process, and is never claimed for the first one's death.
pub fn claim_instance_kill(
    caps: &RwLock<CapTable>,
    instance: ProcId,
    teardown: DeferredTeardown,
) -> Vec<ClaimedKill> {
    let process = teardown.process();
    let table = caps.read();
    if table.instance_of(process) != instance {
        return Vec::new();
    }
    let threads: Vec<u64> = table.threads_of(process).map(|thread| thread.0).collect();
    claim_threads(threads, teardown, None)
}

/// Record `teardown` against each of `threads` but `spare`. The caller holds
/// the thread-group table's read lock across this, which is what makes a claim
/// exact against a thread's admission and teardown: a member always has its
/// gate, installed before it joined and removed only after it left.
fn claim_threads(
    threads: Vec<u64>,
    teardown: DeferredTeardown,
    spare: Option<u64>,
) -> Vec<ClaimedKill> {
    let mut claims = Vec::with_capacity(threads.len());
    let gates = GATES.read();
    for thread in threads.into_iter().filter(|thread| Some(*thread) != spare) {
        let Some(gate) = gates.as_ref().and_then(|gates| gates.get(&thread)) else {
            continue;
        };
        let (recorded, site) = gate.claim(teardown);
        claims.push(ClaimedKill {
            thread,
            site,
            recorded,
        });
    }
    claims
}

/// Take the death `task` owes, for a killer whose scheduler call retired the
/// thread itself (or found no thread to retire) and so owns the landing.
#[must_use]
pub fn take_owed_kill(task: u64) -> Option<DeferredTeardown> {
    gate_of(task).and_then(|gate| gate.take_owed())
}

/// Whether a death is owed by `task`: the in-kernel park loops ask after
/// every wake, and unwind with `Errno::Interrupted` rather than park again.
///
/// A thread asking about itself — every park loop does — reads the gate its
/// CPU publishes for it; only a question about another thread reaches the
/// registry.
#[must_use]
pub fn kill_pending(task: u64) -> bool {
    crate::waitq::wait_arch()
        .and_then(crate::waitq::WaitQueueArch::current_cpu)
        .and_then(|cpu| {
            crate::kthread::with_current_gate(cpu, |gate| {
                (gate.task == task).then(|| gate.kill_pending())
            })
        })
        .flatten()
        .unwrap_or_else(|| gate_of(task).is_some_and(|gate| gate.kill_pending()))
}

/// Drop every trace of `task` from the gates on teardown — any death it owes,
/// and its registration. Idempotent; driven by the one shared thread teardown
/// once the thread has left its group, so a death claimed while it was still
/// a member is cleared here and none can be claimed after.
pub fn clear_kill_gate(task: u64) {
    let removed = GATES.write().as_mut().and_then(|gates| gates.remove(&task));
    if let Some(gate) = removed {
        let _ = gate.take_owed();
    }
}

/// A job-control decision for one process and the threads it governs.
struct JobTransition {
    /// The generation that decides the process.
    job: JobGeneration,
    /// Whether this decision moved the process, rather than restating it.
    moved: bool,
    /// Every live thread of the process, with its gate.
    members: Vec<(u64, Arc<ThreadGate>)>,
}

/// What a fan-out leaves a thread's scheduler state as, for the gate word it
/// now reads.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum JobHold {
    /// Stopped by the scheduler: the thread is outside any kernel body.
    Hold,
    /// Released: no stop is owed.
    Release,
    /// Left alone: the thread stops itself at its kernel edge, or owes a
    /// death that its killer brings it to.
    Leave,
}

fn job_hold(word: u64) -> JobHold {
    if word & OWED != 0 {
        JobHold::Leave
    } else if !ThreadGate::job_in(word).is_stopped() {
        JobHold::Release
    } else if word & IN_KERNEL != 0 {
        JobHold::Leave
    } else {
        JobHold::Hold
    }
}

/// Take the stop the calling thread owes at an edge of a kernel body, where
/// it holds no kernel state, returning once no stop is owed — `false` when no
/// stop can be taken here (no scheduler hook yet, a thread already retired),
/// and the edge goes on without one.
///
/// The thread stops itself through the scheduler, then reads its gate as a
/// read-modify-write: a continue moves the generation on that word before it
/// resumes the thread, so either the read sees the continue and the stop is
/// withdrawn here, or the continue's resume follows the stop and ends it.
pub(crate) fn stop_at_edge(gate: &ThreadGate) -> bool {
    crate::waitq::wait_arch().is_some_and(|hook| {
        stop_through(gate, hook, |cpu| {
            crate::kthread::reschedule_current(cpu, RescheduleAction::Yield)
        })
    })
}

/// [`stop_at_edge`] through `hook`, suspending the stopped thread with
/// `suspend`, which reports whether it could.
fn stop_through(
    gate: &ThreadGate,
    hook: &dyn crate::waitq::WaitQueueArch,
    mut suspend: impl FnMut(tairix_kernel_sched_api::CpuId) -> bool,
) -> bool {
    loop {
        if gate.owed() != Owed::Stop {
            return true;
        }
        let Some(cpu) = hook.current_cpu() else {
            return false;
        };
        if !hook.stop(gate.task) {
            return false;
        }
        if gate.owed() != Owed::Stop {
            let _ = hook.resume(gate.task);
            return true;
        }
        if !suspend(cpu) {
            let _ = hook.resume(gate.task);
            return false;
        }
    }
}

/// Enter a kernel body on `gate`, taking first any stop it owes at this edge;
/// reports whether the body may run, which it may not while a death is owed.
pub(crate) fn enter_body(gate: &ThreadGate) -> bool {
    enter_body_through(gate, stop_at_edge)
}

/// [`enter_body`], taking an owed stop through `stop`, which reports whether
/// it could.
fn enter_body_through(gate: &ThreadGate, mut stop: impl FnMut(&ThreadGate) -> bool) -> bool {
    let mut owed = gate.enter();
    while owed == Owed::Stop {
        if !stop(gate) {
            // No stop can be taken here, but a death still skips the body.
            return gate.owed() != Owed::Death;
        }
        owed = gate.owed();
    }
    owed == Owed::Nothing
}

/// Leave a kernel body on `gate`, taking first any stop it owes when the
/// thread is `returning` to user mode, and return the death it owes.
pub(crate) fn leave_body(gate: &ThreadGate, returning: bool) -> Option<DeferredTeardown> {
    let mut returning = returning;
    loop {
        match gate.leave(returning) {
            Leave::Left => return None,
            Leave::Death(teardown) => return Some(teardown),
            Leave::Stop => returning = stop_at_edge(gate),
        }
    }
}

/// The seam through which the dispatch loop lands a retired thread's death:
/// records the status so the parent's `wait` reaps it and reclaims the
/// process's kernel resources — the same reap and reclaim a boundary
/// performs. Installed per boot by [`install_deferred_kill_lander`] (the
/// leaked [`KernelProcessSignal`] holds both the wait producer and the reclaim
/// seam), so the free-function dispatch loop can drive it without borrowing
/// the producer directly.
pub trait DeferredKillLander: Sync {
    /// Land `teardown`, the death the retired `task` owed. The death has
    /// already been taken from the gate, so this is called exactly once.
    fn land_deferred_teardown(&self, task: TaskId, teardown: DeferredTeardown);

    /// Kill `process` as [`Signal::Kill`] does, provided it is still the
    /// instance `instance`, reporting whether this kill recorded its death and
    /// reached a thread of it — how [`end_session`] ends a member without any
    /// authority check, the member's session having ended being the whole of
    /// the authority. A member already dying of something else is reached but
    /// not recorded, so it is not reported as ended with its session.
    fn kill_session_member(&self, process: ProcessId, instance: ProcId) -> bool;
}

/// The one deferred-kill lander shared by the dispatch loop.
static DEFERRED_KILL_LANDER: OnceCell<&'static (dyn DeferredKillLander + 'static)> =
    OnceCell::new();

/// Error returned when [`install_deferred_kill_lander`] is called twice.
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub struct DeferredKillLanderAlreadyInstalled;

/// Publish the [`DeferredKillLander`] the dispatch loop drives (the boot
/// path's leaked signal producer). Set-once: a second call fails closed
/// rather than re-pointing the live seam.
///
/// # Errors
///
/// [`DeferredKillLanderAlreadyInstalled`] if a lander was already installed.
pub fn install_deferred_kill_lander(
    lander: &'static (dyn DeferredKillLander + 'static),
) -> Result<(), DeferredKillLanderAlreadyInstalled> {
    DEFERRED_KILL_LANDER
        .set(lander)
        .map_err(|_| DeferredKillLanderAlreadyInstalled)
}

/// Land the death `task` owes once its dispatch has returned, if the
/// scheduler has retired it. Called from the dispatch loop after every
/// dispatched task, with `retired` answering whether the scheduler has.
///
/// A dispatch returns on a park and a yield as well as on a retire, and a
/// thread owing a death may be queued again or parked in a kernel body when it
/// does; only a retired thread executes nowhere and never will again, so only
/// its death is landed here. `retired` is consulted only while some death is
/// owed, so the common dispatch pays one relaxed load. Idempotent: a death is
/// taken exactly once, and a thread whose own teardown already ran left
/// nothing to take.
pub fn land_retired_kill(task: u64, retired: impl FnOnce() -> bool) {
    if OWED_KILLS.load(Ordering::Relaxed) == 0 {
        return;
    }
    // A lander must be installed before any death can be claimed; fail closed
    // (leave the death owed) rather than dropping it if not.
    let Ok(Some(lander)) = DEFERRED_KILL_LANDER.get() else {
        return;
    };
    if !retired() {
        return;
    }
    if let Some(teardown) = take_owed_kill(task) {
        lander.land_deferred_teardown(TaskId(task), teardown);
    }
}

/// Members an ending session's walk takes per pass. The table is released
/// between passes, so this bounds only the stack a pass borrows.
const SESSION_WALK_BATCH: usize = 8;

/// One member a walk pass collected under the table's read lock.
struct SessionMember {
    process: ProcessId,
    instance: ProcId,
    name: tairix_kernel_sec::ProcName,
}

/// End the session anchored at `anchor`, whose anchor has just died: kill
/// every process in it and in every session nested in it, recording each
/// (`docs/src/architecture/sessions.md`).
///
/// Driven once the anchor's own teardown has finished. The session is handed
/// to the reaper ([`crate::session_reaper`]), which walks it off this path;
/// only a session the reaper does not take is walked here. A kernel with no
/// lander installed kills nothing — it has no scheduler to drive a death
/// through.
pub fn end_session(caps: &RwLock<CapTable>, audit: &(dyn Sink + Sync), anchor: ProcId) {
    if let Ok(Some(lander)) = DEFERRED_KILL_LANDER.get() {
        if !crate::session_reaper::hand_over(caps, anchor) {
            end_session_through(caps, audit, anchor, *lander, &mut || {});
        }
    }
}

/// [`end_session`] walked on the calling task, which `pause` offers back to
/// the scheduler after each member.
pub(crate) fn end_session_now(
    caps: &RwLock<CapTable>,
    audit: &(dyn Sink + Sync),
    anchor: ProcId,
    pause: &mut dyn FnMut(),
) {
    if let Ok(Some(lander)) = DEFERRED_KILL_LANDER.get() {
        end_session_through(caps, audit, anchor, *lander, pause);
    }
}

/// The walk behind [`end_session`], through `lander`, the seam that drives
/// each death.
///
/// It does nothing unless the session is ending with members left, and nothing
/// while a session enclosing it is ending too: that session's walk reaches
/// every member of this one, which keeps a cascade of nested ends one walk
/// deep.
fn end_session_through(
    caps: &RwLock<CapTable>,
    audit: &(dyn Sink + Sync),
    anchor: ProcId,
    lander: &dyn DeferredKillLander,
    pause: &mut dyn FnMut(),
) {
    {
        let table = caps.read();
        let sessions = table.sessions();
        if !sessions.is_ending(anchor) || sessions.enclosing_ending(anchor) {
            return;
        }
    }
    let mut after = None;
    loop {
        let mut pass: ArrayVec<SessionMember, SESSION_WALK_BATCH> = ArrayVec::new();
        {
            let table = caps.read();
            for process in table.sessions().members_after(anchor, after) {
                let Some(record) = table.caps_of_process(process) else {
                    continue;
                };
                let member = SessionMember {
                    process,
                    instance: record.proc_id(),
                    name: tairix_kernel_sec::ProcName::from_bytes_truncating(
                        record.name().as_bytes(),
                    ),
                };
                if pass.try_push(member).is_err() {
                    break;
                }
            }
        }
        let Some(last) = pass.as_slice().last() else {
            return;
        };
        after = Some(last.process);
        for member in pass.as_slice() {
            if lander.kill_session_member(member.process, member.instance) {
                audit_member_ended(audit, member, anchor);
            }
            pause();
        }
    }
}

fn audit_member_ended(audit: &(dyn Sink + Sync), member: &SessionMember, anchor: ProcId) {
    let mut proc_hex = [0u8; tairix_abi::PROC_ID_HEX_LEN];
    let mut session_hex = [0u8; tairix_abi::PROC_ID_HEX_LEN];
    crate::audit::emit(
        audit,
        tairix_log::Level::Info,
        crate::audit::AuditEvent::SessionMemberEnded,
        &[
            tairix_log::Field {
                key: "task",
                value: tairix_log::FieldValue::UnsignedInt(member.process.0),
            },
            tairix_log::Field {
                key: "proc",
                value: tairix_log::FieldValue::Str(member.instance.write_hex(&mut proc_hex)),
            },
            tairix_log::Field {
                key: "comm",
                value: tairix_log::FieldValue::Str(member.name.as_str()),
            },
            tairix_log::Field {
                key: "session",
                value: tairix_log::FieldValue::Str(anchor.write_hex(&mut session_hex)),
            },
        ],
    );
}

/// Try to record a termination-request `signal` as `target`'s observable
/// pending event instead of terminating it.
///
/// Returns `true` when the signal was recorded (the delivery is complete:
/// the target's wait-set waiter is woken and will drain it). Returns
/// `false` when the default terminate path must run instead — the target
/// never opted in, or its pending slot is already occupied (the
/// escalation rule: a second undrained termination request kills, so an
/// unresponsive opted-in program stays terminable with plain `^C ^C`).
///
/// Only `Interrupt`/`Terminate` are ever offered here; `Kill` is
/// unconditionally fatal and unmaskable, so no caller routes it through
/// the intake. `pub(crate)` solely so the `waitset_wait` host tests can
/// stage a pending observation; production deliveries flow only through
/// [`KernelProcessSignal`].
pub(crate) fn try_intake(target: u64, signal: Signal) -> bool {
    let recorded = match SIGNAL_INTAKE.lock().get_mut(&target) {
        Some(pending @ None) => {
            *pending = Some(signal);
            true
        }
        Some(Some(_)) | None => false,
    };
    if recorded {
        // Wake the target's parked wait-set waiter (if any) outside the
        // intake lock; the wake is targeted, so unrelated waiters sleep on.
        crate::waitq::signal_intake_wake(target);
    }
    recorded
}

/// The one place a foreground `^C`/`^Z` signal is actually delivered.
///
/// Implemented by the scheduler-side signal producer and installed at boot
/// beside `with_process_signal`; the console line discipline reaches it only
/// through [`queue_foreground_signal`] / [`drain_pending_foreground`], never
/// directly, because the queueing side may run in interrupt context where
/// scheduler locks must not be taken.
pub trait ForegroundSignal: Sync {
    /// Deliver `signal` to the terminal's recorded foreground owner.
    ///
    /// The authority was established when the parent marked the task
    /// foreground through `console_foreground` (a live child of the caller
    /// on that console); by delivery time the task may already have exited,
    /// in which case the delivery fails closed with [`Errno::NotFound`] and
    /// signals no one. `owner` carries the process *instance* the ownership
    /// was granted at, not merely its pid, because an id whose task is gone
    /// may be drawn again: an implementation must refuse rather than signal
    /// whichever process holds the number at delivery time.
    ///
    /// # Errors
    ///
    /// [`Errno::NotFound`] when the target no longer exists, or the pid now
    /// names a different process instance; [`Errno::OutOfRange`] for a signal
    /// the line discipline never maps.
    fn deliver(&self, owner: ForegroundOwner, signal: Signal) -> Result<(), Errno>;
}

/// The boot-installed [`ForegroundSignal`] hook (set-once per boot).
static FOREGROUND_SIGNAL: OnceCell<&'static (dyn ForegroundSignal + 'static)> = OnceCell::new();

/// Error returned when [`install_foreground_signal`] is called twice.
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub struct ForegroundSignalAlreadyInstalled;

/// Publish the foreground-signal producer the console line discipline
/// delivers through. Called once by the boot path, beside
/// `with_process_signal`.
///
/// # Errors
///
/// [`ForegroundSignalAlreadyInstalled`] if a producer was already published.
pub fn install_foreground_signal(
    hook: &'static (dyn ForegroundSignal + 'static),
) -> Result<(), ForegroundSignalAlreadyInstalled> {
    FOREGROUND_SIGNAL
        .set(hook)
        .map_err(|_| ForegroundSignalAlreadyInstalled)
}

/// Whether a foreground-signal producer has been installed.
///
/// The console line discipline consults this before consuming a `^C`/`^Z`
/// byte: with no producer the byte flows to the reader unchanged (the inert
/// pre-install behaviour) rather than being swallowed with no one to act.
#[must_use]
pub fn foreground_signal_installed() -> bool {
    matches!(FOREGROUND_SIGNAL.get(), Ok(Some(_)))
}

/// Retires one thread of a dying process and, once the group's **last** thread
/// is down, tears the process itself down — the seam through which the
/// signal-terminate path drives the one
/// `KernelSyscallHandlers::land_thread_down` every death path shares (the
/// `exit` and `thread_exit` syscalls, the fault kill, the deferred kill, and a
/// driver unload).
///
/// Installed per producer instance
/// ([`KernelProcessSignal::install_task_reclaim`]) rather than through a
/// process-global slot, so each host-test fixture observes only its own
/// terminations.
pub trait TaskReclaim: Sync {
    /// Retire `thread` from `process`'s thread group and, when it was the last
    /// thread of the group still executing, record `status` (when a `wait` reap
    /// is owed) and release every kernel-held resource of the process.
    ///
    /// Called once per thread death, which the kill gate guarantees by handing
    /// each death it records to exactly one landing. Not idempotent: a thread
    /// the group table no longer holds reads as the last one down, so a second
    /// call would tear the process down again.
    fn land_thread_down(&self, process: ProcessId, thread: TaskId, status: Option<i32>);
}

/// Error returned when [`KernelProcessSignal::install_task_reclaim`] is
/// called more than once on the same producer.
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub struct TaskReclaimAlreadyInstalled;

/// The one pending foreground signal and the owner it is aimed at.
///
/// A single slot, not a queue: a later `^C`/`^Z` typed before the previous
/// one was delivered simply replaces it, which matches what the keystrokes
/// mean — the newest request wins, and a terminated target makes the older
/// one moot.
///
/// Written from the UART receive interrupt and drained in dispatcher
/// context, so the lock masks the writing CPU for the store rather than
/// risking a self-deadlock against the task it interrupted.
static PENDING_FOREGROUND: IrqSafeSpinLock<Option<(ForegroundOwner, Signal)>> =
    IrqSafeSpinLock::new(None);

/// Whether [`PENDING_FOREGROUND`] holds a signal, for the preemption gate's
/// per-tick peek.
///
/// Advisory and lock-free on purpose: the gate consults it on every timer
/// tick, where an uncontended lock acquire would still cost a mask/unmask
/// pair. A spurious `true` costs one extra reschedule that finds the slot
/// empty; the delivery itself always reads the slot under the lock.
static FOREGROUND_QUEUED: AtomicBool = AtomicBool::new(false);

/// Record a foreground signal for delivery at the next dispatcher-context
/// drain ([`drain_pending_foreground`]).
///
/// Interrupt-safe: the slot's lock masks the writing CPU for one store, so
/// the UART RX handler that maps `^C` cannot deadlock against the task it
/// interrupted, and it takes no scheduler lock at all — the delivery that
/// does runs later, in dispatcher context.
pub fn queue_foreground_signal(owner: ForegroundOwner, signal: Signal) {
    *PENDING_FOREGROUND.lock() = Some((owner, signal));
    FOREGROUND_QUEUED.store(true, Ordering::Release);
}

/// Non-consuming peek: whether a foreground signal (`^C`/`^Z`) is queued
/// awaiting its dispatcher-context [`drain_pending_foreground`].
///
/// The preemption gate consults this so a timer tick on a lone-task CPU
/// still reschedules when a queued signal needs delivering — the delivery
/// only runs once the dispatch loop regains control.
#[must_use]
pub fn has_pending_foreground() -> bool {
    FOREGROUND_QUEUED.load(Ordering::Acquire)
}

/// Deliver the pending foreground signal, if any, through the installed
/// [`ForegroundSignal`] producer.
///
/// Called from the dispatch loop between task dispatches (the same slot
/// `drain_pending_wakes` runs in), where taking scheduler locks is safe.
/// Returns `true` when a delivery was attempted, so the idle path knows
/// work happened. A failed delivery is dropped: the aimed-at process
/// instance is gone, so the signal has no one left to go to and is never
/// re-aimed at whoever holds its pid now.
pub fn drain_pending_foreground() -> bool {
    FOREGROUND_QUEUED.store(false, Ordering::Release);
    let Some((owner, signal)) = PENDING_FOREGROUND.lock().take() else {
        return false;
    };
    let Ok(Some(hook)) = FOREGROUND_SIGNAL.get() else {
        // No producer installed (or the cell poisoned): nothing can be
        // delivered — fail closed. The slot is already cleared, never
        // retried into a later boot phase.
        return false;
    };
    let _ = hook.deliver(owner, signal);
    true
}

/// The kernel-side producer of the `signal` syscall.
///
/// Authority and mechanism are separate halves of this contract: the
/// producer answers *which live task a pid names among the sender's own
/// children* ([`Self::resolve_child`]) and *how a signal reaches a target*
/// ([`Self::signal_task`]), and the syscall handler decides between the
/// widening rules (own child, same principal, `CAP_PROC_CONTROL`) in one
/// place. A producer therefore carries no policy of its own and can never
/// widen the target rule behind the handler's back.
///
/// Implementations must be [`Sync`]: the single installed producer is shared
/// by the per-CPU syscall handlers, exactly like the process-wait producer,
/// the spawn producer, and the console device.
pub trait ProcessSignal: Sync {
    /// Resolve `pid` to the **live** child of `sender` it names.
    ///
    /// `sender` is the kernel-attested identity of the calling task
    /// (supplied by the dispatcher, never by the caller). Only a child the
    /// sender spawned and that is still running resolves; a zombie awaiting
    /// reap, another parent's child, and an unknown `pid` all do not.
    ///
    /// # Errors
    ///
    /// [`Errno::NotFound`] when `pid` does not name a live child of
    /// `sender` — the answer that sends the handler on to its
    /// cross-principal rule. The default producer ([`NullProcessSignal`])
    /// returns [`Errno::NotImplemented`] to mark an inert interface.
    fn resolve_child(&self, sender: ProcessId, pid: i64) -> Result<ProcessId, Errno>;

    /// Deliver `signal` to an **already-authorised** `target`.
    ///
    /// Mechanism only: this performs no ownership or capability check, so
    /// the caller must have established the sender's authority over
    /// `target` first.
    ///
    /// # Errors
    ///
    /// [`Errno::NotFound`] when the scheduler no longer knows `target` (it
    /// exited between authorisation and delivery). The default producer
    /// ([`NullProcessSignal`]) returns [`Errno::NotImplemented`] to mark an
    /// inert interface.
    fn signal_task(&self, target: ProcessId, signal: Signal) -> Result<(), Errno>;

    /// Drive every thread of `process` **except** `keep` to its stopping point,
    /// so the process carries `status` and nothing of the group is still
    /// executing when it is reclaimed (`plans/THREADS.md` decision 10).
    ///
    /// The group-death half of `exit(code)` and of a fault kill. Each sibling
    /// either quiesces immediately — its per-thread state retired through the
    /// installed landing seam — or is still inside a syscall / executing in user
    /// mode, in which case the death is *deferred* against it carrying `status`,
    /// and whichever thread of the group lands last records that status and
    /// reclaims the process. That is why `status` is threaded through here
    /// rather than recorded by the calling thread up front: a `128 + n` written
    /// by a sibling's deferral would overwrite the real exit code.
    ///
    /// The default is a no-op: a single-threaded process has no siblings, and a
    /// producer with no thread-group table cannot see any.
    fn terminate_siblings(&self, process: ProcessId, keep: TaskId, status: i32) {
        let _ = (process, keep, status);
    }

    /// Whether this producer can drive a whole thread group to its stopping
    /// point.
    ///
    /// The prerequisite for admitting a *second* thread into a process: a group
    /// whose siblings cannot be stopped can never be torn down, so
    /// `thread_create` fails closed rather than building a process whose death
    /// would either strand its kernel state or reclaim its address space from
    /// under a running sibling. Defaults to `false` — an inert producer stops
    /// nothing.
    fn can_stop_group(&self) -> bool {
        false
    }
}

/// The process-signal producer installed before any real one exists.
///
/// Every `signal` fails closed with [`Errno::NotImplemented`] — the
/// fail-closed default, so a `signal` issued before the boot path installs
/// the scheduler-side producer announces an inert interface rather than
/// pretending a signal was delivered.
#[derive(Debug, Default, Copy, Clone)]
pub struct NullProcessSignal;

impl ProcessSignal for NullProcessSignal {
    fn resolve_child(&self, _sender: ProcessId, _pid: i64) -> Result<ProcessId, Errno> {
        Err(Errno::NotImplemented)
    }

    fn signal_task(&self, _target: ProcessId, _signal: Signal) -> Result<(), Errno> {
        Err(Errno::NotImplemented)
    }
}

/// The shared [`NullProcessSignal`] instance the syscall handler defaults to.
///
/// `KernelSyscallHandlers::new` points its `process_signal` borrow here so
/// the field is always valid without an `Option` branch on the hot path; the
/// boot path replaces it with the concrete producer through
/// `KernelSyscallHandlers::with_process_signal`.
pub static NULL_PROCESS_SIGNAL: NullProcessSignal = NullProcessSignal;

/// The scheduler-side `signal` producer the boot path installs
/// (`plans/SPAWN.md` `SP7b`).
///
/// It carries **no bookkeeping of its own**: it resolves the sender's own
/// children and records a signalled termination through the one
/// [`KernelProcessWait`] that already
/// owns the parent/child + exit-status table (the `wait` producer), so the
/// two syscalls share a single source of truth for who parents whom and how
/// a child's terminal status is reported. Delivery itself drives the
/// scheduler directly — the only new capability signalling needs over
/// `wait` — through the [`SchedulerPolicy`] contract:
///
/// * [`Signal::Continue`] resumes a stopped child ([`SchedulerPolicy::resume`],
///   clearing any unreported stop);
/// * [`Signal::Terminate`] / [`Signal::Kill`] / [`Signal::Interrupt`]
///   terminate the child ([`SchedulerPolicy::exit`]) and record the
///   signal's POSIX-familiar termination status so the parent's `wait`
///   reaps it;
/// * [`Signal::Stop`] stops the child ([`SchedulerPolicy::stop`]), which no
///   wake ends, and records the stop so a `WaitFlags::STOPPED` wait
///   observes it.
///
/// `P` is the concrete scheduler policy (the `SchedulerPolicy` methods take
/// generic bodies, so the contract is not object-safe and cannot be held as
/// `&dyn`); only `kernel/core` names it, keeping the rest of the kernel
/// policy-agnostic. The producer holds `'static` borrows of both the wait
/// producer and the scheduler, exactly like the other boot-installed seams.
pub struct KernelProcessSignal<A, P>
where
    A: SchedulerArch + Send + Sync + 'static,
    P: SchedulerPolicy<A> + Send + Sync + 'static,
{
    /// The `wait` producer that owns the parent/child bookkeeping this
    /// producer resolves and records against — never a second copy.
    wait: &'static KernelProcessWait<A>,
    /// The live scheduler this producer drives to deliver a signal
    /// (stop, resume, wake or exit the target task).
    scheduler: &'static P,
    /// The authoritative thread-group table, so a process-directed signal
    /// reaches **every** thread of its target (`plans/THREADS.md` decision
    /// 10).
    ///
    /// A signal names a PID, but only threads are schedulable: parking,
    /// unparking, and exiting all name a thread. Delivering to the leader
    /// alone would leave a killed process's siblings running against a
    /// reclaimed address space — a wild-fault hazard, not a behavioural
    /// nuance — and would leave a stopped process still executing. [`None`]
    /// on a host fixture that wired no table; delivery then treats the
    /// target as the single-threaded process its PID names, which is what
    /// every process was before threads existed.
    caps: Option<&'static RwLock<CapTable>>,
    /// The boot-installed [`TaskReclaim`] seam a terminating signal drives
    /// (set-once; the dispatch hook is leaked *after* this producer is
    /// built, so the reference arrives through
    /// [`Self::install_task_reclaim`] rather than the constructor). Unset
    /// — a host fixture of the signal bookkeeping alone — reclaims
    /// nothing: such a build registered no kernel resources either.
    reclaim: OnceCell<&'static (dyn TaskReclaim + 'static)>,
}

impl<A, P> KernelProcessSignal<A, P>
where
    A: SchedulerArch + Send + Sync + 'static,
    P: SchedulerPolicy<A> + Send + Sync + 'static,
{
    /// Build a producer that resolves children and records against `wait`
    /// and delivers through `scheduler`, fanning a process-directed signal out
    /// over the thread groups `caps` holds.
    #[must_use]
    pub const fn new(
        wait: &'static KernelProcessWait<A>,
        scheduler: &'static P,
        caps: &'static RwLock<CapTable>,
    ) -> Self {
        Self {
            wait,
            scheduler,
            caps: Some(caps),
            reclaim: OnceCell::new(),
        }
    }

    /// A producer with no thread-group table: every target is treated as the
    /// single-threaded process its PID names.
    ///
    /// The shape a host fixture of the signal bookkeeping alone needs — it
    /// registers no capability records, so there is no group to resolve.
    #[must_use]
    pub const fn without_thread_groups(
        wait: &'static KernelProcessWait<A>,
        scheduler: &'static P,
    ) -> Self {
        Self {
            wait,
            scheduler,
            caps: None,
            reclaim: OnceCell::new(),
        }
    }

    /// Which process instance holds `process` right now.
    ///
    /// [`ProcId::KERNEL`] both for a principal that is not a distinct user
    /// process and for a record the table no longer holds, so a comparison
    /// against a real minted instance fails closed on a dead target. A
    /// producer wired with no table has no process instances to tell apart at
    /// all and answers the sentinel for every target, matching the sentinel a
    /// grant on such a build recorded.
    fn instance_of(&self, process: ProcessId) -> ProcId {
        self.caps
            .map_or(ProcId::KERNEL, |caps| caps.read().instance_of(process))
    }

    /// Publish the [`TaskReclaim`] seam a terminating signal drives (the
    /// boot path's leaked dispatch hook). Set-once per producer: a second
    /// call fails closed rather than re-pointing the live seam.
    ///
    /// # Errors
    ///
    /// [`TaskReclaimAlreadyInstalled`] if a seam was already installed.
    pub fn install_task_reclaim(
        &self,
        hook: &'static (dyn TaskReclaim + 'static),
    ) -> Result<(), TaskReclaimAlreadyInstalled> {
        self.reclaim
            .set(hook)
            .map_err(|_| TaskReclaimAlreadyInstalled)
    }

    /// Resume a stopped child ([`Signal::Continue`]).
    ///
    /// A continue delivered to a child that is not actually stopped is a
    /// harmless no-op — matching the long-standing Unix behaviour where
    /// continuing a running process succeeds without effect. A child with no
    /// live thread fails closed with [`Errno::NotFound`].
    fn resume(&self, child: ProcessId) -> Result<(), Errno> {
        // The resume also clears any stop the parent never observed: a
        // stale "stopped" report after the child is running again would
        // mislead the job table.
        let (job, _) = self.drive_job(child, false)?;
        self.wait.record_continue(child, job);
        Ok(())
    }

    /// Stop a child without terminating it ([`Signal::Stop`]).
    ///
    /// Every thread of the group is stopped — stopping only the leader would
    /// leave the rest of the process running, which is not what "stopped"
    /// means to a job-control shell — and the stop recorded once for a
    /// `WaitFlags::STOPPED` wait, unless every thread is already dying. A
    /// child with no live thread fails closed with [`Errno::NotFound`].
    fn stop(&self, child: ProcessId) -> Result<(), Errno> {
        let (job, held) = self.drive_job(child, true)?;
        if held {
            self.wait.record_stop(child, Signal::Stop, job);
        }
        Ok(())
    }

    /// Move `process` to the job-control state `stopped` and bring each of
    /// its threads to it, returning the generation that decides it and
    /// whether this call moved the process and any thread of it will hold.
    ///
    /// The generation is advanced, and the threads it governs collected,
    /// under the thread-group table's read lock alone: a registration holds
    /// the write lock and gives the thread the generation it finds, so a
    /// thread joins either before the collection or already holding the
    /// decision. The threads are then driven with no lock held, so a fan-out
    /// over a large group blocks no other CPU's syscall.
    fn drive_job(&self, process: ProcessId, stopped: bool) -> Result<(JobGeneration, bool), Errno> {
        let transition = self
            .job_transition(process, stopped)
            .ok_or(Errno::NotFound)?;
        let mut holds = false;
        for (thread, gate) in &transition.members {
            gate.apply_job(transition.job);
            holds |= self.reconcile(*thread, gate);
        }
        Ok((transition.job, transition.moved && holds))
    }

    /// Advance `process`'s generation toward `stopped` and collect every live
    /// thread of it with its gate. A producer wired with no table treats the
    /// target as the single thread its process id names, whose own gate then
    /// carries the generation.
    fn job_transition(&self, process: ProcessId, stopped: bool) -> Option<JobTransition> {
        let Some(caps) = self.caps else {
            let gate = gate_of(process.0)?;
            let (job, moved) = gate.advance_job(stopped);
            return Some(JobTransition {
                job,
                moved,
                members: alloc::vec![(process.0, gate)],
            });
        };
        let table = caps.read();
        let (job, moved) = table.advance_job(process, stopped)?;
        let gates = GATES.read();
        let members = table
            .threads_of(process)
            .filter_map(|thread| {
                let gate = gates.as_ref()?.get(&thread.0)?;
                Some((thread.0, Arc::clone(gate)))
            })
            .collect();
        Some(JobTransition {
            job,
            moved,
            members,
        })
    }

    /// Bring `thread`'s scheduler state to what its gate now decides,
    /// re-reading the gate after acting until a read agrees with the one
    /// acted on, and report whether the thread will hold stopped.
    ///
    /// Every party that changes the decision on the gate word acts after it:
    /// a stop or continue reconciles, a death's killer brings the thread to
    /// it, and a thread entering a kernel body takes its own stop at the
    /// edge. Only a stop made here on an older read can outlive every one of
    /// them, so a decision to leave the thread alone takes it back.
    fn reconcile(&self, thread: u64, gate: &ThreadGate) -> bool {
        self.reconcile_deciding(thread, gate, |_| {})
    }

    /// [`Self::reconcile`], running `decided` between each read of the gate
    /// and the scheduler action that read decides.
    fn reconcile_deciding(
        &self,
        thread: u64,
        gate: &ThreadGate,
        mut decided: impl FnMut(JobHold),
    ) -> bool {
        let mut word = gate.ordered();
        let mut holding = false;
        loop {
            let hold = job_hold(word);
            decided(hold);
            match hold {
                JobHold::Hold => {
                    let _ = self.scheduler.stop(thread);
                    holding = true;
                }
                JobHold::Release => {
                    let _ = self.scheduler.resume(thread);
                    holding = false;
                }
                JobHold::Leave if holding => {
                    let _ = self.scheduler.resume(thread);
                    holding = false;
                }
                JobHold::Leave => {}
            }
            let now = gate.ordered();
            if job_hold(now) == hold {
                return now & OWED == 0 && ThreadGate::job_in(now).is_stopped();
            }
            word = now;
        }
    }

    /// Terminate a child ([`Signal::Terminate`] / [`Signal::Kill`]), the
    /// process to carry the signal's `128 + n` status for its parent's `wait`.
    ///
    /// A signal names a process, so every thread of the group is driven to its
    /// death, and the process teardown lands once, when the last of them is
    /// down (`plans/THREADS.md` decision 10). The delivery is complete once each
    /// death is recorded: a thread is never destroyed where that is unsafe —
    /// inside a kernel body, or while its code is still executing — so the
    /// death may land later, at the thread's boundary or where the scheduler
    /// retires it, and the parent's `wait` reaps the child when it does.
    ///
    /// # Errors
    ///
    /// [`Errno::NotFound`] when no thread of the child is left to reach;
    /// nothing is recorded for its parent then (never a fabricated zombie).
    fn terminate(&self, child: ProcessId, signal: Signal) -> Result<(), Errno> {
        // Only a terminating signal reaches here, so it names a `128 + n`
        // status; a non-terminating one is refused rather than assumed.
        let Some(status) = signal.termination_status() else {
            return Err(Errno::OutOfRange);
        };
        let teardown = DeferredTeardown::Exit {
            process: child,
            status,
        };
        if self.kill_group(teardown, None) {
            Ok(())
        } else {
            Err(Errno::NotFound)
        }
    }

    /// Record `teardown` against every thread of its process but `spare` and
    /// bring each to where its death lands, reporting whether the scheduler
    /// still knew any of them.
    fn kill_group(&self, teardown: DeferredTeardown, spare: Option<u64>) -> bool {
        self.stop_claims(claim_group_kill(self.caps, teardown, spare))
    }

    /// Bring every claimed thread to where its death lands, reporting whether
    /// the scheduler still knew any of them.
    fn stop_claims(&self, claims: Vec<ClaimedKill>) -> bool {
        let mut reached = false;
        for claim in claims {
            reached |= self.stop_claimed(claim);
        }
        reached
    }

    /// Bring one thread whose death was just claimed to where that death
    /// lands, reporting whether the scheduler still knew the thread.
    fn stop_claimed(&self, claim: ClaimedKill) -> bool {
        match claim.site {
            // Every in-kernel park loop re-tests the gate after a wake and
            // unwinds, but no wake ends a stop, so a stopped thread is resumed
            // first. `InvalidState` is a thread that has already exited, with
            // no boundary left to reach.
            KillSite::Boundary => {
                let _ = self.scheduler.resume(claim.thread);
                matches!(
                    self.scheduler.unpark(claim.thread),
                    Ok(()) | Err(SchedError::InvalidState)
                )
            }
            KillSite::Retire => match self.scheduler.exit(claim.thread) {
                // Retired by this call, so this call lands whichever death the
                // thread owes: the first claim's, which need not be this one.
                Ok(ExitDisposition::Quiesced) => {
                    if let Some(owed) = take_owed_kill(claim.thread) {
                        self.land(owed.process(), claim.thread, owed.reaped_status());
                    }
                    true
                }
                // Still executing, or already dying: the retire the scheduler
                // owes it, or the teardown already under way, takes the death.
                Ok(ExitDisposition::Deferred | ExitDisposition::AlreadyExited) => true,
                // A member the scheduler does not know is either mid-teardown,
                // whose gate clear would drop this death anyway, or has no task
                // to retire at all; either way nothing would land it.
                Err(_) => {
                    if claim.recorded {
                        let _ = take_owed_kill(claim.thread);
                    }
                    false
                }
            },
        }
    }

    /// Retire one quiesced `thread` of `process` through the installed landing
    /// seam, which tears the process down — and records `status` for the
    /// parent's `wait` — once the group's last thread is down.
    fn land(&self, process: ProcessId, thread: u64, status: Option<i32>) {
        if let Ok(Some(hook)) = self.reclaim.get() {
            hook.land_thread_down(process, TaskId(thread), status);
            return;
        }
        // No landing seam installed. Such a build registered no kernel
        // resources to reclaim and holds no thread-group table, so this thread
        // *is* its group — but a parent's `wait` is still owed the status, and
        // dropping it would leave the parent blocked on a child that is gone.
        if let Some(status) = status {
            self.wait.record_exit(process, status);
        }
    }

    /// The one delivery engine: apply `signal` to an already-authorised
    /// `target`.
    ///
    /// Both entry points into this producer end here — the `signal`
    /// syscall's [`ProcessSignal::signal_task`] and the console line
    /// discipline's [`ForegroundSignal::deliver`] — so a `^C` and a
    /// parent's `Terminate` take exactly the same path and can never
    /// diverge. Authority is decided by each caller before it arrives:
    /// nothing here consults the parent/child table or a capability.
    fn deliver_signal(&self, target: ProcessId, signal: Signal) -> Result<(), Errno> {
        match signal {
            Signal::Continue => self.resume(target),
            // A termination *request* is observable: an opted-in target with
            // a free pending slot records it instead of dying (the recorded
            // delivery is complete — the target's waiter is woken to drain
            // it). A target that never opted in, or whose slot is already
            // occupied (the escalation rule), terminates by default.
            Signal::Terminate | Signal::Interrupt => {
                if try_intake(target.0, signal) {
                    Ok(())
                } else {
                    self.terminate(target, signal)
                }
            }
            // `Kill` is unconditionally fatal and unmaskable: it is never
            // offered to the intake.
            Signal::Kill => self.terminate(target, signal),
            Signal::Stop => self.stop(target),
        }
    }
}

impl<A, P> DeferredKillLander for KernelProcessSignal<A, P>
where
    A: SchedulerArch + Send + Sync + 'static,
    P: SchedulerPolicy<A> + Send + Sync + 'static,
{
    fn land_deferred_teardown(&self, thread: TaskId, teardown: DeferredTeardown) {
        // The thread has returned to the dispatch loop and executes nowhere,
        // so the teardown deferred at request time is now safe. The process
        // it owes is carried by the deferral itself.
        match teardown {
            // The very reap+reclaim the immediate terminate path runs, one
            // definition — and, for a multi-threaded victim, only once its last
            // thread is down.
            DeferredTeardown::Exit { process, status } => {
                self.land(process, thread.0, Some(status));
            }
            // A driver unload whose process was still executing: reclaim its
            // kernel resources only, with no `wait` reap (a driver is not a
            // waited-for child).
            DeferredTeardown::Plain { process } => {
                self.land(process, thread.0, None);
            }
        }
    }

    fn kill_session_member(&self, process: ProcessId, instance: ProcId) -> bool {
        let (Some(caps), Some(status)) = (self.caps, Signal::Kill.termination_status()) else {
            return false;
        };
        let claims =
            claim_instance_kill(caps, instance, DeferredTeardown::Exit { process, status });
        let recorded = claims.iter().any(|claim| claim.recorded);
        self.stop_claims(claims) && recorded
    }
}

impl<A, P> ProcessSignal for KernelProcessSignal<A, P>
where
    A: SchedulerArch + Send + Sync + 'static,
    P: SchedulerPolicy<A> + Send + Sync + 'static,
{
    fn terminate_siblings(&self, process: ProcessId, keep: TaskId, status: i32) {
        let _ = self.kill_group(DeferredTeardown::Exit { process, status }, Some(keep.0));
    }

    fn can_stop_group(&self) -> bool {
        // Without the thread-group table a fan-out sees no siblings at all, so
        // a second thread of a process could never be driven to its stopping
        // point.
        self.caps.is_some()
    }

    fn resolve_child(&self, sender: ProcessId, pid: i64) -> Result<ProcessId, Errno> {
        // The `wait` producer already owns the parent/child bookkeeping, so
        // who-parents-whom is answered in one place for both syscalls. A
        // live child of `sender` resolves; anything else is `NotFound`.
        self.wait.authorise_child(sender, pid)
    }

    fn signal_task(&self, target: ProcessId, signal: Signal) -> Result<(), Errno> {
        self.deliver_signal(target, signal)
    }
}

impl<A, P> ForegroundSignal for KernelProcessSignal<A, P>
where
    A: SchedulerArch + Send + Sync + 'static,
    P: SchedulerPolicy<A> + Send + Sync + 'static,
{
    fn deliver(&self, owner: ForegroundOwner, signal: Signal) -> Result<(), Errno> {
        // The ownership was granted to one *instance* of that pid, and an id
        // whose task is gone may be drawn again, so a target whose pid now
        // names a different instance is refused outright: a `^C` must never
        // land on whoever inherited the number.
        if self.instance_of(owner.process) != owner.instance {
            return Err(Errno::NotFound);
        }
        // No parent/child authorisation here: the authority was checked when
        // the parent marked the target foreground on its own console, and
        // the console line discipline is the kernel acting on the terminal
        // owner's standing instruction.
        //
        // The line discipline maps only `^C`/`^Z`; any other signal on this
        // path is a programming error refused outright. What it does map
        // goes through the one shared delivery engine, so a `^C` and a
        // parent's `Interrupt` behave identically (including the observable
        // intake and its escalation rule) and the delivery still fails
        // closed on a target the scheduler no longer knows.
        match signal {
            Signal::Interrupt | Signal::Stop => self.deliver_signal(owner.process, signal),
            Signal::Continue | Signal::Terminate | Signal::Kill => Err(Errno::OutOfRange),
        }
    }
}

/// Serialises host tests that touch the process-global foreground state
/// ([`PENDING_FOREGROUND`], [`FOREGROUND_SIGNAL`]): the tests in this module
/// and the console line-discipline tests share one pending slot, so they
/// take this lock to keep their queue/drain sequences from interleaving.
#[cfg(test)]
pub(crate) fn foreground_test_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    // A panicking holder does not corrupt the `()` state; continue.
    LOCK.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Serialises host tests that touch the process-global kill gate and the
/// once-set [`DEFERRED_KILL_LANDER`]: deaths are keyed by numeric task id and
/// share one lander, so two tests claiming or landing "their" task in
/// parallel would race each other's entries.
#[cfg(test)]
pub(crate) fn running_kill_test_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    // A panicking holder does not corrupt the `()` state; continue.
    LOCK.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Test-only: consume the pending foreground slot, so the console
/// line-discipline tests can assert what the filter queued without invoking
/// whichever process-global hook another test may have installed.
#[cfg(test)]
pub(crate) fn take_pending_foreground_for_test() -> Option<(ForegroundOwner, Signal)> {
    FOREGROUND_QUEUED.store(false, Ordering::Release);
    PENDING_FOREGROUND.lock().take()
}

/// Test-only: whether some [`ForegroundSignal`] hook is installed, and if
/// not, install an inert one — so the console line-discipline tests always
/// run with the interception gate open, regardless of test ordering.
#[cfg(test)]
pub(crate) fn ensure_foreground_hook_for_test() {
    struct InertHook;
    impl ForegroundSignal for InertHook {
        fn deliver(&self, _owner: ForegroundOwner, _signal: Signal) -> Result<(), Errno> {
            Ok(())
        }
    }
    static HOOK: InertHook = InertHook;
    let _ = install_foreground_signal(&HOOK);
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::collections::BTreeSet;
    use std::boxed::Box;

    use tairix_abi::{WaitFlags, WaitStatus};
    use tairix_kernel_sched_api::{
        CpuId, Priority, SchedClass, SchedResult, SchedulerConfig, StepOutcome, TaskAction,
        TaskContext, TaskId as SchedTaskId, TaskState,
    };

    use crate::procwait::{ProcessWait, WaitedChild};
    use crate::sched::Scheduler;
    use crate::test_arch::TestArch;

    /// Build a leaked `'static` wait producer + live single-CPU scheduler for
    /// a producer-level test. The wait producer gets its own leaked
    /// [`TestArch`] (it only reads the current CPU), and the scheduler is a
    /// real [`Scheduler`] so `exit`/`unpark` exercise a genuine
    /// [`SchedulerPolicy`], not a fake double.
    fn scaffold() -> (
        &'static KernelProcessWait<TestArch>,
        &'static Scheduler<TestArch>,
    ) {
        let sched_arch = std::sync::Arc::new(TestArch::with_cpus(1));
        let scheduler =
            Scheduler::new(SchedulerConfig::defaults_for(1), sched_arch).expect("scheduler builds");
        let scheduler: &'static Scheduler<TestArch> = Box::leak(Box::new(scheduler));
        let wait_arch: &'static TestArch = Box::leak(Box::new(TestArch::with_cpus(1)));
        let wait: &'static KernelProcessWait<TestArch> =
            Box::leak(Box::new(KernelProcessWait::new(wait_arch)));
        (wait, scheduler)
    }

    /// A landing seam that records the retirements the producer drove it with,
    /// so a test can assert the fan-out reached a thread without reimplementing
    /// the production landing rule
    /// (`KernelSyscallHandlers::land_thread_down`) in a double.
    ///
    /// A producer-level fixture installs one only when it asserts *that* the
    /// seam is driven. Without one the producer records the terminal status
    /// through its own wait producer — the degenerate path for a build that
    /// registered no kernel resources — which is what the other tests here
    /// observe.
    struct LandingRecorder {
        landed: SpinLock<Vec<(u64, Option<i32>)>>,
    }

    impl LandingRecorder {
        const fn new() -> Self {
            Self {
                landed: SpinLock::new(Vec::new()),
            }
        }

        /// Whether the producer retired `thread` carrying `status`.
        fn landed(&self, thread: u64, status: Option<i32>) -> bool {
            self.landed.lock().contains(&(thread, status))
        }
    }

    impl TaskReclaim for LandingRecorder {
        fn land_thread_down(&self, _process: ProcessId, thread: TaskId, status: Option<i32>) {
            self.landed.lock().push((thread.0, status));
        }
    }

    /// The foreground owner a producer with no capability table records and
    /// resolves: no distinct process instances exist on such a build, so both
    /// sides carry the kernel sentinel and the delivery gate lets the signal
    /// through to the mechanics under test.
    fn fg(process: u64) -> ForegroundOwner {
        ForegroundOwner {
            process: ProcessId(process),
            instance: ProcId::KERNEL,
        }
    }

    /// A live child task, as both the scheduler's `u64` id and the signed pid
    /// spelling the syscall surface names it by.
    fn spawn_child(scheduler: &Scheduler<TestArch>) -> (u64, i64) {
        let id = scheduler
            .spawn(0, Priority::Normal, |_ctx| TaskAction::Exit)
            .expect("task admitted");
        let _ = gate(id);
        (id, id.cast_signed())
    }

    /// A fresh gate for `task`, a single-threaded process of its own, as an
    /// admission installs one.
    fn gate(task: u64) -> Arc<ThreadGate> {
        clear_kill_gate(task);
        install_gate(task, ProcessId(task)).expect("a gate installs");
        gate_in_hand(task)
    }

    /// The registered gate of `task`, which every test thread here has.
    fn gate_in_hand(task: u64) -> Arc<ThreadGate> {
        gate_of(task).expect("the thread has its gate")
    }

    /// Leave `gate`'s kernel body for good, as an exit does: the death taken
    /// there, which no stop defers.
    fn left(gate: &ThreadGate) -> Option<DeferredTeardown> {
        match gate.leave(false) {
            Leave::Death(teardown) => Some(teardown),
            Leave::Left | Leave::Stop => None,
        }
    }

    /// Drive the two halves of the producer in the order the syscall
    /// handler drives them for a signal aimed at the sender's own child:
    /// resolve the pid against the sender's children, then deliver.
    ///
    /// Every cross-principal rule is the handler's, so it is exercised
    /// against the handler; these producer-level tests cover the own-child
    /// path and the delivery mechanics.
    fn signal_child(
        signaller: &KernelProcessSignal<TestArch, Scheduler<TestArch>>,
        sender: ProcessId,
        pid: i64,
        signal: Signal,
    ) -> Result<(), Errno> {
        let target = signaller.resolve_child(sender, pid)?;
        signaller.signal_task(target, signal)
    }

    #[test]
    fn null_process_signal_fails_closed() {
        // Neither half of the inert default answers: it resolves no target
        // and delivers to none, rather than pretending either succeeded.
        assert_eq!(
            NULL_PROCESS_SIGNAL.resolve_child(ProcessId(1), 2),
            Err(Errno::NotImplemented)
        );
        // Every variant of the closed signal set fails closed on delivery.
        for signal in [
            Signal::Continue,
            Signal::Terminate,
            Signal::Kill,
            Signal::Interrupt,
            Signal::Stop,
        ] {
            assert_eq!(
                NULL_PROCESS_SIGNAL.signal_task(ProcessId(2), signal),
                Err(Errno::NotImplemented)
            );
        }
    }

    #[test]
    fn signalling_a_non_child_fails_closed() {
        let (wait, scheduler) = scaffold();
        let signaller = KernelProcessSignal::without_thread_groups(wait, scheduler);
        // A caller with no children resolves nothing to signal.
        assert_eq!(
            signal_child(&signaller, ProcessId(1), 2, Signal::Terminate),
            Err(Errno::NotFound)
        );
        // A live task that is not *this* caller's child is off-limits: task 9
        // may not reach task 7's child through the child rule.
        let (child, child_pid) = spawn_child(scheduler);
        wait.register_child(
            ProcessId(7),
            ProcessId(child),
            crate::procwait::ChildListing::Listed,
        )
        .expect("registered");
        assert_eq!(
            signal_child(&signaller, ProcessId(9), child_pid, Signal::Kill),
            Err(Errno::NotFound)
        );
        // The child was untouched by the unresolved signal.
        assert_eq!(scheduler.live_task_count(), 1);
    }

    #[test]
    fn terminate_ends_the_child_and_records_its_signalled_status() {
        let (wait, scheduler) = scaffold();
        let (child, child_pid) = spawn_child(scheduler);
        wait.register_child(
            ProcessId(7),
            ProcessId(child),
            crate::procwait::ChildListing::Listed,
        )
        .expect("registered");
        let signaller = KernelProcessSignal::without_thread_groups(wait, scheduler);

        assert_eq!(
            signal_child(&signaller, ProcessId(7), child_pid, Signal::Terminate),
            Ok(())
        );
        // The child was terminated on the scheduler.
        assert_eq!(scheduler.live_task_count(), 0);
        // ... and reaps with Terminate's POSIX-familiar 143 status, exactly
        // as if it had exited with that code itself.
        let pid = child;
        assert_eq!(
            wait.wait(
                ProcessId(7),
                TaskId(7),
                tairix_abi::WAIT_PID_ANY,
                WaitFlags::empty()
            ),
            Ok(WaitedChild {
                pid,
                status: WaitStatus::Exited(143)
            })
        );
    }

    #[test]
    fn the_kill_gate_round_trips_enter_take_and_clear() {
        // Pure gate bookkeeping, on raw ids no scheduler-backed test uses.
        let open = gate(0x00de_ad01);
        assert_eq!(open.enter(), Owed::Nothing);
        assert!(!kill_pending(0x00de_ad01));
        assert_eq!(left(&open), None);
        // Clearing an open window leaves nothing behind, registration included,
        // while the thread's own handle still answers.
        let cleared = gate(0x00de_ad02);
        assert_eq!(cleared.enter(), Owed::Nothing);
        clear_kill_gate(0x00de_ad02);
        assert!(gate_of(0x00de_ad02).is_none());
        assert_eq!(left(&cleared), None);
        assert_eq!(
            install_gate(0x00de_ad01, ProcessId(0x00de_ad01)),
            Err(Errno::AlreadyExists),
            "one gate per thread"
        );
        clear_kill_gate(0x00de_ad01);
    }

    /// A death a thread's boundary and its killer race to take is taken by
    /// exactly one of them: the boundary's swap and the killer's take are each
    /// one read-modify-write of the gate word, so neither can land a death the
    /// other already took, and none is lost between them.
    #[test]
    fn a_death_raced_for_by_its_boundary_and_its_killer_is_taken_once() {
        extern crate std;

        const ROUNDS: usize = 100_000;

        let _g = running_kill_test_lock();
        let task = 0x00de_ad04;
        let held = gate(task);
        let victim = {
            let held = Arc::clone(&held);
            std::thread::spawn(move || {
                let mut taken = 0usize;
                for _ in 0..ROUNDS {
                    let _ = held.enter();
                    taken += usize::from(left(&held).is_some());
                }
                taken
            })
        };
        let (mut recorded, mut killer_took) = (0usize, 0usize);
        for _ in 0..ROUNDS {
            recorded += usize::from(held.claim(exit_of(task, 137)).0);
            killer_took += usize::from(held.take_owed().is_some());
        }
        let boundary_took = victim.join().expect("the victim thread completes");
        let leaver_took = usize::from(left(&held).is_some());

        assert_eq!(
            boundary_took + killer_took + leaver_took,
            recorded,
            "every recorded death is taken exactly once"
        );
        clear_kill_gate(task);
    }

    /// An `Exit` status keeps every bit through the word, including a negative
    /// one, and a `Plain` death carries none.
    #[test]
    fn a_gate_word_keeps_the_owed_death_whole() {
        let _g = running_kill_test_lock();
        for status in [0, 137, -1, i32::MIN, i32::MAX] {
            let task = 0x00de_ad03;
            let held = gate(task);
            assert!(held.claim(exit_of(task, status)).0);
            assert_eq!(held.take_owed(), Some(exit_of(task, status)), "{status}");
            let plain = DeferredTeardown::Plain {
                process: ProcessId(task),
            };
            assert!(held.claim(plain).0);
            assert_eq!(left(&held), Some(plain));
            clear_kill_gate(task);
        }
    }

    #[test]
    fn terminating_a_task_inside_a_syscall_defers_the_kill_to_its_boundary() {
        let (wait, scheduler) = scaffold();
        let (child, child_pid) = spawn_child(scheduler);
        wait.register_child(
            ProcessId(7),
            ProcessId(child),
            crate::procwait::ChildListing::Listed,
        )
        .expect("registered");
        let signaller = KernelProcessSignal::without_thread_groups(wait, scheduler);

        // The child is mid-syscall: its handler may hold kernel state only
        // its own unwind can release (a mount's `SleepLock`, an in-flight
        // block-I/O descriptor), so the kill must not land here — the
        // regression this pins down is a killed writer leaving its volume's
        // lock held forever, deadlocking every later filesystem call.
        assert_eq!(gate_in_hand(child).enter(), Owed::Nothing);
        assert_eq!(
            signal_child(&signaller, ProcessId(7), child_pid, Signal::Kill),
            Ok(())
        );
        // The child was not destroyed mid-handler: it is still live on the
        // scheduler, the kill is pending against it, and the parent cannot
        // reap it yet.
        assert_eq!(scheduler.live_task_count(), 1);
        assert!(kill_pending(child));
        assert_eq!(
            wait.poll(ProcessId(7), tairix_abi::WAIT_PID_ANY, WaitFlags::empty()),
            Err(Errno::WouldBlock)
        );
        // The syscall boundary takes the deferred kill exactly once.
        assert_eq!(
            left(&gate_in_hand(child)).and_then(DeferredTeardown::reaped_status),
            Signal::Kill.termination_status()
        );
        assert_eq!(left(&gate_in_hand(child)), None);
    }

    #[test]
    fn a_deferred_kill_keeps_the_first_termination_request() {
        let (wait, scheduler) = scaffold();
        let (child, child_pid) = spawn_child(scheduler);
        wait.register_child(
            ProcessId(7),
            ProcessId(child),
            crate::procwait::ChildListing::Listed,
        )
        .expect("registered");
        let signaller = KernelProcessSignal::without_thread_groups(wait, scheduler);

        assert_eq!(gate_in_hand(child).enter(), Owed::Nothing);
        assert_eq!(
            signal_child(&signaller, ProcessId(7), child_pid, Signal::Terminate),
            Ok(())
        );
        // A follow-up `Kill` against the already-doomed child changes
        // nothing: it dies at the same boundary, with the first request's
        // status — matching the immediate path, where a second signal finds
        // the child already gone.
        assert_eq!(
            signal_child(&signaller, ProcessId(7), child_pid, Signal::Kill),
            Ok(())
        );
        assert_eq!(
            left(&gate_in_hand(child)).and_then(DeferredTeardown::reaped_status),
            Signal::Terminate.termination_status()
        );
        assert_eq!(scheduler.live_task_count(), 1);
    }

    /// A terminating signal drives the installed landing seam with the
    /// victim's thread and its `128 + n` status — the kill-path half of the one
    /// teardown the `exit` handler runs (regression: before the seam existed a
    /// killed task leaked its capability record, IRQ bindings, endpoints, and
    /// open files, and a pipe peer parked forever).
    #[test]
    fn terminate_drives_the_installed_landing_seam() {
        let (wait, scheduler) = scaffold();
        let (child, child_pid) = spawn_child(scheduler);
        wait.register_child(
            ProcessId(7),
            ProcessId(child),
            crate::procwait::ChildListing::Listed,
        )
        .expect("registered");
        let signaller = KernelProcessSignal::without_thread_groups(wait, scheduler);
        let landed: &'static LandingRecorder = Box::leak(Box::new(LandingRecorder::new()));
        signaller
            .install_task_reclaim(landed)
            .expect("first install on this producer");

        assert_eq!(
            signal_child(&signaller, ProcessId(7), child_pid, Signal::Kill),
            Ok(())
        );
        assert!(landed.landed(child, Some(137)));
    }

    /// A death against the single-threaded process `task` names, as a build
    /// with no group table claims it.
    fn claim_one(teardown: DeferredTeardown) -> ClaimedKill {
        let claims = claim_group_kill(None, teardown, None);
        assert_eq!(claims.len(), 1, "a table-less process is its leader");
        claims[0]
    }

    fn exit_of(task: u64, status: i32) -> DeferredTeardown {
        DeferredTeardown::Exit {
            process: ProcessId(task),
            status,
        }
    }

    #[test]
    fn a_claimed_death_is_recorded_once_and_taken_once() {
        let _g = running_kill_test_lock();
        let a = 0x00c0_ffe1;
        let _held = gate(a);

        let first = claim_one(exit_of(a, 137));
        assert_eq!(first.thread, a);
        assert_eq!(first.site, KillSite::Retire);
        assert!(first.recorded);
        assert!(
            !claim_one(exit_of(a, 143)).recorded,
            "the first claim wins; a later one records nothing"
        );
        assert!(
            OWED_KILLS.load(Ordering::Relaxed) >= 1,
            "an owed death keeps the dispatch loop's fast path open"
        );
        assert_eq!(take_owed_kill(a), Some(exit_of(a, 137)));
        assert_eq!(take_owed_kill(a), None, "taken exactly once");
        clear_kill_gate(a);
    }

    /// A thread with no gate — none an admission made — is never claimed, so a
    /// kill records nothing that no boundary or landing would ever take.
    #[test]
    fn a_thread_without_a_gate_is_never_claimed() {
        let _g = running_kill_test_lock();
        let stray = 0x00c0_ffe9;
        clear_kill_gate(stray);
        assert!(claim_group_kill(None, exit_of(stray, 137), None).is_empty());
        assert!(!kill_pending(stray));
        assert_eq!(take_owed_kill(stray), None);
    }

    #[test]
    fn a_death_claimed_inside_a_kernel_body_is_owed_at_its_boundary() {
        let _g = running_kill_test_lock();
        let task = 0x00c0_ffe8;
        let held = gate(task);

        assert_eq!(held.enter(), Owed::Nothing);
        assert_eq!(claim_one(exit_of(task, 137)).site, KillSite::Boundary);
        assert!(kill_pending(task), "the body's park loops must unwind");
        assert_eq!(
            left(&held).and_then(DeferredTeardown::reaped_status),
            Some(137)
        );
        assert!(!kill_pending(task));
        clear_kill_gate(task);
    }

    /// The other order. A thread already owing a death may already have been
    /// told to die, and the scheduler retires such a thread at its next
    /// stopping point — inside a body, that frees a stack whose frames own
    /// kernel state. So it enters the kernel only to reach its boundary.
    #[test]
    fn a_thread_owing_a_death_enters_the_kernel_only_to_die() {
        let _g = running_kill_test_lock();
        let driver = 0x00c0_ffe4;
        let held = gate(driver);
        let plain = DeferredTeardown::Plain {
            process: ProcessId(driver),
        };

        assert_eq!(claim_one(plain).site, KillSite::Retire);
        assert_eq!(held.enter(), Owed::Death, "the body must not run");
        assert_eq!(left(&held), Some(plain));
        clear_kill_gate(driver);
    }

    /// Tasks the deterministic test lander reclaimed, and the signalled
    /// statuses it reaped — kept as process-global sets so the landing tests
    /// share one installed lander regardless of test order (the global lander
    /// is set-once).
    static LAND_RECLAIMED: SpinLock<BTreeSet<u64>> = SpinLock::new(BTreeSet::new());
    static LAND_REAPED: SpinLock<BTreeMap<u64, i32>> = SpinLock::new(BTreeMap::new());

    /// A deterministic [`DeferredKillLander`] for the dispatch-loop landing
    /// tests: it records what it reclaimed and any status it reaped, mirroring
    /// the real producer's `Exit`-versus-`Plain` split.
    struct TestLander;
    impl DeferredKillLander for TestLander {
        fn land_deferred_teardown(&self, task: TaskId, teardown: DeferredTeardown) {
            if let Some(status) = teardown.reaped_status() {
                LAND_REAPED.lock().insert(task.0, status);
            }
            LAND_RECLAIMED.lock().insert(task.0);
        }

        fn kill_session_member(&self, _process: ProcessId, _instance: ProcId) -> bool {
            false
        }
    }
    static TEST_LANDER: TestLander = TestLander;

    /// Ensure some deferred-kill lander is installed and report whether it is
    /// our [`TestLander`]. The lander is a process-global set-once cell, and
    /// another suite may have installed the real producer first; a landing
    /// still *takes* the death then, which every landing test asserts, but its
    /// side effects land elsewhere, so those assertions are gated on this.
    fn ensure_test_lander() -> bool {
        static TEST_LANDER_INSTALLED: AtomicUsize = AtomicUsize::new(0);
        if install_deferred_kill_lander(&TEST_LANDER).is_ok() {
            TEST_LANDER_INSTALLED.store(1, Ordering::Relaxed);
        }
        TEST_LANDER_INSTALLED.load(Ordering::Relaxed) == 1
    }

    /// A dispatch returns on a yield and a park as well as on a retire. A
    /// thread owing a death that its dispatch queued again may yet run, so its
    /// process is reclaimed only once the scheduler has retired it — and then
    /// exactly once.
    #[test]
    fn only_a_retired_thread_has_its_death_landed_by_the_dispatch_loop() {
        let _g = running_kill_test_lock();
        let ours = ensure_test_lander();
        let child = 0x00c0_ffe5;
        let _held = gate(child);
        LAND_RECLAIMED.lock().remove(&child);
        LAND_REAPED.lock().remove(&child);

        let _ = claim_one(exit_of(child, 137));
        land_retired_kill(child, || false);
        assert!(
            kill_pending(child),
            "a thread not retired keeps its death owed"
        );
        if ours {
            assert!(!LAND_RECLAIMED.lock().contains(&child));
        }

        land_retired_kill(child, || true);
        assert!(!kill_pending(child), "a retired thread's death is taken");
        if ours {
            assert!(LAND_RECLAIMED.lock().contains(&child));
            assert_eq!(LAND_REAPED.lock().get(&child), Some(&137));
        }

        LAND_RECLAIMED.lock().remove(&child);
        land_retired_kill(child, || true);
        if ours {
            assert!(!LAND_RECLAIMED.lock().contains(&child), "landed only once");
        }
        clear_kill_gate(child);
    }

    /// A thread's own teardown clears the gate, so a death claimed while it
    /// was alive is never landed a second time on a process already reclaimed.
    #[test]
    fn clearing_the_gate_drops_a_death_without_landing_it() {
        let _g = running_kill_test_lock();
        let ours = ensure_test_lander();
        let child = 0x00c0_ffe6;
        let _held = gate(child);
        LAND_RECLAIMED.lock().remove(&child);

        let _ = claim_one(exit_of(child, 137));
        clear_kill_gate(child);
        land_retired_kill(child, || true);
        assert_eq!(take_owed_kill(child), None);
        if ours {
            assert!(!LAND_RECLAIMED.lock().contains(&child));
        }
    }

    /// An unloaded driver's death reclaims its resources and reaps nothing: a
    /// driver is not a waited-for child.
    #[test]
    fn a_plain_death_lands_without_a_reap() {
        let _g = running_kill_test_lock();
        let ours = ensure_test_lander();
        let driver = 0x00c0_ffe7;
        let _held = gate(driver);
        LAND_RECLAIMED.lock().remove(&driver);
        LAND_REAPED.lock().remove(&driver);

        let _ = claim_one(DeferredTeardown::Plain {
            process: ProcessId(driver),
        });
        land_retired_kill(driver, || true);
        assert!(!kill_pending(driver));
        if ours {
            assert!(LAND_RECLAIMED.lock().contains(&driver));
            assert!(!LAND_REAPED.lock().contains_key(&driver));
        }
        clear_kill_gate(driver);
    }

    /// How [`KillsElsewhere`] answers a kill.
    #[derive(Copy, Clone)]
    enum Victim {
        /// Executing on another CPU, which runs forward to the victim's retire
        /// — its dispatch loop included — before the answer reaches the killer.
        /// The interleaving in which a death recorded after the victim was told
        /// to die arrives after the only point that looks for it: the parent's
        /// `wait` never sees the exit and the process is never reclaimed
        /// (`stress-qemu-aarch64`, `plans/OPEN-DEFECTS.md` D296).
        RetiresBeforeTheKillerReturns,
        /// Executing on another CPU for as long as the test runs: the first
        /// kill is deferred to its retire and every later one finds it doomed.
        StillRunning,
    }

    /// A real scheduler whose `exit` answers as though each victim were on
    /// another CPU, as [`Victim`] says.
    struct KillsElsewhere {
        inner: &'static Scheduler<TestArch>,
        victim: Victim,
        doomed: SpinLock<BTreeSet<SchedTaskId>>,
    }

    impl SchedulerPolicy<TestArch> for KillsElsewhere {
        fn new(config: SchedulerConfig, arch: std::sync::Arc<TestArch>) -> SchedResult<Self> {
            let inner: &'static Scheduler<TestArch> =
                Box::leak(Box::new(Scheduler::new(config, arch)?));
            Ok(Self {
                inner,
                victim: Victim::StillRunning,
                doomed: SpinLock::new(BTreeSet::new()),
            })
        }
        fn cpu_count(&self) -> u32 {
            self.inner.cpu_count()
        }
        fn config(&self) -> SchedulerConfig {
            SchedulerPolicy::config(self.inner)
        }
        fn spawn<F>(&self, cpu: CpuId, priority: Priority, body: F) -> SchedResult<SchedTaskId>
        where
            F: FnMut(&mut TaskContext) -> TaskAction + Send + 'static,
        {
            SchedulerPolicy::spawn(self.inner, cpu, priority, body)
        }
        fn spawn_parked<F>(
            &self,
            cpu: CpuId,
            priority: Priority,
            body: F,
        ) -> SchedResult<SchedTaskId>
        where
            F: FnMut(&mut TaskContext) -> TaskAction + Send + 'static,
        {
            SchedulerPolicy::spawn_parked(self.inner, cpu, priority, body)
        }
        fn spawn_parked_as<F>(
            &self,
            id: SchedTaskId,
            cpu: CpuId,
            priority: Priority,
            body: F,
        ) -> SchedResult<SchedTaskId>
        where
            F: FnMut(&mut TaskContext) -> TaskAction + Send + 'static,
        {
            SchedulerPolicy::spawn_parked_as(self.inner, id, cpu, priority, body)
        }
        fn unpark(&self, id: SchedTaskId) -> SchedResult<()> {
            SchedulerPolicy::unpark(self.inner, id)
        }
        fn stop(&self, id: SchedTaskId) -> SchedResult<()> {
            SchedulerPolicy::stop(self.inner, id)
        }
        fn resume(&self, id: SchedTaskId) -> SchedResult<()> {
            SchedulerPolicy::resume(self.inner, id)
        }
        fn exit(&self, id: SchedTaskId) -> SchedResult<ExitDisposition> {
            match self.victim {
                Victim::RetiresBeforeTheKillerReturns => {
                    match SchedulerPolicy::exit(self.inner, id)? {
                        ExitDisposition::Quiesced => {
                            land_retired_kill(id, || self.inner.state_of(id) == TaskState::Exited);
                            Ok(ExitDisposition::Deferred)
                        }
                        other => Ok(other),
                    }
                }
                Victim::StillRunning if self.doomed.lock().insert(id) => {
                    Ok(ExitDisposition::Deferred)
                }
                Victim::StillRunning => Ok(ExitDisposition::AlreadyExited),
            }
        }
        fn on_timer_tick(&self, cpu: CpuId) -> SchedResult<()> {
            SchedulerPolicy::on_timer_tick(self.inner, cpu)
        }
        fn preemption_count(&self, cpu: CpuId) -> SchedResult<u64> {
            SchedulerPolicy::preemption_count(self.inner, cpu)
        }
        fn total_preemption_count(&self) -> u64 {
            SchedulerPolicy::total_preemption_count(self.inner)
        }
        fn step(&self, cpu: CpuId) -> SchedResult<StepOutcome> {
            SchedulerPolicy::step(self.inner, cpu)
        }
        fn run_count(&self, id: SchedTaskId) -> SchedResult<u64> {
            SchedulerPolicy::run_count(self.inner, id)
        }
        fn cpu_ticks_of(&self, id: SchedTaskId) -> SchedResult<u64> {
            SchedulerPolicy::cpu_ticks_of(self.inner, id)
        }
        fn cpu_busy_ticks(&self, cpu: CpuId) -> SchedResult<u64> {
            SchedulerPolicy::cpu_busy_ticks(self.inner, cpu)
        }
        fn cpu_switches(&self, cpu: CpuId) -> SchedResult<u64> {
            SchedulerPolicy::cpu_switches(self.inner, cpu)
        }
        fn queue_depth(&self, cpu: CpuId) -> SchedResult<u64> {
            SchedulerPolicy::queue_depth(self.inner, cpu)
        }
        fn has_ready_work(&self, cpu: CpuId) -> SchedResult<bool> {
            SchedulerPolicy::has_ready_work(self.inner, cpu)
        }
        fn state_of(&self, id: SchedTaskId) -> TaskState {
            SchedulerPolicy::state_of(self.inner, id)
        }
        fn live_task_count(&self) -> usize {
            SchedulerPolicy::live_task_count(self.inner)
        }
        fn current_task(&self, cpu: CpuId) -> Option<SchedTaskId> {
            SchedulerPolicy::current_task(self.inner, cpu)
        }
        fn set_sched_class(&self, id: SchedTaskId, class: SchedClass) -> SchedResult<()> {
            SchedulerPolicy::set_sched_class(self.inner, id, class)
        }
        fn sched_class(&self, id: SchedTaskId) -> SchedResult<SchedClass> {
            SchedulerPolicy::sched_class(self.inner, id)
        }
        fn set_priority(&self, id: SchedTaskId, priority: Priority) -> SchedResult<()> {
            SchedulerPolicy::set_priority(self.inner, id, priority)
        }
        fn priority(&self, id: SchedTaskId) -> SchedResult<Priority> {
            SchedulerPolicy::priority(self.inner, id)
        }
    }

    #[test]
    fn a_kill_whose_victim_retires_before_the_killer_returns_still_lands() {
        let _g = running_kill_test_lock();
        let ours = ensure_test_lander();
        let (wait, inner) = scaffold();
        let scheduler: &'static KillsElsewhere = Box::leak(Box::new(KillsElsewhere {
            inner,
            victim: Victim::RetiresBeforeTheKillerReturns,
            doomed: SpinLock::new(BTreeSet::new()),
        }));
        let (child, child_pid) = spawn_child(inner);
        wait.register_child(
            ProcessId(7),
            ProcessId(child),
            crate::procwait::ChildListing::Listed,
        )
        .expect("registered");
        let signaller = KernelProcessSignal::without_thread_groups(wait, scheduler);
        LAND_REAPED.lock().remove(&child);

        let target = signaller
            .resolve_child(ProcessId(7), child_pid)
            .expect("a live child");
        assert_eq!(signaller.signal_task(target, Signal::Terminate), Ok(()));

        assert!(!kill_pending(child), "no death left owed after its landing");
        if ours {
            assert_eq!(
                LAND_REAPED.lock().get(&child),
                Some(&143),
                "the retire landed the death the kill recorded"
            );
        }
    }

    /// A group exit that finds a sibling already dying — a kill still
    /// deferred to that sibling's retire — leaves its death to that retire. It
    /// used to land the sibling itself as well, so the retire landed it a
    /// second time: a thread the group table no longer held reads as the
    /// group's last, and the process was torn down twice.
    #[test]
    fn a_group_exit_leaves_a_dying_sibling_to_the_death_it_already_owes() {
        let _g = running_kill_test_lock();
        let (wait, inner) = scaffold();
        let scheduler: &'static KillsElsewhere = Box::leak(Box::new(KillsElsewhere {
            inner,
            victim: Victim::StillRunning,
            doomed: SpinLock::new(BTreeSet::new()),
        }));
        let (leader, _) = spawn_child(inner);
        let (sibling, _) = spawn_child(inner);
        let sink: &'static crate::test_sink::TestSink =
            Box::leak(Box::new(crate::test_sink::TestSink::new()));
        let caps: &'static RwLock<CapTable> = Box::leak(Box::new(RwLock::new(CapTable::new())));
        caps.write()
            .insert(tairix_kernel_sec::TaskCapabilities::derive(
                ProcessId(leader),
                tairix_kernel_sec::UserId(0),
                tairix_caps::CapabilitySet::empty(),
                tairix_caps::CapabilitySet::empty(),
                sink,
            ));
        caps.write()
            .register_thread(TaskId(sibling), ProcessId(leader))
            .expect("the sibling joins the group");
        clear_kill_gate(sibling);
        install_gate(sibling, ProcessId(leader)).expect("the sibling's gate names its group");
        let signaller = KernelProcessSignal::new(wait, scheduler, caps);
        let landed: &'static LandingRecorder = Box::leak(Box::new(LandingRecorder::new()));
        signaller
            .install_task_reclaim(landed)
            .expect("first install on this producer");

        assert_eq!(
            signaller.signal_task(ProcessId(leader), Signal::Terminate),
            Ok(())
        );
        signaller.terminate_siblings(ProcessId(leader), TaskId(leader), 0);

        assert!(
            !landed.landed(sibling, Some(0)) && !landed.landed(sibling, Some(143)),
            "the dying sibling is landed by its own retire alone"
        );
        assert_eq!(take_owed_kill(sibling), Some(exit_of(leader, 143)));
        clear_kill_gate(leader);
    }

    /// The real [`KernelProcessSignal`] lander drives the *same* landing seam
    /// the immediate terminate path does (one definition), passing an `Exit`
    /// teardown's status through and a `Plain` teardown's absence of one — so a
    /// driver unload reclaims without minting a zombie no parent will reap.
    /// Whether a status then reaches the wait table is the landing rule's own
    /// contract, tested against the real implementation in `syscalls`.
    #[test]
    fn the_signal_producer_lander_lands_with_and_without_a_status() {
        let (wait, scheduler) = scaffold();
        let (child, _pid) = spawn_child(scheduler);
        wait.register_child(
            ProcessId(7),
            ProcessId(child),
            crate::procwait::ChildListing::Listed,
        )
        .expect("registered");
        let signaller = KernelProcessSignal::without_thread_groups(wait, scheduler);
        let landed: &'static LandingRecorder = Box::leak(Box::new(LandingRecorder::new()));
        signaller
            .install_task_reclaim(landed)
            .expect("first install on this producer");

        signaller.land_deferred_teardown(
            TaskId(child),
            DeferredTeardown::Exit {
                process: ProcessId(child),
                status: 137,
            },
        );
        assert!(landed.landed(child, Some(137)));

        // A driver unload whose process was still executing: a second,
        // unregistered id, landed with no status.
        let driver = scheduler
            .spawn(0, Priority::Normal, |_| TaskAction::Exit)
            .expect("driver task");
        signaller.land_deferred_teardown(
            TaskId(driver),
            DeferredTeardown::Plain {
                process: ProcessId(driver),
            },
        );
        assert!(landed.landed(driver, None));
    }

    #[test]
    fn kill_records_its_own_status() {
        let (wait, scheduler) = scaffold();
        let (child, child_pid) = spawn_child(scheduler);
        wait.register_child(
            ProcessId(7),
            ProcessId(child),
            crate::procwait::ChildListing::Listed,
        )
        .expect("registered");
        let signaller = KernelProcessSignal::without_thread_groups(wait, scheduler);

        assert_eq!(
            signal_child(&signaller, ProcessId(7), child_pid, Signal::Kill),
            Ok(())
        );
        let pid = child;
        // Kill surfaces as SIGKILL's familiar 137, distinct from Terminate.
        assert_eq!(
            wait.wait(
                ProcessId(7),
                TaskId(7),
                tairix_abi::WAIT_PID_ANY,
                WaitFlags::empty()
            ),
            Ok(WaitedChild {
                pid,
                status: WaitStatus::Exited(137)
            })
        );
    }

    #[test]
    fn interrupt_terminates_with_the_ctrl_c_status() {
        let (wait, scheduler) = scaffold();
        let (child, child_pid) = spawn_child(scheduler);
        wait.register_child(
            ProcessId(7),
            ProcessId(child),
            crate::procwait::ChildListing::Listed,
        )
        .expect("registered");
        let signaller = KernelProcessSignal::without_thread_groups(wait, scheduler);

        assert_eq!(
            signal_child(&signaller, ProcessId(7), child_pid, Signal::Interrupt),
            Ok(())
        );
        assert_eq!(scheduler.live_task_count(), 0);
        let pid = child;
        // Interrupt surfaces as the `^C` 130 every POSIX shell reports.
        assert_eq!(
            wait.wait(
                ProcessId(7),
                TaskId(7),
                tairix_abi::WAIT_PID_ANY,
                WaitFlags::empty()
            ),
            Ok(WaitedChild {
                pid,
                status: WaitStatus::Exited(130)
            })
        );
    }

    #[test]
    fn stop_holds_and_reports_and_continue_releases_it() {
        let (wait, scheduler) = scaffold();
        let (child, child_pid) = spawn_child(scheduler);
        wait.register_child(
            ProcessId(7),
            ProcessId(child),
            crate::procwait::ChildListing::Listed,
        )
        .expect("registered");
        let signaller = KernelProcessSignal::without_thread_groups(wait, scheduler);

        assert_eq!(
            signal_child(&signaller, ProcessId(7), child_pid, Signal::Stop),
            Ok(())
        );
        // The scheduler holds the child stopped, so no wake runs it.
        assert!(scheduler.state_of(child).is_stopped());
        assert_eq!(
            scheduler.unpark(child),
            Ok(()),
            "a broadcast wake reaches it"
        );
        assert!(
            scheduler.state_of(child).is_stopped(),
            "and leaves it stopped"
        );
        assert_eq!(scheduler.step(0), Ok(StepOutcome::Idle), "never dispatched");
        // The child is still live (stopped, not terminated) …
        assert_eq!(scheduler.live_task_count(), 1);
        // … and a STOPPED wait observes the stop without reaping.
        assert_eq!(
            wait.poll(
                ProcessId(7),
                tairix_abi::WAIT_PID_ANY,
                WaitFlags::from_bits(WaitFlags::NONBLOCK.bits() | WaitFlags::STOPPED.bits())
                    .expect("defined bits")
            ),
            Ok(WaitedChild {
                pid: child,
                status: WaitStatus::Stopped(Signal::Stop)
            })
        );
        // Continue resumes the child.
        assert_eq!(
            signal_child(&signaller, ProcessId(7), child_pid, Signal::Continue),
            Ok(())
        );
        assert!(!scheduler.state_of(child).is_stopped());
        assert_eq!(scheduler.live_task_count(), 1);
    }

    #[test]
    fn continue_clears_a_stop_the_parent_never_observed() {
        let (wait, scheduler) = scaffold();
        let (child, child_pid) = spawn_child(scheduler);
        wait.register_child(
            ProcessId(7),
            ProcessId(child),
            crate::procwait::ChildListing::Listed,
        )
        .expect("registered");
        let signaller = KernelProcessSignal::without_thread_groups(wait, scheduler);

        assert_eq!(
            signal_child(&signaller, ProcessId(7), child_pid, Signal::Stop),
            Ok(())
        );
        assert_eq!(
            signal_child(&signaller, ProcessId(7), child_pid, Signal::Continue),
            Ok(())
        );
        // The unobserved stop was cleared by the resume: nothing to report.
        assert_eq!(
            wait.poll(
                ProcessId(7),
                tairix_abi::WAIT_PID_ANY,
                WaitFlags::from_bits(WaitFlags::NONBLOCK.bits() | WaitFlags::STOPPED.bits())
                    .expect("defined bits")
            ),
            Err(Errno::WouldBlock)
        );
    }

    #[test]
    fn a_stopped_child_can_still_be_killed() {
        let (wait, scheduler) = scaffold();
        let (child, child_pid) = spawn_child(scheduler);
        wait.register_child(
            ProcessId(7),
            ProcessId(child),
            crate::procwait::ChildListing::Listed,
        )
        .expect("registered");
        let signaller = KernelProcessSignal::without_thread_groups(wait, scheduler);

        assert_eq!(
            signal_child(&signaller, ProcessId(7), child_pid, Signal::Stop),
            Ok(())
        );
        assert!(scheduler.state_of(child).is_stopped());
        assert_eq!(
            signal_child(&signaller, ProcessId(7), child_pid, Signal::Kill),
            Ok(())
        );
        assert_eq!(scheduler.state_of(child), TaskState::Exited, "it died");
        // The terminal exit superseded the unobserved stop.
        assert_eq!(
            wait.wait(
                ProcessId(7),
                TaskId(7),
                tairix_abi::WAIT_PID_ANY,
                WaitFlags::STOPPED
            ),
            Ok(WaitedChild {
                pid: child,
                status: WaitStatus::Exited(137)
            })
        );
    }

    /// A stop landing after a kill claimed a thread's death — and after the
    /// kill's own resume — is withdrawn rather than holding a thread that must
    /// still reach its boundary, and is not reported as a stop.
    #[test]
    fn a_stop_never_holds_a_thread_that_owes_a_death() {
        let _gate = running_kill_test_lock();
        let (wait, scheduler) = scaffold();
        let (child, child_pid) = spawn_child(scheduler);
        wait.register_child(
            ProcessId(7),
            ProcessId(child),
            crate::procwait::ChildListing::Listed,
        )
        .expect("registered");
        let signaller = KernelProcessSignal::without_thread_groups(wait, scheduler);

        assert!(
            gate_in_hand(child).enter() == Owed::Nothing,
            "inside a kernel body, owing nothing yet"
        );
        assert_eq!(claim_one(exit_of(child, 137)).site, KillSite::Boundary);
        assert_eq!(
            signal_child(&signaller, ProcessId(7), child_pid, Signal::Stop),
            Ok(())
        );
        assert!(!scheduler.state_of(child).is_stopped(), "withdrawn");
        assert_eq!(
            wait.poll(
                ProcessId(7),
                tairix_abi::WAIT_PID_ANY,
                WaitFlags::from_bits(WaitFlags::NONBLOCK.bits() | WaitFlags::STOPPED.bits())
                    .expect("defined bits")
            ),
            Err(Errno::WouldBlock),
            "no stop to report"
        );

        assert_eq!(
            left(&gate_in_hand(child)).and_then(DeferredTeardown::reaped_status),
            Some(137)
        );
        clear_kill_gate(child);
    }

    fn stopped_flags() -> WaitFlags {
        WaitFlags::from_bits(WaitFlags::NONBLOCK.bits() | WaitFlags::STOPPED.bits())
            .expect("defined bits")
    }

    /// A stop never stops a thread inside a kernel body. Such a thread may hold
    /// kernel state another thread waits on — a lock handed to it while it
    /// slept — and stopped there it would hold that state until the continue,
    /// closing a shared disk to every user. The scheduler leaves it parked, a
    /// wake runs it on, and it owes the stop at its edge, while the job reads
    /// stopped at once.
    #[test]
    fn a_stop_leaves_a_thread_inside_a_kernel_body_to_its_edge() {
        let (wait, scheduler) = scaffold();
        let id = scheduler
            .spawn_parked(0, Priority::Normal, |_ctx| TaskAction::Exit)
            .expect("admitted");
        let held = gate(id);
        wait.register_child(
            ProcessId(7),
            ProcessId(id),
            crate::procwait::ChildListing::Listed,
        )
        .expect("registered");
        let signaller = KernelProcessSignal::without_thread_groups(wait, scheduler);

        assert_eq!(held.enter(), Owed::Nothing, "parked inside a kernel body");
        let pid = id.cast_signed();
        assert_eq!(
            signal_child(&signaller, ProcessId(7), pid, Signal::Stop),
            Ok(())
        );
        assert_eq!(scheduler.state_of(id), TaskState::Parked, "left parked");
        assert_eq!(scheduler.unpark(id), Ok(()), "the lock's handoff wakes it");
        assert_eq!(
            scheduler.state_of(id),
            TaskState::Ready,
            "to run on and release it"
        );
        assert_eq!(held.leave(true), Leave::Stop, "its edge owes the stop");
        assert_eq!(
            wait.poll(ProcessId(7), tairix_abi::WAIT_PID_ANY, stopped_flags()),
            Ok(WaitedChild {
                pid: id,
                status: WaitStatus::Stopped(Signal::Stop)
            })
        );

        assert_eq!(
            signal_child(&signaller, ProcessId(7), pid, Signal::Continue),
            Ok(())
        );
        assert_eq!(
            held.leave(true),
            Leave::Left,
            "the continue ended what it owed"
        );
        clear_kill_gate(id);
    }

    /// A stop's fan-out that falls behind a continue's carries an older
    /// generation, and changes nothing the continue decided.
    #[test]
    fn a_stop_fan_out_overtaken_by_a_continue_cannot_undo_it() {
        let (wait, scheduler) = scaffold();
        let (child, child_pid) = spawn_child(scheduler);
        wait.register_child(
            ProcessId(7),
            ProcessId(child),
            crate::procwait::ChildListing::Listed,
        )
        .expect("registered");
        let signaller = KernelProcessSignal::without_thread_groups(wait, scheduler);
        let held = gate_in_hand(child);

        let (stop, moved) = held.advance_job(true);
        assert!(moved && stop.is_stopped(), "the stop is decided");
        assert_eq!(
            signal_child(&signaller, ProcessId(7), child_pid, Signal::Continue),
            Ok(())
        );
        held.apply_job(stop);
        assert_eq!(held.owed(), Owed::Nothing, "the late fan-out is refused");
        assert!(
            !signaller.reconcile(child, &held),
            "so the thread does not hold"
        );
        assert!(!scheduler.state_of(child).is_stopped());
        clear_kill_gate(child);
    }

    /// A stop decided on a read that a kill then overtakes — the thread
    /// entering its body, the kill claiming its death there and resuming it,
    /// all before the stop lands — is taken back. Left standing, it held a
    /// thread owing a death nobody would bring it to, and the process was
    /// never reclaimed.
    #[test]
    fn a_stale_stop_a_kill_overtakes_is_taken_back() {
        let _gate = running_kill_test_lock();
        let (wait, scheduler) = scaffold();
        let (child, _) = spawn_child(scheduler);
        wait.register_child(
            ProcessId(7),
            ProcessId(child),
            crate::procwait::ChildListing::Listed,
        )
        .expect("registered");
        let signaller = KernelProcessSignal::without_thread_groups(wait, scheduler);
        let held = gate_in_hand(child);
        let (stop, _) = held.advance_job(true);
        assert!(stop.is_stopped());

        let mut raced = false;
        let holds = signaller.reconcile_deciding(child, &held, |hold| {
            if hold == JobHold::Hold && !raced {
                raced = true;
                assert_eq!(held.enter(), Owed::Stop, "the thread enters its body");
                let claim = claim_one(exit_of(child, 137));
                assert_eq!(claim.site, KillSite::Boundary);
                assert!(signaller.stop_claimed(claim), "the kill resumes it");
            }
        });
        assert!(raced);
        assert!(!holds, "a thread owing a death does not hold");
        assert!(
            !scheduler.state_of(child).is_stopped(),
            "the stale stop was taken back"
        );
        assert_eq!(
            left(&held).and_then(DeferredTeardown::reaped_status),
            Some(137),
            "its boundary lands the death"
        );
        clear_kill_gate(child);
    }

    /// A stop decided on a read that the thread's entry into a kernel body
    /// then overtakes — a continue lets it in, it parks there on a lock, and a
    /// newer stop finds it inside — is taken back. Left standing, it held the
    /// thread parked inside the body, so the lock handed to it stayed held
    /// until the continue.
    #[test]
    fn a_stale_stop_a_kernel_entry_overtakes_is_taken_back() {
        let (wait, scheduler) = scaffold();
        let id = scheduler
            .spawn_parked(0, Priority::Normal, |_ctx| TaskAction::Exit)
            .expect("admitted");
        let held = gate(id);
        wait.register_child(
            ProcessId(7),
            ProcessId(id),
            crate::procwait::ChildListing::Listed,
        )
        .expect("registered");
        let signaller = KernelProcessSignal::without_thread_groups(wait, scheduler);
        let (stop, _) = held.advance_job(true);

        let mut raced = false;
        let holds = signaller.reconcile_deciding(id, &held, |hold| {
            if hold == JobHold::Hold && !raced {
                raced = true;
                let continued = stop.toward(false);
                held.apply_job(continued);
                assert_eq!(held.enter(), Owed::Nothing, "the thread enters its body");
                held.apply_job(continued.toward(true));
            }
        });
        assert!(raced);
        assert!(holds, "the job is stopped, at the thread's edge");
        assert_eq!(
            scheduler.state_of(id),
            TaskState::Parked,
            "the stale stop was taken back"
        );
        assert_eq!(scheduler.unpark(id), Ok(()), "the lock's handoff wakes it");
        assert_eq!(scheduler.state_of(id), TaskState::Ready, "to release it");
        assert_eq!(
            held.leave(true),
            Leave::Stop,
            "its edge owes the newer stop"
        );
        clear_kill_gate(id);
    }

    /// A body whose owed stop cannot be taken at its edge still never runs
    /// for a thread a death has been recorded against meanwhile.
    #[test]
    fn a_body_whose_edge_stop_fails_still_skips_for_a_death() {
        let _gate = running_kill_test_lock();
        let racing = ThreadGate::new(4, ProcessId(4));
        let _ = racing.advance_job(true);
        let ran = enter_body_through(&racing, |gate| {
            let (recorded, site) = gate.claim(exit_of(4, 137));
            assert!(recorded);
            assert_eq!(site, KillSite::Boundary);
            false
        });
        assert!(!ran, "the death skips the body");
        assert_eq!(
            left(&racing).and_then(DeferredTeardown::reaped_status),
            Some(137)
        );
    }

    /// A thread joining its group takes the group's generation however far
    /// the group has come: compared rather than adopted, a group past half the
    /// generation space reads older than a fresh gate's, and the thread runs
    /// through every stop.
    #[test]
    fn a_joining_thread_adopts_its_groups_generation_however_far_it_has_come() {
        let joining = ThreadGate::new(5, ProcessId(5));
        let far = JobGeneration::from_bits((1 << (JobGeneration::BITS - 1)) | 1);
        joining.adopt_job(far);
        assert_eq!(joining.owed(), Owed::Stop, "it joins stopped");
        joining.apply_job(far.toward(false));
        assert_eq!(
            joining.owed(),
            Owed::Nothing,
            "and follows the next continue"
        );
    }

    /// A hook that stops and resumes a real scheduler, for driving an edge
    /// stop without a kthread; `on_stop` lands a racing call just after the
    /// thread has stopped itself.
    struct EdgeHook {
        scheduler: &'static Scheduler<TestArch>,
        on_stop: SpinLock<Option<Box<dyn FnOnce() + Send>>>,
    }

    impl crate::waitq::WaitQueueArch for EdgeHook {
        fn unpark(&self, id: SchedTaskId) -> bool {
            self.scheduler.unpark(id).is_ok()
        }

        fn now_ns(&self) -> u64 {
            0
        }

        fn set_wakeup(&self, _deadline_ns: Option<u64>) {}

        fn current_cpu(&self) -> Option<CpuId> {
            Some(0)
        }

        fn stop(&self, id: SchedTaskId) -> bool {
            let stopped = self.scheduler.stop(id).is_ok();
            if let Some(racing) = self.on_stop.lock().take() {
                racing();
            }
            stopped
        }

        fn resume(&self, id: SchedTaskId) -> bool {
            self.scheduler.resume(id).is_ok()
        }
    }

    /// Run `edge` as the body of a task stopping itself at a kernel edge, so
    /// the task is on its CPU, as a thread taking an edge stop always is.
    fn at_an_edge(
        edge: impl FnOnce(&'static Scheduler<TestArch>, Arc<ThreadGate>) + Send + 'static,
    ) {
        let (_wait, scheduler) = scaffold();
        let ran = Arc::new(AtomicBool::new(false));
        let body_ran = Arc::clone(&ran);
        let mut edge = Some(edge);
        let id_cell = Arc::new(AtomicU64::new(0));
        let body_id = Arc::clone(&id_cell);
        let id = scheduler
            .spawn(0, Priority::Normal, move |_ctx| {
                if let Some(edge) = edge.take() {
                    edge(scheduler, gate_in_hand(body_id.load(Ordering::Acquire)));
                    body_ran.store(true, Ordering::Release);
                }
                TaskAction::Exit
            })
            .expect("admitted");
        let _ = gate(id);
        id_cell.store(id, Ordering::Release);
        assert_eq!(scheduler.step(0), Ok(StepOutcome::Ran(id)));
        assert!(
            ran.load(Ordering::Acquire),
            "the edge ran on the task's CPU"
        );
        clear_kill_gate(id);
    }

    /// A continue that lands between the thread stopping itself and its
    /// read of the gate withdraws the stop there: the thread never suspends.
    #[test]
    fn an_edge_stop_a_continue_overtakes_is_withdrawn() {
        at_an_edge(|scheduler, held| {
            let (stop, _) = held.advance_job(true);
            let racing = Arc::clone(&held);
            let hook = EdgeHook {
                scheduler,
                on_stop: SpinLock::new(Some(Box::new(move || {
                    racing.apply_job(stop.toward(false));
                }))),
            };
            assert!(stop_through(&held, &hook, |_cpu| panic!(
                "withdrawn, never suspended"
            )));
            assert_eq!(scheduler.state_of(held.task), TaskState::Running);
        });
    }

    /// A stop the gate still owes once the thread has stopped itself suspends
    /// it until the continue, which resumes it.
    #[test]
    fn an_edge_stop_holds_until_the_continue() {
        at_an_edge(|scheduler, held| {
            let (stop, _) = held.advance_job(true);
            let hook = EdgeHook {
                scheduler,
                on_stop: SpinLock::new(None),
            };
            let mut suspended = 0;
            assert!(stop_through(&held, &hook, |_cpu| {
                suspended += 1;
                assert_eq!(scheduler.state_of(held.task), TaskState::StoppedOnCpu);
                held.apply_job(stop.toward(false));
                assert_eq!(scheduler.resume(held.task), Ok(()), "the continue's resume");
                true
            }));
            assert_eq!(suspended, 1);
            assert_eq!(held.owed(), Owed::Nothing);
        });
    }

    /// A death is counted before it is on the gate, where a taker can reach
    /// it. Counted after, a take landing in between drove the count below the
    /// deaths owed, and a dispatch reading it zero left another thread's death
    /// unlanded for good.
    #[test]
    fn a_death_is_counted_before_anyone_can_take_it() {
        let _gate = running_kill_test_lock();
        let racing = ThreadGate::new(2, ProcessId(2));
        let (recorded, _) = racing.claim_then(
            DeferredTeardown::Plain {
                process: ProcessId(2),
            },
            || {
                assert!(
                    OWED_KILLS.load(Ordering::Relaxed) >= 1,
                    "takeable before it was counted"
                );
            },
        );
        assert!(recorded);
        assert!(racing.take_owed().is_some());
    }

    #[test]
    fn foreground_deliver_maps_only_the_line_discipline_signals() {
        let (wait, scheduler) = scaffold();
        let (child, _child_pid) = spawn_child(scheduler);
        wait.register_child(
            ProcessId(7),
            ProcessId(child),
            crate::procwait::ChildListing::Listed,
        )
        .expect("registered");
        let signaller = KernelProcessSignal::without_thread_groups(wait, scheduler);

        // The console path never delivers Continue/Terminate/Kill.
        for signal in [Signal::Continue, Signal::Terminate, Signal::Kill] {
            assert_eq!(signaller.deliver(fg(child), signal), Err(Errno::OutOfRange));
        }
        // `^Z` stops the foreground task …
        assert_eq!(signaller.deliver(fg(child), Signal::Stop), Ok(()));
        assert!(scheduler.state_of(child).is_stopped());
        assert_eq!(scheduler.live_task_count(), 1);
        // … and `^C` terminates it with the 130 status.
        assert_eq!(signaller.deliver(fg(child), Signal::Interrupt), Ok(()));
        assert_eq!(scheduler.live_task_count(), 0);
        assert_eq!(
            wait.wait(
                ProcessId(7),
                TaskId(7),
                tairix_abi::WAIT_PID_ANY,
                WaitFlags::empty()
            ),
            Ok(WaitedChild {
                pid: child,
                status: WaitStatus::Exited(130)
            })
        );
    }

    #[test]
    fn foreground_deliver_to_a_dead_target_fails_closed() {
        let (wait, scheduler) = scaffold();
        let signaller = KernelProcessSignal::without_thread_groups(wait, scheduler);
        // Task 9999 was never admitted: the delivery reaches no one.
        assert_eq!(
            signaller.deliver(fg(9999), Signal::Interrupt),
            Err(Errno::NotFound)
        );
        assert_eq!(
            signaller.deliver(fg(9999), Signal::Stop),
            Err(Errno::NotFound)
        );
        // A refused stop stops nothing.
        assert!(!scheduler.state_of(9999).is_stopped());
    }

    /// Register `process` in `caps` as a live process instance, so the
    /// foreground delivery gate can resolve an instance for it.
    fn admit_instance(
        caps: &RwLock<CapTable>,
        process: ProcessId,
        instance: ProcId,
    ) -> ForegroundOwner {
        let sink = crate::test_sink::TestSink::new();
        let record = tairix_kernel_sec::TaskCapabilities::derive(
            process,
            tairix_kernel_sec::UserId(0),
            tairix_caps::CapabilitySet::empty(),
            tairix_caps::CapabilitySet::empty(),
            &sink,
        )
        .with_proc_id(instance);
        caps.write().insert(record);
        ForegroundOwner { process, instance }
    }

    /// A `^C` aimed at one process instance is refused once the pid names a
    /// different one — the id may be drawn again after its task is gone, and
    /// a mis-delivered kill would land on whoever inherited the number.
    #[test]
    fn foreground_deliver_refuses_a_stale_process_instance() {
        let (wait, scheduler) = scaffold();
        let caps: &'static RwLock<CapTable> = Box::leak(Box::new(RwLock::new(CapTable::new())));
        let (child, _child_pid) = spawn_child(scheduler);
        wait.register_child(
            ProcessId(7),
            ProcessId(child),
            crate::procwait::ChildListing::Listed,
        )
        .expect("registered");
        let signaller: &KernelProcessSignal<TestArch, Scheduler<TestArch>> =
            Box::leak(Box::new(KernelProcessSignal::new(wait, scheduler, caps)));

        // The keystroke was aimed at the instance the grant saw …
        let aimed_at = admit_instance(caps, ProcessId(child), ProcId::from_raw([0x11; 16]));
        // … but by delivery time the pid names a different one.
        admit_instance(caps, ProcessId(child), ProcId::from_raw([0x22; 16]));
        assert_eq!(
            signaller.deliver(aimed_at, Signal::Interrupt),
            Err(Errno::NotFound)
        );
        // The successor is untouched: neither killed nor stopped.
        assert_eq!(scheduler.live_task_count(), 1);
        assert!(!scheduler.state_of(child).is_stopped());
        assert_eq!(
            signaller.deliver(aimed_at, Signal::Stop),
            Err(Errno::NotFound)
        );
        assert!(!scheduler.state_of(child).is_stopped());

        // A record the table no longer holds is equally refused: an absent
        // instance never matches a real one.
        caps.write().remove(ProcessId(child));
        assert_eq!(
            signaller.deliver(aimed_at, Signal::Interrupt),
            Err(Errno::NotFound)
        );
        assert_eq!(scheduler.live_task_count(), 1);
    }

    /// The same gate passes the signal through when the pid still names the
    /// instance the ownership was granted at.
    #[test]
    fn foreground_deliver_reaches_the_instance_it_was_aimed_at() {
        let (wait, scheduler) = scaffold();
        let caps: &'static RwLock<CapTable> = Box::leak(Box::new(RwLock::new(CapTable::new())));
        let (child, _child_pid) = spawn_child(scheduler);
        wait.register_child(
            ProcessId(7),
            ProcessId(child),
            crate::procwait::ChildListing::Listed,
        )
        .expect("registered");
        let signaller: &KernelProcessSignal<TestArch, Scheduler<TestArch>> =
            Box::leak(Box::new(KernelProcessSignal::new(wait, scheduler, caps)));

        let aimed_at = admit_instance(caps, ProcessId(child), ProcId::from_raw([0x33; 16]));
        assert_eq!(signaller.deliver(aimed_at, Signal::Stop), Ok(()));
        assert!(scheduler.state_of(child).is_stopped());
        assert_eq!(signaller.deliver(aimed_at, Signal::Interrupt), Ok(()));
        assert_eq!(scheduler.live_task_count(), 0);
    }

    #[test]
    fn queued_foreground_signal_round_trips_through_the_pending_slot() {
        // The pending slot is process-global, so serialise against the
        // console line-discipline tests that share it.
        let _guard = foreground_test_lock();
        queue_foreground_signal(fg(77), Signal::Interrupt);
        // The drain consumes the slot (delivering only if a boot-style hook
        // was installed by another test; either way the slot empties).
        drain_pending_foreground();
        // A second drain finds the slot empty.
        assert!(!drain_pending_foreground());
    }

    #[test]
    fn continue_of_a_running_child_is_a_harmless_success() {
        let (wait, scheduler) = scaffold();
        let (child, child_pid) = spawn_child(scheduler);
        wait.register_child(
            ProcessId(7),
            ProcessId(child),
            crate::procwait::ChildListing::Listed,
        )
        .expect("registered");
        let signaller = KernelProcessSignal::without_thread_groups(wait, scheduler);

        // The child is runnable, not stopped, so Continue succeeds as a no-op
        // (it neither terminates the child nor records an exit).
        assert_eq!(
            signal_child(&signaller, ProcessId(7), child_pid, Signal::Continue),
            Ok(())
        );
        assert_eq!(scheduler.live_task_count(), 1);
        // The child is still a live, signallable child (no status recorded).
        assert_eq!(
            wait.authorise_child(ProcessId(7), child_pid),
            Ok(ProcessId(child))
        );
    }

    /// A child already registered with its parent whose record is not yet
    /// published has no thread a signal may reach: a continue and a stop find
    /// nothing, as a kill does, rather than waking or parking the task its
    /// number names behind the admission that owns it.
    #[test]
    fn no_signal_reaches_a_child_whose_record_is_unpublished() {
        let (wait, scheduler) = scaffold();
        let caps: &'static RwLock<CapTable> = Box::leak(Box::new(RwLock::new(CapTable::new())));
        let child = scheduler
            .spawn_parked(0, Priority::Normal, |_| TaskAction::Exit)
            .expect("admitted parked");
        wait.register_child(
            ProcessId(7),
            ProcessId(child),
            crate::procwait::ChildListing::Listed,
        )
        .expect("registered");
        let signaller = KernelProcessSignal::new(wait, scheduler, caps);

        for signal in [Signal::Continue, Signal::Stop, Signal::Kill] {
            assert_eq!(
                signal_child(&signaller, ProcessId(7), child.cast_signed(), signal),
                Err(Errno::NotFound),
                "{signal:?}"
            );
        }
        assert_eq!(scheduler.state_of(child), TaskState::Parked, "never woken");
        assert_eq!(
            wait.authorise_child(ProcessId(7), child.cast_signed()),
            Ok(ProcessId(child)),
            "nothing was recorded against the child"
        );
    }

    /// Serialises host tests that touch the process-global signal-intake
    /// map ([`SIGNAL_INTAKE`]): it is keyed by numeric task id, and each
    /// test's own leaked scheduler hands out the same small ids, so two
    /// tests observing "their" intake in parallel would read and clear
    /// each other's entries. Every test that enables, drains, or delivers
    /// through an intake takes this lock and clears its ids on entry.
    fn intake_test_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        // A panicking holder does not corrupt the `()` state; continue.
        LOCK.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    #[test]
    fn intake_lifecycle_is_idempotent_and_fail_closed() {
        let _intake = intake_test_lock();
        clear_intake(4001);
        // Take and disable before any opt-in: no intake exists.
        assert_eq!(intake_take(4001), Err(Errno::NotFound));
        assert!(!intake_enabled(4001));
        assert!(!intake_ready(4001));
        assert_eq!(intake_disable(4001), Ok(()));
        // Enable is idempotent; nothing is pending yet.
        intake_enable(4001);
        intake_enable(4001);
        assert!(intake_enabled(4001));
        assert!(!intake_ready(4001));
        assert_eq!(intake_take(4001), Err(Errno::WouldBlock));
        // A recorded signal is observable, drains exactly once, and a
        // re-enable never discards it.
        assert!(try_intake(4001, Signal::Interrupt));
        intake_enable(4001);
        assert!(intake_ready(4001));
        assert_eq!(intake_take(4001), Ok(Signal::Interrupt));
        assert_eq!(intake_take(4001), Err(Errno::WouldBlock));
        // Disable removes the opt-in; a later delivery goes default again.
        assert_eq!(intake_disable(4001), Ok(()));
        assert!(!try_intake(4001, Signal::Terminate));
        clear_intake(4001);
    }

    #[test]
    fn disable_with_a_pending_signal_is_refused_until_drained() {
        let _intake = intake_test_lock();
        clear_intake(4002);
        intake_enable(4002);
        assert!(try_intake(4002, Signal::Terminate));
        // A recorded termination request is never silently discarded.
        assert_eq!(intake_disable(4002), Err(Errno::WouldBlock));
        assert!(intake_enabled(4002));
        assert_eq!(intake_take(4002), Ok(Signal::Terminate));
        assert_eq!(intake_disable(4002), Ok(()));
        clear_intake(4002);
    }

    #[test]
    fn a_second_pending_termination_request_is_declined_for_escalation() {
        let _intake = intake_test_lock();
        clear_intake(4003);
        intake_enable(4003);
        assert!(try_intake(4003, Signal::Interrupt));
        // The slot is occupied: the second request is declined so the
        // caller escalates to the default terminate path (`^C ^C` kills).
        assert!(!try_intake(4003, Signal::Interrupt));
        assert!(!try_intake(4003, Signal::Terminate));
        // The first observation is still intact for the drain.
        assert_eq!(intake_take(4003), Ok(Signal::Interrupt));
        clear_intake(4003);
    }

    #[test]
    fn opted_in_interrupt_is_observed_not_fatal_and_kill_still_kills() {
        let _intake = intake_test_lock();
        let (wait, scheduler) = scaffold();
        let (child, child_pid) = spawn_child(scheduler);
        clear_intake(child);
        wait.register_child(
            ProcessId(7),
            ProcessId(child),
            crate::procwait::ChildListing::Listed,
        )
        .expect("registered");
        let signaller = KernelProcessSignal::without_thread_groups(wait, scheduler);

        intake_enable(child);
        // The interrupt is recorded, not delivered as a termination: the
        // child stays live and nothing becomes reapable.
        assert_eq!(
            signal_child(&signaller, ProcessId(7), child_pid, Signal::Interrupt),
            Ok(())
        );
        assert_eq!(scheduler.live_task_count(), 1);
        assert_eq!(
            wait.poll(ProcessId(7), tairix_abi::WAIT_PID_ANY, WaitFlags::NONBLOCK),
            Err(Errno::WouldBlock)
        );
        assert_eq!(intake_take(child), Ok(Signal::Interrupt));
        // `Kill` is unconditionally fatal regardless of the opt-in and
        // reaps with SIGKILL's familiar 137.
        assert_eq!(
            signal_child(&signaller, ProcessId(7), child_pid, Signal::Kill),
            Ok(())
        );
        assert_eq!(scheduler.live_task_count(), 0);
        assert_eq!(
            wait.wait(
                ProcessId(7),
                TaskId(7),
                tairix_abi::WAIT_PID_ANY,
                WaitFlags::empty()
            ),
            Ok(WaitedChild {
                pid: child,
                status: WaitStatus::Exited(137)
            })
        );
        clear_intake(child);
    }

    #[test]
    fn a_second_interrupt_escalates_to_the_default_terminate() {
        let _intake = intake_test_lock();
        let (wait, scheduler) = scaffold();
        let (child, child_pid) = spawn_child(scheduler);
        clear_intake(child);
        wait.register_child(
            ProcessId(7),
            ProcessId(child),
            crate::procwait::ChildListing::Listed,
        )
        .expect("registered");
        let signaller = KernelProcessSignal::without_thread_groups(wait, scheduler);

        intake_enable(child);
        assert_eq!(
            signal_child(&signaller, ProcessId(7), child_pid, Signal::Interrupt),
            Ok(())
        );
        assert_eq!(scheduler.live_task_count(), 1);
        // The second interrupt finds the slot occupied and terminates the
        // child with the `^C` 130 — an unresponsive opted-in program stays
        // killable with plain `^C ^C`.
        assert_eq!(
            signal_child(&signaller, ProcessId(7), child_pid, Signal::Interrupt),
            Ok(())
        );
        assert_eq!(scheduler.live_task_count(), 0);
        assert_eq!(
            wait.wait(
                ProcessId(7),
                TaskId(7),
                tairix_abi::WAIT_PID_ANY,
                WaitFlags::empty()
            ),
            Ok(WaitedChild {
                pid: child,
                status: WaitStatus::Exited(130)
            })
        );
        clear_intake(child);
    }

    #[test]
    fn foreground_interrupt_reaches_an_opted_in_target_without_killing_it() {
        let _intake = intake_test_lock();
        let (wait, scheduler) = scaffold();
        let (child, _child_pid) = spawn_child(scheduler);
        clear_intake(child);
        wait.register_child(
            ProcessId(7),
            ProcessId(child),
            crate::procwait::ChildListing::Listed,
        )
        .expect("registered");
        let signaller = KernelProcessSignal::without_thread_groups(wait, scheduler);

        intake_enable(child);
        // The console `^C` is observed, not fatal …
        assert_eq!(signaller.deliver(fg(child), Signal::Interrupt), Ok(()));
        assert_eq!(scheduler.live_task_count(), 1);
        assert_eq!(intake_take(child), Ok(Signal::Interrupt));
        // … and `^Z` still stops the opted-in target (only termination
        // requests are observable; `Stop` stays scheduler-side).
        assert_eq!(signaller.deliver(fg(child), Signal::Stop), Ok(()));
        assert!(scheduler.state_of(child).is_stopped());
        assert_eq!(signaller.deliver(fg(child), Signal::Interrupt), Ok(()));
        assert_eq!(intake_take(child), Ok(Signal::Interrupt));
        clear_intake(child);
    }

    #[test]
    fn a_signalled_child_cannot_be_signalled_twice() {
        let (wait, scheduler) = scaffold();
        let (child, child_pid) = spawn_child(scheduler);
        wait.register_child(
            ProcessId(7),
            ProcessId(child),
            crate::procwait::ChildListing::Listed,
        )
        .expect("registered");
        let signaller = KernelProcessSignal::without_thread_groups(wait, scheduler);

        assert_eq!(
            signal_child(&signaller, ProcessId(7), child_pid, Signal::Terminate),
            Ok(())
        );
        // Once terminated the child is a zombie awaiting reap, not a live
        // process: a second signal fails closed rather than re-terminating it.
        assert_eq!(
            signal_child(&signaller, ProcessId(7), child_pid, Signal::Kill),
            Err(Errno::NotFound)
        );
    }

    /// A session a test drives: the table, the producer that kills through
    /// it, the landing seam that records what died, and the audit sink.
    struct SessionScene {
        caps: &'static RwLock<CapTable>,
        signaller: &'static KernelProcessSignal<TestArch, Scheduler<TestArch>>,
        landed: &'static LandingRecorder,
        audit: &'static crate::test_sink::TestSink,
        scheduler: &'static Scheduler<TestArch>,
    }

    impl SessionScene {
        fn new() -> Self {
            let (wait, scheduler) = scaffold();
            let caps: &'static RwLock<CapTable> = Box::leak(Box::new(RwLock::new(CapTable::new())));
            let signaller: &'static KernelProcessSignal<TestArch, Scheduler<TestArch>> =
                Box::leak(Box::new(KernelProcessSignal::new(wait, scheduler, caps)));
            let landed: &'static LandingRecorder = Box::leak(Box::new(LandingRecorder::new()));
            signaller
                .install_task_reclaim(landed)
                .expect("first install on this producer");
            Self {
                caps,
                signaller,
                landed,
                audit: Box::leak(Box::new(crate::test_sink::TestSink::new())),
                scheduler,
            }
        }

        /// Admit a live task as instance `byte`, spawned by `spawner` (the
        /// kernel when `None`) and placed as `session` asks.
        fn admit(&self, byte: u8, spawner: Option<u64>, session: tairix_abi::SpawnSession) -> u64 {
            let (task, _) = spawn_child(self.scheduler);
            let record = tairix_kernel_sec::TaskCapabilities::derive(
                ProcessId(task),
                tairix_kernel_sec::UserId(1000),
                tairix_caps::CapabilitySet::empty(),
                tairix_caps::CapabilitySet::empty(),
                self.audit,
            )
            .with_proc_id(ProcId::from_raw([byte; 16]));
            let mut caps = self.caps.write();
            let placement = caps
                .resolve_placement(spawner.map_or(ProcessId::KERNEL, ProcessId), session)
                .expect("placeable");
            caps.admit(record, placement).expect("placed");
            task
        }

        fn end(&self, byte: u8) {
            self.end_pausing(byte, &mut || {});
        }

        fn end_pausing(&self, byte: u8, pause: &mut dyn FnMut()) {
            end_session_through(
                self.caps,
                self.audit,
                ProcId::from_raw([byte; 16]),
                self.signaller,
                pause,
            );
        }

        fn ended(&self) -> usize {
            self.audit
                .event_ids()
                .iter()
                .filter(|&&id| id == crate::audit::AuditEvent::SessionMemberEnded.id().0)
                .count()
        }
    }

    use tairix_abi::SpawnSession::{Inherit, New};

    /// The desktop dying ends every app it started, and every shell a terminal
    /// among them anchors, and nothing outside its session.
    #[test]
    fn an_anchor_dying_kills_its_session_and_every_session_nested_in_it() {
        let _g = running_kill_test_lock();
        let scene = SessionScene::new();
        let desktop = scene.admit(0x31, None, New);
        let app = scene.admit(0x32, Some(desktop), Inherit);
        let shell = scene.admit(0x33, Some(desktop), New);
        let job = scene.admit(0x34, Some(shell), Inherit);
        let outsider = scene.admit(0x35, None, New);

        scene.caps.write().remove(ProcessId(desktop));
        scene.end(0x31);

        for member in [app, shell, job] {
            assert!(
                scene.landed.landed(member, Some(137)),
                "member {member} was killed"
            );
        }
        assert!(!scene.landed.landed(outsider, Some(137)));
        assert_eq!(scene.ended(), 3, "each death recorded once");
        for task in [app, shell, job] {
            clear_kill_gate(task);
        }
    }

    /// A nested session whose enclosing one is ending leaves its members to
    /// that walk, so a cascade of ends stays one teardown deep.
    #[test]
    fn a_nested_session_leaves_its_end_to_the_enclosing_walk() {
        let _g = running_kill_test_lock();
        let scene = SessionScene::new();
        let desktop = scene.admit(0x41, None, New);
        let shell = scene.admit(0x42, Some(desktop), New);
        let job = scene.admit(0x43, Some(shell), Inherit);

        scene.caps.write().remove(ProcessId(desktop));
        scene.caps.write().remove(ProcessId(shell));
        scene.end(0x42);
        assert!(!scene.landed.landed(job, Some(137)));
        assert_eq!(scene.ended(), 0);

        scene.end(0x41);
        assert!(scene.landed.landed(job, Some(137)));
        clear_kill_gate(job);
    }

    /// An anchor that was the last of its session ends nothing, and neither
    /// does a session whose anchor lives.
    #[test]
    fn a_session_ends_only_once_its_anchor_is_gone_and_only_with_members_left() {
        let _g = running_kill_test_lock();
        let scene = SessionScene::new();
        let lone = scene.admit(0x51, None, New);
        let anchor = scene.admit(0x52, None, New);
        let app = scene.admit(0x53, Some(anchor), Inherit);

        scene.end(0x52);
        assert!(
            !scene.landed.landed(app, Some(137)),
            "the anchor still lives"
        );
        scene.caps.write().remove(ProcessId(lone));
        scene.end(0x51);
        assert_eq!(scene.ended(), 0);
    }

    /// A member already dying of something else is reached by the walk but
    /// not recorded as ended with its session: its death is not the session's.
    #[test]
    fn a_member_already_dying_is_not_recorded_as_ended_with_its_session() {
        let _g = running_kill_test_lock();
        let scene = SessionScene::new();
        let desktop = scene.admit(0x71, None, New);
        let app = scene.admit(0x72, Some(desktop), Inherit);
        let own = claim_group_kill(Some(scene.caps), exit_of(app, 0), None);
        assert!(own.iter().all(|claim| claim.recorded));

        scene.caps.write().remove(ProcessId(desktop));
        scene.end(0x71);
        assert_eq!(scene.ended(), 0, "the app's own exit stands");
        clear_kill_gate(app);
    }

    /// A walk takes its members a batch at a time, releasing the table between
    /// batches, and a member that departs while it walks is skipped rather
    /// than killed twice or lost track of.
    #[test]
    fn a_walk_past_one_batch_ends_every_member_whatever_departs_under_it() {
        let _g = running_kill_test_lock();
        let scene = SessionScene::new();
        let desktop = scene.admit(0x80, None, New);
        let members: Vec<u64> = (0x81..=0x91)
            .map(|byte| scene.admit(byte, Some(desktop), Inherit))
            .collect();
        assert!(members.len() > 2 * SESSION_WALK_BATCH);
        // The walk goes in process-id order, and ids are drawn at random. Both
        // leave at its first pause: one the batch in hand already holds, one a
        // later batch would have collected.
        let mut walk = members.clone();
        walk.sort_unstable();
        let leavers = [walk[1], walk[walk.len() - 3]];

        scene.caps.write().remove(ProcessId(desktop));
        let mut left = false;
        scene.end_pausing(0x80, &mut || {
            if !left {
                for leaver in leavers {
                    assert!(scene.caps.write().remove(ProcessId(leaver)).is_some());
                }
                left = true;
            }
        });
        for &member in &members {
            assert_eq!(
                scene.landed.landed(member, Some(137)),
                !leavers.contains(&member),
                "member {member}"
            );
        }
        assert_eq!(scene.ended(), members.len() - leavers.len());
        for member in members {
            clear_kill_gate(member);
        }
    }

    /// A kill aimed at an instance is never claimed for a number drawn again by
    /// a different one.
    #[test]
    fn an_instance_kill_claims_only_the_instance_it_names() {
        let _g = running_kill_test_lock();
        let caps: &'static RwLock<CapTable> = Box::leak(Box::new(RwLock::new(CapTable::new())));
        let owner = admit_instance(caps, ProcessId(0x6161), ProcId::from_raw([0x61; 16]));
        let _held = gate(0x6161);
        let teardown = exit_of(0x6161, 137);
        assert!(claim_instance_kill(caps, ProcId::from_raw([0x62; 16]), teardown).is_empty());
        assert!(!kill_pending(0x6161));
        assert_eq!(claim_instance_kill(caps, owner.instance, teardown).len(), 1);
        assert!(kill_pending(0x6161));
        clear_kill_gate(0x6161);
    }
}
