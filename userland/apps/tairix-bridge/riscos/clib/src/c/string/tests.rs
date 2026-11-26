use tairix_riscos_kernel::module::Code;

use super::*;
use crate::c::testing::{Client, FREE, SLOT_END, STATICS};

const S1: u32 = FREE;
const S2: u32 = FREE + 0x100;
const S3: u32 = FREE + 0x200;

fn signed(word: u32) -> i32 {
    word.cast_signed()
}

/// `a` at `S1 + a_offset` and `b` at `S2 + b_offset`, each with a NUL, and
/// `entry`'s signed answer for them.
fn compared(entry: Code, a: &[u8], a_offset: u32, b: &[u8], b_offset: u32, n: u32) -> i32 {
    let mut client = Client::new();
    client.put(S1 + a_offset, &[a, b"\0"].concat());
    client.put(S2 + b_offset, &[b, b"\0"].concat());
    signed(client.answer(entry, &[S1 + a_offset, S2 + b_offset, n]))
}

#[test]
fn strcmp_shifts_the_difference_by_its_place_in_the_word_when_both_are_aligned() {
    let cases: [(&[u8], &[u8], i32); 6] = [
        (b"abc", b"abd", -16384),
        (b"abc", b"abz", -376_832),
        (b"abz", b"abc", 376_832),
        (b"abc", b"ab", 1_622_016),
        (b"ab", b"abc", -1_622_016),
        (b"aXc", b"a\x42c", 22 << 7),
    ];
    for (a, b, expected) in cases {
        assert_eq!(compared(strcmp, a, 0, b, 0, 0), expected, "{a:?} {b:?}");
    }
    assert_eq!(
        compared(strcmp, b"abcdefgX", 0, b"abcdefgD", 0, 0),
        20 << 21,
        "the eighth byte"
    );
    assert_eq!(compared(strcmp, b"same", 0, b"same", 0, 0), 0);
}

#[test]
fn strcmp_gives_the_plain_difference_when_either_is_unaligned() {
    assert_eq!(compared(strcmp, b"\xFF", 1, b"abc", 0, 0), 158);
    assert_eq!(
        compared(strcmp, b"abc", 2, b"abd", 2, 0),
        -1,
        "both alike unaligned"
    );
    assert_eq!(compared(strcmp, b"abc", 0, b"ab", 3, 0), 99);
}

#[test]
fn memcmp_and_strncmp_give_the_first_differing_bytes_difference() {
    assert_eq!(compared(memcmp, b"abc", 0, b"abz", 0, 3), -23);
    assert_eq!(compared(memcmp, b"\xFF", 0, b"a", 0, 1), 158);
    assert_eq!(compared(memcmp, b"a", 0, b"\xFF", 0, 1), -158);
    assert_eq!(
        compared(memcmp, b"ab\0x", 0, b"ab\0y", 0, 4),
        -1,
        "past a NUL"
    );
    assert_eq!(compared(strncmp, b"abc", 0, b"abz", 0, 3), -23);
    assert_eq!(compared(strncmp, b"abc", 0, b"abz", 0, 2), 0);
    assert_eq!(
        compared(strncmp, b"ab\0x", 0, b"ab\0y", 0, 4),
        0,
        "not past a NUL"
    );
}

#[test]
fn memmove_copies_overlapping_ranges_either_way_and_answers_the_destination() {
    let mut client = Client::new();
    client.put(S1, b"0123456789");
    assert_eq!(client.answer(memmove, &[S1 + 2, S1, 6]), S1 + 2);
    assert_eq!(client.bytes(S1, 10), b"0101234589");
    client.put(S1, b"0123456789");
    client.call(memmove, &[S1, S1 + 3, 7]);
    assert_eq!(client.bytes(S1, 10), b"3456789789");
}

#[test]
fn memset_fills_with_the_low_byte_and_the_compiler_s_fill_with_the_whole_word() {
    let mut client = Client::new();
    assert_eq!(client.answer(memset, &[S1, 0x1234_56AB, 5]), S1);
    assert_eq!(client.bytes(S1, 6), b"\xAB\xAB\xAB\xAB\xAB\0");
    client.call(block_set, &[S2, 0x0403_0201, 600]);
    let filled = client.bytes(S2, 601);
    assert!(
        filled[..600].chunks(4).all(|word| word == [1, 2, 3, 4]),
        "across the chunks"
    );
    assert_eq!(filled[600], 0);
}

#[test]
fn the_copies_end_their_strings_and_strncpy_pads_with_noughts() {
    let mut client = Client::new();
    client.put(S2, b"hello\0");
    assert_eq!(client.answer(strcpy, &[S1, S2]), S1);
    assert_eq!(client.string(S1), b"hello");
    client.put(S3, b"XXXXXXXXXX");
    client.call(strncpy, &[S3, S2, 8]);
    assert_eq!(client.bytes(S3, 10), b"hello\0\0\0XX");
    client.put(S3, b"XXXX");
    client.call(strncpy, &[S3, S2, 3]);
    assert_eq!(
        client.bytes(S3, 4),
        b"helX",
        "no NUL where the source is longer"
    );
    client.put(S2 + 0x10, b", world\0");
    assert_eq!(client.answer(strcat, &[S1, S2 + 0x10]), S1);
    assert_eq!(client.string(S1), b"hello, world");
    client.put(S1, b"ab\0");
    client.call(strncat, &[S1, S2, 3]);
    assert_eq!(client.string(S1), b"abhel");
    client.call(strncat, &[S1, S2, 99]);
    assert_eq!(client.string(S1), b"abhelhello");
}

#[test]
fn the_searches_find_their_bytes() {
    let mut client = Client::new();
    client.put(S1, b"a,b;c,d\0");
    assert_eq!(client.answer(memchr, &[S1, u32::from(b';'), 8]), S1 + 3);
    assert_eq!(client.answer(memchr, &[S1, u32::from(b';'), 3]), 0);
    assert_eq!(
        client.answer(memchr, &[S1, 0x100 | u32::from(b'c'), 8]),
        S1 + 4,
        "the low byte"
    );
    assert_eq!(client.answer(strchr, &[S1, u32::from(b',')]), S1 + 1);
    assert_eq!(client.answer(strchr, &[S1, 0]), S1 + 7, "the NUL");
    assert_eq!(client.answer(strchr, &[S1, u32::from(b'z')]), 0);
    assert_eq!(client.answer(strrchr, &[S1, u32::from(b',')]), S1 + 5);
    assert_eq!(client.answer(strrchr, &[S1, 0]), S1 + 7);
    assert_eq!(client.answer(strrchr, &[S1, u32::from(b'z')]), 0);
    client.put(S2, b",;abcd\0");
    assert_eq!(client.answer(strspn, &[S1, S2]), 7, "every byte in the set");
    client.put(S2, b",;a\0");
    assert_eq!(client.answer(strspn, &[S1, S2]), 2);
    client.put(S2, b";\0");
    assert_eq!(client.answer(strcspn, &[S1, S2]), 3);
    assert_eq!(client.answer(strpbrk, &[S1, S2]), S1 + 3);
    client.put(S2, b"xyz\0");
    assert_eq!(client.answer(strpbrk, &[S1, S2]), 0);
    assert_eq!(client.answer(strcspn, &[S1, S2]), 7);
    assert_eq!(client.answer(strlen, &[S1]), 7);
}

#[test]
fn strstr_finds_the_first_whole_match() {
    let mut client = Client::new();
    client.put(S1, b"abababc\0");
    for (needle, expected) in [
        (&b"abc"[..], S1 + 4),
        (b"bab", S1 + 1),
        (b"", S1),
        (b"abcd", 0),
        (b"x", 0),
        (b"c", S1 + 6),
    ] {
        client.put(S2, &[needle, b"\0"].concat());
        assert_eq!(client.answer(strstr, &[S1, S2]), expected, "{needle:?}");
    }
}

#[test]
fn strtok_walks_the_tokens_and_remembers_where_it_left_off() {
    let mut client = Client::new();
    client.put(S1, b";;one;two,,three;\0");
    client.put(S2, b";,\0");
    let first = client.answer(strtok, &[S1, S2]);
    assert_eq!(client.string(first), b"one");
    let second = client.answer(strtok, &[0, S2]);
    assert_eq!(client.string(second), b"two");
    let third = client.answer(strtok, &[0, S2]);
    assert_eq!(client.string(third), b"three");
    assert_eq!(client.answer(strtok, &[0, S2]), 0);
    assert_eq!(client.answer(strtok, &[0, S2]), 0, "and stays at the end");
    client.put(S1, b";;\0");
    assert_eq!(client.answer(strtok, &[S1, S2]), 0, "no token at all");
}

#[test]
fn strerror_writes_its_text_in_the_library_s_buffer() {
    let mut client = Client::new();
    let buffer = STATICS + at::ERROR_TEXT;
    for (n, token) in ERRNO_TOKENS {
        assert_eq!(client.answer(strerror, &[n]), buffer, "errno {n}");
        let text = client.string(buffer);
        if n > 0 {
            assert!(text.starts_with(token.as_bytes()), "{text:?}");
        }
    }
    assert_eq!(client.answer(strerror, &[99]), buffer);
    assert!(!client.string(buffer).is_empty());
    client.put(STATICS + kernel_at::LAST_ERROR + 4, b"An OS error\0");
    let last = client.answer(strerror, &[OS_REFUSED]);
    assert_eq!(
        client.string(last),
        b"An OS error",
        "the last OS error's own text"
    );
}

#[test]
fn nothing_is_read_past_the_page_a_string_ends_in() {
    let mut client = Client::new();
    let end = SLOT_END - 4;
    client.put(end, b"abc\0");
    client.put(S2, b"abc\0");
    assert_eq!(client.answer(strcmp, &[end, S2]), 0);
    assert_eq!(client.answer(strcmp, &[S2, end]), 0);
    assert_eq!(client.answer(strlen, &[end]), 3);
    client.put(S3, b"bcdefgh\0");
    assert_eq!(client.answer(strstr, &[end, S3]), 0);
    assert_eq!(client.answer(strncmp, &[end, S2, 99]), 0);
}
