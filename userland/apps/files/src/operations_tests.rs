use super::*;

fn desk() -> OperationDesk<&'static str> {
    OperationDesk::new(3)
}

fn ended(items: u64, ending: Ending) -> Finished {
    Finished {
        done: Tally { items, bytes: 0 },
        ending,
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
fn operations_run_one_at_a_time_in_the_order_asked() {
    let mut desk = desk();
    desk.submit(1, "copy").expect("taken");
    desk.submit(2, "delete").expect("taken");
    assert_eq!(desk.next_job(), Some((1, "copy")));
    assert_eq!(desk.next_job(), None, "one runs at a time");
    assert_eq!(
        desk.standing(2),
        Some(Standing {
            waiting: true,
            ..Standing::default()
        }),
        "the second waits"
    );
    desk.finish(Ending::Completed);
    assert_eq!(desk.next_job(), Some((2, "delete")));
}

#[test]
fn an_answer_reaches_only_the_operation_it_belongs_to() {
    let mut desk = desk();
    desk.submit(1, "copy").expect("taken");
    desk.submit(2, "delete").expect("taken");
    let _ = desk.next_job();
    assert!(desk.report(items(4), ""));
    desk.finish(Ending::Stopped(String::from("the destination is full")));
    assert_eq!(desk.take(2), None);
    assert_eq!(
        desk.take(1),
        Some(ended(
            4,
            Ending::Stopped(String::from("the destination is full"))
        ))
    );
    assert_eq!(desk.take(1), None, "taken once");
}

#[test]
fn a_cancel_stops_a_running_operation_before_its_next_step() {
    let mut desk = desk();
    desk.submit(1, "copy").expect("taken");
    let _ = desk.next_job();
    assert!(!desk.cancelled());
    desk.cancel(1);
    assert!(desk.cancelled());
    assert_eq!(
        desk.standing(1),
        Some(Standing {
            cancelling: true,
            ..Standing::default()
        })
    );
    desk.finish(Ending::Cancelled);
    assert_eq!(desk.take(1), Some(ended(0, Ending::Cancelled)));
}

#[test]
fn a_cancelled_waiting_operation_ends_at_once_having_done_nothing() {
    let mut desk = desk();
    desk.submit(1, "copy").expect("taken");
    desk.submit(2, "delete").expect("taken");
    let _ = desk.next_job();
    desk.cancel(2);
    assert_eq!(desk.take(2), Some(ended(0, Ending::Cancelled)));
    desk.finish(Ending::Completed);
    assert_eq!(desk.next_job(), None, "the cancelled one never runs");
}

#[test]
fn a_forgotten_operation_stops_and_its_answer_is_dropped() {
    let mut desk = desk();
    desk.submit(1, "copy").expect("taken");
    desk.submit(2, "delete").expect("taken");
    let _ = desk.next_job();
    desk.forget(1);
    desk.forget(2);
    assert!(desk.cancelled(), "the running one is told to stop");
    assert_eq!(desk.standing(1), None);
    assert!(!desk.report(items(3), ""), "nobody is owed a wake for it");
    assert!(!desk.finish(Ending::Cancelled));
    assert_eq!(desk.take(1), None);
    assert_eq!(desk.next_job(), None, "a forgotten waiting job never runs");
    assert!(!desk.holds(1) && !desk.holds(2));
}

#[test]
fn an_id_is_held_once_and_a_full_desk_refuses_with_its_reason() {
    let mut desk = desk();
    desk.submit(1, "copy").expect("taken");
    assert_eq!(desk.submit(1, "again"), Err(("again", Refusal::Busy)));
    desk.submit(2, "b").expect("taken");
    desk.submit(3, "c").expect("taken");
    assert_eq!(desk.submit(4, "d"), Err(("d", Refusal::Full)));
    let _ = desk.next_job();
    desk.submit(4, "d").expect("room once one runs");
}

#[test]
fn an_answer_waiting_uncollected_still_holds_its_id() {
    let mut desk = desk();
    desk.submit(1, "copy").expect("taken");
    let _ = desk.next_job();
    desk.finish(Ending::Completed);
    assert_eq!(desk.submit(1, "again"), Err(("again", Refusal::Busy)));
    let _ = desk.take(1);
    desk.submit(1, "again")
        .expect("taken once the answer is collected");
}

#[test]
fn a_run_of_reports_owes_one_wake_until_the_loop_looks() {
    let mut desk = desk();
    desk.submit(1, "copy").expect("taken");
    let _ = desk.next_job();
    assert!(desk.report(items(1), ""));
    assert!(!desk.report(items(2), ""));
    assert!(!desk.finish(Ending::Completed), "already owed one");
    desk.acknowledge();
    desk.submit(2, "next").expect("taken");
    let _ = desk.next_job();
    assert!(
        desk.report(items(1), ""),
        "owed again once the loop has looked"
    );
}

#[test]
fn a_held_desk_owes_no_wake_until_the_loop_looks_again() {
    let mut desk = desk();
    desk.submit(1, "copy").expect("taken");
    let _ = desk.next_job();
    desk.hold();
    assert!(!desk.report(items(1), ""));
    assert!(!desk.finish(Ending::Completed));
    desk.acknowledge();
    desk.submit(2, "next").expect("taken");
    let _ = desk.next_job();
    assert!(desk.report(items(1), ""));
}

#[test]
fn a_stopping_desk_takes_nothing_and_stops_what_runs() {
    let mut desk = desk();
    desk.submit(1, "copy").expect("taken");
    desk.submit(2, "delete").expect("taken");
    let _ = desk.next_job();
    desk.stop();
    assert!(desk.stopping());
    assert!(desk.cancelled());
    assert_eq!(desk.next_job(), None);
    assert_eq!(desk.submit(3, "late"), Err(("late", Refusal::Stopping)));
}

/// With no worker an operation runs a slice a turn on the loop: it stops when
/// the slice is spent, still working, and a cancel stops it at a unit's end.
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
    let ending = run_slice(
        &mut step,
        || false,
        || {
            units += 1;
            units >= 2
        },
    );
    assert_eq!(ending, None, "the slice spent, the work goes on");
    assert_eq!(
        run_slice(&mut step, || false, || false),
        Some(Ending::Completed)
    );
    assert_eq!(
        run_slice(|| Step::Working, || true, || false),
        Some(Ending::Cancelled),
        "a cancel is taken before the next unit"
    );
    assert_eq!(
        run_slice(
            || Step::Stopped(String::from("refused")),
            || false,
            || false
        ),
        Some(Ending::Stopped(String::from("refused")))
    );
}

/// The window shows the walk's own figures and the item in hand, and the
/// answer carries what was done.
#[test]
fn a_report_carries_the_reading_and_the_item_in_hand() {
    let mut desk = desk();
    desk.submit(1, "copy").expect("taken");
    let _ = desk.next_job();
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
    assert!(desk.report(reading, "two.jpg"));
    let standing = desk.standing(1).expect("running");
    assert_eq!(standing.reading, reading);
    assert_eq!(standing.current, "two.jpg");
    desk.finish(Ending::Completed);
    assert_eq!(
        desk.take(1).map(|finished| finished.done),
        Some(Tally {
            items: 3,
            bytes: 300
        })
    );
}
