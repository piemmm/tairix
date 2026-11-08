//! Tests for the shared fallible allocation helpers.

use super::{
    boxed_array, collected, concatenated, copied, filled, grow_to, push, read_snapshot, reserve,
    trim, try_push, written, zeroed,
};
use alloc::vec::Vec;
use tairix_abi::Errno;

/// A count whose layout of any `T` wider than a byte cannot be reserved on
/// any target: the byte size overflows, so the refusal is the reservation's
/// own answer rather than a property of the host's free memory.
const UNRESERVABLE: usize = usize::MAX / 2;

/// [`UNRESERVABLE`] for bytes: past the most bytes any layout may span, so it
/// is refused before an allocator is asked — `usize::MAX / 2` bytes is a
/// layout an allocator, or an interpreter checking these tests, is handed.
const UNRESERVABLE_BYTES: usize = usize::MAX;

#[test]
fn a_refused_reservation_is_answered_not_aborted() {
    assert_eq!(filled::<u32>(UNRESERVABLE, 0), None);
    assert_eq!(collected(UNRESERVABLE, core::iter::repeat(0_u32)), None);
    let mut scratch: Vec<u32> = Vec::new();
    assert!(!grow_to(&mut scratch, UNRESERVABLE, 0));
    assert!(scratch.is_empty(), "a refused growth leaves the scratch");
    let mut buffer: Vec<u32> = Vec::new();
    assert!(!reserve(&mut buffer, UNRESERVABLE));
}

#[test]
fn a_copy_holds_exactly_what_it_copied() {
    let copy = copied(&[3_u16, 1, 4]).expect("granted");
    assert_eq!((copy.as_slice(), copy.capacity()), (&[3, 1, 4][..], 3));
    assert_eq!(copied::<u8>(&[]), Some(Vec::new()));
}

#[test]
fn zeroed_bytes_are_zero_and_exactly_as_many_as_asked() {
    assert_eq!(zeroed(0), Some(Vec::new()));
    let bytes = zeroed(64 * 1024).expect("granted");
    assert_eq!((bytes.len(), bytes.capacity()), (64 * 1024, 64 * 1024));
    assert!(bytes.iter().all(|&byte| byte == 0));
    assert_eq!(zeroed(usize::MAX), None, "no layout holds that many");
}

#[test]
fn a_list_pushed_an_item_at_a_time_grows_by_doubling() {
    let mut list: Vec<u32> = Vec::new();
    let mut grown = 0;
    for item in 0..4096 {
        let before = list.capacity();
        assert!(push(&mut list, item));
        grown += usize::from(list.capacity() != before);
    }
    assert_eq!(list.len(), 4096);
    assert_eq!(list[4095], 4095);
    assert!(grown <= 13, "{grown} reallocations for 4096 pushes");
    let mut more: Vec<u32> = Vec::new();
    let mut grown = 0;
    for item in 0..4096 {
        let before = more.capacity();
        assert_eq!(try_push(&mut more, item), Ok(()));
        grown += usize::from(more.capacity() != before);
    }
    assert_eq!(more, list);
    assert!(grown <= 13, "{grown} reallocations for 4096 pushes");
}

/// A written string is exactly what its writer wrote, in a string of
/// exactly that length; a writer writing more the second time fails rather
/// than growing the string.
#[test]
fn a_written_string_is_reserved_to_what_its_writer_writes() {
    let (name, uid) = ("ada", 1000);
    let line = written(|out| write!(out, "{name}:{uid}")).expect("held");
    assert_eq!(line, "ada:1000");
    assert_eq!(line.capacity(), line.len());
    let calls = core::cell::Cell::new(0);
    let growing = written(|out| {
        calls.set(calls.get() + 1);
        (0..calls.get()).try_for_each(|_| out.write_str("more"))
    });
    assert_eq!(growing, None);
    assert_eq!(written(|_| Err(core::fmt::Error)), None);
}

#[test]
fn a_boxed_array_holds_its_copies() {
    assert_eq!(boxed_array::<u8, 4>(5).as_deref(), Some(&[5; 4]));
}

#[test]
fn a_granted_reservation_holds_exactly_what_was_asked_for() {
    assert_eq!(filled(3, 7_u8), Some(alloc::vec![7, 7, 7]));
    assert_eq!(
        collected(2, [1_u8, 2, 3].into_iter()),
        Some(alloc::vec![1, 2])
    );
    let mut scratch = alloc::vec![1_u8, 2];
    assert!(grow_to(&mut scratch, 4, 9));
    assert_eq!(scratch, alloc::vec![1, 2, 9, 9]);
    assert!(
        grow_to(&mut scratch, 1, 9),
        "a scratch already long enough is left alone"
    );
    assert_eq!(scratch.len(), 4);
}

#[test]
fn parts_are_joined_into_a_string_of_exactly_their_length() {
    let joined = concatenated(&["/System", "/", "icon", ".png"]).expect("room");
    assert_eq!(joined, "/System/icon.png");
    assert_eq!(joined.capacity(), joined.len());
    assert_eq!(concatenated(&[]).as_deref(), Some(""));
}

#[test]
fn a_whole_read_grows_until_the_snapshot_fits() {
    let snapshot = [7u8; 1000];
    let mut reads = 0;
    let mut buffer = Vec::new();
    let len = read_snapshot(&mut buffer, 64, |into| {
        reads += 1;
        let whole = into
            .get_mut(..snapshot.len())
            .ok_or(Errno::BufferTooSmall)?;
        whole.copy_from_slice(&snapshot);
        Ok(snapshot.len())
    });
    assert_eq!(len, Ok(1000));
    assert_eq!(&buffer[..1000], &snapshot[..]);
    assert_eq!(reads, 5, "64, 128, 256, 512, then 1024 bytes");
}

#[test]
fn a_whole_read_answers_any_other_refusal_at_once() {
    let mut reads = 0;
    let mut buffer = Vec::new();
    let refused = read_snapshot(&mut buffer, 16, |_| {
        reads += 1;
        Err(Errno::PermissionDenied)
    });
    assert_eq!(refused, Err(Errno::PermissionDenied));
    assert_eq!(reads, 1);
}

#[test]
fn a_whole_read_keeps_a_buffer_already_grown() {
    let mut buffer = alloc::vec![0u8; 4096];
    let len = read_snapshot(&mut buffer, 16, |into| Ok(into.len()));
    assert_eq!(len, Ok(4096), "a reused buffer starts at its own size");
}

#[test]
fn a_buffer_holds_the_bytes_of_its_capacity() {
    let mut words: Vec<u32> = Vec::with_capacity(10);
    words.push(1);
    assert_eq!(
        super::held(&words),
        40,
        "what was reserved, not what is used"
    );
    assert_eq!(super::held(&Vec::<u64>::new()), 0);
}

#[test]
fn a_trimmed_buffer_holds_exactly_its_items() {
    let mut words: Vec<u32> = Vec::with_capacity(64);
    words.extend([3, 1, 4]);
    trim(&mut words);
    assert_eq!(
        (words.as_slice(), words.capacity()),
        ([3, 1, 4].as_slice(), 3)
    );
    let mut empty: Vec<u32> = Vec::with_capacity(8);
    trim(&mut empty);
    assert_eq!(empty.capacity(), 0, "an empty buffer lets its memory go");
}

#[test]
fn an_exact_buffer_is_left_where_it_is() {
    let mut words: Vec<u32> = Vec::with_capacity(3);
    words.extend([2, 7, 1]);
    let at = words.as_ptr();
    trim(&mut words);
    assert_eq!(words.as_ptr(), at, "nothing to trim is no copy");
}

#[test]
fn text_grows_by_what_is_written_and_keeps_it() {
    use core::fmt::Write as _;
    let mut text = super::Text::new();
    assert!(text.is_empty());
    let (number, word) = (12, "ab");
    write!(text, "{number}-{word}").expect("room");
    text.write_char('é').expect("room");
    assert_eq!(text.as_str(), "12-abé");
    assert_eq!(text.len(), "12-abé".len());
    text.reserve(64).expect("room");
    assert_eq!(text.into_string(), "12-abé");
    let mut refused = super::Text::new();
    assert!(
        refused.reserve(UNRESERVABLE_BYTES).is_err(),
        "a refusal is answered"
    );
    assert!(refused.is_empty(), "and leaves the text as it was");
}
