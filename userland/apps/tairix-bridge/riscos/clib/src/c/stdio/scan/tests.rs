extern crate std;

use std::format;
use std::string::String;
use std::vec::Vec;

use tairix_fscopy::fixture::Tree;
use tairix_fuzzseed::Prng;
use tairix_riscos_filing::session::DISC;

use super::*;
use crate::c::locale::start_in_c;
use crate::c::stdio::fgetc;
use crate::c::stdio::open::fopen;
use crate::c::testing::{Client, FREE, STATICS};

mod recorded;

use recorded::SSCANF;

const INPUT: u32 = FREE;
const FORMAT: u32 = FREE + 0x400;
const NAME: u32 = FREE + 0x480;
const MODE: u32 = FREE + 0x4C0;
/// Two targets of sixteen bytes, every byte `&AA` before a call.
const TARGETS: u32 = FREE + 0x500;
const TARGET_BYTES: u32 = 16;

fn client() -> Client {
    let mut client = Client::new();
    start_in_c(&mut client.session.kernel.os(), Statics::of_base(STATICS)).expect("the C locale");
    client
}

/// `format` scanned from `input` into the two targets: the answer, and the
/// targets' bytes.
fn scanned(client: &mut Client, input: &[u8], format: &[u8]) -> (i32, Vec<u8>) {
    client.put(INPUT, &[input, b"\0"].concat());
    client.put(FORMAT, &[format, b"\0"].concat());
    client.put(TARGETS, &[0xAA; 2 * TARGET_BYTES as usize]);
    let target = |n: u32| TARGETS + n * TARGET_BYTES;
    let answer = client.answer(sscanf, &[INPUT, FORMAT, target(0), target(1)]);
    (
        answer.cast_signed(),
        client.bytes(TARGETS, 2 * TARGET_BYTES as usize),
    )
}

/// Two targets holding `stored`, `&AA` past it.
fn targets(stored: &[&[u8]]) -> Vec<u8> {
    let mut bytes = std::vec![0xAA; 2 * TARGET_BYTES as usize];
    for (target, stored) in bytes.chunks_mut(TARGET_BYTES as usize).zip(stored) {
        target[..stored.len()].copy_from_slice(stored);
    }
    bytes
}

#[test]
fn every_recorded_sscanf_answers_as_genuine_does() {
    let mut client = client();
    let mut disagreements = Vec::new();
    for (input, format, answer, stored) in SSCANF {
        let ours = scanned(&mut client, input, format);
        if ours != (answer, targets(stored)) {
            let (input, format) = (input.escape_ascii(), format.escape_ascii());
            disagreements.push(format!(
                "{input:?} {format:?}: {ours:02X?}, not {answer} {stored:02X?}"
            ));
        }
    }
    assert!(
        disagreements.is_empty(),
        "{} of {}: {disagreements:#?}",
        disagreements.len(),
        SSCANF.len()
    );
}

#[test]
fn the_input_ending_before_a_conversion_answers_eof_and_after_one_the_count() {
    let mut client = client();
    assert_eq!(
        scanned(&mut client, b"", b"a").0,
        -1,
        "a literal at the end of the input"
    );
    assert_eq!(
        scanned(&mut client, b"b", b"a").0,
        0,
        "a literal that does not match"
    );
    assert_eq!(
        scanned(&mut client, b"7", b"%d %d").0,
        1,
        "the second conversion's input ended"
    );
    assert_eq!(
        scanned(&mut client, b"x", b"%2c").0,
        -1,
        "a character short of the first conversion"
    );
    assert_eq!(
        scanned(&mut client, b"x", b"%c%c").0,
        1,
        "a character short of the second"
    );
    assert_eq!(
        scanned(&mut client, b"  ", b"%s").0,
        -1,
        "nothing but white space before a string"
    );
}

#[test]
fn a_suppressed_conversion_takes_its_input_and_no_target() {
    let mut client = client();
    let (answer, bytes) = scanned(&mut client, b"skip keep 9", b"%*s %s %*d%n");
    assert_eq!(answer, 1);
    assert_eq!(&bytes[..5], b"keep\0");
    assert_eq!(
        &bytes[16..20],
        &11u32.to_le_bytes(),
        "%n's target was the next argument"
    );
}

#[test]
fn a_width_takes_what_is_a_whole_number_within_it() {
    let mut client = client();
    let (answer, bytes) = scanned(&mut client, b"infinity", b"%3lf%s");
    assert_eq!(answer, 2, "\"inf\" is a number on its own");
    assert_eq!(
        &bytes[..8],
        &fpa_words(f64::INFINITY.to_bits())
            .map(u32::to_le_bytes)
            .concat()[..]
    );
    assert_eq!(&bytes[16..22], b"inity\0");
    let (answer, bytes) = scanned(&mut client, b"12345", b"%2x%3d");
    assert_eq!(
        (answer, &bytes[..4], &bytes[16..20]),
        (2, &[0x12, 0, 0, 0][..], &345u32.to_le_bytes()[..])
    );
}

/// A stream opened to read on the disc's file `name`.
fn stream_on(client: &mut Client, name: &str) -> u32 {
    client.put(
        NAME,
        &[format!("HostFS:$.{name}").as_bytes(), b"\0"].concat(),
    );
    client.put(MODE, b"r\0");
    let stream = client.answer(fopen, &[NAME, MODE]);
    assert_ne!(stream, 0, "{name} opened");
    stream
}

#[test]
fn a_stream_keeps_the_character_a_scan_stopped_at_and_loses_what_a_failed_conversion_took() {
    let tree = Tree::new()
        .dir(DISC)
        .file(&format!("{DISC}/In"), b"12 34 1e+x");
    let mut client = Client::with_streams(&tree, &[]);
    let stream = stream_on(&mut client, "In");
    client.put(FORMAT, b"%d\0");
    assert_eq!(client.answer(fscanf, &[stream, FORMAT, TARGETS]), 1);
    assert_eq!(client.word(TARGETS), 12);
    assert_eq!(
        client.answer(fgetc, &[stream]),
        u32::from(b' '),
        "the character after the number given back"
    );
    client.put(FORMAT, b"%d %lf\0");
    assert_eq!(
        client.answer(fscanf, &[stream, FORMAT, TARGETS, TARGETS + 16]),
        1
    );
    assert_eq!(
        client.answer(fgetc, &[stream]),
        u32::from(b'x'),
        "\"1e+\" lost, the x given back"
    );
    assert_eq!(client.answer(fgetc, &[stream]), EOF);
}

#[test]
fn integers_read_back_what_the_host_writes() {
    let mut prng = Prng::new(tairix_fuzzseed::start(
        "sscanf_integers",
        "TAIRIX_CLIB_SSCANF_SEED",
    ));
    let mut client = client();
    for _ in 0..2_000 {
        let value = prng.next_u64();
        let narrow = u32::try_from(value >> 32).expect("the high word");
        for (format, text, expected) in [
            (&b"%d"[..], format!("{}", narrow.cast_signed()), narrow),
            (b"%u", format!("{narrow}"), narrow),
            (b"%x", format!("{narrow:x}"), narrow),
            (b"%X", format!("0X{narrow:X}"), narrow),
            (b"%o", format!("{narrow:o}"), narrow),
            (b"%i", format!("0{narrow:o}"), narrow),
            (b"%i", format!("-0x{narrow:x}"), narrow.wrapping_neg()),
        ] {
            let (answer, bytes) = scanned(&mut client, text.as_bytes(), format);
            assert_eq!(
                (answer, &bytes[..4]),
                (1, &expected.to_le_bytes()[..]),
                "{text} {:?}",
                format.escape_ascii()
            );
        }
        let text = format!("{}", value.cast_signed());
        let (answer, bytes) = scanned(&mut client, text.as_bytes(), b"%lld");
        assert_eq!(
            (answer, &bytes[..8]),
            (1, &value.to_le_bytes()[..]),
            "{text}"
        );
    }
}

#[test]
fn floating_point_numbers_read_back_the_double_the_host_reads_and_its_float() {
    let mut prng = Prng::new(tairix_fuzzseed::start(
        "sscanf_reals",
        "TAIRIX_CLIB_SSCANF_REAL_SEED",
    ));
    let mut client = client();
    for _ in 0..2_000 {
        let x = f64::from_bits(prng.next_u64());
        if !x.is_finite() {
            continue;
        }
        for text in [format!("{x:e}"), format!("{x}"), format!("{x:.3e}")] {
            let read: f64 = text.parse().expect("the host reads it");
            let (answer, bytes) = scanned(&mut client, text.as_bytes(), b"%lf");
            let double = fpa_words(read.to_bits()).map(u32::to_le_bytes).concat();
            assert_eq!((answer, &bytes[..8]), (1, &double[..]), "{text}");
            #[expect(
                clippy::cast_possible_truncation,
                reason = "a float conversion rounds the double read to a float"
            )]
            let single = read as f32;
            let (answer, bytes) = scanned(&mut client, text.as_bytes(), b"%f");
            assert_eq!(
                (answer, &bytes[..4]),
                (1, &single.to_bits().to_le_bytes()[..]),
                "{text} as a float"
            );
        }
    }
}

#[test]
fn a_float_keeps_a_nan_s_sign_and_payload_s_top_bits() {
    let mut client = client();
    let (answer, bytes) = scanned(&mut client, b"-nan", b"%f");
    assert_eq!(
        (answer, &bytes[..4]),
        (1, &0xFFC0_0001u32.to_le_bytes()[..]),
        "the payload's top bits kept"
    );
}

#[test]
fn strings_and_scansets_read_back_what_was_written() {
    let mut prng = Prng::new(tairix_fuzzseed::start(
        "sscanf_strings",
        "TAIRIX_CLIB_SSCANF_TEXT_SEED",
    ));
    let mut client = client();
    for _ in 0..1_000 {
        let length = 1 + prng.below(TARGET_BYTES as usize - 1);
        let word: String = (0..length)
            .map(|_| char::from(b'!' + u8::try_from(prng.below(94)).expect("printable")))
            .collect();
        let (answer, bytes) = scanned(&mut client, format!("  {word} tail").as_bytes(), b"%s");
        assert_eq!(
            (answer, &bytes[..=length]),
            (1, &[word.as_bytes(), b"\0"].concat()[..]),
            "{word}"
        );
        let (answer, bytes) = scanned(&mut client, format!("{word}\n").as_bytes(), b"%[^\n]");
        assert_eq!(
            (answer, &bytes[..=length]),
            (1, &[word.as_bytes(), b"\0"].concat()[..]),
            "{word}"
        );
    }
}
