//! Decimal conversion held to the host: its exact formatting of a double's
//! whole expansion, and its correctly rounded parsing.

extern crate std;

use std::format;
use std::string::String;
use std::vec::Vec;

use tairix_fuzzseed::Prng;

use super::*;
use crate::{Binary, Ieee, Nans, Policy, Rounding, Tininess, Unpacked};

fn parts_of(x: f64) -> Parts {
    match Binary::DOUBLE.unpack(x.to_bits()) {
        Unpacked::Finite { parts, .. } => parts,
        other => panic!("{x:e} is not finite and nonzero: {other:?}"),
    }
}

/// Doubles over every exponent, subnormals among them.
fn random_double(rng: &mut Prng) -> f64 {
    loop {
        let x = f64::from_bits(rng.next_u64());
        if x.is_finite() && x != 0.0 {
            return x;
        }
    }
}

/// The digits of `x`'s whole expansion, and the place of the first. The host
/// writes a double exactly when asked for more digits than any has.
fn expansion(x: f64) -> (Vec<u8>, i32) {
    let text = format!("{:.1100e}", x.abs());
    let (mantissa, exponent) = text.split_once('e').unwrap();
    let digits = mantissa
        .bytes()
        .filter(u8::is_ascii_digit)
        .map(|b| b - b'0')
        .collect();
    (digits, exponent.parse().unwrap())
}

/// `x` to `n` digits in `mode`, as the definitions of the directions make it
/// from the whole expansion, and as the host rounds to nearest.
fn expected(x: f64, n: u32, mode: Mode) -> Decimal {
    let negative = x.is_sign_negative();
    if mode == Mode::Nearest {
        let text = format!("{:.*e}", (n - 1) as usize, x.abs());
        let (mantissa, exponent) = text.split_once('e').unwrap();
        let significand: String = mantissa.chars().filter(char::is_ascii_digit).collect();
        let place: i32 = exponent.parse().unwrap();
        return Decimal {
            negative,
            significand: significand.parse().unwrap(),
            exponent: place - i32::try_from(n - 1).unwrap(),
        };
    }
    let (digits, mut place) = expansion(x);
    let n = n as usize;
    let toward_zero = digits[..n]
        .iter()
        .fold(0u128, |s, &d| s * 10 + u128::from(d));
    let discarded = digits[n..].iter().any(|&d| d != 0);
    let away = match mode {
        Mode::Up => !negative && discarded,
        Mode::Down => negative && discarded,
        Mode::Zero => false,
        Mode::NearestAway => digits[n] >= 5,
        Mode::Nearest => unreachable!(),
    };
    let mut significand = toward_zero + u128::from(away);
    if significand == 10u128.pow(u32::try_from(n).unwrap()) {
        significand /= 10;
        place += 1;
    }
    Decimal {
        negative,
        significand,
        exponent: place - i32::try_from(n - 1).unwrap(),
    }
}

#[test]
fn every_double_is_written_to_any_digits_in_every_mode_as_its_expansion_says() {
    let mut rng = Prng::new(0x5EED_DEC1);
    for _ in 0..3000 {
        let x = random_double(&mut rng);
        let n = u32::try_from(rng.next_u64() % u64::from(MAX_DIGITS)).unwrap() + 1;
        for mode in Mode::ALL {
            let (ours, inexact) = Decimal::of(parts_of(x), n, mode).unwrap();
            assert_eq!(ours, expected(x, n, mode), "{x:e} to {n} digits {mode:?}");
            let (digits, _) = expansion(x);
            let exact = digits[n as usize..].iter().all(|&d| d == 0);
            assert_eq!(inexact, !exact, "{x:e} to {n} digits");
        }
    }
}

#[test]
fn a_tie_rounds_to_even_away_or_by_direction() {
    let tie = |x: f64, n: u32, mode: Mode| Decimal::of(parts_of(x), n, mode).unwrap().0;
    let d = |significand: u128, exponent: i32| Decimal {
        negative: false,
        significand,
        exponent,
    };
    assert_eq!(tie(2.5, 1, Mode::Nearest), d(2, 0));
    assert_eq!(tie(3.5, 1, Mode::Nearest), d(4, 0));
    assert_eq!(tie(2.5, 1, Mode::NearestAway), d(3, 0));
    assert_eq!(tie(2.5, 1, Mode::Zero), d(2, 0));
    assert_eq!(tie(2.5, 1, Mode::Up), d(3, 0));
    assert_eq!(tie(-2.5, 1, Mode::Down).significand, 3);
    assert_eq!(tie(9.5, 1, Mode::Nearest), d(1, 1));
    // A tie only when nothing follows the five.
    assert_eq!(tie(2.500_000_000_000_001, 1, Mode::Nearest), d(3, 0));
}

/// The values the 8087's debugger was held to, worked out with 6000-digit
/// decimal arithmetic: the largest double-extended value, the least
/// subnormal, and the least normal.
#[test]
fn the_ends_of_double_extended_are_written_exactly() {
    let shown = |exponent: u16, sig: u64| {
        let parts = Parts::of(crate::Extended::new(false, exponent, sig));
        Decimal::of(parts, 19, Mode::Nearest).unwrap().0
    };
    let d = |significand: u128, place: i32| Decimal {
        negative: false,
        significand,
        exponent: place - 18,
    };
    assert_eq!(shown(0x7FFE, u64::MAX), d(1_189_731_495_357_231_765, 4932));
    assert_eq!(shown(0, 1), d(3_645_199_531_882_474_603, -4951));
    assert_eq!(shown(1, 1 << 63), d(3_362_103_143_112_093_506, -4932));
}

#[test]
fn zero_and_a_digit_count_out_of_range_are_answered() {
    let zero = Parts {
        negative: true,
        exp: 7,
        sig: 0,
    };
    let answer = Decimal {
        negative: true,
        significand: 0,
        exponent: 0,
    };
    assert_eq!(Decimal::of(zero, 5, Mode::Nearest), Some((answer, false)));
    let one = parts_of(1.0);
    assert_eq!(Decimal::of(one, 0, Mode::Nearest), None);
    assert_eq!(Decimal::of(one, MAX_DIGITS + 1, Mode::Nearest), None);
    let beyond = Parts {
        exp: MOST_EXP + 1,
        ..one
    };
    assert_eq!(Decimal::of(beyond, 5, Mode::Nearest), None);
    let least = Parts {
        exp: LEAST_EXP,
        ..one
    };
    assert_eq!(
        Decimal::of(least, 5, Mode::Nearest),
        Some((
            Decimal {
                negative: false,
                significand: 18_226,
                exponent: -4955,
            },
            true
        )),
        "the FPA's least denormal, 2^-16446"
    );
    assert_eq!(
        Decimal::of(
            Parts {
                exp: LEAST_EXP - 1,
                ..one
            },
            5,
            Mode::Nearest
        ),
        None
    );
}

fn round_to(format: Binary, x: Exact) -> u64 {
    let ieee = Ieee {
        format,
        mode: Mode::Nearest,
        policy: Policy {
            tininess: Tininess::AfterRounding,
            nans: Nans::Propagate,
            flush_to_zero: false,
        },
    };
    ieee.round(x).bits
}

/// What `d` reads as in `format`, rounded to nearest.
fn read(d: Decimal, format: Binary) -> u64 {
    match d.exact() {
        Ok(x) if x.is_zero() => format.zero(d.negative),
        Ok(x) => round_to(format, x),
        Err(Beyond::Above) => format.infinity(d.negative),
        Err(Beyond::Below) => format.zero(d.negative),
    }
}

#[test]
fn decimal_text_reads_as_the_host_reads_it() {
    let mut rng = Prng::new(0x5EED_7EAD);
    for _ in 0..20_000 {
        let n = u32::try_from(rng.next_u64() % u64::from(MAX_DIGITS)).unwrap() + 1;
        let significand = u128::from(rng.next_u64()) << 64 | u128::from(rng.next_u64());
        let significand = significand % 10u128.pow(n);
        // Exponents past both ends of binary64's range, and well inside.
        let exponent = i32::try_from(rng.next_u64() % 760).unwrap() - 380;
        let d = Decimal {
            negative: rng.next_u64() & 1 == 1,
            significand,
            exponent,
        };
        let text = format!(
            "{}{significand}e{exponent}",
            if d.negative { "-" } else { "" }
        );
        let host: f64 = text.parse().unwrap();
        assert_eq!(read(d, Binary::DOUBLE), host.to_bits(), "{text}");
        let host: f32 = text.parse().unwrap();
        assert_eq!(read(d, Binary::SINGLE), u64::from(host.to_bits()), "{text}");
    }
}

#[test]
fn a_decimal_tie_between_doubles_breaks_by_what_follows_it() {
    let d = |significand: u128, exponent: i32| Decimal {
        negative: false,
        significand,
        exponent,
    };
    // 2^53 + 1 lies halfway between 2^53 and 2^53 + 2.
    let half = d(9_007_199_254_740_993, 0);
    assert_eq!(
        read(half, Binary::DOUBLE),
        9_007_199_254_740_992f64.to_bits()
    );
    let half_up = d(9_007_199_254_740_995, 0);
    assert_eq!(
        read(half_up, Binary::DOUBLE),
        9_007_199_254_740_996f64.to_bits()
    );
    let past = d(9_007_199_254_740_993 * 10u128.pow(22) + 1, -22);
    assert_eq!(
        read(past, Binary::DOUBLE),
        9_007_199_254_740_994f64.to_bits()
    );
}

#[test]
fn every_double_extended_value_comes_back_from_twenty_one_digits() {
    let mut rng = Prng::new(0x5EED_0021);
    for _ in 0..3000 {
        let sig = rng.next_u64() | 1 << 63;
        let exp = i32::try_from(rng.next_u64() % 32_765).unwrap() - 16_382;
        let x = Parts {
            negative: rng.next_u64() & 1 == 1,
            exp,
            sig,
        };
        let (d, _) = Decimal::of(x, 21, Mode::Nearest).unwrap();
        let back = d.exact().unwrap().normalized(true);
        let rounded = back.rounded(Rounding::extended(Mode::Nearest));
        assert_eq!(
            (back.exp + i32::from(rounded.carried), rounded.sig),
            (exp, sig),
            "{d:?}"
        );
    }
}

#[test]
fn the_range_s_bounds_are_where_double_extended_ends() {
    // 10^p as 5^p × 2^p: its bits.
    let bits = |p: u32| {
        let mut five = Big::from_u128(1);
        assert!(five.mul_pow5(p));
        five.bit_len() + p
    };
    // 10^4933 reaches 2^16384, past the largest finite value; 10^4932 does
    // not.
    assert!(bits(4933) > 16_384);
    assert!(bits(4932) <= 16_384);
    // 10^−4952 is below 2^−16447, half the FPA's least denormal; 10^−4951
    // is not.
    assert!(bits(4952) > 16_447);
    assert!(bits(4951) <= 16_447);
    let one = |exponent: i32| {
        Decimal {
            negative: false,
            significand: 1,
            exponent,
        }
        .exact()
    };
    assert_eq!(one(4933), Err(Beyond::Above));
    assert!(one(4932).is_ok());
    assert_eq!(one(-4953), Err(Beyond::Below));
    assert!(one(-4952).is_ok());
    let many = Decimal {
        negative: true,
        significand: 10u128.pow(37),
        exponent: 4932 - 37,
    };
    assert!(many.exact().is_ok());
    assert_eq!(
        Decimal {
            exponent: many.exponent + 1,
            ..many
        }
        .exact(),
        Err(Beyond::Above)
    );
}

/// The decimal digits of `n`, most significant first.
fn digits(n: u128) -> Vec<u8> {
    format!("{n}").bytes().map(|b| b - b'0').collect()
}

#[test]
fn a_significand_of_any_length_reads_as_a_short_one_does() {
    let mut rng = Prng::new(0x5EED_D161);
    for _ in 0..2_000 {
        let significand = (u128::from(rng.next_u64()) << (rng.next_u64() % 64)) % 10u128.pow(38);
        let exponent = i32::try_from(rng.next_u64() % 801).unwrap() - 400;
        let negative = rng.next_u64() & 1 != 0;
        let short = Decimal {
            negative,
            significand,
            exponent,
        }
        .exact();
        let mut long = std::vec![0, 0];
        long.extend(digits(significand));
        assert_eq!(
            exact_digits(negative, long.iter().copied(), exponent),
            short
        );
        long.extend([0; 40]);
        assert_eq!(
            exact_digits(negative, long.iter().copied(), exponent - 40),
            short
        );
    }
}

#[test]
fn every_digit_of_a_long_significand_counts() {
    // (2^33 + 1) / 2^200: 150 significant digits, every one needed for the
    // value to read exactly, with nothing below its last bit.
    let text = "534552942080669282006718710501737016300840618426912261279599169763090894345323060074453485123708337861097986999647790895551224821247160434722900390625";
    let whole: Vec<u8> = text.bytes().map(|b| b - b'0').collect();
    let x = exact_digits(false, whole.iter().copied(), -200)
        .unwrap()
        .normalized(true);
    assert_eq!(
        (x.sig, x.exp, x.sticky),
        (((1 << 33) + 1) << 93, -167, false)
    );
    let short = exact_digits(false, whole[..149].iter().copied(), -199)
        .unwrap()
        .normalized(true);
    assert!(short.sticky, "short of its last digit it is not exact");
    assert_eq!(exact_digits(true, [0, 0, 0], 7), Ok(zero(true)));
}

#[test]
fn plain_integers_read_a_small_value_as_the_general_way_does() {
    let mut rng = Prng::new(0x5EED_5A11);
    for _ in 0..20_000 {
        let digits = 1 + rng.next_u64() % 38;
        let n = (u128::from(rng.next_u64()) << 64 | u128::from(rng.next_u64()))
            % 10u128.pow(u32::try_from(digits).unwrap());
        let n = n.max(1);
        let exponent = i32::try_from(rng.next_u64() % 81).unwrap() - 40;
        let general = scaled(false, Big::from_u128(n), digits_of(n), exponent).unwrap();
        if let Some(fast) = small(n, exponent) {
            assert_eq!(fast, general, "{n}E{exponent}");
        }
    }
    for (n, exponent) in [
        (1, -27),
        (u128::from(u64::MAX), -27),
        (10u128.pow(38) - 1, 0),
        (1, 38),
        (u128::MAX / 11, 1),
    ] {
        if let Some(fast) = small(n, exponent) {
            assert_eq!(
                fast,
                scaled(false, Big::from_u128(n), digits_of(n), exponent).unwrap(),
                "{n}E{exponent}"
            );
        }
    }
    assert!(small(1, -28).is_none(), "five to the 28th passes a word");
    assert!(
        small(u128::from(u64::MAX) + 1, -1).is_none(),
        "a quotient's dividend passes a word"
    );
}
