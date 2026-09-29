//! Host tests for reading a document in a step at a time.

use alloc::vec::Vec;

use super::{ReadStep, Reading};

/// A read of `data` as a file does: what fits from `offset`, short at the end.
fn file(data: &[u8]) -> impl FnMut(u64, &mut [u8]) -> Result<usize, ()> + '_ {
    move |offset, buf| {
        let from = usize::try_from(offset).expect("small").min(data.len());
        let got = buf.len().min(data.len() - from);
        buf[..got].copy_from_slice(&data[from..from + got]);
        Ok(got)
    }
}

/// Step `reading` to its end, `budget` bytes a step, answering the steps it
/// took and what it read.
fn read_through(
    reading: &mut Reading,
    data: &[u8],
    budget: usize,
    chunk: usize,
) -> (usize, Vec<Vec<u8>>) {
    for steps in 1..=data.len() + 2 {
        match reading.step(budget, chunk, file(data)) {
            ReadStep::Partial => {}
            ReadStep::Done(chunks) => return (steps, chunks),
            other => panic!("a read of plain bytes failed: {other:?}"),
        }
    }
    panic!("a read of {} bytes never finished", data.len());
}

#[test]
fn a_file_is_read_in_steps_into_chunks_that_fit_it() {
    let data: Vec<u8> = (0..25u8).collect();
    let mut reading = Reading::new(25);
    let (steps, chunks) = read_through(&mut reading, &data, 10, 4);
    assert!(
        steps >= 3,
        "a budget of ten reads twenty-five in three steps"
    );
    assert!(chunks
        .iter()
        .all(|chunk| chunk.len() <= 4 && chunk.capacity() == chunk.len()));
    assert_eq!(chunks.concat(), data);
}

#[test]
fn a_file_that_grew_since_it_was_measured_is_read_to_its_end() {
    let data: Vec<u8> = (0..25u8).collect();
    let (_, chunks) = read_through(&mut Reading::new(10), &data, 1 << 20, 4);
    assert_eq!(chunks.concat(), data, "nothing added since is left behind");
    assert!(chunks.iter().all(|chunk| chunk.capacity() == chunk.len()));
}

#[test]
fn a_file_cut_short_since_it_was_measured_is_read_to_where_it_ends() {
    let data: Vec<u8> = (0..10u8).collect();
    let (_, chunks) = read_through(&mut Reading::new(25), &data, 1 << 20, 4);
    assert_eq!(chunks.concat(), data);
}

#[test]
fn an_empty_file_reads_as_nothing() {
    let (_, chunks) = read_through(&mut Reading::new(0), &[], 1 << 20, 4);
    assert!(chunks.is_empty());
}

#[test]
fn a_refused_read_and_a_refused_room_are_answered_not_hidden() {
    let mut reading = Reading::new(8);
    assert!(matches!(
        reading.step(8, 4, |_, _| Err::<usize, u8>(7)),
        ReadStep::Refused(7)
    ));
    let mut huge = Reading::new(u64::MAX);
    assert!(matches!(
        huge.step(usize::MAX, usize::MAX, |_, _| Ok::<usize, ()>(0)),
        ReadStep::NoMemory
    ));
}

#[test]
fn a_file_that_has_not_grown_costs_one_byte_of_probing() {
    let data: Vec<u8> = (0..10u8).collect();
    let mut asked = Vec::new();
    let mut reading = Reading::new(10);
    let mut read = file(&data);
    let step = reading.step(1 << 20, 1 << 20, |offset, into| {
        asked.push(into.len());
        read(offset, into)
    });
    assert!(matches!(step, ReadStep::Done(_)));
    assert_eq!(
        asked,
        [10, 1],
        "the measured length, then one byte to find the end"
    );
}
