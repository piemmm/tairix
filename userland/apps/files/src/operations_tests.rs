use super::*;

fn desk() -> OperationDesk<&'static str> {
    OperationDesk::new(3)
}

fn ended(done: usize, ending: Ending) -> Finished {
    Finished { done, ending }
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
            done: 0,
            cancelling: false
        }),
        "the second waits"
    );
    desk.finish(Ending::Completed);
    assert_eq!(desk.next_job(), Some((2, "delete")));
}

#[test]
fn an_answer_reaches_only_the_window_that_asked() {
    let mut desk = desk();
    desk.submit(1, "copy").expect("taken");
    desk.submit(2, "delete").expect("taken");
    let _ = desk.next_job();
    assert!(desk.report(4));
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
            done: 0,
            cancelling: true
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
fn a_closed_windows_operation_stops_and_its_answer_is_dropped() {
    let mut desk = desk();
    desk.submit(1, "copy").expect("taken");
    desk.submit(2, "delete").expect("taken");
    let _ = desk.next_job();
    desk.forget(1);
    desk.forget(2);
    assert!(desk.cancelled(), "the running one is told to stop");
    assert_eq!(desk.standing(1), None);
    assert!(!desk.report(3), "nobody is owed a wake for it");
    assert!(!desk.finish(Ending::Cancelled));
    assert_eq!(desk.take(1), None);
    assert_eq!(
        desk.next_job(),
        None,
        "a closed window's waiting job never runs"
    );
    assert!(!desk.holds(1) && !desk.holds(2));
}

#[test]
fn a_window_holds_one_operation_and_a_full_desk_refuses_with_its_reason() {
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
fn a_window_whose_answer_waits_uncollected_still_holds_its_operation() {
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
    assert!(desk.report(1));
    assert!(!desk.report(2));
    assert!(!desk.finish(Ending::Completed), "already owed one");
    desk.acknowledge();
    desk.submit(2, "next").expect("taken");
    let _ = desk.next_job();
    assert!(desk.report(1), "owed again once the loop has looked");
}

#[test]
fn a_held_desk_owes_no_wake_until_the_loop_looks_again() {
    let mut desk = desk();
    desk.submit(1, "copy").expect("taken");
    let _ = desk.next_job();
    desk.hold();
    assert!(!desk.report(1));
    assert!(!desk.finish(Ending::Completed));
    desk.acknowledge();
    desk.submit(2, "next").expect("taken");
    let _ = desk.next_job();
    assert!(desk.report(1));
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
