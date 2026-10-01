use super::{JobDesk, JobQueue};

/// A desk the loop has not touched owes nobody anything.
#[test]
fn a_fresh_desk_has_no_work_and_no_answer() {
    let mut desk = JobDesk::<u32, u32>::new();
    assert!(!desk.has_work());
    assert!(!desk.in_flight());
    assert_eq!(desk.next_job(), None);
    assert_eq!(desk.collect(), None);
}

/// The first submission is takeable, so the worker is worth waking.
#[test]
fn submitting_asks_for_a_worker_and_hands_the_job_over() {
    let mut desk = JobDesk::<u32, u32>::new();
    assert!(desk.submit(7).wake);
    assert!(desk.has_work());
    assert_eq!(desk.next_job(), Some(7));
    assert!(desk.in_flight());
    assert!(!desk.has_work());
}

/// Two settles before any worker looks cost one job, not two: the second
/// submission replaces the first rather than queueing behind it.
#[test]
fn submissions_before_the_job_is_taken_coalesce_to_the_latest() {
    let mut desk = JobDesk::<u32, u32>::new();
    assert_eq!(desk.submit(1).displaced, None);
    assert_eq!(desk.submit(2).displaced, Some(1));
    assert_eq!(desk.submit(3).displaced, Some(2));
    assert_eq!(desk.next_job(), Some(3));
    assert_eq!(desk.next_job(), None);
}

/// A submission during a write is held, not dropped, and needs no wake —
/// the worker looks again the moment it has delivered.
#[test]
fn a_submission_while_in_flight_is_held_without_a_wake() {
    let mut desk = JobDesk::<u32, u32>::new();
    let _ = desk.submit(1);
    assert_eq!(desk.next_job(), Some(1));
    assert!(!desk.submit(2).wake);
    assert_eq!(desk.next_job(), None);
    assert!(!desk.deliver(10));
    assert_eq!(desk.next_job(), Some(2));
}

/// Only one job is out at a time, so two workers cannot write concurrently.
#[test]
fn only_one_job_is_in_flight_at_a_time() {
    let mut desk = JobDesk::<u32, u32>::new();
    let _ = desk.submit(1);
    assert_eq!(desk.next_job(), Some(1));
    let _ = desk.submit(2);
    assert_eq!(desk.next_job(), None);
}

/// The answer to a superseded job is dropped: adopting it would show a state
/// the queued job is about to replace.
#[test]
fn a_superseded_answer_is_dropped_rather_than_delivered() {
    let mut desk = JobDesk::<u32, u32>::new();
    let _ = desk.submit(1);
    desk.next_job();
    let _ = desk.submit(2);
    assert!(!desk.deliver(10));
    assert_eq!(desk.collect(), None);
    assert_eq!(desk.next_job(), Some(2));
    assert!(desk.deliver(20));
    assert_eq!(desk.collect(), Some(20));
}

/// An answer nobody superseded is delivered and collected exactly once.
#[test]
fn an_answer_is_collected_once() {
    let mut desk = JobDesk::<u32, u32>::new();
    let _ = desk.submit(1);
    desk.next_job();
    assert!(desk.deliver(99));
    assert_eq!(desk.collect(), Some(99));
    assert_eq!(desk.collect(), None);
}

/// Delivering frees the desk for the next job even when nothing is waiting.
#[test]
fn delivering_clears_the_in_flight_marker() {
    let mut desk = JobDesk::<u32, u32>::new();
    let _ = desk.submit(1);
    desk.next_job();
    assert!(desk.in_flight());
    desk.deliver(1);
    assert!(!desk.in_flight());
    assert!(desk.submit(2).wake);
}

/// A stopping desk hands out nothing and accepts nothing, so a parked worker
/// leaves rather than finding fresh work on the way out.
#[test]
fn stopping_refuses_submissions_and_hands_out_no_work() {
    let mut desk = JobDesk::<u32, u32>::new();
    let _ = desk.submit(1);
    desk.stop();
    assert!(desk.stopping());
    assert!(!desk.has_work());
    assert_eq!(desk.next_job(), None);
    let refused = desk.submit(2);
    assert!(!refused.wake);
    assert_eq!(
        refused.displaced,
        Some(2),
        "a stopping desk hands the request straight back"
    );
    assert_eq!(desk.next_job(), None);
}

/// A worker mid-write still delivers after a stop, so a published document is
/// never left half-written and its outcome is still reportable.
#[test]
fn a_job_in_flight_when_stopping_can_still_be_delivered() {
    let mut desk = JobDesk::<u32, u32>::new();
    let _ = desk.submit(1);
    assert_eq!(desk.next_job(), Some(1));
    desk.stop();
    assert!(desk.deliver(5));
    assert_eq!(desk.collect(), Some(5));
}

/// The displaced request is handed back so a caller waiting on it can be told
/// it was superseded, rather than parked for an answer nobody will produce.
#[test]
fn a_displaced_request_is_handed_back_to_the_submitter() {
    let mut desk = JobDesk::<u32, u32>::new();
    assert_eq!(desk.submit(1).displaced, None);
    let second = desk.submit(2);
    assert_eq!(second.displaced, Some(1));
    assert!(
        second.wake,
        "nothing has taken a job, so one is still wanted"
    );
    assert_eq!(desk.next_job(), Some(2));
}

/// A job already taken is not displaceable — it is being written — so a
/// submission during one displaces nothing.
#[test]
fn a_job_in_flight_is_never_displaced() {
    let mut desk = JobDesk::<u32, u32>::new();
    let _ = desk.submit(1);
    assert_eq!(desk.next_job(), Some(1));
    assert_eq!(desk.submit(2).displaced, None);
}

/// Every request is answered, in the order it was asked.
#[test]
fn a_queue_answers_every_request_in_turn() {
    let mut queue = JobQueue::<u32, u32>::with_capacity(4).expect("room for four");
    assert_eq!(queue.submit(1), Ok(()));
    assert_eq!(queue.submit(2), Ok(()));
    assert!(queue.has_work());
    assert_eq!(queue.next_job(), Some(1));
    assert_eq!(
        queue.next_job(),
        Some(2),
        "a second worker may carry the next"
    );
    assert_eq!(queue.next_job(), None);
    assert!(queue.deliver(10), "the first answer wakes the loop");
    assert!(!queue.deliver(20), "a second finds it already owed a look");
    assert_eq!(queue.collect(), Some(10));
    assert_eq!(queue.collect(), Some(20));
    assert_eq!(queue.collect(), None);
}

/// Everything not yet collected counts against the bound, so a burst is
/// refused rather than grown without limit — and room returns as answers are
/// collected.
#[test]
fn a_full_queue_hands_the_request_back() {
    let mut queue = JobQueue::<u32, u32>::with_capacity(2).expect("room for two");
    assert_eq!(queue.submit(1), Ok(()));
    assert_eq!(queue.next_job(), Some(1));
    assert_eq!(queue.submit(2), Ok(()));
    assert_eq!(queue.submit(3), Err(3));
    assert!(queue.deliver(10));
    assert_eq!(
        queue.submit(3),
        Err(3),
        "an uncollected answer still holds its place"
    );
    assert_eq!(queue.collect(), Some(10));
    assert_eq!(queue.submit(3), Ok(()));
}

/// A stopping queue takes nothing more and hands back what was waiting, while
/// a job already in flight still delivers.
#[test]
fn stopping_hands_back_the_waiting_and_keeps_the_in_flight_deliverable() {
    let mut queue = JobQueue::<u32, u32>::with_capacity(4).expect("room for four");
    queue.submit(1).expect("room");
    queue.submit(2).expect("room");
    assert_eq!(queue.next_job(), Some(1));
    assert_eq!(
        queue.stop().into_iter().collect::<alloc::vec::Vec<_>>(),
        [2]
    );
    assert!(queue.stopping());
    assert_eq!(queue.next_job(), None);
    assert!(!queue.has_work());
    assert_eq!(queue.submit(3), Err(3));
    assert!(queue.deliver(10));
    assert_eq!(queue.collect(), Some(10));
}

/// A queue with no room refuses everything, which is what lets an embedder
/// fall back to doing the work itself.
#[test]
fn a_queue_with_no_room_refuses_every_request() {
    let mut queue = JobQueue::<u32, u32>::new();
    assert_eq!(queue.submit(1), Err(1));
    assert!(!queue.has_work());
}

/// An answer for no job in flight is dropped, so the queue never holds more
/// than the room it reserved.
#[test]
fn an_answer_nobody_took_a_job_for_is_dropped() {
    let mut queue = JobQueue::<u32, u32>::with_capacity(1).expect("room for one");
    assert!(!queue.deliver(7), "nothing was in flight");
    assert_eq!(queue.collect(), None);
    assert_eq!(queue.submit(1), Ok(()), "the room is still free");
}

/// A job its asker carries out itself lands behind the answers already there,
/// and one past the room is handed back without being run.
#[test]
fn a_job_the_asker_carries_out_lands_in_turn_within_the_room() {
    let mut queue = JobQueue::<u32, u32>::with_capacity(2).expect("room for two");
    queue.submit(1).expect("room");
    assert_eq!(queue.next_job(), Some(1));
    assert!(queue.deliver(10));
    assert_eq!(queue.carry_out(2, |n| n * 10), Ok(()));
    let mut ran = false;
    assert_eq!(
        queue.carry_out(3, |n| {
            ran = true;
            n * 10
        }),
        Err(3),
        "past the room"
    );
    assert!(!ran, "and not run");
    assert_eq!((queue.collect(), queue.collect()), (Some(10), Some(20)));
    assert!(!queue.outstanding());
}

/// Withdrawn requests are never handed out; what a worker holds is not
/// touched, and is still outstanding until it is answered.
#[test]
fn withdrawing_drops_only_the_waiting_requests_turned_down() {
    let mut queue = JobQueue::<u32, u32>::with_capacity(4).expect("room for four");
    for request in 1..=4 {
        queue.submit(request).expect("room");
    }
    assert_eq!(queue.next_job(), Some(1));
    queue.retain_waiting(|&request| request % 2 == 1);
    assert!(queue.outstanding());
    assert_eq!(queue.next_job(), Some(3));
    assert_eq!(queue.next_job(), None);
    assert!(queue.deliver(10) && !queue.deliver(30));
    assert!(!queue.outstanding(), "everything taken has been answered");
    assert_eq!(queue.submit(5), Ok(()), "withdrawn requests hold no room");
}

/// A bound that follows what the queue serves grows before it refuses, and
/// shrinking loses nothing already held.
#[test]
fn the_bound_grows_and_shrinks_without_losing_what_is_held() {
    let mut queue = JobQueue::<u32, u32>::new();
    assert_eq!(queue.submit(1), Err(1));
    queue.grow(2).expect("room");
    queue.submit(1).expect("room");
    queue.submit(2).expect("room");
    assert_eq!(queue.submit(3), Err(3));
    queue.shrink(2);
    assert_eq!(queue.submit(3), Err(3), "still full");
    assert_eq!(queue.next_job(), Some(1));
    assert_eq!(queue.next_job(), Some(2), "nothing held was lost");
    assert!(queue.deliver(10) && !queue.deliver(20));
    assert_eq!((queue.collect(), queue.collect()), (Some(10), Some(20)));
    assert_eq!(queue.submit(3), Err(3), "the room given up is gone");
}

/// What has landed is counted until it is collected, and nothing else is.
#[test]
fn landed_counts_answers_until_they_are_collected() {
    let mut queue = JobQueue::<u32, u32>::with_capacity(2).expect("room for two");
    queue.submit(1).expect("room");
    queue.submit(2).expect("room");
    assert_eq!(queue.landed(), 0, "waiting is not landed");
    assert_eq!(queue.next_job(), Some(1));
    assert_eq!(queue.landed(), 0, "in flight is not landed");
    assert!(queue.deliver(10));
    assert_eq!(queue.landed(), 1);
    assert_eq!(queue.collect(), Some(10));
    assert_eq!(queue.landed(), 0);
}
