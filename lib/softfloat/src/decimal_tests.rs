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
}

fn round_to(format: Binary, x: Exact) -> u64 {
    let ieee = Ieee {
        format,
        mode: Mode::Nearest,
        policy: Policy {
            tininess: Tininess::AfterRounding,
            nans: Nans::Propagate,
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
    // 10^−4951 is below 2^−16446, half the least subnormal; 10^−4950 is not.
    assert!(bits(4951) > 16_446);
    assert!(bits(4950) <= 16_446);
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
    assert_eq!(one(-4952), Err(Beyond::Below));
    assert!(one(-4951).is_ok());
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
