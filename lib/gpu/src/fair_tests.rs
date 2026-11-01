use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};

use tairix_abi_trap::seam;

use super::{Fair, Turn, PLACES};
use crate::device_tests::{until, wakes, FUTEX_WAKE};

#[test]
fn a_turn_s_end_notifies_the_next_turn_s_place_only_where_a_locker_waits() {
    // On a thread of its own, as the seam stays armed for the thread.
    std::thread::spawn(|| {
        let fair = Fair::new(0u32);
        // The word the next turn's place parks its lockers on.
        let (_, next) = seam::capture(0, || fair.places[1 % PLACES].notify_all());
        let held = fair.lock();
        // A locker given the next turn and parked in its place.
        {
            let mut turns = fair.turns.lock();
            turns.given += 1;
            turns.parked[1 % PLACES] += 1;
        }
        seam::arm(0);
        drop(held);
        let calls = seam::calls();
        assert_eq!(wakes(&calls, u32::MAX), 1, "one place woken");
        assert!(
            calls
                .iter()
                .any(|(number, args)| *number == FUTEX_WAKE && args.first() == next.first()),
            "the next turn's"
        );
        fair.turns.lock().parked[1 % PLACES] -= 1;
        let before = seam::calls().len();
        drop(Turn(&fair));
        assert_eq!(
            wakes(&seam::calls()[before..], u32::MAX),
            0,
            "none woken where none waits"
        );
    })
    .join()
    .expect("the test's thread");
}

#[test]
fn lockers_have_the_value_in_the_order_they_asked() {
    let fair = Arc::new(Fair::new(0u32));
    let order = Arc::new(std::sync::Mutex::new(Vec::new()));
    let held = fair.lock();
    let waiter = {
        let (fair, order) = (Arc::clone(&fair), Arc::clone(&order));
        std::thread::spawn(move || {
            let _value = fair.lock();
            order.lock().expect("the order").push("waiter");
        })
    };
    // The waiter has asked once its turn is given.
    let asked = until(|| fair.turns.lock().given >= 2);
    drop(held);
    // Locking again at once, it waits behind the one that asked first.
    let again = fair.lock();
    order.lock().expect("the order").push("again");
    drop(again);
    waiter.join().expect("the waiter");
    assert!(asked, "the waiter never asked");
    assert_eq!(*order.lock().expect("the order"), ["waiter", "again"]);
}

#[test]
fn a_locker_waits_for_every_turn_given_before_its_own() {
    let fair = Arc::new(Fair::new(0u32));
    // An earlier locker's turn, given and not yet served.
    fair.turns.lock().given = 1;
    let entered = Arc::new(AtomicBool::new(false));
    let locker = {
        let (fair, entered) = (Arc::clone(&fair), Arc::clone(&entered));
        std::thread::spawn(move || {
            let _value = fair.lock();
            entered.store(true, Ordering::SeqCst);
        })
    };
    let settled =
        until(|| fair.turns.lock().parked != [0; PLACES] || entered.load(Ordering::SeqCst));
    let early = entered.load(Ordering::SeqCst);
    let parked = fair.turns.lock().parked;
    // The earlier turn ends.
    drop(Turn(&fair));
    locker.join().expect("the locker");
    assert!(settled, "the locker neither parked nor entered");
    assert!(!early, "it waits though the value is free");
    assert_eq!(parked.iter().sum::<usize>(), 1);
    assert_eq!(parked[1 % PLACES], 1, "in its own turn's place");
    assert!(entered.load(Ordering::SeqCst), "its own turn came");
}

#[test]
fn a_turn_s_end_wakes_the_next_turn_s_place_alone() {
    let fair = Fair::new(0u32);
    let mut turns = fair.turns.lock();
    // The first turn served, the next two and one a lap of places on parked.
    turns.given = 2 + PLACES;
    for turn in [1, 2, 1 + PLACES] {
        turns.parked[turn % PLACES] += 1;
    }
    assert_eq!(turns.end(), Some(1 % PLACES), "the next turn's place");
    turns.parked[1 % PLACES] -= 1;
    assert_eq!(turns.end(), Some(2 % PLACES));
    turns.parked[2 % PLACES] -= 1;
    assert_eq!(turns.end(), None, "no one parked in the next turn's place");
    turns.served = PLACES;
    assert_eq!(
        turns.end(),
        Some(1 % PLACES),
        "a turn a lap on shares its place"
    );
}

#[test]
fn turns_go_on_in_order_across_the_counters_wrap() {
    let fair = Fair::new(0u32);
    {
        let mut turns = fair.turns.lock();
        turns.given = usize::MAX;
        turns.served = usize::MAX;
    }
    let last = fair.lock();
    assert_eq!(
        fair.turns.lock().given,
        0,
        "the turn after the last is zero"
    );
    drop(last);
    let first = fair.lock();
    let turns = fair.turns.lock();
    assert_eq!((turns.given, turns.served), (1, 0), "served past the wrap");
    drop(turns);
    drop(first);
    let mut turns = fair.turns.lock();
    turns.served = usize::MAX;
    turns.parked[0] = 1;
    assert_eq!(turns.end(), Some(0), "zero's place woken past the wrap");
}

#[test]
fn a_drop_noted_under_a_fair_lock_is_counted() {
    let fair = Fair::new(0u32);
    let before = super::held::drops_under();
    super::held::note_drop();
    assert_eq!(super::held::drops_under(), before, "none held");
    let held = fair.lock();
    super::held::note_drop();
    drop(held);
    assert_eq!(super::held::drops_under(), before + 1);
    super::held::note_drop();
    assert_eq!(super::held::drops_under(), before + 1, "let go again");
}

#[test]
fn a_waiter_parking_wakes_the_next_turn_s_place() {
    std::thread::spawn(|| {
        let fair = Fair::new(0u32);
        let (_, next) = seam::capture(0, || fair.places[1 % PLACES].notify_all());
        let held = fair.lock();
        // A locker given the next turn and parked in its place.
        {
            let mut turns = fair.turns.lock();
            turns.given += 1;
            turns.parked[1 % PLACES] += 1;
        }
        seam::arm(0);
        let again = fair.park(held, |mut turns| {
            // That locker has its turn and ends it.
            turns.parked[1 % PLACES] -= 1;
            turns.served = turns.served.wrapping_add(1);
            turns
        });
        let calls = seam::calls();
        assert_eq!(wakes(&calls, u32::MAX), 1, "one place woken");
        assert!(
            calls
                .iter()
                .any(|(number, args)| *number == FUTEX_WAKE && args.first() == next.first()),
            "the next turn's, as the waiter parked"
        );
        drop(again);
    })
    .join()
    .expect("the test's thread");
}

#[test]
fn a_waiter_parking_wakes_no_place_where_none_waits() {
    std::thread::spawn(|| {
        let fair = Fair::new(0u32);
        let held = fair.lock();
        seam::arm(0);
        let again = fair.park(held, |turns| turns);
        assert_eq!(wakes(&seam::calls(), u32::MAX), 0);
        drop(again);
    })
    .join()
    .expect("the test's thread");
}

#[test]
fn a_waiter_parks_with_its_turn_and_the_value_given_up() {
    let fair = Fair::new(0u32);
    let held = fair.lock();
    let again = fair.park(held, |turns| {
        assert_eq!(turns.served, turns.given, "no turn kept while parked");
        assert!(fair.value.try_lock().is_some(), "nor the value");
        turns
    });
    let turns = fair.turns.lock();
    assert_eq!(
        (turns.given, turns.served),
        (2, 1),
        "a turn of its own after"
    );
    drop(turns);
    drop(again);
}
