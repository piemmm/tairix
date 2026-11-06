//! Host tests for a drop's steps ([`crate::landing`]).

use alloc::rc::Rc;
use alloc::vec::Vec;
use core::cell::RefCell;

use tairix_abi::window_ipc::{LandingName, LandingOutcome, LandingStep};
use tairix_abi::Errno;
use tairix_fscopy::op::{Conflict, Root};

use crate::landing::{outcome_of, DropLanding, LandingSink, OwedEnds};
use crate::operations::{carry, Carried, Carrier, Ending, OperationDesk, Step};

/// What a [`Recorder`] was sent, and when the desktop ends the drop.
#[derive(Default)]
struct Sent {
    steps: Vec<LandingStep>,
    owed: usize,
    /// The desktop refuses every step once this many are taken.
    taken_most: Option<usize>,
    /// The steps, numbered in the order they are tried, that never reach it.
    lost: Vec<usize>,
    tried: usize,
}

/// A sink recording every step and every end owed.
struct Recorder(Rc<RefCell<Sent>>);

impl LandingSink for Recorder {
    fn step(&mut self, step: &LandingStep) -> Result<(), Errno> {
        let mut sent = self.0.borrow_mut();
        let tried = sent.tried;
        sent.tried += 1;
        if sent.lost.contains(&tried) {
            return Err(Errno::LimitExceeded);
        }
        if sent.taken_most.is_some_and(|most| sent.steps.len() >= most) {
            return Err(Errno::NotFound);
        }
        sent.steps.push(*step);
        Ok(())
    }

    fn owe_end(&mut self) {
        self.0.borrow_mut().owed += 1;
    }
}

/// A drop's steps, and what they send.
fn recorded() -> (Rc<RefCell<Sent>>, DropLanding<Recorder>) {
    let sent = Rc::new(RefCell::new(Sent::default()));
    let landing = DropLanding::to(Recorder(Rc::clone(&sent)));
    (sent, landing)
}

fn making(item: u32, name: &str) -> LandingStep {
    LandingStep::Making {
        item,
        name: LandingName::new(name).expect("a valid name"),
    }
}

const fn made(item: u32, outcome: LandingOutcome) -> LandingStep {
    LandingStep::Made { item, outcome }
}

/// A job writing `items` items, one unit to begin each and one to finish it,
/// stepping the drop it lands as a paste does.
struct Copying {
    landing: DropLanding<Recorder>,
    items: u32,
    next: u32,
    open: bool,
}

impl Carried for Copying {
    type Remains = ();

    fn step(&mut self) -> Step {
        if self.open {
            self.landing.made(LandingOutcome::Created);
            self.open = false;
            self.next += 1;
            return Step::Working;
        }
        if self.next == self.items {
            self.landing.end();
            return Step::Done;
        }
        self.landing
            .making(self.next, &alloc::format!("{}.txt", self.next));
        self.open = true;
        Step::Working
    }

    fn abandon(&mut self) {
        self.landing.made(LandingOutcome::Failed);
        self.landing.end();
    }

    fn remains(&mut self) -> Self::Remains {}
}

/// A worker's desk asking a cancel from the first report where `cancelled`
/// says, owing no wake and taking no question.
struct Quiet {
    cancelled: bool,
}

impl Carrier<Copying> for Quiet {
    fn cancelled(&mut self) -> bool {
        false
    }

    fn report(&mut self, _: &mut Copying) -> (bool, bool) {
        (false, self.cancelled)
    }

    fn ask(&mut self, job: Copying, _: Conflict) -> Result<bool, Copying> {
        Err(job)
    }

    fn settle(&mut self, _: Ending, (): ()) -> bool {
        false
    }

    fn nudge(&mut self) {}
}

/// Each item is announced before it is written and reported once it is done,
/// and the drop ends once.
#[test]
fn a_drop_steps_each_item_around_its_transfer_and_ends_once() {
    let (sent, mut landing) = recorded();
    landing.making(0, "a.txt");
    landing.made(LandingOutcome::Created);
    landing.making(1, "b.txt");
    landing.made(LandingOutcome::Failed);
    landing.end();
    landing.end();
    drop(landing);
    let sent = sent.borrow();
    assert_eq!(
        sent.steps,
        [
            making(0, "a.txt"),
            made(0, LandingOutcome::Created),
            making(1, "b.txt"),
            made(1, LandingOutcome::Failed),
            LandingStep::Ended,
        ]
    );
    assert_eq!(sent.owed, 0, "an ended drop owes nothing");
}

/// A running job's drop ends with it: on completion, and on a cancel with the
/// item in hand failed.
#[test]
fn a_running_drop_ends_on_completion_and_on_cancel() {
    for cancelled in [false, true] {
        let (sent, landing) = recorded();
        let job = Copying {
            landing,
            items: 2,
            next: 0,
            open: false,
        };
        carry(job, &mut Quiet { cancelled });
        let ended = if cancelled {
            alloc::vec![
                making(0, "0.txt"),
                made(0, LandingOutcome::Failed),
                LandingStep::Ended,
            ]
        } else {
            alloc::vec![
                making(0, "0.txt"),
                made(0, LandingOutcome::Created),
                making(1, "1.txt"),
                made(1, LandingOutcome::Created),
                LandingStep::Ended,
            ]
        };
        assert_eq!(sent.borrow().steps, ended);
        assert_eq!(sent.borrow().owed, 0);
    }
}

/// A job's drop let go of before it ran owes its end however the job went:
/// cancelled, its window closed, the workers sent away, or never started.
#[test]
fn a_drop_let_go_before_its_job_ran_owes_its_end() {
    let sent = Rc::new(RefCell::new(Sent::default()));
    let landing = || DropLanding::to(Recorder(Rc::clone(&sent)));
    let mut desk = OperationDesk::new(4);
    for id in 1..=3 {
        assert!(desk.submit(id, landing(), Vec::new()).is_ok());
    }
    desk.cancel(1);
    assert_eq!(sent.borrow().owed, 1, "cancelled");
    desk.forget(2);
    assert_eq!(sent.borrow().owed, 2, "its window closed");
    desk.stop();
    assert_eq!(sent.borrow().owed, 3, "the workers sent away");
    drop(landing());
    assert_eq!(sent.borrow().owed, 4, "refused its start");
    assert!(sent.borrow().steps.is_empty());
}

/// Once the desktop refuses a step — it ended the drop — nothing more is sent
/// and nothing is owed.
#[test]
fn a_drop_the_desktop_ended_is_stepped_no_further() {
    let (sent, mut landing) = recorded();
    sent.borrow_mut().taken_most = Some(1);
    landing.making(0, "a.txt");
    landing.made(LandingOutcome::Created);
    landing.making(1, "b.txt");
    landing.end();
    drop(landing);
    assert_eq!(sent.borrow().steps, [making(0, "a.txt")]);
    assert_eq!(sent.borrow().owed, 0);
}

/// A step that never reached the desktop leaves its item to land unannounced,
/// and the drop is still ended; an end that could not be sent is owed.
#[test]
fn a_step_not_taken_leaves_its_item_and_the_drop_still_ends() {
    let (sent, mut landing) = recorded();
    sent.borrow_mut().lost = alloc::vec![0];
    landing.making(0, "a.txt");
    landing.made(LandingOutcome::Created);
    landing.making(1, "b.txt");
    landing.made(LandingOutcome::Created);
    landing.end();
    assert_eq!(
        sent.borrow().steps,
        [
            making(1, "b.txt"),
            made(1, LandingOutcome::Created),
            LandingStep::Ended,
        ]
    );
    assert_eq!(sent.borrow().owed, 0);

    let (sent, mut landing) = recorded();
    sent.borrow_mut().lost = alloc::vec![0];
    landing.end();
    drop(landing);
    assert!(sent.borrow().steps.is_empty());
    assert_eq!(sent.borrow().owed, 1, "an end lost on the way is owed once");
}

/// An end owed by a drop that kept room for it takes no memory to record, and
/// each is taken for posting once; a drop that ended owing nothing gives its
/// room back.
#[test]
fn an_end_owed_in_its_kept_room_takes_no_memory() {
    let mut ends = OwedEnds::default();
    for _ in 0..3 {
        ends.hold().expect("room for a drop's end");
    }
    let room = ends.owed.capacity();
    ends.owe((1, 7));
    ends.release();
    ends.owe((2, 8));
    assert_eq!(ends.owed.capacity(), room, "owing an end took memory");
    assert_eq!(ends.held, 0, "a drop's room was kept after it ended");
    let mut taken = alloc::vec![ends.take(), ends.take(), ends.take()];
    taken.sort_unstable();
    assert_eq!(taken, [None, Some((1, 7)), Some((2, 8))]);
}

/// What a transfer left under an item's name is what the desktop is told
/// became of it, and an item stopped before it settled its name failed.
#[test]
fn an_items_outcome_is_what_its_transfer_left_under_its_name() {
    assert_eq!(outcome_of(Some(Root::Created)), LandingOutcome::Created);
    assert_eq!(outcome_of(Some(Root::Replaced)), LandingOutcome::Replaced);
    assert_eq!(outcome_of(Some(Root::Merged)), LandingOutcome::Merged);
    assert_eq!(outcome_of(Some(Root::Skipped)), LandingOutcome::Skipped);
    assert_eq!(outcome_of(None), LandingOutcome::Failed);
}

/// A paste landing nowhere on the desktop steps nothing, and a name no file
/// can have is not announced.
#[test]
fn no_drop_and_no_name_step_nothing() {
    let mut none = DropLanding::<Recorder>::none();
    none.making(0, "a.txt");
    none.end();
    drop(none);
    let (sent, mut landing) = recorded();
    landing.making(0, "");
    landing.made(LandingOutcome::Created);
    landing.end();
    assert_eq!(sent.borrow().steps, [LandingStep::Ended]);
}
