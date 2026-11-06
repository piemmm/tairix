use super::*;
use crate::{Reversed, Threaded, SERIAL};
use core::sync::atomic::AtomicUsize;
use std::vec::Vec;

/// Each lane's record of the pieces it took, in the order it took them.
fn visits(runner: &dyn JobRunner, lanes: usize, pieces: usize) -> Vec<Vec<(usize, usize)>> {
    let mut taken = std::vec![Vec::new(); lanes];
    for_each_ordered(runner, &mut taken, 0..pieces, &|lane, number, piece| {
        lane.push((number, piece));
    });
    taken
}

/// Every piece taken once, numbered as yielded, and each lane's in the order
/// it was yielded.
fn assert_each_once_in_order(taken: &[Vec<(usize, usize)>], pieces: usize) {
    let mut all: Vec<usize> = taken.iter().flatten().map(|&(number, _)| number).collect();
    for lane in taken {
        assert!(lane.iter().all(|&(number, piece)| number == piece));
        assert!(
            lane.windows(2).all(|pair| pair[0].0 < pair[1].0),
            "{lane:?}"
        );
    }
    all.sort_unstable();
    assert_eq!(all, (0..pieces).collect::<Vec<_>>());
}

#[test]
fn a_runner_one_thread_wide_works_every_piece_on_one_lane_in_order() {
    let taken = visits(&SERIAL, 3, 9);
    assert_eq!(taken[0], (0..9).map(|at| (at, at)).collect::<Vec<_>>());
    assert!(taken[1].is_empty() && taken[2].is_empty());
}

#[test]
fn lanes_run_in_any_order_still_begin_the_pieces_in_order() {
    let reversed = Reversed::new(4);
    let taken = visits(&reversed, 4, 9);
    assert_each_once_in_order(&taken, 9);
    assert_eq!(taken[3].len(), 9, "the lane run first takes every piece");
}

#[test]
fn every_piece_is_taken_once_across_real_threads() {
    for lanes in [1, 2, 5] {
        let taken = visits(&Threaded::new(lanes), lanes, 200);
        assert_each_once_in_order(&taken, 200);
    }
}

#[test]
fn no_pieces_visit_nothing() {
    let taken = visits(&Threaded::new(3), 3, 0);
    assert!(taken.iter().all(Vec::is_empty));
}

/// A wavefront of `rows` rows of `columns` cells: each cell one more than the
/// cell above and the one above-right, which each row waits for.
fn wavefront(runner: &dyn JobRunner, lanes: usize, rows: usize, columns: usize) -> Vec<u32> {
    let cells: Vec<Published<u32>> = (0..rows * columns).map(|_| Published::new()).collect();
    let mut idle = std::vec![(); lanes];
    for_each_ordered(runner, &mut idle, 0..rows, &|(), row, _| {
        for column in 0..columns {
            let mut value = 1;
            if let Some(above) = row.checked_sub(1) {
                let right = (column + 1).min(columns - 1);
                value +=
                    cells[above * columns + column].wait() + cells[above * columns + right].wait();
            }
            assert_eq!(cells[row * columns + column].publish(value), Ok(()));
        }
    });
    cells
        .iter()
        .map(|cell| cell.get().copied().unwrap_or(0))
        .collect()
}

#[test]
fn a_wavefront_reads_what_the_row_above_published_whatever_runs_it() {
    let (rows, columns) = (24, 11);
    let serial = wavefront(&SERIAL, 1, rows, columns);
    assert_eq!(serial[..columns], [1; 11]);
    assert_eq!(serial[columns], 3);
    assert_eq!(wavefront(&SERIAL, 4, rows, columns), serial);
    assert_eq!(wavefront(&Reversed::new(4), 4, rows, columns), serial);
    for lanes in [2, 3, 8] {
        assert_eq!(
            wavefront(&Threaded::new(lanes), lanes, rows, columns),
            serial
        );
    }
}

#[test]
fn a_slot_is_written_once_and_read_without_waiting_once_written() {
    let slot = Published::new();
    assert_eq!(slot.get(), None);
    assert_eq!(slot.publish(5), Ok(()));
    assert_eq!(slot.publish(6), Err(6), "a second write is refused");
    assert_eq!(*slot.wait(), 5);
    assert_eq!(slot.get(), Some(&5));
}

#[test]
fn a_waiting_mark_neither_fills_a_slot_nor_stops_its_write() {
    let slot = Published::new();
    assert_eq!(
        Futex(&slot.state).compare_exchange(EMPTY, WAITING),
        Ok(EMPTY)
    );
    assert_eq!(slot.get(), None);
    assert_eq!(slot.publish(9), Ok(()));
    assert_eq!(
        slot.state.load(Ordering::Relaxed),
        SET,
        "the write clears the mark"
    );
}

/// A value that counts its drops.
struct Counted<'a>(&'a AtomicUsize);

impl Drop for Counted<'_> {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
}

#[test]
fn a_reset_hands_the_value_back_and_a_drop_drops_it_once() {
    let drops = AtomicUsize::new(0);
    let mut slot = Published::new();
    assert!(slot.reset().is_none());
    assert!(slot.publish(Counted(&drops)).is_ok());
    let held = slot.reset();
    assert!(held.is_some() && slot.get().is_none());
    drop(held);
    assert_eq!(drops.load(Ordering::Relaxed), 1);
    assert!(
        slot.publish(Counted(&drops)).is_ok(),
        "an empty slot takes a write"
    );
    drop(slot);
    assert_eq!(drops.load(Ordering::Relaxed), 2);
    drop(Published::<Counted<'_>>::new());
    assert_eq!(
        drops.load(Ordering::Relaxed),
        2,
        "an empty slot drops nothing"
    );
}
