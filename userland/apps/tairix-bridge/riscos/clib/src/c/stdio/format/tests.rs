extern crate std;

use std::format;
use std::string::String;
use std::vec::Vec;

use tairix_fuzzseed::Prng;

use super::*;
use crate::c::testing::{Client, FREE};

mod recorded;

use recorded::SPRINTF;

/// An argument as a recorded case gives it: a word, a double's two words,
/// a string's address, or `%n`'s place.
#[derive(Clone, Copy, Debug)]
pub(super) enum Arg {
    Word(u32),
    Double(u64),
    Text(&'static [u8]),
    Count,
}

const FORMAT: u32 = FREE;
const COUNT: u32 = FREE + 0x3F0;
const STRINGS: u32 = FREE + 0x400;

struct Words {
    words: Vec<u32>,
    next: usize,
}

impl Arguments for Words {
    fn word(&mut self, _os: &mut Os<'_>) -> Result<u32, Fail> {
        let word = self.words.get(self.next).copied().unwrap_or(0);
        self.next += 1;
        Ok(word)
    }

    /// Host-side words lie nowhere in guest memory; a format never asks.
    fn rest(&mut self, _os: &mut Os<'_>) -> Result<u32, Fail> {
        Ok(0)
    }
}

struct Collected(Vec<u8>);

impl Sink for Collected {
    fn put(&mut self, _os: &mut Os<'_>, bytes: &[u8]) -> Result<(), Fail> {
        self.0.extend_from_slice(bytes);
        Ok(())
    }
}

/// A sink that keeps nothing, as `snprintf` into no room only counts.
struct Counted(usize);

impl Sink for Counted {
    fn put(&mut self, _os: &mut Os<'_>, bytes: &[u8]) -> Result<(), Fail> {
        self.0 += bytes.len();
        Ok(())
    }

    fn keeps(&self) -> usize {
        0
    }

    fn count(&mut self, bytes: usize) {
        self.0 += bytes;
    }
}

/// `format` written with `args`: its answer and its text.
fn formatted(client: &mut Client, format: &[u8], args: &[Arg], floats: Floats) -> (u32, Vec<u8>) {
    let mut sink = Collected(Vec::new());
    let written = written_to(client, format, args, floats, &mut sink);
    (written, sink.0)
}

/// `format` written with `args` to `sink`: its answer.
fn written_to(
    client: &mut Client,
    format: &[u8],
    args: &[Arg],
    floats: Floats,
    sink: &mut impl Sink,
) -> u32 {
    client.put(FORMAT, &[format, b"\0"].concat());
    let mut words = Vec::new();
    let mut strings = STRINGS;
    for &arg in args {
        match arg {
            Arg::Word(word) => words.push(word),
            Arg::Double(bits) => words.extend(tairix_softfloat::fpa_words(bits)),
            Arg::Text(text) => {
                client.put(strings, &[text, b"\0"].concat());
                words.push(strings);
                strings += u32::try_from(text.len()).expect("short") + 1;
            }
            Arg::Count => words.push(COUNT),
        }
    }
    let mut args = Words { words, next: 0 };
    super::format(
        &mut client.session.kernel.os(),
        FORMAT,
        &mut args,
        sink,
        floats,
    )
    .expect("formatted")
}

#[test]
fn a_width_or_precision_past_int_max_is_int_max_and_counting_it_costs_nothing() {
    const MOST: u32 = i32::MAX.cast_unsigned();
    let mut client = Client::new();
    let one = Arg::Double(1.0f64.to_bits());
    for (format, args, answer) in [
        (&b"%99999999999d"[..], &[Arg::Word(1)][..], MOST),
        (b"%-*d", &[Arg::Word(0x8000_0000), Arg::Word(1)], MOST),
        (b"%.99999999999999999999e", &[one], MOST + 6),
        (b"%.*f", &[Arg::Word(MOST), one], MOST + 2),
        (b"%.99999999999x", &[Arg::Word(1)], MOST),
    ] {
        let mut sink = Counted(0);
        let written = written_to(&mut client, format, args, Floats::Converted, &mut sink);
        let shown = format.escape_ascii();
        assert_eq!(written, answer, "{shown}");
        assert_eq!(sink.0, answer as usize, "{shown}: every byte counted");
    }
}

#[test]
fn a_string_s_precision_bounds_what_is_read_of_it() {
    let mut client = Client::new();
    let end = crate::c::testing::SLOT_END;
    assert!(
        client.session.kernel.os().read_byte(end).is_err(),
        "nothing past the slot"
    );
    client.put(end - 3, b"abc");
    let (written, text) = formatted(
        &mut client,
        b"[%.3s|%.0s|%.2s]",
        &[Arg::Word(end - 3), Arg::Word(end), Arg::Word(end - 3)],
        Floats::Converted,
    );
    assert_eq!((written, text.as_slice()), (9, &b"[abc||ab]"[..]));
}

#[test]
fn literal_text_longer_than_a_run_is_written_whole() {
    let mut client = Client::new();
    let long: Vec<u8> = (0..9000u32).map(|n| b'a' + (n % 26) as u8).collect();
    let (written, text) = formatted(
        &mut client,
        &[&long[..], b"%d!"].concat(),
        &[Arg::Word(42)],
        Floats::Converted,
    );
    assert_eq!(written, 9003);
    assert_eq!(text, [&long[..], b"42!"].concat());
}

#[test]
fn every_recorded_sprintf_formats_as_genuine_does() {
    let mut client = Client::new();
    let mut disagreements = Vec::new();
    for (format, args, answer, text) in SPRINTF {
        let (written, ours) = formatted(&mut client, format, args, Floats::Converted);
        // A recording keeps the first of a long text only.
        let shown = &ours[..ours.len().min(text.len())];
        if written.cast_signed() != answer || shown != text {
            let (format, ours, text) = (
                format.escape_ascii(),
                ours.escape_ascii(),
                text.escape_ascii(),
            );
            disagreements.push(format!(
                "{format} {args:?}: {written} {ours} for {answer} {text}"
            ));
        }
    }
    assert!(
        disagreements.is_empty(),
        "{} of {}: {disagreements:#?}",
        disagreements.len(),
        SPRINTF.len()
    );
}

#[test]
fn n_stores_the_count_so_far_in_its_length_s_size() {
    let mut client = Client::new();
    for (format, stored) in [
        (&b"ab%ncd"[..], [2, 0, 0, 0, 0xAA, 0xAA, 0xAA, 0xAA]),
        (b"abc%hhn", [3, 0xAA, 0xAA, 0xAA, 0xAA, 0xAA, 0xAA, 0xAA]),
        (b"%lln", [0; 8]),
    ] {
        client.put(COUNT, &[0xAA; 8]);
        formatted(&mut client, format, &[Arg::Count], Floats::Converted);
        assert_eq!(
            client.bytes(COUNT, 8),
            stored,
            "{:?}",
            format.escape_ascii()
        );
    }
}

#[test]
fn the_float_less_family_writes_the_text_at_address_nought_for_a_float() {
    let mut client = Client::new();
    let nought = client
        .session
        .kernel
        .os()
        .read_until(0, ZERO_PAGE_TEXT, |b| b == 0)
        .expect("zero page")
        .0;
    let args = [Arg::Word(1), Arg::Double(1.5f64.to_bits()), Arg::Word(2)];
    let (written, text) = formatted(&mut client, b"%d|%5.2f|%d", &args, Floats::Absent);
    let expected = [&b"1|"[..], &nought, b"|2"].concat();
    assert_eq!(
        (written as usize, text),
        (expected.len(), expected),
        "the double's words taken, no width"
    );
}

#[test]
fn padding_and_precision_run_past_any_buffer() {
    let mut client = Client::new();
    let (written, text) = formatted(
        &mut client,
        b"%300d|%.300d|%-300s|",
        &[Arg::Word(7), Arg::Word(8), Arg::Text(b"x")],
        Floats::Converted,
    );
    assert_eq!(written, 903);
    assert_eq!(&text[..300], [&[b' '; 299][..], b"7"].concat());
    assert_eq!(&text[301..601], [&[b'0'; 299][..], b"8"].concat());
    assert_eq!(&text[602..902], [&b"x"[..], &[b' '; 299]].concat());
    let (written, text) = formatted(
        &mut client,
        b"%.400f",
        &[Arg::Double(1.0f64.to_bits())],
        Floats::Converted,
    );
    assert_eq!(
        (written, &text[..2], text[2..].iter().all(|&b| b == b'0')),
        (402, &b"1."[..], true)
    );
}

/// The host's seventeen correctly rounded digits of `x` and the exponent of
/// the first.
fn host_digits(x: f64) -> (String, i32) {
    let text = format!("{:.16e}", x.abs());
    let (mantissa, exponent) = text.split_once('e').expect("an exponent");
    (
        mantissa.replace('.', ""),
        exponent.parse().expect("a number"),
    )
}

#[test]
fn seventeen_digits_are_the_correctly_rounded_ones_for_any_double() {
    let mut prng = Prng::new(tairix_fuzzseed::start(
        "printf_digits",
        "TAIRIX_CLIB_PRINTF_SEED",
    ));
    let mut client = Client::new();
    for _ in 0..3_000 {
        let x = loop {
            let x = f64::from_bits(prng.next_u64());
            if x.is_finite() && x != 0.0 {
                break x;
            }
        };
        let (_, text) = formatted(
            &mut client,
            b"%.16e",
            &[Arg::Double(x.to_bits())],
            Floats::Converted,
        );
        let text = String::from_utf8(text).expect("text");
        let (digits, exponent) = host_digits(x);
        let sign = if x < 0.0 { "-" } else { "" };
        let expected = format!(
            "{sign}{}.{}e{}{:02}",
            &digits[..1],
            &digits[1..],
            if exponent < 0 { '-' } else { '+' },
            exponent.unsigned_abs()
        );
        assert_eq!(text, expected, "{x:e}");
    }
}
