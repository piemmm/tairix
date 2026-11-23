//! The desk's turns, held white-box on one thread: the host's locks never
//! park, so a waiting thread would only spin.

use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicUsize, Ordering};

use tairix_abi::DriverError;

use super::{Answer, Desk, Order, Rouse};

/// A rouse that fails its first `failing` times, counting each.
struct Counted {
    rouses: Arc<AtomicUsize>,
    failing: usize,
}

impl Rouse for Counted {
    fn rouse(&self) -> bool {
        self.rouses.fetch_add(1, Ordering::SeqCst) >= self.failing
    }
}

fn desk(failing: usize) -> (Desk<u32>, Arc<AtomicUsize>) {
    let rouses = Arc::new(AtomicUsize::new(0));
    let rouse = Counted {
        rouses: Arc::clone(&rouses),
        failing,
    };
    (Desk::new(Box::new(rouse)), rouses)
}

fn order(desk: &Desk<u32>, then: u32) {
    assert!(desk.order(Order::MakeContext, then).is_ok());
}

#[test]
fn orders_are_carried_out_in_turn_and_answered_with_what_was_left_with_them() {
    let (desk, _) = desk(0);
    order(&desk, 7);
    order(&desk, 8);
    let (first, _) = desk.take().expect("an order");
    let (second, _) = desk.take().expect("an order");
    assert!(first < second, "tags in the order the orders came");
    assert!(desk.take().is_none());
    desk.answer(second, Answer::Done);
    desk.answer(first, Answer::Refused(DriverError::Busy, None));
    let (then, answer) = desk.next_answer().expect("answered");
    assert_eq!(then, 8, "answers in the order they came");
    assert!(matches!(answer, Answer::Done));
    let (then, answer) = desk.next_answer().expect("answered");
    assert_eq!(then, 7);
    assert!(matches!(answer, Answer::Refused(DriverError::Busy, None)));
}

#[test]
fn an_order_put_back_is_first_in_line_again() {
    let (desk, _) = desk(0);
    order(&desk, 1);
    order(&desk, 2);
    let (tag, put) = desk.take().expect("an order");
    desk.put_back(tag, put);
    let (again, _) = desk.take().expect("the order put back");
    assert_eq!(again, tag);
    desk.answer(again, Answer::Done);
    assert_eq!(desk.next_answer().map(|(then, _)| then), Some(1));
}

#[test]
fn a_burst_of_orders_rouses_the_display_loop_once_per_turn() {
    let (desk, rouses) = desk(0);
    order(&desk, 1);
    order(&desk, 2);
    assert_eq!(rouses.load(Ordering::SeqCst), 1, "one rouse for the burst");
    desk.begin_turn();
    let taken: Vec<u64> = core::iter::from_fn(|| desk.take().map(|(tag, _)| tag)).collect();
    assert_eq!(taken.len(), 2);
    order(&desk, 3);
    assert_eq!(
        rouses.load(Ordering::SeqCst),
        2,
        "an order after the turn began rouses again"
    );
    assert!(desk.has_orders());
}

#[test]
fn a_rouse_that_could_not_be_given_is_owed_and_given_with_the_next_order() {
    let (desk, rouses) = desk(1);
    order(&desk, 1);
    assert_eq!(rouses.load(Ordering::SeqCst), 1);
    order(&desk, 2);
    assert_eq!(
        rouses.load(Ordering::SeqCst),
        2,
        "the owed rouse given again"
    );
    order(&desk, 3);
    assert_eq!(rouses.load(Ordering::SeqCst), 2, "and none past it");
}

#[test]
fn a_closed_desk_ends_its_answers_once_every_order_is_answered() {
    let (desk, _) = desk(0);
    order(&desk, 1);
    let (tag, _) = desk.take().expect("an order");
    desk.close();
    desk.answer(tag, Answer::Done);
    assert_eq!(desk.next_answer().map(|(then, _)| then), Some(1));
    assert!(desk.next_answer().is_none());
}

#[test]
fn an_answer_to_no_order_in_service_is_dropped() {
    let (desk, _) = desk(0);
    desk.answer(99, Answer::Done);
    desk.close();
    assert!(desk.next_answer().is_none());
}
