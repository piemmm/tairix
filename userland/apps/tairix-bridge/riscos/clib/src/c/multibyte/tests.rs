use alloc::vec::Vec;

use super::*;
use crate::c::testing::{Client, FREE};

const S: u32 = FREE;
const W: u32 = FREE + 0x100;
const OUT: u32 = FREE + 0x200;

#[test]
fn a_character_is_its_byte() {
    let mut client = Client::new();
    client.put(S, b"A\0\xE9\0");
    assert_eq!(client.answer(mblen, &[S, 2]), 1);
    assert_eq!(client.answer(mblen, &[S + 1, 1]), 0, "the NUL");
    assert_eq!(client.answer(mblen, &[0, 0]), 0, "no shift state");
    assert_eq!(client.answer(mblen, &[S, 0]), NONE);
    assert_eq!(client.answer(mbtowc, &[W, S + 2, 1]), 1);
    assert_eq!(client.word(W), 0xE9);
    assert_eq!(client.answer(mbtowc, &[W, S + 1, 5]), 0);
    assert_eq!(client.word(W), 0, "the NUL stored");
    assert_eq!(client.answer(mbtowc, &[W, S, 0]), NONE);
    assert_eq!(client.answer(mbtowc, &[0, 0, 0]), 0);
    assert_eq!(client.answer(wctomb, &[OUT, 0xE9]), 1);
    assert_eq!(client.bytes(OUT, 1), [0xE9]);
    assert_eq!(
        client.answer(wctomb, &[OUT, 0x1E9]),
        NONE,
        "no byte holds it"
    );
    assert_eq!(client.answer(wctomb, &[0, 0]), 0);
}

#[test]
fn strings_convert_to_the_nul_or_the_limit() {
    let mut client = Client::new();
    client.put(S, b"abc\0");
    assert_eq!(client.answer(mbstowcs, &[W, S, 10]), 3);
    assert_eq!(
        [client.word(W), client.word(W + 12)],
        [u32::from(b'a'), 0],
        "the NUL too"
    );
    client.put(W, &[0xEE; 16]);
    assert_eq!(client.answer(mbstowcs, &[W, S, 2]), 2);
    assert_eq!(client.word(W + 8), 0xEEEE_EEEE, "nothing past the limit");
    client.put(W, &[0x61, 0, 0, 0, 0x62, 0, 0, 0, 0, 0, 0, 0]);
    assert_eq!(client.answer(wcstombs, &[OUT, W, 10]), 2);
    assert_eq!(client.string(OUT), b"ab");
    client.put(OUT, b"XXX");
    assert_eq!(client.answer(wcstombs, &[OUT, W, 1]), 1);
    assert_eq!(client.bytes(OUT, 2), b"aX", "no NUL past the limit");
    client.put(W + 4, &0x1E9_u32.to_le_bytes());
    assert_eq!(client.answer(wcstombs, &[OUT, W, 10]), NONE);
}

#[test]
fn wide_strings_convert_across_chunks_and_pages_each_byte_before_a_stop_written() {
    let mut client = Client::new();
    // Unaligned, one word straddling the page boundary at FREE + 0x1000.
    let wide = FREE + 0x1000 - 6;
    let out = FREE + 0x2000;
    let text: Vec<u8> = (0u8..150).map(|n| b'a' + n % 26).collect();
    let words: Vec<u8> = text
        .iter()
        .flat_map(|&b| u32::from(b).to_le_bytes())
        .chain([0; 4])
        .collect();
    client.put(wide, &words);
    assert_eq!(client.answer(wcstombs, &[out, wide, 1000]), 150);
    assert_eq!(client.bytes(out, 151), [&text[..], b"\0"].concat());
    client.put(out, &[0xEE; 151]);
    client.put(wide + 4 * 120, &0x1E9_u32.to_le_bytes());
    assert_eq!(client.answer(wcstombs, &[out, wide, 1000]), NONE);
    assert_eq!(client.bytes(out, 121), [&text[..120], &[0xEE]].concat());
    let end = crate::c::testing::SLOT_END - 40;
    client.put(end, &words[..40]);
    client.put(out, &[0xEE; 20]);
    assert!(
        client.outcome(wcstombs, &[out, end, 20]).is_err(),
        "aborted past the end"
    );
    assert_eq!(client.bytes(out, 11), [&text[..10], &[0xEE]].concat());
}

#[test]
fn a_long_string_converts_across_chunks() {
    let mut client = Client::new();
    let text: Vec<u8> = (0u8..150).map(|n| b'a' + n % 26).chain([0]).collect();
    client.put(S, &text);
    assert_eq!(client.answer(mbstowcs, &[W, S, 1000]), 150);
    assert_eq!(client.word(W + 4 * 149), u32::from(text[149]));
    assert_eq!(client.word(W + 4 * 150), 0);
}
