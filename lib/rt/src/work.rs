//! Handing slow work off an interactive loop, onto a worker thread.
//!
//! An interactive surface owes the user a frame, so it must not carry out a
//! store write, an IPC round trip, or any other wait itself. [`Worker`] is the
//! one arrangement that takes such work: the loop *submits* and carries on
//! drawing, the worker parks until there is something to do, and the answer
//! arrives as a wake on the loop's own wait-set.
//!
//! It is one desk, one worker, latest-wins ([`tairix_util::defer::JobDesk`]),
//! so an interaction that settles repeatedly costs one further job rather than
//! one each, and two answers can never race for what a store ends up saying.
//!
//! # The work keeps its own state
//!
//! A job is not always a self-contained round trip. A viewer's sandboxed
//! decode is a *session* — open the document once, then draw from the page it
//! holds — so the thing carrying the work has to remember what it did last
//! time, and the loop must not be able to reach it. The state therefore
//! belongs to the worker: `run` receives it by exclusive reference, and it is
//! the same state on the fall-back path, so there is one session however the
//! job ran.
//!
//! The job arrives by **exclusive reference**, so the work may take a lent
//! buffer out of it and hand it back in the answer. A window's worth of
//! pixels is megabytes; allocating and freeing that per pointer sample is the
//! cost that removes — and a job the work only reads is still not copied to
//! reach it.
//!
//! # A machine that grants no worker is slower, never wrong
//!
//! The kernel may refuse the wake pipe or the thread. [`Worker::start`] then
//! leaves the desk stopped, which makes every later [`submit`](Worker::submit)
//! carry the job out on the caller's own thread and leave the answer where
//! [`collect`](Worker::collect) finds it. There is one adopt path either way,
//! so no caller needs a second one — and the loop is exactly as responsive as
//! it was before there was a worker at all.

use alloc::collections::TryReserveError;
use alloc::sync::Arc;

use tairix_abi::Errno;
use tairix_util::defer::{JobDesk, JobQueue};

use crate::sync::{Condvar, Mutex, WorkerWake};
use crate::thread::{JoinHandle, Thread};

/// Why a program has no worker thread, so it can say so in its own words.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NoWorker {
    /// The kernel refused the pipe an answer would have woken the loop over.
    /// Without it a submitted job's answer would never be collected.
    Wake,
    /// The kernel refused the thread.
    Thread(Errno),
}

/// What a [`Worker`] takes its jobs from: one latest-wins [`JobDesk`], or a
/// [`JobQueue`] each of whose jobs is answered in turn.
///
/// Sealed: these two are the whole set, so the worker's teardown and
/// fall-back rules are stated once for every desk.
pub trait Desk<Req, Ans>: sealed::Sealed {
    /// The next job for the worker, if one is waiting.
    fn take(&mut self) -> Option<Req>;
    /// Record the worker's answer, answering whether the loop is owed a
    /// wake.
    fn answer(&mut self, answer: Ans) -> bool;
    /// Whether the worker has been sent away — and so whether a submission
    /// is carried out by its asker.
    fn leaving(&self) -> bool;
    /// Send the worker away.
    fn leave(&mut self);
}

mod sealed {
    pub trait Sealed {}
    impl<Req, Ans> Sealed for tairix_util::defer::JobDesk<Req, Ans> {}
    impl<Req, Ans> Sealed for tairix_util::defer::JobQueue<Req, Ans> {}
}

impl<Req, Ans> Desk<Req, Ans> for JobDesk<Req, Ans> {
    fn take(&mut self) -> Option<Req> {
        self.next_job()
    }

    fn answer(&mut self, answer: Ans) -> bool {
        self.deliver(answer)
    }

    fn leaving(&self) -> bool {
        self.stopping()
    }

    fn leave(&mut self) {
        self.stop();
    }
}

impl<Req, Ans> Desk<Req, Ans> for JobQueue<Req, Ans> {
    fn take(&mut self) -> Option<Req> {
        self.next_job()
    }

    fn answer(&mut self, answer: Ans) -> bool {
        self.deliver(answer)
    }

    fn leaving(&self) -> bool {
        self.stopping()
    }

    /// What is still waiting is dropped: an asker that must see a job
    /// answered waits it out with [`Worker::wait`] before sending the worker
    /// away.
    fn leave(&mut self) {
        let _ = self.stop();
    }
}

/// The desk, and whether an asker is blocked waiting for an answer to land.
struct Held<D> {
    desk: D,
    awaited: bool,
}

/// A worker thread, the desk it takes work from, and the state its work
/// keeps between jobs.
///
/// `run` is the work itself. It is a plain function pointer rather than a
/// closure or a trait so the same one serves the worker thread and the
/// fall-back path, and no caller can supply two that disagree — and its
/// state travels the same way, so neither path can be given a session the
/// other does not have.
///
/// `S` is `()` for work that carries nothing over from one job to the next.
/// `D` is the desk: latest-wins by default, or a [`JobQueue`] for work where
/// every job is its own and each must be answered.
pub struct Worker<S, Req, Ans, D = JobDesk<Req, Ans>> {
    held: Mutex<Held<D>>,
    /// Signalled when a job is submitted, and on teardown.
    work: Condvar,
    /// Signalled when an answer lands while an asker waits for one.
    answered: Condvar,
    wake: WorkerWake,
    run: fn(&mut S, &mut Req) -> Ans,
    /// Held apart from the desk so a submission never waits on a job in
    /// flight: only the thread carrying work out ever locks this.
    state: Mutex<S>,
}

impl<S, Req, Ans, D: Desk<Req, Ans>> Worker<S, Req, Ans, D> {
    fn over(run: fn(&mut S, &mut Req) -> Ans, state: S, wake: WorkerWake, desk: D) -> Self {
        Self {
            held: Mutex::new(Held {
                desk,
                awaited: false,
            }),
            work: Condvar::new(),
            answered: Condvar::new(),
            wake,
            run,
            state: Mutex::new(state),
        }
    }

    /// The wake whose read end the loop adds to its wait-set, and drains when
    /// that token fires.
    #[must_use]
    pub const fn wake(&self) -> &WorkerWake {
        &self.wake
    }

    /// Ask the worker to leave, and wake it so it does.
    ///
    /// Also what puts the desk into the state that runs later submissions on
    /// the caller's own thread.
    pub fn stop(&self) {
        self.held.lock().desk.leave();
        self.work.notify_all();
        self.answered.notify_all();
    }

    /// Carry `job` out on this thread, for a desk no worker will take it
    /// from.
    fn run_here(&self, job: Req) -> Ans {
        let mut job = job;
        (self.run)(&mut self.state.lock(), &mut job)
    }

    /// One worker's whole life: park until there is a job, carry it out, leave
    /// the answer, nudge the loop.
    fn serve(&self) {
        loop {
            let mut job = {
                let mut held = self.held.lock();
                loop {
                    if held.desk.leaving() {
                        return;
                    }
                    if let Some(job) = held.desk.take() {
                        break job;
                    }
                    held = self.work.wait(held);
                }
            };
            // The wait itself, with the desk unlocked: this is the call that
            // would otherwise have frozen the window.
            let answer = (self.run)(&mut self.state.lock(), &mut job);
            let mut held = self.held.lock();
            if held.desk.answer(answer) {
                self.wake.nudge();
            }
            if held.awaited {
                self.answered.notify_all();
            }
        }
    }
}

impl<S, Req, Ans> Worker<S, Req, Ans> {
    /// A worker that carries out `run` over `state`, waking its loop over
    /// `wake`.
    ///
    /// Nothing is started until [`start`](Self::start).
    #[must_use]
    pub fn new(run: fn(&mut S, &mut Req) -> Ans, state: S, wake: WorkerWake) -> Self {
        Self::over(run, state, wake, JobDesk::new())
    }

    /// Ask for `job` to be carried out.
    ///
    /// Answers whether one is already waiting to be collected — `true` only
    /// where there is no worker, in which case the job ran on this thread and
    /// its answer is where [`collect`](Self::collect) will find it.
    pub fn submit(&self, job: Req) -> bool {
        let submitted = {
            let mut held = self.held.lock();
            if held.desk.stopping() {
                // No worker will ever take it, so it is carried out here and
                // left on the desk: one adopt path however it was run.
                drop(held);
                let answer = self.run_here(job);
                let _ = self.held.lock().desk.deliver(answer);
                return true;
            }
            held.desk.submit(job)
        };
        if submitted.wake {
            self.work.notify_one();
        }
        false
    }

    /// Take a landed answer, if one has.
    pub fn collect(&self) -> Option<Ans> {
        self.held.lock().desk.collect()
    }
}

impl<S, Req, Ans> Worker<S, Req, Ans, JobQueue<Req, Ans>> {
    /// A worker that carries out `run` over `state` for each job it is
    /// asked, in turn, waking its loop over `wake` — holding at most
    /// `capacity` jobs not yet collected.
    ///
    /// # Errors
    ///
    /// The refusal of the memory for `capacity` jobs.
    pub fn queued(
        run: fn(&mut S, &mut Req) -> Ans,
        state: S,
        wake: WorkerWake,
        capacity: usize,
    ) -> Result<Self, TryReserveError> {
        Ok(Self::over(
            run,
            state,
            wake,
            JobQueue::with_capacity(capacity)?,
        ))
    }

    /// Queue `job`, answering whether its answer is already waiting — `true`
    /// only where there is no worker and it ran on this thread.
    ///
    /// # Errors
    ///
    /// `job`, back, when the queue holds its capacity: nothing ran.
    pub fn submit(&self, job: Req) -> Result<bool, Req> {
        let mut held = self.held.lock();
        if held.desk.stopping() {
            // No worker will take it, so it runs here under the desk's own lock:
            // the room it was admitted to cannot be taken before it lands.
            return held
                .desk
                .carry_out(job, |job| self.run_here(job))
                .map(|()| true);
        }
        held.desk.submit(job)?;
        drop(held);
        self.work.notify_one();
        Ok(false)
    }

    /// Take the oldest landed answer, if one has.
    pub fn collect(&self) -> Option<Ans> {
        self.held.lock().desk.collect()
    }

    /// Withdraw every waiting job `keep` turns down; nobody answers them.
    pub fn retain_waiting(&self, keep: impl FnMut(&Req) -> bool) {
        self.held.lock().desk.retain_waiting(keep);
    }

    /// Hold room for `more` further jobs.
    ///
    /// # Errors
    ///
    /// The refusal of the memory; the bound is as it was.
    pub fn grow(&self, more: usize) -> Result<(), TryReserveError> {
        self.held.lock().desk.grow(more)
    }

    /// Give up room for `fewer` jobs; what is already held is still answered.
    pub fn shrink(&self, fewer: usize) {
        self.held.lock().desk.shrink(fewer);
    }

    /// Block until an answer lands and take it, or answer `None` once
    /// nothing is left outstanding — for an asker with nothing else to do,
    /// such as a program seeing its saves out before it ends.
    pub fn wait(&self) -> Option<Ans> {
        let mut held = self.held.lock();
        let answer = loop {
            if let Some(answer) = held.desk.collect() {
                break Some(answer);
            }
            if !held.desk.outstanding() {
                break None;
            }
            held.awaited = true;
            held = self.answered.wait(held);
        };
        held.awaited = false;
        answer
    }
}

impl<S, Req, Ans, D> Worker<S, Req, Ans, D>
where
    S: Send + 'static,
    Req: Send + 'static,
    Ans: Send + 'static,
    D: Desk<Req, Ans> + Send + 'static,
{
    /// Start `worker` on its own thread.
    ///
    /// On failure the desk is left stopped, so the program is still correct
    /// without a thread and the caller only has to state why it has none.
    ///
    /// # Errors
    ///
    /// [`NoWorker`] when the kernel refused the wake pipe or the thread.
    pub fn start(worker: &Arc<Self>) -> Result<JoinHandle<()>, NoWorker> {
        if !worker.wake.is_armed() {
            worker.stop();
            return Err(NoWorker::Wake);
        }
        let served = Arc::clone(worker);
        Thread::spawn(move || served.serve()).map_err(|err| {
            worker.stop();
            NoWorker::Thread(err)
        })
    }
}

/// Stops its worker on every way out of the scope holding it, so one is never
/// left working for a program that has ended.
///
/// The thread is *detached* rather than joined: a worker mid-write of a slow
/// store would otherwise hold the teardown for as long as that store takes, and
/// it leaves at its next turn round its loop anyway. Its own handle on the desk
/// keeps it alive until then.
pub struct WorkerGuard<S, Req, Ans, D: Desk<Req, Ans> = JobDesk<Req, Ans>>(
    Arc<Worker<S, Req, Ans, D>>,
);

impl<S, Req, Ans, D: Desk<Req, Ans>> WorkerGuard<S, Req, Ans, D> {
    /// Guard `worker`.
    #[must_use]
    pub fn new(worker: &Arc<Worker<S, Req, Ans, D>>) -> Self {
        Self(Arc::clone(worker))
    }
}

impl<S, Req, Ans, D: Desk<Req, Ans>> Drop for WorkerGuard<S, Req, Ans, D> {
    fn drop(&mut self) {
        self.0.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A job carrying more than a scalar, as the real ones do (a profile, an
    /// encoded document), so a reference is the natural way to hand it over.
    #[derive(Clone, Copy)]
    struct Job {
        value: u32,
        what: &'static str,
    }

    /// And an answer that says what it was about, as the real ones do, plus
    /// which job of the session it was.
    #[derive(Debug, Eq, PartialEq)]
    struct Done {
        doubled: u32,
        of: &'static str,
        run_number: u32,
    }

    /// Doubling stands in for a store round trip, and the running count for
    /// the session a real one keeps: the point is *where* it ran and *what it
    /// remembered*, not what it computed.
    fn double(runs: &mut u32, job: &mut Job) -> Done {
        *runs += 1;
        Done {
            doubled: job.value * 2,
            of: job.what,
            run_number: *runs,
        }
    }

    /// A job, and the answer carrying out the *first* one of a session gives.
    fn job() -> (Job, Done) {
        let job = Job {
            value: 21,
            what: "a settled edit",
        };
        (job, double(&mut 0, &mut { job }))
    }

    /// The property the whole fall-back exists for: with no worker the job is
    /// carried out on the caller's own thread and its answer is left exactly
    /// where the loop's collect looks, so there is one adopt path either way.
    #[test]
    fn a_stopped_worker_carries_the_job_out_on_the_caller() {
        let (job, done) = job();
        let worker: Worker<u32, Job, Done> = Worker::new(double, 0, WorkerWake::create());
        worker.stop();
        assert!(
            worker.submit(job),
            "a job nobody will take is run here, and says so"
        );
        assert_eq!(worker.collect(), Some(done));
        assert_eq!(worker.collect(), None, "an answer is collected once");
    }

    /// A running desk defers instead: the loop is told nothing is waiting, and
    /// nothing has been carried out on its thread.
    #[test]
    fn a_running_worker_defers_the_job() {
        let (job, _) = job();
        let worker: Worker<u32, Job, Done> = Worker::new(double, 0, WorkerWake::create());
        assert!(!worker.submit(job), "the job went to the desk");
        assert_eq!(
            worker.collect(),
            None,
            "and no answer was produced on this thread"
        );
    }

    /// The guard stops its worker, so work submitted after the scope that owns
    /// it has gone is still carried out rather than silently dropped.
    #[test]
    fn the_guard_stops_the_worker_it_holds() {
        let (job, done) = job();
        let worker: Arc<Worker<u32, Job, Done>> =
            Arc::new(Worker::new(double, 0, WorkerWake::create()));
        {
            let _guard = WorkerGuard::new(&worker);
            assert!(!worker.submit(job), "still running inside the scope");
        }
        assert!(worker.submit(job), "stopped, so this one runs here");
        assert_eq!(worker.collect(), Some(done));
    }

    /// The property the state exists for: a session is carried from one job to
    /// the next, and the loop that submitted them never held it.
    #[test]
    fn the_work_keeps_its_state_across_jobs() {
        let (job, _) = job();
        let worker: Worker<u32, Job, Done> = Worker::new(double, 0, WorkerWake::create());
        // Stopped, so each job runs on this thread — which is also the path
        // that must see the same session as a worker thread would.
        worker.stop();
        for expected in 1..=3 {
            assert!(worker.submit(job));
            let answer = worker.collect().expect("an answer per job");
            assert_eq!(answer.run_number, expected);
        }
    }

    /// A machine that grants no wake pipe grants no worker either: starting
    /// leaves the desk in the state that runs later submissions inline, so the
    /// caller only has to state why.
    #[test]
    fn a_start_without_a_wake_leaves_the_work_on_the_caller() {
        let (job, done) = job();
        let worker: Arc<Worker<u32, Job, Done>> =
            Arc::new(Worker::new(double, 0, WorkerWake::create()));
        // The host grants no pipe, which is exactly the refusal being modelled.
        assert!(!worker.wake().is_armed());
        assert!(matches!(Worker::start(&worker), Err(NoWorker::Wake)));
        assert!(worker.submit(job));
        assert_eq!(worker.collect(), Some(done));
    }

    type Queued = Worker<u32, Job, Done, JobQueue<Job, Done>>;

    fn queued(capacity: usize) -> Queued {
        Worker::queued(double, 0, WorkerWake::create(), capacity).expect("room")
    }

    /// With no worker a queue's jobs run on the asker in turn, each answered
    /// once, and one past the room is refused without running.
    #[test]
    fn a_stopped_queue_runs_every_job_here_in_turn_within_its_room() {
        let (job, _) = job();
        let worker = queued(2);
        worker.stop();
        assert_eq!(worker.submit(job).map_err(|_| ()), Ok(true));
        assert_eq!(worker.submit(job).map_err(|_| ()), Ok(true));
        assert!(worker.submit(job).is_err(), "past the room, and not run");
        let runs: alloc::vec::Vec<u32> =
            core::iter::from_fn(|| worker.collect().map(|done| done.run_number)).collect();
        assert_eq!(runs, [1, 2], "two jobs ran, in turn, the third never");
        assert_eq!(worker.wait(), None, "nothing is outstanding");
    }

    /// A running queue defers; what it is told to withdraw is never run, and
    /// its bound follows what it is told to hold.
    #[test]
    fn a_running_queue_defers_withdraws_and_grows() {
        let (job, _) = job();
        let worker = queued(1);
        assert_eq!(worker.submit(job).map_err(|_| ()), Ok(false));
        assert!(worker.submit(job).is_err(), "one job fills a room of one");
        worker.retain_waiting(|_| false);
        assert_eq!(worker.wait(), None, "withdrawn, so nothing is outstanding");
        worker.grow(1).expect("room");
        assert_eq!(worker.submit(job).map_err(|_| ()), Ok(false));
        assert_eq!(worker.submit(job).map_err(|_| ()), Ok(false));
        worker.shrink(2);
        worker.retain_waiting(|_| false);
        assert!(worker.submit(job).is_err(), "the room given up is gone");
        assert_eq!(worker.collect(), None, "and nothing ran on this thread");
    }

    /// Waiting takes what has already landed, oldest first, without blocking.
    #[test]
    fn waiting_takes_landed_answers_in_turn() {
        let (job, _) = job();
        let worker = queued(2);
        worker.stop();
        let _ = worker.submit(job);
        let _ = worker.submit(Job { value: 1, ..job });
        assert_eq!(worker.wait().map(|done| done.doubled), Some(42));
        assert_eq!(worker.wait().map(|done| done.doubled), Some(2));
        assert_eq!(worker.wait(), None);
    }
}
