//! Bounded-concurrency runner for independent jobs.
//!
//! A sequential step ([`Context::run_with_timeout`]) is a run of one job, so a
//! step run alone and one run beside others are bounded, killed and reported
//! alike; and a stage made of independent, isolated jobs — the fuzz,
//! proptest, miri and loom harnesses, the static gates, the QEMU matrix,
//! `ci-long`'s flake hunt — overlaps them instead of paying the *sum* of their
//! wall-clock costs. "Run more than one at a time" lives in exactly one place.
//!
//! A job is one of two kinds. A [`Work::Command`] is an external command the
//! runner budgets and, past its budget, kills with everything it forked. A
//! [`Work::Closure`] is work done in-process — a static gate, a QEMU guest
//! driven through [`tairix_qemu::Runner::run`], a failure known before
//! anything could run — which nothing can cancel from outside, so it bounds
//! its own work.
//!
//! [`Context::run_with_timeout`]: crate::Context::run_with_timeout
//!
//! # Weighted admission
//!
//! A job carries a **weight** and the runner admits jobs while the sum of the
//! in-flight weights stays within a **budget**. For the fuzz/proptest
//! harnesses every job weighs `1` and the budget is a job *count*, so the
//! model collapses to "run at most N at a time" — its historical behaviour.
//!
//! The QEMU matrix uses weight as **host-capacity demand**. A uniprocessor
//! guest charges two units (its vCPU plus emulator/I/O work); an SMP TCG guest
//! charges the complete budget and therefore runs alone, because a co-scheduled
//! SMP guest starves so badly it trips its own in-guest lockup watchdog. The
//! budget is one third of the host's logical CPUs — deliberate headroom, since
//! a QEMU guest is far heavier than its lone vCPU thread and each guest runs
//! its own real-time watchdogs, so admitting one guest per logical CPU
//! oversubscribes the host into missing those internal deadlines. Within that
//! headroom a few uniprocessor guests overlap; the guest's *inactivity*
//! deadline ([`tairix_qemu::Runner::run`]) is reset by every line it prints,
//! so a guest that merely runs a little slower is never falsely timed out —
//! the no-flaky-tests / no-retry rules hold — while its absolute runtime
//! ceiling still bounds a guest that keeps talking without finishing. (A
//! single job heavier than the whole budget — a guest with more vCPUs than the
//! budget — still runs, alone, when nothing else is in flight, rather than
//! deadlocking.)
//!
//! # Output handling
//!
//! When more than one job can run at once, each job's lines reach the log in
//! whole blocks, so concurrent jobs never interleave: a command job's stdio is
//! captured and logged as it ends, a job killed at its budget included, and
//! each job is announced as it is *admitted*, with the in-flight weight that
//! leaves, so the log shows real concurrency rather than a wall of start
//! lines. When at most one job can run at a time (budget `1`), a command job's
//! stdio is live instead, as a plain sequential run's is; an accounted one's
//! standard output is relayed a line at a time, so its account is kept from
//! the lines that frame each libtest run without holding the rest.
//!
//! On either path a job's check is a step of its own ahead of its work, and
//! every step ends with its closing line, so one still outstanding is never
//! mistaken for one that has ended. A failed step's closing line comes after
//! why, save a closure's: its detail, a guest's whole serial log say, goes back
//! in the run's error, for its caller to show once, when the run is over. The
//! runner fails closed: every job runs to completion even if an earlier one
//! failed, and the run's error names every failure.

use std::process::Command;
use std::sync::{Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use super::libtest::{Account, Tally};
use crate::children::Children;
use crate::{elapsed_line, output_within, run_within, WaitFailure, DEFAULT_COMMAND_TIMEOUT};

/// A unit of work done in-process that reports its own pass or failure: a
/// closure job's work, or a job's check.
type Task = Box<dyn FnOnce() -> Result<(), String> + Send>;

/// What a [`Job`] actually does when run.
enum Work {
    /// An external command whose stdio the runner owns (captures or streams).
    Command(Command),
    /// An arbitrary unit of work that reports its own pass/fail and prints
    /// (or buffers) its own output. Used by the QEMU matrix, where the work
    /// is "plant images, then drive a guest to completion".
    Closure(Task),
}

/// One labelled, weighted unit of work to run to completion.
pub struct Job {
    label: String,
    /// In-flight cost charged against the runner's budget while this job runs
    /// (see the module-level "Weighted admission"). Always at least one.
    weight: usize,
    /// Wall-clock budget for a [`Work::Command`] job, after which its process
    /// group is killed and the job fails.
    ///
    /// Unused by a [`Work::Closure`] job: a running thread cannot be
    /// cancelled from outside, so a closure **must** bound its own work (the
    /// QEMU closure does so through its guest runtime ceiling). A closure that
    /// can run forever would hang the whole runner.
    budget: Duration,
    /// Made as the job starts, before its work: a refusal is the job's failure.
    check: Option<Task>,
    /// What a command job's tests must have run, settled against the libtest
    /// summaries in its output once it exits.
    account: Option<Account>,
    work: Work,
}

impl Job {
    /// Pair a human-readable label with the command that performs the job.
    /// The job weighs one unit, so a count-based budget runs at most `budget`
    /// such jobs at a time.
    pub fn new(label: impl Into<String>, command: Command) -> Self {
        Self {
            label: label.into(),
            weight: 1,
            budget: DEFAULT_COMMAND_TIMEOUT,
            check: None,
            account: None,
            work: Work::Command(command),
        }
    }

    /// Give this job a wall-clock budget other than the default.
    ///
    /// For a job that is legitimately slower than an ordinary step — a
    /// cross-compile that rebuilds the standard library from source, say — so
    /// that a correct-but-slow job is not mistaken for a hung one. The
    /// operator's override can only raise it.
    #[must_use]
    pub fn with_budget(mut self, budget: Duration) -> Self {
        self.budget = budget;
        self
    }

    /// This job's work done only once `check` passes, the check made as the
    /// job starts, so it overlaps every other job rather than delaying them
    /// all; its refusal is the job's failure.
    ///
    /// The check is reported as a step of its own, its refusal's reason with
    /// it, and its time comes out of a command job's budget; but like a
    /// closure it is never cancelled from outside, so it must bound its own
    /// work.
    #[must_use]
    pub fn checked_by(
        mut self,
        check: impl FnOnce() -> Result<(), String> + Send + 'static,
    ) -> Self {
        self.check = Some(match self.check.take() {
            None => Box::new(check),
            Some(first) => Box::new(move || {
                first()?;
                check()
            }),
        });
        self
    }

    /// Hold this job's tests to `account`, settled against the libtest
    /// summaries in its output once it exits, so a run that skipped a test it
    /// was given fails however it exited. A closure reports its own outcome,
    /// so an account binds only a command job, and a closure given one fails.
    #[must_use]
    pub fn accounted(mut self, account: Account) -> Self {
        self.account = Some(account);
        self
    }

    /// The account this job is held to, for the tests that pin each stage's.
    #[cfg(test)]
    pub(crate) fn account(&self) -> Option<Account> {
        self.account
    }

    /// Pair a label with a closure that performs the job and reports its own
    /// pass/fail, charging `weight` against the runner's budget while it runs
    /// (clamped to at least one).
    pub fn closure(
        label: impl Into<String>,
        weight: usize,
        work: impl FnOnce() -> Result<(), String> + Send + 'static,
    ) -> Self {
        Self {
            label: label.into(),
            weight: weight.max(1),
            budget: DEFAULT_COMMAND_TIMEOUT,
            check: None,
            account: None,
            work: Work::Closure(Box::new(work)),
        }
    }
}

/// The host's available parallelism (logical CPU count), or one when it cannot
/// be determined so callers degrade to a sequential run rather than failing.
#[must_use]
pub fn host_parallelism() -> usize {
    thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get)
}

/// Concurrency to use for `count` weight-one jobs: the host's available
/// parallelism, clamped to the job count and to at least one.
#[must_use]
pub fn default_concurrency(count: usize) -> usize {
    count.min(host_parallelism()).max(1)
}

/// Order jobs heaviest-weight first, keeping equal weights in the order given.
///
/// Admission is head-of-line: a job waits until its weight fits the remaining
/// budget. A whole-budget job therefore starts only once nothing else is
/// running, so meeting one part-way through drains the runner to empty and
/// leaves the host idle while it finishes — one bubble per heavy job. Taking
/// the heavy jobs while the runner is *already* empty pays that drain once, at
/// the start, and the lighter jobs then pack in behind them. Simulated over the
/// measured QEMU matrix (11 whole-budget guests among 151) this lands within
/// three seconds of the theoretical floor, where the given order costs fifty
/// more.
///
/// A stable sort, so a set of equal-weight jobs keeps its caller's order and
/// the uniform weight-one gate groups are unaffected.
fn heaviest_first(jobs: &mut [Job]) {
    jobs.sort_by_key(|job| core::cmp::Reverse(job.weight));
}

/// Where a run's log goes and the children its commands are: stderr and the
/// run's own in a real run, while a test hands its own, so what the runner
/// reports can be read back and no environment reaches it.
struct Runtime<'a> {
    log: &'a (dyn Fn(&str) + Sync),
    children: &'a Children,
    /// Holds each block of the log whole where jobs run side by side.
    lock: Mutex<()>,
    /// Where an accounted job running alone relays its output.
    relay: Relay,
}

/// Where an accounted job running alone relays its output.
#[derive(Clone, Copy)]
enum Relay {
    /// To this process's own standard streams, each line whole as it arrives,
    /// so a line from one stream never splits one from the other.
    Live,
    /// To the log, once the job ends: a recorded run keeps every line of its
    /// jobs out of the recorder's own output.
    #[cfg(test)]
    Log,
}

/// Which of a relayed job's streams a relay carries.
#[derive(Clone, Copy)]
enum Stream {
    Out,
    Err,
}

impl<'a> Runtime<'a> {
    fn new(log: &'a (dyn Fn(&str) + Sync), children: &'a Children, relay: Relay) -> Self {
        Self {
            log,
            children,
            lock: Mutex::new(()),
            relay,
        }
    }

    /// Log `block`, which no other job's lines interrupt.
    fn say(&self, block: &str) {
        let _guard = self.lock.lock().expect("log lock");
        (self.log)(block);
    }
}

/// Run every job, keeping the sum of in-flight weights within `budget`, and
/// fail closed.
///
/// All jobs run to completion regardless of earlier failures, so one broken
/// job never hides another. The returned error names every job that failed.
pub fn run(children: &Children, jobs: Vec<Job>, budget: usize) -> Result<(), String> {
    run_with(
        jobs,
        budget,
        &Runtime::new(&|block| crate::note(block), children, Relay::Live),
    )
}

/// [`run`] with nothing logged and no operator floor, for another module's
/// tests.
#[cfg(test)]
pub(crate) fn run_unlogged(jobs: Vec<Job>, budget: usize) -> Result<(), String> {
    run_with(
        jobs,
        budget,
        &Runtime::new(&|_| {}, &Children::default(), Relay::Log),
    )
}

/// [`run`] against `children`, keeping its log for a test to read, and
/// holding it to having released every job's group.
#[cfg(test)]
pub(crate) fn run_recorded(
    jobs: Vec<Job>,
    budget: usize,
    children: &Children,
) -> (Result<(), String>, String) {
    let log = Mutex::new(String::new());
    let keep = |block: &str| {
        log.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push_str(block);
    };
    let outcome = run_with(jobs, budget, &Runtime::new(&keep, children, Relay::Log));
    assert!(
        children.live_children().is_empty(),
        "every job's group released"
    );
    let log = log
        .into_inner()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    (outcome, log)
}

/// [`run`] against `runtime`'s log and children.
fn run_with(mut jobs: Vec<Job>, budget: usize, runtime: &Runtime<'_>) -> Result<(), String> {
    let total = jobs.len();
    if total == 0 {
        return Ok(());
    }
    let budget = budget.max(1);
    heaviest_first(&mut jobs);

    if budget == 1 {
        // At most one job can ever be in flight (every weight is >= 1), so
        // stream each job's output live (no thread overhead) and still run
        // them all so the failure report is complete.
        let failures: Vec<String> = jobs
            .into_iter()
            .filter_map(|job| run_job(job, Shown::Live, runtime).err())
            .collect();
        return aggregate(failures);
    }

    let results: Mutex<Vec<Option<Result<(), String>>>> =
        Mutex::new((0..total).map(|_| None).collect());
    // Current sum of in-flight weights, guarded by a condvar the producer
    // waits on for capacity and each finished worker notifies.
    let in_flight = Mutex::new(0usize);
    let capacity = Condvar::new();

    // Shared, borrowed by every worker (distinct names so the owned `results`
    // can still be consumed after the scope); the per-iteration
    // `job`/`idx`/`weight` are moved into each worker instead.
    let results_ref = &results;
    let in_flight_ref = &in_flight;
    let capacity_ref = &capacity;

    thread::scope(|scope| {
        for (idx, job) in jobs.into_iter().enumerate() {
            let weight = job.weight;
            // Admit the job once it fits the remaining budget, or once nothing
            // is running (so a job heavier than the whole budget still runs,
            // alone, rather than deadlocking).
            let in_flight_now = {
                let mut flight = in_flight_ref.lock().expect("in-flight lock");
                while *flight != 0 && *flight + weight > budget {
                    flight = capacity_ref.wait(flight).expect("capacity wait");
                }
                *flight += weight;
                *flight
            };
            runtime.say(&format!(
                "xtask: [{}] admitted ({in_flight_now}/{budget} in flight)\n",
                job.label
            ));
            scope.spawn(move || {
                let outcome = run_job(job, Shown::Captured, runtime);
                results_ref.lock().expect("results lock")[idx] = Some(outcome);
                let mut flight = in_flight_ref.lock().expect("in-flight lock");
                *flight -= weight;
                capacity_ref.notify_all();
            });
        }
    });

    let failures: Vec<String> = results
        .into_inner()
        .expect("results lock")
        .into_iter()
        .flatten()
        .filter_map(Result::err)
        .collect();
    aggregate(failures)
}

/// How a command job's output reaches the log.
#[derive(Clone, Copy)]
enum Shown {
    /// As it is written, the job running alone.
    Live,
    /// Whole, as the job ends, others running beside it.
    Captured,
}

/// Run one job: its check, then its work, a command's output [`Shown`] as
/// asked.
fn run_job(job: Job, shown: Shown, runtime: &Runtime<'_>) -> Result<(), String> {
    let Job {
        label,
        work,
        budget,
        check,
        account,
        ..
    } = job;
    let checked = check
        .map(|check| run_check(&label, check, runtime))
        .transpose()?;
    match work {
        Work::Closure(_) if account.is_some() => {
            let why = format!("{label} holds an account, which binds only a command job");
            runtime.say(&verdict(&label, Some(&why), Duration::ZERO));
            Err(why)
        }
        Work::Closure(work) => run_closure(&label, work, runtime),
        Work::Command(command) => {
            // Refused before it starts, the work's step ends at once.
            let budget = remaining(&label, runtime.children.raised(budget), checked)
                .inspect_err(|why| runtime.say(&verdict(&label, Some(why), Duration::ZERO)))?;
            match (shown, account) {
                (Shown::Live, None) => run_live(&label, command, budget, runtime),
                (Shown::Live, Some(account)) => {
                    run_relayed(&label, command, budget, account, runtime)
                }
                (Shown::Captured, account) => {
                    run_captured(&label, command, budget, account, runtime)
                }
            }
        }
    }
}

/// Run a command job inheriting the parent's stdio, so its output is live.
fn run_live(
    label: &str,
    mut command: Command,
    budget: Duration,
    runtime: &Runtime<'_>,
) -> Result<(), String> {
    runtime.say(&format!(
        "xtask: [{label}] {command:?} (timeout {budget:?})\n"
    ));
    let started = Instant::now();
    let outcome = run_within(
        runtime.children,
        label,
        &mut command,
        budget,
        |mut child| child.wait(),
        |status| *status,
    );
    let failure = outcome.err().map(|failure| failure.message);
    runtime.say(&verdict(label, failure.as_deref(), started.elapsed()));
    failure.map_or(Ok(()), Err)
}

/// Run an accounted command job alone, relaying its output as it arrives and
/// keeping the lines libtest frames each binary's run with, to hold the job to
/// `account` once it exits cleanly. Both streams pass through here, so this
/// process writes each line whole and none lands inside another.
fn run_relayed(
    label: &str,
    mut command: Command,
    budget: Duration,
    account: Account,
    runtime: &Runtime<'_>,
) -> Result<(), String> {
    runtime.say(&format!(
        "xtask: [{label}] {command:?} (timeout {budget:?})\n"
    ));
    let started = Instant::now();
    command
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let relay = runtime.relay;
    let outcome = run_within(
        runtime.children,
        label,
        &mut command,
        budget,
        move |mut child| {
            let errors = child
                .stderr
                .take()
                .map(|stderr| thread::spawn(move || relay_lines(stderr, Stream::Err, relay)));
            let relayed = child
                .stdout
                .take()
                .map(|stdout| relay_lines(stdout, Stream::Out, relay));
            let status = child.wait()?;
            let mut relayed = relayed.transpose()?.unwrap_or_default();
            if let Some(errors) = errors {
                let errors = errors
                    .join()
                    .map_err(|_| std::io::Error::other("the stderr relay panicked"))??;
                relayed.logged.push_str(&errors.logged);
            }
            Ok((status, relayed))
        },
        |(status, _)| *status,
    );
    let (relayed, failure) = match outcome {
        Ok((_, relayed)) => {
            let failure = Tally::of(&relayed.runs)
                .and_then(|tally| account.settle(&tally))
                .err()
                .map(|why| format!("{label} {why}"));
            (Some(relayed), failure)
        }
        Err(WaitFailure {
            message, reaped, ..
        }) => (reaped.map(|(_, relayed)| relayed), Some(message)),
    };
    if let Some(logged) = relayed.as_ref().map(|relayed| relayed.logged.as_str()) {
        if !logged.is_empty() {
            runtime.say(logged);
        }
    }
    runtime.say(&verdict(label, failure.as_deref(), started.elapsed()));
    failure.map_or(Ok(()), Err)
}

/// What a relayed job wrote: the lines that frame its libtest runs, and the
/// whole of it where it is relayed to the log.
#[derive(Default)]
struct Relayed {
    runs: String,
    logged: String,
}

/// Relay `stream` a line at a time to where `relay` says, keeping the lines
/// of standard output that frame a libtest run, so a job's account costs no
/// copy of its output.
fn relay_lines(
    stream: impl std::io::Read,
    which: Stream,
    relay: Relay,
) -> std::io::Result<Relayed> {
    use std::io::{BufRead as _, Write as _};

    let mut reader = std::io::BufReader::new(stream);
    let mut relayed = Relayed::default();
    let mut line = Vec::new();
    while reader.read_until(b'\n', &mut line)? > 0 {
        let text = String::from_utf8_lossy(&line);
        match (relay, which) {
            (Relay::Live, Stream::Out) => std::io::stdout().lock().write_all(&line)?,
            (Relay::Live, Stream::Err) => std::io::stderr().lock().write_all(&line)?,
            #[cfg(test)]
            (Relay::Log, _) => relayed.logged.push_str(&text),
        }
        if matches!(which, Stream::Out) && super::libtest::frames_a_run(&text) {
            relayed.runs.push_str(text.trim_end_matches(['\r', '\n']));
            relayed.runs.push('\n');
        }
        line.clear();
    }
    if !relayed.logged.is_empty() && !relayed.logged.ends_with('\n') {
        relayed.logged.push('\n');
    }
    Ok(relayed)
}

/// Run a command job with its output captured, logged whole as it ends; a
/// killed job's too, as it names the work the budget ran out on. A job that
/// exits cleanly is then held to its `account`, if it has one.
fn run_captured(
    label: &str,
    mut command: Command,
    budget: Duration,
    account: Option<Account>,
    runtime: &Runtime<'_>,
) -> Result<(), String> {
    runtime.say(&format!(
        "xtask: [{label}] starting: {command:?} (timeout {budget:?})\n"
    ));
    let started = Instant::now();
    let outcome = output_within(runtime.children, label, &mut command, budget);
    let elapsed = started.elapsed();
    let (output, failure) = match outcome {
        Ok(output) => (Some(output), None),
        Err(WaitFailure {
            message, reaped, ..
        }) => (reaped, Some(message)),
    };
    let failure = failure.or_else(|| {
        let account = account?;
        let stdout = output
            .as_ref()
            .map(|output| String::from_utf8_lossy(&output.stdout));
        Tally::of(stdout.as_deref().unwrap_or(""))
            .and_then(|tally| account.settle(&tally))
            .err()
            .map(|why| format!("{label} {why}"))
    });
    let mut block = format!("xtask: [{label}] ----- output -----\n");
    for stream in output
        .iter()
        .flat_map(|output| [&output.stdout, &output.stderr])
    {
        let text = String::from_utf8_lossy(stream);
        if !text.trim().is_empty() {
            block.push_str(&text);
            // A stream cut off mid-line, as a killed test binary's is, must
            // not run into the line after it.
            if !text.ends_with('\n') {
                block.push('\n');
            }
        }
    }
    // Closes the block as well as reporting the cost, so the output above it
    // has an unambiguous end.
    block.push_str(&verdict(label, failure.as_deref(), elapsed));
    runtime.say(&block);
    failure.map_or(Ok(()), Err)
}

/// Run a closure job as a step: its start line, then its closing line. Its
/// failure's detail is not logged, as a guest's serial log is far too long to
/// show twice: it goes back in the run's error, for its caller to show once
/// the run is over.
fn run_closure(label: &str, work: Task, runtime: &Runtime<'_>) -> Result<(), String> {
    runtime.say(&format!("xtask: [{label}] starting\n"));
    let started = Instant::now();
    let outcome = work();
    runtime.say(&elapsed_line(label, outcome.is_ok(), started.elapsed()));
    outcome
}

/// Make a job's check, reported as a step of its own: the time it took, or
/// the refusal that fails the job.
fn run_check(label: &str, check: Task, runtime: &Runtime<'_>) -> Result<Duration, String> {
    let label = format!("{label} check");
    runtime.say(&format!("xtask: [{label}] starting\n"));
    let started = Instant::now();
    let outcome = check();
    let elapsed = started.elapsed();
    runtime.say(&verdict(
        &label,
        outcome.as_ref().err().map(String::as_str),
        elapsed,
    ));
    outcome.map(|()| elapsed)
}

/// A step's closing line, after why where it failed, so the reason is logged
/// as the step ends rather than only once the whole run does.
fn verdict(label: &str, failure: Option<&str>, elapsed: Duration) -> String {
    let closing = elapsed_line(label, failure.is_none(), elapsed);
    match failure {
        None => closing,
        Some(why) => format!("xtask: [{label}] {why}\n{closing}"),
    }
}

/// What is left of a command job's `budget` once its check, if it has one,
/// took `checked`, refusing the job if the check took all of it.
fn remaining(label: &str, budget: Duration, checked: Option<Duration>) -> Result<Duration, String> {
    let Some(checked) = checked else {
        return Ok(budget);
    };
    budget
        .checked_sub(checked)
        .filter(|left| !left.is_zero())
        .ok_or_else(|| format!("{label}: its check took {checked:?}, its whole {budget:?} budget"))
}

/// Collapse the collected failures into one `Result`.
fn aggregate(failures: Vec<String>) -> Result<(), String> {
    match failures.len() {
        0 => Ok(()),
        1 => Err(failures.into_iter().next().unwrap_or_default()),
        n => Err(format!("{n} job(s) failed:\n{}", failures.join("\n"))),
    }
}

#[cfg(test)]
mod tests {
    use super::{default_concurrency, host_parallelism, remaining, Job};
    use crate::children::Children;
    use crate::commands::libtest::Account;
    use std::process::Command;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    /// Run `jobs` with the log kept for the test to read and `floor` the
    /// operator's, whatever the environment says.
    fn recorded_with(
        jobs: Vec<Job>,
        budget: usize,
        floor: Option<Duration>,
    ) -> (Result<(), String>, String) {
        super::run_recorded(jobs, budget, &Children::new(floor))
    }

    /// [`recorded_with`] with no operator floor.
    fn recorded(jobs: Vec<Job>, budget: usize) -> (Result<(), String>, String) {
        recorded_with(jobs, budget, None)
    }

    fn ok_job(label: &str) -> Job {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "exit 0"]);
        Job::new(label, cmd)
    }

    fn fail_job(label: &str) -> Job {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "exit 1"]);
        Job::new(label, cmd)
    }

    /// A job that exits cleanly having run two tests and printed their
    /// summary as libtest would.
    fn summarising_job(label: &str, summary: &str) -> Job {
        let mut cmd = Command::new("sh");
        cmd.args([
            "-c",
            "printf 'running 2 tests\\n%s\\n' \"$1\"",
            "sh",
            summary,
        ]);
        Job::new(label, cmd)
    }

    /// A job whose tests fall short of its account fails though it exited
    /// cleanly, alone or beside others, and its log says how it fell short.
    #[test]
    fn a_clean_exit_short_of_its_account_fails_the_job() {
        const TWO: &str =
            "test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 5 filtered out; finished in 0.01s";
        for budget in [1, 4] {
            let (outcome, _) = recorded(
                vec![summarising_job("whole", TWO).accounted(Account::Exactly(2))],
                budget,
            );
            assert_eq!(outcome, Ok(()), "budget {budget}");
            let (outcome, log) = recorded(
                vec![summarising_job("short", TWO).accounted(Account::Exactly(3))],
                budget,
            );
            assert!(outcome.is_err(), "budget {budget}");
            assert!(
                log.contains("short ran 2 of the 3 tests dealt to it"),
                "budget {budget}: {log}"
            );
            let (outcome, _) =
                recorded(vec![ok_job("silent").accounted(Account::Selected)], budget);
            assert!(outcome.is_err(), "budget {budget}: no summary is no count");
        }
        // Captured, so the summary stays out of this binary's own output,
        // which the gate reads its tests from.
        let (outcome, _) = recorded(vec![summarising_job("unheld", TWO)], 4);
        assert_eq!(
            outcome,
            Ok(()),
            "a job with no account is judged by its exit"
        );
    }

    /// A relayed job's standard error reaches the log with its output, never
    /// the recording process's own stream.
    #[test]
    fn a_relayed_job_relays_both_its_streams() {
        let mut cmd = Command::new("sh");
        cmd.args([
            "-c",
            "printf 'running 1 test\\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; \
             0 filtered out; finished in 0.00s\\n'; printf 'a warning\\n' >&2",
        ]);
        let (outcome, log) = recorded(
            vec![Job::new("both", cmd).accounted(Account::Everything)],
            1,
        );
        assert_eq!(outcome, Ok(()), "{log}");
        assert!(log.contains("a warning\n"), "{log}");
    }

    /// A closure reports its own verdict, so an account given to one cannot be
    /// kept, and the job fails rather than run unheld.
    #[test]
    fn a_closure_given_an_account_fails() {
        for budget in [1, 2] {
            let job = Job::closure("judged", 1, || Ok(())).accounted(Account::Everything);
            let (outcome, log) = recorded(vec![job], budget);
            let err = outcome.expect_err("an account a closure cannot keep");
            assert!(err.contains("binds only a command job"), "{err}");
            assert!(log.contains("[judged] FAILED in"), "{log}");
        }
    }

    /// A command job that would never finish on its own, budgeted so tightly
    /// that only a working kill can end it.
    ///
    /// It runs as long as this test does against a sub-second budget, so the
    /// margin is enormous in both directions: a busy host cannot make it pass
    /// by accident, and it can only complete quickly if the job was really
    /// killed.
    fn hung_job(label: &str) -> Job {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", crate::process::WHILE_ITS_PARENT_RUNS]);
        Job::new(label, cmd).with_budget(Duration::from_millis(200))
    }

    #[test]
    fn host_parallelism_is_at_least_one() {
        assert!(host_parallelism() >= 1);
    }

    #[test]
    fn a_hung_job_is_killed_and_fails_on_either_path() {
        // A budget of one streams the job's output, two captures it: separate
        // implementations, each needing its own proof that it is bounded.
        for budget in [1, 2] {
            let started = std::time::Instant::now();
            let (outcome, log) = recorded(vec![hung_job("stuck"), ok_job("fine")], budget);
            let err = outcome.expect_err("a job that outlives its budget must fail the run");
            assert!(err.contains("stuck"), "error should name the job: {err}");
            assert!(err.contains("timeout"), "error should say why: {err}");
            assert!(
                err.contains("killed"),
                "and that it is not left running: {err}"
            );
            assert!(!err.contains("fine"), "{err}");
            assert!(log.contains("[stuck] stuck exceeded its"), "{log}");
            assert!(log.contains("[stuck] FAILED in"), "{log}");
            assert!(
                started.elapsed() < Duration::from_secs(60),
                "the runner must not wait out a hung job, took {:?}",
                started.elapsed()
            );
        }
    }

    #[test]
    fn concurrency_is_clamped_to_the_job_count_and_at_least_one() {
        assert_eq!(default_concurrency(0), 1);
        assert_eq!(default_concurrency(1), 1);
        // Never exceeds the job count even on a many-core host.
        assert!(default_concurrency(2) <= 2);
        assert!(default_concurrency(2) >= 1);
    }

    #[test]
    fn an_empty_job_list_succeeds() {
        assert_eq!(recorded(Vec::new(), 4).0, Ok(()));
    }

    #[test]
    fn all_successful_jobs_pass_in_parallel() {
        let jobs = vec![ok_job("a"), ok_job("b"), ok_job("c")];
        assert_eq!(recorded(jobs, 3).0, Ok(()));
    }

    #[test]
    fn all_successful_jobs_pass_when_forced_serial() {
        let jobs = vec![ok_job("a"), ok_job("b")];
        assert_eq!(recorded(jobs, 1).0, Ok(()));
    }

    #[test]
    fn a_single_failing_job_fails_closed() {
        for budget in [1, 3] {
            let (outcome, log) = recorded(vec![ok_job("a"), fail_job("bad"), ok_job("c")], budget);
            let err = outcome.expect_err("a failing job must fail the run");
            assert!(
                err.contains("bad"),
                "error should name the failed job: {err}"
            );
            assert!(log.contains("[bad] bad failed with"), "{log}");
            assert!(log.contains("[bad] FAILED in"), "{log}");
        }
    }

    #[test]
    fn every_failure_is_reported_not_just_the_first() {
        let err = recorded(vec![fail_job("first"), fail_job("second")], 2)
            .0
            .expect_err("failures must propagate");
        assert!(err.contains("first"), "missing first failure: {err}");
        assert!(err.contains("second"), "missing second failure: {err}");
    }

    #[test]
    fn a_command_that_cannot_be_spawned_ends_its_step_at_once() {
        for budget in [1, 3] {
            let job = Job::new("unspawnable", Command::new("/nonexistent/xtask-unit-test"));
            let (outcome, log) = recorded(vec![job], budget);
            let err = outcome.expect_err("there is nothing to run");
            assert!(err.contains("unspawnable could not be spawned"), "{err}");
            assert!(
                log.contains("[unspawnable] unspawnable could not be spawned"),
                "{log}"
            );
            assert!(log.contains("[unspawnable] FAILED in"), "{log}");
        }
    }

    #[test]
    fn a_captured_stream_cut_off_mid_line_ends_its_own_line() {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "printf 'test cut::off ... '; printf started >&2"]);
        let (outcome, log) = recorded(vec![Job::new("cut", cmd)], 2);
        assert_eq!(outcome, Ok(()), "{log}");
        assert!(
            log.contains("test cut::off ... \nstarted\nxtask: [cut] done in"),
            "{log}"
        );
    }

    #[test]
    fn the_operators_floor_reaches_every_command_job() {
        for budget in [1, 3] {
            let jobs = vec![
                ok_job("ordinary"),
                ok_job("long").with_budget(Duration::from_hours(3)),
            ];
            let (outcome, log) = recorded_with(jobs, budget, Some(Duration::from_hours(2)));
            assert_eq!(outcome, Ok(()), "{log}");
            let started = |label: &str, budget: &str| {
                log.lines().any(|line| {
                    line.starts_with(&format!("xtask: [{label}] "))
                        && line.ends_with(&format!("(timeout {budget})"))
                })
            };
            assert!(started("ordinary", "7200s"), "raised to the floor: {log}");
            assert!(started("long", "10800s"), "and never lowered by it: {log}");
        }
    }

    #[test]
    fn a_checked_job_runs_its_work_only_once_its_check_passes() {
        for budget in [1, 3] {
            let checks = Arc::new(AtomicUsize::new(0));
            let worked = Arc::new(AtomicUsize::new(0));
            let counted = |verdict: Result<(), String>| {
                let checks = Arc::clone(&checks);
                move || {
                    checks.fetch_add(1, Ordering::SeqCst);
                    verdict
                }
            };
            let working = |label: &str| {
                let worked = Arc::clone(&worked);
                Job::closure(label, 1, move || {
                    worked.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                })
            };
            let refused = working("refused").checked_by(counted(Err("stale skip".into())));
            let passed = working("passed").checked_by(counted(Ok(())));
            let err = recorded(vec![refused, passed], budget)
                .0
                .expect_err("the check fails it");
            assert!(err.contains("stale skip"), "{err}");
            assert!(!err.contains("passed"), "{err}");
            assert_eq!(worked.load(Ordering::SeqCst), 1, "the refused work ran");
            let chained = working("chained")
                .checked_by(counted(Ok(())))
                .checked_by(counted(Err("second".into())));
            let err = recorded(vec![chained], budget)
                .0
                .expect_err("its second check fails it");
            assert!(err.contains("second"), "{err}");
            assert_eq!(worked.load(Ordering::SeqCst), 1, "the chained work ran");
            assert_eq!(
                checks.load(Ordering::SeqCst),
                4,
                "every check made, as its job starts"
            );
        }
    }

    #[test]
    fn a_check_that_outlasts_its_jobs_budget_fails_the_job() {
        for budget in [1, 3] {
            let starved = ok_job("starved")
                .with_budget(Duration::from_millis(50))
                .checked_by(|| {
                    std::thread::sleep(Duration::from_millis(200));
                    Ok(())
                });
            let (outcome, log) = recorded(vec![starved], budget);
            let err = outcome.expect_err("nothing is left for its work");
            assert!(err.contains("starved: its check took"), "{err}");
            assert!(log.contains("[starved] starved: its check took"), "{log}");
            assert!(log.contains("[starved] FAILED in"), "{log}");
        }
    }

    #[test]
    fn a_checks_time_comes_out_of_its_jobs_budget() {
        let budget = Duration::from_secs(10);
        assert_eq!(remaining("j", budget, None), Ok(budget), "no check");
        assert_eq!(
            remaining("j", budget, Some(Duration::from_secs(4))),
            Ok(Duration::from_secs(6))
        );
        for spent in [budget, Duration::from_secs(11)] {
            let refusal = remaining("j", budget, Some(spent)).expect_err("nothing left");
            assert!(refusal.contains("j: its check took"), "{refusal}");
        }
    }

    #[test]
    fn a_checked_command_job_runs_once_its_check_passes() {
        for budget in [1, 3] {
            let (outcome, log) = recorded(vec![ok_job("passed").checked_by(|| Ok(()))], budget);
            assert_eq!(outcome, Ok(()), "{log}");
            assert!(log.contains("[passed check] done in"), "{log}");
            assert!(log.contains("[passed] done in"), "{log}");
        }
    }

    #[test]
    fn a_refused_check_logs_its_reason_as_it_happens() {
        for budget in [1, 3] {
            let refused =
                Job::closure("refused", 1, || Ok(())).checked_by(|| Err("stale skip".into()));
            let (outcome, log) = recorded(vec![refused], budget);
            assert!(outcome.is_err(), "{log}");
            assert!(log.contains("[refused check] stale skip\n"), "{log}");
            assert!(log.contains("[refused check] FAILED in"), "{log}");
        }
    }

    #[test]
    fn a_closure_job_reports_its_start_and_verdict_on_either_path() {
        for budget in [1, 3] {
            let jobs = vec![
                Job::closure("fine", 1, || Ok(())),
                Job::closure("broken", 1, || Err("the guest's serial log".into())),
            ];
            let (outcome, log) = recorded(jobs, budget);
            for line in ["[fine] starting", "[fine] done in", "[broken] FAILED in"] {
                assert!(
                    log.contains(line),
                    "{line} missing at budget {budget}: {log}"
                );
            }
            assert_eq!(
                outcome,
                Err("the guest's serial log".into()),
                "the detail in the run's error"
            );
            assert!(!log.contains("serial log"), "and only there: {log}");
        }
    }

    #[test]
    fn closure_jobs_run_and_report_their_own_result() {
        let ran = Arc::new(AtomicUsize::new(0));
        let jobs: Vec<Job> = (0..4)
            .map(|i| {
                let ran = Arc::clone(&ran);
                Job::closure(format!("c{i}"), 1, move || {
                    ran.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                })
            })
            .collect();
        assert_eq!(recorded(jobs, 2).0, Ok(()));
        assert_eq!(ran.load(Ordering::SeqCst), 4);
    }

    #[test]
    fn a_failing_closure_job_is_reported_by_label_and_detail() {
        let jobs = vec![
            Job::closure("good", 1, || Ok(())),
            Job::closure("weighty", 2, || Err("boom detail".to_string())),
        ];
        let err = recorded(jobs, 4)
            .0
            .expect_err("a failing closure must fail the run");
        assert!(err.contains("boom detail"), "missing detail: {err}");
    }

    #[test]
    fn weighted_admission_never_exceeds_the_budget() {
        // Track concurrent in-flight weight and assert it never exceeds the
        // budget. Each job holds its weight briefly so overlap is real.
        let live = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let budget = 4;
        let weights = [1usize, 2, 1, 3, 1, 1, 2];
        let jobs: Vec<Job> = weights
            .iter()
            .enumerate()
            .map(|(i, &w)| {
                let live = Arc::clone(&live);
                let peak = Arc::clone(&peak);
                Job::closure(format!("w{i}"), w, move || {
                    let now = live.fetch_add(w, Ordering::SeqCst) + w;
                    peak.fetch_max(now, Ordering::SeqCst);
                    std::thread::sleep(std::time::Duration::from_millis(20));
                    live.fetch_sub(w, Ordering::SeqCst);
                    Ok(())
                })
            })
            .collect();
        assert_eq!(recorded(jobs, budget).0, Ok(()));
        assert!(
            peak.load(Ordering::SeqCst) <= budget,
            "in-flight weight {} exceeded budget {budget}",
            peak.load(Ordering::SeqCst)
        );
    }

    #[test]
    fn a_whole_budget_job_runs_strictly_alone() {
        // A job weighing the entire budget must never overlap another job:
        // this is the isolation the QEMU matrix relies on for a full-boot SMP
        // guest, whose mutually synchronising vCPU threads starve if
        // co-scheduled. Small jobs mark themselves "live" while they run; the
        // whole-budget job asserts nothing else is live for its whole run.
        let budget = 4;
        let live = Arc::new(AtomicUsize::new(0));
        let overlapped = Arc::new(AtomicBool::new(false));
        let mut jobs: Vec<Job> = Vec::new();
        for i in 0..8 {
            let live_small = Arc::clone(&live);
            jobs.push(Job::closure(format!("small{i}"), 1, move || {
                live_small.fetch_add(1, Ordering::SeqCst);
                std::thread::sleep(std::time::Duration::from_millis(5));
                live_small.fetch_sub(1, Ordering::SeqCst);
                Ok(())
            }));
            // Drop the whole-budget job into the middle of the stream so
            // small jobs are in flight both before and after it.
            if i == 4 {
                let live_whole = Arc::clone(&live);
                let overlapped_whole = Arc::clone(&overlapped);
                jobs.push(Job::closure("whole", budget, move || {
                    // On entry and after a deliberate hold, no other job may
                    // be live — the whole-budget job owns the runner alone.
                    if live_whole.load(Ordering::SeqCst) != 0 {
                        overlapped_whole.store(true, Ordering::SeqCst);
                    }
                    std::thread::sleep(std::time::Duration::from_millis(25));
                    if live_whole.load(Ordering::SeqCst) != 0 {
                        overlapped_whole.store(true, Ordering::SeqCst);
                    }
                    Ok(())
                }));
            }
        }
        assert_eq!(recorded(jobs, budget).0, Ok(()));
        assert!(
            !overlapped.load(Ordering::SeqCst),
            "a whole-budget job ran concurrently with another job"
        );
    }

    #[test]
    fn a_job_heavier_than_the_budget_still_runs_alone() {
        let ran = Arc::new(AtomicUsize::new(0));
        let ran2 = Arc::clone(&ran);
        // Weight 9 against a budget of 4: it cannot fit the budget, so it must
        // run when nothing else is in flight rather than deadlock.
        let jobs = vec![
            Job::closure("oversized", 9, move || {
                ran2.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }),
            ok_job("small"),
        ];
        assert_eq!(recorded(jobs, 4).0, Ok(()));
        assert_eq!(ran.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_whole_budget_job_is_admitted_before_the_lighter_ones() {
        // Admission is head-of-line, so a whole-budget job listed last drains
        // the runner to empty first and leaves the host idle while it runs.
        // Recording the admission order proves the heavy job goes first.
        let order = Arc::new(Mutex::new(Vec::new()));
        let mut jobs = Vec::new();
        for label in ["light-a", "light-b", "light-c"] {
            let seen = Arc::clone(&order);
            jobs.push(Job::closure(label, 2, move || {
                seen.lock().expect("order").push(label);
                Ok(())
            }));
        }
        let seen = Arc::clone(&order);
        jobs.push(Job::closure("heavy", 8, move || {
            seen.lock().expect("order").push("heavy");
            Ok(())
        }));

        assert_eq!(recorded(jobs, 8).0, Ok(()));
        let seen = order.lock().expect("order");
        assert_eq!(
            seen.first().copied(),
            Some("heavy"),
            "the whole-budget job must run before the lighter ones, got {seen:?}"
        );
        assert_eq!(seen.len(), 4, "every job must still run: {seen:?}");
    }

    #[test]
    fn equal_weight_jobs_keep_the_order_they_were_given() {
        // The gate groups are all weight one, so reordering must be a no-op
        // for them: a stable sort is what keeps their reporting deterministic.
        let order = Arc::new(Mutex::new(Vec::new()));
        let mut jobs = Vec::new();
        for label in ["first", "second", "third"] {
            let seen = Arc::clone(&order);
            jobs.push(Job::closure(label, 1, move || {
                seen.lock().expect("order").push(label);
                Ok(())
            }));
        }
        // Budget one takes the streaming path, which runs strictly one at a
        // time, so the recorded order is the admission order exactly.
        assert_eq!(recorded(jobs, 1).0, Ok(()));
        assert_eq!(
            *order.lock().expect("order"),
            vec!["first", "second", "third"]
        );
    }
}
