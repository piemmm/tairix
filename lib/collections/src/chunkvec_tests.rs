use alloc::vec::Vec;

use super::ChunkVec;

/// A pushed element answers at the index its push gave, and never moves
/// however many follow it: each chunk is allocated once, whole.
#[test]
fn a_pushed_element_keeps_its_index_and_its_place() {
    let mut list = ChunkVec::<u32, 8>::new();
    assert!(list.is_empty() && list.get(0).is_none() && list.chunks.is_empty());
    assert_eq!(list.try_push(7), Ok(0));
    let first: *const u32 = list.get(0).expect("pushed");
    for value in 1..1000u32 {
        let index = usize::try_from(value).expect("an index");
        assert_eq!(list.try_push(value * 3), Ok(index));
    }
    assert_eq!(list.len(), 1000);
    assert!(core::ptr::eq(first, list.get(0).expect("held")));
    assert_eq!(list.chunks.len(), 125);
    assert!(list
        .chunks
        .iter()
        .all(|chunk| chunk.len() == 8 && chunk.capacity() >= 8));
    for value in 1..1000u32 {
        let index = usize::try_from(value).expect("an index");
        assert_eq!(list.get(index).copied(), Some(value * 3));
    }
    assert!(list.get(1000).is_none());
}

/// A walk from any place, a chunk's first or its middle or past the end,
/// reads on in the order the elements were pushed.
#[test]
fn a_walk_from_any_place_reads_on_in_pushed_order() {
    let mut list = ChunkVec::<u64, 4>::new();
    for value in 0..37u64 {
        list.try_push(value).expect("room");
    }
    let all: Vec<u64> = list.iter().copied().collect();
    assert_eq!(all, (0..37).collect::<Vec<_>>());
    for start in 0..40u64 {
        let from: Vec<u64> = list
            .iter_from(usize::try_from(start).expect("an index"))
            .copied()
            .collect();
        assert_eq!(from, (start..37).collect::<Vec<_>>(), "{start}");
    }
}

/// An element changed in place, one by one or along a walk, reads back
/// changed, and nothing past the end is handed out to change.
#[test]
fn an_element_changed_in_place_reads_back_changed() {
    let mut list = ChunkVec::<i32, 2>::default();
    for value in 0..5 {
        list.try_push(value).expect("room");
    }
    *list.get_mut(3).expect("held") = -3;
    for value in list.iter_mut() {
        *value *= 2;
    }
    for value in &mut list {
        *value += 1;
    }
    let read: Vec<i32> = (&list).into_iter().copied().collect();
    assert_eq!(read, [1, 3, 5, -5, 9]);
    assert_eq!(list.iter().copied().collect::<Vec<_>>(), read);
    assert!(list.get_mut(5).is_none());
}
