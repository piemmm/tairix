//! The task lifecycle every policy shares: the park/unpark handshake, the
//! job-control stop, taking a task from a run-queue entry, and the settle
//! after a body returns.
//!
//! Parking and stopping are the task lifecycle, not scheduling policy: which
//! CPU a woken task lands on differs per policy, but the window between
//! "decide to park" and "actually parked" — and the token that closes it — is
//! identical, as is how a stop is completed by whichever party next holds the
//! task, and how a returning body's request is reconciled with whatever a
//! stop, wake or kill did while it ran. One definition here, so the three
//! policies carry only their own placement.

use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use core::cell::Cell;
use core::sync::atomic::{fence, AtomicBool, Ordering};

use tairix_sync::RwLock;

use crate::{CpuId, SchedError, SchedResult, SchedulerArch, TaskAction, TaskId, TaskState};

/// The lifecycle state and wake token the handshake needs from a policy's own
/// per-task record.
pub trait ParkableTask {
    /// The task's current lifecycle state.
    fn load_state(&self) -> TaskState;

    /// Compare-exchange the state, reporting the observed value on failure.
    fn cas_state(&self, expected: TaskState, new: TaskState) -> Result<(), TaskState>;

    /// Store `new` unconditionally, returning the state it replaced.
    fn swap_state(&self, new: TaskState) -> TaskState;

    /// Record that a wake arrived before the task committed to park, so the
    /// next park is cancelled.
    fn set_wake_pending(&self);

    /// Consume the wake token, reporting whether one was set.
    fn take_wake_pending(&self) -> bool;

    /// Whether the task holds a place in a CPU's competition — queued or on
    /// a CPU, including a stop not yet completed — which is the condition a
    /// policy counts its weight under.
    fn in_competition(&self) -> bool {
        matches!(
            self.load_state(),
            TaskState::Ready
                | TaskState::Running
                | TaskState::StoppedOnQueue
                | TaskState::StoppedOnCpu
        )
    }
}

/// End a committed park with a wake: a parked task is made ready and
/// admitted, and one a stop holds is left owed its run for the `resume`.
///
/// Each transition is a compare-exchange, which keeps the admission single
/// when two wakers — or a waker and the task's own park commit — reach it
/// together, and a stop or `resume` landing in between is followed to the
/// state it left.
fn deliver_wake<T, F>(task: &T, admit: F)
where
    T: ParkableTask + ?Sized,
    F: FnOnce(&T),
{
    loop {
        match task.load_state() {
            TaskState::Parked => {
                if task.cas_state(TaskState::Parked, TaskState::Ready).is_ok() {
                    admit(task);
                    return;
                }
            }
            TaskState::StoppedParked => {
                if task
                    .cas_state(TaskState::StoppedParked, TaskState::Stopped)
                    .is_ok()
                {
                    return;
                }
            }
            _ => return,
        }
    }
}

/// Make `task` runnable, admitting it through `admit` when this call is the
/// one that claimed it out of [`TaskState::Parked`].
///
/// Cancellation-safe: a wake of a task that has not yet committed to park
/// records a token rather than erroring, so it is never lost. A wake of a
/// stopped task leaves it stopped — only `resume` ends a stop — but is kept: a
/// [`TaskState::StoppedParked`] task becomes [`TaskState::Stopped`], runnable
/// once resumed, and a [`TaskState::Stopped`] one keeps the token, so a park
/// it commits once resumed does not sleep through the wake that arrived
/// meanwhile.
///
/// # Errors
/// [`SchedError::InvalidState`] only when the task is terminal, which is the
/// one answer meaning it can never run again. A wake another waker (or
/// the task's own park commit) already satisfied is `Ok` — the task is
/// runnable, which is what the wake asked for. Reading that as a failure is
/// what let a wait-queue ownership handoff mistake a live waiter for a corpse
/// and delete its registration.
pub fn unpark_task<T, F>(task: &T, admit: F) -> SchedResult<()>
where
    T: ParkableTask + ?Sized,
    F: FnOnce(&T),
{
    match task.load_state() {
        TaskState::Exited => Err(SchedError::InvalidState),
        TaskState::Stopped => {
            task.set_wake_pending();
            Ok(())
        }
        // Already committed to park: end the park directly.
        TaskState::Parked | TaskState::StoppedParked => {
            deliver_wake(task, admit);
            match task.load_state() {
                TaskState::Exited => Err(SchedError::InvalidState),
                _ => Ok(()),
            }
        }
        // Not yet committed (running its body, or already queued, its stop
        // not yet completed). Record the token, then re-read the state: this
        // store-then-load against [`commit_park`]'s store-then-take, fenced on
        // each side, forbids the store-buffering outcome where the waker sees
        // the task not-yet-parked *and* the parker misses the token — one side
        // always observes the other. A stop that a `resume` withdraws before
        // the body returns leaves that body to park, so its token must stand.
        TaskState::Ready
        | TaskState::Running
        | TaskState::StoppedOnQueue
        | TaskState::StoppedOnCpu => {
            task.set_wake_pending();
            fence(Ordering::SeqCst);
            if matches!(
                task.load_state(),
                TaskState::Parked | TaskState::StoppedParked
            ) && task.take_wake_pending()
            {
                deliver_wake(task, admit);
            }
            Ok(())
        }
    }
}

/// Complete a park the dispatcher has just published, re-admitting the task
/// through `admit` when a wake raced the commit.
///
/// The caller stores [`TaskState::Parked`] — or [`TaskState::StoppedParked`],
/// for a body that parked as a stop landed — itself, together with whatever
/// per-CPU accounting the transition owes, and calls this to consume any token
/// a waker left behind.
pub fn commit_park<T, F>(task: &T, admit: F)
where
    T: ParkableTask + ?Sized,
    F: FnOnce(&T),
{
    fence(Ordering::SeqCst);
    if task.take_wake_pending() {
        deliver_wake(task, admit);
    }
}

/// Stop `task` for job control: only [`resume_task`] (or an exit) ends it.
///
/// A queued task keeps its entry and a running one its CPU until the
/// scheduler next reaches it — the party that takes the entry, or the
/// dispatch whose body returns, completes the stop — so a stop never leaves a
/// stale entry for a later wake to queue beside, and no weight moves here. A
/// parked task holds neither and is stopped at once, still parked.
///
/// Returns the stop state the task is in afterwards. A task left
/// [`TaskState::StoppedOnCpu`] is still executing, so the caller preempts its
/// CPU ([`nudge_running`]): alone on a tickless core it may have no next
/// quantum to stop at. Nothing else here touches the task's CPU, its
/// current-task slot or its body lock, all of which belong to the dispatch
/// running it.
///
/// # Errors
/// [`SchedError::InvalidState`] if the task is terminal. Stopping a task
/// already stopped is `Ok`.
pub fn stop_task<T>(task: &T) -> SchedResult<TaskState>
where
    T: ParkableTask + ?Sized,
{
    loop {
        let observed = task.load_state();
        let stopped = match observed {
            TaskState::Exited => return Err(SchedError::InvalidState),
            TaskState::Stopped
            | TaskState::StoppedOnQueue
            | TaskState::StoppedOnCpu
            | TaskState::StoppedParked => return Ok(observed),
            TaskState::Ready => TaskState::StoppedOnQueue,
            TaskState::Running => TaskState::StoppedOnCpu,
            TaskState::Parked => TaskState::StoppedParked,
        };
        if task.cas_state(observed, stopped).is_ok() {
            return Ok(stopped);
        }
    }
}

/// End a job-control stop, admitting the task through `admit` when the stop
/// had completed on a runnable task, which then holds no entry.
///
/// A stop the scheduler has yet to complete is simply withdrawn: the task's
/// entry, or its body on a CPU, still stands. A task parked under the stop is
/// left parked, for the wake it is waiting on. Resuming a task that is not
/// stopped is `Ok` and changes nothing.
///
/// # Errors
/// [`SchedError::InvalidState`] if the task is terminal.
pub fn resume_task<T, F>(task: &T, admit: F) -> SchedResult<()>
where
    T: ParkableTask + ?Sized,
    F: FnOnce(&T),
{
    loop {
        let observed = task.load_state();
        let resumed = match observed {
            TaskState::Exited => return Err(SchedError::InvalidState),
            TaskState::Ready | TaskState::Running | TaskState::Parked => return Ok(()),
            TaskState::StoppedOnQueue => TaskState::Ready,
            TaskState::StoppedOnCpu => TaskState::Running,
            TaskState::StoppedParked => TaskState::Parked,
            TaskState::Stopped => {
                if task.cas_state(TaskState::Stopped, TaskState::Ready).is_ok() {
                    admit(task);
                    return Ok(());
                }
                continue;
            }
        };
        if task.cas_state(observed, resumed).is_ok() {
            return Ok(());
        }
    }
}

/// What the holder of a run-queue entry it has just removed owes the task.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Taken {
    /// The task was ready and `take` claimed it: the holder runs it, or
    /// keeps its entry where it moved it.
    Ready,
    /// The task is [`TaskState::Exited`] and the entry was its last.
    Exited,
    /// Nothing is owed: a stop the entry held is now complete, or the entry
    /// was stale.
    Spent,
}

/// Decide a run-queue entry for `task` its caller has just removed.
///
/// A task stopped while queued is still owed that entry, so its stop is
/// completed here; a stop withdrawn before then leaves it `Ready` with the
/// entry the caller holds, which `take` then claims. `take` claims a ready
/// task for the caller and reports whether it did — a dispatch moves it to
/// `Running`, a steal moves its weight — and is offered the task again after
/// any transition beats it. `depart` performs a departure from the
/// competition together with the policy's weight accounting.
pub fn take_entry<T, C, D>(task: &T, take: C, depart: D) -> Taken
where
    T: ParkableTask + ?Sized,
    C: Fn() -> bool,
    D: Fn(&dyn Fn() -> bool) -> bool,
{
    loop {
        match task.load_state() {
            TaskState::Ready => {
                if take() {
                    return Taken::Ready;
                }
            }
            TaskState::StoppedOnQueue => {
                if depart(&|| {
                    task.cas_state(TaskState::StoppedOnQueue, TaskState::Stopped)
                        .is_ok()
                }) {
                    return Taken::Spent;
                }
            }
            TaskState::Exited => return Taken::Exited,
            TaskState::Running
            | TaskState::Parked
            | TaskState::Stopped
            | TaskState::StoppedOnCpu
            | TaskState::StoppedParked => return Taken::Spent,
        }
    }
}

/// What a dispatcher owes a task whose body has just returned.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Settled {
    /// The task is [`TaskState::Exited`]: retire it.
    Retire,
    /// The task is [`TaskState::Parked`] by its own request — or
    /// [`TaskState::StoppedParked`], for one a stop landed on: finish with
    /// [`commit_park`].
    Park,
    /// The task yielded and is [`TaskState::Ready`] again: enqueue it.
    Requeue,
    /// Nothing is owed: a stop that landed while the body ran is complete,
    /// or another party already holds the task.
    Released,
}

/// Request a task's termination through its `doomed` mark, reporting whether
/// this call was the first to.
///
/// The caller probes the task's body lock only after this returns. The fence
/// pairs with [`observe_doom`]'s: either the dispatch holding the body reads
/// the mark when its body returns, or the probe finds the body already
/// released. Without the pairing the probe can see the body held while the
/// run reads the mark stale and queues the task again, so a kill reported as
/// deferred to that run is never carried out by it.
#[must_use]
pub fn doom(mark: &AtomicBool) -> bool {
    let first = !mark.swap(true, Ordering::AcqRel);
    fence(Ordering::SeqCst);
    first
}

/// Read a task's `doomed` mark for [`settle`], once its dispatch has released
/// the body lock; the other half of [`doom`]'s pairing.
#[must_use]
pub fn observe_doom(mark: &AtomicBool) -> bool {
    fence(Ordering::SeqCst);
    mark.load(Ordering::Acquire)
}

/// Preempt the CPU executing a task this caller has just doomed or stopped, so
/// it reaches its stopping point now: alone on a tickless core it may have no
/// next quantum to stop at.
///
/// `running` is the CPU whose current-task slot names the task. A dispatch
/// publishes that slot only after claiming the task, and nothing orders the
/// slot for a caller that saw only the claim or the body lock, so the task can
/// be on a CPU while no slot the caller reads names it; every CPU is signalled
/// then, since missing the one running the task leaves it running.
pub fn nudge_running<A: SchedulerArch + ?Sized>(arch: &A, running: Option<CpuId>, cpus: u32) {
    match running {
        Some(cpu) => arch.send_ipi(cpu),
        None => (0..cpus).for_each(|cpu| arch.send_ipi(cpu)),
    }
}

/// Retire `task` for an exit that holds its body lock, returning the state it
/// left. [`TaskState::Exited`] means another party retired it first and owns
/// its teardown. `depart` is [`settle`]'s.
pub fn retire<T, D>(task: &T, depart: D) -> TaskState
where
    T: ParkableTask + ?Sized,
    D: Fn(&dyn Fn() -> bool) -> bool,
{
    let left = Cell::new(TaskState::Exited);
    depart(&|| {
        left.set(task.swap_state(TaskState::Exited));
        true
    });
    left.get()
}

/// Remove the record of `task`, admitted as `id`, from a policy's registry —
/// unless the id has since been drawn again and names another task.
pub fn drop_record<T>(tasks: &RwLock<BTreeMap<TaskId, Arc<T>>>, id: TaskId, task: &T) {
    let mut tasks = tasks.write();
    if tasks
        .get(&id)
        .is_some_and(|held| core::ptr::eq(Arc::as_ptr(held), task))
    {
        tasks.remove(&id);
    }
}

/// Move a task whose body just returned `action` off its CPU.
///
/// A dispatch owns its task until it settles: no entry exists for a running
/// task, so only a stop, its withdrawal, or an exit can land while the body
/// unwinds. Each transition is a compare-exchange from the state it was
/// decided on, so one landing in between is honoured rather than overwritten.
/// `doomed` — a termination requested while the task ran, read through
/// [`observe_doom`] — wins over anything but its own park, since a task parked
/// inside a syscall holds kernel state only its own unwind can release. A stop
/// wins over the body's own yield or park, so no ordinary wake can run the
/// task; a park it asked for is kept beneath the stop, for the wake it waits
/// on to end. `depart` performs a transition out of the
/// competition together with whatever competing-weight accounting the policy
/// keeps, and returns the transition's result.
pub fn settle<T, D>(task: &T, action: TaskAction, doomed: bool, depart: D) -> Settled
where
    T: ParkableTask + ?Sized,
    D: Fn(&dyn Fn() -> bool) -> bool,
{
    let exit = action == TaskAction::Exit || (doomed && action != TaskAction::Park);
    loop {
        let observed = task.load_state();
        let settled = match observed {
            TaskState::Exited => return Settled::Retire,
            _ if exit => depart(&|| task.cas_state(observed, TaskState::Exited).is_ok())
                .then_some(Settled::Retire),
            TaskState::StoppedOnCpu if action == TaskAction::Park => depart(&|| {
                task.cas_state(TaskState::StoppedOnCpu, TaskState::StoppedParked)
                    .is_ok()
            })
            .then_some(Settled::Park),
            TaskState::StoppedOnCpu => depart(&|| {
                task.cas_state(TaskState::StoppedOnCpu, TaskState::Stopped)
                    .is_ok()
            })
            .then_some(Settled::Released),
            TaskState::Running if action == TaskAction::Park => depart(&|| {
                task.cas_state(TaskState::Running, TaskState::Parked)
                    .is_ok()
            })
            .then_some(Settled::Park),
            TaskState::Running => task
                .cas_state(TaskState::Running, TaskState::Ready)
                .is_ok()
                .then_some(Settled::Requeue),
            // No transition reaches these from a task on a CPU; whoever put
            // the task there owns its next step.
            TaskState::Parked | TaskState::StoppedParked => return Settled::Park,
            TaskState::Ready | TaskState::Stopped | TaskState::StoppedOnQueue => {
                return Settled::Released
            }
        };
        if let Some(settled) = settled {
            return settled;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A bare `ParkableTask` with no scheduler behind it: the handshake is
    /// pure state, so the model needs nothing more.
    struct Model {
        state: Cell<TaskState>,
        token: Cell<bool>,
        admits: Cell<usize>,
    }

    impl Model {
        fn new(state: TaskState) -> Self {
            Self {
                state: Cell::new(state),
                token: Cell::new(false),
                admits: Cell::new(0),
            }
        }

        fn admit(&self) {
            self.admits.set(self.admits.get() + 1);
        }
    }

    impl ParkableTask for Model {
        fn load_state(&self) -> TaskState {
            self.state.get()
        }

        fn cas_state(&self, expected: TaskState, new: TaskState) -> Result<(), TaskState> {
            let current = self.state.get();
            if current == expected {
                self.state.set(new);
                Ok(())
            } else {
                Err(current)
            }
        }

        fn swap_state(&self, new: TaskState) -> TaskState {
            self.state.replace(new)
        }

        fn set_wake_pending(&self) {
            self.token.set(true);
        }

        fn take_wake_pending(&self) -> bool {
            self.token.replace(false)
        }
    }

    /// A task whose first compare-exchange loses to `beaten_by`, landing in
    /// between the read and the exchange; every later one is honest.
    struct Raced {
        state: Cell<TaskState>,
        beaten_by: TaskState,
        lost: Cell<bool>,
        admits: Cell<usize>,
    }

    impl Raced {
        fn new(state: TaskState, beaten_by: TaskState) -> Self {
            Self {
                state: Cell::new(state),
                beaten_by,
                lost: Cell::new(false),
                admits: Cell::new(0),
            }
        }

        fn admit(&self) {
            self.admits.set(self.admits.get() + 1);
        }
    }

    impl ParkableTask for Raced {
        fn load_state(&self) -> TaskState {
            self.state.get()
        }

        fn cas_state(&self, expected: TaskState, new: TaskState) -> Result<(), TaskState> {
            if !self.lost.replace(true) {
                self.state.set(self.beaten_by);
                return Err(self.beaten_by);
            }
            let current = self.state.get();
            if current == expected {
                self.state.set(new);
                Ok(())
            } else {
                Err(current)
            }
        }

        fn swap_state(&self, new: TaskState) -> TaskState {
            self.state.replace(new)
        }

        fn set_wake_pending(&self) {}

        fn take_wake_pending(&self) -> bool {
            false
        }
    }

    /// A departure that performs the transition and counts it when it happens.
    fn counting(departures: &Cell<usize>) -> impl Fn(&dyn Fn() -> bool) -> bool + '_ {
        move |transition: &dyn Fn() -> bool| {
            let departed = transition();
            if departed {
                departures.set(departures.get() + 1);
            }
            departed
        }
    }

    #[test]
    fn a_task_competes_while_it_holds_a_queue_entry_or_a_cpu() {
        for state in [
            TaskState::Ready,
            TaskState::Running,
            TaskState::StoppedOnQueue,
            TaskState::StoppedOnCpu,
        ] {
            assert!(Model::new(state).in_competition(), "{state:?}");
        }
        for state in [TaskState::Parked, TaskState::Stopped, TaskState::Exited] {
            assert!(!Model::new(state).in_competition(), "{state:?}");
        }
    }

    #[test]
    fn a_parked_task_is_claimed_and_admitted_once() {
        let task = Model::new(TaskState::Parked);
        assert_eq!(unpark_task(&task, Model::admit), Ok(()));
        assert_eq!(task.load_state(), TaskState::Ready);
        assert_eq!(task.admits.get(), 1);
        // A second wake finds it runnable: nothing to admit, still success.
        assert_eq!(unpark_task(&task, Model::admit), Ok(()));
        assert_eq!(task.admits.get(), 1);
    }

    #[test]
    fn a_wake_before_the_park_commits_cancels_it() {
        let task = Model::new(TaskState::Running);
        assert_eq!(unpark_task(&task, Model::admit), Ok(()));
        assert!(task.token.get(), "the token is owed to the coming park");
        // The dispatcher publishes the park and consumes the token, so the
        // task is re-admitted rather than slept.
        task.state.set(TaskState::Parked);
        commit_park(&task, Model::admit);
        assert_eq!(task.load_state(), TaskState::Ready);
        assert_eq!(task.admits.get(), 1);
    }

    #[test]
    fn a_park_with_no_token_takes_effect() {
        let task = Model::new(TaskState::Parked);
        commit_park(&task, Model::admit);
        assert_eq!(task.load_state(), TaskState::Parked);
        assert_eq!(task.admits.get(), 0, "no wake was owed");
    }

    #[test]
    fn a_wake_whose_claim_lost_to_another_waker_still_reports_success() {
        // The defect this contract exists for: the task was `Parked` when the
        // state was read and `Ready` by the time the claim ran, because
        // another waker got there first. It is live and runnable, so the wake
        // landed — reporting an error here is what let an ownership handoff
        // delete a live waiter's wait-queue row.
        let task = Raced::new(TaskState::Parked, TaskState::Ready);
        assert_eq!(
            unpark_task(&task, |_| unreachable!("the claim lost, so no admit")),
            Ok(())
        );
    }

    #[test]
    fn a_wake_whose_claim_lost_to_a_retirement_fails_closed() {
        let task = Raced::new(TaskState::Parked, TaskState::Exited);
        assert_eq!(
            unpark_task(&task, |_| unreachable!("a corpse is never admitted")),
            Err(SchedError::InvalidState)
        );
    }

    #[test]
    fn a_terminal_task_can_never_run_again() {
        let task = Model::new(TaskState::Exited);
        assert_eq!(
            unpark_task(&task, Model::admit),
            Err(SchedError::InvalidState)
        );
        assert_eq!(task.admits.get(), 0);
    }

    /// A broadcast wake reaching a stopped task leaves it stopped: the state
    /// itself refuses the wake, so nothing need check again when the task is
    /// next dispatched.
    #[test]
    fn a_wake_leaves_a_stopped_task_stopped() {
        let task = Model::new(TaskState::Stopped);
        assert_eq!(unpark_task(&task, Model::admit), Ok(()));
        assert_eq!(task.load_state(), TaskState::Stopped);
        assert_eq!(task.admits.get(), 0);
    }

    /// A thread stopped at the edge of a syscall that asked to park commits
    /// that park once a continue resumes it. A wake that reached it while it
    /// was stopped cancels the park rather than being lost to it, which would
    /// leave the thread asleep on a wake already delivered.
    #[test]
    fn a_wake_while_stopped_cancels_the_park_committed_after_the_continue() {
        let task = Model::new(TaskState::Stopped);
        assert_eq!(unpark_task(&task, Model::admit), Ok(()));
        assert_eq!(resume_task(&task, Model::admit), Ok(()));
        assert_eq!(
            task.load_state(),
            TaskState::Ready,
            "the continue re-runs it"
        );
        task.state.set(TaskState::Running);
        let departures = Cell::new(0);
        assert_eq!(
            settle(&task, TaskAction::Park, false, counting(&departures)),
            Settled::Park
        );
        commit_park(&task, Model::admit);
        assert_eq!(task.load_state(), TaskState::Ready, "the wake was not lost");
        assert_eq!(
            task.admits.get(),
            2,
            "admitted by the continue and the wake"
        );
    }

    /// A wake that reaches a running task whose stop is then withdrawn is
    /// still owed to the park its body goes on to commit.
    #[test]
    fn a_wake_over_a_withdrawn_stop_cancels_the_coming_park() {
        let task = Model::new(TaskState::StoppedOnCpu);
        assert_eq!(unpark_task(&task, Model::admit), Ok(()));
        assert_eq!(resume_task(&task, Model::admit), Ok(()));
        assert_eq!(task.load_state(), TaskState::Running);
        let departures = Cell::new(0);
        assert_eq!(
            settle(&task, TaskAction::Park, false, counting(&departures)),
            Settled::Park
        );
        commit_park(&task, Model::admit);
        assert_eq!(task.load_state(), TaskState::Ready, "the wake was not lost");
        assert_eq!(task.admits.get(), 1);
    }

    #[test]
    fn a_stop_holds_each_state_where_the_scheduler_can_complete_it() {
        for (from, to) in [
            (TaskState::Ready, TaskState::StoppedOnQueue),
            (TaskState::Running, TaskState::StoppedOnCpu),
            (TaskState::Parked, TaskState::StoppedParked),
        ] {
            let task = Model::new(from);
            assert_eq!(stop_task(&task), Ok(to), "{from:?}");
            assert_eq!(task.load_state(), to, "{from:?}");
            assert_eq!(
                stop_task(&task),
                Ok(to),
                "{from:?}: a repeat stops nothing and reports where the task is"
            );
            assert_eq!(task.load_state(), to, "{from:?}");
        }
        assert_eq!(
            stop_task(&Model::new(TaskState::Exited)),
            Err(SchedError::InvalidState)
        );
    }

    #[test]
    fn a_stop_that_loses_a_race_is_decided_again() {
        let task = Raced::new(TaskState::Running, TaskState::Parked);
        assert_eq!(stop_task(&task), Ok(TaskState::StoppedParked));
        assert_eq!(task.load_state(), TaskState::StoppedParked);
    }

    /// A stop holds a parked task where it is: the continue leaves it parked,
    /// for the wake it waits on, and admits nothing. Resumed as runnable, a
    /// thread born parked would run before its creator had made it whole, and
    /// a waiter would run with nothing woken.
    #[test]
    fn a_parked_task_resumed_from_a_stop_is_parked_again() {
        let task = Model::new(TaskState::Parked);
        assert_eq!(stop_task(&task), Ok(TaskState::StoppedParked));
        assert_eq!(resume_task(&task, Model::admit), Ok(()));
        assert_eq!(task.load_state(), TaskState::Parked);
        assert_eq!(task.admits.get(), 0, "nothing woke it");
    }

    /// A wake reaching a task parked under a stop is kept for the continue,
    /// which then runs it; the stop still holds it until then.
    #[test]
    fn a_wake_while_parked_under_a_stop_runs_it_once_resumed() {
        let task = Model::new(TaskState::Parked);
        assert_eq!(stop_task(&task), Ok(TaskState::StoppedParked));
        assert_eq!(unpark_task(&task, Model::admit), Ok(()));
        assert_eq!(task.load_state(), TaskState::Stopped, "still held");
        assert_eq!(task.admits.get(), 0);
        assert_eq!(resume_task(&task, Model::admit), Ok(()));
        assert_eq!(task.load_state(), TaskState::Ready);
        assert_eq!(task.admits.get(), 1, "admitted once, by the continue");
    }

    /// A wake whose exchange loses to a continue follows the task to the park
    /// the continue left it in and ends that, so it is not lost.
    #[test]
    fn a_wake_racing_the_continue_of_a_stopped_park_is_kept() {
        let task = Raced::new(TaskState::StoppedParked, TaskState::Parked);
        assert_eq!(unpark_task(&task, Raced::admit), Ok(()));
        assert_eq!(task.load_state(), TaskState::Ready);
        assert_eq!(task.admits.get(), 1);
    }

    /// A continue whose exchange loses to a wake finds the task owed its run,
    /// and admits it.
    #[test]
    fn a_continue_racing_a_wake_of_a_stopped_park_runs_it() {
        let task = Raced::new(TaskState::StoppedParked, TaskState::Stopped);
        assert_eq!(resume_task(&task, Raced::admit), Ok(()));
        assert_eq!(task.load_state(), TaskState::Ready);
        assert_eq!(task.admits.get(), 1);
    }

    /// An exit decides its retirement on the state its swap replaced, so one
    /// that finds the task already retired owns no teardown.
    #[test]
    fn a_retirement_reports_the_state_it_ended() {
        for from in [
            TaskState::Ready,
            TaskState::Running,
            TaskState::Parked,
            TaskState::Stopped,
            TaskState::StoppedOnQueue,
            TaskState::StoppedOnCpu,
            TaskState::StoppedParked,
            TaskState::Exited,
        ] {
            let task = Model::new(from);
            let departures = Cell::new(0);
            assert_eq!(retire(&task, counting(&departures)), from, "{from:?}");
            assert_eq!(task.load_state(), TaskState::Exited, "{from:?}");
            assert_eq!(
                departures.get(),
                1,
                "{from:?}: weight leaves through depart"
            );
        }
    }

    /// A record goes only with the task it was admitted for: once an id is
    /// drawn again, the old task's last reference leaves the new one's alone.
    #[test]
    fn a_record_is_dropped_only_for_the_task_it_holds() {
        let tasks: RwLock<BTreeMap<TaskId, Arc<u32>>> = RwLock::new(BTreeMap::new());
        let old = Arc::new(1);
        let new = Arc::new(2);
        tasks.write().insert(7, Arc::clone(&new));
        drop_record(&tasks, 7, &*old);
        assert!(
            tasks.read().contains_key(&7),
            "a reissued id keeps its task"
        );
        drop_record(&tasks, 7, &*new);
        assert!(!tasks.read().contains_key(&7));
        drop_record(&tasks, 7, &*new);
        assert!(tasks.read().is_empty(), "a repeat drop finds nothing");
    }

    #[test]
    fn a_resume_withdraws_an_open_stop_and_requeues_a_completed_one() {
        for (from, to, admits) in [
            (TaskState::StoppedOnQueue, TaskState::Ready, 0),
            (TaskState::StoppedOnCpu, TaskState::Running, 0),
            (TaskState::Stopped, TaskState::Ready, 1),
        ] {
            let task = Model::new(from);
            assert_eq!(resume_task(&task, Model::admit), Ok(()), "{from:?}");
            assert_eq!(task.load_state(), to, "{from:?}");
            assert_eq!(task.admits.get(), admits, "{from:?}");
        }
        for state in [TaskState::Ready, TaskState::Running, TaskState::Parked] {
            let task = Model::new(state);
            assert_eq!(resume_task(&task, Model::admit), Ok(()), "{state:?}");
            assert_eq!(task.load_state(), state, "{state:?}: not stopped");
            assert_eq!(task.admits.get(), 0, "{state:?}");
        }
        assert_eq!(
            resume_task(&Model::new(TaskState::Exited), Model::admit),
            Err(SchedError::InvalidState)
        );
    }

    /// The race that makes a stop on a queued task safe: the entry's holder
    /// completes the stop just before the resume withdraws it, and the resume
    /// then re-queues the task rather than trusting an entry that is gone.
    #[test]
    fn a_resume_that_loses_to_the_entrys_holder_requeues_the_task() {
        let task = Raced::new(TaskState::StoppedOnQueue, TaskState::Stopped);
        assert_eq!(resume_task(&task, Raced::admit), Ok(()));
        assert_eq!(task.load_state(), TaskState::Ready);
        assert_eq!(task.admits.get(), 1);
    }

    #[test]
    fn a_taken_entry_runs_a_ready_task_and_completes_a_queued_stop() {
        let departures = Cell::new(0);
        let task = Model::new(TaskState::Ready);
        let take = || task.cas_state(TaskState::Ready, TaskState::Running).is_ok();
        assert_eq!(take_entry(&task, take, counting(&departures)), Taken::Ready);
        assert_eq!(task.load_state(), TaskState::Running);

        let task = Model::new(TaskState::StoppedOnQueue);
        assert_eq!(
            take_entry(
                &task,
                || unreachable!("never offered"),
                counting(&departures)
            ),
            Taken::Spent
        );
        assert_eq!(task.load_state(), TaskState::Stopped);
        assert_eq!(
            departures.get(),
            1,
            "the completed stop left the competition"
        );

        assert_eq!(
            take_entry(
                &Model::new(TaskState::Exited),
                || false,
                counting(&departures)
            ),
            Taken::Exited
        );
        for stale in [
            TaskState::Running,
            TaskState::Parked,
            TaskState::Stopped,
            TaskState::StoppedOnCpu,
        ] {
            let task = Model::new(stale);
            assert_eq!(
                take_entry(&task, || false, counting(&departures)),
                Taken::Spent,
                "{stale:?}"
            );
            assert_eq!(task.load_state(), stale, "{stale:?}: left untouched");
        }
        assert_eq!(departures.get(), 1);
    }

    /// A completion that loses to a resume finds the task ready, and the
    /// entry the caller holds is the only one it has: dropping it would lose
    /// the task.
    #[test]
    fn a_completion_that_loses_to_a_resume_takes_the_task() {
        let task = Raced::new(TaskState::StoppedOnQueue, TaskState::Ready);
        let departures = Cell::new(0);
        let offers = Cell::new(0);
        let taken = take_entry(
            &task,
            || {
                offers.set(offers.get() + 1);
                true
            },
            counting(&departures),
        );
        assert_eq!(taken, Taken::Ready);
        assert_eq!((offers.get(), departures.get()), (1, 0));
    }

    /// A take that loses to a stop completes the stop it lost to.
    #[test]
    fn a_take_that_loses_to_a_stop_completes_it() {
        let task = Model::new(TaskState::Ready);
        let departures = Cell::new(0);
        let taken = take_entry(
            &task,
            || {
                task.state.set(TaskState::StoppedOnQueue);
                false
            },
            counting(&departures),
        );
        assert_eq!(taken, Taken::Spent);
        assert_eq!(task.load_state(), TaskState::Stopped);
        assert_eq!(departures.get(), 1);
    }

    /// Settle `task` after a body returned `action`, counting the departures.
    fn settle_counting(task: &Model, action: TaskAction, doomed: bool) -> (Settled, usize) {
        let departures = Cell::new(0);
        let settled = settle(task, action, doomed, counting(&departures));
        (settled, departures.get())
    }

    #[test]
    fn a_body_that_owned_its_run_settles_as_it_asked() {
        let task = Model::new(TaskState::Running);
        assert_eq!(
            settle_counting(&task, TaskAction::Yield, false),
            (Settled::Requeue, 0)
        );
        assert_eq!(task.load_state(), TaskState::Ready);

        let task = Model::new(TaskState::Running);
        assert_eq!(
            settle_counting(&task, TaskAction::Park, false),
            (Settled::Park, 1)
        );
        assert_eq!(task.load_state(), TaskState::Parked);

        let task = Model::new(TaskState::Running);
        assert_eq!(
            settle_counting(&task, TaskAction::Exit, false),
            (Settled::Retire, 1)
        );
        assert_eq!(task.load_state(), TaskState::Exited);
    }

    /// A stop that landed while the body ran holds the task, so neither a
    /// wake nor its own queue entry can run it: its yield ends stopped, and
    /// its park ends parked beneath the stop.
    #[test]
    fn a_stop_that_landed_while_the_body_ran_wins_over_its_request() {
        let task = Model::new(TaskState::StoppedOnCpu);
        assert_eq!(
            settle_counting(&task, TaskAction::Yield, false),
            (Settled::Released, 1)
        );
        assert_eq!(task.load_state(), TaskState::Stopped);

        let task = Model::new(TaskState::StoppedOnCpu);
        assert_eq!(
            settle_counting(&task, TaskAction::Park, false),
            (Settled::Park, 1)
        );
        assert_eq!(task.load_state(), TaskState::StoppedParked);
        commit_park(&task, Model::admit);
        assert_eq!(
            task.load_state(),
            TaskState::StoppedParked,
            "nothing woke it"
        );
    }

    /// A wake that reached a body parking as a stop landed is kept by the park
    /// commit for the continue, not slept through.
    #[test]
    fn a_park_a_stop_overtook_keeps_the_wake_that_raced_it() {
        let task = Model::new(TaskState::StoppedOnCpu);
        assert_eq!(unpark_task(&task, Model::admit), Ok(()), "a token");
        assert_eq!(
            settle_counting(&task, TaskAction::Park, false),
            (Settled::Park, 1)
        );
        commit_park(&task, Model::admit);
        assert_eq!(task.load_state(), TaskState::Stopped, "owed its run");
        assert_eq!(task.admits.get(), 0, "and still held");
        assert_eq!(resume_task(&task, Model::admit), Ok(()));
        assert_eq!(task.load_state(), TaskState::Ready);
        assert_eq!(task.admits.get(), 1);
    }

    #[test]
    fn a_kill_wins_over_everything_but_the_tasks_own_park() {
        let task = Model::new(TaskState::Running);
        assert_eq!(
            settle_counting(&task, TaskAction::Yield, true),
            (Settled::Retire, 1)
        );
        let task = Model::new(TaskState::Running);
        assert_eq!(
            settle_counting(&task, TaskAction::Park, true),
            (Settled::Park, 1),
            "a task parked in a syscall must unwind before it can die"
        );
        let task = Model::new(TaskState::StoppedOnCpu);
        assert_eq!(
            settle_counting(&task, TaskAction::Yield, true),
            (Settled::Retire, 1),
            "a kill wins over a stop"
        );
        assert_eq!(task.load_state(), TaskState::Exited);
        let task = Model::new(TaskState::StoppedOnCpu);
        assert_eq!(
            settle_counting(&task, TaskAction::Park, true),
            (Settled::Park, 1),
            "parked beneath the stop, it too must unwind before it can die"
        );
        assert_eq!(task.load_state(), TaskState::StoppedParked);
        assert_eq!(resume_task(&task, Model::admit), Ok(()), "the killer");
        assert_eq!(
            unpark_task(&task, Model::admit),
            Ok(()),
            "resumes and wakes it"
        );
        assert_eq!(task.load_state(), TaskState::Ready);
        assert_eq!(task.admits.get(), 1);
    }

    /// A compare-exchange that loses to a stop landing between the read and
    /// the exchange is decided again against the new state.
    #[test]
    fn a_transition_that_loses_a_race_is_decided_again() {
        let task = Raced::new(TaskState::Running, TaskState::StoppedOnCpu);
        let settled = settle(
            &task,
            TaskAction::Yield,
            false,
            |transition: &dyn Fn() -> bool| transition(),
        );
        assert_eq!(settled, Settled::Released, "the yield lost to a stop");
        assert_eq!(task.load_state(), TaskState::Stopped);
    }

    /// States no transition reaches from a task on a CPU are left to whoever
    /// put the task there, never overwritten.
    #[test]
    fn a_state_no_running_task_can_reach_is_left_to_its_owner() {
        for state in [
            TaskState::Ready,
            TaskState::Stopped,
            TaskState::StoppedOnQueue,
        ] {
            let task = Model::new(state);
            assert_eq!(
                settle_counting(&task, TaskAction::Yield, false),
                (Settled::Released, 0),
                "{state:?}"
            );
            assert_eq!(task.load_state(), state, "{state:?}");
        }
        for state in [TaskState::Parked, TaskState::StoppedParked] {
            let task = Model::new(state);
            assert_eq!(
                settle_counting(&task, TaskAction::Yield, false),
                (Settled::Park, 0),
                "{state:?}"
            );
            assert_eq!(task.load_state(), state, "{state:?}");
        }
    }

    #[test]
    fn a_doomed_task_no_cpu_names_is_nudged_everywhere() {
        let arch = crate::TestArch::new(3).expect("three CPUs");
        nudge_running(&arch, Some(1), 3);
        assert_eq!(
            (arch.ipi_count(0), arch.ipi_count(1), arch.ipi_count(2)),
            (0, 1, 0)
        );
        nudge_running(&arch, None, 3);
        assert_eq!(
            (arch.ipi_count(0), arch.ipi_count(1), arch.ipi_count(2)),
            (1, 2, 1),
            "an owned body no slot names could be running on any CPU"
        );
        assert_eq!(arch.stray_ipi_count(), 0);
    }

    #[test]
    fn only_the_first_doom_is_reported_as_first() {
        let mark = AtomicBool::new(false);
        assert!(!observe_doom(&mark));
        assert!(doom(&mark));
        assert!(!doom(&mark), "a repeat owes no teardown");
        assert!(observe_doom(&mark));
    }

    /// A store-buffering litmus run of the kill against a returning body, on
    /// real threads and the real body lock. Each round a dispatch releases the
    /// body and reads the mark while a killer dooms and probes the body; the
    /// killer finding the body held while the dispatch reads the mark clear is
    /// the outcome that let a deferred kill be requeued instead of retired.
    #[test]
    fn a_kill_and_a_returning_body_never_both_miss_each_other() {
        extern crate std;

        use core::sync::atomic::AtomicUsize;
        use std::sync::Arc;
        use tairix_sync::SpinLock;

        const ROUNDS: usize = 400_000;

        struct Round {
            body: SpinLock<()>,
            mark: AtomicBool,
            armed: AtomicUsize,
            fired: AtomicUsize,
            probed: AtomicUsize,
        }

        let round = Arc::new(Round {
            body: SpinLock::new(()),
            mark: AtomicBool::new(false),
            armed: AtomicUsize::new(0),
            fired: AtomicUsize::new(0),
            probed: AtomicUsize::new(0),
        });

        let killer = {
            let round = Arc::clone(&round);
            std::thread::spawn(move || {
                let mut held = std::vec::Vec::with_capacity(ROUNDS);
                for r in 1..=ROUNDS {
                    while round.armed.load(Ordering::Acquire) != r {
                        core::hint::spin_loop();
                    }
                    round.fired.store(r, Ordering::Release);
                    let _ = doom(&round.mark);
                    held.push(round.body.try_lock().is_none());
                    round.probed.store(r, Ordering::Release);
                }
                held
            })
        };

        let mut saw = std::vec::Vec::with_capacity(ROUNDS);
        for r in 1..=ROUNDS {
            while round.probed.load(Ordering::Acquire) != r - 1 {
                core::hint::spin_loop();
            }
            round.mark.store(false, Ordering::Relaxed);
            let body = round.body.lock();
            round.armed.store(r, Ordering::Release);
            while round.fired.load(Ordering::Acquire) != r {
                core::hint::spin_loop();
            }
            drop(body);
            saw.push(observe_doom(&round.mark));
        }
        let held = killer.join().expect("the killer thread completes");

        let missed = held.iter().zip(&saw).filter(|(held, saw)| **held && !**saw);
        assert_eq!(
            missed.count(),
            0,
            "a killer saw the body held while the run read the mark clear"
        );
    }
}
