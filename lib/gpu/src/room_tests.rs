use alloc::collections::VecDeque;
use alloc::sync::Arc;

use tairix_collections::{SlotKey, SlotMap};

use super::{Charged, Room};
use crate::memory::{Budget, Charge, Held};
use crate::Error;

fn charge(budget: &Arc<Budget>, bytes: u64) -> Charge {
    Charge::take(&[budget], bytes, Held::Host).expect("charged")
}

/// Make room for one more entry in `charged`, charged to `budget`.
fn make_room<S: Room>(charged: &mut Charged<S>, budget: &Arc<Budget>) -> Result<(), Error> {
    if let Some((more, bytes)) = charged.growth() {
        let taken = Charge::take(&[budget], bytes, Held::Host)?;
        charged.grow(more, taken)?;
    }
    Ok(())
}

#[test]
fn a_queue_is_charged_for_the_room_it_grows_to_and_gives_back_what_a_burst_left() {
    let budget = Arc::new(Budget::new(1 << 20, Error::OverLimit));
    let mut queue = Charged::new(VecDeque::new(), charge(&budget, 0)).expect("empty");
    for k in 0..1000u64 {
        if queue.growth().is_some() {
            assert_eq!(queue.push_back(k), Err(k), "no room was made");
        }
        make_room(&mut queue, &budget).expect("room");
        queue.push_back(k).expect("room made");
        assert_eq!(budget.taken(), queue.room_bytes(), "after {k}");
    }
    let peak = budget.taken();
    while queue.len() > 1 {
        let _ = queue.pop_front();
        queue.settle();
        assert_eq!(budget.taken(), queue.room_bytes());
    }
    assert!(
        budget.taken() * 64 <= peak,
        "{} of a peak of {peak} kept for one entry",
        budget.taken()
    );
}

#[test]
fn a_table_s_room_stays_charged_while_it_is_kept_and_reused() {
    let budget = Arc::new(Budget::new(1 << 20, Error::OverLimit));
    let mut table: Charged<SlotMap<SlotKey, u64>> =
        Charged::new(SlotMap::new(), charge(&budget, 0)).expect("empty");
    let mut keys = alloc::vec::Vec::new();
    for k in 0..100 {
        make_room(&mut table, &budget).expect("room");
        keys.push(table.try_insert(k).expect("room made"));
        assert_eq!(budget.taken(), table.room_bytes());
    }
    let peak = budget.taken();
    for key in keys {
        let _ = table.remove(key);
    }
    assert_eq!(
        budget.taken(),
        peak,
        "the slots are kept, and so their charge"
    );
    for k in 0..100 {
        make_room(&mut table, &budget).expect("room");
        let _ = table.try_insert(k).expect("a vacant slot");
    }
    assert_eq!(budget.taken(), peak, "the vacant slots were reused");
}

#[test]
fn growth_past_the_budget_is_refused_and_changes_nothing() {
    let budget = Arc::new(Budget::new(40, Error::OverLimit));
    let mut queue = Charged::new(VecDeque::<u64>::new(), charge(&budget, 0)).expect("empty");
    make_room(&mut queue, &budget).expect("four entries' room");
    for k in 0..4 {
        queue.push_back(k).expect("room made");
    }
    assert_eq!(make_room(&mut queue, &budget), Err(Error::OverLimit));
    assert_eq!(queue.capacity(), 4);
    assert_eq!(budget.taken(), 32);
}

#[test]
fn a_charge_of_other_budgets_is_not_folded_in() {
    let budget = Arc::new(Budget::new(1 << 20, Error::OverLimit));
    let other = Arc::new(Budget::new(1 << 20, Error::OverLimit));
    let mut queue = Charged::new(VecDeque::<u64>::new(), charge(&budget, 0)).expect("empty");
    assert_eq!(queue.grow(4, charge(&other, 32)), Err(Error::Malformed));
    assert_eq!(queue.capacity(), 0);
    assert_eq!(other.taken(), 0, "the refused charge was given back");
}
