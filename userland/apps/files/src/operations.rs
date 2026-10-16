//! The long file operations the file manager runs off its event loop: a
//! paste, a delete, a move to Trash.
//!
//! One worker carries them out one at a time, in the order they were asked:
//! two copies sharing one disk finish no sooner side by side, and each would
//! read as stalled. An operation is answered to the window that started it
//! and to no other, and a window that closes cancels its own operation and has
//! its answer dropped.
//!
//! # No I/O, and no lock
//!
//! [`OperationDesk`] is the policy alone, so it compiles and is tested on the
//! host. The program holds it under the reader's mutex, carries each job out
//! on its worker thread, and wakes the loop when the desk says a wake is owed.

use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::vec::Vec;

/// How an operation ended. What was done before the end stays done.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Ending {
    /// Everything it was asked to do was done.
    Completed,
    /// The user cancelled it.
    Cancelled,
    /// A refusal stopped it, for this reason.
    Stopped(String),
}

/// Where one unit of an operation left it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Step {
    /// More remains.
    Working,
    /// Nothing remains.
    Done,
    /// A refusal stopped it, stated in these words.
    Stopped(String),
}

/// Carry an operation on a unit at a time through `step`, for as long as
/// `out_of_time` allows, answering how it ended — or `None`, still working,
/// once the slice is spent. A cancel is honoured at a unit's end: `cancelled`
/// is asked before each, and the caller abandons what was under way.
pub fn run_slice(
    mut step: impl FnMut() -> Step,
    mut cancelled: impl FnMut() -> bool,
    mut out_of_time: impl FnMut() -> bool,
) -> Option<Ending> {
    loop {
        if cancelled() {
            return Some(Ending::Cancelled);
        }
        match step() {
            Step::Working if out_of_time() => return None,
            Step::Working => {}
            Step::Done => return Some(Ending::Completed),
            Step::Stopped(reason) => return Some(Ending::Stopped(reason)),
        }
    }
}

/// An operation's answer: how many items it did, and how it ended.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Finished {
    /// The items done before it ended.
    pub done: usize,
    /// How it ended.
    pub ending: Ending,
}

/// Where a window's operation stands, as its progress panel shows it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Standing {
    /// The items done so far.
    pub done: usize,
    /// Whether a cancel was asked for.
    pub cancelling: bool,
}

/// Why an operation was not taken. The job is handed back with it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Refusal {
    /// The window already has one.
    Busy,
    /// As many as the desk holds are already waiting.
    Full,
    /// No worker will take it, so it is carried out by its asker.
    Stopping,
}

/// The operation the worker is carrying out.
struct Running {
    window: u64,
    done: usize,
    cancel: bool,
    /// Its window closed, so its answer belongs to nobody.
    orphaned: bool,
}

/// The operations waiting, the one running, and the answers not yet
/// collected.
pub struct OperationDesk<J> {
    waiting: VecDeque<(u64, J)>,
    running: Option<Running>,
    landed: Vec<(u64, Finished)>,
    /// A wake is owed the loop only for the first report since it last
    /// looked, so a run of reports costs it one wake.
    nudged: bool,
    stopping: bool,
    capacity: usize,
}

impl<J> OperationDesk<J> {
    /// A desk holding at most `capacity` operations waiting.
    #[must_use]
    pub const fn new(capacity: usize) -> Self {
        Self {
            waiting: VecDeque::new(),
            running: None,
            landed: Vec::new(),
            nudged: false,
            stopping: false,
            capacity,
        }
    }

    /// Ask for `job` to run on behalf of `window`.
    ///
    /// # Errors
    ///
    /// The job, handed back with why it was not taken.
    pub fn submit(&mut self, window: u64, job: J) -> Result<(), (J, Refusal)> {
        if self.stopping {
            return Err((job, Refusal::Stopping));
        }
        if self.holds(window) {
            return Err((job, Refusal::Busy));
        }
        // Room for this job and for every answer still to come, so recording
        // an answer later never allocates.
        let answers = self.waiting.len() + usize::from(self.running.is_some()) + 1;
        if self.waiting.len() >= self.capacity
            || self.waiting.try_reserve(1).is_err()
            || self.landed.try_reserve(answers).is_err()
        {
            return Err((job, Refusal::Full));
        }
        self.waiting.push_back((window, job));
        Ok(())
    }

    /// Whether `window` has an operation waiting, running, or answered and
    /// not collected.
    #[must_use]
    pub fn holds(&self, window: u64) -> bool {
        self.waiting.iter().any(|(owner, _)| *owner == window)
            || self.running_for(window).is_some()
            || self.landed.iter().any(|(owner, _)| *owner == window)
    }

    /// Where `window`'s unfinished operation stands, if it has one.
    #[must_use]
    pub fn standing(&self, window: u64) -> Option<Standing> {
        if let Some(running) = self.running_for(window) {
            return Some(Standing {
                done: running.done,
                cancelling: running.cancel,
            });
        }
        self.waiting
            .iter()
            .any(|(owner, _)| *owner == window)
            .then_some(Standing {
                done: 0,
                cancelling: false,
            })
    }

    /// Cancel `window`'s operation: one still waiting ends at once with
    /// nothing done, one running stops before its next step.
    pub fn cancel(&mut self, window: u64) {
        if let Some(running) = self.running_for_mut(window) {
            running.cancel = true;
            return;
        }
        if let Some(at) = self.waiting.iter().position(|(owner, _)| *owner == window) {
            drop(self.waiting.remove(at));
            self.landed.push((
                window,
                Finished {
                    done: 0,
                    ending: Ending::Cancelled,
                },
            ));
        }
    }

    /// Take `window`'s answer, if it has landed.
    pub fn take(&mut self, window: u64) -> Option<Finished> {
        let at = self.landed.iter().position(|(owner, _)| *owner == window)?;
        Some(self.landed.swap_remove(at).1)
    }

    /// The loop has looked at the desk: the next report owes a wake again.
    pub fn acknowledge(&mut self) {
        self.nudged = false;
    }

    /// The loop will look again at a time of its own choosing, so no report
    /// owes it a wake before then.
    pub fn hold(&mut self) {
        self.nudged = true;
    }

    /// `window` has closed: what it was waiting for never runs, what is
    /// running stops at its next step, and no answer is kept for it.
    pub fn forget(&mut self, window: u64) {
        self.waiting.retain(|(owner, _)| *owner != window);
        if let Some(running) = self.running_for_mut(window) {
            running.cancel = true;
            running.orphaned = true;
        }
        self.landed.retain(|(owner, _)| *owner != window);
    }

    /// Send the worker away: nothing waiting runs, and what runs stops at its
    /// next step.
    pub fn stop(&mut self) {
        self.stopping = true;
        self.waiting.clear();
        if let Some(running) = self.running.as_mut() {
            running.cancel = true;
        }
    }

    /// Whether the worker has been sent away.
    #[must_use]
    pub const fn stopping(&self) -> bool {
        self.stopping
    }

    /// The worker's next job, oldest first: none while one runs, or once the
    /// desk is stopping.
    pub fn next_job(&mut self) -> Option<(u64, J)> {
        if self.stopping || self.running.is_some() {
            return None;
        }
        let (window, job) = self.waiting.pop_front()?;
        self.running = Some(Running {
            window,
            done: 0,
            cancel: false,
            orphaned: false,
        });
        Some((window, job))
    }

    /// Whether the running operation should stop before its next step.
    #[must_use]
    pub fn cancelled(&self) -> bool {
        self.stopping || self.running.as_ref().is_some_and(|running| running.cancel)
    }

    /// The running operation has done `done` items, answering whether the
    /// loop is owed a wake to show it.
    pub fn report(&mut self, done: usize) -> bool {
        let Some(running) = self.running.as_mut() else {
            return false;
        };
        running.done = done;
        !running.orphaned && self.owe()
    }

    /// The running operation ended, answering whether the loop is owed a wake
    /// to collect it.
    pub fn finish(&mut self, ending: Ending) -> bool {
        let Some(running) = self.running.take() else {
            return false;
        };
        if running.orphaned {
            return false;
        }
        self.landed.push((
            running.window,
            Finished {
                done: running.done,
                ending,
            },
        ));
        self.owe()
    }

    fn owe(&mut self) -> bool {
        !core::mem::replace(&mut self.nudged, true)
    }

    fn running_for(&self, window: u64) -> Option<&Running> {
        self.running
            .as_ref()
            .filter(|running| running.window == window && !running.orphaned)
    }

    fn running_for_mut(&mut self, window: u64) -> Option<&mut Running> {
        self.running
            .as_mut()
            .filter(|running| running.window == window && !running.orphaned)
    }
}

#[cfg(test)]
#[path = "operations_tests.rs"]
mod tests;
