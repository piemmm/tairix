use super::*;

fn desk() -> OperationDesk<&'static str> {
    OperationDesk::new(3)
}

/// The lanes of the volumes numbered `volumes`.
fn on(volumes: &[u8]) -> Vec<Lane> {
    volumes
        .iter()
        .map(|volume| Lane::Volume([*volume; 16]))
        .collect()
}

/// Start the next operation, answering its turn and id.
fn start(desk: &mut OperationDesk<&'static str>) -> (Turn, u64) {
    let (turn, id, _) = desk.next_job().expect("an operation to start");
    (turn, id)
}

fn ended(items: u64, ending: Ending) -> Finished {
    Finished {
        done: Tally { items, bytes: 0 },
        ending,
        omissions: Omissions::default(),
    }
}

/// A walk's reading of `items` done, counted whole or not.
fn items(items: u64) -> Reading {
    Reading {
        done: Tally { items, bytes: 0 },
        ..Reading::default()
    }
}

#[test]
fn operations_on_one_volume_run_one_at_a_time_in_the_order_asked() {
    let mut desk = desk();
    desk.submit(1, "copy", on(&[1])).expect("taken");
    desk.submit(2, "delete", on(&[1])).expect("taken");
    let (turn, id) = start(&mut desk);
    assert_eq!(id, 1);
    assert!(desk.next_job().is_none(), "one runs at a time on a volume");
    assert_eq!(
        desk.standing(2),
        Some(Standing {
            waiting: true,
            ..Standing::default()
        }),
        "the second waits"
    );
    desk.finish(turn, Ending::Completed, Omissions::default());
    assert_eq!(start(&mut desk).1, 2);
}

/// A copy onto one device holds up nothing on another: operations touching no
/// volume in common run side by side.
#[test]
fn operations_on_volumes_apart_run_side_by_side() {
    let mut desk = desk();
    desk.submit(1, "copy to the stick", on(&[1, 3]))
        .expect("taken");
    desk.submit(2, "delete at home", on(&[2])).expect("taken");
    assert_eq!(start(&mut desk).1, 1);
    assert_eq!(start(&mut desk).1, 2, "the home volume is not the stick's");
    assert_eq!(
        desk.standing(2).map(|standing| standing.waiting),
        Some(false)
    );
}

/// An operation waiting for a volume holds it for those asked after it, so a
/// stream of later operations can never keep an earlier one waiting.
#[test]
fn an_operation_never_overtakes_an_earlier_one_on_a_volume_they_share() {
    let mut desk = OperationDesk::new(4);
    desk.submit(1, "a", on(&[1])).expect("taken");
    desk.submit(2, "b", on(&[1, 2])).expect("taken");
    desk.submit(3, "c", on(&[2])).expect("taken");
    desk.submit(4, "d", on(&[3])).expect("taken");
    let (a, _) = start(&mut desk);
    assert_eq!(
        start(&mut desk).1,
        4,
        "c waits behind b on volume 2, though nothing runs there"
    );
    assert!(desk.next_job().is_none());
    desk.finish(a, Ending::Completed, Omissions::default());
    let (b, id) = start(&mut desk);
    assert_eq!(id, 2);
    assert!(desk.next_job().is_none(), "b runs on volume 2 now");
    desk.finish(b, Ending::Completed, Omissions::default());
    assert_eq!(start(&mut desk).1, 3);
}

#[test]
fn an_answer_carries_back_what_its_copies_could_not_keep() {
    let mut desk = desk();
    desk.submit(1, "copy", on(&[1])).expect("taken");
    let (turn, _) = start(&mut desk);
    let mut omissions = Omissions::default();
    omissions.note(&[(
        String::from("acorn.filetype"),
        tairix_fscopy::Refusal::Unsupported,
    )]);
    desk.finish(turn, Ending::Completed, omissions.clone());
    let answer = desk.take(1).expect("landed");
    assert_eq!(answer.ending, Ending::Completed);
    assert_eq!(answer.omissions, omissions);
}

#[test]
fn an_answer_reaches_only_the_operation_it_belongs_to() {
    let mut desk = desk();
    desk.submit(1, "copy", on(&[1])).expect("taken");
    desk.submit(2, "delete", on(&[2])).expect("taken");
    let (first, _) = start(&mut desk);
    let (second, _) = start(&mut desk);
    assert!(desk.report(first, items(4), ""));
    desk.finish(
        first,
        Ending::Stopped(String::from("the destination is full")),
        Omissions::default(),
    );
    assert_eq!(desk.take(2), None);
    assert_eq!(
        desk.take(1),
        Some(ended(
            4,
            Ending::Stopped(String::from("the destination is full"))
        ))
    );
    assert_eq!(desk.take(1), None, "taken once");
    assert!(!desk.cancelled(second), "the other runs on");
}

#[test]
fn a_cancel_stops_a_running_operation_before_its_next_step() {
    let mut desk = desk();
    desk.submit(1, "copy", on(&[1])).expect("taken");
    desk.submit(2, "delete", on(&[2])).expect("taken");
    let (turn, _) = start(&mut desk);
    let (other, _) = start(&mut desk);
    assert!(!desk.cancelled(turn));
    desk.cancel(1);
    assert!(desk.cancelled(turn));
    assert!(
        !desk.cancelled(other),
        "a cancel reaches its own operation alone"
    );
    assert_eq!(
        desk.standing(1),
        Some(Standing {
            cancelling: true,
            ..Standing::default()
        })
    );
    desk.finish(turn, Ending::Cancelled, Omissions::default());
    assert_eq!(desk.take(1), Some(ended(0, Ending::Cancelled)));
}

#[test]
fn a_cancelled_waiting_operation_ends_at_once_having_done_nothing() {
    let mut desk = desk();
    desk.submit(1, "copy", on(&[1])).expect("taken");
    desk.submit(2, "delete", on(&[1])).expect("taken");
    let (turn, _) = start(&mut desk);
    desk.cancel(2);
    assert_eq!(desk.take(2), Some(ended(0, Ending::Cancelled)));
    desk.finish(turn, Ending::Completed, Omissions::default());
    assert!(desk.next_job().is_none(), "the cancelled one never runs");
}

#[test]
fn a_forgotten_operation_stops_and_its_answer_is_dropped() {
    let mut desk = desk();
    desk.submit(1, "copy", on(&[1])).expect("taken");
    desk.submit(2, "delete", on(&[1])).expect("taken");
    let (turn, _) = start(&mut desk);
    desk.forget(1);
    desk.forget(2);
    assert!(desk.cancelled(turn), "the running one is told to stop");
    assert_eq!(desk.standing(1), None);
    assert!(
        !desk.report(turn, items(3), ""),
        "nobody is owed a wake for it"
    );
    assert!(!desk.finish(turn, Ending::Cancelled, Omissions::default()));
    assert_eq!(desk.take(1), None);
    assert!(
        desk.next_job().is_none(),
        "a forgotten waiting job never runs"
    );
    assert!(!desk.holds(1) && !desk.holds(2));
}

/// A run is reported by its turn, never by its window's id: a window reopened
/// under the id of one whose operation still runs, cancelled, is answered by
/// its own operation alone, which waits for the abandoned one's volume.
#[test]
fn a_run_is_told_apart_from_a_later_operation_under_its_windows_id() {
    let mut desk = desk();
    desk.submit(1, "copy", on(&[1])).expect("taken");
    let (abandoned, _) = start(&mut desk);
    desk.forget(1);
    desk.submit(1, "again", on(&[1]))
        .expect("the id is free again");
    assert!(
        desk.next_job().is_none(),
        "the abandoned run still works on the volume"
    );
    assert!(
        !desk.report(abandoned, items(2), ""),
        "nobody is owed its reading"
    );
    assert!(!desk.finish(abandoned, Ending::Cancelled, Omissions::default()));
    assert_eq!(desk.take(1), None, "the abandoned run answers nobody");
    let (turn, id, job) = desk.next_job().expect("the new operation");
    assert_eq!((id, job), (1, "again"));
    assert_ne!(turn, abandoned);
    assert!(desk.report(turn, items(1), ""));
    assert_eq!(
        desk.standing(1).map(|standing| standing.reading),
        Some(items(1))
    );
}

#[test]
fn an_id_is_held_once_and_a_full_desk_refuses_with_its_reason() {
    let mut desk = desk();
    desk.submit(1, "copy", on(&[1])).expect("taken");
    assert_eq!(
        desk.submit(1, "again", on(&[1])),
        Err(("again", Refusal::Busy))
    );
    desk.submit(2, "b", on(&[1])).expect("taken");
    desk.submit(3, "c", on(&[1])).expect("taken");
    assert_eq!(desk.submit(4, "d", on(&[1])), Err(("d", Refusal::Full)));
    let _ = start(&mut desk);
    desk.submit(4, "d", on(&[1])).expect("room once one runs");
}

#[test]
fn an_answer_waiting_uncollected_still_holds_its_id() {
    let mut desk = desk();
    desk.submit(1, "copy", on(&[1])).expect("taken");
    let (turn, _) = start(&mut desk);
    desk.finish(turn, Ending::Completed, Omissions::default());
    assert_eq!(
        desk.submit(1, "again", on(&[1])),
        Err(("again", Refusal::Busy))
    );
    let _ = desk.take(1);
    desk.submit(1, "again", on(&[1]))
        .expect("taken once the answer is collected");
}

#[test]
fn a_run_of_reports_owes_one_wake_until_the_loop_looks() {
    let mut desk = desk();
    desk.submit(1, "copy", on(&[1])).expect("taken");
    desk.submit(2, "next", on(&[2])).expect("taken");
    let (first, _) = start(&mut desk);
    let (second, _) = start(&mut desk);
    assert!(desk.report(first, items(1), ""));
    assert!(!desk.report(first, items(2), ""));
    assert!(
        !desk.report(second, items(1), ""),
        "one wake shows every operation's reading"
    );
    assert!(
        !desk.finish(first, Ending::Completed, Omissions::default()),
        "already owed one"
    );
    desk.acknowledge();
    assert!(
        desk.report(second, items(2), ""),
        "owed again once the loop has looked"
    );
}

#[test]
fn a_held_desk_owes_no_wake_until_the_loop_looks_again() {
    let mut desk = desk();
    desk.submit(1, "copy", on(&[1])).expect("taken");
    let (turn, _) = start(&mut desk);
    desk.hold();
    assert!(!desk.report(turn, items(1), ""));
    assert!(!desk.finish(turn, Ending::Completed, Omissions::default()));
    desk.acknowledge();
    desk.submit(2, "next", on(&[1])).expect("taken");
    let (turn, _) = start(&mut desk);
    assert!(desk.report(turn, items(1), ""));
}

#[test]
fn a_stopping_desk_takes_nothing_and_stops_what_runs() {
    let mut desk = desk();
    desk.submit(1, "copy", on(&[1])).expect("taken");
    desk.submit(2, "delete", on(&[2])).expect("taken");
    desk.submit(3, "move", on(&[1])).expect("taken");
    let (first, _) = start(&mut desk);
    let (second, _) = start(&mut desk);
    desk.stop();
    assert!(desk.stopping());
    assert!(desk.cancelled(first) && desk.cancelled(second));
    assert!(desk.next_job().is_none());
    assert_eq!(
        desk.submit(4, "late", on(&[3])),
        Err(("late", Refusal::Stopping))
    );
}

/// What a job asks about a name already taken.
fn conflict() -> Conflict {
    Conflict {
        source: String::from("/a/notes.txt"),
        target: String::from("/b/notes.txt"),
        occupant: tairix_fscopy::tree::Kind::File,
    }
}

/// With no worker an operation runs a slice a turn on the loop: it stops when
/// the slice is spent, still working, or when it asks, and a cancel stops it
/// at a unit's end.
#[test]
fn an_operation_run_on_the_loop_runs_a_slice_at_a_time() {
    let mut left = 5;
    let mut step = || {
        left -= 1;
        if left == 0 {
            Step::Done
        } else {
            Step::Working
        }
    };
    let mut units = 0;
    let sliced = run_slice(
        &mut step,
        || false,
        || {
            units += 1;
            units >= 2
        },
    );
    assert_eq!(sliced, Sliced::Spent, "the slice spent, the work goes on");
    assert_eq!(
        run_slice(&mut step, || false, || false),
        Sliced::Ended(Ending::Completed)
    );
    assert_eq!(
        run_slice(|| Step::Working, || true, || false),
        Sliced::Ended(Ending::Cancelled),
        "a cancel is taken before the next unit"
    );
    assert_eq!(
        run_slice(
            || Step::Stopped(String::from("refused")),
            || false,
            || false
        ),
        Sliced::Ended(Ending::Stopped(String::from("refused")))
    );
    assert_eq!(
        run_slice(|| Step::Asks(conflict()), || false, || false),
        Sliced::Asks(conflict()),
        "a question ends the slice, its place held"
    );
}

/// An operation that asks holds no worker but keeps its volumes, so nothing
/// overtakes it on them while one elsewhere runs; answered, it resumes on its
/// own turn before anything waiting.
#[test]
fn an_asking_operation_holds_no_worker_but_keeps_its_volumes_and_resumes_first() {
    let mut desk = OperationDesk::new(4);
    desk.submit(1, "copy", on(&[1])).expect("taken");
    desk.submit(2, "after it", on(&[1])).expect("taken");
    desk.submit(3, "elsewhere", on(&[2])).expect("taken");
    let (turn, id, job) = desk.next_job().expect("the copy");
    assert_eq!(id, 1);
    assert_eq!(desk.ask(turn, job, conflict()), Ok(true));
    assert_eq!(
        desk.standing(1).and_then(|standing| standing.asking),
        Some(&conflict())
    );
    assert_eq!(
        start(&mut desk).1,
        3,
        "its worker is free for another volume's"
    );
    assert!(
        desk.next_job().is_none(),
        "nothing overtakes it on its volume"
    );
    desk.submit(4, "later elsewhere", on(&[3])).expect("taken");
    assert!(desk.answer(1, |job| *job = "copy, answered"));
    assert!(!desk.answer(1, |_| {}), "answered once");
    assert_eq!(desk.standing(1).and_then(|standing| standing.asking), None);
    assert_eq!(
        desk.next_job(),
        Some((turn, 1, "copy, answered")),
        "it resumes first, on its own turn"
    );
    desk.finish(turn, Ending::Completed, Omissions::default());
    assert_eq!(start(&mut desk).1, 2);
}

/// A cancel, a closed window or a stop while an operation asks hands it to a
/// worker to stop, and no answer reaches it afterwards; an ask after a cancel
/// is handed straight back.
#[test]
fn an_asking_operation_is_handed_to_a_worker_to_stop() {
    let mut desk = desk();
    desk.submit(1, "copy", on(&[1])).expect("taken");
    desk.submit(2, "move", on(&[2])).expect("taken");
    desk.submit(3, "delete", on(&[3])).expect("taken");
    let (first, _, job) = desk.next_job().expect("copy");
    assert!(desk.ask(first, job, conflict()).is_ok());
    let (second, _, job) = desk.next_job().expect("move");
    assert!(desk.ask(second, job, conflict()).is_ok());
    assert!(desk.cancel(1), "a worker is owed the stop");
    assert_eq!(
        desk.standing(1)
            .map(|standing| (standing.cancelling, standing.asking)),
        Some((true, None))
    );
    assert!(
        !desk.answer(1, |_| {}),
        "a cancelled question takes no answer"
    );
    let (turn, id, _) = desk.next_job().expect("handed out to be stopped");
    assert_eq!((turn, id), (first, 1));
    assert!(desk.cancelled(turn));
    desk.finish(turn, Ending::Cancelled, Omissions::default());
    assert_eq!(desk.take(1), Some(ended(0, Ending::Cancelled)));

    assert!(desk.forget(2), "its window closed");
    let (turn, _, _) = desk.next_job().expect("handed out to be stopped");
    assert_eq!(turn, second);
    assert!(desk.cancelled(turn));
    assert!(
        !desk.finish(turn, Ending::Cancelled, Omissions::default()),
        "its answer belongs to nobody"
    );
    assert!(!desk.holds(2));

    let (third, _, job) = desk.next_job().expect("delete");
    desk.stop();
    assert_eq!(desk.ask(third, job, conflict()), Err("delete"));

    let mut desk = OperationDesk::new(3);
    desk.submit(1, "copy", on(&[1])).expect("taken");
    let (turn, _, job) = desk.next_job().expect("copy");
    assert!(desk.ask(turn, job, conflict()).is_ok());
    desk.stop();
    assert_eq!(
        desk.next_job().map(|(stopped, _, _)| stopped),
        Some(turn),
        "a stopping desk still hands out what asks, to be stopped"
    );
    assert!(desk.cancelled(turn));
    assert!(desk.next_job().is_none());
}

/// A worker's job of `units` units ending as `last` does, recording in `log`
/// what is done with it.
struct Logged<'a> {
    units: usize,
    last: Step,
    log: &'a core::cell::RefCell<Vec<&'static str>>,
}

impl Carried for Logged<'_> {
    type Remains = &'static str;

    fn step(&mut self) -> Step {
        self.log.borrow_mut().push("step");
        self.units -= 1;
        if self.units == 0 {
            self.last.clone()
        } else {
            Step::Working
        }
    }

    fn abandon(&mut self) {
        self.log.borrow_mut().push("abandon");
    }

    fn remains(&mut self) -> &'static str {
        "remains"
    }
}

impl Drop for Logged<'_> {
    fn drop(&mut self) {
        self.log.borrow_mut().push("closed");
    }
}

/// The desk a test worker reports to: a wake owed on the first report, a
/// cancel asked from report `cancel_at` — before the first unit at 0 — and a
/// question parked where `parks` says, else handed back.
struct Desk<'a> {
    log: &'a core::cell::RefCell<Vec<&'static str>>,
    reported: usize,
    cancel_at: usize,
    parks: bool,
    parked: Option<Logged<'a>>,
    settled: Option<Ending>,
}

impl<'a> Carrier<Logged<'a>> for Desk<'a> {
    fn cancelled(&mut self) -> bool {
        self.cancel_at == 0
    }

    fn report(&mut self, _: &mut Logged<'a>) -> (bool, bool) {
        self.reported += 1;
        (self.reported == 1, self.reported >= self.cancel_at)
    }

    fn ask(&mut self, job: Logged<'a>, _: Conflict) -> Result<bool, Logged<'a>> {
        if !self.parks {
            return Err(job);
        }
        self.log.borrow_mut().push("parked");
        self.parked = Some(job);
        Ok(true)
    }

    fn settle(&mut self, ending: Ending, remains: &'static str) -> bool {
        assert_eq!(remains, "remains");
        self.log.borrow_mut().push("settled");
        self.settled = Some(ending);
        true
    }

    fn nudge(&mut self) {
        self.log.borrow_mut().push("nudge");
    }
}

/// Carry a job of `units` units ending as `last` on a worker whose desk asks
/// a cancel from report `cancel_at` and parks a question where `parks` says:
/// what the worker did, in order, and how the job ended, if it did.
fn carried(
    units: usize,
    last: Step,
    (cancel_at, parks): (usize, bool),
) -> (Vec<&'static str>, Option<Ending>) {
    let log = core::cell::RefCell::new(Vec::new());
    let mut desk = Desk {
        log: &log,
        reported: 0,
        cancel_at,
        parks,
        parked: None,
        settled: None,
    };
    carry(
        Logged {
            units,
            last,
            log: &log,
        },
        &mut desk,
    );
    let done = log.borrow().clone();
    (done, desk.settled.take())
}

/// A worker's job is dropped, closing its files, before its answer is
/// settled under the lock the loop and every reader share.
#[test]
fn a_carried_operations_files_close_before_its_answer_is_settled() {
    assert_eq!(
        carried(2, Step::Done, (usize::MAX, true)),
        (
            vec!["step", "nudge", "step", "closed", "settled", "nudge"],
            Some(Ending::Completed)
        )
    );
    assert_eq!(
        carried(3, Step::Working, (2, true)),
        (
            vec!["step", "nudge", "step", "abandon", "closed", "settled", "nudge"],
            Some(Ending::Cancelled)
        ),
        "a cancel is honoured at the end of the unit it is reported after"
    );
    assert_eq!(
        carried(
            1,
            Step::Stopped(String::from("refused")),
            (usize::MAX, true)
        ),
        (
            vec!["step", "nudge", "closed", "settled", "nudge"],
            Some(Ending::Stopped(String::from("refused")))
        ),
        "a refusal abandons nothing"
    );
    assert_eq!(
        carried(2, Step::Done, (0, true)),
        (
            vec!["abandon", "closed", "settled", "nudge"],
            Some(Ending::Cancelled)
        ),
        "one stopped before its first unit, resumed only to stop, does nothing"
    );
}

/// A job that asks is parked on the desk and the worker is free: it neither
/// closes the job nor settles an answer. Handed back, a cancel having come
/// first, it is stopped as any cancel stops it.
#[test]
fn a_carried_job_that_asks_is_parked_holding_no_worker() {
    assert_eq!(
        carried(2, Step::Asks(conflict()), (usize::MAX, true)),
        (vec!["step", "nudge", "step", "parked", "nudge"], None)
    );
    for refused in [(usize::MAX, false), (1, true)] {
        assert_eq!(
            carried(1, Step::Asks(conflict()), refused),
            (
                vec!["step", "nudge", "abandon", "closed", "settled", "nudge"],
                Some(Ending::Cancelled)
            ),
            "{refused:?}"
        );
    }
}

/// The window shows the walk's own figures and the item in hand, and the
/// answer carries what was done.
#[test]
fn a_report_carries_the_reading_and_the_item_in_hand() {
    let mut desk = desk();
    desk.submit(1, "copy", on(&[1])).expect("taken");
    let (turn, _) = start(&mut desk);
    let reading = Reading {
        done: Tally {
            items: 3,
            bytes: 300,
        },
        total: Some(Tally {
            items: 9,
            bytes: 900,
        }),
        ..Reading::default()
    };
    assert!(desk.report(turn, reading, "two.jpg"));
    let standing = desk.standing(1).expect("running");
    assert_eq!(standing.reading, reading);
    assert_eq!(standing.current, "two.jpg");
    desk.finish(turn, Ending::Completed, Omissions::default());
    assert_eq!(
        desk.take(1).map(|finished| finished.done),
        Some(Tally {
            items: 3,
            bytes: 300
        })
    );
}
