//! Unit tests for a held file's written bytes.

use alloc::vec;
use alloc::vec::Vec;

use super::{NoRoom, Written, BLOCK, BLOCK_PINNED};
use crate::wire::IO_MAX;

const BLOCK_LEN: u64 = BLOCK as u64;

/// What `written` lays over `len` zero bytes from `offset`.
fn seen(written: &Written, offset: u64, len: usize) -> Vec<u8> {
    let mut out = vec![0; len];
    written.overlay(offset, &mut out);
    out
}

/// Every stretch a wave would send, from the start.
fn stretches(written: &Written) -> Vec<(u64, usize)> {
    let mut found = Vec::new();
    let mut at = 0;
    while let Some((offset, len)) = written.stretch(at, IO_MAX) {
        found.push((offset, len));
        at = offset + len as u64;
    }
    found
}

#[test]
fn writes_that_meet_join_and_a_later_one_wins() {
    let mut written = Written::default();
    assert_eq!(written.write(10, b"hello", 5), Ok(5));
    assert_eq!(written.write(15, b" world", 6), Ok(6));
    assert_eq!(written.write(10, b"J", 7), Ok(0), "over bytes already held");
    assert_eq!(&seen(&written, 10, 11), b"Jello world");
    assert_eq!(seen(&written, 0, 12)[..10], [0; 10], "nothing before");
    assert_eq!(written.since(), Some(5), "the oldest byte's age");
    assert_eq!(stretches(&written), [(10, 11)]);
    assert_eq!(written.let_go(10, 11), 11);
    assert!(written.is_empty());
    assert_eq!(written.since(), None, "nothing held, nothing aged");
    assert_eq!(
        (written.blocks(), written.pinned()),
        (0, 0),
        "nothing pinned"
    );
}

#[test]
fn writes_apart_stay_apart_and_one_bridging_them_joins_them() {
    let mut written = Written::default();
    written.write(0, b"aa", 1).expect("room");
    written.write(10, b"bb", 1).expect("room");
    assert_eq!(seen(&written, 0, 12), b"aa\0\0\0\0\0\0\0\0bb");
    assert_eq!(stretches(&written), [(0, 2), (10, 2)]);
    written.write(1, b"cccccccccc", 2).expect("room");
    assert_eq!(seen(&written, 0, 12), b"acccccccccc\x62");
    assert_eq!(stretches(&written), [(0, 12)]);
    assert_eq!(written.bytes(), 12);
}

#[test]
fn a_stretch_runs_on_across_blocks_and_stops_at_one_call() {
    let mut written = Written::default();
    let data: Vec<u8> = (0..IO_MAX + 2).map(|at| at.to_le_bytes()[0]).collect();
    written.write(100, &data, 1).expect("room");
    assert_eq!(
        stretches(&written),
        [(100, IO_MAX), (100 + IO_MAX as u64, 2)],
        "no write past one call's worth"
    );
    let joined: Vec<u8> = written.parts(100, IO_MAX).flatten().copied().collect();
    assert_eq!(joined, data[..IO_MAX], "the parts are the bytes, in order");
    assert!(written.parts(100, IO_MAX).all(|part| part.len() <= BLOCK));
}

#[test]
fn a_stretch_stops_where_a_block_boundary_has_a_gap() {
    let mut written = Written::default();
    written.write(BLOCK_LEN - 2, b"ab", 1).expect("room");
    written.write(BLOCK_LEN + 1, b"cd", 1).expect("room");
    assert_eq!(
        stretches(&written),
        [(BLOCK_LEN - 2, 2), (BLOCK_LEN + 1, 2)]
    );
    written.write(BLOCK_LEN, b"x", 1).expect("room");
    assert_eq!(stretches(&written), [(BLOCK_LEN - 2, 5)], "the gap filled");
    assert_eq!(&seen(&written, BLOCK_LEN - 2, 5), b"abxcd");
}

#[test]
fn every_other_byte_is_its_own_stretch() {
    let mut written = Written::default();
    for at in (0..200).step_by(2) {
        written
            .write(at, &[at.to_le_bytes()[0] | 1], 1)
            .expect("room");
    }
    let found = stretches(&written);
    assert_eq!(found.len(), 100);
    assert!(found
        .iter()
        .all(|&(offset, len)| offset % 2 == 0 && len == 1));
    let bytes = seen(&written, 0, 200);
    assert!(bytes.iter().enumerate().all(|(at, &byte)| {
        if at % 2 == 0 {
            byte == at.to_le_bytes()[0] | 1
        } else {
            byte == 0
        }
    }));
    assert_eq!(written.blocks(), 1, "one block holds them all");
}

#[test]
fn letting_part_of_a_stretch_go_leaves_the_rest_to_send() {
    let mut written = Written::default();
    written.write(60, &[7; 100], 3).expect("room");
    assert_eq!(written.let_go(60, 40), 40);
    assert_eq!(written.bytes(), 60);
    assert_eq!(stretches(&written), [(100, 60)]);
    assert_eq!(written.since(), Some(3), "what is left is as old");
    assert_eq!(written.let_go(0, 1000), 60, "only what was held");
    assert!(written.is_empty());
}

#[test]
fn letting_go_frees_the_blocks_it_empties() {
    let mut written = Written::default();
    written.write(0, &[1; 3 * BLOCK], 1).expect("room");
    assert_eq!(written.blocks(), 3);
    assert_eq!(written.let_go(BLOCK_LEN, BLOCK), BLOCK);
    assert_eq!(written.blocks(), 2, "the middle block went");
    assert_eq!(stretches(&written), [(0, BLOCK), (2 * BLOCK_LEN, BLOCK)]);
    assert_eq!(written.let_all_go(), 2 * BLOCK);
    assert_eq!(
        (written.bytes(), written.blocks(), written.pinned()),
        (0, 0, 0)
    );
}

#[test]
fn what_is_pinned_counts_every_block_and_its_index() {
    let mut written = Written::default();
    assert_eq!(written.new_blocks(0, 1), 1);
    assert_eq!(
        written.new_blocks(BLOCK_LEN - 1, 2),
        2,
        "a write across a boundary"
    );
    assert_eq!(written.new_blocks(0, 0), 0);
    written.write(0, b"a", 1).expect("room");
    written.write(1000 * BLOCK_LEN, b"b", 1).expect("room");
    assert_eq!(written.bytes(), 2, "two bytes held");
    assert_eq!(written.blocks(), 2);
    assert!(
        written.pinned() > 2 * BLOCK_PINNED,
        "pinned counts both blocks and the index reaching them"
    );
    assert_eq!(written.new_blocks(0, BLOCK), 0, "the first block is had");
    assert_eq!(written.new_blocks(0, BLOCK + 1), 1);
}

#[test]
fn bits_across_a_word_are_held_and_let_go_exactly() {
    let mut written = Written::default();
    written.write(60, &[1; 10], 1).expect("room");
    assert_eq!(written.write(62, &[2; 4], 1), Ok(0));
    assert_eq!(written.write(70, &[3; 60], 1), Ok(60));
    assert_eq!(written.bytes(), 70);
    assert_eq!(written.let_go(63, 2), 2);
    assert_eq!(stretches(&written), [(60, 3), (65, 65)]);
}

#[test]
fn an_overlay_takes_only_the_window_asked() {
    let mut written = Written::default();
    written.write(100, b"0123456789", 1).expect("room");
    assert_eq!(seen(&written, 105, 3), b"567");
    assert_eq!(seen(&written, 95, 7), b"\0\0\0\0\0\x30\x31");
    assert_eq!(seen(&written, 108, 6), b"89\0\0\0\0");
    assert_eq!(seen(&written, 0, 10), [0; 10]);
    written.write(5 * BLOCK_LEN + 1, b"far", 1).expect("room");
    let window = seen(&written, 4 * BLOCK_LEN, 2 * BLOCK);
    assert_eq!(&window[BLOCK + 1..BLOCK + 4], b"far");
    assert_eq!(window.iter().filter(|&&byte| byte != 0).count(), 3);
}

#[test]
fn a_write_past_the_last_offset_is_refused_whole() {
    let mut written = Written::default();
    written.write(0, b"kept", 1).expect("room");
    assert_eq!(written.write(u64::MAX - 1, b"abc", 2), Err(NoRoom));
    assert_eq!(written.bytes(), 4);
    assert_eq!(written.blocks(), 1);
    assert_eq!(written.write(5, b"", 3), Ok(0), "nothing to hold");
}
