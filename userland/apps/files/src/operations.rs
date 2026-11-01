//! The long file operations the file manager runs off its event loop: a
//! paste, a delete, a move to Trash.
//!
//! Each operation is known by the id of its own progress window, and a folder
//! window may have several. Operations touching one volume run one at a time,
//! in the order they were asked: two copies sharing one disk finish no sooner
//! side by side, and each would read as stalled. So an operation waits behind
//! any asked before it that touches one of its volumes, and one touching none
//! of theirs runs at once beside them on a worker of its own: a copy onto a
//! failing stick holds up no delete on the local disk. An operation is
//! answered to its own id and no other, and one whose window closes is
//! cancelled and has its answer dropped.
//!
//! # No I/O, and no lock
//!
//! [`OperationDesk`] is the policy alone, so it compiles and is tested on the
//! host. The program holds it under the reader's mutex, carries each job out
//! on a worker thread through [`carry`], which closes a finished job's files
//! before that mutex is taken for its answer, and wakes the loop when the desk
//! says a wake is owed.

use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::vec::Vec;

pub use tairix_browse::Reading;
use tairix_browse::Tally;
use tairix_fscopy::Omissions;

use crate::lanes::Lane;

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

/// What a worker carries out: a unit at a time, abandoned part-way on a
/// cancel, and dropped at its end, which closes its files.
pub trait Carried {
    /// What its answer needs once its files are closed.
    type Remains;

    /// Carry out one bounded unit.
    fn step(&mut self) -> Step;

    /// Stop part-way, settling what the unit under way left half-done.
    fn abandon(&mut self);

    /// Take out what its answer needs, before it is dropped.
    fn remains(&mut self) -> Self::Remains;
}

/// Carry `job` out on a worker to its end, `nudge`ing the loop whenever a
/// wake is owed. After each unit `report` answers whether one is owed and
/// whether a cancel was asked, which is honoured at that unit's end. The job
/// is dropped, closing its files, before `settle` records its answer: a close
/// on a served volume waits for that volume's driver, so it never runs under
/// the lock `settle` takes.
pub fn carry<J: Carried>(
    mut job: J,
    mut report: impl FnMut(&mut J) -> (bool, bool),
    settle: impl FnOnce(Ending, J::Remains) -> bool,
    mut nudge: impl FnMut(),
) {
    let ending = loop {
        let step = job.step();
        let (owed, cancelled) = report(&mut job);
        if owed {
            nudge();
        }
        match step {
            Step::Working if cancelled => {
                job.abandon();
                break Ending::Cancelled;
            }
            Step::Working => {}
            Step::Done => break Ending::Completed,
            Step::Stopped(reason) => break Ending::Stopped(reason),
        }
    };
    let remains = job.remains();
    drop(job);
    if settle(ending, remains) {
        nudge();
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
    /// Whether it waits for an operation asked before it on a volume they
    /// share.
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

/// An operation waiting, and the lanes of the volumes it touches.
struct Waiting<J> {
    id: u64,
    job: J,
    lanes: Vec<Lane>,
}

/// An operation a worker is carrying out.
struct Running {
    turn: u64,
    id: u64,
    lanes: Vec<Lane>,
    reading: Reading,
    current: String,
    cancel: bool,
    /// Its window closed, so its answer belongs to nobody.
    orphaned: bool,
}

/// The run of one operation, by which its worker reports on it: apart from
/// its id, which names its window, so a window opened under an id another's
/// operation still runs under can never be answered with that one's run.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Turn(u64);

/// The operations waiting, those running, and the answers not yet collected.
pub struct OperationDesk<J> {
    waiting: VecDeque<Waiting<J>>,
    running: Vec<Running>,
    landed: Vec<(u64, Finished)>,
    /// The turns handed out, so each run is told apart.
    turns: u64,
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
            running: Vec::new(),
            landed: Vec::new(),
            turns: 0,
            nudged: false,
            stopping: false,
            capacity,
        }
    }

    /// Ask for `job` to run as operation `id`, touching the volumes whose
    /// lanes are `lanes`.
    ///
    /// # Errors
    ///
    /// The job, handed back with why it was not taken.
    pub fn submit(&mut self, id: u64, job: J, lanes: Vec<Lane>) -> Result<(), (J, Refusal)> {
        if self.stopping {
            return Err((job, Refusal::Stopping));
        }
        if self.holds(id) {
            return Err((job, Refusal::Busy));
        }
        // Room for this job to run and for every answer still to come, so
        // starting one and recording an answer later never allocate.
        let held = self.waiting.len() + self.running.len() + 1;
        if self.waiting.len() >= self.capacity
            || self.waiting.try_reserve(1).is_err()
            || self.running.try_reserve(self.waiting.len() + 1).is_err()
            || self.landed.try_reserve(held).is_err()
        {
            return Err((job, Refusal::Full));
        }
        self.waiting.push_back(Waiting { id, job, lanes });
        Ok(())
    }

    /// Whether operation `id` is waiting, running, or answered and not
    /// collected.
    #[must_use]
    pub fn holds(&self, id: u64) -> bool {
        self.waiting.iter().any(|held| held.id == id)
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
            .any(|held| held.id == id)
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
        if let Some(at) = self.waiting.iter().position(|held| held.id == id) {
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
        self.waiting.retain(|held| held.id != id);
        if let Some(running) = self.running_for_mut(id) {
            running.cancel = true;
            running.orphaned = true;
        }
        self.landed.retain(|(held, _)| *held != id);
    }

    /// Send the workers away: nothing waiting runs, and what runs stops at its
    /// next step.
    pub fn stop(&mut self) {
        self.stopping = true;
        self.waiting.clear();
        for running in &mut self.running {
            running.cancel = true;
        }
    }

    /// Whether the workers have been sent away.
    #[must_use]
    pub const fn stopping(&self) -> bool {
        self.stopping
    }

    /// A worker's next operation and its turn: the oldest whose volumes no
    /// operation running touches, nor one asked before it and still waiting,
    /// so an operation never overtakes an earlier one on a volume they share.
    /// None once the desk is stopping.
    pub fn next_job(&mut self) -> Option<(Turn, u64, J)> {
        if self.stopping {
            return None;
        }
        let at = (0..self.waiting.len()).find(|&at| self.may_start(at))?;
        let Waiting { id, job, lanes } = self.waiting.remove(at)?;
        self.turns = self.turns.wrapping_add(1);
        self.running.push(Running {
            turn: self.turns,
            id,
            lanes,
            reading: Reading::default(),
            current: String::new(),
            cancel: false,
            orphaned: false,
        });
        Some((Turn(self.turns), id, job))
    }

    /// Whether the operation waiting at `at` touches no volume one running, or
    /// one waiting ahead of it, touches.
    fn may_start(&self, at: usize) -> bool {
        let lanes = &self.waiting[at].lanes;
        let shares = |other: &[Lane]| other.iter().any(|lane| lanes.contains(lane));
        !self.running.iter().any(|running| shares(&running.lanes))
            && !self
                .waiting
                .iter()
                .take(at)
                .any(|earlier| shares(&earlier.lanes))
    }

    /// Whether the operation run as `turn` should stop before its next step.
    #[must_use]
    pub fn cancelled(&self, turn: Turn) -> bool {
        self.stopping || self.run(turn).is_none_or(|running| running.cancel)
    }

    /// The operation run as `turn` says `reading` of itself, working on
    /// `current`, answering whether the loop is owed a wake to show it.
    pub fn report(&mut self, turn: Turn, reading: Reading, current: &str) -> bool {
        let Some(running) = self
            .running
            .iter_mut()
            .find(|running| running.turn == turn.0)
        else {
            return false;
        };
        running.reading = reading;
        if running.current != current {
            running.current.clear();
            running.current.push_str(current);
        }
        let shown = !running.orphaned;
        shown && self.owe()
    }

    /// The operation run as `turn` ended as `ending`, its copies having left
    /// `omissions`, answering whether the loop is owed a wake to collect it.
    pub fn finish(&mut self, turn: Turn, ending: Ending, omissions: Omissions) -> bool {
        let Some(at) = self
            .running
            .iter()
            .position(|running| running.turn == turn.0)
        else {
            return false;
        };
        let running = self.running.swap_remove(at);
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

    fn run(&self, turn: Turn) -> Option<&Running> {
        self.running.iter().find(|running| running.turn == turn.0)
    }

    fn running_for(&self, id: u64) -> Option<&Running> {
        self.running
            .iter()
            .find(|running| running.id == id && !running.orphaned)
    }

    fn running_for_mut(&mut self, id: u64) -> Option<&mut Running> {
        self.running
            .iter_mut()
            .find(|running| running.id == id && !running.orphaned)
    }
}

#[cfg(test)]
#[path = "operations_tests.rs"]
mod tests;
