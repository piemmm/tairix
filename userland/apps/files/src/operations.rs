//! The long file operations the file manager runs off its event loop: a
//! paste, a delete, a move to Trash.
//!
//! Each operation is known by the id of its own progress window, and a folder
//! window may have several. One worker carries them out one at a time, in the
//! order they were asked: two copies sharing one disk finish no sooner side by
//! side, and each would read as stalled. So an operation that touches a tree
//! an earlier one is still working in waits behind it, as every operation
//! does. An operation is answered to its own id and no other, and one whose
//! window closes is cancelled and has its answer dropped.
//!
//! # No I/O, and no lock
//!
//! [`OperationDesk`] is the policy alone, so it compiles and is tested on the
//! host. The program holds it under the reader's mutex, carries each job out
//! on its worker thread, and wakes the loop when the desk says a wake is owed.

use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::vec::Vec;

pub use tairix_browse::Reading;
use tairix_browse::Tally;
use tairix_fscopy::Omissions;

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

/// An operation's answer: what it did, how it ended, and what its copies
/// could not keep.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Finished {
    /// What was done before it ended.
    pub done: Tally,
    /// How it ended.
    pub ending: Ending,
    /// The extended attributes its copies' destinations could not keep.
    pub omissions: Omissions,
}

/// Where an operation stands, as its progress window shows it: borrowed from
/// the desk, so looking costs no copy until something new is to be shown.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct Standing<'a> {
    /// What its walk last said of itself.
    pub reading: Reading,
    /// The item in hand, by its name.
    pub current: &'a str,
    /// Whether a cancel was asked for.
    pub cancelling: bool,
    /// Whether it waits for the operations asked before it.
    pub waiting: bool,
}

/// Why an operation was not taken. The job is handed back with it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Refusal {
    /// An operation of that id is already held.
    Busy,
    /// As many as the desk holds are already waiting.
    Full,
    /// No worker will take it, so it is carried out by its asker.
    Stopping,
}

/// The operation the worker is carrying out.
struct Running {
    id: u64,
    reading: Reading,
    current: String,
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

    /// Ask for `job` to run as operation `id`.
    ///
    /// # Errors
    ///
    /// The job, handed back with why it was not taken.
    pub fn submit(&mut self, id: u64, job: J) -> Result<(), (J, Refusal)> {
        if self.stopping {
            return Err((job, Refusal::Stopping));
        }
        if self.holds(id) {
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
        self.waiting.push_back((id, job));
        Ok(())
    }

    /// Whether operation `id` is waiting, running, or answered and not
    /// collected.
    #[must_use]
    pub fn holds(&self, id: u64) -> bool {
        self.waiting.iter().any(|(held, _)| *held == id)
            || self.running_for(id).is_some()
            || self.landed.iter().any(|(held, _)| *held == id)
    }

    /// Where unfinished operation `id` stands, if it is held.
    #[must_use]
    pub fn standing(&self, id: u64) -> Option<Standing<'_>> {
        if let Some(running) = self.running_for(id) {
            return Some(Standing {
                reading: running.reading,
                current: &running.current,
                cancelling: running.cancel,
                waiting: false,
            });
        }
        self.waiting
            .iter()
            .any(|(held, _)| *held == id)
            .then(|| Standing {
                waiting: true,
                ..Standing::default()
            })
    }

    /// Cancel operation `id`: one still waiting ends at once with nothing
    /// done, one running stops before its next step.
    pub fn cancel(&mut self, id: u64) {
        if let Some(running) = self.running_for_mut(id) {
            running.cancel = true;
            return;
        }
        if let Some(at) = self.waiting.iter().position(|(held, _)| *held == id) {
            drop(self.waiting.remove(at));
            self.landed.push((
                id,
                Finished {
                    done: Tally::default(),
                    ending: Ending::Cancelled,
                    omissions: Omissions::default(),
                },
            ));
        }
    }

    /// Take operation `id`'s answer, if it has landed.
    pub fn take(&mut self, id: u64) -> Option<Finished> {
        let at = self.landed.iter().position(|(held, _)| *held == id)?;
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

    /// Operation `id`'s window has closed: if it waits it never runs, if it
    /// runs it stops at its next step, and no answer is kept for it.
    pub fn forget(&mut self, id: u64) {
        self.waiting.retain(|(held, _)| *held != id);
        if let Some(running) = self.running_for_mut(id) {
            running.cancel = true;
            running.orphaned = true;
        }
        self.landed.retain(|(held, _)| *held != id);
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
        let (id, job) = self.waiting.pop_front()?;
        self.running = Some(Running {
            id,
            reading: Reading::default(),
            current: String::new(),
            cancel: false,
            orphaned: false,
        });
        Some((id, job))
    }

    /// Whether the running operation should stop before its next step.
    #[must_use]
    pub fn cancelled(&self) -> bool {
        self.stopping || self.running.as_ref().is_some_and(|running| running.cancel)
    }

    /// The running operation's walk says `reading` of itself, working on
    /// `current`, answering whether the loop is owed a wake to show it.
    pub fn report(&mut self, reading: Reading, current: &str) -> bool {
        let Some(running) = self.running.as_mut() else {
            return false;
        };
        running.reading = reading;
        if running.current != current {
            running.current.clear();
            running.current.push_str(current);
        }
        !running.orphaned && self.owe()
    }

    /// The running operation ended as `ending`, its copies having left
    /// `omissions`, answering whether the loop is owed a wake to collect it.
    pub fn finish(&mut self, ending: Ending, omissions: Omissions) -> bool {
        let Some(running) = self.running.take() else {
            return false;
        };
        if running.orphaned {
            return false;
        }
        self.landed.push((
            running.id,
            Finished {
                done: running.reading.done,
                ending,
                omissions,
            },
        ));
        self.owe()
    }

    fn owe(&mut self) -> bool {
        !core::mem::replace(&mut self.nudged, true)
    }

    fn running_for(&self, id: u64) -> Option<&Running> {
        self.running
            .as_ref()
            .filter(|running| running.id == id && !running.orphaned)
    }

    fn running_for_mut(&mut self, id: u64) -> Option<&mut Running> {
        self.running
            .as_mut()
            .filter(|running| running.id == id && !running.orphaned)
    }
}

#[cfg(test)]
#[path = "operations_tests.rs"]
mod tests;
