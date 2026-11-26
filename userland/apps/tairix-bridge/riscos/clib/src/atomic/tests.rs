//! The probes' objects and values: genuine's answers on RISC OS 5.30.

use super::*;
use crate::c::testing::{Client, FREE};

/// The object's bytes, the expected value's when it matches, and when not.
const OBJECT: u32 = FREE;
const EXPECTED: u32 = FREE + 0x100;
const OTHER: u32 = FREE + 0x200;
const PATTERN: [u8; 8] = [0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88];
const DIFFERENT: [u8; 8] = [0x21, 0x32, 0x43, 0x54, 0x65, 0x76, 0x87, 0x98];
/// The value given, in R2 and R3.
const LOW: u32 = 0x1A2B_3C4D;
const HIGH: u32 = 0x1526_3748;

fn routine(n: usize) -> tairix_riscos_kernel::module::Code {
    match ENTRIES[n] {
        Entry::Own(code) => code,
        Entry::Shared { .. } => panic!("{n} shares"),
    }
}

fn fresh() -> Client {
    let mut client = Client::new();
    client.put(OBJECT, &PATTERN);
    client.put(EXPECTED, &PATTERN);
    client.put(OTHER, &DIFFERENT);
    client
}

#[test]
fn store_writes_r2_or_r2_and_r3_little_endian() {
    for (n, size) in [(1, 1), (2, 2), (3, 4), (4, 8)] {
        let mut client = fresh();
        client.call(routine(n), &[OBJECT, 5, LOW, HIGH]);
        let mut want = PATTERN;
        want[..size]
            .copy_from_slice(&(u64::from(HIGH) << 32 | u64::from(LOW)).to_le_bytes()[..size]);
        assert_eq!(client.bytes(OBJECT, 8), want, "store of {size}");
    }
}

#[test]
fn load_exchange_and_the_fetches_answer_the_old_value() {
    let old = [0x11, 0x2211, 0x4433_2211, 0x8877_6655_4433_2211_u64];
    let operand = u64::from(HIGH) << 32 | u64::from(LOW);
    let mask = |size: usize| {
        if size == 8 {
            u64::MAX
        } else {
            (1 << (8 * size)) - 1
        }
    };
    for (k, size) in [1, 2, 4, 8].into_iter().enumerate() {
        type Update = fn(u64, u64) -> u64;
        let groups: [(usize, Update); 6] = [
            (5, |old, _| old),
            (9, |_, v| v),
            (21, u64::wrapping_add),
            (25, |o, v| o ^ v),
            (29, |o, v| o | v),
            (33, |o, v| o & v),
        ];
        for (first, op) in groups {
            let mut client = fresh();
            let regs = client.call(routine(first + k), &[OBJECT, 5, LOW, HIGH]);
            let answered = if size == 8 {
                u64::from(regs.r[1]) << 32 | u64::from(regs.r[0])
            } else {
                u64::from(regs.r[0])
            };
            assert_eq!(answered, old[k], "{}: the old value", first + k);
            let held = u64::from_le_bytes(client.bytes(OBJECT, 8).try_into().expect("eight"));
            let want = op(old[k], operand & mask(size)) & mask(size) | old[3] & !mask(size);
            assert_eq!(held, want, "{}: the object after", first + k);
        }
    }
}

#[test]
fn compare_exchange_swaps_on_a_match_and_reports_the_object_otherwise() {
    for (k, size) in [1, 2, 4, 8].into_iter().enumerate() {
        let mut client = fresh();
        client.running();
        let stack = client.session.kernel.os().registers().r[13];
        client.put(stack, &HIGH.to_le_bytes());
        assert_eq!(
            client.answer(routine(STRONG + k), &[OBJECT, 5, EXPECTED, LOW]),
            1,
            "{size}: a match"
        );
        let mut want = PATTERN;
        want[..size]
            .copy_from_slice(&(u64::from(HIGH) << 32 | u64::from(LOW)).to_le_bytes()[..size]);
        assert_eq!(
            client.bytes(OBJECT, 8),
            want,
            "made the one desired, its high word from the stack"
        );
        assert_eq!(
            client.bytes(EXPECTED, 8),
            PATTERN,
            "the expected one untouched"
        );
        let mut client = fresh();
        client.running();
        assert_eq!(
            client.answer(routine(STRONG + k), &[OBJECT, 5, OTHER, LOW]),
            0,
            "{size}: no match"
        );
        assert_eq!(client.bytes(OBJECT, 8), PATTERN, "the object untouched");
        let mut reported = DIFFERENT;
        reported[..size].copy_from_slice(&PATTERN[..size]);
        assert_eq!(
            client.bytes(OTHER, 8),
            reported,
            "the expected made the object's"
        );
    }
    for n in 0..4 {
        assert!(
            matches!(ENTRIES[WEAK + n], Entry::Shared { chunk: 6, entry } if entry == STRONG + n),
            "weak is strong's"
        );
    }
}

#[test]
fn the_flag_is_a_word_set_to_one_answering_its_old_word_and_cleared_to_nought() {
    let mut client = fresh();
    assert_eq!(
        client.answer(routine(38), &[OBJECT, 5]),
        0x4433_2211,
        "the old word"
    );
    assert_eq!(
        client.bytes(OBJECT, 8),
        [1, 0, 0, 0, 0x55, 0x66, 0x77, 0x88]
    );
    assert_eq!(client.answer(routine(39), &[OBJECT, 5]), OBJECT, "R0 left");
    assert_eq!(client.bytes(OBJECT, 4), [0; 4]);
    assert_eq!(
        client.answer(routine(37), &[OBJECT, 5]),
        OBJECT,
        "a fence touches nothing"
    );
    for size in [1, 2, 3, 4, 8, 16, OBJECT] {
        assert_eq!(
            client.answer(routine(0), &[size, OBJECT]),
            0,
            "never lock-free"
        );
    }
}
