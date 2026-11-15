//! Deterministic fuzz harness for decimal conversion and the FPA's packed
//! decimal, which read values a guest program controls. For any input:
//!
//! 1. Nothing panics.
//! 2. A finite value in range is written to exactly the digits asked for:
//!    toward zero and away from it one unit apart where inexact, and to
//!    nearest on one of the two.
//! 3. Twenty-one digits bring a normal double-extended value back exactly.
//! 4. Any decimal value reads as an exact value of its sign, or past the end
//!    of the range its exponent points to.
//! 5. Any packed words read back as they were, but for the two bits the
//!    format keeps zero, and the value their digits hold survives being
//!    packed again.
//!
//! Runs the fixed smoke sweep under plain `cargo test`; keeps drawing from
//! the same seeded stream until `TAIRIX_FUZZ_BUDGET_SECS` elapses under
//! `cargo xtask fuzz`.

use tairix_fuzzseed::Prng;
use tairix_softfloat::decimal::MAX_DIGITS;
use tairix_softfloat::{Beyond, Decimal, Mode, Packed, Parts, Rounding};

/// Fixed-iteration sweep run once by a plain `cargo test` (no budget set).
const SMOKE_ITERATIONS: u64 = 400;

/// Any finite value's parts: zeros, unnormals and exponents past both ends
/// of double-extended's range among them.
fn hostile_parts(rng: &mut Prng) -> Parts {
    let sig = match rng.below(4) {
        0 => 0,
        1 => rng.next_u64() >> rng.below(64),
        _ => rng.next_u64() | 1 << 63,
    };
    let exp = i32::try_from(rng.below(34_000)).unwrap() - 17_000;
    Parts {
        negative: rng.next_u64() & 1 == 1,
        exp,
        sig,
    }
}

fn digits_of(n: u128) -> u32 {
    n.checked_ilog10().map_or(0, |log| log + 1)
}

/// The decimal one unit in the last of `digits` places further from zero.
fn further(d: Decimal, digits: u32) -> Decimal {
    let significand = d.significand + 1;
    if significand == 10u128.pow(digits) {
        Decimal {
            significand: significand / 10,
            exponent: d.exponent + 1,
            ..d
        }
    } else {
        Decimal { significand, ..d }
    }
}

fn written(rng: &mut Prng) {
    let p = hostile_parts(rng);
    let digits = u32::try_from(rng.below(41)).unwrap();
    let all = Mode::ALL.map(|mode| Decimal::of(p, digits, mode));
    let Some((toward, inexact)) = Decimal::of(p, digits, Mode::Zero) else {
        assert!(all.iter().all(Option::is_none), "{p:?} to {digits}");
        return;
    };
    assert!((1..=MAX_DIGITS).contains(&digits));
    if p.sig == 0 {
        assert_eq!(
            (toward.significand, toward.exponent, inexact),
            (0, 0, false)
        );
        return;
    }
    assert_eq!(digits_of(toward.significand), digits, "{p:?}");
    let answer = |mode| Decimal::of(p, digits, mode).map(|(d, _)| d);
    let (up, down) = (answer(Mode::Up), answer(Mode::Down));
    let (away, toward_by_direction) = if p.negative { (down, up) } else { (up, down) };
    assert_eq!(toward_by_direction, Some(toward), "{p:?}");
    let expected_away = if inexact {
        further(toward, digits)
    } else {
        toward
    };
    assert_eq!(away, Some(expected_away), "{p:?} to {digits}");
    for mode in [Mode::Nearest, Mode::NearestAway] {
        let near = answer(mode).unwrap();
        assert!(near == toward || near == expected_away, "{p:?} {mode:?}");
    }
}

fn brought_back(rng: &mut Prng) {
    let sig = rng.next_u64() | 1 << 63;
    let exp = i32::try_from(rng.below(32_766)).unwrap() - 16_382;
    let p = Parts {
        negative: rng.next_u64() & 1 == 1,
        exp,
        sig,
    };
    let (d, _) = Decimal::of(p, 21, Mode::Nearest).unwrap();
    let back = d.exact().unwrap().normalized(true);
    let rounded = back.rounded(Rounding::extended(Mode::Nearest));
    assert_eq!(
        (back.exp + i32::from(rounded.carried), rounded.sig),
        (exp, sig),
        "{d:?}"
    );
}

fn read(rng: &mut Prng) {
    let significand = u128::from(rng.next_u64()) << 64 | u128::from(rng.next_u64());
    let d = Decimal {
        negative: rng.next_u64() & 1 == 1,
        significand: significand >> rng.below(128),
        exponent: match rng.below(3) {
            0 => {
                i32::try_from(rng.next_u32() >> 1).unwrap() * if rng.below(2) == 0 { 1 } else { -1 }
            }
            _ => i32::try_from(rng.below(10_000)).unwrap() - 5_000,
        },
    };
    match d.exact() {
        Ok(x) => {
            assert_eq!(x.negative, d.negative);
            if d.significand == 0 {
                assert!(x.is_zero());
            } else {
                assert_eq!(x.sig >> 126, 1, "{d:?} reads as {x:?}");
            }
        }
        Err(Beyond::Above) => assert!(d.exponent > 0, "{d:?}"),
        Err(Beyond::Below) => assert!(d.exponent < 0, "{d:?}"),
    }
}

/// A packed value's words, its value, and that value packed again.
fn packed<const W: usize>(words: [u32; W]) {
    let value = Packed::<W>::from_words(words);
    let mut kept = words;
    kept[0] &= !0x3000_0000;
    assert_eq!(value.to_words(), Some(kept));
    let Some(decimal) = value.value() else {
        return;
    };
    let _ = decimal.exact();
    match Packed::<W>::of(decimal) {
        Some(again) => {
            assert_eq!(
                again.value().map(Decimal::exact),
                Some(decimal.exact()),
                "{words:08X?}"
            );
        }
        // Only an unnormalized significand moves its first digit's place,
        // and only past what the exponent's digits can say.
        None => assert!(digits_of(decimal.significand) < Packed::<W>::DIGITS),
    }
}

fn hostile_words(rng: &mut Prng) {
    // Mostly digits, so the arithmetic is reached as well as the refusals.
    let mut digit_word = || {
        if rng.below(4) == 0 {
            rng.next_u32()
        } else {
            (0..8).fold(0, |word, _| {
                word << 4 | u32::try_from(rng.below(10)).unwrap()
            })
        }
    };
    let standard = [digit_word(), digit_word(), digit_word()];
    let expanded = [digit_word(), digit_word(), digit_word(), digit_word()];
    packed(standard);
    packed(expanded);
}

#[test]
fn hostile_values_and_words_convert_exactly_and_never_panic() {
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "hostile_values_and_words_convert_exactly_and_never_panic",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    loop {
        for _ in 0..SMOKE_ITERATIONS {
            written(&mut rng);
            brought_back(&mut rng);
            read(&mut rng);
            hostile_words(&mut rng);
        }
        if !tairix_fuzzseed::within_budget(deadline) {
            break;
        }
    }
}
