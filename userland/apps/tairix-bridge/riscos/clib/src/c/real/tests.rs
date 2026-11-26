extern crate std;

use std::format;
use std::string::String;
use std::vec::Vec;

use tairix_fuzzseed::Prng;
use tairix_riscos_kernel::module::Code;

use super::*;
use crate::c::locale::start_in_c;
use crate::c::testing::{Client, FREE, STATICS};

mod recorded;

use recorded::{ATOF, STRTOD};

const END: u32 = FREE;
const POINT: u32 = FREE + 4;
const TEXT: u32 = FREE + 0x10;

fn client() -> Client {
    let mut client = Client::new();
    start_in_c(&mut client.session.kernel.os(), Statics::of_base(STATICS)).expect("the C locale");
    client
}

/// `entry` given `text` and [`END`], `errno` `errno_before` first: F0's bits
/// after, how far the text read runs, and `errno`.
fn read_text(client: &mut Client, entry: Code, text: &[u8], errno_before: u32) -> (u64, u32, u32) {
    client.put(TEXT, &[text, b"\0"].concat());
    client.put(END, &[0xFF; 4]);
    client.put(STATICS + at::ERRNO, &errno_before.to_le_bytes());
    client.call(entry, &[TEXT, END]);
    let bits = client.session.kernel.os().fpa_double(0).expect("F0");
    (
        bits,
        client.word(END).wrapping_sub(TEXT),
        client.word(STATICS + at::ERRNO),
    )
}

#[test]
fn strtod_reads_as_genuine_does() {
    let mut client = client();
    for (text, bits, end, errno) in STRTOD {
        let read = read_text(&mut client, strtod, text, 0);
        assert_eq!(read, (bits, end, errno), "{:?}", text.escape_ascii());
    }
}

#[test]
fn atof_reads_as_genuine_does_leaving_errno_as_it_is() {
    let mut client = client();
    for (text, bits, errno) in ATOF {
        assert_eq!(
            read_text(&mut client, atof, text, 0).0,
            bits,
            "{:?}",
            text.escape_ascii()
        );
        assert_eq!(client.word(STATICS + at::ERRNO), errno);
        assert_eq!(
            read_text(&mut client, atof, text, 7).2,
            7,
            "errno untouched"
        );
    }
}

/// A decimal number's text spread over every kind `strtod` reads and well
/// past a double's range either way.
fn random_decimal(prng: &mut Prng) -> String {
    let digits: String = (0..=prng.below(40))
        .map(|_| char::from(b'0' + u8::try_from(prng.below(10)).expect("a digit")))
        .collect();
    let point = prng.below(digits.len() + 1);
    let exponent = i32::try_from(prng.below(760)).expect("small") - 380;
    let sign = ["", "-", "+"][prng.below(3)];
    format!("{sign}{}.{}e{exponent}", &digits[..point], &digits[point..])
}

#[test]
fn strtod_rounds_every_decimal_number_as_the_host_does() {
    let mut prng = Prng::new(tairix_fuzzseed::start(
        "strtod_rounding",
        "TAIRIX_CLIB_STRTOD_SEED",
    ));
    let mut client = client();
    for _ in 0..3_000 {
        let text = random_decimal(&mut prng);
        let host: f64 = text.parse().expect("the host reads it");
        let nonzero = text
            .bytes()
            .take_while(|&b| b != b'e')
            .any(|b| (b'1'..=b'9').contains(&b));
        let (expected, range_error) = if host.is_infinite() {
            (HUGE_VAL.copysign(host), true)
        } else {
            (host, nonzero && host.abs() < f64::MIN_POSITIVE)
        };
        let errno = if range_error { ERANGE } else { 0 };
        let read = read_text(&mut client, strtod, text.as_bytes(), 0);
        let length = u32::try_from(text.len()).expect("short");
        assert_eq!(read, (expected.to_bits(), length, errno), "{text}");
    }
}

#[test]
fn strtod_reads_hexadecimal_numbers_rounded_once() {
    let mut client = client();
    for (text, value, end, errno) in [
        ("0x1p-1074", f64::from_bits(1), 9, ERANGE),
        ("0x1.fffffffffffffp1023", f64::MAX, 22, 0),
        ("0x1.fffffffffffff8p1023", f64::MAX, 23, ERANGE),
        ("-0x0p0", -0.0, 6, 0),
        ("0xABC.8p-4", 171.78125, 10, 0),
        ("0x1.000000000000000000000000000000001p0", 1.0, 39, 0),
        (
            "0x1.000000000000080000000000000000001p0",
            1.0 + f64::EPSILON,
            39,
            0,
        ),
        ("0x", 0.0, 1, 0),
        ("0xg", 0.0, 1, 0),
        ("0x1p", 0.0, 0, 0),
    ] {
        let read = read_text(&mut client, strtod, text.as_bytes(), 0);
        assert_eq!(read, (value.to_bits(), end, errno), "{text}");
    }
}

#[test]
fn strtod_reads_more_digits_than_any_double_needs() {
    let mut client = client();
    let mut text = String::from("1.");
    text.extend(core::iter::repeat_n('0', 10_000));
    text.push('1');
    let length = u32::try_from(text.len()).expect("short");
    assert_eq!(
        read_text(&mut client, strtod, text.as_bytes(), 0),
        (1f64.to_bits(), length, 0)
    );
}

#[test]
fn strtod_reads_nan_s_payload_and_the_sign_of_either_special() {
    let mut client = client();
    assert_eq!(
        read_text(&mut client, strtod, b"nan(12ab_)x", 0),
        (NAN, 10, 0)
    );
    assert_eq!(
        read_text(&mut client, strtod, b"nan(12", 0),
        (NAN, 3, 0),
        "an unclosed payload unread"
    );
    assert_eq!(
        read_text(&mut client, strtod, b"-NaN", 0),
        (NAN | 1 << 63, 4, 0)
    );
    assert_eq!(
        read_text(&mut client, strtod, b"-Infinite", 0),
        (f64::NEG_INFINITY.to_bits(), 4, 0)
    );
}

#[test]
fn digits_far_past_those_kept_round_as_the_whole_number_does() {
    let mut client = client();
    let zeros = "0".repeat(5000);
    for (text, want) in [
        (format!("1{zeros}e-5000"), 1.0f64),
        (format!("1{zeros}1e-5001"), 1.0),
        // 2⁵³ + 1, halfway between two doubles: to even, and up for
        // anything past the half however far along.
        (
            format!("9007199254740993{zeros}e-5000"),
            9_007_199_254_740_992.0,
        ),
        (
            format!("9007199254740993{zeros}1e-5001"),
            9_007_199_254_740_994.0,
        ),
        (format!("0.{zeros}25e5001"), 2.5),
    ] {
        let read = read_text(&mut client, strtod, text.as_bytes(), 0);
        assert_eq!(read.0, want.to_bits(), "{}…", &text[..20]);
        assert_eq!(read.1 as usize, text.len());
    }
}

/// What RISC OS 5.30's `strtof` was recorded answering: F0 single-rounded,
/// a float's overflow or underflow a range error.
#[test]
fn strtof_reads_a_float_rounded_once_as_genuine_does() {
    let mut client = client();
    for (text, value, end, errno) in [
        (&b"0.1"[..], f64::from(0.1f32), 3, 0),
        (b"1e40", f64::INFINITY, 4, ERANGE),
        (b"1e-50", 0.0, 5, ERANGE),
        (b"1e-40", f64::from(1e-40f32), 5, ERANGE),
        (b"-0x1.8p1 q", -3.0, 8, 0),
        (b"12.75e1 xyz", 127.5, 7, 0),
    ] {
        let read = read_text(&mut client, strtof, text, 0);
        assert_eq!(
            read,
            (value.to_bits(), end, errno),
            "{:?}",
            text.escape_ascii()
        );
    }
    let (bits, end, errno) = read_text(&mut client, strtof, b"nan(7)", 0);
    assert_eq!((end, errno), (6, 0));
    assert_eq!(
        bits,
        crate::c::fpa::widened(single_bits(f64::from_bits(NAN))).to_bits(),
        "the NaN narrowed and widened alike on every host"
    );
}

#[test]
fn strtod_takes_the_locale_s_decimal_point() {
    let mut client = client();
    client.put(POINT, b",\0");
    client.put(STATICS + at::LCONV, &POINT.to_le_bytes());
    assert_eq!(
        read_text(&mut client, strtod, b"1,5", 0),
        (1.5f64.to_bits(), 3, 0)
    );
    assert_eq!(
        read_text(&mut client, strtod, b"1.5", 0),
        (1f64.to_bits(), 1, 0)
    );
    let without: Vec<u8> = b"2,25".to_vec();
    assert_eq!(
        read_text(&mut client, atof, &without, 0).0,
        2.25f64.to_bits()
    );
}
