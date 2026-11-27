//! The ring: records placed in order, wrapping whole, their space given
//! back in order, and both sides moving alike.

use super::*;

#[test]
fn records_follow_one_another_and_wrap_whole() {
    let mut worker = Ring::new();
    let mut front = Ring::new();
    let mut placed = alloc::vec::Vec::new();
    let mut written = 0;
    while let Some(record) = worker.place(RECORD_MOST - 4) {
        worker.take(record);
        assert_eq!(front.accept(record.offset, record.bytes), Ok(record));
        written += record.released();
        placed.push(record);
    }
    assert_eq!(placed.len(), 4, "as many as fit");
    assert!(placed.iter().all(|record| record.skipped == 0));
    assert_eq!(worker.held(), written);
    front
        .release(placed[0].released())
        .expect("the oldest given back");
    worker.release(placed[0].released()).expect("likewise");
    assert_eq!(
        worker.place(RECORD_MOST),
        None,
        "it would run into the second, held"
    );
    let wrapped = worker.place(RECORD_MOST - 4).expect("the first's room");
    assert_eq!(wrapped.offset, 0, "past the end, started again");
    assert_eq!(wrapped.skipped, RING_BYTES - 4 * (RECORD_MOST - 4));
    worker.take(wrapped);
    assert_eq!(front.accept(0, RECORD_MOST - 4), Ok(wrapped));
    assert_eq!(front, worker, "both sides alike");
    assert_eq!(worker.held(), RING_BYTES, "full to the byte");
}

#[test]
fn a_record_where_the_next_does_not_go_or_of_no_size_is_refused() {
    let mut front = Ring::new();
    assert_eq!(front.accept(4, 16), Err(RingError::Misplaced));
    assert_eq!(front.accept(0, 0), Err(RingError::Misplaced));
    assert_eq!(front.accept(0, RECORD_MOST + 1), Err(RingError::Misplaced));
    assert_eq!(front, Ring::new(), "nothing moved");
    front.accept(0, 16).expect("the first");
    assert_eq!(front.accept(0, 16), Err(RingError::Misplaced), "not again");
    assert_eq!(front.accept(16, 16).map(|r| r.offset), Ok(16));
}

#[test]
fn no_record_is_placed_over_one_held_nor_more_given_back_than_held() {
    let mut worker = Ring::new();
    for _ in 0..4 {
        let record = worker.place(RECORD_MOST).expect("room");
        worker.take(record);
    }
    assert_eq!(worker.place(1), None, "full");
    assert_eq!(worker.release(RING_BYTES + 1), Err(RingError::Overreleased));
    worker.release(RECORD_MOST).expect("one given back");
    let record = worker.place(RECORD_MOST).expect("its room");
    assert_eq!(record.offset, 0);
    worker.take(record);
    assert_eq!(worker.place(1), None, "full again");
}
